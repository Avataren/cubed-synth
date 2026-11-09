//! The mixer's equaliser (`EffectKind::Eq`): its parameters, its response against the
//! DAW's (numbers from another_synth's TypeScript, see `daw_reference.mjs`) and against
//! sines played through it, flat, live parameter changes and bypass without clicks, and its
//! numeric safety.

mod reference;

use cubed_mixer::eq::{response_db, BANDS, PARAM_COUNT};
use cubed_mixer::{effects::MAX_PARAMS, Effect, EffectKind, Insert};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;
const BAND_FREQS: [f32; BANDS] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

/// The parameter named `band<n>_<what>` (n from 1).
fn name(band: usize, what: &str) -> String {
    format!("band{}_{what}", band + 1)
}

fn eq() -> Effect {
    Effect::new(EffectKind::Eq)
}

fn set_gains(effect: &mut Effect, gains: &[f32]) {
    for (band, &g) in gains.iter().enumerate() {
        assert!(effect.set(&name(band, "gain_db"), g));
    }
}

fn params(effect: &Effect) -> Vec<f32> {
    effect.params[..PARAM_COUNT].to_vec()
}

fn response(effect: &Effect, rate: f32, hz: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0; hz.len()];
    response_db(&params(effect), rate, hz, &mut out);
    out
}

/// Runs `blocks` blocks of `signal(frame)` through the insert, returning the left channel
/// (the right one is checked to match).
fn run(
    insert: &mut dyn Insert,
    start_frame: usize,
    blocks: usize,
    signal: &dyn Fn(usize) -> f32,
) -> Vec<f32> {
    let mut out = Vec::with_capacity(blocks * BLOCK);
    for b in 0..blocks {
        let mut l: Vec<f32> = (0..BLOCK)
            .map(|i| signal(start_frame + b * BLOCK + i))
            .collect();
        let mut r = l.clone();
        insert.process(&mut l, &mut r);
        assert_eq!(l, r, "the channels differ");
        out.extend(l);
    }
    out
}

/// A sine of `period` frames and amplitude 0.5.
fn sine(period: usize) -> impl Fn(usize) -> f32 {
    move |n| 0.5 * (std::f32::consts::TAU * (n % period) as f32 / period as f32).sin()
}

/// The gain in dB of a sine with a period of `period` frames through the effect, measured
/// after it settled over whole periods (a lock-in on the output).
fn measured_gain_db(effect: &Effect, rate: f32, period: usize) -> f32 {
    let mut insert = effect.build(rate, BLOCK);
    let signal = sine(period);
    let settle = 24_000 / BLOCK;
    run(insert.as_mut(), 0, settle, &signal);
    // Whole periods, about a second.
    let frames = (rate as usize / period).max(1) * period;
    let blocks = frames.div_ceil(BLOCK);
    let start = settle * BLOCK;
    let out = run(insert.as_mut(), start, blocks, &signal);
    let periods = (out.len() / period).max(1);
    let n = periods * period;
    let (mut s, mut c) = (0.0f64, 0.0f64);
    for (i, &y) in out[..n].iter().enumerate() {
        let phase = std::f64::consts::TAU * ((start + i) % period) as f64 / period as f64;
        s += y as f64 * phase.sin();
        c += y as f64 * phase.cos();
    }
    let amplitude = 2.0 / n as f64 * (s * s + c * c).sqrt();
    (20.0 * (amplitude / 0.5).log10()) as f32
}

fn assert_close(what: &str, got: f32, want: f32, tolerance: f32) {
    assert!(
        (got - want).abs() <= tolerance,
        "{what}: {got} dB, wanted {want} dB (+-{tolerance})"
    );
}

#[test]
fn the_parameters_are_the_daws_bands() {
    let kind = EffectKind::Eq;
    assert_eq!(kind.name(), "eq");
    assert_eq!(EffectKind::from_name("eq"), Some(kind));
    assert_eq!(EffectKind::from_index(kind.index()), Some(kind));
    // New kinds go last: the other effects keep the numbers they have in the byte protocol.
    assert_eq!(kind.index() as usize, EffectKind::ALL.len() - 1);
    assert_eq!(EffectKind::Bitcrusher.index(), 8);
    let infos = kind.params();
    assert_eq!(infos.len(), PARAM_COUNT);
    assert_eq!(MAX_PARAMS, PARAM_COUNT);
    assert_eq!(infos[0].name, "bypass");
    assert_eq!(infos[1].name, "graphic");
    let effect = eq();
    for band in 0..BANDS {
        let freq = infos[kind.param(&name(band, "freq_hz")).unwrap()];
        assert_eq!(freq.default, BAND_FREQS[band]);
        assert_eq!((freq.min, freq.max, freq.unit), (20.0, 20_000.0, "Hz"));
        let gain = infos[kind.param(&name(band, "gain_db")).unwrap()];
        assert_eq!((gain.min, gain.max, gain.default), (-24.0, 24.0, 0.0));
        let q = effect.get(&name(band, "q")).unwrap();
        let shelf = band == 0 || band == BANDS - 1;
        assert_eq!(q, if shelf { 0.707_106_77 } else { 0.7 }, "band {band}");
        assert_eq!(effect.get(&name(band, "enabled")), Some(1.0));
        let kind_of = effect.get(&name(band, "type")).unwrap();
        assert_eq!(
            kind_of,
            [0.0, 1.0, 2.0][(band > 0) as usize + (band == BANDS - 1) as usize]
        );
    }
    assert_eq!(effect.get("bypass"), Some(0.0));
    assert_eq!(effect.get("graphic"), Some(0.0));
    // Names are unique and every default is in range.
    let mut names: Vec<_> = infos.iter().map(|p| p.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), PARAM_COUNT);
    for info in infos {
        assert!(
            info.min <= info.default && info.default <= info.max,
            "{}",
            info.name
        );
    }
}

#[test]
fn the_default_equaliser_is_flat() {
    let hz: Vec<f32> = reference::FREQS.to_vec();
    assert!(response(&eq(), RATE, &hz).iter().all(|&d| d == 0.0));
    // And passes the signal through untouched, bit for bit.
    let mut insert = eq().build(RATE, BLOCK);
    let signal = |n: usize| ((n * 7919 % 2003) as f32 / 1001.5 - 1.0) * 0.8;
    let out = run(insert.as_mut(), 0, 50, &signal);
    for (n, &y) in out.iter().enumerate() {
        assert_eq!(y, signal(n), "frame {n}");
    }
}

/// The DAW's bands at the DAW's Q, the gains applied as they are.
#[test]
fn parametric_response_matches_the_daw() {
    for case in &reference::RAW {
        let mut effect = eq();
        set_gains(&mut effect, &case.gains);
        let got = response(&effect, 48_000.0, &reference::FREQS);
        for (k, &want) in case.response.iter().enumerate() {
            assert_close(
                &format!("{} at {} Hz", case.name, reference::FREQS[k]),
                got[k],
                want as f32,
                0.002,
            );
        }
    }
}

/// Graphic mode: the response that the fitted gains give is the DAW's.
#[test]
fn graphic_response_matches_the_daw() {
    let mut worst = 0.0f32;
    for case in &reference::GRAPHIC {
        let mut effect = eq();
        effect.set("graphic", 1.0);
        set_gains(&mut effect, &case.set);
        let got = response(&effect, 48_000.0, &reference::FREQS);
        for (k, &want) in case.response.iter().enumerate() {
            worst = worst.max((got[k] - want as f32).abs());
            assert_close(
                &format!("{} at {} Hz", case.name, reference::FREQS[k]),
                got[k],
                want as f32,
                0.005,
            );
        }
    }
    println!("worst graphic deviation from the DAW: {worst} dB");
}

/// A graphic curve passes through the gains set at the band centres: that is its point.
#[test]
fn graphic_gains_are_the_curve() {
    let mut effect = eq();
    effect.set("graphic", 1.0);
    set_gains(&mut effect, &[6.0; BANDS]);
    let got = response(&effect, RATE, &BAND_FREQS);
    for (band, &g) in got.iter().enumerate() {
        assert_close(&format!("band {band}"), g, 6.0, 0.35);
    }
    // Where raw gains stack to about +9 dB between neighbours.
    let mut raw = eq();
    set_gains(&mut raw, &[6.0; BANDS]);
    let stacked = response(&raw, RATE, &[1_000.0])[0];
    assert!(stacked > 8.0, "{stacked}");
}

/// Sines through the real filters, at whole-period frequencies, against the design.
#[test]
fn a_sine_through_the_equaliser_has_the_designed_gain() {
    let mut cases: Vec<(&str, Effect)> = Vec::new();
    let mut smile = eq();
    smile.set("graphic", 1.0);
    set_gains(
        &mut smile,
        &[5.0, 4.0, 2.0, 0.0, -3.0, -3.0, 0.0, 2.0, 4.0, 5.0],
    );
    cases.push(("graphic smile", smile));
    let mut bass = eq();
    set_gains(
        &mut bass,
        &[6.0, 6.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, -9.0],
    );
    cases.push(("raw bass, treble cut", bass));
    let mut narrow = eq();
    narrow.set("band6_gain_db", 9.0);
    narrow.set("band6_q", 4.0);
    narrow.set("band3_gain_db", -12.0);
    narrow.set("band3_freq_hz", 120.0);
    narrow.set("band3_q", 2.5);
    narrow.set("band8_enabled", 1.0);
    narrow.set("band8_gain_db", 24.0);
    narrow.set("band8_freq_hz", 3_000.0);
    narrow.set("band8_q", 0.3);
    cases.push(("narrow bands", narrow));
    let mut moved_shelves = eq();
    moved_shelves.set("band1_gain_db", -10.0);
    moved_shelves.set("band1_freq_hz", 200.0);
    moved_shelves.set("band1_q", 1.5);
    moved_shelves.set("band10_gain_db", 7.0);
    moved_shelves.set("band10_freq_hz", 3_000.0);
    moved_shelves.set("band10_q", 0.5);
    cases.push(("moved shelves", moved_shelves));
    // Periods in frames at 48 kHz: 1,536 is 31.25 Hz, 3 is 16 kHz.
    let periods = [
        1536usize, 768, 384, 192, 96, 48, 24, 12, 6, 3, 400, 80, 40, 16,
    ];
    for (label, effect) in &cases {
        for &period in &periods {
            let hz = RATE / period as f32;
            let want = response(effect, RATE, &[hz])[0];
            let got = measured_gain_db(effect, RATE, period);
            assert_close(&format!("{label} at {hz} Hz"), got, want, 0.02);
        }
    }
}

#[test]
fn it_works_at_other_sample_rates() {
    let mut effect = eq();
    effect.set("graphic", 1.0);
    set_gains(
        &mut effect,
        &[3.0, 6.0, -4.0, 0.0, 5.0, -6.0, 2.0, 8.0, -3.0, 6.0],
    );
    for rate in [22_050.0f32, 44_100.0, 96_000.0] {
        // Periods that divide the rate: whole-period sines for the measurement.
        for hz in [50.0f32, 100.0, 250.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0] {
            let period = (rate / hz).round() as usize;
            let actual = rate / period as f32;
            let want = response(&effect, rate, &[actual])[0];
            let got = measured_gain_db(&effect, rate, period);
            assert_close(&format!("{rate} Hz rate, {actual} Hz"), got, want, 0.03);
        }
    }
}

#[test]
fn a_disabled_band_is_flat_and_bypass_is_dry() {
    let mut effect = eq();
    effect.set("band6_gain_db", 12.0);
    effect.set("band6_enabled", 0.0);
    assert!(response(&effect, RATE, &BAND_FREQS)
        .iter()
        .all(|&d| d == 0.0));
    effect.set("band6_enabled", 1.0);
    assert!(response(&effect, RATE, &[1_000.0])[0] > 11.0);
    // Bypass, built that way: the signal comes out as it went in.
    effect.set("bypass", 1.0);
    let mut insert = effect.build(RATE, BLOCK);
    let signal = sine(48);
    let out = run(insert.as_mut(), 0, 100, &signal);
    for (n, &y) in out.iter().enumerate().skip(BLOCK) {
        assert_eq!(y, signal(n), "frame {n}");
    }
}

/// The biggest change between consecutive samples.
fn max_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

/// Fails if the tone (`period` frames) jumps anywhere: a sample may differ from the one before
/// by no more than `factor` times what a sine as loud as its surroundings can step by.
fn assert_no_click(what: &str, out: &[f32], period: usize, factor: f32) {
    let w = std::f32::consts::TAU / period as f32;
    let mut worst = 0.0f32;
    for n in 1..out.len() {
        let near = &out[n.saturating_sub(period)..(n + period).min(out.len())];
        let loudest = near.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let step = (out[n] - out[n - 1]).abs();
        worst = worst.max(step / (w * loudest));
        assert!(
            step <= factor * w * loudest + 1e-4,
            "{what}: a step of {step} at frame {n}, in a tone {loudest} loud"
        );
    }
    println!("{what}: the biggest step is {worst} times a sine's");
}

/// A band moved while a tone plays through it: the output may swell, but it does not jump.
#[test]
fn live_changes_do_not_click() {
    let period = 192; // 250 Hz at 48 kHz, band 4's centre
    let signal = sine(period);
    let edits: [(&str, f32); 10] = [
        ("band4_gain_db", 12.0),
        ("band4_gain_db", -12.0),
        ("band4_gain_db", 12.0),
        ("band4_freq_hz", 1_000.0),
        ("band4_freq_hz", 250.0),
        ("band4_q", 6.0),
        ("band4_enabled", 0.0),
        ("band4_enabled", 1.0),
        ("graphic", 1.0),
        ("bypass", 1.0),
    ];
    let mut effect = eq();
    effect.set("band4_gain_db", 9.0);
    effect.set("band5_gain_db", -6.0);
    let mut insert = effect.build(RATE, BLOCK);
    let mut frame = 0;
    run(insert.as_mut(), frame, 100, &signal);
    frame += 100 * BLOCK;
    for (param, value) in edits {
        insert.set_param(EffectKind::Eq.param(param).unwrap(), value);
        let out = run(insert.as_mut(), frame, 120, &signal);
        frame += 120 * BLOCK;
        assert_no_click(&format!("{param} = {value}"), &out, period, 1.35);
    }
    // A fast, wide sweep of the gain: a new value every block.
    let mut insert = eq().build(RATE, BLOCK);
    let gain = EffectKind::Eq.param("band4_gain_db").unwrap();
    let mut out = Vec::new();
    for step in 0..400 {
        insert.set_param(gain, (step as f32 * 0.37).sin() * 20.0);
        out.extend(run(insert.as_mut(), step * BLOCK, 1, &signal));
    }
    assert!(out.iter().all(|s| s.is_finite()));
    assert_no_click("a gain sweep", &out, period, 1.9);
}

/// A step in one gain is a smooth glide: after 50 ms it has arrived, and not before 2 ms.
#[test]
fn a_gain_glides_in_a_few_milliseconds() {
    let mut insert = eq().build(RATE, BLOCK);
    let gain = EffectKind::Eq.param("band6_gain_db").unwrap();
    let signal = sine(48); // 1 kHz
    run(insert.as_mut(), 0, 40, &signal);
    insert.set_param(gain, 12.0);
    let out = run(insert.as_mut(), 40 * BLOCK, 20, &signal);
    let peak =
        |range: std::ops::Range<usize>| out[range].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak(0..48) < 0.65, "{}", peak(0..48));
    let want = 0.5 * 10f32.powf(12.0 / 20.0);
    assert!(
        (peak(2_400..2_448) - want).abs() < 0.02 * want,
        "{}",
        peak(2_400..2_448)
    );
}

#[test]
fn bypass_crossfades_in_five_milliseconds() {
    let mut effect = eq();
    effect.set("band6_gain_db", 12.0);
    let mut insert = effect.build(RATE, BLOCK);
    let signal = sine(48);
    run(insert.as_mut(), 0, 20, &signal);
    insert.set_param(EffectKind::Eq.param("bypass").unwrap(), 1.0);
    let out = run(insert.as_mut(), 20 * BLOCK, 10, &signal);
    // The first frames are still the filtered signal, within 5 ms the dry one.
    let peak =
        |range: std::ops::Range<usize>| out[range].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak(0..48) > 1.5, "{}", peak(0..48));
    for n in 300..out.len() {
        assert!((out[n] - signal(20 * BLOCK + n)).abs() < 1e-6, "frame {n}");
    }
    // A 1 kHz sine of amplitude 2 steps by up to 0.26 by itself; a click would be a whole unit.
    assert!(max_step(&out) < 0.32, "{}", max_step(&out));
    // And back again.
    insert.set_param(EffectKind::Eq.param("bypass").unwrap(), 0.0);
    let back = run(insert.as_mut(), 30 * BLOCK, 10, &signal);
    assert!(max_step(&back) < 0.32, "{}", max_step(&back));
    assert!(back[BLOCK * 8..].iter().fold(0.0f32, |m, s| m.max(s.abs())) > 1.5);
}

/// A graphic-mode edit changes several bands' applied gains at once: each glides.
#[test]
fn graphic_edits_do_not_click() {
    let period = 192;
    let signal = sine(period);
    // `speed` is how fast the gains swing (radians a block); the wild one is a new random-ish
    // set every 2.7 ms, far beyond what a hand does.
    for (what, speed, factor) in [
        ("dragging sliders", 0.05f32, 1.5f32),
        ("wild edits", 2.1, 2.0),
    ] {
        let gain_at =
            |step: usize, band: usize| (step as f32 * speed + band as f32 * 0.9).sin() * 12.0;
        let mut effect = eq();
        effect.set("graphic", 1.0);
        for band in 0..BANDS {
            effect.set(&name(band, "gain_db"), gain_at(0, band));
        }
        let mut insert = effect.build(RATE, BLOCK);
        let mut out = Vec::new();
        for step in 0..300 {
            for band in 0..BANDS {
                let gain = EffectKind::Eq.param(&name(band, "gain_db")).unwrap();
                insert.set_param(gain, gain_at(step, band));
            }
            out.extend(run(insert.as_mut(), step * BLOCK, 1, &signal));
        }
        assert!(out.iter().all(|s| s.is_finite()));
        assert_no_click(what, &out, period, factor);
    }
}

#[test]
fn bad_values_are_clamped_and_never_break_it() {
    let mut insert = eq().build(RATE, BLOCK);
    let signal = sine(48);
    for index in 0..MAX_PARAMS + 3 {
        for value in [f32::NAN, f32::INFINITY, -1e9, 1e9, 0.0] {
            insert.set_param(index, value);
        }
    }
    let out = run(insert.as_mut(), 0, 200, &signal);
    assert!(out.iter().all(|s| s.is_finite() && s.abs() < 100.0));
    let mut effect = eq();
    assert!(effect.set("band1_gain_db", 99.0));
    assert_eq!(effect.get("band1_gain_db"), Some(24.0));
    assert!(effect.set("band2_q", 0.0));
    assert_eq!(effect.get("band2_q"), Some(0.1));
    assert!(!effect.set("band11_gain_db", 1.0));
    // A frequency above Nyquist (a high band at a low rate) is held below it.
    let mut high = eq();
    high.set("band10_gain_db", 12.0);
    let mut insert = high.build(8_000.0, BLOCK);
    let out = run(insert.as_mut(), 0, 200, &signal);
    assert!(out.iter().all(|s| s.is_finite()));
}

/// After the signal ends the filters' states decay to exactly nothing, not into denormals.
#[test]
fn it_does_not_linger_in_denormals() {
    let mut effect = eq();
    effect.set("graphic", 1.0);
    set_gains(
        &mut effect,
        &[12.0, -12.0, 6.0, 8.0, -9.0, 12.0, -3.0, 6.0, -12.0, 12.0],
    );
    let mut insert = effect.build(RATE, BLOCK);
    run(insert.as_mut(), 0, 400, &sine(48));
    let tail = run(insert.as_mut(), 0, 5 * 375, &|_| 0.0);
    assert!(tail[tail.len() - 4 * BLOCK..].iter().all(|&s| s == 0.0));
    // A burst of denormal inputs does not leave anything behind either.
    let tiny = run(insert.as_mut(), 0, 400, &|n| {
        if n % 3 == 0 {
            1e-40
        } else {
            0.0
        }
    });
    assert!(tiny.iter().all(|s| s.is_finite()));
}

#[test]
fn reset_clears_the_tail() {
    let mut effect = eq();
    effect.set("band1_gain_db", 12.0);
    effect.set("band6_gain_db", 12.0);
    let mut insert = effect.build(RATE, BLOCK);
    run(insert.as_mut(), 0, 20, &sine(48));
    insert.reset();
    let out = run(insert.as_mut(), 0, 1, &|_| 0.0);
    assert!(out.iter().all(|&s| s == 0.0));
}

#[test]
fn response_helper_follows_the_parameters() {
    let mut effect = eq();
    effect.set("band6_gain_db", 6.0);
    let at = response(&effect, RATE, &[1_000.0, 250.0, 16_000.0]);
    assert_close("centre", at[0], 6.0, 1e-3);
    assert!(at[1].abs() < 1.0 && at[2].abs() < 0.2);
    // The frequency parameter moves the peak.
    effect.set("band6_freq_hz", 2_000.0);
    assert_close("moved", response(&effect, RATE, &[2_000.0])[0], 6.0, 1e-3);
    // A shelf's gain is reached beyond its corner (the low one is at 31 Hz).
    let mut shelf = eq();
    shelf.set("band1_gain_db", -12.0);
    shelf.set("band10_gain_db", 12.0);
    let at = response(&shelf, RATE, &[20.0, 4_000.0, 20_000.0]);
    assert!(at[0] < -9.0 && at[0] > -12.0, "{}", at[0]);
    assert!(at[1].abs() < 0.5);
    assert!(at[2] > 10.0);
}
