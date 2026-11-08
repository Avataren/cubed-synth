//! The `.sng` writer (`cubed_sid::goattracker::export`) against the another_synth DAW's own
//! (`src/audio/tracker/sid-doc/gt-sng-write.ts`), which it ports, and against the importer:
//! `import(export(import(x)))` is `import(x)`.
//!
//! - `fixtures/goattracker/expected_sng.json` is the DAW's output (`oracle_write.ts`) for the
//!   synthetic fixtures and the two `.asid` songs: the bytes it writes, or its refusal. The
//!   Rust writer must give the same bytes and the same refusals; the bytes are also pinned by
//!   a hash here, so a change to either writer shows up.
//! - Real songs are not in this repository. `CUBED_SNG_WRITE_ORACLE=<json>` compares the DAW's
//!   output for any corpus (the DAW's 194 demo and fixture songs were checked this way, all
//!   byte-identical); `CUBED_GT_SONGS=<dir>` checks the round trip on every `.sng` below a
//!   folder, with no oracle.

use std::path::Path;

use cubed_sid::goattracker::{
    export, export_problem, hints_from_name, import, orderlist_bytes, GtSongHints,
};
use cubed_sid::song::{OrderEntry, Orderlist, SidSong, TableRow};
use cubed_sid::SidModel;
use serde_json::Value;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/goattracker")
}

/// The song a file holds, imported as the DAW imports it (or parsed, for an `.asid`).
fn load(path: &Path) -> Option<SidSong> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if path.extension().is_some_and(|e| e == "asid") {
        return Some(SidSong::parse(&bytes).expect("the fixture parses"));
    }
    let name = path.file_name().unwrap().to_string_lossy();
    import(&bytes, hints_from_name(&name)).ok().map(|i| i.song)
}

/// One file's record in the oracle, checked; `None` when the Rust side agrees.
fn mismatch(path: &Path, expected: &Value) -> Option<String> {
    let song = load(path)?;
    let label = path.display();
    match (expected["ok"].as_bool().unwrap(), export(&song)) {
        (true, Ok(got)) => {
            let want = expected["sng"].as_str().unwrap();
            if hex(&got.bytes) != want {
                let at = hex(&got.bytes)
                    .bytes()
                    .zip(want.bytes())
                    .position(|(a, b)| a != b)
                    .unwrap_or(want.len().min(got.bytes.len() * 2))
                    / 2;
                return Some(format!(
                    "{label}: .sng differs from byte {at} ({} bytes, expected {})",
                    got.bytes.len(),
                    want.len() / 2
                ));
            }
            // The DAW's notes, plus the one this writer adds for a text with no terminator.
            let notes: Vec<&str> = got
                .notes
                .iter()
                .map(String::as_str)
                .filter(|n| !n.contains("no terminator"))
                .collect();
            let want_notes: Vec<&str> = expected["notes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap())
                .collect();
            (notes != want_notes)
                .then(|| format!("{label}: notes {notes:?}, expected {want_notes:?}"))
        }
        (false, Err(e)) => (e.0 != expected["reason"].as_str().unwrap()).then(|| {
            format!(
                "{label}: refused with {:?}, expected {}",
                e.0, expected["reason"]
            )
        }),
        (true, Err(e)) => Some(format!("{label}: refused ({}), the DAW writes it", e.0)),
        (false, Ok(_)) => Some(format!(
            "{label}: written, the DAW refuses it: {}",
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
        "{} of {} songs differ from the DAW's writer:\n{}",
        problems.len(),
        records.len(),
        problems.join("\n")
    );
    records.len()
}

#[test]
fn synthetic_songs_are_written_as_the_daw_writes_them() {
    let dir = fixtures();
    let checked = check_oracle(&dir.join("expected_sng.json"), &dir);
    assert!(checked >= 10, "only {checked} songs checked");
}

#[test]
fn a_corpus_is_written_as_the_daw_writes_it() {
    let Some(oracle) = std::env::var_os("CUBED_SNG_WRITE_ORACLE") else {
        eprintln!(
            "CUBED_SNG_WRITE_ORACLE is not set: no corpus to compare (see the file's header)"
        );
        return;
    };
    let checked = check_oracle(Path::new(&oracle), Path::new(""));
    eprintln!("{checked} songs are written as the DAW writes them");
}

/// `import(export(song))` with the hints a `.sng` cannot carry.
fn round_trip(song: &SidSong) -> SidSong {
    let sng = export(song).unwrap_or_else(|e| panic!("cannot be written: {e}"));
    let hints = GtSongHints {
        model: Some(song.model),
        speed_multiplier: Some(song.speed_multiplier),
    };
    import(&sng.bytes, hints)
        .unwrap_or_else(|e| panic!("the written song does not import: {e}"))
        .song
}

#[test]
fn the_fixtures_survive_write_and_read() {
    let dir = fixtures();
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sng"))
        .collect();
    names.sort();
    let mut checked = 0;
    for path in names {
        let Some(song) = load(&path) else { continue };
        if export(&song).is_err() {
            continue;
        }
        assert_eq!(
            round_trip(&song),
            song,
            "{}: load(write(load(x))) differs from load(x)",
            path.display()
        );
        checked += 1;
    }
    assert!(checked >= 9, "only {checked} fixtures round-tripped");
    for name in ["s3-chain.asid", "s59-drum-example.asid"] {
        let song = load(&dir.join("..").join("sid").join(name)).unwrap();
        assert_eq!(round_trip(&song), song, "{name}");
    }
}

/// The bytes of the writer, pinned: the DAW's writer agrees on all of them
/// (`synthetic_songs_are_written_as_the_daw_writes_them`), so a hash that moves is a change.
#[test]
fn written_bytes_are_pinned() {
    let dir = fixtures();
    let mut got = Vec::new();
    for name in [
        "gts5_basics.sng",
        "gts5_loop_transpose.sng",
        "gts5_padded_table.sng",
        "gt1_commands.sng",
        "gt1_slots_full.sng",
        "x_6581_3x.sng",
    ] {
        let song = load(&dir.join(name)).unwrap();
        let sng = export(&song).unwrap();
        got.push((name, sng.bytes.len(), fnv(&sng.bytes)));
    }
    let expected: &[(&str, usize, u64)] = PINNED;
    assert_eq!(
        got.as_slice(),
        expected,
        "write these down if the change is meant"
    );
}

const PINNED: &[(&str, usize, u64)] = &[
    ("gts5_basics.sng", 266, 14929756535179153694),
    ("gts5_loop_transpose.sng", 142, 4188259624464904594),
    ("gts5_padded_table.sng", 173, 9884071128551815529),
    ("gt1_commands.sng", 297, 4623677425179434798),
    ("gt1_slots_full.sng", 2471, 7106725625060313947),
    ("x_6581_3x.sng", 128, 5689686125385308564),
];

#[test]
fn a_corpus_survives_write_and_read() {
    let Some(root) = std::env::var_os("CUBED_GT_SONGS") else {
        eprintln!("CUBED_GT_SONGS is not set: no corpus to round-trip");
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
    let (mut written, mut refused, mut skipped) = (0, 0, 0);
    for path in &files {
        let Some(song) = load(path) else {
            skipped += 1;
            continue;
        };
        match export(&song) {
            Ok(_) => {
                assert_eq!(round_trip(&song), song, "{}", path.display());
                written += 1;
            }
            Err(e) => {
                eprintln!("{}: not written: {e}", path.display());
                refused += 1;
            }
        }
    }
    eprintln!(
        "{written} songs round-trip, {refused} refused, {skipped} are no song ({} files)",
        files.len()
    );
    assert!(written > 0, "no song in {}", Path::new(&root).display());
}

fn blank_song() -> SidSong {
    import(
        &std::fs::read(fixtures().join("gts5_basics.sng")).unwrap(),
        GtSongHints::default(),
    )
    .unwrap()
    .song
}

#[test]
fn what_a_sng_cannot_hold_is_refused_with_the_reason() {
    let base = blank_song();
    assert!(export_problem(&base).is_none());

    let mut s = base.clone();
    s.tempo = 7;
    let why = export_problem(&s).unwrap();
    assert!(
        why.contains("no tempo field") && why.contains("tempo 7"),
        "{why}"
    );

    for (transpose, ok) in [
        (-16, true),
        (14, true),
        (-17, false),
        (15, false),
        (40, false),
    ] {
        let mut s = base.clone();
        s.subsongs[0].orderlists[1].entries[0].transpose = transpose;
        assert_eq!(export_problem(&s).is_none(), ok, "transpose {transpose}");
        if !ok {
            let why = export_problem(&s).unwrap();
            assert!(
                why.contains("subsong 0 channel 2 entry 0") && why.contains("-16..+14"),
                "{why}"
            );
        }
    }

    // 128 entries that each change the transpose need 256 bytes.
    let mut s = base.clone();
    s.subsongs[0].orderlists[0] = Orderlist {
        entries: (0..128)
            .map(|i| OrderEntry {
                pattern: 0,
                transpose: (i % 2) as i8 + 1,
                repeat: 1,
            })
            .collect(),
        restart: 0,
    };
    let why = export_problem(&s).unwrap();
    assert!(
        why.contains("needs 256 bytes") && why.contains("holds 254"),
        "{why}"
    );

    let mut s = base.clone();
    s.name = b"a\0b".to_vec();
    assert!(export_problem(&s)
        .unwrap()
        .contains("the song name contains a NUL"));
    let mut s = base.clone();
    s.instruments[0].name = b"x\0".to_vec();
    assert!(export_problem(&s)
        .unwrap()
        .contains("instrument 1's name contains a NUL"));

    // A song that breaks the model is refused before anything else.
    let mut s = base.clone();
    s.instruments[0].wave_ptr = 200;
    assert!(export_problem(&s)
        .unwrap()
        .contains("breaks a rule of the SID model"));
    let mut s = base;
    s.tables.wave = vec![TableRow::default(); 256];
    assert!(export_problem(&s).unwrap().contains("breaks a rule"));
}

#[test]
fn the_chip_and_the_speed_are_reported_not_stored() {
    let mut s = blank_song();
    s.model = SidModel::Sid8580;
    s.speed_multiplier = 4;
    let sng = export(&s).unwrap();
    assert_eq!(sng.notes.len(), 2);
    assert!(sng.notes[0].contains("8580") && sng.notes[0].contains("-E"));
    assert!(sng.notes[1].contains("4x") && sng.notes[1].contains("-S4"));
    // The file says neither: hints bring them back.
    let back = round_trip(&s);
    assert_eq!(back, s);
    let blind = import(&sng.bytes, GtSongHints::default()).unwrap().song;
    assert_eq!(blind.model, SidModel::Sid6581);
    assert_eq!(blind.speed_multiplier, 1);
}

#[test]
fn a_loop_keeps_the_transpose_of_its_own_restart_entry() {
    // GoatTracker keeps the running transpose when an orderlist loops: an entry the loop
    // returns to must restate its transpose if the end of the list left another one running.
    let entry = |pattern, transpose, repeat| OrderEntry {
        pattern,
        transpose,
        repeat,
    };
    let mut s = blank_song();
    s.patterns.push(s.patterns[0].clone());
    s.patterns.push(s.patterns[0].clone());
    let list = Orderlist {
        entries: vec![entry(0, 0, 1), entry(1, 0, 3), entry(2, 3, 1)],
        restart: 0,
    };
    // 0: pattern 0 at +0 needs a restatement ($F0) at the restart; 1: repeat $D2; 2: $F3.
    assert_eq!(
        orderlist_bytes(&list),
        vec![0xF0, 0, 0xD2, 1, 0xF3, 2, 0xFF, 0]
    );
    s.subsongs[0].orderlists[0] = list;
    assert_eq!(
        round_trip(&s).subsongs[0].orderlists[0],
        s.subsongs[0].orderlists[0]
    );

    // A restart in the middle, at an entry with its own repeat and transpose.
    let list = Orderlist {
        entries: vec![entry(0, 0, 1), entry(1, 5, 2), entry(2, 0, 1)],
        restart: 1,
    };
    assert_eq!(
        orderlist_bytes(&list),
        vec![0, 0xF5, 0xD1, 1, 0xF0, 2, 0xFF, 1]
    );
    s.subsongs[0].orderlists[1] = list;
    assert_eq!(
        round_trip(&s).subsongs[0].orderlists[1],
        s.subsongs[0].orderlists[1]
    );
}
