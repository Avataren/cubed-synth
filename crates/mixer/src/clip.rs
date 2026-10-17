//! Decoded sounds and the voices that play them.

use crate::smooth::{apply_ramp, balance, Smoothed};

/// A decoded sound: mono or stereo PCM at its own sample rate. Voices
/// resample it to the mixer's rate as they play.
#[derive(Clone, Debug)]
pub struct Clip {
    sample_rate: f32,
    left: Box<[f32]>,
    /// `None` for a mono clip, which plays at full level on both channels.
    right: Option<Box<[f32]>>,
}

impl Clip {
    pub fn mono(sample_rate: f32, samples: Vec<f32>) -> Clip {
        Clip {
            sample_rate: sample_rate.max(1.0),
            left: samples.into_boxed_slice(),
            right: None,
        }
    }

    /// Both channels are cut to the shorter one's length.
    pub fn stereo(sample_rate: f32, mut left: Vec<f32>, mut right: Vec<f32>) -> Clip {
        let frames = left.len().min(right.len());
        left.truncate(frames);
        right.truncate(frames);
        Clip {
            sample_rate: sample_rate.max(1.0),
            left: left.into_boxed_slice(),
            right: Some(right.into_boxed_slice()),
        }
    }

    /// From interleaved samples: mono stays mono, and of more channels the
    /// first two are kept.
    pub fn from_interleaved(sample_rate: f32, channels: usize, samples: &[f32]) -> Clip {
        match channels {
            0 => Clip::mono(sample_rate, Vec::new()),
            1 => Clip::mono(sample_rate, samples.to_vec()),
            n => {
                let left = samples.iter().step_by(n).copied().collect();
                let right = samples.iter().skip(1).step_by(n).copied().collect();
                Clip::stereo(sample_rate, left, right)
            }
        }
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn channels(&self) -> usize {
        if self.right.is_some() {
            2
        } else {
            1
        }
    }

    pub fn duration_seconds(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }
}

/// Catmull-Rom interpolation between `x0` and `x1` at `t` in [0, 1). At
/// `t = 0` it returns `x0` exactly, so unresampled playback is bit-exact.
#[inline]
fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

/// Sample `i` of `data`, wrapping when looped and silent outside otherwise.
#[inline]
fn sample(data: &[f32], i: isize, looped: bool) -> f32 {
    let len = data.len() as isize;
    if (0..len).contains(&i) {
        data[i as usize]
    } else if looped && len > 0 {
        data[i.rem_euclid(len) as usize]
    } else {
        0.0
    }
}

#[inline]
fn interpolate(data: &[f32], position: f64, looped: bool) -> f32 {
    let base = position.floor();
    let t = (position - base) as f32;
    let i = base as isize;
    hermite(
        sample(data, i - 1, looped),
        sample(data, i, looped),
        sample(data, i + 1, looped),
        sample(data, i + 2, looped),
        t,
    )
}

/// A playing clip.
#[derive(Debug)]
pub(crate) struct ClipVoice {
    pub(crate) clip: usize,
    /// Read position in clip frames.
    position: f64,
    /// Clip frames per output frame at pitch 1 (`clip rate / mixer rate`).
    base_rate: f64,
    pitch: f32,
    pub(crate) gain: Smoothed,
    pub(crate) pan: Smoothed,
    pub(crate) looped: bool,
    /// The fade-out of a stop: frames left and the fade's full length.
    fade: Option<(u32, u32)>,
}

impl ClipVoice {
    pub(crate) fn new(
        clip: usize,
        clip_rate: f32,
        mixer_rate: f32,
        gain: f32,
        pitch: f32,
        pan: f32,
        looped: bool,
    ) -> ClipVoice {
        ClipVoice {
            clip,
            position: 0.0,
            base_rate: clip_rate as f64 / mixer_rate as f64,
            pitch: sanitize_pitch(pitch),
            gain: Smoothed::new(gain.max(0.0)),
            pan: Smoothed::new(pan.clamp(-1.0, 1.0)),
            looped,
            fade: None,
        }
    }

    pub(crate) fn set_pitch(&mut self, pitch: f32) {
        self.pitch = sanitize_pitch(pitch);
    }

    /// Fades out over `frames` (0 = at once) and then ends. A second stop
    /// only ever shortens the fade.
    pub(crate) fn stop(&mut self, frames: u32) {
        let frames = frames.max(1);
        match self.fade {
            Some((left, _)) if left <= frames => {}
            _ => self.fade = Some((frames, frames)),
        }
    }

    /// Adds the voice's next `left.len()` frames into the buffers. Returns
    /// false once it has finished: the clip ran out, or a stop's fade ended.
    pub(crate) fn render(
        &mut self,
        clip: &Clip,
        left: &mut [f32],
        right: &mut [f32],
        scratch: &mut [f32],
        coeff: f32,
    ) -> bool {
        let n = left.len().min(right.len()).min(scratch.len());
        let frames = clip.frames() as f64;
        if frames == 0.0 {
            return false;
        }
        let step = self.base_rate * self.pitch as f64;
        // A stop's fade is exact to the sample: it may end partway through.
        let (fade_start, fade_end, fade_frames) = self.advance_fade(n);
        let audible = fade_frames.unwrap_or(n).min(n);

        // Mono or the left channel into scratch, sample by sample, so a
        // non-looped clip can end partway through the block.
        let mut written = audible;
        let mut finished = fade_frames.is_some_and(|f| f <= n);
        let mut position = self.position;
        for (i, out) in scratch[..audible].iter_mut().enumerate() {
            if !self.looped && position >= frames {
                written = i;
                finished = true;
                break;
            }
            *out = interpolate(&clip.left, position, self.looped);
            position += step;
            if self.looped && position >= frames {
                position -= frames * (position / frames).floor();
            }
        }
        scratch[written..n].fill(0.0);

        let (gain_start, gain_end) = self.gain.advance(coeff);
        let (pan_start, pan_end) = self.pan.advance(coeff);
        let (l0, r0) = balance(pan_start);
        let (l1, r1) = balance(pan_end);
        let g0 = gain_start * fade_start;
        let g1 = gain_end * fade_end;

        match &clip.right {
            None => {
                // Mono: the same signal on both sides.
                let step_l = (g1 * l1 - g0 * l0) / n as f32;
                let step_r = (g1 * r1 - g0 * r0) / n as f32;
                for i in 0..written {
                    let s = scratch[i];
                    left[i] += s * (g0 * l0 + step_l * i as f32);
                    right[i] += s * (g0 * r0 + step_r * i as f32);
                }
            }
            Some(clip_right) => {
                apply_ramp(&mut scratch[..n], g0 * l0, g1 * l1);
                for i in 0..written {
                    left[i] += scratch[i];
                }
                // Then the right channel, read with the same positions.
                let mut position = self.position;
                let step_r = (g1 * r1 - g0 * r0) / n as f32;
                for (i, out) in right[..written].iter_mut().enumerate() {
                    *out += interpolate(clip_right, position, self.looped)
                        * (g0 * r0 + step_r * i as f32);
                    position += step;
                    if self.looped && position >= frames {
                        position -= frames * (position / frames).floor();
                    }
                }
            }
        }

        self.position = position;
        !finished
    }

    /// The fade's level at the start of an `n`-frame block and where its
    /// line would be at the block's end (below zero if it ends sooner), and
    /// the frames it has left.
    fn advance_fade(&mut self, n: usize) -> (f32, f32, Option<usize>) {
        match &mut self.fade {
            None => (1.0, 1.0, None),
            Some((left, total)) => {
                let frames = *left as usize;
                let total = *total as f32;
                *left = left.saturating_sub(n as u32);
                (
                    frames as f32 / total,
                    (frames as f32 - n as f32) / total,
                    Some(frames),
                )
            }
        }
    }
}

fn sanitize_pitch(pitch: f32) -> f32 {
    if pitch.is_finite() {
        pitch.clamp(0.01, 16.0)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn render_all(voice: &mut ClipVoice, clip: &Clip, frames: usize) -> (Vec<f32>, Vec<f32>, bool) {
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        let mut scratch = vec![0.0; frames];
        let alive = voice.render(clip, &mut l, &mut r, &mut scratch, 0.0);
        (l, r, alive)
    }

    #[test]
    fn plays_a_clip_at_its_own_rate_bit_exactly() {
        let samples: Vec<f32> = (0..100).map(|i| (i as f32 * 0.1).sin()).collect();
        let clip = Clip::mono(RATE, samples.clone());
        let mut voice = ClipVoice::new(0, RATE, RATE, 1.0, 1.0, 0.0, false);
        let (l, r, alive) = render_all(&mut voice, &clip, 128);
        assert!(!alive, "a 100-frame clip ends within 128 frames");
        assert_eq!(&l[..100], &samples[..]);
        assert_eq!(&r[..100], &samples[..]);
        assert!(l[100..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn resamples_to_the_mixer_rate() {
        // A 24 kHz clip lasts twice as many frames at 48 kHz.
        let clip = Clip::mono(24_000.0, vec![0.5; 100]);
        let mut voice = ClipVoice::new(0, 24_000.0, RATE, 1.0, 1.0, 0.0, false);
        let (l, _, alive) = render_all(&mut voice, &clip, 256);
        assert!(!alive);
        let heard = l.iter().filter(|&&s| s != 0.0).count();
        assert!((199..=201).contains(&heard), "{heard} frames");
    }

    #[test]
    fn loops_until_stopped_then_fades_out() {
        let clip = Clip::stereo(RATE, vec![1.0; 50], vec![-1.0; 50]);
        let mut voice = ClipVoice::new(0, RATE, RATE, 1.0, 1.0, 0.0, true);
        let (l, r, alive) = render_all(&mut voice, &clip, 300);
        assert!(alive);
        assert!(l.iter().all(|&s| (s - 1.0).abs() < 1e-6));
        assert!(r.iter().all(|&s| (s + 1.0).abs() < 1e-6));

        voice.stop(64);
        let (l, _, alive) = render_all(&mut voice, &clip, 128);
        assert!(!alive);
        assert_eq!(l[0], 1.0);
        assert!(l[127].abs() < 1e-6);
    }
}
