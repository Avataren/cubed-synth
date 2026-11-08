//! Edits made while a sine plays do not click: the biggest step between two
//! samples while the change settles stays within what the sound has before or
//! after it.
#![cfg(feature = "native-host")]

mod common;
use common::*;
use cubed_dsp::audio_engine::native::AudioEngine;
use cubed_dsp::catalog;
use cubed_dsp::patch_edit::PatchDoc;
use cubed_dsp::traits::PortId;
use cubed_dsp::AutomationFrame;
use serde_json::{json, Value};

/// A sine through a filter, with an LFO nudging its level, a held note; the
/// effects are off and the compressor too, so only what an edit does is heard.
fn sine_doc() -> (PatchDoc, Vec<(String, String)>) {
    let mut doc = PatchDoc::new("sine");
    let osc = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "oscillator")
        .unwrap()
        .id;
    doc.set_param(&osc, "waveform", &json!(0)).unwrap();
    doc.set_param(&osc, "gain", &json!(0.5)).unwrap();
    let env = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "envelope")
        .unwrap()
        .id;
    doc.set_param(&env, "sustain", &json!(1.0)).unwrap();
    doc.set_param(&env, "attack", &json!(0.005)).unwrap();
    let mixer = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "mixer")
        .unwrap()
        .id;
    // osc -> filter -> mixer
    let filter = doc.add_node("filter").unwrap();
    doc.disconnect(&osc, PortId::AudioOutput0, &mixer, PortId::AudioInput0)
        .unwrap();
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
    doc.set_param(&filter, "cutoff", &json!(8000)).unwrap();
    // An LFO on the oscillator's gain.
    let lfo = doc.add_node("lfo").unwrap();
    doc.set_param(&lfo, "frequency", &json!(5.0)).unwrap();
    doc.connect(&lfo, PortId::AudioOutput0, &osc, PortId::GainMod, 0.2, 2, 0)
        .unwrap();
    doc.set_param("10005", "active", &json!(false)).unwrap(); // the compressor
    let ids = vec![
        ("oscillator".to_string(), osc),
        ("envelope".to_string(), env),
        ("filter".to_string(), filter),
        ("lfo".to_string(), lfo),
    ];
    (doc, ids)
}

fn node_id(ids: &[(String, String)], type_name: &str) -> String {
    match type_name {
        "chorus" => "10000".into(),
        "delay" => "10001".into(),
        "freeverb" => "10002".into(),
        "convolver" => "10003".into(),
        "compressor" => "10005".into(),
        "saturation" => "10006".into(),
        "bitcrusher" => "10007".into(),
        other => ids.iter().find(|(t, _)| t == other).unwrap().1.clone(),
    }
}

fn max_step(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

struct Run {
    old: f32,
    /// The biggest step in the first three blocks after the edit.
    near: f32,
    /// ... and in the 40 blocks after it.
    transition: f32,
    new: f32,
}

impl Run {
    fn ratio(&self) -> f32 {
        self.transition / self.old.max(self.new).max(1e-4)
    }

    fn near_ratio(&self) -> f32 {
        self.near / self.old.max(self.new).max(1e-4)
    }
}

fn run(doc: &PatchDoc, id: &str, key: &str, from: &Value, to: &Value) -> Run {
    let mut doc = doc.clone();
    doc.set_param(id, key, from).unwrap();
    cubed_dsp::seed::set_seed(5);
    let mut engine: AudioEngine = engine_with(doc.value());
    let mut frame = AutomationFrame::with_dimensions(engine.num_voices(), 4, BLOCK);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut render = |engine: &mut AudioEngine, count: usize| {
        let mut out = Vec::new();
        for _ in 0..count {
            frame.set_voice_values(0, 1.0, 220.0, 1.0, 1.0);
            engine.fill_macro_values(&mut frame);
            engine.process_with_frame_instrument(&frame, &mut l, &mut r);
            out.extend_from_slice(&l);
        }
        out
    };
    render(&mut engine, 120);
    let old = render(&mut engine, 12);
    let set = doc.set_param(id, key, to).unwrap();
    engine.edit_param(set.edit.unwrap()).unwrap();
    let transition = render(&mut engine, 40);
    let near = transition[..3 * BLOCK].to_vec();
    render(&mut engine, 80);
    let new = render(&mut engine, 12);
    Run {
        old: max_step(&old),
        near: max_step(&near),
        transition: max_step(&transition),
        new: max_step(&new),
    }
}

/// (node type, key, from, to) cases of a sine path.
fn cases() -> Vec<(&'static str, &'static str, Value, Value)> {
    vec![
        ("oscillator", "gain", json!(0.5), json!(0.1)),
        ("oscillator", "gain", json!(0.1), json!(1.0)),
        ("oscillator", "waveform", json!(0), json!(1)),
        ("oscillator", "detune_cents", json!(0), json!(60)),
        ("oscillator", "detune_semi", json!(0), json!(7)),
        ("oscillator", "unison_voices", json!(1), json!(3)),
        ("oscillator", "unison_voices", json!(5), json!(2)),
        ("oscillator", "spread", json!(0), json!(60)),
        ("oscillator", "feedback_amount", json!(0), json!(0.3)),
        ("oscillator", "active", json!(true), json!(false)),
        ("envelope", "sustain", json!(1.0), json!(0.2)),
        ("envelope", "sustain", json!(0.2), json!(1.0)),
        ("envelope", "attack", json!(0.005), json!(1.0)),
        ("envelope", "decay", json!(0.1), json!(2.0)),
        ("envelope", "release", json!(0.1), json!(2.0)),
        ("envelope", "decayCurve", json!(0), json!(-8)),
        ("envelope", "attackCurve", json!(0), json!(6)),
        ("lfo", "frequency", json!(5), json!(15)),
        ("lfo", "gain", json!(1), json!(0.2)),
        ("lfo", "gain", json!(0.2), json!(1)),
        ("lfo", "phaseOffset", json!(0), json!(0.5)),
        ("lfo", "waveform", json!(0), json!(1)),
        ("lfo", "useAbsolute", json!(false), json!(true)),
        ("filter", "cutoff", json!(8000), json!(500)),
        ("filter", "cutoff", json!(500), json!(8000)),
        ("filter", "resonance", json!(0.2), json!(0.9)),
        ("filter", "filter_type", json!(0), json!(5)),
        ("filter", "filter_slope", json!(0), json!(1)),
        ("filter", "keytracking", json!(0), json!(1)),
        ("filter", "active", json!(true), json!(false)),
        ("chorus", "mix", json!(0.5), json!(1.0)),
        ("chorus", "baseDelayMs", json!(15), json!(40)),
        ("chorus", "depthMs", json!(5), json!(20)),
        ("chorus", "lfoRateHz", json!(0.5), json!(4)),
        ("chorus", "feedback", json!(0.3), json!(0.8)),
        ("chorus", "feedback_filter", json!(0.5), json!(0.1)),
        ("chorus", "stereoPhaseOffsetDeg", json!(90), json!(200)),
        ("chorus", "active", json!(false), json!(true)),
        ("delay", "delayMs", json!(250), json!(100)),
        ("delay", "feedback", json!(0.5), json!(0.9)),
        ("delay", "wetMix", json!(0.1), json!(0.9)),
        ("delay", "active", json!(false), json!(true)),
        ("freeverb", "wet", json!(0.3), json!(0.9)),
        ("freeverb", "dry", json!(0.7), json!(0.1)),
        ("freeverb", "room_size", json!(0.95), json!(0.4)),
        ("freeverb", "damp", json!(0.5), json!(0.9)),
        ("freeverb", "width", json!(1), json!(0.2)),
        ("freeverb", "active", json!(false), json!(true)),
        ("convolver", "wetMix", json!(0.1), json!(0.9)),
        ("convolver", "active", json!(false), json!(true)),
        ("compressor", "thresholdDb", json!(-12), json!(-40)),
        ("compressor", "ratio", json!(4), json!(12)),
        ("compressor", "makeupGainDb", json!(3), json!(12)),
        ("compressor", "mix", json!(0.5), json!(1.0)),
        ("compressor", "active", json!(false), json!(true)),
        ("saturation", "drive", json!(2), json!(7)),
        ("saturation", "mix", json!(0.5), json!(1.0)),
        ("saturation", "active", json!(false), json!(true)),
        ("bitcrusher", "mix", json!(0.5), json!(1.0)),
        ("bitcrusher", "active", json!(false), json!(true)),
    ]
}

/// How far a change may stray from the sound before and after it while it
/// settles (the first three blocks after the edit must stay within 1.5 of it,
/// whatever the parameter). A delay line's time sweeps its pitch as it moves
/// and a feedback path recirculates a change: that is the effect's own
/// character, not a step.
fn tolerance(type_name: &str, key: &str) -> f32 {
    match (type_name, key) {
        ("chorus", "baseDelayMs" | "depthMs") | ("delay", "delayMs") => 14.0,
        (_, "feedback") => 3.0,
        _ => 2.0,
    }
}

#[test]
fn edits_while_a_sine_plays_do_not_click() {
    let (mut doc, ids) = sine_doc();
    // The effects under test start on, and the bit crusher is mild.
    doc.set_param("10007", "bits", &json!(24)).unwrap();
    doc.set_param("10007", "downsampleFactor", &json!(1))
        .unwrap();
    let mut worst = Vec::new();
    for (type_name, key, from, to) in cases() {
        let id = node_id(&ids, type_name);
        let mut base = doc.clone();
        if catalog::node_spec(type_name).unwrap().is_effect() && key != "active" {
            base.set_param(&id, "active", &json!(true)).unwrap();
        }
        if type_name == "oscillator" && key == "spread" {
            base.set_param(&id, "unison_voices", &json!(3)).unwrap();
        }
        let run = run(&base, &id, key, &from, &to);
        let (ratio, near) = (run.ratio(), run.near_ratio());
        println!(
            "{type_name}.{key} {from} -> {to}: near {near:.2}, settling {ratio:.2} (old {:.4}, new {:.4})",
            run.old, run.new
        );
        if ratio > tolerance(type_name, key) || near > 1.5 {
            worst.push(format!(
                "{type_name}.{key} {from} -> {to}: near {near:.2}, settling {ratio:.2}"
            ));
        }
    }
    assert!(worst.is_empty(), "clicks:\n{}", worst.join("\n"));
}

#[test]
fn the_detector_sees_a_click() {
    // A sine whose level steps by 80% half way: the ratio is large.
    let sine: Vec<f32> = (0..4_000)
        .map(|i| 0.5 * (std::f32::consts::TAU * 220.0 * i as f32 / RATE).sin())
        .collect();
    let mut clicked = sine.clone();
    for s in &mut clicked[2_000..] {
        *s *= 0.2;
    }
    let clean = max_step(&sine);
    assert!(
        max_step(&clicked) > 5.0 * clean,
        "{} vs {clean}",
        max_step(&clicked)
    );
}
