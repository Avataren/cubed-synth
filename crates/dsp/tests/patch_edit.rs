//! Editing patch documents: the operations, their refusals, and DAW patches taken
//! through edits, saved and loaded by the real loader.
#![cfg(feature = "native-host")]

mod common;
use common::*;
use cubed_dsp::catalog::{node_spec, MACRO_COUNT};
use cubed_dsp::patch_edit::PatchDoc;
use cubed_dsp::traits::PortId;
use serde_json::{json, Value};

fn ids_of(doc: &PatchDoc, type_name: &str) -> Vec<String> {
    doc.nodes()
        .into_iter()
        .filter(|n| n.type_name == type_name)
        .map(|n| n.id)
        .collect()
}

fn one(doc: &PatchDoc, type_name: &str) -> String {
    let ids = ids_of(doc, type_name);
    assert_eq!(ids.len(), 1, "one {type_name}");
    ids.into_iter().next().unwrap()
}

#[test]
fn a_new_patch_is_valid_loads_and_plays() {
    let doc = PatchDoc::new("Init");
    assert_eq!(doc.validate(), Vec::<String>::new());
    assert_eq!(doc.name(), "Init");
    assert_eq!(doc.voice_count(), 8);
    let types: Vec<_> = doc.nodes().into_iter().map(|n| n.type_name).collect();
    for expected in [
        "global_frequency",
        "glide",
        "global_velocity",
        "gatemixer",
        "mixer",
        "oscillator",
        "envelope",
        "chorus",
        "bitcrusher",
    ] {
        assert!(types.iter().any(|t| t == expected), "{expected}");
    }
    assert_eq!(doc.effects().len(), 8);
    assert_eq!(doc.effect_chain()[0], "10000");
    assert!(doc.editor().is_none());

    cubed_dsp::seed::set_seed(3);
    let mut engine = engine_with(doc.value());
    let (peak, rms, finite) = note_stats(&mut engine);
    assert!(finite && peak > 0.05 && rms > 0.01, "peak {peak} rms {rms}");
}

#[test]
fn adding_a_node_gives_it_its_state_and_the_engines_wires() {
    let mut doc = PatchDoc::new("t");
    let env = doc.add_node("envelope").unwrap();
    assert!(uuid_like(&env));
    let info = doc.node(&env).unwrap();
    assert_eq!(info.type_name, "envelope");
    assert_eq!(info.name, "Envelope 2", "a second envelope is numbered");
    // Default state, with the catalogue's defaults.
    assert_eq!(doc.get_param(&env, "decay").unwrap(), json!(0.1));
    assert_eq!(doc.node_state(&env).unwrap()["id"], json!(env));
    // The gate wire the engine makes.
    let gate = one(&doc, "gatemixer");
    let wire = doc
        .connection(&gate, PortId::CombinedGate, &env, PortId::CombinedGate)
        .expect("the gate reaches the envelope");
    assert_eq!(wire.modulation_type, 2);

    let osc = doc.add_node("oscillator").unwrap();
    let glide = one(&doc, "glide");
    assert!(doc
        .connection(&glide, PortId::AudioOutput0, &osc, PortId::GlobalFrequency)
        .is_some());
    assert_eq!(doc.validate(), Vec::<String>::new());

    let filter = doc.add_node("filter").unwrap();
    assert!(doc
        .connection(
            &glide,
            PortId::AudioOutput0,
            &filter,
            PortId::GlobalFrequency
        )
        .is_some());
}

fn uuid_like(id: &str) -> bool {
    id.len() == 36 && id.matches('-').count() == 4
}

#[test]
fn every_node_type_can_be_added_loaded_and_removed() {
    for spec in cubed_dsp::catalog::NODES.iter().filter(|n| n.deletable) {
        let mut doc = PatchDoc::new("t");
        let id = doc.add_node(spec.type_name).unwrap();
        assert_eq!(doc.validate(), Vec::<String>::new(), "{}", spec.type_name);
        let mut engine = engine_with(doc.value());
        let _ = note_stats(&mut engine);
        doc.remove_node(&id).unwrap();
        assert!(doc.node(&id).is_none());
        assert_eq!(doc.validate(), Vec::<String>::new());
        engine_with(doc.value());
    }
}

#[test]
fn a_patch_has_one_noise_node_and_cannot_lose_its_system_nodes() {
    let mut doc = PatchDoc::new("t");
    let noise = doc.add_node("noise").unwrap();
    let err = doc.add_node("noise").unwrap_err();
    assert!(err.contains("once"), "{err}");
    doc.remove_node(&noise).unwrap();
    doc.add_node("noise").expect("room again");

    for system in [
        "global_frequency",
        "glide",
        "global_velocity",
        "gatemixer",
        "mixer",
    ] {
        let id = one(&doc, system);
        let err = doc.remove_node(&id).unwrap_err();
        assert!(err.contains("cannot be removed"), "{system}: {err}");
        assert!(doc.node(&id).is_some());
        // ... and a second one is refused as well.
        assert!(doc.add_node(system).unwrap_err().contains("once"));
    }
    assert!(doc
        .add_node("flux_capacitor")
        .unwrap_err()
        .contains("unknown node type"));
    assert!(doc
        .remove_node("nope")
        .unwrap_err()
        .contains("no such node"));
}

#[test]
fn removing_a_node_removes_what_belongs_to_it() {
    let mut doc = PatchDoc::new("t");
    let lfo = doc.add_node("lfo").unwrap();
    let osc = one(&doc, "oscillator");
    doc.connect(
        &lfo,
        PortId::AudioOutput0,
        &osc,
        PortId::DetuneMod,
        0.5,
        2,
        0,
    )
    .unwrap();
    doc.set_macro_route(1, &lfo, PortId::GainMod, 0.5, 0, 0)
        .unwrap();
    assert_eq!(doc.macro_routes().len(), 1);
    let before = doc.connections().len();
    doc.remove_node(&lfo).unwrap();
    assert!(doc
        .connections()
        .iter()
        .all(|c| c.from_id != lfo && c.to_id != lfo));
    assert!(doc.connections().len() < before);
    assert!(doc.macro_routes().is_empty());
    assert!(doc.value()["synthState"]["lfos"].get(&lfo).is_none());
}

#[test]
fn parameters_are_checked_clamped_and_typed() {
    let mut doc = PatchDoc::new("t");
    let osc = one(&doc, "oscillator");
    let set = doc.set_param(&osc, "gain", &json!(0.25)).unwrap();
    assert_eq!(set.value, json!(0.25));
    let edit = set.edit.unwrap();
    let index = node_spec("oscillator")
        .unwrap()
        .param_index("gain")
        .unwrap();
    assert_eq!(edit.param as usize, index);
    assert_eq!(edit.value, 0.25);
    assert_eq!(edit.node, uuid::Uuid::parse_str(&osc).unwrap().as_u128());

    assert_eq!(
        doc.set_param(&osc, "gain", &json!(7)).unwrap().value,
        json!(1)
    );
    assert_eq!(
        doc.set_param(&osc, "unison_voices", &json!(3.6))
            .unwrap()
            .value,
        json!(4)
    );
    assert_eq!(doc.get_param(&osc, "unison_voices").unwrap(), json!(4));
    assert_eq!(
        doc.set_param(&osc, "waveform", &json!(2)).unwrap().value,
        json!(2)
    );
    assert_eq!(
        doc.set_param(&osc, "hard_sync", &json!(true))
            .unwrap()
            .value,
        json!(true)
    );
    assert_eq!(
        doc.set_param(&osc, "hard_sync", &json!(true))
            .unwrap()
            .edit
            .unwrap()
            .value,
        1.0
    );

    let err = doc.set_param(&osc, "gain", &json!("loud")).unwrap_err();
    assert!(err.contains("number"), "{err}");
    let err = doc.set_param(&osc, "waveform", &json!(9)).unwrap_err();
    assert!(err.contains("option index"), "{err}");
    let err = doc.set_param(&osc, "cutoff", &json!(1)).unwrap_err();
    assert!(
        err.contains("no parameter 'cutoff'") && err.contains("gain"),
        "{err}"
    );
    assert!(doc
        .set_param("nope", "gain", &json!(1))
        .unwrap_err()
        .contains("no such node"));
    // A refused change leaves the document alone.
    assert_eq!(doc.get_param(&osc, "waveform").unwrap(), json!(2));
}

#[test]
fn detune_parts_keep_the_total_in_step() {
    let mut doc = PatchDoc::new("t");
    let osc = one(&doc, "oscillator");
    doc.set_param(&osc, "detune_oct", &json!(-2)).unwrap();
    doc.set_param(&osc, "detune_semi", &json!(7)).unwrap();
    doc.set_param(&osc, "detune_cents", &json!(-30)).unwrap();
    assert_eq!(
        doc.node_state(&osc).unwrap()["detune"],
        json!(-2 * 1200 + 700 - 30)
    );
    // The sampler's frequency follows.
    let sampler = doc.add_node("sampler").unwrap();
    doc.set_param(&sampler, "detune_oct", &json!(1)).unwrap();
    let f = doc.node_state(&sampler).unwrap()["frequency"]
        .as_f64()
        .unwrap();
    assert!((f - 880.0).abs() < 1e-9, "{f}");
}

#[test]
fn single_state_nodes_and_effects_take_parameters() {
    let mut doc = PatchDoc::new("t");
    let noise = doc.add_node("noise").unwrap();
    doc.set_param(&noise, "is_enabled", &json!(true)).unwrap();
    doc.set_param(&noise, "noiseType", &json!(2)).unwrap();
    assert_eq!(doc.value()["synthState"]["noise"]["noiseType"], json!(2));
    assert_eq!(
        doc.value()["synthState"]["noise"]["is_enabled"],
        json!(true)
    );

    let velocity = one(&doc, "global_velocity");
    doc.set_param(&velocity, "sensitivity", &json!(0.5))
        .unwrap();
    assert_eq!(
        doc.value()["synthState"]["velocity"]["sensitivity"],
        json!(0.5)
    );
    assert_eq!(doc.value()["synthState"]["velocity"]["active"], json!(true));

    let delay = one(&doc, "delay");
    assert_eq!(delay, "10001");
    let set = doc.set_param(&delay, "delayMs", &json!(300)).unwrap();
    assert_eq!(set.edit.unwrap().node, 10001);
    assert_eq!(
        doc.value()["synthState"]["delays"]["10001"]["delayMs"],
        json!(300)
    );
    // The limiter has nothing to set.
    let limiter = one(&doc, "limiter");
    assert!(doc
        .set_param(&limiter, "threshold", &json!(1))
        .unwrap_err()
        .contains("no parameter"));
}

#[test]
fn patch_level_values_come_back_as_edits() {
    let mut doc = PatchDoc::new("t");
    assert_eq!(doc.instrument_gain(), 1.0);
    let set = doc.set_instrument_gain(1.5).unwrap();
    assert_eq!(set.edit.unwrap(), cubed_dsp::live::ParamEdit::gain(1.5));
    assert_eq!(doc.instrument_gain(), 1.5);
    assert_eq!(doc.set_instrument_gain(9.0).unwrap().value, json!(2));

    let set = doc.set_macro_value(2, 0.75).unwrap();
    assert_eq!(
        set.edit.unwrap(),
        cubed_dsp::live::ParamEdit::macro_value(2, 0.75)
    );
    assert_eq!(doc.macro_values(), [0.0, 0.0, 0.75, 0.0]);
    assert!(doc.set_macro_value(MACRO_COUNT, 0.5).is_err());

    doc.set_voice_count(4).unwrap();
    assert_eq!(doc.voice_count(), 4);
    assert!(doc.set_voice_count(0).is_err() && doc.set_voice_count(99).is_err());
}

#[test]
fn connections_are_an_upsert_with_reasons_for_refusals() {
    let mut doc = PatchDoc::new("t");
    let osc = one(&doc, "oscillator");
    let mixer = one(&doc, "mixer");
    let env = one(&doc, "envelope");
    let lfo = doc.add_node("lfo").unwrap();
    let filter = doc.add_node("filter").unwrap();

    // New connection.
    let c = doc
        .connect(
            &lfo,
            PortId::AudioOutput0,
            &filter,
            PortId::CutoffMod,
            0.3,
            2,
            1,
        )
        .unwrap();
    assert_eq!((c.amount, c.modulation_type, c.transform), (0.3, 2, 1));
    let count = doc.connections().len();
    // The same ports again change the connection.
    let c = doc
        .connect(
            &lfo,
            PortId::AudioOutput0,
            &filter,
            PortId::CutoffMod,
            0.6,
            1,
            2,
        )
        .unwrap();
    assert_eq!((c.amount, c.modulation_type, c.transform), (0.6, 1, 2));
    assert_eq!(doc.connections().len(), count, "an upsert");
    // Amount alone.
    let c = doc
        .set_amount(&lfo, PortId::AudioOutput0, &filter, PortId::CutoffMod, 0.9)
        .unwrap();
    assert_eq!((c.amount, c.modulation_type), (0.9, 1));
    // Two sources into one input are allowed (they add).
    doc.connect(
        &env,
        PortId::AudioOutput0,
        &filter,
        PortId::CutoffMod,
        1.0,
        0,
        0,
    )
    .unwrap();

    let refusals: Vec<(Result<_, String>, &str)> = vec![
        (
            doc.connect(
                &osc,
                PortId::AudioOutput0,
                &osc,
                PortId::PhaseMod,
                1.0,
                2,
                0,
            ),
            "feed itself",
        ),
        (
            doc.connect(
                "nope",
                PortId::AudioOutput0,
                &osc,
                PortId::PhaseMod,
                1.0,
                2,
                0,
            ),
            "no such node",
        ),
        (
            doc.connect(
                &lfo,
                PortId::AudioOutput1,
                &osc,
                PortId::PhaseMod,
                1.0,
                2,
                0,
            ),
            "no output",
        ),
        (
            doc.connect(
                &lfo,
                PortId::AudioOutput0,
                &osc,
                PortId::CutoffMod,
                1.0,
                2,
                0,
            ),
            "no input",
        ),
        (
            doc.connect(
                &lfo,
                PortId::AudioOutput0,
                &osc,
                PortId::GlobalFrequency,
                1.0,
                2,
                0,
            ),
            "wired by the engine",
        ),
        (
            doc.connect(
                &lfo,
                PortId::AudioOutput0,
                &mixer,
                PortId::AudioInput0,
                1.0,
                2,
                0,
            ),
            "cannot be heard",
        ),
        (
            doc.connect(&lfo, PortId::AudioOutput0, &osc, PortId::GainMod, 1.0, 5, 0),
            "modulation type",
        ),
        (
            doc.connect(&lfo, PortId::AudioOutput0, &osc, PortId::GainMod, 1.0, 2, 9),
            "transform",
        ),
        (
            doc.connect(
                &lfo,
                PortId::AudioOutput0,
                &osc,
                PortId::GainMod,
                f32::NAN,
                2,
                0,
            ),
            "finite",
        ),
    ];
    for (result, fragment) in refusals {
        let err = result.expect_err(fragment);
        assert!(
            err.contains(fragment),
            "'{err}' should mention '{fragment}'"
        );
    }
    // The audio of an oscillator may modulate (FM), and an LFO may drive its pitch.
    doc.connect(
        &osc,
        PortId::AudioOutput0,
        &filter,
        PortId::AudioInput0,
        1.0,
        2,
        0,
    )
    .unwrap();
    doc.connect(
        &filter,
        PortId::AudioOutput0,
        &mixer,
        PortId::AudioInput0,
        1.0,
        2,
        0,
    )
    .unwrap();
    assert_eq!(doc.validate(), Vec::<String>::new());
}

#[test]
fn a_loop_is_refused() {
    let mut doc = PatchDoc::new("t");
    let osc = one(&doc, "oscillator");
    let osc2 = doc.add_node("oscillator").unwrap();
    let osc3 = doc.add_node("oscillator").unwrap();
    doc.connect(
        &osc,
        PortId::AudioOutput0,
        &osc2,
        PortId::PhaseMod,
        1.0,
        2,
        0,
    )
    .unwrap();
    doc.connect(
        &osc2,
        PortId::AudioOutput0,
        &osc3,
        PortId::PhaseMod,
        1.0,
        2,
        0,
    )
    .unwrap();
    let err = doc
        .connect(
            &osc3,
            PortId::AudioOutput0,
            &osc,
            PortId::PhaseMod,
            1.0,
            2,
            0,
        )
        .unwrap_err();
    assert!(err.contains("loop"), "{err}");
    let err = doc
        .connect(
            &osc2,
            PortId::AudioOutput1,
            &osc,
            PortId::FeedbackMod,
            1.0,
            2,
            0,
        )
        .unwrap_err();
    assert!(err.contains("loop"), "{err}");
    // Not a loop: the other way round.
    doc.connect(
        &osc,
        PortId::AudioOutput0,
        &osc3,
        PortId::FrequencyMod,
        1.0,
        1,
        0,
    )
    .unwrap();
}

#[test]
fn disconnect_removes_one_connection_and_never_an_engine_wire() {
    let mut doc = PatchDoc::new("t");
    let osc = one(&doc, "oscillator");
    let mixer = one(&doc, "mixer");
    let count = doc.connections().len();
    assert!(doc
        .disconnect(&osc, PortId::AudioOutput0, &mixer, PortId::AudioInput0)
        .unwrap());
    assert_eq!(doc.connections().len(), count - 1);
    assert!(!doc
        .disconnect(&osc, PortId::AudioOutput0, &mixer, PortId::AudioInput0)
        .unwrap());
    let glide = one(&doc, "glide");
    let err = doc
        .disconnect(&glide, PortId::AudioOutput0, &osc, PortId::GlobalFrequency)
        .unwrap_err();
    assert!(err.contains("wired by the engine"), "{err}");
}

#[test]
fn a_connection_from_before_from_port_is_the_same_connection() {
    let mut doc = PatchDoc::from_value(basic_patch(2)).unwrap();
    let osc = OSCILLATOR;
    let filter = FILTER;
    let before = doc.connections().len();
    let legacy = doc
        .connection(osc, PortId::AudioOutput1, filter, PortId::AudioInput0)
        .unwrap();
    assert_eq!(legacy.from_port, None);
    let c = doc
        .connect(
            osc,
            PortId::AudioOutput1,
            filter,
            PortId::AudioInput0,
            0.5,
            2,
            0,
        )
        .unwrap();
    assert_eq!(c.from_port, Some(PortId::AudioOutput1));
    assert_eq!(doc.connections().len(), before, "it took the old one");
}

#[test]
fn effects_are_added_removed_and_reordered_in_the_chain() {
    let mut doc = PatchDoc::new("t");
    assert!(
        doc.value()["synthState"].get("effectChain").is_none(),
        "the default chain has no key"
    );
    let names = |doc: &PatchDoc| {
        doc.effects()
            .into_iter()
            .map(|e| e.type_name)
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&doc)[..3], ["chorus", "delay", "freeverb"]);

    doc.move_effect(1, 0).unwrap();
    assert_eq!(names(&doc)[..3], ["delay", "chorus", "freeverb"]);
    assert_eq!(doc.effect_chain()[..2], ["10001", "10000"]);
    doc.move_effect(0, 1).unwrap();
    assert!(
        doc.value()["synthState"].get("effectChain").is_none(),
        "back to the default"
    );
    assert!(doc.move_effect(0, 8).is_err());

    let second_delay = doc.add_node("delay").unwrap();
    assert_eq!(second_delay, "10008");
    assert_eq!(doc.effect_chain().last().unwrap(), &second_delay);
    assert_eq!(
        doc.value()["synthState"]["delays"]["10008"]["delayMs"],
        json!(250)
    );
    doc.set_param(&second_delay, "feedback", &json!(0.2))
        .unwrap();

    doc.remove_node("10003").unwrap();
    assert!(!doc.effect_chain().contains(&"10003".to_string()));
    assert!(doc.value()["synthState"]["convolvers"]
        .get("10003")
        .is_none());
    assert_eq!(names(&doc).len(), 8);
    // The id of the removed one is free again.
    assert_eq!(doc.add_node("convolver").unwrap(), "10003");
    assert_eq!(doc.validate(), Vec::<String>::new());
}

#[test]
fn macro_routes_are_an_upsert_and_an_amount_of_zero_removes() {
    let mut doc = PatchDoc::new("t");
    let filter = doc.add_node("filter").unwrap();
    doc.set_macro_route(0, &filter, PortId::CutoffMod, 0.5, 2, 0)
        .unwrap();
    doc.set_macro_route(0, &filter, PortId::CutoffMod, 0.8, 1, 0)
        .unwrap();
    doc.set_macro_route(1, &filter, PortId::ResonanceMod, 0.8, 2, 0)
        .unwrap();
    let routes = doc.macro_routes();
    assert_eq!(routes.len(), 2);
    assert_eq!((routes[0].amount, routes[0].modulation_type), (0.8, 1));
    doc.set_macro_route(0, &filter, PortId::CutoffMod, 0.0, 2, 0)
        .unwrap();
    assert_eq!(doc.macro_routes().len(), 1);
    assert!(doc.remove_macro_route(1, &filter, PortId::ResonanceMod));
    assert!(!doc.remove_macro_route(1, &filter, PortId::ResonanceMod));
    assert!(doc
        .set_macro_route(0, "nope", PortId::CutoffMod, 1.0, 2, 0)
        .is_err());
    assert!(doc
        .set_macro_route(0, &filter, PortId::AudioOutput0, 1.0, 2, 0)
        .unwrap_err()
        .contains("no input"));
    assert!(doc
        .set_macro_route(9, &filter, PortId::CutoffMod, 1.0, 2, 0)
        .is_err());
}

#[test]
fn the_editor_key_survives_edits_and_a_round_trip() {
    let mut doc = PatchDoc::new("t");
    let editor = json!({"view": {"x": 10.5, "y": -3, "zoom": 1.25}, "nodes": {"a": {"x": 1, "y": 2}}, "future": [1, "two", null]});
    doc.set_editor(editor.clone());
    let osc = doc.add_node("oscillator").unwrap();
    doc.set_param(&osc, "gain", &json!(0.1)).unwrap();
    doc.remove_node(&osc).unwrap();
    doc.move_effect(0, 3).unwrap();
    let text = doc.to_json();
    let again = PatchDoc::from_json(&text).unwrap();
    assert_eq!(again.editor(), Some(&editor));
    assert_eq!(again.value(), doc.value());
    // The loader does not mind it.
    let mut engine = engine_with(again.value());
    note_stats(&mut engine);
}

#[test]
fn json_operations_do_what_the_methods_do() {
    let mut doc = PatchDoc::new("t");
    let id = doc
        .apply(&json!({"op": "add_node", "type": "lfo"}))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let osc = one(&doc, "oscillator");
    let result = doc
        .apply(
            &json!({"op": "connect", "from": id, "from_port": "AudioOutput0",
                       "to": osc, "to_port": "FrequencyMod", "amount": 0.25}),
        )
        .unwrap();
    assert_eq!(
        result["connection"]["modulationType"],
        json!(1),
        "the port's default"
    );
    assert_eq!(result["connection"]["fromPort"], json!(4));
    // Only the amount given changes.
    let result = doc
        .apply(&json!({"op": "connect", "from": id, "from_port": 4,
                       "to": osc, "to_port": 12, "modulation_type": "additive"}))
        .unwrap();
    assert_eq!(result["connection"]["amount"], json!(0.25));
    assert_eq!(result["connection"]["modulationType"], json!(2));

    let result = doc
        .apply(&json!({"op": "set_param", "id": id, "key": "frequency", "value": 5}))
        .unwrap();
    assert_eq!(result["value"], json!(5));
    assert_eq!(
        result["edit"]["node"],
        json!(uuid::Uuid::parse_str(&id).unwrap().as_u128().to_string())
    );
    let result = doc
        .apply(
            &json!({"op": "disconnect", "from": id, "from_port": "AudioOutput0",
                       "to": osc, "to_port": "FrequencyMod"}),
        )
        .unwrap();
    assert_eq!(result["removed"], json!(true));
    doc.apply(&json!({"op": "set_editor", "editor": {"a": 1}}))
        .unwrap();
    assert_eq!(doc.editor(), Some(&json!({"a": 1})));
    doc.apply(&json!({"op": "move_effect", "from": 0, "to": 1}))
        .unwrap();
    doc.apply(&json!({"op": "remove_node", "id": id})).unwrap();
    assert!(doc
        .apply(&json!({"op": "teleport"}))
        .unwrap_err()
        .contains("unknown operation"));
    assert!(doc
        .apply(&json!({"nothing": 1}))
        .unwrap_err()
        .contains("'op'"));
    assert!(doc
        .apply(
            &json!({"op": "connect", "from": osc, "from_port": "Bogus", "to": osc, "to_port": 0})
        )
        .unwrap_err()
        .contains("not a port"));
}

// --- DAW patches ---------------------------------------------------------------------

/// Edits a DAW patch the way the node editor does, saves it, loads it with the
/// real loader and plays a note. Returns the notes' loudness before and after.
fn edit_session(name: &str, patch: &Value) -> (f32, f32) {
    // The same random numbers every time an engine is built.
    let stats = |doc: &PatchDoc| {
        cubed_dsp::seed::set_seed(7);
        let mut engine = engine_with(doc.value());
        note_stats(&mut engine)
    };
    let mut doc = PatchDoc::from_value(patch.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    let before = stats(&doc);
    assert!(before.2, "{name}: finite before");
    let original = doc.clone();

    // Add a node of every kind a patch may have more of, wire and set it up.
    let mixer = one(&doc, "mixer");
    let osc = doc.add_node("oscillator").unwrap();
    doc.set_param(&osc, "gain", &json!(0.2)).unwrap();
    doc.set_param(&osc, "waveform", &json!(1)).unwrap();
    doc.set_param(&osc, "detune_oct", &json!(1)).unwrap();
    doc.connect(
        &osc,
        PortId::AudioOutput0,
        &mixer,
        PortId::AudioInput0,
        0.5,
        2,
        0,
    )
    .unwrap();
    let env = doc.add_node("envelope").unwrap();
    doc.set_param(&env, "attack", &json!(0.01)).unwrap();
    doc.connect(&env, PortId::AudioOutput0, &osc, PortId::GainMod, 1.0, 0, 0)
        .unwrap();
    let lfo = doc.add_node("lfo").unwrap();
    doc.set_param(&lfo, "frequency", &json!(4.0)).unwrap();
    doc.connect(
        &lfo,
        PortId::AudioOutput0,
        &osc,
        PortId::DetuneMod,
        0.1,
        2,
        0,
    )
    .unwrap();
    let filter = doc.add_node("filter").unwrap();
    doc.set_param(&filter, "cutoff", &json!(3000.0)).unwrap();
    doc.connect(
        &osc,
        PortId::AudioOutput1,
        &filter,
        PortId::AudioInput0,
        1.0,
        2,
        0,
    )
    .unwrap();
    doc.connect(
        &filter,
        PortId::AudioOutput0,
        &mixer,
        PortId::AudioInput0,
        0.3,
        2,
        0,
    )
    .unwrap();
    doc.connect(
        &lfo,
        PortId::AudioOutput0,
        &filter,
        PortId::CutoffMod,
        0.4,
        2,
        0,
    )
    .unwrap();
    doc.set_macro_route(3, &filter, PortId::ResonanceMod, 0.5, 2, 0)
        .unwrap();
    let delay = doc.add_node("delay").unwrap();
    doc.set_param(&delay, "active", &json!(true)).unwrap();
    doc.move_effect(0, 2).unwrap();
    doc.set_editor(json!({"nodes": {osc.clone(): {"x": 1, "y": 2}}}));
    assert_eq!(
        doc.validate(),
        original.validate(),
        "{name}: edits add no problems"
    );

    // Saved, loaded, played.
    let text = doc.to_json();
    let reloaded = PatchDoc::from_json(&text).unwrap();
    assert_eq!(reloaded.value(), doc.value());
    let edited = stats(&reloaded);
    assert!(
        edited.2 && edited.0 > 0.0,
        "{name}: the edited patch plays: {edited:?}"
    );

    // Everything undone gives back the original's sound.
    doc.remove_node(&delay).unwrap();
    doc.move_effect(2, 0).unwrap();
    for id in [&filter, &lfo, &env, &osc] {
        doc.remove_node(id).unwrap();
    }
    doc.remove_macro_route(3, &filter, PortId::ResonanceMod);
    assert_eq!(doc.nodes(), original.nodes(), "{name}: the same nodes");
    assert_eq!(
        doc.connections(),
        original.connections(),
        "{name}: the same wires"
    );
    // The editor's data and an empty macro list are all that is left.
    doc.set_editor(Value::Null);
    let mut left = doc.value().clone();
    let right = original.value().clone();
    if right["synthState"].get("macros").is_none() {
        left["synthState"].as_object_mut().unwrap().remove("macros");
    }
    assert_eq!(left, right, "{name}: the document is as it was");
    let after = stats(&doc);
    assert!(after.2 && after.0 > 0.0, "{name}: plays again");
    (before.1, edited.1)
}

#[test]
fn checked_in_daw_patches_take_edits_and_load() {
    let patches = daw_patches();
    assert!(patches.len() >= 8);
    for (name, patch) in &patches {
        edit_session(name, patch);
    }
}

#[test]
fn every_patch_of_the_dawss_bank_takes_edits_and_loads() {
    let patches = full_daw_bank();
    for (name, patch) in &patches {
        edit_session(name, patch);
    }
    if !patches.is_empty() {
        assert_eq!(patches.len(), 53);
    }
}

#[test]
fn daw_patches_read_without_changes() {
    for (name, patch) in daw_patches() {
        let doc = PatchDoc::from_value(patch.clone()).unwrap();
        // Nothing but a canonical voice is added to a patch that had one.
        assert_eq!(doc.value(), &patch, "{name}");
        let problems = doc.validate();
        assert!(
            problems.iter().all(|p| p.contains("is not a UUID")
                || p.contains("no known type")
                || p.contains("Chorus has no input")),
            "{name}: {problems:?}"
        );
    }
}

#[test]
fn a_legacy_layout_with_voices_is_brought_up_to_date() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/real_patch.json"
    ))
    .unwrap();
    let doc = PatchDoc::from_json(&text).unwrap();
    assert!(doc.value()["synthState"]["layout"].get("voices").is_none());
    assert!(doc.value()["synthState"]["layout"]["canonicalVoice"]["nodes"].is_object());
    assert!(doc.voice_count() >= 1);
    let mut engine = engine_with(doc.value());
    let (peak, _, finite) = note_stats(&mut engine);
    assert!(finite && peak > 0.0);
}

#[test]
fn bank_files_list_and_open_their_patches() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/daw_patches.json"
    ))
    .unwrap();
    let entries = PatchDoc::bank_patches(&text).unwrap();
    assert_eq!(entries[0].name, "SawBass1");
    assert_eq!(entries[0].category, "Factory/Bass");
    let doc = PatchDoc::from_bank_json(&text, 3).unwrap();
    assert_eq!(doc.name(), "FMBass 1");
    assert!(PatchDoc::from_bank_json(&text, 99).is_err());
    assert!(PatchDoc::from_json("{")
        .unwrap_err()
        .contains("invalid JSON"));
    assert!(PatchDoc::from_json("{}")
        .unwrap_err()
        .contains("synthState"));
}

#[test]
fn the_loader_builds_the_effect_chain_the_document_describes() {
    let mut doc = PatchDoc::new("t");
    let engine = engine_with(doc.value());
    assert_eq!(
        engine.effect_ids(),
        [10000, 10001, 10002, 10003, 10004, 10005, 10006, 10007]
    );
    assert_eq!(engine.effect_kinds()[0], "chorus");

    doc.move_effect(0, 3).unwrap();
    doc.remove_node("10007").unwrap();
    let second_delay = doc.add_node("delay").unwrap();
    doc.set_param(&second_delay, "active", &json!(true))
        .unwrap();
    doc.set_param(&second_delay, "wetMix", &json!(1.0)).unwrap();
    doc.set_param(&second_delay, "delayMs", &json!(20)).unwrap();
    let engine = engine_with(doc.value());
    assert_eq!(
        engine.effect_ids(),
        [10001, 10002, 10003, 10000, 10004, 10005, 10006, 10007]
    );
    assert_eq!(
        engine.effect_kinds(),
        [
            "delay",
            "freeverb",
            "convolver",
            "chorus",
            "limiter",
            "compressor",
            "saturation",
            "delay"
        ]
    );

    // The new delay is set up from its state: its echo is heard.
    let heard = |doc: &PatchDoc| {
        cubed_dsp::seed::set_seed(2);
        let mut engine = engine_with(doc.value());
        play(&mut engine, 330.0, 20, 40)
    };
    let with_echo = heard(&doc);
    doc.set_param(&second_delay, "active", &json!(false))
        .unwrap();
    let without = heard(&doc);
    assert!(
        max_abs_diff(&with_echo, &without) > 0.01,
        "the second delay is in the chain"
    );
}

#[test]
fn a_chain_in_another_order_sounds_different_and_the_default_chain_is_unchanged() {
    let mut doc = PatchDoc::new("t");
    doc.set_param("10006", "active", &json!(true)).unwrap();
    doc.set_param("10006", "drive", &json!(6.0)).unwrap();
    let render = |doc: &PatchDoc| {
        cubed_dsp::seed::set_seed(4);
        let mut engine = engine_with(doc.value());
        play(&mut engine, 196.0, 30, 10)
    };
    let default_order = render(&doc);
    doc.move_effect(6, 4).unwrap(); // the saturation before the limiter and compressor
    let moved = render(&doc);
    assert!(
        max_abs_diff(&default_order, &moved) > 0.001,
        "order matters"
    );
    doc.move_effect(4, 6).unwrap();
    assert_eq!(render(&doc), default_order, "and back");
}
