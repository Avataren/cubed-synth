//! The live control of an AHX/HVL song (cubed-ahx's transport hooks) allocates
//! nothing: seeks, loops, mute and solo, the audition, levels, the probe, and the
//! live edits a playing song takes (a step, a position's track, the restart).
//!
//! cargo test -p cubed-rt-check --test ahx_control
//!
//! With `CUBED_RT_TRACE=1` every counted allocation prints a backtrace.

use cubed_ahx::player::AhxPlayer;
use cubed_ahx::probe::AhxProbe;
use cubed_rt_check::{allocations_during, trace_allocations, CountingAllocator};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
const WARMUP_BLOCKS: usize = RATE as usize / BLOCK;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../ahx/tests/fixtures/songs/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The controls a host sends in a block's turn, cycling through all of them.
fn control(p: &mut AhxPlayer, block: usize, positions: usize, channels: usize) {
    let ch = block % channels;
    match block % 60 {
        3 => {
            p.seek(block % positions, block % 7);
        }
        7 => {
            p.loop_positions(block % positions, block % positions);
        }
        11 => {
            p.loop_rows(block % positions, 0, 3);
        }
        15 => p.loop_off(),
        19 => p.set_mute(ch, true),
        21 => p.set_mute(ch, false),
        23 => p.set_solo(ch, true),
        25 => p.set_solo(ch, false),
        29 => {
            p.audition_on(1, 36 + (block % 24) as i32, 100, 0);
        }
        33 => p.audition_off(),
        37 => {
            p.audition_on(2, 40 + (block % 20) as i32, 90, 2000);
        }
        41 => {
            p.pause();
        }
        43 => p.resume(),
        45 => {
            p.set_step(1, block % 8, (block % 40) as u8 + 1, 1, 0, 0, 0, 0);
        }
        47 => {
            p.set_position_entry(0, ch, 1, (block % 5) as i32 - 2);
        }
        49 => {
            p.set_restart(block % positions);
        }
        51 => p.set_continue_phase_on_trigger(block.is_multiple_of(2)),
        _ => {}
    }
    // What a UI reads every frame.
    let mut levels = [0.0f32; 17];
    p.levels(&mut levels);
    let _ = (p.play_position(), p.play_row(), p.play_tick(), p.status());
}

#[test]
fn ahx_transport_control_allocates_nothing() {
    trace_allocations(std::env::var_os("CUBED_RT_TRACE").is_some());
    for name in ["a_new_beginning.ahx", "a_little_cyberfunk.hvl", "karma.ahx"] {
        for hifi in [false, true] {
            let bytes = fixture(name);
            let mut p = AhxPlayer::new(&bytes, RATE, 2).expect("fixture parses");
            p.set_hifi(hifi);
            p.set_levels(true);
            let probe = AhxProbe::new(p.channels());
            p.attach_probe(probe.clone());
            p.play();
            let (positions, channels) = (p.position_count().max(1), p.channels());
            let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
            // The first blocks size the scratch buffer and build tables.
            for block in 0..WARMUP_BLOCKS {
                p.render(&mut left, &mut right);
                let _ = block;
            }
            // Audition once before counting: with hi-fi on the first note of an
            // instrument builds nothing (it uses the plain oscillators), but the
            // first tick of anything is a warm-up.
            p.audition_on(1, 48, 100, 0);
            p.render(&mut left, &mut right);
            p.audition_off();
            for block in 0..WARMUP_BLOCKS / 4 {
                p.render(&mut left, &mut right);
                let _ = block;
            }
            for block in 0..60 * 400 {
                let events = allocations_during(|| {
                    control(&mut p, block, positions, channels);
                    p.render(&mut left, &mut right);
                });
                assert_eq!(
                    events, 0,
                    "{name} hifi={hifi}: block {block} (control {}) made {events} allocations or frees",
                    block % 60
                );
            }
            assert!(probe.status().frames > 0);
        }
    }
}
