//! S3 tests: the song file (`song.rs`) and the player (`player.rs`).
//!
//! Every test builds a `SidSong`, sends it through the file bytes
//! (`to_bytes` -> `parse`, the path the app's songs take) and plays it on a
//! real `Chip` through `SidSongPlayer`. Expected values are derived in the
//! comment above each assertion from the player's documented semantics and
//! the note table, never captured from a run. The app-produced song (the
//! store -> file -> chip chain) is `tests/sid_song_chain.rs`.

use super::envelope::Stage;
use super::player::{frame_cycles, note_index, SidSongPlayer, FRAME_HZ, PAL_FRAME_CYCLES};
use super::song::*;
use super::*;

/// One PAL frame at 44.1 kHz is 879.8 samples (`player::frame_cycles`, GT
/// parity 0925b): a buffer that holds one; `samples_in_next_frame` says how
/// much of it a frame is.
const SPF: usize = 880;

fn ins(waveform: u8) -> Instrument {
    Instrument {
        name: b"t".to_vec(),
        attack: 0,
        decay: 0,
        sustain: 15,
        release: 0,
        first_wave: waveform | 0x01,
        ..Default::default()
    }
}

fn row(note: u8, instrument: u8, command: u8, param: u8) -> Row {
    Row {
        note,
        instrument,
        command,
        param,
    }
}

fn blank(rows: usize) -> Pattern {
    Pattern {
        rows: vec![Row::default(); rows],
    }
}

/// A one-subsong song: channel c plays `lists[c]` (pattern, transpose, repeat).
fn song(
    patterns: Vec<Pattern>,
    lists: [Vec<(u8, i8, u8)>; 3],
    instruments: Vec<Instrument>,
    tables: Tables,
) -> SidSong {
    let orderlists = lists
        .into_iter()
        .map(|l| Orderlist {
            entries: l
                .into_iter()
                .map(|(pattern, transpose, repeat)| OrderEntry {
                    pattern,
                    transpose,
                    repeat,
                })
                .collect(),
            restart: 0,
        })
        .collect();
    SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid8580,
        channels: 3,
        speed_multiplier: 1,
        tempo: 6,
        name: b"s3".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong { orderlists }],
        patterns,
        instruments,
        tables,
    }
}

/// Through the file bytes, as every app song arrives.
fn via_file(s: &SidSong) -> SidSong {
    let bytes = s.to_bytes();
    let back = SidSong::parse(&bytes).expect("parses");
    assert_eq!(&back, s, "parse(to_bytes(song)) == song");
    assert_eq!(back.to_bytes(), bytes, "to_bytes(parse(bytes)) == bytes");
    back
}

fn player(s: &SidSong) -> SidSongPlayer {
    SidSongPlayer::new(via_file(s), DEFAULT_SAMPLE_RATE).expect("player builds")
}

/// Renders one frame's samples; the frame's register writes happen first.
fn frame(p: &mut SidSongPlayer) -> Vec<f32> {
    let mut out = vec![0.0f32; SPF];
    let n = p.samples_in_next_frame();
    p.render(&mut out[..n]);
    out
}

/// Goertzel power of `x` at `hz`.
fn power(x: &[f32], hz: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * hz / DEFAULT_SAMPLE_RATE;
    let c = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s0 = v as f64 + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - c * s1 * s2
}

// ---------------------------------------------------------------------------
// The note table
// ---------------------------------------------------------------------------

#[test]
fn note_table_pins_match_the_app() {
    // reg = round(440 * 2^((i - 57) / 12) * 2^24 / 985 248):
    // GT's table: equal temperament at a nominal 985 000 Hz clock (2^24 / 985000
    // = 17.0326 units per Hz): C-0: 16.3516 Hz -> 278.5 -> 279; A-4: 440 ->
    // 7494.3 -> 7494; G#7: 3322.44 -> 56590. The app's
    // `sidNoteFreqReg` test pins the same three.
    assert_eq!(gt_note_freq_reg(0), 279);
    assert_eq!(gt_note_freq_reg(57), 7494);
    assert_eq!(gt_note_freq_reg(92), 56590);
    // S5.10: the index is 7 bits, as GT's table reads it (200 & 0x7f = 72);
    // strictly rising over the row notes.
    assert_eq!(gt_note_freq_reg(200), gt_note_freq_reg(72));
    for i in 1..GT_NOTE_COUNT {
        assert!(gt_note_freq_reg(i) > gt_note_freq_reg(i - 1));
    }
    // Row note 1 is C-0; S5.10: transposes wrap as GT's u8 note does
    // (gplay.c:350, 921; `tests_s510.rs`), no longer clamped.
    assert_eq!(note_index(1, 0), 0);
    assert_eq!(note_index(58, 0), 57);
    assert_eq!(note_index(1, -12), 244);
    assert_eq!(note_index(93, 5), 97);
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

fn full_song() -> SidSong {
    let mut i1 = ins(0x40);
    i1.first_wave = 0x09;
    i1.gate_timer = 63;
    i1.hard_restart = true;
    i1.vibrato_delay = 255;
    i1.wave_ptr = 1;
    i1.pulse_ptr = 1;
    i1.filter_ptr = 1;
    i1.speed_ptr = 1;
    let t = |l: u8, r: u8| TableRow { left: l, right: r };
    let mut s = song(
        vec![
            Pattern {
                rows: vec![
                    row(93, 1, 0xF, 0xFF),
                    row(NOTE_KEY_OFF, 0, 0, 0),
                    row(NOTE_KEY_ON, 0, 0, 0),
                ],
            },
            blank(128),
        ],
        [
            vec![(0, -64, 16)],
            vec![(1, 63, 1), (0, 0, 1)],
            vec![(1, 0, 1)],
        ],
        vec![i1, Instrument::default()],
        Tables {
            wave: vec![t(0x41, 0x80), t(0xFF, 1)],
            pulse: vec![t(0x88, 0)],
            filter: vec![t(0x90, 0xF1)],
            speed: vec![t(2, 3)],
        },
    );
    s.model = SidModel::Sid6581;
    s.speed_multiplier = 16;
    s.tempo = 127;
    s.name = vec![0xFF; 32];
    s.author = b" edge ".to_vec();
    s.subsongs.push(s.subsongs[0].clone());
    s.subsongs[1].orderlists[2].restart = 0;
    s
}

#[test]
fn file_round_trips_every_field_at_its_limits() {
    let s = full_song();
    // via_file asserts both directions.
    let back = via_file(&s);
    assert_eq!(back.model, SidModel::Sid6581);
    assert_eq!(back.subsongs[0].orderlists[0].entries[0].transpose, -64);
}

#[test]
fn file_refuses_what_the_model_refuses() {
    let good = full_song().to_bytes();
    let refuse = |bytes: &[u8], why: &str| {
        let e = SidSong::parse(bytes).expect_err(why);
        assert!(!e.0.is_empty(), "{why}: a reason");
    };
    let mut b = good.clone();
    b[0] = b'X';
    refuse(&b, "bad magic");
    let mut b = good.clone();
    b.push(0);
    refuse(&b, "a trailing byte");
    refuse(&good[..good.len() - 1], "a truncated file");
    let mut b = good.clone();
    b[5] = 2;
    refuse(&b, "an unknown chip model");
    let mut b = good.clone();
    b[6] = 6;
    refuse(&b, "dual SID is S7");
    // Version 1 (instruments with a waveform, pulse width and filter of
    // their own) is not read (plan-sid-authoring.md D1).
    let mut b = good.clone();
    b[4] = 1;
    refuse(&b, "version 1");
}

// ---------------------------------------------------------------------------
// The player
// ---------------------------------------------------------------------------

/// Channel 1 plays A-4 on a triangle, the others nothing.
fn solo_a4() -> SidSong {
    let mut p0 = blank(8);
    p0.rows[0] = row(58, 1, 0, 0); // row note 58 = index 57 = A-4
    song(
        vec![p0, blank(8)],
        [vec![(0, 0, 1)], vec![(1, 0, 1)], vec![(1, 0, 1)]],
        vec![ins(0x10)],
        Tables::default(),
    )
}

#[test]
fn a_note_plays_its_table_register_on_the_songs_chip() {
    let mut p = player(&solo_a4());
    assert_eq!(p.chip().model(), SidModel::Sid8580);
    let first = frame(&mut p);
    // The trigger frame wrote A-4's register, triangle + gate, AD 00 SR F0.
    // Attack 0 is 2 ms and the frame 20 ms, so the envelope has already
    // reached the sustain level 0xFF by the frame's end.
    let v = p.chip().voice(0);
    assert_eq!(v.frequency(), 7494);
    assert_eq!(v.control(), 0x11);
    assert_eq!(v.envelope_stage(), Stage::DecaySustain);
    assert_eq!(v.envelope_level(), 0xFF);
    // 20 frames = 0.4 s of it: the triangle's fundamental, 7493 * 985248 /
    // 2^24 = 440.03 Hz, dominates a semitone off (466.2 Hz) by far.
    let mut x = first;
    for _ in 0..19 {
        x.extend(frame(&mut p));
    }
    let tail = &x[4410..];
    assert!(power(tail, 440.03) > 1000.0 * power(tail, 466.16));
    assert!(x.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
}

#[test]
fn rows_advance_every_tempo_frames_at_the_pal_frame_rate() {
    let mut s = solo_a4();
    s.tempo = 3;
    let mut p = player(&s);
    assert_eq!(p.samples_per_frame(), DEFAULT_SAMPLE_RATE / FRAME_HZ);
    // Tempo 3: row r is read on frame 3r, so after 3r frames the channel is at row r.
    for r in 0..8 {
        assert_eq!(p.position(0), (0, r));
        for _ in 0..3 {
            frame(&mut p);
        }
    }
    // 8 rows, one entry, restart 0: back to the top, flagged as a loop.
    assert_eq!(p.position(0), (0, 0));
    assert!(p.looped());
    assert_eq!(p.frames(), 24);
}

#[test]
fn multispeed_ticks_on_gts_cia_period() {
    let mut s = solo_a4();
    s.speed_multiplier = 2;
    let mut p = player(&s);
    // GT's exported 2x tune: CIA latch $4CC7 / 2 = 9827, a period of 9828
    // cycles (half a PAL frame), 100.249 Hz: 439.9 samples a frame, so one
    // second of output starts 101 frames (was 100 at a flat 100 Hz).
    assert_eq!(
        p.samples_per_frame(),
        9828.0 * DEFAULT_SAMPLE_RATE / PAL_CLOCK_HZ
    );
    let mut out = vec![0.0f32; 44_100];
    p.render(&mut out);
    assert_eq!(p.frames(), 101);
}

#[test]
fn the_frame_is_the_pal_vertical_blank_and_gts_cia_period() {
    // GT-parity 0925b: 1x runs on the PAL vertical blank, 312 lines x 63
    // cycles (GT's PSID speed bit 0, greloc.c:1590-1596): 50.1245 Hz, not
    // GT's editor's 50. Multispeed m: GT's CIA latch $4CC7 / m, period
    // latch + 1 (greloc.c:1551-1562), a whole PAL frame split m ways where
    // m divides 19 656.
    assert_eq!(PAL_FRAME_CYCLES, 19_656);
    assert!((FRAME_HZ - 50.124_54).abs() < 1e-4, "{FRAME_HZ}");
    assert_eq!(frame_cycles(1), 19_656);
    for m in [2u8, 3, 4, 6, 8] {
        assert_eq!(frame_cycles(m), 19_656 / m as u32, "x{m}");
    }
    assert_eq!(frame_cycles(5), 3_932);
    assert_eq!(frame_cycles(16), 1_229);
    let p = player(&solo_a4());
    assert!(
        (p.samples_per_frame() - 879.809).abs() < 1e-3,
        "{}",
        p.samples_per_frame()
    );
}

#[test]
fn orderlist_repeat_transpose_and_restart() {
    // Channel 1: pattern 0 twice at +12, then pattern 1 at -1; restart at entry 1.
    let mut p0 = blank(2);
    p0.rows[0] = row(58, 1, 0, 0);
    let mut p1 = blank(2);
    p1.rows[0] = row(58, 1, 0, 0);
    let mut s = song(
        vec![p0, p1],
        [
            vec![(0, 12, 2), (1, -1, 1)],
            vec![(1, 0, 1)],
            vec![(1, 0, 1)],
        ],
        vec![ins(0x10)],
        Tables::default(),
    );
    s.tempo = 1;
    s.subsongs[0].orderlists[0].restart = 1;
    let mut p = player(&s);
    let mut notes = Vec::new();
    for _ in 0..10 {
        frame(&mut p);
        notes.push(p.channel_note(0));
    }
    // One frame a row, a note every 2 rows: A-5 (69), held, A-5 again (the
    // repeat), held, G#4 (56), held, then the restart plays entry 1 again.
    assert_eq!(notes, vec![69, 69, 69, 69, 56, 56, 56, 56, 56, 56]);
    assert!(p.looped());
    assert_eq!(p.chip().voice(0).frequency(), gt_note_freq_reg(56));
}

#[test]
fn wave_table_is_waveform_and_arpeggio() {
    // Rows: pulse +0, pulse +4, saw +7, jump to 1; each row's gate bit set
    // (S5.9: the row's byte is the control byte, gate included).
    let t = |l: u8, r: u8| TableRow { left: l, right: r };
    let mut i = ins(0x10);
    i.wave_ptr = 1;
    let mut p0 = blank(8);
    p0.rows[0] = row(49, 1, 0, 0); // C-4 = index 48
    let s = song(
        vec![p0, blank(8)],
        [vec![(0, 0, 1)], vec![(1, 0, 1)], vec![(1, 0, 1)]],
        vec![i],
        Tables {
            wave: vec![t(0x41, 0x00), t(0x41, 0x04), t(0x21, 0x07), t(0xFF, 0x01)],
            ..Default::default()
        },
    );
    let mut p = player(&s);
    let mut seen = Vec::new();
    for _ in 0..7 {
        frame(&mut p);
        let v = p.chip().voice(0);
        seen.push((v.frequency(), v.control()));
    }
    let (c, e, g) = (
        gt_note_freq_reg(48),
        gt_note_freq_reg(52),
        gt_note_freq_reg(55),
    );
    // The note's frame plays the first-frame byte and leaves the pitch to the
    // table (gplay.c:359-365, 509-512): no pitch yet on a fresh channel. Then
    // one table row a frame; the jump row costs no frame.
    assert_eq!(
        seen,
        vec![
            (0, 0x11),
            (c, 0x41),
            (e, 0x41),
            (g, 0x21),
            (c, 0x41),
            (e, 0x41),
            (g, 0x21)
        ]
    );
}

#[test]
fn pulse_table_sets_then_sweeps_the_width() {
    let t = |l: u8, r: u8| TableRow { left: l, right: r };
    let mut i = ins(0x40);
    i.pulse_ptr = 1;
    let mut p0 = blank(8);
    p0.rows[0] = row(49, 1, 0, 0);
    let s = song(
        vec![p0, blank(8)],
        [vec![(0, 0, 1)], vec![(1, 0, 1)], vec![(1, 0, 1)]],
        vec![i],
        // Set 0x400, then +0x10 for 3 frames, then -0x20 (0xE0) for 2, then stop.
        Tables {
            pulse: vec![t(0x84, 0x00), t(0x03, 0x10), t(0x02, 0xE0)],
            ..Default::default()
        },
    );
    let mut p = player(&s);
    let widths: Vec<u16> = (0..8)
        .map(|_| {
            frame(&mut p);
            p.chip().voice(0).pulse_width()
        })
        .collect();
    // The note's frame skips the pulse table as GT's does (gplay.c:509-512;
    // S5.19): a fresh channel's width 0, then the table from frame 1.
    assert_eq!(
        widths,
        vec![0, 0x400, 0x410, 0x420, 0x430, 0x410, 0x3F0, 0x3F0]
    );
}

#[test]
fn each_instruments_filter_table_drives_the_chip_filter() {
    let t = |l: u8, r: u8| TableRow { left: l, right: r };
    // Instrument 2 (row 1): LP with $17 = 0xA2 (res 10, voice 2 routed),
    // cutoff 0x40<<3, then +1 (8 register steps) for 2 frames. Instrument 1
    // (row 5): BP with $17 = 0x91 (res 9, voice 1 routed), cutoff 0x24<<3.
    let mut i1 = ins(0x20);
    i1.filter_ptr = 5;
    let mut i2 = ins(0x20);
    i2.filter_ptr = 1;
    let mut p0 = blank(8);
    p0.rows[0] = row(49, 1, 0, 0);
    p0.rows[2] = row(49, 2, 0, 0);
    let s = song(
        vec![p0, blank(8)],
        [vec![(0, 0, 1)], vec![(1, 0, 1)], vec![(1, 0, 1)]],
        vec![i1, i2],
        Tables {
            filter: vec![
                t(0x90, 0xA2),
                t(0x00, 0x40),
                t(0x02, 0x01),
                t(0xFF, 0x00),
                t(0xA0, 0x91),
                t(0x00, 0x24),
                t(0xFF, 0x00),
            ],
            ..Default::default()
        },
    );
    let mut s = s;
    s.tempo = 1;
    let mut p = player(&s);
    // The filter registers are the frame's first (GT's order, S5.19): a
    // note's filter is heard from the next frame.
    frame(&mut p);
    let f = p.chip().filter();
    assert_eq!((f.cutoff_reg(), f.resonance(), f.mode() & 0x70), (0, 0, 0));
    frame(&mut p); // row 1: instrument 1's table
    let f = p.chip().filter();
    assert_eq!(
        (f.cutoff_reg(), f.resonance(), f.mode() & 0x70),
        (0x120, 9, 0x20)
    );
    frame(&mut p); // row 2: instrument 2's table, heard from the next frame
    let mut cutoffs = Vec::new();
    for _ in 0..4 {
        frame(&mut p);
        let f = p.chip().filter();
        cutoffs.push((f.cutoff_reg(), f.resonance(), f.mode() & 0x70));
    }
    // The frame after row 2's: the mode row (LP, res 10) and the cutoff-set row after it
    // together (GT combines them on one frame, gplay.c:271-275; S5.16 -- this
    // used to be pinned a frame apart), so 0x200; then 0x208, 0x210; then the
    // table has ended and the cutoff holds.
    assert_eq!(
        cutoffs,
        vec![
            (0x200, 10, 0x10),
            (0x208, 10, 0x10),
            (0x210, 10, 0x10),
            (0x210, 10, 0x10)
        ]
    );
}

#[test]
fn key_off_releases_and_hard_restart_gates_off_before_the_next_note() {
    let mut i = ins(0x10);
    i.gate_timer = 2;
    i.hard_restart = true;
    i.first_wave = 0x09;
    i.wave_ptr = 1;
    let mut p0 = blank(4);
    p0.rows[0] = row(49, 1, 0, 0);
    p0.rows[1] = row(NOTE_KEY_OFF, 0, 0, 0);
    p0.rows[2] = row(NOTE_KEY_ON, 0, 0, 0);
    p0.rows[3] = row(49, 1, 0, 0);
    // Wave row 1: triangle and gate, then stop.
    let wave = vec![
        TableRow {
            left: 0x11,
            right: 0x00,
        },
        TableRow {
            left: 0xFF,
            right: 0x00,
        },
    ];
    let s = song(
        vec![p0, blank(4)],
        [vec![(0, 0, 1)], vec![(1, 0, 1)], vec![(1, 0, 1)]],
        vec![i],
        Tables {
            wave,
            ..Default::default()
        },
    );
    let mut p = player(&s);
    // Frame 0: the first-frame waveform 0x09 (test + gate), then triangle + gate.
    frame(&mut p);
    assert_eq!(p.chip().voice(0).control(), 0x09);
    frame(&mut p);
    assert_eq!(p.chip().voice(0).control(), 0x11);
    // Row 1 (frames 6-11): key off, the release stage.
    for _ in 0..5 {
        frame(&mut p);
    }
    assert_eq!(p.chip().voice(0).control(), 0x10);
    assert_eq!(p.chip().voice(0).envelope_stage(), Stage::Release);
    // Row 2 (frames 12-17): key on; tempo 6 - gate timer 2 = frame 16 clears
    // the gate (a note follows) and zeroes AD/SR.
    for _ in 0..6 {
        frame(&mut p);
    }
    assert_eq!(p.chip().voice(0).control(), 0x11);
    for _ in 0..4 {
        frame(&mut p);
    }
    assert_eq!(
        p.chip().voice(0).control(),
        0x10,
        "frame 16: hard restart gate off"
    );
    assert_eq!(p.chip().voice(0).envelope_stage(), Stage::Release);
    frame(&mut p);
    frame(&mut p);
    assert_eq!(
        p.chip().voice(0).control(),
        0x09,
        "frame 18: the new note's first frame"
    );
}

#[test]
fn portamento_and_tone_portamento_use_the_speed_table() {
    let t = |l: u8, r: u8| TableRow { left: l, right: r };
    let mut p0 = blank(3);
    p0.rows[0] = row(49, 1, 0x1, 1); // porta up at speed row 1 = 0x0100
    p0.rows[1] = row(0, 0, 0x2, 1);
    p0.rows[2] = row(50, 0, 0x3, 2); // glide to C#4 at speed row 2 = 0x0050
    let mut s = song(
        vec![p0, blank(3)],
        [vec![(0, 0, 1)], vec![(1, 0, 1)], vec![(1, 0, 1)]],
        vec![ins(0x10)],
        Tables {
            speed: vec![t(0x01, 0x00), t(0x00, 0x50)],
            ..Default::default()
        },
    );
    s.tempo = 3;
    let mut p = player(&s);
    let c4 = gt_note_freq_reg(48);
    let cs4 = gt_note_freq_reg(49);
    let freqs: Vec<u16> = (0..9)
        .map(|_| {
            frame(&mut p);
            p.channel_freq(0)
        })
        .collect();
    // Row 0: trigger (no slide on the trigger frame), +256, +256.
    // Row 1: tick 0 holds, then -256 x2. Row 2: tick 0 holds, then the glide
    // from c4 up to C#4 by 0x50 a frame. Every row's tick 0 skips the tick
    // effects (goattrk2.c:55 optimizerealtime, gplay.c:728; S5.12 — the S3
    // pin slid on tick 0 too).
    let up = c4 + 256 * 2;
    let down = up - 256 * 2;
    assert_eq!(&freqs[..6], &[c4, c4 + 256, up, up, up - 256, down]);
    assert_eq!(freqs[6], down);
    assert_eq!(freqs[7], (down + 0x50).min(cs4));
    assert_eq!(freqs[8], (down + 2 * 0x50).min(cs4));
    assert_eq!(
        p.channel_note(0),
        49,
        "the glide's note is the channel's note"
    );
}

#[test]
fn instrument_vibrato_waits_then_swings_around_the_note() {
    let t = |l: u8, r: u8| TableRow { left: l, right: r };
    let mut i = ins(0x10);
    i.speed_ptr = 1;
    i.vibrato_delay = 2;
    let mut p0 = blank(8);
    p0.rows[0] = row(58, 1, 0, 0);
    let s = song(
        vec![p0, blank(8)],
        [vec![(0, 0, 1)], vec![(1, 0, 1)], vec![(1, 0, 1)]],
        vec![i],
        Tables {
            speed: vec![t(4, 10)],
            ..Default::default()
        },
    );
    let mut p = player(&s);
    let a4 = gt_note_freq_reg(57) as i32;
    let regs: Vec<i32> = (0..12)
        .map(|_| {
            frame(&mut p);
            p.chip().voice(0).frequency() as i32 - a4
        })
        .collect();
    // S5.10, GoatTracker's instrument vibrato (gplay.c:767-800, the
    // `tests_s510.rs` oracle): trigger frame; delay 2 counts down once (frame
    // 1) and swings from frame 2, at 1; turn value 4 gives a first swing of
    // 4/2 + 1 = 3 frames up, then 6 each way; frame 6 is tick 0 and holds.
    // (S3 pinned its own model: 2 delay frames, 4-frame half-swings.)
    assert_eq!(regs, vec![0, 0, 10, 20, 30, 20, 20, 10, 0, -10, -20, -30]);
}

#[test]
fn the_songs_chip_model_is_the_players_and_both_models_play() {
    let mut s = solo_a4();
    s.model = SidModel::Sid6581;
    let mut p6581 = player(&s);
    assert_eq!(p6581.chip().model(), SidModel::Sid6581);
    let mut p8580 =
        SidSongPlayer::with_model(via_file(&s), SidModel::Sid8580, DEFAULT_SAMPLE_RATE, 0).unwrap();
    let mut a = vec![0.0f32; 8820];
    let mut b = vec![0.0f32; 8820];
    p6581.render(&mut a);
    p8580.render(&mut b);
    let rms =
        |x: &[f32]| (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
    assert!(rms(&a) > 0.01 && rms(&b) > 0.01, "both audible");
    assert!(
        a.iter().zip(&b).any(|(x, y)| (x - y).abs() > 1e-3),
        "the models differ"
    );
}
