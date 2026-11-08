//! Playing one OPL instrument by hand, with no song: the instrument editor's
//! keyboard.
//!
//! [`OplAudition`] is a chip of its own (an OPL3 in the mode Adlib Tracker II
//! runs it in) with up to 18 two-operator voices. A note loads the instrument
//! ([`FmPatch`]: eleven FM bytes, panning, fine-tune) into a free channel the way
//! the song player does at a note-on (same register order, the empty-envelope
//! workaround, the AT2 note table `a2::engine::note_frequency`) and keys it on;
//! a key-off clears the key bit and the instrument's release plays out. The song's
//! macros are not part of it: they belong to the song.
//!
//! It is shaped to be a mixer source (`cubed_mixer::Source`): [`render`] writes
//! the next block over the buffers (it never allocates, locks or blocks),
//! [`note_on`] / [`note_off`] / [`all_notes_off`] are `SourceEvent::NoteOn`
//! (`note`, `velocity`, `duration`), `NoteOff` and `AllNotesOff`, and
//! [`set_patch`] is the live edit: it rewrites the sounding channels' operator
//! registers on the spot, so a slider moved while a note is held is heard at
//! once. Only [`OplAudition::new`] allocates (the chip and the resampler): make
//! it off the audio thread.
//!
//! Percussion voice types and 4-op pairs are played as plain melodic 2-op
//! voices, as the DAW's own audition does.
//!
//! [`render`]: OplAudition::render
//! [`note_on`]: OplAudition::note_on
//! [`note_off`]: OplAudition::note_off
//! [`all_notes_off`]: OplAudition::all_notes_off
//! [`set_patch`]: OplAudition::set_patch

use crate::a2::engine::note_frequency;
use crate::chip::{Chip, CHANNELS, NATIVE_RATE};
use crate::fm::FmPatch;
use crate::resample::Resampler;

/// Modulator operator offset of channel `ch % 9`; the carrier is 3 higher.
const OP_OFFSET: [u16; 9] = [0x00, 0x01, 0x02, 0x08, 0x09, 0x0a, 0x10, 0x11, 0x12];
/// Full scale of the chip's 16-bit output (what `A2Player` uses).
const FULL_SCALE: f32 = 1.0 / 32768.0;
/// Register writes a block of notes can queue before they are applied at once.
const PENDING: usize = 128;

/// MIDI note `midi` as the Adlib Tracker II note the song plays (1 is C-0, MIDI
/// 12; notes below and above the 96 are held at the ends).
pub fn at2_note(midi: u8) -> u8 {
    midi.saturating_sub(11).clamp(1, 96)
}

/// The pan bits of register 0xC0 for a panning value (0 centre, 1 left, 2 right).
fn pan_bits(panning: u8) -> u8 {
    match panning {
        1 => 0x10,
        2 => 0x20,
        _ => 0x30,
    }
}

/// Register offsets of channel `ch`: (channel, modulator operator).
fn offsets(ch: usize) -> (u16, u16) {
    let bank = ((ch / 9) as u16) << 8;
    (bank | (ch % 9) as u16, bank | OP_OFFSET[ch % 9])
}

#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    note: u8,
    keyed: bool,
    /// The frequency written (block and F-number), for the key-off.
    freq: u16,
    velocity: f32,
    /// Order of the note-ons, for stealing the oldest.
    age: u32,
    /// Output frames until the note lets go by itself (0 holds it).
    frames_left: u32,
}

/// A polyphonic OPL instrument voice. See the module docs.
pub struct OplAudition {
    chip: Chip,
    resampler: Resampler,
    patch: FmPatch,
    slots: [Slot; CHANNELS],
    voices: usize,
    clock: u32,
    /// Slots whose `frames_left` is counting down.
    timed: usize,
    gain: f32,
    /// 0xBD: the tremolo and vibrato depths.
    depths: u8,
    /// The next writes, one per native sample (about 20 µs apart): a key-off
    /// and a key-on in the same instant would not restart the envelope.
    pending: [(u16, u8); PENDING],
    pending_len: usize,
    pending_next: usize,
    /// The loudest frame of the last `render`.
    peak: f32,
}

impl OplAudition {
    /// An audition of `patch` at `sample_rate`, with 9 voices.
    pub fn new(sample_rate: f64, patch: FmPatch) -> OplAudition {
        OplAudition::with_voices(sample_rate, patch, 9)
    }

    /// An audition with `voices` (1-18) notes at once.
    pub fn with_voices(sample_rate: f64, patch: FmPatch, voices: usize) -> OplAudition {
        let mut chip = Chip::new();
        // What the song player's reset leaves: waveform select on, the
        // keyboard split style, OPL3 mode, no 4-op pairs.
        chip.write(0x001, 0x20);
        chip.write(0x008, 0x40);
        chip.write(0x105, 0x01);
        chip.write(0x104, 0x00);
        chip.write(0x0bd, 0x00);
        OplAudition {
            chip,
            resampler: Resampler::new(NATIVE_RATE, sample_rate),
            patch,
            slots: [Slot::default(); CHANNELS],
            voices: voices.clamp(1, CHANNELS),
            clock: 0,
            timed: 0,
            gain: 1.0,
            depths: 0,
            pending: [(0, 0); PENDING],
            pending_len: 0,
            pending_next: 0,
            peak: 0.0,
        }
    }

    pub fn set_gain(&mut self, gain: f32) {
        self.gain = gain;
    }

    /// The tremolo and vibrato depths the song's flags give (bit 3 and 4 of
    /// its flags byte): deep is 4.8 dB and 14 cents, shallow 1 dB and 7.
    pub fn set_depths(&mut self, deep_tremolo: bool, deep_vibrato: bool) {
        self.depths = (deep_tremolo as u8) << 7 | (deep_vibrato as u8) << 6;
        self.queue(0x0bd, self.depths);
    }

    /// The instrument that plays.
    pub fn patch(&self) -> &FmPatch {
        &self.patch
    }

    /// Notes sounding (keyed on).
    pub fn active_notes(&self) -> usize {
        self.slots[..self.voices].iter().filter(|s| s.keyed).count()
    }

    /// The loudest output frame of the last [`render`](Self::render).
    pub fn peak(&self) -> f32 {
        self.peak
    }

    fn queue(&mut self, reg: u16, val: u8) {
        if self.pending_len == PENDING {
            // More than the queue holds: apply what is waiting at once.
            self.flush();
        }
        self.pending[self.pending_len] = (reg, val);
        self.pending_len += 1;
    }

    fn flush(&mut self) {
        for &(reg, val) in &self.pending[self.pending_next..self.pending_len] {
            self.chip.write(reg, val);
        }
        self.pending_len = 0;
        self.pending_next = 0;
    }

    /// The total-level bytes for the carrier and modulator at `velocity`.
    fn levels(patch: &FmPatch, velocity: f32) -> (u8, u8) {
        let fm = &patch.fm;
        let v = velocity.clamp(0.0, 1.0);
        let scale = |reg: u8| {
            let level = (63 - (reg & 0x3f)) as f32;
            (reg & 0xc0) | (63 - (level * v).round() as u8)
        };
        let additive = fm[10] & 1 != 0;
        let (m, c) = (if additive { scale(fm[2]) } else { fm[2] }, scale(fm[3]));
        // Adlib Tracker II's "OPL3 emulation workaround": an empty envelope
        // would freeze where the last note left it, so both levels go to 63.
        if fm[4..8].iter().all(|&b| b == 0) {
            return ((m & 0xc0) | 0x3f, (c & 0xc0) | 0x3f);
        }
        (m, c)
    }

    /// Queues the operator registers of channel `ch` for the patch at
    /// `velocity` (everything but the pitch and the key).
    fn load(&mut self, ch: usize, velocity: f32) {
        let (chan, m) = offsets(ch);
        let c = m + 3;
        let fm = self.patch.fm;
        let (tl_m, tl_c) = Self::levels(&self.patch, velocity);
        let panning = self.patch.panning;
        self.queue(0x20 + m, fm[0]);
        self.queue(0x20 + c, fm[1]);
        self.queue(0x40 + m, tl_m);
        self.queue(0x40 + c, tl_c);
        self.queue(0x60 + m, fm[4]);
        self.queue(0x60 + c, fm[5]);
        self.queue(0x80 + m, fm[6]);
        self.queue(0x80 + c, fm[7]);
        self.queue(0xe0 + m, fm[8]);
        self.queue(0xe0 + c, fm[9]);
        self.queue(0xc0 + chan, fm[10] | pan_bits(panning));
    }

    /// Which channel a new note plays on: the one already playing it, else
    /// the longest idle, else the oldest note.
    fn pick(&self, note: u8) -> usize {
        let slots = &self.slots[..self.voices];
        if let Some(i) = slots.iter().position(|s| s.keyed && s.note == note) {
            return i;
        }
        let oldest = |keyed: bool| {
            slots
                .iter()
                .enumerate()
                .filter(|(_, s)| s.keyed == keyed)
                .min_by_key(|(_, s)| s.age)
                .map(|(i, _)| i)
        };
        oldest(false).or_else(|| oldest(true)).unwrap_or(0)
    }

    /// Starts MIDI note `note` (69 is A4) at `velocity` (0-1); with `duration`
    /// output frames it lets go by itself after that long (0 holds it until
    /// [`note_off`](Self::note_off)). A note already sounding is struck again.
    pub fn note_on(&mut self, note: u8, velocity: f32, duration: u32) {
        let ch = self.pick(note);
        let (chan, _) = offsets(ch);
        let was = self.slots[ch];
        if was.keyed {
            self.queue(0xb0 + chan, 0);
            if was.frames_left > 0 {
                self.timed -= 1;
            }
        }
        self.load(ch, velocity);
        let freq = note_frequency(at2_note(note), self.patch.finetune);
        self.queue(0xa0 + chan, freq as u8);
        self.queue(0xb0 + chan, 0x20 | (freq >> 8) as u8 & 0x1f);
        self.clock = self.clock.wrapping_add(1);
        self.slots[ch] = Slot {
            note,
            keyed: true,
            freq,
            velocity,
            age: self.clock,
            frames_left: duration,
        };
        if duration > 0 {
            self.timed += 1;
        }
    }

    fn release(&mut self, ch: usize) {
        let (chan, _) = offsets(ch);
        let slot = &mut self.slots[ch];
        slot.keyed = false;
        if slot.frames_left > 0 {
            slot.frames_left = 0;
            self.timed -= 1;
        }
        let freq = slot.freq;
        self.queue(0xb0 + chan, (freq >> 8) as u8 & 0x1f);
    }

    /// Lets go of every voice playing `note`; the release plays out.
    pub fn note_off(&mut self, note: u8) {
        for ch in 0..self.voices {
            if self.slots[ch].keyed && self.slots[ch].note == note {
                self.release(ch);
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        for ch in 0..self.voices {
            if self.slots[ch].keyed {
                self.release(ch);
            }
        }
    }

    /// Changes the instrument. The sounding notes take it at once (their
    /// operator registers are rewritten, at the velocity they were struck);
    /// later notes use it.
    pub fn set_patch(&mut self, patch: FmPatch) {
        if patch == self.patch {
            return;
        }
        self.patch = patch;
        for ch in 0..self.voices {
            if self.slots[ch].keyed {
                let velocity = self.slots[ch].velocity;
                self.load(ch, velocity);
            }
        }
    }

    fn step_timers(&mut self) {
        for ch in 0..self.voices {
            let slot = &mut self.slots[ch];
            if slot.frames_left > 0 {
                slot.frames_left -= 1;
                if slot.frames_left == 0 {
                    self.timed -= 1;
                    self.release(ch);
                }
            }
        }
    }

    /// Renders the next `left.len()` frames over `left` and `right` (the
    /// shorter of the two decides).
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        let mut peak = 0f32;
        for i in 0..n {
            if self.timed > 0 {
                self.step_timers();
            }
            let (chip, pending, len, next) = (
                &mut self.chip,
                &self.pending,
                &mut self.pending_len,
                &mut self.pending_next,
            );
            let [l, r] = self.resampler.next(|| {
                if *next < *len {
                    let (reg, val) = pending[*next];
                    chip.write(reg, val);
                    *next += 1;
                    if *next == *len {
                        *len = 0;
                        *next = 0;
                    }
                }
                let (l, r) = chip.clock_sample();
                [l as f32, r as f32]
            });
            let (l, r) = (l * FULL_SCALE * self.gain, r * FULL_SCALE * self.gain);
            left[i] = l;
            right[i] = r;
            peak = peak.max(l.abs()).max(r.abs());
        }
        self.peak = peak;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn fnv(samples: &[f32]) -> u64 {
        let mut h = 0xcbf29ce484222325u64;
        for s in samples {
            for b in s.to_bits().to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
        }
        h
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// Renders `frames` in 128-frame blocks and returns left then right.
    fn run(a: &mut OplAudition, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (vec![0f32; frames], vec![0f32; frames]);
        for (lc, rc) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
            a.render(lc, rc);
        }
        (l, r)
    }

    fn strike(patch: FmPatch, midi: u8, velocity: f32, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut a = OplAudition::new(RATE, patch);
        a.note_on(midi, velocity, 0);
        run(&mut a, frames)
    }

    /// Frequency of the strongest of the semitone-spaced bins around `hz`.
    fn level_at(signal: &[f32], hz: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * hz / RATE;
        let (mut s, mut c) = (0.0, 0.0);
        for (n, &x) in signal.iter().enumerate() {
            s += x as f64 * (w * n as f64).sin();
            c += x as f64 * (w * n as f64).cos();
        }
        2.0 * (s * s + c * c).sqrt() / signal.len() as f64
    }

    #[test]
    fn at2_notes_follow_midi() {
        assert_eq!(at2_note(12), 1);
        assert_eq!(at2_note(60), 49);
        assert_eq!(at2_note(107), 96);
        assert_eq!(at2_note(0), 1);
        assert_eq!(at2_note(127), 96);
    }

    #[test]
    fn a_held_note_sounds_at_its_pitch_and_dies_away_after_note_off() {
        // A2M's tuning puts note 49 (C-4, MIDI 60) at block 4, F-number 0x157: ~261.6 Hz.
        let mut a = OplAudition::new(RATE, FmPatch::plain());
        a.note_on(60, 1.0, 0);
        let (l, _) = run(&mut a, 24_000);
        let tail = &l[12_000..];
        assert!(rms(tail) > 0.01, "a held note sounds: {}", rms(tail));
        let freq = 0x157 as f64 * NATIVE_RATE / (1u64 << (20 - 4)) as f64;
        let at = level_at(tail, freq);
        let off = level_at(tail, freq * 1.5);
        assert!(at > 20.0 * off, "pitch {freq:.1} Hz: {at} vs {off}");
        assert_eq!(a.active_notes(), 1);
        a.note_off(60);
        assert_eq!(a.active_notes(), 0);
        let (l, _) = run(&mut a, 96_000);
        assert!(rms(&l[..2400]) > rms(&l[90_000..]), "the release dies away");
        assert!(rms(&l[90_000..]) < 1e-3);
    }

    #[test]
    fn velocity_scales_the_carrier() {
        let loud = rms(&strike(FmPatch::plain(), 60, 1.0, 12_000).0[4_000..]);
        let soft = rms(&strike(FmPatch::plain(), 60, 0.25, 12_000).0[4_000..]);
        assert!(loud > 2.0 * soft, "{loud} vs {soft}");
        assert!(soft > 0.0);
        let none = strike(FmPatch::plain(), 60, 0.0, 12_000).0;
        assert!(
            rms(&none[4_000..]) < 0.01 * loud,
            "velocity 0 is nearly silent"
        );
    }

    #[test]
    fn panning_goes_to_one_side() {
        let mut p = FmPatch::plain();
        p.panning = 1;
        let (l, r) = strike(p, 60, 1.0, 12_000);
        assert!(rms(&l[4_000..]) > 0.01 && rms(&r[4_000..]) < 1e-4, "left");
        p.panning = 2;
        let (l, r) = strike(p, 60, 1.0, 12_000);
        assert!(rms(&r[4_000..]) > 0.01 && rms(&l[4_000..]) < 1e-4, "right");
        p.panning = 0;
        let (l, r) = strike(p, 60, 1.0, 12_000);
        assert!(rms(&l[4_000..]) > 0.01 && rms(&r[4_000..]) > 0.01, "centre");
    }

    #[test]
    fn a_duration_lets_go_by_itself() {
        let mut a = OplAudition::new(RATE, FmPatch::plain());
        a.note_on(60, 1.0, 4_800);
        assert_eq!(a.active_notes(), 1);
        run(&mut a, 4_700);
        assert_eq!(a.active_notes(), 1);
        run(&mut a, 200);
        assert_eq!(a.active_notes(), 0);
        let (l, _) = run(&mut a, 96_000);
        assert!(rms(&l[90_000..]) < 1e-3, "and the release plays out");
    }

    #[test]
    fn notes_play_together_and_the_oldest_is_stolen() {
        let mut a = OplAudition::with_voices(RATE, FmPatch::plain(), 3);
        for n in [60, 64, 67] {
            a.note_on(n, 1.0, 0);
        }
        assert_eq!(a.active_notes(), 3);
        run(&mut a, 2_000);
        a.note_on(72, 1.0, 0);
        assert_eq!(a.active_notes(), 3, "the fourth note takes a voice");
        assert!(
            !a.slots[..3].iter().any(|s| s.note == 60),
            "the oldest went"
        );
        // Striking a sounding note again uses its own voice.
        a.note_on(64, 1.0, 0);
        assert_eq!(a.slots[..3].iter().filter(|s| s.note == 64).count(), 1);
        a.all_notes_off();
        assert_eq!(a.active_notes(), 0);
    }

    #[test]
    fn a_patch_edit_is_heard_while_the_note_is_held() {
        // Switching the carrier's waveform from sine to square changes the sound at once.
        let mut a = OplAudition::new(RATE, FmPatch::plain());
        a.note_on(60, 1.0, 0);
        let (before, _) = run(&mut a, 12_000);
        let mut p = *a.patch();
        p.fm[9] = 6;
        a.set_patch(p);
        run(&mut a, 2_400);
        let (after, _) = run(&mut a, 12_000);
        let f = 0x157 as f64 * NATIVE_RATE / (1u64 << 16) as f64;
        let third = |x: &[f32]| level_at(x, 3.0 * f) / level_at(x, f);
        assert!(
            third(&after[2_000..]) > 10.0 * third(&before[2_000..]),
            "a square wave has a third harmonic: {} vs {}",
            third(&after[2_000..]),
            third(&before[2_000..])
        );
        assert_eq!(a.active_notes(), 1, "the note went on");
    }

    #[test]
    fn instruments_differ_in_sound() {
        let fm1 = [
            0x21, 0x21, 0x3f, 0x00, 0xf2, 0xf4, 0x24, 0x27, 0x00, 0x00, 0x00,
        ];
        let fm2 = [
            0x01, 0x32, 0x1d, 0x08, 0xa3, 0xc5, 0x4b, 0x66, 0x02, 0x05, 0x0d,
        ];
        let a = strike(FmPatch::new(fm1), 60, 1.0, 6_000).0;
        let b = strike(FmPatch::new(fm2), 60, 1.0, 6_000).0;
        assert_ne!(fnv(&a), fnv(&b));
        assert!(rms(&b[1_000..]) > 0.0);
    }

    #[test]
    fn a_second_audition_with_the_same_notes_renders_the_same_samples() {
        let render = || {
            let mut a = OplAudition::new(44_100.0, FmPatch::plain());
            a.note_on(60, 0.8, 0);
            let (l1, _) = run(&mut a, 5_000);
            a.note_on(67, 1.0, 3_000);
            a.note_off(60);
            let (l2, r2) = run(&mut a, 9_000);
            fnv(&[l1, l2, r2].concat())
        };
        assert_eq!(render(), render());
    }

    /// Golden hashes of three notes of two instruments: a change to the chip,
    /// the note table, the velocity law or the register order shows here.
    #[test]
    fn golden_renders() {
        let fm_bell = [
            0x01, 0x32, 0x1d, 0x08, 0xa3, 0xc5, 0x4b, 0x66, 0x02, 0x05, 0x0d,
        ];
        let mut bell = FmPatch::new(fm_bell);
        bell.finetune = 3;
        let cases = [
            ("plain C-4", FmPatch::plain(), 60u8, 1.0f32),
            ("plain A-3 soft", FmPatch::plain(), 57, 0.5),
            ("bell C-5", bell, 72, 1.0),
        ];
        let mut got = [0u64; 3];
        for (i, (name, patch, note, velocity)) in cases.into_iter().enumerate() {
            let (l, r) = strike(patch, note, velocity, 8_192);
            got[i] = fnv(&[l, r].concat());
            println!("{name}: {:#018x}", got[i]);
        }
        assert_eq!(got, GOLDEN);
    }

    const GOLDEN: [u64; 3] = [0x2ca8f29bfc504a31, 0x1f822c49e5072065, 0x594b11a3df2c2139];
}
