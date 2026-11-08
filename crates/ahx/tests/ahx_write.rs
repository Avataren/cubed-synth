//! The `.ahx` / `.hvl` writer: `parse(write(parse(x))) == parse(x)` on every
//! fixture, with and without the file the song came from, and what the bytes
//! look like next to the original.
//!
//! `CUBED_AHX_SONGS=<dir>` adds every `.ahx` and `.hvl` below that directory.

use cubed_ahx::format::{self, Instrument, Song, SongFormat};
use cubed_ahx::write::{self, Origin};
use std::fs;
use std::path::{Path, PathBuf};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/songs")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = read.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(&p, out);
        } else if matches!(
            p.extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("ahx" | "hvl")
        ) {
            out.push(p);
        }
    }
}

/// The committed fixtures, plus any directory `CUBED_AHX_SONGS` names.
fn songs() -> Vec<(String, Vec<u8>)> {
    let mut paths = Vec::new();
    collect(&fixture_dir(), &mut paths);
    if let Some(extra) = std::env::var_os("CUBED_AHX_SONGS") {
        collect(Path::new(&extra), &mut paths);
    }
    assert!(
        paths.len() >= 100,
        "the fixtures are missing: {}",
        paths.len()
    );
    paths
        .into_iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read(&p).unwrap(),
            )
        })
        .collect()
}

#[test]
fn every_song_writes_back_to_the_same_song_with_its_file() {
    let mut identical = 0;
    let mut total = 0;
    for (name, bytes) in songs() {
        let song = format::parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let out = write::write(&song, Some(&bytes)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let back = format::parse(&out).unwrap_or_else(|e| panic!("{name} written: {e}"));
        assert_eq!(
            back, song,
            "{name}: the written file parses to another song"
        );
        total += 1;
        identical += (out == bytes) as usize;
        // Nothing but the names may have moved the body: the string table offset
        // is the size of everything before it.
        let origin = Origin::from_bytes(&bytes).unwrap();
        let offset = u16::from_be_bytes([out[4], out[5]]) as usize;
        assert_eq!(
            write::used_bytes(&song, &origin),
            offset,
            "{name}: used_bytes"
        );
    }
    eprintln!("{identical} of {total} songs are written back byte for byte");
    assert!(
        identical * 10 >= total * 8,
        "most songs must come back byte for byte"
    );
}

#[test]
fn every_song_writes_back_to_the_same_song_without_its_file() {
    for (name, bytes) in songs() {
        let song = format::parse(&bytes).unwrap();
        let out = write::write(&song, None).unwrap_or_else(|e| panic!("{name}: {e}"));
        let back = format::parse(&out).unwrap_or_else(|e| panic!("{name} written: {e}"));
        assert_eq!(
            back, song,
            "{name}: the written file parses to another song"
        );
        // Writing what was written changes nothing at all.
        let again = write::write(&back, None).unwrap();
        assert_eq!(again, out, "{name}: a second write differs");
    }
}

#[test]
fn every_instrument_survives_its_wire_form() {
    for (name, bytes) in songs() {
        let song = format::parse(&bytes).unwrap();
        for n in 1..song.instruments.len() {
            let ins = &song.instruments[n];
            let wire = write::write_instrument(ins, song.format, song.version)
                .unwrap_or_else(|e| panic!("{name} instrument {n}: {e}"));
            assert_eq!(wire.len(), write::instrument_bytes(ins, song.format));
            let back = format::parse_instrument(&wire, song.format, song.version, ins.name.clone())
                .unwrap();
            assert_eq!(&back, ins, "{name} instrument {n}");
        }
    }
}

fn tiny(format: SongFormat) -> Song {
    let mut song = format::parse(
        &fs::read(fixture_dir().join(match format {
            SongFormat::Ahx => "karma.ahx",
            SongFormat::Hvl => "doobrey_gubbins.hvl",
        }))
        .unwrap(),
    )
    .unwrap();
    // Keep it small: one position, a few tracks.
    song.positions.truncate(1);
    song.position_nr = 1;
    song.restart = 0;
    song.subsongs.clear();
    song.subsong_nr = 0;
    song
}

#[test]
fn what_the_format_cannot_hold_is_refused_and_named() {
    let ahx = tiny(SongFormat::Ahx);
    let hvl = tiny(SongFormat::Hvl);
    let refused = |song: &Song, what: &str| -> String {
        let e = write::write(song, None).expect_err(what).to_string();
        assert!(e.starts_with("cannot write this AHX/HVL song: "), "{e}");
        e
    };

    let mut s = ahx.clone();
    s.tracks[1][0].note = 64;
    assert!(refused(&s, "note 64").contains("track 1 row 0 note"));

    let mut s = ahx.clone();
    s.tracks[1][0].instrument = 64;
    assert!(refused(&s, "instrument 64").contains("instrument"));

    let mut s = ahx.clone();
    s.tracks[1][0].fxb = 1;
    assert!(refused(&s, "second column").contains("second effect column"));

    let mut s = hvl.clone();
    s.tracks[1][0] = format::Step {
        note: 63,
        instrument: 1,
        ..Default::default()
    };
    assert!(refused(&s, "note 63").contains("blank step"));

    let mut s = ahx.clone();
    s.instruments[1].name = "caf\u{e9}".into();
    write::write(&s, None).expect("latin-1 is fine");
    s.instruments[1].name = "\u{4e16}".into();
    assert!(refused(&s, "a wide name").contains("above U+00FF"));
    s.instruments[1].name = "a\0b".into();
    assert!(refused(&s, "a NUL").contains("NUL"));

    let mut s = ahx.clone();
    s.instruments[1].plist.entries.push(format::PListEntry {
        fx: [6, 0],
        ..Default::default()
    });
    assert!(refused(&s, "AHX has no PList command 6").contains("command 6"));
    let mut s = hvl.clone();
    s.instruments[1].plist.entries.push(format::PListEntry {
        fx: [6, 0],
        ..Default::default()
    });
    write::write(&s, None).expect("HVL has command 6");

    let mut s = ahx.clone();
    s.position_nr = 7;
    assert!(refused(&s, "position_nr").contains("position_nr is 7"));

    let mut s = ahx.clone();
    s.mixgain_raw = Some(1);
    assert!(refused(&s, "AHX has no mix gain").contains("HVL fields"));

    let mut s = hvl.clone();
    s.channels = 68;
    assert!(refused(&s, "68 channels").contains("channels"));

    let mut s = ahx.clone();
    s.instruments[1].wave_length = 8;
    assert!(refused(&s, "wave length").contains("wave_length"));
}

#[test]
fn a_song_past_the_string_table_offset_is_refused() {
    let mut s = tiny(SongFormat::Ahx);
    // 256 tracks of 64 rows are 49 KB and still fit.
    let track = s.tracks[1].clone();
    s.tracks = vec![track; 256];
    s.track_nr = 255;
    s.track_length = 64;
    let fits = write::write(&s, None);
    assert!(fits.is_ok(), "256 full tracks fit: {:?}", fits.err());
    // 1000 positions, and every instrument's PList filled up, do not.
    s.positions = vec![s.positions[0].clone(); 1000];
    s.position_nr = 1000;
    for ins in s.instruments.iter_mut().skip(1) {
        ins.plist.entries = vec![format::PListEntry::default(); 255];
    }
    let e = write::write(&s, None).expect_err("too big").to_string();
    assert!(e.contains("past the 16-bit limit"), "{e}");
}

#[test]
fn the_blank_first_track_flag_follows_the_file_and_never_drops_content() {
    // A version-0 file that stores an explicit blank track 0 keeps it.
    let (_, bytes) = songs().into_iter().next().unwrap();
    let song = format::parse(&bytes).unwrap();
    let mut origin = Origin::from_bytes(&bytes).unwrap();
    origin.blank_first_track = Some(true);
    let mut s = song.clone();
    s.tracks[0][0].note = 1;
    let e = write::write_with(&s, &origin).expect_err("content in a track the file drops");
    assert!(e.to_string().contains("blank-first-track"), "{e}");
    // Without a flag the guess is "track 0 is blank": content keeps it stored.
    let out = write::write_with(&s, &Origin::default()).unwrap();
    assert_eq!(format::parse(&out).unwrap(), s);
    assert_eq!(out[6] & 0x80, 0, "flag clear");
}

#[test]
fn an_hvl_song_converts_to_ahx_when_it_fits_and_says_why_when_not() {
    let mut converted = 0;
    let mut refused = 0;
    for (name, bytes) in songs() {
        let song = format::parse(&bytes).unwrap();
        if song.format != SongFormat::Hvl {
            assert!(write::convert_hvl_to_ahx(&song).is_err());
            continue;
        }
        match write::convert_hvl_to_ahx(&song) {
            Ok(ahx) => {
                converted += 1;
                let out = write::write(&ahx, None).unwrap_or_else(|e| panic!("{name}: {e}"));
                let back = format::parse(&out).unwrap();
                assert_eq!(back.format, SongFormat::Ahx);
                assert_eq!(back, ahx, "{name}");
                // The conversion keeps what plays: the same steps in the same positions.
                assert_eq!(back.positions.len(), song.positions.len());
                for (a, b) in back.positions.iter().zip(&song.positions) {
                    assert_eq!(a.track[..], b.track[..4]);
                    assert_eq!(a.transpose[..], b.transpose[..4]);
                }
            }
            Err(why) => {
                refused += 1;
                assert!(why.contains("Export it as HVL instead."), "{name}: {why}");
            }
        }
    }
    eprintln!("{converted} HVL songs convert to AHX, {refused} are refused");
}

/// The reasons another_synth's `convertHvlToAhx` gives for these songs
/// (it refuses every HVL fixture: they are wider than four tracks, or use the
/// second effect column), word for word.
#[test]
fn the_conversion_refuses_as_the_daws_does() {
    const INSTEAD: &str = "Export it as HVL instead.";
    for (name, tracks) in [
        ("a_little_cyberfunk.hvl", 8),
        ("afterstorm.hvl", 12),
        ("hexplosion.hvl", 16),
        ("sliding_away.hvl", 6),
        ("yoake_no_myoujou.hvl", 16),
    ] {
        let song = format::parse(&fs::read(fixture_dir().join(name)).unwrap()).unwrap();
        assert_eq!(
            write::convert_hvl_to_ahx(&song).unwrap_err(),
            format!("AHX files have 4 tracks; this song reaches track {tracks}. {INSTEAD}"),
            "{name}"
        );
    }
    let song =
        format::parse(&fs::read(fixture_dir().join("ring_modulation_test_song.hvl")).unwrap())
            .unwrap();
    assert_eq!(
        write::convert_hvl_to_ahx(&song).unwrap_err(),
        format!("This song uses a second effect column, which AHX files don't have. {INSTEAD}")
    );
}

#[test]
fn an_ahx_instrument_for_version_zero_loses_the_filter_toggle_high_nibble() {
    let mut ins = Instrument::default();
    ins.plist.entries.push(format::PListEntry {
        fx: [4, 4],
        fx_param: [0x35, 0x75],
        ..Default::default()
    });
    let v1 = write::write_instrument(&ins, SongFormat::Ahx, 1).unwrap();
    let v0 = write::write_instrument(&ins, SongFormat::Ahx, 0).unwrap();
    assert_ne!(v0, v1);
    let back = format::parse_instrument(&v0, SongFormat::Ahx, 0, String::new()).unwrap();
    assert_eq!(back.plist.entries[0].fx_param, [0x05, 0x05]);
    let back = format::parse_instrument(&v1, SongFormat::Ahx, 1, String::new()).unwrap();
    assert_eq!(back.plist.entries[0].fx_param, [0x35, 0x75]);
}
