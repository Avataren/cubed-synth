//! A random-operation soak over `SidDoc`: every kind of operation, valid and not,
//! grouped and not, on the fixtures and on empty songs. After every step the song
//! is a valid one that saves as a `.asid` and loads back equal, and as a `.sng`
//! (or is refused for a reason a `.sng` really has) and loads back equal; a
//! refused operation changed nothing; at the end every undo lands on the state
//! the step before it left, and redoing everything lands on the end.
//!
//! `CUBED_SOAK_STATS=1` prints how many of each operation applied and were
//! refused; `CUBED_GT_SONGS=<dir>` soaks every `.sng` below a folder too.

use std::collections::BTreeMap;
use std::path::Path;

use cubed_sid::edit::SidDoc;
use cubed_sid::goattracker::{export, import, GtSongHints};
use cubed_sid::song::SidSong;
use serde_json::{json, Value};

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
    fn byte(&mut self) -> u8 {
        self.below(256) as u8
    }
}

fn table_name(rng: &mut Rng) -> &'static str {
    ["wave", "pulse", "filter", "speed"][rng.below(4)]
}

fn random_op(d: &SidDoc, rng: &mut Rng) -> Value {
    let s = d.song();
    let pattern = rng.below(s.patterns.len());
    let rows = s.patterns[pattern].rows.len();
    let row = rng.below(rows);
    let row1 = (row + rng.below(8)).min(rows - 1);
    let subsong = rng.below(s.subsongs.len());
    let voice = rng.below(3);
    let list_len = s.subsongs[subsong].orderlists[voice].entries.len();
    let instruments = s.instruments.len();
    let table = table_name(rng);
    let table_len = match table {
        "wave" => s.tables.wave.len(),
        "pulse" => s.tables.pulse.len(),
        "filter" => s.tables.filter.len(),
        _ => s.tables.speed.len(),
    };
    // Mostly valid numbers, sometimes one a step out of range.
    let near = |rng: &mut Rng, n: usize| {
        let past = usize::from(rng.chance(5));
        rng.below(n + 1 + past)
    };
    match rng.below(40) {
        0..=3 => {
            let note: usize = if rng.chance(10) {
                [0, 126, 127][rng.below(3)]
            } else if rng.chance(3) {
                rng.byte() as usize
            } else {
                1 + rng.below(93)
            };
            let mut op = json!({"op": "set_cell", "pattern": pattern, "row": row, "note": note});
            if rng.chance(60) {
                op["instrument"] = json!(near(rng, instruments));
            }
            if rng.chance(50) {
                op["command"] = json!(rng.below(16));
                op["param"] = json!(rng.byte());
            }
            op
        }
        4 => json!({"op": "clear_range", "pattern": pattern, "row0": row, "row1": row1,
            "columns": ([vec!["note"], vec!["instrument"], vec!["command"], vec!["note", "command"]][rng.below(4)])}),
        5 => {
            json!({"op": "insert_row", "pattern": pattern, "row": row, "count": 1 + rng.below(3), "grow": rng.chance(30)})
        }
        6 => {
            json!({"op": "delete_row", "pattern": pattern, "row": row, "count": 1 + rng.below(3), "shrink": rng.chance(30)})
        }
        7 => json!({"op": "transpose", "pattern": pattern, "row0": row, "row1": row1,
            "semitones": rng.below(29) as i64 - 14, "clamp": rng.chance(50),
            "instrument": if rng.chance(30) { json!(rng.below(instruments + 1)) } else { Value::Null }}),
        8 => {
            let cells: Vec<Value> = (0..1 + rng.below(6))
                .map(|_| {
                    json!([
                        if rng.chance(50) { 1 + rng.below(93) } else { 0 },
                        near(rng, instruments),
                        rng.below(16),
                        rng.byte()
                    ])
                })
                .collect();
            json!({"op": "paste", "pattern": pattern, "row": row, "cells": cells, "mode": (["overwrite", "mix"][rng.below(2)])})
        }
        9 => {
            json!({"op": "set_pattern_rows", "pattern": pattern, "rows": 1 + rng.below(129), "force": rng.chance(50)})
        }
        10 => json!({"op": "new_pattern", "rows": 1 + rng.below(70)}),
        11 => json!({"op": "duplicate_pattern", "pattern": pattern}),
        12 => json!({"op": "delete_pattern", "pattern": pattern, "force": rng.chance(40)}),
        13 => {
            json!({"op": "set_order", "subsong": subsong, "voice": voice, "index": near(rng, list_len - 1),
            "pattern": rng.below(s.patterns.len() + 1), "transpose": rng.below(40) as i64 - 18, "repeat": rng.below(18)})
        }
        14 => {
            json!({"op": "insert_order", "subsong": subsong, "voice": if rng.chance(20) { json!("all") } else { json!(voice) },
            "index": rng.below(list_len + 1), "pattern": rng.below(s.patterns.len()),
            "transpose": rng.below(34) as i64 - 17, "repeat": 1 + rng.below(16)})
        }
        15 => {
            json!({"op": "delete_order", "subsong": subsong, "voice": voice, "index": near(rng, list_len - 1)})
        }
        16 => {
            json!({"op": "move_order", "subsong": subsong, "voice": voice, "from": rng.below(list_len), "to": near(rng, list_len - 1)})
        }
        17 => {
            json!({"op": "set_restart", "subsong": subsong, "voice": voice, "restart": near(rng, list_len - 1)})
        }
        18 => {
            json!({"op": "add_subsong", "copy_of": if rng.chance(50) { json!(subsong) } else { Value::Null }, "rows": 1 + rng.below(40)})
        }
        19 => json!({"op": "remove_subsong", "subsong": subsong}),
        20 => json!({"op": "select_subsong", "subsong": rng.below(s.subsongs.len() + 1)}),
        21..=22 => {
            let mut op = json!({"op": "set_instrument", "instrument": rng.below(instruments + 2)});
            for (key, max) in [
                ("attack", 15),
                ("decay", 15),
                ("sustain", 15),
                ("release", 15),
                ("gate_timer", 63),
                ("vibrato_delay", 255),
                ("first_wave", 255),
            ] {
                if rng.chance(25) {
                    op[key] = json!(near(rng, max));
                }
            }
            for key in ["hard_restart", "no_gate_off"] {
                if rng.chance(20) {
                    op[key] = json!(rng.chance(50));
                }
            }
            for (key, len) in [
                ("wave_ptr", s.tables.wave.len()),
                ("pulse_ptr", s.tables.pulse.len()),
                ("filter_ptr", s.tables.filter.len()),
                ("speed_ptr", s.tables.speed.len()),
            ] {
                if rng.chance(20) {
                    op[key] = json!(near(rng, len));
                }
            }
            if rng.chance(20) {
                op["name"] =
                    json!(["", "Lead", "Bass", "Noise hit", "0123456789abcdef"][rng.below(5)]);
            }
            op
        }
        23 => json!({"op": "add_instrument", "name": "n", "tables": rng.chance(70)}),
        24 => {
            json!({"op": "clone_instrument", "instrument": rng.below(instruments + 1), "to": if rng.chance(25) { json!(rng.below(instruments + 1)) } else { Value::Null }})
        }
        25 => {
            json!({"op": "delete_instrument", "instrument": rng.below(instruments + 1), "clear_uses": rng.chance(60)})
        }
        26 => json!({"op": "clear_instrument", "instrument": rng.below(instruments + 1)}),
        27..=29 => json!({"op": "set_table_row", "table": table, "row": near(rng, table_len),
            "left": if rng.chance(30) { json!(0xFF) } else { json!(rng.byte()) }, "right": rng.byte()}),
        30..=31 => json!({"op": "insert_table_row", "table": table, "row": near(rng, table_len),
            "left": if rng.chance(30) { json!(0xFF) } else { json!(rng.byte()) }, "right": rng.below(table_len + 2)}),
        32 => {
            json!({"op": "delete_table_row", "table": table, "row": near(rng, table_len.saturating_sub(1))})
        }
        33 => {
            json!({"op": "clear_table_row", "table": table, "row": near(rng, table_len.saturating_sub(1))})
        }
        34 => json!({"op": "remove_unused_table_rows"}),
        35 => {
            json!({"op": "set_texts", "title": (["", "Tune", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "é"][rng.below(4)]), "author": "me"})
        }
        36 => json!({"op": "set_model", "model": (["6581", "8580"][rng.below(2)])}),
        37 => json!({"op": "set_speed_multiplier", "multiplier": 1 + rng.below(16)}),
        38 => {
            json!({"op": "set_tempo", "tempo": if rng.chance(70) { 6 } else { 1 + rng.below(127) }})
        }
        _ => json!({"op": "bake_tempo"}),
    }
}

/// What a `.sng` can honestly refuse: nothing about the SID model itself.
fn is_sng_limit(e: &str) -> bool {
    e.contains("no tempo field")
        || e.contains("-16..+14")
        || e.contains("holds 254")
        || e.contains("NUL")
}

/// Every state is a song that saves and loads back as itself.
fn check_saves(d: &SidDoc, label: &str) {
    let s = d.song();
    s.validate()
        .unwrap_or_else(|e| panic!("{label}: the song is invalid: {e}"));
    let back = SidSong::parse(&s.to_bytes())
        .unwrap_or_else(|e| panic!("{label}: the .asid does not load: {e}"));
    assert_eq!(&back, s, "{label}: the .asid loads back different");
    match export(s) {
        Ok(sng) => {
            let hints = GtSongHints {
                model: Some(s.model),
                speed_multiplier: Some(s.speed_multiplier),
            };
            let back = import(&sng.bytes, hints)
                .unwrap_or_else(|e| panic!("{label}: the .sng does not load: {e}"))
                .song;
            assert_eq!(&back, s, "{label}: the .sng loads back different");
        }
        Err(e) => assert!(
            is_sng_limit(&e.0),
            "{label}: the .sng is refused for a reason that is not a .sng limit: {e}"
        ),
    }
}

fn soak_one(
    name: &str,
    mut d: SidDoc,
    seed: u64,
    steps: usize,
    tally: &mut BTreeMap<String, (usize, usize)>,
) {
    let start = d.song().clone();
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(1));
    let mut applied = 0;
    let mut snapshots: Vec<SidSong> = vec![start.clone()];
    for step in 0..steps {
        let grouped = rng.chance(15);
        if grouped {
            d.begin_group();
        }
        for _ in 0..if grouped { 1 + rng.below(3) } else { 1 } {
            let op = random_op(&d, &mut rng);
            let label = format!("{name} seed {seed} step {step}: {op}");
            let before = d.song().clone();
            let result = d.apply(&op);
            let entry = tally
                .entry(op["op"].as_str().unwrap().to_string())
                .or_default();
            match result {
                Ok(_) => {
                    entry.0 += 1;
                    applied += 1;
                    check_saves(&d, &label);
                }
                Err(_) => {
                    entry.1 += 1;
                    assert_eq!(
                        *d.song(),
                        before,
                        "{label}: refused, yet it changed the song"
                    );
                }
            }
        }
        if grouped {
            d.end_group();
        }
        let undo_steps = d.info()["undo_steps"].as_u64().unwrap() as usize;
        assert!(
            undo_steps <= snapshots.len(),
            "{name} seed {seed} step {step}: a step made two undo steps"
        );
        if undo_steps == snapshots.len() {
            snapshots.push(d.song().clone());
        }
    }
    assert!(
        applied > steps / 5,
        "{name}: only {applied} of {steps} operations applied"
    );
    let end = d.song().clone();
    let mut at = snapshots.len() - 1;
    while d.undo().is_some() {
        at -= 1;
        assert_eq!(
            *d.song(),
            snapshots[at],
            "{name} seed {seed}: undo went to the wrong state"
        );
    }
    assert_eq!(at, 0, "{name} seed {seed}: fewer undo steps than states");
    assert_eq!(*d.song(), start, "{name} seed {seed}: undone to the start");
    while d.redo().is_some() {}
    assert_eq!(*d.song(), end, "{name} seed {seed}: redone to the end");
}

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(path),
    )
    .unwrap()
}

fn soak(name: &str, seeds: std::ops::Range<u64>, steps: usize, make: impl Fn() -> SidDoc) {
    let mut tally = BTreeMap::new();
    for seed in seeds {
        soak_one(name, make(), seed, steps, &mut tally);
    }
    if std::env::var_os("CUBED_SOAK_STATS").is_some() {
        eprintln!("{name}: operations applied/refused: {tally:?}");
    }
    // Every kind of operation applied at least once somewhere (the soak is not vacuous).
    for (op, (ok, _)) in &tally {
        assert!(*ok > 0, "{name}: {op} never applied");
    }
}

#[test]
fn soak_a_new_song() {
    soak("new", 0..8, 160, SidDoc::new);
}

#[test]
fn soak_the_goattracker_fixtures() {
    for file in [
        "goattracker/gts5_basics.sng",
        "goattracker/gts5_loop_transpose.sng",
        "goattracker/gt1_slots_full.sng",
        "goattracker/gts5_padded_table.sng",
    ] {
        soak(file, 0..4, 120, || {
            SidDoc::from_bytes(&fixture(file)).unwrap()
        });
    }
}

#[test]
fn soak_the_asid_fixtures() {
    for file in ["sid/s3-chain.asid", "sid/s59-drum-example.asid"] {
        soak(file, 0..4, 120, || {
            SidDoc::from_bytes(&fixture(file)).unwrap()
        });
    }
}

#[test]
fn soak_a_corpus() {
    let Some(root) = std::env::var_os("CUBED_GT_SONGS") else {
        eprintln!("CUBED_GT_SONGS is not set: no corpus to soak");
        return;
    };
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("sng"))
            {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(Path::new(&root), &mut files);
    files.sort();
    let mut tally = BTreeMap::new();
    let mut songs = 0;
    for (i, path) in files.iter().enumerate() {
        let bytes = std::fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        let Ok(d) = SidDoc::from_sng(&bytes, cubed_sid::goattracker::hints_from_name(&name)) else {
            continue;
        };
        soak_one(&path.display().to_string(), d, i as u64 + 1, 40, &mut tally);
        songs += 1;
    }
    eprintln!("{songs} songs soaked");
}
