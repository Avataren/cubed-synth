//! S5.17 tests: what GoatTracker does to the envelope registers that the
//! player did not, found on voice 3 of "Coconut Conundrum" (Stinsen), whose
//! release sounded much shorter here than in GT.
//!
//! Voice 3 plays an instrument (AD 00, SR 77) with a hard restart and the wave
//! table `21 | 41 | 00 | 21 | F6 60 | FF`. Two things differed from GT's
//! playroutine, register by register (frame-aligned, 1895 frames):
//!   - the hard restart wrote AD 0x00; GT writes `adparam>>8` = 0x0F
//!     (gplay.c:929). With decay 15 the rate period is huge while the gate is
//!     still on, the counter is past the release period when the gate closes,
//!     and the ADSR delay bug holds the note for ~33 ms before it releases: a
//!     tail 2 frames long in GT, gone in ~6 ms here;
//!   - wave-table command $F6 (set SR, gplay.c:647) was skipped: GT drops the
//!     sustain from 7 to 6 on the last frame, the whole remaining 1.17x (7/6)
//!     level gap in the tail. $F5 (set AD, gplay.c:643) likewise.
//!
//! Voice-3 RMS after both, against GT's own playroutine on reSID: ratios 0.98
//! (6581) and 0.99 (8580) overall, the tail within 1% (was 0.10 at 20-40 ms).

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

fn row(note: u8, instrument: u8) -> Row {
    Row {
        note,
        instrument,
        command: 0,
        param: 0,
    }
}

/// Voice 1's envelope level at the end of each of the first `frames` frames
/// of `rows` (tempo 6) played with `ins` and the wave table `wave`.
fn levels(ins: Instrument, wave: Vec<TableRow>, rows: &[(usize, u8)], frames: usize) -> Vec<u8> {
    let n = 16;
    let list = |pattern: u8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose: 0,
            repeat: 1,
        }],
        restart: 0,
    };
    let mut r = vec![Row::default(); n];
    for &(at, note) in rows {
        r[at] = row(note, 1);
    }
    let s = SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid6581,
        channels: 3,
        speed_multiplier: 1,
        tempo: 6,
        name: b"s517".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![
            Pattern { rows: r },
            Pattern {
                rows: vec![Row::default(); n],
            },
        ],
        instruments: vec![ins],
        tables: Tables {
            wave,
            ..Default::default()
        },
    };
    let s = SidSong::parse(&s.to_bytes()).expect("parses");
    let mut p = SidSongPlayer::new(s, DEFAULT_SAMPLE_RATE).expect("player builds");
    let mut out = vec![0.0f32; SPF];
    (0..frames)
        .map(|_| {
            let n = p.samples_in_next_frame();
            p.render(&mut out[..n]);
            p.chip().voice(0).envelope_level()
        })
        .collect()
}

fn base() -> Instrument {
    Instrument {
        name: b"s517".to_vec(),
        sustain: 15,
        release: 7,
        first_wave: 0x09,
        wave_ptr: 1,
        ..Default::default()
    }
}

#[test]
fn a_hard_restart_holds_the_note_for_the_adsr_delay_before_it_releases() {
    // Row 0 and row 1 (frame 6) each play a note; the hard restart is 2 frames
    // before the second: the gate closes on frame 4 with AD 0x0F, SR 0x00. The
    // envelope must still be at the top at the end of that frame (~15 ms after
    // the gate: the counter runs on to 0x8000, ~33 ms) and gone by the end of
    // frame 6's start; with AD 0x00 it had released within ~6 ms.
    let ins = Instrument {
        gate_timer: 2,
        hard_restart: true,
        ..base()
    };
    let l = levels(
        ins,
        vec![t(0x41, 0x00), t(0xFF, 0x00)],
        &[(0, 49), (1, 49)],
        6,
    );
    assert_eq!(l[3], 255, "frame 3: sustaining, levels {l:?}");
    assert!(
        l[4] >= 200,
        "frame 4: the gate is closed but the release has not started, levels {l:?}"
    );
    assert!(
        l[5] < 20,
        "frame 5: the hold is over and the release (rate 0) has run, levels {l:?}"
    );
}

#[test]
fn wave_command_f6_sets_the_sustain() {
    // Rows: pulse, F6 with SR 0x60 (sustain 6 = level 0x66), stop. The
    // instrument's own sustain is 15; once the command has run (frame 2) the
    // envelope decays to the new level and stays there.
    let l = levels(
        base(),
        vec![t(0x41, 0x00), t(0xF6, 0x60), t(0xFF, 0x00)],
        &[(0, 49)],
        6,
    );
    assert_eq!(
        l[1], 255,
        "frame 1: still at the instrument's sustain, levels {l:?}"
    );
    assert_eq!(l[5], 0x66, "frame 5: sustain 6 after $F6, levels {l:?}");
}

#[test]
fn wave_command_f5_sets_the_attack_and_decay() {
    // Attack 9 (250 ms) is far from done at frame 2 (~50 ms); $F5 with AD 0x00
    // there speeds it to 2 ms, so the envelope is at the top by frame 4. The
    // same song without the command is still climbing.
    let slow = Instrument {
        attack: 9,
        ..base()
    };
    let with = levels(
        slow.clone(),
        vec![t(0x41, 0x00), t(0xF5, 0x00), t(0xFF, 0x00)],
        &[(0, 49)],
        5,
    );
    let without = levels(slow, vec![t(0x41, 0x00), t(0xFF, 0x00)], &[(0, 49)], 5);
    assert_eq!(
        with[4], 255,
        "with $F5 the attack finishes, levels {with:?}"
    );
    assert!(
        without[4] < 200,
        "without it the attack is still running, levels {without:?}"
    );
}

#[test]
fn a_wave_command_leaves_the_channels_note_alone() {
    // $F6's right column is a parameter, not a note (gplay.c:711-712): the
    // pitch register after it is the note's, not note + 0x60.
    let ins = base();
    let n = 16;
    let list = |pattern: u8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose: 0,
            repeat: 1,
        }],
        restart: 0,
    };
    let mut r = vec![Row::default(); n];
    r[0] = row(49, 1);
    let s = SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid6581,
        channels: 3,
        speed_multiplier: 1,
        tempo: 6,
        name: b"s517n".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![
            Pattern { rows: r },
            Pattern {
                rows: vec![Row::default(); n],
            },
        ],
        instruments: vec![ins],
        tables: Tables {
            wave: vec![t(0x41, 0x00), t(0xF6, 0x60), t(0xFF, 0x00)],
            ..Default::default()
        },
    };
    let s = SidSong::parse(&s.to_bytes()).expect("parses");
    let mut p = SidSongPlayer::new(s, DEFAULT_SAMPLE_RATE).expect("player builds");
    let mut out = vec![0.0f32; SPF];
    let mut freqs = Vec::new();
    for _ in 0..5 {
        let n = p.samples_in_next_frame();
        p.render(&mut out[..n]);
        freqs.push(p.chip().voice(0).frequency());
    }
    assert_eq!(
        freqs[1], freqs[4],
        "the pitch does not move under $F6, freqs {freqs:?}"
    );
}
