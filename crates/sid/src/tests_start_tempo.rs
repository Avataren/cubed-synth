//! sid_decisions.md §4 (plan-sid-authoring.md phase 2): the tempo a subsong
//! starts at. GoatTracker starts every channel at 6 frames per row per 1x,
//! `6 * multiplier - 1` stored (gplay.c:207-218), or at instrument 63's AD
//! byte when instrument 63 has no wave table and an AD of 2 or more
//! (gplay.c:220-221). The player started at the doc's tempo whatever the
//! multispeed, so a 2x subsong with no F command on its first row ran twice as
//! fast as in GT. The doc's tempo is the start tempo at 1x (always 6 in a doc
//! the app writes, D6); the player scales it by the multiplier.

use super::player::SidSongPlayer;
use super::song::*;
use super::*;

/// A song whose three channels play one blank 16-row pattern each, header tempo 6.
fn song(mult: u8, instruments: Vec<Instrument>, first: Row) -> SidSong {
    song_with(mult, instruments, first, Vec::new())
}

fn song_with(mult: u8, instruments: Vec<Instrument>, first: Row, wave: Vec<TableRow>) -> SidSong {
    let list = |pattern: u8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose: 0,
            repeat: 1,
        }],
        restart: 0,
    };
    let mut rows = vec![Row::default(); 16];
    rows[0] = first;
    let s = SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid8580,
        channels: 3,
        speed_multiplier: mult,
        tempo: 6,
        name: b"start".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![
            Pattern { rows },
            Pattern {
                rows: vec![Row::default(); 16],
            },
        ],
        instruments,
        tables: Tables {
            wave,
            ..Default::default()
        },
    };
    SidSong::parse(&s.to_bytes()).expect("parses")
}

fn one() -> Vec<Instrument> {
    vec![Instrument {
        name: b"i".to_vec(),
        sustain: 15,
        ..Default::default()
    }]
}

/// Frames per row of channel 1 after its first row, measured over rows 2-5.
fn frames_per_row(song: SidSong) -> usize {
    let mut p = SidSongPlayer::new(song, 44_100.0).expect("player");
    let mut out = vec![0.0f32; 4096];
    let mut starts = Vec::new();
    let mut row = p.position(0).1;
    for f in 0..2000 {
        let n = p.samples_in_next_frame();
        p.render(&mut out[..n]);
        let now = p.position(0).1;
        if now != row {
            starts.push(f);
            row = now;
        }
        if starts.len() == 6 {
            break;
        }
    }
    (starts[5] - starts[1]) / 4
}

#[test]
fn a_multispeed_subsong_without_a_tempo_command_starts_at_six_frames_per_row_per_1x() {
    assert_eq!(frames_per_row(song(1, one(), Row::default())), 6);
    assert_eq!(frames_per_row(song(2, one(), Row::default())), 12);
    assert_eq!(frames_per_row(song(4, one(), Row::default())), 24);
}

#[test]
fn a_tempo_command_on_the_first_row_still_sets_it() {
    let f = Row {
        note: 0,
        instrument: 0,
        command: 0xF,
        param: 9,
    };
    assert_eq!(frames_per_row(song(2, one(), f)), 9);
}

#[test]
fn instrument_63_with_no_wave_table_gives_its_ad_byte_as_the_start_tempo() {
    let mut ins = vec![
        Instrument {
            name: b"i".to_vec(),
            sustain: 15,
            ..Default::default()
        };
        63
    ];
    ins[62] = Instrument {
        name: b"t".to_vec(),
        attack: 0,
        decay: 8,
        ..Default::default()
    };
    assert_eq!(frames_per_row(song(1, ins.clone(), Row::default())), 8);
    // At 2x it is the same byte, not scaled (GT stores ad - 1 as it stands).
    assert_eq!(frames_per_row(song(2, ins.clone(), Row::default())), 8);
    // With a wave table instrument 63 is an instrument like any other.
    ins[62].wave_ptr = 1;
    let wave = vec![
        TableRow {
            left: 0x41,
            right: 0,
        },
        TableRow {
            left: 0xFF,
            right: 0,
        },
    ];
    assert_eq!(frames_per_row(song_with(1, ins, Row::default(), wave)), 6);
}
