//! Voices whose sound is made as they play instead of read from a clip:
//! chip-music players, streams, synths.

use crate::clip::ClipVoice;
use crate::smooth::{balance, fade_step, start_fade, step_ramp, GainRamp, Smoothed};
use crate::spatial::{equal_power, Hearing, Spatial, SpatialVoice};

/// Makes sound as it plays. It runs on the audio thread: `render` must not
/// allocate, lock or block.
pub trait Source: Send {
    /// Writes the next `left.len()` frames over `left` and `right` (the same
    /// length, at most `MixerConfig::max_block`). Returns false once the
    /// source has finished: what it wrote this time still plays, and it is
    /// not called again.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool;
}

/// A playing source.
pub(crate) struct SourceVoice {
    /// Taken when the voice ends, to be handed back off the audio thread.
    pub(crate) source: Option<Box<dyn Source>>,
    pub(crate) gain: Smoothed,
    pub(crate) pan: Smoothed,
    fade: Option<(u32, u32)>,
    pub(crate) ramp: Option<GainRamp>,
    pub(crate) spatial: Option<SpatialVoice>,
}

impl SourceVoice {
    pub(crate) fn new(
        source: Box<dyn Source>,
        gain: f32,
        pan: f32,
        spatial: Option<(Spatial, u64)>,
    ) -> SourceVoice {
        SourceVoice {
            source: Some(source),
            gain: Smoothed::new(gain.max(0.0)),
            pan: Smoothed::new(pan.clamp(-1.0, 1.0)),
            fade: None,
            ramp: None,
            spatial: spatial.map(|(params, frame)| SpatialVoice::new(params, frame)),
        }
    }

    pub(crate) fn stop(&mut self, frames: u32) {
        start_fade(&mut self.fade, frames);
    }

    /// Adds the voice's next `left.len()` frames into the buffers, rendering
    /// the source into `scratch`. Returns false once it has finished.
    pub(crate) fn render(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        scratch: (&mut [f32], &mut [f32]),
        coeff: f32,
        hearing: &Hearing,
    ) -> bool {
        let n = left.len().min(right.len());
        let Some(source) = self.source.as_mut() else {
            return false;
        };
        let (sl, sr) = (&mut scratch.0[..n], &mut scratch.1[..n]);
        // A source keeps its own clock, so it renders even when silent.
        let alive = source.render(sl, sr);
        step_ramp(&mut self.gain, &mut self.ramp, n);
        let (fade_start, fade_end, fade_frames) = fade_step(&mut self.fade, n);
        let audible = fade_frames.unwrap_or(n).min(n);
        let (gain_start, gain_end) = self.gain.advance(coeff);
        let (pan_start, pan_end) = self.pan.advance(coeff);
        let g0 = gain_start * fade_start;
        let g1 = gain_end * fade_end;

        let (l0, r0, l1, r1) = match &mut self.spatial {
            None => {
                let (l0, r0) = balance(pan_start);
                let (l1, r1) = balance(pan_end);
                (g0 * l0, g0 * r0, g1 * l1, g1 * r1)
            }
            Some(spatial) => {
                // Summed to mono, low-passed and placed; no doppler, since a
                // source plays at its own rate.
                let block = spatial.advance(hearing, coeff);
                for (l, r) in sl.iter_mut().zip(sr.iter()) {
                    *l = 0.5 * (*l + r);
                }
                spatial.filter(sl, block.lowpass_coeff);
                sr.copy_from_slice(sl);
                let (a0, b0) = equal_power(block.pan.0);
                let (a1, b1) = equal_power(block.pan.1);
                let (g0, g1) = (g0 * block.gain.0, g1 * block.gain.1);
                (g0 * a0, g0 * b0, g1 * a1, g1 * b1)
            }
        };
        let step_l = (l1 - l0) / n as f32;
        let step_r = (r1 - r0) / n as f32;
        for i in 0..audible {
            left[i] += sl[i] * (l0 + step_l * i as f32);
            right[i] += sr[i] * (r0 + step_r * i as f32);
        }
        alive && !fade_frames.is_some_and(|f| f <= n)
    }
}

/// A voice of either kind.
pub(crate) enum Voice {
    Clip(ClipVoice),
    Source(SourceVoice),
}

impl Voice {
    pub(crate) fn clip(&self) -> Option<usize> {
        match self {
            Voice::Clip(v) => Some(v.clip),
            Voice::Source(_) => None,
        }
    }

    pub(crate) fn gain(&mut self) -> &mut Smoothed {
        match self {
            Voice::Clip(v) => &mut v.gain,
            Voice::Source(v) => &mut v.gain,
        }
    }

    pub(crate) fn pan(&mut self) -> &mut Smoothed {
        match self {
            Voice::Clip(v) => &mut v.pan,
            Voice::Source(v) => &mut v.pan,
        }
    }

    pub(crate) fn ramp(&mut self) -> &mut Option<GainRamp> {
        match self {
            Voice::Clip(v) => &mut v.ramp,
            Voice::Source(v) => &mut v.ramp,
        }
    }

    pub(crate) fn spatial(&mut self) -> &mut Option<SpatialVoice> {
        match self {
            Voice::Clip(v) => &mut v.spatial,
            Voice::Source(v) => &mut v.spatial,
        }
    }

    pub(crate) fn stop(&mut self, frames: u32) {
        match self {
            Voice::Clip(v) => v.stop(frames),
            Voice::Source(v) => v.stop(frames),
        }
    }

    /// Only clips change speed; a source plays at its own rate.
    pub(crate) fn set_pitch(&mut self, pitch: f32) {
        if let Voice::Clip(v) = self {
            v.set_pitch(pitch);
        }
    }
}
