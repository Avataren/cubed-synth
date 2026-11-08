//! `fromPort`: a patch connection names the output port it leaves its source
//! by; patches from before it existed load as they did.
#![cfg(feature = "native-host")]

mod common;
use common::*;
use serde_json::{json, Value};

fn stereo_oscillator_patch(from_port: Option<u32>) -> Value {
    let mut patch = basic_patch(2);
    // Unison with a spread, so the oscillator's left and right outputs differ.
    patch["synthState"]["oscillators"][OSCILLATOR]["unison_voices"] = json!(5);
    patch["synthState"]["oscillators"][OSCILLATOR]["spread"] = json!(80);
    let connections = patch["synthState"]["layout"]["canonicalVoice"]["connections"]
        .as_array_mut()
        .unwrap();
    let osc_to_filter = connections
        .iter_mut()
        .find(|c| c["fromId"] == OSCILLATOR && c["toId"] == FILTER)
        .unwrap();
    if let Some(port) = from_port {
        osc_to_filter["fromPort"] = json!(port);
    }
    patch
}

fn render(from_port: Option<u32>) -> Vec<f32> {
    cubed_dsp::seed::set_seed(5);
    let mut engine = engine_with(&stereo_oscillator_patch(from_port));
    play(&mut engine, 220.0, 60, 10)
}

#[test]
fn the_named_port_picks_the_oscillators_left_or_right_output() {
    let left = render(Some(4));
    let right = render(Some(5));
    assert!(rms(&left) > 0.01 && rms(&right) > 0.01, "both are heard");
    assert!(
        max_abs_diff(&left, &right) > 0.01,
        "the two outputs are not the same sound"
    );
}

#[test]
fn a_connection_without_a_port_takes_the_first_output_as_before() {
    let unnamed = render(None);
    let left = render(Some(4));
    let right = render(Some(5));
    // The old rule picks one of the two, always the same one.
    assert!(unnamed == left || unnamed == right);
    assert_eq!(unnamed, render(None), "deterministic");
}

#[test]
fn a_port_the_node_does_not_output_is_refused() {
    cubed_dsp::seed::set_seed(5);
    // Port 11 is `Frequency`, an input; 99 is no port.
    for bad in [11, 99] {
        let mut engine =
            cubed_dsp::audio_engine::native::AudioEngine::new_with_block_size(RATE, 8, BLOCK);
        let err = engine
            .init_with_patch(&stereo_oscillator_patch(Some(bad)).to_string())
            .expect_err("a bad source port is an error");
        assert!(err.contains("port"), "{err}");
    }
}
