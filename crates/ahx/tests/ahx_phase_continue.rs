//! Phase-continue on trigger: the flag-on/flag-off matrix on the real engine.
//!
//! The deliberate divergence from the reference: with the flag on, an
//! instrument trigger keeps each voice's wave read position instead of
//! restarting it at 0 (`hvl_replay.c:893`), as the 68k player does
//! (`.ai/ahx/68k-investigation.md` sections 2a/4). Off is the reference, byte
//! for byte, so the goldens (`ahx_render_golden.rs`, which builds its engines
//! with the default) keep proving it; this file pins the *difference*.
//!
//! The trigger-level claim (first sample after a trigger is neither wave[0]
//! nor the flag-off sample, mid-cycle, saw and square) is a unit test next to
//! `Voice::trigger_instrument`, where a voice can be put mid-cycle exactly.

use cubed_ahx::engine::{AhxEngine, SeekKind};
use cubed_ahx::format;
use cubed_ahx::player::AhxPlayer;
use std::fs;
use std::path::Path;

const RATE: u32 = 44100;
const SONGS: [&str; 4] = [
    "karma.ahx",
    "robocop_iii_j_tel.ahx",
    "blondie.ahx",
    "sunspots.hvl",
];

fn bytes(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/songs")
            .join(name),
    )
    .unwrap()
}

fn engine(name: &str, on: Option<bool>, hifi: bool) -> AhxEngine {
    let mut e = AhxEngine::new(format::parse(&bytes(name)).unwrap(), RATE, 2).unwrap();
    if let Some(on) = on {
        e.set_continue_phase_on_trigger(on);
    }
    if hifi {
        e.set_hifi(true);
        e.prewarm_hifi();
    }
    e
}

fn render(e: &mut AhxEngine, frames: usize) -> Vec<i16> {
    let mut out = vec![0i16; frames * 2];
    for chunk in out.chunks_mut(2 * 441) {
        e.render_block(chunk);
    }
    out
}

fn first_difference(a: &[i16], b: &[i16]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
}

#[test]
fn the_default_is_off_and_off_is_the_default_render() {
    for name in SONGS {
        let mut implicit = engine(name, None, false);
        assert!(
            !implicit.continue_phase_on_trigger(),
            "{name}: default must be off"
        );
        let mut explicit = engine(name, Some(false), false);
        let (a, b) = (
            render(&mut implicit, RATE as usize * 3),
            render(&mut explicit, RATE as usize * 3),
        );
        assert!(a == b, "{name}: explicit off differs from the default");
    }
}

#[test]
fn matrix_on_differs_from_off_and_is_deterministic() {
    for name in SONGS {
        for hifi in [false, true] {
            let frames = RATE as usize * 4;
            let off = render(&mut engine(name, Some(false), hifi), frames);
            let on = render(&mut engine(name, Some(true), hifi), frames);
            let on_again = render(&mut engine(name, Some(true), hifi), frames);
            assert!(
                on == on_again,
                "{name} hifi={hifi}: flag-on render is not deterministic"
            );
            let at = first_difference(&off, &on);
            assert!(
                at.is_some(),
                "{name} hifi={hifi}: flag on changed nothing in 4 s"
            );
            // A song's first notes trigger on a fresh voice (phase 0 either way),
            // so the divergence must start after the first trigger, not at frame 0.
            assert!(
                on.iter().any(|&s| s != 0),
                "{name} hifi={hifi}: flag on is silent"
            );
        }
    }
}

#[test]
fn flipping_the_flag_back_off_restores_the_reference_for_later_triggers() {
    // Only future triggers are affected: on then off from the start is the
    // reference render.
    let name = "karma.ahx";
    let frames = RATE as usize * 3;
    let reference = render(&mut engine(name, None, false), frames);
    let mut e = engine(name, Some(true), false);
    e.set_continue_phase_on_trigger(false);
    assert!(render(&mut e, frames) == reference);
}

#[test]
fn seek_stays_exact_with_the_flag_on() {
    // A seek replays the song without mixing and must land exactly where a
    // full render would; with phase-continue the phase at a trigger is history
    // too, so `skip_mix` has to keep it right.
    for name in ["karma.ahx", "blondie.ahx"] {
        for hifi in [false, true] {
            let mut probe = engine(name, Some(true), false);
            let tick = probe.samples_per_tick();
            let mut buf = vec![0i16; tick * 2];
            let mut target = None;
            let mut last = (-1, -1);
            for ticks in 0..50 * 30 {
                let here = (probe.pos_nr(), probe.note_nr());
                if here != last {
                    last = here;
                    if ticks > 50 * 12 && here.1 != 0 {
                        target = Some((here.0 as usize, here.1 as usize, ticks));
                        break;
                    }
                }
                probe.render_block(&mut buf);
            }
            let (pos, row, ticks) = target.unwrap_or_else(|| panic!("{name}: no target row"));

            let mut full = engine(name, Some(true), hifi);
            for _ in 0..ticks {
                full.render_block(&mut buf);
            }
            let mut seeked = engine(name, Some(true), hifi);
            assert_eq!(seeked.seek(pos, row), Some(SeekKind::Exact), "{name}");
            assert!(
                seeked.continue_phase_on_trigger(),
                "the flag survives a seek"
            );
            let (a, b) = (
                render(&mut full, RATE as usize * 2),
                render(&mut seeked, RATE as usize * 2),
            );
            assert!(
                a == b,
                "{name} hifi={hifi}: seek with the flag on differs from a full run"
            );
        }
    }
}

#[test]
fn the_player_exposes_the_flag_and_keeps_it_across_restart() {
    let mut p = AhxPlayer::new(&bytes("karma.ahx"), RATE, 2).unwrap();
    assert!(!p.continue_phase_on_trigger());
    p.set_continue_phase_on_trigger(true);
    assert!(p.continue_phase_on_trigger());
    assert!(p.restart(0));
    assert!(p.continue_phase_on_trigger());
}
