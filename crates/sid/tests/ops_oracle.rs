//! The document's table and instrument edits against the another_synth DAW's own
//! (`sid-table-rows.ts`, `sid-instrument-edit.ts`, `sid-doc/instrument-ops.ts`),
//! which they follow: pointer semantics on inserting and deleting table rows,
//! cloning and deleting instruments, a new instrument, removing unused rows.
//!
//! `fixtures/goattracker/oracle_ops.ts` applies each case of `ops_cases.json` with
//! the DAW's code to every song and records the `.asid` it saves (or its refusal);
//! the Rust edit must save the same bytes, and refuse where the DAW refuses.
//! `CUBED_OPS_ORACLE=<json>` runs a corpus the same way (the DAW's 194 demo and
//! fixture songs were checked: the differences are listed below).
//!
//! Two deliberate differences, which the test accounts for rather than hides:
//!
//! - The DAW remaps `FF xx` in the *speed* table as a jump. There `FF` is data (a
//!   fast slide, `$FFxx`), not a jump, so this crate leaves a speed table's rows
//!   alone and only moves the references to it.
//! - Deleting a table row: a reference to the deleted row names the row that took its
//!   place, in both; one past the table's end (a pattern command whose parameter is
//!   more than the table's rows, which GoatTracker reads as zeros) the DAW also clamps
//!   to the last row, which would give it a meaning it did not have: this crate keeps it
//!   past the end (shifted with the rest).

use std::path::Path;

use cubed_sid::edit::SidDoc;
use cubed_sid::goattracker::{hints_from_name, import};
use cubed_sid::song::{SidSong, TableKind};
use serde_json::{json, Value};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn load(path: &Path) -> Option<SidSong> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if path.extension().is_some_and(|e| e == "asid") {
        return SidSong::parse(&bytes).ok();
    }
    let name = path.file_name().unwrap().to_string_lossy();
    import(&bytes, hints_from_name(&name)).ok().map(|i| i.song)
}

/// "first", "mid", "last", "end" as a row of a table or an instrument of `len`.
fn pick(spec: &str, len: usize) -> usize {
    match spec {
        "first" => 0,
        "mid" => len / 2,
        "last" => len.saturating_sub(1),
        _ => len,
    }
}

/// The Rust operation of a case, its symbolic places resolved against `song`.
fn resolve(op: &Value, song: &SidSong) -> Value {
    let mut op = op.clone();
    if let Some(table) = op["table"].as_str().and_then(TableKind::from_name) {
        let len = song.tables.get(table).len();
        op["row"] = json!(pick(op["row"].as_str().unwrap(), len));
    }
    if let Some(spec) = op["instrument"].as_str() {
        op["instrument"] = json!(pick(spec, song.instruments.len()) + 1);
    }
    op
}

/// Whether `song` has a reference to a row past the end of `table`: a pattern command
/// that names one (8/9/A, 1-4, E), or a jump. GoatTracker reads zero rows there.
fn dangles(song: &SidSong, table: TableKind) -> bool {
    let len = song.tables.get(table).len();
    let commands: &[u8] = match table {
        TableKind::Wave => &[0x8],
        TableKind::Pulse => &[0x9],
        TableKind::Filter => &[0xA],
        TableKind::Speed => &[0x1, 0x2, 0x3, 0x4, 0xE],
    };
    song.patterns
        .iter()
        .flat_map(|p| &p.rows)
        .any(|r| commands.contains(&r.command) && r.param as usize > len)
}

/// Whether a difference from the DAW in `case` is one of the two deliberate ones.
fn explained(song: &SidSong, case: &str) -> bool {
    let speed_ff = song.tables.speed.iter().any(|r| r.left == 0xFF);
    let table = TableKind::ALL.into_iter().find(|t| case.contains(t.name()));
    match table {
        Some(TableKind::Speed) if speed_ff && case.starts_with("insert") => true,
        Some(t) if case.starts_with("delete") => {
            dangles(song, t) || (t == TableKind::Speed && speed_ff)
        }
        _ => false,
    }
}

fn check(oracle: &Path, base: &Path) -> (usize, usize, Vec<String>) {
    let text =
        std::fs::read_to_string(oracle).unwrap_or_else(|e| panic!("{}: {e}", oracle.display()));
    let records: serde_json::Map<String, Value> = serde_json::from_str(&text).unwrap();
    let cases: Vec<Value> = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/goattracker/ops_cases.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let (mut same, mut differ) = (0, 0);
    let mut problems = Vec::new();
    for (path, record) in &records {
        let path = base.join(path);
        let song = load(&path).unwrap_or_else(|| panic!("{} does not load", path.display()));
        for case in &cases {
            let name = case["name"].as_str().unwrap();
            let want = &record[name];
            let mut d = SidDoc::from_song(song.clone()).unwrap();
            let got = d.apply(&resolve(&case["rust"], &song));
            let label = format!("{} / {name}", path.display());
            match (want["ok"].as_bool().unwrap(), got) {
                (true, Ok(_)) => {
                    d.song()
                        .validate()
                        .unwrap_or_else(|e| panic!("{label}: invalid song: {e}"));
                    if hex(&d.to_asid_bytes()) == want["asid"].as_str().unwrap() {
                        same += 1;
                    } else if explained(&song, name) {
                        differ += 1;
                    } else {
                        problems.push(format!("{label}: the saved song differs from the DAW's"));
                    }
                }
                (false, Err(_)) => same += 1,
                // The DAW refuses an instrument delete that leaves rows naming it (we pass clear_uses),
                // a table-row insert into a full table with a different message, ...: only a mismatch of
                // verdict is a difference.
                (true, Err(e)) => problems.push(format!("{label}: refused ({e}), the DAW does it")),
                (false, Ok(_)) => problems.push(format!(
                    "{label}: done, the DAW refuses it: {}",
                    want["reason"]
                )),
            }
        }
    }
    (same, differ, problems)
}

#[test]
fn table_and_instrument_edits_save_what_the_daw_saves() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/goattracker");
    let (same, differ, problems) = check(&dir.join("expected_ops.json"), &dir);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(
        same > 200,
        "only {same} cases agree ({differ} differ for the reasons above)"
    );
}

#[test]
fn a_corpus_edits_as_the_daw_edits() {
    let Some(oracle) = std::env::var_os("CUBED_OPS_ORACLE") else {
        eprintln!("CUBED_OPS_ORACLE is not set: no corpus to compare (see the file's header)");
        return;
    };
    let (same, differ, problems) = check(Path::new(&oracle), Path::new(""));
    eprintln!(
        "{same} cases are byte-identical to the DAW's, {differ} differ for the deliberate reasons"
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
