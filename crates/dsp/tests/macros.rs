//! Macros and the instrument gain, read from the patch as the DAW applies them.
#![cfg(feature = "native-host")]

mod common;
use common::*;
use cubed_dsp::patch_edit::PatchDoc;
use cubed_dsp::traits::PortId;
use serde_json::json;

fn macro_doc(value: f64) -> PatchDoc {
    let mut doc = PatchDoc::from_value(basic_patch(2)).unwrap();
    // Macro 0 scales the voice mixer's gain (a VCA route).
    doc.set_macro_route(0, MIXER, PortId::GainMod, 1.0, 0, 0)
        .unwrap();
    doc.set_macro_value(0, value).unwrap();
    doc
}

fn render(doc: &PatchDoc) -> Vec<f32> {
    cubed_dsp::seed::set_seed(9);
    let mut engine = engine_with(doc.value());
    play(&mut engine, 220.0, 50, 5)
}

#[test]
fn a_vca_macro_scales_what_it_drives() {
    let full = rms(&render(&macro_doc(1.0)));
    let half = rms(&render(&macro_doc(0.5)));
    let none = rms(&render(&macro_doc(0.0)));
    assert!(full > 0.05, "{full}");
    assert!(
        half > 0.3 * full && half < 0.7 * full,
        "half the macro, about half the level: {}",
        half / full
    );
    assert!(none < 1e-6, "a closed macro is silent: {none}");
}

#[test]
fn a_patch_without_routes_ignores_its_macro_values() {
    let mut doc = PatchDoc::from_value(basic_patch(2)).unwrap();
    let plain = render(&doc);
    doc.set_macro_value(1, 0.9).unwrap();
    assert_eq!(render(&doc), plain);
}

#[test]
fn a_route_with_no_amount_or_no_target_is_left_out() {
    let plain = render(&PatchDoc::from_value(basic_patch(2)).unwrap());
    let mut patch = basic_patch(2);
    patch["synthState"]["macros"] = json!({
        "values": [0, 0, 0, 0],
        "routes": [
            // Closed, but its amount is 0: the DAW removes such a route.
            {"macroIndex": 0, "targetId": MIXER, "targetPort": 17, "amount": 0},
            // Nodes that are not there, an effect, a port that is not.
            {"macroIndex": 0, "targetId": "00000000-0000-4000-8000-0000000000ff", "targetPort": 17, "amount": 1},
            {"macroIndex": 0, "targetId": "10003", "targetPort": 17, "amount": 1},
            {"macroIndex": 0, "targetId": MIXER, "targetPort": 99, "amount": 1},
            {"macroIndex": 7, "targetId": MIXER, "targetPort": 17, "amount": 1},
        ],
    });
    cubed_dsp::seed::set_seed(9);
    let mut engine = engine_with(&patch);
    assert_eq!(play(&mut engine, 220.0, 50, 5), plain);
}

#[test]
fn macros_with_nulls_and_defaults_read_like_the_daw() {
    let mut patch = basic_patch(2);
    patch["synthState"]["macros"] = json!({"values": [0.25], "routes": null});
    let engine = engine_with(&patch);
    assert_eq!(engine.macro_values(), [0.25, 0.0, 0.0, 0.0]);
    // A route without a type is additive.
    patch["synthState"]["macros"] = json!({
        "values": [1, 1, 1, 1],
        "routes": [{"macroIndex": 1, "targetId": OSCILLATOR, "targetPort": 21, "amount": 0.5}],
    });
    let engine = engine_with(&patch);
    assert_eq!(engine.macro_values(), [1.0; 4]);
    patch["synthState"]["macros"] = json!({"values": [3.0, -1.0, 0.5, "x"]});
    assert!(
        cubed_dsp::audio_engine::native::AudioEngine::new(48_000.0, 1)
            .init_with_patch(&patch.to_string())
            .is_err(),
        "a value that is no number is a malformed patch"
    );
}

#[test]
fn connecting_a_route_again_replaces_it() {
    use cubed_dsp::audio_engine::native::AudioEngine;
    use cubed_dsp::graph::{ModulationTransformation as T, ModulationType as M};
    use cubed_dsp::NodeId;
    let mut patch = basic_patch(2);
    patch["synthState"]["macros"] = json!({"values": [0.5, 0, 0, 0], "routes": []});
    let mixer = NodeId::from_string(MIXER).unwrap();
    let mut once = AudioEngine::new_with_block_size(RATE, 8, BLOCK);
    once.init_with_patch(&patch.to_string()).unwrap();
    let mut twice = AudioEngine::new_with_block_size(RATE, 8, BLOCK);
    twice.init_with_patch(&patch.to_string()).unwrap();
    for voice in 0..2 {
        once.connect_macro(voice, 0, mixer, PortId::GainMod, 1.0, M::VCA, T::None)
            .unwrap();
        for _ in 0..3 {
            twice
                .connect_macro(voice, 0, mixer, PortId::GainMod, 1.0, M::VCA, T::None)
                .unwrap();
        }
    }
    cubed_dsp::seed::set_seed(9);
    let a = play(&mut once, 220.0, 30, 5);
    cubed_dsp::seed::set_seed(9);
    let b = play(&mut twice, 220.0, 30, 5);
    assert_eq!(a, b);
    // Zero removes it (the macro no longer scales anything).
    for voice in 0..2 {
        twice
            .connect_macro(voice, 0, mixer, PortId::GainMod, 0.0, M::VCA, T::None)
            .unwrap();
    }
    assert!(twice
        .connect_macro(0, 4, mixer, PortId::GainMod, 1.0, M::VCA, T::None)
        .is_err());
    assert!(twice
        .connect_macro(9, 0, mixer, PortId::GainMod, 1.0, M::VCA, T::None)
        .is_err());
}

#[test]
fn instrument_gain_is_read_and_clamped() {
    let mut doc = PatchDoc::from_value(basic_patch(2)).unwrap();
    assert_eq!(engine_with(doc.value()).instrument_gain(), 1.0);
    doc.set_instrument_gain(0.5).unwrap();
    assert_eq!(engine_with(doc.value()).instrument_gain(), 0.5);
    let mut patch = doc.into_value();
    patch["synthState"]["instrumentGain"] = json!(40.0);
    assert_eq!(engine_with(&patch).instrument_gain(), 2.0);
    patch["synthState"]["instrumentGain"] = json!(-1.0);
    assert_eq!(engine_with(&patch).instrument_gain(), 0.0);
}

/// A DAW patch with macros (Flutey: two macros drive an LFO, two the filter),
/// pinned bit for bit with the macro values it was saved with.
#[test]
fn a_daw_patch_with_macros_renders_exactly_as_pinned() {
    let patches = daw_patches();
    let (name, patch) = patches.iter().find(|(n, _)| n == "Flutey").expect("Flutey");
    assert_eq!(
        patch["synthState"]["macros"]["routes"]
            .as_array()
            .unwrap()
            .len(),
        4
    );

    let render = |patch: &serde_json::Value| {
        cubed_dsp::seed::set_seed(21);
        let mut engine = engine_with(patch);
        play(&mut engine, 293.66, 80, 20)
    };
    let as_saved = render(patch);
    println!("{name}: {:#018x}", hash(&as_saved));
    assert!(rms(&as_saved) > 0.01 && as_saved.iter().all(|s| s.is_finite()));
    assert_eq!(render(patch), as_saved, "deterministic");

    // The macros are heard: the same patch with other values is another sound.
    let mut other = patch.clone();
    other["synthState"]["macros"]["values"] = json!([0.7, 1.0, 0.2, 0.6]);
    assert!(max_abs_diff(&render(&other), &as_saved) > 0.01);
    // ... and a patch without them (routes dropped) is another sound again.
    let mut without = patch.clone();
    without["synthState"]["macros"]["routes"] = json!([]);
    assert!(max_abs_diff(&render(&without), &as_saved) > 0.01);

    assert_eq!(hash(&as_saved), 0xb64b_7745_5c08_287d);
}

/// What the wasm engine can do live that the native one could not: make a node
/// of any type, delete it, take a connection away.
#[test]
fn the_native_engine_edits_a_running_graph_like_the_wasm_one() {
    use cubed_dsp::audio_engine::native::AudioEngine;
    use cubed_dsp::NodeId;
    let patch = basic_patch(2);
    let mut engine = engine_with(&patch);
    cubed_dsp::seed::set_seed(1);
    let before = play(&mut engine, 220.0, 20, 5);
    assert!(rms(&before) > 0.01);

    for node_type in cubed_dsp::catalog::NODES.iter().map(|n| n.type_name) {
        let mut fresh = AudioEngine::new_with_block_size(RATE, 8, BLOCK);
        fresh.init_with_patch(&patch.to_string()).unwrap();
        let id = fresh.create_node(node_type).unwrap();
        let deleted = fresh.delete_node(id);
        let system = cubed_dsp::catalog::node_spec(node_type).is_some_and(|s| !s.deletable);
        if system {
            // A second one of the voice's own node types takes its place as the
            // voice's, so it is the one that is refused.
            assert!(deleted.is_err() || node_type == "mixer", "{node_type}");
        } else {
            deleted.unwrap();
        }
    }

    // The oscillator's connection to the filter taken away: silence.
    let osc = NodeId::from_string(OSCILLATOR).unwrap();
    let filter = NodeId::from_string(FILTER).unwrap();
    let mut engine = engine_with(&patch);
    engine.remove_specific_connection(osc, filter, cubed_dsp::traits::PortId::AudioInput0);
    let after = play(&mut engine, 220.0, 20, 5);
    assert!(rms(&after) < 1e-6, "{}", rms(&after));
    // The nodes of the voice are not deletable.
    assert!(engine
        .delete_node(NodeId::from_string(MIXER).unwrap())
        .is_err());
    assert!(engine
        .delete_node(NodeId::from_string(GLIDE).unwrap())
        .is_err());
    engine.delete_node(filter).unwrap();
}
