//! S5.18 tests: GoatTracker's per-channel tempo (gplay.c:319-333, 483-508),
//! found on "Covert Ops in 2D" (Cadaver), whose rows ran at a flat 6 frames
//! here against GT's alternating 10 and 6.
//!
//! GT keeps a `tick` counter and a `tempo` per channel. Command E (funktempo,
//! from GT1's `7 00`) loads two row lengths from a speed-table row and puts
//! every channel on them, alternating; command F sets the row length minus 1
//! from 3 up, on all channels, or on the calling channel alone with bit 7.
//! The player took F as one global tempo (bit 7 masked off) and ignored E.
//!
//! Against GT's own playroutine on the corpus's 84 songs (control bytes of
//! all three voices, first 4000 frames): 57 matched exactly before, 81 after;
//! mismatching frames 121 587 -> 790. The funktempo song: 0 of 5995 frames
//! on every voice, the per-channel `F 84` stretch included.

use super::player::SidSongPlayer;
use super::song::*;
use super::*;

/// One PAL frame at 44.1 kHz is 879.8 samples (`player::frame_cycles`, GT
/// parity 0925b): a buffer that holds one; `samples_in_next_frame` says how
/// much of it a frame is.
const SPF: usize = 880;

fn t(l: u8, r: u8) -> TableRow {
    TableRow { left: l, right: r }
}

fn cmd_row(command: u8, param: u8) -> Row {
    Row {
        note: 0,
        instrument: 0,
        command,
        param,
    }
}

/// A song whose three channels play the given single patterns (one entry
/// each, restart 0), header tempo 6, with `speed` as its speed table.
fn song(patterns: [Vec<Row>; 3], speed: Vec<TableRow>) -> SidSong {
    let list = |pattern: u8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose: 0,
            repeat: 1,
        }],
        restart: 0,
    };
    let s = SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid8580,
        channels: 3,
        speed_multiplier: 1,
        tempo: 6,
        name: b"s518".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(2)],
        }],
        patterns: patterns.into_iter().map(|rows| Pattern { rows }).collect(),
        instruments: vec![Instrument {
            name: b"t".to_vec(),
            sustain: 15,
            wave_ptr: 1,
            ..Default::default()
        }],
        tables: Tables {
            wave: vec![t(0x41, 0x00), t(0xFF, 0x00)],
            speed,
            ..Default::default()
        },
    };
    SidSong::parse(&s.to_bytes()).expect("parses")
}

/// The frames after which channel `c`'s row index moved on, over `frames`.
fn row_ends(p: &mut SidSongPlayer, c: usize, frames: usize) -> Vec<usize> {
    let mut out = vec![0.0f32; SPF];
    let mut ends = Vec::new();
    let mut row = p.position(c).1;
    for f in 0..frames {
        let n = p.samples_in_next_frame();
        p.render(&mut out[..n]);
        let now = p.position(c).1;
        if now != row {
            ends.push(f);
            row = now;
        }
    }
    ends
}

fn blank(n: usize) -> Vec<Row> {
    vec![Row::default(); n]
}

#[test]
fn funktempo_alternates_the_two_row_lengths_of_its_speed_row() {
    // Speed row 1 = (10, 6); command E 01 on row 0 starts it (GT's funktable
    // holds the lengths minus 1, gplay.c:486-487), and E's own row gets the
    // first length.
    let mut a = blank(8);
    a[0] = cmd_row(0xE, 1);
    let mut p = SidSongPlayer::new(
        song([a, blank(8), blank(8)], vec![t(10, 6)]),
        DEFAULT_SAMPLE_RATE,
    )
    .unwrap();
    let ends = row_ends(&mut p, 0, 60);
    assert_eq!(
        ends,
        vec![9, 15, 25, 31, 41, 47, 57],
        "rows of 10 and 6 frames alternate"
    );
    // Every channel switched, not just the one that ran E.
    let mut p = SidSongPlayer::new(
        song(
            [
                {
                    let mut a = blank(8);
                    a[0] = cmd_row(0xE, 1);
                    a
                },
                blank(8),
                blank(8),
            ],
            vec![t(10, 6)],
        ),
        DEFAULT_SAMPLE_RATE,
    )
    .unwrap();
    assert_eq!(row_ends(&mut p, 2, 32), vec![9, 15, 25, 31]);
}

#[test]
fn a_tempo_command_without_bit_7_sets_every_channel_and_with_it_only_its_own() {
    // F 05 (row length 5) on channel 1's row 0: all three run 5-frame rows.
    let mut a = blank(8);
    a[0] = cmd_row(0xF, 0x05);
    let mut p =
        SidSongPlayer::new(song([a, blank(8), blank(8)], vec![]), DEFAULT_SAMPLE_RATE).unwrap();
    assert_eq!(row_ends(&mut p, 0, 20), vec![4, 9, 14, 19]);
    let mut a = blank(8);
    a[0] = cmd_row(0xF, 0x05);
    let mut p =
        SidSongPlayer::new(song([a, blank(8), blank(8)], vec![]), DEFAULT_SAMPLE_RATE).unwrap();
    assert_eq!(
        row_ends(&mut p, 2, 20),
        vec![4, 9, 14, 19],
        "channel 3 follows"
    );

    // F 84 (bit 7, length 4) on channel 1 alone: it runs 4-frame rows while
    // channel 2 keeps the song's 6.
    let mut a = blank(8);
    a[0] = cmd_row(0xF, 0x84);
    let mut p =
        SidSongPlayer::new(song([a, blank(8), blank(8)], vec![]), DEFAULT_SAMPLE_RATE).unwrap();
    assert_eq!(row_ends(&mut p, 0, 16), vec![3, 7, 11, 15]);
    let mut a = blank(8);
    a[0] = cmd_row(0xF, 0x84);
    let mut p =
        SidSongPlayer::new(song([a, blank(8), blank(8)], vec![]), DEFAULT_SAMPLE_RATE).unwrap();
    assert_eq!(
        row_ends(&mut p, 1, 18),
        vec![5, 11, 17],
        "channel 2 keeps 6"
    );
}

#[test]
fn tempo_values_below_3_are_gts_own_not_a_literal_row_length() {
    // GT stores the length minus 1 from 3 up and takes 0-2 as they are
    // (gplay.c:496-498): F 02 is a 3-frame row, F 03 too (stored 2, both
    // reload with 2). The corpus's "alien funk" is 151 rows of F 01/F 02.
    let mut a = blank(8);
    a[0] = cmd_row(0xF, 0x02);
    let mut p =
        SidSongPlayer::new(song([a, blank(8), blank(8)], vec![]), DEFAULT_SAMPLE_RATE).unwrap();
    assert_eq!(row_ends(&mut p, 0, 9), vec![2, 5, 8]);
    let mut a = blank(8);
    a[0] = cmd_row(0xF, 0x03);
    let mut p =
        SidSongPlayer::new(song([a, blank(8), blank(8)], vec![]), DEFAULT_SAMPLE_RATE).unwrap();
    assert_eq!(row_ends(&mut p, 0, 9), vec![2, 5, 8]);
}

#[test]
fn the_song_row_counts_the_longest_channel_at_its_own_tempo() {
    // Channel 2 has the longest pattern (8 rows against 4) and keeps the
    // song's 6-frame rows; channel 1 runs 4-frame rows (F 84). The song row
    // follows channel 2, so 12 frames are 2 rows, not the 3 channel 1 played.
    let mut a = blank(4);
    a[0] = cmd_row(0xF, 0x84);
    let mut p =
        SidSongPlayer::new(song([a, blank(8), blank(4)], vec![]), DEFAULT_SAMPLE_RATE).unwrap();
    assert_eq!(p.song_rows(), 8);
    let mut out = vec![0.0f32; SPF];
    for _ in 0..12 {
        let n = p.samples_in_next_frame();
        p.render(&mut out[..n]);
    }
    assert_eq!(p.song_row(), 2);
    assert_eq!(p.position(0).1, 3, "channel 1 is three rows in");
}
