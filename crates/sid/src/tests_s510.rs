//! S5.10 tests: GoatTracker's note arithmetic and vibrato, pinned against the
//! GT2 source (`/tmp/gt2-src/src/gplay.c`, read for its facts).
//!
//! - Notes: `cptr->note = (u8)(newnote - FIRSTNOTE)` with `newnote =
//!   (u8)(pattern note + trans)` (gplay.c:350, 921) WRAPS; the wave table's
//!   note column is `note < 0x80 ? note + cptr->note : note`, then `& 0x7f`
//!   (gplay.c:714-721), read from a 128-entry table whose entries past B-7
//!   (index 95) are 0 (gplay.c:9-35). GT never clamps.
//! - Vibrato: `vibtime` is a u8; per effect frame `if (vibtime < 0x80 &&
//!   vibtime > cmp) vibtime ^= 0xff; vibtime += 2;` then odd = down, even = up
//!   by `speed` (gplay.c:615-640, 767-800). Tick-N effects skip tick 0
//!   (goattrk2.c:55 `optimizerealtime = 1`, gplay.c:728) and the frame a wave
//!   row sets a note (gplay.c:722 `goto PULSEEXEC`). Instrument vibrato is
//!   command 0's fall-through: `vibdelay` 0 never vibrates, above 1 counts
//!   down (gplay.c:767-772). Commands 5-F leave the running command alone
//!   (gplay.c:397-505 set `command` only for 0-4), a new note resets it to 0
//!   (gplay.c:351).
//!
//! The expected vibrato sequences come from an independent Python
//! transcription of those gplay.c lines (`.ai/s510-vibrato-oracle.py`), not
//! from a run of this player.

use super::player::{note_index, SidSongPlayer};
use super::song::*;
use super::*;

/// One PAL frame at 44.1 kHz is 879.8 samples (`player::frame_cycles`, GT
/// parity 0925b): a buffer that holds one; `samples_in_next_frame` says how
/// much of it a frame is.
const SPF: usize = 880;

fn t(l: u8, r: u8) -> TableRow {
    TableRow { left: l, right: r }
}

fn row(note: u8, instrument: u8, command: u8, param: u8) -> Row {
    Row {
        note,
        instrument,
        command,
        param,
    }
}

/// A sustaining triangle; `first_wave` 0x09 so the wave table's first row
/// sets the note on frame 1, as GT's does (the new-note frame runs nothing
/// else, gplay.c:510-514).
fn ins() -> Instrument {
    Instrument {
        name: b"t".to_vec(),
        sustain: 15,
        first_wave: 0x09,
        wave_ptr: 1,
        ..Default::default()
    }
}

/// Channel 1 plays `rows` once under `transpose`; channels 2-3 are silent.
fn song(
    rows: Vec<Row>,
    transpose: i8,
    tempo: u8,
    instruments: Vec<Instrument>,
    tables: Tables,
) -> SidSong {
    let n = rows.len();
    let list = |pattern: u8, transpose: i8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose,
            repeat: 1,
        }],
        restart: 0,
    };
    let s = SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid8580,
        channels: 3,
        speed_multiplier: 1,
        tempo,
        name: b"s510".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0, transpose), list(1, 0), list(1, 0)],
        }],
        patterns: vec![
            Pattern { rows },
            Pattern {
                rows: vec![Row::default(); n],
            },
        ],
        instruments,
        tables,
    };
    SidSong::parse(&s.to_bytes()).expect("parses")
}

/// The wave table every test instrument starts: the triggered note, then stop.
fn note_wave() -> Vec<TableRow> {
    vec![t(0x41, 0x00), t(0xFF, 0x00)]
}

/// Voice 1's frequency register after each of `frames` frames.
fn freqs(s: &SidSong, frames: usize) -> Vec<i32> {
    let mut p = SidSongPlayer::new(s.clone(), DEFAULT_SAMPLE_RATE).expect("player builds");
    let mut out = vec![0.0f32; SPF];
    (0..frames)
        .map(|_| {
            let n = p.samples_in_next_frame();
            p.render(&mut out[..n]);
            p.chip().voice(0).frequency() as i32
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The note table and note arithmetic
// ---------------------------------------------------------------------------

#[test]
fn the_note_table_has_gts_128_entries_96_notes_then_zero() {
    // GT's freqtbllo/hi hold 128 entries: C-0..B-7 (0..95), B-7 at $FFFF,
    // then 32 zero entries (gplay.c:9-35). Our entries are derived (equal
    // temperament at the PAL clock, plan §3 rule 3: no GT table is copied);
    // what is GT's is the SHAPE: 96 notes, the $FFFF clamp, zeros after, and
    // the 7-bit index.
    assert_eq!(
        gt_note_freq_reg(95),
        0xFFFF,
        "B-7 clamps to $FFFF, as GT's table does"
    );
    assert!(
        gt_note_freq_reg(93) > gt_note_freq_reg(92),
        "A-7 is a real note past the input range"
    );
    assert!(gt_note_freq_reg(94) > gt_note_freq_reg(93));
    for i in 96..=127u8 {
        assert_eq!(
            gt_note_freq_reg(i),
            0,
            "index {i} is past GT's notes: register 0"
        );
    }
    // The index is 7 bits (`note &= 0x7f`, gplay.c:720).
    assert_eq!(gt_note_freq_reg(128 + 57), gt_note_freq_reg(57));
    assert_eq!(gt_note_freq_reg(255), 0);
}

#[test]
fn note_index_wraps_as_gts_unsigned_char_does() {
    // Row note 93 (G#7, index 92) +5: GT's 0xBC + 5 = 0xC1, - 0x60 = 97.
    assert_eq!(note_index(93, 5), 97);
    // Row note 1 (C-0) -1: (u8)(0x5F - 0x60) = 0xFF.
    assert_eq!(note_index(1, -1), 255);
    assert_eq!(note_index(1, -16), 240);
    assert_eq!(note_index(58, 0), 57);
    assert_eq!(note_index(58, -12), 45);
}

#[test]
fn a_transpose_past_g_sharp_7_plays_gts_higher_note_not_a_clamped_one() {
    // G#7 +2 = index 94 (A#7), a real note in GT's table; the old clamp
    // played G#7, 2 semitones flat.
    let s = song(
        vec![row(93, 1, 0, 0), row(0, 0, 0, 0)],
        2,
        6,
        vec![ins()],
        Tables {
            wave: note_wave(),
            ..Default::default()
        },
    );
    let f = freqs(&s, 3);
    assert_eq!(f[1], gt_note_freq_reg(94) as i32);
    // +5 = index 97: past B-7, GT's table reads 0.
    let s = song(
        vec![row(93, 1, 0, 0), row(0, 0, 0, 0)],
        5,
        6,
        vec![ins()],
        Tables {
            wave: note_wave(),
            ..Default::default()
        },
    );
    assert_eq!(freqs(&s, 3)[1], 0);
}

#[test]
fn a_transpose_below_c0_wraps_as_gt_does() {
    // C-0 -1: cptr->note = 0xFF, & 0x7f = 127, table entry 0 (silence), not C-0.
    let s = song(
        vec![row(1, 1, 0, 0), row(0, 0, 0, 0)],
        -1,
        6,
        vec![ins()],
        Tables {
            wave: note_wave(),
            ..Default::default()
        },
    );
    assert_eq!(freqs(&s, 3)[1], 0);
    // D-0 -1 is a plain C#-0.
    let s = song(
        vec![row(3, 1, 0, 0), row(0, 0, 0, 0)],
        -1,
        6,
        vec![ins()],
        Tables {
            wave: note_wave(),
            ..Default::default()
        },
    );
    assert_eq!(freqs(&s, 3)[1], gt_note_freq_reg(1) as i32);
}

#[test]
fn the_wave_tables_note_column_is_gts_mod_128_arithmetic() {
    // Base index 50 (row note 51). Rows, one per frame from frame 1:
    //   $00 -> 50; $5F -> (50 + 95) & 127 = 17 (the old player clamped 145
    //   to 92); $7F -> (50 + 127) & 127 = 49; $85 -> absolute 5; $80 -> no
    //   change; then stop.
    let mut i = ins();
    i.wave_ptr = 1;
    let wave = vec![
        t(0x41, 0x00),
        t(0x41, 0x5F),
        t(0x41, 0x7F),
        t(0x41, 0x85),
        t(0x41, 0x80),
        t(0xFF, 0x00),
    ];
    let s = song(
        vec![row(51, 1, 0, 0), row(0, 0, 0, 0)],
        0,
        32,
        vec![i.clone()],
        Tables {
            wave,
            ..Default::default()
        },
    );
    let f = freqs(&s, 7);
    let r = |n: u8| gt_note_freq_reg(n) as i32;
    assert_eq!(&f[1..7], &[r(50), r(17), r(49), r(5), r(5), r(5)]);
    // Base 10 with $60 ("down 32" in the old reading): (10 + 96) & 127 = 106,
    // past B-7, so register 0 (the old player clamped to C-0).
    let wave = vec![t(0x41, 0x00), t(0x41, 0x60), t(0xFF, 0x00)];
    let s = song(
        vec![row(11, 1, 0, 0), row(0, 0, 0, 0)],
        0,
        32,
        vec![i],
        Tables {
            wave,
            ..Default::default()
        },
    );
    let f = freqs(&s, 3);
    assert_eq!(&f[1..3], &[r(10), 0]);
}

// ---------------------------------------------------------------------------
// Vibrato
// ---------------------------------------------------------------------------

/// Offsets from the note's register, frame 1 on (frame 0 is the new-note frame).
fn offsets(f: &[i32]) -> Vec<i32> {
    f[1..].iter().map(|v| v - f[1]).collect()
}

#[test]
fn command_4_vibrato_follows_gts_vibtime_rate_and_phase() {
    // 4 01, speed row (cmp 16, speed 5), tempo 40: every row restates 4 01.
    // GT: first up-swing cmp/2 + 1 = 9 frames, then half-swings of cmp + 2 =
    // 18 frames; frame 40 (tick 0) holds. The old player swung 16 per half
    // (8 first), so a cycle 4 frames short.
    let rows = vec![row(49, 1, 4, 1), row(0, 0, 4, 1), row(0, 0, 4, 1)];
    let s = song(
        rows,
        0,
        40,
        vec![ins()],
        Tables {
            wave: note_wave(),
            speed: vec![t(16, 5)],
            ..Default::default()
        },
    );
    let f = freqs(&s, 80);
    let gt: Vec<i32> = vec![
        0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 40, 35, 30, 25, 20, 15, 10, 5, 0, -5, -10, -15, -20,
        -25, -30, -35, -40, -45, -40, -35, -30, -25, -20, -15, -10, -5, 0, 5, 10, 10, 15, 20, 25,
        30, 35, 40, 45, 40, 35, 30, 25, 20, 15, 10, 5, 0, -5, -10, -15, -20, -25, -30, -35, -40,
        -45, -40, -35, -30, -25, -20, -15, -10, -5, 0, 5, 10, 15, 20, 25,
    ];
    assert_eq!(offsets(&f), gt);
}

#[test]
fn a_small_vibrato_depth_is_not_twice_too_fast() {
    // cmp 1 (a GT1 $1X vibrato): GT's half-swing is 3 frames; the old player
    // flipped every frame. Tempo 6: frames 6 and 12 are tick 0 and hold.
    let rows = vec![row(49, 1, 4, 1), row(0, 0, 4, 1), row(0, 0, 4, 1)];
    let s = song(
        rows,
        0,
        6,
        vec![ins()],
        Tables {
            wave: note_wave(),
            speed: vec![t(1, 10)],
            ..Default::default()
        },
    );
    let f = freqs(&s, 14);
    assert_eq!(
        offsets(&f),
        vec![0, 10, 0, -10, -20, -20, -10, 0, 10, 0, -10, -10, -20]
    );
}

#[test]
fn instrument_vibrato_starts_when_its_delay_reaches_1() {
    let mut i = ins();
    i.speed_ptr = 1;
    i.vibrato_delay = 3;
    let rows = vec![row(49, 1, 0, 0), row(0, 0, 0, 0)];
    let s = song(
        rows.clone(),
        0,
        100,
        vec![i.clone()],
        Tables {
            wave: note_wave(),
            speed: vec![t(2, 7)],
            ..Default::default()
        },
    );
    assert_eq!(
        offsets(&freqs(&s, 12)),
        vec![0, 0, 0, 7, 14, 7, 0, -7, -14, -7, 0]
    );
    // Tempo 6: tick-0 frames neither count the delay down nor swing.
    i.vibrato_delay = 2;
    let three = vec![row(49, 1, 0, 0), row(0, 0, 0, 0), row(0, 0, 0, 0)];
    let s = song(
        three,
        0,
        6,
        vec![i.clone()],
        Tables {
            wave: note_wave(),
            speed: vec![t(2, 7)],
            ..Default::default()
        },
    );
    assert_eq!(
        offsets(&freqs(&s, 14)),
        vec![0, 0, 7, 14, 7, 7, 0, -7, -14, -7, 0, 0, 7]
    );
    // Delay 0 never vibrates (gplay.c:767 `!cptr->vibdelay`).
    i.vibrato_delay = 0;
    let s = song(
        rows,
        0,
        100,
        vec![i],
        Tables {
            wave: note_wave(),
            speed: vec![t(2, 7)],
            ..Default::default()
        },
    );
    assert!(offsets(&freqs(&s, 20)).iter().all(|&o| o == 0));
}

#[test]
fn instrument_vibrato_runs_only_under_command_0() {
    // A 1 00 row (portamento at speed 0: no slide) stops the instrument
    // vibrato: GT's command is 1, not 0, so nothing falls through to it.
    let mut i = ins();
    i.speed_ptr = 1;
    i.vibrato_delay = 1;
    let rows = vec![row(49, 1, 1, 0), row(0, 0, 1, 0)];
    let s = song(
        rows,
        0,
        100,
        vec![i],
        Tables {
            wave: note_wave(),
            speed: vec![t(2, 7)],
            ..Default::default()
        },
    );
    assert!(offsets(&freqs(&s, 20)).iter().all(|&o| o == 0));
}

#[test]
fn commands_5_to_f_leave_a_running_vibrato_alone() {
    // Row 0: 4 01; row 1: 5 00 (set AD). GT sets `command` only for 0-4, so
    // the vibrato runs on through row 1; the old player stopped it.
    let rows = vec![row(49, 1, 4, 1), row(0, 0, 5, 0x00), row(0, 0, 5, 0x00)];
    let s = song(
        rows,
        0,
        6,
        vec![ins()],
        Tables {
            wave: note_wave(),
            speed: vec![t(1, 10)],
            ..Default::default()
        },
    );
    let f = freqs(&s, 14);
    assert_eq!(
        offsets(&f),
        vec![0, 10, 0, -10, -20, -20, -10, 0, 10, 0, -10, -10, -20]
    );
}
