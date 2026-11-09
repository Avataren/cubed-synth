//! A wavetable of 256 frames is built (mipmapped) once: a rebuild of the patch, and a rebuild
//! after one frame was edited, only build what is new. Its own test binary, so that no other
//! test's tables count here.
#![cfg(feature = "native-host")]

mod common;
use common::*;
use cubed_dsp::nodes::morph_wavetable::frame_cache_stats;
use cubed_dsp::patch_edit::PatchDoc;
use cubed_dsp::traits::PortId;
use serde_json::json;
use std::sync::Mutex;
use std::time::Instant;

/// The two tests build tables; the cache counts are only the first test's if they take turns.
static TURN: Mutex<()> = Mutex::new(());

#[test]
fn a_rebuild_builds_only_the_frames_that_changed() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = PatchDoc::new("cache");
    let wt = doc.add_node("wavetable_oscillator").unwrap();
    let mixer = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "mixer")
        .unwrap()
        .id;
    doc.connect(
        &wt,
        PortId::AudioOutput0,
        &mixer,
        PortId::AudioInput0,
        1.0,
        2,
        0,
    )
    .unwrap();
    doc.apply(&json!({"op": "wavetable_preset", "node": wt, "preset": "pulsePWM", "frames": 256}))
        .unwrap();

    let build = |doc: &PatchDoc| {
        let t = Instant::now();
        let _engine = engine_with(&serde_json::from_str(&doc.to_json()).unwrap());
        t.elapsed()
    };
    let (_, built_before) = frame_cache_stats();
    let cold = build(&doc);
    let (hits, built) = frame_cache_stats();
    assert_eq!(
        built - built_before,
        256,
        "256 different frames are built the first time"
    );
    assert_eq!(hits, 0);

    let warm = build(&doc);
    let (hits, built_again) = frame_cache_stats();
    assert_eq!(built_again, built, "the second build builds nothing");
    assert_eq!(hits, 256);

    // One frame edited: one frame built.
    doc.apply(&json!({"op": "wavetable_from_harmonics", "node": wt, "frame": 100, "harmonics": [1, 0.3, 0.1]})).unwrap();
    let edited = build(&doc);
    let (_, built_after) = frame_cache_stats();
    assert_eq!(built_after - built_again, 1);

    // The numbers a user of the edit layer wants (run with --nocapture; a debug build is slow).
    eprintln!("build of a 256-frame patch: cold {cold:?}, warm {warm:?}, after one frame edited {edited:?}");
    assert!(
        warm < cold,
        "a warm build is the quicker one: {warm:?} {cold:?}"
    );
}

#[test]
fn what_the_operations_cost() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    use cubed_dsp::wav::{base64_encode, encode_wav_i16};
    let mut doc = PatchDoc::new("cost");
    let wt = doc.add_node("wavetable_oscillator").unwrap();
    let sampler = doc.add_node("sampler").unwrap();
    let time = |label: &str, f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        eprintln!("{label}: {:?}", t.elapsed());
    };
    time("wavetable_preset analogPWM, 256 frames", &mut || {
        doc.apply(
            &json!({"op": "wavetable_preset", "node": wt, "preset": "analogPWM", "frames": 256}),
        )
        .unwrap();
    });
    time("set_frame (one frame of a 256-frame table)", &mut || {
        doc.apply(
            &json!({"op": "set_frame", "node": wt, "frame": 7, "values": vec![0.5f64; 2048]}),
        )
        .unwrap();
    });
    time("wavetable_from_harmonics (one frame)", &mut || {
        doc.apply(&json!({"op": "wavetable_from_harmonics", "node": wt, "frame": 7, "harmonics": [1, 0.5, 0.25]})).unwrap();
    });
    time("wavetable_preview (256 points)", &mut || {
        doc.query(&json!({"query": "wavetable_preview", "node": wt, "frame": 7}))
            .unwrap();
    });
    time("wavetable_harmonics (32 of a frame)", &mut || {
        doc.query(&json!({"query": "wavetable_harmonics", "node": wt, "frame": 7}))
            .unwrap();
    });
    time("to_json (the document with the table)", &mut || {
        let _ = doc.to_json();
    });
    let seconds = 10;
    let samples: Vec<f32> = (0..44_100 * seconds * 2)
        .map(|n| (n as f32 * 0.01).sin() * 0.5)
        .collect();
    let data = base64_encode(&encode_wav_i16(&samples, 2, 44_100));
    eprintln!("a 10 s stereo sample is {} KB of base64", data.len() / 1000);
    time("load_sample (10 s stereo 44.1 kHz)", &mut || {
        doc.apply(&json!({"op": "load_sample", "node": sampler, "data": data}))
            .unwrap();
    });
    time(
        "sample_peaks (800 buckets, decoded and cached)",
        &mut || {
            doc.query(&json!({"query": "sample_peaks", "node": sampler, "buckets": 800}))
                .unwrap();
        },
    );
    let mut fresh = PatchDoc::from_json(&doc.to_json()).unwrap();
    time(
        "sample_peaks (800 buckets, first read of a loaded patch)",
        &mut || {
            fresh
                .query(&json!({"query": "sample_peaks", "node": sampler, "buckets": 800}))
                .unwrap();
        },
    );
    fresh
        .apply(&json!({"op": "set_history", "steps": 50}))
        .unwrap();
    time(
        "set_param with the history on (the patch holds both assets)",
        &mut || {
            fresh
                .apply(&json!({"op": "set_param", "id": sampler, "key": "gain", "value": 0.5}))
                .unwrap();
        },
    );
    time(
        "a rebuild of the patch with the sample and the table",
        &mut || {
            let _ = engine_with(&serde_json::from_str(&fresh.to_json()).unwrap());
        },
    );
}
