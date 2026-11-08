//! The A2M edit layer: operations one by one (and their refusals), the compact
//! shapes, undo and redo, the writers on every fixture, live edits against a
//! playing song, and a random-operation soak.

use cubed_opl::a2::model::{parse, A2mSong, Cell};
use cubed_opl::a2::player::A2Player;
use cubed_opl::a2::write::{output_version, write};
use cubed_opl::edit::{A2mDoc, Region};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/a2m")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("a2m")) {
            out.push(p);
        }
    }
}

fn corpus_in(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    walk(dir, &mut v);
    v.sort();
    v
}

/// Every fixture, and every file of `CUBED_A2M=<dir>` (real songs kept out of git).
fn corpus() -> Vec<PathBuf> {
    let mut v = corpus_in(&corpus_root());
    if let Some(dir) = std::env::var_os("CUBED_A2M") {
        v.extend(corpus_in(Path::new(&dir)));
    }
    v
}

/// A fixture by its file name (one per version, small).
fn named(file: &str) -> Vec<u8> {
    let found = corpus_in(&corpus_root())
        .into_iter()
        .find(|p| p.file_name().is_some_and(|n| n == file))
        .unwrap_or_else(|| panic!("no fixture {file}"));
    std::fs::read(found).unwrap()
}

/// One small song of each version the corpus has.
const BY_VERSION: [(&str, u8); 8] = [
    ("worms.a2m", 1),
    ("imadick.a2m", 5),
    ("song103.a2m", 9),
    ("psychedelic sound synthesis.a2m", 10),
    ("battletoads briefing theme.a2m", 11),
    ("phone.a2m", 12),
    ("reflections.a2m", 13),
    ("andromeda.a2m", 14),
];

fn doc(file: &str) -> A2mDoc {
    A2mDoc::from_bytes(&named(file)).unwrap()
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn ok(d: &mut A2mDoc, op: Value) -> Value {
    match d.apply(&op) {
        Ok(v) => v,
        Err(e) => panic!("{op} failed: {e}"),
    }
}

/// Runs `op`, expects a refusal naming `word`, and checks nothing changed.
fn refused(d: &mut A2mDoc, op: Value, word: &str) {
    let before = d.song().clone();
    let revision = d.revision();
    let e = d.apply(&op).expect_err(&format!("{op} should fail"));
    assert!(e.contains(word), "{op}: '{e}' lacks '{word}'");
    assert_eq!(*d.song(), before, "a refused {op} changed the song");
    assert_eq!(d.revision(), revision, "a refused {op} counted as a change");
}

fn cell(d: &A2mDoc, p: usize, r: usize, c: usize) -> Vec<u64> {
    let v = d.rows(p, r, 1, false).unwrap();
    v["cells"][0][c]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap())
        .collect()
}

// ---------------------------------------------------------------------------
// the writer

/// What `A2mDoc::to_bytes` promises: the saved file loads back to the song that
/// was saved (the version aside: 1 and 5 are written as the stored 4 and 8), and
/// saving that again gives the same bytes.
fn check_round_trip(name: &str, bytes: &[u8]) -> bool {
    let Ok(raw) = parse(bytes) else {
        return false;
    };
    let d = A2mDoc::from_bytes(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    let top = raw
        .order
        .iter()
        .filter(|&&o| o < 0x80)
        .max()
        .map_or(0, |&o| o as usize);
    let promoted = top >= raw.patterns.len();
    if !promoted {
        assert_eq!(*d.song(), raw, "{name}: loading changed the song");
    }
    let written = d.to_bytes().unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut want = d.song().clone();
    want.version = output_version(want.version);
    let again = parse(&written).unwrap_or_else(|e| panic!("{name}: rewritten file: {e}"));
    assert_eq!(again, want, "{name}: load(write(load(x))) != load(x)");
    let twice = write(&again).unwrap();
    assert_eq!(twice, written, "{name}: the bytes are not stable");
    assert!(
        d.validate_content().is_empty(),
        "{name}: {:?}",
        d.validate_content()
    );
    promoted
}

#[test]
fn every_fixture_loads_saves_and_loads_back_the_same_song() {
    let mut promoted = 0;
    let mut n = 0;
    for path in corpus() {
        let bytes = std::fs::read(&path).unwrap();
        if check_round_trip(&path.display().to_string(), &bytes) {
            promoted += 1;
        }
        n += 1;
    }
    assert!(n >= 266, "{n} songs");
    // Songs whose order list names patterns past the file's count: shown, not lost.
    println!("{n} songs, {promoted} with patterns the order names past the file's count");
    assert!(promoted > 0);
}

#[test]
fn a_song_whose_order_names_missing_patterns_shows_them() {
    let bytes = named("cracker.a2m");
    let raw = parse(&bytes).unwrap();
    let top = raw
        .order
        .iter()
        .filter(|&&o| o < 0x80)
        .max()
        .copied()
        .unwrap() as usize;
    assert!(top >= raw.patterns.len(), "the fixture needs the case");
    let d = A2mDoc::from_bytes(&bytes).unwrap();
    assert_eq!(d.song().patterns.len(), top + 1);
    // They can be read and edited like any pattern.
    d.rows(top, 0, 1, false).unwrap();
    assert!(d.refusal().is_none());
}

#[test]
fn the_written_bytes_are_pinned_for_new_songs() {
    // A song built by operations, written by the aPLib and LZH encoders: a change
    // to the model, the writer or the encoders shows here.
    let build = |opl3: bool| {
        let mut d = A2mDoc::new(opl3);
        ok(
            &mut d,
            json!({"op": "batch", "ops": [
                {"op": "set_title", "title": "Pinned"},
                {"op": "set_tempo", "tempo": 70},
                {"op": "set_instrument", "instrument": 2, "name": "Bell",
                 "fm": [1, 50, 29, 8, 163, 197, 75, 102, 2, 5, 13], "panning": 2, "finetune": -3},
                {"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": "C-4", "instrument": 1},
                {"op": "set_cell", "pattern": 0, "row": 4, "channel": 0, "note": "E-4", "instrument": 2, "effect": "F", "param": 3},
                {"op": "set_cell", "pattern": 0, "row": 8, "channel": 1, "note": "###", "effect2": "C", "param2": 20},
                {"op": "new_pattern"},
                {"op": "insert_order", "index": 1, "pattern": 1},
                {"op": "set_arpeggio_macro", "macro": 1, "length": 3, "entries": [{"entry": 2, "value": 4}, {"entry": 3, "value": 7}]},
            ]}),
        );
        d.to_bytes().unwrap()
    };
    let (a, b) = (build(false), build(true));
    println!("pins: {:#018x} {:#018x}", fnv(&a), fnv(&b));
    assert_eq!(fnv(&a), PIN_OPL2, "OPL2 song");
    assert_eq!(fnv(&b), PIN_OPL3, "OPL3 song");
    // And they play.
    for bytes in [a, b] {
        let mut p = A2Player::new(&bytes, 48_000.0).unwrap();
        p.play();
        let (mut l, mut r) = (vec![0f32; 4096], vec![0f32; 4096]);
        p.render(&mut l, &mut r);
        assert!(l.iter().any(|&v| v != 0.0));
    }
}

const PIN_OPL2: u64 = 0x19de8c3b90066f91;
const PIN_OPL3: u64 = 0xafae0df5fc36a08e;

// ---------------------------------------------------------------------------
// reading

#[test]
fn info_states_the_versions_limits() {
    for (file, version) in BY_VERSION {
        let d = doc(file);
        let i = d.info();
        assert_eq!(i["version"], version, "{file}");
        assert_eq!(i["format"], "a2m");
        assert_eq!(i["note"]["octave_offset"], 1);
        let want = if version >= 9 { 255 } else { 250 };
        assert_eq!(i["limits"]["instruments"], want, "{file}");
        assert_eq!(i["instruments"], want, "{file}");
        assert_eq!(i["limits"]["two_effects"], version >= 9, "{file}");
        assert_eq!(i["limits"]["macros"], version >= 9, "{file}");
        assert_eq!(i["can_undo"], false);
        assert!(i["playable"].is_null(), "{file}: {}", i["playable"]);
        assert_eq!(i["file_version"], output_version(version));
    }
}

#[test]
fn rows_have_the_compact_shape() {
    let d = doc("andromeda.a2m");
    let v = d.rows(0, 0, 4, false).unwrap();
    assert_eq!(v["count"], 4);
    assert_eq!(v["rows"], 72, "the song's pattern length");
    assert_eq!(v["channels"], 18);
    assert_eq!(v["cells"][0][0].as_array().unwrap().len(), 6);
    let flat = d.rows(0, 0, 4, true).unwrap();
    assert_eq!(flat["cells"].as_array().unwrap().len(), 4 * 18 * 6);
    // Rows past the end are left out.
    let tail = d.rows(0, 70, 10, false).unwrap();
    assert_eq!(tail["count"], 2);
    assert!(d
        .rows(999, 0, 1, false)
        .unwrap_err()
        .contains("does not exist"));
}

#[test]
fn cells_show_notes_as_midi_and_fixed_notes_apart() {
    // The first pattern of a song with notes: C-4 is 60, a key off 255, and a
    // fixed note is the note plus 128; the file's note byte is never lost.
    let d = doc("andromeda.a2m");
    let song = d.song();
    let mut seen_fixed = false;
    let mut seen_note = false;
    for (p, pat) in song.patterns.iter().enumerate() {
        for r in 0..d.pattern_len() {
            for c in 0..d.tracks() {
                let raw = pat.cell(r, c).note;
                let shown = cell(&d, p, r, c)[0];
                match raw {
                    0 => assert_eq!(shown, 0),
                    255 => assert_eq!(shown, 255),
                    1..=97 => {
                        assert_eq!(shown, raw as u64 + 11);
                        seen_note = true;
                    }
                    0x91..=0xf1 => {
                        assert_eq!(shown, (raw - 0x90) as u64 + 11 + 128);
                        seen_fixed = true;
                    }
                    other => assert_eq!(shown, 256 + other as u64),
                }
            }
        }
    }
    assert!(seen_note);
    println!("fixed notes in andromeda: {seen_fixed}");
}

#[test]
fn the_order_list_shows_its_patterns_and_where_it_restarts() {
    let d = doc("andromeda.a2m");
    let o = d.order();
    let orders = o["orders"].as_array().unwrap();
    assert!(!orders.is_empty());
    assert_eq!(d.info()["orders"], orders.len());
    assert!(o["restart"].is_u64(), "a loop marker follows the patterns");
    assert_eq!(o["rows"].as_array().unwrap().len(), orders.len());
    assert!(o["rows"].as_array().unwrap().iter().all(|r| *r == 72));
}

// ---------------------------------------------------------------------------
// cells

#[test]
fn set_cell_takes_numbers_text_and_the_second_column() {
    let mut d = A2mDoc::new(true);
    let r = ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 5, "channel": 1, "note": "C-4", "instrument": 1,
               "effect": "F", "param": 6, "effect2": "C", "param2": 32}),
    );
    assert_eq!(r["cell"], json!([60, 1, 15, 6, 12, 32]));
    assert_eq!(
        r["changed"]["patterns"][0],
        json!({"pattern": 0, "row0": 5, "row1": 5})
    );
    // Fields not given stay.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 5, "channel": 1, "note": 62}),
    );
    assert_eq!(cell(&d, 0, 5, 1), [62, 1, 15, 6, 12, 32]);
    // A new effect clears the parameter unless one comes with it.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 5, "channel": 1, "effect": "A"}),
    );
    assert_eq!(cell(&d, 0, 5, 1), [62, 1, 10, 0, 12, 32]);
    // A key off, then a whole cell, then a fixed note.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 5, "channel": 1, "note": "###"}),
    );
    assert_eq!(cell(&d, 0, 5, 1)[0], 255);
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 6, "channel": 0, "cell": [72, 2, 0, 0, 0, 0]}),
    );
    assert_eq!(cell(&d, 0, 6, 0), [72, 2, 0, 0, 0, 0]);
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 6, "channel": 0, "note": 72, "fixed": true}),
    );
    assert_eq!(cell(&d, 0, 6, 0)[0], 72 + 128);
    assert_eq!(
        d.song().patterns[0].cell(6, 0).note,
        0x90 + 61,
        "stored as the file holds it"
    );
    // The edits are in the song's own numbers.
    assert_eq!(d.song().patterns[0].cell(5, 1).note, 255);
    // The text a grid shows.
    assert_eq!(
        ok(&mut d, json!({"op": "note_name", "note": 60}))["text"],
        "C-4"
    );
    assert_eq!(
        ok(
            &mut d,
            json!({"op": "effect_text", "effect": 15, "param": 6})
        )["text"],
        "F06"
    );
}

#[test]
fn set_cell_refusals() {
    let mut d = A2mDoc::new(false);
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 64, "channel": 0, "note": 60}),
        "past the end",
    );
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 9, "note": 60}),
        "does not exist",
    );
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 3, "row": 0, "channel": 0, "note": 60}),
        "pattern 3 does not exist",
    );
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": 11}),
        "not an Adlib Tracker II note",
    );
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": "H-4"}),
        "not a note",
    );
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "effect": "?"}),
        "not an effect letter",
    );
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "effect": 48}),
        "does not exist",
    );
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "instrument": 256}),
        "0-255",
    );
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "param": 300}),
        "0-255",
    );
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "fixed": true}),
        "needs a note",
    );
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "cell": [60, 1, 0]}),
        "a cell is",
    );
    refused(&mut d, json!({"op": "nonsense"}), "unknown operation");
    refused(&mut d, json!({}), "'op'");
    // Versions before 9 have one column, fewer effects, no fixed notes.
    let mut v1 = doc("worms.a2m");
    refused(
        &mut v1,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "effect2": "C", "param2": 1}),
        "one effect column",
    );
    refused(
        &mut v1,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "effect": "G"}),
        "does not exist in version 1",
    );
    refused(
        &mut v1,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": 60, "fixed": true}),
        "no fixed notes",
    );
    refused(
        &mut v1,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 9, "note": 60}),
        "does not exist",
    );
    ok(
        &mut v1,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "effect": "F", "param": 2}),
    );
    let mut v5 = doc("imadick.a2m");
    ok(
        &mut v5,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 17, "effect": "Z", "param": 1}),
    );
    refused(
        &mut v5,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "effect": 48}),
        "does not exist in version 5",
    );
    refused(
        &mut v5,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "effect": "F", "param": 1, "effect2": "F", "param2": 1}),
        "one effect column",
    );
    // 255 instruments only from version 9.
    refused(
        &mut v5,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "instrument": 251}),
        "does not exist",
    );
    let mut v14 = doc("andromeda.a2m");
    ok(
        &mut v14,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "instrument": 255, "effect": "<", "param": 1}),
    );
}

#[test]
fn clear_insert_delete_and_transpose() {
    let mut d = A2mDoc::new(true);
    for (row, note) in [(0, "C-4"), (1, "D-4"), (2, "E-4"), (3, "F-4")] {
        ok(
            &mut d,
            json!({"op": "set_cell", "pattern": 0, "row": row, "channel": 0, "note": note, "instrument": 1, "effect": "C", "param": 8}),
        );
    }
    ok(
        &mut d,
        json!({"op": "insert_row", "pattern": 0, "row": 1, "count": 2, "channel": 0}),
    );
    assert_eq!(cell(&d, 0, 1, 0)[0], 0);
    assert_eq!(cell(&d, 0, 3, 0)[0], 62, "D-4 moved down two rows");
    ok(
        &mut d,
        json!({"op": "delete_row", "pattern": 0, "row": 1, "count": 2, "channel": 0}),
    );
    assert_eq!(cell(&d, 0, 1, 0)[0], 62);
    // Rows pushed off the end are lost.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 63, "channel": 0, "note": "C-5"}),
    );
    ok(
        &mut d,
        json!({"op": "insert_row", "pattern": 0, "row": 0, "channel": 0}),
    );
    assert_eq!(cell(&d, 0, 63, 0)[0], 0);
    ok(
        &mut d,
        json!({"op": "delete_row", "pattern": 0, "row": 0, "channel": 0}),
    );
    // Clear columns.
    ok(
        &mut d,
        json!({"op": "clear_range", "pattern": 0, "row0": 0, "row1": 3, "channel": 0, "columns": ["effect", "instrument"]}),
    );
    assert_eq!(cell(&d, 0, 0, 0), [60, 0, 0, 0, 0, 0]);
    // Transpose.
    let r = ok(
        &mut d,
        json!({"op": "transpose", "pattern": 0, "row0": 0, "row1": 3, "channel": 0, "semitones": 12}),
    );
    assert_eq!(r["count"], 4);
    assert_eq!(cell(&d, 0, 0, 0)[0], 72);
    refused(
        &mut d,
        json!({"op": "transpose", "pattern": 0, "row0": 0, "row1": 3, "channel": 0, "semitones": 100}),
        "out of C-0 to B-7",
    );
    let r = ok(
        &mut d,
        json!({"op": "transpose", "pattern": 0, "row0": 0, "row1": 3, "channel": 0, "semitones": 100, "clamp": true}),
    );
    assert_eq!(r["count"], 4);
    assert_eq!(cell(&d, 0, 0, 0)[0], 107, "held at B-7");
    // Fixed notes and key offs stay.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 4, "channel": 0, "note": 60, "fixed": true}),
    );
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 5, "channel": 0, "note": "###"}),
    );
    ok(
        &mut d,
        json!({"op": "transpose", "pattern": 0, "row0": 4, "row1": 5, "channel": 0, "semitones": -2}),
    );
    assert_eq!(cell(&d, 0, 4, 0)[0], 60 + 128);
    assert_eq!(cell(&d, 0, 5, 0)[0], 255);
    refused(
        &mut d,
        json!({"op": "insert_row", "pattern": 0, "row": 0, "count": 0}),
        "at least 1",
    );
    refused(
        &mut d,
        json!({"op": "clear_range", "pattern": 0, "columns": ["volume"]}),
        "not a column",
    );
}

#[test]
fn copy_and_paste_round_trip_and_mix() {
    let mut d = A2mDoc::new(true);
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": "C-4", "instrument": 1, "effect": "F", "param": 3}),
    );
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 1, "channel": 1, "note": "###", "effect2": "C", "param2": 9}),
    );
    let clip = ok(
        &mut d,
        json!({"op": "copy_range", "pattern": 0, "row0": 0, "row1": 1, "channel0": 0, "channel1": 1}),
    );
    assert_eq!(clip["rows"], 2);
    assert_eq!(clip["channels"], 2);
    let r = ok(
        &mut d,
        json!({"op": "paste", "pattern": 0, "row": 10, "channel": 3, "cells": clip}),
    );
    assert_eq!(r["clipped"], false);
    assert_eq!(cell(&d, 0, 10, 3), [60, 1, 15, 3, 0, 0]);
    assert_eq!(cell(&d, 0, 11, 4), [255, 0, 0, 0, 12, 9]);
    // A paste past the end is cut, and says so.
    let r = ok(
        &mut d,
        json!({"op": "paste", "pattern": 0, "row": 63, "channel": 17, "cells": clip}),
    );
    assert_eq!(r["clipped"], true);
    assert_eq!(
        (r["rows"].as_u64(), r["channels"].as_u64()),
        (Some(1), Some(1))
    );
    // Mix fills only empty fields.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 20, "channel": 0, "note": "G-4"}),
    );
    ok(
        &mut d,
        json!({"op": "paste", "pattern": 0, "row": 20, "channel": 0, "cells": clip, "mode": "mix"}),
    );
    assert_eq!(
        cell(&d, 0, 20, 0),
        [67, 1, 15, 3, 0, 0],
        "the note stays, the rest fills in"
    );
    refused(
        &mut d,
        json!({"op": "paste", "pattern": 0, "row": 0, "channel": 0, "cells": []}),
        "empty",
    );
    refused(
        &mut d,
        json!({"op": "paste", "pattern": 0, "row": 0, "channel": 0}),
        "needs 'cells'",
    );
    refused(
        &mut d,
        json!({"op": "paste", "pattern": 0, "row": 0, "channel": 0, "cells": clip, "mode": "x"}),
        "not a paste mode",
    );
    refused(
        &mut d,
        json!({"op": "paste", "pattern": 0, "row": 99, "channel": 0, "cells": clip}),
        "outside the pattern",
    );
    // A cell the version cannot hold refuses the paste.
    let mut v1 = doc("worms.a2m");
    refused(
        &mut v1,
        json!({"op": "paste", "pattern": 0, "row": 0, "channel": 0, "cells": clip}),
        "version 1",
    );
}

// ---------------------------------------------------------------------------
// patterns and orders

#[test]
fn pattern_operations_keep_orders_names_and_blocks_in_step() {
    let mut d = A2mDoc::new(true);
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": "C-4"}),
    );
    let n = ok(&mut d, json!({"op": "new_pattern"}));
    assert_eq!(n["pattern"], 1);
    let n = ok(&mut d, json!({"op": "duplicate_pattern", "pattern": 0}));
    assert_eq!(n["pattern"], 2);
    assert_eq!(cell(&d, 2, 0, 0)[0], 60);
    assert_eq!(d.info()["patterns"], 3);
    // The file's last block always holds 8 patterns.
    assert_eq!(d.song().spare_patterns.len(), 5);
    ok(
        &mut d,
        json!({"op": "set_orders", "orders": [0, 1, 2, 1], "restart": 2}),
    );
    ok(
        &mut d,
        json!({"op": "set_pattern_name", "pattern": 2, "name": "Chorus"}),
    );
    assert_eq!(d.order()["names"][2], "Chorus");
    // Deleting pattern 1 removes its orders and renumbers the rest.
    ok(&mut d, json!({"op": "delete_pattern", "pattern": 1}));
    let o = d.order();
    assert_eq!(o["orders"], json!([0, 1]), "pattern 2 is now 1");
    assert_eq!(o["restart"], 1, "the jump target followed its position");
    assert_eq!(o["names"][1], "Chorus");
    assert_eq!(cell(&d, 1, 0, 0)[0], 60);
    assert_eq!(d.song().spare_patterns.len(), 6);
    // The last order's pattern cannot go.
    ok(
        &mut d,
        json!({"op": "set_orders", "orders": [0], "restart": 0}),
    );
    refused(
        &mut d,
        json!({"op": "delete_pattern", "pattern": 0}),
        "only one the order list plays",
    );
    ok(&mut d, json!({"op": "delete_pattern", "pattern": 1}));
    refused(
        &mut d,
        json!({"op": "delete_pattern", "pattern": 0}),
        "at least one pattern",
    );
    refused(
        &mut d,
        json!({"op": "delete_pattern", "pattern": 5}),
        "does not exist",
    );
    // Up to 128 patterns in version 14, 64 before.
    for _ in 0..127 {
        ok(&mut d, json!({"op": "new_pattern"}));
    }
    refused(&mut d, json!({"op": "new_pattern"}), "holds 128 patterns");
    let mut v1 = doc("worms.a2m");
    for _ in v1.song().patterns.len()..64 {
        ok(&mut v1, json!({"op": "new_pattern"}));
    }
    refused(&mut v1, json!({"op": "new_pattern"}), "holds 64 patterns");
    assert!(v1.validate().is_empty());
    check_round_trip("128 patterns", &d.to_bytes().unwrap());
    check_round_trip("64 patterns v1", &v1.to_bytes().unwrap());
    refused(
        &mut v1,
        json!({"op": "set_pattern_name", "pattern": 0, "name": "x"}),
        "version 11",
    );
}

#[test]
fn order_operations() {
    let mut d = A2mDoc::new(true);
    for _ in 0..3 {
        ok(&mut d, json!({"op": "new_pattern"}));
    }
    ok(
        &mut d,
        json!({"op": "set_orders", "orders": [0, 1, 2, 3], "restart": 1}),
    );
    ok(
        &mut d,
        json!({"op": "insert_order", "index": 2, "pattern": 0}),
    );
    assert_eq!(d.order()["orders"], json!([0, 1, 0, 2, 3]));
    assert_eq!(d.order()["restart"], 1);
    ok(
        &mut d,
        json!({"op": "insert_order", "index": 0, "pattern": 3}),
    );
    assert_eq!(d.order()["restart"], 2, "the restart kept its place");
    ok(&mut d, json!({"op": "delete_order", "index": 0}));
    assert_eq!(d.order()["restart"], 1);
    ok(&mut d, json!({"op": "move_order", "from": 4, "to": 0}));
    assert_eq!(d.order()["orders"], json!([3, 0, 1, 0, 2]));
    assert_eq!(
        d.order()["restart"],
        2,
        "the restart's order moved with its neighbours"
    );
    ok(&mut d, json!({"op": "set_order", "index": 1, "pattern": 2}));
    ok(&mut d, json!({"op": "set_restart", "order": 4}));
    assert_eq!(d.order()["restart"], 4);
    assert_eq!(
        d.song().order[5],
        0x80 + 4,
        "a jump marker follows the patterns"
    );
    assert_eq!(d.song().order[127], 0x80 + 4);
    assert_eq!(d.refusal(), None);
    refused(
        &mut d,
        json!({"op": "set_order", "index": 5, "pattern": 0}),
        "does not exist",
    );
    refused(
        &mut d,
        json!({"op": "set_order", "index": 0, "pattern": 9}),
        "pattern 9 does not exist",
    );
    refused(
        &mut d,
        json!({"op": "set_order", "index": 0, "pattern": 200}),
        "past the 128",
    );
    refused(
        &mut d,
        json!({"op": "insert_order", "index": 9, "pattern": 0}),
        "past the end",
    );
    refused(
        &mut d,
        json!({"op": "set_restart", "order": 5}),
        "does not exist",
    );
    refused(
        &mut d,
        json!({"op": "move_order", "from": 0, "to": 9}),
        "does not exist",
    );
    refused(
        &mut d,
        json!({"op": "set_orders", "orders": [0, 1], "restart": 2}),
        "past the song",
    );
    refused(
        &mut d,
        json!({"op": "set_orders", "orders": [], "restart": 0}),
        "at least one order",
    );
    ok(
        &mut d,
        json!({"op": "set_orders", "orders": [0], "restart": 0}),
    );
    refused(
        &mut d,
        json!({"op": "delete_order", "index": 0}),
        "at least one order",
    );
    // A full list of 128 has no room for a marker; a position goes in only if one leaves.
    let full: Vec<usize> = vec![0; 128];
    ok(
        &mut d,
        json!({"op": "set_orders", "orders": full, "restart": 0}),
    );
    assert!(d.order()["restart"].is_null());
    refused(
        &mut d,
        json!({"op": "insert_order", "index": 0, "pattern": 0}),
        "128 positions",
    );
    refused(&mut d, json!({"op": "set_restart", "order": 0}), "no room");
    ok(&mut d, json!({"op": "delete_order", "index": 5}));
    assert_eq!(d.order()["restart"], 0, "deleting made room for the marker");
}

#[test]
fn pattern_length_tracks_and_the_header() {
    let mut d = A2mDoc::new(true);
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 60, "channel": 3, "note": "C-4"}),
    );
    refused(
        &mut d,
        json!({"op": "set_pattern_len", "rows": 32}),
        "force",
    );
    refused(&mut d, json!({"op": "set_pattern_len", "rows": 0}), "1-256");
    refused(
        &mut d,
        json!({"op": "set_pattern_len", "rows": 257}),
        "1-256",
    );
    ok(&mut d, json!({"op": "set_pattern_len", "rows": 128}));
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 100, "channel": 0, "note": "D-4"}),
    );
    ok(
        &mut d,
        json!({"op": "set_pattern_rows", "rows": 32, "force": true}),
    );
    assert_eq!(d.rows(0, 0, 999, false).unwrap()["count"], 32);
    assert_eq!(
        d.song().patterns[0].cell(60, 3).note,
        0,
        "cleared, not hidden"
    );
    ok(&mut d, json!({"op": "undo"}));
    assert_eq!(d.song().patterns[0].cell(60, 3).note, 49);
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 17, "note": "C-4"}),
    );
    refused(&mut d, json!({"op": "set_tracks", "count": 9}), "force");
    refused(&mut d, json!({"op": "set_tracks", "count": 21}), "1-20");
    refused(&mut d, json!({"op": "set_tracks", "count": 0}), "1-20");
    ok(
        &mut d,
        json!({"op": "set_tracks", "count": 9, "force": true}),
    );
    assert_eq!(d.info()["tracks"], 9);
    refused(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 9, "note": 60}),
        "does not exist",
    );
    ok(&mut d, json!({"op": "set_tracks", "count": 20}));
    assert_eq!(
        d.song().patterns[0].cell(0, 17).note,
        0,
        "tracks dropped with force are cleared"
    );
    // Header fields.
    ok(
        &mut d,
        json!({"op": "set_title", "title": "Ünïcode ½ test"}),
    );
    assert_eq!(d.info()["title"], "Ünïcode ½ test");
    let long = "x".repeat(60);
    assert_eq!(
        ok(&mut d, json!({"op": "set_title", "title": long}))["title"]
            .as_str()
            .unwrap()
            .len(),
        42
    );
    ok(&mut d, json!({"op": "set_composer", "composer": "Me"}));
    ok(&mut d, json!({"op": "set_tempo", "tempo": 100}));
    ok(&mut d, json!({"op": "set_speed", "speed": 3}));
    assert_eq!(
        (d.info()["tempo"].as_u64(), d.info()["speed"].as_u64()),
        (Some(100), Some(3))
    );
    refused(&mut d, json!({"op": "set_tempo", "tempo": 0}), "1-255");
    refused(&mut d, json!({"op": "set_speed", "speed": 256}), "0-255");
    ok(
        &mut d,
        json!({"op": "set_flag", "flag": "percussion", "value": true}),
    );
    ok(
        &mut d,
        json!({"op": "set_flag", "flag": "tremolo_depth", "value": true}),
    );
    assert_eq!(d.song().flags, 0x48);
    assert_eq!(d.info()["flags"]["percussion"], true);
    refused(
        &mut d,
        json!({"op": "set_flag", "flag": "nonsense", "value": true}),
        "not a flag",
    );
    refused(
        &mut d,
        json!({"op": "set_flag", "flag": "percussion"}),
        "needs 'value'",
    );
    ok(&mut d, json!({"op": "set_macro_speedup", "factor": 2}));
    refused(
        &mut d,
        json!({"op": "set_macro_speedup", "factor": 0}),
        "1-255",
    );
    ok(
        &mut d,
        json!({"op": "set_four_op_tracks", "pair": 1, "on": true}),
    );
    ok(
        &mut d,
        json!({"op": "set_four_op_tracks", "pair": 3, "on": true}),
    );
    assert_eq!(d.song().four_op_tracks, 0b1010);
    refused(
        &mut d,
        json!({"op": "set_four_op_tracks", "mask": 0x40}),
        "bits 0-5",
    );
    ok(
        &mut d,
        json!({"op": "set_lock_flags", "track": 2, "value": 0x11}),
    );
    refused(
        &mut d,
        json!({"op": "set_lock_flags", "track": 20, "value": 0}),
        "0-19",
    );
    ok(&mut d, json!({"op": "set_rows_per_beat", "rows": 8}));
    ok(&mut d, json!({"op": "set_tempo_finetune", "finetune": -12}));
    ok(
        &mut d,
        json!({"op": "set_four_op_instrument", "pair": 2, "on": true}),
    );
    assert_eq!(d.song().four_op_instruments[0], 1);
    assert_eq!(d.song().four_op_instruments[2], 1);
    refused(
        &mut d,
        json!({"op": "set_four_op_instrument", "pair": 129, "on": true}),
        "1-128",
    );
    check_round_trip("header", &d.to_bytes().unwrap());
    // What older versions do not have.
    let mut v1 = doc("worms.a2m");
    refused(
        &mut v1,
        json!({"op": "set_pattern_len", "rows": 32}),
        "64 rows",
    );
    refused(
        &mut v1,
        json!({"op": "set_tracks", "count": 4}),
        "has 9 tracks",
    );
    refused(
        &mut v1,
        json!({"op": "set_flag", "flag": "percussion", "value": true}),
        "no song flags",
    );
    refused(
        &mut v1,
        json!({"op": "set_macro_speedup", "factor": 2}),
        "no macro speed-up",
    );
    refused(
        &mut v1,
        json!({"op": "set_four_op_tracks", "mask": 1}),
        "no 4-op",
    );
    refused(
        &mut v1,
        json!({"op": "set_rows_per_beat", "rows": 4}),
        "no rows per beat",
    );
    refused(
        &mut v1,
        json!({"op": "set_four_op_instrument", "pair": 1, "on": true}),
        "no 4-op",
    );
    ok(&mut v1, json!({"op": "set_tempo", "tempo": 40}));
    let mut v13 = doc("reflections.a2m");
    refused(
        &mut v13,
        json!({"op": "set_rows_per_beat", "rows": 4}),
        "version 14",
    );
    ok(
        &mut v13,
        json!({"op": "set_four_op_instrument", "pair": 1, "on": true}),
    );
}

// ---------------------------------------------------------------------------
// instruments and macros

#[test]
fn instruments_edit_by_operator_field() {
    let mut d = A2mDoc::new(true);
    let r = ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 2, "name": "Bell ½",
               "modulator": {"multiplier": 1, "level": 40, "attack": 10, "decay": 3, "sustain_level": 4,
                             "release": 11, "waveform": 2, "tremolo": true, "sustain": true, "ksl": 2},
               "carrier": {"multiplier": 2, "total_level": 0, "attack": 15, "waveform": 5, "vibrato": true, "ksr": true},
               "feedback": 5, "connection": "additive", "panning": 2, "finetune": -7}),
    );
    assert_eq!(r["name"], "Bell ½");
    let i = ok(&mut d, json!({"op": "instrument", "instrument": 2}));
    assert_eq!(i["name"], "Bell ½");
    assert_eq!(i["panning"], 2);
    assert_eq!(i["finetune"], -7);
    assert_eq!(i["voice"]["modulator"]["level"], 40);
    assert_eq!(i["voice"]["modulator"]["total_level"], 23);
    assert_eq!(i["voice"]["carrier"]["waveform"], 5);
    assert_eq!(i["voice"]["connection"], "additive");
    assert_eq!(i["voice"]["feedback"], 5);
    let fm: Vec<u8> = i["fm"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u8)
        .collect();
    assert_eq!(fm[0], 0x80 | 0x20 | 1, "tremolo, sustain, multiplier 1");
    assert_eq!(fm[2], 0x80 | 23, "key scale 2, total level 23");
    assert_eq!(fm[10] & 0x0f, 5 << 1 | 1);
    // One operator's fields on their own.
    ok(
        &mut d,
        json!({"op": "set_operator", "instrument": 2, "operator": "carrier", "release": 3, "decay": 9}),
    );
    let v = ok(&mut d, json!({"op": "instrument", "instrument": 2}))["voice"].clone();
    assert_eq!(
        (
            v["carrier"]["release"].as_u64(),
            v["carrier"]["decay"].as_u64()
        ),
        (Some(3), Some(9))
    );
    assert_eq!(
        v["modulator"]["release"], 11,
        "the other operator is untouched"
    );
    // Swap, copy, reset, clear.
    ok(&mut d, json!({"op": "swap_operators", "instrument": 2}));
    assert_eq!(
        ok(&mut d, json!({"op": "instrument", "instrument": 2}))["voice"]["carrier"]["multiplier"],
        1
    );
    ok(&mut d, json!({"op": "copy_instrument", "from": 2, "to": 7}));
    let a = ok(&mut d, json!({"op": "instrument", "instrument": 2}));
    let b = ok(&mut d, json!({"op": "instrument", "instrument": 7}));
    assert_eq!(a["fm"], b["fm"]);
    assert_eq!(b["name"], "Bell ½");
    ok(&mut d, json!({"op": "reset_instrument", "instrument": 7}));
    let b = ok(&mut d, json!({"op": "instrument", "instrument": 7}));
    assert_eq!(b["fm"], json!([33, 33, 63, 0, 242, 244, 36, 39, 0, 0, 0]));
    assert_eq!(b["name"], "Bell ½", "reset keeps the name");
    assert_eq!(b["panning"], 0);
    ok(&mut d, json!({"op": "clear_instrument", "instrument": 7}));
    let b = ok(&mut d, json!({"op": "instrument", "instrument": 7}));
    assert_eq!(
        (b["empty"].as_bool(), b["name"].as_str()),
        (Some(true), Some(""))
    );
    ok(
        &mut d,
        json!({"op": "clear_instrument", "instrument": 2, "keep_name": true}),
    );
    assert_eq!(
        ok(&mut d, json!({"op": "instrument", "instrument": 2}))["name"],
        "Bell ½"
    );
    assert_eq!(ok(&mut d, json!({"op": "instruments"}))["count"], 255);
    // The whole `fm` array outright.
    ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 3, "fm": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]}),
    );
    assert_eq!(
        ok(&mut d, json!({"op": "instrument", "instrument": 3}))["fm"],
        json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11])
    );
}

#[test]
fn instrument_refusals() {
    let mut d = A2mDoc::new(true);
    for (op, word) in [
        (
            json!({"op": "set_instrument", "instrument": 0, "name": "x"}),
            "does not exist",
        ),
        (
            json!({"op": "set_instrument", "instrument": 256, "name": "x"}),
            "does not exist",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "carrier": {"attack": 16}}),
            "attack",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "carrier": {"level": 64}}),
            "level",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "modulator": {"ksl": 4}}),
            "ksl",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "carrier": {"waveform": 8}}),
            "waveform",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "feedback": 8}),
            "feedback",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "connection": "x"}),
            "\"fm\" or \"additive\"",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "carrier": {"level": 3, "total_level": 4}}),
            "not both",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "panning": 3}),
            "panning",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "voice_type": 6}),
            "voice type",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "finetune": 200}),
            "-128 to 127",
        ),
        (
            json!({"op": "set_instrument", "instrument": 1, "fm": [1, 2, 3]}),
            "11 bytes",
        ),
        (
            json!({"op": "set_operator", "instrument": 1, "operator": "both"}),
            "not an operator",
        ),
        (
            json!({"op": "copy_instrument", "from": 1, "to": 1}),
            "onto another",
        ),
        (
            json!({"op": "copy_instrument", "from": 1, "to": 300}),
            "does not exist",
        ),
    ] {
        refused(&mut d, op, word);
    }
    // A name is cut to the version's length: 42 from version 9, 32 before.
    let long = "n".repeat(50);
    assert_eq!(
        ok(
            &mut d,
            json!({"op": "set_instrument", "instrument": 1, "name": long})
        )["name"]
            .as_str()
            .unwrap()
            .len(),
        42
    );
    let mut v1 = doc("worms.a2m");
    assert_eq!(
        ok(
            &mut v1,
            json!({"op": "set_instrument", "instrument": 1, "name": long})
        )["name"]
            .as_str()
            .unwrap()
            .len(),
        32
    );
    refused(
        &mut v1,
        json!({"op": "set_instrument", "instrument": 251, "name": "x"}),
        "does not exist",
    );
    refused(
        &mut v1,
        json!({"op": "set_instrument", "instrument": 1, "panning": 1}),
        "version 1 has no instrument panning",
    );
    refused(
        &mut v1,
        json!({"op": "set_instrument", "instrument": 1, "voice_type": 1}),
        "no voice types",
    );
    let mut v5 = doc("imadick.a2m");
    ok(
        &mut v5,
        json!({"op": "set_instrument", "instrument": 1, "panning": 1}),
    );
    refused(
        &mut v5,
        json!({"op": "set_instrument", "instrument": 1, "voice_type": 1}),
        "no voice types",
    );
    refused(
        &mut v5,
        json!({"op": "set_disabled_columns", "instrument": 1, "columns": vec![0; 28]}),
        "version 11",
    );
    refused(
        &mut v5,
        json!({"op": "set_fm_macro", "macro": 1, "length": 1}),
        "no FM macros",
    );
    refused(
        &mut v5,
        json!({"op": "macro", "kind": "fm", "macro": 1}),
        "no macros",
    );
}

#[test]
fn macros_edit_and_validate() {
    let mut d = A2mDoc::new(true);
    ok(
        &mut d,
        json!({"op": "set_fm_macro", "macro": 2, "length": 3, "loop_begin": 2, "loop_length": 2,
        "keyoff_pos": 3, "arpeggio_table": 1, "vibrato_table": 1,
        "steps": [
            {"step": 1, "fm": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11], "freq_slide": -20, "duration": 4},
            {"step": 2, "carrier": {"level": 10}, "panning": 1, "duration": 2},
        ]}),
    );
    let m = ok(&mut d, json!({"op": "macro", "kind": "fm", "macro": 2}));
    assert_eq!(m["length"], 3);
    assert_eq!(m["steps"].as_array().unwrap().len(), 3, "up to the length");
    assert_eq!(m["steps"][0]["freq_slide"], -20);
    assert_eq!(m["steps"][1]["panning"], 1);
    assert_eq!(m["steps"][1]["voice"]["carrier"]["level"], 10);
    ok(
        &mut d,
        json!({"op": "set_arpeggio_macro", "macro": 1, "length": 4, "speed": 2, "data": [0, 4, 7, 12]}),
    );
    ok(
        &mut d,
        json!({"op": "set_arpeggio_macro", "macro": 1, "entries": [{"entry": 4, "value": 0x8d}]}),
    );
    assert_eq!(
        ok(
            &mut d,
            json!({"op": "macro", "kind": "arpeggio", "macro": 1})
        )["data"],
        json!([0, 4, 7, 0x8d])
    );
    ok(
        &mut d,
        json!({"op": "set_vibrato_macro", "macro": 1, "length": 3, "delay": 5, "speed": 2, "data": [0, 3, -3]}),
    );
    assert_eq!(
        ok(
            &mut d,
            json!({"op": "macro", "kind": "vibrato", "macro": 1})
        )["data"],
        json!([0, 3, -3])
    );
    let list = ok(&mut d, json!({"op": "macros"}));
    assert_eq!(
        (
            list["fm"].clone(),
            list["arpeggio"].clone(),
            list["vibrato"].clone()
        ),
        (json!([2]), json!([1]), json!([1]))
    );
    ok(
        &mut d,
        json!({"op": "clear_macro", "kind": "fm", "macro": 2}),
    );
    assert_eq!(ok(&mut d, json!({"op": "macros"}))["fm"], json!([]));
    // The macro goes with its instrument when copied.
    ok(
        &mut d,
        json!({"op": "set_fm_macro", "macro": 1, "length": 1, "steps": [{"step": 1, "duration": 9}]}),
    );
    ok(&mut d, json!({"op": "copy_instrument", "from": 1, "to": 5}));
    assert_eq!(
        ok(&mut d, json!({"op": "macro", "kind": "fm", "macro": 5}))["steps"][0]["duration"],
        9
    );
    ok(&mut d, json!({"op": "clear_instrument", "instrument": 5}));
    assert_eq!(
        ok(&mut d, json!({"op": "macro", "kind": "fm", "macro": 5}))["length"],
        0
    );
    for (op, word) in [
        (
            json!({"op": "set_fm_macro", "macro": 0, "length": 1}),
            "1-255",
        ),
        (
            json!({"op": "set_fm_macro", "macro": 256, "length": 1}),
            "1-255",
        ),
        (
            json!({"op": "set_fm_macro", "macro": 1, "length": 2, "loop_begin": 0, "loop_length": 1}),
            "starts at entry 1",
        ),
        (
            json!({"op": "set_fm_macro", "macro": 1, "length": 2, "loop_begin": 2, "loop_length": 3}),
            "runs past",
        ),
        (
            json!({"op": "set_fm_macro", "macro": 1, "length": 2, "keyoff_pos": 3}),
            "key-off",
        ),
        (
            json!({"op": "set_fm_macro", "macro": 1, "steps": [{"step": 256}]}),
            "1-255",
        ),
        (
            json!({"op": "set_fm_macro", "macro": 1, "steps": [{"step": 1, "panning": 3}]}),
            "panning",
        ),
        (
            json!({"op": "set_fm_macro", "macro": 1, "steps": [{"step": 1, "carrier": {"attack": 99}}]}),
            "attack",
        ),
        (
            json!({"op": "set_arpeggio_macro", "macro": 1, "entries": [{"entry": 1, "value": 99}]}),
            "not an arpeggio step",
        ),
        (
            json!({"op": "set_vibrato_macro", "macro": 1, "entries": [{"entry": 1, "value": -128}]}),
            "-127 to 127",
        ),
        (
            json!({"op": "clear_macro", "kind": "x", "macro": 1}),
            "not a macro kind",
        ),
        (
            json!({"op": "set_disabled_columns", "instrument": 1, "columns": [1, 2]}),
            "28 flags",
        ),
    ] {
        refused(&mut d, op, word);
    }
    ok(
        &mut d,
        json!({"op": "set_disabled_columns", "instrument": 1, "columns": (0..28).map(|i| i % 2).collect::<Vec<_>>()}),
    );
    assert_eq!(
        ok(&mut d, json!({"op": "instrument", "instrument": 1}))["disabled_columns"][1],
        1
    );
    check_round_trip("macros", &d.to_bytes().unwrap());
}

// ---------------------------------------------------------------------------
// history

#[test]
fn undo_and_redo_restore_exactly() {
    let mut d = A2mDoc::new(true);
    let start = d.song().clone();
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 3, "channel": 2, "note": "C-4", "instrument": 1}),
    );
    let one = d.song().clone();
    ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 1, "carrier": {"attack": 3}}),
    );
    d.begin_group();
    ok(&mut d, json!({"op": "new_pattern"}));
    ok(
        &mut d,
        json!({"op": "insert_order", "index": 1, "pattern": 1}),
    );
    ok(&mut d, json!({"op": "set_title", "title": "Grouped"}));
    d.end_group();
    let end = d.song().clone();
    assert_eq!(d.info()["undo_steps"], 3, "the group is one step");
    let c = ok(&mut d, json!({"op": "undo"}));
    assert_eq!(c["changed"]["layout"], true);
    assert_eq!(c["changed"]["orders"], true);
    assert_eq!(c["changed"]["header"], true);
    assert_eq!(d.song().patterns.len(), 1);
    ok(&mut d, json!({"op": "undo"}));
    ok(&mut d, json!({"op": "undo"}));
    assert_eq!(*d.song(), start);
    refused(&mut d, json!({"op": "undo"}), "nothing to undo");
    for _ in 0..3 {
        ok(&mut d, json!({"op": "redo"}));
        if d.info()["undo_steps"] == 1 {
            assert_eq!(*d.song(), one);
        }
    }
    assert_eq!(*d.song(), end);
    refused(&mut d, json!({"op": "redo"}), "nothing to redo");
    // A new edit ends the redo line.
    ok(&mut d, json!({"op": "undo"}));
    ok(&mut d, json!({"op": "set_tempo", "tempo": 9}));
    assert_eq!(d.info()["can_redo"], false);
    // A failing batch leaves none of its operations, a good one is one step.
    let before = d.song().clone();
    let steps = d.info()["undo_steps"].as_u64().unwrap();
    let e = d
        .apply(&json!({"op": "batch", "ops": [
            {"op": "set_tempo", "tempo": 33},
            {"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": 61},
            {"op": "set_cell", "pattern": 0, "row": 99, "channel": 0, "note": 61},
        ]}))
        .unwrap_err();
    assert!(e.contains("operation 2 (set_cell) failed"), "{e}");
    assert_eq!(*d.song(), before);
    assert_eq!(d.info()["undo_steps"].as_u64().unwrap(), steps);
    let r = ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_tempo", "tempo": 33},
            {"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": 61},
        ]}),
    );
    assert_eq!(r["results"].as_array().unwrap().len(), 2);
    assert_eq!(r["changed"]["header"], Value::Null);
    assert_eq!(r["changed"]["timing"], true);
    assert_eq!(d.info()["undo_steps"].as_u64().unwrap(), steps + 1);
    // The log is bounded.
    d.set_history_limits(3, usize::MAX);
    for t in 10..20 {
        ok(&mut d, json!({"op": "set_tempo", "tempo": t}));
    }
    assert_eq!(d.info()["undo_steps"], 3);
    d.clear_history();
    assert_eq!(d.info()["can_undo"], false);
}

/// Operations of every kind, in a random order, on songs of every version: after
/// each the song saves and loads back; undoing everything gives the start and
/// redoing everything the end.
fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

fn random_op(d: &A2mDoc, s: &mut u64) -> Value {
    let lim = d.info()["limits"].clone();
    let patterns = d.song().patterns.len().max(1);
    let rows = d.pattern_len();
    let tracks = d.tracks();
    let mut r = |n: usize| (lcg(s) % n.max(1) as u64) as usize;
    let pattern = r(patterns);
    let row = r(rows);
    let channel = r(tracks);
    let instruments = lim["instruments"].as_u64().unwrap() as usize;
    match r(22) {
        0..=3 => {
            json!({"op": "set_cell", "pattern": pattern, "row": row, "channel": channel,
            "note": 12 + r(96), "instrument": r(instruments + 1),
            "effect": r(lim["max_effect"].as_u64().unwrap() as usize + 1), "param": r(256)})
        }
        4 => {
            json!({"op": "set_cell", "pattern": pattern, "row": row, "channel": channel, "note": 255})
        }
        5 => {
            json!({"op": "clear_range", "pattern": pattern, "row0": row, "row1": row + r(4).min(rows - 1 - row), "channel0": channel})
        }
        6 => json!({"op": "insert_row", "pattern": pattern, "row": row, "count": 1 + r(3)}),
        7 => {
            json!({"op": "delete_row", "pattern": pattern, "row": row, "count": 1 + r(3), "channel": channel})
        }
        8 => {
            json!({"op": "transpose", "pattern": pattern, "row0": 0, "row1": rows - 1, "semitones": r(25) as i64 - 12, "clamp": true})
        }
        9 => json!({"op": "paste", "pattern": pattern, "row": row, "channel": channel,
            "cells": [[[60, 1, 0, 0, 0, 0], [255, 0, 0, 0, 0, 0]]], "mode": if r(2) == 0 {"mix"} else {"overwrite"}}),
        10 => json!({"op": "new_pattern"}),
        11 => json!({"op": "duplicate_pattern", "pattern": pattern}),
        12 => json!({"op": "delete_pattern", "pattern": pattern}),
        13 => {
            json!({"op": "insert_order", "index": r(d.order()["orders"].as_array().unwrap().len() + 1), "pattern": pattern})
        }
        14 => {
            json!({"op": "delete_order", "index": r(d.order()["orders"].as_array().unwrap().len())})
        }
        15 => {
            json!({"op": "move_order", "from": r(d.order()["orders"].as_array().unwrap().len()), "to": r(d.order()["orders"].as_array().unwrap().len())})
        }
        16 => {
            json!({"op": "set_restart", "order": r(d.order()["orders"].as_array().unwrap().len())})
        }
        17 => {
            json!({"op": "set_instrument", "instrument": 1 + r(instruments), "name": format!("i{}", r(1000)),
            "carrier": {"attack": r(16), "level": r(64), "waveform": r(8)}, "modulator": {"multiplier": r(16)},
            "feedback": r(8), "panning": r(3), "finetune": r(255) as i64 - 128})
        }
        18 => json!({"op": "set_tempo", "tempo": 1 + r(255)}),
        19 => json!({"op": "set_pattern_len", "rows": 1 + r(256), "force": true}),
        20 => {
            json!({"op": "set_fm_macro", "macro": 1 + r(255), "length": r(5), "steps": [{"step": 1 + r(5), "duration": r(9)}]})
        }
        _ => json!({"op": "copy_instrument", "from": 1 + r(instruments), "to": 1 + r(instruments)}),
    }
}

fn soak(name: &str, mut d: A2mDoc, seed: u64, steps: usize) {
    let start = d.song().clone();
    let mut s = seed;
    let (mut applied, mut refused_n) = (0, 0);
    for step in 0..steps {
        let op = random_op(&d, &mut s);
        let before = d.song().clone();
        let group = lcg(&mut s).is_multiple_of(5);
        if group {
            d.begin_group();
        }
        match d.apply(&op) {
            Ok(_) => applied += 1,
            Err(_) => {
                refused_n += 1;
                assert_eq!(
                    *d.song(),
                    before,
                    "{name} step {step}: a refused {op} changed the song"
                );
            }
        }
        if group {
            d.end_group();
        }
        let issues = d.validate_content();
        assert!(
            issues.is_empty(),
            "{name} step {step} after {op}: {issues:?}"
        );
        if step % 7 == 0 {
            check_round_trip(&format!("{name} step {step}"), &d.to_bytes().unwrap());
        }
    }
    let end = d.song().clone();
    let steps_logged = d.info()["undo_steps"].as_u64().unwrap();
    while d.undo().is_some() {}
    assert_eq!(
        *d.song(),
        start,
        "{name}: undoing everything gives the start"
    );
    for _ in 0..steps_logged {
        d.redo().unwrap();
    }
    assert_eq!(*d.song(), end, "{name}: redoing everything gives the end");
    println!(
        "{name}: {applied} operations applied, {refused_n} refused, {steps_logged} undo steps"
    );
}

#[test]
fn random_operations_keep_every_state_saveable_and_undoable() {
    for (i, (file, _)) in BY_VERSION.iter().enumerate() {
        soak(file, doc(file), 1 + i as u64, 60);
    }
    soak("new opl2", A2mDoc::new(false), 99, 120);
    soak("new opl3", A2mDoc::new(true), 100, 120);
}

// ---------------------------------------------------------------------------
// live edits

fn render(p: &mut A2Player, frames: usize) -> Vec<f32> {
    let (mut l, mut r) = (vec![0f32; frames], vec![0f32; frames]);
    for (lc, rc) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
        p.render(lc, rc);
    }
    l.extend(r);
    l
}

/// A song that is playing takes an edit with `apply_edit` and sounds as the edited
/// song would from the start: the edits land before the song reaches them.
#[test]
fn live_edits_equal_reopening_the_song() {
    let mut d = A2mDoc::new(true);
    ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 1, "carrier": {"attack": 15, "decay": 2}}),
    );
    for row in 0..16 {
        ok(
            &mut d,
            json!({"op": "set_cell", "pattern": 0, "row": row, "channel": 0, "note": 48 + row, "instrument": 1}),
        );
    }
    d.clear_history();
    let original = d.to_bytes().unwrap();
    let mut p = A2Player::new(&original, 48_000.0).unwrap();
    p.play();
    // Edit rows the song has not reached, the instrument and the macros.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 12, "channel": 1, "note": "G-5", "instrument": 2}),
    );
    let edits = d.live_edits().expect("a cell edit is live");
    assert_eq!(edits.len(), 1);
    for e in &edits {
        assert!(p.apply_edit(e));
    }
    ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 2, "fm": [1, 50, 29, 8, 163, 197, 75, 102, 2, 5, 13]}),
    );
    let edits = d.live_edits().expect("an instrument edit is live");
    assert_eq!(edits.len(), 1);
    assert!(p.apply_edit(&edits[0]));
    ok(
        &mut d,
        json!({"op": "set_arpeggio_macro", "macro": 3, "length": 2, "entries": [{"entry": 2, "value": 7}]}),
    );
    for e in d.live_edits().expect("a macro edit is live") {
        assert!(p.apply_edit(&e));
    }
    ok(
        &mut d,
        json!({"op": "set_fm_macro", "macro": 2, "length": 1, "steps": [{"step": 1, "duration": 3}]}),
    );
    for e in d.live_edits().expect("a macro edit is live") {
        assert!(p.apply_edit(&e));
    }
    ok(&mut d, json!({"op": "set_tempo", "tempo": 60}));
    for e in d.live_edits().expect("a tempo edit is live") {
        assert!(p.apply_edit(&e));
    }
    // The song edited and played from the start is the live-edited song: row 12
    // (1.4 s in) is where the edits show.
    let frames = 150_000;
    let live = render(&mut p, frames);
    let mut q = A2Player::new(&d.to_bytes().unwrap(), 48_000.0).unwrap();
    q.play();
    assert_eq!(
        live,
        render(&mut q, frames),
        "live edits sound like the edited song"
    );
    // And they are not the song without them.
    let mut o = A2Player::new(&original, 48_000.0).unwrap();
    o.play();
    assert_ne!(live, render(&mut o, frames));
}

#[test]
fn live_edits_say_what_a_song_cannot_take() {
    let mut d = A2mDoc::new(true);
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": 60}),
    );
    assert!(d.live_edits().is_some());
    ok(&mut d, json!({"op": "set_title", "title": "t"}));
    assert_eq!(
        d.live_edits().map(|v| v.len()),
        Some(0),
        "a title needs nothing"
    );
    ok(&mut d, json!({"op": "set_tempo", "tempo": 70}));
    assert_eq!(d.live_edits().map(|v| v.len()), Some(1));
    // Shape: the song must be reopened.
    for (op, what) in [
        (json!({"op": "new_pattern"}), "patterns"),
        (
            json!({"op": "insert_order", "index": 0, "pattern": 0}),
            "orders",
        ),
        (
            json!({"op": "set_pattern_len", "rows": 32, "force": true}),
            "length",
        ),
        (
            json!({"op": "set_tracks", "count": 12, "force": true}),
            "tracks",
        ),
        (
            json!({"op": "set_flag", "flag": "percussion", "value": true}),
            "flags",
        ),
        (json!({"op": "set_macro_speedup", "factor": 3}), "speed-up"),
        (json!({"op": "set_four_op_tracks", "mask": 1}), "4-op"),
        (
            json!({"op": "set_lock_flags", "track": 0, "value": 1}),
            "locks",
        ),
    ] {
        ok(&mut d, op);
        assert!(d.live_edits().is_none(), "{what} moves the song's shape");
    }
    // Undo of a cell edit is live, and says the cells.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 0, "row": 2, "channel": 1, "note": 62}),
    );
    ok(&mut d, json!({"op": "undo"}));
    let edits = d.live_edits().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].to_json()["kind"], "cell");
    assert_eq!(edits[0].to_json()["raw"][0], 0, "the cell as it is now");
    // Too many cells at once is a new song.
    let mut big = A2mDoc::new(true);
    ok(
        &mut big,
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": 60}),
    );
    for c in 0..20 {
        big.apply(&json!({"op": "paste", "pattern": 0, "row": 0, "channel": c, "cells": vec![vec![[61, 1, 0, 0, 0, 0]; 1]; 64]})).ok();
    }
    ok(
        &mut big,
        json!({"op": "paste", "pattern": 0, "row": 0, "channel": 0,
        "cells": vec![vec![[62, 1, 0, 0, 0, 0]; 20]; 64]}),
    );
    assert!(big.live_edits().is_none(), "past 1024 cells");
    let lj = ok(&mut d, json!({"op": "live_edits"}));
    assert!(lj["edits"].is_array());
}

#[test]
fn a_region_can_be_asked_for_directly() {
    let d = A2mDoc::new(true);
    let r: Region = d.region(0, (3, 1), None).unwrap();
    assert_eq!((r.row0, r.row1, r.channel0, r.channel1), (1, 3, 0, 17));
    assert!(d.region(0, (0, 64), None).is_err());
    let _ = Cell::default();
    let _: Option<A2mSong> = None;
}
