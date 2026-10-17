//! A hi-fi render baseline: FNV-1a-64 of what the engine renders with hi-fi
//! **on** (the app's only mode), pinned from the build before the B2.1 speed-ups
//! (per-tick allocation removal in `Voice::set_audio`, prewarm changes).
//!
//! The reference goldens (`ahx_render_golden.rs`) hold the hi-fi *off* path; a
//! band-limited render is not what the C reference plays, so hi-fi never had a
//! golden, and a change to the tables or to what feeds them could shift the
//! sound of the shipped app without failing anything. This pins the current
//! output instead: a rewrite that claims to be bit-exact has to reproduce these
//! hashes. It is a regression pin, not a claim about correctness -- when hi-fi
//! is *meant* to change, regenerate the constants (run with `PRINT_HIFI_BASELINE=1
//! cargo test --test ahx_hifi_baseline -- --nocapture`) and say why in the commit.
//!
//! Each case is the app's own set-up: continue-phase on, hi-fi on, a full
//! prewarm, then 10 s rendered in 50-frame chunks. The hash also folds in the
//! prewarm's table and source counts, so a change in *which* tables get built
//! shows even where a render would not notice it. Nothing here touches
//! `tests/golden/`.

use cubed_ahx::engine::AhxEngine;
use cubed_ahx::format;
use std::fs;
use std::path::Path;

const RATE: u32 = 44100;
const FRAMES: usize = RATE as usize * 10;
const CHUNK: usize = 882;

/// `(fixture, expected hash)`: AHX and HVL songs (the HVL ones with more than four voices).
const BASELINE: &[(&str, u64)] = &[
    ("karma.ahx", 0x6353cfd9a5feb989),
    ("robocop_iii_j_tel.ahx", 0x8b1c0f39abb933a4),
    ("wave_stepper.ahx", 0x5a506591c96c07fe),
    ("sunspots.hvl", 0x821db970d41cae37),
    ("doobrey_gubbins.hvl", 0x60691afc3591b94f),
];

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn hifi_hash(fixture: &str) -> u64 {
    let bytes = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/songs")
            .join(fixture),
    )
    .unwrap();
    let song = format::parse(&bytes).unwrap();
    let mut e = AhxEngine::new(song, RATE, 2).unwrap();
    e.set_continue_phase_on_trigger(true);
    e.set_hifi(true);
    let pre = e.prewarm_hifi();
    let mut h = 0xcbf29ce484222325u64;
    h = fnv(h, &(pre.tables as u64).to_le_bytes());
    h = fnv(h, &(pre.sources as u64).to_le_bytes());
    h = fnv(h, &pre.ticks.to_le_bytes());
    let mut buf = vec![0i16; CHUNK * 2];
    let mut done = 0;
    while done < FRAMES {
        let n = CHUNK.min(FRAMES - done);
        e.render_block(&mut buf[..n * 2]);
        let raw: Vec<u8> = buf[..n * 2].iter().flat_map(|s| s.to_le_bytes()).collect();
        h = fnv(h, &raw);
        done += n;
    }
    assert_eq!(
        e.hifi_misses(),
        0,
        "{fixture}: a prewarmed song must not miss"
    );
    h
}

#[test]
fn hifi_render_matches_the_pinned_baseline() {
    let print = std::env::var("PRINT_HIFI_BASELINE").is_ok();
    for &(fixture, want) in BASELINE {
        let got = hifi_hash(fixture);
        if print {
            println!("    (\"{fixture}\", 0x{got:016x}),");
        } else {
            assert_eq!(
                got, want,
                "{fixture}: the hi-fi render moved (got 0x{got:016x})"
            );
        }
    }
}
