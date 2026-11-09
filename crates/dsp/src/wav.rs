//! WAV files and base64, for the audio a patch carries (`audioAssets`).
//!
//! The DAW stores a sample or a wavetable as the base64 text of a WAV file; this
//! reads such a file into floats and writes the two shapes the DAW writes:
//!
//! - [`encode_wav_i16`]: 16-bit PCM, what its `float32ArrayToWav` makes for a
//!   sampler's sample (`round(x * 32768)`, clamped), so a sample that is already
//!   on the 16-bit grid (`k / 32768`) goes through read and write unchanged;
//! - [`encode_wav_f32`]: a 32-bit float WAV, mono, 44.1 kHz, 44-byte header, what
//!   its wavetable editor exports (`encodeWavFile`).
//!
//! Reading accepts 8, 16, 24 and 32-bit integer and 32-bit float PCM (what
//! `hound` reads, including WAVE_FORMAT_EXTENSIBLE), with every integer width
//! scaled by its own full scale (`/ 2^(bits-1)`), so 16-bit data comes back as
//! `k / 32768` exactly. A damaged file is an error, never a panic.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

/// Decoded audio: interleaved floats, the channel count and the sample rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub channels: usize,
    pub rate: u32,
}

impl Pcm {
    /// Frames (a frame is one sample per channel).
    pub fn frames(&self) -> usize {
        self.samples.len().checked_div(self.channels).unwrap_or(0)
    }

    /// The largest absolute sample.
    pub fn peak(&self) -> f32 {
        self.samples.iter().fold(0.0, |peak, s| peak.max(s.abs()))
    }

    /// Duration in seconds.
    pub fn seconds(&self) -> f64 {
        if self.rate == 0 {
            return 0.0;
        }
        self.frames() as f64 / self.rate as f64
    }

    /// The channels averaged to one.
    pub fn to_mono(&self) -> Pcm {
        if self.channels <= 1 {
            return self.clone();
        }
        let n = self.channels;
        let samples = self
            .samples
            .chunks_exact(n)
            .map(|frame| frame.iter().sum::<f32>() / n as f32)
            .collect();
        Pcm {
            samples,
            channels: 1,
            rate: self.rate,
        }
    }

    /// The samples scaled so that the loudest is `peak` (silence stays silent).
    pub fn normalize(&mut self, peak: f32) {
        let current = self.peak();
        if current > 0.0 {
            let gain = peak / current;
            for s in &mut self.samples {
                *s *= gain;
            }
        }
    }

    /// Minimum and maximum of every channel over `buckets` equal parts of the
    /// frames `from..to`, as a waveform view draws them. Fewer buckets than
    /// asked for when there are fewer frames.
    pub fn peaks(&self, from: usize, to: usize, buckets: usize) -> (Vec<f32>, Vec<f32>) {
        let frames = self.frames();
        let to = to.min(frames);
        let from = from.min(to);
        let span = to - from;
        let buckets = buckets.min(span);
        let mut lows = Vec::with_capacity(buckets);
        let mut highs = Vec::with_capacity(buckets);
        for b in 0..buckets {
            let start = from + span * b / buckets;
            let end = (from + span * (b + 1) / buckets).max(start + 1);
            let mut low = f32::MAX;
            let mut high = f32::MIN;
            for frame in start..end.min(to) {
                for ch in 0..self.channels {
                    let s = self.samples[frame * self.channels + ch];
                    low = low.min(s);
                    high = high.max(s);
                }
            }
            lows.push(low);
            highs.push(high);
        }
        (lows, highs)
    }
}

/// A WAV file's samples as floats. See the module docs for what is read.
pub fn decode_wav(bytes: &[u8]) -> Result<Pcm, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a WAV file (no RIFF/WAVE header)".into());
    }
    let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("not a readable WAV file: {e}"))?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err("the WAV file has no channels".into());
    }
    let samples: Result<Vec<f32>, hound::Error> = match (spec.bits_per_sample, spec.sample_format)
    {
        (32, hound::SampleFormat::Float) => reader.samples::<f32>().collect(),
        (8, hound::SampleFormat::Int) => reader
            .samples::<i8>()
            .map(|s| s.map(|s| s as f32 / 128.0))
            .collect(),
        (16, hound::SampleFormat::Int) => reader
            .samples::<i16>()
            .map(|s| s.map(|s| s as f32 / 32768.0))
            .collect(),
        (24, hound::SampleFormat::Int) => reader
            .samples::<i32>()
            .map(|s| s.map(|s| s as f32 / 8_388_608.0))
            .collect(),
        (32, hound::SampleFormat::Int) => reader
            .samples::<i32>()
            .map(|s| s.map(|s| (s as f64 / 2_147_483_648.0) as f32))
            .collect(),
        (bits, format) => {
            return Err(format!(
                "unsupported WAV format: {bits}-bit {format:?} (8, 16, 24, 32-bit integer or 32-bit float PCM)"
            ))
        }
    };
    let samples = samples.map_err(|e| format!("the WAV data is damaged: {e}"))?;
    Ok(Pcm {
        samples,
        channels: spec.channels as usize,
        rate: spec.sample_rate,
    })
}

/// How raw PCM bytes are laid out (little endian, interleaved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawFormat {
    U8,
    I16,
    I24,
    I32,
    F32,
}

impl RawFormat {
    pub fn parse(name: &str) -> Option<RawFormat> {
        Some(match name {
            "u8" => RawFormat::U8,
            "i16" => RawFormat::I16,
            "i24" => RawFormat::I24,
            "i32" => RawFormat::I32,
            "f32" => RawFormat::F32,
            _ => return None,
        })
    }

    pub fn bytes(self) -> usize {
        match self {
            RawFormat::U8 => 1,
            RawFormat::I16 => 2,
            RawFormat::I24 => 3,
            RawFormat::I32 | RawFormat::F32 => 4,
        }
    }
}

/// Raw PCM (no header) as floats.
pub fn decode_raw(bytes: &[u8], format: RawFormat) -> Vec<f32> {
    let width = format.bytes();
    bytes
        .chunks_exact(width)
        .map(|b| match format {
            RawFormat::U8 => (b[0] as f32 - 128.0) / 128.0,
            RawFormat::I16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            RawFormat::I24 => {
                let v = (i32::from_le_bytes([0, b[0], b[1], b[2]])) >> 8;
                v as f32 / 8_388_608.0
            }
            RawFormat::I32 => {
                (i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64 / 2_147_483_648.0) as f32
            }
            RawFormat::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        })
        .collect()
}

fn header(
    out: &mut Vec<u8>,
    format_tag: u16,
    channels: usize,
    rate: u32,
    bits: u16,
    data_bytes: usize,
) {
    let block = channels as u32 * (bits as u32 / 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&format_tag.to_le_bytes());
    out.extend_from_slice(&(channels as u16).to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * block).to_le_bytes());
    out.extend_from_slice(&(block as u16).to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_bytes as u32).to_le_bytes());
}

/// The sample as a 16-bit PCM WAV file, the DAW's way: clamped to -1..1, then
/// `round(x * 32768)` (halves up, as JavaScript's `Math.round`) clamped to the
/// 16-bit range.
pub fn encode_wav_i16(samples: &[f32], channels: usize, rate: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    header(&mut out, 1, channels, rate, 16, samples.len() * 2);
    for &s in samples {
        let x = (s.clamp(-1.0, 1.0) as f64) * 32768.0;
        let v = ((x + 0.5).floor()).clamp(-32768.0, 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// A 32-bit float WAV file, mono, at `rate` (the DAW writes 44,100 Hz) with its
/// 44-byte header, as its wavetable editor's `encodeWavFile` does.
pub fn encode_wav_f32(samples: &[f32], rate: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + samples.len() * 4);
    header(&mut out, 3, 1, rate, 32, samples.len() * 4);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// How many bytes base64 text of this length (padded) decodes to.
pub fn base64_decoded_len(text: &str) -> usize {
    let padding = text.bytes().rev().take(2).filter(|b| *b == b'=').count();
    (text.len() / 4 * 3).saturating_sub(padding)
}

pub fn base64_encode(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

/// Base64 text to bytes; whitespace and a `data:...;base64,` prefix are
/// tolerated (a file read in a browser may have them).
pub fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let text = match text.find(";base64,") {
        Some(at) if text.starts_with("data:") => &text[at + 8..],
        _ => text,
    };
    let clean: String;
    let text = if text.bytes().any(|b| b.is_ascii_whitespace()) {
        clean = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        clean.as_str()
    } else {
        text
    };
    BASE64
        .decode(text.as_bytes())
        .map_err(|e| format!("not base64: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_bit_data_goes_through_unchanged() {
        // Every value of the 16-bit grid reads as k / 32768 and writes back as k.
        let grid: Vec<f32> = (-32768i32..=32767).map(|k| k as f32 / 32768.0).collect();
        let wav = encode_wav_i16(&grid, 1, 44_100);
        let pcm = decode_wav(&wav).unwrap();
        assert_eq!(pcm.samples, grid);
        assert_eq!(pcm.rate, 44_100);
        assert_eq!(encode_wav_i16(&pcm.samples, 1, 44_100), wav);
    }

    #[test]
    fn the_header_is_the_daws_for_both_encodings() {
        let wav = encode_wav_i16(&[0.0, 0.5], 2, 22_050);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 4);
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 22_050);
        assert_eq!(
            u32::from_le_bytes(wav[28..32].try_into().unwrap()),
            22_050 * 4
        );
        assert_eq!(u16::from_le_bytes(wav[32..34].try_into().unwrap()), 4);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(wav.len(), 44 + 4);
        assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 16384);

        let float = encode_wav_f32(&[0.25, -1.0], 44_100);
        assert_eq!(u16::from_le_bytes(float[20..22].try_into().unwrap()), 3);
        assert_eq!(u16::from_le_bytes(float[34..36].try_into().unwrap()), 32);
        assert_eq!(
            u32::from_le_bytes(float[28..32].try_into().unwrap()),
            44_100 * 4
        );
        assert_eq!(f32::from_le_bytes(float[44..48].try_into().unwrap()), 0.25);
        assert_eq!(decode_wav(&float).unwrap().samples, vec![0.25, -1.0]);
    }

    #[test]
    fn writing_clamps_and_rounds_like_the_daw() {
        let wav = encode_wav_i16(
            &[2.0, -2.0, 1.0, -1.0, 0.5 / 32768.0, -0.5 / 32768.0],
            1,
            8000,
        );
        let read: Vec<i16> = wav[44..]
            .chunks(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        // 1.0 is 32768 clamped to 32767; a half rounds up, as Math.round does.
        assert_eq!(read, vec![32767, -32768, 32767, -32768, 1, 0]);
    }

    #[test]
    fn other_widths_read_by_their_own_full_scale() {
        for (bits, max) in [(8u16, 127i32), (24, 8_388_607), (32, i32::MAX)] {
            let mut data = Vec::new();
            for v in [max, 0, -max - 1] {
                match bits {
                    8 => data.push((v + 128) as u8),
                    24 => data.extend_from_slice(&v.to_le_bytes()[0..3]),
                    _ => data.extend_from_slice(&v.to_le_bytes()),
                }
            }
            let mut wav = Vec::new();
            header(&mut wav, 1, 1, 8000, bits, data.len());
            wav.extend_from_slice(&data);
            let pcm = decode_wav(&wav).unwrap();
            assert_eq!(pcm.samples.len(), 3, "{bits} bit");
            assert!(
                (pcm.samples[0] - 1.0).abs() < 0.01,
                "{bits}: {:?}",
                pcm.samples
            );
            assert_eq!(pcm.samples[1], 0.0);
            assert_eq!(pcm.samples[2], -1.0, "{bits} bit");
        }
    }

    #[test]
    fn a_damaged_file_is_an_error_not_a_panic() {
        assert!(decode_wav(b"").unwrap_err().contains("not a WAV"));
        assert!(decode_wav(b"RIFFxxxxWAVEjunkjunkjunk").is_err());
        let mut wav = encode_wav_i16(&[0.1; 100], 1, 8000);
        wav.truncate(60);
        assert!(decode_wav(&wav).is_err());
        let mut odd = encode_wav_f32(&[0.1; 4], 8000);
        odd[34] = 12; // 12 bits per sample
        assert!(decode_wav(&odd).is_err());
    }

    #[test]
    fn raw_pcm_reads_by_format() {
        assert_eq!(
            decode_raw(&[0, 64, 0, 192], RawFormat::I16),
            vec![0.5, -0.5]
        );
        assert_eq!(
            decode_raw(&[128, 255], RawFormat::U8),
            vec![0.0, 127.0 / 128.0]
        );
        assert_eq!(decode_raw(&1.5f32.to_le_bytes(), RawFormat::F32), vec![1.5]);
        // A trailing partial sample is dropped.
        assert_eq!(decode_raw(&[0, 64, 7], RawFormat::I16), vec![0.5]);
        let i24 = [0x00, 0x00, 0x40, 0x00, 0x00, 0xc0];
        assert_eq!(decode_raw(&i24, RawFormat::I24), vec![0.5, -0.5]);
    }

    #[test]
    fn base64_tolerates_whitespace_and_a_data_uri() {
        let bytes: Vec<u8> = (0..=255).collect();
        let text = base64_encode(&bytes);
        assert_eq!(base64_decode(&text).unwrap(), bytes);
        let wrapped = format!("data:audio/wav;base64,{}\n", text.replace("AAEC", "AA\nEC"));
        assert_eq!(base64_decode(&wrapped).unwrap(), bytes);
        assert!(base64_decode("not base64!!")
            .unwrap_err()
            .contains("base64"));
    }

    #[test]
    fn peaks_cover_the_frames_and_never_ask_for_more_buckets_than_frames() {
        let pcm = Pcm {
            samples: (0..100).map(|n| n as f32 / 100.0).collect(),
            channels: 1,
            rate: 8000,
        };
        let (low, high) = pcm.peaks(0, 100, 4);
        assert_eq!(low, vec![0.0, 0.25, 0.5, 0.75]);
        assert_eq!(high, vec![0.24, 0.49, 0.74, 0.99]);
        let (low, _) = pcm.peaks(10, 13, 50);
        assert_eq!(low.len(), 3);
        let (low, high) = pcm.peaks(90, 400, 2);
        assert_eq!((low.len(), high.len()), (2, 2));
        let (low, _) = pcm.peaks(50, 50, 8);
        assert!(low.is_empty());
        let stereo = Pcm {
            samples: vec![0.1, -0.9, 0.2, 0.8],
            channels: 2,
            rate: 8000,
        };
        assert_eq!(stereo.peaks(0, 2, 1), (vec![-0.9], vec![0.8]));
        let mono = stereo.to_mono().samples;
        assert!((mono[0] + 0.4).abs() < 1e-6 && (mono[1] - 0.5).abs() < 1e-6);
    }
}
