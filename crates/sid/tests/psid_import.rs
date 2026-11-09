//! Importing `.sid` tunes as GoatTracker songs (`psid::import`), end to end: the synthetic tunes of
//! `tests/fixtures/psid/` (made by `make_fixtures.py`, and `export_gt_fixture.ts` for the two a
//! GoatTracker player plays: no real C64 tune is ever committed), the song the DAW's own importer
//! makes of each (`expected_import.json`, written by `import_oracle.ts`), and, with the environment
//! variables below, a smoke run over a corpus of real tunes and a comparison with the DAW on them.
//!
//!   CUBED_PSID=<dir>                a folder of `.sid` files, searched recursively
//!   CUBED_PSID_IMPORT_STRIDE=500    every n-th file only (default 500)
//!   CUBED_PSID_IMPORT_SECONDS=20    capture cap per subsong, seconds (default 20)
//!   CUBED_PSID_IMPORT_ORACLE=<json> what `import_oracle.ts` recorded for a list of tunes (with
//!                                   `import` or `start`, not `capture`); every tune in it is imported
//!                                   and compared with the DAW's song, byte for byte
//!
//! cargo test --release -p cubed-sid --test psid_import -- --nocapture

use cubed_sid::edit::SidDoc;
use cubed_sid::psid::import::{import, ImportOptions};
use cubed_sid::song::{NOTE_FIRST, NOTE_LAST};
use serde_json::Value;
use std::path::{Path, PathBuf};

const TUNE: &[u8] = include_bytes!("fixtures/psid/tune.sid");
const ARP: &[u8] = include_bytes!("fixtures/psid/arp.sid");
const CIA2X: &[u8] = include_bytes!("fixtures/psid/cia2x.sid");
const IRQ: &[u8] = include_bytes!("fixtures/psid/irq.sid");
const RASTER: &[u8] = include_bytes!("fixtures/psid/raster.sid");
const TWO: &[u8] = include_bytes!("fixtures/psid/two.sid");
const GT_DRUM: &[u8] = include_bytes!("fixtures/psid/gt_drum.sid");
const GT_CHAIN: &[u8] = include_bytes!("fixtures/psid/gt_chain.sid");
const EXPECTED: &str = include_str!("fixtures/psid/expected_import.json");

const FIXTURES: [(&str, &[u8]); 8] = [
    ("tune.sid", TUNE),
    ("arp.sid", ARP),
    ("cia2x.sid", CIA2X),
    ("irq.sid", IRQ),
    ("raster.sid", RASTER),
    ("two.sid", TWO),
    ("gt_drum.sid", GT_DRUM),
    ("gt_chain.sid", GT_CHAIN),
];

fn unbase64(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

fn notes_of(song: &cubed_sid::song::SidSong) -> Vec<u8> {
    let mut notes: Vec<u8> = song
        .patterns
        .iter()
        .flat_map(|p| p.rows.iter().map(|r| r.note))
        .filter(|n| (NOTE_FIRST..=NOTE_LAST).contains(n))
        .collect();
    notes.sort_unstable();
    notes.dedup();
    notes
}

// --- The synthetic tunes ------------------------------------------------------

#[test]
fn a_small_tune_becomes_a_song_with_its_notes() {
    let r = import(TUNE, &ImportOptions::default()).unwrap();
    let song = &r.song;
    song.validate().unwrap();
    assert!(!r.report.used_gt_unpack);
    assert_eq!(r.report.subsong, 0);
    assert_eq!(song.subsongs.len(), 1);
    assert_eq!(song.name, b"Tune (test)");
    // The lead plays C-4 D#4 F-4 G-4 A#4 C-5 (note indices 48 51 53 55 58 60) and the bass C-2 D#2 F-2 A#1 (24 27 29 22);
    // a row's note is the index plus one.
    let notes = notes_of(song);
    for index in [48u8, 51, 53, 55, 58, 60, 24, 27, 29, 22] {
        assert!(
            notes.contains(&(index + 1)),
            "note {index} is missing: {notes:?}"
        );
    }
    assert_eq!(notes.len(), 10, "and no others: {notes:?}");
    // Two voices play; the third rests.
    let first = &song.subsongs[0];
    assert_eq!(first.orderlists.len(), 3);
    let fid = r.report.fidelity.as_ref().expect("measured");
    assert!(fid.score > 0.95, "fidelity {}", fid.score);
    assert!(r.report.frames > 700, "{}", r.report.frames);
    // It is the same song every time.
    let again = import(TUNE, &ImportOptions::default()).unwrap();
    assert_eq!(again.song.to_bytes(), song.to_bytes());
}

#[test]
fn the_song_is_a_document_that_edits_and_writes_as_sng_and_asid() {
    let (doc, report) = SidDoc::from_psid(TUNE, &ImportOptions::default()).unwrap();
    assert!(report.fidelity.is_some());
    let asid = doc.to_asid_bytes();
    assert_eq!(SidDoc::from_bytes(&asid).unwrap().to_asid_bytes(), asid);
    let sng = doc.to_sng_bytes().unwrap();
    let back = SidDoc::from_bytes(&sng).unwrap();
    assert_eq!(back.song().patterns, doc.song().patterns);
    assert_eq!(back.song().instruments.len(), doc.song().instruments.len());
    let (problems, _warnings) = doc.validate();
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn options_pick_the_subsongs_and_skip_the_measure() {
    let opts = ImportOptions {
        subsongs: Some(vec![1]),
        measure: false,
        ..Default::default()
    };
    let r = import(ARP, &opts).unwrap();
    assert_eq!(r.report.subsong, 1);
    assert!(r.report.fidelity.is_none());
    assert_eq!(r.report.subsongs.len(), 1);
    assert_eq!(r.report.subsongs[0].gt_subsong, Some(0));
    // The default imports the start song first, then the others.
    let both = import(ARP, &ImportOptions::default()).unwrap();
    assert_eq!(both.song.subsongs.len(), 2);
    assert_eq!(
        both.report
            .subsongs
            .iter()
            .map(|s| s.subsong)
            .collect::<Vec<_>>(),
        [0, 1]
    );
}

#[test]
fn a_tune_the_gt_unpack_reads_is_unpacked_unless_told_not() {
    let r = import(GT_DRUM, &ImportOptions::default()).unwrap();
    assert!(r.report.used_gt_unpack);
    assert!(r.report.subsongs.is_empty());
    r.song.validate().unwrap();
    assert!(r.report.fidelity.unwrap().score >= 0.98);
    let t = import(
        GT_DRUM,
        &ImportOptions {
            gt_unpack: false,
            measure: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!t.report.used_gt_unpack);
    assert!(!t.report.subsongs.is_empty());
}

#[test]
fn what_cannot_be_imported_is_refused_with_the_reason() {
    assert_eq!(
        import(b"hello", &ImportOptions::default()).unwrap_err(),
        "it is not a SID file"
    );
    // A PSID whose header is cut off.
    assert!(import(&TUNE[..40], &ImportOptions::default()).is_err());
    // A tune that never writes a note: two subsongs, both refused.
    let e = import(CIA2X, &ImportOptions::default()).unwrap_err();
    assert!(e.contains("plays no notes"), "{e}");
    // A tune's code that is not code (jams at once): refused, not a panic.
    let mut jam = TUNE.to_vec();
    let at = 0x7c + 2;
    jam[at..at + 4].fill(0x02);
    jam[0x0a] = 0x10;
    jam[0x0b] = 0x00;
    let e = import(&jam, &ImportOptions::default()).unwrap_err();
    assert!(e.contains("init routine"), "{e}");
}

#[test]
fn the_song_is_the_one_the_daws_importer_makes() {
    let expected: Value = serde_json::from_str(EXPECTED).unwrap();
    for (name, bytes) in FIXTURES {
        if name == "gt_chain.sid" {
            // The DAW unpacks it (its song packs again to the file's bytes); this crate decides by how it plays.
            continue;
        }
        let want = &expected[name];
        let got = import(bytes, &ImportOptions::default());
        if want["ok"] == false {
            assert_eq!(got.unwrap_err(), want["reason"].as_str().unwrap(), "{name}");
            continue;
        }
        let r = got.unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            r.song.to_bytes(),
            unbase64(want["asid"].as_str().unwrap()),
            "{name}"
        );
        let notes: Vec<&str> = want["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(r.report.notes, notes, "{name}");
        assert_eq!(
            r.report.used_gt_unpack,
            want["method"] == "unpacked",
            "{name}"
        );
        if let (Some(f), Some(score)) = (&r.report.fidelity, want["score"].as_f64()) {
            assert!(
                (f.score - score).abs() < 0.005,
                "{name}: {} against the DAW's {score}",
                f.score
            );
        }
    }
}

// --- A corpus of real tunes -----------------------------------------------------

fn sid_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            sid_files(&p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sid")) {
            out.push(p);
        }
    }
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[test]
fn corpus_smoke() {
    let Some(root) = std::env::var_os("CUBED_PSID") else {
        eprintln!("CUBED_PSID is not set: no corpus to run");
        return;
    };
    let stride = env_f64("CUBED_PSID_IMPORT_STRIDE", 500.0).max(1.0) as usize;
    let seconds = env_f64("CUBED_PSID_IMPORT_SECONDS", 20.0);
    let mut files = Vec::new();
    sid_files(Path::new(&root), &mut files);
    let (mut ok, mut refused) = (0, 0);
    for path in files.iter().step_by(stride) {
        let bytes = std::fs::read(path).unwrap();
        let start = cubed_sid::psid::PsidFile::parse(&bytes).map(|f| f.start_subsong());
        let opts = ImportOptions {
            subsongs: start.ok().map(|s| vec![s]),
            max_seconds: Some(seconds),
            ..Default::default()
        };
        match import(&bytes, &opts) {
            Ok(r) => {
                r.song
                    .validate()
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                SidDoc::from_song(r.song).unwrap();
                ok += 1;
            }
            Err(_) => refused += 1,
        }
    }
    eprintln!("imported {ok}, refused {refused}");
    assert!(ok > 0);
}

#[test]
fn oracle() {
    let Some(json) = std::env::var_os("CUBED_PSID_IMPORT_ORACLE") else {
        eprintln!("CUBED_PSID_IMPORT_ORACLE is not set: nothing to compare with");
        return;
    };
    let oracle: Value = serde_json::from_str(&std::fs::read_to_string(json).unwrap()).unwrap();
    let seconds = std::env::var("CUBED_PSID_IMPORT_SECONDS")
        .ok()
        .and_then(|s| s.parse::<f64>().ok());
    let mut same = 0;
    let mut differ = Vec::new();
    for (path, want) in oracle.as_object().unwrap() {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        // The oracle ran `start` (only the start song) when its report holds one subsong; this run does the same.
        let only_start = want["reports"].as_array().is_some_and(|r| r.len() == 1)
            && want["method"] == "transcribed";
        let start = cubed_sid::psid::PsidFile::parse(&bytes).map(|f| f.start_subsong());
        let opts = ImportOptions {
            subsongs: if only_start {
                start.ok().map(|s| vec![s])
            } else {
                None
            },
            max_seconds: seconds,
            ..Default::default()
        };
        match (import(&bytes, &opts), want["ok"].as_bool().unwrap()) {
            (Ok(r), true) if r.song.to_bytes() == unbase64(want["asid"].as_str().unwrap()) => {
                same += 1
            }
            (Err(e), false) if e == want["reason"].as_str().unwrap_or("") => same += 1,
            _ => differ.push(path.clone()),
        }
    }
    eprintln!(
        "same as the DAW: {same}; different: {}: {differ:?}",
        differ.len()
    );
    assert!(
        differ.len() * 20 <= same + differ.len(),
        "{} of {} differ",
        differ.len(),
        same + differ.len()
    );
}
