//! `AhxPlayer` (the wasm-facing shell, `src/ahx/player.rs`) against the same
//! goldens the engine is proven with: the planar-`f32` stream it produces,
//! converted back to `i16`, must hash to the C reference's per-chunk values.
//! `i16 -> f32` (`x / 32768`) is exact in `f32`, so the round trip is lossless
//! and any adapter bug (channel swap, off-by-one, scale) shows as a hash miss.

use cubed_ahx::player::AhxPlayer;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(root().join("tests/fixtures/songs").join(name)).unwrap()
}

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// `(frames_upto, hash)` chunk lines of a golden.
fn chunks(name: &str) -> Vec<(usize, u64)> {
    let text = fs::read_to_string(root().join("tests/golden").join(name)).unwrap();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            match f.as_slice() {
                [n, h] if n.chars().all(|c| c.is_ascii_digit()) => {
                    Some((n.parse().unwrap(), u64::from_str_radix(h, 16).unwrap()))
                }
                _ => None,
            }
        })
        .collect()
}

/// Renders in worklet-sized 128-frame quanta, the way the browser will.
fn render_quanta(p: &mut AhxPlayer, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut l_all, mut r_all) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    let (mut l, mut r) = ([0f32; 128], [0f32; 128]);
    let mut done = 0;
    while done < frames {
        let n = 128.min(frames - done);
        assert_eq!(p.render(&mut l[..n], &mut r[..n]), n);
        l_all.extend_from_slice(&l[..n]);
        r_all.extend_from_slice(&r[..n]);
        done += n;
    }
    (l_all, r_all)
}

#[test]
fn karma_planar_f32_round_trips_to_the_c_reference_hashes() {
    let mut p = AhxPlayer::new(&fixture("karma.ahx"), 44100, 2).unwrap();
    p.play();
    let mut prev = 0;
    for (upto, want) in chunks("karma.44100.s2.cap0.txt").into_iter().take(20) {
        // 44100 Hz, speed multiplier 1: 882 samples per DecodeFrame.
        let frames = (upto - prev) * 882;
        let (l, r) = render_quanta(&mut p, frames);
        let mut bytes = Vec::with_capacity(frames * 4);
        for i in 0..frames {
            for s in [l[i], r[i]] {
                let v = s * 32768.0;
                assert_eq!(v, v.trunc(), "f32 sample must be an exact i16");
                bytes.extend_from_slice(&(v as i16).to_le_bytes());
            }
        }
        assert_eq!(
            fnv(0xcbf29ce484222325, &bytes),
            want,
            "chunk {prev}..{upto}"
        );
        prev = upto;
    }
}

#[test]
fn paused_player_is_silent_and_does_not_advance() {
    let mut p = AhxPlayer::new(&fixture("karma.ahx"), 44100, 2).unwrap();
    let (l, r) = render_quanta(&mut p, 4410);
    assert!(l.iter().chain(r.iter()).all(|&s| s == 0.0));
    assert_eq!((p.ticks(), p.position(), p.row()), (0, 0, 0));
    p.play();
    let (l, _) = render_quanta(&mut p, 44100);
    assert!(l.iter().any(|&s| s != 0.0), "audible once playing");
    assert!(p.ticks() > 0);
    let ticks = p.ticks();
    p.pause();
    render_quanta(&mut p, 4410);
    assert_eq!(p.ticks(), ticks);
}

#[test]
fn gain_scales_and_restart_rewinds() {
    let mut a = AhxPlayer::new(&fixture("karma.ahx"), 44100, 2).unwrap();
    let mut b = AhxPlayer::new(&fixture("karma.ahx"), 44100, 2).unwrap();
    b.set_gain(0.5);
    b.set_gain(f32::NAN); // ignored
    a.play();
    b.play();
    let (la, _) = render_quanta(&mut a, 8820);
    let (lb, _) = render_quanta(&mut b, 8820);
    assert!(la.iter().zip(&lb).all(|(x, y)| *y == *x * 0.5));

    assert!(a.restart(0));
    assert!(!a.is_playing());
    assert_eq!(a.ticks(), 0);
    assert!(!a.restart(99));
}

#[test]
fn metadata_and_native_channel_count_are_exposed() {
    let p = AhxPlayer::new(&fixture("karma.ahx"), 44100, 2).unwrap();
    assert_eq!(p.channels(), 4);
    assert_eq!(p.dropped_channels(), 0);
    assert_eq!(p.sample_rate(), 44100);
    assert!(p.position_count() > 0 && p.track_length() > 0);
    let h = AhxPlayer::new(&fixture("drainage_proble.hvl"), 48000, 2).unwrap();
    assert_eq!((h.channels(), h.dropped_channels()), (7, 0));
}

#[test]
fn garbage_input_is_an_error_not_a_panic() {
    assert!(AhxPlayer::new(b"not an ahx file at all, really", 44100, 2).is_err());
    assert!(AhxPlayer::new(&[], 44100, 2).is_err());
    assert!(
        AhxPlayer::new(&fixture("karma.ahx"), 10, 2).is_err(),
        "sample rate too low"
    );
}

/// The product path: `set_hifi(true)` on the wasm-facing player prewarms, so
/// `render` (the audio thread) never builds a table, and turning it off again
/// is the reference.
#[test]
fn set_hifi_on_prewarms_so_render_builds_nothing() {
    let mut p = AhxPlayer::new(&fixture("robocop_iii_j_tel.ahx"), 44100, 2).unwrap();
    assert_eq!(
        (p.hifi_enabled(), p.hifi_locked(), p.hifi_table_count()),
        (false, false, 0)
    );

    p.set_hifi(true);
    assert!(p.hifi_enabled() && p.hifi_locked());
    let tables = p.hifi_table_count();
    assert!(tables > 0, "nothing prewarmed");

    p.play();
    let (l, _) = render_quanta(&mut p, 44100 * 20);
    assert!(l.iter().any(|&x| x != 0.0));
    assert_eq!(p.hifi_table_count(), tables, "render built tables");
    assert_eq!(p.hifi_miss_count(), 0.0);

    // Already on: not prewarmed (or rebuilt) a second time.
    p.set_hifi(true);
    assert_eq!(p.hifi_table_count(), tables);

    p.set_hifi(false);
    assert_eq!(
        (p.hifi_enabled(), p.hifi_locked(), p.hifi_table_count()),
        (false, false, 0)
    );
}
