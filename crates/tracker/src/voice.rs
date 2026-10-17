//! Sample voices: a sample played at a rate, with a 32-tap windowed-sinc
//! interpolator, loops, and click-free gain and pan changes.
//!
//! The kernel (Kaiser, beta 8, cut off at the source's Nyquist rate) is
//! within a fraction of a dB of the DAW's path (4x sinc oversampling read
//! back linearly) up to 0.9 of Nyquist. Samples carry guard frames on both
//! sides and a tiled copy of their loop, so a read never wraps mid-kernel.

use std::f64::consts::PI;
use std::sync::OnceLock;

use crate::module::{Envelope, Instrument, LoopKind, Sample};

const TAPS: usize = 32;
/// Frames a read reaches either side of its position.
const HALF: usize = TAPS / 2;
/// Guard frames before and after a sample's frames.
const PAD: usize = HALF + 1;
const PHASES: usize = 1024;
/// Kernels for reading at up to 1x, 1.25x, 1.6x and 2x the source rate:
/// faster reads use a lower cutoff so they alias less.
const STEP_BANDS: [f64; 4] = [1.0, 1.25, 1.6, 2.0];
const KAISER_BETA: f64 = 8.0;
/// Frames a gain or pan change glides over (at 48 kHz, 1.3 ms).
const RAMP_SECONDS: f64 = 0.0013;
/// A cut note fades out over this long.
pub(crate) const CUT_SECONDS: f64 = 0.003;
/// A key-off without an envelope fades out over this long.
pub(crate) const RELEASE_SECONDS: f64 = 0.01;

struct Kernels {
    /// `[band][phase]`.
    taps: Vec<[f32; TAPS]>,
}

/// The modified Bessel function of the first kind, order 0.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..50 {
        term *= (x / (2.0 * k as f64)).powi(2);
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

fn kernels() -> &'static Kernels {
    static KERNELS: OnceLock<Kernels> = OnceLock::new();
    KERNELS.get_or_init(|| {
        let mut taps = Vec::with_capacity(STEP_BANDS.len() * PHASES);
        let norm = bessel_i0(KAISER_BETA);
        for band in STEP_BANDS {
            let cutoff = 1.0 / band;
            for phase in 0..PHASES {
                let frac = phase as f64 / PHASES as f64;
                let mut k = [0.0f64; TAPS];
                for (i, k) in k.iter_mut().enumerate() {
                    // Tap i reads frame floor(pos) - (HALF - 1) + i.
                    let x = i as f64 - (HALF as f64 - 1.0) - frac;
                    let sinc = if x == 0.0 {
                        1.0
                    } else {
                        (PI * cutoff * x).sin() / (PI * cutoff * x)
                    };
                    let t = x / HALF as f64;
                    let window = bessel_i0(KAISER_BETA * (1.0 - t * t).max(0.0).sqrt()) / norm;
                    *k = sinc * window;
                }
                let sum: f64 = k.iter().sum();
                let mut out = [0.0f32; TAPS];
                for (o, k) in out.iter_mut().zip(k) {
                    *o = (k / sum) as f32;
                }
                taps.push(out);
            }
        }
        Kernels { taps }
    })
}

/// Builds the interpolation tables, which the first voice would otherwise
/// build on the audio thread.
pub(crate) fn prepare_tables() {
    kernels();
}

/// A sample's loop, ready to read.
#[derive(Clone, Debug)]
struct PreparedLoop {
    /// Loop start, in sample frames.
    start: usize,
    len: usize,
    /// The loop tiled: `PAD` frames of its end, the loop, `PAD` of its start.
    tile: Vec<f32>,
}

/// A sample ready to play: its frames between guard frames, its loop, its
/// tuning.
#[derive(Clone, Debug)]
pub(crate) struct PreparedSample {
    /// `PAD` frames of silence, the sample up to its loop end, and `PAD`
    /// frames of what follows (the loop's start again, or silence).
    main: Vec<f32>,
    /// Frames of the sample proper (its loop end when it loops).
    end: usize,
    looped: Option<PreparedLoop>,
    /// Source frames per second at a musical frequency of 1 Hz.
    pub(crate) rate_per_hz: f64,
}

impl PreparedSample {
    /// `rate_per_hz`: the sample's declared rate over its root frequency,
    /// detune included.
    pub(crate) fn new(sample: &Sample, rate_per_hz: f64) -> PreparedSample {
        let mut data = sample.data.clone();
        remove_dc_offset(&mut data);
        let len = data.len();
        let mut range = match sample.loop_kind {
            LoopKind::Off => None,
            _ if len == 0 => None,
            kind => {
                let start = sample.loop_start.min(len - 1);
                let end = (start + sample.loop_len).min(len);
                (end - start >= 2).then_some((start, end, kind))
            }
        };
        if let Some((start, end, LoopKind::PingPong)) = range {
            // Forwards then backwards, as one forward loop.
            data.truncate(end);
            for i in 0..end - start {
                data.push(data[end - 1 - i]);
            }
            range = Some((start, data.len(), LoopKind::Forward));
        }
        let end = range.map_or(len, |(_, end, _)| end);
        let mut main = vec![0.0; PAD];
        main.extend_from_slice(&data[..end]);
        let looped = range.map(|(start, end, _)| {
            let loop_len = end - start;
            let at = |i: isize| data[start + i.rem_euclid(loop_len as isize) as usize];
            PreparedLoop {
                start,
                len: loop_len,
                tile: (-(PAD as isize)..(loop_len + PAD) as isize)
                    .map(at)
                    .collect(),
            }
        });
        match &looped {
            Some(l) => main.extend((0..PAD).map(|i| l.tile[PAD + i % l.len])),
            None => main.extend(std::iter::repeat_n(0.0, PAD)),
        }
        PreparedSample {
            main,
            end,
            looped,
            rate_per_hz,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.end
    }

    fn loop_start(&self) -> Option<usize> {
        self.looped.as_ref().map(|l| l.start)
    }
}

/// The DAW's `combinePan`: a pan envelope (32 centre) swings the voice's pan
/// by as much room as it has on its side.
fn combine_pan(base: f64, envelope: f64) -> f64 {
    let pan255 = base.clamp(0.0, 1.0) * 255.0;
    let headroom = (128.0 - (pan255 - 128.0).abs()) / 32.0;
    ((pan255 + (envelope - 32.0) * headroom) / 255.0).clamp(0.0, 1.0)
}

/// The DAW's sample conditioning: a mean of 0.2% or more is taken out.
fn remove_dc_offset(data: &mut [f32]) {
    if data.is_empty() {
        return;
    }
    let mean = (data.iter().map(|&s| s as f64).sum::<f64>() / data.len() as f64) as f32;
    if mean.abs() < 0.002 {
        return;
    }
    for s in data {
        *s -= mean;
    }
}

/// A gain that glides to its target.
#[derive(Clone, Copy, Debug, Default)]
struct Glide {
    value: f32,
    target: f32,
    step: f32,
    frames: u32,
}

impl Glide {
    fn set(&mut self, target: f32, frames: u32) {
        self.target = target;
        if frames == 0 {
            self.value = target;
            self.frames = 0;
        } else {
            self.step = (target - self.value) / frames as f32;
            self.frames = frames;
        }
    }

    fn jump(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.frames = 0;
    }

    #[inline]
    fn next(&mut self) -> f32 {
        if self.frames > 0 {
            self.frames -= 1;
            self.value = if self.frames == 0 {
                self.target
            } else {
                self.value + self.step
            };
        }
        self.value
    }
}

/// An XM instrument's envelopes, fadeout and auto-vibrato on one note,
/// stepped once a tick as FT2 does.
#[derive(Clone, Copy, Debug)]
pub(crate) struct XmVoice {
    pub(crate) instrument: usize,
    volume_tick: u16,
    pan_tick: u16,
    released: bool,
    /// 32768 down to 0 after key-off.
    fade: u32,
    vibrato_phase: u8,
    ticks: u32,
    /// The auto-vibrato's full depth, in cents.
    vibrato_cents: f64,
    /// What the envelopes and vibrato make of the voice this tick.
    pub(crate) volume_factor: f64,
    pub(crate) pan_envelope: Option<f64>,
    pub(crate) pitch_factor: f64,
}

/// An envelope's value at `tick`, 0-64, between its points.
fn envelope_value(envelope: &Envelope, tick: u16) -> f64 {
    let points = &envelope.points;
    let Some(&(first_tick, first_value)) = points.first() else {
        return 32.0;
    };
    if tick <= first_tick {
        return first_value as f64;
    }
    for pair in points.windows(2) {
        let ((t0, v0), (t1, v1)) = (pair[0], pair[1]);
        if tick > t1 {
            continue;
        }
        if t1 <= t0 {
            return v1 as f64;
        }
        let t = (tick - t0) as f64 / (t1 - t0) as f64;
        return v0 as f64 + (v1 as f64 - v0 as f64) * t;
    }
    points[points.len() - 1].1 as f64
}

/// The next tick of an envelope: held at its sustain point while the key is
/// down; on reaching its loop's end point, back to the loop's start.
fn envelope_advance(envelope: &Envelope, tick: u16, released: bool) -> u16 {
    let points = &envelope.points;
    if let Some(&(sustain, _)) = envelope
        .sustain
        .filter(|_| !released)
        .and_then(|s| points.get(s))
    {
        if tick == sustain {
            return tick;
        }
    }
    let next = tick.saturating_add(1);
    if let Some((start, end)) = envelope.looped {
        if let (Some(&(start_tick, _)), Some(&(end_tick, _))) = (points.get(start), points.get(end))
        {
            if end_tick > start_tick && tick < end_tick && next >= end_tick {
                return start_tick;
            }
        }
    }
    next
}

impl XmVoice {
    /// A note's envelopes at their start.
    pub(crate) fn new(index: usize, instrument: &Instrument, vibrato_cents: f64) -> XmVoice {
        let mut voice = XmVoice {
            instrument: index,
            volume_tick: 0,
            pan_tick: 0,
            released: false,
            fade: 32768,
            vibrato_phase: 0,
            ticks: 0,
            vibrato_cents,
            volume_factor: 1.0,
            pan_envelope: None,
            pitch_factor: 1.0,
        };
        voice.levels(instrument);
        voice
    }

    /// The envelopes' values where they stand.
    fn levels(&mut self, instrument: &Instrument) {
        let volume = instrument
            .volume_envelope
            .as_ref()
            .map_or(1.0, |e| envelope_value(e, self.volume_tick) / 64.0);
        self.volume_factor = volume * self.fade as f64 / 32768.0;
        self.pan_envelope = instrument
            .panning_envelope
            .as_ref()
            .map(|e| envelope_value(e, self.pan_tick));
    }

    /// Key off: the envelopes leave their sustain, the fadeout begins.
    pub(crate) fn release(&mut self) {
        self.released = true;
    }

    pub(crate) fn set_position(&mut self, tick: u16) {
        self.volume_tick = tick;
        self.pan_tick = tick;
    }

    /// Steps a tick on, to the levels the voice glides to over the tick
    /// (envelope points sit on ticks, so this follows them exactly, as the
    /// DAW's ramps do). False once faded out.
    pub(crate) fn tick(&mut self, instrument: &Instrument) -> bool {
        if let Some(e) = &instrument.volume_envelope {
            self.volume_tick = envelope_advance(e, self.volume_tick, self.released);
        }
        if let Some(e) = &instrument.panning_envelope {
            self.pan_tick = envelope_advance(e, self.pan_tick, self.released);
        }
        if self.released {
            self.fade = self.fade.saturating_sub(instrument.fadeout as u32);
        }
        self.levels(instrument);
        let v = instrument.vibrato;
        self.pitch_factor = 1.0;
        if v.depth > 0 && v.rate > 0 && self.vibrato_cents != 0.0 {
            let depth = if v.sweep > 0 {
                self.vibrato_cents * (self.ticks as f64 / v.sweep as f64).min(1.0)
            } else {
                self.vibrato_cents
            };
            // The DAW's oscillator shapes, signed as it signs them.
            let phase = self.vibrato_phase as f64 / 256.0;
            let (wave, sign) = match v.kind & 3 {
                1 => (if phase < 0.5 { 1.0 } else { -1.0 }, -1.0),
                2 => (sawtooth(phase), 1.0),
                3 => (sawtooth(phase), -1.0),
                _ => ((2.0 * PI * phase).sin(), -1.0),
            };
            self.pitch_factor = 2f64.powf(sign * depth * wave / 1200.0);
            self.vibrato_phase = self.vibrato_phase.wrapping_add(v.rate);
        }
        self.ticks += 1;
        !self.released || self.fade > 0 || instrument.fadeout == 0
    }
}

/// Web Audio's sawtooth: 0 at the start, up to 1, a jump to -1 at half way.
fn sawtooth(phase: f64) -> f64 {
    let p = phase + 0.5;
    2.0 * (p - p.floor()) - 1.0
}

/// One playing sample.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Voice {
    pub(crate) active: bool,
    pub(crate) sample: usize,
    /// Position in source frames.
    pos: f64,
    /// Whether the position has wrapped round the loop (and so reads the
    /// loop's tile).
    wrapped: bool,
    /// Source frames per output frame.
    step: f64,
    /// The note's musical frequency.
    pub(crate) frequency: f64,
    /// 0-1.
    pub(crate) volume: f64,
    /// 0 (left) to 1.
    pub(crate) pan: f64,
    left: Glide,
    right: Glide,
    /// Frames left of a fade to silence, after which the voice stops.
    ending: Option<u32>,
    /// XM: the instrument's envelopes and vibrato.
    pub(crate) xm: Option<XmVoice>,
    /// Source frames per output frame before the auto-vibrato.
    base_step: f64,
}

impl Voice {
    pub(crate) fn start(
        &mut self,
        sample: usize,
        prepared: &PreparedSample,
        offset: Option<u32>,
        frequency: f64,
        volume: f64,
        pan: f64,
        out_rate: f64,
    ) {
        let len = prepared.len();
        let pos = match offset {
            Some(frames) if frames > 0 => {
                let frames = frames as usize;
                if frames < len {
                    frames
                } else if let Some(start) = prepared.loop_start() {
                    start
                } else {
                    len.saturating_sub(1)
                }
            }
            _ => 0,
        };
        *self = Voice {
            active: true,
            sample,
            pos: pos as f64,
            wrapped: false,
            step: 0.0,
            frequency,
            volume,
            pan,
            left: Glide::default(),
            right: Glide::default(),
            ending: None,
            xm: None,
            base_step: 0.0,
        };
        self.set_frequency(frequency, prepared, out_rate);
        let (l, r) = self.gains();
        self.left.jump(l);
        self.right.jump(r);
    }

    fn gains(&self) -> (f32, f32) {
        let (volume, pan) = match &self.xm {
            None => (self.volume, self.pan),
            Some(xm) => (
                self.volume * xm.volume_factor,
                xm.pan_envelope
                    .map_or(self.pan, |e| combine_pan(self.pan, e)),
            ),
        };
        // Web Audio's StereoPannerNode on a mono input: equal power.
        let x = pan.clamp(0.0, 1.0) * PI / 2.0;
        ((volume * x.cos()) as f32, (volume * x.sin()) as f32)
    }

    /// Starts an XM note's envelopes, at their first values.
    pub(crate) fn start_xm(&mut self, xm: XmVoice) {
        self.xm = Some(xm);
        self.step = self.base_step * xm.pitch_factor;
        let (l, r) = self.gains();
        self.left.jump(l);
        self.right.jump(r);
    }

    /// Steps the XM envelopes and vibrato a tick, gliding to their next
    /// values over the tick's `frames`.
    pub(crate) fn tick_xm(&mut self, instrument: &Instrument, frames: u32) {
        let Some(xm) = &mut self.xm else {
            return;
        };
        let more = xm.tick(instrument);
        self.step = self.base_step * xm.pitch_factor;
        if self.ending.is_none() {
            let (l, r) = self.gains();
            self.left.set(l, frames.max(1));
            self.right.set(r, frames.max(1));
            if !more {
                // Faded out: silent by the end of this tick.
                self.ending = Some(frames.max(1));
            }
        }
    }

    fn glide(&mut self, out_rate: f64) {
        let frames = (RAMP_SECONDS * out_rate) as u32;
        let (l, r) = self.gains();
        self.left.set(l, frames);
        self.right.set(r, frames);
    }

    pub(crate) fn set_frequency(
        &mut self,
        frequency: f64,
        prepared: &PreparedSample,
        out_rate: f64,
    ) {
        self.frequency = frequency;
        self.base_step = (frequency * prepared.rate_per_hz / out_rate).max(0.0);
        self.step = self.base_step * self.xm.map_or(1.0, |x| x.pitch_factor);
    }

    pub(crate) fn set_volume(&mut self, volume: f64, out_rate: f64) {
        self.volume = volume;
        if self.ending.is_none() {
            self.glide(out_rate);
        }
    }

    pub(crate) fn set_pan(&mut self, pan: f64, out_rate: f64) {
        self.pan = pan;
        if self.ending.is_none() {
            self.glide(out_rate);
        }
    }

    /// Fades to silence over `seconds`, then stops.
    pub(crate) fn fade_out(&mut self, seconds: f64, out_rate: f64) {
        if !self.active || self.ending.is_some() {
            return;
        }
        let frames = ((seconds * out_rate) as u32).max(1);
        self.left.set(0.0, frames);
        self.right.set(0.0, frames);
        self.ending = Some(frames);
    }

    /// Adds `left.len()` frames of the voice into the buffers.
    pub(crate) fn render(
        &mut self,
        prepared: &PreparedSample,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        if !self.active {
            return;
        }
        if prepared.end == 0 {
            self.active = false;
            return;
        }
        let band = STEP_BANDS
            .iter()
            .position(|&b| self.step <= b)
            .unwrap_or(STEP_BANDS.len() - 1);
        let table = &kernels().taps[band * PHASES..(band + 1) * PHASES];
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if let Some(frames) = &mut self.ending {
                if *frames == 0 {
                    self.active = false;
                    return;
                }
                *frames -= 1;
            }
            // Where the read lands, in the buffer it reads from.
            let (buffer, at) = match (&prepared.looped, self.wrapped) {
                (Some(l), true) => (&l.tile[..], self.pos - l.start as f64 + PAD as f64),
                _ => (&prepared.main[..], self.pos + PAD as f64),
            };
            let index = at as usize;
            let phase = (((at - index as f64) * PHASES as f64) as usize).min(PHASES - 1);
            let window = &buffer[index + 1 - HALF..index + 1 + HALF];
            let s = dot(window, &table[phase]);
            *l += s * self.left.next();
            *r += s * self.right.next();
            self.pos += self.step;
            match &prepared.looped {
                Some(lp) => {
                    let end = (lp.start + lp.len) as f64;
                    if self.pos >= end {
                        self.pos = lp.start as f64 + (self.pos - end) % lp.len as f64;
                        self.wrapped = true;
                    }
                }
                None => {
                    if self.pos >= prepared.end as f64 {
                        self.active = false;
                        return;
                    }
                }
            }
        }
    }
}

/// The kernel applied: eight running sums, so it vectorizes.
#[inline]
fn dot(window: &[f32], kernel: &[f32; TAPS]) -> f32 {
    let mut acc = [0.0f32; 8];
    for (w, k) in window
        .as_chunks::<8>()
        .0
        .iter()
        .zip(kernel.as_chunks::<8>().0)
    {
        for i in 0..8 {
            acc[i] += w[i] * k[i];
        }
    }
    acc.iter().sum()
}
