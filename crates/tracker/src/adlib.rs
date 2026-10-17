//! S3M AdLib channels: Scream Tracker 3's OPL2 driver, playing channel
//! commands on a cubed-opl chip. A port of the DAW's `opl-driver.ts`, which
//! follows st3play's `digadl.c` (frequency numbers, c2spd scaling, total
//! levels).

use cubed_opl::chip::Chip;
use cubed_opl::resample::Resampler;
use cubed_opl::NATIVE_RATE;

use crate::effects::PitchSource;
use crate::module::Module;

const OPERATOR_OFFSET: [u16; 9] = [0, 1, 2, 8, 9, 10, 16, 17, 18];
/// What `initadlib` loads on every channel: both operators silent.
const EMPTY_INSTRUMENT: [u8; 12] = [0, 0, 63, 63, 0, 0, 0, 0, 0, 0, 0, 0];
const RATE_NUMERATOR: f64 = 14_317_056.0;
const MUSICAL_SCALE: f64 = 16.0;
const C2FREQ: i64 = 8363;
const KEY_ON: u16 = 0x2000;
/// The chip's 16-bit output at full scale.
const FULL_SCALE: f32 = 1.0 / 32768.0;
/// Writes held back for one native sample (a key-on after its key-off).
const MAX_DEFERRED: usize = 32;
/// Register writes remembered for an observer, per event.
const MAX_LOGGED: usize = 256;

/// ST3's period for a musical frequency (`14317056 / hz`, the DAW's scale).
fn period_for_frequency(frequency: f64) -> i64 {
    if frequency > 0.0 {
        crate::effects::js_round(RATE_NUMERATOR / (frequency * MUSICAL_SCALE)) as i64
    } else {
        0
    }
}

/// `scalec2spd`: a note period for an instrument of `c2spd`.
fn scale_c2spd(period: i64, c2spd: u32) -> i64 {
    let mut c2 = (c2spd & 0xffff) as i64;
    if c2 < 1000 {
        c2 = C2FREQ;
    }
    let product = period * C2FREQ;
    if product.div_euclid(65536) >= c2 {
        return 32767;
    }
    product.div_euclid(c2).min(32767)
}

/// `setspd`: the rate a scaled period plays at, inside the song's limits.
fn hz_for_period(scaled: i64, limits: (i64, i64)) -> i64 {
    if scaled <= 0 {
        return 0;
    }
    (RATE_NUMERATOR / scaled.clamp(limits.0, limits.1) as f64).floor() as i64
}

/// `adlib_note`: block and frequency number, with the key-on bit set.
fn adlib_note(hz: i64) -> u16 {
    let mut h = (hz * 2) as u32;
    let mut block = 0u32;
    while h >= 3125 {
        block += 1;
        h >>= 1;
    }
    let b0 = ((block << 2) | 32) & 0xff;
    let fnum = h * 1024 / 3125;
    (((b0 << 8) | fnum) & 0xffff) as u16
}

/// An operator's total level for an instrument level and a volume (0-63).
fn total_level(level: u8, avol: u8) -> u8 {
    let mut out = 63 - (level & 63) as u32;
    if avol < 63 {
        let vol = if avol != 0 { avol as u32 + 1 } else { 0 };
        out = (out * vol) >> 6;
    }
    ((63 - out) & 63) as u8 | (level & 0xc0)
}

#[derive(Clone, Copy, Debug, Default)]
struct OplChannel {
    instrument: Option<u8>,
    note: u16,
    period_base: i64,
    scaled_base: i64,
    keyed: bool,
    avol: u8,
    /// A note started this tick: key off and on again at the flush.
    retrigger: bool,
    /// The volume changed this tick: written at the flush.
    volume: bool,
}

/// The AdLib half of an S3M's playback.
pub(crate) struct AdlibDriver {
    chip: Chip,
    resampler: Resampler,
    gain: f32,
    /// The value last written to each register (0xfc: none yet).
    cache: [u8; 256],
    channels: [OplChannel; 9],
    /// Which OPL channel each track plays on, once it has played.
    track_channel: Vec<Option<u8>>,
    /// The header's AdLib channel for each track.
    preset: Vec<Option<u8>>,
    /// Each instrument's registers and c2spd, by instrument number - 1.
    instruments: Vec<Option<([u8; 12], u32)>>,
    limits: (i64, i64),
    deferred: [(u16, u8); MAX_DEFERRED],
    deferred_len: usize,
    /// This event's writes (register, value, held back), for an observer.
    logged: [(u16, u8, bool); MAX_LOGGED],
    logged_len: usize,
}

impl AdlibDriver {
    /// A driver for `module`'s AdLib instruments, or None when it has none.
    pub(crate) fn new(module: &Module, rate: f64) -> Option<AdlibDriver> {
        let instruments: Vec<Option<([u8; 12], u32)>> = module
            .samples
            .iter()
            .map(|s| s.adlib.map(|a| (a.registers, s.c2spd)))
            .collect();
        if instruments.iter().all(Option::is_none) {
            return None;
        }
        let mut driver = AdlibDriver {
            chip: Chip::new(),
            resampler: Resampler::new(NATIVE_RATE, rate),
            gain: module.adlib_gain,
            cache: [0xfc; 256],
            channels: [OplChannel::default(); 9],
            track_channel: vec![None; module.channels],
            preset: module.adlib_channels.clone(),
            instruments,
            limits: if module.flags.amiga_limits {
                (453, 3424)
            } else {
                (64, 32767)
            },
            deferred: [(0, 0); MAX_DEFERRED],
            deferred_len: 0,
            logged: [(0, 0, false); MAX_LOGGED],
            logged_len: 0,
        };
        // Initialized by the player's first seek.
        driver.channels.iter_mut().for_each(|c| c.avol = 63);
        Some(driver)
    }

    /// Whether instrument `instrument` (1-based) plays here.
    pub(crate) fn handles(&self, instrument: u8) -> bool {
        self.instruments
            .get((instrument as usize).wrapping_sub(1))
            .is_some_and(Option::is_some)
    }

    /// `initadlib`: every channel silent and keyed off.
    pub(crate) fn reset(&mut self) {
        self.cache = [0xfc; 256];
        self.channels = [OplChannel::default(); 9];
        for c in &mut self.channels {
            c.avol = 63;
        }
        self.track_channel.fill(None);
        self.out(0x01, 0x20, false);
        self.out(0x08, 0x00, false);
        self.out(0xbd, 0x00, false);
        for ch in 0..9 {
            self.load_instrument(ch, &EMPTY_INSTRUMENT);
            self.out_note(ch, 0, false);
        }
    }

    /// The OPL channel `track` plays on, assigning one the first time.
    fn channel(&mut self, track: usize) -> Option<usize> {
        if let Some(ch) = self.track_channel.get(track).copied().flatten() {
            return Some(ch as usize);
        }
        let ch = match self.preset.get(track).copied().flatten() {
            Some(ch) => ch,
            None => (0..9u8).find(|c| !self.track_channel.contains(&Some(*c)))?,
        };
        if ch > 8 {
            return None;
        }
        *self.track_channel.get_mut(track)? = Some(ch);
        Some(ch as usize)
    }

    fn existing(&self, track: usize) -> Option<usize> {
        self.track_channel
            .get(track)
            .copied()
            .flatten()
            .map(usize::from)
    }

    pub(crate) fn note_on(&mut self, instrument: u8, velocity: u8, track: usize, frequency: f64) {
        let Some(&Some((registers, c2spd))) =
            self.instruments.get((instrument as usize).wrapping_sub(1))
        else {
            return;
        };
        let Some(ch) = self.channel(track) else {
            return;
        };
        if self.channels[ch].instrument != Some(instrument) {
            self.channels[ch].instrument = Some(instrument);
            self.load_instrument(ch, &registers);
        }
        let limits = self.limits;
        let c = &mut self.channels[ch];
        c.avol = crate::effects::js_round(velocity as f64 * 64.0 / 127.0).clamp(0.0, 63.0) as u8;
        c.period_base = period_for_frequency(frequency);
        c.scaled_base = if c.period_base == 0 {
            0
        } else {
            scale_c2spd(c.period_base, c2spd)
        };
        let hz = hz_for_period(c.scaled_base, limits);
        if hz == 0 {
            c.retrigger = false;
            self.key_off(ch);
        } else {
            let c = &mut self.channels[ch];
            c.note = adlib_note(hz);
            c.keyed = true;
            c.retrigger = true;
        }
        self.channels[ch].volume = true;
    }

    pub(crate) fn set_pitch(&mut self, track: usize, frequency: f64, source: Option<PitchSource>) {
        let Some(ch) = self.existing(track) else {
            return;
        };
        let c = self.channels[ch];
        let Some(instrument) = c.instrument.filter(|_| c.keyed) else {
            return;
        };
        let c2spd = self.instruments[instrument as usize - 1].map_or(0, |(_, c2)| c2);
        let period = period_for_frequency(frequency);
        let scaled = if period == 0 {
            0
        } else if source.is_some() {
            let scaled = scale_c2spd(period, c2spd);
            if source == Some(PitchSource::Target) {
                self.channels[ch].period_base = period;
                self.channels[ch].scaled_base = scaled;
            }
            scaled
        } else {
            (c.scaled_base + (period - c.period_base)).min(32767)
        };
        let hz = hz_for_period(scaled, self.limits);
        if hz == 0 {
            self.flush_channel(ch);
            self.key_off(ch);
            return;
        }
        self.channels[ch].note = adlib_note(hz);
        if !self.channels[ch].retrigger {
            self.out_note(ch, self.channels[ch].note, false);
        }
    }

    pub(crate) fn set_volume(&mut self, track: usize, volume: f64) {
        let Some(ch) = self.existing(track) else {
            return;
        };
        if self.channels[ch].instrument.is_none() {
            return;
        }
        self.channels[ch].avol = crate::effects::js_round(volume * 64.0).clamp(0.0, 63.0) as u8;
        self.channels[ch].volume = true;
    }

    pub(crate) fn note_off(&mut self, track: usize) {
        let Some(ch) = self.existing(track) else {
            return;
        };
        self.flush_channel(ch);
        self.key_off(ch);
    }

    pub(crate) fn all_notes_off(&mut self) {
        for ch in 0..9 {
            if self.track_channel.contains(&Some(ch as u8)) {
                self.flush_channel(ch);
                self.key_off(ch);
            }
        }
    }

    /// Writes what this tick's commands left pending: retriggers (key off,
    /// then on a native sample later) and volumes.
    pub(crate) fn flush(&mut self) {
        for ch in 0..9 {
            self.flush_channel(ch);
        }
    }

    fn flush_channel(&mut self, ch: usize) {
        let c = self.channels[ch];
        if c.retrigger {
            self.channels[ch].retrigger = false;
            self.out_note(ch, c.note & !KEY_ON, false);
            if c.keyed {
                self.out_note(ch, c.note, true);
            }
        }
        if c.volume {
            self.channels[ch].volume = false;
            self.write_volume(ch);
        }
    }

    fn key_off(&mut self, ch: usize) {
        self.channels[ch].keyed = false;
        self.out_note(ch, self.channels[ch].note & !KEY_ON, false);
    }

    fn load_instrument(&mut self, ch: usize, d: &[u8; 12]) {
        let op = OPERATOR_OFFSET[ch];
        for (i, base) in [
            0x20u16, 0x23, 0x40, 0x43, 0x60, 0x63, 0x80, 0x83, 0xe0, 0xe3,
        ]
        .into_iter()
        .enumerate()
        {
            self.out(base + op, d[i], false);
        }
        self.out(0xc0 + ch as u16, d[10], false);
    }

    fn write_volume(&mut self, ch: usize) {
        let Some(instrument) = self.channels[ch].instrument else {
            return;
        };
        let Some((d, _)) = self.instruments[instrument as usize - 1] else {
            return;
        };
        let op = OPERATOR_OFFSET[ch];
        let avol = self.channels[ch].avol;
        // An additive voice hears its modulator too.
        if d[10] & 1 != 0 {
            self.out(0x40 + op, total_level(d[2], avol), false);
        }
        self.out(0x43 + op, total_level(d[3], avol), false);
    }

    fn out_note(&mut self, ch: usize, note: u16, deferred: bool) {
        self.out(0xa0 + ch as u16, (note & 0xff) as u8, deferred);
        self.out(0xb0 + ch as u16, (note >> 8) as u8, deferred);
    }

    fn out(&mut self, reg: u16, val: u8, deferred: bool) {
        if self.cache[reg as usize] == val {
            return;
        }
        self.cache[reg as usize] = val;
        if self.logged_len < MAX_LOGGED {
            self.logged[self.logged_len] = (reg, val, deferred);
            self.logged_len += 1;
        }
        if deferred && self.deferred_len < MAX_DEFERRED {
            self.deferred[self.deferred_len] = (reg, val);
            self.deferred_len += 1;
        } else {
            self.chip.write(reg, val);
        }
    }

    /// The register writes since the last call: register, value, and
    /// whether it waits a native sample.
    pub(crate) fn take_logged(&mut self) -> &[(u16, u8, bool)] {
        let n = std::mem::take(&mut self.logged_len);
        &self.logged[..n]
    }

    /// Adds the chip's next `left.len()` frames into the buffers, at `gain`.
    pub(crate) fn render(&mut self, left: &mut [f32], right: &mut [f32], gain: f32) {
        let scale = FULL_SCALE * self.gain * gain;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (chip, deferred, deferred_len) =
                (&mut self.chip, &self.deferred, &mut self.deferred_len);
            let [a, b] = self.resampler.next(|| {
                let (a, b) = chip.clock_sample();
                // Key-ons held back a native sample go in after it.
                for &(reg, val) in &deferred[..*deferred_len] {
                    chip.write(reg, val);
                }
                *deferred_len = 0;
                [a as f32, b as f32]
            });
            *l += a * scale;
            *r += b * scale;
        }
    }
}
