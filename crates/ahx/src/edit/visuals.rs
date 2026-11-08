//! What an instrument's controls draw: its envelope, the waveform shapes, the
//! sweeps and the vibrato, simulated with the replayer's own code (the
//! envelope's `AdsrState`, the voices' sweeps, the wave tables and the vibrato
//! table), so a drawing cannot drift from what plays. another_synth's
//! `ahx-instrument-visuals.ts` and `ahx-envelope-sim.ts` are ports of the same.

use serde_json::{json, Value};

use super::limits::Res;
use super::AhxDoc;
use crate::envelope::AdsrState;
use crate::filter_sweep::FilterSweep;
use crate::voice::{SquareSweep, VIB_TAB};
use crate::waveform::{
    WAVELENGTH_OFFSETS, WAVES, WO_SAWTOOTH_04, WO_SQUARES, WO_TRIANGLE_04, WO_WHITENOISE,
};

/// The most frames an envelope simulation runs: attack, decay, sustain and
/// release are at most 255 each.
const ENVELOPE_FRAMES: usize = 4 * 255;

impl AhxDoc {
    /// The envelope of instrument `n` frame by frame, as the voice steps it from
    /// a trigger (one frame is one replay tick): `{"volumes": [the level 0-64
    /// after each tick, until the release has run out], "attack", "decay",
    /// "sustain", "release": [the frame each stage ends at]}`. A stage of 0
    /// frames is skipped as the engine skips it, which is why an envelope with
    /// no attack and no decay never rises.
    pub fn envelope_curve(&self, n: usize) -> Res<Value> {
        let ins = self.instrument_ref(n)?;
        let env = &ins.envelope;
        let mut state = AdsrState::trigger(env);
        let mut volumes = Vec::new();
        let mut ends = [0usize; 4];
        for frame in 0..ENVELOPE_FRAMES {
            let stage = if state.a_frames != 0 {
                0
            } else if state.d_frames != 0 {
                1
            } else if state.s_frames != 0 {
                2
            } else if state.r_frames != 0 {
                3
            } else {
                break;
            };
            state.step(env);
            ends[stage] = frame + 1;
            volumes.push(state.volume as f32 / 256.0);
        }
        // A stage that was skipped ends where the one before it did.
        for i in 1..4 {
            if ends[i] == 0 {
                ends[i] = ends[i - 1];
            }
        }
        Ok(json!({
            "volumes": volumes,
            "attack": ends[0], "decay": ends[1], "sustain": ends[2], "release": ends[3],
        }))
    }

    /// One cycle of a waveform as the replayer plays it before any filter:
    /// `waveform` 1 triangle, 2 sawtooth, 3 square (at `square_pos`, a position
    /// of a square sweep in the engine's units: the limits shifted right by `5 -
    /// wave_length`, so the pulse is widest, 50%, at `0x20 >> (5 - wave_length)`),
    /// 4 noise (the first burst of the noise table: the engine draws a new one
    /// every tick); `wave_length` 0 to 5 is a cycle of `4 << wave_length`
    /// samples. `{"waveform", "wave_length", "samples": [-128..127]}`.
    pub fn waveform_shape(
        &self,
        waveform: usize,
        wave_length: usize,
        square_pos: usize,
    ) -> Res<Value> {
        if !(1..=4).contains(&waveform) {
            return Err(format!(
                "A waveform is 1 (triangle) to 4 (noise), not {waveform}."
            ));
        }
        if wave_length > 5 {
            return Err(format!("A wave length is 0 to 5, not {wave_length}."));
        }
        let n = 4usize << wave_length;
        let at = |o: usize| WAVES[o..o + n].to_vec();
        let samples: Vec<i8> = match waveform {
            1 => at(WO_TRIANGLE_04 + WAVELENGTH_OFFSETS[wave_length]),
            2 => at(WO_SAWTOOTH_04 + WAVELENGTH_OFFSETS[wave_length]),
            3 => {
                // `calc_square` with the filter at its neutral position (32).
                let mut x = (square_pos.min(255) as i64) << (5 - wave_length);
                if x > 0x20 {
                    x = 0x40 - x;
                }
                let mut ptr = WO_SQUARES as i64;
                if x > 0 {
                    ptr += (x - 1) << 7;
                }
                let delta = (32 >> wave_length) as i64;
                (0..n)
                    .map(|i| WAVES[(ptr + i as i64 * delta) as usize])
                    .collect()
            }
            _ => at(WO_WHITENOISE),
        };
        Ok(json!({"waveform": waveform, "wave_length": wave_length, "samples": samples}))
    }

    /// The position a sweep of instrument `n` walks, per tick for `frames` ticks
    /// from the moment it is turned on heading up (`sign` -1 heads down):
    /// `{"kind", "positions": [..], "lower", "upper"}`. The filter's positions are
    /// 1 to 63 (it starts at 32), the square's are in the engine's units (the
    /// limits scaled by the wave length; `start` is where it begins).
    pub fn sweep_trace(
        &self,
        n: usize,
        kind: super::SweepKind,
        frames: usize,
        sign: i32,
        start: i32,
    ) -> Res<Value> {
        let ins = self.instrument_ref(n)?;
        let frames = frames.min(4096);
        let sign = if sign < 0 { -1 } else { 1 };
        let (positions, lower, upper): (Vec<i32>, i32, i32) = match kind {
            super::SweepKind::Filter => {
                let mut f = FilterSweep::trigger(ins);
                f.toggle(sign);
                let bounds = (f.lower_limit, f.upper_limit);
                let mut out = vec![f.pos];
                for _ in 0..frames {
                    f.step();
                    out.push(f.pos);
                }
                (out, bounds.0, bounds.1)
            }
            super::SweepKind::Square => {
                let wave_length = (ins.wave_length as i32).min(5);
                let mut sq = SquareSweep::default();
                sq.trigger(ins, wave_length);
                sq.pos = start;
                sq.toggle(sign);
                let bounds = (sq.lower_limit, sq.upper_limit);
                let mut out = vec![sq.pos];
                for _ in 0..frames {
                    sq.step();
                    out.push(sq.pos);
                }
                (out, bounds.0, bounds.1)
            }
        };
        Ok(json!({
            "kind": match kind { super::SweepKind::Filter => "filter", super::SweepKind::Square => "square" },
            "positions": positions, "lower": lower, "upper": upper,
        }))
    }

    /// The pitch offset the vibrato of instrument `n` adds, per tick for `frames`
    /// ticks from a trigger (0 through its delay, then `VIB_TAB * depth >> 7`
    /// stepping `vibrato_speed` entries a tick): `{"offsets": [..]}`, in the
    /// replayer's period units (a larger period is a lower note).
    pub fn vibrato_curve(&self, n: usize, frames: usize) -> Res<Value> {
        let ins = self.instrument_ref(n)?;
        let (mut delay, mut current) = (ins.vibrato_delay as i32, 0usize);
        let mut out = Vec::new();
        for _ in 0..frames.min(4096) {
            if ins.vibrato_depth == 0 || delay > 0 {
                delay -= (ins.vibrato_depth != 0) as i32;
                out.push(0);
            } else {
                out.push((VIB_TAB[current] as i32 * ins.vibrato_depth as i32) >> 7);
                current = (current + ins.vibrato_speed as usize) & 0x3f;
            }
        }
        Ok(json!({"offsets": out}))
    }
}
