//! Positional audio: where a voice is heard from, in 2D or 3D, and the
//! doppler shift of its motion.
//!
//! The host sends the listener and each spatial voice's position and
//! velocity once per game frame; the mixer extrapolates positions by their
//! velocity between updates, and every result (gain, pan, low-pass, pitch
//! ratio) glides from block to block, so nothing clicks or zips.

use std::f32::consts::{FRAC_PI_4, TAU};

use crate::smooth::{db_to_gain, Smoothed};

/// How a spatial voice is heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpatialMode {
    /// In the listener's view plane: the axis along the listener's forward is
    /// ignored for distance, and the pan follows the offset along its right,
    /// reaching a side at `Listener::pan_width`. For side-scrollers and
    /// top-down games.
    Planar,
    /// Full 3D: distance in space, equal-power pan by direction, and a slight
    /// low-pass for sounds behind the listener.
    Full,
}

/// How a voice fades with distance beyond `min_distance`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rolloff {
    /// `min_distance / distance`, as sound falls off in air; it stops
    /// falling at `max_distance`.
    Inverse,
    /// Straight down to silence at `max_distance`.
    Linear,
}

/// A voice's place in the world. Distances are in world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spatial {
    pub mode: SpatialMode,
    pub position: [f32; 3],
    /// World units per second, for doppler and for extrapolating the position
    /// between updates.
    pub velocity: [f32; 3],
    /// Full volume within this distance.
    pub min_distance: f32,
    pub max_distance: f32,
    pub rolloff: Rolloff,
    /// Scales the doppler shift; 0 turns it off for this voice.
    pub doppler: f32,
}

impl Spatial {
    pub fn new(mode: SpatialMode, position: [f32; 3]) -> Spatial {
        Spatial {
            mode,
            position,
            velocity: [0.0; 3],
            min_distance: 1.0,
            max_distance: 50.0,
            rolloff: Rolloff::Inverse,
            doppler: 1.0,
        }
    }
}

/// Where the mix is heard from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Listener {
    pub position: [f32; 3],
    /// Unit vectors; the right ear is along `forward x up`.
    pub forward: [f32; 3],
    pub up: [f32; 3],
    pub velocity: [f32; 3],
    /// For 2D voices: the offset along the listener's right that pans a
    /// sound fully to one side (typically half the view's width).
    pub pan_width: f32,
    /// World units per second; 343 for metres.
    pub speed_of_sound: f32,
    /// The project-wide doppler switch.
    pub doppler: bool,
    /// How a fully occluded voice (`Command::SetVoiceOcclusion` 1) is heard:
    /// this much quieter (dB, below 0) and low-passed at this cutoff. A
    /// partly occluded one is part of the way there.
    pub occlusion_db: f32,
    pub occlusion_cutoff_hz: f32,
}

impl Default for Listener {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
            velocity: [0.0; 3],
            pan_width: 10.0,
            speed_of_sound: 343.0,
            doppler: true,
            occlusion_db: -12.0,
            occlusion_cutoff_hz: 1_000.0,
        }
    }
}

/// The low-pass cutoff at `min_distance` and at `max_distance`.
const NEAR_CUTOFF_HZ: f32 = 20_000.0;
const FAR_CUTOFF_HZ: f32 = 4_000.0;
/// How much a sound straight behind the listener is low-passed (cutoff
/// factor).
const BEHIND_CUTOFF: f32 = 0.55;
/// The doppler pitch ratio stays within this factor either way.
const MAX_DOPPLER: f32 = 2.0;
/// Positions are extrapolated by their velocity for at most this long after
/// an update, in case updates stop.
const MAX_EXTRAPOLATION_SECONDS: f32 = 0.1;
/// Occlusion glides this many times slower than the other spatial results
/// (about 60 ms), so a wall passing between does not thump.
const OCCLUSION_GLIDE: f32 = 6.0;
/// For this long after a voice starts, occlusion glides as fast as the rest:
/// the host's first occlusion for a new sound lands within a game frame, and
/// the sound's attack should not be heard through the wall meanwhile.
const YOUNG_SECONDS: f32 = 0.05;

/// What a spatial voice sounds like from the listener this block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Heard {
    /// Distance gain, 0..1.
    pub gain: f32,
    /// -1 (left) to 1 (right).
    pub pan: f32,
    pub cutoff_hz: f32,
    /// Playback rate factor from doppler.
    pub ratio: f32,
}

pub(crate) fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn add_scaled(a: [f32; 3], b: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = length(a);
    if l > 1e-9 {
        [a[0] / l, a[1] / l, a[2] / l]
    } else {
        [0.0; 3]
    }
}

/// Removes the part of `v` along the unit vector `axis`.
fn flatten(v: [f32; 3], axis: [f32; 3]) -> [f32; 3] {
    add_scaled(v, axis, -dot(v, axis))
}

/// The distance gain for `rolloff` between `min` and `max`.
fn rolloff_gain(rolloff: Rolloff, distance: f32, min: f32, max: f32) -> f32 {
    let min = min.max(1e-3);
    let max = max.max(min);
    if distance <= min {
        return 1.0;
    }
    match rolloff {
        Rolloff::Inverse => min / distance.min(max),
        Rolloff::Linear => {
            if max <= min {
                0.0
            } else {
                (1.0 - (distance - min) / (max - min)).clamp(0.0, 1.0)
            }
        }
    }
}

/// Where the voice is now: its last position moved on by its velocity.
pub(crate) fn extrapolate(spatial: &Spatial, seconds_since_update: f32) -> [f32; 3] {
    let t = seconds_since_update.clamp(0.0, MAX_EXTRAPOLATION_SECONDS);
    add_scaled(spatial.position, spatial.velocity, t)
}

/// How the voice at `position` (moving at `spatial.velocity`) sounds to the
/// listener (at `ear`, moving at `listener.velocity`).
pub(crate) fn hear(
    listener: &Listener,
    ear: [f32; 3],
    spatial: &Spatial,
    position: [f32; 3],
) -> Heard {
    let forward = normalize(listener.forward);
    let up = normalize(listener.up);
    let right = normalize(cross(forward, up));
    let offset = sub(position, ear);

    let (distance, pan, behind) = match spatial.mode {
        SpatialMode::Full => {
            let distance = length(offset);
            let (pan, behind) = if distance > 1e-6 {
                let x = dot(offset, right) / distance;
                let z = dot(offset, forward) / distance;
                (x, (-z).max(0.0))
            } else {
                (0.0, 0.0)
            };
            (distance, pan, behind)
        }
        SpatialMode::Planar => {
            let distance = length(flatten(offset, forward));
            let width = listener.pan_width.max(1e-3);
            (distance, (dot(offset, right) / width).clamp(-1.0, 1.0), 0.0)
        }
    };

    let gain = rolloff_gain(
        spatial.rolloff,
        distance,
        spatial.min_distance,
        spatial.max_distance,
    );
    // Distance low-pass, on a log scale from near to far, and the head's
    // shadow for sounds behind.
    let span = (spatial.max_distance - spatial.min_distance).max(1e-3);
    let far = ((distance - spatial.min_distance) / span).clamp(0.0, 1.0);
    let cutoff = NEAR_CUTOFF_HZ * (FAR_CUTOFF_HZ / NEAR_CUTOFF_HZ).powf(far);
    let cutoff = cutoff * (1.0 - (1.0 - BEHIND_CUTOFF) * behind);

    let ratio = doppler(listener, ear, spatial, position);
    Heard {
        gain,
        pan: pan.clamp(-1.0, 1.0),
        cutoff_hz: cutoff,
        ratio,
    }
}

/// The pitch ratio of the source's and listener's motion along the line
/// between them (in 2D, the line in the view plane).
fn doppler(listener: &Listener, ear: [f32; 3], spatial: &Spatial, position: [f32; 3]) -> f32 {
    let c = listener.speed_of_sound;
    if !listener.doppler || spatial.doppler <= 0.0 || c <= 0.0 {
        return 1.0;
    }
    let forward = normalize(listener.forward);
    let mut to_listener = sub(ear, position);
    let mut source_velocity = spatial.velocity;
    let mut listener_velocity = listener.velocity;
    if spatial.mode == SpatialMode::Planar {
        to_listener = flatten(to_listener, forward);
        source_velocity = flatten(source_velocity, forward);
        listener_velocity = flatten(listener_velocity, forward);
    }
    let u = normalize(to_listener);
    if u == [0.0; 3] {
        return 1.0;
    }
    let toward_listener = dot(source_velocity, u) * spatial.doppler;
    let toward_source = -dot(listener_velocity, u) * spatial.doppler;
    // Keep the source well below the speed of sound.
    let denominator = (c - toward_listener).max(c / MAX_DOPPLER);
    ((c + toward_source) / denominator).clamp(1.0 / MAX_DOPPLER, MAX_DOPPLER)
}

/// `cutoff` moved toward `occluded` (when that is lower) by `amount`, on a
/// log scale.
fn occluded_cutoff(cutoff: f32, occluded: f32, amount: f32) -> f32 {
    if amount <= 0.0 || occluded >= cutoff || occluded <= 0.0 {
        return cutoff;
    }
    cutoff * (occluded / cutoff).powf(amount.min(1.0))
}

/// Equal-power gains for `pan` in -1..1: the centre is -3 dB on each side.
pub(crate) fn equal_power(pan: f32) -> (f32, f32) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * FRAC_PI_4;
    (angle.cos(), angle.sin())
}

/// The one-pole low-pass coefficient for `cutoff_hz` at `sample_rate`.
pub(crate) fn lowpass_coeff(cutoff_hz: f32, sample_rate: f32) -> f32 {
    if cutoff_hz >= sample_rate * 0.45 {
        return 1.0;
    }
    1.0 - (-TAU * cutoff_hz / sample_rate).exp()
}

/// A voice's spatial state: its last update and the gliding results.
#[derive(Debug)]
pub(crate) struct SpatialVoice {
    pub(crate) params: Spatial,
    /// The mixer frame of the last position update.
    pub(crate) updated_at: u64,
    /// The mixer frame it became spatial at.
    born: u64,
    gain: Smoothed,
    pan: Smoothed,
    cutoff: Smoothed,
    ratio: Smoothed,
    /// 0 (heard directly) to 1 (behind a wall).
    pub(crate) occlusion: Smoothed,
    lowpass: f32,
    started: bool,
}

/// What every spatial voice needs from the mixer for one block.
pub(crate) struct Hearing<'a> {
    pub listener: &'a Listener,
    /// The listener's position now (extrapolated).
    pub ear: [f32; 3],
    /// The block's first frame.
    pub frame: u64,
    pub sample_rate: f32,
}

/// One block's spatial results, each as (start, end).
pub(crate) struct Block {
    pub gain: (f32, f32),
    pub pan: (f32, f32),
    pub ratio: f32,
    pub lowpass_coeff: f32,
}

impl SpatialVoice {
    pub(crate) fn new(params: Spatial, frame: u64) -> SpatialVoice {
        SpatialVoice {
            params,
            updated_at: frame,
            born: frame,
            gain: Smoothed::new(0.0),
            pan: Smoothed::new(0.0),
            cutoff: Smoothed::new(NEAR_CUTOFF_HZ),
            ratio: Smoothed::new(1.0),
            occlusion: Smoothed::new(0.0),
            lowpass: 0.0,
            started: false,
        }
    }

    /// Moves on to this block: where the voice is, how it is heard, and the
    /// glides from the last block. The first block starts where it is.
    pub(crate) fn advance(&mut self, hearing: &Hearing, coeff: f32) -> Block {
        let since = hearing.frame.saturating_sub(self.updated_at) as f32 / hearing.sample_rate;
        let position = extrapolate(&self.params, since);
        let heard = hear(hearing.listener, hearing.ear, &self.params, position);
        let listener = hearing.listener;
        if !self.started {
            self.occlusion.snap(self.occlusion.target());
        }
        let young =
            (hearing.frame.saturating_sub(self.born) as f32) < YOUNG_SECONDS * hearing.sample_rate;
        let glide = if young {
            coeff
        } else {
            coeff.powf(1.0 / OCCLUSION_GLIDE)
        };
        let (o0, o1) = self.occlusion.advance(glide);
        let cutoff = occluded_cutoff(heard.cutoff_hz, listener.occlusion_cutoff_hz, o1);
        if self.started {
            self.gain.set(heard.gain);
            self.pan.set(heard.pan);
            self.cutoff.set(cutoff);
            self.ratio.set(heard.ratio);
        } else {
            self.gain.snap(heard.gain);
            self.pan.snap(heard.pan);
            self.cutoff.snap(cutoff);
            self.ratio.snap(heard.ratio);
            self.started = true;
        }
        let (g0, g1) = self.gain.advance(coeff);
        let db = listener.occlusion_db.min(0.0);
        let gain = (g0 * db_to_gain(db * o0), g1 * db_to_gain(db * o1));
        let pan = self.pan.advance(coeff);
        let (_, cutoff) = self.cutoff.advance(coeff);
        let (r0, r1) = self.ratio.advance(coeff);
        Block {
            gain,
            pan,
            ratio: 0.5 * (r0 + r1),
            lowpass_coeff: lowpass_coeff(cutoff, hearing.sample_rate),
        }
    }

    /// Runs `samples` through the voice's low-pass.
    pub(crate) fn filter(&mut self, samples: &mut [f32], coeff: f32) {
        if coeff >= 1.0 {
            if let Some(&last) = samples.last() {
                self.lowpass = last;
            }
            return;
        }
        let mut y = self.lowpass;
        for s in samples.iter_mut() {
            y += coeff * (*s - y);
            *s = y;
        }
        self.lowpass = y;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard(listener: &Listener, spatial: Spatial) -> Heard {
        hear(listener, listener.position, &spatial, spatial.position)
    }

    #[test]
    fn full_3d_pans_by_direction_and_falls_off_with_distance() {
        let listener = Listener::default();
        let right = heard(&listener, Spatial::new(SpatialMode::Full, [5.0, 0.0, 0.0]));
        assert!((right.pan - 1.0).abs() < 1e-6);
        assert!((right.gain - 0.2).abs() < 1e-6, "inverse: 1/5");
        let ahead = heard(&listener, Spatial::new(SpatialMode::Full, [0.0, 0.0, -2.0]));
        assert!(ahead.pan.abs() < 1e-6 && (ahead.gain - 0.5).abs() < 1e-6);
        let behind = heard(&listener, Spatial::new(SpatialMode::Full, [0.0, 0.0, 2.0]));
        assert!(behind.cutoff_hz < ahead.cutoff_hz * 0.6, "behind is duller");
    }

    #[test]
    fn planar_2d_ignores_the_view_axis_and_pans_by_offset() {
        // A top-down camera: looking down -Y, screen up is -Z, screen right +X.
        let listener = Listener {
            position: [0.0, 20.0, 0.0],
            forward: [0.0, -1.0, 0.0],
            up: [0.0, 0.0, -1.0],
            pan_width: 8.0,
            ..Listener::default()
        };
        let below = heard(
            &listener,
            Spatial::new(SpatialMode::Planar, [0.0, 0.0, 0.0]),
        );
        assert_eq!(below.gain, 1.0, "the 20 units of height do not count");
        assert_eq!(below.pan, 0.0);
        let half = heard(
            &listener,
            Spatial::new(SpatialMode::Planar, [4.0, 0.0, 0.0]),
        );
        assert!((half.pan - 0.5).abs() < 1e-6);
        let far_left = heard(
            &listener,
            Spatial::new(SpatialMode::Planar, [-20.0, 0.0, 0.0]),
        );
        assert_eq!(far_left.pan, -1.0);
    }

    #[test]
    fn linear_rolloff_reaches_silence_at_max_distance() {
        let listener = Listener::default();
        let mut s = Spatial::new(SpatialMode::Full, [0.0, 0.0, -30.0]);
        s.rolloff = Rolloff::Linear;
        s.min_distance = 10.0;
        s.max_distance = 50.0;
        assert!((heard(&listener, s).gain - 0.5).abs() < 1e-6);
        s.position = [0.0, 0.0, -60.0];
        assert_eq!(heard(&listener, s).gain, 0.0);
    }

    #[test]
    fn doppler_raises_an_approaching_sound_and_can_be_switched_off() {
        let listener = Listener::default();
        let mut s = Spatial::new(SpatialMode::Full, [0.0, 0.0, -100.0]);
        s.velocity = [0.0, 0.0, 34.3]; // toward the listener at c/10
        let r = heard(&listener, s).ratio;
        assert!((r - 1.0 / 0.9).abs() < 1e-4, "{r}");
        s.velocity = [0.0, 0.0, -34.3];
        assert!((heard(&listener, s).ratio - 1.0 / 1.1).abs() < 1e-4);
        s.doppler = 0.0;
        assert_eq!(heard(&listener, s).ratio, 1.0);
        s.doppler = 1.0;
        let off = Listener {
            doppler: false,
            ..listener
        };
        assert_eq!(heard(&off, s).ratio, 1.0);
        // Faster than sound is clamped.
        s.velocity = [0.0, 0.0, 1000.0];
        assert_eq!(heard(&listener, s).ratio, MAX_DOPPLER);
    }

    #[test]
    fn equal_power_is_minus_3_db_in_the_centre() {
        let (l, r) = equal_power(0.0);
        assert!((l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6 && (l - r).abs() < 1e-6);
        let (l, r) = equal_power(1.0);
        assert!(l.abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
    }
}
