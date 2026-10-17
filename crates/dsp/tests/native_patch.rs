//! DAW patches on the native engine: every node type the wasm engine builds
//! (the sampler included), their saved settings, and the samples embedded as
//! audio assets.
#![cfg(feature = "native-host")]

use cubed_dsp::audio_engine::native::AudioEngine;
use cubed_dsp::AutomationFrame;

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;

fn real_patch() -> serde_json::Value {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/real_patch.json"
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

/// A 16-bit mono WAV of a 3 kHz square wave, quiet at the end.
fn square_wav(rate: u32, frames: usize) -> Vec<u8> {
    let data_len = (frames * 2) as u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    let period = rate as usize / 3_000;
    for i in 0..frames {
        let v: i16 = if (i / (period / 2).max(1)).is_multiple_of(2) {
            12_000
        } else {
            -12_000
        };
        wav.extend_from_slice(&v.to_le_bytes());
    }
    wav
}

fn base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// One note held for a quarter second and released: the left channel.
fn render_note(engine: &mut AudioEngine) -> Vec<f32> {
    let voices = engine.num_voices();
    let mut frame = AutomationFrame::with_dimensions(voices, 4, BLOCK);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut out = Vec::new();
    for block in 0..200 {
        let gate = if block < 90 { 1.0 } else { 0.0 };
        frame.set_voice_values(0, gate, 261.63, 1.0, 1.0);
        engine.process_with_frame(&frame, 1.0, &mut l, &mut r);
        out.extend_from_slice(&l);
    }
    out
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
}

#[test]
fn a_daw_patch_with_a_sampler_loads_and_plays() {
    let mut engine = AudioEngine::new_with_block_size(RATE, 8, BLOCK);
    let voices = engine
        .init_with_patch(&real_patch().to_string())
        .expect("the patch loads");
    assert_eq!(voices, 8);
    let out = render_note(&mut engine);
    assert!(rms(&out) > 0.01, "the note is heard: {}", rms(&out));
    assert!(out.iter().all(|s| s.is_finite()));
}

#[test]
fn a_patch_sample_replaces_the_placeholder() {
    let plain = {
        let mut engine = AudioEngine::new_with_block_size(RATE, 8, BLOCK);
        engine.init_with_patch(&real_patch().to_string()).unwrap();
        render_note(&mut engine)
    };

    let mut patch = real_patch();
    let sampler_id = patch["synthState"]["samplers"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let asset_id = format!("sample_{sampler_id}");
    patch["audioAssets"][&asset_id] = serde_json::json!({
        "id": asset_id,
        "type": "sample",
        "base64Data": base64(&square_wav(44_100, 44_100)),
        "sampleRate": 44_100.0,
        "channels": 1,
    });
    let mut engine = AudioEngine::new_with_block_size(RATE, 8, BLOCK);
    engine.init_with_patch(&patch.to_string()).unwrap();
    let with_sample = render_note(&mut engine);

    assert!(rms(&with_sample) > 0.01);
    let difference: Vec<f32> = plain.iter().zip(&with_sample).map(|(a, b)| a - b).collect();
    assert!(
        rms(&difference) > 0.01,
        "the imported sample changes the sound: {}",
        rms(&difference)
    );
}

#[test]
fn a_broken_asset_is_an_error() {
    let mut patch = real_patch();
    patch["audioAssets"]["sample_x"] = serde_json::json!({
        "id": "sample_cde4c116-b5f7-9097-8e19-9c75371de51b",
        "type": "sample",
        "base64Data": base64(b"not a wav"),
        "sampleRate": 44_100.0,
        "channels": 1,
    });
    let mut engine = AudioEngine::new_with_block_size(RATE, 8, BLOCK);
    assert!(engine.init_with_patch(&patch.to_string()).is_err());
}

/// The real patch with an embedded sample, pinned bit for bit, so reworking
/// how its nodes use memory cannot change its sound.
#[test]
fn the_real_patch_renders_exactly_as_before() {
    cubed_dsp::seed::set_seed(11);
    let mut patch = real_patch();
    let sampler_id = patch["synthState"]["samplers"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let asset_id = format!("sample_{sampler_id}");
    patch["audioAssets"][&asset_id] = serde_json::json!({
        "id": asset_id,
        "type": "sample",
        "base64Data": base64(&square_wav(44_100, 44_100)),
        "sampleRate": 44_100.0,
        "channels": 1,
    });
    let mut engine = AudioEngine::new_with_block_size(RATE, 8, BLOCK);
    engine.init_with_patch(&patch.to_string()).unwrap();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for _ in 0..3 {
        for s in render_note(&mut engine) {
            for byte in s.to_bits().to_le_bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    assert_eq!(hash, 0x4bc0_c3bc_e994_f196);
}
