//! The edit layer: operations one by one (and their refusals), the compact
//! shapes, undo and redo, and a random-operation soak that checks every state
//! reached can be saved and loaded back.

use cubed_tracker::edit::{ModuleDoc, Region};
use cubed_tracker::{load, write, Format, Module, Note};
use serde_json::{json, Value};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn doc(name: &str) -> ModuleDoc {
    ModuleDoc::from_bytes(&fixture(name)).unwrap()
}

fn ok(d: &mut ModuleDoc, op: Value) -> Value {
    match d.apply(&op) {
        Ok(v) => v,
        Err(e) => panic!("{op} failed: {e}"),
    }
}

fn err(d: &mut ModuleDoc, op: Value) -> String {
    let before = d.module().clone();
    let e = d.apply(&op).expect_err(&format!("{op} should fail"));
    assert_eq!(*d.module(), before, "a refused {op} changed the module");
    e
}

fn cell(d: &ModuleDoc, p: usize, r: usize, c: usize) -> [u64; 5] {
    let v = d.rows(p, r, 1, false).unwrap();
    let a = v["cells"][0][c].as_array().unwrap();
    [0, 1, 2, 3, 4].map(|i| a[i].as_u64().unwrap())
}

// ---------------------------------------------------------------------------
// reading

#[test]
fn rows_have_the_compact_shape() {
    let d = doc("demo.xm");
    let v = d.rows(0, 0, 4, false).unwrap();
    assert_eq!(v["count"], 4);
    assert_eq!(v["channels"], d.module().channels);
    let first = &v["cells"][0][0];
    assert_eq!(first.as_array().unwrap().len(), 5);
    let flat = d.rows(0, 0, 4, true).unwrap();
    assert_eq!(
        flat["cells"].as_array().unwrap().len(),
        4 * d.module().channels * 5
    );
    // Rows past the end are left out.
    let tail = d
        .rows(0, d.module().patterns[0].rows() - 2, 10, false)
        .unwrap();
    assert_eq!(tail["count"], 2);
}

#[test]
fn info_states_the_format_limits() {
    for (name, notes, offset, slots) in [
        ("demo.mod", (24, 59), 1, 31),
        ("demo.s3m", (24, 119), 2, 99),
        ("demo.xm", (12, 107), 1, 128),
    ] {
        let d = doc(name);
        let i = d.info();
        assert_eq!(i["note"]["min"], notes.0, "{name}");
        assert_eq!(i["note"]["max"], notes.1, "{name}");
        assert_eq!(i["note"]["octave_offset"], offset, "{name}");
        assert_eq!(i["limits"]["slots"], slots, "{name}");
        assert_eq!(i["can_undo"], false);
    }
}

#[test]
fn note_names_follow_each_trackers_octaves() {
    use cubed_tracker::edit::{note_name, parse_note};
    assert_eq!(note_name(Format::Mod, 24), "C-1");
    assert_eq!(note_name(Format::Xm, 60), "C-4");
    assert_eq!(note_name(Format::S3m, 72), "C-4");
    assert_eq!(parse_note(Format::Xm, "F#2"), Some(Note::On(42)));
    assert_eq!(parse_note(Format::Xm, "Bb3"), Some(Note::On(58)));
    assert_eq!(parse_note(Format::S3m, "c-4"), Some(Note::On(72)));
    assert_eq!(parse_note(Format::Xm, "==="), Some(Note::Off));
    assert_eq!(parse_note(Format::Xm, "---"), Some(Note::None));
    assert_eq!(parse_note(Format::Xm, "H-4"), None);
}

// ---------------------------------------------------------------------------
// cells

#[test]
fn set_cell_per_format() {
    let mut x = doc("demo.xm");
    let r = ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":5,"channel":1,"note":"C-4","instrument":1,"volume":0x40,"effect":"C","param":32}),
    );
    assert_eq!(r["cell"], json!([60, 1, 64, 12, 32]));
    assert_eq!(
        r["changed"]["patterns"][0],
        json!({"pattern":0,"row0":5,"row1":5})
    );
    // Fields not given stay.
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":5,"channel":1,"note":62}),
    );
    assert_eq!(cell(&x, 0, 5, 1), [62, 1, 64, 12, 32]);
    // A key off, then a whole cell.
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":5,"channel":1,"cell":[255,0,0,0,0]}),
    );
    assert_eq!(cell(&x, 0, 5, 1), [255, 0, 0, 0, 0]);

    let mut m = doc("demo.mod");
    let r = ok(
        &mut m,
        json!({"op":"set_cell","pattern":0,"row":1,"channel":0,"note":"C-2","instrument":2,"effect":"C","param":16}),
    );
    assert_eq!(r["cell"], json!([36, 2, 0, 12, 16]));
    // The period follows the note.
    assert_eq!(m.module().patterns[0].cell(1, 0).period, 428);
    let mut s = doc("demo.s3m");
    let r = ok(
        &mut s,
        json!({"op":"set_cell","pattern":0,"row":1,"channel":0,"note":"C-4","instrument":1,"volume":0x30,"effect":"K","param":4}),
    );
    assert_eq!(r["cell"], json!([72, 1, 48, 11, 4]));
}

#[test]
fn set_cell_refusals() {
    let mut m = doc("demo.mod");
    let at = json!({"op":"set_cell","pattern":0,"row":1,"channel":0});
    let with = |extra: Value| {
        let mut o = at.clone();
        for (k, v) in extra.as_object().unwrap() {
            o[k] = v.clone();
        }
        o
    };
    assert!(err(&mut m, with(json!({"note": "C-0"}))).contains("range"));
    assert!(err(&mut m, with(json!({"note": 80}))).contains("range"));
    assert!(err(&mut m, with(json!({"note": "==="}))).contains("no key-off"));
    assert!(err(&mut m, with(json!({"volume": 0x30}))).contains("no volume column"));
    assert!(err(&mut m, with(json!({"effect": 16}))).contains("0-F"));
    assert!(err(&mut m, with(json!({"instrument": 32}))).contains("does not exist"));
    assert!(err(
        &mut m,
        json!({"op":"set_cell","pattern":9,"row":0,"channel":0})
    )
    .contains("pattern 9"));
    assert!(err(
        &mut m,
        json!({"op":"set_cell","pattern":0,"row":64,"channel":0})
    )
    .contains("past the end"));
    assert!(err(
        &mut m,
        json!({"op":"set_cell","pattern":0,"row":0,"channel":4})
    )
    .contains("channel"));

    let mut s = doc("demo.s3m");
    let at = |extra: Value| {
        let mut o = json!({"op":"set_cell","pattern":0,"row":1,"channel":0});
        for (k, v) in extra.as_object().unwrap() {
            o[k] = v.clone();
        }
        o
    };
    assert!(err(&mut s, at(json!({"volume": 0x60}))).contains("0x10-0x50"));
    assert!(err(&mut s, at(json!({"effect": "C", "param": 0x1a}))).contains("decimal"));
    assert!(err(&mut s, at(json!({"param": 5}))).contains("needs an effect"));
    assert!(err(&mut s, at(json!({"note": 120}))).contains("range"));

    let mut x = doc("demo.xm");
    let at = |extra: Value| {
        let mut o = json!({"op":"set_cell","pattern":0,"row":1,"channel":0});
        for (k, v) in extra.as_object().unwrap() {
            o[k] = v.clone();
        }
        o
    };
    assert!(err(&mut x, at(json!({"effect": 0x12}))).contains("not an XM effect"));
    assert!(err(&mut x, at(json!({"volume": 0x55}))).contains("volume column"));
    assert!(err(&mut x, at(json!({"note": 108}))).contains("range"));
    ok(&mut x, at(json!({"volume": 0xc8})));
}

#[test]
fn clear_insert_delete_rows() {
    let mut x = doc("demo.xm");
    let before = x.module().patterns[0].clone();
    ok(
        &mut x,
        json!({"op":"insert_row","pattern":0,"row":2,"count":2}),
    );
    let after = &x.module().patterns[0];
    assert!(after.row(2).iter().all(|c| c.is_empty()) && after.row(3).iter().all(|c| c.is_empty()));
    assert_eq!(after.row(4), before.row(2));
    assert_eq!(after.rows(), before.rows());
    ok(
        &mut x,
        json!({"op":"delete_row","pattern":0,"row":2,"count":2}),
    );
    // The two rows pushed off the end are gone; the rest is as it was.
    let again = &x.module().patterns[0];
    for r in 0..before.rows() - 2 {
        assert_eq!(again.row(r), before.row(r), "row {r}");
    }
    assert!(again.row(before.rows() - 1).iter().all(|c| c.is_empty()));

    // One channel only.
    let mut y = doc("demo.xm");
    let b = y.module().patterns[0].clone();
    ok(
        &mut y,
        json!({"op":"insert_row","pattern":0,"row":0,"channel":1}),
    );
    let a = &y.module().patterns[0];
    assert!(a.cell(0, 1).is_empty());
    assert_eq!(a.cell(1, 1), b.cell(0, 1));
    assert_eq!(a.cell(0, 0), b.cell(0, 0));

    ok(
        &mut y,
        json!({"op":"clear_range","pattern":0,"row0":0,"row1":63,"channel0":0,"channel1":0,"columns":["effect"]}),
    );
    assert!((0..64).all(|r| {
        let c = y.module().patterns[0].cell(r, 0);
        c.effect == 0 && c.param == 0
    }));
    assert!(err(
        &mut y,
        json!({"op":"insert_row","pattern":0,"row":0,"count":0})
    )
    .contains("count"));
    assert!(err(
        &mut y,
        json!({"op":"clear_range","pattern":0,"columns":["tone"]})
    )
    .contains("not a column"));
}

#[test]
fn transpose_respects_the_format() {
    let mut m = doc("demo.mod");
    ok(
        &mut m,
        json!({"op":"set_cell","pattern":0,"row":0,"channel":0,"note":"C-2"}),
    );
    let r = ok(
        &mut m,
        json!({"op":"transpose","pattern":0,"row0":0,"row1":0,"channel":0,"semitones":12}),
    );
    assert_eq!(r["count"], 1);
    assert_eq!(cell(&m, 0, 0, 0)[0], 48);
    assert_eq!(m.module().patterns[0].cell(0, 0).period, 214);
    // Past B-3 refuses, unless clamped.
    assert!(err(
        &mut m,
        json!({"op":"transpose","pattern":0,"row0":0,"row1":0,"channel":0,"semitones":12})
    )
    .contains("leaves"));
    ok(
        &mut m,
        json!({"op":"transpose","pattern":0,"row0":0,"row1":0,"channel":0,"semitones":12,"clamp":true}),
    );
    assert_eq!(cell(&m, 0, 0, 0)[0], 59);

    let mut x = doc("demo.xm");
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":0,"channel":0,"note":60,"instrument":1}),
    );
    ok(
        &mut x,
        json!({"op":"transpose","pattern":0,"row0":0,"row1":0,"channel":0,"semitones":-48}),
    );
    assert_eq!(cell(&x, 0, 0, 0)[0], 12);
    assert!(err(
        &mut x,
        json!({"op":"transpose","pattern":0,"row0":0,"row1":0,"channel":0,"semitones":-1})
    )
    .contains("leaves"));
}

#[test]
fn copy_and_paste() {
    let mut x = doc("demo.xm");
    let clip = ok(
        &mut x,
        json!({"op":"copy_range","pattern":0,"row0":0,"row1":3,"channel0":0,"channel1":1}),
    );
    assert_eq!(clip["rows"], 4);
    assert_eq!(clip["channels"], 2);
    ok(
        &mut x,
        json!({"op":"paste","pattern":0,"row":8,"channel":2,"cells":clip.clone()}),
    );
    for r in 0..4 {
        for c in 0..2 {
            assert_eq!(
                x.module().patterns[0].cell(r, c),
                x.module().patterns[0].cell(8 + r, 2 + c),
                "row {r} channel {c}"
            );
        }
    }
    // A paste that runs off the edge is clipped.
    let r = ok(
        &mut x,
        json!({"op":"paste","pattern":0,"row":62,"channel":0,"cells":clip.clone()}),
    );
    assert_eq!(r["clipped"], true);
    assert_eq!(r["rows"], 2);

    // Mix fills only what is empty.
    let mut y = ModuleDoc::new(Format::Xm);
    ok(&mut y, json!({"op":"add_instrument"}));
    ok(
        &mut y,
        json!({"op":"set_cell","pattern":0,"row":0,"channel":0,"note":60}),
    );
    ok(
        &mut y,
        json!({"op":"set_cell","pattern":0,"row":1,"channel":0,"note":62,"instrument":1}),
    );
    let clip = json!({"cells": [[[64, 1, 0x30, 0, 0]], [[65, 0, 0x20, 0, 0]]]});
    ok(
        &mut y,
        json!({"op":"paste","pattern":0,"row":0,"channel":0,"cells":clip,"mode":"mix"}),
    );
    assert_eq!(cell(&y, 0, 0, 0), [60, 1, 0x30, 0, 0]);
    assert_eq!(cell(&y, 0, 1, 0), [62, 1, 0x20, 0, 0]);
    // A cell the format cannot hold refuses the paste.
    let bad = json!({"cells": [[[64, 1, 0, 0x12, 0]]]});
    assert!(err(
        &mut y,
        json!({"op":"paste","pattern":0,"row":0,"channel":0,"cells":bad})
    )
    .contains("does not fit"));

    // A MOD clip keeps an exact period.
    let mut m = doc("demo.mod");
    let region = Region {
        pattern: 0,
        row0: 0,
        row1: 0,
        channel0: 0,
        channel1: 0,
    };
    m.begin_group();
    let c = m.copy_range(region).unwrap();
    m.end_group();
    m.paste(0, 5, 0, &c, Default::default()).unwrap();
    assert_eq!(
        m.module().patterns[0].cell(5, 0),
        m.module().patterns[0].cell(0, 0)
    );
}

#[test]
fn pattern_list_and_orders() {
    let mut x = doc("demo.xm");
    let n = x.module().patterns.len();
    let r = ok(&mut x, json!({"op":"new_pattern","rows":32}));
    assert_eq!(r["pattern"], n);
    assert_eq!(x.module().patterns[n].rows(), 32);
    ok(
        &mut x,
        json!({"op":"set_pattern_rows","pattern":n,"rows":16}),
    );
    assert_eq!(x.module().patterns[n].rows(), 16);
    let r = ok(&mut x, json!({"op":"duplicate_pattern","pattern":0}));
    assert_eq!(r["pattern"], n + 1);
    assert_eq!(x.module().patterns[n + 1], x.module().patterns[0]);
    ok(&mut x, json!({"op":"insert_order","index":0,"pattern":n}));
    assert_eq!(x.module().orders[0], n);
    ok(&mut x, json!({"op":"set_order","index":0,"pattern":n + 1}));
    // Deleting pattern 0 renumbers the order list.
    let before = x.module().orders.clone();
    ok(&mut x, json!({"op":"delete_pattern","pattern":0}));
    let expected: Vec<usize> = before.iter().filter(|&&o| o != 0).map(|&o| o - 1).collect();
    assert_eq!(x.module().orders, expected);
    assert!(
        err(&mut x, json!({"op":"set_order","index":0,"pattern":99})).contains("does not exist")
    );
    assert!(err(&mut x, json!({"op":"delete_order","index":99})).contains("does not exist"));
    assert!(err(
        &mut x,
        json!({"op":"set_pattern_rows","pattern":0,"rows":300})
    )
    .contains("1-256"));

    // Shortening a pattern with notes below refuses without force.
    assert!(err(
        &mut x,
        json!({"op":"set_pattern_rows","pattern":0,"rows":4})
    )
    .contains("force"));
    ok(
        &mut x,
        json!({"op":"set_pattern_rows","pattern":0,"rows":4,"force":true}),
    );

    // A MOD's patterns are 64 rows.
    let mut m = doc("demo.mod");
    assert!(err(
        &mut m,
        json!({"op":"set_pattern_rows","pattern":0,"rows":32})
    )
    .contains("always has 64"));
    // The last pattern in the song cannot go.
    let mut one = ModuleDoc::new(Format::Mod);
    assert!(
        err(&mut one, json!({"op":"delete_pattern","pattern":0})).contains("at least one pattern")
    );
    ok(&mut one, json!({"op":"new_pattern"}));
    assert!(err(&mut one, json!({"op":"delete_pattern","pattern":0})).contains("whole song"));
}

#[test]
fn xm_restart_position() {
    let mut x = doc("demo.xm");
    let n = x.module().orders.len();
    assert!(n >= 2, "the fixture has several orders");
    ok(&mut x, json!({"op":"set_restart","order":1}));
    assert_eq!(x.module().restart, 1);
    ok(&mut x, json!({"op":"insert_order","index":0,"pattern":0}));
    assert_eq!(x.module().restart, 2, "the restart follows its order");
    let bytes = x.to_bytes(Format::Xm).unwrap();
    assert_eq!(load(&bytes).unwrap().restart, 2);
    assert!(err(&mut x, json!({"op":"set_restart","order":999})).contains("does not exist"));
    let mut m = doc("demo.mod");
    assert!(err(&mut m, json!({"op":"set_restart","order":0})).contains("cannot store"));
}

// ---------------------------------------------------------------------------
// song

#[test]
fn speed_tempo_title() {
    let mut x = doc("demo.xm");
    ok(&mut x, json!({"op":"set_speed","speed":4}));
    ok(&mut x, json!({"op":"set_tempo","tempo":140}));
    assert_eq!((x.module().speed, x.module().tempo), (4, 140));
    assert!(err(&mut x, json!({"op":"set_speed","speed":32})).contains("1-31"));
    assert!(err(&mut x, json!({"op":"set_tempo","tempo":31})).contains("32-255"));
    let r = ok(
        &mut x,
        json!({"op":"set_title","title":"A very long title for an XM file indeed"}),
    );
    assert_eq!(r["title"].as_str().unwrap().len(), 20);
    assert!(err(&mut x, json!({"op":"set_title","title":"snowman \u{2603}"})).contains("Latin-1"));

    // A MOD keeps them as Fxx on the first row.
    let mut m = ModuleDoc::new(Format::Mod);
    ok(&mut m, json!({"op":"set_speed","speed":4}));
    ok(&mut m, json!({"op":"set_tempo","tempo":140}));
    assert_eq!(m.info()["speed"], 4);
    assert_eq!(m.info()["tempo"], 140);
    let row0 = m.module().patterns[0].row(0).to_vec();
    assert_eq!(row0.iter().filter(|c| c.effect == 0x0f).count(), 2);
    // Back to the defaults removes them.
    ok(&mut m, json!({"op":"set_speed","speed":6}));
    ok(&mut m, json!({"op":"set_tempo","tempo":125}));
    assert!(m.module().patterns[0].row(0).iter().all(|c| c.is_empty()));
    // Reloaded, the song starts the same way.
    let mut m2 = ModuleDoc::new(Format::Mod);
    ok(&mut m2, json!({"op":"set_speed","speed":3}));
    let back = ModuleDoc::from_bytes(&m2.to_bytes(Format::Mod).unwrap()).unwrap();
    assert_eq!(back.info()["speed"], 3);
    // No free effect on the first row refuses.
    let mut full = ModuleDoc::new(Format::Mod);
    for c in 0..4 {
        ok(
            &mut full,
            json!({"op":"set_cell","pattern":0,"row":0,"channel":c,"effect":"1","param":1}),
        );
    }
    assert!(err(&mut full, json!({"op":"set_speed","speed":3})).contains("no free effect"));

    let mut s = doc("demo.s3m");
    ok(&mut s, json!({"op":"set_global_volume","volume":32}));
    assert_eq!(s.module().global_volume, 0.5);
    assert!(err(&mut x, json!({"op":"set_global_volume","volume":32})).contains("no global volume"));
}

#[test]
fn channels() {
    let mut x = doc("demo.xm");
    let n = x.module().channels;
    ok(&mut x, json!({"op":"set_channels","count":n + 4}));
    assert_eq!(x.module().patterns[0].channels(), n + 4);
    assert_eq!(x.module().channel_pan.len(), n + 4);
    // Shrinking over data refuses without force.
    assert!(err(&mut x, json!({"op":"set_channels","count":1})).contains("force"));
    ok(&mut x, json!({"op":"set_channels","count":1,"force":true}));
    assert_eq!(x.module().channels, 1);
    assert!(err(&mut x, json!({"op":"set_channels","count":33})).contains("1-32"));

    let mut m = ModuleDoc::new(Format::Mod);
    ok(&mut m, json!({"op":"set_channels","count":8}));
    assert_eq!(m.module().stereo_separation, 1.0);
    assert!(!m.module().flags.amiga_limits);
    let bytes = m.to_bytes(Format::Mod).unwrap();
    assert_eq!(&bytes[1080..1084], b"8CHN");
    assert_eq!(load(&bytes).unwrap().channels, 8);

    let mut s = ModuleDoc::new(Format::S3m);
    ok(
        &mut s,
        json!({"op":"set_channel","channel":2,"pan":3,"muted":true}),
    );
    assert!(s.module().muted[2]);
    assert!(err(&mut s, json!({"op":"set_channel","channel":2,"pan":16})).contains("0-15"));
    assert!(err(&mut x, json!({"op":"set_channel","channel":0,"pan":3})).contains("no per-channel"));
}

#[test]
fn flags() {
    let mut x = doc("demo.xm");
    ok(
        &mut x,
        json!({"op":"set_flag","flag":"linear_frequency","value":false}),
    );
    assert!(!x.module().flags.linear_frequency);
    assert!(err(
        &mut x,
        json!({"op":"set_flag","flag":"amiga_limits","value":true})
    )
    .contains("no such flag"));
    let mut m = ModuleDoc::new(Format::Mod);
    ok(
        &mut m,
        json!({"op":"set_cell","pattern":0,"row":0,"channel":0,"note":"C-1"}),
    );
    ok(
        &mut m,
        json!({"op":"set_flag","flag":"amiga_limits","value":false}),
    );
    // Seven octaves now: a low note fits...
    ok(
        &mut m,
        json!({"op":"set_cell","pattern":0,"row":1,"channel":0,"note":12}),
    );
    // ...and the limit cannot come back over it.
    assert!(err(
        &mut m,
        json!({"op":"set_flag","flag":"amiga_limits","value":true})
    )
    .contains("outside ProTracker"));
}

// ---------------------------------------------------------------------------
// samples and instruments

fn wav(rate: u32, channels: u16, frames: &[f32]) -> Vec<u8> {
    let mut out = Vec::new();
    let data: Vec<u8> = frames
        .iter()
        .flat_map(|v| ((v * 32767.0).round() as i16).to_le_bytes())
        .collect();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2 * channels as u32).to_le_bytes());
    out.extend_from_slice(&(2 * channels).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

fn b64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn sine(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (i as f32 / n as f32 * std::f32::consts::TAU).sin() * 0.8)
        .collect()
}

#[test]
fn set_sample_fields_and_loops() {
    let mut m = doc("demo.mod");
    let r = ok(
        &mut m,
        json!({"op":"set_sample","instrument":1,"name":"renamed","volume":20,"finetune":-3}),
    );
    assert_eq!(r["sample"]["name"], "renamed");
    assert_eq!(m.module().samples[0].volume, 20);
    assert_eq!(m.module().samples[0].finetune, -3);
    // A loop is fitted: whole words, inside the data.
    let r = ok(
        &mut m,
        json!({"op":"set_sample","instrument":4,"loop_kind":"forward","loop_start":3,"loop_len":999999}),
    );
    let len = m.module().samples[3].data.len();
    assert_eq!(r["sample"]["loop_start"], 2);
    assert_eq!(r["sample"]["loop_len"], (len - 2) & !1);
    assert!(err(
        &mut m,
        json!({"op":"set_sample","instrument":1,"volume":65})
    )
    .contains("0-64"));
    assert!(err(
        &mut m,
        json!({"op":"set_sample","instrument":1,"finetune":9})
    )
    .contains("-8 to 7"));
    assert!(err(
        &mut m,
        json!({"op":"set_sample","instrument":1,"relative_note":1})
    )
    .contains("no relative note"));
    assert!(err(
        &mut m,
        json!({"op":"set_sample","instrument":1,"loop_kind":"pingpong"})
    )
    .contains("ping-pong"));
    assert!(err(
        &mut m,
        json!({"op":"set_sample","instrument":32,"volume":1})
    )
    .contains("does not exist"));
    assert!(err(
        &mut m,
        json!({"op":"set_sample","instrument":1,"c2spd":8000})
    )
    .contains("no c2spd"));

    let mut x = doc("demo.xm");
    let r = ok(
        &mut x,
        json!({"op":"set_sample","instrument":1,"relative_note":-12,"finetune":64,"pan":10,"loop_kind":"pingpong"}),
    );
    assert_eq!(r["sample"]["relative_note"], -12);
    assert_eq!(r["sample"]["pan"], 10);
    assert_eq!(r["sample"]["loop_kind"], "pingpong");
    assert!(err(
        &mut x,
        json!({"op":"set_sample","instrument":1,"relative_note":96})
    )
    .contains("-96 to 95"));
    assert!(err(
        &mut x,
        json!({"op":"set_sample","instrument":1,"sample":9,"volume":1})
    )
    .contains("sample 9"));

    let mut s = doc("demo.s3m");
    ok(
        &mut s,
        json!({"op":"set_sample","instrument":1,"c2spd":16000}),
    );
    assert_eq!(s.module().samples[0].c2spd, 16000);
    assert!(err(
        &mut s,
        json!({"op":"set_sample","instrument":1,"c2spd":70000})
    )
    .contains("1-65535"));
    assert!(err(
        &mut s,
        json!({"op":"set_sample","instrument":1,"finetune":1})
    )
    .contains("c2spd"));
}

#[test]
fn import_wav_into_each_format() {
    let source = sine(4410);
    let file = wav(44100, 1, &source);
    let data = b64(&file);

    // MOD: resampled to C-2's Paula rate, 8-bit, whole words.
    let mut m = doc("demo.mod");
    let r = ok(
        &mut m,
        json!({"op":"import_sample","instrument":3,"data":data,"name":"wavy"}),
    );
    let len = m.module().samples[2].data.len();
    assert_eq!(len % 2, 0);
    let expected = (4410.0 * (3546895.0 / 428.0) / 44100.0) as usize;
    assert!(len.abs_diff(expected) <= 2, "{len} against {expected}");
    assert_eq!(r["sample"]["name"], "wavy");
    assert!(m.module().samples[2]
        .data
        .iter()
        .all(|v| (v * 128.0).fract() == 0.0));
    // A MOD note picks the rate: C-3 plays twice as fast, so twice the frames.
    ok(
        &mut m,
        json!({"op":"import_sample","instrument":4,"data":b64(&file),"note":48}),
    );
    let double = m.module().samples[3].data.len();
    assert!(
        double.abs_diff(len * 2) <= 4,
        "{double} against {}",
        len * 2
    );
    // And the file saves.
    m.to_bytes(Format::Mod).unwrap();

    // S3M: kept at its rate, as c2spd; 8-bit by default.
    let mut s = doc("demo.s3m");
    ok(
        &mut s,
        json!({"op":"import_sample","instrument":1,"data":b64(&file)}),
    );
    assert_eq!(s.module().samples[0].c2spd, 44100);
    assert_eq!(s.module().samples[0].data.len(), 4410);
    ok(
        &mut s,
        json!({"op":"import_sample","instrument":2,"data":b64(&file),"target_rate":22050,"bits":16}),
    );
    assert_eq!(s.module().samples[1].c2spd, 22050);
    assert!(s.module().samples[1].data.len().abs_diff(2205) <= 1);
    s.to_bytes(Format::S3m).unwrap();

    // XM: 16-bit, the rate carried by the relative note and finetune.
    let mut x = doc("demo.xm");
    ok(
        &mut x,
        json!({"op":"import_sample","instrument":1,"data":b64(&file)}),
    );
    let s0 = &x.module().samples[x.module().instruments[0].samples[0]];
    assert_eq!(s0.data.len(), 4410);
    let semis = 12.0 * (44100.0f64 / 8363.0).log2();
    assert_eq!(s0.relative_note as f64, semis.round());
    assert!(((s0.finetune as f64 / 128.0) - (semis - semis.round())).abs() < 0.01);
    x.to_bytes(Format::Xm).unwrap();

    // Stereo mixes down, or picks a side.
    let stereo: Vec<f32> = source.iter().flat_map(|&v| [v, 0.0]).collect();
    let f2 = wav(44100, 2, &stereo);
    ok(
        &mut x,
        json!({"op":"import_sample","instrument":1,"data":b64(&f2),"mix":"left"}),
    );
    let left = x.module().samples[0].data.clone();
    ok(
        &mut x,
        json!({"op":"import_sample","instrument":1,"data":b64(&f2),"mix":"right"}),
    );
    assert!(left.iter().any(|&v| v.abs() > 0.5));
    assert!(x.module().samples[0].data.iter().all(|&v| v == 0.0));

    // Refusals.
    assert!(err(
        &mut x,
        json!({"op":"import_sample","instrument":1,"data":b64(b"not a wav file at all")})
    )
    .contains("not a WAV"));
    assert!(err(
        &mut x,
        json!({"op":"import_sample","instrument":1,"data":"!!!"})
    )
    .contains("base64"));
    let long = wav(48000, 1, &vec![0.1; 200_000]);
    // 200k frames at 48 kHz is too long for a MOD (the C-2 rate is 8287 Hz: ~35k frames, fine)...
    ok(
        &mut m,
        json!({"op":"import_sample","instrument":1,"data":b64(&long)}),
    );
    // ...but not at C-3 with the loop of rate 16.5 kHz: 69k frames still fine; force the limit with 5x longer.
    let huge = wav(8287, 1, &vec![0.1; 140_000]);
    assert!(err(
        &mut m,
        json!({"op":"import_sample","instrument":1,"data":b64(&huge)})
    )
    .contains("too long"));
    ok(
        &mut m,
        json!({"op":"import_sample","instrument":1,"data":b64(&huge),"truncate":true}),
    );
    assert_eq!(m.module().samples[0].data.len(), 131070);

    // Raw PCM.
    let raw: Vec<u8> = (0..1000).map(|i| ((i % 100) as i8 - 50) as u8).collect();
    ok(
        &mut x,
        json!({"op":"import_sample","instrument":1,"data":b64(&raw),"kind":"pcm8","rate":11025}),
    );
    assert_eq!(x.module().samples[0].data.len(), 1000);
    assert!(err(
        &mut x,
        json!({"op":"import_sample","instrument":1,"data":b64(&raw),"kind":"pcm8"})
    )
    .contains("rate"));
}

#[test]
fn wav_loops_and_exports() {
    let mut file = wav(8363, 1, &sine(800));
    // A smpl chunk with one forward loop of frames 100..=499.
    let mut smpl = vec![0u8; 36 + 24];
    smpl[28..32].copy_from_slice(&1u32.to_le_bytes());
    smpl[36 + 8..36 + 12].copy_from_slice(&100u32.to_le_bytes());
    smpl[36 + 12..36 + 16].copy_from_slice(&499u32.to_le_bytes());
    file.extend_from_slice(b"smpl");
    file.extend_from_slice(&(smpl.len() as u32).to_le_bytes());
    file.extend_from_slice(&smpl);
    let mut s = doc("demo.s3m");
    ok(
        &mut s,
        json!({"op":"import_sample","instrument":1,"data":b64(&file)}),
    );
    let sm = &s.module().samples[0];
    assert_eq!((sm.loop_start, sm.loop_len), (100, 400));
    // The sample leaves as a WAV at its own rate.
    let out = s
        .sample_wav(cubed_tracker::edit::SampleRef::new(1, 0))
        .unwrap();
    let pcm = cubed_tracker::edit::decode_wav(&out, Default::default()).unwrap();
    assert_eq!(pcm.rate, 8363);
    assert_eq!(pcm.data.len(), 800);
}

#[test]
fn generate_and_edit_samples() {
    let mut m = doc("demo.mod");
    let r = ok(
        &mut m,
        json!({"op":"generate_wave","instrument":2,"wave":"triangle","length":100}),
    );
    assert_eq!(r["sample"]["length"], 100);
    assert_eq!(r["sample"]["loop_kind"], "forward");
    let before = m.module().samples[1].data.clone();
    ok(&mut m, json!({"op":"reverse_sample","instrument":2}));
    let mut rev = before.clone();
    rev.reverse();
    assert_eq!(m.module().samples[1].data, rev);
    ok(&mut m, json!({"op":"normalize_sample","instrument":2}));
    let peak = m.module().samples[1]
        .data
        .iter()
        .fold(0f32, |p, v| p.max(v.abs()));
    assert!(peak >= 126.0 / 128.0, "{peak}");
    ok(&mut m, json!({"op":"clear_sample","instrument":2}));
    assert!(m.module().samples[1].data.is_empty());
    assert_eq!(
        m.module().samples[1].loop_kind,
        cubed_tracker::LoopKind::Off
    );
    assert!(err(
        &mut m,
        json!({"op":"generate_wave","instrument":2,"wave":"fizz"})
    )
    .contains("not a wave"));
    assert!(err(
        &mut m,
        json!({"op":"generate_wave","instrument":2,"wave":"sine","length":3})
    )
    .contains("4 to"));
    // Waveform peaks for drawing.
    let p = ok(&mut m, json!({"op":"peaks","instrument":1,"buckets":16}));
    assert_eq!(p["peaks"].as_array().unwrap().len(), 16);
}

#[test]
fn instruments_in_an_xm() {
    let mut x = ModuleDoc::new(Format::Xm);
    let r = ok(&mut x, json!({"op":"add_instrument","name":"lead"}));
    assert_eq!(r["instrument"], 1);
    ok(&mut x, json!({"op":"add_instrument"}));
    ok(
        &mut x,
        json!({"op":"generate_wave","instrument":1,"wave":"saw","length":64}),
    );
    ok(
        &mut x,
        json!({"op":"generate_wave","instrument":2,"wave":"square","length":32}),
    );
    let r = ok(&mut x, json!({"op":"add_sample","instrument":1}));
    assert_eq!(r["sample"], 1);
    ok(
        &mut x,
        json!({"op":"generate_wave","instrument":1,"sample":1,"wave":"sine","length":16}),
    );
    // Instruments number their samples across the module, in order.
    assert_eq!(x.module().instruments[0].samples, vec![0, 1]);
    assert_eq!(x.module().instruments[1].samples, vec![2]);
    ok(
        &mut x,
        json!({"op":"set_keymap","instrument":1,"from":60,"to":71,"sample":1}),
    );
    assert_eq!(x.module().instruments[0].keymap[48], 1);
    assert_eq!(x.module().instruments[0].keymap[47], 0);
    assert!(err(
        &mut x,
        json!({"op":"set_keymap","instrument":1,"from":60,"to":71,"sample":5})
    )
    .contains("sample 5"));
    assert!(err(
        &mut x,
        json!({"op":"set_keymap","instrument":1,"from":5,"to":71,"sample":0})
    )
    .contains("keys run"));
    // Envelopes.
    ok(
        &mut x,
        json!({"op":"set_envelope","instrument":1,"kind":"volume","enabled":true,"points":[[0,64],[10,32],[40,0]],"sustain":1,"loop":[1,2]}),
    );
    let e = x.module().instruments[0].volume_envelope.clone().unwrap();
    assert_eq!(e.points, vec![(0, 64), (10, 32), (40, 0)]);
    assert_eq!(e.sustain, Some(1));
    assert_eq!(e.looped, Some((1, 2)));
    assert!(err(
        &mut x,
        json!({"op":"set_envelope","instrument":1,"kind":"volume","points":[[0,64],[0,32]]})
    )
    .contains("rise"));
    assert!(err(
        &mut x,
        json!({"op":"set_envelope","instrument":1,"kind":"volume","points":[[1,64]]})
    )
    .contains("tick 0"));
    assert!(err(
        &mut x,
        json!({"op":"set_envelope","instrument":1,"kind":"volume","points":[[0,65]]})
    )
    .contains("0-64"));
    assert!(err(
        &mut x,
        json!({"op":"set_envelope","instrument":1,"kind":"volume","sustain":9})
    )
    .contains("does not exist"));
    assert!(err(
        &mut x,
        json!({"op":"set_envelope","instrument":1,"kind":"panning","sustain":0})
    )
    .contains("off"));
    ok(
        &mut x,
        json!({"op":"set_envelope","instrument":1,"kind":"panning","points":[[0,32]]}),
    );
    assert_eq!(
        x.module().instruments[0]
            .panning_envelope
            .as_ref()
            .unwrap()
            .points,
        vec![(0, 32)]
    );
    ok(
        &mut x,
        json!({"op":"set_envelope","instrument":1,"kind":"panning","enabled":false}),
    );
    assert!(x.module().instruments[0].panning_envelope.is_none());
    ok(
        &mut x,
        json!({"op":"set_instrument","instrument":1,"fadeout":512,"vibrato_depth":3,"vibrato_rate":20,"name":"Lead"}),
    );
    assert_eq!(x.module().instruments[0].fadeout, 512);
    assert!(err(
        &mut x,
        json!({"op":"set_instrument","instrument":1,"fadeout":5000})
    )
    .contains("0-4095"));
    assert!(err(
        &mut x,
        json!({"op":"set_instrument","instrument":1,"vibrato_depth":16})
    )
    .contains("0-15"));
    // The file keeps it all.
    let bytes = x.to_bytes(Format::Xm).unwrap();
    assert!(x.module().differences(&load(&bytes).unwrap()).is_empty());

    // Removing a sample remaps the keymap; removing an instrument renumbers the cells.
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":0,"channel":0,"note":60,"instrument":2}),
    );
    assert!(err(&mut x, json!({"op":"remove_instrument","instrument":2})).contains("is played"));
    ok(
        &mut x,
        json!({"op":"remove_sample","instrument":1,"sample":0}),
    );
    assert_eq!(x.module().instruments[0].samples, vec![0]);
    assert_eq!(x.module().instruments[0].keymap[48], 0);
    ok(&mut x, json!({"op":"add_instrument"}));
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":1,"channel":0,"note":60,"instrument":3}),
    );
    ok(
        &mut x,
        json!({"op":"remove_instrument","instrument":2,"force":true}),
    );
    assert_eq!(x.module().instruments.len(), 2);
    assert_eq!(cell(&x, 0, 0, 0)[1], 0);
    assert_eq!(cell(&x, 0, 1, 0)[1], 2);
    // The last sample of an instrument goes: it becomes plain.
    ok(
        &mut x,
        json!({"op":"remove_sample","instrument":1,"sample":0}),
    );
    assert!(x.module().instruments[0].samples.is_empty());
    assert!(x.module().instruments[0].volume_envelope.is_none());
    let bytes = x.to_bytes(Format::Xm).unwrap();
    assert!(x.module().differences(&load(&bytes).unwrap()).is_empty());
    assert!(err(
        &mut x,
        json!({"op":"set_envelope","instrument":1,"kind":"volume","enabled":true})
    )
    .contains("add a sample first"));
}

#[test]
fn instrument_limits() {
    let mut x = ModuleDoc::new(Format::Xm);
    for _ in 0..128 {
        ok(&mut x, json!({"op":"add_instrument"}));
    }
    assert!(err(&mut x, json!({"op":"add_instrument"})).contains("128"));
    for _ in 0..15 {
        ok(&mut x, json!({"op":"add_sample","instrument":1}));
    }
    assert!(err(&mut x, json!({"op":"add_sample","instrument":1})).contains("16"));
    let mut m = ModuleDoc::new(Format::Mod);
    assert!(err(&mut m, json!({"op":"add_instrument"})).contains("31 sample slots"));
    let mut s = ModuleDoc::new(Format::S3m);
    for _ in 0..99 {
        ok(&mut s, json!({"op":"add_instrument"}));
    }
    assert!(err(&mut s, json!({"op":"add_instrument"})).contains("99"));
    assert!(err(&mut s, json!({"op":"remove_instrument","instrument":3})).contains("last"));
    ok(&mut s, json!({"op":"remove_instrument","instrument":99}));
    assert_eq!(s.module().samples.len(), 98);
    s.to_bytes(Format::S3m).unwrap();
}

#[test]
fn s3m_adlib_instrument_edit() {
    let mut s = doc("demo.s3m");
    let slot = s
        .module()
        .samples
        .iter()
        .position(|x| x.adlib.is_some())
        .expect("the demo has an AdLib instrument")
        + 1;
    let r = ok(
        &mut s,
        json!({"op":"set_sample","instrument":slot,"volume":30}),
    );
    assert_eq!(r["sample"]["adlib"]["melodic"], true);
    assert!(err(
        &mut s,
        json!({"op":"set_sample","instrument":slot,"loop_kind":"forward"})
    )
    .contains("no loop"));
    assert!(err(
        &mut s,
        json!({"op":"import_sample","instrument":slot,"data":b64(&wav(8000, 1, &sine(100)))})
    )
    .contains("AdLib"));
    ok(&mut s, json!({"op":"add_instrument"}));
    let n = s.module().samples.len();
    let regs: Vec<u8> = (1..=12).collect();
    ok(
        &mut s,
        json!({"op":"set_sample","instrument":n,"adlib":{"registers":regs,"melodic":true}}),
    );
    s.to_bytes(Format::S3m).unwrap();
}

// ---------------------------------------------------------------------------
// writing

#[test]
fn new_modules_save_and_reload() {
    for format in [Format::Mod, Format::S3m, Format::Xm] {
        let d = ModuleDoc::new(format);
        let bytes = d.to_bytes(format).unwrap();
        let back = load(&bytes).unwrap();
        assert_eq!(
            d.module().differences(&back),
            Vec::<String>::new(),
            "{format:?}"
        );
    }
}

#[test]
fn to_bytes_other_format_is_refused() {
    let d = ModuleDoc::new(Format::Xm);
    assert!(d
        .to_bytes(Format::Mod)
        .unwrap_err()
        .contains("not supported"));
}

#[test]
fn validate_names_what_the_writer_would_refuse() {
    let mut m = doc("demo.mod");
    assert!(m.validate().is_empty());
    // A hand-made module the format cannot hold.
    let mut module = m.module().clone();
    module.patterns[0].cell_mut(0, 0).note = Note::On(80);
    module.patterns[0].cell_mut(0, 0).period = 0;
    let d = ModuleDoc::from_module(module);
    let problems = d.validate();
    assert!(problems.iter().any(|p| p.contains("range")), "{problems:?}");
    assert!(
        problems.iter().any(|p| p.contains("cannot be saved")),
        "{problems:?}"
    );
    let _ = &mut m;
}

// ---------------------------------------------------------------------------
// undo and redo

#[test]
fn undo_redo_basics() {
    let mut x = ModuleDoc::new(Format::Xm);
    let start = x.module().clone();
    assert!(x.undo().is_none());
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":0,"channel":0,"note":60}),
    );
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":1,"channel":0,"note":62}),
    );
    assert_eq!(x.info()["undo_steps"], 2);
    let c = x.undo().unwrap();
    assert_eq!(
        c.patterns[&0],
        (1, 1),
        "the undo reports the rows it changed"
    );
    assert_eq!(
        cell(&x, 0, 1, 0)[0],
        start.patterns[0].cell(1, 0).note_number()
    );
    assert!(x.can_redo());
    x.redo().unwrap();
    assert_eq!(cell(&x, 0, 1, 0)[0], 62);
    // A new edit ends the redo line.
    x.undo().unwrap();
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":2,"channel":0,"note":64}),
    );
    assert!(!x.can_redo());
    while x.undo().is_some() {}
    assert_eq!(*x.module(), start);
}

trait NoteNumber {
    fn note_number(&self) -> u64;
}
impl NoteNumber for cubed_tracker::Cell {
    fn note_number(&self) -> u64 {
        match self.note {
            Note::None => 0,
            Note::Off => 255,
            Note::On(n) => n as u64,
        }
    }
}

#[test]
fn groups_are_one_step() {
    let mut x = doc("demo.xm");
    let start = x.module().clone();
    x.begin_group();
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":0,"channel":0,"note":60}),
    );
    ok(&mut x, json!({"op":"insert_row","pattern":0,"row":0}));
    ok(&mut x, json!({"op":"set_speed","speed":3}));
    x.begin_group();
    ok(&mut x, json!({"op":"set_title","title":"nested"}));
    x.end_group();
    assert!(x.undo().is_none(), "a group is open");
    x.end_group();
    assert_eq!(x.info()["undo_steps"], 1);
    x.undo().unwrap();
    assert_eq!(*x.module(), start);
    x.redo().unwrap();
    assert_eq!(x.module().title, "nested");

    // A batch is one step too, and all or nothing.
    let mut y = doc("demo.xm");
    let start = y.module().clone();
    let r = ok(
        &mut y,
        json!({"op":"batch","ops":[
            {"op":"set_cell","pattern":0,"row":0,"channel":0,"note":60},
            {"op":"set_cell","pattern":0,"row":3,"channel":1,"note":61},
        ]}),
    );
    assert_eq!(r["results"].as_array().unwrap().len(), 2);
    assert_eq!(y.info()["undo_steps"], 1);
    let failing = json!({"op":"batch","ops":[
        {"op":"set_cell","pattern":0,"row":4,"channel":0,"note":60},
        {"op":"set_cell","pattern":0,"row":5,"channel":0,"note":999},
    ]});
    let after_good = y.module().clone();
    let e = y.apply(&failing).unwrap_err();
    assert!(e.contains("operation 1"), "{e}");
    assert_eq!(*y.module(), after_good);
    y.undo().unwrap();
    assert_eq!(*y.module(), start);
}

#[test]
fn history_is_bounded() {
    let mut x = doc("demo.xm");
    x.set_history_limits(5, usize::MAX);
    for r in 0..20 {
        ok(
            &mut x,
            json!({"op":"set_cell","pattern":0,"row":r,"channel":0,"note":60}),
        );
    }
    assert_eq!(x.info()["undo_steps"], 5);
    // A byte bound keeps the newest step at least.
    x.set_history_limits(1000, 1);
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":30,"channel":0,"note":60}),
    );
    assert_eq!(x.info()["undo_steps"], 1);
    // Sample edits cost their data, not the module.
    let mut y = doc("demo.xm");
    let before = y.info()["history_bytes"].as_u64().unwrap();
    ok(
        &mut y,
        json!({"op":"set_sample","instrument":1,"volume":10}),
    );
    let after = y.info()["history_bytes"].as_u64().unwrap();
    assert!(after - before < 1000, "{}", after - before);
}

// ---------------------------------------------------------------------------
// the soak: random operations, undo everything, redo everything

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
    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

fn random_op(d: &ModuleDoc, rng: &mut Rng) -> Value {
    let m = d.module();
    let pattern = rng.below(m.patterns.len());
    let rows = m.patterns[pattern].rows();
    let row = rng.below(rows);
    let row1 = (row + rng.below(8)).min(rows - 1);
    let ch = rng.below(m.channels);
    let ch1 = (ch + rng.below(3)).min(m.channels - 1);
    let (lo, hi) = {
        let i = d.info();
        (
            i["note"]["min"].as_u64().unwrap() as usize,
            i["note"]["max"].as_u64().unwrap() as usize,
        )
    };
    let slots = d.info()["instruments"].as_u64().unwrap() as usize;
    let note = lo + rng.below(hi - lo + 1);
    let instrument = if slots == 0 { 0 } else { rng.below(slots + 1) };
    let target = rng.below(slots.max(1)) + 1;
    match rng.below(40) {
        0..=7 => {
            let mut o = json!({"op":"set_cell","pattern":pattern,"row":row,"channel":ch});
            if rng.chance(70) {
                o["note"] = json!(note);
            }
            if rng.chance(50) {
                o["instrument"] = json!(instrument);
            }
            if rng.chance(30) {
                o["volume"] = json!(0x10 + rng.below(0x41));
            }
            if rng.chance(40) {
                o["effect"] = json!(rng.below(16));
                o["param"] = json!(rng.below(256));
            }
            o
        }
        8 => {
            json!({"op":"clear_range","pattern":pattern,"row0":row,"row1":row1,"channel0":ch,"channel1":ch1})
        }
        9 => json!({"op":"insert_row","pattern":pattern,"row":row,"count":1 + rng.below(3)}),
        10 => json!({"op":"delete_row","pattern":pattern,"row":row,"channel0":ch,"channel1":ch1}),
        11 => {
            json!({"op":"transpose","pattern":pattern,"row0":row,"row1":row1,"channel0":ch,"channel1":ch1,"semitones":rng.below(25) as i64 - 12,"clamp":rng.chance(50)})
        }
        12 | 13 => {
            let clip = d
                .copy_range(Region {
                    pattern,
                    row0: row,
                    row1,
                    channel0: ch,
                    channel1: ch1,
                })
                .unwrap()
                .to_json(m.format);
            json!({"op":"paste","pattern":rng.below(m.patterns.len()),"row":rng.below(8),"channel":rng.below(m.channels),"cells":clip,"mode":if rng.chance(50) {"mix"} else {"overwrite"}})
        }
        14 => {
            json!({"op":"set_pattern_rows","pattern":pattern,"rows":1 + rng.below(80),"force":rng.chance(60)})
        }
        15 => json!({"op":"new_pattern"}),
        16 => json!({"op":"duplicate_pattern","pattern":pattern}),
        17 => json!({"op":"delete_pattern","pattern":pattern}),
        18 => {
            json!({"op":"insert_order","index":rng.below(m.orders.len() + 1),"pattern":rng.below(m.patterns.len())})
        }
        19 => {
            json!({"op":"set_order","index":rng.below(m.orders.len()),"pattern":rng.below(m.patterns.len())})
        }
        20 => json!({"op":"delete_order","index":rng.below(m.orders.len())}),
        21 => {
            json!({"op":"move_order","from":rng.below(m.orders.len()),"to":rng.below(m.orders.len())})
        }
        22 => json!({"op":"set_restart","order":rng.below(m.orders.len())}),
        23 => json!({"op":"set_speed","speed":1 + rng.below(40)}),
        24 => json!({"op":"set_tempo","tempo":30 + rng.below(100)}),
        25 => json!({"op":"set_title","title":format!("song {}", rng.below(1000))}),
        26 => json!({"op":"set_channels","count":1 + rng.below(12),"force":rng.chance(70)}),
        27 => {
            let mut o = json!({"op":"set_sample","instrument":target,"sample":0,"volume":rng.below(66),"loop_kind":(["off","forward","pingpong"][rng.below(3)]),"loop_start":rng.below(100),"loop_len":rng.below(300),"name":format!("s{}", rng.below(99))});
            match m.format {
                Format::Mod => o["finetune"] = json!(rng.below(18) as i64 - 8),
                Format::S3m => o["c2spd"] = json!(100 + rng.below(30000)),
                Format::Xm => {
                    o["finetune"] = json!(rng.below(260) as i64 - 130);
                    o["relative_note"] = json!(rng.below(200) as i64 - 98);
                    o["pan"] = json!(rng.below(256));
                }
            }
            if rng.chance(60) {
                o["loop_kind"] = json!(if rng.chance(50) { "off" } else { "forward" });
            }
            o
        }
        28 => {
            let rate = [8000u32, 11025, 22050, 44100][rng.below(4)];
            let frames = 100 + rng.below(3000);
            json!({"op":"import_sample","instrument":target,"data":b64(&wav(rate, 1, &sine(frames))),"normalize":rng.chance(50),"bits":if rng.chance(50) {8} else {16}})
        }
        29 => json!({"op":"reverse_sample","instrument":target}),
        30 => json!({"op":"normalize_sample","instrument":target}),
        31 => json!({"op":"clear_sample","instrument":target}),
        32 => {
            json!({"op":"generate_wave","instrument":target,"wave":(["sine","triangle","saw","square","pulse","noise"][rng.below(6)]),"length":4 + rng.below(500)})
        }
        33 => json!({"op":"add_instrument"}),
        34 => json!({"op":"remove_instrument","instrument":target,"force":rng.chance(70)}),
        35 => json!({"op":"add_sample","instrument":target}),
        36 => json!({"op":"remove_sample","instrument":target,"sample":rng.below(3)}),
        37 => {
            json!({"op":"set_envelope","instrument":target,"kind":(["volume","panning"][rng.below(2)]),"enabled":rng.chance(80),"points":[[0,rng.below(65)],[1 + rng.below(50),rng.below(65)],[60 + rng.below(100),rng.below(65)]],"sustain":rng.below(3),"loop":if rng.chance(50) {json!([0, 1 + rng.below(2)])} else {Value::Null}})
        }
        38 => {
            json!({"op":"set_keymap","instrument":target,"from":12 + rng.below(40),"to":12 + rng.below(90),"sample":rng.below(3)})
        }
        _ => {
            json!({"op":"set_instrument","instrument":target,"fadeout":rng.below(5000),"vibrato_depth":rng.below(17),"vibrato_rate":rng.below(64)})
        }
    }
}

/// Only what a MOD's file cannot hold may differ after a reload.
fn mod_flags_only(diffs: &[String]) -> bool {
    diffs.iter().all(|d| {
        d.starts_with("flags") || d.starts_with("mod_flavor") || d.starts_with("stereo_separation")
    })
}

fn soak(name: &str, seeds: std::ops::Range<u64>, steps: usize) {
    let mut tally: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for seed in seeds {
        soak_one(
            name,
            ModuleDoc::from_module(load(&fixture(name)).unwrap()),
            seed,
            steps,
            &mut tally,
        );
    }
    if std::env::var_os("CUBED_SOAK_STATS").is_some() {
        eprintln!("{name}: operations applied/refused: {tally:?}");
    }
}

fn soak_one(
    name: &str,
    mut d: ModuleDoc,
    seed: u64,
    steps: usize,
    tally: &mut std::collections::BTreeMap<String, (usize, usize)>,
) {
    {
        let format = d.format();
        let start = d.module().clone();
        let mut rng = Rng(seed.wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(1));
        let mut applied = 0;
        let mut snapshots: Vec<Module> = vec![start.clone()];
        for step in 0..steps {
            let grouped = rng.chance(15);
            if grouped {
                d.begin_group();
            }
            let n = if grouped { 1 + rng.below(3) } else { 1 };
            for _ in 0..n {
                let op = random_op(&d, &mut rng);
                let before = d.module().clone();
                let result = d.apply(&op);
                let entry = tally
                    .entry(op["op"].as_str().unwrap().to_string())
                    .or_default();
                match &result {
                    Ok(_) => entry.0 += 1,
                    Err(_) => entry.1 += 1,
                }
                match result {
                    Ok(_) => {
                        applied += 1;
                        // Every state reached can be saved, and loads back equal.
                        match write(d.module()) {
                            Ok(bytes) => {
                                let back = load(&bytes).unwrap_or_else(|e| panic!("{name} seed {seed} step {step}: {op}: the saved file does not load: {e}"));
                                let diffs = d.module().differences(&back);
                                let fine = if format == Format::Mod { mod_flags_only(&diffs) } else { diffs.is_empty() };
                                assert!(fine, "{name} seed {seed} step {step}: {op}: reload differs: {diffs:?}");
                            }
                            // Two things a file cannot hold once edited: the 15-sample
                            // MOD layouts' flavour is guessed from the data, and ST3
                            // plays channels that share a hardware channel as one.
                            Err(e) if e.contains("load back as") || e.contains("shares a hardware channel") => {}
                            Err(e) => panic!("{name} seed {seed} step {step}: {op}: the module cannot be saved: {e}"),
                        }
                    }
                    Err(_) => assert_eq!(
                        *d.module(),
                        before,
                        "{name} seed {seed}: refused {op} changed the module"
                    ),
                }
            }
            if grouped {
                d.end_group();
            }
            // One more undo step means one more state to come back through.
            let steps = d.info()["undo_steps"].as_u64().unwrap() as usize;
            assert!(steps <= snapshots.len(), "a step made two undo steps");
            if steps == snapshots.len() {
                snapshots.push(d.module().clone());
            }
        }
        assert!(
            applied > steps / 5,
            "{name}: only {applied} of {steps} operations applied"
        );
        let end = d.module().clone();
        // Undo walks back through every state the steps made.
        let mut at = snapshots.len() - 1;
        while d.undo().is_some() {
            at -= 1;
            assert_eq!(
                *d.module(),
                snapshots[at],
                "{name} seed {seed}: undo went to the wrong state"
            );
        }
        assert_eq!(at, 0, "{name} seed {seed}: fewer undo steps than states");
        assert_eq!(
            *d.module(),
            start,
            "{name} seed {seed}: undone to the start"
        );
        while d.redo().is_some() {}
        assert_eq!(*d.module(), end, "{name} seed {seed}: redone to the end");
    }
}

#[test]
fn soak_xm() {
    soak("demo.xm", 0..6, 120);
}

#[test]
fn soak_s3m() {
    soak("demo.s3m", 0..6, 120);
}

#[test]
fn soak_mod() {
    soak("demo.mod", 0..6, 120);
}

#[test]
fn soak_new_modules() {
    // From scratch, where there are few samples and instruments to begin with.
    for format in [Format::Mod, Format::S3m, Format::Xm] {
        for seed in 100..104 {
            let mut d = ModuleDoc::new(format);
            let mut rng = Rng(seed * 7919 + 3);
            for _ in 0..8 {
                let _ = d.apply(&json!({"op":"add_instrument"}));
            }
            for _ in 0..150 {
                let op = random_op(&d, &mut rng);
                let _ = d.apply(&op);
                let bytes = write(d.module())
                    .unwrap_or_else(|e| panic!("{format:?} seed {seed}: {op}: {e}"));
                let back = load(&bytes).unwrap();
                let diffs = d.module().differences(&back);
                let fine = if format == Format::Mod {
                    mod_flags_only(&diffs)
                } else {
                    diffs.is_empty()
                };
                assert!(fine, "{format:?} seed {seed}: {op}: {diffs:?}");
            }
            while d.undo().is_some() {}
            assert_eq!(
                *d.module(),
                ModuleDoc::new(format).module().clone(),
                "{format:?} seed {seed}"
            );
        }
    }
}

/// Real songs, edited at random: `CUBED_MODULES=<dir>` (searched recursively).
#[test]
fn real_songs_edit() {
    let Some(dir) = std::env::var_os("CUBED_MODULES") else {
        return;
    };
    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    collect(std::path::Path::new(&dir), &mut files);
    files.sort();
    let mut tally = Default::default();
    let mut count = 0;
    for (i, path) in files.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(d) = ModuleDoc::from_bytes(&bytes) else {
            continue;
        };
        soak_one(&path.display().to_string(), d, i as u64 + 1, 40, &mut tally);
        count += 1;
    }
    eprintln!("{count} real songs edited at random, saved after every operation, and undone");
}

// ---------------------------------------------------------------------------
// live edits: what a playing song takes

/// A player that has been given every live edit of an edit session holds the
/// same pattern cells as the document.
fn follow(name: &str, ops: &[Value]) {
    use cubed_tracker::{Player, PlayerOptions};
    let mut d = doc(name);
    let mut player = Player::new(d.module().clone(), 48_000.0, PlayerOptions::default());
    for op in ops {
        let r = d.apply(op);
        assert!(r.is_ok(), "{op}: {r:?}");
        match d.live_edits() {
            Some(edits) => {
                for e in edits {
                    assert!(
                        player.set_cell(
                            e.pattern as usize,
                            e.row as usize,
                            e.channel as usize,
                            e.cell()
                        ),
                        "{op}: the player has no cell {e:?}"
                    );
                }
            }
            None => player = Player::new(d.module().clone(), 48_000.0, PlayerOptions::default()),
        }
        assert_eq!(
            player.module().patterns,
            d.module().patterns,
            "{name}: after {op}"
        );
    }
    // Undo and redo are live edits too.
    while let Some(_) = d.undo() {
        match d.live_edits() {
            Some(edits) => {
                for e in edits {
                    assert!(player.set_cell(
                        e.pattern as usize,
                        e.row as usize,
                        e.channel as usize,
                        e.cell()
                    ));
                }
            }
            None => player = Player::new(d.module().clone(), 48_000.0, PlayerOptions::default()),
        }
        assert_eq!(
            player.module().patterns,
            d.module().patterns,
            "{name}: after an undo"
        );
    }
}

#[test]
fn live_edits_keep_a_player_in_step() {
    let ops = [
        json!({"op":"set_cell","pattern":0,"row":3,"channel":1,"note":48,"instrument":1,"effect":"C","param":20}),
        json!({"op":"clear_range","pattern":0,"row0":0,"row1":7,"channel0":0,"channel1":1}),
        json!({"op":"insert_row","pattern":0,"row":4,"count":2}),
        json!({"op":"delete_row","pattern":0,"row":1,"channel":0}),
        json!({"op":"transpose","pattern":0,"row0":0,"row1":20,"channel0":0,"channel1":1,"semitones":2,"clamp":true}),
        json!({"op":"batch","ops":[
            {"op":"set_cell","pattern":0,"row":9,"channel":0,"note":50,"instrument":1},
            {"op":"set_cell","pattern":0,"row":10,"channel":0,"note":52,"instrument":1},
        ]}),
        json!({"op":"set_speed","speed":3}),
    ];
    for name in ["demo.mod", "demo.s3m", "demo.xm"] {
        follow(name, &ops);
    }
}

#[test]
fn live_edits_say_when_the_song_must_be_reopened() {
    let mut x = doc("demo.xm");
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":2,"channel":0,"note":60,"instrument":1}),
    );
    let edits = x.live_edits().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(
        (
            edits[0].pattern,
            edits[0].row,
            edits[0].channel,
            edits[0].note
        ),
        (0, 2, 0, 60)
    );
    let r = ok(&mut x, json!({"op":"live_edits"}));
    assert_eq!(r["cells"][0]["note"], 60);
    // A change with no effect has nothing to send.
    ok(
        &mut x,
        json!({"op":"set_cell","pattern":0,"row":2,"channel":0,"note":60}),
    );
    assert_eq!(x.live_edits().unwrap().len(), 0);
    // Shape changes need a new song.
    for op in [
        json!({"op":"set_speed","speed":3}),
        json!({"op":"set_pattern_rows","pattern":0,"rows":32,"force":true}),
        json!({"op":"insert_order","index":0,"pattern":0}),
        json!({"op":"set_sample","instrument":1,"volume":3}),
        json!({"op":"new_pattern"}),
        json!({"op":"set_channels","count":3,"force":true}),
        json!({"op":"set_title","title":"x"}),
    ] {
        ok(&mut x, op.clone());
        assert!(x.live_edits().is_none(), "{op}");
        assert_eq!(
            x.apply(&json!({"op":"live_edits"})).unwrap()["cells"],
            Value::Null
        );
    }
    // A change too big to list.
    let mut big = ModuleDoc::new(Format::Xm);
    ok(
        &mut big,
        json!({"op":"set_pattern_rows","pattern":0,"rows":256}),
    );
    ok(&mut big, json!({"op":"set_channels","count":32}));
    let one = json!({"op":"set_cell","pattern":0,"row":0,"channel":0,"note":60});
    ok(&mut big, one);
    let clip = json!({"cells": (0..256).map(|_| (0..32).map(|_| json!([60, 0, 0, 0, 0])).collect::<Vec<_>>()).collect::<Vec<_>>()});
    ok(
        &mut big,
        json!({"op":"paste","pattern":0,"row":0,"channel":0,"cells":clip}),
    );
    assert!(big.live_edits().is_none());
    // Undo reports the cell it put back.
    let mut u = ModuleDoc::new(Format::Mod);
    ok(
        &mut u,
        json!({"op":"set_cell","pattern":0,"row":5,"channel":2,"note":"C-2","instrument":1}),
    );
    u.undo().unwrap();
    let edits = u.live_edits().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].cell(), Default::default());
}
