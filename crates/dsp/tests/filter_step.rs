//! A filter whose cutoff is moved while a note plays leaves no tick: a 200 Hz
//! sine through a low-pass, the cutoff stepped, the second difference of the
//! output (which a sine keeps tiny and a slope kink makes a spike) at the step
//! no bigger than the sound's own before and after it.
#![cfg(feature = "native-host")]

mod common;
use common::*;
use cubed_dsp::live::ParamEdit;
use cubed_dsp::patch_edit::PatchDoc;
use cubed_dsp::traits::PortId;
use cubed_dsp::AutomationFrame;
use serde_json::json;

/// A 200 Hz sine through a low-pass and a steady envelope; the filter's id.
fn doc(cutoff: f64) -> (PatchDoc, String) {
    let mut doc = PatchDoc::new("filter");
    let find = |doc: &PatchDoc, t: &str| {
        doc.nodes()
            .into_iter()
            .find(|n| n.type_name == t)
            .unwrap()
            .id
    };
    let (osc, env, mixer) = (
        find(&doc, "oscillator"),
        find(&doc, "envelope"),
        find(&doc, "mixer"),
    );
    doc.set_param(&osc, "waveform", &json!(0)).unwrap();
    doc.set_param(&osc, "gain", &json!(0.8)).unwrap();
    doc.set_param(&env, "sustain", &json!(1.0)).unwrap();
    doc.set_param(&env, "attack", &json!(0.005)).unwrap();
    doc.set_param("10005", "active", &json!(false)).unwrap();
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
    doc.set_param(&filter, "cutoff", &json!(cutoff)).unwrap();
    (doc, filter)
}

/// The second difference of a signal: a sine has a small, smooth one, a tick a
/// spike.
fn second_difference(x: &[f32]) -> Vec<f32> {
    x.windows(3).map(|w| w[2] - 2.0 * w[1] + w[0]).collect()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// Renders 320 blocks of a held note, with `edit` sent before block 200.
fn render_edit(doc: &PatchDoc, edit: Option<ParamEdit>) -> Vec<f32> {
    cubed_dsp::seed::set_seed(1);
    let mut engine = engine_with(doc.value());
    let mut frame = AutomationFrame::with_dimensions(engine.num_voices(), 4, BLOCK);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut out = Vec::new();
    for b in 0..320 {
        if b == 200 {
            if let Some(edit) = edit {
                engine.edit_param(edit).unwrap();
            }
        }
        frame.set_voice_values(0, 1.0, 200.0, 1.0, 1.0);
        engine.fill_macro_values(&mut frame);
        engine.process_with_frame_instrument(&frame, &mut l, &mut r);
        out.extend_from_slice(&l);
    }
    out
}

/// The biggest second difference around the edit (block 200) over the biggest
/// of the sound before it and long after it.
fn tick_ratio(out: &[f32]) -> f32 {
    let d2 = second_difference(out);
    let before = peak(&d2[120 * BLOCK..190 * BLOCK]);
    let after = peak(&d2[280 * BLOCK..318 * BLOCK]);
    let at = peak(&d2[200 * BLOCK..260 * BLOCK]);
    at / before.max(after)
}

#[test]
fn a_cutoff_step_leaves_no_tick() {
    for (from, to) in [
        (5650.0, 150.0),
        (150.0, 5650.0),
        (5650.0, 400.0),
        (1000.0, 8000.0),
        // Small steps glide instead of ducking.
        (5650.0, 3000.0),
        (2000.0, 1000.0),
        (800.0, 1100.0),
    ] {
        let (doc, filter) = doc(from);
        let set = doc
            .clone()
            .set_param(&filter, "cutoff", &json!(to))
            .unwrap();
        let ratio = tick_ratio(&render_edit(&doc, set.edit));
        assert!(
            ratio < 1.8,
            "cutoff {from} -> {to}: a tick, {ratio:.2} x the sound's own"
        );
    }
}

#[test]
fn the_instrument_gain_and_other_big_steps_leave_no_tick_either() {
    let (doc, filter) = doc(5650.0);
    // The instrument gain shut.
    let ratio = tick_ratio(&render_edit(&doc, Some(ParamEdit::gain(0.0))));
    assert!(ratio < 1.8, "gain: {ratio}");
    // A resonance jump and a type change (both duck).
    let set = doc
        .clone()
        .set_param(&filter, "resonance", &json!(0.95))
        .unwrap();
    let ratio = tick_ratio(&render_edit(&doc, set.edit));
    assert!(ratio < 2.5, "resonance: {ratio}");
    let set = doc
        .clone()
        .set_param(&filter, "filter_type", &json!(5))
        .unwrap();
    let ratio = tick_ratio(&render_edit(&doc, set.edit));
    assert!(ratio < 2.5, "high-pass: {ratio}");
}

#[test]
fn the_detector_sees_a_slope_kink() {
    // The same sine with its level changed by a linear ramp over 128 samples:
    // the ramp's corners are a kink, and the measure finds them.
    let sine: Vec<f32> = (0..64 * 128)
        .map(|i| 0.5 * (std::f32::consts::TAU * 200.0 * i as f32 / RATE).sin())
        .collect();
    let mut ramped = sine.clone();
    for (k, s) in ramped[32 * 128..33 * 128].iter_mut().enumerate() {
        *s *= 1.0 - (k + 1) as f32 / 128.0;
    }
    for s in &mut ramped[33 * 128..] {
        *s = 0.0;
    }
    let d2 = second_difference(&ramped);
    let kink = peak(&d2[32 * 128 - 2..33 * 128 + 2]);
    let clean = peak(&second_difference(&sine));
    assert!(kink > 4.0 * clean, "{kink} vs {clean}");
}
