//! The mixer's built-in effects, described as plain data (a kind and its
//! parameter values) so a host can save them in a layout, send them across
//! a message port, and change their parameters while they run
//! (`Command::SetInsertParam`).

use cubed_dsp::biquad::{Biquad, Filter, FilterType};
use cubed_dsp::nodes::{Bitcrusher, Chorus, Compressor, Delay, Freeverb, Limiter, Saturation};
use cubed_dsp::{AudioNode, StereoEffect};

use crate::insert::Insert;

/// The most parameters an effect has.
pub const MAX_PARAMS: usize = 6;

/// The longest delay the delay effect is built for.
const MAX_DELAY_MS: f32 = 2000.0;
/// The longest base delay the chorus is built for.
const MAX_CHORUS_MS: f32 = 40.0;
/// How fast a filter's cutoff glides to a new value.
const FILTER_GLIDE_SECONDS: f32 = 0.03;

/// A built-in effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EffectKind {
    Reverb,
    Delay,
    LowPass,
    HighPass,
    Compressor,
    Limiter,
    Chorus,
    Saturation,
    Bitcrusher,
}

/// One of an effect's parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamInfo {
    /// snake_case, as saved in layouts.
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// "dB", "ms", "Hz" or "" (a plain amount).
    pub unit: &'static str,
}

const fn param(
    name: &'static str,
    min: f32,
    max: f32,
    default: f32,
    unit: &'static str,
) -> ParamInfo {
    ParamInfo {
        name,
        min,
        max,
        default,
        unit,
    }
}

const REVERB: [ParamInfo; 5] = [
    param("room_size", 0.0, 1.0, 0.8, ""),
    param("damping", 0.0, 1.0, 0.5, ""),
    param("wet", 0.0, 1.0, 0.3, ""),
    param("dry", 0.0, 1.0, 1.0, ""),
    param("width", 0.0, 1.0, 1.0, ""),
];
const DELAY: [ParamInfo; 3] = [
    param("time_ms", 1.0, MAX_DELAY_MS, 350.0, "ms"),
    param("feedback", 0.0, 0.95, 0.35, ""),
    param("mix", 0.0, 1.0, 0.3, ""),
];
const LOW_PASS: [ParamInfo; 2] = [
    param("cutoff_hz", 20.0, 20_000.0, 1_000.0, "Hz"),
    param("q", 0.1, 10.0, 0.707, ""),
];
const HIGH_PASS: [ParamInfo; 2] = [
    param("cutoff_hz", 20.0, 20_000.0, 200.0, "Hz"),
    param("q", 0.1, 10.0, 0.707, ""),
];
const COMPRESSOR: [ParamInfo; 6] = [
    param("threshold_db", -60.0, 0.0, -18.0, "dB"),
    param("ratio", 1.0, 20.0, 4.0, ""),
    param("attack_ms", 0.1, 200.0, 10.0, "ms"),
    param("release_ms", 5.0, 2000.0, 120.0, "ms"),
    param("makeup_db", 0.0, 24.0, 0.0, "dB"),
    param("mix", 0.0, 1.0, 1.0, ""),
];
const LIMITER: [ParamInfo; 2] = [
    param("threshold_db", -24.0, 0.0, -1.0, "dB"),
    param("release_ms", 5.0, 1000.0, 50.0, "ms"),
];
const CHORUS: [ParamInfo; 5] = [
    param("delay_ms", 1.0, MAX_CHORUS_MS, 15.0, "ms"),
    param("depth_ms", 0.0, 20.0, 5.0, "ms"),
    param("rate_hz", 0.01, 10.0, 0.5, "Hz"),
    param("feedback", 0.0, 0.9, 0.2, ""),
    param("mix", 0.0, 1.0, 0.4, ""),
];
const SATURATION: [ParamInfo; 2] = [
    param("drive", 0.1, 10.0, 2.0, ""),
    param("mix", 0.0, 1.0, 0.5, ""),
];
const BITCRUSHER: [ParamInfo; 3] = [
    param("bits", 1.0, 16.0, 8.0, ""),
    param("downsample", 1.0, 32.0, 4.0, ""),
    param("mix", 0.0, 1.0, 1.0, ""),
];

impl EffectKind {
    pub const ALL: [EffectKind; 9] = [
        EffectKind::Reverb,
        EffectKind::Delay,
        EffectKind::LowPass,
        EffectKind::HighPass,
        EffectKind::Compressor,
        EffectKind::Limiter,
        EffectKind::Chorus,
        EffectKind::Saturation,
        EffectKind::Bitcrusher,
    ];

    /// snake_case, as saved in layouts.
    pub fn name(self) -> &'static str {
        match self {
            EffectKind::Reverb => "reverb",
            EffectKind::Delay => "delay",
            EffectKind::LowPass => "low_pass",
            EffectKind::HighPass => "high_pass",
            EffectKind::Compressor => "compressor",
            EffectKind::Limiter => "limiter",
            EffectKind::Chorus => "chorus",
            EffectKind::Saturation => "saturation",
            EffectKind::Bitcrusher => "bitcrusher",
        }
    }

    pub fn from_name(name: &str) -> Option<EffectKind> {
        EffectKind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// The kind's number in the byte protocol.
    pub fn index(self) -> u8 {
        EffectKind::ALL
            .iter()
            .position(|&k| k == self)
            .expect("listed") as u8
    }

    pub fn from_index(index: u8) -> Option<EffectKind> {
        EffectKind::ALL.get(index as usize).copied()
    }

    pub fn params(self) -> &'static [ParamInfo] {
        match self {
            EffectKind::Reverb => &REVERB,
            EffectKind::Delay => &DELAY,
            EffectKind::LowPass => &LOW_PASS,
            EffectKind::HighPass => &HIGH_PASS,
            EffectKind::Compressor => &COMPRESSOR,
            EffectKind::Limiter => &LIMITER,
            EffectKind::Chorus => &CHORUS,
            EffectKind::Saturation => &SATURATION,
            EffectKind::Bitcrusher => &BITCRUSHER,
        }
    }

    /// The index of the named parameter.
    pub fn param(self, name: &str) -> Option<usize> {
        self.params().iter().position(|p| p.name == name)
    }
}

/// An effect and its parameter values (in the order of `kind.params()`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Effect {
    pub kind: EffectKind,
    pub params: [f32; MAX_PARAMS],
}

impl Effect {
    /// The effect with its default parameters.
    pub fn new(kind: EffectKind) -> Effect {
        let mut params = [0.0; MAX_PARAMS];
        for (value, info) in params.iter_mut().zip(kind.params()) {
            *value = info.default;
        }
        Effect { kind, params }
    }

    /// Sets the named parameter (clamped to its range); false if the effect
    /// has no such parameter.
    pub fn set(&mut self, name: &str, value: f32) -> bool {
        match self.kind.param(name) {
            Some(i) => {
                self.params[i] = clamp_param(self.kind, i, value);
                true
            }
            None => false,
        }
    }

    pub fn get(&self, name: &str) -> Option<f32> {
        self.kind.param(name).map(|i| self.params[i])
    }

    /// Builds the effect as an insert, for blocks up to `max_block` frames.
    /// Build it off the audio thread.
    pub fn build(&self, sample_rate: f32, max_block: usize) -> Box<dyn Insert> {
        let p = |i: usize| clamp_param(self.kind, i, self.params[i]);
        let sr = sample_rate.max(1.0);
        let mut insert: Box<dyn Insert> = match self.kind {
            EffectKind::Reverb => node(
                self.kind,
                Freeverb::new(sr, p(0), p(1), p(2), p(3), p(4)),
                |n, i, v| match i {
                    0 => n.set_room_size(v),
                    1 => n.set_damp(v),
                    2 => n.set_wet(v),
                    3 => n.set_dry(v),
                    _ => n.set_width(v),
                },
            ),
            EffectKind::Delay => node(
                self.kind,
                Delay::new(sr, MAX_DELAY_MS, p(0), p(1), p(2)),
                |n, i, v| match i {
                    0 => n.set_delay_ms(v),
                    1 => n.set_feedback(v),
                    _ => n.set_mix(v),
                },
            ),
            EffectKind::LowPass | EffectKind::HighPass => {
                Box::new(FilterInsert::new(self.kind, sr, p(0), p(1)))
            }
            EffectKind::Compressor => node(
                self.kind,
                Compressor::new(sr, p(0), p(1), p(2), p(3), p(4), p(5)),
                |n, i, v| match i {
                    0 => n.set_threshold_db(v),
                    1 => n.set_ratio(v),
                    2 => n.set_attack_ms(v),
                    3 => n.set_release_ms(v),
                    4 => n.set_makeup_gain_db(v),
                    _ => n.set_mix(v),
                },
            ),
            EffectKind::Limiter => node(
                self.kind,
                Limiter::new(sr, p(0), 0.1, p(1), 1.5, true),
                |n, i, v| match i {
                    0 => n.set_threshold_db(v),
                    _ => n.set_release_ms(v),
                },
            ),
            EffectKind::Chorus => node(
                self.kind,
                Chorus::new(sr, MAX_CHORUS_MS, p(0), p(1), p(2), p(3), p(4), 90.0),
                |n, i, v| match i {
                    0 => n.set_base_delay_ms(v),
                    1 => n.set_depth_ms(v),
                    2 => n.set_rate_hz(v),
                    3 => n.set_feedback(v),
                    _ => n.set_mix(v),
                },
            ),
            EffectKind::Saturation => {
                node(self.kind, Saturation::new(p(0), p(1)), |n, i, v| match i {
                    0 => n.set_drive(v),
                    _ => n.set_mix(v),
                })
            }
            EffectKind::Bitcrusher => node(
                self.kind,
                Bitcrusher::new(p(0).round() as u8, p(1).round() as usize, p(2)),
                |n, i, v| match i {
                    0 => n.set_bits(v.round() as u8),
                    1 => n.set_downsample_factor(v.round() as usize),
                    _ => n.set_mix(v),
                },
            ),
        };
        // One block of silence sizes any buffers the effect grows on first use.
        let (mut left, mut right) = (vec![0.0; max_block.max(1)], vec![0.0; max_block.max(1)]);
        insert.process(&mut left, &mut right);
        insert.reset();
        insert
    }
}

fn clamp_param(kind: EffectKind, index: usize, value: f32) -> f32 {
    match kind.params().get(index) {
        Some(info) if value.is_finite() => value.clamp(info.min, info.max),
        Some(info) => info.default,
        None => 0.0,
    }
}

/// A `cubed-dsp` node as an insert, with its parameter setters.
struct NodeInsert<N> {
    kind: EffectKind,
    node: N,
    set: fn(&mut N, usize, f32),
}

fn node<N: AudioNode + StereoEffect + Send + 'static>(
    kind: EffectKind,
    node: N,
    set: fn(&mut N, usize, f32),
) -> Box<dyn Insert> {
    Box::new(NodeInsert { kind, node, set })
}

impl<N: AudioNode + StereoEffect + Send> Insert for NodeInsert<N> {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.node.process_stereo(left, right);
    }

    fn reset(&mut self) {
        AudioNode::reset(&mut self.node);
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index < self.kind.params().len() {
            (self.set)(&mut self.node, index, clamp_param(self.kind, index, value));
        }
    }
}

/// A 12 dB/octave low- or high-pass whose cutoff glides, so sweeping it
/// (a door closing, going under water) does not zip.
struct FilterInsert {
    kind: EffectKind,
    filters: [Biquad; 2],
    sample_rate: f32,
    cutoff: f32,
    target_cutoff: f32,
}

impl FilterInsert {
    fn new(kind: EffectKind, sample_rate: f32, cutoff: f32, q: f32) -> FilterInsert {
        let filter_type = if kind == EffectKind::LowPass {
            FilterType::LowPass
        } else {
            FilterType::HighPass
        };
        let filter = Biquad::new(filter_type, sample_rate, cutoff, q, 0.0);
        FilterInsert {
            kind,
            filters: [filter, filter],
            sample_rate,
            cutoff,
            target_cutoff: cutoff,
        }
    }

    fn retune(&mut self, cutoff: f32, q: f32) {
        for f in &mut self.filters {
            f.frequency = cutoff.min(self.sample_rate * 0.49);
            f.q = q;
            f.update_coefficients();
        }
    }
}

impl Insert for FilterInsert {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.cutoff != self.target_cutoff {
            // Glide on a log scale, once per block.
            let keep = (-(left.len() as f32) / (FILTER_GLIDE_SECONDS * self.sample_rate)).exp();
            let next = (self.target_cutoff.ln()
                + (self.cutoff.ln() - self.target_cutoff.ln()) * keep)
                .exp();
            self.cutoff = if (next / self.target_cutoff - 1.0).abs() < 1e-3 {
                self.target_cutoff
            } else {
                next
            };
            let q = self.filters[0].q;
            self.retune(self.cutoff, q);
        }
        let [l, r] = &mut self.filters;
        for s in left.iter_mut() {
            *s = l.process(*s);
        }
        for s in right.iter_mut() {
            *s = r.process(*s);
        }
    }

    fn reset(&mut self) {
        for f in &mut self.filters {
            Filter::reset(f);
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let value = clamp_param(self.kind, index, value);
        match index {
            0 => self.target_cutoff = value,
            1 => self.retune(self.cutoff, value),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_indices_and_defaults_round_trip() {
        for kind in EffectKind::ALL {
            assert_eq!(EffectKind::from_name(kind.name()), Some(kind));
            assert_eq!(EffectKind::from_index(kind.index()), Some(kind));
            assert!(kind.params().len() <= MAX_PARAMS);
            let effect = Effect::new(kind);
            for (i, info) in kind.params().iter().enumerate() {
                assert!(info.min <= info.default && info.default <= info.max);
                assert_eq!(effect.params[i], info.default);
            }
        }
        let mut reverb = Effect::new(EffectKind::Reverb);
        assert!(reverb.set("wet", 5.0));
        assert_eq!(reverb.get("wet"), Some(1.0), "clamped");
        assert!(!reverb.set("cutoff_hz", 100.0));
    }

    #[test]
    fn every_effect_builds_and_processes() {
        for kind in EffectKind::ALL {
            let mut insert = Effect::new(kind).build(48_000.0, 128);
            let mut left: Vec<f32> = (0..128).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
            let mut right = left.clone();
            for _ in 0..50 {
                insert.process(&mut left, &mut right);
            }
            assert!(left.iter().chain(&right).all(|s| s.is_finite()), "{kind:?}");
            for i in 0..MAX_PARAMS {
                insert.set_param(i, f32::NAN);
                insert.set_param(i, 1e9);
            }
            insert.process(&mut left, &mut right);
        }
    }

    #[test]
    fn a_low_pass_glides_and_cuts_highs() {
        let mut lp = Effect::new(EffectKind::LowPass).build(48_000.0, 128);
        lp.set_param(0, 200.0);
        let tone = |hz: f32, lp: &mut Box<dyn Insert>| {
            let mut energy = 0.0;
            for block in 0..200 {
                let mut l: Vec<f32> = (0..128)
                    .map(|i| {
                        let t = (block * 128 + i) as f32 / 48_000.0;
                        (std::f32::consts::TAU * hz * t).sin()
                    })
                    .collect();
                let mut r = l.clone();
                lp.process(&mut l, &mut r);
                if block >= 100 {
                    energy += l.iter().map(|s| s * s).sum::<f32>();
                }
            }
            (energy / (100.0 * 128.0)).sqrt()
        };
        let low = tone(50.0, &mut lp);
        let high = tone(5_000.0, &mut lp);
        assert!(low > 0.6, "{low}");
        assert!(high < 0.01, "{high}");
    }
}
