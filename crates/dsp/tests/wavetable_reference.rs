//! `wavetable_edit` against numbers the DAW's own TypeScript computed
//! (`fixtures/wavetable_reference.json`, made by `fixtures/make_wavetable_reference.py`
//! from `WavetableUtils.ts` and the preset code of `WavetableEditor.vue`).

use cubed_dsp::wavetable_edit::{
    apply_wave_warp, generate_from_harmonics, generate_table, preset, process_waveform, Harmonic,
    Keyframe, Warp, WarpKeyframe, PRESETS,
};
use serde_json::Value;

fn reference() -> Value {
    let text = include_str!("fixtures/wavetable_reference.json");
    serde_json::from_str(text).expect("the reference parses")
}

fn numbers(v: &Value) -> Vec<f64> {
    v.as_array()
        .expect("a list")
        .iter()
        .map(|x| x.as_f64().expect("a number"))
        .collect()
}

fn assert_close(got: &[f64], want: &[f64], tolerance: f64, what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: length");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!((g - w).abs() <= tolerance, "{what}[{i}]: {g} vs {w}");
    }
}

fn harmonic(v: &Value) -> Harmonic {
    Harmonic {
        amplitude: v["amplitude"].as_f64().unwrap(),
        phase: v["phase"].as_f64().unwrap(),
    }
}

#[test]
fn harmonics_and_warps_match_the_daws_numbers() {
    let r = reference();
    let wave = generate_from_harmonics(
        &[
            Harmonic {
                amplitude: 1.0,
                phase: 0.0,
            },
            Harmonic {
                amplitude: 0.5,
                phase: 0.3,
            },
            Harmonic {
                amplitude: 0.25,
                phase: -1.0,
            },
        ],
        16,
    );
    assert_close(&wave, &numbers(&r["harm16"]), 1e-12, "harm16");

    let ramp: Vec<f64> = (0..16).map(|i| (i as f64 / 15.0) * 2.0 - 1.0).collect();
    let symmetric = apply_wave_warp(
        &ramp,
        Warp {
            x_amount: 2.0,
            y_amount: 1.0,
            asymmetric: false,
        },
    );
    assert_close(&symmetric, &numbers(&r["warp_sym"]), 1e-12, "warp_sym");
    let asymmetric = apply_wave_warp(
        &ramp,
        Warp {
            x_amount: -1.5,
            y_amount: 3.0,
            asymmetric: true,
        },
    );
    assert_close(&asymmetric, &numbers(&r["warp_asym"]), 1e-12, "warp_asym");
}

#[test]
fn process_waveform_keeps_the_daws_peak_before_the_dc_is_taken_off() {
    let r = reference();
    let wave = [0.5f32, 1.5, 1.0, 0.0, 2.0, -1.0];
    for (key, dc, normalize) in [
        ("dc_norm", true, true),
        ("dc_only", true, false),
        ("norm_only", false, true),
    ] {
        let got: Vec<f64> = process_waveform(&wave, dc, normalize)
            .iter()
            .map(|&x| x as f64)
            .collect();
        assert_close(&got, &numbers(&r[key]), 1e-6, key);
    }
}

#[test]
fn the_twelve_presets_make_the_daws_frames() {
    let r = reference();
    let daw = [
        "sine",
        "harmonicSeries",
        "square",
        "sawtooth",
        "triangle",
        "pulsePWM",
        "analogPWM",
        "brightBrass",
        "warmPad",
        "vocalFormant",
        "metallicBell",
        "evolvingTexture",
    ];
    for name in daw {
        let want = &r["presets"][name];
        let p = preset(name, 32).unwrap_or_else(|| panic!("preset {name}"));
        assert_eq!(
            p.keyframes.len() as u64,
            want["keyframes"].as_u64().unwrap(),
            "{name}: keyframes"
        );
        // The reference has 5 frames of 32 samples, the editor's Apply settings.
        let table = generate_table(&p.keyframes, &p.warps, 5, 32, false, true).unwrap();
        for (f, frame) in want["frames"].as_array().unwrap().iter().enumerate() {
            let got: Vec<f64> = table.frame(f).iter().map(|&x| x as f64).collect();
            assert_close(&got, &numbers(frame), 2e-6, &format!("{name} frame {f}"));
        }
    }
    // They are all in the list of presets the API offers.
    for name in daw {
        assert!(PRESETS.iter().any(|(n, _, _)| *n == name), "{name}");
    }
}

#[test]
fn the_pwm_presets_have_the_daws_keyframes() {
    let r = reference();
    let pwm = preset("pulsePWM", 32).unwrap();
    for (i, want) in r["pwm_kf100"].as_array().unwrap().iter().enumerate() {
        let (got, want) = (pwm.keyframes[100].harmonics[i], harmonic(want));
        assert!(
            (got.amplitude - want.amplitude).abs() < 1e-12,
            "pulsePWM h{i}: {got:?} {want:?}"
        );
        assert!(
            (got.phase - want.phase).abs() < 1e-12,
            "pulsePWM h{i}: {got:?} {want:?}"
        );
    }
    let times = numbers(&r["pwm_kf_time"]);
    assert_close(
        &[
            pwm.keyframes[0].time,
            pwm.keyframes[100].time,
            pwm.keyframes[255].time,
        ],
        &times,
        1e-12,
        "times",
    );
    let analog = preset("analogPWM", 32).unwrap();
    for (i, want) in r["apwm_kf200"].as_array().unwrap().iter().enumerate() {
        let (got, want) = (analog.keyframes[200].harmonics[i], harmonic(want));
        assert!(
            (got.amplitude - want.amplitude).abs() < 1e-12,
            "analogPWM h{i}"
        );
        assert!((got.phase - want.phase).abs() < 1e-12, "analogPWM h{i}");
    }
    let tri = preset("triangle", 8).unwrap();
    for (i, want) in r["tri8"].as_array().unwrap().iter().enumerate() {
        let want = harmonic(want);
        assert!((tri.keyframes[0].harmonics[i].amplitude - want.amplitude).abs() < 1e-12);
        assert!((tri.keyframes[0].harmonics[i].phase - want.phase).abs() < 1e-12);
    }
}

#[test]
fn a_whole_export_matches_the_daws_file() {
    let r = reference();
    let h = |a: f64, p: f64| Harmonic {
        amplitude: a,
        phase: p,
    };
    let keyframes = vec![
        Keyframe {
            time: 0.0,
            harmonics: vec![h(1.0, 0.0), h(0.0, 0.0), h(0.0, 0.0)],
        },
        Keyframe {
            time: 100.0,
            harmonics: vec![h(0.3, 0.5), h(0.6, 1.0), h(0.2, 2.0)],
        },
    ];
    let warps = vec![
        WarpKeyframe {
            time: 0.0,
            params: Warp {
                x_amount: 0.0,
                y_amount: 0.0,
                asymmetric: false,
            },
        },
        WarpKeyframe {
            time: 100.0,
            params: Warp {
                x_amount: 1.0,
                y_amount: 2.0,
                asymmetric: true,
            },
        },
    ];
    let table = generate_table(&keyframes, &warps, 5, 16, true, true).unwrap();
    let want = &r["export5x16"];
    let got: Vec<f64> = table.data.iter().map(|&x| x as f64).collect();
    assert_close(&got, &numbers(&want["samples"]), 2e-6, "export");
    // The file: 44 bytes of header, 32-bit samples.
    assert_eq!(table.to_wav().len() as u64, want["bytes"].as_u64().unwrap());
}
