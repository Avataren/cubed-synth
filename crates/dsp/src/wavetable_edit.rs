//! Making and editing wavetables, the way the DAW's wavetable editor does.
//!
//! A wavetable oscillator plays a *table*: frames of one cycle each (the DAW
//! stores 2,048 samples a frame, up to 256 frames), and its `wave_index`
//! (0 to 1) morphs along the frames. The DAW's editor makes one from keyframes
//! (a list of harmonics, amplitude and phase, at positions 0 to 100) and
//! warp keyframes, interpolates both between the keyframes for every frame,
//! and writes the frames as one 32-bit float WAV that goes into the patch as
//! the asset `wavetable_<node id>`. This module is that code, in Rust:
//!
//! - [`generate_from_harmonics`], [`apply_wave_warp`], [`process_waveform`],
//!   [`generate_table`]: `WavetableUtils.ts` (`generateWaveformFromHarmonics`,
//!   `applyWaveWarp`, `processWaveform`, `generateWavetableWav`), in `f64` like
//!   JavaScript and stored as `f32`, so the numbers match (the tests compare
//!   them with values the TypeScript produced, `tests/fixtures/
//!   wavetable_reference.json`);
//! - [`preset`]: the editor's twelve presets (`WavetableEditor.vue`'s
//!   `handlePresetChange`), plus a few of ours;
//! - [`harmonics_of`]: the harmonics a frame is made of (the inverse of
//!   [`generate_from_harmonics`], so a frame can be edited as harmonics);
//! - [`resample_cycle`]: a cycle at another length (band limited, by FFT);
//! - [`Table`]: the frames, with the edits a table needs (insert, delete,
//!   copy, blend), and its WAV file ([`Table::to_wav`], [`Table::from_wav`]).
//!
//! `PatchDoc` (`patch_edit`) stores a [`Table`] as the patch's asset.

use std::f64::consts::PI;

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use crate::wav::{decode_wav, encode_wav_f32};

/// Samples in a frame, as the DAW's engine slices a wavetable file
/// (`import_wavetable(.., 2048)`).
pub const FRAME_SIZE: usize = 2048;
/// Frames in a table at most (the DAW generates 256).
pub const MAX_FRAMES: usize = 256;
/// The sample rate the DAW writes a wavetable file at.
pub const FILE_RATE: u32 = 44_100;

/// One harmonic: a sine of `amplitude` at `phase` radians, at its number
/// times the fundamental.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Harmonic {
    pub amplitude: f64,
    pub phase: f64,
}

/// A warp of a waveform (`WaveWarpParams`): `x_amount` bends the time axis, `y_amount`
/// the level, `asymmetric` bends the two halves separately. The DAW's sliders go from -4 to 50.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Warp {
    pub x_amount: f64,
    pub y_amount: f64,
    pub asymmetric: bool,
}

/// Harmonics at a position (0 to 100) on the table's timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Keyframe {
    pub time: f64,
    pub harmonics: Vec<Harmonic>,
}

/// A warp at a position (0 to 100).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarpKeyframe {
    pub time: f64,
    pub params: Warp,
}

fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// `findKeyframeIndices`: the keyframe at or before `position` and how far
/// (0 to 1) the way to the next one it is. `times` is not empty.
pub fn find_keyframe(position: f64, times: &[f64]) -> (usize, f64) {
    let mut index = 0;
    while index + 1 < times.len() && times[index + 1] <= position {
        index += 1;
    }
    if index + 1 >= times.len() {
        return (index, 0.0);
    }
    let range = times[index + 1] - times[index];
    if range == 0.0 {
        return (index, 0.0);
    }
    let morph = (position - times[index]) / range;
    (index, morph.clamp(0.0, 1.0))
}

/// `interpolateHarmonics`: a harmonic missing from `next` counts as zero.
fn interpolate_harmonics(current: &[Harmonic], next: &[Harmonic], amount: f64) -> Vec<Harmonic> {
    current
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let n = next.get(i).copied().unwrap_or(Harmonic {
                amplitude: 0.0,
                phase: 0.0,
            });
            Harmonic {
                amplitude: h.amplitude * (1.0 - amount) + n.amplitude * amount,
                phase: h.phase * (1.0 - amount) + n.phase * amount,
            }
        })
        .collect()
}

fn interpolate_warp(current: Warp, next: Warp, amount: f64) -> Warp {
    Warp {
        x_amount: current.x_amount * (1.0 - amount) + next.x_amount * amount,
        y_amount: current.y_amount * (1.0 - amount) + next.y_amount * amount,
        asymmetric: if amount < 0.5 {
            current.asymmetric
        } else {
            next.asymmetric
        },
    }
}

/// `generateWaveformFromHarmonics`: the sum of `amplitude * sin(2 pi (h+1) i / size + phase)`
/// over the harmonics `h`, for `i` in `0..size`. The sines come from one table
/// of the cycle (the arguments are whole steps of it plus the phase), which is
/// what makes 256 frames of 32 harmonics take milliseconds.
pub fn generate_from_harmonics(harmonics: &[Harmonic], size: usize) -> Vec<f64> {
    let mut wave = vec![0.0; size];
    if size == 0 {
        return wave;
    }
    let step = 2.0 * PI / size as f64;
    let (sin, cos): (Vec<f64>, Vec<f64>) = (0..size)
        .map(|k| {
            let a = step * k as f64;
            (a.sin(), a.cos())
        })
        .unzip();
    for (h, harmonic) in harmonics.iter().enumerate() {
        if harmonic.amplitude == 0.0 {
            continue;
        }
        let (ps, pc) = harmonic.phase.sin_cos();
        let number = h + 1;
        let mut at = 0usize;
        for sample in wave.iter_mut() {
            // sin(a + p) = sin a cos p + cos a sin p
            *sample += harmonic.amplitude * (sin[at] * pc + cos[at] * ps);
            at += number;
            at %= size;
        }
    }
    wave
}

/// `applyWaveWarp`.
pub fn apply_wave_warp(wave: &[f64], params: Warp) -> Vec<f64> {
    if params.x_amount == 0.0 && params.y_amount == 0.0 {
        return wave.to_vec();
    }
    let n = wave.len();
    if n == 0 {
        return Vec::new();
    }
    let x_exp = 1.0 + params.x_amount * 0.2;
    let y_exp = 1.0 + params.y_amount * 0.2;
    let last = (n - 1) as f64;
    (0..n)
        .map(|i| {
            let normalized = (i as f64 / n as f64) * 2.0 - 1.0;
            let mut x = normalized;
            if params.x_amount != 0.0 {
                x = if params.asymmetric {
                    if normalized >= 0.0 {
                        normalized.powf(x_exp)
                    } else {
                        -(-normalized).powf(x_exp)
                    }
                } else {
                    sign(normalized) * normalized.abs().powf(x_exp)
                };
            }
            let position = ((x + 1.0) * 0.5 * last).clamp(0.0, last);
            let index1 = position.floor() as usize;
            let index2 = (index1 + 1).min(n - 1);
            let frac = position - index1 as f64;
            let mut sample = wave[index1] * (1.0 - frac) + wave[index2] * frac;
            if params.y_amount != 0.0 {
                sample = if params.asymmetric {
                    if sample >= 0.0 {
                        sample.powf(y_exp)
                    } else {
                        -(-sample).powf(y_exp)
                    }
                } else {
                    sign(sample) * sample.abs().powf(y_exp)
                };
            }
            sample
        })
        .collect()
}

/// `processWaveform`, bug for bug: the peak that `normalize` divides by is the
/// peak of the waveform *before* the DC offset is taken off.
pub fn process_waveform(wave: &[f32], remove_dc: bool, normalize: bool) -> Vec<f32> {
    let mut out = wave.to_vec();
    if !remove_dc && !normalize {
        return out;
    }
    let mut sum = 0.0f64;
    let mut max = 0.0f32;
    for &s in &out {
        if remove_dc {
            sum += s as f64;
        }
        if normalize {
            max = max.max(s.abs());
        }
    }
    if remove_dc {
        let mean = sum / out.len().max(1) as f64;
        for s in &mut out {
            *s = (*s as f64 - mean) as f32;
        }
    }
    if normalize && max > 0.0 {
        for s in &mut out {
            *s /= max;
        }
    }
    out
}

/// `generateWarpedWaveform`: the waveform at `position` (0 to 100) of the
/// timeline, `length` samples.
pub fn waveform_at(
    position: f64,
    keyframes: &[Keyframe],
    warps: &[WarpKeyframe],
    length: usize,
) -> Vec<f64> {
    let times: Vec<f64> = keyframes.iter().map(|k| k.time).collect();
    let (hi, hm) = find_keyframe(position, &times);
    let warp_times: Vec<f64> = warps.iter().map(|k| k.time).collect();
    let (wi, wm) = find_keyframe(position, &warp_times);
    let harmonics = if hi + 1 < keyframes.len() {
        interpolate_harmonics(&keyframes[hi].harmonics, &keyframes[hi + 1].harmonics, hm)
    } else {
        keyframes[hi].harmonics.clone()
    };
    let warp = if wi + 1 < warps.len() {
        interpolate_warp(warps[wi].params, warps[wi + 1].params, wm)
    } else {
        warps[wi].params
    };
    apply_wave_warp(&generate_from_harmonics(&harmonics, length), warp)
}

/// The frames of a wavetable: `frames()` cycles of `size` samples, one after
/// the other.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub data: Vec<f32>,
    pub size: usize,
}

impl Table {
    pub fn new(size: usize) -> Table {
        Table {
            data: Vec::new(),
            size,
        }
    }

    /// A table of these frames (each `size` long).
    pub fn from_frames(frames: &[Vec<f32>], size: usize) -> Table {
        let mut data = Vec::with_capacity(frames.len() * size);
        for frame in frames {
            data.extend_from_slice(frame);
        }
        Table { data, size }
    }

    pub fn frames(&self) -> usize {
        self.data.len().checked_div(self.size).unwrap_or(0)
    }

    pub fn frame(&self, index: usize) -> &[f32] {
        &self.data[index * self.size..(index + 1) * self.size]
    }

    pub fn frame_mut(&mut self, index: usize) -> &mut [f32] {
        let size = self.size;
        &mut self.data[index * size..(index + 1) * size]
    }

    pub fn push(&mut self, frame: &[f32]) {
        debug_assert_eq!(frame.len(), self.size);
        self.data.extend_from_slice(frame);
    }

    pub fn insert(&mut self, at: usize, frame: &[f32]) {
        debug_assert_eq!(frame.len(), self.size);
        let at = at * self.size;
        self.data.splice(at..at, frame.iter().copied());
    }

    pub fn remove(&mut self, index: usize) {
        self.data.drain(index * self.size..(index + 1) * self.size);
    }

    /// The largest absolute sample.
    pub fn peak(&self) -> f32 {
        self.data.iter().fold(0.0, |p, s| p.max(s.abs()))
    }

    /// The table as the DAW writes it: a 32-bit float WAV, mono, 44.1 kHz.
    pub fn to_wav(&self) -> Vec<u8> {
        encode_wav_f32(&self.data, FILE_RATE)
    }

    /// A table from a WAV file cut into frames of `size` samples (all the
    /// channels one after the other, as the DAW's import reads them); samples
    /// after the last whole frame are dropped. At most [`MAX_FRAMES`] frames
    /// are kept; the second value says whether there were more.
    pub fn from_wav(bytes: &[u8], size: usize) -> Result<(Table, bool), String> {
        let pcm = decode_wav(bytes)?;
        Table::from_samples(pcm.samples, size)
    }

    /// As [`from_wav`](Self::from_wav), from samples already read.
    pub fn from_samples(mut samples: Vec<f32>, size: usize) -> Result<(Table, bool), String> {
        let whole = samples.len() / size.max(1);
        if whole == 0 {
            return Err(format!(
                "a wavetable needs at least one frame of {size} samples, this has {}",
                samples.len()
            ));
        }
        let truncated = whole > MAX_FRAMES;
        samples.truncate(whole.min(MAX_FRAMES) * size);
        Ok((
            Table {
                data: samples,
                size,
            },
            truncated,
        ))
    }

    /// The frame at `position` (0 to 1 over the frames), blended from the two
    /// frames either side of it as the oscillator's `wave_index` does.
    pub fn at_position(&self, position: f64) -> Vec<f32> {
        let n = self.frames();
        if n == 0 {
            return Vec::new();
        }
        let index = position.clamp(0.0, 1.0) * (n - 1) as f64;
        let low = index.floor() as usize;
        let high = (low + 1).min(n - 1);
        let mix = (index - low as f64) as f32;
        self.frame(low)
            .iter()
            .zip(self.frame(high))
            .map(|(a, b)| a + mix * (b - a))
            .collect()
    }
}

/// `generateWavetableWav`: `frames` frames of `size` samples made from the
/// keyframes, normalised and with the DC offset taken off as asked. With one
/// keyframe of each kind every frame is the same. (One frame is the waveform
/// at position 0: the DAW divides by zero there.)
pub fn generate_table(
    keyframes: &[Keyframe],
    warps: &[WarpKeyframe],
    frames: usize,
    size: usize,
    remove_dc: bool,
    normalize: bool,
) -> Result<Table, String> {
    if keyframes.is_empty() || warps.is_empty() {
        return Err("keyframes cannot be empty".into());
    }
    if frames == 0 || frames > MAX_FRAMES {
        return Err(format!(
            "a table has 1 to {MAX_FRAMES} frames, not {frames}"
        ));
    }
    if size < 4 {
        return Err(format!("a frame has at least 4 samples, not {size}"));
    }
    let make = |position: f64| -> Vec<f32> {
        let wave: Vec<f32> = waveform_at(position, keyframes, warps, size)
            .into_iter()
            .map(|x| x as f32)
            .collect();
        process_waveform(&wave, remove_dc, normalize)
    };
    let mut table = Table::new(size);
    if keyframes.len() == 1 && warps.len() == 1 {
        let one = make(0.0);
        for _ in 0..frames {
            table.push(&one);
        }
    } else {
        for i in 0..frames {
            let position = if frames > 1 {
                (i as f64 / (frames - 1) as f64) * 100.0
            } else {
                0.0
            };
            table.push(&make(position));
        }
    }
    Ok(table)
}

/// A preset of the editor: its keyframes and how many frames the editor
/// makes of it by default.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub keyframes: Vec<Keyframe>,
    pub warps: Vec<WarpKeyframe>,
}

/// The names of the presets with the DAW's labels, and whether the preset
/// moves along the table (a preset that does not is one frame, repeated).
pub const PRESETS: &[(&str, &str, bool)] = &[
    ("analogPWM", "Analog PWM", true),
    ("pulsePWM", "Pulse PWM", true),
    ("sine", "Sine", false),
    ("harmonicSeries", "Harmonic Series", true),
    ("square", "Square Wave", false),
    ("sawtooth", "Sawtooth Wave", false),
    ("triangle", "Triangle Wave", false),
    ("brightBrass", "Bright Brass", false),
    ("warmPad", "Warm Pad", true),
    ("vocalFormant", "Vocal Formant", true),
    ("metallicBell", "Metallic Bell", false),
    ("evolvingTexture", "Evolving Texture", true),
    ("pulse", "Pulse 25%", false),
    ("organ", "Organ", false),
];

/// A preset's canonical name from the name a caller gives (any case, with or
/// without `_`, `-` or spaces; `saw` is `sawtooth`, `pwm` is `analogPWM`).
pub fn preset_name(name: &str) -> Option<&'static str> {
    let flat: String = name
        .chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect();
    let flat = match flat.as_str() {
        "saw" => "sawtooth",
        "pwm" => "analogpwm",
        "squarewave" => "square",
        "trianglewave" => "triangle",
        other => other,
    };
    PRESETS
        .iter()
        .find(|(n, _, _)| n.to_lowercase() == flat)
        .map(|(n, _, _)| *n)
}

fn flat_harmonics(count: usize, f: impl Fn(usize) -> Harmonic) -> Vec<Harmonic> {
    (0..count).map(f).collect()
}

fn single(harmonics: Vec<Harmonic>) -> Vec<Keyframe> {
    vec![Keyframe {
        time: 0.0,
        harmonics,
    }]
}

fn h(amplitude: f64, phase: f64) -> Harmonic {
    Harmonic { amplitude, phase }
}

/// The editor's preset `name` with `count` harmonics (`WavetableEditor.vue`'s
/// `handlePresetChange`; `name` as [`preset_name`] reads it). Every preset
/// has one warp keyframe, no warp.
pub fn preset(name: &str, count: usize) -> Option<Preset> {
    let name = preset_name(name)?;
    let n = count;
    let keyframes = match name {
        "sine" => single(flat_harmonics(n, |i| {
            h(if i == 0 { 1.0 } else { 0.0 }, 0.0)
        })),
        "harmonicSeries" => {
            let total = 8;
            (0..total)
                .map(|i| Keyframe {
                    time: (i as f64 / (total - 1) as f64) * 100.0,
                    harmonics: flat_harmonics(n, |j| h(if j == i { 1.0 } else { 0.0 }, 0.0)),
                })
                .collect()
        }
        "square" => single(flat_harmonics(n, |i| {
            let k = i + 1;
            h(if k % 2 == 1 { 1.0 / k as f64 } else { 0.0 }, 0.0)
        })),
        "sawtooth" => single(flat_harmonics(n, |i| {
            let k = i + 1;
            h(1.0 / k as f64, if k % 2 == 0 { PI } else { 0.0 })
        })),
        "triangle" => single(flat_harmonics(n, |i| {
            let k = i + 1;
            if k % 2 == 1 {
                h(1.0 / (k * k) as f64, if k % 4 == 1 { 0.0 } else { PI })
            } else {
                h(0.0, 0.0)
            }
        })),
        "pulsePWM" => {
            let total = 256;
            let (start, end) = (PI, PI / 50.0);
            (0..total)
                .map(|i| {
                    let t = i as f64 / (total - 1) as f64;
                    let delta = start * (1.0 - t) + end * t;
                    Keyframe {
                        time: t * 100.0,
                        harmonics: flat_harmonics(n, |j| {
                            let k = (j + 1) as f64;
                            let real = 1.0 - (k * delta).cos();
                            let imag = -(k * delta).sin();
                            h((real * real + imag * imag).sqrt() / k, imag.atan2(real))
                        }),
                    }
                })
                .collect()
        }
        "analogPWM" => {
            let total = 256;
            let (start, end) = (0.5, 0.01);
            (0..total)
                .map(|i| {
                    let t = i as f64 / (total - 1) as f64;
                    let duty = start * (1.0 - t) + end * t;
                    Keyframe {
                        time: t * 100.0,
                        harmonics: flat_harmonics(n, |j| {
                            let k = (j + 1) as f64;
                            h((2.0 * (k * PI * duty).sin()) / (k * PI), -k * PI * duty)
                        }),
                    }
                })
                .collect()
        }
        "brightBrass" => single(flat_harmonics(n, |i| {
            if i == 0 {
                h(0.5, 0.0)
            } else if i <= 4 {
                h(1.0, 0.0)
            } else {
                h(0.5 / (i + 1) as f64, 0.0)
            }
        })),
        "warmPad" => {
            let total = 8;
            (0..total)
                .map(|i| {
                    let t = i as f64 / (total - 1) as f64;
                    Keyframe {
                        time: t * 100.0,
                        harmonics: flat_harmonics(n, |j| {
                            if j == 0 {
                                h(1.0, 0.0)
                            } else {
                                h(t * (1.0 / (j + 1) as f64), 0.0)
                            }
                        }),
                    }
                })
                .collect()
        }
        "vocalFormant" => {
            let formant = |full: [usize; 2], half: [usize; 2]| {
                flat_harmonics(n, |i| {
                    if full.contains(&i) {
                        h(1.0, 0.0)
                    } else if half.contains(&i) {
                        h(0.5, 0.0)
                    } else {
                        h(0.0, 0.0)
                    }
                })
            };
            vec![
                Keyframe {
                    time: 0.0,
                    harmonics: formant([0, 1], [2, 3]),
                },
                Keyframe {
                    time: 50.0,
                    harmonics: formant([1, 2], [0, 3]),
                },
                Keyframe {
                    time: 100.0,
                    harmonics: formant([2, 3], [1, 4]),
                },
            ]
        }
        "metallicBell" => single(flat_harmonics(n, |i| {
            h(1.0 / (i + 1) as f64, (i as f64 * 2.3).sin() * 0.3)
        })),
        "evolvingTexture" => {
            let total = 8;
            (0..total)
                .map(|i| Keyframe {
                    time: (i as f64 / (total - 1) as f64) * 100.0,
                    harmonics: flat_harmonics(n, |j| {
                        h(
                            0.5 + 0.5
                                * (2.0 * PI * (i as f64 / total as f64 + j as f64 / n as f64))
                                    .sin(),
                            0.0,
                        )
                    }),
                })
                .collect()
        }
        // Ours: a pulse of a quarter of the cycle, and drawbars (1, 2, 3, 4, 6 and 8 times).
        "pulse" => single(flat_harmonics(n, |j| {
            let k = (j + 1) as f64;
            h((2.0 * (k * PI * 0.25).sin()) / (k * PI), -k * PI * 0.25)
        })),
        "organ" => single(flat_harmonics(n, |j| {
            let bar = match j + 1 {
                1 => 1.0,
                2 => 0.8,
                3 => 0.55,
                4 => 0.45,
                6 => 0.3,
                8 => 0.2,
                _ => 0.0,
            };
            h(bar, 0.0)
        })),
        _ => return None,
    };
    Some(Preset {
        keyframes,
        warps: vec![WarpKeyframe {
            time: 0.0,
            params: Warp::default(),
        }],
    })
}

/// The four waveforms of the oscillator's built-in table (sine, triangle, saw,
/// square: what it plays until it has a table of its own), as frames.
pub fn builtin_table() -> Table {
    use crate::nodes::{Waveform, WavetableBank};
    let mut table = Table::new(FRAME_SIZE);
    for waveform in [
        Waveform::Sine,
        Waveform::Triangle,
        Waveform::Saw,
        Waveform::Square,
    ] {
        let bank = WavetableBank::new(waveform, FRAME_SIZE, FILE_RATE as f32)
            .expect("the built-in waveforms have harmonics");
        // The first table of a bank has every harmonic (and a wrap-around sample).
        table.push(&bank.tables[0].samples[..FRAME_SIZE]);
    }
    table
}

/// The first `count` harmonics of a frame, as [`generate_from_harmonics`] makes
/// frames: `frame[i] = sum of amplitude * sin(2 pi (h+1) i / n + phase)`. The
/// DC offset and the harmonics above `count` are left out. Direct sums (the
/// counts are small), so there is no FFT size to match.
pub fn harmonics_of(frame: &[f32], count: usize) -> Vec<Harmonic> {
    let n = frame.len();
    if n == 0 {
        return Vec::new();
    }
    let count = count.min(n / 2);
    let step = 2.0 * PI / n as f64;
    let (sin, cos): (Vec<f64>, Vec<f64>) = (0..n)
        .map(|k| {
            let a = step * k as f64;
            (a.sin(), a.cos())
        })
        .unzip();
    (1..=count)
        .map(|number| {
            let (mut a, mut b) = (0.0f64, 0.0f64);
            let mut at = 0usize;
            for &x in frame {
                a += x as f64 * cos[at];
                b += x as f64 * sin[at];
                at += number;
                at %= n;
            }
            let (a, b) = (2.0 * a / n as f64, 2.0 * b / n as f64);
            // amplitude * sin(t + p) = amplitude sin p * cos t + amplitude cos p * sin t
            Harmonic {
                amplitude: a.hypot(b),
                phase: if a == 0.0 && b == 0.0 {
                    0.0
                } else {
                    a.atan2(b)
                },
            }
        })
        .collect()
}

/// A cycle at another length: the harmonics up to the shorter length's
/// Nyquist, so a smooth cycle comes through exactly and a longer one loses
/// what the shorter cannot hold.
pub fn resample_cycle(frame: &[f32], to: usize) -> Vec<f32> {
    let n = frame.len();
    if n == to || n == 0 || to == 0 {
        return frame.to_vec();
    }
    let mut planner = FftPlanner::<f64>::new();
    let mut spectrum: Vec<Complex<f64>> =
        frame.iter().map(|&x| Complex::new(x as f64, 0.0)).collect();
    planner.plan_fft_forward(n).process(&mut spectrum);
    let mut out = vec![Complex::new(0.0, 0.0); to];
    let keep = n.min(to);
    // Every harmonic that fits below the shorter length's Nyquist, with its mirror.
    for k in 0..=(keep - 1) / 2 {
        out[k] = spectrum[k];
        if k > 0 {
            out[to - k] = spectrum[n - k];
        }
    }
    // The shorter length's own Nyquist bin: split between the two mirrors going up, dropped
    // going down (it cannot be told from its alias).
    if keep % 2 == 0 && n < to {
        let half = spectrum[n / 2] * 0.5;
        out[n / 2] = half;
        out[to - n / 2] = half;
    }
    planner.plan_fft_inverse(to).process(&mut out);
    out.iter().map(|c| (c.re / n as f64) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn a_harmonic_is_a_sine_at_its_phase() {
        let wave = generate_from_harmonics(&[h(1.0, 0.0), h(0.5, 0.3), h(0.25, -1.0)], 16);
        // The values the DAW's generateWaveformFromHarmonics gives (node, f64).
        let expected = [
            -0.06260764287130435,
            0.8692173675714407,
            1.4290403417242192,
            1.2998230861306124,
        ];
        for (got, want) in wave.iter().zip(expected) {
            assert!(close(*got, want, 1e-12), "{got} vs {want}");
        }
    }

    #[test]
    fn harmonics_come_back_out_of_the_frame_they_made() {
        let harmonics = vec![h(0.9, 0.4), h(0.0, 0.0), h(0.3, -2.0), h(0.1, 3.0)];
        let wave = generate_from_harmonics(&harmonics, 512);
        let frame: Vec<f32> = wave.iter().map(|&x| x as f32).collect();
        let back = harmonics_of(&frame, 6);
        assert_eq!(back.len(), 6);
        for (want, got) in harmonics.iter().zip(&back) {
            assert!(
                close(want.amplitude, got.amplitude, 1e-5),
                "{want:?} {got:?}"
            );
            if want.amplitude > 0.0 {
                assert!(close(want.phase, got.phase, 1e-4), "{want:?} {got:?}");
            }
        }
        assert!(back[4].amplitude < 1e-5 && back[5].amplitude < 1e-5);
    }

    #[test]
    fn resampling_a_smooth_cycle_keeps_its_shape_both_ways() {
        let wave = generate_from_harmonics(&[h(1.0, 0.0), h(0.5, 1.0), h(0.2, 2.0)], 256);
        let frame: Vec<f32> = wave.iter().map(|&x| x as f32).collect();
        let up = resample_cycle(&frame, 2048);
        assert_eq!(up.len(), 2048);
        let exact = generate_from_harmonics(&[h(1.0, 0.0), h(0.5, 1.0), h(0.2, 2.0)], 2048);
        for (got, want) in up.iter().zip(&exact) {
            assert!(close(*got as f64, *want, 1e-4), "{got} vs {want}");
        }
        let down = resample_cycle(&up, 64);
        let exact64 = generate_from_harmonics(&[h(1.0, 0.0), h(0.5, 1.0), h(0.2, 2.0)], 64);
        for (got, want) in down.iter().zip(&exact64) {
            assert!(close(*got as f64, *want, 1e-4), "{got} vs {want}");
        }
        assert_eq!(resample_cycle(&frame, 256), frame);
    }

    #[test]
    fn find_keyframe_walks_the_timeline_like_the_daws() {
        let times = [0.0, 50.0, 100.0];
        assert_eq!(find_keyframe(0.0, &times), (0, 0.0));
        assert_eq!(find_keyframe(25.0, &times), (0, 0.5));
        assert_eq!(find_keyframe(50.0, &times), (1, 0.0));
        assert_eq!(find_keyframe(75.0, &times), (1, 0.5));
        assert_eq!(find_keyframe(100.0, &times), (2, 0.0));
        assert_eq!(find_keyframe(250.0, &times), (2, 0.0));
        // Two keyframes at one time: no division by zero.
        assert_eq!(find_keyframe(10.0, &[10.0, 10.0, 20.0]), (1, 0.0));
        assert_eq!(find_keyframe(5.0, &[7.0]), (0, 0.0));
    }

    #[test]
    fn a_table_edits_and_round_trips_through_its_wav() {
        let mut table = Table::from_frames(&[vec![0.0, 1.0, 0.0, -1.0], vec![0.5; 4]], 4);
        assert_eq!(table.frames(), 2);
        table.insert(1, &[0.25; 4]);
        assert_eq!(table.frame(1), &[0.25; 4]);
        assert_eq!(table.frames(), 3);
        table.remove(0);
        assert_eq!(table.frame(0), &[0.25; 4]);
        let (back, truncated) = Table::from_wav(&table.to_wav(), 4).unwrap();
        assert_eq!((back.clone(), truncated), (table.clone(), false));
        // Samples after the last whole frame are dropped; too few is an error.
        let (cut, _) = Table::from_wav(&encode_wav_f32(&[0.1; 10], 44_100), 4).unwrap();
        assert_eq!(cut.frames(), 2);
        assert!(Table::from_wav(&encode_wav_f32(&[0.1; 3], 44_100), 4)
            .unwrap_err()
            .contains("at least one frame"));
        // More than 256 frames are cut and say so.
        let (long, truncated) = Table::from_samples(vec![0.0; 300 * 4], 4).unwrap();
        assert_eq!((long.frames(), truncated), (256, true));
        // The position blends the neighbours, as wave_index does.
        let blend = table.at_position(0.5);
        assert_eq!(blend, vec![0.375; 4]);
        assert_eq!(table.at_position(0.0), table.frame(0));
        assert_eq!(table.at_position(7.0), table.frame(1));
    }

    #[test]
    fn preset_names_are_forgiving_and_every_preset_makes_a_table() {
        assert_eq!(preset_name("Analog_PWM"), Some("analogPWM"));
        assert_eq!(preset_name("saw"), Some("sawtooth"));
        assert_eq!(preset_name("harmonic series"), Some("harmonicSeries"));
        assert_eq!(preset_name("nope"), None);
        for (name, _, animated) in PRESETS {
            let p = preset(name, 32).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(p.keyframes.len() > 1, *animated, "{name}");
            let table = generate_table(&p.keyframes, &p.warps, 8, 128, false, true).unwrap();
            assert_eq!(table.frames(), 8);
            assert!((table.peak() - 1.0).abs() < 1e-5, "{name} is normalised");
        }
    }

    #[test]
    fn generating_refuses_what_makes_no_table() {
        let p = preset("sine", 8).unwrap();
        assert!(generate_table(&[], &p.warps, 4, 64, false, true).is_err());
        assert!(generate_table(&p.keyframes, &[], 4, 64, false, true).is_err());
        assert!(generate_table(&p.keyframes, &p.warps, 0, 64, false, true).is_err());
        assert!(generate_table(&p.keyframes, &p.warps, 257, 64, false, true).is_err());
        assert!(generate_table(&p.keyframes, &p.warps, 4, 3, false, true).is_err());
        // One frame of a moving preset is its start (the DAW divides by zero).
        let moving = preset("pulsePWM", 16).unwrap();
        let one = generate_table(&moving.keyframes, &moving.warps, 1, 64, false, false).unwrap();
        assert_eq!(one.frames(), 1);
        assert!(one.data.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn the_builtin_table_is_four_waveforms() {
        let table = builtin_table();
        assert_eq!((table.frames(), table.size), (4, FRAME_SIZE));
        // Sine: one cycle, peak a quarter of the way.
        let sine = table.frame(0);
        assert!(
            (sine[FRAME_SIZE / 4] - 1.0).abs() < 1e-3,
            "{}",
            sine[FRAME_SIZE / 4]
        );
        // Square: positive then negative.
        let square = table.frame(3);
        assert!(square[FRAME_SIZE / 4] > 0.5 && square[3 * FRAME_SIZE / 4] < -0.5);
    }
}
