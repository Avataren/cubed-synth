//! Module render cost: plays modules through `Player::render` in 128-frame
//! blocks (the worklet's quantum) and reports the realtime factor and the
//! cost of a block against its deadline. The demo module always; with
//! `CUBED_MODULES=<dir>`, every module in that directory too.
//!
//! cargo bench -p cubed-tracker --bench module_render

use std::hint::black_box;
use std::time::Instant;

use cubed_tracker::{Player, PlayerOptions};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;
const SECONDS: f64 = 60.0;
const RUNS: usize = 3;

/// Seconds to render `SECONDS` of the module (looped), the best of `RUNS`.
fn time_module(module: &cubed_tracker::Module) -> f64 {
    let blocks = (RATE as f64 * SECONDS) as usize / BLOCK;
    let mut best = f64::INFINITY;
    for _ in 0..RUNS {
        let mut player = Player::new(module.clone(), RATE, PlayerOptions::default());
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
    let mut files = vec![concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/demo.mod").into()];
    if let Some(dir) = std::env::var_os("CUBED_MODULES") {
        let mut more: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        more.sort();
        files.extend(more);
    }
    let deadline_us = BLOCK as f64 / RATE as f64 * 1e6;
    let blocks = (RATE as f64 * SECONDS) as usize / BLOCK;
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let Ok(module) = cubed_tracker::load(&bytes) else {
            continue;
        };
        let secs = time_module(&module);
        let block_us = secs / blocks as f64 * 1e6;
        println!(
            "{:<34} {:>2} ch: {:>6.0}x realtime, {:>6.2} us/block ({:.2}% of {:.0} us)",
            path.file_name().unwrap().to_string_lossy(),
            module.channels,
            SECONDS / secs,
            block_us,
            block_us / deadline_us * 100.0,
            deadline_us,
        );
    }
}
