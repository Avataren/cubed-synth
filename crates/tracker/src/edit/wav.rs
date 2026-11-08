//! Reading and writing sample files, and resampling: what loading a sample
//! into a module needs. WAV (PCM 8/16/24/32-bit, float 32/64, any channel
//! count; its `smpl` loop) and raw PCM.

use crate::module::LoopKind;

type Res<T> = Result<T, String>;

/// Decoded audio: mono frames, -1 to 1, and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Pcm {
    pub rate: u32,
    pub data: Vec<f32>,
    /// A loop the file carries (start frame, length, kind).
    pub loop_: Option<(usize, usize, LoopKind)>,
}

/// How a stereo (or wider) source becomes mono.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChannelMix {
    /// The average of all channels.
    #[default]
    Mix,
    Left,
    Right,
}

impl ChannelMix {
    pub fn parse(text: &str) -> Option<ChannelMix> {
        match text {
            "mix" | "mixdown" | "average" => Some(ChannelMix::Mix),
            "left" | "l" => Some(ChannelMix::Left),
            "right" | "r" => Some(ChannelMix::Right),
            _ => None,
        }
    }

    fn fold(self, frame: &[f32]) -> f32 {
        match self {
            ChannelMix::Mix => frame.iter().sum::<f32>() / frame.len() as f32,
            ChannelMix::Left => frame[0],
            ChannelMix::Right => frame[frame.len() - 1],
        }
    }
}

fn le16(b: &[u8], at: usize) -> usize {
    u16::from_le_bytes([b[at], b[at + 1]]) as usize
}

fn le32(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
}

/// Whether `bytes` start like a WAV file.
pub fn looks_like_wav(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

/// Decodes a WAV file to mono.
pub fn decode_wav(bytes: &[u8], mix: ChannelMix) -> Res<Pcm> {
    if !looks_like_wav(bytes) {
        return Err("not a WAV file".into());
    }
    let (mut format, mut channels, mut rate, mut bits) = (0usize, 0usize, 0u32, 0usize);
    let mut data: Option<&[u8]> = None;
    let mut loop_ = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = le32(bytes, at + 4);
        let start = at + 8;
        let end = bytes.len().min(start.saturating_add(size));
        let body = &bytes[start..end];
        match id {
            b"fmt " if body.len() >= 16 => {
                format = le16(body, 0);
                channels = le16(body, 2);
                rate = le32(body, 4) as u32;
                bits = le16(body, 14);
                // WAVE_FORMAT_EXTENSIBLE: the real tag opens the sub-format.
                if format == 0xfffe && body.len() >= 26 {
                    format = le16(body, 24);
                }
            }
            b"data" => data = Some(body),
            b"smpl" if body.len() >= 36 + 24 && le32(body, 28) >= 1 => {
                let kind = match le32(body, 36 + 4) {
                    1 => LoopKind::PingPong,
                    _ => LoopKind::Forward,
                };
                let (s, e) = (le32(body, 36 + 8), le32(body, 36 + 12));
                if e >= s {
                    loop_ = Some((s, e - s + 1, kind));
                }
            }
            _ => {}
        }
        at = start.saturating_add(size).saturating_add(size & 1);
    }
    let data = data.ok_or("the WAV has no data chunk")?;
    if channels == 0 || rate == 0 {
        return Err("the WAV has no format chunk".into());
    }
    let width = bits / 8;
    let supported = matches!((format, bits), (1, 8 | 16 | 24 | 32) | (3, 32 | 64));
    if !supported {
        return Err(format!(
            "WAV format {format} with {bits} bits is not supported (PCM 8/16/24/32 and float 32/64 are)"
        ));
    }
    let frame_bytes = width * channels;
    let frames = data.len() / frame_bytes;
    let mut out = Vec::with_capacity(frames);
    let mut frame = vec![0f32; channels];
    for f in 0..frames {
        for (c, slot) in frame.iter_mut().enumerate() {
            let p = f * frame_bytes + c * width;
            *slot = match (format, bits) {
                (1, 8) => (data[p] as f32 - 128.0) / 128.0,
                (1, 16) => i16::from_le_bytes([data[p], data[p + 1]]) as f32 / 32768.0,
                (1, 24) => {
                    let v = ((data[p + 2] as i32) << 24
                        | (data[p + 1] as i32) << 16
                        | (data[p] as i32) << 8)
                        >> 8;
                    v as f32 / 8_388_608.0
                }
                (1, 32) => {
                    i32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as f32
                        / 2_147_483_648.0
                }
                (3, 32) => f32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]),
                _ => f64::from_le_bytes(data[p..p + 8].try_into().unwrap()) as f32,
            };
        }
        out.push(mix.fold(&frame).clamp(-1.0, 1.0));
    }
    let loop_ = loop_.filter(|&(s, l, _)| l >= 2 && s + l <= out.len());
    Ok(Pcm {
        rate,
        data: out,
        loop_,
    })
}

/// Raw PCM bytes to mono: `bits` 8 (signed, or unsigned) or 16 (signed,
/// little-endian), `channels` interleaved.
pub fn decode_raw(
    bytes: &[u8],
    bits: u8,
    unsigned: bool,
    channels: usize,
    mix: ChannelMix,
) -> Res<Vec<f32>> {
    if channels == 0 {
        return Err("channels must be at least 1".into());
    }
    let width = match bits {
        8 => 1,
        16 => 2,
        _ => return Err("raw PCM is 8 or 16 bits".into()),
    };
    let frame_bytes = width * channels;
    let mut out = Vec::with_capacity(bytes.len() / frame_bytes);
    let mut frame = vec![0f32; channels];
    for f in 0..bytes.len() / frame_bytes {
        for (c, slot) in frame.iter_mut().enumerate() {
            let p = f * frame_bytes + c * width;
            *slot = if width == 1 {
                if unsigned {
                    (bytes[p] as f32 - 128.0) / 128.0
                } else {
                    bytes[p] as i8 as f32 / 128.0
                }
            } else {
                i16::from_le_bytes([bytes[p], bytes[p + 1]]) as f32 / 32768.0
            };
        }
        out.push(mix.fold(&frame));
    }
    Ok(out)
}

/// A 16-bit mono WAV.
pub fn encode_wav(data: &[f32], rate: u32) -> Vec<u8> {
    let bytes = data.len() * 2;
    let mut out = Vec::with_capacity(44 + bytes);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + bytes) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(bytes as u32).to_le_bytes());
    for &v in data {
        let q = (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        out.extend_from_slice(&q.to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// resampling

/// How a sample's rate is changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    /// Linear interpolation (what the DAW does; aliases when lowering the
    /// rate a lot).
    Linear,
    /// Windowed sinc with a low-pass at the new Nyquist.
    #[default]
    Sinc,
}

impl Quality {
    pub fn parse(text: &str) -> Option<Quality> {
        match text {
            "linear" => Some(Quality::Linear),
            "sinc" | "best" | "high" => Some(Quality::Sinc),
            _ => None,
        }
    }
}

/// Frames of `data` (recorded at `from` Hz) at `to` Hz.
pub fn resample(data: &[f32], from: f64, to: f64, quality: Quality) -> Vec<f32> {
    if data.is_empty() || from <= 0.0 || to <= 0.0 {
        return Vec::new();
    }
    if (from - to).abs() < 1e-9 {
        return data.to_vec();
    }
    let ratio = from / to;
    let out_len = ((data.len() as f64) / ratio).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    match quality {
        Quality::Linear => {
            for i in 0..out_len {
                let at = i as f64 * ratio;
                let i0 = at.floor() as usize;
                let frac = (at - i0 as f64) as f32;
                let a = data.get(i0).copied().unwrap_or(0.0);
                let b = data.get(i0 + 1).copied().unwrap_or(a);
                out.push(a * (1.0 - frac) + b * frac);
            }
        }
        Quality::Sinc => {
            // The cutoff drops below the source Nyquist when the rate falls;
            // the window spans a fixed number of cutoff periods.
            let cutoff = (1.0 / ratio).min(1.0);
            let half = 12.0 / cutoff;
            for i in 0..out_len {
                let at = i as f64 * ratio;
                let first = (at - half).ceil().max(0.0) as usize;
                let last = ((at + half).floor() as usize).min(data.len() - 1);
                let (mut sum, mut weight) = (0.0f64, 0.0f64);
                for j in first..=last {
                    let x = j as f64 - at;
                    let t = x * cutoff;
                    let sinc = if t.abs() < 1e-12 {
                        1.0
                    } else {
                        (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
                    };
                    // Blackman window across the kernel.
                    let w = (x / half + 1.0) * 0.5;
                    let window = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * w).cos()
                        + 0.08 * (4.0 * std::f64::consts::PI * w).cos();
                    let k = cutoff * sinc * window;
                    sum += data[j] as f64 * k;
                    weight += k;
                }
                // Normalised so a constant stays constant at the edges.
                out.push(if weight.abs() > 1e-9 {
                    (sum / weight) as f32
                } else {
                    0.0
                });
            }
        }
    }
    out
}

/// Rounds onto a bit depth's grid (8 or 16 bits), clamping to range.
pub fn quantize_to(data: &mut [f32], bits: u8) {
    let scale = if bits == 8 { 128.0 } else { 32768.0 };
    for v in data {
        *v = (*v * scale).round().clamp(-scale, scale - 1.0) / scale;
    }
}

/// Scales so the loudest value reaches `target` (a peak of 0 stays silent).
pub fn normalize(data: &mut [f32], target: f32) {
    let peak = data.iter().fold(0f32, |p, v| p.max(v.abs()));
    if peak > 0.0 {
        let gain = target / peak;
        for v in data {
            *v *= gain;
        }
    }
}
