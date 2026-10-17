//! S5.19 tests: the player's register stream against GoatTracker 2.72's own
//! playroutine, every register (not just the control bytes S5.18 compared).
//!
//! Method (`.ai/sid-oracle/`): the 84-song corpus, subsong 0, 4000 frames, all
//! 25 registers of ours and of GT's playroutine frame-aligned. Before: 1 330 563
//! mismatching register-frames, no song exact (pulse width and cutoff off on
//! nearly every frame). After: 1 130 in one song (ballad: GT's editor shifts a
//! fine vibrato by 66 mod 32, its C64 player and ours by 66), 83 songs exact;
//! also the other 18 subsongs (4000 frames) and subsong 0 over 15 000 frames.
//! Each test below pins one of the causes on a song built here.

use super::player::SidSongPlayer;
use super::song::*;
use super::*;

fn t(l: u8, r: u8) -> TableRow {
    TableRow { left: l, right: r }
}

fn row(note: u8, instrument: u8) -> Row {
    Row {
        note,
        instrument,
        command: 0,
        param: 0,
    }
}

fn cmd(note: u8, instrument: u8, command: u8, param: u8) -> Row {
    Row {
        note,
        instrument,
        command,
        param,
    }
}

/// A GT-style instrument, as the importer makes one: no waveform, width or
/// filter of its own, first-frame byte $09, gate timer 2 with hard restart,
/// the wave table from row 1.
fn gt_ins() -> Instrument {
    Instrument {
        name: b"s519".to_vec(),
        sustain: 15,
        release: 7,
        first_wave: 0x09,
        gate_timer: 2,
        hard_restart: true,
        wave_ptr: 1,
        ..Default::default()
    }
}

const WAVE: [TableRow; 2] = [
    TableRow {
        left: 0x41,
        right: 0x00,
    },
    TableRow {
        left: 0xFF,
        right: 0x00,
    },
];

/// Channel 1 plays `rows` (the pattern loops), the other two a silent
/// pattern; tempo 6.
fn song(instruments: Vec<Instrument>, tables: Tables, rows: Vec<Row>) -> SidSong {
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
        model: SidModel::Sid6581,
        channels: 3,
        speed_multiplier: 1,
        tempo: 6,
        name: b"s519".to_vec(),
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
        tables,
    };
    SidSong::parse(&s.to_bytes()).expect("parses")
}

/// The 25 registers as written by the end of each of the first `frames` frames.
fn regs(song: SidSong, frames: usize) -> Vec<[u8; 25]> {
    let mut p = SidSongPlayer::new(song, DEFAULT_SAMPLE_RATE).expect("player builds");
    (0..frames)
        .map(|_| {
            p.frame();
            std::array::from_fn(|r| p.chip().written(r as u8))
        })
        .collect()
}

fn column(r: &[[u8; 25]], reg: usize) -> Vec<u8> {
    r.iter().map(|f| f[reg]).collect()
}

fn width(r: &[[u8; 25]]) -> Vec<u16> {
    r.iter().map(|f| (f[3] as u16) << 8 | f[2] as u16).collect()
}

fn freq(r: &[[u8; 25]]) -> Vec<u16> {
    r.iter().map(|f| (f[1] as u16) << 8 | f[0] as u16).collect()
}

fn wave() -> Tables {
    Tables {
        wave: WAVE.to_vec(),
        ..Default::default()
    }
}

#[test]
fn the_coming_notes_instrument_decides_the_gate_off_and_hard_restart() {
    // GT switches to the row's instrument as it reads the row, gate-timer
    // ticks early, and tests THAT instrument's $40/$80 bits (gplay.c:907-926).
    // Row 1's instrument 2 has $40 (no gate-off): the gate stays on before it
    // although instrument 1 (sounding) has a hard restart. Row 2's instrument
    // 1 does: its gate-off and AD $0F come while instrument 2 sounds.
    // ("Ups and Downs" voice 3: 768 frames off, the gate dropped early.)
    let quiet = Instrument {
        no_gate_off: true,
        ..gt_ins()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    rows[1] = row(49, 2);
    rows[2] = row(49, 1);
    let r = regs(song(vec![gt_ins(), quiet], wave(), rows), 12);
    let ctl = column(&r, 4);
    let ad = column(&r, 5);
    assert_eq!(
        &ctl[4..6],
        &[0x41, 0x41],
        "frames 4-5: no gate-off before instrument 2, controls {ctl:02x?}"
    );
    assert_eq!(
        &ctl[10..12],
        &[0x40, 0x40],
        "frames 10-11: gate-off before instrument 1, controls {ctl:02x?}"
    );
    assert_eq!(
        &ad[10..12],
        &[0x0F, 0x0F],
        "frames 10-11: instrument 1's hard restart, AD {ad:02x?}"
    );
}

#[test]
fn a_hard_restart_comes_from_the_coming_instrument_too() {
    // Instrument 1 has $80 (no hard restart), instrument 2 not: before
    // instrument 2's note the gate-off writes AD $0F ("Gremlin Funk").
    let no_hr = Instrument {
        hard_restart: false,
        ..gt_ins()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    rows[1] = row(49, 2);
    let r = regs(song(vec![no_hr, gt_ins()], wave(), rows), 6);
    assert_eq!(
        &column(&r, 5)[4..6],
        &[0x0F, 0x0F],
        "AD {:02x?}",
        column(&r, 5)
    );
}

#[test]
fn every_channel_starts_on_instrument_1() {
    // GT's channels start on instrument 1 (gplay.c:62), so its gate timer
    // brings the hard restart before a channel's first note: row 1's note
    // (frame 6) has AD $0F on frames 4-5.
    let mut rows = vec![Row::default(); 4];
    rows[1] = row(49, 1);
    let r = regs(song(vec![gt_ins()], wave(), rows), 6);
    assert_eq!(
        &column(&r, 5)[4..6],
        &[0x0F, 0x0F],
        "AD {:02x?}",
        column(&r, 5)
    );
}

#[test]
fn the_pulse_table_rests_on_the_note_frame_the_row_read_and_a_patterns_last_row() {
    // Width $800, then +2 a frame. GT skips the pulse table on the note's
    // frame (frame 0), on the frame the next row is read (tick 2: frames 4,
    // 10, 16, 22) and on the first frame of the pattern's last row (frame 18,
    // row 3 of 4), with its default pulse optimisation.
    let ins = Instrument {
        pulse_ptr: 1,
        ..gt_ins()
    };
    let tables = Tables {
        wave: WAVE.to_vec(),
        pulse: vec![t(0x88, 0x00), t(0x7F, 0x02)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    let w = width(&regs(song(vec![ins], tables, rows), 20));
    let want: Vec<u16> = vec![
        0x000, 0x800, 0x802, 0x804, 0x804, 0x806, 0x808, 0x80A, 0x80C, 0x80E, 0x80E, 0x810, 0x812,
        0x814, 0x816, 0x818, 0x818, 0x81A, 0x81A, 0x81C,
    ];
    assert_eq!(w, want);
}

#[test]
fn the_pulse_table_walks_as_gts() {
    // A jump takes its target row as data, even another jump ($FF $02 sets
    // $F02: "Maximum Rastertime Test"'s every note); a left of 0 stalls.
    let ins = Instrument {
        pulse_ptr: 1,
        ..gt_ins()
    };
    let mut rows = vec![Row::default(); 8];
    rows[0] = row(49, 1);
    let tables = Tables {
        wave: WAVE.to_vec(),
        pulse: vec![t(0x80, 0x40), t(0xFF, 0x02)],
        ..Default::default()
    };
    assert_eq!(
        &width(&regs(song(vec![ins.clone()], tables, rows.clone()), 4))[1..],
        &[0x040, 0xF02, 0xF02]
    );
    let tables = Tables {
        wave: WAVE.to_vec(),
        pulse: vec![t(0x88, 0x00), t(0x00, 0x00), t(0x84, 0x00)],
        ..Default::default()
    };
    assert_eq!(
        &width(&regs(song(vec![ins], tables, rows), 5))[1..],
        &[0x800, 0x800, 0x800, 0x800]
    );
}

#[test]
fn the_pulse_widths_low_bit_is_not_written() {
    // GT writes the low byte `& 0xfe` (gplay.c:943).
    let ins = Instrument {
        pulse_ptr: 1,
        ..gt_ins()
    };
    let tables = Tables {
        wave: WAVE.to_vec(),
        pulse: vec![t(0x80, 0x41)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    assert_eq!(width(&regs(song(vec![ins], tables, rows), 2))[1], 0x040);
}

#[test]
fn the_filter_registers_are_the_frames_first() {
    // GT runs its filter table and sets $15-$18 before the channels, so a
    // note's filter table is heard from the frame after the note's
    // (gplay.c:252-302), a row's cutoff command likewise ("Coconut
    // Conundrum": every filter step one frame early).
    let ins = Instrument {
        filter_ptr: 1,
        ..gt_ins()
    };
    let tables = Tables {
        wave: WAVE.to_vec(),
        filter: vec![t(0x90, 0xF1), t(0x00, 0x40), t(0xFF, 0x00)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    rows[1] = cmd(0, 0, 0xC, 0x20);
    let r = regs(song(vec![ins], tables, rows), 8);
    let (hi, res, mode) = (column(&r, 22), column(&r, 23), column(&r, 24));
    assert_eq!(
        (hi[0], res[0], mode[0]),
        (0x00, 0x00, 0x0F),
        "frame 0: nothing yet"
    );
    assert_eq!(
        (hi[1], res[1], mode[1]),
        (0x40, 0xF1, 0x1F),
        "frame 1: the table's first row"
    );
    assert_eq!(
        (hi[6], hi[7]),
        (0x40, 0x20),
        "row 1's C 20 from frame 7: {hi:02x?}"
    );
}

#[test]
fn a_note_leaves_the_filter_routing_to_the_table() {
    // GT never routes on a note; a GT instrument has the neutral filter, which
    // must not take its voice out of the filter the table put it in.
    let routed = Instrument {
        filter_ptr: 1,
        ..gt_ins()
    };
    let tables = Tables {
        wave: WAVE.to_vec(),
        filter: vec![t(0x90, 0xF1), t(0xFF, 0x00)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    rows[1] = row(49, 2);
    let r = regs(song(vec![routed, gt_ins()], tables, rows), 10);
    assert_eq!(
        column(&r, 23)[9],
        0xF1,
        "resonance/routing {:02x?}",
        column(&r, 23)
    );
}

#[test]
fn a_note_frame_keeps_the_old_frequency() {
    // GT sets no pitch on a note: the wave table's first step does, a frame
    // later. With a first-frame byte without the test bit ($21, "Lolo") the
    // old pitch sounds for that frame.
    let ins = Instrument {
        first_wave: 0x21,
        ..gt_ins()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    rows[1] = row(61, 1);
    let f = freq(&regs(song(vec![ins], wave(), rows), 8));
    assert_eq!(f[0], 0, "frame 0: the channel's old (none)");
    assert_eq!(f[1], gt_note_freq_reg(48));
    assert_eq!(
        f[6],
        gt_note_freq_reg(48),
        "frame 6: row 1's note frame still at the old note"
    );
    assert_eq!(f[7], gt_note_freq_reg(60));
}

#[test]
fn a_gt_instrument_holds_its_first_frame_byte_until_the_table_sets_a_waveform() {
    // GT keeps the first-frame byte as the channel's waveform (gplay.c:359-365):
    // a table that starts with no-waveform rows keeps $09 on ("Unleash the
    // Cheese", voice 2), not $01.
    let tables = Tables {
        wave: vec![t(0x00, 0x00), t(0x00, 0x00), t(0x21, 0x00), t(0xFF, 0x00)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    let ctl = column(&regs(song(vec![gt_ins()], tables, rows), 4), 4);
    assert_eq!(ctl, vec![0x09, 0x09, 0x09, 0x21]);
}

#[test]
fn a_first_frame_byte_of_0_leaves_the_gate_as_it_is() {
    // $00: waveform and gate as they are (gplay.c:359). After the hard
    // restart's gate-off the note does not gate on ("Coconut Conundrum"'s
    // first note); the table's waveform plays gate-off.
    let silent = Instrument {
        first_wave: 0x00,
        ..gt_ins()
    };
    let tables = Tables {
        wave: vec![t(0x81, 0x00), t(0xFF, 0x00)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    rows[1] = row(49, 2);
    let ctl = column(&regs(song(vec![gt_ins(), silent], tables, rows), 9), 4);
    assert_eq!(
        &ctl[4..9],
        &[0x80, 0x80, 0x80, 0x80, 0x80],
        "controls {ctl:02x?}"
    );
}

#[test]
fn the_cutoff_steps_wrap() {
    // GT's cutoff is a u8 that a filter step wraps (gplay.c:293): 2, 1, 0,
    // $FF, $FE, not stuck at 0 ("Midnight Dream", "Flumbos Keps").
    let ins = Instrument {
        filter_ptr: 1,
        ..gt_ins()
    };
    let tables = Tables {
        wave: WAVE.to_vec(),
        filter: vec![t(0x90, 0xF1), t(0x00, 0x02), t(0x05, 0xFF)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 8];
    rows[0] = row(49, 1);
    let hi = column(&regs(song(vec![ins], tables, rows), 7), 22);
    assert_eq!(&hi[1..7], &[0x02, 0x01, 0x00, 0xFF, 0xFE, 0xFD]);
}

#[test]
fn a_slide_speed_from_8000_is_the_fine_speed_and_slides_wrap() {
    // Speed row $80 $02: a quarter of the gap from the last note to the next
    // per tick (gplay.c:736-741), not $8002.
    let tables = Tables {
        wave: WAVE.to_vec(),
        speed: vec![t(0x80, 0x02), t(0x7F, 0xFF)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    rows[1] = cmd(0, 0, 0x1, 1);
    let f = freq(&regs(song(vec![gt_ins()], tables.clone(), rows.clone()), 9));
    let step = (gt_note_freq_reg(49) - gt_note_freq_reg(48)) >> 2;
    assert_eq!(f[7] - f[6], step, "freqs {f:04x?}");
    assert_eq!(f[8] - f[7], step, "freqs {f:04x?}");
    // Speed $7FFF from a high note wraps past $FFFF (u16, gplay.c:744).
    rows[0] = row(90, 1);
    rows[1] = cmd(0, 0, 0x1, 2);
    let f = freq(&regs(song(vec![gt_ins()], tables, rows), 9));
    assert_eq!(f[7], gt_note_freq_reg(89).wrapping_add(0x7FFF));
    assert_eq!(
        f[8],
        gt_note_freq_reg(89).wrapping_add(0xFFFE),
        "freqs {f:04x?}"
    );
}

#[test]
fn a_glide_arriving_restarts_the_vibrato() {
    // Vibrato on row 0, a glide that arrives at once on row 1, vibrato again
    // on row 2: GT zeroes the phase on arrival (gplay.c:822-832), so row 2's
    // swing is row 0's, from the new note ("Pirate" voice 3).
    let tables = Tables {
        wave: WAVE.to_vec(),
        speed: vec![t(0x04, 0x20), t(0x7F, 0xFF)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = cmd(49, 1, 0x4, 1);
    rows[1] = cmd(52, 0, 0x3, 2);
    rows[2] = cmd(0, 0, 0x4, 1);
    let f = freq(&regs(song(vec![gt_ins()], tables, rows), 18));
    let from = |base: u16, at: &[u16]| {
        at.iter()
            .map(|&v| v.wrapping_sub(base) as i16)
            .collect::<Vec<_>>()
    };
    let first = from(gt_note_freq_reg(48), &f[2..6]);
    let again = from(gt_note_freq_reg(51), &f[13..17]);
    assert_eq!(first, again, "freqs {f:04x?}");
}

#[test]
fn wave_table_commands_run_as_the_pattern_commands() {
    // $F7 sets the waveform, $FC the cutoff (heard the next frame, as the
    // filter registers are the frame's first), $F1 slides with a speed row.
    let tables = Tables {
        wave: vec![
            t(0x41, 0x00),
            t(0xF7, 0x21),
            t(0xFC, 0x33),
            t(0xF1, 0x01),
            t(0xF1, 0x01),
            t(0xFF, 0x00),
        ],
        speed: vec![t(0x00, 0x10)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 8];
    rows[0] = row(49, 1);
    let r = regs(song(vec![gt_ins()], tables, rows), 7);
    let (ctl, hi, f) = (column(&r, 4), column(&r, 22), freq(&r));
    assert_eq!(ctl[2], 0x21, "frame 2: $F7 21, controls {ctl:02x?}");
    assert_eq!(
        (hi[3], hi[4]),
        (0x00, 0x33),
        "$FC 33 on frame 3, heard on 4: {hi:02x?}"
    );
    let n = gt_note_freq_reg(48);
    assert_eq!(&f[4..6], &[n + 0x10, n + 0x20], "$F1 01 twice: {f:04x?}");
}

#[test]
fn filter_control_0_stops_the_table_and_volume_from_10_is_ignored() {
    // B 00 also stops the filter table (gplay.c:468-471); D with $10 up is
    // ignored (gplay.c:477-479), not masked to its low nibble.
    let ins = Instrument {
        filter_ptr: 1,
        ..gt_ins()
    };
    let tables = Tables {
        wave: WAVE.to_vec(),
        filter: vec![t(0x90, 0xF1), t(0x00, 0x10), t(0x7F, 0x01)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    rows[1] = cmd(0, 0, 0xB, 0x00);
    rows[2] = cmd(0, 0, 0xD, 0x13);
    let r = regs(song(vec![ins], tables, rows), 16);
    let hi = column(&r, 22);
    assert_eq!(hi[8], hi[15], "the cutoff stands after B 00: {hi:02x?}");
    assert_eq!(
        column(&r, 24)[15] & 0x0F,
        0x0F,
        "the volume stays 15 after D 13"
    );
}

#[test]
fn wave_command_fd_tests_its_own_parameter() {
    // GT-parity 0925b: $FD sets the volume only below $10, testing the wave
    // row's own parameter as GT's C64 player does (player.s:291-305). The
    // old test of the pattern row's parameter (the editor's, gplay.c:
    // 686-689) let `$FD 1F` under a row with parameter 0 write $1F: $D418
    // read $1F (low-pass on) from frame 3; now it stays $0F.
    let tables = Tables {
        wave: vec![t(0x41, 0x00), t(0xFD, 0x1F), t(0xFD, 0x07), t(0xFF, 0x00)],
        ..Default::default()
    };
    let mut rows = vec![Row::default(); 4];
    rows[0] = row(49, 1);
    let r = regs(song(vec![gt_ins()], tables, rows), 6);
    let mv = column(&r, 24);
    // $FD 1F runs on frame 2, $FD 07 on frame 3; $D418 is written at the top
    // of the frame (gplay.c:299-302), so each is heard a frame later.
    assert_eq!(
        &mv[2..6],
        &[0x0F, 0x0F, 0x07, 0x07],
        "mode/volume: {mv:02x?}"
    );
}

#[test]
fn illegal_wave_commands_only_advance() {
    // $F0, $F8, $FE are illegal in GT (readme §3.4.1; its editor stops the
    // song, gplay.c:534-538). Here the row costs its frame, changes nothing
    // (no wave, no note, no jump, no tempo) and the table moves on.
    for illegal in [0xF0u8, 0xF8, 0xFE] {
        let tables = Tables {
            wave: vec![
                t(0x41, 0x00),
                t(illegal, 0x01),
                t(0x21, 0x0C),
                t(0xFF, 0x00),
            ],
            speed: vec![t(0x02, 0x02)],
            ..Default::default()
        };
        let mut rows = vec![Row::default(); 4];
        rows[0] = row(49, 1);
        rows[1] = row(49, 1);
        let r = regs(song(vec![gt_ins()], tables, rows), 8);
        let ctl = column(&r, 4);
        let f = freq(&r);
        let n = gt_note_freq_reg(48);
        assert_eq!(
            &ctl[1..4],
            &[0x41, 0x41, 0x21],
            "${illegal:02X}: controls {ctl:02x?}"
        );
        assert_eq!(
            &f[1..4],
            &[n, n, gt_note_freq_reg(60)],
            "${illegal:02X}: freqs {f:04x?}"
        );
        // Row 1's hard restart (gate timer 2) still comes on frame 4: the
        // tempo stands (no funktempo from $FE).
        assert_eq!(ctl[4], 0x20, "${illegal:02X}: controls {ctl:02x?}");
    }
}
