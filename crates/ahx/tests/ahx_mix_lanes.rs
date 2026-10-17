//! The mixer's four-frame lanes against the scalar loop it replaces: same
//! engine state, same bytes out, on every demo, hi-fi on and off, whole and
//! ragged block sizes, muted voices. The lanes are integer arithmetic that
//! reassociates only wrapping adds, so equality is the bar -- any difference is
//! a bug, never a tolerance. (The 39 reference goldens and the hi-fi baseline
//! run the lanes too, since they are the default; this test is the wider net
//! and the only one that puts the two paths side by side.)
//!
//! `mix_lanes_timing` is the before/after: `cargo test --release --test
//! ahx_mix_lanes -- --ignored --nocapture`. Std `Instant`, best of several
//! runs, no benchmark crate.

use cubed_ahx::engine::AhxEngine;
use cubed_ahx::format;
use std::fs;
use std::path::Path;
use std::time::Instant;

const RATE: u32 = 44100;

fn demo(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/songs")
            .join(name),
    )
    .unwrap()
}

fn engine(name: &str, hifi: bool, lanes: bool) -> AhxEngine {
    let song = format::parse(&demo(name)).unwrap();
    let mut e = AhxEngine::new(song, RATE, 2).unwrap();
    e.set_continue_phase_on_trigger(true);
    e.set_mix_lanes(lanes);
    if hifi {
        e.set_hifi(true);
        e.prewarm_hifi();
    }
    e
}

fn render(e: &mut AhxEngine, frames: usize, chunk: usize) -> Vec<i16> {
    let mut out = vec![0i16; frames * 2];
    let mut done = 0;
    while done < frames {
        let n = chunk.min(frames - done);
        e.render_block(&mut out[done * 2..(done + n) * 2]);
        done += n;
    }
    out
}

fn assert_same(name: &str, hifi: bool, frames: usize, chunk: usize, mute: u32, solo: u32) {
    let mut scalar = engine(name, hifi, false);
    let mut lanes = engine(name, hifi, true);
    scalar.set_mute_solo(mute, solo);
    lanes.set_mute_solo(mute, solo);
    let want = render(&mut scalar, frames, chunk);
    let got = render(&mut lanes, frames, chunk);
    if let Some(at) = want.iter().zip(&got).position(|(a, b)| a != b) {
        panic!("{name} hifi={hifi} chunk={chunk} mute={mute:#x} solo={solo:#x}: first difference at sample {at}: scalar {} vs lanes {}", want[at], got[at]);
    }
}

fn all_demos() -> Vec<String> {
    let mut v: Vec<String> =
        fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/songs"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".ahx") || n.ends_with(".hvl"))
            .collect();
    v.sort();
    v
}

#[test]
fn lanes_match_scalar_on_every_demo_hifi_off() {
    for name in all_demos() {
        assert_same(&name, false, RATE as usize * 4, 882, 0, 0);
    }
}

#[test]
fn lanes_match_scalar_on_every_demo_hifi_on() {
    for name in all_demos() {
        assert_same(&name, true, RATE as usize * 3, 882, 0, 0);
    }
}

#[test]
fn lanes_match_scalar_at_ragged_block_sizes() {
    // Sizes that leave 0-3 leftover frames per run, and blocks shorter than a lane.
    for &chunk in &[1usize, 2, 3, 5, 7, 50, 127, 128, 883] {
        for &hifi in &[false, true] {
            assert_same("karma.ahx", hifi, 30_000, chunk, 0, 0);
            assert_same("sunspots.hvl", hifi, 30_000, chunk, 0, 0);
        }
    }
}

#[test]
fn lanes_match_scalar_with_voices_muted_and_soloed() {
    for &(mute, solo) in &[
        (0b0001u32, 0u32),
        (0b0110, 0),
        (0, 0b0100),
        (0b1111, 0),
        (0b1_0101_0101, 0),
    ] {
        for &hifi in &[false, true] {
            assert_same(
                "robocop_iii_j_tel.ahx",
                hifi,
                RATE as usize * 2,
                441,
                mute,
                solo,
            );
            assert_same(
                "doobrey_gubbins.hvl",
                hifi,
                RATE as usize * 2,
                441,
                mute,
                solo,
            );
        }
    }
}

#[test]
#[ignore = "timing; run in --release with --nocapture"]
fn mix_lanes_timing() {
    const SECONDS: usize = 30;
    for (name, hifi) in [
        ("karma.ahx", false),
        ("karma.ahx", true),
        ("wave_stepper.ahx", true),
        ("sunspots.hvl", true),
        ("doobrey_gubbins.hvl", true),
    ] {
        let mut best = [f64::MAX; 2];
        for (slot, lanes) in [(0usize, false), (1, true)] {
            for _ in 0..7 {
                let mut e = engine(name, hifi, lanes);
                let t = Instant::now();
                let out = render(&mut e, RATE as usize * SECONDS, 128);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                std::hint::black_box(&out);
                best[slot] = best[slot].min(ms);
            }
        }
        println!("TIMING mix {name} hifi={hifi}: scalar {:.2} ms, lanes {:.2} ms per {SECONDS} s of audio ({:.2}x)", best[0], best[1], best[0] / best[1]);
    }
}
