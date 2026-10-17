//! Procedural sound effects rendered live: an oscillator (sine, square, saw,
//! triangle or noise) sweeping from one pitch to another, mixed with seeded
//! noise, low-passed and shaped by an attack, a release and an exponential
//! decay. These are the recipes the engine's `sound_generate` writes to files,
//! played without one: a voice made from plain data, so a single command
//! starts it, natively and in the AudioWorklet alike.

use std::f64::consts::TAU;

/// The oscillator of a sound effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SfxWave {
    Sine,
    Square,
    Saw,
    Triangle,
    Noise,
}

impl SfxWave {
    pub const ALL: [SfxWave; 5] = [
        SfxWave::Sine,
        SfxWave::Square,
        SfxWave::Saw,
        SfxWave::Triangle,
        SfxWave::Noise,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SfxWave::Sine => "sine",
            SfxWave::Square => "square",
            SfxWave::Saw => "saw",
            SfxWave::Triangle => "triangle",
            SfxWave::Noise => "noise",
        }
    }

    pub fn from_name(name: &str) -> Option<SfxWave> {
        SfxWave::ALL.into_iter().find(|w| w.name() == name)
    }

    pub(crate) fn index(self) -> u8 {
        SfxWave::ALL.iter().position(|&w| w == self).unwrap_or(0) as u8
    }

    pub(crate) fn from_index(index: u8) -> SfxWave {
        SfxWave::ALL
            .get(index as usize)
            .copied()
            .unwrap_or(SfxWave::Sine)
    }
}

/// The built-in recipes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SfxPreset {
    Tone,
    Blip,
    Laser,
    Explosion,
    Pickup,
    Jump,
    Hit,
    Powerup,
}

impl SfxPreset {
    pub const ALL: [SfxPreset; 8] = [
        SfxPreset::Tone,
        SfxPreset::Blip,
        SfxPreset::Laser,
        SfxPreset::Explosion,
        SfxPreset::Pickup,
        SfxPreset::Jump,
        SfxPreset::Hit,
        SfxPreset::Powerup,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SfxPreset::Tone => "tone",
            SfxPreset::Blip => "blip",
            SfxPreset::Laser => "laser",
            SfxPreset::Explosion => "explosion",
            SfxPreset::Pickup => "pickup",
            SfxPreset::Jump => "jump",
            SfxPreset::Hit => "hit",
            SfxPreset::Powerup => "powerup",
        }
    }

    pub fn from_name(name: &str) -> Option<SfxPreset> {
        SfxPreset::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The preset's sound, with attack and release fitted to its length.
    pub fn sfx(self) -> Sfx {
        let (duration, start_hz, end_hz, wave, noise_mix, decay) = match self {
            SfxPreset::Tone => (0.5, 440.0, 440.0, SfxWave::Sine, 0.0, 0.0),
            SfxPreset::Blip => (0.08, 880.0, 440.0, SfxWave::Square, 0.0, 3.0),
            SfxPreset::Laser => (0.25, 1800.0, 80.0, SfxWave::Saw, 0.1, 3.0),
            SfxPreset::Explosion => (0.9, 100.0, 25.0, SfxWave::Sine, 0.85, 5.0),
            SfxPreset::Pickup => (0.18, 880.0, 1760.0, SfxWave::Sine, 0.0, 1.0),
            SfxPreset::Jump => (0.25, 180.0, 700.0, SfxWave::Square, 0.0, 1.5),
            SfxPreset::Hit => (0.12, 160.0, 40.0, SfxWave::Triangle, 0.6, 5.0),
            SfxPreset::Powerup => (0.6, 220.0, 1320.0, SfxWave::Triangle, 0.0, 0.5),
        };
        Sfx {
            duration,
            start_hz,
            end_hz,
            wave,
            noise_mix,
            attack: (0.003f32).min(duration * 0.1),
            release: (0.03f32).min(duration * 0.25),
            decay,
            vibrato_hz: 0.0,
            vibrato_depth: 0.0,
            lowpass_hz: 0.0,
            duty_cycle: 0.5,
            seed: 0,
            level: 1.0,
        }
    }
}

/// One sound effect, as plain data. Times are in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sfx {
    pub duration: f32,
    /// The pitch sweeps exponentially from `start_hz` to `end_hz`.
    pub start_hz: f32,
    pub end_hz: f32,
    pub wave: SfxWave,
    /// How much of the sound is seeded noise, 0-1.
    pub noise_mix: f32,
    pub attack: f32,
    pub release: f32,
    /// The exponential fade over the sound's length (0: none).
    pub decay: f32,
    pub vibrato_hz: f32,
    /// A fraction of the pitch.
    pub vibrato_depth: f32,
    /// A one-pole low-pass; 0 is 40% of the sample rate.
    pub lowpass_hz: f32,
    /// The square's pulse width.
    pub duty_cycle: f32,
    /// The noise's seed: the same seed, the same noise.
    pub seed: u64,
    /// The output scale; [`Sfx::normalized`] sets it from the sound's peak.
    pub level: f32,
}

impl Default for Sfx {
    fn default() -> Self {
        SfxPreset::Tone.sfx()
    }
}

impl Sfx {
    /// The same sound with `level` set so its peak reaches `volume` at
    /// `sample_rate` (at pitch 1): rendered once to find the peak, which
    /// takes well under a millisecond for a typical effect.
    pub fn normalized(mut self, volume: f32, sample_rate: f32) -> Sfx {
        self.level = 1.0;
        let mut voice = SfxVoice::new(self, sample_rate, 1.0);
        let mut block = [0f32; 256];
        let mut peak = 0f32;
        while voice.render(&mut block) {
            peak = block.iter().fold(peak, |p, s| p.max(s.abs()));
        }
        peak = block.iter().fold(peak, |p, s| p.max(s.abs()));
        self.level = if peak > 1e-9 {
            volume.max(0.0) / peak
        } else {
            0.0
        };
        self
    }

    /// Clamped to workable ranges (a bad value cannot make a voice blow up).
    pub(crate) fn sanitized(mut self, sample_rate: f32) -> Sfx {
        let finite = |v: f32, default: f32| if v.is_finite() { v } else { default };
        let nyquist = sample_rate * 0.45;
        self.duration = finite(self.duration, 0.25).clamp(0.005, 30.0);
        self.start_hz = finite(self.start_hz, 440.0).clamp(1.0, nyquist);
        self.end_hz = finite(self.end_hz, self.start_hz).clamp(1.0, nyquist);
        self.noise_mix = finite(self.noise_mix, 0.0).clamp(0.0, 1.0);
        self.attack = finite(self.attack, 0.0).clamp(0.0, self.duration);
        self.release = finite(self.release, 0.0).clamp(0.0, self.duration);
        self.decay = finite(self.decay, 0.0).clamp(0.0, 40.0);
        self.vibrato_hz = finite(self.vibrato_hz, 0.0).clamp(0.0, 100.0);
        self.vibrato_depth = finite(self.vibrato_depth, 0.0).clamp(0.0, 0.9);
        self.lowpass_hz = finite(self.lowpass_hz, 0.0).clamp(0.0, sample_rate * 0.49);
        self.duty_cycle = finite(self.duty_cycle, 0.5).clamp(0.05, 0.95);
        self.level = finite(self.level, 0.0).clamp(0.0, 100.0);
        self
    }
}

/// The noise hash (splitmix64), the one the engine's generator uses.
fn hash(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// PolyBLEP softens the saw's and square's edges to reduce aliasing.
fn blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// A sound effect playing.
#[derive(Clone, Debug)]
pub(crate) struct SfxVoice {
    sfx: Sfx,
    rate: f64,
    /// Frames rendered and the sound's length in frames.
    frame: usize,
    frames: usize,
    phase: f64,
    filtered: f64,
    coeff: f64,
    pitch: f64,
}

impl SfxVoice {
    pub(crate) fn new(sfx: Sfx, sample_rate: f32, pitch: f32) -> SfxVoice {
        let sfx = sfx.sanitized(sample_rate);
        let rate = sample_rate.max(1.0) as f64;
        let cutoff = if sfx.lowpass_hz > 0.0 {
            sfx.lowpass_hz as f64
        } else {
            rate * 0.4
        };
        SfxVoice {
            sfx,
            rate,
            frame: 0,
            frames: ((sfx.duration as f64 * rate).round() as usize).max(2),
            phase: 0.0,
            filtered: 0.0,
            coeff: 1.0 - (-TAU * cutoff / rate).exp(),
            pitch: if pitch.is_finite() {
                pitch.clamp(0.05, 8.0) as f64
            } else {
                1.0
            },
        }
    }

    /// Writes the next `out.len()` frames (mono, zeros after the end).
    /// Returns false once the sound has ended.
    pub(crate) fn render(&mut self, out: &mut [f32]) -> bool {
        let s = &self.sfx;
        let last = (self.frames - 1) as f64;
        let nyquist = self.rate * 0.49;
        let sweep = (s.end_hz as f64 / s.start_hz as f64).ln();
        let attack = (s.attack as f64).max(0.001);
        let release = (s.release as f64).max(0.001);
        let duty = s.duty_cycle as f64;
        for sample in out.iter_mut() {
            if self.frame >= self.frames {
                *sample = 0.0;
                continue;
            }
            let i = self.frame;
            let t = i as f64 / last;
            let seconds = i as f64 / self.rate;
            let frequency = (s.start_hz as f64
                * (sweep * t).exp()
                * (1.0 + s.vibrato_depth as f64 * (TAU * s.vibrato_hz as f64 * seconds).sin())
                * self.pitch)
                .min(nyquist);
            let dt = frequency / self.rate;
            let noise = (hash(s.seed.wrapping_add(i as u64)) >> 11) as f64 / (1u64 << 53) as f64
                * 2.0
                - 1.0;
            let phase = self.phase;
            let wave = match s.wave {
                SfxWave::Sine => (TAU * phase).sin(),
                SfxWave::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
                SfxWave::Saw => 2.0 * phase - 1.0 - blep(phase, dt),
                SfxWave::Square => {
                    let raw = if phase < duty { 1.0 } else { -1.0 };
                    raw + blep(phase, dt) - blep((phase - duty).rem_euclid(1.0), dt)
                }
                SfxWave::Noise => noise,
            };
            self.phase = (phase + dt).rem_euclid(1.0);
            let mix = s.noise_mix as f64;
            self.filtered += self.coeff * (wave * (1.0 - mix) + noise * mix - self.filtered);
            let remaining = (last - i as f64) / self.rate;
            let envelope = (seconds / attack).min(1.0)
                * (remaining / release).min(1.0)
                * (-(s.decay as f64) * t).exp();
            *sample = (self.filtered * envelope) as f32 * s.level;
            self.frame += 1;
        }
        self.frame < self.frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn render_all(sfx: Sfx, pitch: f32) -> Vec<f32> {
        let mut voice = SfxVoice::new(sfx, RATE, pitch);
        let mut out = Vec::new();
        let mut block = [0f32; 128];
        loop {
            let more = voice.render(&mut block);
            out.extend_from_slice(&block);
            if !more {
                break;
            }
        }
        out
    }

    #[test]
    fn every_preset_is_audible_bounded_and_ends_at_silence() {
        for preset in SfxPreset::ALL {
            let sfx = preset.sfx().normalized(0.8, RATE);
            let out = render_all(sfx, 1.0);
            let expected = (sfx.duration * RATE).round() as usize;
            assert!(
                out.len() >= expected && out.len() < expected + 128,
                "{preset:?}"
            );
            let peak = out.iter().fold(0f32, |p, s| p.max(s.abs()));
            assert!((peak - 0.8).abs() < 1e-3, "{preset:?}: peak {peak}");
            assert!(out[0].abs() < 1e-3, "{preset:?} starts at silence");
            assert!(out[expected - 1].abs() < 1e-3, "{preset:?} ends at silence");
            assert!(out.iter().all(|s| s.is_finite()));
        }
    }

    #[test]
    fn the_seed_and_the_pitch_change_the_sound() {
        let base = SfxPreset::Explosion.sfx();
        let a = render_all(base, 1.0);
        let b = render_all(Sfx { seed: 9, ..base }, 1.0);
        assert_eq!(a, render_all(base, 1.0), "the same seed, the same noise");
        assert_ne!(a, b);
        // A higher pitch crosses zero more often.
        let tone = SfxPreset::Tone.sfx();
        let crossings = |x: &[f32]| {
            x.windows(2)
                .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
                .count()
        };
        let low = crossings(&render_all(tone, 1.0));
        let high = crossings(&render_all(tone, 2.0));
        assert!(
            (high as f32 / low as f32 - 2.0).abs() < 0.05,
            "{low} {high}"
        );
    }

    #[test]
    fn bad_values_are_tamed() {
        let wild = Sfx {
            duration: f32::NAN,
            start_hz: 1e9,
            end_hz: -5.0,
            lowpass_hz: f32::INFINITY,
            level: f32::NAN,
            ..Sfx::default()
        };
        let out = render_all(wild, f32::NAN);
        assert!(out.iter().all(|s| s.is_finite()));
    }
}
