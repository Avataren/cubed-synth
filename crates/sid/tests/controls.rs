//! The player's control hooks: seek by place, loops, mute and solo, instrument
//! auditions, the position and the peaks (and the probe that shares them), and
//! live edits. The sound itself is the player's (`src/tests*.rs` pin it); these
//! check that the controls move the right things and nothing else.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cubed_sid::edit::SidDoc;
use cubed_sid::song::{LiveEdit, SidSong};
use cubed_sid::{SeekError, SidLoop, SidProbe, SidSongPlayer, SidStatus};
use serde_json::{json, Value};

const RATE: f64 = 44_100.0;

fn ok(d: &mut SidDoc, op: Value) {
    d.apply(&op).unwrap_or_else(|e| panic!("{op}: {e}"));
}

/// Three voices in step: each plays two 16-row patterns (8 rows each time through
/// the first), a note every 4 rows; voice 2's second pattern speeds the tempo up.
/// Instrument 1 sustains (a plain pulse), instrument 2 is a wave-table arpeggio.
fn song() -> SidSong {
    let mut d = SidDoc::new();
    ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 1, "decay": 0, "sustain": 15, "release": 4}),
    );
    ok(
        &mut d,
        json!({"op": "add_instrument", "name": "arp", "tables": false}),
    );
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_instrument", "instrument": 2, "decay": 0, "sustain": 15, "first_wave": 0x41, "wave_ptr": 1, "pulse_ptr": 1},
            {"op": "set_table_row", "table": "wave", "row": 2, "left": 0x41, "right": 0},
            {"op": "set_table_row", "table": "wave", "row": 3, "left": 0x41, "right": 4},
            {"op": "set_table_row", "table": "wave", "row": 4, "left": 0x41, "right": 7},
            {"op": "set_table_row", "table": "wave", "row": 5, "left": 0xFF, "right": 3},
        ]}),
    );
    // wave: 1-2 instrument 1's, 3.. the arpeggio at pointer 3 (instrument 2).
    ok(
        &mut d,
        json!({"op": "set_instrument", "instrument": 2, "wave_ptr": 3}),
    );
    for v in 0..3 {
        ok(&mut d, json!({"op": "new_pattern", "rows": 16}));
        ok(
            &mut d,
            json!({"op": "set_pattern_rows", "pattern": v, "rows": 16}),
        );
        ok(
            &mut d,
            json!({"op": "insert_order", "voice": v, "index": 1, "pattern": 3 + v}),
        );
        for (r, note) in [(0, 49), (4, 53), (8, 56), (12, 61)] {
            ok(
                &mut d,
                json!({"op": "set_cell", "pattern": v, "row": r, "cell": [note + 3 * v, 1, 0, 0]}),
            );
            ok(
                &mut d,
                json!({"op": "set_cell", "pattern": 3 + v, "row": r, "cell": [note + 12 + 3 * v, if v == 1 { 2 } else { 1 }, 0, 0]}),
            );
        }
    }
    // A tempo change on the second pattern's first row: 4 frames a row.
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 3, "row": 0, "command": 15, "param": 4}),
    );
    // A second subsong: one 8-row pattern per voice.
    ok(&mut d, json!({"op": "add_subsong", "rows": 8}));
    ok(
        &mut d,
        json!({"op": "set_cell", "pattern": 6, "row": 2, "cell": [60, 1, 0, 0]}),
    );
    d.song().clone()
}

/// The song without what a player does not read (the texts, the instruments' names).
fn unplayed(s: &SidSong) -> SidSong {
    let mut s = s.clone();
    s.name.clear();
    s.author.clear();
    s.copyright.clear();
    for i in s.instruments.iter_mut() {
        i.name.clear();
    }
    s
}

fn player(s: &SidSong) -> SidSongPlayer {
    SidSongPlayer::new(s.clone(), RATE).unwrap()
}

/// What the display shows, without the counters that differ between two routes to a place.
fn place(s: &SidStatus) -> (u8, [u8; 3], [u8; 3], [u8; 3], [u8; 3], u8) {
    (s.subsong, s.order, s.pattern, s.row, s.tick, s.tempo)
}

fn regs(p: &SidSongPlayer) -> Vec<(u16, u16, u8)> {
    (0..3)
        .map(|v| {
            let c = p.chip().voice(v);
            (c.frequency(), c.pulse_width(), c.control())
        })
        .collect()
}

/// Frames until voice `voice` has just read row `row` of order position `order`.
fn play_to(p: &mut SidSongPlayer, voice: usize, order: u8, row: u8) {
    for _ in 0..100_000 {
        p.frame();
        let s = p.status();
        if s.order[voice] == order && s.row[voice] == row && s.tick[voice] == 0 {
            return;
        }
    }
    panic!("never reached order {order} row {row}");
}

#[test]
fn status_follows_the_rows_frame_by_frame() {
    let mut p = player(&song());
    let s0 = p.status();
    assert_eq!((s0.order, s0.row, s0.tick), ([0; 3], [0; 3], [0; 3]));
    assert_eq!((s0.pattern, s0.tempo, s0.subsong), ([0, 1, 2], 6, 0));
    // Tempo 6: row 0 on frame 1 (tick 0), rows last 6 frames.
    for frame in 1..=14u32 {
        p.frame();
        let s = p.status();
        let (row, tick) = ((frame - 1) / 6, (frame - 1) % 6);
        assert_eq!(
            (s.row[0] as u32, s.tick[0] as u32),
            (row, tick),
            "frame {frame}"
        );
        assert_eq!(s.row, [s.row[0]; 3], "the voices run in step");
        assert_eq!(s.frames, frame as u64);
    }
    // Voice 1 of pattern 4 (order 1) plays instrument 2 (the arpeggio): the song's own tempo
    // command on pattern 3 (voice 0) takes every voice to 4 frames a row.
    play_to(&mut p, 0, 1, 1);
    let s = p.status();
    assert_eq!((s.order, s.pattern), ([1; 3], [3, 4, 5]));
    assert_eq!(s.tempo, 4);
    assert_eq!(s.song_rows, 32);
    assert_eq!(s.song_row, 17);
    assert!(!s.preview && !s.looping && !s.looped);
}

#[test]
fn seek_lands_where_the_song_has_the_place() {
    let s = song();
    // Natural play to voice 0 reading row 5 of order 1.
    let mut natural = player(&s);
    play_to(&mut natural, 0, 1, 5);
    // Seeking there, from somewhere else, then one frame, gives the same moment.
    let mut p = player(&s);
    for _ in 0..40 {
        p.frame();
    }
    p.seek(0, 0, 1, 5).unwrap();
    p.frame();
    assert_eq!(place(&p.status()), place(&natural.status()));
    assert_eq!(
        p.status().tempo,
        4,
        "the tempo the song reaches there, not the start's"
    );
    assert_eq!(p.status().song_row, natural.status().song_row);
    assert_eq!(regs(&p), regs(&natural));
    // The rest of the song from there on is the same too.
    for f in 0..200 {
        p.frame();
        natural.frame();
        assert_eq!(place(&p.status()), place(&natural.status()), "frame {f}");
        assert_eq!(regs(&p), regs(&natural), "frame {f}");
    }
    // Seeking by another voice's place puts the others where the song has them.
    p.seek(0, 2, 0, 9).unwrap();
    let mut by_two = player(&s);
    play_to(&mut by_two, 2, 0, 9);
    p.frame();
    assert_eq!(place(&p.status()), place(&by_two.status()));
    // The start, and the second subsong.
    p.seek(0, 0, 0, 0).unwrap();
    p.frame();
    assert_eq!(place(&p.status()), place(&player(&s).tap_frame()));
    p.seek(1, 0, 0, 2).unwrap();
    p.frame();
    let st = p.status();
    assert_eq!(
        (st.subsong, st.order[0], st.row[0], st.pattern),
        (1, 0, 2, [6, 7, 8])
    );
    assert_eq!(st.song_rows, 8);
    p.seek(0, 0, 0, 0).unwrap();
    assert_eq!(p.subsong(), 0);
}

/// One frame of a fresh player.
trait TapFrame {
    fn tap_frame(self) -> SidStatus;
}
impl TapFrame for SidSongPlayer {
    fn tap_frame(mut self) -> SidStatus {
        self.frame();
        self.status()
    }
}

#[test]
fn a_seek_to_a_place_that_is_not_there_moves_nothing() {
    let s = song();
    let mut p = player(&s);
    for _ in 0..30 {
        p.frame();
    }
    let before = (p.status(), regs(&p), p.song_row());
    for (sub, voice, order, row) in [
        (2, 0, 0, 0),
        (0, 3, 0, 0),
        (0, 0, 2, 0),
        (0, 0, 0, 16),
        (1, 0, 0, 8),
        (0, 0, 1, 99),
    ] {
        assert_eq!(
            p.seek(sub, voice, order, row),
            Err(SeekError::NoSuchPlace),
            "{sub} {voice} {order} {row}"
        );
    }
    assert_eq!((p.status(), regs(&p), p.song_row()), before);
    // Too far: the replay gives up and puts everything back.
    p.set_seek_limit(100);
    assert_eq!(p.seek(0, 0, 1, 15), Err(SeekError::TooFar));
    assert_eq!((p.status(), regs(&p), p.song_row()), before);
    p.frame();
    p.set_seek_limit(cubed_sid::SEEK_MAX_FRAMES);
    p.seek(0, 0, 1, 15).unwrap();
}

#[test]
fn a_seek_keeps_the_chip_and_the_masks() {
    let mut p = player(&song());
    let mut out = vec![0.0f32; 20_000];
    p.render(&mut out);
    let cycles = p.chip().cycles();
    p.set_mute_solo(0b001, 0);
    p.seek(0, 0, 1, 3).unwrap();
    assert_eq!(p.chip().cycles(), cycles);
    assert_eq!(p.chip().voice_mask(), 0b110);
    assert_eq!(p.mute_solo(), (1, 0));
    // Seeking by song row is the same replay.
    p.seek_row(10);
    assert_eq!(p.song_row(), 10);
    p.frame();
    assert_eq!((p.status().order, p.status().row), ([0; 3], [10; 3]));
}

#[test]
fn a_loop_repeats_its_moment_exactly() {
    let s = song();
    let (start, end) = s.order_rows(0, 0, 1, 1).unwrap();
    assert_eq!((start, end), (16, 32));
    // Played through order position 1 (rows 16..32) from the top, looped there.
    let mut p = player(&s);
    p.set_loop(Some(SidLoop {
        voice: 0,
        start,
        end,
    }));
    assert!(p.loop_range().is_some());
    let mut trace = Vec::new();
    let mut out = vec![0.0f32; 441];
    for _ in 0..3000 {
        p.render(&mut out);
        trace.push(p.status());
    }
    assert!(
        trace.iter().all(|s| s.song_row < 32),
        "never leaves the range"
    );
    assert!(trace.iter().all(|s| s.looping));
    // The first pass plays 0..16 once; from then on the range repeats, as a period of frames.
    let after: Vec<&SidStatus> = trace.iter().skip(400).collect();
    assert!(
        after.iter().all(|s| s.order[0] == 1 && s.pattern[0] == 3),
        "stays in order position 1"
    );
    // The sequence of places repeats with a fixed period (16 rows of 4 frames).
    let places: Vec<_> = after.iter().map(|s| (s.row, s.tick, s.tempo)).collect();
    let period = places
        .iter()
        .skip(1)
        .position(|p| *p == places[0])
        .map(|i| i + 1);
    assert!(period.is_some());
    let mut seen_rows = std::collections::BTreeSet::new();
    for s in &after {
        seen_rows.insert(s.row[0]);
    }
    assert_eq!(seen_rows.len(), 16, "every row of the pattern comes round");
    // A loop set inside its range, or past it, replays to its start at the first end.
    let mut q = player(&s);
    for _ in 0..(16 * 6 + 5 * 4 + 2) {
        q.frame();
    }
    q.set_loop_rows(Some((start, end)));
    let mut lo = u64::MAX;
    for _ in 0..4000 {
        q.render(&mut out);
        lo = lo.min(q.status().song_row);
    }
    assert!(q.status().song_row >= 16 && q.status().song_row < 32);
    assert!(lo < 32);
    // The editor's way: loop an orderlist position.
    let mut r = player(&s);
    assert!(r.loop_orders(0, 1, 1));
    assert_eq!(
        r.loop_range(),
        Some(SidLoop {
            voice: 0,
            start,
            end
        })
    );
    assert!(!r.loop_orders(0, 5, 5), "no entry 5");
    assert!(!r.loop_orders(3, 0, 0), "no voice 3");
    assert_eq!(
        r.loop_range(),
        Some(SidLoop {
            voice: 0,
            start,
            end
        }),
        "a refused loop changes nothing"
    );
    // Clearing the loop lets the song play on to its end and round again.
    q.set_loop(None);
    assert!(q.loop_range().is_none());
    for _ in 0..400 {
        q.render(&mut out);
    }
    assert!(q.status().song_row >= 32 || q.status().order[0] == 0);
    // Empty and out-of-range loops are no loop.
    q.set_loop_rows(Some((5, 5)));
    assert!(q.loop_range().is_none());
    q.set_loop(Some(SidLoop {
        voice: 3,
        start: 0,
        end: 4,
    }));
    assert!(q.loop_range().is_none());
}

#[test]
fn a_loop_on_one_voice_restores_every_voice_to_the_moment() {
    // Voice 1 plays patterns of another length: its place at the loop's start is its own, and comes back.
    let mut d = SidDoc::from_song(song()).unwrap();
    ok(
        &mut d,
        json!({"op": "set_pattern_rows", "pattern": 1, "rows": 12, "force": true}),
    );
    let s = d.song().clone();
    let (start, end) = s.order_rows(0, 0, 0, 0).unwrap();
    let mut p = player(&s);
    p.set_loop(Some(SidLoop {
        voice: 0,
        start,
        end,
    }));
    let mut out = vec![0.0f32; 441];
    let mut at_start: Option<(u8, [u8; 3], [u8; 3])> = None;
    for _ in 0..600 {
        p.render(&mut out);
        let st = p.status();
        if st.song_row == 0 && st.tick[0] == 0 {
            let now = (st.order[1], st.row, st.tick);
            match at_start {
                None => at_start = Some(now),
                Some(first) => {
                    assert_eq!(now, first, "voice 1's place at the loop start comes back")
                }
            }
        }
    }
    assert!(at_start.is_some());
}

#[test]
fn mute_and_solo_pick_the_voices_that_are_heard() {
    let s = song();
    let render = |mute: u8, solo: u8| {
        let mut p = player(&s);
        p.set_mute_solo(mute, solo);
        let mut out = vec![0.0f32; 6000];
        let (mut a, mut b, mut c) = (vec![0.0f32; 6000], vec![0.0f32; 6000], vec![0.0f32; 6000]);
        p.render_taps(&mut out, [&mut a, &mut b, &mut c]);
        (p.peaks(), out, [a, b, c])
    };
    let rms =
        |x: &[f32]| (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
    let (all, mix_all, _) = render(0, 0);
    assert!(
        all.iter().all(|&p| p > 0.01),
        "all three voices sound: {all:?}"
    );
    let (m, mix_m, taps) = render(0b010, 0);
    assert!(m[1] == 0.0 && m[0] > 0.01 && m[2] > 0.01, "{m:?}");
    assert!(rms(&taps[1]) == 0.0 && rms(&taps[0]) > 0.0);
    assert!(rms(&mix_m) < rms(&mix_all));
    let (so, mix_s, _) = render(0, 0b100);
    assert!(so[0] == 0.0 && so[1] == 0.0 && so[2] > 0.01, "{so:?}");
    // A voice both soloed and muted is not heard; solo 0 is no solo.
    let (both, mix_b, _) = render(0b100, 0b100);
    assert!(
        both == [0.0; 3] && rms(&mix_b[2000..]) < 0.05 * rms(&mix_all),
        "{both:?}"
    );
    assert!(mix_s.iter().any(|&x| x.abs() > 0.01));
    // Bits above the three voices are ignored.
    let (hi, _, _) = render(0b1000, 0);
    assert!(hi.iter().all(|&p| p > 0.01));
    // The song keeps playing all three: unmuting later finds the voice where it is.
    let mut p = player(&s);
    p.set_mute_solo(0b111, 0);
    let mut out = vec![0.0f32; 441 * 20];
    p.render(&mut out);
    assert_eq!(p.peaks(), [0.0; 3]);
    p.set_mute_solo(0, 0);
    p.render(&mut out);
    assert!(p.peaks().iter().all(|&x| x > 0.0));
    assert_eq!(p.status().order, [0; 3]);
    assert_eq!(p.mute_solo(), (0, 0));
}

#[test]
fn peaks_rise_with_the_voice_and_fall_24_db_a_second() {
    let s = song();
    let mut p = player(&s);
    let mut out = vec![0.0f32; 4410];
    p.render(&mut out);
    let peak = p.peaks()[0];
    assert!(peak > 0.01);
    // The peak is what the voice's tap reached (a block's loudest sample, at least).
    let mut q = player(&s);
    let (mut a, mut b, mut c) = (vec![0.0f32; 4410], vec![0.0f32; 4410], vec![0.0f32; 4410]);
    let mut mix = vec![0.0f32; 4410];
    q.render_taps(&mut mix, [&mut a, &mut b, &mut c]);
    let tap_max = a.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    // (It holds the loudest sample of the block, less what fell since: at most 2.4 dB in 0.1 s.)
    assert!(
        q.peaks()[0] >= tap_max * 0.75 && q.peaks()[0] <= tap_max * 1.01,
        "{} vs {tap_max}",
        q.peaks()[0]
    );
    assert_eq!(q.peaks(), p.peaks(), "with or without the caller's taps");
    // Silence the voice: its peak falls 24 dB (x 0.0631) in a second.
    p.set_mute_solo(0b001, 0);
    let before = p.peaks()[0];
    let mut second = vec![0.0f32; RATE as usize];
    p.render(&mut second);
    let after = p.peaks()[0];
    let ratio = after / before;
    assert!((ratio - 0.0631).abs() < 0.004, "fell by {ratio}");
}

#[test]
fn an_audition_plays_an_instrument_without_a_song() {
    let s = song();
    let mut p = player(&s);
    p.set_preview(true);
    assert!(p.status().preview);
    assert!(!p.audition_on(3, 1, 48), "no voice 4");
    assert!(!p.audition_on(0, 9, 48), "no instrument 9");
    assert!(!p.audition_on(0, 0, 48), "no instrument 0");
    // A chord of instrument 1 on voices 0 and 2 and the arpeggio instrument on voice 1.
    assert!(p.audition_on(0, 1, 48));
    assert!(p.audition_on(2, 1, 55));
    assert!(p.audition_on(1, 2, 40));
    assert_eq!(p.auditioning(), 0b111);
    let mut seen = std::collections::BTreeSet::new();
    let mut out = vec![0.0f32; 4410];
    for _ in 0..30 {
        p.render(&mut out);
        seen.insert(p.chip().voice(1).frequency());
    }
    let gt = cubed_sid::gt_note_freq_reg;
    assert_eq!(p.chip().voice(0).frequency(), gt(48));
    assert_eq!(p.chip().voice(2).frequency(), gt(55));
    // The wave table runs: root, +4, +7 on voice 1.
    for n in [40, 44, 47] {
        assert!(
            seen.contains(&gt(n)),
            "the arpeggio reaches note {n}: {seen:?}"
        );
    }
    assert!(p.peaks().iter().all(|&x| x > 0.01), "{:?}", p.peaks());
    // No song plays: the sequencer is off.
    assert_eq!(p.status().song_row, 0);
    // Release: the gate goes and the sound dies away (release 4).
    p.audition_off(0);
    p.audition_off(1);
    p.audition_off(2);
    assert_eq!(p.auditioning(), 0);
    for _ in 0..40 {
        p.render(&mut out);
    }
    assert!(p.peaks().iter().all(|&x| x < 0.002), "{:?}", p.peaks());
    // Over a playing song the voice is taken, and given back at its next row.
    let mut q = player(&s);
    for _ in 0..8 {
        q.frame();
    }
    assert!(q.audition_on(1, 1, 30));
    for _ in 0..40 {
        q.frame();
    }
    assert_eq!(
        q.chip().voice(1).frequency(),
        gt(30),
        "its rows are not read"
    );
    assert_ne!(q.chip().voice(0).frequency(), gt(30));
    assert_eq!(
        q.status().row[1],
        q.status().row[0],
        "the row counter runs on"
    );
    q.audition_off(1);
    for _ in 0..80 {
        q.frame();
    }
    assert_ne!(
        q.chip().voice(1).frequency(),
        gt(30),
        "the song has the voice back"
    );
    // Leaving preview mode gives every voice back too.
    p.set_preview(false);
    assert!(!p.status().preview);
}

#[test]
fn the_probe_shares_the_status_and_the_peaks_with_other_threads() {
    let s = song();
    let mut p = player(&s);
    let probe = SidProbe::new();
    p.attach_probe(probe.clone());
    let mut out = vec![0.0f32; 441 * 7];
    p.render(&mut out);
    let (mine, theirs) = (p.status(), probe.status());
    assert_eq!(place(&mine), place(&theirs));
    assert_eq!(
        (mine.song_row, mine.song_rows, mine.frames),
        (theirs.song_row, theirs.song_rows, theirs.frames)
    );
    assert_eq!(probe.levels(), p.peaks());
    let v = probe.status().version;
    assert!(v > 0);
    p.set_mute_solo(0b010, 0b011);
    p.render(&mut out);
    assert_eq!((probe.status().mute, probe.status().solo), (0b010, 0b011));
    assert_ne!(probe.status().version, v, "anything moved: a new version");
    // A reader on another thread never sees a torn frame: the voices of this song run in step.
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (probe, stop) = (probe.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 0u32;
            while !stop.load(Ordering::Relaxed) {
                let s = probe.status();
                assert_eq!(s.row[0], s.row[1], "torn read: {s:?}");
                assert_eq!(s.row[1], s.row[2], "torn read: {s:?}");
                assert_eq!(s.tick[0], s.tick[2], "torn read: {s:?}");
                n += 1;
            }
            n
        })
    };
    let mut block = vec![0.0f32; 128];
    for _ in 0..4000 {
        p.render(&mut block);
    }
    stop.store(true, Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0);
}

#[test]
fn live_edits_reach_a_playing_song_as_if_it_had_been_opened_with_them() {
    let base = song();
    let mut d = SidDoc::from_song(base.clone()).unwrap();
    let mut p = player(&base);
    let mut out = vec![0.0f32; 441 * 3];
    for _ in 0..5 {
        p.render(&mut out);
    }
    d.clear_history();
    // Edits to what has not been read yet: a row far ahead, a pulse row and an
    // instrument that the next notes read, the chip.
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_cell", "pattern": 3, "row": 8, "cell": [70, 1, 4, 0]},
            {"op": "set_cell", "pattern": 0, "row": 12, "cell": [0, 0, 0xC, 0x20]},
            {"op": "set_instrument", "instrument": 2, "attack": 3, "release": 9},
            {"op": "set_table_row", "table": "wave", "row": 3, "left": 0x41, "right": 5},
        ]}),
    );
    let edits = d.live_edits().expect("a live edit");
    assert_eq!(edits.len(), 4);
    for e in &edits {
        assert!(p.apply_live(e), "{e:?}");
    }
    // The played song now has the edited data, apart from names.
    assert_eq!(unplayed(p.song()), unplayed(d.song()));
    // It sounds as a player opened with the edits does: both are at frame 5 of the edited song.
    let mut fresh = SidSongPlayer::new(d.song().clone(), RATE).unwrap();
    for _ in 0..5 {
        fresh.render(&mut out);
    }
    let (mut a, mut b) = (vec![0.0f32; 441 * 300], vec![0.0f32; 441 * 300]);
    p.render(&mut a);
    fresh.render(&mut b);
    assert!(
        a == b,
        "the live-edited song sounds as the one opened with the edits"
    );
    // The chip is switched in place; texts are no play.
    ok(&mut d, json!({"op": "set_model", "model": "8580"}));
    for e in d.live_edits().unwrap() {
        assert!(p.apply_live(&e));
    }
    assert_eq!(p.chip().model(), cubed_sid::SidModel::Sid8580);
    // Refusals: nothing that does not exist, nothing the song cannot hold.
    use cubed_sid::song::{Row, TableKind, TableRow};
    let bad = Row {
        instrument: 9,
        ..Row::default()
    };
    for e in [
        LiveEdit::Row {
            pattern: 99,
            row: 0,
            cell: Row::default(),
        },
        LiveEdit::Row {
            pattern: 0,
            row: 99,
            cell: Row::default(),
        },
        LiveEdit::Row {
            pattern: 0,
            row: 0,
            cell: bad,
        },
        LiveEdit::Row {
            pattern: 0,
            row: 0,
            cell: Row {
                command: 16,
                ..Row::default()
            },
        },
        LiveEdit::TableRow {
            table: TableKind::Wave,
            row: 99,
            value: TableRow::default(),
        },
        LiveEdit::Instrument {
            number: 0,
            instrument: Default::default(),
        },
        LiveEdit::Instrument {
            number: 9,
            instrument: Default::default(),
        },
        LiveEdit::Instrument {
            number: 1,
            instrument: cubed_sid::song::Instrument {
                wave_ptr: 99,
                ..Default::default()
            },
        },
    ] {
        assert!(!p.apply_live(&e), "{e:?}");
    }
    // Edits that move the song's shape give no live edits: the host rebuilds at the place.
    ok(
        &mut d,
        json!({"op": "insert_order", "voice": 0, "index": 1, "pattern": 1}),
    );
    assert!(d.live_edits().is_none());
    let at = p.status();
    let mut rebuilt = SidSongPlayer::new(d.song().clone(), RATE).unwrap();
    rebuilt
        .seek(
            at.subsong as usize,
            0,
            at.order[0] as usize,
            at.row[0] as usize,
        )
        .unwrap();
    rebuilt.frame();
    assert_eq!(rebuilt.status().row[0], at.row[0]);
}

#[test]
fn a_player_kept_in_step_with_a_document_through_random_edits() {
    // The host's loop: after every change, live edits where there are some, else a new player at the old place.
    let mut d = SidDoc::from_song(song()).unwrap();
    let mut p = player(d.song());
    let mut out = vec![0.0f32; 441];
    let mut seed = 7u64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let (mut live, mut rebuilt) = (0, 0);
    for step in 0..300 {
        for _ in 0..next(4) {
            p.render(&mut out);
        }
        let s = d.song();
        let (pat, rows) = (next(s.patterns.len()), s.patterns[0].rows.len());
        let op = match next(12) {
            0..=3 => {
                json!({"op": "set_cell", "pattern": pat, "row": next(s.patterns[pat].rows.len()), "cell": [1 + next(93), next(s.instruments.len() + 1), next(16), next(256)]})
            }
            4 => {
                json!({"op": "clear_range", "pattern": pat, "row0": 0, "row1": rows.min(s.patterns[pat].rows.len()) - 1, "columns": ["note"]})
            }
            5 => json!({"op": "insert_row", "pattern": pat, "row": 0, "count": 1 + next(3)}),
            6 => {
                json!({"op": "set_instrument", "instrument": 1 + next(s.instruments.len()), "attack": next(16), "release": next(16), "vibrato_delay": next(9)})
            }
            7 => {
                json!({"op": "set_table_row", "table": (["wave", "pulse"][next(2)]), "row": 0, "left": next(256), "right": next(256)})
            }
            8 => json!({"op": "set_model", "model": (["6581", "8580"][next(2)])}),
            9 => json!({"op": "undo"}),
            10 => {
                json!({"op": "insert_order", "voice": next(3), "index": 0, "pattern": next(s.patterns.len())})
            }
            _ => json!({"op": "set_title", "title": "t"}),
        };
        if d.apply(&op).is_err() {
            continue;
        }
        match d.live_edits() {
            Some(edits) => {
                live += 1;
                for e in &edits {
                    assert!(p.apply_live(e), "step {step}: {op}: {e:?}");
                }
            }
            None => {
                rebuilt += 1;
                let at = p.status();
                p = SidSongPlayer::new(d.song().clone(), RATE).unwrap();
                // Back at the old place when it is still there.
                let _ = p.seek(
                    at.subsong as usize,
                    0,
                    at.order[0] as usize,
                    at.row[0] as usize,
                );
            }
        }
        assert_eq!(unplayed(p.song()), unplayed(d.song()), "step {step}: {op}");
    }
    assert!(live > 50 && rebuilt > 5, "{live} live, {rebuilt} rebuilt");
}

#[test]
fn a_baked_tempo_plays_as_the_start_tempo_did() {
    // tempo 5 at 2x: 10 frames a row from the start; baked, an F command says the same.
    let mut d = SidDoc::from_song(song()).unwrap();
    ok(
        &mut d,
        json!({"op": "batch", "ops": [
            {"op": "set_speed_multiplier", "multiplier": 2},
            {"op": "set_tempo", "tempo": 5},
            {"op": "set_instrument", "instrument": 1, "gate_timer": 3},
            {"op": "set_instrument", "instrument": 2, "gate_timer": 3},
        ]}),
    );
    let before = d.song().clone();
    ok(&mut d, json!({"op": "bake_tempo"}));
    let after = d.song().clone();
    assert_eq!(after.tempo, 6);
    let (mut a, mut b) = (player(&before), player(&after));
    let mut compared = 0;
    // The first pass: once a voice wraps, the baked F command sets the tempo again on row 0, as
    // GoatTracker plays a song whose tempo is in its first row (the start tempo is not set again).
    for f in 0..400 {
        a.frame();
        b.frame();
        if a.status().looped {
            break;
        }
        compared += 1;
        let (sa, sb) = (a.status(), b.status());
        assert_eq!(
            (sa.row, sa.tick, sa.order),
            (sb.row, sb.tick, sb.order),
            "frame {f}"
        );
        assert_eq!(regs(&a), regs(&b), "frame {f}");
    }
    assert!(compared > 200, "{compared} frames compared");
}
