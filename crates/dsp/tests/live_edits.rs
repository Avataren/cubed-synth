//! Live parameter edits: every parameter of every node type reaches a running
//! engine, which ends up with the state the patch document has.
#![cfg(feature = "native-host")]

mod common;
use common::*;
use cubed_dsp::audio_engine::native::{AudioEngine, EditError};
use cubed_dsp::catalog::{self, ParamKind};
use cubed_dsp::live::ParamEdit;
use cubed_dsp::patch_edit::PatchDoc;
use cubed_dsp::AutomationFrame;
use serde_json::{json, Value};

/// A patch with one of every node type a patch can hold, and the eight effects
/// (turned on, so they are in the sound).
fn full_doc() -> PatchDoc {
    let mut doc = PatchDoc::new("all");
    for spec in catalog::NODES.iter().filter(|n| n.deletable) {
        let have = doc
            .nodes()
            .iter()
            .filter(|n| n.type_name == spec.type_name)
            .count();
        if have == 0 {
            doc.add_node(spec.type_name).unwrap();
        }
    }
    for effect in doc.effects() {
        if doc.value()["synthState"].get("limiter").is_none() {
            let _ = doc.set_param(&effect.id, "active", &json!(true));
        }
    }
    doc
}

/// Values worth setting a parameter to: its extremes, middle and options.
fn probes(param: &catalog::ParamSpec) -> Vec<Value> {
    match param.kind {
        ParamKind::Bool => vec![json!(false), json!(true)],
        ParamKind::Enum => (0..param.options.len()).map(|i| json!(i)).collect(),
        ParamKind::Int => vec![json!(param.min), json!(param.max), json!(param.default)],
        ParamKind::Float => vec![
            json!(param.min),
            json!(param.max),
            json!((param.min + param.max) / 2.0),
            json!(param.default),
        ],
    }
}

fn blocks(
    engine: &mut AudioEngine,
    frame: &mut AutomationFrame,
    count: usize,
    held: bool,
) -> Vec<f32> {
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut out = Vec::new();
    for _ in 0..count {
        frame.set_voice_values(0, if held { 1.0 } else { 0.0 }, 220.0, 1.0, 1.0);
        engine.fill_macro_values(frame);
        engine.process_with_frame_instrument(frame, &mut l, &mut r);
        out.extend_from_slice(&l);
    }
    out
}

#[test]
fn every_parameter_of_every_node_takes_every_probe_value() {
    cubed_dsp::seed::set_seed(1);
    let mut doc = full_doc();
    let mut engine = engine_with(doc.value());
    let mut frame = AutomationFrame::with_dimensions(engine.num_voices(), 4, BLOCK);
    blocks(&mut engine, &mut frame, 20, true);

    let nodes = doc.nodes();
    let mut edited = 0;
    for node in &nodes {
        let Some(spec) = catalog::node_spec(&node.type_name) else {
            continue;
        };
        for (index, param) in spec.params.iter().enumerate() {
            for value in probes(param) {
                let set = doc
                    .set_param(&node.id, param.key, &value)
                    .unwrap_or_else(|e| panic!("{}.{}: {e}", node.type_name, param.key));
                let edit = set.edit.expect("an edit for a node of the document");
                assert_eq!(edit.param as usize, index);
                engine
                    .edit_param(edit)
                    .unwrap_or_else(|e| panic!("{}.{} = {value}: {e}", node.type_name, param.key));
                let out = blocks(&mut engine, &mut frame, 14, true);
                assert!(
                    out.iter().all(|s| s.is_finite()),
                    "{}.{} = {value}: the sound is finite",
                    node.type_name,
                    param.key
                );
                edited += 1;
            }
            // The engine holds what the document holds (a glide has finished).
            blocks(&mut engine, &mut frame, 40, true);
            let stored = doc.get_param(&node.id, param.key).unwrap();
            let held = engine
                .param_value(cubed_dsp::live::node_key(&node.id).unwrap(), index as u16)
                .unwrap();
            let expected = match &stored {
                Value::Bool(b) => *b as u8 as f32,
                other => other.as_f64().unwrap() as f32,
            };
            assert_eq!(held, expected, "{}.{}", node.type_name, param.key);
        }
    }
    assert!(edited > 150, "{edited} edits");
    assert!(!engine.edits_in_flight());
    blocks(&mut engine, &mut frame, 10, false);
}

#[test]
fn bad_edits_are_refused_without_harm() {
    let doc = PatchDoc::new("t");
    let mut engine = engine_with(doc.value());
    let osc = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "oscillator")
        .unwrap();
    let key = cubed_dsp::live::node_key(&osc.id).unwrap();
    assert_eq!(
        engine.edit_param(ParamEdit::new(12345, 0, 1.0)),
        Err(EditError::UnknownNode)
    );
    assert_eq!(
        engine.edit_param(ParamEdit::new(key, 99, 1.0)),
        Err(EditError::UnknownParam)
    );
    // A value that is no number is the parameter's default; out of range is clamped.
    engine.edit_param(ParamEdit::new(key, 2, f32::NAN)).unwrap();
    engine.edit_param(ParamEdit::new(key, 2, 1e9)).unwrap();
    let mut frame = AutomationFrame::with_dimensions(engine.num_voices(), 4, BLOCK);
    blocks(&mut engine, &mut frame, 30, true);
    assert_eq!(engine.param_value(key, 2), Some(1.0));
    assert_eq!(
        engine.edit_param(ParamEdit::new(cubed_dsp::live::PATCH, 77, 1.0)),
        Err(EditError::UnknownParam)
    );
}

#[test]
fn the_edited_engine_has_the_edited_state_of_a_daw_patch() {
    for (name, patch) in daw_patches() {
        let mut doc = PatchDoc::from_value(patch).unwrap();
        cubed_dsp::seed::set_seed(2);
        let mut engine = engine_with(doc.value());
        let mut frame = AutomationFrame::with_dimensions(engine.num_voices(), 4, BLOCK);
        blocks(&mut engine, &mut frame, 10, true);
        let nodes = doc.nodes();
        for node in nodes {
            let Some(spec) = catalog::node_spec(&node.type_name) else {
                continue;
            };
            if spec.params.is_empty() {
                continue;
            }
            // The first parameter of every node with some, moved.
            let param = &spec.params[spec.params.len() - 1];
            let value = match param.kind {
                ParamKind::Bool => json!(true),
                ParamKind::Enum => json!(param.options.len() - 1),
                _ => json!(param.max * 0.6 + param.min * 0.4),
            };
            let set = doc.set_param(&node.id, param.key, &value).unwrap();
            if engine.edit_param(set.edit.unwrap()).is_err() {
                continue; // a node without a state in the patch of that kind
            }
            blocks(&mut engine, &mut frame, 12, true);
        }
        assert!(!engine.edits_in_flight(), "{name}");
        // A fresh engine from the edited document agrees on every parameter.
        let mut fresh = engine_with(doc.value());
        let ids: Vec<u128> = engine.editable_nodes().collect();
        for id in ids {
            let spec = fresh_spec(&mut fresh, id);
            let Some(spec) = spec else { continue };
            for (index, param) in spec.params.iter().enumerate() {
                let a = engine.param_value(id, index as u16).unwrap();
                let b = fresh.param_value(id, index as u16).unwrap();
                assert_eq!(a, b, "{name}: {id} {}", param.key);
            }
        }
    }
}

fn fresh_spec(engine: &mut AudioEngine, id: u128) -> Option<&'static catalog::NodeSpec> {
    // The type is whatever has as many parameters as the node answers for.
    let mut count = 0;
    while engine.param_value(id, count).is_some() {
        count += 1;
    }
    if count == 0 {
        return None;
    }
    catalog::all_specs().find(|s| s.params.len() == count as usize)
}

#[test]
fn macros_and_the_instrument_gain_glide_per_sample() {
    let mut doc = PatchDoc::from_value(basic_patch(2)).unwrap();
    // A sine, so a step in the sound is a click and not a sawtooth's edge.
    doc.set_param(OSCILLATOR, "waveform", &json!(0)).unwrap();
    doc.set_macro_route(0, MIXER, cubed_dsp::traits::PortId::GainMod, 1.0, 0, 0)
        .unwrap();
    doc.set_macro_value(0, 1.0).unwrap();
    cubed_dsp::seed::set_seed(3);
    let mut engine = engine_with(doc.value());
    let mut frame = AutomationFrame::with_dimensions(2, 4, BLOCK);
    blocks(&mut engine, &mut frame, 60, true);
    let loud = rms(&blocks(&mut engine, &mut frame, 10, true));

    // The macro closes: the level falls without a step between samples.
    engine.edit_param(ParamEdit::macro_value(0, 0.0)).unwrap();
    let out = blocks(&mut engine, &mut frame, 2, true);
    let biggest_step = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    let slope = 0.8 * std::f32::consts::TAU * 220.0 / RATE; // a sine at the pitch
    assert!(
        biggest_step < 4.0 * slope,
        "no step in a macro: {biggest_step} vs {slope}"
    );
    let quiet = rms(&blocks(&mut engine, &mut frame, 10, true));
    assert!(quiet < 0.05 * loud, "{quiet} vs {loud}");

    // The instrument gain: half.
    engine.edit_param(ParamEdit::macro_value(0, 1.0)).unwrap();
    blocks(&mut engine, &mut frame, 20, true);
    let before = rms(&blocks(&mut engine, &mut frame, 10, true));
    engine.edit_param(ParamEdit::gain(0.5)).unwrap();
    blocks(&mut engine, &mut frame, 2, true);
    let after = rms(&blocks(&mut engine, &mut frame, 10, true));
    assert!((after / before - 0.5).abs() < 0.08, "{after} / {before}");
}

#[test]
fn an_engine_with_nothing_in_flight_renders_as_it_always_did() {
    // The edit machinery is out of the way until used: a patch rendered through
    // the instrument path equals the plain path.
    let doc = PatchDoc::from_value(basic_patch(2)).unwrap();
    let render_plain = || {
        cubed_dsp::seed::set_seed(4);
        let mut engine = engine_with(doc.value());
        play(&mut engine, 220.0, 30, 10)
    };
    let render_instrument = || {
        cubed_dsp::seed::set_seed(4);
        let mut engine = engine_with(doc.value());
        let mut frame = AutomationFrame::with_dimensions(2, 4, BLOCK);
        blocks(&mut engine, &mut frame, 30, true)
    };
    let a = render_plain();
    let b = render_instrument();
    assert_eq!(a[..30 * BLOCK], b[..30 * BLOCK]);
}
