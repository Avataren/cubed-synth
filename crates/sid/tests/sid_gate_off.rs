//! S5.9 (`.ai/sid-gateoff-verdict.md`): the gate bit of wave-table and
//! command-7 waveform bytes.
//!
//! GoatTracker 2 stores a wave-table row's byte whole, gate bit included
//! (gplay.c:525; `$E0-$EF` keep the low nibble, gplay.c:527), as does command
//! 7 (gplay.c:433 on a pattern row, gplay.c:652 from the wave table), and
//! writes `wave & gate` to $D404 every frame (gplay.c:945), where `gate` is a
//! mask: 0xFF while the channel's gate is on (gplay.c:362, 918), 0xFE after a
//! key off or the hard restart (gplay.c:129, 148, 916, 926). The readme says
//! the same: "The actual state of the gatebit will be the gatebit mask ANDed
//! with data from the wavetable". So a drum row `$80` or `$40` (gate bit
//! clear) releases the note; the player used to strip that bit and OR the
//! channel's gate back in, so the noise sustained until the next note.
//!
//! `fixtures/sid/s59-drum-example.asid` is Cadaver's "GoatTracker drum
//! example" (`src/tests/fixtures/gt-songs/cadaver/goattracker_drum_example.sng`)
//! through the app's real importer and song-file codec, pinned byte for byte
//! by `src/tests/sid-gate-off.test.ts`. Its wave table (1-based rows):
//!   bass drum  01 81 C4, 02 41 A8, 03 40 A4, 04 40 90, 05 FF 00;
//!   snare      06 81 C6, 07 41 AC, 08 41 A9, 09 80 C2, 0A 80 C4, 0B FF 00;
//!   hi-hat     0C 81 CA, 0D 80 C4, 0E FF 00;
//! every instrument first-frame $09, gate timer 2 with hard restart, and
//! voice 3 plays one drum a row at tempo 6: row 0 bass drum, row 1 hi-hat.

use cubed_sid::envelope::Stage;
use cubed_sid::song::{
    Instrument, OrderEntry, Orderlist, Pattern, Row, Subsong, TableRow, Tables, NOTE_KEY_OFF,
    SONG_FILE_VERSION,
};
use cubed_sid::{SidModel, SidSong, SidSongPlayer, DEFAULT_SAMPLE_RATE};

const DRUMS: &[u8] = include_bytes!("fixtures/sid/s59-drum-example.asid");
/// One PAL frame at 44.1 kHz is 879.8 samples (`player::frame_cycles`, GT
/// parity 0925b): a buffer that holds one; `samples_in_next_frame` says how
/// much of it a frame is.
const SPF: usize = 880;

fn frame(p: &mut SidSongPlayer) {
    let mut out = vec![0.0f32; SPF];
    let n = p.samples_in_next_frame();
    p.render(&mut out[..n]);
}

/// Voice `v`'s (control, envelope stage, envelope level) after each of `n` frames.
fn trace(p: &mut SidSongPlayer, v: usize, n: usize) -> Vec<(u8, Stage, u8)> {
    (0..n)
        .map(|_| {
            frame(p);
            let voice = p.chip().voice(v);
            (
                voice.control(),
                voice.envelope_stage(),
                voice.envelope_level(),
            )
        })
        .collect()
}

#[test]
fn a_drum_tables_gate_off_rows_release_the_note_in_the_real_song() {
    let song = SidSong::parse(DRUMS).expect("the imported song parses");
    let mut p = SidSongPlayer::new(song, DEFAULT_SAMPLE_RATE).expect("player builds");
    let t = trace(&mut p, 2, 12);
    let controls: Vec<u8> = t.iter().map(|x| x.0).collect();
    // GT (`wave & gate`, gplay.c:945), frame by frame:
    //   f0 bass drum trigger: the first-frame $09 (the S5.6 passthrough);
    //   f1 row 01 $81 & $FF; f2 row 02 $41; f3 row 03 $40 -> gate OFF;
    //   f4 row 04 $40 (and the hard restart, tick 4 = tempo 6 - timer 2);
    //   f5 row 05 stops the table, $40 stands;
    //   f6 hi-hat trigger $09; f7 row 0C $81; f8 row 0D $80 -> gate OFF;
    //   f9 row 0E stops, $80 stands; f10-f11 the hard restart, $80.
    assert_eq!(
        controls,
        vec![0x09, 0x81, 0x41, 0x40, 0x40, 0x40, 0x09, 0x81, 0x80, 0x80, 0x80, 0x80]
    );
    // The table's gate-off row starts the release on its own frame, before
    // the hard restart would.
    for f in [3, 8, 9] {
        assert_eq!(t[f].1, Stage::Release, "frame {f}: the release");
    }
}

const T: fn(u8, u8) -> TableRow = |left, right| TableRow { left, right };

/// One channel, one instrument (AD $09: instant attack, a slow decay to
/// sustain 0, so a retrigger shows as the level jumping back up), no hard
/// restart. GT plays the first-frame byte on the note's frame and the wave
/// table from the next (gplay.c:359-365, 509-512), so the instrument's
/// first-frame byte is row 1's waveform and its table starts at row 2: the
/// register reads row 1, row 2, ... from the trigger frame on, and a jump to
/// row 1 loops through it. Row 0 triggers C-4 with `row0_cmd`; row 1 carries
/// `row1`; tempo 8.
fn one_voice(wave: Vec<TableRow>, row0_cmd: (u8, u8), row1: Row) -> SidSongPlayer {
    let mut p0 = Pattern {
        rows: vec![Row::default(); 4],
    };
    p0.rows[0] = Row {
        note: 49,
        instrument: 1,
        command: row0_cmd.0,
        param: row0_cmd.1,
    };
    p0.rows[1] = row1;
    let blank = Pattern {
        rows: vec![Row::default(); 4],
    };
    // Row 1's waveform, as the table plays it (`$E0-$EF` are their low nibble).
    let first_wave = match wave[0].left {
        l @ 0xE0..=0xEF => l & 0x0F,
        l => l,
    };
    let list = |pattern: u8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose: 0,
            repeat: 1,
        }],
        restart: 0,
    };
    let song = SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid8580,
        channels: 3,
        speed_multiplier: 1,
        tempo: 8,
        name: b"s59".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![p0, blank],
        instruments: vec![Instrument {
            name: b"g".to_vec(),
            decay: 9,
            first_wave,
            wave_ptr: 2,
            ..Default::default()
        }],
        tables: Tables {
            wave,
            ..Default::default()
        },
    };
    let song = SidSong::parse(&song.to_bytes()).expect("parses");
    SidSongPlayer::new(song, DEFAULT_SAMPLE_RATE).expect("player builds")
}

#[test]
fn a_gate_on_waveform_change_from_the_table_does_not_retrigger() {
    // $41 -> $21 -> $11 -> $41 ... under a standing gate: the register's gate
    // bit stays high on every frame, so the chip sees no edge. The envelope
    // retriggers only on the gate bit's rising edge (a table row only writes
    // $D404): the decay runs on undisturbed, the level never goes back up.
    let wave = vec![T(0x41, 0x80), T(0x21, 0x80), T(0x11, 0x80), T(0xFF, 0x01)];
    let mut p = one_voice(wave, (0, 0), Row::default());
    let t = trace(&mut p, 0, 8);
    let controls: Vec<u8> = t.iter().map(|x| x.0).collect();
    assert_eq!(
        controls,
        vec![0x41, 0x21, 0x11, 0x41, 0x21, 0x11, 0x41, 0x21]
    );
    for (f, w) in t.windows(2).enumerate() {
        assert_eq!(
            w[1].1,
            Stage::DecaySustain,
            "frame {}: still decaying",
            f + 1
        );
        assert!(
            w[1].2 < w[0].2,
            "frame {}: the level fell ({} -> {}), no retrigger",
            f + 1,
            w[0].2,
            w[1].2
        );
    }
}

#[test]
fn a_gate_off_row_releases_and_a_later_gate_on_row_retriggers_while_the_channel_gate_is_on() {
    // The channel's gate mask is 0xFF throughout (no key off), so the
    // register follows the table's own gate bit: $40 releases, and the $41
    // after it is a rising edge the chip retriggers on (GT: wave & 0xFF).
    let wave = vec![
        T(0x41, 0x80),
        T(0x41, 0x80),
        T(0x40, 0x80),
        T(0x40, 0x80),
        T(0x41, 0x80),
        T(0xFF, 0x00),
    ];
    let mut p = one_voice(wave, (0, 0), Row::default());
    let t = trace(&mut p, 0, 6);
    let controls: Vec<u8> = t.iter().map(|x| x.0).collect();
    assert_eq!(controls, vec![0x41, 0x41, 0x40, 0x40, 0x41, 0x41]);
    assert_eq!((t[2].1, t[3].1), (Stage::Release, Stage::Release));
    // The retrigger: an instant attack back to the top, above the released level.
    assert_eq!(t[4].1, Stage::DecaySustain);
    assert!(t[4].2 > t[3].2, "retriggered: {} -> {}", t[3].2, t[4].2);
}

#[test]
fn after_a_key_off_no_table_byte_sets_the_gate_again() {
    // A key off makes the mask 0xFE (gplay.c:916): `$41 & $FE` = $40, so a
    // table that keeps writing $41 cannot re-open the gate — only a note,
    // key on or first-frame $FF can (gplay.c:358, 362, 918).
    let wave = vec![T(0x41, 0x80), T(0xFF, 0x01)];
    let mut p = one_voice(
        wave,
        (0, 0),
        Row {
            note: NOTE_KEY_OFF,
            ..Default::default()
        },
    );
    let t = trace(&mut p, 0, 12);
    let controls: Vec<u8> = t.iter().map(|x| x.0).collect();
    assert_eq!(controls, [&[0x41u8; 8][..], &[0x40; 4]].concat());
    assert!(t[8..].iter().all(|x| x.1 == Stage::Release));
}

#[test]
fn e0_to_ef_rows_keep_the_low_nibble_gate_bit_included() {
    // gplay.c:527: `$E0-$EF` set the waveform to `wave & $0F` — test, ring,
    // sync AND the gate bit. $E9 (the readme's "testbit+gate") holds the
    // gate; $E8 (test, gate bit clear) releases. (Row 1 is the first-frame
    // byte, so the rows under test start at row 2.)
    let wave = vec![
        T(0x41, 0x80),
        T(0xE9, 0x80),
        T(0x41, 0x80),
        T(0xE8, 0x80),
        T(0xFF, 0x00),
    ];
    let mut p = one_voice(wave, (0, 0), Row::default());
    let t = trace(&mut p, 0, 5);
    let controls: Vec<u8> = t.iter().map(|x| x.0).collect();
    assert_eq!(controls, vec![0x41, 0x09, 0x41, 0x08, 0x08]);
    assert_eq!(t[1].1, Stage::DecaySustain, "$E9 holds the gate");
    assert_eq!(t[3].1, Stage::Release);
}

#[test]
fn command_7_sets_the_whole_control_byte() {
    // CMD_SETWAVE (gcommon.h:11 = 7) stores its parameter whole, on a pattern
    // row (gplay.c:432-433) as from the wave table (gplay.c:651-652): `7 80`
    // on a gate-on channel writes $80 — noise, released.
    let wave = vec![T(0x41, 0x80), T(0xFF, 0x00)];
    let mut p = one_voice(
        wave,
        (0, 0),
        Row {
            command: 0x7,
            param: 0x80,
            ..Default::default()
        },
    );
    let t = trace(&mut p, 0, 10);
    let controls: Vec<u8> = t.iter().map(|x| x.0).collect();
    assert_eq!(controls, [&[0x41u8; 8][..], &[0x80; 2]].concat());
    assert_eq!(t[8].1, Stage::Release);
    // And `7 81` keeps the gate.
    let mut p = one_voice(
        vec![T(0x41, 0x80), T(0xFF, 0x00)],
        (0, 0),
        Row {
            command: 0x7,
            param: 0x81,
            ..Default::default()
        },
    );
    let controls: Vec<u8> = trace(&mut p, 0, 10).iter().map(|x| x.0).collect();
    assert_eq!(controls, [&[0x41u8; 8][..], &[0x81; 2]].concat());
}
