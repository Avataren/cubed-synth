//! Patches built in code for the tests: a small DAW patch (one voice layout
//! cloned `voices` times) and a helper to play a note through an engine.
#![allow(dead_code)]

use cubed_dsp::audio_engine::native::AudioEngine;
use cubed_dsp::AutomationFrame;
use serde_json::{json, Value};

pub const RATE: f32 = 48_000.0;
pub const BLOCK: usize = 128;

pub const GLOBAL_FREQUENCY: &str = "00000000-0000-4000-8000-000000000001";
pub const GLIDE: &str = "00000000-0000-4000-8000-000000000002";
pub const GLOBAL_VELOCITY: &str = "00000000-0000-4000-8000-000000000003";
pub const GATE_MIXER: &str = "00000000-0000-4000-8000-000000000004";
pub const MIXER: &str = "00000000-0000-4000-8000-000000000005";
pub const OSCILLATOR: &str = "00000000-0000-4000-8000-000000000006";
pub const ENVELOPE: &str = "00000000-0000-4000-8000-000000000007";
pub const FILTER: &str = "00000000-0000-4000-8000-000000000008";

fn connection(from: &str, to: &str, target: u32, amount: f64, modulation: i32) -> Value {
    json!({
        "fromId": from, "toId": to, "target": target, "amount": amount,
        "modulationType": modulation, "modulationTransformation": 0,
    })
}

/// An oscillator through a filter to the voice mixer, with an amp envelope:
/// the DAW's default shape. Ports are the `PortId` numbers.
pub fn basic_patch(voices: usize) -> Value {
    let node = |id: &str, kind: &str, name: &str| json!({"id": id, "type": kind, "name": name});
    json!({
        "metadata": {"id": "test", "name": "Test", "version": 1},
        "synthState": {
            "layout": {
                "voiceCount": voices,
                "canonicalVoice": {
                    "id": 0,
                    "nodes": {
                        "global_frequency": [node(GLOBAL_FREQUENCY, "global_frequency", "Global Frequency")],
                        "glide": [node(GLIDE, "glide", "Glide")],
                        "global_velocity": [node(GLOBAL_VELOCITY, "global_velocity", "Global Velocity")],
                        "gatemixer": [node(GATE_MIXER, "gatemixer", "Gate Mixer")],
                        "mixer": [node(MIXER, "mixer", "Mixer")],
                        "oscillator": [node(OSCILLATOR, "oscillator", "Oscillator")],
                        "filter": [node(FILTER, "filter", "Filter")],
                        "envelope": [node(ENVELOPE, "envelope", "Amp Envelope")],
                    },
                    "connections": [
                        connection(GATE_MIXER, GLIDE, 26, 1.0, 2),
                        connection(GLOBAL_FREQUENCY, GLIDE, 0, 1.0, 2),
                        connection(GATE_MIXER, ENVELOPE, 26, 1.0, 2),
                        connection(GLIDE, FILTER, 9, 1.0, 2),
                        connection(GLIDE, OSCILLATOR, 9, 1.0, 2),
                        connection(OSCILLATOR, FILTER, 0, 1.0, 2),
                        connection(FILTER, MIXER, 0, 1.0, 2),
                        connection(ENVELOPE, MIXER, 17, 1.0, 0),
                    ],
                },
                "globalNodes": {},
            },
            "oscillators": {OSCILLATOR: {
                "id": OSCILLATOR, "phase_mod_amount": 0, "freq_mod_amount": 0,
                "detune_oct": 0, "detune_semi": 0, "detune_cents": 0, "detune": 0,
                "hard_sync": false, "gain": 0.8, "feedback_amount": 0, "waveform": 2,
                "active": true, "unison_voices": 1, "spread": 0, "wave_index": 0,
            }},
            "filters": {FILTER: {
                "id": FILTER, "cutoff": 8000, "resonance": 0.2, "keytracking": 0,
                "comb_frequency": 220, "comb_dampening": 0.5, "oversampling": 0, "gain": 0.5,
                "filter_type": 0, "filter_slope": 0, "active": true,
            }},
            "envelopes": {ENVELOPE: {
                "id": ENVELOPE, "attack": 0.005, "decay": 0.1, "sustain": 0.7, "release": 0.1,
                "active": true, "attackCurve": 0, "decayCurve": 0, "releaseCurve": 0,
            }},
            "glides": {GLIDE: {"id": GLIDE, "time": 0, "active": false}},
            "velocity": {"sensitivity": 1, "randomize": 0, "active": true},
            "noise": {"noiseType": 0, "cutoff": 1, "gain": 1, "is_enabled": false},
        },
        "audioAssets": {},
    })
}

/// Plays `frequency` for `held` blocks and then releases it for `rest` blocks
/// on voice 0, returning the left channel.
pub fn play(engine: &mut AudioEngine, frequency: f32, held: usize, rest: usize) -> Vec<f32> {
    let mut frame = AutomationFrame::with_dimensions(engine.num_voices(), 4, BLOCK);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut out = Vec::new();
    for block in 0..held + rest {
        let gate = if block < held { 1.0 } else { 0.0 };
        frame.set_voice_values(0, gate, frequency, 1.0, 1.0);
        engine.process_with_frame(&frame, 1.0, &mut l, &mut r);
        out.extend_from_slice(&l);
    }
    out
}

pub fn engine_with(patch: &Value) -> AudioEngine {
    let mut engine = AudioEngine::new_with_block_size(RATE, 8, BLOCK);
    engine
        .init_with_patch(&patch.to_string())
        .unwrap_or_else(|e| panic!("the patch loads: {e}"));
    engine
}

pub fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

pub fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

/// The DAW patches checked in for the tests (a few of the factory bank, without
/// their embedded samples), as `(name, patch)`.
pub fn daw_patches() -> Vec<(String, Value)> {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/daw_patches.json"
    ))
    .expect("the fixture bank");
    bank_patches(&text)
}

/// Every patch of the DAW's factory bank (`default-patch.json`, 38 MB), when it
/// is on this machine: `CUBED_DAW_BANK`, else `../another_synth` next to this
/// workspace. Empty when absent.
pub fn full_daw_bank() -> Vec<(String, Value)> {
    let path = std::env::var_os("CUBED_DAW_BANK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("..")
                .join("another_synth")
                .join("public")
                .join("default-patch.json")
        });
    match std::fs::read_to_string(&path) {
        Ok(text) => bank_patches(&text),
        Err(_) => {
            eprintln!("(the DAW's bank is not at {}: skipped)", path.display());
            Vec::new()
        }
    }
}

fn bank_patches(text: &str) -> Vec<(String, Value)> {
    let bank: Value = serde_json::from_str(text).expect("a bank");
    bank["patches"]
        .as_array()
        .expect("patches")
        .iter()
        .map(|p| {
            (
                p["metadata"]["name"].as_str().unwrap_or("?").to_string(),
                p.clone(),
            )
        })
        .collect()
}

/// A note held 40 blocks then released: the peak and the RMS of the left channel
/// of voice 0, and whether every sample is finite.
pub fn note_stats(engine: &mut AudioEngine) -> (f32, f32, bool) {
    let out = play(engine, 261.63, 40, 20);
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    (peak, rms(&out), out.iter().all(|s| s.is_finite()))
}
