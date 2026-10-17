//! `OplRenderer`: the chip as the app's OPL worklet (`src/audio/worklets/
//! opl-core.ts`) drives it. It is a register-stream device, not a song
//! player: whoever plays the music (the S3M engine in the tracker-playback
//! library, later an A2M player) sends register writes stamped with the
//! output frame they belong to, and this renders.
//!
//! Timing: the chip runs at its native ≈ 49 716 Hz behind the resampler
//! (plan D5). A write stamped for output frame `f` is applied before native
//! sample `ceil(f × native / out)`, inside the resampler's pull, so it lands
//! within one native sample (≈ 20 µs) of its time rather than on a render
//! quantum or output-frame boundary. A write for a frame already rendered
//! applies at once and is counted (`late_writes`).
//!
//! The wasm-bindgen attributes are `cfg_attr`-gated as in `sid/wasm.rs`, so
//! the class also compiles and is tested natively.

use super::chip::{Chip, CHANNELS, NATIVE_RATE};
use super::resample::Resampler;
use std::collections::VecDeque;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

/// One queued write, at a native sample index.
#[derive(Clone, Copy, Debug)]
struct Write {
    at: u64,
    reg: u16,
    val: u8,
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct OplRenderer {
    chip: Chip,
    resampler: Resampler,
    out_rate: f64,
    /// Native samples produced so far (the next one's index).
    native_pos: u64,
    /// Output frames rendered so far.
    frames: u64,
    queue: VecDeque<Write>,
    late: u32,
    gain: f32,
    /// Per-channel scope taps of the last `render`, channel-major
    /// (`TAP_CHANNELS` × frames), when enabled.
    taps_enabled: bool,
    taps: Vec<f32>,
    tap_frames: usize,
}

/// One operator's full swing (the chip's signed 14-bit operator output):
/// a tap reads ±1 for a full-level carrier.
const TAP_SCALE: f32 = 1.0 / 8192.0;

/// Full scale of the chip's 16-bit output.
const FULL_SCALE: f32 = 1.0 / 32768.0;

#[cfg_attr(feature = "wasm", wasm_bindgen)]
impl OplRenderer {
    #[cfg_attr(feature = "wasm", wasm_bindgen(constructor))]
    pub fn new(sample_rate: f64) -> OplRenderer {
        OplRenderer {
            chip: Chip::new(),
            resampler: Resampler::new(NATIVE_RATE, sample_rate),
            out_rate: sample_rate,
            native_pos: 0,
            frames: 0,
            queue: VecDeque::new(),
            late: 0,
            gain: 1.0,
            taps_enabled: false,
            taps: Vec::new(),
            tap_frames: 0,
        }
    }

    /// Write a register now: before the next native sample.
    pub fn write(&mut self, reg: u16, val: u8) {
        self.chip.write(reg, val);
    }

    /// Queue a write for output frame `frame` (this renderer's own clock,
    /// `frames_rendered`; fractional frames are honoured). Writes for the
    /// same frame keep their order.
    pub fn write_at(&mut self, frame: f64, reg: u16, val: u8) {
        let at = (frame.max(0.0) * NATIVE_RATE / self.out_rate).ceil() as u64;
        if at <= self.native_pos && self.queue.is_empty() {
            if at < self.native_pos {
                self.late = self.late.saturating_add(1);
            }
            self.chip.write(reg, val);
            return;
        }
        let w = Write { at, reg, val };
        // Writes arrive nearly sorted: search back from the end.
        let mut i = self.queue.len();
        while i > 0 && self.queue[i - 1].at > at {
            i -= 1;
        }
        self.queue.insert(i, w);
    }

    /// Render `left.len()` frames (and the same into `right`). Returns frames.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        let n = left.len().min(right.len());
        let taps_on = self.taps_enabled;
        if taps_on {
            self.taps.clear();
            self.taps.resize(CHANNELS * n, 0.0);
        }
        self.tap_frames = if taps_on { n } else { 0 };
        let (chip, queue, native_pos, late) = (
            &mut self.chip,
            &mut self.queue,
            &mut self.native_pos,
            &mut self.late,
        );
        for i in 0..n {
            let [l, r] = self.resampler.next(|| {
                while let Some(w) = queue.front() {
                    if w.at > *native_pos {
                        break;
                    }
                    if w.at < *native_pos {
                        *late = late.saturating_add(1);
                    }
                    chip.write(w.reg, w.val);
                    queue.pop_front();
                }
                *native_pos += 1;
                let (l, r) = chip.clock_sample();
                [l as f32, r as f32]
            });
            left[i] = l * FULL_SCALE * self.gain;
            right[i] = r * FULL_SCALE * self.gain;
            // Scopes: the latest native sample per output frame. No
            // anti-aliasing: a picture, not audio.
            if taps_on {
                for ch in 0..CHANNELS {
                    self.taps[ch * n + i] = chip.channel_output(ch) as f32 * TAP_SCALE;
                }
            }
        }
        self.frames += n as u64;
        n
    }

    /// Output frames rendered so far: the clock `write_at` frames are on.
    pub fn frames_rendered(&self) -> f64 {
        self.frames as f64
    }

    /// Writes applied after their time (a late batch from the main thread).
    pub fn late_writes(&self) -> u32 {
        self.late
    }

    pub fn queued_writes(&self) -> u32 {
        self.queue.len() as u32
    }

    pub fn set_gain(&mut self, gain: f32) {
        self.gain = gain;
    }

    /// Bit per channel (0..17): 1 plays, 0 mutes. The chip keeps running.
    pub fn set_channel_mask(&mut self, mask: u32) {
        self.chip.set_channel_mask(mask);
    }

    /// Drop queued writes and silence the chip: every channel keyed off with
    /// its release forced to the fastest rate, rhythm off. Registers other
    /// than those keep their values (the next song re-programs them).
    pub fn panic(&mut self) {
        self.queue.clear();
        for bank in [0x000u16, 0x100] {
            for op in 0..0x16u16 {
                if op & 7 < 6 {
                    let sr = self.chip.written(bank + 0x80 + op);
                    self.chip.write(bank + 0x80 + op, sr | 0x0f);
                }
            }
            for ch in 0..9u16 {
                let b0 = self.chip.written(bank + 0xb0 + ch);
                self.chip.write(bank + 0xb0 + ch, b0 & !0x20);
            }
        }
        self.chip.write(0xbd, self.chip.written(0xbd) & !0x3f);
    }

    /// Record per-channel scope taps during `render` (off by default: it is
    /// work nobody needs without a scope on screen).
    pub fn set_taps_enabled(&mut self, enabled: bool) {
        self.taps_enabled = enabled;
        if !enabled {
            self.taps = Vec::new();
            self.tap_frames = 0;
        }
    }

    /// How many channels `read_tap` covers (18: every OPL3 channel).
    pub fn tap_channels(&self) -> u32 {
        CHANNELS as u32
    }

    /// Copy channel `ch`'s tap from the last `render` into `out` (its
    /// frames; ±1 is one operator's full swing). Zeros when taps are off.
    pub fn read_tap(&self, ch: usize, out: &mut [f32]) {
        let n = out.len().min(self.tap_frames);
        if ch >= CHANNELS || n == 0 {
            out.fill(0.0);
            return;
        }
        out[..n].copy_from_slice(&self.taps[ch * self.tap_frames..ch * self.tap_frames + n]);
        out[n..].fill(0.0);
    }

    /// The chip's native rate, for callers converting tick timing.
    pub fn native_rate(&self) -> f64 {
        NATIVE_RATE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Channel 0: a sine carrier, instant attack, held (EG type sustain),
    /// fastest release.
    fn program_sine(r: &mut OplRenderer) {
        for (reg, val) in [
            (0x20, 0x01),
            (0x23, 0x21),
            (0x40, 0x3f),
            (0x43, 0x00),
            (0x60, 0xf0),
            (0x63, 0xf0),
            (0x80, 0x00),
            (0x83, 0x0f),
            (0xa0, 0x44),
        ] {
            r.write(reg, val);
        }
    }

    fn render(r: &mut OplRenderer, frames: usize) -> Vec<f32> {
        let (mut l, mut rr) = (vec![0f32; frames], vec![0f32; frames]);
        for (lc, rc) in l.chunks_mut(128).zip(rr.chunks_mut(128)) {
            r.render(lc, rc);
        }
        l
    }

    fn first_sound(x: &[f32]) -> Option<usize> {
        x.iter().position(|s| s.abs() > 1e-3)
    }

    #[test]
    fn taps_carry_each_channel_alone_and_follow_the_mask() {
        let mut r = OplRenderer::new(48_000.0);
        program_sine(&mut r);
        r.write(0xb0, 0x31); // key on, channel 0
        let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
        let mut tap = vec![0f32; 128];

        // Off by default: nothing recorded.
        r.render(&mut l, &mut rr);
        r.read_tap(0, &mut tap);
        assert!(tap.iter().all(|&v| v == 0.0));

        r.set_taps_enabled(true);
        r.render(&mut l, &mut rr);
        r.read_tap(0, &mut tap);
        let peak = tap.iter().fold(0f32, |m, v| m.max(v.abs()));
        // A full-level sine carrier swings to about ±1 of one operator.
        assert!(peak > 0.9 && peak <= 1.0, "peak {peak}");
        r.read_tap(1, &mut tap);
        assert!(tap.iter().all(|&v| v == 0.0), "channel 1 is silent");

        // Muted: the tap goes flat with the audio.
        r.set_channel_mask(!1);
        r.render(&mut l, &mut rr);
        r.read_tap(0, &mut tap);
        assert!(tap.iter().skip(1).all(|&v| v == 0.0));
        assert_eq!(r.tap_channels(), 18);
    }

    #[test]
    fn a_stamped_key_on_sounds_at_its_frame_regardless_of_quantum() {
        // Stamps on, off and between 128-frame quantum boundaries, fractional
        // too: each onset must follow its stamp by the same delay (the
        // resampler's group delay, half its taps at native rate ≈ 0.7 ms).
        let mut delays = Vec::new();
        for key_frame in [1000.0, 1000.5, 1023.9, 1024.0, 1063.0, 1100.25] {
            let mut r = OplRenderer::new(48_000.0);
            program_sine(&mut r);
            r.write_at(key_frame, 0xb0, 0x32);
            let onset = first_sound(&render(&mut r, 4096)).unwrap() as f64;
            delays.push(onset - key_frame);
        }
        let (lo, hi) = delays
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), &d| (lo.min(d), hi.max(d)));
        // One frame for detecting the onset on integer frames, one for the
        // write landing on a native-sample boundary.
        assert!(
            hi - lo <= 2.0,
            "onset delays vary with the stamp: {delays:?}"
        );
        assert!(
            lo > 25.0 && hi < 40.0,
            "group delay out of range: {delays:?}"
        );
    }

    #[test]
    fn output_does_not_depend_on_how_rendering_is_chunked() {
        let run = |chunk: usize| {
            let mut r = OplRenderer::new(44_100.0);
            program_sine(&mut r);
            for (i, frame) in [300.0, 1210.7, 2047.0, 2048.0, 3333.3]
                .into_iter()
                .enumerate()
            {
                r.write_at(frame, 0xa0, 0x40 + i as u8 * 16);
                r.write_at(frame, 0xb0, if i % 2 == 0 { 0x32 } else { 0x12 });
            }
            let (mut l, mut rr) = (vec![0f32; 5000], vec![0f32; 5000]);
            for (lc, rc) in l.chunks_mut(chunk).zip(rr.chunks_mut(chunk)) {
                r.render(lc, rc);
            }
            l
        };
        let reference = run(128);
        for chunk in [37, 1000, 1] {
            assert_eq!(run(chunk), reference, "chunk {chunk} differs from 128");
        }
    }

    #[test]
    fn writes_in_the_past_apply_at_once_and_are_counted() {
        let mut r = OplRenderer::new(44_100.0);
        program_sine(&mut r);
        render(&mut r, 512);
        r.write_at(100.0, 0xb0, 0x32);
        assert_eq!(r.late_writes(), 1);
        let out = render(&mut r, 512);
        assert!(first_sound(&out).unwrap() < 64);
    }

    #[test]
    fn out_of_order_writes_are_applied_in_time_order() {
        // Key off stamped before key on, but sent after it: the note must end.
        let mut r = OplRenderer::new(48_000.0);
        program_sine(&mut r);
        r.write(0x83, 0x0f);
        r.write_at(2000.0, 0xb0, 0x12);
        r.write_at(500.0, 0xb0, 0x32);
        assert_eq!(r.queued_writes(), 2);
        let out = render(&mut r, 6000);
        assert!(out[1000..1900].iter().any(|s| s.abs() > 0.05));
        assert!(
            out[5000..].iter().all(|s| s.abs() < 1e-3),
            "note never released"
        );
        assert_eq!(r.late_writes(), 0);
    }

    #[test]
    fn panic_silences_a_held_note() {
        let mut r = OplRenderer::new(48_000.0);
        program_sine(&mut r);
        r.write(0x83, 0x00);
        r.write(0xb0, 0x32);
        let before = render(&mut r, 2000);
        assert!(before.iter().any(|s| s.abs() > 0.05));
        r.panic();
        let after = render(&mut r, 6000);
        assert!(after[4000..].iter().all(|s| s.abs() < 1e-3));
    }
}
