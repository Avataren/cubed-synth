//! AHX/HVL render throughput: plays fixture songs through `AhxPlayer::render`
//! in 128-frame blocks (the worklet's quantum), with the reference oscillators
//! and with hifi, and reports the realtime factor and the cost of one block
//! against its deadline. The mixer is the crate's SIMD path, so this is also
//! the check that the stable `cubed-simd` fallback keeps up with `std::simd`
//! (`--features nightly-simd` on a nightly toolchain).
//!
//! cargo bench -p cubed-ahx --bench ahx_render

use std::hint::black_box;
use std::time::Instant;

use cubed_ahx::player::AhxPlayer;

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
const SECONDS: f64 = 60.0;
const RUNS: usize = 3;
const SONGS: [&str; 4] = [
    "tests/fixtures/songs/a_new_beginning.ahx",
    "tests/fixtures/songs/blue_mazda_323.ahx",
    "tests/fixtures/songs/a_little_cyberfunk.hvl",
    "tests/fixtures/songs/doobrey_gubbins.hvl",
];

/// Seconds to render `SECONDS` of the song, the best of `RUNS`.
fn time_song(bytes: &[u8], hifi: bool) -> f64 {
    let blocks = (RATE as f64 * SECONDS) as usize / BLOCK;
    let mut best = f64::INFINITY;
    for _ in 0..RUNS {
        let mut player = AhxPlayer::new(bytes, RATE, 2).expect("fixture parses");
        // Prewarming the hifi tables happens here, outside the timed loop.
        player.set_hifi(hifi);
        player.play();
        let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
        let start = Instant::now();
        for _ in 0..blocks {
            player.render(&mut left, &mut right);
            black_box((&left, &right));
        }
        best = best.min(start.elapsed().as_secs_f64());
    }
    best
}

fn main() {
    let deadline_us = BLOCK as f64 / RATE as f64 * 1e6;
    let blocks = (RATE as f64 * SECONDS) as usize / BLOCK;
    for path in SONGS {
        let bytes = std::fs::read(path).expect("reads the fixture");
        for hifi in [false, true] {
            let secs = time_song(&bytes, hifi);
            let block_us = secs / blocks as f64 * 1e6;
            println!(
                "{:<26} {:<5}: {:>6.1}x realtime, {:>6.2} us/block ({:.2}% of {:.0} us)",
                path.rsplit('/').next().unwrap(),
                if hifi { "hifi" } else { "ref" },
                SECONDS / secs,
                block_us,
                block_us / deadline_us * 100.0,
                deadline_us,
            );
        }
    }
}
