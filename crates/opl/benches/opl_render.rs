//! OPL render throughput: plays A2M fixture songs through `A2Player::render`
//! in 128-sample blocks (the worklet's quantum) and reports the realtime
//! factor, the cost of one block against its deadline, and a hash of the
//! output so an optimisation can show it changed nothing.
//!
//! cargo bench -p cubed-opl --bench opl_render

use std::hash::{DefaultHasher, Hasher};
use std::hint::black_box;
use std::time::Instant;

use cubed_opl::a2::player::A2Player;

const RATE: f64 = 48_000.0;
const BLOCK: usize = 128;
const SECONDS: f64 = 60.0;
const RUNS: usize = 5;
const SONGS: [&str; 3] = [
    "tests/fixtures/a2m/NAB622/corridors of time.a2m",
    "tests/fixtures/a2m/Kvee/mega man 3 title.a2m",
    "tests/fixtures/a2m/Kvee/ym3812 funk.a2m",
];

/// Seconds to render `SECONDS` of the song (the best of `RUNS`), and the
/// hash of the output.
fn time_song(bytes: &[u8]) -> (f64, u64) {
    let blocks = (RATE * SECONDS) as usize / BLOCK;
    let mut best = f64::INFINITY;
    let mut hash = 0;
    for _ in 0..RUNS {
        let mut player = A2Player::new(bytes, RATE).expect("player builds");
        player.play();
        let (mut l, mut r) = ([0f32; BLOCK], [0f32; BLOCK]);
        let mut h = DefaultHasher::new();
        let start = Instant::now();
        for _ in 0..blocks {
            player.render(&mut l, &mut r);
            black_box((&l, &r));
            for (a, b) in l.iter().zip(&r) {
                h.write_u32(a.to_bits());
                h.write_u32(b.to_bits());
            }
        }
        best = best.min(start.elapsed().as_secs_f64());
        hash = h.finish();
    }
    (best, hash)
}

fn main() {
    let deadline_us = BLOCK as f64 / RATE * 1e6;
    let blocks = (RATE * SECONDS) as usize / BLOCK;
    for path in SONGS {
        let bytes = std::fs::read(path).expect("reads the fixture");
        let (secs, hash) = time_song(&bytes);
        let block_us = secs / blocks as f64 * 1e6;
        println!(
            "{:<26} {:>6.1}x realtime, {:>6.2} us/block ({:.2}% of {:.0} us)  hash {hash:016x}",
            path.rsplit('/').next().unwrap(),
            SECONDS / secs,
            block_us,
            block_us / deadline_us * 100.0,
            deadline_us,
        );
    }
}
