//! Band-limited ("hi-fi") oscillators (`ahx::hifi`, `AhxEngine::set_hifi`).
//!
//! Three claims, each with its own tests:
//!
//! 1. **Off is the reference.** Never touching the switch, or flipping it on
//!    and back off, renders the same bytes; the corpus-wide proof is the render
//!    goldens, which also run a hi-fi-toggled-off engine.
//! 2. **On removes fold-back.** A synthetic probe -- the highest note on the
//!    shortest tables, where the reference is at its worst -- has a known
//!    fundamental, so everything that is not one of its harmonics is aliasing
//!    (or hi-fi's own error). Robocop iii (the song that prompted this) is
//!    measured on its real high-pitched passages, one solo'd voice at a time.
//! 3. **On keeps the music.** Same timing, same loudness, and the same note.

use cubed_ahx::engine::AhxEngine;
use cubed_ahx::format::{self, Envelope, Instrument, PList, PListEntry, Position, Song, Step};
use cubed_ahx::voice::{WAVEFORM_NOISE, WAVEFORM_SAWTOOTH, WAVEFORM_SQUARE};
use rustfft::{num_complex::Complex, FftPlanner};
use std::fs;
use std::path::Path;

const RATE: u32 = 44100;
const BLOCKS: [usize; 4] = [128, 1, 333, 882];

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/songs")
            .join(name),
    )
    .unwrap()
}

fn engine_of(song: Song) -> AhxEngine {
    AhxEngine::new(song, RATE, 2).unwrap()
}

fn engine(name: &str) -> AhxEngine {
    engine_of(format::parse(&fixture(name)).unwrap())
}

fn render(e: &mut AhxEngine, frames: usize, blocks: &[usize]) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames * 2);
    let mut done = 0;
    let mut k = 0;
    while done < frames {
        let n = blocks[k % blocks.len()].min(frames - done);
        let at = out.len();
        out.resize(at + n * 2, 0);
        e.render_block(&mut out[at..]);
        done += n;
        k += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// 1. Off is the reference.
// ---------------------------------------------------------------------------

#[test]
fn hifi_off_is_bit_identical_to_never_touching_it() {
    for name in ["karma.ahx", "sunspots.hvl", "robocop_iii_j_tel.ahx"] {
        let frames = RATE as usize * 6;
        let plain = render(&mut engine(name), frames, &BLOCKS);
        assert!(plain.iter().any(|&s| s != 0), "{name}: silent");

        let mut off = engine(name);
        off.set_hifi(false);
        assert!(!off.hifi_enabled());
        assert_eq!(
            render(&mut off, frames, &BLOCKS),
            plain,
            "{name}: explicit off"
        );

        let mut cycled = engine(name);
        cycled.set_hifi(true);
        cycled.set_hifi(false);
        assert_eq!(
            render(&mut cycled, frames, &BLOCKS),
            plain,
            "{name}: on then off before the first sample"
        );
    }
}

#[test]
fn hifi_switched_off_mid_song_rejoins_the_reference_exactly() {
    // Song state, timing and every voice's position are independent of the
    // switch, so once it is off again the output is the reference's, sample
    // for sample -- there is nothing for hi-fi to leave behind.
    let name = "robocop_iii_j_tel.ahx";
    let head = RATE as usize * 3;
    let tail = RATE as usize * 3;
    let plain = render(&mut engine(name), head * 2 + tail, &BLOCKS);

    let mut e = engine(name);
    let a = render(&mut e, head, &BLOCKS);
    e.set_hifi(true);
    let mid = render(&mut e, head, &BLOCKS);
    e.set_hifi(false);
    let b = render(&mut e, tail, &BLOCKS);

    assert_eq!(a[..], plain[..head * 2]);
    assert_ne!(
        mid[..],
        plain[head * 2..head * 4],
        "hi-fi on changed nothing at all"
    );
    assert_eq!(b[..], plain[head * 4..]);
}

#[test]
fn hifi_setting_survives_a_rewind() {
    let mut e = engine("karma.ahx");
    e.set_hifi(true);
    assert!(e.init_subsong(0));
    assert!(e.hifi_enabled());
}

// ---------------------------------------------------------------------------
// 2. On removes fold-back.
// ---------------------------------------------------------------------------

/// A one-note song: instrument 1 sustaining at full volume on `waveform`
/// (`waveform` as `PListEntry` encodes it, 1 = triangle .. 4 = noise) with
/// tables of `4 << wave_length` bytes, `note` held for the whole run on
/// voice 0. Everything the instrument could modulate (vibrato, filter, PWM
/// sweep, envelope) is switched off, so the pitch is exactly constant.
/// `square_pos` is the raw `9xx` parameter (PWM duty; ignored off-square).
fn probe_song(waveform: u8, wave_length: u8, note: u8, square_pos: u8) -> Song {
    let mut song = format::parse(&fixture("karma.ahx")).unwrap();
    song.speed_multiplier = 1;
    song.position_nr = 1;
    song.restart = 0;
    song.positions = vec![Position {
        track: vec![1, 0, 0, 0],
        transpose: vec![0; 4],
    }];
    let mut steps = vec![Step::default(); song.track_length as usize];
    // Effect 9 sets the square position (PWM duty) directly; without it a
    // square starts at its narrowest, which is a constant at short lengths.
    steps[0] = Step {
        note,
        instrument: 1,
        fx: 0x9,
        fx_param: square_pos,
        ..Step::default()
    };
    song.tracks[1] = steps;
    song.tracks[0] = vec![Step::default(); song.track_length as usize];
    song.instruments[1] = Instrument {
        name: "probe".into(),
        volume: 64,
        wave_length,
        envelope: Envelope {
            a_frames: 1,
            a_volume: 64,
            d_frames: 1,
            d_volume: 64,
            s_frames: 255,
            r_frames: 1,
            r_volume: 0,
        },
        plist: PList {
            speed: 1,
            entries: vec![PListEntry {
                waveform,
                ..PListEntry::default()
            }],
        },
        ..Instrument::default()
    };
    song
}

/// Left channel of a stretch of `frames` frames, after `skip` frames.
fn left(out: &[i16], skip: usize, frames: usize) -> Vec<f64> {
    (skip..skip + frames).map(|i| out[2 * i] as f64).collect()
}

/// Power spectrum of a Hann-windowed block, `len` a power of two.
fn power_spectrum(x: &[f64]) -> Vec<f64> {
    let n = x.len().next_power_of_two();
    let mut buf: Vec<Complex<f64>> = (0..n)
        .map(|i| {
            if i < x.len() {
                let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / x.len() as f64).cos();
                Complex::new(x[i] * w, 0.0)
            } else {
                Complex::new(0.0, 0.0)
            }
        })
        .collect();
    FftPlanner::new().plan_fft_forward(n).process(&mut buf);
    buf[..n / 2].iter().map(|c| c.norm_sqr()).collect()
}

/// Energy that is not within `guard` Hz of a true harmonic of `f0`, as a
/// fraction of the total (DC excluded).
fn inharmonic_fraction(x: &[f64], f0: f64, guard: f64) -> f64 {
    let p = power_spectrum(x);
    let n = x.len().next_power_of_two();
    let bin_hz = RATE as f64 / n as f64;
    let (mut bad, mut total) = (0.0, 0.0);
    for (k, &e) in p.iter().enumerate().skip(2) {
        let hz = k as f64 * bin_hz;
        let nearest = (hz / f0).round().max(1.0) * f0;
        total += e;
        if (hz - nearest).abs() > guard {
            bad += e;
        }
    }
    bad / total
}

/// Fundamental of voice `v` right now, Hz (bytes of the 0x280 buffer per
/// output sample, over the `4 << wave_length` bytes in a cycle).
fn voice_f0(e: &AhxEngine, v: usize) -> f64 {
    let voice = e.voice(v);
    let n = 4usize << voice.wave_length;
    voice.delta as f64 / 65536.0 / n as f64 * RATE as f64
}

#[test]
fn a_high_probe_note_folds_back_in_the_reference_and_not_in_hifi() {
    // Note 60 is the top of the period table; on 8- and 16-byte tables that
    // is a fundamental of ~3.6 kHz and ~1.8 kHz with partials running far
    // past Nyquist.
    for (waveform, wave_length, label) in [
        (WAVEFORM_SQUARE as u8 + 1, 1u8, "square wl1"),
        (WAVEFORM_SAWTOOTH as u8 + 1, 1, "sawtooth wl1"),
        (WAVEFORM_SQUARE as u8 + 1, 2, "square wl2"),
        (WAVEFORM_SAWTOOTH as u8 + 1, 0, "sawtooth wl0"),
    ] {
        let song = probe_song(waveform, wave_length, 60, 0x10);
        let mut reference = engine_of(song.clone());
        let mut hifi = engine_of(song);
        hifi.set_hifi(true);
        let frames = RATE as usize;
        let r = render(&mut reference, frames, &BLOCKS);
        let h = render(&mut hifi, frames, &BLOCKS);

        let f0 = voice_f0(&reference, 0);
        assert!(f0 > 1000.0 && f0 < 8000.0, "{label}: f0 {f0}");
        let (rx, hx) = (left(&r, 8820, 16384), left(&h, 8820, 16384));
        let guard = 3.0 * RATE as f64 / 16384.0; // a few bins around each harmonic (Hann main lobe)
        let (rf, hf) = (
            inharmonic_fraction(&rx, f0, guard),
            inharmonic_fraction(&hx, f0, guard),
        );
        eprintln!(
            "{label}: f0 {f0:.1} Hz, inharmonic energy: reference {:.2} dB, hifi {:.2} dB",
            10.0 * rf.log10(),
            10.0 * hf.log10()
        );
        assert!(
            rf > 1e-4,
            "{label}: the probe does not alias in the reference ({rf:e}); it proves nothing"
        );
        assert!(
            hf < rf * 0.01,
            "{label}: hifi inharmonic {hf:e} not >20 dB under the reference's {rf:e}"
        );
    }
}

#[test]
fn hifi_keeps_the_probe_at_the_same_pitch_and_level() {
    let song = probe_song(WAVEFORM_SQUARE as u8 + 1, 3, 30, 0x10);
    let mut reference = engine_of(song.clone());
    let mut hifi = engine_of(song);
    hifi.set_hifi(true);
    let frames = RATE as usize;
    let r = left(&render(&mut reference, frames, &BLOCKS), 8820, 16384);
    let h = left(&render(&mut hifi, frames, &BLOCKS), 8820, 16384);
    let f0 = voice_f0(&reference, 0);

    let (pr, ph) = (power_spectrum(&r), power_spectrum(&h));
    let bin_hz = RATE as f64 / 16384.0;
    let peak = |p: &[f64]| {
        p.iter()
            .enumerate()
            .skip(2)
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(k, _)| k as f64 * bin_hz)
            .unwrap()
    };
    // Strongest partial is the same one, within a bin.
    assert!(
        (peak(&pr) - peak(&ph)).abs() <= 2.0 * bin_hz,
        "peak {} vs {}",
        peak(&pr),
        peak(&ph)
    );
    // Level: RMS within 1.5 dB.
    let rms = |x: &[f64]| (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt();
    let db = 20.0 * (rms(&h) / rms(&r)).log10();
    eprintln!("f0 {f0:.1} Hz: rms hifi vs reference {db:+.2} dB");
    assert!(db.abs() < 1.5, "level moved by {db} dB");
}

#[test]
fn a_low_note_is_left_essentially_alone() {
    // Nothing folds at low pitch, so the two renders should agree to within
    // the reference's own 8-bit rounding: a hi-fi that changes bass would be
    // changing the music, not the aliasing.
    let song = probe_song(WAVEFORM_SAWTOOTH as u8 + 1, 5, 20, 0);
    let mut reference = engine_of(song.clone());
    let mut hifi = engine_of(song);
    hifi.set_hifi(true);
    let r = left(&render(&mut reference, RATE as usize, &BLOCKS), 4410, 16384);
    let h = left(&render(&mut hifi, RATE as usize, &BLOCKS), 4410, 16384);
    let rms = |x: &[f64]| (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt();
    let diff: Vec<f64> = r.iter().zip(&h).map(|(a, b)| a - b).collect();
    let rel = rms(&diff) / rms(&r);
    eprintln!("low note: rms(hifi - reference) / rms(reference) = {rel:.4}");
    assert!(rel < 0.10, "a low note moved by {rel}");
}

/// Robocop iii, measured per voice on what it actually plays.
///
/// The song is rendered tick by tick in lockstep as reference and hi-fi,
/// each with one voice soloed. Every run of ticks where that voice holds one
/// steady high pitch (no slide, not noise) is a window; a steady note has no
/// energy between DC and its fundamental, so anything in `[150 Hz, 0.7 f0]`
/// is fold-back (or envelope splatter, which both renders share). The test
/// asserts hi-fi cuts that energy across the song's high passages.
#[test]
fn robocop_iii_high_passages_fold_back_less_in_hifi() {
    const MIN_F0: f64 = 1500.0;
    const RUN_TICKS: usize = 3;
    let tick = RATE as usize / 50;
    let ticks = 50 * 40; // 40 s
    let mut low = [0.0f64; 2]; // [reference, hifi]
    let mut total = [0.0f64; 2];
    let mut windows = 0;

    for voice in 0..4 {
        let song = format::parse(&fixture("robocop_iii_j_tel.ahx")).unwrap();
        let mut e = [engine_of(song.clone()), engine_of(song)];
        e[1].set_hifi(true);
        for x in e.iter_mut() {
            x.set_mute_solo(0, 1 << voice);
        }
        let mut chunk = [vec![0i16; tick * 2], vec![0i16; tick * 2]];
        let mut history: Vec<(f64, [Vec<f64>; 2])> = Vec::new();

        for t in 0..ticks {
            for k in 0..2 {
                e[k].render_block(&mut chunk[k]);
            }
            let v = e[0].voice(voice);
            let steady = v.track_on && v.voice_volume > 8 && v.waveform != WAVEFORM_NOISE;
            let f0 = if steady { voice_f0(&e[0], voice) } else { 0.0 };
            let this = (f0, [left(&chunk[0], 0, tick), left(&chunk[1], 0, tick)]);
            let breaks = f0 < MIN_F0
                || history
                    .last()
                    .is_some_and(|h| (h.0 - f0).abs() > f0 * 0.005);
            if breaks {
                flush(&mut history, &mut low, &mut total, &mut windows);
            }
            if f0 >= MIN_F0 {
                history.push(this);
            }
            let _ = t;
        }
        flush(&mut history, &mut low, &mut total, &mut windows);
    }

    let (r, h) = (low[0] / total[0], low[1] / total[1]);
    eprintln!(
        "robocop iii: {windows} steady high windows; energy in [150 Hz, 0.7 f0]: reference {:.2} dB, hifi {:.2} dB of the window energy ({:.1} dB lower)",
        10.0 * r.log10(),
        10.0 * h.log10(),
        10.0 * (r / h).log10()
    );

    fn flush(
        history: &mut Vec<(f64, [Vec<f64>; 2])>,
        low: &mut [f64; 2],
        total: &mut [f64; 2],
        windows: &mut usize,
    ) {
        if history.len() >= RUN_TICKS {
            let f0 = history.iter().map(|h| h.0).sum::<f64>() / history.len() as f64;
            for k in 0..2 {
                let x: Vec<f64> = history
                    .iter()
                    .flat_map(|h| h.1[k].iter().copied())
                    .collect();
                let p = power_spectrum(&x);
                let bin_hz = RATE as f64 / x.len().next_power_of_two() as f64;
                for (i, &e) in p.iter().enumerate().skip(1) {
                    let hz = i as f64 * bin_hz;
                    total[k] += e;
                    if (150.0..0.7 * f0).contains(&hz) {
                        low[k] += e;
                    }
                }
            }
            *windows += 1;
        }
        history.clear();
    }

    assert!(
        windows >= 8,
        "only {windows} steady high windows found; the measurement is too thin to mean anything"
    );
    assert!(
        h < r * 0.5,
        "hifi low-band energy {h:e} is not at least 3 dB under the reference's {r:e}"
    );
}

// ---------------------------------------------------------------------------
// 4. Prewarm: the render path never builds a table.
// ---------------------------------------------------------------------------

use std::time::Instant;

/// Every fixture name in the demo directory, sorted.
fn all_fixtures() -> Vec<String> {
    let mut names: Vec<String> =
        fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/songs"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".ahx") || n.ends_with(".hvl"))
            .collect();
    names.sort();
    names
}

/// A 16-voice HVL: the 11-channel doobrey_gubbins with five more columns cut
/// from its first five (the engine's maximum, and every voice busy).
fn sixteen_voice_song() -> Song {
    let mut s = format::parse(&fixture("doobrey_gubbins.hvl")).unwrap();
    let native = s.channels;
    assert_eq!(native, 11);
    for p in s.positions.iter_mut() {
        for c in native..16 {
            p.track.push(p.track[c - native]);
            p.transpose.push(p.transpose[c - native]);
        }
    }
    s.channels = 16;
    s
}

#[test]
fn prewarming_mid_song_changes_nothing_but_the_cache() {
    // Hi-fi output is a function of song state alone, so an engine prewarmed
    // half way through renders the same bytes as one that built its tables
    // as it went -- unless the prewarm disturbed the voices, the transport,
    // the tick phase or the capture rings it borrows.
    for name in [
        "robocop_iii_j_tel.ahx",
        "sunspots.hvl",
        "get_to_the_chopper.ahx",
    ] {
        let frames = RATE as usize * 6;
        let mut lazy = engine(name);
        lazy.set_hifi(true);
        lazy.enable_capture(true);
        let want = render(&mut lazy, frames, &BLOCKS);
        let mut want_scope = vec![0i16; 2048];
        assert_eq!(lazy.read_channel_snapshot(0, &mut want_scope), 2048);

        let mut warm = engine(name);
        warm.set_hifi(true);
        warm.enable_capture(true);
        let head = 3 * RATE as usize + 77; // mid-tick on purpose
        let mut got = render(&mut warm, head, &BLOCKS);
        let stats = warm.prewarm_hifi();
        assert!(
            warm.hifi_locked() && stats.tables > 0 && !stats.cache_full,
            "{name}: {stats:?}"
        );
        got.extend(render(&mut warm, frames - head, &BLOCKS));
        assert_eq!(got, want, "{name}: prewarm mid-song disturbed the render");
        let mut got_scope = vec![0i16; 2048];
        warm.read_channel_snapshot(0, &mut got_scope);
        assert_eq!(got_scope, want_scope, "{name}: capture rings");
        assert_eq!(warm.hifi_misses(), 0, "{name}: misses after a prewarm");
    }
}

#[test]
fn prewarm_with_hifi_off_is_a_no_op() {
    let mut e = engine("karma.ahx");
    let stats = e.prewarm_hifi();
    assert_eq!((stats.ticks, stats.tables), (0, 0));
    assert!(!e.hifi_enabled() && !e.hifi_locked());
}

/// The claim the lock rests on, over the whole corpus: after a prewarm, every
/// fixture plays through -- the first lap, the wraps, and every subsong
/// restarted from its top -- without one lookup the exact table could not
/// serve, and without the cache growing by a table.
///
/// "Converged" is the prewarm's own word for having stopped finding new
/// tables; the test holds it to that over many more laps than the prewarm
/// walked. A song that did not converge (its sweeps outlast the tick cap) is
/// not asserted to have zero misses -- it is asserted to *say* so, and its
/// miss rate is printed.
#[test]
fn a_prewarmed_song_never_misses_across_laps_and_subsongs() {
    const BLOCK: usize = 512;
    // The full horizon (12 laps or 12 minutes per subsong) is what
    // `cargo test --release` runs; an unoptimised build gets a short one
    // (3 laps or 90 s), which still walks every fixture past its first wrap.
    let (max_laps, max_frames) = if cfg!(debug_assertions) {
        (3, RATE as usize * 90)
    } else {
        (12, RATE as usize * 60 * 12)
    };
    let mut out = vec![0i16; BLOCK * 2];
    let mut report = String::new();
    for name in all_fixtures() {
        let mut e = engine(&name);
        e.set_hifi(true);
        let stats = e.prewarm_hifi();
        assert!(!stats.cache_full, "{name}: cache filled");
        let tables = e.hifi_table_count();
        let (mut laps_total, mut frames_total) = (0u32, 0usize);
        for sub in 0..=e.song().subsong_nr as usize {
            assert!(e.init_subsong(sub));
            let (mut frames, mut laps, mut last_pos) = (0usize, 0u32, 0i32);
            while frames < max_frames && laps < max_laps {
                e.render_block(&mut out);
                frames += BLOCK;
                if e.pos_nr() < last_pos {
                    laps += 1;
                }
                last_pos = e.pos_nr();
            }
            frames_total += frames;
            laps_total += laps;
        }
        let misses = e.hifi_misses();
        assert_eq!(
            e.hifi_table_count(),
            tables,
            "{name}: tables built while rendering"
        );
        if stats.converged {
            assert_eq!(
                misses, 0,
                "{name}: prewarm converged, yet the audio thread missed"
            );
        }
        report.push_str(&format!(
            "{name:28} prewarm: {:2} laps {:6} ticks {:5} tables {:5} sources converged={:5} | rendered {:5.0}s over {:2} laps: misses {misses}\n",
            stats.laps,
            stats.ticks,
            stats.tables,
            stats.sources,
            stats.converged,
            frames_total as f64 / RATE as f64,
            laps_total,
        ));
    }
    eprintln!("{report}");
}

/// The timing the reviewer asked for: a 16-voice HVL rendered from a cold
/// start in the audio thread's own 128-frame quanta, hi-fi building lazily
/// against hi-fi prewarmed. Numbers go to stderr (`--nocapture`); only the
/// deterministic facts are asserted, because wall-clock does not belong in a
/// pass/fail. Run with `--release` for numbers that mean anything (the wasm
/// build is optimised, this test binary by default is not).
#[test]
fn cold_start_render_of_a_sixteen_voice_hvl_with_and_without_prewarm() {
    const QUANTUM: usize = 128;
    const SECONDS: usize = 10;
    let budget_us = QUANTUM as f64 / RATE as f64 * 1e6;

    let run = |prewarm: bool| {
        let mut e = engine_of(sixteen_voice_song());
        assert_eq!(e.channels(), 16);
        e.set_hifi(true);
        let t0 = Instant::now();
        let stats = if prewarm {
            Some(e.prewarm_hifi())
        } else {
            None
        };
        let prewarm_ms = t0.elapsed().as_secs_f64() * 1e3;
        let tables_before = e.hifi_table_count();
        let mut out = vec![0i16; QUANTUM * 2];
        let mut all = Vec::with_capacity(RATE as usize * SECONDS * 2);
        let mut times: Vec<f64> = Vec::new();
        for _ in 0..RATE as usize * SECONDS / QUANTUM {
            let t = Instant::now();
            e.render_block(&mut out);
            times.push(t.elapsed().as_secs_f64() * 1e6);
            all.extend_from_slice(&out);
        }
        (e, stats, prewarm_ms, tables_before, times, all)
    };
    let (cold, _, _, cold_before, mut cold_t, cold_out) = run(false);
    let (warm, stats, prewarm_ms, warm_before, mut warm_t, warm_out) = run(true);
    let stats = stats.unwrap();

    let summarise = |t: &mut Vec<f64>| {
        let over = t.iter().filter(|&&x| x > budget_us).count();
        let sum: f64 = t.iter().sum();
        t.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| t[((t.len() - 1) as f64 * p) as usize];
        (
            sum / t.len() as f64,
            pct(0.5),
            pct(0.99),
            pct(0.999),
            *t.last().unwrap(),
            over,
        )
    };
    let c = summarise(&mut cold_t);
    let w = summarise(&mut warm_t);
    eprintln!("16-voice HVL, {SECONDS} s in {QUANTUM}-frame quanta at {RATE} Hz (budget {budget_us:.0} us)");
    eprintln!("                  mean us   p50     p99     p99.9   worst   quanta over budget");
    eprintln!(
        "  cold (lazy)     {:7.1} {:7.1} {:7.1} {:7.1} {:7.1}   {}",
        c.0, c.1, c.2, c.3, c.4, c.5
    );
    eprintln!(
        "  prewarmed       {:7.1} {:7.1} {:7.1} {:7.1} {:7.1}   {}",
        w.0, w.1, w.2, w.3, w.4, w.5
    );
    eprintln!(
        "  tables built while rendering: cold {}, prewarmed {}",
        cold.hifi_table_count() - cold_before,
        warm.hifi_table_count() - warm_before
    );
    eprintln!(
        "  prewarm: {prewarm_ms:.1} ms, {} ticks simulated, {} sources, {} tables ({:.1} MiB of tables), misses after: {}",
        stats.ticks,
        stats.sources,
        stats.tables,
        warm.hifi_table_bytes() as f64 / 1048576.0,
        warm.hifi_misses()
    );

    assert!(
        cold.hifi_table_count() > cold_before,
        "the cold run built nothing, so it measured nothing"
    );
    assert_eq!(
        warm.hifi_table_count(),
        warm_before,
        "the prewarmed run built tables while rendering"
    );
    assert_eq!(warm.hifi_misses(), 0);
    assert!(warm.hifi_locked());
    // Nothing was degraded, so it is the same audio, sample for sample.
    assert_eq!(
        warm_out, cold_out,
        "prewarmed hi-fi differs from lazily built hi-fi"
    );
}
