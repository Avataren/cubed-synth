//! Shared WAV sample decoding for the wavetable, impulse-response and sampler
//! imports in `wasm.rs`. Lives outside `wasm.rs` (which only compiles for
//! wasm32) so its unit tests run under native `cargo test`.

/// Decode every sample of a WAV into f32, whatever its bit depth/format.
/// Errors (e.g. a truncated `data` chunk) are returned instead of panicking:
/// with `panic = "abort"` a panic here traps the whole shared worklet.
pub(crate) fn read_wav_samples_f32<R: std::io::Read>(
    reader: &mut hound::WavReader<R>,
) -> Result<Vec<f32>, String> {
    let spec = reader.spec();
    let samples: Result<Vec<f32>, hound::Error> = match (spec.bits_per_sample, spec.sample_format) {
        (32, hound::SampleFormat::Float) => reader.samples::<f32>().collect(),
        (16, hound::SampleFormat::Int) => reader
            .samples::<i16>()
            .map(|s| s.map(|s| s as f32 / i16::MAX as f32))
            .collect(),
        (24, hound::SampleFormat::Int) => {
            let shift = 32 - 24;
            reader
                .samples::<i32>()
                .map(|s| s.map(|s| (s << shift >> shift) as f32 / 8_388_607.0))
                .collect()
        }
        (32, hound::SampleFormat::Int) => reader
            .samples::<i32>()
            .map(|s| s.map(|s| s as f32 / i32::MAX as f32))
            .collect(),
        (bits, format) => {
            return Err(format!(
                "Unsupported WAV format: bits_per_sample={} sample_format={:?}",
                bits, format
            ))
        }
    };
    samples.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::read_wav_samples_f32;
    use std::io::Cursor;

    /// Minimal mono 16-bit PCM WAV whose `data` chunk declares `declared_bytes`
    /// but carries only `payload`.
    fn wav_16bit_mono(declared_bytes: u32, payload: &[u8]) -> Vec<u8> {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + declared_bytes).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // channels
        wav.extend_from_slice(&44_100u32.to_le_bytes()); // sample rate
        wav.extend_from_slice(&88_200u32.to_le_bytes()); // byte rate
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&declared_bytes.to_le_bytes());
        wav.extend_from_slice(payload);
        wav
    }

    #[test]
    fn truncated_data_chunk_is_an_error_not_a_panic() {
        // Declares 50 samples, carries 2 and a half.
        let wav = wav_16bit_mono(100, &[0x00, 0x40, 0xff, 0x7f, 0x01]);
        let mut reader = hound::WavReader::new(Cursor::new(wav)).expect("header parses");
        assert!(read_wav_samples_f32(&mut reader).is_err());
    }

    #[test]
    fn complete_16bit_data_keeps_the_i16_max_scale() {
        let wav = wav_16bit_mono(4, &[0xff, 0x7f, 0x01, 0x80]);
        let mut reader = hound::WavReader::new(Cursor::new(wav)).expect("header parses");
        let samples = read_wav_samples_f32(&mut reader).expect("complete WAV decodes");
        assert_eq!(samples, vec![1.0, -32767.0 / 32767.0]);
    }
}
