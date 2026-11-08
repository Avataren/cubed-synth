//! `AhxDoc`: every operation, its refusals, undo and redo, the writer after
//! edits, and the shapes the JSON entry point returns.

use cubed_ahx::edit::{
    AhxDoc, At, ClearMask, InsertKind, PasteMode, PositionMap, StepClip, StepUpdate,
};
use cubed_ahx::format::{self, Song, SongFormat, Step};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn songs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/songs")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(songs_dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn ahx() -> AhxDoc {
    AhxDoc::from_bytes(&fixture("karma.ahx")).unwrap()
}

fn hvl() -> AhxDoc {
    AhxDoc::from_bytes(&fixture("doobrey_gubbins.hvl")).unwrap()
}

fn op(doc: &mut AhxDoc, v: Value) -> Value {
    doc.apply(&v).unwrap_or_else(|e| panic!("{v}: {e}"))
}

fn refused(doc: &mut AhxDoc, v: Value) -> String {
    let before = doc.song().clone();
    let revision = doc.revision();
    let e = doc.apply(&v).expect_err(&format!("{v} should be refused"));
    assert_eq!(doc.song(), &before, "{v}: a refused edit changed the song");
    assert_eq!(
        doc.revision(),
        revision,
        "{v}: a refused edit counted as a change"
    );
    e
}

/// The song as the file holds it: write and parse back.
fn saved(doc: &AhxDoc) -> Song {
    let bytes = doc
        .to_bytes()
        .unwrap_or_else(|e| panic!("cannot be saved: {e}"));
    let song = format::parse(&bytes).unwrap();
    assert_eq!(
        &song,
        doc.song(),
        "the written file does not parse back to the song"
    );
    let offset = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    assert_eq!(doc.size().0, offset, "size() is the string table offset");
    song
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

// ----- making and reading ---------------------------------------------------

#[test]
fn a_new_song_is_a_valid_empty_song_that_saves() {
    for (format, channels) in [
        (SongFormat::Ahx, 4),
        (SongFormat::Hvl, 4),
        (SongFormat::Hvl, 16),
    ] {
        let doc = AhxDoc::with_options(&cubed_ahx::edit::NewSong {
            channels,
            ..cubed_ahx::edit::NewSong::new(format)
        })
        .unwrap();
        assert!(doc.validate().is_empty(), "{:?}", doc.validate());
        saved(&doc);
        let info = doc.info();
        assert_eq!(info["positions"], 1);
        assert_eq!(info["tracks"], 2);
        assert_eq!(info["track_length"], 64);
        assert_eq!(info["instruments"], 0);
        assert_eq!(info["channels"], channels);
        assert_eq!(info["blank_track0"], true);
        assert_eq!(info["can_undo"], false);
        assert_eq!(info["limits"]["instruments"], 63);
        if format == SongFormat::Hvl {
            assert_eq!(info["defstereo"], 2);
            assert_eq!(info["effect_columns"], 2);
        } else {
            assert_eq!(info["effect_columns"], 1);
        }
        // The first channel plays the blank track 1, the rest the shared track 0.
        let pos = doc.positions(0, 10);
        assert_eq!(pos["entries"][0][0], 1);
        assert_eq!(pos["entries"][0][2], 0);
    }
    assert!(AhxDoc::create(&json!({"format": "hvl", "channels": 17})).is_err());
    assert!(AhxDoc::create(&json!({"format": "ahx", "channels": 5})).is_err());
    assert!(AhxDoc::create(&json!({"track_length": 65})).is_err());
    assert!(AhxDoc::create(&json!({"speed_multiplier": 5})).is_err());
    assert!(AhxDoc::create(&json!({"format": "mod"})).is_err());
    let d = AhxDoc::create(&json!({"format": "hvl", "channels": 8, "track_length": 32, "speed_multiplier": 2, "name": "x\u{4e16}"})).unwrap();
    assert_eq!(d.song().track_length, 32);
    assert_eq!(d.song().name, "x?", "names are Latin-1");
    assert_eq!(d.info()["tick_hz"], 100);
}

#[test]
fn reads_show_the_song() {
    let doc = ahx();
    let info = doc.info();
    assert_eq!(info["format"], "ahx");
    assert_eq!(info["name"], "Karma");
    assert_eq!(info["positions"], 38);
    assert_eq!(info["tracks"], 75);
    assert_eq!(info["instruments"], 31);
    assert_eq!(info["note"]["midi_offset"], 23);

    let rows = doc.rows(1, 0, 4, false).unwrap();
    assert_eq!(rows["count"], 4);
    assert_eq!(rows["rows"], 64);
    assert_eq!(rows["cells"][0].as_array().unwrap().len(), 6);
    let flat = doc.rows(1, 60, 100, true).unwrap();
    assert_eq!(flat["count"], 4, "rows past the end are left out");
    assert_eq!(flat["cells"].as_array().unwrap().len(), 24);
    assert!(doc.rows(75, 0, 1, false).is_err());

    let pr = doc.position_rows(0, 0, 8, false).unwrap();
    assert_eq!(pr["channels"], 4);
    assert_eq!(pr["cells"].as_array().unwrap().len(), 8);
    assert_eq!(pr["cells"][0].as_array().unwrap().len(), 4);
    let t0 = pr["tracks"][0].as_u64().unwrap() as usize;
    assert_eq!(
        pr["cells"][3][0],
        doc.rows(t0, 3, 1, false).unwrap()["cells"][0]
    );
    let flat = doc.position_rows(0, 0, 8, true).unwrap();
    assert_eq!(flat["cells"].as_array().unwrap().len(), 8 * 4 * 6);
    assert!(doc.position_rows(38, 0, 1, false).is_err());

    let list = doc.positions(2, 3);
    assert_eq!(
        (
            list["first"].clone(),
            list["count"].clone(),
            list["total"].clone()
        ),
        (json!(2), json!(3), json!(38))
    );
    assert_eq!(list["entries"][0].as_array().unwrap().len(), 8);

    let tracks = doc.tracks();
    assert_eq!(tracks["count"], 75);
    assert_eq!(
        tracks["used"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u.as_u64().unwrap())
            .sum::<u64>(),
        38 * 4
    );

    let ins = doc.instruments();
    assert_eq!(ins["count"], 31);
    assert_eq!(ins["instruments"][0]["n"], 1);
    let i1 = doc.instrument(1).unwrap();
    assert_eq!(
        i1["envelope"]["a_frames"],
        doc.song().instruments[1].envelope.a_frames
    );
    assert_eq!(
        i1["plist_length"],
        doc.song().instruments[1].plist.entries.len()
    );
    assert!(doc.instrument(0).is_err());
    assert!(doc.instrument(32).is_err());
    let pl = doc.plist(1, 0, 3, false).unwrap();
    assert_eq!(pl["rows"][0].as_array().unwrap().len(), 7);
    assert_eq!(pl["length"], doc.song().instruments[1].plist.entries.len());
    let flat = doc.plist(1, 0, 3, true).unwrap();
    assert_eq!(flat["rows"].as_array().unwrap().len(), 21);
    assert!(doc.validate().is_empty());
}

// ----- steps ------------------------------------------------------------------

#[test]
fn set_step_in_all_its_forms_and_its_refusals() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    // The first channel of the new song plays the blank track 1.
    let r = op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 3, "note": "C-3", "instrument": 2}),
    );
    assert_eq!(r["step"], json!([25, 2, 0, 0, 0, 0]));
    assert_eq!(
        r["changed"]["tracks"],
        json!([{"track": 1, "row0": 3, "row1": 3}])
    );
    let r = op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 3, "fx": 12, "param": 64}),
    );
    assert_eq!(
        r["step"],
        json!([25, 2, 12, 64, 0, 0]),
        "the other fields stay"
    );
    op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 4, "cell": [1, 0, 15, 3]}),
    );
    op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 5, "cell": [60, 63, 15, 255, 0, 0]}),
    );
    op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 5, "note": 0}),
    );
    saved(&doc);

    assert!(refused(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 64, "note": 1})
    )
    .contains("outside"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 0, "note": 64})
    )
    .contains("note"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 0, "note": "C-7"})
    )
    .contains("not a note"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 0, "instrument": 64})
    )
    .contains("63"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 0, "fx": 16})
    )
    .contains("hex digit"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 0, "param": 256})
    )
    .contains("256"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 0, "fxb": 1})
    )
    .contains("one effect column"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_step", "track": 2, "row": 0, "note": 1})
    )
    .contains("no track 2"));
    assert!(
        refused(&mut doc, json!({"op": "set_step", "track": 1, "row": 0}))
            .contains("nothing to set")
    );
    assert!(
        refused(&mut doc, json!({"op": "set_step", "row": 0, "note": 1})).contains("needs 'track'")
    );
    assert!(refused(
        &mut doc,
        json!({"op": "set_step", "position": 1, "channel": 0, "row": 0, "note": 1})
    )
    .contains("no position 1"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_step", "position": 0, "channel": 4, "row": 0, "note": 1})
    )
    .contains("channels 0 to 3"));
    assert!(refused(&mut doc, json!({"op": "nonsense"})).contains("not an operation"));

    // Track 0 is blank and shared: no write goes into it, `unique` gives the cell its own.
    let e = refused(
        &mut doc,
        json!({"op": "set_step", "position": 0, "channel": 1, "row": 0, "note": 5}),
    );
    assert!(e.contains("Track 0"), "{e}");
    let r = op(
        &mut doc,
        json!({"op": "set_step", "position": 0, "channel": 1, "row": 0, "note": 5, "unique": true}),
    );
    assert_eq!(r["changed"]["positions"], json!({"first": 0, "last": 0}));
    assert_eq!(
        doc.song().positions[0].track[1],
        2,
        "a new track at the end"
    );
    assert_eq!(doc.song().tracks[2][0].note, 5);
    assert!(doc.song().tracks[0].iter().all(|s| *s == Step::default()));
    saved(&doc);
    // Writing again does not make another.
    op(
        &mut doc,
        json!({"op": "set_step", "position": 0, "channel": 1, "row": 1, "note": 6, "unique": true}),
    );
    assert_eq!(doc.song().tracks.len(), 3);
}

#[test]
fn an_hvl_step_has_two_effect_columns_and_no_note_63() {
    let mut doc = AhxDoc::new(SongFormat::Hvl);
    let r = op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 0, "cell": [13, 1, 12, 64, 7, 3]}),
    );
    assert_eq!(r["step"], json!([13, 1, 12, 64, 7, 3]));
    let e = refused(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 1, "note": 63}),
    );
    assert!(e.contains("note 63"), "{e}");
    op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 1, "note": 62}),
    );
    saved(&doc);
}

#[test]
fn clear_insert_delete_transpose_copy_and_paste() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    for row in 0..8u8 {
        op(
            &mut doc,
            json!({"op": "set_step", "track": 1, "row": row, "cell": [row + 10, 1, row, row + 1]}),
        );
    }
    // clear_range with columns
    let r = op(
        &mut doc,
        json!({"op": "clear_range", "track": 1, "row0": 0, "row1": 1, "columns": ["note"]}),
    );
    assert_eq!(r["changed"]["tracks"][0]["row1"], 1);
    assert_eq!(
        doc.song().tracks[1][0],
        Step {
            note: 0,
            instrument: 1,
            fx: 0,
            fx_param: 1,
            ..Step::default()
        }
    );
    op(
        &mut doc,
        json!({"op": "clear_range", "track": 1, "row0": 0, "row1": 1, "columns": ["fx", "instrument"]}),
    );
    assert_eq!(doc.song().tracks[1][1], Step::default());
    assert!(refused(
        &mut doc,
        json!({"op": "clear_range", "track": 1, "row0": 3, "row1": 2})
    )
    .contains("backwards"));
    assert!(refused(
        &mut doc,
        json!({"op": "clear_range", "track": 1, "row0": 0, "row1": 1, "columns": ["volume"]})
    )
    .contains("not a column"));

    // insert / delete shift the rows below, in the same track only
    let before = doc.song().tracks[1].clone();
    op(
        &mut doc,
        json!({"op": "insert_row", "track": 1, "row": 2, "count": 2}),
    );
    let t = &doc.song().tracks[1];
    assert_eq!(t[2], Step::default());
    assert_eq!(t[3], Step::default());
    assert_eq!(t[4], before[2]);
    assert_eq!(t[63], before[61], "the rows pushed off the end are lost");
    op(
        &mut doc,
        json!({"op": "delete_row", "track": 1, "row": 2, "count": 2}),
    );
    assert_eq!(doc.song().tracks[1][2], before[2]);
    assert_eq!(
        doc.song().tracks[1][63],
        Step::default(),
        "blank rows fill the end"
    );
    assert!(
        refused(&mut doc, json!({"op": "insert_row", "track": 1, "row": 64})).contains("outside")
    );
    assert!(refused(
        &mut doc,
        json!({"op": "insert_row", "track": 1, "row": 0, "count": 0})
    )
    .contains("at least 1"));

    // transpose
    let r = op(
        &mut doc,
        json!({"op": "transpose", "track": 1, "row0": 0, "row1": 7, "semitones": 12}),
    );
    assert_eq!(r["count"], 6);
    assert_eq!(doc.song().tracks[1][2].note, 12 + 12);
    let e = refused(
        &mut doc,
        json!({"op": "transpose", "track": 1, "row0": 0, "row1": 7, "semitones": 40}),
    );
    assert!(e.contains("outside C-1 to B-5"), "{e}");
    let r = op(
        &mut doc,
        json!({"op": "transpose", "track": 1, "row0": 0, "row1": 7, "semitones": 40, "clamp": true}),
    );
    assert_eq!(r["count"], 6);
    assert!(doc.song().tracks[1][..8]
        .iter()
        .all(|s| s.note == 0 || s.note == 60));
    let r = op(
        &mut doc,
        json!({"op": "transpose", "track": 1, "row0": 0, "row1": 7, "semitones": -1, "instrument": 9}),
    );
    assert_eq!(r["count"], 0, "only steps with that instrument");

    // copy and paste
    let clip = op(
        &mut doc,
        json!({"op": "copy_range", "track": 1, "row0": 2, "row1": 5}),
    );
    assert_eq!(clip["rows"], 4);
    let r = op(
        &mut doc,
        json!({"op": "paste", "track": 1, "row": 62, "cells": clip["cells"].clone()}),
    );
    assert_eq!(
        (r["rows"].clone(), r["clipped"].clone()),
        (json!(2), json!(2))
    );
    assert_eq!(doc.song().tracks[1][62], doc.song().tracks[1][2]);
    // mix fills only the empty fields
    op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 20, "cell": [30, 0, 0, 0]}),
    );
    op(
        &mut doc,
        json!({"op": "paste", "track": 1, "row": 20, "cells": [[40, 5, 15, 9, 0, 0]], "mode": "mix"}),
    );
    assert_eq!(
        doc.song().tracks[1][20],
        Step {
            note: 30,
            instrument: 5,
            fx: 15,
            fx_param: 9,
            ..Step::default()
        }
    );
    op(
        &mut doc,
        json!({"op": "paste", "track": 1, "row": 20, "cells": [[40, 5, 15, 9, 0, 0]]}),
    );
    assert_eq!(doc.song().tracks[1][20].note, 40);
    assert!(refused(
        &mut doc,
        json!({"op": "paste", "track": 1, "row": 0, "cells": [[64, 0, 0, 0, 0, 0]]})
    )
    .contains("Row 0 of the clip"));
    assert!(refused(
        &mut doc,
        json!({"op": "paste", "track": 1, "row": 0, "cells": [[1, 0, 0, 0, 1, 0]]})
    )
    .contains("one effect column"));
    assert!(refused(
        &mut doc,
        json!({"op": "paste", "track": 1, "row": 0, "cells": "x"})
    )
    .contains("clip"));
    assert!(refused(
        &mut doc,
        json!({"op": "paste", "track": 1, "row": 0, "cells": [], "mode": "xor"})
    )
    .contains("not a paste mode"));
    saved(&doc);

    // the typed API agrees
    let clip = doc.copy_range(1, 0, 3).unwrap();
    assert_eq!(StepClip::from_json(&clip.to_json()).unwrap(), clip);
    doc.paste(At::Track(1), false, 30, &clip, PasteMode::Overwrite)
        .unwrap();
    doc.clear_range(At::Track(1), false, 30, 33, ClearMask::ALL)
        .unwrap();
    doc.set_step(
        At::Track(1),
        false,
        40,
        StepUpdate {
            note: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    saved(&doc);
}

#[test]
fn track_length_alloc_and_compact() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 40, "note": 3}),
    );
    let e = refused(&mut doc, json!({"op": "set_track_length", "rows": 32}));
    assert!(e.contains("force"), "{e}");
    op(
        &mut doc,
        json!({"op": "set_track_length", "rows": 32, "force": true}),
    );
    assert_eq!(doc.song().track_length, 32);
    assert!(doc.song().tracks.iter().all(|t| t.len() == 32));
    op(&mut doc, json!({"op": "set_track_length", "rows": 48}));
    assert!(doc.song().tracks.iter().all(|t| t.len() == 48));
    op(&mut doc, json!({"op": "set_track_length", "rows": 48}));
    assert!(refused(&mut doc, json!({"op": "set_track_length", "rows": 0})).contains("1 to 64"));
    assert!(refused(&mut doc, json!({"op": "set_track_length", "rows": 65})).contains("1 to 64"));
    saved(&doc);

    // alloc_track reuses an unused blank track, else appends
    let r = op(&mut doc, json!({"op": "alloc_track"}));
    assert_eq!(r["track"], 2, "a new one");
    assert_eq!(doc.song().tracks.len(), 3);
    let r = op(&mut doc, json!({"op": "alloc_track"}));
    assert_eq!(r["track"], 2, "the unused blank one is reused");
    let r = op(&mut doc, json!({"op": "alloc_track", "append": true}));
    assert_eq!(r["track"], 3);
    let r = op(&mut doc, json!({"op": "alloc_track", "copy_of": 1}));
    assert_eq!(r["track"], 2);
    assert_eq!(doc.song().tracks[2], doc.song().tracks[1]);
    assert!(refused(&mut doc, json!({"op": "alloc_track", "copy_of": 9})).contains("no track 9"));
    // fill to 256
    while doc.song().tracks.len() < 256 {
        op(&mut doc, json!({"op": "alloc_track", "append": true}));
    }
    assert!(refused(&mut doc, json!({"op": "alloc_track", "append": true})).contains("256"));

    // compact: only track 0 and the ones positions play stay
    op(
        &mut doc,
        json!({"op": "assign_track", "position": 0, "channel": 1, "track": 200}),
    );
    op(
        &mut doc,
        json!({"op": "set_step", "track": 200, "row": 0, "note": 9}),
    );
    let r = op(&mut doc, json!({"op": "compact_tracks"}));
    assert_eq!(r["removed"], 253);
    assert_eq!(r["changed"]["layout"], true);
    assert_eq!(doc.song().tracks.len(), 3);
    assert_eq!(doc.song().positions[0].track, vec![1, 2, 0, 0]);
    assert_eq!(doc.song().tracks[2][0].note, 9);
    let r = op(&mut doc, json!({"op": "compact_tracks"}));
    assert_eq!(r["removed"], 0);
    saved(&doc);
    assert!(doc.undo().is_some() && doc.undo().is_some());
}

// ----- positions and the song --------------------------------------------------

#[test]
fn positions_tracks_and_the_header() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 0, "note": 1}),
    );
    // a blank position, a duplicate sharing tracks, a unique duplicate
    let r = op(
        &mut doc,
        json!({"op": "insert_position", "at": 1, "kind": "duplicate", "of": 0}),
    );
    assert_eq!(r["map"], json!({"kind": "insert", "at": 1}));
    assert_eq!(doc.song().positions[1], doc.song().positions[0]);
    assert_eq!(doc.song().tracks.len(), 2);
    let r = op(
        &mut doc,
        json!({"op": "insert_position", "at": 2, "kind": "duplicate_unique", "of": 0}),
    );
    assert_eq!(r["changed"]["position_list"], true);
    assert_eq!(
        doc.song().tracks.len(),
        3,
        "the blank track 0 is not copied, track 1 is"
    );
    assert_eq!(doc.song().positions[2].track, vec![2, 0, 0, 0]);
    assert_eq!(doc.song().tracks[2], doc.song().tracks[1]);
    op(&mut doc, json!({"op": "insert_position", "at": 3}));
    assert_eq!(doc.song().positions[3].track, vec![0, 0, 0, 0]);
    assert_eq!(doc.song().position_nr, 4);
    assert!(
        refused(&mut doc, json!({"op": "insert_position", "at": 9})).contains("cannot be inserted")
    );
    assert!(refused(
        &mut doc,
        json!({"op": "insert_position", "at": 0, "kind": "duplicate", "of": 9})
    )
    .contains("no position 9"));
    assert!(refused(
        &mut doc,
        json!({"op": "insert_position", "at": 0, "kind": "weird"})
    )
    .contains("not a kind"));

    // assign, transpose, make_unique
    op(
        &mut doc,
        json!({"op": "assign_track", "position": 3, "channel": 2, "track": 1}),
    );
    assert_eq!(doc.song().positions[3].track[2], 1);
    let r = op(
        &mut doc,
        json!({"op": "make_unique", "position": 3, "channel": 2}),
    );
    assert_eq!(r["track"], 3, "track 1 is shared, so a copy is made");
    assert_eq!(doc.song().tracks[3], doc.song().tracks[1]);
    let r = op(
        &mut doc,
        json!({"op": "make_unique", "position": 3, "channel": 2}),
    );
    assert_eq!(r["track"], 3, "already its own");
    assert_eq!(r["changed"], json!({}));
    op(
        &mut doc,
        json!({"op": "set_transpose", "position": 3, "channel": 2, "value": -12}),
    );
    assert_eq!(doc.song().positions[3].transpose[2], -12);
    assert!(refused(
        &mut doc,
        json!({"op": "set_transpose", "position": 3, "channel": 2, "value": 128})
    )
    .contains("-128 to 127"));
    assert!(refused(
        &mut doc,
        json!({"op": "assign_track", "position": 3, "channel": 2, "track": 40})
    )
    .contains("no track 40"));

    // restart and subsongs follow position changes
    op(&mut doc, json!({"op": "set_restart", "position": 3}));
    op(&mut doc, json!({"op": "set_subsongs", "starts": [2, 3]}));
    assert!(refused(&mut doc, json!({"op": "set_restart", "position": 4})).contains("0 to 3"));
    assert!(refused(&mut doc, json!({"op": "set_subsongs", "starts": [4]})).contains("position 4"));
    let r = op(&mut doc, json!({"op": "insert_position", "at": 0}));
    assert_eq!(doc.song().restart, 4);
    assert_eq!(doc.song().subsongs, vec![3, 4]);
    let map = PositionMap::Insert { at: 0 };
    assert_eq!(r["map"], map.to_json());
    assert_eq!(map.apply(3), Some(4));
    let r = op(&mut doc, json!({"op": "delete_position", "at": 4}));
    assert_eq!(r["map"], json!({"kind": "delete", "at": 4}));
    assert_eq!(
        doc.song().restart,
        3,
        "a deleted restart falls to the position now there, clamped into the list"
    );
    assert_eq!(doc.song().subsongs, vec![3, 3]);
    assert_eq!(PositionMap::Delete { at: 4 }.apply(4), None);
    assert_eq!(PositionMap::Delete { at: 4 }.apply(5), Some(4));
    let r = op(&mut doc, json!({"op": "move_position", "from": 0, "to": 3}));
    assert_eq!(r["map"], json!({"kind": "move", "from": 0, "to": 3}));
    assert_eq!(PositionMap::Move { from: 0, to: 3 }.apply(0), Some(3));
    assert_eq!(PositionMap::Move { from: 0, to: 3 }.apply(2), Some(1));
    assert_eq!(PositionMap::Move { from: 3, to: 0 }.apply(0), Some(1));
    assert_eq!(PositionMap::Move { from: 3, to: 0 }.apply(3), Some(0));
    assert_eq!(PositionMap::Move { from: 3, to: 0 }.apply(4), Some(4));
    op(&mut doc, json!({"op": "move_position", "from": 2, "to": 2}));
    saved(&doc);
    while doc.song().positions.len() > 1 {
        let last = doc.song().positions.len() - 1;
        op(&mut doc, json!({"op": "delete_position", "at": last}));
    }
    assert!(refused(&mut doc, json!({"op": "delete_position", "at": 0})).contains("at least one"));
    assert_eq!(doc.song().restart, 0);
    assert!(doc.song().subsongs.iter().all(|&s| s == 0));
    saved(&doc);
}

#[test]
fn a_blank_position_over_a_track_0_with_content_gets_blank_tracks_of_its_own() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    // Put content in track 0 the only way it can happen: a song that came with it.
    let mut song = doc.song().clone();
    song.tracks[0][0].note = 7;
    doc = AhxDoc::from_song(song).unwrap();
    op(&mut doc, json!({"op": "insert_position", "at": 1}));
    let t = &doc.song().positions[1].track;
    assert!(t.iter().all(|&x| x != 0), "{t:?}");
    assert_eq!(
        doc.song().tracks.len(),
        2 + 4,
        "track 1 is played by position 0, so every channel gets a new one"
    );
    // while track 0 holds content, it takes writes
    op(
        &mut doc,
        json!({"op": "set_step", "track": 0, "row": 1, "note": 8}),
    );
}

#[test]
fn the_header_name_speed_mix_and_latin_1() {
    let mut doc = AhxDoc::new(SongFormat::Hvl);
    let r = op(
        &mut doc,
        json!({"op": "set_name", "name": "Caf\u{e9}\u{4e16}\u{0}x"}),
    );
    assert_eq!(r["name"], "Caf\u{e9}?x");
    assert_eq!(r["altered"], true);
    assert_eq!(saved(&doc).name, "Caf\u{e9}?x");
    let r = op(&mut doc, json!({"op": "set_name", "name": "Plain"}));
    assert_eq!(r["altered"], false);
    op(&mut doc, json!({"op": "set_speed_multiplier", "value": 3}));
    assert_eq!(doc.song().speed_multiplier, 3);
    assert!(
        refused(&mut doc, json!({"op": "set_speed_multiplier", "value": 5})).contains("1 to 4")
    );
    op(
        &mut doc,
        json!({"op": "set_mix", "mixgain": 50, "defstereo": 4}),
    );
    assert_eq!(
        (doc.song().mixgain_raw, doc.song().defstereo),
        (Some(50), Some(4))
    );
    op(&mut doc, json!({"op": "set_mix", "mixgain": 60}));
    assert_eq!(
        (doc.song().mixgain_raw, doc.song().defstereo),
        (Some(60), Some(4))
    );
    assert!(refused(&mut doc, json!({"op": "set_mix", "defstereo": 5})).contains("0 to 4"));
    assert!(refused(&mut doc, json!({"op": "set_mix"})).contains("give"));
    saved(&doc);
    let mut a = AhxDoc::new(SongFormat::Ahx);
    assert!(refused(&mut a, json!({"op": "set_mix", "mixgain": 1})).contains("no mix"));
}

// ----- instruments ------------------------------------------------------------

#[test]
fn instruments_add_set_copy_clear_rename() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    let r = op(&mut doc, json!({"op": "add_instrument"}));
    assert_eq!(r["instrument"], 1);
    assert_eq!(r["changed"]["instrument_list"], true);
    assert_eq!(doc.song().instruments[1].name, "New instrument");
    assert_eq!(doc.song().instrument_nr, 1);
    let r = op(
        &mut doc,
        json!({"op": "add_instrument", "name": "Bass", "volume": 50, "wave_length": 4, "envelope": {"a_frames": 3, "r_volume": 7}}),
    );
    assert_eq!(r["instrument"], 2);
    let i = &doc.song().instruments[2];
    assert_eq!((i.name.as_str(), i.volume, i.wave_length), ("Bass", 50, 4));
    assert_eq!(
        (
            i.envelope.a_frames,
            i.envelope.r_volume,
            i.envelope.d_frames
        ),
        (3, 7, 10)
    );
    saved(&doc);

    let r = op(
        &mut doc,
        json!({"op": "set_instrument", "n": 2, "filter_lower_limit": 10, "filter_upper_limit": 40, "filter_speed": 33, "hard_cut_release": true, "hard_cut_release_frames": 5, "vibrato_depth": 9, "envelope": {"s_frames": 20}}),
    );
    assert_eq!(r["changed"]["instruments"], json!([2]));
    let i = &doc.song().instruments[2];
    assert_eq!(
        (i.filter_lower_limit, i.filter_upper_limit, i.filter_speed),
        (10, 40, 33)
    );
    assert!(i.hard_cut_release && i.hard_cut_release_frames == 5 && i.vibrato_depth == 9);
    assert_eq!(i.envelope.s_frames, 20);
    assert_eq!(i.plist.entries.len(), 1, "the PList rows stay");
    for (field, value, expect) in [
        ("volume", 65, "volume"),
        ("wave_length", 6, "wave_length"),
        ("filter_upper_limit", 64, "filter_upper_limit"),
        ("filter_lower_limit", 128, "0..127"),
        ("vibrato_depth", 16, "vibrato_depth"),
        ("hard_cut_release_frames", 8, "hard_cut_release_frames"),
        ("filter_speed", 64, "filter_speed"),
    ] {
        let e = refused(
            &mut doc,
            json!({"op": "set_instrument", "n": 2, field: value}),
        );
        assert!(e.contains(expect), "{field}: {e}");
    }
    assert!(refused(
        &mut doc,
        json!({"op": "set_instrument", "n": 2, "envelope": {"a_volume": 65}})
    )
    .contains("a_volume"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_instrument", "n": 3, "volume": 1})
    )
    .contains("no instrument 3"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_instrument", "n": 2, "envelope": 5})
    )
    .contains("object"));

    op(
        &mut doc,
        json!({"op": "rename_instrument", "n": 2, "name": "Lead\u{4e16}"}),
    );
    assert_eq!(doc.song().instruments[2].name, "Lead?");
    let r = op(&mut doc, json!({"op": "copy_instrument", "from": 2}));
    assert_eq!(r["instrument"], 3);
    assert_eq!(doc.song().instruments[3], doc.song().instruments[2]);
    let r = op(
        &mut doc,
        json!({"op": "copy_instrument", "from": 3, "to": 1}),
    );
    assert_eq!(r["instrument"], 1);
    assert_eq!(doc.song().instruments[1], doc.song().instruments[3]);
    let r = op(
        &mut doc,
        json!({"op": "copy_instrument", "from": 1, "to": 1}),
    );
    assert_eq!(r["changed"], json!({}));
    op(&mut doc, json!({"op": "clear_instrument", "n": 1}));
    assert_eq!(doc.song().instruments[1], Default::default());
    assert_eq!(doc.song().instrument_nr, 3, "the slot stays");
    saved(&doc);

    while doc.song().instruments.len() <= 63 {
        op(&mut doc, json!({"op": "add_instrument"}));
    }
    assert!(refused(&mut doc, json!({"op": "add_instrument"})).contains("63"));
    assert_eq!(doc.song().instrument_nr, 63);
    saved(&doc);
    // undo of an add takes the instrument off again
    doc.undo().unwrap();
    assert_eq!(doc.song().instrument_nr, 62);
    doc.redo().unwrap();
    assert_eq!(doc.song().instrument_nr, 63);
}

#[test]
fn plist_rows_set_insert_delete_duplicate_clear_paste_transpose() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    op(&mut doc, json!({"op": "add_instrument"}));
    let r = op(
        &mut doc,
        json!({"op": "set_plist_row", "n": 1, "row": 0, "note": "C-2", "waveform": 3, "fixed": true, "fx0": 12, "param0": 40, "fx1": 15, "param1": 2}),
    );
    assert_eq!(
        r["changed"]["plists"],
        json!([{"instrument": 1, "row0": 0, "row1": 0}])
    );
    assert_eq!(
        doc.plist(1, 0, 1, false).unwrap()["rows"][0],
        json!([13, 3, 1, 12, 40, 15, 2])
    );
    op(
        &mut doc,
        json!({"op": "set_plist_row", "n": 1, "row": 0, "entry": [1, 2, 0, 0, 0, 0, 0]}),
    );
    assert_eq!(
        doc.plist(1, 0, 1, false).unwrap()["rows"][0],
        json!([1, 2, 0, 0, 0, 0, 0])
    );
    assert!(refused(
        &mut doc,
        json!({"op": "set_plist_row", "n": 1, "row": 0, "waveform": 5})
    )
    .contains("0 to 4"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_plist_row", "n": 1, "row": 0, "fx0": 6})
    )
    .contains("not available in an AHX"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_plist_row", "n": 1, "row": 0, "note": 64})
    )
    .contains("0 to 63"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_plist_row", "n": 1, "row": 1, "note": 1})
    )
    .contains("no PList row 1"));
    assert!(
        refused(&mut doc, json!({"op": "set_plist_row", "n": 1, "row": 0}))
            .contains("nothing to set")
    );
    assert!(refused(
        &mut doc,
        json!({"op": "set_plist_row", "n": 2, "row": 0, "note": 1})
    )
    .contains("no instrument"));

    // rows
    for n in 2..=5u8 {
        let end = doc.song().instruments[1].plist.entries.len();
        op(
            &mut doc,
            json!({"op": "insert_plist_row", "n": 1, "row": end, "entry": [n, 1, 0, 5, n, 0, 0]}),
        );
    }
    assert_eq!(doc.song().instruments[1].plist.entries.len(), 5);
    op(
        &mut doc,
        json!({"op": "insert_plist_row", "n": 1, "row": 2, "count": 2}),
    );
    let notes: Vec<u8> = doc.song().instruments[1]
        .plist
        .entries
        .iter()
        .map(|e| e.note)
        .collect();
    assert_eq!(notes, vec![1, 2, 0, 0, 3, 4, 5]);
    op(
        &mut doc,
        json!({"op": "delete_plist_row", "n": 1, "row": 2, "count": 2}),
    );
    op(
        &mut doc,
        json!({"op": "duplicate_plist_row", "n": 1, "row": 1}),
    );
    let notes: Vec<u8> = doc.song().instruments[1]
        .plist
        .entries
        .iter()
        .map(|e| e.note)
        .collect();
    assert_eq!(notes, vec![1, 2, 2, 3, 4, 5]);
    assert!(refused(
        &mut doc,
        json!({"op": "insert_plist_row", "n": 1, "row": 7})
    )
    .contains("no place"));
    assert!(refused(
        &mut doc,
        json!({"op": "insert_plist_row", "n": 1, "row": 0, "entry": [1, 9, 0, 0, 0, 0, 0]})
    )
    .contains("0 to 4"));
    assert!(refused(
        &mut doc,
        json!({"op": "delete_plist_row", "n": 1, "row": 6})
    )
    .contains("no PList row"));
    op(
        &mut doc,
        json!({"op": "clear_plist_range", "n": 1, "row0": 0, "row1": 1, "columns": ["note", "fx0"]}),
    );
    assert_eq!(
        doc.plist(1, 0, 2, false).unwrap()["rows"],
        json!([[0, 2, 0, 0, 0, 0, 0], [0, 1, 0, 0, 0, 0, 0]])
    );
    assert!(refused(
        &mut doc,
        json!({"op": "clear_plist_range", "n": 1, "row0": 0, "row1": 1, "columns": ["x"]})
    )
    .contains("not a column"));
    let clip = op(
        &mut doc,
        json!({"op": "copy_plist", "n": 1, "row0": 3, "row1": 5}),
    );
    assert_eq!(clip["rows"], 3);
    // pasted rows past the end are appended
    op(
        &mut doc,
        json!({"op": "paste_plist", "n": 1, "row": 5, "cells": clip["cells"].clone()}),
    );
    assert_eq!(doc.song().instruments[1].plist.entries.len(), 8);
    assert_eq!(doc.song().instruments[1].plist.entries[7].note, 5);
    op(
        &mut doc,
        json!({"op": "paste_plist", "n": 1, "row": 0, "cells": [[9, 4, 0, 5, 9, 0, 0]], "mode": "mix"}),
    );
    let e0 = doc.song().instruments[1].plist.entries[0];
    assert_eq!(
        (e0.note, e0.waveform, e0.fx[0], e0.fx_param[0]),
        (9, 2, 5, 9)
    );
    assert!(refused(
        &mut doc,
        json!({"op": "paste_plist", "n": 1, "row": 9, "cells": []})
    )
    .contains("no place"));
    // transpose
    let r = op(
        &mut doc,
        json!({"op": "transpose_plist", "n": 1, "row0": 0, "row1": 7, "semitones": 5}),
    );
    assert!(r["count"].as_u64().unwrap() >= 4);
    assert!(refused(
        &mut doc,
        json!({"op": "transpose_plist", "n": 1, "row0": 0, "row1": 7, "semitones": 60})
    )
    .contains("outside 1 to 63"));
    op(
        &mut doc,
        json!({"op": "transpose_plist", "n": 1, "row0": 0, "row1": 7, "semitones": 60, "clamp": true}),
    );
    // the limit of 255 rows
    let e = refused(
        &mut doc,
        json!({"op": "insert_plist_row", "n": 1, "row": 0, "count": 255}),
    );
    assert!(e.contains("255"), "{e}");
    op(
        &mut doc,
        json!({"op": "insert_plist_row", "n": 1, "row": 0, "count": 247}),
    );
    assert_eq!(doc.song().instruments[1].plist.entries.len(), 255);
    saved(&doc);
}

#[test]
fn plist_commands_follow_the_format_and_the_version() {
    let mut hvl = AhxDoc::new(SongFormat::Hvl);
    op(&mut hvl, json!({"op": "add_instrument"}));
    op(
        &mut hvl,
        json!({"op": "set_plist_row", "n": 1, "row": 0, "fx0": 6, "param0": 3, "fx1": 9, "param1": 200}),
    );
    saved(&hvl);
    // a version-0 AHX file keeps only the low nibble of a filter toggle's parameter
    let mut song = AhxDoc::new(SongFormat::Ahx).into_song();
    song.version = 0;
    let mut v0 = AhxDoc::from_song(song).unwrap();
    op(&mut v0, json!({"op": "add_instrument"}));
    let e = refused(
        &mut v0,
        json!({"op": "set_plist_row", "n": 1, "row": 0, "fx0": 4, "param0": 16}),
    );
    assert!(e.contains("version-0"), "{e}");
    op(
        &mut v0,
        json!({"op": "set_plist_row", "n": 1, "row": 0, "fx0": 4, "param0": 5}),
    );
    assert!(refused(
        &mut v0,
        json!({"op": "enable_sweep", "n": 1, "kind": "filter"})
    )
    .contains("version-0"));
}

#[test]
fn start_waveform_start_filter_and_sweeps() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    op(&mut doc, json!({"op": "add_instrument"}));
    op(
        &mut doc,
        json!({"op": "clear_plist_range", "n": 1, "row0": 0, "row1": 0}),
    );
    op(
        &mut doc,
        json!({"op": "delete_plist_row", "n": 1, "row": 0}),
    );
    assert!(doc.song().instruments[1].plist.entries.is_empty());
    // an empty PList gets a row 0
    op(
        &mut doc,
        json!({"op": "set_start_waveform", "n": 1, "waveform": 3}),
    );
    assert_eq!(doc.song().instruments[1].plist.entries.len(), 1);
    assert_eq!(doc.song().instruments[1].plist.entries[0].waveform, 3);
    assert!(refused(
        &mut doc,
        json!({"op": "set_start_waveform", "n": 1, "waveform": 5})
    )
    .contains("0 to 4"));
    op(
        &mut doc,
        json!({"op": "set_start_filter", "n": 1, "position": 20}),
    );
    assert_eq!(doc.song().instruments[1].plist.entries[0].fx_param[0], 20);
    op(
        &mut doc,
        json!({"op": "set_start_filter", "n": 1, "position": 0}),
    );
    assert_eq!(doc.song().instruments[1].plist.entries[0].fx_param[0], 0);
    assert!(refused(
        &mut doc,
        json!({"op": "set_start_filter", "n": 1, "position": 64})
    )
    .contains("0 to 63"));

    // sweeps: command 4 on row 0, filter 0x10 and square 0x00
    op(
        &mut doc,
        json!({"op": "enable_sweep", "n": 1, "kind": "filter"}),
    );
    let e = doc.song().instruments[1].plist.entries[0];
    assert_eq!((e.fx[0], e.fx_param[0]), (4, 0x10));
    let r = op(
        &mut doc,
        json!({"op": "enable_sweep", "n": 1, "kind": "filter"}),
    );
    assert_eq!(r["changed"], json!({}), "already on");
    op(
        &mut doc,
        json!({"op": "set_plist_row", "n": 1, "row": 0, "waveform": 2}),
    );
    op(
        &mut doc,
        json!({"op": "enable_sweep", "n": 1, "kind": "square"}),
    );
    let e = doc.song().instruments[1].plist.entries[0];
    assert_eq!(
        (e.fx[1], e.fx_param[1], e.waveform),
        (4, 0, 3),
        "square also selects the square wave"
    );
    // row 0 now has no free slot
    op(&mut doc, json!({"op": "add_instrument"}));
    op(
        &mut doc,
        json!({"op": "set_plist_row", "n": 2, "row": 0, "fx0": 1, "param0": 1, "fx1": 2, "param1": 1}),
    );
    assert!(refused(
        &mut doc,
        json!({"op": "enable_sweep", "n": 2, "kind": "square"})
    )
    .contains("no free command slot"));
    assert!(refused(
        &mut doc,
        json!({"op": "set_start_filter", "n": 2, "position": 3})
    )
    .contains("no free command slot"));
    assert!(refused(
        &mut doc,
        json!({"op": "enable_sweep", "n": 2, "kind": "bounce"})
    )
    .contains("not a sweep"));
    saved(&doc);
}

// ----- the writer after edits ----------------------------------------------------

#[test]
fn a_built_song_is_written_as_pinned() {
    for (format, expect) in [
        (SongFormat::Ahx, 0xc756ed929f412a02u64),
        (SongFormat::Hvl, 0xd31cfae68e338234u64),
    ] {
        let mut doc = AhxDoc::new(format);
        let ops = json!([
            {"op": "set_name", "name": "Pinned"},
            {"op": "add_instrument", "name": "Saw"},
            {"op": "add_instrument", "name": "Square", "wave_length": 4},
            {"op": "set_plist_row", "n": 2, "row": 0, "waveform": 3},
            {"op": "enable_sweep", "n": 2, "kind": "square"},
            {"op": "set_step", "track": 1, "row": 0, "cell": [25, 1, 0, 0]},
            {"op": "set_step", "track": 1, "row": 4, "cell": [27, 1, 12, 48]},
            {"op": "set_step", "track": 1, "row": 8, "cell": [30, 2, 0, 0]},
            {"op": "set_step", "position": 0, "channel": 1, "row": 0, "cell": [13, 2, 15, 3], "unique": true},
            {"op": "insert_position", "at": 1, "kind": "duplicate", "of": 0},
            {"op": "set_transpose", "position": 1, "channel": 0, "value": 5},
            {"op": "set_restart", "position": 1}
        ]);
        let r = op(&mut doc, json!({"op": "batch", "ops": ops}));
        assert_eq!(r["results"].as_array().unwrap().len(), 12);
        saved(&doc);
        let bytes = doc.to_bytes().unwrap();
        let h = fnv(&bytes);
        eprintln!("{format:?}: {} bytes, hash {h:#x}", bytes.len());
        assert_eq!(h, expect, "{format:?}: the written bytes changed");
        // and it is a song the engine plays
        let mut p = cubed_ahx::player::AhxPlayer::new(&bytes, 44100, 2).unwrap();
        p.play();
        let (mut l, mut r) = (vec![0.0f32; 4410], vec![0.0f32; 4410]);
        p.render(&mut l, &mut r);
        assert!(l.iter().any(|&x| x != 0.0), "{format:?}: silence");
    }
}

#[test]
fn from_bytes_to_bytes_is_the_file_for_every_unedited_fixture() {
    let mut same = 0;
    let mut total = 0;
    for entry in std::fs::read_dir(songs_dir()).unwrap().flatten() {
        let path = entry.path();
        let bytes = std::fs::read(&path).unwrap();
        // Songs wider than the engine plays have no document.
        let doc = match AhxDoc::from_bytes(&bytes) {
            Ok(d) => d,
            Err(e) => {
                assert!(e.contains("channels"), "{path:?}: {e}");
                continue;
            }
        };
        let out = doc.to_bytes().unwrap();
        total += 1;
        same += (out == bytes) as usize;
        assert_eq!(format::parse(&out).unwrap(), *doc.song(), "{path:?}");
    }
    eprintln!("{same} of {total}");
    assert!(same * 10 >= total * 8);
}

#[test]
fn an_hvl_song_converts_to_ahx_bytes_when_it_fits() {
    let mut doc = AhxDoc::new(SongFormat::Hvl);
    op(&mut doc, json!({"op": "add_instrument"}));
    op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 0, "cell": [13, 1, 0, 0, 0, 0]}),
    );
    let ahx = doc.to_ahx_bytes().unwrap();
    assert_eq!(&ahx[..3], b"THX");
    let back = format::parse(&ahx).unwrap();
    assert_eq!(back.format, SongFormat::Ahx);
    assert_eq!(back.tracks[1][0].note, 13);
    op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 1, "cell": [13, 1, 0, 0, 1, 1]}),
    );
    assert!(doc
        .to_ahx_bytes()
        .unwrap_err()
        .contains("second effect column"));
    // an AHX document is its own AHX bytes
    let a = AhxDoc::new(SongFormat::Ahx);
    assert_eq!(a.to_ahx_bytes().unwrap(), a.to_bytes().unwrap());
}

#[test]
fn a_song_that_would_outgrow_the_file_is_refused_and_a_full_one_can_still_shrink() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    for _ in 0..63 {
        op(&mut doc, json!({"op": "add_instrument"}));
    }
    // 63 instruments with 255 PList rows (1042 bytes each) do not fit: the one
    // that would not is refused and put back.
    let mut added = 0;
    for n in 1..=63 {
        match doc.apply(&json!({"op": "insert_plist_row", "n": n, "row": 0, "count": 254})) {
            Ok(_) => added += 1,
            Err(e) => {
                assert!(e.contains("of 65535 bytes"), "{e}");
                break;
            }
        }
    }
    assert!(added > 30 && added < 63, "{added}");
    let (used, max) = doc.size();
    assert!(used <= max);
    assert!(doc.info()["size"]["remaining"].as_i64().unwrap() >= 0);
    saved(&doc);
    // shrinking is never refused
    op(
        &mut doc,
        json!({"op": "delete_plist_row", "n": 1, "row": 0, "count": 100}),
    );
    // growing a track table past the limit is refused as well
    let mut tracks = 0;
    loop {
        match doc.apply(&json!({"op": "alloc_track", "append": true})) {
            Ok(_) => tracks += 1,
            Err(e) => {
                assert!(e.contains("bytes") || e.contains("256"), "{e}");
                break;
            }
        }
    }
    assert!(tracks < 255);
    saved(&doc);
}

#[test]
fn hvl_blank_steps_count_one_byte_and_a_step_that_overflows_is_refused() {
    let mut doc = AhxDoc::new(SongFormat::Hvl);
    let empty = doc.size().0;
    op(
        &mut doc,
        json!({"op": "set_step", "track": 1, "row": 0, "note": 1}),
    );
    assert_eq!(doc.size().0, empty + 4);
    saved(&doc);
}

// ----- history ---------------------------------------------------------------------

#[test]
fn undo_and_redo_walk_the_edits_with_groups() {
    let mut doc = ahx();
    let start = doc.song().clone();
    assert!(doc.undo().is_none());
    op(
        &mut doc,
        json!({"op": "set_step", "track": 3, "row": 2, "cell": [5, 1, 0, 0]}),
    );
    let one = doc.song().clone();
    doc.begin_group();
    op(
        &mut doc,
        json!({"op": "set_step", "track": 3, "row": 3, "note": 7}),
    );
    op(&mut doc, json!({"op": "set_name", "name": "Grouped"}));
    assert!(!doc.can_undo(), "no undo while a group is open");
    doc.end_group();
    let two = doc.song().clone();
    let c = doc.undo().unwrap();
    assert!(c.header && c.tracks.contains_key(&3));
    assert_eq!(doc.song(), &one);
    doc.undo().unwrap();
    assert_eq!(doc.song(), &start);
    assert!(doc.undo().is_none());
    let c = doc.redo().unwrap();
    assert_eq!(c.steps, vec![(3, 2)]);
    doc.redo().unwrap();
    assert_eq!(doc.song(), &two);
    assert!(doc.redo().is_none());
    // a new edit ends the redo line
    doc.undo().unwrap();
    op(
        &mut doc,
        json!({"op": "set_step", "track": 3, "row": 9, "note": 7}),
    );
    assert!(!doc.can_redo());
    // through JSON
    let r = op(&mut doc, json!({"op": "undo"}));
    assert_eq!(r["changed"]["tracks"][0]["track"], 3);
    op(&mut doc, json!({"op": "redo"}));
    assert!(doc
        .apply(&json!({"op": "redo"}))
        .unwrap_err()
        .contains("nothing to redo"));
    let info = doc.info();
    assert!(info["undo_steps"].as_u64().unwrap() >= 2);
    assert!(info["history_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn a_failed_batch_changes_nothing_and_a_good_one_is_one_step() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    let start = doc.song().clone();
    let e = doc
        .apply(&json!({"op": "batch", "ops": [
            {"op": "add_instrument"},
            {"op": "set_step", "track": 1, "row": 0, "note": 5},
            {"op": "set_step", "track": 1, "row": 99, "note": 5}
        ]}))
        .unwrap_err();
    assert!(e.contains("operation 2 (set_step) failed"), "{e}");
    assert_eq!(doc.song(), &start);
    assert!(!doc.can_undo());
    let r = op(
        &mut doc,
        json!({"op": "batch", "ops": [
            {"op": "add_instrument"},
            {"op": "set_step", "track": 1, "row": 0, "note": 5}
        ]}),
    );
    assert_eq!(r["changed"]["instrument_list"], true);
    assert_eq!(r["changed"]["tracks"][0]["track"], 1);
    doc.undo().unwrap();
    assert_eq!(doc.song(), &start, "one undo undoes the whole batch");
}

#[test]
fn the_history_is_bounded_in_steps_and_bytes() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    doc.set_history_limits(3, usize::MAX);
    for row in 0..10 {
        op(
            &mut doc,
            json!({"op": "set_step", "track": 1, "row": row, "note": 1}),
        );
    }
    assert_eq!(doc.info()["undo_steps"], 3);
    for _ in 0..3 {
        doc.undo().unwrap();
    }
    assert!(doc.undo().is_none());
    assert_eq!(
        doc.song().tracks[1][6].note,
        1,
        "only the last three were undone"
    );
    assert_eq!(doc.song().tracks[1][7].note, 0);

    let mut doc = AhxDoc::new(SongFormat::Ahx);
    doc.set_history_limits(1000, 1);
    for row in 0..10 {
        op(
            &mut doc,
            json!({"op": "set_step", "track": 1, "row": row, "note": 1}),
        );
    }
    assert_eq!(doc.info()["undo_steps"], 1, "the newest step always stays");
    doc.clear_history();
    assert!(!doc.can_undo());
}

/// A small deterministic generator for the soak.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// A random operation, sometimes invalid.
fn random_op(doc: &AhxDoc, rng: &mut Rng) -> Value {
    let s = doc.song();
    let tracks = s.tracks.len() + 1;
    let positions = s.positions.len() + 1;
    let instruments = s.instruments.len();
    let rows = s.track_length as usize + 1;
    let ch = s.channels + 1;
    let n = 1 + rng.below(instruments.max(1) + 1);
    let plist_rows = s.instruments.get(n).map_or(0, |i| i.plist.entries.len()) + 1;
    let hvl = s.format == SongFormat::Hvl;
    match rng.below(34) {
        0..=5 => json!({"op": "set_step", "track": rng.below(tracks), "row": rng.below(rows),
            "cell": [rng.below(62), rng.below(65), rng.below(16), rng.below(256),
                     if hvl { rng.below(16) } else { 0 }, if hvl { rng.below(256) } else { 0 }]}),
        6 => json!({"op": "set_step", "position": rng.below(positions), "channel": rng.below(ch),
            "row": rng.below(rows), "note": rng.below(61), "unique": true}),
        7 => {
            json!({"op": "clear_range", "track": rng.below(tracks), "row0": rng.below(rows), "row1": rng.below(rows)})
        }
        8 => {
            json!({"op": "insert_row", "track": rng.below(tracks), "row": rng.below(rows), "count": 1 + rng.below(3)})
        }
        9 => {
            json!({"op": "delete_row", "track": rng.below(tracks), "row": rng.below(rows), "count": 1 + rng.below(3)})
        }
        10 => {
            json!({"op": "transpose", "track": rng.below(tracks), "row0": rng.below(rows), "row1": rng.below(rows),
            "semitones": rng.below(25) as i64 - 12, "clamp": rng.below(2) == 0})
        }
        11 => json!({"op": "paste", "track": rng.below(tracks), "row": rng.below(rows),
            "cells": [[1 + rng.below(60), rng.below(4), 0, 0, 0, 0], [0, 0, 12, rng.below(65), 0, 0]],
            "mode": if rng.below(2) == 0 { "mix" } else { "overwrite" }}),
        12 => {
            json!({"op": "assign_track", "position": rng.below(positions), "channel": rng.below(ch), "track": rng.below(tracks)})
        }
        13 => {
            json!({"op": "set_transpose", "position": rng.below(positions), "channel": rng.below(ch), "value": rng.below(260) as i64 - 130})
        }
        14 => {
            json!({"op": "make_unique", "position": rng.below(positions), "channel": rng.below(ch)})
        }
        15 => {
            let kind = ["blank", "duplicate", "duplicate_unique"][rng.below(3)];
            json!({"op": "insert_position", "at": rng.below(positions + 1), "kind": kind, "of": rng.below(positions)})
        }
        16 => json!({"op": "delete_position", "at": rng.below(positions)}),
        17 => {
            json!({"op": "move_position", "from": rng.below(positions), "to": rng.below(positions)})
        }
        18 => json!({"op": "set_restart", "position": rng.below(positions)}),
        19 => {
            json!({"op": "set_track_length", "rows": 1 + rng.below(66), "force": rng.below(2) == 0})
        }
        20 => {
            json!({"op": "alloc_track", "append": rng.below(2) == 0, "copy_of": rng.below(tracks)})
        }
        21 => json!({"op": "compact_tracks"}),
        22 => json!({"op": "add_instrument", "volume": rng.below(70)}),
        23 => {
            json!({"op": "set_instrument", "n": n, "volume": rng.below(66), "wave_length": rng.below(7),
            "envelope": {"a_frames": rng.below(256), "d_volume": rng.below(66)}, "vibrato_depth": rng.below(17)})
        }
        24 => {
            let to = if rng.below(2) == 0 {
                Value::Null
            } else {
                json!(1 + rng.below(instruments.max(1)))
            };
            json!({"op": "copy_instrument", "from": n, "to": to})
        }
        25 => json!({"op": "clear_instrument", "n": n}),
        26 => {
            json!({"op": "set_plist_row", "n": n, "row": rng.below(plist_rows), "note": rng.below(65),
            "waveform": rng.below(6), "fx0": rng.below(16), "param0": rng.below(256)})
        }
        27 => {
            json!({"op": "insert_plist_row", "n": n, "row": rng.below(plist_rows + 1), "count": 1 + rng.below(4)})
        }
        28 => {
            json!({"op": "delete_plist_row", "n": n, "row": rng.below(plist_rows), "count": 1 + rng.below(3)})
        }
        29 => {
            json!({"op": "paste_plist", "n": n, "row": rng.below(plist_rows), "cells": [[3, 1, 0, 0, 0, 0, 0], [4, 2, 0, 5, 0, 0, 0]]})
        }
        30 => {
            json!({"op": "transpose_plist", "n": n, "row0": 0, "row1": rng.below(plist_rows), "semitones": rng.below(9) as i64 - 4, "clamp": true})
        }
        31 => {
            let kind = if rng.below(2) == 0 {
                "filter"
            } else {
                "square"
            };
            json!({"op": "enable_sweep", "n": n, "kind": kind})
        }
        32 => json!({"op": "set_name", "name": format!("song {}", rng.below(1000))}),
        _ => json!({"op": "rename_instrument", "n": n, "name": format!("ins {}", rng.below(100))}),
    }
}

/// Every operation kind on every starting point: a refused operation changes
/// nothing, every state that is reached saves (and reads back as itself), and
/// undoing everything gets back to the start and redoing it to the end.
#[test]
fn a_random_operation_soak_saves_undoes_and_redoes() {
    let starts: Vec<(&str, AhxDoc)> = vec![
        ("new ahx", AhxDoc::new(SongFormat::Ahx)),
        ("new hvl", AhxDoc::new(SongFormat::Hvl)),
        ("karma", ahx()),
        ("hvl", hvl()),
    ];
    for (name, start) in starts {
        let mut doc = start;
        let first = doc.song().clone();
        doc.set_history_limits(100_000, usize::MAX);
        let mut rng = Rng(0x9e3779b97f4a7c15 ^ name.len() as u64);
        let (mut ok, mut refused_count) = (0, 0);
        let mut steps = 0;
        for i in 0..1500 {
            let o = random_op(&doc, &mut rng);
            let before = doc.song().clone();
            let revision = doc.revision();
            let undo_steps = doc.info()["undo_steps"].as_u64().unwrap();
            let group = rng.below(6) == 0;
            if group {
                doc.begin_group();
            }
            match doc.apply(&o) {
                Ok(_) => {
                    ok += 1;
                    if !group && doc.revision() != revision {
                        steps += 1;
                        assert_eq!(
                            doc.info()["undo_steps"].as_u64().unwrap(),
                            undo_steps + 1,
                            "{name} {i}: {o}"
                        );
                    }
                }
                Err(e) => {
                    refused_count += 1;
                    assert_eq!(
                        doc.song(),
                        &before,
                        "{name} {i}: {o} was refused ({e}) but changed the song"
                    );
                    assert_eq!(doc.revision(), revision);
                }
            }
            if group {
                let _ = doc.apply(&json!({"op": "set_name", "name": "in a group"}));
                doc.end_group();
                steps += 1;
            }
            // Every state reached saves and reads back as itself.
            let bytes = doc
                .to_bytes()
                .unwrap_or_else(|e| panic!("{name} {i}: {o}: cannot be saved: {e}"));
            assert_eq!(
                format::parse(&bytes).unwrap(),
                *doc.song(),
                "{name} {i}: {o}"
            );
            let problems = doc.validate();
            assert!(problems.is_empty(), "{name} {i}: {o}: {problems:?}");
        }
        eprintln!("{name}: {ok} operations done, {refused_count} refused, {steps} undo steps");
        assert!(ok > 300, "{name}: too few operations went through ({ok})");
        let last = doc.song().clone();
        let mut undone = 0;
        while doc.undo().is_some() {
            undone += 1;
        }
        assert!(undone > 100);
        assert_eq!(
            doc.song(),
            &first,
            "{name}: undoing everything does not get back to the start"
        );
        let mut redone = 0;
        while doc.redo().is_some() {
            redone += 1;
        }
        assert_eq!(undone, redone);
        assert_eq!(
            doc.song(),
            &last,
            "{name}: redoing everything does not get back to the end"
        );
    }
}

/// A short random session on every fixture (and, with `CUBED_AHX_SONGS=<dir>`, on
/// every song below that directory): a refused edit changes nothing, every state
/// saves and reads back as itself, and undoing it all gets back to the song.
#[test]
fn a_short_soak_on_every_real_song() {
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = read.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                collect(&p, out);
            } else if matches!(p.extension().and_then(|e| e.to_str()), Some("ahx" | "hvl")) {
                out.push(p);
            }
        }
    }
    let mut paths = Vec::new();
    collect(&songs_dir(), &mut paths);
    if let Some(extra) = std::env::var_os("CUBED_AHX_SONGS") {
        collect(Path::new(&extra), &mut paths);
    }
    let (mut done, mut skipped, mut ops_ok) = (0, 0, 0);
    for (n, path) in paths.iter().enumerate() {
        let bytes = std::fs::read(path).unwrap();
        let Ok(mut doc) = AhxDoc::from_bytes(&bytes) else {
            skipped += 1;
            continue;
        };
        let first = doc.song().clone();
        let mut rng = Rng(0x2545f4914f6cdd1d ^ n as u64);
        for i in 0..80 {
            let o = random_op(&doc, &mut rng);
            let before = doc.song().clone();
            match doc.apply(&o) {
                Ok(_) => ops_ok += 1,
                Err(e) => assert_eq!(
                    doc.song(),
                    &before,
                    "{path:?} {i}: {o} was refused ({e}) but changed the song"
                ),
            }
        }
        let bytes = doc
            .to_bytes()
            .unwrap_or_else(|e| panic!("{path:?}: cannot be saved: {e}"));
        assert_eq!(format::parse(&bytes).unwrap(), *doc.song(), "{path:?}");
        let last = doc.song().clone();
        while doc.undo().is_some() {}
        assert_eq!(doc.song(), &first, "{path:?}: undo");
        while doc.redo().is_some() {}
        assert_eq!(doc.song(), &last, "{path:?}: redo");
        done += 1;
    }
    eprintln!("{done} songs edited ({ops_ok} operations), {skipped} wider than the engine plays");
    assert!(done >= 100);
}

#[test]
fn undo_returns_what_to_redraw() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    op(&mut doc, json!({"op": "add_instrument"}));
    op(
        &mut doc,
        json!({"op": "set_plist_row", "n": 1, "row": 0, "note": 9}),
    );
    let c = doc.undo().unwrap();
    assert_eq!(c.instruments, vec![1]);
    assert_eq!(c.plists.get(&1), Some(&(0, 0)));
    let c = doc.undo().unwrap();
    assert!(c.instrument_list);
    let c = doc.redo().unwrap();
    assert!(c.instrument_list);
    assert_eq!(doc.song().instrument_nr, 1);
    let _ = InsertKind::Blank;
}

// ----- what the controls draw ----------------------------------------------------

#[test]
fn the_instrument_visuals_come_from_the_replayer() {
    let mut doc = AhxDoc::new(SongFormat::Ahx);
    op(&mut doc, json!({"op": "add_instrument"}));
    // The default envelope: attack 1 frame to 64, decay 10 to 48, no sustain, release 10 to 0.
    let c = op(&mut doc, json!({"op": "envelope_curve", "n": 1}));
    let v: Vec<f64> = c["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    assert_eq!(v.len(), 21);
    assert_eq!(v[0], 64.0);
    assert_eq!(v[10], 48.0);
    assert_eq!(*v.last().unwrap(), 0.0);
    assert_eq!(
        (
            c["attack"].clone(),
            c["decay"].clone(),
            c["sustain"].clone(),
            c["release"].clone()
        ),
        (json!(1), json!(11), json!(11), json!(21))
    );
    assert!(doc.apply(&json!({"op": "envelope_curve", "n": 2})).is_err());

    // Waveforms: a cycle of 4 << wave_length samples.
    for wl in 0..=5usize {
        for w in 1..=4usize {
            let s = op(
                &mut doc,
                json!({"op": "waveform_shape", "waveform": w, "wave_length": wl, "square_pos": 20}),
            );
            assert_eq!(
                s["samples"].as_array().unwrap().len(),
                4 << wl,
                "waveform {w} length {wl}"
            );
        }
    }
    let saw = op(
        &mut doc,
        json!({"op": "waveform_shape", "waveform": 2, "wave_length": 3}),
    );
    let saw: Vec<i64> = saw["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_i64().unwrap())
        .collect();
    assert_eq!(saw[0], -128);
    assert!(saw.windows(2).all(|w| w[1] > w[0]));
    let square = op(
        &mut doc,
        json!({"op": "waveform_shape", "waveform": 3, "wave_length": 3, "square_pos": 8}),
    );
    let square: Vec<i64> = square["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_i64().unwrap())
        .collect();
    assert_eq!(
        square.iter().filter(|&&x| x > 0).count(),
        square.iter().filter(|&&x| x < 0).count(),
        "a 50% duty"
    );
    assert!(doc
        .apply(&json!({"op": "waveform_shape", "waveform": 5, "wave_length": 0}))
        .is_err());
    assert!(doc
        .apply(&json!({"op": "waveform_shape", "waveform": 1, "wave_length": 6}))
        .is_err());

    // A filter sweep heads for its upper limit, bounces and stays inside its limits.
    op(
        &mut doc,
        json!({"op": "set_instrument", "n": 1, "filter_lower_limit": 10, "filter_upper_limit": 40, "filter_speed": 4,
        "square_lower_limit": 8, "square_upper_limit": 40, "square_speed": 1, "wave_length": 5}),
    );
    let t = op(
        &mut doc,
        json!({"op": "sweep_trace", "n": 1, "kind": "filter", "frames": 400}),
    );
    let p: Vec<i64> = t["positions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_i64().unwrap())
        .collect();
    assert_eq!(p[0], 32);
    assert_eq!(
        (t["lower"].clone(), t["upper"].clone()),
        (json!(10), json!(40))
    );
    assert!(p.iter().all(|&x| (10..=40).contains(&x)), "{p:?}");
    assert!(p.contains(&10) && p.contains(&40));
    let t = op(
        &mut doc,
        json!({"op": "sweep_trace", "n": 1, "kind": "square", "frames": 400, "start": 10}),
    );
    let p: Vec<i64> = t["positions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_i64().unwrap())
        .collect();
    assert!(p.iter().all(|&x| (8..=40).contains(&x)));
    assert!(p.len() == 401 && p.iter().max() != p.iter().min());

    // Vibrato: still through its delay, then the table times the depth.
    op(
        &mut doc,
        json!({"op": "set_instrument", "n": 1, "vibrato_delay": 3, "vibrato_depth": 8, "vibrato_speed": 8}),
    );
    let v = op(
        &mut doc,
        json!({"op": "vibrato_curve", "n": 1, "frames": 8}),
    );
    assert_eq!(v["offsets"], json!([0, 0, 0, 0, 11, 15, 11, 0]));
}
