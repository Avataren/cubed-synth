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
use cubed_sid::psid::import::export::{export_sid, SID_EXPORT_DEFAULTS};
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
    // Packing it again gives the file's bytes: nothing to measure.
    assert_eq!(r.report.exact, Some(true));
    assert!(r.report.fidelity.is_none());
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
fn an_unpacked_song_packs_to_the_file_it_came_from() {
    // The fixtures are what the DAW's exporter writes; unpacking and exporting again gives the same file.
    for (name, bytes) in [("gt_drum.sid", GT_DRUM), ("gt_chain.sid", GT_CHAIN)] {
        let r = import(bytes, &ImportOptions::default()).unwrap();
        assert!(r.report.used_gt_unpack, "{name}");
        assert_eq!(r.report.exact, Some(true), "{name}");
        let again = export_sid(&r.song, &SID_EXPORT_DEFAULTS).unwrap();
        assert_eq!(again, bytes, "{name}");
    }
}

#[test]
fn a_transcription_exports_as_a_playable_sid() {
    let r = import(TUNE, &ImportOptions::default()).unwrap();
    let bytes = export_sid(&r.song, &SID_EXPORT_DEFAULTS).unwrap();
    let file = cubed_sid::psid::PsidFile::parse(&bytes).unwrap();
    assert_eq!((file.songs, file.start_song), (1, 1));
    let mut player =
        cubed_sid::psid::PsidPlayer::new(&bytes, 44_100.0, Default::default()).unwrap();
    let mut out = vec![0f32; 44_100];
    player.render(&mut out);
    assert!(out.iter().any(|&s| s.abs() > 0.01), "silent");
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
fn an_import_can_be_cancelled_from_another_thread() {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let opts = ImportOptions {
        cancel: Some(cancel),
        ..Default::default()
    };
    assert_eq!(import(TUNE, &opts).unwrap_err(), "the import was cancelled");
    assert_eq!(import(IRQ, &opts).unwrap_err(), "the import was cancelled");
}

/// What an import made, enough to tell two of them apart.
fn digest(r: &cubed_sid::psid::import::ImportResult) -> (Vec<u8>, Vec<String>, Option<f64>) {
    (
        r.song.to_bytes(),
        r.report.notes.clone(),
        r.report.fidelity.as_ref().map(|f| f.score),
    )
}

#[test]
fn an_import_reports_its_progress_and_is_the_same_import_for_it() {
    use cubed_sid::psid::import::capture::PROGRESS_DONE;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;
    // The result crosses threads (a host runs the import on one).
    fn send<T: Send>(_: &T) {}
    let mut imported = 0;
    for (name, tune) in FIXTURES {
        // (Some fixtures are tunes that cannot be imported: they are for the refusals.)
        let Ok(plain) = import(tune, &ImportOptions::default()) else {
            continue;
        };
        imported += 1;
        send(&plain);
        let progress = Arc::new(AtomicU32::new(0));
        let opts = ImportOptions {
            progress: Some(progress.clone()),
            ..Default::default()
        };
        // Watched from another thread while the import runs: it only ever goes up.
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (progress, stop) = (progress.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut seen = vec![0u32];
                while !stop.load(Ordering::Relaxed) {
                    let now = progress.load(Ordering::Relaxed);
                    if now != *seen.last().unwrap() {
                        seen.push(now);
                    }
                    std::thread::yield_now();
                }
                seen.push(progress.load(Ordering::Relaxed));
                seen
            })
        };
        let watched = import(tune, &opts).unwrap();
        stop.store(true, Ordering::Relaxed);
        let seen = watcher.join().unwrap();
        assert_eq!(digest(&watched), digest(&plain), "{name}: the same song");
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{name}: {seen:?}");
        assert_eq!(progress.load(Ordering::Relaxed), PROGRESS_DONE, "{name}");
        assert_eq!(*seen.last().unwrap(), PROGRESS_DONE, "{name}: {seen:?}");
        // Without the measure the progress still ends at the whole.
        let progress = Arc::new(AtomicU32::new(0));
        import(
            tune,
            &ImportOptions {
                progress: Some(progress.clone()),
                measure: false,
                gt_unpack: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(progress.load(Ordering::Relaxed), PROGRESS_DONE, "{name}");
    }
    assert!(imported >= 4, "{imported} fixtures imported");
    // A cancelled import does not reach the whole, a refused one neither.
    let progress = Arc::new(AtomicU32::new(0));
    let cancel = Arc::new(AtomicBool::new(true));
    let opts = ImportOptions {
        progress: Some(progress.clone()),
        cancel: Some(cancel),
        ..Default::default()
    };
    assert!(import(TUNE, &opts).is_err());
    assert!(progress.load(Ordering::Relaxed) < PROGRESS_DONE);
}

#[test]
fn damaged_tunes_are_refused_or_imported_never_a_panic() {
    // A deterministic scramble of the tunes' bytes: whatever runs, the import ends with a song or a reason.
    let mut x: u32 = 0x1234_5678;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    for (name, base) in FIXTURES {
        for round in 0..24 {
            let mut bytes = base.to_vec();
            for _ in 0..1 + round % 5 {
                let at = 0x7c + 2 + next() as usize % (bytes.len() - 0x7c - 2);
                bytes[at] = next() as u8;
            }
            if round % 6 == 5 {
                bytes.truncate(0x7c + 2 + next() as usize % (bytes.len() - 0x7c - 2));
            }
            let opts = ImportOptions {
                max_seconds: Some(4.0),
                ..Default::default()
            };
            if let Ok(r) = import(&bytes, &opts) {
                r.song
                    .validate()
                    .unwrap_or_else(|e| panic!("{name} round {round}: {e}"));
            }
        }
    }
}

#[test]
fn the_song_is_the_one_the_daws_importer_makes() {
    let expected: Value = serde_json::from_str(EXPECTED).unwrap();
    for (name, bytes) in FIXTURES {
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
            // (The DAW's text for a BASIC program says "importer" where this crate's, like the player's, says "player".)
            (Err(e), false)
                if e == want["reason"]
                    .as_str()
                    .unwrap_or("")
                    .replace("this importer", "this player") =>
            {
                same += 1
            }
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
