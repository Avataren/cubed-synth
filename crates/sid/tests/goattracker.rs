//! The GoatTracker import (`cubed_sid::goattracker`) against the another_synth
//! DAW's own (`src/audio/tracker/sid-doc/gt-sng-read.ts`), which it ports.
//!
//! Every expected value here is the DAW's output, not written by hand:
//! `fixtures/goattracker/oracle.ts` runs the DAW's `importGtSong` and
//! `serializeSidFile` over `.sng` files and records, per file, the verdict,
//! the notes and the `.asid` bytes the DAW saves. The Rust import must give
//! the same bytes, the same notes and the same refusals.
//!
//! - `fixtures/goattracker/*.sng` are synthetic songs (`make_fixtures.py`)
//!   built to reach every branch of the import and the GoatTracker 1
//!   conversion; `expected.json` is the oracle's output for them.
//! - Real songs are not in this repository. `CUBED_SNG_ORACLE=<oracle.json>`
//!   runs the oracle's output for any corpus (the DAW's 195 demo and fixture
//!   songs were checked this way, all identical) through the same comparison.

use std::path::Path;

use cubed_sid::goattracker::{hints_from_name, import, GtVariant};
use serde_json::Value;

/// Compares one file's import with the oracle's record; `None` when they agree.
fn mismatch(path: &Path, expected: &Value) -> Option<String> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let name = path.file_name().unwrap().to_string_lossy();
    let got = import(&bytes, hints_from_name(&name));
    let label = path.display();
    match (expected["ok"].as_bool().unwrap(), got) {
        (true, Ok(got)) => {
            let variant = match got.variant {
                GtVariant::Gts5 => "GTS5",
                GtVariant::Gt1 => "GTS!",
            };
            if variant != expected["variant"] {
                return Some(format!(
                    "{label}: variant {variant}, expected {}",
                    expected["variant"]
                ));
            }
            let notes: Vec<Value> = got
                .notes
                .iter()
                .map(|n| serde_json::json!([n.kind.as_str(), n.message]))
                .collect();
            if Value::Array(notes.clone()) != expected["notes"] {
                return Some(format!(
                    "{label}: notes differ\n  got      {}\n  expected {}",
                    Value::Array(notes),
                    expected["notes"]
                ));
            }
            let asid: String = got
                .song
                .to_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if asid != expected["asid"].as_str().unwrap() {
                let want = expected["asid"].as_str().unwrap();
                let at = asid
                    .as_bytes()
                    .iter()
                    .zip(want.as_bytes())
                    .position(|(a, b)| a != b)
                    .unwrap_or(asid.len().min(want.len()))
                    / 2;
                return Some(format!(
                    "{label}: .asid differs from byte {at} ({} bytes, expected {})",
                    asid.len() / 2,
                    want.len() / 2
                ));
            }
            None
        }
        (false, Err(e)) => (e.0 != expected["reason"].as_str().unwrap()).then(|| {
            format!(
                "{label}: refused with {:?}, expected {}",
                e.0, expected["reason"]
            )
        }),
        (true, Err(e)) => Some(format!("{label}: refused ({}), the DAW imports it", e.0)),
        (false, Ok(_)) => Some(format!(
            "{label}: imported, the DAW refuses it: {}",
            expected["reason"]
        )),
    }
}

fn check_oracle(oracle: &Path, base: &Path) -> usize {
    let text =
        std::fs::read_to_string(oracle).unwrap_or_else(|e| panic!("{}: {e}", oracle.display()));
    let records: serde_json::Map<String, Value> = serde_json::from_str(&text).unwrap();
    let problems: Vec<String> = records
        .iter()
        .filter_map(|(path, expected)| mismatch(&base.join(path), expected))
        .collect();
    assert!(
        problems.is_empty(),
        "{} of {} songs differ from the DAW:\n{}",
        problems.len(),
        records.len(),
        problems.join("\n")
    );
    records.len()
}

#[test]
fn synthetic_songs_import_as_the_daw_imports_them() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/goattracker");
    let checked = check_oracle(&dir.join("expected.json"), &dir);
    assert!(checked > 0);
}

#[test]
fn a_corpus_imports_as_the_daw_imports_it() {
    let Some(oracle) = std::env::var_os("CUBED_SNG_ORACLE") else {
        eprintln!("CUBED_SNG_ORACLE is not set: no corpus to compare (see the file's header)");
        return;
    };
    // The oracle records absolute paths (or paths relative to where it ran).
    let checked = check_oracle(Path::new(&oracle), Path::new(""));
    eprintln!("{checked} songs match the DAW");
}

#[test]
fn file_names_give_the_chip_and_the_speed() {
    use cubed_sid::goattracker::GtSongHints;
    use cubed_sid::SidModel::{Sid6581, Sid8580};
    let hint = |model, speed_multiplier| GtSongHints {
        model,
        speed_multiplier,
    };
    // The DAW's cases (sid-sng-import.test.ts).
    for (name, expected) in [
        (
            "defunkt_final_fv_po_ro_6581_ffff.sng",
            hint(Some(Sid6581), None),
        ),
        ("stinsen/song_8580.sng", hint(Some(Sid8580), None)),
        ("space_2x.sng", hint(None, Some(2))),
        ("mw%20title%20remix%2C%202x-speed.sng", hint(None, Some(2))),
        ("Tune 4X 6581.SNG", hint(Some(Sid6581), Some(4))),
        ("covert ops in 2d (funktempo).sng", hint(None, None)),
        ("tribal18.sng", hint(None, None)),
        ("x1x.sng", hint(None, None)),
        ("a_1x.sng", hint(None, None)),
        ("a_65810.sng", hint(None, None)),
        // And a few edges: two digits, a path with backslashes, a stray '%'.
        ("songs\\fast_16x.sng", hint(None, Some(16))),
        ("odd_17x.sng", hint(None, None)),
        ("100%_6581.sng", hint(Some(Sid6581), None)),
    ] {
        assert_eq!(hints_from_name(name), expected, "{name}");
    }
}

#[test]
fn imported_songs_play() {
    use cubed_sid::{SidSongPlayer, DEFAULT_SAMPLE_RATE};
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/goattracker");
    for name in [
        "gts5_basics.sng",
        "gt1_commands.sng",
        "gt1_arpeggio.sng",
        "gt1_filter.sng",
    ] {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        let song = import(&bytes, hints_from_name(name)).unwrap().song;
        let mut player = SidSongPlayer::new(song, DEFAULT_SAMPLE_RATE).unwrap();
        // Two seconds: every song here starts a note on its first row.
        let mut out = vec![0.0f32; DEFAULT_SAMPLE_RATE as usize * 2];
        player.render(&mut out);
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.01 && peak.is_finite(), "{name}: peak {peak}");
    }
}
