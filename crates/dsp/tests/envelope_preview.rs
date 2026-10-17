use cubed_dsp::audio_engine::native::AudioEngine;
use cubed_dsp::nodes::{Envelope, EnvelopeConfig};
use serde_json::json;

#[test]
fn wasm_envelope_preview_contract_has_attack_decay_release() {
    // This mirrors the exact JSON shape sent by the UI through the worklet.
    let js_config = json!({
        "active": true,
        "attack": 0.01,
        "decay": 0.1,
        "sustain": 0.5,
        "release": 0.1,
        "attackCurve": 0.0,
        "decayCurve": 0.0,
        "releaseCurve": 0.0,
    });

    let engine = AudioEngine::new(48_000.0, 1);
    let preview = engine
        .preview_envelope_from_js(&serde_json::to_vec(&js_config).unwrap(), 0.5)
        .unwrap();

    assert!(preview.len() >= 1000);
    assert!(preview[0] < 0.05, "attack should start near zero");
    assert!(
        preview.iter().any(|&v| v > 0.9),
        "preview should reach attack peak"
    );
    assert!(
        preview.iter().any(|&v| (v - 0.5).abs() < 0.05),
        "preview should reach sustain level"
    );
}

#[test]
fn envelope_preview_directly_matches_expected_shape() {
    let config = EnvelopeConfig {
        attack: 0.01,
        decay: 0.1,
        sustain: 0.5,
        release: 0.1,
        attack_curve: 0.0,
        decay_curve: 0.0,
        release_curve: 0.0,
        active: true,
        attack_smoothing_samples: 16,
    };
    let envelope = Envelope::new(48_000.0, config);
    let preview = envelope.preview(0.5);

    let max = preview.iter().fold(0.0f32, |a, b| a.max(*b));
    let min = preview.iter().fold(1.0f32, |a, b| a.min(*b));

    assert!(max > 0.9);
    assert!(min < 0.02);
    assert_ne!(preview.first(), preview.last());
}
