//! The SID song document (`cubed_sid::edit::SidDoc`): every operation and its
//! refusals, shapes, undo and redo, live edits, reading and writing files.
//! (`edit_soak.rs` runs random operations over the same document.)

use cubed_sid::edit::{NewSong, SidDoc};
use cubed_sid::song::{SidSong, TableKind};
use cubed_sid::SidModel;
use serde_json::{json, Value};

fn doc() -> SidDoc {
    SidDoc::new()
}

fn ok(d: &mut SidDoc, op: Value) -> Value {
    d.apply(&op)
        .unwrap_or_else(|e| panic!("{op} was refused: {e}"))
}

/// The refusal of `op`, and the song unchanged by it.
fn refused(d: &mut SidDoc, op: Value) -> String {
    let before = d.song().clone();
    let rev = d.revision();
    let err = d
        .apply(&op)
        .expect_err(&format!("{op} should have been refused"));
    assert_eq!(*d.song(), before, "{op} was refused but changed the song");
    assert_eq!(
        d.revision(),
        rev,
        "{op} was refused but counted as a change"
    );
    err
}

fn rows(d: &SidDoc, pattern: usize) -> Vec<[u8; 4]> {
    d.song().patterns[pattern]
        .rows
        .iter()
        .map(|r| [r.note, r.instrument, r.command, r.param])
        .collect()
}

#[test]
fn a_new_song_is_goattrackers_own_start() {
    let d = doc();
    let s = d.song();
    s.validate().unwrap();
    assert_eq!(s.subsongs.len(), 1);
    assert_eq!(s.patterns.len(), 3);
    assert!(s.patterns.iter().all(|p| p.rows.len() == 64));
    assert_eq!(s.instruments.len(), 1);
    // Instrument 1 plays a plain pulse on rows of its own.
    assert_eq!(s.instruments[0].wave_ptr, 1);
    assert_eq!(s.tables.wave.len(), 2);
    assert_eq!(s.tables.pulse.len(), 2);
    assert_eq!(s.tempo, 6);
    // It saves as a .asid and a .sng and both load back as it.
    let back = SidDoc::from_bytes(&d.to_asid_bytes()).unwrap();
    assert_eq!(back.song(), s);
    let sng = SidDoc::from_bytes(&d.to_sng_bytes().unwrap()).unwrap();
    assert_eq!(sng.song(), s);
    let info = d.info();
    assert_eq!(info["model"], "6581");
    assert_eq!(info["note"]["midi_offset"], 11);
    assert_eq!(info["limits"]["patterns"], 208);
    assert_eq!(info["commands"].as_array().unwrap().len(), 16);
}

#[test]
fn new_songs_take_options_and_put_a_tempo_in_the_song() {
    let d = SidDoc::with_options(&NewSong {
        title: "Tune".into(),
        author: "Me".into(),
        model: SidModel::Sid8580,
        speed_multiplier: 2,
        pattern_rows: 32,
        tempo: Some(18),
    })
    .unwrap();
    let s = d.song();
    assert_eq!(
        (s.model, s.speed_multiplier, s.tempo),
        (SidModel::Sid8580, 2, 6)
    );
    assert_eq!(s.patterns[0].rows.len(), 32);
    // GoatTracker's way: an F command on row 0 of voice 1's pattern.
    assert_eq!(
        (s.patterns[0].rows[0].command, s.patterns[0].rows[0].param),
        (0xF, 18)
    );
    assert_eq!(s.instruments[0].gate_timer, 4);
    d.to_sng_bytes().unwrap();
    for (o, why) in [
        (
            NewSong {
                pattern_rows: 0,
                ..NewSong::default()
            },
            "1-128",
        ),
        (
            NewSong {
                pattern_rows: 129,
                ..NewSong::default()
            },
            "1-128",
        ),
        (
            NewSong {
                speed_multiplier: 17,
                ..NewSong::default()
            },
            "1-16",
        ),
        (
            NewSong {
                tempo: Some(2),
                ..NewSong::default()
            },
            "3-127",
        ),
        (
            NewSong {
                tempo: Some(2),
                speed_multiplier: 2,
                ..NewSong::default()
            },
            "3-127",
        ),
        (
            NewSong {
                tempo: Some(4),
                speed_multiplier: 2,
                ..NewSong::default()
            },
            "gate timer",
        ),
        (
            NewSong {
                title: "é€".into(),
                ..NewSong::default()
            },
            "latin-1",
        ),
    ] {
        let e = SidDoc::with_options(&o).unwrap_err();
        assert!(e.contains(why), "{e}");
    }
}

#[test]
fn cells_take_every_shape_and_refuse_what_is_not_a_row() {
    let mut d = doc();
    let r = ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 2, "cell": [49, 1, 0, 0]}),
    );
    assert_eq!(r["cell"], json!([49, 1, 0, 0]));
    assert_eq!(
        r["changed"]["patterns"][0],
        json!({"pattern": 0, "row0": 2, "row1": 2})
    );
    // Fields one at a time, a note as text, a MIDI number, a command as a hex digit.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 3, "note": "C#4"}),
    );
    assert_eq!(rows(&d, 0)[3], [50, 0, 0, 0]);
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 3, "midi": 60}),
    );
    assert_eq!(rows(&d, 0)[3][0], 49, "C-4 is MIDI 60 and note 49");
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 3, "command": "A", "param": 7}),
    );
    assert_eq!(rows(&d, 0)[3], [49, 0, 10, 7]);
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 4, "note": "off"}),
    );
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 5, "note": "on"}),
    );
    assert_eq!((rows(&d, 0)[4][0], rows(&d, 0)[5][0]), (126, 127));
    // Setting what is there changes nothing and records nothing.
    let steps = d.info()["undo_steps"].as_u64();
    let r = ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 5, "note": 127}),
    );
    assert_eq!(r["changed"], json!({}));
    assert_eq!(d.info()["undo_steps"].as_u64(), steps);

    for (op, why) in [
        (
            json!({"op": "set_cell", "pattern": 9, "row": 0, "note": 49}),
            "pattern 9 does not exist",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 64, "note": 49}),
            "no row 64",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "note": 94}),
            "not a note",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "note": 100}),
            "not a note",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "note": "H-4"}),
            "not a note",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "note": "C-8"}),
            "not a note",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "midi": 11}),
            "outside C-0..G#7",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "midi": 105}),
            "outside C-0..G#7",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "instrument": 2}),
            "instrument 2 does not exist",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "command": 16}),
            "0-F",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "command": "G"}),
            "command digit",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "param": 256}),
            "0-255",
        ),
        (
            json!({"op": "set_cell", "pattern": 0, "row": 0, "cell": [1, 2, 3]}),
            "[note, instrument, command, param]",
        ),
        (
            json!({"op": "set_cell", "row": 0, "note": 1}),
            "'pattern' is needed",
        ),
    ] {
        let e = refused(&mut d, op.clone());
        assert!(e.contains(why), "{op}: {e}");
    }
}

#[test]
fn ranges_clear_shift_and_transpose() {
    let mut d = doc();
    for (r, n) in [(0, 49), (1, 51), (2, 53), (3, 55)] {
        ok(
            &mut d,
            json!({"op": "set_cell", "pattern": 0, "row": r, "cell": [n, 1, 4, r + 1]}),
        );
    }
    // Clear one column of a range.
    ok(
        &mut d,
        json!({"op": "clear_range", "pattern": 0, "row0": 1, "row1": 2, "columns": ["note"]}),
    );
    assert_eq!(
        &rows(&d, 0)[..4],
        &[[49, 1, 4, 1], [0, 1, 4, 2], [0, 1, 4, 3], [55, 1, 4, 4]]
    );
    ok(
        &mut d,
        json!({"op": "clear_range", "pattern": 0, "row0": 1, "row1": 2, "columns": ["command"]}),
    );
    assert_eq!(&rows(&d, 0)[1..3], &[[0, 1, 0, 0], [0, 1, 0, 0]]);
    ok(
        &mut d,
        json!({"op": "clear_range", "pattern": 0, "row0": 0, "row1": 3}),
    );
    assert!(rows(&d, 0).iter().all(|r| *r == [0, 0, 0, 0]));
    assert!(refused(
        &mut d,
        json!({"op": "clear_range", "pattern": 0, "row0": 3, "row1": 64})
    )
    .contains("not inside"));
    assert!(refused(
        &mut d,
        json!({"op": "clear_range", "pattern": 0, "row0": 3, "row1": 2})
    )
    .contains("not inside"));
    assert!(refused(
        &mut d,
        json!({"op": "clear_range", "pattern": 0, "row0": 0, "row1": 1, "columns": ["volume"]})
    )
    .contains("not a column"));

    // Insert and delete rows keep the pattern's length by default.
    for r in 0..4 {
        ok(
            &mut d,
            json!({"op": "set_cell", "pattern": 0, "row": r, "note": 49 + r}),
        );
    }
    ok(
        &mut d,
        json!({"op": "insert_row", "pattern": 0, "row": 1, "count": 2}),
    );
    let notes: Vec<u8> = rows(&d, 0).iter().map(|r| r[0]).collect();
    assert_eq!(&notes[..6], &[49, 0, 0, 50, 51, 52]);
    assert_eq!(notes.len(), 64);
    ok(
        &mut d,
        json!({"op": "delete_row", "pattern": 0, "row": 1, "count": 2}),
    );
    let notes: Vec<u8> = rows(&d, 0).iter().map(|r| r[0]).collect();
    assert_eq!(&notes[..5], &[49, 50, 51, 52, 0]);
    // Rows pushed off the end are gone, and a huge count is cut to the pattern.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 63, "note": 60}),
    );
    ok(
        &mut d,
        json!({"op": "insert_row", "pattern": 0, "row": 0, "count": 1}),
    );
    assert_eq!(rows(&d, 0)[63][0], 0);
    ok(
        &mut d,
        json!({"op": "delete_row", "pattern": 0, "row": 60, "count": 100}),
    );
    // grow and shrink change the length.
    ok(
        &mut d,
        json!({"op": "insert_row", "pattern": 0, "row": 64, "count": 3, "grow": true}),
    );
    assert_eq!(d.song().patterns[0].rows.len(), 67);
    ok(
        &mut d,
        json!({"op": "delete_row", "pattern": 0, "row": 0, "count": 4, "shrink": true}),
    );
    assert_eq!(d.song().patterns[0].rows.len(), 63);
    assert!(refused(
        &mut d,
        json!({"op": "insert_row", "pattern": 0, "row": 63, "count": 70, "grow": true})
    )
    .contains("most is 128"));
    assert!(refused(
        &mut d,
        json!({"op": "insert_row", "pattern": 0, "row": 63, "count": 1})
    )
    .contains("no row 63"));
    assert!(refused(
        &mut d,
        json!({"op": "delete_row", "pattern": 0, "row": 0, "count": 63, "shrink": true})
    )
    .contains("at least one row"));
    assert!(refused(
        &mut d,
        json!({"op": "insert_row", "pattern": 0, "row": 0, "count": 0})
    )
    .contains("1 or more"));

    // Transpose: notes only, within C-0..G#7, optionally one instrument's.
    let mut d = doc();
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_cell", "pattern": 1, "row": 0, "cell": [1, 1, 0, 0]},
            {"op": "set_cell", "pattern": 1, "row": 1, "cell": [93, 0, 0, 0]},
            {"op": "set_cell", "pattern": 1, "row": 2, "cell": [126, 0, 0, 0]},
            {"op": "set_cell", "pattern": 1, "row": 3, "cell": [40, 0, 0, 0]},
        ]}),
    );
    let e = refused(
        &mut d,
        json!({"op": "transpose", "pattern": 1, "row0": 0, "row1": 3, "semitones": 1}),
    );
    assert!(e.contains("row 1 would leave C-0..G#7"), "{e}");
    let r = ok(
        &mut d,
        json!({"op": "transpose", "pattern": 1, "row0": 0, "row1": 3, "semitones": -2, "clamp": true}),
    );
    assert_eq!(r["count"], 3);
    assert_eq!(
        rows(&d, 1).iter().map(|r| r[0]).collect::<Vec<_>>()[..4],
        [1, 91, 126, 38]
    );
    let r = ok(
        &mut d,
        json!({"op": "transpose", "pattern": 1, "row0": 0, "row1": 3, "semitones": 12, "instrument": 1}),
    );
    assert_eq!(r["count"], 1);
    assert_eq!(rows(&d, 1)[0][0], 13);
}

#[test]
fn clips_copy_and_paste() {
    let mut d = doc();
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_cell", "pattern": 0, "row": 0, "cell": [49, 1, 4, 9]},
            {"op": "set_cell", "pattern": 0, "row": 1, "cell": [0, 0, 0xA, 3]},
            {"op": "set_cell", "pattern": 0, "row": 2, "cell": [126, 0, 0, 0]},
        ]}),
    );
    let clip = ok(
        &mut d,
        json!({"op": "copy_range", "pattern": 0, "row0": 0, "row1": 2}),
    );
    assert_eq!(clip["rows"], 3);
    assert_eq!(
        clip["cells"],
        json!([[49, 1, 4, 9], [0, 0, 10, 3], [126, 0, 0, 0]])
    );
    let r = ok(
        &mut d,
        json!({"op": "paste", "pattern": 1, "row": 10, "cells": clip["cells"]}),
    );
    assert_eq!(
        (r["rows"].clone(), r["clipped"].clone()),
        (json!(3), json!(false))
    );
    assert_eq!(
        &rows(&d, 1)[10..13],
        &[[49, 1, 4, 9], [0, 0, 10, 3], [126, 0, 0, 0]]
    );
    // Cut at the end of the pattern.
    let r = ok(
        &mut d,
        json!({"op": "paste", "pattern": 1, "row": 62, "cells": clip["cells"]}),
    );
    assert_eq!(
        (r["rows"].clone(), r["clipped"].clone()),
        (json!(2), json!(true))
    );
    // Mix fills only what is empty.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 2, "row": 0, "cell": [51, 0, 0, 0]}),
    );
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 2, "row": 1, "cell": [0, 1, 7, 7]}),
    );
    ok(
        &mut d,
        json!({"op": "paste", "pattern": 2, "row": 0, "cells": clip["cells"], "mode": "mix"}),
    );
    assert_eq!(
        &rows(&d, 2)[..3],
        &[[51, 1, 4, 9], [0, 1, 7, 7], [126, 0, 0, 0]]
    );
    // Refusals: an instrument the song lacks, an empty clip, a place past the end, a bad mode.
    for (op, why) in [
        (
            json!({"op": "paste", "pattern": 1, "row": 0, "cells": [[1, 5, 0, 0]]}),
            "instrument 5 does not exist",
        ),
        (
            json!({"op": "paste", "pattern": 1, "row": 0, "cells": []}),
            "nothing to paste",
        ),
        (
            json!({"op": "paste", "pattern": 1, "row": 64, "cells": [[1, 0, 0, 0]]}),
            "no row 64",
        ),
        (
            json!({"op": "paste", "pattern": 1, "row": 0, "cells": [[1, 0, 0, 0]], "mode": "add"}),
            "not a paste mode",
        ),
        (
            json!({"op": "paste", "pattern": 1, "row": 0}),
            "needs 'cells'",
        ),
        (
            json!({"op": "copy_range", "pattern": 0, "row0": 0, "row1": 64}),
            "not inside",
        ),
    ] {
        let e = refused(&mut d, op.clone());
        assert!(e.contains(why), "{op}: {e}");
    }
}

#[test]
fn patterns_come_and_go_with_their_orderlist_entries() {
    let mut d = doc();
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 1, "row": 0, "cell": [49, 1, 0, 0]}),
    );
    let r = ok(&mut d, json!({"op": "new_pattern", "rows": 16}));
    assert_eq!(r["pattern"], 3);
    assert_eq!(r["changed"]["layout"], true);
    let r = ok(&mut d, json!({"op": "duplicate_pattern", "pattern": 1}));
    assert_eq!(r["pattern"], 4);
    assert_eq!(rows(&d, 4)[0], [49, 1, 0, 0]);
    // Lengths: shortening over data needs force.
    ok(
        &mut d,
        json!({"op": "set_pattern_rows", "pattern": 4, "rows": 100}),
    );
    assert_eq!(d.song().patterns[4].rows.len(), 100);
    ok(
        &mut d,
        json!({"op": "set_pattern_rows", "pattern": 4, "rows": 1}),
    );
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 4, "row": 0, "note": 0}),
    );
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 3, "row": 5, "note": 50}),
    );
    let e = refused(
        &mut d,
        json!({"op": "set_pattern_rows", "pattern": 3, "rows": 4}),
    );
    assert!(e.contains("would drop row 5") && e.contains("force"), "{e}");
    ok(
        &mut d,
        json!({"op": "set_pattern_rows", "pattern": 3, "rows": 4, "force": true}),
    );
    for (rows, why) in [(0, "1-128"), (129, "1-128")] {
        let e = refused(
            &mut d,
            json!({"op": "set_pattern_rows", "pattern": 3, "rows": rows}),
        );
        assert!(e.contains(why), "{e}");
    }
    // A pattern an orderlist plays cannot go; unused ones can, and the later ones renumber.
    ok(
        &mut d,
        json!({"op": "insert_order", "voice": 1, "index": 1, "pattern": 4, "repeat": 3}),
    );
    let e = refused(&mut d, json!({"op": "delete_pattern", "pattern": 4}));
    assert!(
        e.contains("played by 1 orderlist entry (the first in subsong 0 voice 1)"),
        "{e}"
    );
    ok(&mut d, json!({"op": "delete_pattern", "pattern": 3}));
    assert_eq!(d.song().patterns.len(), 4);
    assert_eq!(
        d.song().subsongs[0].orderlists[1].entries[1].pattern,
        3,
        "entry follows its pattern down"
    );
    // force removes the entries that play it.
    ok(
        &mut d,
        json!({"op": "delete_pattern", "pattern": 3, "force": true}),
    );
    assert_eq!(d.song().subsongs[0].orderlists[1].entries.len(), 1);
    // ... but never empties an orderlist, and the last pattern stays.
    let e = refused(
        &mut d,
        json!({"op": "delete_pattern", "pattern": 0, "force": true}),
    );
    assert!(e.contains("no orderlist entry"), "{e}");
    let mut e1 = doc();
    ok(
        &mut e1,
        json!({"op": "batch", "ops": [
            {"op": "set_order", "voice": 1, "index": 0, "pattern": 0},
            {"op": "set_order", "voice": 2, "index": 0, "pattern": 0},
            {"op": "delete_pattern", "pattern": 1}, {"op": "delete_pattern", "pattern": 1},
        ]}),
    );
    assert_eq!(e1.song().patterns.len(), 1);
    assert!(refused(
        &mut e1,
        json!({"op": "delete_pattern", "pattern": 0, "force": true})
    )
    .contains("at least one pattern"));
    // 208 patterns at most.
    let mut full = doc();
    for _ in 0..205 {
        ok(&mut full, json!({"op": "new_pattern", "rows": 1}));
    }
    assert_eq!(full.song().patterns.len(), 208);
    assert!(refused(&mut full, json!({"op": "new_pattern"})).contains("208"));
    assert!(refused(&mut full, json!({"op": "duplicate_pattern", "pattern": 0})).contains("208"));
    full.song().validate().unwrap();
}

#[test]
fn orderlists_edit_per_voice_and_keep_their_restart() {
    let mut d = doc();
    ok(&mut d, json!({"op": "new_pattern"}));
    ok(&mut d, json!({"op": "new_pattern"}));
    // Insert: with a transpose and a repeat; the restart stays on its entry.
    ok(
        &mut d,
        json!({"op": "insert_order", "voice": 0, "index": 1, "pattern": 3, "transpose": -5, "repeat": 4}),
    );
    ok(
        &mut d,
        json!({"op": "insert_order", "voice": 0, "index": 2, "pattern": 4}),
    );
    ok(
        &mut d,
        json!({"op": "set_restart", "voice": 0, "restart": 1}),
    );
    let l = ok(&mut d, json!({"op": "orderlist", "voice": 0}));
    assert_eq!(l["entries"], json!([[0, 0, 1], [3, -5, 4], [4, 0, 1]]));
    assert_eq!(l["restart"], 1);
    assert_eq!(l["rows"], json!([64, 256, 64]));
    ok(
        &mut d,
        json!({"op": "insert_order", "voice": 0, "index": 0, "pattern": 4}),
    );
    assert_eq!(
        d.song().subsongs[0].orderlists[0].restart,
        2,
        "still on the same entry"
    );
    ok(
        &mut d,
        json!({"op": "delete_order", "voice": 0, "index": 0}),
    );
    assert_eq!(d.song().subsongs[0].orderlists[0].restart, 1);
    // Set any subset of fields.
    ok(
        &mut d,
        json!({"op": "set_order", "voice": 0, "index": 1, "transpose": 7}),
    );
    ok(
        &mut d,
        json!({"op": "set_order", "voice": 0, "index": 1, "repeat": 2, "pattern": 1}),
    );
    assert_eq!(
        d.song().subsongs[0].orderlists[0].entries[1],
        cubed_sid::song::OrderEntry {
            pattern: 1,
            transpose: 7,
            repeat: 2
        }
    );
    // Move: the restart follows its entry.
    ok(
        &mut d,
        json!({"op": "move_order", "voice": 0, "from": 1, "to": 2}),
    );
    assert_eq!(d.song().subsongs[0].orderlists[0].restart, 2);
    ok(
        &mut d,
        json!({"op": "move_order", "voice": 0, "from": 0, "to": 1}),
    );
    assert_eq!(d.song().subsongs[0].orderlists[0].restart, 2);
    ok(
        &mut d,
        json!({"op": "move_order", "voice": 0, "from": 2, "to": 0}),
    );
    assert_eq!(d.song().subsongs[0].orderlists[0].restart, 0);
    // All voices at once, with a pattern each.
    ok(
        &mut d,
        json!({"op": "insert_order", "voice": "all", "index": 1, "pattern": [0, 1, 2]}),
    );
    for v in 0..3 {
        assert_eq!(
            d.song().subsongs[0].orderlists[v].entries[1].pattern as usize,
            v
        );
    }
    // The other subsong keeps its own.
    ok(&mut d, json!({"op": "add_subsong"}));
    assert_eq!(d.song().subsongs[1].orderlists[0].entries.len(), 1);
    // Refusals.
    for (op, why) in [
        (
            json!({"op": "insert_order", "voice": 0, "index": 99, "pattern": 0}),
            "no place 99",
        ),
        (
            json!({"op": "insert_order", "voice": 0, "index": 0, "pattern": 99}),
            "pattern 99 does not exist",
        ),
        (
            json!({"op": "insert_order", "voice": 0, "index": 0}),
            "needs a 'pattern'",
        ),
        (
            json!({"op": "insert_order", "voice": 0, "index": 0, "pattern": 0, "transpose": 64}),
            "transpose",
        ),
        (
            json!({"op": "insert_order", "voice": 0, "index": 0, "pattern": 0, "transpose": -65}),
            "transpose",
        ),
        (
            json!({"op": "insert_order", "voice": 0, "index": 0, "pattern": 0, "repeat": 17}),
            "1-16",
        ),
        (
            json!({"op": "insert_order", "voice": 0, "index": 0, "pattern": 0, "repeat": 0}),
            "1-16",
        ),
        (
            json!({"op": "insert_order", "voice": 3, "index": 0, "pattern": 0}),
            "0-2",
        ),
        (
            json!({"op": "insert_order", "index": 0, "pattern": 0}),
            "'voice' is needed",
        ),
        (
            json!({"op": "insert_order", "voice": 0, "subsong": 5, "index": 0, "pattern": 0}),
            "subsong 5 does not exist",
        ),
        (
            json!({"op": "insert_order", "voice": "all", "index": 0, "pattern": [0, 1]}),
            "needs 3 numbers",
        ),
        (
            json!({"op": "set_order", "voice": 0, "index": 99, "pattern": 0}),
            "no entry 99",
        ),
        (
            json!({"op": "set_restart", "voice": 0, "restart": 99}),
            "cannot restart at 99",
        ),
        (
            json!({"op": "move_order", "voice": 0, "from": 0, "to": 99}),
            "must be inside",
        ),
    ] {
        let e = refused(&mut d, op.clone());
        assert!(e.contains(why), "{op}: {e}");
    }
    // A list keeps one entry; and at most 254.
    let mut one = doc();
    assert!(refused(
        &mut one,
        json!({"op": "delete_order", "voice": 0, "index": 0})
    )
    .contains("at least one entry"));
    for _ in 0..253 {
        ok(
            &mut one,
            json!({"op": "insert_order", "voice": 0, "index": 0, "pattern": 0}),
        );
    }
    assert!(refused(
        &mut one,
        json!({"op": "insert_order", "voice": 0, "index": 0, "pattern": 0})
    )
    .contains("at most 254"));
    one.song().validate().unwrap();
}

#[test]
fn subsongs_add_copy_remove_and_select() {
    let mut d = doc();
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "note": 49}),
    );
    let r = ok(&mut d, json!({"op": "add_subsong", "rows": 8}));
    assert_eq!(r["subsong"], 1);
    assert_eq!(d.song().patterns.len(), 6);
    assert_eq!(d.song().patterns[3].rows.len(), 8);
    let r = ok(&mut d, json!({"op": "add_subsong", "copy_of": 0}));
    assert_eq!(r["subsong"], 2);
    assert_eq!(d.song().subsongs[2], d.song().subsongs[0]);
    assert_eq!(d.song().patterns.len(), 6, "a copy shares the patterns");
    assert_eq!(
        ok(&mut d, json!({"op": "select_subsong", "subsong": 2}))["subsong"],
        2
    );
    // Orderlist operations default to the subsong shown.
    ok(
        &mut d,
        json!({"op": "insert_order", "voice": 0, "index": 0, "pattern": 1}),
    );
    assert_eq!(d.song().subsongs[2].orderlists[0].entries.len(), 2);
    assert_eq!(d.song().subsongs[0].orderlists[0].entries.len(), 1);
    assert_eq!(
        ok(&mut d, json!({"op": "orderlist", "voice": 0}))["subsong"],
        2
    );
    // Removing a subsong before the one shown keeps the same one shown; the shown one going clamps.
    ok(&mut d, json!({"op": "remove_subsong", "subsong": 0}));
    assert_eq!(d.subsong(), 1);
    ok(&mut d, json!({"op": "remove_subsong", "subsong": 1}));
    assert_eq!(d.subsong(), 0);
    assert!(
        refused(&mut d, json!({"op": "remove_subsong", "subsong": 0}))
            .contains("at least one subsong")
    );
    assert!(
        refused(&mut d, json!({"op": "remove_subsong", "subsong": 4})).contains("does not exist")
    );
    assert!(
        refused(&mut d, json!({"op": "select_subsong", "subsong": 4})).contains("does not exist")
    );
    assert!(refused(&mut d, json!({"op": "add_subsong", "copy_of": 9})).contains("does not exist"));
    assert!(refused(&mut d, json!({"op": "add_subsong", "rows": 0})).contains("1-128"));
    // 32 subsongs at most.
    for _ in 0..31 {
        ok(&mut d, json!({"op": "add_subsong", "copy_of": 0}));
    }
    assert_eq!(d.song().subsongs.len(), 32);
    assert!(refused(&mut d, json!({"op": "add_subsong", "copy_of": 0})).contains("32"));
    d.song().validate().unwrap();
}

#[test]
fn instruments_edit_by_field_and_refuse_what_is_out_of_range() {
    let mut d = doc();
    let i = ok(&mut d, json!({"op": "instrument", "instrument": 1}));
    assert_eq!(i["decay"], 9);
    assert_eq!(i["first_wave"], 9);
    assert_eq!(i["gate_timer"], 2);
    assert_eq!(
        (i["wave_ptr"].clone(), i["pulse_ptr"].clone()),
        (json!(1), json!(1))
    );
    ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 1, "name": "Lead", "attack": 2, "sustain": 12, "release": 5,
        "hard_restart": false, "no_gate_off": true, "gate_timer": 63, "vibrato_delay": 20, "first_wave": 0x41, "speed_ptr": 0}),
    );
    let i = ok(&mut d, json!({"op": "instrument", "instrument": 1}));
    assert_eq!(i["name"], "Lead");
    assert_eq!(
        (i["ad"].clone(), i["sr"].clone()),
        (json!(0x29), json!(0xC5))
    );
    assert_eq!(
        (i["hard_restart"].clone(), i["no_gate_off"].clone()),
        (json!(false), json!(true))
    );
    ok(
        &mut d,
        json!({"op": "rename_instrument", "instrument": 1, "name": "Pad"}),
    );
    for (op, why) in [
        (
            json!({"op": "set_instrument", "instrument": 2, "name": "x"}),
            "instrument 2 does not exist",
        ),
        (
            json!({"op": "set_instrument", "instrument": 0, "name": "x"}),
            "does not exist",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "attack": 16}),
            "attack is 0-15",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "release": 16}),
            "release is 0-15",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "gate_timer": 64}),
            "0-63",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "name": "0123456789abcdefg"}),
            "at most 16",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "name": "ÿ€"}),
            "latin-1",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "wave_ptr": 3}),
            "wave table has 2 rows",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "filter_ptr": 1}),
            "filter table has 0 rows",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "decay": 300}),
            "0-255",
        ),
        (
            json!({"op": "rename_instrument", "instrument": 1}),
            "'name' is needed",
        ),
    ] {
        let e = refused(&mut d, op.clone());
        assert!(e.contains(why), "{op}: {e}");
    }
    // add: with rows of its own, or none.
    let r = ok(&mut d, json!({"op": "add_instrument", "name": "Bass"}));
    assert_eq!(r["instrument"], 2);
    assert_eq!(d.song().tables.wave.len(), 4);
    assert_eq!(
        (
            d.song().instruments[1].wave_ptr,
            d.song().instruments[1].pulse_ptr
        ),
        (3, 3)
    );
    ok(&mut d, json!({"op": "add_instrument", "tables": false}));
    assert_eq!(d.song().instruments[2].wave_ptr, 0);
    assert_eq!(d.song().tables.wave.len(), 4);
    // clear: back to GoatTracker's new instrument, no pointers, its rows stay.
    ok(&mut d, json!({"op": "clear_instrument", "instrument": 2}));
    assert_eq!(d.song().instruments[1].name, b"");
    assert_eq!(d.song().instruments[1].wave_ptr, 0);
    assert_eq!(d.song().tables.wave.len(), 4);
    // The 63 instruments of GoatTracker.
    for _ in 0..60 {
        ok(&mut d, json!({"op": "add_instrument", "tables": false}));
    }
    assert_eq!(d.song().instruments.len(), 63);
    assert!(refused(&mut d, json!({"op": "add_instrument"})).contains("63"));
    assert!(refused(&mut d, json!({"op": "clone_instrument", "instrument": 1})).contains("63"));
    d.song().validate().unwrap();
}

#[test]
fn clone_copies_the_instruments_table_rows() {
    let mut d = doc();
    // Instrument 1's wave program: arpeggio rows with a jump inside, then a table row it does not own.
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_table_row", "table": "wave", "row": 0, "left": 0x21, "right": 0},
            {"op": "set_table_row", "table": "wave", "row": 1, "left": 0x21, "right": 4},
            {"op": "insert_table_row", "table": "wave", "row": 2, "left": 0xFF, "right": 1},
            {"op": "add_instrument", "name": "other"},
        ]}),
    );
    let wave_before = d.song().tables.wave.clone();
    assert_eq!(wave_before.len(), 5);
    let r = ok(&mut d, json!({"op": "clone_instrument", "instrument": 1}));
    assert_eq!(r["instrument"], 3);
    let copy = &d.song().instruments[2];
    assert_eq!(copy.wave_ptr, 6, "the copy starts at its own rows");
    let t = &d.song().tables.wave;
    assert_eq!(t.len(), 8);
    assert_eq!(
        &t[5..],
        &[
            cubed_sid::song::TableRow {
                left: 0x21,
                right: 0
            },
            cubed_sid::song::TableRow {
                left: 0x21,
                right: 4
            },
            cubed_sid::song::TableRow {
                left: 0xFF,
                right: 6
            },
        ],
        "its jump moved with it"
    );
    assert_eq!(&t[..5], &wave_before[..], "the original is untouched");
    // Over an existing instrument.
    let r = ok(
        &mut d,
        json!({"op": "clone_instrument", "instrument": 2, "to": 1}),
    );
    assert_eq!(r["instrument"], 1);
    assert_eq!(d.song().instruments[0].name, d.song().instruments[1].name);
    assert!(refused(
        &mut d,
        json!({"op": "clone_instrument", "instrument": 1, "to": 9})
    )
    .contains("does not exist"));
    // No room: a table of 255 rows.
    let mut f2 = doc();
    ok(
        &mut f2,
        json!({"op": "batch", "ops": (0..253).map(|_| json!({"op": "insert_table_row", "table": "wave", "row": 2})).collect::<Vec<_>>()}),
    );
    let e = refused(&mut f2, json!({"op": "clone_instrument", "instrument": 1}));
    assert!(e.contains("wave table has no room"), "{e}");
}

#[test]
fn deleting_an_instrument_renumbers_the_rows_that_name_later_ones() {
    let mut d = doc();
    ok(&mut d, json!({"op": "add_instrument"}));
    ok(&mut d, json!({"op": "add_instrument"}));
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_cell", "pattern": 0, "row": 0, "cell": [49, 1, 0, 0]},
            {"op": "set_cell", "pattern": 0, "row": 1, "cell": [49, 2, 0, 0]},
            {"op": "set_cell", "pattern": 1, "row": 0, "cell": [49, 3, 0, 0]},
        ]}),
    );
    let e = refused(&mut d, json!({"op": "delete_instrument", "instrument": 2}));
    assert!(
        e.contains("named on 1 row (the first: pattern 0 row 1)"),
        "{e}"
    );
    ok(
        &mut d,
        json!({"op": "delete_instrument", "instrument": 2, "clear_uses": true}),
    );
    assert_eq!(d.song().instruments.len(), 2);
    assert_eq!(rows(&d, 0)[..2], [[49, 1, 0, 0], [49, 0, 0, 0]]);
    assert_eq!(rows(&d, 1)[0], [49, 2, 0, 0]);
    // Its table rows stay: tables are shared.
    assert_eq!(d.song().tables.wave.len(), 6);
    assert!(
        refused(&mut d, json!({"op": "delete_instrument", "instrument": 3}))
            .contains("does not exist")
    );
    // The undo is one step and puts it all back.
    d.undo().unwrap();
    assert_eq!(d.song().instruments.len(), 3);
    assert_eq!(rows(&d, 1)[0], [49, 3, 0, 0]);
}

/// A song with every kind of table reference, for the pointer semantics.
fn linked() -> SidDoc {
    let mut d = doc();
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "add_instrument", "name": "b"},
            {"op": "set_instrument", "instrument": 1, "speed_ptr": 0},
        ]}),
    );
    // wave: 1-2 instrument 1 (loop), 3-4 instrument 2; row 5: F9 03 (pulse pointer 3), row 6: F1 02 (slide speed row 2).
    // Make the tables long enough: pulse 1-4 (two instruments' rows) then 5, 6; speed 1-3.
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_table_row", "table": "wave", "row": 4, "left": 0xF9, "right": 3},
            {"op": "set_table_row", "table": "wave", "row": 5, "left": 0xF1, "right": 2},
            {"op": "set_table_row", "table": "wave", "row": 6, "left": 0xFF, "right": 5},
            {"op": "set_table_row", "table": "pulse", "row": 4, "left": 0x20, "right": 1},
            {"op": "set_table_row", "table": "pulse", "row": 5, "left": 0xFF, "right": 5},
            {"op": "set_table_row", "table": "speed", "row": 0, "left": 0x04, "right": 0x20},
            {"op": "set_table_row", "table": "speed", "row": 1, "left": 0xFF, "right": 0x10},
            {"op": "set_table_row", "table": "speed", "row": 2, "left": 0x02, "right": 0x05},
            {"op": "set_instrument", "instrument": 2, "speed_ptr": 3},
            {"op": "set_cell", "pattern": 0, "row": 0, "cell": [49, 1, 8, 3]},
            {"op": "set_cell", "pattern": 0, "row": 1, "cell": [0, 0, 9, 3]},
            {"op": "set_cell", "pattern": 0, "row": 2, "cell": [0, 0, 4, 2]},
            {"op": "set_cell", "pattern": 0, "row": 3, "cell": [0, 0, 0xE, 3]},
            {"op": "set_cell", "pattern": 0, "row": 4, "cell": [0, 0, 5, 3]},
            {"op": "set_cell", "pattern": 0, "row": 5, "cell": [0, 0, 8, 0]},
        ]}),
    );
    d
}

#[test]
fn inserting_a_table_row_moves_every_reference_to_later_rows_with_their_data() {
    let mut d = linked();
    assert_eq!(d.song().tables.wave.len(), 7);
    ok(
        &mut d,
        json!({"op": "insert_table_row", "table": "wave", "row": 2, "left": 0x11, "right": 0}),
    );
    let s = d.song();
    assert_eq!(s.tables.wave[2].left, 0x11);
    // Instrument 2 started at wave row 3 (index 2): its data is now at 4.
    assert_eq!(
        (s.instruments[0].wave_ptr, s.instruments[1].wave_ptr),
        (1, 4)
    );
    // The jump (FF 05 at the end of the table, now row 8) lands on the same data as before, now row 6.
    assert_eq!(
        s.tables.wave[7],
        cubed_sid::song::TableRow {
            left: 0xFF,
            right: 6
        }
    );
    // Pattern commands 8 (wave pointer) follow; others are untouched.
    assert_eq!(rows(&d, 0)[0], [49, 1, 8, 4]);
    assert_eq!(rows(&d, 0)[1][3], 3, "a pulse pointer is not a wave row");
    assert_eq!(rows(&d, 0)[5][3], 0, "a zero parameter is no pointer");

    // The pulse table: instrument pointers, an F9 wave-table command, the 9 command and a jump.
    let mut d = linked();
    ok(
        &mut d,
        json!({"op": "insert_table_row", "table": "pulse", "row": 0}),
    );
    let s = d.song();
    assert_eq!(
        (s.instruments[0].pulse_ptr, s.instruments[1].pulse_ptr),
        (2, 4)
    );
    assert_eq!(
        s.tables.wave[4],
        cubed_sid::song::TableRow {
            left: 0xF9,
            right: 4
        },
        "F9 names a pulse row"
    );
    assert_eq!(rows(&d, 0)[1], [0, 0, 9, 4]);
    assert_eq!(
        s.tables.pulse[6],
        cubed_sid::song::TableRow {
            left: 0xFF,
            right: 6
        }
    );

    // The speed table: instrument speed pointers, commands 1-4 and E, F1 in the wave table; its FF row is data, not a jump.
    let mut d = linked();
    ok(
        &mut d,
        json!({"op": "insert_table_row", "table": "speed", "row": 1, "left": 9, "right": 9}),
    );
    let s = d.song();
    assert_eq!(s.instruments[1].speed_ptr, 4);
    assert_eq!(
        s.tables.wave[5],
        cubed_sid::song::TableRow {
            left: 0xF1,
            right: 3
        }
    );
    assert_eq!(rows(&d, 0)[2], [0, 0, 4, 3]);
    assert_eq!(rows(&d, 0)[3], [0, 0, 0xE, 4]);
    assert_eq!(
        s.tables.speed[2],
        cubed_sid::song::TableRow {
            left: 0xFF,
            right: 0x10
        },
        "FF in the speed table is a speed, left alone"
    );
    // Refusals.
    assert!(refused(
        &mut d,
        json!({"op": "insert_table_row", "table": "speed", "row": 9})
    )
    .contains("no place 9"));
    assert!(refused(
        &mut d,
        json!({"op": "insert_table_row", "table": "sound", "row": 0})
    )
    .contains("not a table"));
    // The undo is one step.
    let steps = d.info()["undo_steps"].as_u64().unwrap();
    d.undo().unwrap();
    assert_eq!(d.info()["undo_steps"].as_u64().unwrap(), steps - 1);
    assert_eq!(d.song().instruments[1].speed_ptr, 3);
}

#[test]
fn deleting_a_table_row_moves_references_up_and_the_deleted_one_to_its_successor() {
    let mut d = linked();
    // Delete wave row index 1 (pointer 2, the second step of instrument 1's rows).
    ok(
        &mut d,
        json!({"op": "delete_table_row", "table": "wave", "row": 1}),
    );
    let s = d.song();
    assert_eq!(s.tables.wave.len(), 6);
    assert_eq!(
        (s.instruments[0].wave_ptr, s.instruments[1].wave_ptr),
        (1, 2)
    );
    assert_eq!(
        s.tables.wave[5],
        cubed_sid::song::TableRow {
            left: 0xFF,
            right: 4
        }
    );
    assert_eq!(rows(&d, 0)[0], [49, 1, 8, 2]);
    // A reference to the deleted row now names the row that took its place.
    let mut d = linked();
    ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 1, "wave_ptr": 2}),
    );
    ok(
        &mut d,
        json!({"op": "delete_table_row", "table": "wave", "row": 1}),
    );
    assert_eq!(d.song().instruments[0].wave_ptr, 2);
    // Deleting the last row: references to it name the new last row.
    let mut d = linked();
    ok(
        &mut d,
        json!({"op": "delete_table_row", "table": "speed", "row": 2}),
    );
    assert_eq!(d.song().instruments[1].speed_ptr, 2);
    // Emptying a table leaves no pointer.
    let mut d = linked();
    for _ in 0..3 {
        ok(
            &mut d,
            json!({"op": "delete_table_row", "table": "speed", "row": 0}),
        );
    }
    assert_eq!(d.song().tables.speed.len(), 0);
    assert_eq!(d.song().instruments[1].speed_ptr, 0);
    assert_eq!(rows(&d, 0)[2], [0, 0, 4, 0]);
    assert!(refused(
        &mut d,
        json!({"op": "delete_table_row", "table": "speed", "row": 0})
    )
    .contains("there is no row 0"));
    d.song().validate().unwrap();
}

#[test]
fn table_rows_set_clear_and_append() {
    let mut d = doc();
    let r = ok(
        &mut d,
        json!({"op": "set_table_row", "table": "filter", "row": 0, "left": 0x90, "right": 0xF1}),
    );
    assert_eq!(
        r["changed"]["tables"][0],
        json!({"table": "filter", "row0": 0, "row1": 0})
    );
    assert_eq!(
        r["changed"]["table_layout"], true,
        "an appended row resizes"
    );
    ok(
        &mut d,
        json!({"op": "set_table_row", "table": "filter", "row": 0, "right": 0xF3}),
    );
    assert_eq!(d.song().tables.filter[0].left, 0x90);
    let r = ok(
        &mut d,
        json!({"op": "set_table_row", "table": "filter", "row": 0, "left": 0x00}),
    );
    assert_eq!(r["changed"]["table_layout"], Value::Null);
    ok(
        &mut d,
        json!({"op": "clear_table_row", "table": "filter", "row": 0}),
    );
    assert_eq!(d.song().tables.filter[0], Default::default());
    let t = ok(
        &mut d,
        json!({"op": "table", "table": "wave", "row0": 1, "count": 5}),
    );
    assert_eq!(t["cells"], json!([[255, 0]]));
    assert_eq!(
        (t["row0"].clone(), t["count"].clone(), t["rows"].clone()),
        (json!(1), json!(1), json!(2))
    );
    assert_eq!(
        ok(&mut d, json!({"op": "tables"}))["pulse"],
        json!([[0x88, 0], [0xFF, 0]])
    );
    for (op, why) in [
        (
            json!({"op": "set_table_row", "table": "wave", "row": 3, "left": 1}),
            "there is no row 3",
        ),
        (
            json!({"op": "set_table_row", "table": "wave", "row": 0, "left": 256}),
            "0-255",
        ),
        (
            json!({"op": "clear_table_row", "table": "wave", "row": 2}),
            "there is no row 2",
        ),
    ] {
        let e = refused(&mut d, op.clone());
        assert!(e.contains(why), "{op}: {e}");
    }
    // 255 rows at most.
    let mut full = doc();
    for row in 0..253 {
        ok(
            &mut full,
            json!({"op": "set_table_row", "table": "speed", "row": row, "left": 1, "right": 1}),
        );
    }
    ok(
        &mut full,
        json!({"op": "set_table_row", "table": "speed", "row": 253, "left": 1}),
    );
    ok(
        &mut full,
        json!({"op": "set_table_row", "table": "speed", "row": 254, "left": 1}),
    );
    assert_eq!(full.song().tables.speed.len(), 255);
    assert!(refused(
        &mut full,
        json!({"op": "set_table_row", "table": "speed", "row": 255})
    )
    .contains("is full"));
    assert!(refused(
        &mut full,
        json!({"op": "insert_table_row", "table": "speed", "row": 0})
    )
    .contains("is full"));
    full.song().validate().unwrap();
    full.to_sng_bytes().unwrap();
}

#[test]
fn unused_table_rows_go_and_the_rest_is_renumbered() {
    let mut d = doc();
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            // Rows nothing reaches, before, between and after instrument 1's and 2's.
            {"op": "insert_table_row", "table": "wave", "row": 0, "left": 0x81, "right": 0},
            {"op": "insert_table_row", "table": "wave", "row": 3, "left": 0x41, "right": 3},
            {"op": "add_instrument"},
            {"op": "insert_table_row", "table": "speed", "row": 0, "left": 1, "right": 1},
            {"op": "insert_table_row", "table": "speed", "row": 1, "left": 2, "right": 2},
            {"op": "set_cell", "pattern": 0, "row": 0, "cell": [49, 1, 4, 2]},
        ]}),
    );
    assert_eq!(d.song().tables.wave.len(), 6);
    let r = ok(&mut d, json!({"op": "remove_unused_table_rows"}));
    // wave rows 1 and 4 (pointers) are not reached; speed row 1 is not.
    assert_eq!(r["removed"], 3);
    let s = d.song();
    assert_eq!(s.tables.wave.len(), 4);
    assert_eq!(
        s.tables.speed,
        vec![cubed_sid::song::TableRow { left: 2, right: 2 }]
    );
    assert_eq!(
        rows(&d, 0)[0],
        [49, 1, 4, 1],
        "the 4 command names the speed row that stayed"
    );
    assert_eq!(
        (s.instruments[0].wave_ptr, s.instruments[1].wave_ptr),
        (1, 3)
    );
    s.validate().unwrap();
    // Nothing to remove: nothing recorded.
    let steps = d.info()["undo_steps"].as_u64();
    let r = ok(&mut d, json!({"op": "remove_unused_table_rows"}));
    assert_eq!(r["removed"], 0);
    assert_eq!(d.info()["undo_steps"].as_u64(), steps);
}

#[test]
fn the_header_edits_and_a_tempo_moves_into_the_song_for_a_sng() {
    let mut d = doc();
    let r = ok(&mut d, json!({"op": "set_title", "title": "Cold Open"}));
    assert_eq!(r["changed"], json!({"header": true}));
    ok(
        &mut d,
        json!({"op": "set_texts", "author": "me", "copyright": "2026"}),
    );
    ok(&mut d, json!({"op": "set_model", "model": "8580"}));
    ok(
        &mut d,
        json!({"op": "set_speed_multiplier", "multiplier": 2}),
    );
    let info = d.info();
    assert_eq!(
        (
            info["title"].clone(),
            info["author"].clone(),
            info["copyright"].clone()
        ),
        (json!("Cold Open"), json!("me"), json!("2026"))
    );
    assert_eq!(
        (info["model"].clone(), info["speed_multiplier"].clone()),
        (json!("8580"), json!(2))
    );
    for (op, why) in [
        (
            json!({"op": "set_title", "title": "x".repeat(33)}),
            "longer than 32",
        ),
        (json!({"op": "set_title", "title": "€"}), "latin-1"),
        (json!({"op": "set_title"}), "'title' is needed"),
        (
            json!({"op": "set_model", "model": "6510"}),
            "not a chip model",
        ),
        (
            json!({"op": "set_speed_multiplier", "multiplier": 0}),
            "1-16",
        ),
        (
            json!({"op": "set_speed_multiplier", "multiplier": 17}),
            "1-16",
        ),
        (json!({"op": "set_tempo", "tempo": 0}), "1-127"),
        (json!({"op": "set_tempo", "tempo": 128}), "1-127"),
    ] {
        let e = refused(&mut d, op.clone());
        assert!(e.contains(why), "{op}: {e}");
    }
    // A start tempo other than 6 is a .asid fact: a .sng refuses it until it is an F command.
    ok(&mut d, json!({"op": "set_tempo", "tempo": 5}));
    let e = d.to_sng_bytes().unwrap_err();
    assert!(e.contains("no tempo field") && e.contains("tempo 5"), "{e}");
    let (problems, _) = d.validate();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("cannot be written as a .sng")),
        "{problems:?}"
    );
    let before = d.song().clone();
    let r = ok(&mut d, json!({"op": "bake_tempo"}));
    assert_eq!(r["changed"]["patterns"][0]["pattern"], 0);
    assert_eq!(d.song().tempo, 6);
    // 5 frames per 1x at 2x is 10 frames a row, as the player starts it.
    assert_eq!(
        (
            d.song().patterns[0].rows[0].command,
            d.song().patterns[0].rows[0].param
        ),
        (0xF, 10)
    );
    d.to_sng_bytes().unwrap();
    d.undo().unwrap();
    assert_eq!(*d.song(), before);
    // Refusals: the row has a command; the tempo does not fit.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "command": 5, "param": 1}),
    );
    assert!(refused(&mut d, json!({"op": "bake_tempo"})).contains("already has the command 501"));
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "command": 0, "param": 0}),
    );
    ok(
        &mut d,
        json!({"op": "set_speed_multiplier", "multiplier": 16}),
    );
    ok(&mut d, json!({"op": "set_tempo", "tempo": 9}));
    assert!(refused(&mut d, json!({"op": "bake_tempo"})).contains("does not fit an F command"));
    // Nothing to do at 6.
    let mut d = doc();
    let r = ok(&mut d, json!({"op": "bake_tempo"}));
    assert_eq!(r["changed"], json!({}));
}

#[test]
fn undo_and_redo_step_through_groups_and_batches() {
    let mut d = doc();
    let start = d.song().clone();
    assert!(!d.can_undo() && !d.can_redo());
    assert!(d.apply(&json!({"op": "undo"})).is_err());
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "note": 49}),
    );
    let s1 = d.song().clone();
    // A group is one step, however many edits.
    d.begin_group();
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 1, "note": 50}),
    );
    ok(
        &mut d,
        json!({"op": "insert_order", "voice": 0, "index": 1, "pattern": 1}),
    );
    ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 1, "name": "x"}),
    );
    assert!(!d.can_undo(), "no undo while a group is open");
    d.end_group();
    let s2 = d.song().clone();
    // A batch is one step; a failing one is none.
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_cell", "pattern": 0, "row": 2, "note": 51},
            {"op": "set_cell", "pattern": 0, "row": 3, "note": 52},
        ]}),
    );
    let s3 = d.song().clone();
    let e = d
        .apply(&json!({"op": "batch", "ops": [
            {"op": "set_cell", "pattern": 0, "row": 4, "note": 53},
            {"op": "set_cell", "pattern": 0, "row": 5, "note": 200},
        ]}))
        .unwrap_err();
    assert!(
        e.contains("operation 1 (set_cell) failed") && e.contains("not a note"),
        "{e}"
    );
    assert_eq!(*d.song(), s3);
    assert_eq!(d.info()["undo_steps"], 3);
    let c = d.undo().unwrap();
    assert_eq!(*d.song(), s2);
    assert_eq!(c.patterns[&0], (2, 3));
    d.undo().unwrap();
    assert_eq!(*d.song(), s1);
    let c = d.redo().unwrap();
    assert_eq!(*d.song(), s2);
    assert!(c.orders.contains(&(0, 0)) && c.instruments == vec![1]);
    // An edit after an undo ends the redo line.
    d.undo().unwrap();
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 9, "note": 60}),
    );
    assert!(!d.can_redo());
    while d.undo().is_some() {}
    assert_eq!(*d.song(), start);
    while d.redo().is_some() {}
    assert_eq!(d.info()["undo_steps"], 2);
    // Limits: the oldest step goes first, the newest stays.
    let mut d = doc();
    d.set_history_limits(3, 1 << 20);
    for r in 0..10 {
        ok(
            &mut d,
            json!({"op": "set_cell", "pattern": 0, "row": r, "note": 49}),
        );
    }
    assert_eq!(d.info()["undo_steps"], 3);
    d.set_history_limits(100, 0);
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 11, "note": 49}),
    );
    assert_eq!(
        d.info()["undo_steps"],
        1,
        "over the byte bound only the newest stays"
    );
    assert!(d.info()["history_bytes"].as_u64().unwrap() > 0);
    d.clear_history();
    assert!(!d.can_undo());
}

#[test]
fn changes_say_what_to_redraw() {
    let mut d = doc();
    let c = |d: &mut SidDoc, op: Value| ok(d, op)["changed"].clone();
    assert_eq!(
        c(
            &mut d,
            json!({"op": "set_cell", "pattern": 2, "row": 5, "note": 49})
        ),
        json!({"patterns": [{"pattern": 2, "row0": 5, "row1": 5}]})
    );
    assert_eq!(
        c(&mut d, json!({"op": "new_pattern"})),
        json!({"layout": true})
    );
    assert_eq!(
        c(
            &mut d,
            json!({"op": "insert_order", "voice": 2, "index": 0, "pattern": 3})
        ),
        json!({"orders": [{"subsong": 0, "voice": 2}]})
    );
    assert_eq!(
        c(
            &mut d,
            json!({"op": "set_instrument", "instrument": 1, "attack": 3})
        ),
        json!({"instruments": [1]})
    );
    assert_eq!(
        c(&mut d, json!({"op": "add_instrument"})),
        json!({
            "instrument_list": true, "instruments": [2],
            "tables": [{"table": "wave", "row0": 0, "row1": 3}, {"table": "pulse", "row0": 0, "row1": 3}],
            "table_layout": true,
        })
    );
    assert_eq!(
        c(&mut d, json!({"op": "add_subsong"})).get("subsongs"),
        Some(&json!(true))
    );
    // The undo result is the same kind of record.
    let u = d.undo().unwrap().to_json();
    assert_eq!(u["subsongs"], true);
    assert_eq!(d.last_changes().to_json(), u);
}

#[test]
fn live_edits_are_what_a_playing_song_takes_and_the_rest_is_a_rebuild() {
    use cubed_sid::song::LiveEdit;
    let mut d = linked();
    d.clear_history();
    let edits = |d: &SidDoc| d.live_edits();
    // A row, an instrument field, a table row's content, the chip.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 7, "cell": [49, 1, 4, 2]}),
    );
    let e = edits(&d).unwrap();
    assert_eq!(
        e,
        vec![LiveEdit::Row {
            pattern: 0,
            row: 7,
            cell: cubed_sid::song::Row {
                note: 49,
                instrument: 1,
                command: 4,
                param: 2
            }
        }]
    );
    ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 2, "attack": 7, "name": "z"}),
    );
    match &edits(&d).unwrap()[..] {
        [LiveEdit::Instrument {
            number: 2,
            instrument,
        }] => {
            assert_eq!(instrument.attack, 7);
            assert!(instrument.name.is_empty(), "a live edit carries no name");
        }
        other => panic!("{other:?}"),
    }
    ok(
        &mut d,
        json!({"op": "set_table_row", "table": "pulse", "row": 4, "left": 0x30}),
    );
    assert_eq!(
        edits(&d).unwrap(),
        vec![LiveEdit::TableRow {
            table: TableKind::Pulse,
            row: 4,
            value: cubed_sid::song::TableRow {
                left: 0x30,
                right: 1
            }
        }]
    );
    ok(&mut d, json!({"op": "set_model", "model": "8580"}));
    assert_eq!(edits(&d).unwrap(), vec![LiveEdit::Model(SidModel::Sid8580)]);
    // Typing, clearing, pasting, transposing and shifting rows are all rows; so are their undos.
    ok(
        &mut d,
        json!({"op": "insert_row", "pattern": 0, "row": 0, "count": 2}),
    );
    let shifted = edits(&d).unwrap();
    assert!(
        shifted.len() >= 4 && shifted.iter().all(|e| matches!(e, LiveEdit::Row { .. })),
        "{shifted:?}"
    );
    d.undo().unwrap();
    assert!(edits(&d).is_some());
    // The title is not played: nothing to do.
    ok(&mut d, json!({"op": "set_title", "title": "t"}));
    assert_eq!(edits(&d).unwrap(), vec![]);
    // Everything that moves the song's shape needs a new player.
    for op in [
        json!({"op": "set_pattern_rows", "pattern": 1, "rows": 32}),
        json!({"op": "new_pattern"}),
        json!({"op": "insert_row", "pattern": 1, "row": 0, "grow": true}),
        json!({"op": "insert_order", "voice": 0, "index": 0, "pattern": 1}),
        json!({"op": "set_restart", "voice": 0, "restart": 0}),
        json!({"op": "set_order", "voice": 0, "index": 0, "transpose": 2}),
        json!({"op": "add_subsong"}),
        json!({"op": "add_instrument", "tables": false}),
        json!({"op": "insert_table_row", "table": "filter", "row": 0}),
        json!({"op": "set_table_row", "table": "filter", "row": 1, "left": 1}),
        json!({"op": "set_tempo", "tempo": 7}),
        json!({"op": "set_speed_multiplier", "multiplier": 3}),
        json!({"op": "delete_instrument", "instrument": 3}),
    ] {
        ok(&mut d, op.clone());
        if matches!(op["op"].as_str(), Some("set_table_row")) {
            // The filter table was empty: an append is a resize.
        }
        assert!(edits(&d).is_none(), "{op} is not a live edit");
        assert_eq!(
            ok(&mut d, json!({"op": "live_edits"}))["edits"],
            Value::Null
        );
    }
    // More than 1024 rows is a new song.
    let mut big = doc();
    for _ in 0..30 {
        ok(&mut big, json!({"op": "new_pattern", "rows": 128}));
    }
    ok(
        &mut big,
        json!({"op": "batch", "ops": (0..10).map(|p| json!({"op": "paste", "pattern": p, "row": 0,
        "cells": (0..128).map(|_| json!([49, 1, 0, 0])).collect::<Vec<_>>()})).collect::<Vec<_>>()}),
    );
    assert!(big.live_edits().is_none());
    // The JSON shapes.
    let mut d = doc();
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 1, "cell": [49, 1, 0, 0]}),
    );
    assert_eq!(
        ok(&mut d, json!({"op": "live_edits"}))["edits"],
        json!([{"kind": "row", "pattern": 0, "row": 1, "cell": [49, 1, 0, 0]}])
    );
}

#[test]
fn files_load_by_their_magic_and_refuse_what_they_are_not() {
    let d = doc();
    let asid = d.to_asid_bytes();
    assert!(SidDoc::from_bytes(&asid).is_ok());
    let fixtures =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/goattracker");
    // A .sng, whatever its name: GTS5 and GTS!, with hints for what it cannot say.
    for name in ["gts5_basics.sng", "gt1_commands.sng"] {
        let bytes = std::fs::read(fixtures.join(name)).unwrap();
        let d = SidDoc::from_bytes(&bytes).unwrap();
        d.song().validate().unwrap();
    }
    let bytes = std::fs::read(fixtures.join("x_6581_3x.sng")).unwrap();
    let hints = cubed_sid::goattracker::hints_from_name("x_6581_3x.sng");
    let d = SidDoc::from_sng(&bytes, hints).unwrap();
    assert_eq!(
        (d.song().model, d.song().speed_multiplier),
        (SidModel::Sid6581, 3)
    );
    for (bytes, why) in [
        (b"RIFF....".to_vec(), "not a SID song"),
        (b"".to_vec(), "not a SID song"),
        (
            std::fs::read(fixtures.join("refuse_beta_gts3.sng")).unwrap(),
            "beta",
        ),
        (
            std::fs::read(fixtures.join("refuse_dual_sid.sng")).unwrap(),
            "dual-SID",
        ),
        (asid[..20].to_vec(), "ends inside"),
    ] {
        let e = SidDoc::from_bytes(&bytes).unwrap_err();
        assert!(e.contains(why), "{e}");
    }
    // A song the layout cannot hold does not become a document.
    let mut s = SidSong::parse(&asid).unwrap();
    s.instruments[0].attack = 16;
    assert!(SidDoc::from_song(s).is_err());
}

#[test]
fn validate_reports_what_plays_oddly() {
    let mut d = doc();
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_cell", "pattern": 0, "row": 0, "cell": [49, 1, 8, 99]},
            {"op": "set_cell", "pattern": 0, "row": 1, "cell": [0, 0, 0xD, 0x1F]},
            {"op": "set_cell", "pattern": 0, "row": 2, "cell": [0, 0, 0xF, 2]},
            {"op": "set_cell", "pattern": 0, "row": 3, "cell": [0, 0, 4, 1]},
        ]}),
    );
    let v = ok(&mut d, json!({"op": "validate"}));
    assert_eq!(v["problems"], json!([]));
    let w: Vec<&str> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert_eq!(w.len(), 4, "{w:?}");
    assert!(w[0].contains("pattern 0 row 0") && w[0].contains("wave table row 99"));
    assert!(w[1].contains("volume 1F"));
    assert!(w[2].contains("tempo 02"));
    assert!(w[3].contains("speed table row 1, past the table's 0 rows"));
}

#[test]
fn reads_give_the_shapes_an_editor_draws_from() {
    let mut d = doc();
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 1, "cell": [49, 1, 4, 2]}),
    );
    let r = ok(
        &mut d,
        json!({"op": "rows", "pattern": 0, "row0": 0, "count": 3}),
    );
    assert_eq!(
        r["cells"],
        json!([[0, 0, 0, 0], [49, 1, 4, 2], [0, 0, 0, 0]])
    );
    assert_eq!(
        (r["rows"].clone(), r["count"].clone()),
        (json!(64), json!(3))
    );
    let r = ok(
        &mut d,
        json!({"op": "rows", "pattern": 0, "row0": 63, "count": 9, "flat": true}),
    );
    assert_eq!(
        (r["count"].clone(), r["cells"].clone()),
        (json!(1), json!([0, 0, 0, 0]))
    );
    let r = ok(
        &mut d,
        json!({"op": "rows", "pattern": 0, "row0": 70, "count": 9}),
    );
    assert_eq!(r["count"], 0);
    assert!(d.apply(&json!({"op": "rows", "pattern": 9})).is_err());
    let p = ok(&mut d, json!({"op": "patterns"}));
    assert_eq!(
        p["patterns"][1],
        json!({"pattern": 1, "rows": 64, "used": 1})
    );
    let all = ok(&mut d, json!({"op": "orderlists"}));
    assert_eq!(all["voices"][2]["entries"], json!([[2, 0, 1]]));
    let i = ok(&mut d, json!({"op": "instruments"}));
    assert_eq!(i["instruments"][0]["uses"], 1);
    assert_eq!(
        ok(&mut d, json!({"op": "note_name", "note": 49}))["name"],
        "C-4"
    );
    assert_eq!(
        ok(&mut d, json!({"op": "note_name", "note": 93}))["name"],
        "G#7"
    );
    assert_eq!(
        ok(&mut d, json!({"op": "note_name", "note": 126}))["name"],
        "==="
    );
    assert_eq!(
        ok(&mut d, json!({"op": "parse_note", "text": "a#2"}))["note"],
        35
    );
    assert!(d.apply(&json!({"op": "parse_note", "text": "x"})).is_err());
    let e = d.apply(&json!({"op": "frobnicate"})).unwrap_err();
    assert!(e.contains("not an operation"), "{e}");
    assert!(d.apply(&json!({"no_op": 1})).is_err());
    let s = ok(&mut d, json!({"op": "export_sng"}));
    assert_eq!(s["bytes"][0], 71, "G of GTS5");
    assert!(s["notes"][0].as_str().unwrap().contains("6581"));
}
