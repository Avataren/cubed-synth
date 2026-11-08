//! Following an edit with a song that is playing: which edits a player takes
//! without being opened again (`AhxDoc::live_edits`), that applying them leaves
//! the player's song level with the document, and what has to be rebuilt.

use cubed_ahx::edit::AhxDoc;
use cubed_ahx::format::{Song, SongFormat};
use cubed_ahx::player::AhxPlayer;
use serde_json::{json, Value};
use std::path::Path;

const RATE: u32 = 44100;

fn doc(name: &str) -> AhxDoc {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/songs")
        .join(name);
    AhxDoc::from_bytes(&std::fs::read(path).unwrap()).unwrap()
}

fn player(d: &AhxDoc) -> AhxPlayer {
    let mut p = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
    p.play();
    p
}

/// The two songs without the names a live edit leaves alone (an instrument
/// replaced in a playing song keeps its name).
fn same(a: &Song, b: &Song) -> bool {
    let strip = |s: &Song| {
        let mut s = s.clone();
        for i in &mut s.instruments {
            i.name.clear();
        }
        s.name.clear();
        s
    };
    strip(a) == strip(b)
}

fn render(p: &mut AhxPlayer, frames: usize) -> Vec<f32> {
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    p.render(&mut l, &mut r);
    l.extend(r);
    l
}

/// Runs `ops` on `d` and brings `p` level with it, live when the document says
/// so and by `reload_song` when not. Returns whether it was live.
fn follow(d: &mut AhxDoc, p: &mut AhxPlayer, ops: Value) -> bool {
    d.apply(&ops).unwrap_or_else(|e| panic!("{ops}: {e}"));
    sync(d, p, &ops)
}

fn sync(d: &AhxDoc, p: &mut AhxPlayer, what: &Value) -> bool {
    match d.live_edits() {
        Some(live) => {
            d.apply_live(p, &live)
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            assert!(
                same(p.song(), d.song()),
                "{what}: the player's song is not the document's"
            );
            true
        }
        None => {
            let place = (
                p.play_position().max(0) as usize,
                p.play_row().max(0) as usize,
            );
            p.reload_song(d.song().clone(), Some(place)).unwrap();
            assert!(same(p.song(), d.song()), "{what}: reload");
            false
        }
    }
}

#[test]
fn every_kind_of_edit_is_live_or_not_as_documented() {
    let mut d = doc("karma.ahx");
    let mut p = player(&d);
    render(&mut p, 5000);

    let live: Vec<Value> = vec![
        json!({"op": "set_step", "track": 3, "row": 2, "cell": [5, 1, 0, 0]}),
        json!({"op": "clear_range", "track": 3, "row0": 0, "row1": 7}),
        json!({"op": "insert_row", "track": 4, "row": 1, "count": 2}),
        json!({"op": "delete_row", "track": 4, "row": 1, "count": 1}),
        json!({"op": "transpose", "track": 5, "row0": 0, "row1": 63, "semitones": 2, "clamp": true}),
        json!({"op": "paste", "track": 6, "row": 4, "cells": [[10, 1, 0, 0, 0, 0]]}),
        json!({"op": "assign_track", "position": 3, "channel": 1, "track": 7}),
        json!({"op": "set_transpose", "position": 3, "channel": 1, "value": -3}),
        json!({"op": "make_unique", "position": 4, "channel": 1}),
        json!({"op": "set_restart", "position": 2}),
        json!({"op": "set_instrument", "n": 3, "volume": 30, "envelope": {"a_frames": 5}}),
        json!({"op": "set_plist_row", "n": 3, "row": 0, "note": 12}),
        json!({"op": "insert_plist_row", "n": 3, "row": 0}),
        json!({"op": "enable_sweep", "n": 4, "kind": "square"}),
        json!({"op": "rename_instrument", "n": 3, "name": "Renamed"}),
        json!({"op": "set_name", "name": "A new name"}),
        json!({"op": "add_instrument", "name": "Added"}),
        json!({"op": "alloc_track", "append": true}),
        json!({"op": "batch", "ops": [
            {"op": "set_step", "position": 6, "channel": 0, "row": 1, "note": 9, "unique": true},
            {"op": "set_plist_row", "n": 2, "row": 0, "waveform": 1}
        ]}),
    ];
    for ops in live {
        assert!(follow(&mut d, &mut p, ops.clone()), "{ops} should be live");
        // undoing it is live too
        let c = d.undo().unwrap();
        assert!(d.live_edits().is_some(), "undo of {ops}: {c:?}");
        assert!(sync(&d, &mut p, &json!("undo")));
        d.redo().unwrap();
        assert!(sync(&d, &mut p, &json!("redo")));
    }

    let rebuild: Vec<Value> = vec![
        json!({"op": "set_track_length", "rows": 48, "force": true}),
        json!({"op": "compact_tracks"}),
        json!({"op": "insert_position", "at": 2}),
        json!({"op": "delete_position", "at": 2}),
        json!({"op": "move_position", "from": 0, "to": 4}),
        json!({"op": "set_speed_multiplier", "value": 2}),
        json!({"op": "set_subsongs", "starts": [3]}),
        json!({"op": "batch", "ops": [
            {"op": "set_step", "track": 3, "row": 0, "note": 9},
            {"op": "insert_position", "at": 0}
        ]}),
    ];
    for ops in rebuild {
        assert!(
            !follow(&mut d, &mut p, ops.clone()),
            "{ops} needs a new song"
        );
        let c = d.undo().unwrap();
        assert!(!c.is_live(), "undo of {ops}");
        assert!(!sync(&d, &mut p, &json!("undo")));
        d.redo().unwrap();
        sync(&d, &mut p, &json!("redo"));
    }

    // more than MAX_STEPS steps at once is a new song
    let ops: Vec<Value> = (0..40)
        .flat_map(|t| {
            (0..40).map(move |r| json!({"op": "set_step", "track": 10 + t % 40, "row": r, "note": 1 + (t % 40)}))
        })
        .collect();
    let r = d.apply(&json!({"op": "batch", "ops": ops})).unwrap();
    assert!(r["changed"]["tracks"].as_array().unwrap().len() > 1);
    assert!(d.last_changes().steps_overflow);
    assert!(d.live_edits().is_none());
}

#[test]
fn what_a_live_edit_hands_over() {
    let mut d = AhxDoc::new(SongFormat::Ahx);
    d.apply(&json!({"op": "add_instrument"})).unwrap();
    d.apply(&json!({"op": "set_step", "track": 1, "row": 0, "cell": [25, 1, 0, 0]}))
        .unwrap();
    // an edit that gives a blank channel a track of its own: a track is appended,
    // its steps travel whole, the position names it
    d.apply(&json!({"op": "set_step", "position": 0, "channel": 1, "row": 3, "note": 7, "unique": true})).unwrap();
    let live = d.live_edits().unwrap();
    assert_eq!(live.track_count, 3);
    assert_eq!(live.new_tracks.len(), 1);
    assert_eq!(live.new_tracks[0][3].note, 7);
    assert!(
        live.steps.is_empty(),
        "the appended track carries its own steps"
    );
    assert_eq!(live.positions.len(), 1);
    assert_eq!(
        (
            live.positions[0].position,
            live.positions[0].channel,
            live.positions[0].track
        ),
        (0, 1, 2)
    );
    let j = d.apply(&json!({"op": "live_edits"})).unwrap();
    assert_eq!(j["live"]["new_tracks"].as_array().unwrap().len(), 1);
    assert_eq!(j["live"]["positions"][0]["track"], 2);

    d.apply(&json!({"op": "set_step", "track": 1, "row": 2, "note": 9}))
        .unwrap();
    let live = d.live_edits().unwrap();
    assert!(live.new_tracks.is_empty());
    assert_eq!(live.steps.len(), 1);
    assert_eq!(
        (
            live.steps[0].track,
            live.steps[0].row,
            live.steps[0].step.note
        ),
        (1, 2, 9)
    );

    d.apply(&json!({"op": "set_instrument", "n": 1, "volume": 20}))
        .unwrap();
    assert_eq!(d.live_edits().unwrap().instruments, vec![1]);
    d.apply(&json!({"op": "rename_instrument", "n": 1, "name": "x"}))
        .unwrap();
    assert!(
        d.live_edits().unwrap().is_empty(),
        "a rename makes no sound"
    );
    d.apply(&json!({"op": "set_restart", "position": 0}))
        .unwrap();
    assert!(d.live_edits().unwrap().is_empty(), "nothing moved");
    d.apply(&json!({"op": "add_instrument"})).unwrap();
    let live = d.live_edits().unwrap();
    assert_eq!(
        (live.instrument_count, live.new_instruments.clone()),
        (2, vec![2])
    );
    assert!(
        live.instruments.is_empty(),
        "an appended instrument travels whole"
    );
    // undo of the add takes it off again
    d.undo().unwrap();
    let live = d.live_edits().unwrap();
    assert_eq!(live.instrument_count, 1);
    assert!(live.new_instruments.is_empty());
}

#[test]
fn a_player_that_follows_edits_hears_what_a_fresh_one_hears() {
    for name in ["karma.ahx", "doobrey_gubbins.hvl"] {
        let mut d = doc(name);
        let mut p = player(&d);
        render(&mut p, 9000);
        let positions = d.song().positions.len();
        let scripts: Vec<Value> = vec![
            json!({"op": "set_step", "track": d.song().positions[1].track[0], "row": 0, "cell": [30, 1, 0, 0]}),
            json!({"op": "set_transpose", "position": 1, "channel": 0, "value": 7}),
            json!({"op": "set_instrument", "n": 1, "volume": 20, "envelope": {"r_frames": 3, "a_volume": 40}}),
            json!({"op": "set_plist_row", "n": 1, "row": 0, "waveform": 3}),
            json!({"op": "set_restart", "position": positions - 1}),
            json!({"op": "add_instrument", "name": "Added"}),
            json!({"op": "set_step", "position": 1, "channel": 1, "row": 0, "cell": [20, d.song().instrument_nr as usize + 1, 0, 0], "unique": true}),
            json!({"op": "insert_position", "at": 1}),
            json!({"op": "set_speed_multiplier", "value": 1}),
        ];
        for ops in scripts {
            follow(&mut d, &mut p, ops.clone());
            render(&mut p, 3000);
        }
        // A seek replays the song from the top: a player that is level with the
        // document does it exactly as a fresh player does.
        for (pos, row) in [(0, 0), (1, 0), (1, 5), (2, 3)] {
            let mut fresh = AhxPlayer::from_song(d.song().clone(), RATE, 2).unwrap();
            fresh.play();
            assert_eq!(fresh.seek(pos, row), p.seek(pos, row));
            assert_eq!(
                render(&mut p, 6000),
                render(&mut fresh, 6000),
                "{name} at {pos}:{row}"
            );
        }
    }
}

#[test]
fn an_edit_to_a_row_not_yet_played_sounds_when_it_is_reached() {
    // A one-position song: channel 0 plays a note on row 0 and row 8 is empty.
    let mut d = AhxDoc::new(SongFormat::Ahx);
    d.apply(&json!({"op": "set_track_length", "rows": 16}))
        .unwrap();
    d.apply(&json!({"op": "add_instrument", "name": "Saw", "envelope": {"a_frames": 1, "a_volume": 64, "d_frames": 0, "d_volume": 64, "s_frames": 255, "r_frames": 4, "r_volume": 0}})).unwrap();
    d.apply(&json!({"op": "set_step", "track": 1, "row": 0, "cell": [25, 1, 15, 2]}))
        .unwrap();
    let mut p = player(&d);
    p.set_loop_for_test();
    let first = render(&mut p, 1000);
    assert!(first.iter().any(|&x| x != 0.0));
    // silence on channel 1 so far; give it a note on row 8, live
    let before_row_8 = p.play_row();
    assert!(before_row_8 < 8);
    d.apply(&json!({"op": "set_step", "position": 0, "channel": 1, "row": 8, "cell": [30, 1, 0, 0], "unique": true})).unwrap();
    assert!(sync(&d, &mut p, &json!("the edit")));
    // run to row 8 and a bit past: channel 1 is heard (it is panned right in
    // the default stereo separation, so the right channel gets its energy)
    p.set_levels(true);
    let mut seen = false;
    let mut out = [0.0f32; LEVELS];
    for _ in 0..200 {
        render(&mut p, 441);
        p.levels(&mut out);
        if p.play_row() >= 8 && out[1] > 0.0 {
            seen = true;
            break;
        }
    }
    assert!(seen, "channel 1 never sounded");
}

const LEVELS: usize = 17;

trait LoopForTest {
    fn set_loop_for_test(&mut self);
}

impl LoopForTest for AhxPlayer {
    fn set_loop_for_test(&mut self) {
        assert!(self.loop_positions(0, 0));
    }
}

#[test]
fn a_player_out_of_step_with_the_document_is_refused() {
    // A step on a track the player lacks.
    let mut small = player(&AhxDoc::new(SongFormat::Ahx));
    let mut big = doc("karma.ahx");
    big.apply(&json!({"op": "set_step", "track": 50, "row": 0, "note": 9}))
        .unwrap();
    let live = big.live_edits().unwrap();
    assert!(big
        .apply_live(&mut small, &live)
        .unwrap_err()
        .contains("track 50"));
    // A position naming a track the player lacks.
    let mut small = player(&AhxDoc::new(SongFormat::Ahx));
    big.apply(&json!({"op": "assign_track", "position": 0, "channel": 0, "track": 60}))
        .unwrap();
    let live = big.live_edits().unwrap();
    assert!(big
        .apply_live(&mut small, &live)
        .unwrap_err()
        .contains("position 0, channel 0"));
    // An instrument the player lacks.
    let mut small = player(&AhxDoc::new(SongFormat::Ahx));
    big.apply(&json!({"op": "set_instrument", "n": 5, "volume": 9}))
        .unwrap();
    let live = big.live_edits().unwrap();
    assert!(big
        .apply_live(&mut small, &live)
        .unwrap_err()
        .contains("no instrument 5"));
}
