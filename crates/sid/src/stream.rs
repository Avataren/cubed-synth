//! `SidChipPlayer`: a bare SID chip fed by a host that already knows what to
//! write and when, the wasm-facing class behind playing a C64 `.sid` as it is
//! (`.ai/plan-psid-playback.md`).
//!
//! `SidPlayer` (`wasm.rs`) plays the app's own song model: it owns a
//! sequencer. A `.sid` file has none to own: it is 6502 code, which the
//! worklet runs on an emulated C64 (`src/audio/tracker/psid/psid-runner.ts`),
//! and every write that code makes to the SID reaches this class with the
//! cycle it was made on. So this is the chip and the transport state a
//! worklet needs (a gain, the voice mask) and nothing that decides a sound.
//!
//! Timing: `write_after(delay, ..)` lands `delay` chip cycles after the
//! start of the next `render`, in scheduling order for equal cycles, exactly
//! as `Chip::write_after` does for the song player. The host schedules one
//! render quantum's writes before each `render`, so the chip never holds
//! more than a quantum of them.

use super::chip::{Chip, ALL_VOICES};
use super::revision::DieRevision;
use super::SidModel;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct SidChipPlayer {
    chip: Chip,
    gain: f32,
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
impl SidChipPlayer {
    /// A powered-on chip: an 8580 when `model_8580`, else a 6581, running at
    /// `clock_hz` chip cycles per second (985 248 PAL, 1 022 727 NTSC) and
    /// rendering at `sample_rate`.
    #[cfg_attr(feature = "wasm", wasm_bindgen(constructor))]
    pub fn new(model_8580: bool, sample_rate: f64, clock_hz: f64) -> Result<SidChipPlayer, String> {
        let model = if model_8580 {
            SidModel::Sid8580
        } else {
            SidModel::Sid6581
        };
        let mut chip = Chip::with_sample_rate(model, sample_rate).map_err(|e| e.to_string())?;
        chip.set_clock_hz(clock_hz);
        Ok(SidChipPlayer { chip, gain: 1.0 })
    }

    /// Write register `reg` (0-31) `delay` chip cycles after the start of the next `render`.
    pub fn write_after(&mut self, delay: u32, reg: u8, value: u8) {
        self.chip.write_after(delay as u64, reg, value);
    }

    /// Write register `reg` now.
    pub fn write(&mut self, reg: u8, value: u8) {
        self.chip.write(reg, value);
    }

    pub fn set_gain(&mut self, gain: f32) {
        self.gain = if gain.is_finite() { gain.max(0.0) } else { 1.0 };
    }

    /// Bit masks, bit `i` = voice `i`: muted voices, and (when non-zero) the
    /// only voices heard. Same rule as `SidPlayer::set_mute_solo`.
    pub fn set_mute_solo(&mut self, mute: u32, solo: u32) {
        let heard = if solo & ALL_VOICES as u32 != 0 {
            solo
        } else {
            ALL_VOICES as u32
        };
        self.chip.set_voice_mask((heard & !mute) as u8);
    }

    /// Switch to the 8580 (`true`) or the 6581 from the next sample on; the
    /// tune's notes keep sounding.
    pub fn set_chip_model(&mut self, model_8580: bool) {
        let model = if model_8580 {
            SidModel::Sid8580
        } else {
            SidModel::Sid6581
        };
        let _ = self.chip.set_model(model);
    }

    /// Play the 6581 as revision `name` (`DieRevision::name`); `false` for an unknown name.
    pub fn set_revision(&mut self, name: &str) -> bool {
        match DieRevision::from_name(name) {
            Some(r) => {
                self.chip.set_profile(r.profile());
                true
            }
            None => false,
        }
    }

    /// Fills `out` with the mix and `v0`..`v2` with the three voices' taps.
    /// Every buffer must be as long as `out`. Returns the frames written.
    pub fn render(
        &mut self,
        out: &mut [f32],
        v0: &mut [f32],
        v1: &mut [f32],
        v2: &mut [f32],
    ) -> usize {
        let n = out.len().min(v0.len()).min(v1.len()).min(v2.len());
        self.chip
            .render_taps(&mut out[..n], [&mut v0[..n], &mut v1[..n], &mut v2[..n]]);
        if self.gain != 1.0 {
            for buffer in [&mut out[..n], &mut v0[..n], &mut v1[..n], &mut v2[..n]] {
                for s in buffer.iter_mut() {
                    *s *= self.gain;
                }
            }
        }
        n
    }

    /// `"8580"` or `"6581"`.
    pub fn chip_model(&self) -> String {
        match self.chip.model() {
            SidModel::Sid8580 => "8580".to_string(),
            SidModel::Sid6581 => "6581".to_string(),
        }
    }

    /// A voice tap's full scale (`Chip::tap_full_scale`) at gain 1.0.
    pub fn tap_full_scale(&self) -> f64 {
        self.chip.tap_full_scale()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(p: &mut SidChipPlayer, n: usize) -> (Vec<f32>, [Vec<f32>; 3]) {
        let mut out = vec![0.0; n];
        let mut t = [vec![0.0; n], vec![0.0; n], vec![0.0; n]];
        let [a, b, c] = &mut t;
        p.render(&mut out, a, b, c);
        (out, t)
    }

    /// Voice 1 sawtooth at full volume, gate on.
    fn play_saw(p: &mut SidChipPlayer, delay: u32) {
        p.write_after(delay, 0x18, 0x0f);
        p.write_after(delay, 0x01, 0x20);
        p.write_after(delay, 0x05, 0x00);
        p.write_after(delay, 0x06, 0xf0);
        p.write_after(delay, 0x04, 0x21);
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    #[test]
    fn scheduled_writes_sound_only_voice_one() {
        let mut p = SidChipPlayer::new(true, 44_100.0, 985_248.0).unwrap();
        play_saw(&mut p, 0);
        let (out, taps) = render(&mut p, 4096);
        assert!(rms(&out[1024..]) > 0.01, "the mix is silent");
        assert!(rms(&taps[0][1024..]) > 0.01, "voice 1's tap is silent");
        assert!(
            rms(&taps[1]) < 1e-6 && rms(&taps[2]) < 1e-6,
            "voices 2 and 3 should be silent"
        );
    }

    #[test]
    fn a_write_waits_for_its_cycle() {
        let mut p = SidChipPlayer::new(true, 44_100.0, 985_248.0).unwrap();
        // 20 000 cycles is ~890 samples at 44.1 kHz.
        play_saw(&mut p, 20_000);
        let (out, _) = render(&mut p, 4096);
        assert!(rms(&out[..800]) < 1e-6, "sounded before its write");
        assert!(rms(&out[1500..]) > 0.01, "silent after its write");
    }

    #[test]
    fn mute_silences_a_voice_and_its_tap() {
        let mut p = SidChipPlayer::new(true, 44_100.0, 985_248.0).unwrap();
        play_saw(&mut p, 0);
        p.set_mute_solo(1, 0);
        let (out, taps) = render(&mut p, 4096);
        assert!(rms(&out) < 1e-6 && rms(&taps[0]) < 1e-6);
    }

    #[test]
    fn ntsc_clock_raises_the_pitch() {
        // The same register value is a higher note on the faster clock: count rising zero crossings.
        fn crossings(clock: f64) -> usize {
            let mut p = SidChipPlayer::new(true, 44_100.0, clock).unwrap();
            play_saw(&mut p, 0);
            let (out, _) = render(&mut p, 44_100);
            out.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count()
        }
        let pal = crossings(985_248.0) as f64;
        let ntsc = crossings(1_022_727.0) as f64;
        let ratio = ntsc / pal;
        assert!(
            (ratio - 1_022_727.0 / 985_248.0).abs() < 0.01,
            "ratio {ratio}"
        );
    }

    #[test]
    fn an_unknown_revision_changes_nothing() {
        let mut p = SidChipPlayer::new(false, 44_100.0, 985_248.0).unwrap();
        assert!(!p.set_revision("nope"));
        assert!(p.set_revision("r3"));
        assert_eq!(p.chip_model(), "6581");
    }
}
