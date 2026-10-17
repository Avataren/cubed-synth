//! SID render throughput: plays each fixture song on both chip models through
//! `render_taps` in 128-sample blocks (the worklet's quantum) and reports the
//! realtime factor and the cost of one block against its deadline.
//!
//! cargo bench -p cubed-sid --bench sid_render

use std::hint::black_box;
use std::time::Instant;

use cubed_sid::{SidModel, SidSong, SidSongPlayer, DEFAULT_SAMPLE_RATE};

const BLOCK: usize = 128;
const SECONDS: f64 = 60.0;
const RUNS: usize = 5;
const SONGS: [&str; 2] = [
    "tests/fixtures/sid/s3-chain.asid",
    "tests/fixtures/sid/s59-drum-example.asid",
];

/// Seconds to render `SECONDS` of the song, the best of `RUNS`.
fn time_song(bytes: &[u8], model: SidModel) -> f64 {
    let blocks = (DEFAULT_SAMPLE_RATE * SECONDS) as usize / BLOCK;
    let mut best = f64::INFINITY;
    for _ in 0..RUNS {
        let song = SidSong::parse(bytes).expect("fixture parses");
        let mut player =
            SidSongPlayer::with_model(song, model, DEFAULT_SAMPLE_RATE, 0).expect("player builds");
        let mut out = [0f32; BLOCK];
        let (mut a, mut b, mut c) = ([0f32; BLOCK], [0f32; BLOCK], [0f32; BLOCK]);
        let start = Instant::now();
        for _ in 0..blocks {
            player.render_taps(&mut out, [&mut a, &mut b, &mut c]);
            black_box((&out, &a, &b, &c));
        }
        best = best.min(start.elapsed().as_secs_f64());
    }
    best
}

fn main() {
    let deadline_us = BLOCK as f64 / DEFAULT_SAMPLE_RATE * 1e6;
    let blocks = (DEFAULT_SAMPLE_RATE * SECONDS) as usize / BLOCK;
    for path in SONGS {
        let bytes = std::fs::read(path).expect("reads the fixture");
        for model in [SidModel::Sid8580, SidModel::Sid6581] {
            let secs = time_song(&bytes, model);
            let block_us = secs / blocks as f64 * 1e6;
            println!(
                "{:<24} {:?}: {:>5.1}x realtime, {:>6.2} us/block ({:.2}% of {:.0} us)",
                path.rsplit('/').next().unwrap(),
                model,
                SECONDS / secs,
                block_us,
                block_us / deadline_us * 100.0,
                deadline_us,
            );
        }
    }
}
