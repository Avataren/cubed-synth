//! S5 tests: player pins the GoatTracker `.sng` corpus forced
//! (`.ai/sid-import-dlog.md`). Expected values come from GT's readme and the
//! note table, never from a run, as in `tests_s3.rs`.

use super::player::SidSongPlayer;
use super::song::*;
use super::*;

/// One PAL frame at 44.1 kHz is 879.8 samples (`player::frame_cycles`, GT
/// parity 0925b): a buffer that holds one; `samples_in_next_frame` says how
/// much of it a frame is.
const SPF: usize = 880;

fn row(note: u8, instrument: u8, command: u8, param: u8) -> Row {
    Row {
        note,
        instrument,
        command,
        param,
    }
}

fn tie_song(param: u8) -> SidSong {
    let mut p0 = Pattern {
        rows: vec![Row::default(); 2],
    };
    // Row 0: C-4 (note 49 = index 48) triggers; row 1: E-4 (index 52) with 3XY.
    p0.rows[0] = row(49, 1, 0, 0);
    p0.rows[1] = row(53, 0, 0x3, param);
    let blank = Pattern {
        rows: vec![Row::default(); 2],
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
        tempo: 3,
        name: b"s5".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![p0, blank],
        instruments: vec![Instrument {
            name: b"t".to_vec(),
            sustain: 15,
            first_wave: 0x09,
            wave_ptr: 1,
            ..Default::default()
        }],
        // Wave row 1: triangle and gate, then stop. Speed row 1: 0x0010 a
        // frame, for the gliding control.
        tables: Tables {
            wave: vec![
                TableRow {
                    left: 0x11,
                    right: 0x00,
                },
                TableRow {
                    left: 0xFF,
                    right: 0x00,
                },
            ],
            speed: vec![TableRow {
                left: 0x00,
                right: 0x10,
            }],
            ..Default::default()
        },
    };
    // Through the file bytes, as every app song arrives.
    SidSong::parse(&song.to_bytes()).expect("parses")
}

fn frame(p: &mut SidSongPlayer) {
    let mut out = vec![0.0f32; SPF];
    let n = p.samples_in_next_frame();
    p.render(&mut out[..n]);
}

#[test]
fn tone_portamento_zero_is_gt_tie_note_the_pitch_jumps_without_a_trigger() {
    // GT readme §3.2: "3XY ... or $00 for "tie-note" effect (move pitch
    // instantly to target note)". Tempo 3: row 1 starts at frame 3. S5.6
    // phase correction: GT's realtime optimisation (goattrk2.c:55,
    // gplay.c:728) skips the tick-0 effects, so the instant jump
    // (gplay.c:807-811) lands on the row's SECOND frame (frame 4) — still no
    // retrigger and no glide.
    let mut p = SidSongPlayer::new(tie_song(0), DEFAULT_SAMPLE_RATE).expect("player builds");
    for _ in 0..3 {
        frame(&mut p);
    }
    assert_eq!(p.channel_freq(0), gt_note_freq_reg(48));
    frame(&mut p);
    // Frame 3 reads row 1 (tick 0): the tick-0 effects are skipped
    // (gplay.c:728), so the pitch is STILL C-4 while the note is registered
    // (cptr->note updated at tick 0, gplay.c:350).
    assert_eq!(p.channel_freq(0), gt_note_freq_reg(48));
    assert_eq!(p.channel_note(0), 52);
    frame(&mut p);
    // Frame 4 (tick 1): the tie jump lands — instantly, without a glide.
    assert_eq!(p.channel_freq(0), gt_note_freq_reg(52));
    assert_eq!(p.channel_note(0), 52);
    // No trigger: not the first-frame waveform (0x09), the held triangle with the gate.
    assert_eq!(p.chip().voice(0).control(), 0x11);
}

#[test]
fn tone_portamento_with_a_speed_still_glides() {
    // The control: 3 01 glides by speed row 1 (0x10 a frame). A glide row
    // triggers nothing; its tick 0 (frame 3) skips the tick effects like
    // every row's (gplay.c:728, S5.12 — S5 pinned a tick-0 step here), so
    // the glide starts on tick 1, the same phase as the `3 00` tie jump.
    let mut p = SidSongPlayer::new(tie_song(1), DEFAULT_SAMPLE_RATE).expect("player builds");
    for _ in 0..4 {
        frame(&mut p);
    }
    let c4 = gt_note_freq_reg(48);
    assert_eq!(p.channel_freq(0), c4);
    assert_eq!(p.channel_note(0), 52);
    frame(&mut p);
    assert_eq!(p.channel_freq(0), c4 + 0x10);
    frame(&mut p);
    assert_eq!(p.channel_freq(0), c4 + 0x20);
}

#[test]
fn a_delayed_wave_step_waits_then_sets_its_note_the_readme_minor_chord() {
    // GT readme §3.4.1: "21 00 | 02 03 | 02 07 | 02 00 | FF 02 — A delayed
    // minor chord arpeggio with sawtooth waveform. Each step takes 3 ticks."
    // C-4 (index 48) triggers on frame 0, whose frame skips the wave table and
    // keeps the channel's old pitch (none) as GT does (S5.19); row 1 sets saw +
    // C-4 on frame 1; each delayed row waits 2 frames and sets its note on the
    // third (the note after the wait: INFERRED, see player.rs); the jump loops
    // to row 2.
    let t = |l: u8, r: u8| TableRow { left: l, right: r };
    let mut p0 = Pattern {
        rows: vec![Row::default(); 1],
    };
    p0.rows[0] = row(49, 1, 0, 0);
    let blank = Pattern {
        rows: vec![Row::default(); 1],
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
        tempo: 20,
        name: b"s5".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![p0, blank],
        instruments: vec![Instrument {
            name: b"t".to_vec(),
            sustain: 15,
            wave_ptr: 1,
            first_wave: 0x09,
            ..Default::default()
        }],
        tables: Tables {
            wave: vec![
                t(0x21, 0x00),
                t(0x02, 0x03),
                t(0x02, 0x07),
                t(0x02, 0x00),
                t(0xFF, 0x02),
            ],
            ..Default::default()
        },
    };
    let mut p = SidSongPlayer::new(
        SidSong::parse(&song.to_bytes()).expect("parses"),
        DEFAULT_SAMPLE_RATE,
    )
    .expect("player builds");
    let notes: Vec<u16> = (0..16)
        .map(|_| {
            frame(&mut p);
            p.channel_freq(0)
        })
        .collect();
    let [c, eb, g] = [
        gt_note_freq_reg(48),
        gt_note_freq_reg(51),
        gt_note_freq_reg(55),
    ];
    // The jump is taken on the frame the step after it starts (the player's
    // one-jump-per-frame rule), so the loop keeps 3 frames per step too.
    assert_eq!(
        notes,
        vec![0, c, c, c, eb, eb, eb, g, g, g, c, c, c, eb, eb, eb]
    );
    assert_eq!(p.chip().voice(0).control(), 0x21);
}

/// A C-4 trigger over a looping wave program (tri+gate/no-note, saw+gate/note +3,
/// jump loop), with `3 XY` on row 1 to note E-4 (index 52). `row1_param` 0 is
/// the tie, 1 the speed glide at speed row 1 (0x10 a frame, the S5 control's
/// speed). Tempo 4: row 0 = frames 0-3, row 1 = frames 4-7.
fn wave_porta_song(row1_param: u8) -> SidSong {
    let t = |l: u8, r: u8| TableRow { left: l, right: r };
    let mut p0 = Pattern {
        rows: vec![Row::default(); 2],
    };
    p0.rows[0] = row(49, 1, 0, 0);
    p0.rows[1] = row(53, 0, 0x3, row1_param);
    let blank = Pattern {
        rows: vec![Row::default(); 2],
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
        tempo: 4,
        name: b"s56".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![p0, blank],
        instruments: vec![Instrument {
            name: b"t".to_vec(),
            sustain: 15,
            wave_ptr: 1,
            // GT's default; $00 would leave the fresh channel's gate shut (S5.19).
            first_wave: 0x09,
            ..Default::default()
        }],
        tables: Tables {
            speed: vec![TableRow {
                left: 0x00,
                right: 0x10,
            }],
            wave: vec![t(0x11, 0x80), t(0x21, 0x03), t(0xFF, 0x01)],
            ..Default::default()
        },
    };
    // Through the file bytes, as every app song arrives.
    SidSong::parse(&song.to_bytes()).expect("parses")
}

#[test]
fn a_wave_table_note_passes_through_a_tie_and_the_base_pitch_is_re_asserted() {
    // GT lets wave-table note rows through under a standing `3 00`: the wave
    // note path (gplay.c:714-722) has no command check, and its frame jumps
    // past the tick effects (gplay.c:722) — the arpeggio note wins that
    // frame, and the next non-wave frame re-asserts the tie's base pitch
    // (gplay.c:802-811). The S3-era player suppressed wave notes under any
    // portamento and jumped on tick 0; both are the S5.6 corrections. The
    // wave program cycles every 2 frames (the jump row costs no frame), so
    // the saw/+3 note lands on odd frames, the tri/no-note row on even ones.
    let mut p = SidSongPlayer::new(wave_porta_song(0), DEFAULT_SAMPLE_RATE).expect("player builds");
    let [c, eb, g, base] = [
        gt_note_freq_reg(48),
        gt_note_freq_reg(51),
        gt_note_freq_reg(55),
        gt_note_freq_reg(52),
    ];
    let freqs: Vec<u16> = (0..8)
        .map(|_| {
            frame(&mut p);
            p.channel_freq(0)
        })
        .collect();
    // f0 (the note's frame): no wave table, no pitch yet (GT, S5.19); f1 the
    // no-note row, f2 the program's note 51, f3 no-note. f4 (the tie's tick
    // 0, the note now 52): the wave note 55 passes THROUGH the tie and wins
    // its frame. f5 (tick 1): the no-note row lets the tie re-assert its base
    // 52. f6: note 55 again; f7 the base.
    let _ = c;
    assert_eq!(freqs, vec![0, 0, eb, eb, g, base, g, base]);
    // Gate never drops and the wave table's own waveforms hold: no retrigger
    // under the tie.
    // (f7 is the program's triangle row.)
    assert_eq!(p.chip().voice(0).control(), 0x11);
}

#[test]
fn a_wave_table_note_passes_through_a_speed_glide_and_the_glide_resumes() {
    // The same passthrough under a real portamento — the suppression was
    // "under any portamento" (commands 1-3): the wave note sets the frequency
    // on its frame (gplay.c:714-722, the effects skipped by the gplay.c:722
    // jump) and the glide resumes from it. The glide starts on tick 1
    // (gplay.c:728; S5.12, tests_s3's portamento test and the S5 control
    // pin it). Speed row 1
    // = 0x10 a frame toward E-4 (index 52); the glide is far from clamped at
    // 0x10 steps.
    let mut p = SidSongPlayer::new(wave_porta_song(1), DEFAULT_SAMPLE_RATE).expect("player builds");
    let [c, eb, g] = [
        gt_note_freq_reg(48),
        gt_note_freq_reg(51),
        gt_note_freq_reg(55),
    ];
    let freqs: Vec<u16> = (0..8)
        .map(|_| {
            frame(&mut p);
            p.channel_freq(0)
        })
        .collect();
    // f0-f3 as in the tie test. f4 (the glide row's tick 0): the wave note 55
    // wins its frame. f5 (tick 1): the glide steps DOWN from 55 toward the
    // target 52. f6: the wave note again; f7 the glide.
    let _ = c;
    assert_eq!(freqs, vec![0, 0, eb, eb, g, g - 0x10, g, g - 0x10]);
    assert_eq!(p.chip().voice(0).control(), 0x11);
}
