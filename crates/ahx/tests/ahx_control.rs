//! Live control of a playing song (an editor's transport): loops, the position
//! and tick, per-channel mute and solo, levels, the probe, and the audition.

use cubed_ahx::edit::AhxDoc;
use cubed_ahx::engine::{AhxEngine, Loop};
use cubed_ahx::format::{self, SongFormat};
use cubed_ahx::player::AhxPlayer;
use cubed_ahx::probe::AhxProbe;
use serde_json::json;
use std::path::Path;

const RATE: u32 = 44100;

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/songs")
            .join(name),
    )
    .unwrap()
}

/// Renders one tick and returns where it was: (position, row, tick).
fn tick(e: &mut AhxEngine, buf: &mut Vec<i16>) -> (i32, i32, i32) {
    buf.resize(e.samples_per_tick() * 2, 0);
    e.render_block(buf);
    e.position()
}

/// A song of `positions` positions, each `rows` rows, each row a tick or two:
/// channel 0 plays a note on every row (so there is something to hear).
fn grid_song(positions: usize, rows: usize) -> AhxDoc {
    let mut d = AhxDoc::new(SongFormat::Ahx);
    d.apply(&json!({"op": "set_track_length", "rows": rows}))
        .unwrap();
    d.apply(&json!({"op": "add_instrument", "name": "Saw", "envelope": {"a_frames": 1, "a_volume": 64, "d_frames": 0, "d_volume": 64, "s_frames": 255, "r_frames": 4, "r_volume": 0}})).unwrap();
    for r in 0..rows {
        d.apply(&json!({"op": "set_step", "track": 1, "row": r, "cell": [20 + (r % 12), 1, 0, 0]}))
            .unwrap();
    }
    // Every row lasts 2 ticks.
    d.apply(&json!({"op": "set_step", "track": 1, "row": 0, "cell": [20, 1, 15, 2]}))
        .unwrap();
    for p in 1..positions {
        d.apply(&json!({"op": "insert_position", "at": p, "kind": "duplicate", "of": 0}))
            .unwrap();
    }
    d
}

fn engine_of(d: &AhxDoc) -> AhxEngine {
    AhxEngine::new(d.song().clone(), RATE, 2).unwrap()
}

#[test]
fn the_position_row_and_tick_of_the_tick_that_played() {
    let d = grid_song(3, 8);
    let mut e = engine_of(&d);
    let mut buf = Vec::new();
    // Row 0 sets the speed to 2 in its first tick, so every row is two ticks.
    let mut seen = Vec::new();
    for _ in 0..8 {
        seen.push(tick(&mut e, &mut buf));
    }
    assert_eq!(seen[0], (0, 0, 0));
    assert_eq!(seen[1], (0, 0, 1));
    assert_eq!(seen[2], (0, 1, 0));
    assert_eq!(seen[3], (0, 1, 1));
    assert_eq!(seen[7], (0, 3, 1));
    assert_eq!(e.rows_started(), 4);
    assert_eq!(e.frames_rendered(), 8 * e.samples_per_tick() as u64);
    // The row after a position's last row is the next position's first.
    for _ in 0..(16 - 8) {
        tick(&mut e, &mut buf);
    }
    assert_eq!(tick(&mut e, &mut buf), (1, 0, 0));
    // Partial blocks leave the position where the last tick was.
    let mut e = engine_of(&d);
    let mut small = vec![0i16; 2 * 100];
    e.render_block(&mut small);
    assert_eq!(e.position(), (0, 0, 0));
}

#[test]
fn a_position_loop_repeats_the_position_and_never_ends_the_song() {
    let d = grid_song(4, 8);
    let mut e = engine_of(&d);
    assert_eq!(e.seek(2, 0), Some(cubed_ahx::engine::SeekKind::Exact));
    assert!(e.set_loop(Loop::Position(2)));
    let mut buf = Vec::new();
    let mut rows = std::collections::BTreeSet::new();
    let mut laps = 0;
    let mut last_row = -1;
    for _ in 0..200 {
        let (p, r, _) = tick(&mut e, &mut buf);
        assert_eq!(p, 2, "the loop stays on position 2");
        rows.insert(r);
        if r < last_row {
            laps += 1;
        }
        last_row = r;
        assert!(!e.song_end_reached());
    }
    assert_eq!(rows.len(), 8);
    assert!(laps >= 10);
    // Loop off: the song moves on again.
    e.set_loop(Loop::None);
    let mut left = false;
    for _ in 0..40 {
        left |= tick(&mut e, &mut buf).0 != 2;
    }
    assert!(left);
}

#[test]
fn a_loop_of_positions_wraps_from_the_last_to_the_first() {
    let d = grid_song(6, 4);
    let mut e = engine_of(&d);
    e.seek(1, 0).unwrap();
    assert!(e.set_loop(Loop::Positions { first: 1, last: 3 }));
    let mut buf = Vec::new();
    let mut order = Vec::new();
    for _ in 0..200 {
        let (p, _, _) = tick(&mut e, &mut buf);
        if order.last() != Some(&p) {
            order.push(p);
        }
    }
    assert_eq!(&order[..7], &[1, 2, 3, 1, 2, 3, 1]);
    assert!(order.iter().all(|p| (1..=3).contains(p)));
}

#[test]
fn a_loop_somewhere_else_is_waited_for() {
    let d = grid_song(6, 4);
    let mut e = engine_of(&d);
    assert!(e.set_loop(Loop::Position(3)));
    let mut buf = Vec::new();
    let mut order = Vec::new();
    for _ in 0..120 {
        let (p, _, _) = tick(&mut e, &mut buf);
        if order.last() != Some(&p) {
            order.push(p);
        }
    }
    assert_eq!(
        order,
        vec![0, 1, 2, 3],
        "it plays on to position 3, then stays"
    );
}

#[test]
fn a_row_loop_repeats_some_rows_of_a_position() {
    let d = grid_song(3, 16);
    let mut e = engine_of(&d);
    e.seek(1, 4).unwrap();
    assert!(e.set_loop(Loop::Rows {
        position: 1,
        row0: 4,
        row1: 7
    }));
    let mut buf = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..200 {
        let (p, r, _) = tick(&mut e, &mut buf);
        assert_eq!(p, 1);
        assert!((4..=7).contains(&r), "row {r}");
        seen.insert(r);
    }
    assert_eq!(seen.len(), 4);
    // A row sought to beyond the range plays on to the end of the position and moves on.
    let mut e = engine_of(&d);
    e.seek(1, 9).unwrap();
    assert!(e.set_loop(Loop::Rows {
        position: 1,
        row0: 4,
        row1: 7
    }));
    let mut moved = false;
    for _ in 0..100 {
        moved |= tick(&mut e, &mut buf).0 != 1;
    }
    assert!(moved);
}

#[test]
fn a_jump_out_of_a_loop_wraps_to_its_start_and_a_jump_inside_it_is_kept() {
    let mut d = grid_song(5, 4);
    // Position 1, row 3: jump to position 4 (B04); position 2, row 3: jump to position 0 (B00).
    d.apply(&json!({"op": "set_step", "position": 1, "channel": 1, "row": 3, "cell": [0, 0, 11, 4], "unique": true})).unwrap();
    d.apply(&json!({"op": "set_step", "position": 2, "channel": 1, "row": 3, "cell": [0, 0, 11, 1], "unique": true})).unwrap();
    // Without a loop the song follows the jumps.
    let mut e = engine_of(&d);
    let mut buf = Vec::new();
    let mut order = Vec::new();
    for _ in 0..80 {
        let (p, _, _) = tick(&mut e, &mut buf);
        if order.last() != Some(&p) {
            order.push(p);
        }
    }
    assert_eq!(&order[..4], &[0, 1, 4, 0]);
    // A loop over positions 0..=2: the jump to 4 leaves it, so it wraps to 0; the
    // jump from 2 to 1 stays inside it and is kept.
    let mut e = engine_of(&d);
    assert!(e.set_loop(Loop::Positions { first: 0, last: 2 }));
    let mut order = Vec::new();
    for _ in 0..120 {
        let (p, _, _) = tick(&mut e, &mut buf);
        if order.last() != Some(&p) {
            order.push(p);
        }
        assert!(!e.song_end_reached());
    }
    assert_eq!(
        &order[..6],
        &[0, 1, 0, 1, 0, 1],
        "B04 from position 1 wraps to 0: {order:?}"
    );
    // A rows loop ignores the jump and stays in its rows.
    let mut e = engine_of(&d);
    e.seek(1, 0).unwrap();
    assert!(e.set_loop(Loop::Rows {
        position: 1,
        row0: 0,
        row1: 3
    }));
    for _ in 0..60 {
        assert_eq!(tick(&mut e, &mut buf).0, 1);
    }
}

#[test]
fn set_loop_refuses_what_the_song_does_not_have_and_a_seek_keeps_the_loop() {
    let d = grid_song(3, 8);
    let mut e = engine_of(&d);
    assert!(!e.set_loop(Loop::Position(3)));
    assert!(!e.set_loop(Loop::Positions { first: 2, last: 1 }));
    assert!(!e.set_loop(Loop::Positions { first: 0, last: 3 }));
    assert!(!e.set_loop(Loop::Rows {
        position: 0,
        row0: 5,
        row1: 4
    }));
    assert!(!e.set_loop(Loop::Rows {
        position: 0,
        row0: 0,
        row1: 8
    }));
    assert_eq!(e.loop_range(), Loop::None);
    assert!(e.set_loop(Loop::Rows {
        position: 1,
        row0: 2,
        row1: 3
    }));
    assert!(!e.set_loop(Loop::Position(9)));
    assert_eq!(
        e.loop_range(),
        Loop::Rows {
            position: 1,
            row0: 2,
            row1: 3
        },
        "a refused loop leaves the old one"
    );
    e.seek(2, 1).unwrap();
    assert_eq!(
        e.loop_range(),
        Loop::Rows {
            position: 1,
            row0: 2,
            row1: 3
        }
    );
    e.init_subsong(0);
    assert_eq!(
        e.loop_range(),
        Loop::Rows {
            position: 1,
            row0: 2,
            row1: 3
        }
    );
    let mut p = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    assert!(p.loop_positions(1, 2));
    assert_eq!(p.loop_range(), Loop::Positions { first: 1, last: 2 });
    assert!(!p.loop_rows(0, 3, 2));
    p.loop_off();
    assert_eq!(p.loop_range(), Loop::None);
}

#[test]
fn the_loop_is_what_the_old_position_loop_was_for_the_current_position() {
    // `Loop::Position(p)` after a seek to (p, 0) is the transport `set_loop_position(true)` made.
    let b = bytes("karma.ahx");
    let song = format::parse(&b).unwrap();
    let mut a = AhxEngine::new(song.clone(), RATE, 2).unwrap();
    let mut c = AhxEngine::new(song, RATE, 2).unwrap();
    a.seek(5, 0).unwrap();
    c.seek(5, 0).unwrap();
    a.set_loop_position(true);
    c.set_loop(Loop::Position(5));
    let mut out_a = vec![0i16; 2 * 44100];
    let mut out_c = out_a.clone();
    a.render_block(&mut out_a);
    c.render_block(&mut out_c);
    // The two agree until a Bxx/Dxx in the position (which the old flag lets move on).
    let tick = a.samples_per_tick() * 2;
    let _ = tick;
    assert_eq!(a.position().0, 5);
    assert_eq!(c.position().0, 5);
    assert_eq!(out_a, out_c);
}

#[test]
fn per_channel_mute_and_solo_are_the_masks() {
    let b = bytes("karma.ahx");
    let mut p = AhxPlayer::new(&b, RATE, 2).unwrap();
    let mut q = AhxPlayer::new(&b, RATE, 2).unwrap();
    p.play();
    q.play();
    p.set_mute(1, true);
    p.set_mute(3, true);
    p.set_mute(3, false);
    q.engine_mut().set_mute_solo(0b10, 0);
    let (mut l1, mut r1, mut l2, mut r2) = (
        vec![0.0; 4410],
        vec![0.0; 4410],
        vec![0.0; 4410],
        vec![0.0; 4410],
    );
    p.render(&mut l1, &mut r1);
    q.render(&mut l2, &mut r2);
    assert_eq!((l1.clone(), r1.clone()), (l2, r2));
    p.set_mute(1, false);
    p.set_solo(2, true);
    assert_eq!(p.engine().mute_solo(), (0, 0b100));
    p.set_solo(2, false);
    assert_eq!(p.engine().mute_solo(), (0, 0));
    // channels past the 32nd are no bit
    p.set_mute(40, true);
    assert_eq!(p.engine().mute_solo(), (0, 0));
}

#[test]
fn levels_follow_the_channels_and_zero_when_silenced() {
    let d = grid_song(2, 8);
    let mut p = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    p.play();
    let mut out = [9.0f32; 8];
    assert_eq!(p.levels(&mut out), 0, "off by default");
    p.set_levels(true);
    let (mut l, mut r) = (vec![0.0; 4410], vec![0.0; 4410]);
    p.render(&mut l, &mut r);
    assert_eq!(p.levels(&mut out), 5, "four channels and the audition's");
    assert!(out[0] > 0.01 && out[0] <= 1.0, "{out:?}");
    assert_eq!(
        out[1..4],
        [0.0, 0.0, 0.0],
        "the other channels play nothing"
    );
    assert_eq!(out[4], 0.0);
    // The peak is a bound on what the output reaches: left and right are each below it.
    let loudest = l.iter().chain(&r).fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(
        loudest <= out[0] * 1.001 + 0.001,
        "{loudest} against {}",
        out[0]
    );
    p.set_mute(0, true);
    p.render(&mut l, &mut r);
    p.levels(&mut out);
    assert_eq!(out[0], 0.0, "a muted channel reads 0");
    p.set_mute(0, false);
    p.render(&mut l, &mut r);
    p.set_levels(false);
    assert_eq!(p.levels(&mut out), 0);
}

#[test]
fn levels_fall_after_a_note_ends() {
    let mut d = grid_song(1, 4);
    d.apply(&json!({"op": "clear_range", "track": 1, "row0": 1, "row1": 3}))
        .unwrap();
    d.apply(&json!({"op": "set_step", "track": 1, "row": 0, "cell": [20, 1, 15, 40]}))
        .unwrap();
    d.apply(&json!({"op": "set_instrument", "n": 1, "envelope": {"s_frames": 4}}))
        .unwrap();
    let mut p = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    p.play();
    p.set_levels(true);
    let mut out = [0.0f32; 5];
    let (mut l, mut r) = (vec![0.0; 441], vec![0.0; 441]);
    let mut series = vec![];
    // 3 s = 150 ticks: the note lasts about 10 of them, the row 40.
    for _ in 0..300 {
        p.render(&mut l, &mut r);
        p.levels(&mut out);
        series.push(out[0]);
    }
    let peak = series.iter().cloned().fold(0.0f32, f32::max);
    assert!(peak > 0.05, "{peak}");
    let end = *series.last().unwrap();
    assert!(
        end < peak * 0.01,
        "the level has fallen by the end: {end} against {peak}"
    );
    // It falls monotonically once the note is over (24 dB a second).
    let top = series.iter().position(|&x| x == peak).unwrap();
    let tail = &series[top + 60..];
    assert!(tail.windows(2).all(|w| w[1] <= w[0]), "{tail:?}");
}

#[test]
fn the_probe_shows_the_position_and_levels_to_another_thread() {
    let d = grid_song(3, 8);
    let mut p = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    p.set_levels(true);
    let probe = AhxProbe::new(p.channels());
    p.attach_probe(probe.clone());
    assert!(probe.alive());
    assert!(!probe.status().playing);
    p.play();
    let (mut l, mut r) = (vec![0.0; 882], vec![0.0; 882]);
    let mut versions = std::collections::BTreeSet::new();
    let reader = {
        let probe = probe.clone();
        std::thread::spawn(move || {
            let mut out = [0.0f32; 5];
            probe.levels(&mut out);
            probe.status()
        })
    };
    for _ in 0..30 {
        p.render(&mut l, &mut r);
        versions.insert(probe.status().version);
    }
    let s = probe.status();
    assert!(s.playing && s.speed == 2 && s.speed_multiplier == 1);
    assert_eq!(s.frames, 30 * 882);
    let local = p.status();
    assert_eq!(
        (s.position, s.row, s.tick, s.rows),
        (local.position, local.row, local.tick, local.rows)
    );
    assert!(s.rows >= 6, "{s:?}");
    assert!(versions.len() > 5, "the version moves with the song");
    let mut out = [0.0f32; 5];
    assert_eq!(probe.levels(&mut out), 5);
    assert!(out[0] > 0.0);
    let _ = reader.join().unwrap();
    // A stopped song is quiet: the version stops moving.
    p.pause();
    p.render(&mut l, &mut r);
    p.render(&mut l, &mut r);
    let v = probe.status().version;
    p.render(&mut l, &mut r);
    assert_eq!(probe.status().version, v);
    assert!(!probe.status().playing);
    drop(p);
    assert!(!probe.alive());
}

// ----- the audition ----------------------------------------------------------------

fn audition_song() -> AhxDoc {
    let mut d = AhxDoc::new(SongFormat::Ahx);
    d.apply(&json!({"op": "add_instrument", "name": "Pad", "envelope": {"a_frames": 2, "a_volume": 64, "d_frames": 0, "d_volume": 64, "s_frames": 255, "r_frames": 6, "r_volume": 0}})).unwrap();
    d.apply(&json!({"op": "add_instrument", "name": "Plain", "envelope": {"a_frames": 1, "a_volume": 64, "d_frames": 0, "d_volume": 64, "s_frames": 255, "r_frames": 0, "r_volume": 0}, "hard_cut_release": true, "hard_cut_release_frames": 3})).unwrap();
    d.apply(&json!({"op": "set_step", "track": 1, "row": 0, "cell": [13, 1, 0, 0]}))
        .unwrap();
    d
}

#[test]
fn an_audition_plays_alone_while_the_song_is_stopped_and_ends_after_its_release() {
    let d = audition_song();
    let mut p = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    let (mut l, mut r) = (vec![0.0; 2205], vec![0.0; 2205]);
    // Stopped, no audition: silence.
    p.render(&mut l, &mut r);
    assert!(l.iter().chain(&r).all(|&x| x == 0.0));
    assert!(!p.audition_on(0, 48, 100, 0));
    assert!(
        !p.audition_on(3, 48, 100, 0),
        "the song has two instruments"
    );
    assert!(!p.audition_active());
    assert!(p.audition_on(1, 48, 100, 0), "midi 48 is C-3");
    assert!(p.audition_active());
    p.render(&mut l, &mut r);
    let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
    assert!(rms(&l) > 0.01 && rms(&r) > 0.01, "{} {}", rms(&l), rms(&r));
    // Centred: both channels carry it equally.
    assert!((rms(&l) - rms(&r)).abs() < 0.001);
    assert_eq!(p.play_position(), 0, "the song did not move");
    assert_eq!(p.rows_started(), 0);
    // Held until it is let go, then it releases and the voice ends.
    for _ in 0..10 {
        p.render(&mut l, &mut r);
    }
    assert!(p.audition_active(), "a held note goes on");
    p.audition_off();
    for _ in 0..4 {
        p.render(&mut l, &mut r);
    }
    assert!(!p.audition_active());
    p.render(&mut l, &mut r);
    assert!(l.iter().chain(&r).all(|&x| x == 0.0));
    // A duration lets go by itself.
    assert!(p.audition_on(1, 48, 100, 4410));
    for _ in 0..4 {
        p.render(&mut l, &mut r);
    }
    assert!(
        !p.audition_active(),
        "a 0.1 s note and its release are over after 0.2 s"
    );
}

#[test]
fn an_audition_is_added_to_the_song_and_does_not_move_it() {
    let d = grid_song(2, 8);
    let mut with = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    let mut without = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    let mut alone = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    for p in [&mut with, &mut without] {
        p.play();
    }
    let n = 3000;
    let (mut a, mut b, mut c) = (
        (vec![0.0; n], vec![0.0; n]),
        (vec![0.0; n], vec![0.0; n]),
        (vec![0.0; n], vec![0.0; n]),
    );
    assert!(with.audition_on(1, 60, 127, 0));
    assert!(alone.audition_on(1, 60, 127, 0));
    // Several blocks, with the song playing in two of the players.
    for _ in 0..6 {
        with.render(&mut a.0, &mut a.1);
        without.render(&mut b.0, &mut b.1);
        alone.render(&mut c.0, &mut c.1);
        for i in 0..n {
            let clips = (b.0[i] + c.0[i]).abs() > 0.999 || (b.1[i] + c.1[i]).abs() > 0.999;
            if !clips {
                assert!(
                    (a.0[i] - (b.0[i] + c.0[i])).abs() < 1e-6
                        && (a.1[i] - (b.1[i] + c.1[i])).abs() < 1e-6,
                    "frame {i}: {} vs {} + {}",
                    a.0[i],
                    b.0[i],
                    c.0[i]
                );
            }
        }
        assert_eq!(with.engine().position(), without.engine().position());
    }
    assert!(with.audition_active());
    // A muted channel and mute-all do not touch the audition; it is "whatever is muted".
    with.engine_mut().set_mute_solo(0b1111, 0);
    with.render(&mut a.0, &mut a.1);
    alone.render(&mut c.0, &mut c.1);
    assert_eq!((a.0.clone(), a.1.clone()), (c.0.clone(), c.1.clone()));
}

#[test]
fn the_audition_plays_the_instruments_plist_and_reports_its_row() {
    let mut d = audition_song();
    d.apply(&json!({"op": "set_instrument", "n": 1, "plist_speed": 2}))
        .unwrap();
    d.apply(&json!({"op": "insert_plist_row", "n": 1, "row": 1, "count": 3, "entry": [8, 3, 0, 0, 0, 0, 0]})).unwrap();
    let mut p = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    assert_eq!(p.audition_plist_row(), -1);
    assert!(p.audition_on(1, 48, 100, 0));
    let (mut l, mut r) = (vec![0.0; 441], vec![0.0; 441]);
    let mut rows = std::collections::BTreeSet::new();
    for _ in 0..20 {
        p.render(&mut l, &mut r);
        rows.insert(p.audition_plist_row());
        assert_eq!(p.audition_plist_instrument(), 1);
    }
    assert!(rows.len() >= 3, "{rows:?}");
    assert!(rows.iter().all(|&x| (0..4).contains(&x)));
    // A new note cuts the last and starts from row 0.
    assert!(p.audition_on(2, 50, 90, 0));
    p.render(&mut l, &mut r);
    assert_eq!(p.audition_plist_instrument(), 2);
}

#[test]
fn an_audition_does_not_use_hifi_and_survives_a_rewind() {
    let d = audition_song();
    let mut p = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    p.set_hifi(true);
    assert!(p.audition_on(1, 48, 100, 0));
    let (mut l, mut r) = (vec![0.0; 441], vec![0.0; 441]);
    p.render(&mut l, &mut r);
    assert!(l.iter().any(|&x| x != 0.0));
    assert_eq!(p.hifi_miss_count(), 0.0);
    assert!(p.restart(0));
    assert!(
        p.audition_active(),
        "a rewind of the song leaves the audition"
    );
}

#[test]
fn seek_reload_and_restart_still_do_what_they_did() {
    let b = bytes("karma.ahx");
    let mut p = AhxPlayer::new(&b, RATE, 2).unwrap();
    p.play();
    assert_eq!(p.seek(3, 5), 1);
    let (mut l, mut r) = (vec![0.0; 441], vec![0.0; 441]);
    p.render(&mut l, &mut r);
    assert_eq!((p.play_position(), p.play_row(), p.play_tick()), (3, 5, 0));
    // reload with the same song, from the place it was at, renders what a seek there does
    let place = (p.play_position() as usize, p.play_row() as usize);
    let mut q = AhxPlayer::new(&b, RATE, 2).unwrap();
    q.play();
    q.set_mute(1, true);
    q.reload_song(format::parse(&b).unwrap(), Some(place))
        .unwrap();
    assert_eq!(q.engine().mute_solo(), (0b10, 0), "the mutes stay");
    let (mut l2, mut r2) = (vec![0.0; 4410], vec![0.0; 4410]);
    q.render(&mut l2, &mut r2);
    let mut s = AhxPlayer::new(&b, RATE, 2).unwrap();
    s.play();
    s.set_mute(1, true);
    s.seek(place.0, place.1);
    let (mut l3, mut r3) = (vec![0.0; 4410], vec![0.0; 4410]);
    s.render(&mut l3, &mut r3);
    assert_eq!((l2, r2), (l3, r3));
    // A place past the new song is clamped; a song the engine cannot play is refused untouched.
    let mut empty = format::parse(&b).unwrap();
    empty.positions.clear();
    empty.position_nr = 0;
    assert!(q.reload_song(empty, None).is_err());
    q.reload_song(format::parse(&b).unwrap(), Some((999, 99)))
        .unwrap();
    q.render(&mut l, &mut r);
    // The song end is reached from the top again after a resume.
    q.pause();
    q.resume();
    assert!(q.is_playing());
}

#[test]
fn the_new_controls_never_change_what_an_untouched_player_renders() {
    // Every control is off by default: the output is the reference's, bit for bit,
    // with the audition and levels having existed all along.
    let b = bytes("blondie.ahx");
    let mut p = AhxPlayer::new(&b, RATE, 2).unwrap();
    let mut q = AhxPlayer::new(&b, RATE, 2).unwrap();
    p.play();
    q.play();
    q.set_levels(true);
    q.attach_probe(AhxProbe::new(4));
    let (mut l1, mut r1, mut l2, mut r2) = (
        vec![0.0; 8820],
        vec![0.0; 8820],
        vec![0.0; 8820],
        vec![0.0; 8820],
    );
    for _ in 0..10 {
        p.render(&mut l1, &mut r1);
        q.render(&mut l2, &mut r2);
        assert_eq!((l1.clone(), r1.clone()), (l2.clone(), r2.clone()));
    }
}
