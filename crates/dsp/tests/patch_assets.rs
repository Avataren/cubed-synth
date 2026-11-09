//! The patch's embedded audio, edited as data: a sampler's sample and a wavetable
//! oscillator's table (`load_sample`, `set_wavetable`, ...), in the DAW's encoding, undone,
//! and heard through the native engine.
#![cfg(feature = "native-host")]

mod common;
use common::*;
use cubed_dsp::audio_engine::native::AudioEngine;
use cubed_dsp::patch_edit::PatchDoc;
use cubed_dsp::traits::PortId;
use cubed_dsp::wav::{base64_decode, base64_encode, decode_wav, encode_wav_f32, encode_wav_i16};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------------------
// helpers

fn sine(freq: f32, rate: u32, frames: usize, amp: f32) -> Vec<f32> {
    (0..frames)
        .map(|n| amp * (2.0 * std::f32::consts::PI * freq * n as f32 / rate as f32).sin())
        .collect()
}

fn wav_b64(samples: &[f32], channels: usize, rate: u32) -> String {
    base64_encode(&encode_wav_i16(samples, channels, rate))
}

/// A patch with the oscillator silenced and one node of `kind` playing into the mixer.
fn doc_with(kind: &str) -> (PatchDoc, String) {
    let mut doc = PatchDoc::new("assets");
    let osc = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "oscillator")
        .unwrap()
        .id;
    doc.set_param(&osc, "active", &json!(false)).unwrap();
    let node = doc.add_node(kind).unwrap();
    let mixer = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "mixer")
        .unwrap()
        .id;
    doc.connect(
        &node,
        PortId::AudioOutput0,
        &mixer,
        PortId::AudioInput0,
        1.0,
        2,
        0,
    )
    .unwrap();
    (doc, node)
}

fn engine_for(doc: &PatchDoc) -> AudioEngine {
    engine_with(&serde_json::from_str::<Value>(&doc.to_json()).unwrap())
}

/// The note, played at `freq` on voice 0 (the sampler's root note 69 plays a sample at its own
/// speed at 440 Hz).
fn heard(doc: &PatchDoc, freq: f32) -> Vec<f32> {
    let mut engine = engine_for(doc);
    play(&mut engine, freq, 60, 0)
}

/// Rising zero crossings per second of the middle of the sound.
fn pitch(out: &[f32]) -> f32 {
    let mid = &out[out.len() / 4..];
    let crossings = mid.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    crossings as f32 / (mid.len() as f32 / RATE)
}

/// How much of the sound is fast: the energy of the differences over the energy.
fn brightness(out: &[f32]) -> f32 {
    let mid = &out[out.len() / 4..];
    let diff: f32 = mid.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
    let energy: f32 = mid.iter().map(|x| x * x).sum();
    diff / energy.max(1e-12)
}

fn ok(doc: &mut PatchDoc, op: Value) -> Value {
    doc.apply(&op).unwrap_or_else(|e| panic!("{op}: {e}"))
}

fn err(doc: &mut PatchDoc, op: Value) -> String {
    let before = doc.to_json();
    let e = doc
        .apply(&op)
        .expect_err(&format!("{op} should be refused"));
    assert_eq!(
        doc.to_json(),
        before,
        "{op}: a refused operation changes nothing"
    );
    e
}

fn q(doc: &PatchDoc, question: Value) -> Value {
    doc.query(&question)
        .unwrap_or_else(|e| panic!("{question}: {e}"))
}

fn asset(doc: &PatchDoc, id: &str) -> Value {
    doc.value()["audioAssets"][id].clone()
}

fn wave_of(doc: &PatchDoc, node: &str, frame: usize, points: usize) -> Vec<f64> {
    q(
        doc,
        json!({"query": "wavetable", "node": node, "from": frame, "count": 1, "size": points}),
    )["data"][0]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect()
}

// ---------------------------------------------------------------------------------------
// samples

#[test]
fn a_sample_goes_into_the_patch_as_the_daws_asset_and_the_node_state() {
    let (mut doc, node) = doc_with("sampler");
    let samples = sine(1000.0, 44_100, 4410, 0.5);
    let got = ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&samples, 1, 44_100),
               "root_note": 69, "name": "ping.wav", "peaks": 8}),
    );
    assert_eq!(got["asset"], json!(format!("sample_{node}")));
    assert_eq!(got["frames"], 4410);
    assert_eq!(got["rate"], 44_100);
    assert_eq!(got["channels"], 1);
    assert_eq!(got["root_note"], 69);
    assert_eq!(got["mixed"], false);
    assert!((got["duration"].as_f64().unwrap() - 0.1).abs() < 1e-9);
    assert!((got["peak"].as_f64().unwrap() - 0.5).abs() < 1e-3);
    assert_eq!(got["peaks"]["buckets"], 8);

    // The asset as the DAW's AudioAsset: a 16-bit PCM WAV in base64, beside its numbers.
    let a = asset(&doc, &format!("sample_{node}"));
    assert_eq!(a["type"], "sample");
    assert_eq!(a["id"], json!(format!("sample_{node}")));
    assert_eq!(a["sampleRate"], 44_100);
    assert_eq!(a["channels"], 1);
    assert_eq!(a["rootNote"], 69);
    assert_eq!(a["fileName"], "ping.wav");
    let wav = base64_decode(a["base64Data"].as_str().unwrap()).unwrap();
    assert_eq!(wav.len(), 44 + 4410 * 2);
    assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1, "PCM");
    assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16, "16 bits");
    let pcm = decode_wav(&wav).unwrap();
    for (got, want) in pcm.samples.iter().zip(&samples) {
        assert!((got - want).abs() < 1.0 / 32768.0, "{got} {want}");
    }

    // The node's state says what the DAW's panel shows.
    let state = &doc.value()["synthState"]["samplers"][&node];
    assert_eq!(state["sampleLength"], 4410);
    assert_eq!(state["sampleRate"], 44_100);
    assert_eq!(state["channels"], 1);
    assert_eq!(state["fileName"], "ping.wav");
    assert_eq!(state["rootNote"], 69);
    assert_eq!(state["loopStart"], 0);
    assert_eq!(state["loopEnd"], 1);
    assert!(doc.validate().is_empty());
}

#[test]
fn the_sample_survives_saving_and_loading_and_comes_back_out_as_its_wav() {
    let (mut doc, node) = doc_with("sampler");
    let samples = sine(330.0, 22_050, 3000, 0.7);
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&samples, 1, 22_050), "name": "a.wav"}),
    );
    let before = q(&doc, json!({"query": "sample_info", "node": node}));
    let saved = doc.to_json();
    let mut again = PatchDoc::from_json(&saved).unwrap();
    assert_eq!(again, doc);
    assert_eq!(
        q(&again, json!({"query": "sample_info", "node": node})),
        before
    );

    // sample_wav gives the file; loading that file again makes the very same asset.
    let file = q(&doc, json!({"query": "sample_wav", "node": node}));
    assert_eq!(file["name"], "a.wav");
    assert_eq!(file["frames"], 3000);
    let first = asset(&doc, &format!("sample_{node}"))["base64Data"].clone();
    ok(
        &mut again,
        json!({"op": "load_sample", "node": node, "data": file["data"], "name": "a.wav"}),
    );
    assert_eq!(
        asset(&again, &format!("sample_{node}"))["base64Data"],
        first
    );
    assert_eq!(
        again.value()["synthState"]["samplers"],
        doc.value()["synthState"]["samplers"]
    );
}

#[test]
fn a_sample_can_be_mixed_normalised_and_given_as_raw_pcm_or_numbers() {
    let (mut doc, node) = doc_with("sampler");
    // Stereo, left loud and right silent: mixed to mono it is half.
    let stereo: Vec<f32> = sine(500.0, 8000, 800, 0.8)
        .into_iter()
        .flat_map(|x| [x, 0.0])
        .collect();
    let got = ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&stereo, 2, 8000), "mix": true}),
    );
    assert_eq!(
        (got["channels"].as_i64(), got["mixed"].as_bool()),
        (Some(1), Some(true))
    );
    assert!((got["peak"].as_f64().unwrap() - 0.4).abs() < 1e-3);
    // Normalised to 0.9.
    let got = ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&stereo, 2, 8000), "normalize": 0.9}),
    );
    assert_eq!(got["channels"], 2);
    assert!((got["peak"].as_f64().unwrap() - 0.9).abs() < 1e-3);
    // Raw 16-bit PCM with its rate.
    let raw: Vec<u8> = [0i16, 16384, 0, -16384]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let got = ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": base64_encode(&raw), "rate": 11025}),
    );
    assert_eq!(
        (got["frames"].as_i64(), got["rate"].as_i64()),
        (Some(4), Some(11025))
    );
    assert!((got["peak"].as_f64().unwrap() - 0.5).abs() < 1e-6);
    // Raw floats, with a format.
    let floats: Vec<u8> = [0.25f32, -0.25]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let got = ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": base64_encode(&floats), "rate": 8000, "format": "f32"}),
    );
    assert_eq!(got["frames"], 2);
    // A list of numbers.
    let got = ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "samples": [0, 0.5, 1, 0.5, 0, -0.5], "rate": 8000, "normalize": true}),
    );
    assert_eq!(got["frames"], 6);
    assert_eq!(got["peak"], 1);
    // A WAV of 24 bits (the DAW's would be 16) is stored as 16.
    let mut wav24 = Vec::new();
    wav24.extend_from_slice(b"RIFF");
    wav24.extend_from_slice(&(36u32 + 6).to_le_bytes());
    wav24.extend_from_slice(b"WAVEfmt ");
    wav24.extend_from_slice(&16u32.to_le_bytes());
    for v in [1u16, 1] {
        wav24.extend_from_slice(&v.to_le_bytes());
    }
    wav24.extend_from_slice(&8000u32.to_le_bytes());
    wav24.extend_from_slice(&24_000u32.to_le_bytes());
    wav24.extend_from_slice(&3u16.to_le_bytes());
    wav24.extend_from_slice(&24u16.to_le_bytes());
    wav24.extend_from_slice(b"data");
    wav24.extend_from_slice(&6u32.to_le_bytes());
    wav24.extend_from_slice(&[0x00, 0x00, 0x40, 0x00, 0x00, 0xc0]);
    let got = ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": base64_encode(&wav24)}),
    );
    assert_eq!(got["frames"], 2);
    let stored = decode_wav(
        &base64_decode(
            asset(&doc, &format!("sample_{node}"))["base64Data"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(stored.samples, vec![0.5, -0.5]);
    // Without a name the node has no file name.
    assert!(doc.value()["synthState"]["samplers"][&node]
        .get("fileName")
        .is_none());
}

#[test]
fn a_sample_that_cannot_be_loaded_is_refused_with_a_reason_and_changes_nothing() {
    let (mut doc, node) = doc_with("sampler");
    let osc = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "oscillator")
        .unwrap()
        .id;
    let good = wav_b64(&sine(100.0, 8000, 80, 0.5), 1, 8000);
    let cases = [
        (
            json!({"op": "load_sample", "node": osc, "data": good}),
            "is a Oscillator, not a Sampler",
        ),
        (
            json!({"op": "load_sample", "node": "nope", "data": good}),
            "no such node",
        ),
        (json!({"op": "load_sample", "node": node}), "needs 'data'"),
        (
            json!({"op": "load_sample", "node": node, "data": "!!!"}),
            "not base64",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": base64_encode(b"hello, this is not a wav")}),
            "not a WAV",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": base64_encode(&[0u8; 8]), "format": "i16"}),
            "needs a 'rate'",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": base64_encode(&[0u8; 8]), "rate": 8000, "format": "mp3"}),
            "'format'",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": base64_encode(&[]), "rate": 8000}),
            "no audio",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": base64_encode(&[0u8; 8]), "rate": 8000, "channels": 3}),
            "mix",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": good, "rate": 8}),
            "'rate'",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": good, "normalize": 2}),
            "'normalize'",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": good, "mix": "yes"}),
            "'mix'",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": good, "root_note": "C4"}),
            "'root_note'",
        ),
        (
            json!({"op": "load_sample", "node": node, "samples": [0.1, "x"], "rate": 8000}),
            "not a number",
        ),
        (
            json!({"op": "load_sample", "node": node, "samples": [0.1]}),
            "'rate'",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": good, "peaks": 0}),
            "'peaks'",
        ),
        (
            json!({"op": "load_sample", "node": node, "data": good, "name": 5}),
            "'name'",
        ),
    ];
    for (op, why) in cases {
        let message = err(&mut doc, op.clone());
        assert!(message.contains(why), "{op}: {message}");
    }
    // Too long: more numbers than a patch holds.
    let long = base64_encode(&vec![0u8; (16 << 20) * 2 + 2]);
    let message = err(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": long, "rate": 8000}),
    );
    assert!(message.contains("too long"), "{message}");
}

#[test]
fn a_sample_is_cleared_and_the_node_keeps_the_rest() {
    let (mut doc, node) = doc_with("sampler");
    assert_eq!(
        ok(&mut doc, json!({"op": "clear_sample", "node": node})),
        json!({"cleared": false})
    );
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&[0.1; 100], 1, 8000), "name": "x.wav"}),
    );
    assert_eq!(
        ok(&mut doc, json!({"op": "clear_sample", "node": node})),
        json!({"cleared": true})
    );
    assert!(doc.value()["audioAssets"].as_object().unwrap().is_empty());
    let state = &doc.value()["synthState"]["samplers"][&node];
    assert_eq!(state["sampleLength"], 0);
    assert!(state.get("fileName").is_none());
    let info = q(&doc, json!({"query": "sample_info", "node": node}));
    assert_eq!(info["loaded"], false);
    assert!(doc.validate().is_empty());
    // Removing a node takes its sample with it.
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&[0.1; 100], 1, 8000)}),
    );
    doc.remove_node(&node).unwrap();
    assert!(doc.value()["audioAssets"].as_object().unwrap().is_empty());
}

#[test]
fn set_sample_sets_what_the_panel_has_and_loop_points_move_like_the_daws_sliders() {
    let (mut doc, node) = doc_with("sampler");
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&[0.2; 2000], 1, 8000), "root_note": 60}),
    );
    let got = ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "root_note": 72, "loop_mode": "pingpong",
               "gain": 1.25, "octave": -1, "semitones": 3, "cents": -20, "trigger": "oneshot"}),
    );
    let state = &got["state"];
    assert_eq!(state["rootNote"], 72);
    assert_eq!(state["loopMode"], 2);
    assert_eq!(state["triggerMode"], 2);
    assert_eq!(state["gain"], 1.25);
    // The tuning in cents and the frequency follow from the parts.
    assert_eq!(state["detune"].as_f64().unwrap(), -1200.0 + 300.0 - 20.0);
    let expected = 440.0 * 2f64.powf(-920.0 / 1200.0);
    assert!((state["frequency"].as_f64().unwrap() - expected).abs() < 1e-4);
    // One live edit per thing that changed, in the form set_param answers with.
    let edits = got["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 7, "{edits:?}");
    assert!(edits
        .iter()
        .all(|e| e["node"].is_string() && e["param"].is_u64() && e["value"].is_number()));
    // Setting the same again changes nothing, so it makes no edits.
    let again = ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "root_note": 72, "gain": 1.25}),
    );
    assert_eq!(again["edits"], json!([]));
    // The asset's root note follows.
    assert_eq!(asset(&doc, &format!("sample_{node}"))["rootNote"], 72);

    // Loop points: the start pushes the end on, the end stays 0.0005 after the start.
    let got = ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "loop_start": 0.25, "loop_end": 0.5}),
    );
    assert_eq!(
        (
            got["state"]["loopStart"].as_f64(),
            got["state"]["loopEnd"].as_f64()
        ),
        (Some(0.25), Some(0.5))
    );
    let got = ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "loop_start": 0.75}),
    );
    assert_eq!(got["state"]["loopStart"], 0.75);
    assert!((got["state"]["loopEnd"].as_f64().unwrap() - 0.7505).abs() < 1e-12);
    let got = ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "loop_end": 0.1}),
    );
    assert!((got["state"]["loopEnd"].as_f64().unwrap() - 0.7505).abs() < 1e-12);
    let got = ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "loop_start": 1.0}),
    );
    assert!((got["state"]["loopStart"].as_f64().unwrap() - 0.9995).abs() < 1e-12);
    assert_eq!(got["state"]["loopEnd"], 1);
    // In frames of the sample (2000 of them).
    let got = ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "loop_start": 500, "loop_end": 1500, "unit": "frames"}),
    );
    assert_eq!(
        (
            got["state"]["loopStart"].as_f64(),
            got["state"]["loopEnd"].as_f64()
        ),
        (Some(0.25), Some(0.75))
    );
    let info = q(&doc, json!({"query": "sample_info", "node": node}));
    assert_eq!(
        (
            info["loop_start_frame"].as_i64(),
            info["loop_end_frame"].as_i64()
        ),
        (Some(500), Some(1500))
    );
    assert_eq!(info["loop_mode"], "pingpong");
    assert_eq!(info["trigger"], "oneshot");
    assert_eq!(info["root_note"], 72);
    assert_eq!(info["octave"], -1);
    assert_eq!(info["loaded"], true);

    // Refusals leave everything as it was, even the fields before the bad one.
    for (op, why) in [
        (
            json!({"op": "set_sample", "node": node, "gain": 1.0, "loop_mode": "sideways"}),
            "loop_mode",
        ),
        (
            json!({"op": "set_sample", "node": node, "gain": 1.0, "trigger": 7}),
            "trigger",
        ),
        (
            json!({"op": "set_sample", "node": node, "gain": "loud"}),
            "'gain'",
        ),
        (
            json!({"op": "set_sample", "node": node, "loop_start": 0.1, "unit": "bars"}),
            "'unit'",
        ),
        (
            json!({"op": "set_sample", "node": node, "active": 3}),
            "'active'",
        ),
    ] {
        let message = err(&mut doc, op.clone());
        assert!(message.contains(why), "{op}: {message}");
    }
    // Frames need a sample.
    let (mut empty, bare) = doc_with("sampler");
    let message = err(
        &mut empty,
        json!({"op": "set_sample", "node": bare, "loop_end": 10, "unit": "frames"}),
    );
    assert!(message.contains("load_sample"), "{message}");
}

#[test]
fn the_waveform_view_asks_for_peaks() {
    let (mut doc, node) = doc_with("sampler");
    // A ramp from -1 to 1 over 1000 frames, so every bucket's range is known.
    let ramp: Vec<f32> = (0..1000).map(|n| n as f32 / 500.0 - 1.0).collect();
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&ramp, 1, 8000)}),
    );
    let all = q(
        &doc,
        json!({"query": "sample_peaks", "node": node, "buckets": 4}),
    );
    assert_eq!(
        (
            all["from"].as_i64(),
            all["to"].as_i64(),
            all["frames"].as_i64(),
            all["buckets"].as_i64()
        ),
        (Some(0), Some(1000), Some(1000), Some(4))
    );
    let low: Vec<f64> = all["min"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    let high: Vec<f64> = all["max"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    for b in 0..4 {
        assert!((low[b] - (-1.0 + b as f64 * 0.5)).abs() < 1e-3, "{low:?}");
        assert!((high[b] - (-0.5 + b as f64 * 0.5)).abs() < 5e-3, "{high:?}");
    }
    // A part of it (zoomed), with fewer frames than buckets.
    let part = q(
        &doc,
        json!({"query": "sample_peaks", "node": node, "buckets": 100, "from": 100, "to": 108}),
    );
    assert_eq!(part["buckets"], 8);
    assert_eq!(
        (part["from"].as_i64(), part["to"].as_i64()),
        (Some(100), Some(108))
    );
    // Out of range is clamped, the default is 256 buckets.
    let wide = q(
        &doc,
        json!({"query": "sample_peaks", "node": node, "from": 900, "to": 50000}),
    );
    assert_eq!(
        (
            wide["from"].as_i64(),
            wide["to"].as_i64(),
            wide["buckets"].as_i64()
        ),
        (Some(900), Some(1000), Some(100))
    );
    let default = q(&doc, json!({"query": "sample_peaks", "node": node}));
    assert_eq!(default["buckets"], 256);
    // No sample, no peaks.
    let (empty, bare) = doc_with("sampler");
    assert!(empty
        .query(&json!({"query": "sample_peaks", "node": bare}))
        .unwrap_err()
        .contains("load_sample"));
    assert!(empty
        .query(&json!({"query": "sample_wav", "node": bare}))
        .unwrap_err()
        .contains("load_sample"));
    assert!(doc
        .query(&json!({"query": "sample_peaks", "node": node, "buckets": 0}))
        .is_err());
}

#[test]
fn reads_are_not_edits_and_edits_are_not_reads() {
    let (mut doc, node) = doc_with("sampler");
    let message = doc
        .apply(&json!({"op": "sample_peaks", "node": node}))
        .unwrap_err();
    assert!(
        message.contains("only reads") && message.contains("query"),
        "{message}"
    );
    assert!(doc
        .apply(&json!({"op": "wavetable_preview", "node": node}))
        .is_err());
    let message = doc
        .query(&json!({"query": "load_sample", "node": node}))
        .unwrap_err();
    assert!(message.contains("apply"), "{message}");
    assert!(doc
        .query(&json!({"query": "nonsense"}))
        .unwrap_err()
        .contains("unknown query"));
    assert!(doc
        .query(&json!({"node": node}))
        .unwrap_err()
        .contains("'query'"));
}

#[test]
fn a_loaded_sample_is_heard_and_a_different_one_sounds_different() {
    let (mut doc, node) = doc_with("sampler");
    let rate = RATE as u32;
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&sine(1000.0, rate, 48_000, 0.8), 1, rate), "root_note": 69}),
    );
    ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "loop_mode": "loop", "trigger": "gate"}),
    );
    let low = heard(&doc, 440.0);
    assert!(rms(&low) > 0.05, "the sample is heard: {}", rms(&low));
    let low_pitch = pitch(&low);
    assert!(
        (low_pitch - 1000.0).abs() < 30.0,
        "a 1 kHz sample at its root: {low_pitch}"
    );
    // Loaded again, a 2.5 kHz one: the sound follows the document.
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&sine(2500.0, rate, 48_000, 0.8), 1, rate), "root_note": 69}),
    );
    ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "loop_mode": "loop"}),
    );
    let high = heard(&doc, 440.0);
    let high_pitch = pitch(&high);
    assert!((high_pitch - 2500.0).abs() < 60.0, "{high_pitch}");
    // An octave up in the root note: half the speed.
    ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "root_note": 81}),
    );
    let slow = pitch(&heard(&doc, 440.0));
    assert!((slow - 1250.0).abs() < 40.0, "{slow}");
    // A loop of a part of the sample plays that part only: the first fifth is silence, then a tone.
    let mut gap = vec![0.0f32; 9_600];
    gap.extend(sine(1000.0, rate, 38_400, 0.8));
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&gap, 1, rate), "root_note": 69}),
    );
    ok(
        &mut doc,
        json!({"op": "set_sample", "node": node, "loop_mode": "loop", "loop_start": 0.5, "loop_end": 1.0}),
    );
    let looped = heard(&doc, 440.0);
    let tail = &looped[looped.len() / 2..];
    assert!(
        rms(tail) > 0.05,
        "the loop is the tone, not the gap: {}",
        rms(tail)
    );
    // Cleared, the node plays its built-in tone again (a sine of 220 Hz at root 69).
    ok(&mut doc, json!({"op": "clear_sample", "node": node}));
    assert!(rms(&heard(&doc, 440.0)) > 0.01);
}

// ---------------------------------------------------------------------------------------
// history

#[test]
fn the_history_is_off_until_asked_for_and_undoes_an_asset_in_one_step() {
    let (mut doc, node) = doc_with("sampler");
    assert!(doc
        .apply(&json!({"op": "undo"}))
        .unwrap_err()
        .contains("nothing to undo"));
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&[0.3; 500], 1, 8000)}),
    );
    assert_eq!(q(&doc, json!({"query": "history"}))["enabled"], false);
    assert!(
        doc.apply(&json!({"op": "undo"})).is_err(),
        "nothing was recorded"
    );

    ok(&mut doc, json!({"op": "set_history", "steps": 20}));
    let first = asset(&doc, &format!("sample_{node}"));
    let state_first = doc.value()["synthState"]["samplers"][&node].clone();
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&[0.6; 900], 1, 16000), "name": "b.wav"}),
    );
    let second = asset(&doc, &format!("sample_{node}"));
    assert_ne!(first, second);
    ok(
        &mut doc,
        json!({"op": "set_param", "id": node, "key": "gain", "value": 0.7}),
    );
    let h = q(&doc, json!({"query": "history"}));
    assert_eq!(
        (
            h["undo"].as_i64(),
            h["redo"].as_i64(),
            h["next_undo"].as_str()
        ),
        (Some(2), Some(0), Some("set_param"))
    );
    assert!(h["asset_bytes"].as_u64().unwrap() > 0);

    // Undo the gain, then the sample (the asset and the node's state come back together).
    let u = ok(&mut doc, json!({"op": "undo"}));
    assert_eq!(u["undone"], "set_param");
    assert_eq!(doc.get_param(&node, "gain").unwrap(), json!(1));
    ok(&mut doc, json!({"op": "undo"}));
    assert_eq!(asset(&doc, &format!("sample_{node}")), first);
    assert_eq!(doc.value()["synthState"]["samplers"][&node], state_first);
    assert!(
        doc.apply(&json!({"op": "undo"})).is_err(),
        "the step before the history was switched on is not there"
    );
    // Redo both.
    ok(&mut doc, json!({"op": "redo"}));
    assert_eq!(asset(&doc, &format!("sample_{node}")), second);
    ok(&mut doc, json!({"op": "redo"}));
    assert_eq!(doc.get_param(&node, "gain").unwrap(), json!(0.7));
    assert!(doc
        .apply(&json!({"op": "redo"}))
        .unwrap_err()
        .contains("nothing to redo"));
    // A new edit forgets the redo.
    ok(&mut doc, json!({"op": "undo"}));
    ok(
        &mut doc,
        json!({"op": "set_param", "id": node, "key": "gain", "value": 0.2}),
    );
    assert_eq!(q(&doc, json!({"query": "history"}))["redo"], 0);
}

#[test]
fn undo_puts_back_a_cleared_sample_a_removed_node_and_a_removed_wavetable() {
    let (mut doc, node) = doc_with("sampler");
    let (_, wt) = (0, doc.add_node("wavetable_oscillator").unwrap());
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": node, "data": wav_b64(&[0.3; 500], 1, 8000)}),
    );
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "saw", "frames": 3}),
    );
    ok(&mut doc, json!({"op": "set_history", "steps": 10}));
    let before = doc.clone();
    ok(&mut doc, json!({"op": "clear_sample", "node": node}));
    ok(&mut doc, json!({"op": "clear_wavetable", "node": wt}));
    ok(&mut doc, json!({"op": "remove_node", "id": node}));
    ok(&mut doc, json!({"op": "remove_node", "id": wt}));
    assert!(doc.value()["audioAssets"].as_object().unwrap().is_empty());
    for _ in 0..4 {
        ok(&mut doc, json!({"op": "undo"}));
    }
    assert_eq!(
        doc, before,
        "four undos give the document back, assets and all"
    );
    for _ in 0..4 {
        ok(&mut doc, json!({"op": "redo"}));
    }
    assert!(doc.value()["audioAssets"].as_object().unwrap().is_empty());
    assert!(doc.node(&node).is_none());
}

#[test]
fn a_group_is_one_step_and_the_editors_own_data_is_not_undone() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "sine"}),
    );
    ok(&mut doc, json!({"op": "set_history", "steps": 10}));
    let start = asset(&doc, &format!("wavetable_{wt}"));
    for i in 0..30 {
        ok(
            &mut doc,
            json!({"op": "wavetable_from_harmonics", "node": wt, "harmonics": [1, 0.5, i as f64 / 30.0], "group": "drag-1"}),
        );
        // A drag moves nodes too; the editor's key is not a step.
        ok(
            &mut doc,
            json!({"op": "set_editor", "editor": {"x": i}, "group": "drag-1"}),
        );
    }
    let h = q(&doc, json!({"query": "history"}));
    assert_eq!(h["undo"], 1, "thirty edits of a drag are one step");
    // The step holds the one old table, not thirty.
    assert!(
        h["asset_bytes"].as_u64().unwrap() < 2 * start["base64Data"].as_str().unwrap().len() as u64
    );
    ok(&mut doc, json!({"op": "undo"}));
    assert_eq!(asset(&doc, &format!("wavetable_{wt}")), start);
    assert_eq!(
        doc.editor().unwrap()["x"],
        29,
        "the editor's data stayed as it was"
    );
    // A different group (or none) is a new step.
    ok(&mut doc, json!({"op": "redo"}));
    ok(
        &mut doc,
        json!({"op": "set_param", "id": wt, "key": "gain", "value": 0.3, "group": "g"}),
    );
    ok(
        &mut doc,
        json!({"op": "set_param", "id": wt, "key": "gain", "value": 0.4, "group": "h"}),
    );
    assert_eq!(q(&doc, json!({"query": "history"}))["undo"], 3);
    // Setting the same value is not a step.
    ok(
        &mut doc,
        json!({"op": "set_param", "id": wt, "key": "gain", "value": 0.4}),
    );
    assert_eq!(q(&doc, json!({"query": "history"}))["undo"], 3);
}

#[test]
fn the_history_is_bounded_in_steps_and_in_bytes_and_a_clone_has_none() {
    let (mut doc, node) = doc_with("sampler");
    ok(&mut doc, json!({"op": "set_history", "steps": 3}));
    for i in 0..10 {
        ok(
            &mut doc,
            json!({"op": "set_param", "id": node, "key": "gain", "value": i as f64 / 10.0}),
        );
    }
    assert_eq!(q(&doc, json!({"query": "history"}))["undo"], 3);
    ok(&mut doc, json!({"op": "undo"}));
    ok(&mut doc, json!({"op": "undo"}));
    ok(&mut doc, json!({"op": "undo"}));
    assert_eq!(doc.get_param(&node, "gain").unwrap(), json!(0.6));
    assert!(doc.apply(&json!({"op": "undo"})).is_err());
    assert!(
        doc.clone().apply(&json!({"op": "redo"})).is_err(),
        "a clone has no history"
    );
    // Samples of 1 MiB each: the steps hold at most 128 MiB of them, oldest first.
    ok(&mut doc, json!({"op": "set_history", "steps": 1000}));
    let big: Vec<f32> = vec![0.1; 700_000];
    let data = wav_b64(&big, 1, 8000);
    for i in 0..220 {
        ok(
            &mut doc,
            json!({"op": "load_sample", "node": node, "data": data, "name": format!("{i}.wav")}),
        );
    }
    let h = q(&doc, json!({"query": "history"}));
    assert!(h["asset_bytes"].as_u64().unwrap() <= 128 << 20, "{h}");
    assert!(
        h["undo"].as_u64().unwrap() > 50 && h["undo"].as_u64().unwrap() < 220,
        "{h}"
    );
    // Switching it off forgets it.
    ok(&mut doc, json!({"op": "set_history", "steps": 0}));
    assert_eq!(q(&doc, json!({"query": "history"}))["undo"], 0);
    assert!(doc
        .apply(&json!({"op": "set_history", "steps": 5000}))
        .is_err());
}

// ---------------------------------------------------------------------------------------
// wavetables

#[test]
fn a_wavetable_goes_into_the_patch_as_the_daws_float_wav() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    let got = ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "sawtooth", "frames": 4}),
    );
    assert_eq!(
        (got["frames"].as_i64(), got["size"].as_i64()),
        (Some(4), Some(2048))
    );
    let a = asset(&doc, &format!("wavetable_{wt}"));
    assert_eq!(a["type"], "wavetable");
    assert_eq!(a["id"], json!(format!("wavetable_{wt}")));
    assert_eq!(a["channels"], 1);
    assert_eq!(a["sampleRate"], 44_100);
    let wav = base64_decode(a["base64Data"].as_str().unwrap()).unwrap();
    // `encodeWavFile`: format 3 (float), mono, 44.1 kHz, 32 bits, 44 bytes of header.
    assert_eq!(wav.len(), 44 + 4 * 2048 * 4);
    assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 3);
    assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 44_100);
    assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 32);
    assert_eq!(
        u32::from_le_bytes(wav[40..44].try_into().unwrap()),
        4 * 2048 * 4
    );
    // The frame is the editor's sawtooth (32 harmonics, normalised), sample for sample.
    let preset = cubed_dsp::wavetable_edit::preset("sawtooth", 32).unwrap();
    let table = cubed_dsp::wavetable_edit::generate_table(
        &preset.keyframes,
        &preset.warps,
        4,
        2048,
        false,
        true,
    )
    .unwrap();
    assert_eq!(decode_wav(&wav).unwrap().samples, table.data);
    assert!(doc.validate().is_empty());
    // The node's other parameters are untouched.
    assert_eq!(doc.get_param(&wt, "wave_index").unwrap(), json!(0));
}

#[test]
fn the_wavetable_survives_saving_and_loading() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "analog_pwm", "frames": 16, "harmonics": 24}),
    );
    let info = q(&doc, json!({"query": "wavetable_info", "node": wt}));
    let again = PatchDoc::from_json(&doc.to_json()).unwrap();
    assert_eq!(again, doc);
    assert_eq!(
        q(&again, json!({"query": "wavetable_info", "node": wt})),
        info
    );
    assert_eq!(wave_of(&again, &wt, 5, 64), wave_of(&doc, &wt, 5, 64));
}

#[test]
fn presets_are_the_daws_twelve_and_a_few_of_ours_and_refuse_nonsense() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    let list = q(&doc, json!({"query": "wavetable_presets"}));
    let names: Vec<&str> = list["presets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    for daw in [
        "analogPWM",
        "pulsePWM",
        "sine",
        "harmonicSeries",
        "square",
        "sawtooth",
        "triangle",
        "brightBrass",
        "warmPad",
        "vocalFormant",
        "metallicBell",
        "evolvingTexture",
    ] {
        assert!(names.contains(&daw), "{daw}");
    }
    for name in &names {
        let got = ok(
            &mut doc,
            json!({"op": "wavetable_preset", "node": wt, "preset": name, "frames": 3}),
        );
        assert!(got["frames"].as_i64().unwrap() >= 1, "{name}");
    }
    // Frames: a preset that moves has 256 by default, one that does not, one.
    assert_eq!(
        ok(
            &mut doc,
            json!({"op": "wavetable_preset", "node": wt, "preset": "pulsePWM"})
        )["frames"],
        256
    );
    assert_eq!(
        ok(
            &mut doc,
            json!({"op": "wavetable_preset", "node": wt, "preset": "square"})
        )["frames"],
        1
    );
    assert_eq!(
        ok(
            &mut doc,
            json!({"op": "wavetable_preset", "node": wt, "preset": "default"})
        )["frames"],
        4
    );
    for (op, why) in [
        (
            json!({"op": "wavetable_preset", "node": wt, "preset": "kazoo"}),
            "no preset",
        ),
        (
            json!({"op": "wavetable_preset", "node": wt, "preset": "sine", "frames": 0}),
            "'frames'",
        ),
        (
            json!({"op": "wavetable_preset", "node": wt, "preset": "sine", "frames": 257}),
            "'frames'",
        ),
        (
            json!({"op": "wavetable_preset", "node": wt, "preset": "sine", "harmonics": 2000}),
            "'harmonics'",
        ),
        (json!({"op": "wavetable_preset", "node": wt}), "'preset'"),
        (
            json!({"op": "wavetable_preset", "node": wt, "preset": "sine", "normalize": 1.5}),
            "'normalize'",
        ),
    ] {
        let message = err(&mut doc, op.clone());
        assert!(message.contains(why), "{op}: {message}");
    }
    let osc = doc.add_node("oscillator").unwrap();
    assert!(err(
        &mut doc,
        json!({"op": "wavetable_preset", "node": osc, "preset": "sine"})
    )
    .contains("not a Wavetable oscillator"));
}

#[test]
fn a_table_is_set_from_frames_of_any_length_integers_or_a_wav_file() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    // Two frames of 256 samples (stored at 2048, band limited, so a sine stays a sine).
    let one: Vec<f64> = (0..256)
        .map(|n| (2.0 * std::f64::consts::PI * n as f64 / 256.0).sin())
        .collect();
    let two: Vec<f64> = (0..256)
        .map(|n| (4.0 * std::f64::consts::PI * n as f64 / 256.0).sin() * 0.5)
        .collect();
    let got = ok(
        &mut doc,
        json!({"op": "set_wavetable", "node": wt, "frames": [one, two], "name": "mine.wav"}),
    );
    assert_eq!(
        (
            got["frames"].as_i64(),
            got["size"].as_i64(),
            got["truncated"].as_bool()
        ),
        (Some(2), Some(2048), Some(false))
    );
    let frame1 = wave_of(&doc, &wt, 1, 2048);
    for (i, v) in frame1.iter().enumerate() {
        let want = (4.0 * std::f64::consts::PI * i as f64 / 2048.0).sin() * 0.5;
        assert!((v - want).abs() < 1e-4, "{i}: {v} {want}");
    }
    assert_eq!(
        asset(&doc, &format!("wavetable_{wt}"))["fileName"],
        "mine.wav"
    );
    // 16-bit integers.
    let ints: Vec<i64> = (0..64)
        .map(|n| if n < 32 { 16384 } else { -16384 })
        .collect();
    ok(
        &mut doc,
        json!({"op": "set_wavetable", "node": wt, "frames": [ints], "format": "i16", "size": 64}),
    );
    let square = wave_of(&doc, &wt, 0, 8);
    assert!(square[1] > 0.4 && square[5] < -0.4, "{square:?}");
    // A WAV file, cut into frames of 2048 (the DAW's import), the rest dropped.
    let mut samples = Vec::new();
    for f in 0..3 {
        samples.extend(
            (0..2048).map(|n| ((n as f32 / 2048.0) * (f + 1) as f32 * std::f32::consts::TAU).sin()),
        );
    }
    samples.extend([0.5; 100]);
    let wav = encode_wav_f32(&samples, 44_100);
    let got = ok(
        &mut doc,
        json!({"op": "set_wavetable", "node": wt, "wav": base64_encode(&wav)}),
    );
    assert_eq!(got["frames"], 3);
    // The same file in frames of 1024: six of them, stored at 2048.
    let got = ok(
        &mut doc,
        json!({"op": "set_wavetable", "node": wt, "wav": base64_encode(&wav), "size": 1024}),
    );
    assert_eq!(got["frames"], 6);
    // Normalised, per frame.
    let quiet: Vec<f64> = (0..64).map(|n| 0.1 * (n as f64 * 0.3).sin()).collect();
    let got = ok(
        &mut doc,
        json!({"op": "set_wavetable", "node": wt, "frames": [quiet], "normalize": true}),
    );
    assert!((got["peak"].as_f64().unwrap() - 1.0).abs() < 1e-3);
    // More than 256 frames are cut.
    let many: Vec<Vec<f64>> = (0..300).map(|_| vec![0.0, 1.0, 0.0, -1.0]).collect();
    let got = ok(
        &mut doc,
        json!({"op": "set_wavetable", "node": wt, "frames": many}),
    );
    assert_eq!(
        (got["frames"].as_i64(), got["truncated"].as_bool()),
        (Some(256), Some(true))
    );

    for (op, why) in [
        (
            json!({"op": "set_wavetable", "node": wt}),
            "needs 'frames' or 'wav'",
        ),
        (
            json!({"op": "set_wavetable", "node": wt, "frames": []}),
            "empty",
        ),
        (
            json!({"op": "set_wavetable", "node": wt, "frames": [[0, 1, 0, -1], [0, 1]]}),
            "frame 1 has 2 samples",
        ),
        (
            json!({"op": "set_wavetable", "node": wt, "frames": [[0, 1]]}),
            "at least 4",
        ),
        (
            json!({"op": "set_wavetable", "node": wt, "frames": [[0, 1, 0, "x"]]}),
            "not a number",
        ),
        (
            json!({"op": "set_wavetable", "node": wt, "frames": [[0, 1, 0, -1]], "size": 8}),
            "frame 0 has 4",
        ),
        (
            json!({"op": "set_wavetable", "node": wt, "frames": [[0, 1, 0, -1]], "format": "u8"}),
            "'format'",
        ),
        (
            json!({"op": "set_wavetable", "node": wt, "wav": "!!"}),
            "base64",
        ),
        (
            json!({"op": "set_wavetable", "node": wt, "wav": base64_encode(&encode_wav_f32(&[0.0; 100], 44_100))}),
            "at least one frame",
        ),
    ] {
        let message = err(&mut doc, op.clone());
        assert!(message.contains(why), "{op}: {message}");
    }
}

#[test]
fn a_frame_is_made_from_harmonics_and_the_harmonics_read_back() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    // No table yet: one is made, of three copies.
    let got = ok(
        &mut doc,
        json!({"op": "wavetable_from_harmonics", "node": wt, "harmonics": [1.0, 0.0, 0.3], "phases": [0.0, 0.0, 1.0], "frames": 3, "normalize": false}),
    );
    assert_eq!(got["frames"], 3);
    // The DAW's sum of amplitude * sin(2 pi h i / n + phase), exactly.
    let frame = wave_of(&doc, &wt, 0, 2048);
    for i in [0usize, 100, 500, 1500] {
        let t = 2.0 * std::f64::consts::PI * i as f64 / 2048.0;
        let want = t.sin() + 0.3 * (3.0 * t + 1.0).sin();
        assert!((frame[i] - want).abs() < 1e-6, "{i}: {} {want}", frame[i]);
    }
    let back = q(
        &doc,
        json!({"query": "wavetable_harmonics", "node": wt, "frame": 2, "count": 4}),
    );
    let amps: Vec<f64> = back["amplitudes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    let phases: Vec<f64> = back["phases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    assert!(
        (amps[0] - 1.0).abs() < 1e-5
            && amps[1].abs() < 1e-5
            && (amps[2] - 0.3).abs() < 1e-5
            && amps[3].abs() < 1e-5,
        "{amps:?}"
    );
    assert!((phases[2] - 1.0).abs() < 1e-4);
    // One frame set from {amplitude, phase} objects, normalised (the editor's default): the others stay.
    let others = wave_of(&doc, &wt, 0, 64);
    ok(
        &mut doc,
        json!({"op": "wavetable_from_harmonics", "node": wt, "frame": 1, "harmonics": [{"amplitude": 0.2, "phase": 0.5}, {"amplitude": 0.1}]}),
    );
    let set = wave_of(&doc, &wt, 1, 2048);
    assert!((set.iter().fold(0.0f64, |m, x| m.max(x.abs())) - 1.0).abs() < 1e-5);
    assert_eq!(wave_of(&doc, &wt, 0, 64), others);
    // "all" sets every frame.
    ok(
        &mut doc,
        json!({"op": "wavetable_from_harmonics", "node": wt, "frame": "all", "harmonics": [1]}),
    );
    assert_eq!(wave_of(&doc, &wt, 0, 64), wave_of(&doc, &wt, 2, 64));
    for (op, why) in [
        (
            json!({"op": "wavetable_from_harmonics", "node": wt, "harmonics": []}),
            "harmonics",
        ),
        (
            json!({"op": "wavetable_from_harmonics", "node": wt}),
            "'harmonics'",
        ),
        (
            json!({"op": "wavetable_from_harmonics", "node": wt, "harmonics": [1], "frame": 3}),
            "no frame 3",
        ),
        (
            json!({"op": "wavetable_from_harmonics", "node": wt, "harmonics": ["a"]}),
            "number or",
        ),
        (
            json!({"op": "wavetable_from_harmonics", "node": wt, "harmonics": vec![1; 1025]}),
            "1 to 1024",
        ),
    ] {
        let message = err(&mut doc, op.clone());
        assert!(message.contains(why), "{op}: {message}");
    }
}

#[test]
fn a_table_is_made_from_keyframes_and_warps_as_the_editor_does() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    let got = ok(
        &mut doc,
        json!({"op": "wavetable_from_keyframes", "node": wt, "frames": 5, "normalize": false,
               "keyframes": [
                   {"time": 100, "harmonics": [0, 1]},
                   {"time": 0, "harmonics": [1, 0]}]}),
    );
    assert_eq!(got["frames"], 5);
    // Frame 0 is the first harmonic, the last the second, the middle half of each (the keyframes
    // were given out of order and are sorted).
    let first = wave_of(&doc, &wt, 0, 2048);
    let last = wave_of(&doc, &wt, 4, 2048);
    let mid = wave_of(&doc, &wt, 2, 2048);
    for i in [100usize, 700, 1300] {
        let t = 2.0 * std::f64::consts::PI * i as f64 / 2048.0;
        assert!((first[i] - t.sin()).abs() < 1e-6);
        assert!((last[i] - (2.0 * t).sin()).abs() < 1e-6);
        assert!((mid[i] - 0.5 * (t.sin() + (2.0 * t).sin())).abs() < 1e-6);
    }
    // A warp keyframe bends the last frame's level.
    ok(
        &mut doc,
        json!({"op": "wavetable_from_keyframes", "node": wt, "frames": 2, "normalize": false,
        "keyframes": [{"time": 0, "harmonics": [1]}],
        "warps": [{"time": 0}, {"time": 100, "y_amount": 5.0}]}),
    );
    let flat = wave_of(&doc, &wt, 0, 2048);
    let bent = wave_of(&doc, &wt, 1, 2048);
    assert!((flat[512] - 1.0).abs() < 1e-9);
    assert!(
        (bent[256]).abs() < flat[256].abs(),
        "{} {}",
        bent[256],
        flat[256]
    );
    for (op, why) in [
        (
            json!({"op": "wavetable_from_keyframes", "node": wt}),
            "'keyframes'",
        ),
        (
            json!({"op": "wavetable_from_keyframes", "node": wt, "keyframes": []}),
            "'keyframes'",
        ),
        (
            json!({"op": "wavetable_from_keyframes", "node": wt, "keyframes": [{"time": 150, "harmonics": [1]}]}),
            "'time'",
        ),
        (
            json!({"op": "wavetable_from_keyframes", "node": wt, "keyframes": [{"time": 0}]}),
            "needs 'harmonics'",
        ),
        (
            json!({"op": "wavetable_from_keyframes", "node": wt, "keyframes": [{"time": 0, "harmonics": [1]}], "warps": [{"time": 0, "x_amount": 80}]}),
            "-4 to 50",
        ),
        (
            json!({"op": "wavetable_from_keyframes", "node": wt, "keyframes": [{"time": 0, "harmonics": [1]}], "warps": []}),
            "'warps'",
        ),
    ] {
        let message = err(&mut doc, op.clone());
        assert!(message.contains(why), "{op}: {message}");
    }
}

#[test]
fn frames_are_set_inserted_copied_deleted_blended_warped_and_normalised() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    // Starting from the built-in table (sine, triangle, saw, square): editing materialises it.
    assert_eq!(
        q(&doc, json!({"query": "wavetable_info", "node": wt}))["builtin"],
        true
    );
    let builtin_saw = wave_of(&doc, &wt, 2, 64);
    let got = ok(
        &mut doc,
        json!({"op": "set_frame", "node": wt, "frame": 0, "values": [0, 1, 0, -1], "normalize": true}),
    );
    assert_eq!(
        (got["frames"].as_i64(), got["frame"].as_i64()),
        (Some(4), Some(0))
    );
    assert_eq!(
        q(&doc, json!({"query": "wavetable_info", "node": wt}))["builtin"],
        false
    );
    assert_eq!(
        wave_of(&doc, &wt, 2, 64),
        builtin_saw,
        "the other frames are the built-in ones"
    );
    let tri = wave_of(&doc, &wt, 0, 8);
    assert!(tri[2] > 0.9 && tri[6] < -0.9, "{tri:?}");

    // insert (a copy of the one before), at the end, and as a copy of another frame.
    ok(&mut doc, json!({"op": "insert_frame", "node": wt, "at": 1}));
    assert_eq!(wave_of(&doc, &wt, 1, 64), wave_of(&doc, &wt, 0, 64));
    ok(
        &mut doc,
        json!({"op": "insert_frame", "node": wt, "at": 5, "source": 3}),
    );
    assert_eq!(
        q(&doc, json!({"query": "wavetable_info", "node": wt}))["frames"],
        6
    );
    ok(&mut doc, json!({"op": "insert_frame", "node": wt, "at": 0}));
    assert_eq!(wave_of(&doc, &wt, 0, 64), wave_of(&doc, &wt, 1, 64));
    // copy 7 -> 2, delete 0 and 1.
    ok(
        &mut doc,
        json!({"op": "copy_frame", "node": wt, "from": 6, "to": 2}),
    );
    assert_eq!(wave_of(&doc, &wt, 2, 64), wave_of(&doc, &wt, 6, 64));
    ok(
        &mut doc,
        json!({"op": "delete_frame", "node": wt, "frame": 0}),
    );
    ok(
        &mut doc,
        json!({"op": "delete_frame", "node": wt, "frame": 0}),
    );
    assert_eq!(
        q(&doc, json!({"query": "wavetable_info", "node": wt}))["frames"],
        5
    );

    // Blend the frames between 0 and 4.
    ok(
        &mut doc,
        json!({"op": "interpolate_frames", "node": wt, "from": 0, "to": 4}),
    );
    let (a, b, m) = (
        wave_of(&doc, &wt, 0, 64),
        wave_of(&doc, &wt, 4, 64),
        wave_of(&doc, &wt, 2, 64),
    );
    for i in 0..64 {
        assert!((m[i] - (a[i] + b[i]) / 2.0).abs() < 1e-6);
    }
    // Warp one frame (the others stay), then normalise all.
    let kept = wave_of(&doc, &wt, 1, 64);
    ok(
        &mut doc,
        json!({"op": "wavetable_warp", "node": wt, "frame": 3, "x_amount": 3.0, "y_amount": 2.0}),
    );
    assert_eq!(wave_of(&doc, &wt, 1, 64), kept);
    assert_ne!(wave_of(&doc, &wt, 3, 64), wave_of(&doc, &wt, 2, 64));
    ok(&mut doc, json!({"op": "normalize_wavetable", "node": wt}));
    for f in 0..5 {
        let peak = wave_of(&doc, &wt, f, 2048)
            .iter()
            .fold(0.0f64, |m, x| m.max(x.abs()));
        assert!((peak - 1.0).abs() < 1e-5, "frame {f}: {peak}");
    }
    // Back to the built-in table.
    assert_eq!(
        ok(&mut doc, json!({"op": "clear_wavetable", "node": wt})),
        json!({"cleared": true})
    );
    assert_eq!(
        ok(&mut doc, json!({"op": "clear_wavetable", "node": wt})),
        json!({"cleared": false})
    );
    assert_eq!(
        q(&doc, json!({"query": "wavetable_info", "node": wt}))["builtin"],
        true
    );

    // Refusals.
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "sine"}),
    );
    for (op, why) in [
        (
            json!({"op": "delete_frame", "node": wt, "frame": 0}),
            "keeps at least one frame",
        ),
        (
            json!({"op": "delete_frame", "node": wt, "frame": 4}),
            "no frame 4",
        ),
        (
            json!({"op": "set_frame", "node": wt, "frame": 1, "values": [0, 1, 0, -1]}),
            "no frame 1",
        ),
        (
            json!({"op": "set_frame", "node": wt, "frame": 0}),
            "'values'",
        ),
        (
            json!({"op": "set_frame", "node": wt, "frame": 0, "values": [0, 1, 0, -1], "size": 8}),
            "'size'",
        ),
        (
            json!({"op": "insert_frame", "node": wt, "at": 2}),
            "'at' is 0 to 1",
        ),
        (
            json!({"op": "insert_frame", "node": wt, "at": 0, "source": 3}),
            "no frame 3",
        ),
        (
            json!({"op": "copy_frame", "node": wt, "from": 0, "to": 1}),
            "no frame 1",
        ),
        (
            json!({"op": "interpolate_frames", "node": wt, "from": 0, "to": 0}),
            "must be before",
        ),
        (
            json!({"op": "wavetable_warp", "node": wt, "x_amount": 99}),
            "-4 to 50",
        ),
        (
            json!({"op": "wavetable_warp", "node": wt, "frame": 5, "x_amount": 1}),
            "no frame 5",
        ),
    ] {
        let message = err(&mut doc, op.clone());
        assert!(message.contains(why), "{op}: {message}");
    }
    // 256 frames is the most.
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "sine", "frames": 256}),
    );
    assert!(
        err(&mut doc, json!({"op": "insert_frame", "node": wt, "at": 0})).contains("at most 256")
    );
}

#[test]
fn the_position_is_a_live_parameter_and_set_frame_position_answers_like_set_param() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "harmonic_series", "frames": 5}),
    );
    let got = ok(
        &mut doc,
        json!({"op": "set_frame_position", "node": wt, "frame": 3}),
    );
    assert_eq!(got["value"], 0.75);
    assert_eq!(doc.get_param(&wt, "wave_index").unwrap(), json!(0.75));
    let edit = &got["edit"];
    assert_eq!(
        edit["node"],
        json!(cubed_dsp::live::node_key(&wt).unwrap().to_string())
    );
    assert_eq!(edit["value"], 0.75);
    assert!(err(
        &mut doc,
        json!({"op": "set_frame_position", "node": wt, "frame": 5})
    )
    .contains("no frame 5"));
    // The built-in table has four frames, a table of one frame has position 0.
    let (mut fresh, other) = doc_with("wavetable_oscillator");
    let third = ok(
        &mut fresh,
        json!({"op": "set_frame_position", "node": other, "frame": 2}),
    )["value"]
        .as_f64()
        .unwrap();
    assert!((third - 2.0 / 3.0).abs() < 1e-6, "{third}");
    ok(
        &mut fresh,
        json!({"op": "wavetable_preset", "node": other, "preset": "sine"}),
    );
    assert_eq!(
        ok(
            &mut fresh,
            json!({"op": "set_frame_position", "node": other, "frame": 0})
        )["value"],
        0
    );
}

#[test]
fn the_views_a_wavetable_editor_draws() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "sawtooth", "frames": 2}),
    );
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "pulsePWM", "frames": 6, "harmonics": 16}),
    );
    let info = q(&doc, json!({"query": "wavetable_info", "node": wt}));
    assert_eq!(
        (
            info["frames"].as_i64(),
            info["size"].as_i64(),
            info["loaded"].as_bool()
        ),
        (Some(6), Some(2048), Some(true))
    );
    assert_eq!(info["name"], "pulsePWM.wav");
    assert_eq!(info["bytes"], 44 + 6 * 2048 * 4);
    // A preview of a frame, at a few points, and at a position between frames.
    let p = q(
        &doc,
        json!({"query": "wavetable_preview", "node": wt, "frame": 0, "points": 16}),
    );
    assert_eq!(p["points"], 16);
    assert_eq!(p["data"].as_array().unwrap().len(), 16);
    let frame0 = wave_of(&doc, &wt, 0, 16);
    assert_eq!(
        p["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect::<Vec<_>>(),
        frame0
    );
    let between = q(
        &doc,
        json!({"query": "wavetable_preview", "node": wt, "position": 0.1, "points": 8}),
    );
    let (f0, f1) = (wave_of(&doc, &wt, 0, 8), wave_of(&doc, &wt, 1, 8));
    for (i, v) in between["data"].as_array().unwrap().iter().enumerate() {
        // wave_index 0.1 of 5 intervals: halfway between frames 0 and 1.
        assert!((v.as_f64().unwrap() - (f0[i] + f1[i]) / 2.0).abs() < 1e-6);
    }
    // With neither, the node's own wave_index.
    ok(
        &mut doc,
        json!({"op": "set_param", "id": wt, "key": "wave_index", "value": 1.0}),
    );
    let at_end = q(
        &doc,
        json!({"query": "wavetable_preview", "node": wt, "points": 8}),
    );
    assert_eq!(
        at_end["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect::<Vec<_>>(),
        wave_of(&doc, &wt, 5, 8)
    );
    // The table in parts.
    let part = q(
        &doc,
        json!({"query": "wavetable", "node": wt, "from": 2, "count": 3, "size": 4}),
    );
    assert_eq!(
        (
            part["frames"].as_i64(),
            part["from"].as_i64(),
            part["size"].as_i64()
        ),
        (Some(6), Some(2), Some(4))
    );
    assert_eq!(part["data"].as_array().unwrap().len(), 3);
    assert_eq!(
        q(&doc, json!({"query": "wavetable", "node": wt}))["data"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    for (question, why) in [
        (
            json!({"query": "wavetable_preview", "node": wt, "frame": 6}),
            "no frame 6",
        ),
        (
            json!({"query": "wavetable_preview", "node": wt, "points": 1}),
            "'points'",
        ),
        (
            json!({"query": "wavetable", "node": wt, "from": 4, "count": 3}),
            "'count'",
        ),
        (
            json!({"query": "wavetable_harmonics", "node": wt, "count": 0}),
            "'count'",
        ),
        (
            json!({"query": "wavetable_info", "node": "nope"}),
            "no such node",
        ),
    ] {
        let message = doc.query(&question).unwrap_err();
        assert!(message.contains(why), "{question}: {message}");
    }
}

#[test]
fn a_wavetable_is_heard_and_an_edited_frame_is_heard_after_the_rebuild() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    ok(
        &mut doc,
        json!({"op": "set_param", "id": wt, "key": "gain", "value": 0.8}),
    );
    // A single frame (a table of one frame used to panic in the oscillator).
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "sine"}),
    );
    let sine_out = heard(&doc, 220.0);
    assert!(rms(&sine_out) > 0.1);
    assert!(
        (pitch(&sine_out) - 220.0).abs() < 5.0,
        "{}",
        pitch(&sine_out)
    );
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "square"}),
    );
    let square_out = heard(&doc, 220.0);
    assert!(
        brightness(&square_out) > 3.0 * brightness(&sine_out),
        "{} {}",
        brightness(&square_out),
        brightness(&sine_out)
    );
    // Frame edits: a table of a sine and a saw, then the saw frame becomes a sine.
    ok(
        &mut doc,
        json!({"op": "set_wavetable", "node": wt, "frames": [vec![0.0f64; 4], vec![0.0f64; 4]]}),
    );
    ok(
        &mut doc,
        json!({"op": "wavetable_from_harmonics", "node": wt, "frame": 0, "harmonics": [1]}),
    );
    let saw: Vec<f64> = (1..=40).map(|n| 1.0 / n as f64).collect();
    ok(
        &mut doc,
        json!({"op": "wavetable_from_harmonics", "node": wt, "frame": 1, "harmonics": saw}),
    );
    ok(
        &mut doc,
        json!({"op": "set_param", "id": wt, "key": "wave_index", "value": 1.0}),
    );
    let bright = brightness(&heard(&doc, 220.0));
    ok(
        &mut doc,
        json!({"op": "copy_frame", "node": wt, "from": 0, "to": 1}),
    );
    let dull = brightness(&heard(&doc, 220.0));
    assert!(
        bright > 5.0 * dull,
        "the saw frame is brighter than the copied sine: {bright} {dull}"
    );
    // The position morphs live: a ParamEdit on a running engine moves from the sine to the saw.
    ok(
        &mut doc,
        json!({"op": "wavetable_from_harmonics", "node": wt, "frame": 1, "harmonics": (1..=40).map(|n| 1.0 / n as f64).collect::<Vec<_>>()}),
    );
    let mut engine = engine_for(&doc);
    let got = ok(
        &mut doc,
        json!({"op": "set_frame_position", "node": wt, "frame": 0}),
    );
    let low = {
        engine
            .edit_param(cubed_dsp::live::ParamEdit::new(
                cubed_dsp::live::node_key(&wt).unwrap(),
                2, // wave_index
                got["edit"]["value"].as_f64().unwrap() as f32,
            ))
            .unwrap();
        brightness(&play(&mut engine, 220.0, 60, 0))
    };
    let got = ok(
        &mut doc,
        json!({"op": "set_frame_position", "node": wt, "frame": 1}),
    );
    engine
        .edit_param(cubed_dsp::live::ParamEdit::new(
            cubed_dsp::live::node_key(&wt).unwrap(),
            2,
            got["edit"]["value"].as_f64().unwrap() as f32,
        ))
        .unwrap();
    let high = brightness(&play(&mut engine, 220.0, 60, 0));
    assert!(high > 3.0 * low, "the position edit is heard: {low} {high}");
}

#[test]
fn a_wavetable_asset_from_the_daw_plays_on_the_native_engine() {
    // What the DAW's editor writes: 256 frames of 2048, 32-bit float. Here a sine in the first
    // frame and a saw in the last (all else the sine).
    let mut samples = Vec::new();
    for f in 0..256 {
        for n in 0..2048 {
            let t = n as f32 / 2048.0;
            samples.push(if f == 255 {
                (1..=30)
                    .map(|h| (std::f32::consts::TAU * h as f32 * t).sin() / h as f32)
                    .sum::<f32>()
                    * 0.5
            } else {
                (std::f32::consts::TAU * t).sin()
            });
        }
    }
    let (doc, wt) = doc_with("wavetable_oscillator");
    // Put it in as the DAW would have (not through the edit layer).
    let mut value: Value = serde_json::from_str(&doc.to_json()).unwrap();
    value["audioAssets"][format!("wavetable_{wt}")] = json!({
        "id": format!("wavetable_{wt}"), "type": "wavetable",
        "base64Data": base64_encode(&encode_wav_f32(&samples, 44_100)),
        "sampleRate": 44_100, "channels": 1, "fileName": "generated-wavetable.wav",
    });
    value["synthState"]["wavetableOscillators"][&wt]["gain"] = json!(0.8);
    let mut with_asset = PatchDoc::from_value(value).unwrap();
    let info = q(&with_asset, json!({"query": "wavetable_info", "node": wt}));
    assert_eq!(
        (info["frames"].as_i64(), info["loaded"].as_bool()),
        (Some(256), Some(true))
    );
    let dull = brightness(&heard(&with_asset, 220.0));
    ok(
        &mut with_asset,
        json!({"op": "set_param", "id": wt, "key": "wave_index", "value": 1.0}),
    );
    let bright = brightness(&heard(&with_asset, 220.0));
    assert!(
        bright > 3.0 * dull,
        "the saw at the end of the table is heard: {dull} {bright}"
    );
    // The edit layer reads and writes it as it does its own.
    ok(
        &mut with_asset,
        json!({"op": "copy_frame", "node": wt, "from": 0, "to": 255}),
    );
    assert!(brightness(&heard(&with_asset, 220.0)) < bright / 3.0);
}

#[test]
fn a_silent_frame_in_a_table_is_a_silent_table_not_a_failed_patch() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    ok(
        &mut doc,
        json!({"op": "set_wavetable", "node": wt, "frames": [[0, 0, 0, 0], [0.3, 0.3, 0.3, 0.3]]}),
    );
    let out = heard(&doc, 220.0);
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(rms(&out) < 1e-6, "silence (and a DC offset is not a tone)");
}

#[test]
fn the_edit_layer_leaves_everything_else_in_the_patch_alone() {
    let (mut doc, wt) = doc_with("wavetable_oscillator");
    let sampler = doc.add_node("sampler").unwrap();
    ok(
        &mut doc,
        json!({"op": "set_editor", "editor": {"nodes": {"a": [1, 2]}}}),
    );
    let before: Value = serde_json::from_str(&doc.to_json()).unwrap();
    ok(
        &mut doc,
        json!({"op": "wavetable_preset", "node": wt, "preset": "square"}),
    );
    ok(
        &mut doc,
        json!({"op": "load_sample", "node": sampler, "data": wav_b64(&[0.1; 64], 1, 8000)}),
    );
    let mut after: Value = serde_json::from_str(&doc.to_json()).unwrap();
    let assets = after["audioAssets"].as_object().unwrap().clone();
    assert_eq!(assets.len(), 2);
    // Only the two assets and the sampler's own state differ.
    after["audioAssets"] = json!({});
    after["synthState"]["samplers"][&sampler] = before["synthState"]["samplers"][&sampler].clone();
    assert_eq!(after, before);
    assert!(doc.validate().is_empty());
    // Every asset is in the DAW's shape: id, type, base64Data, sampleRate, channels.
    for (id, a) in assets {
        assert_eq!(a["id"], json!(id));
        for key in ["type", "base64Data", "sampleRate", "channels", "duration"] {
            assert!(!a[key].is_null(), "{id}.{key}");
        }
    }
}
