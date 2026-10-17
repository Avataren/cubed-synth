/// How long a parameter takes to glide most of the way to a new value: about
/// 95% in three time constants, so a fader move settles within ~30 ms.
const TIME_CONSTANT_SECONDS: f32 = 0.01;

/// The per-block factor for [`Smoothed::advance`] when a block is `frames`
/// long at `sample_rate`.
pub(crate) fn block_coeff(frames: usize, sample_rate: f32) -> f32 {
    (-(frames as f32) / (TIME_CONSTANT_SECONDS * sample_rate)).exp()
}

/// A parameter that glides to its target instead of jumping, so changing it
/// never clicks. It moves once per block; within a block the value ramps
/// linearly between the block's start and end values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Smoothed {
    current: f32,
    target: f32,
}

impl Smoothed {
    pub(crate) fn new(value: f32) -> Self {
        Self {
            current: value,
            target: value,
        }
    }

    pub(crate) fn set(&mut self, target: f32) {
        self.target = target;
    }

    /// Jumps to `value` (for a voice's first block).
    pub(crate) fn snap(&mut self, value: f32) {
        self.current = value;
        self.target = value;
    }

    pub(crate) fn target(&self) -> f32 {
        self.target
    }

    #[cfg(test)]
    pub(crate) fn is_settled(&self) -> bool {
        self.current == self.target
    }

    /// Moves one block toward the target and returns the block's start and
    /// end values.
    pub(crate) fn advance(&mut self, coeff: f32) -> (f32, f32) {
        let start = self.current;
        let next = self.target + (self.current - self.target) * coeff;
        self.current = if (next - self.target).abs() < 1e-5 {
            self.target
        } else {
            next
        };
        (start, self.current)
    }
}

/// A long gain change (a fade-in or a crossfade): the target the voice's gain
/// glides to moves in a straight line over `frames_left`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GainRamp {
    target: f32,
    frames_left: u32,
}

impl GainRamp {
    pub(crate) fn new(target: f32, frames: u32) -> GainRamp {
        GainRamp {
            target: target.max(0.0),
            frames_left: frames,
        }
    }
}

/// Moves `gain`'s target one block of `n` frames along the ramp.
pub(crate) fn step_ramp(gain: &mut Smoothed, ramp: &mut Option<GainRamp>, n: usize) {
    let Some(r) = ramp else {
        return;
    };
    if r.frames_left as usize <= n {
        gain.set(r.target);
        *ramp = None;
    } else {
        let current = gain.target();
        let t = n as f32 / r.frames_left as f32;
        gain.set(current + (r.target - current) * t);
        r.frames_left -= n as u32;
    }
}

/// Starts (or shortens) a stop's fade-out of `frames` frames.
pub(crate) fn start_fade(fade: &mut Option<(u32, u32)>, frames: u32) {
    let frames = frames.max(1);
    match fade {
        Some((left, _)) if *left <= frames => {}
        _ => *fade = Some((frames, frames)),
    }
}

/// A stop fade's level at the start of an `n`-frame block and where its line
/// would be at the block's end (below zero if it ends sooner), and the frames
/// it has left.
pub(crate) fn fade_step(fade: &mut Option<(u32, u32)>, n: usize) -> (f32, f32, Option<usize>) {
    match fade {
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

/// Scales `buffer` by a linear ramp from `start` to `end`.
pub(crate) fn apply_ramp(buffer: &mut [f32], start: f32, end: f32) {
    if start == end {
        if start != 1.0 {
            buffer.iter_mut().for_each(|s| *s *= start);
        }
        return;
    }
    let step = (end - start) / buffer.len() as f32;
    for (i, s) in buffer.iter_mut().enumerate() {
        *s *= start + step * i as f32;
    }
}

/// The left and right gains of a balance control: centre (0) leaves both
/// channels at unity, -1 silences the right, 1 silences the left.
pub(crate) fn balance(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    if pan < 0.0 {
        (1.0, 1.0 + pan)
    } else {
        (1.0 - pan, 1.0)
    }
}

pub(crate) fn db_to_gain(db: f32) -> f32 {
    if db <= MIN_DB {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Faders at or below this are silent.
pub const MIN_DB: f32 = -80.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glides_to_the_target_and_settles_exactly() {
        let mut gain = Smoothed::new(0.0);
        gain.set(1.0);
        let coeff = block_coeff(128, 48_000.0);
        let (start, end) = gain.advance(coeff);
        assert_eq!(start, 0.0);
        assert!(end > 0.0 && end < 1.0);
        for _ in 0..200 {
            gain.advance(coeff);
        }
        assert!(gain.is_settled());
    }

    #[test]
    fn balance_keeps_the_centre_at_unity() {
        assert_eq!(balance(0.0), (1.0, 1.0));
        assert_eq!(balance(-1.0), (1.0, 0.0));
        assert_eq!(balance(0.5), (0.5, 1.0));
    }
}
