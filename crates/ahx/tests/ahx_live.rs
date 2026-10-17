//! Live (keyboard preview) mode of `AhxPlayer`: one mono voice played by
//! note-on / note-off with the song's own instruments. The song player's
//! output is pinned by the goldens (`ahx_render_golden.rs`); this only covers
//! what live mode adds.

use cubed_ahx::player::AhxPlayer;
use std::fs;
use std::path::Path;

const RATE: usize = 44100;

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/songs")
            .join(name),
    )
    .unwrap()
}

fn preview_player(name: &str) -> AhxPlayer {
    let mut p = AhxPlayer::new(&fixture(name), RATE as u32, 2).unwrap();
    p.enable_preview();
    p
}

/// Renders `seconds` and returns `(peak, left, right)`.
fn render(p: &mut AhxPlayer, seconds: f32) -> (f32, Vec<f32>, Vec<f32>) {
    let frames = (seconds * RATE as f32) as usize;
    let (mut l, mut r) = (vec![0f32; frames], vec![0f32; frames]);
    for start in (0..frames).step_by(128) {
        let end = (start + 128).min(frames);
        p.render(&mut l[start..end], &mut r[start..end]);
    }
    let peak = l.iter().chain(r.iter()).fold(0f32, |m, s| m.max(s.abs()));
    (peak, l, r)
}

fn crossings(x: &[f32]) -> usize {
    x.windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count()
}

#[test]
fn a_preview_player_is_silent_until_a_note_is_played() {
    let mut p = preview_player("karma.ahx");
    assert!(p.preview_enabled());
    assert_eq!(render(&mut p, 0.3).0, 0.0);
}

#[test]
fn note_on_is_refused_outside_preview_mode_and_for_missing_instruments() {
    let mut song = AhxPlayer::new(&fixture("karma.ahx"), RATE as u32, 2).unwrap();
    assert!(!song.preview_note_on(1, 30, 127));
    let mut p = preview_player("karma.ahx");
    assert!(!p.preview_note_on(0, 30, 127));
    assert!(!p.preview_note_on(200, 30, 127));
    assert!(p.preview_note_on(1, 30, 127));
}

#[test]
fn a_held_note_sustains_and_note_off_releases_to_silence() {
    let mut p = preview_player("karma.ahx");
    let mut heard = 0;
    for instrument in 1..=8usize {
        assert!(p.preview_note_on(instrument, 30, 127));
        let (attack, ..) = render(&mut p, 0.3);
        if attack == 0.0 {
            p.preview_note_off();
            render(&mut p, 6.0);
            continue;
        }
        heard += 1;
        // Held for a good while longer than any sustain the file sets: still sounding.
        render(&mut p, 2.0);
        let (held, ..) = render(&mut p, 0.2);
        assert!(
            held > 0.0,
            "instrument {instrument} died while its key was down"
        );
        p.preview_note_off();
        // Release frames top out at 255 ticks (5.1 s at 50 Hz).
        render(&mut p, 6.0);
        let (after, ..) = render(&mut p, 0.2);
        assert_eq!(
            after, 0.0,
            "instrument {instrument} still sounds after its release"
        );
    }
    assert!(heard > 0, "no instrument made a sound");
}

#[test]
fn pitch_follows_the_note_an_octave_up_doubles_the_frequency() {
    let mut p = preview_player("karma.ahx");
    let mut checked = false;
    for instrument in 1..=8usize {
        p.preview_note_on(instrument, 25, 127);
        render(&mut p, 0.4);
        let (peak, low, _) = render(&mut p, 0.5);
        if peak == 0.0 {
            p.preview_note_off();
            render(&mut p, 6.0);
            continue;
        }
        p.preview_note_off();
        render(&mut p, 6.0);
        p.preview_note_on(instrument, 37, 127);
        render(&mut p, 0.4);
        let (_, high, _) = render(&mut p, 0.5);
        p.preview_note_off();
        render(&mut p, 6.0);
        let (lo, hi) = (crossings(&low), crossings(&high));
        // Zero crossings are a rough pitch read (filters, PList notes and
        // vibrato smear it), so only require clearly higher, not exactly 2x.
        if lo > 20 && hi > lo * 3 / 2 {
            checked = true;
            break;
        }
    }
    assert!(checked, "no instrument rose in pitch with the note");
}

#[test]
fn velocity_scales_the_level() {
    let mut p = preview_player("karma.ahx");
    for instrument in 1..=8usize {
        p.preview_note_on(instrument, 30, 127);
        let (loud, ..) = render(&mut p, 0.5);
        p.preview_note_off();
        render(&mut p, 6.0);
        if loud == 0.0 {
            continue;
        }
        p.preview_note_on(instrument, 30, 32);
        let (soft, ..) = render(&mut p, 0.5);
        assert!(
            soft < loud * 0.6,
            "velocity 32 ({soft}) not clearly quieter than 127 ({loud})"
        );
        return;
    }
    panic!("no instrument made a sound");
}

#[test]
fn the_preview_voice_is_centred() {
    let mut p = preview_player("karma.ahx");
    for instrument in 1..=8usize {
        p.preview_note_on(instrument, 30, 127);
        let (peak, l, r) = render(&mut p, 0.5);
        if peak == 0.0 {
            p.preview_note_off();
            render(&mut p, 6.0);
            continue;
        }
        assert_eq!(l, r, "a centred mono voice is the same in both channels");
        return;
    }
    panic!("no instrument made a sound");
}

#[test]
fn hifi_in_preview_mode_is_locked_at_once_and_note_on_prewarms_the_pressed_instrument() {
    let mut p = preview_player("karma.ahx");
    p.set_hifi(true);
    // No song walk, and no building on the render path: locked and empty.
    assert!(p.hifi_locked());
    assert_eq!(p.hifi_table_count(), 0);
    let mut heard = 0;
    for instrument in 1..=31usize {
        for note in [30, 44] {
            assert!(p.preview_note_on(instrument, note, 127));
            let built = p.hifi_table_count();
            let (attack, ..) = render(&mut p, 0.4);
            // A held note (long enough for envelope, sweeps and vibrato), then its release.
            render(&mut p, 3.0);
            p.preview_note_off();
            render(&mut p, 6.0);
            if attack > 0.0 {
                heard += 1;
            }
            assert_eq!(
                p.hifi_table_count(),
                built,
                "instrument {instrument} note {note}: render built a table"
            );
            assert_eq!(
                p.hifi_miss_count(),
                0.0,
                "instrument {instrument} note {note}: render missed a table"
            );
        }
    }
    assert!(heard > 0, "no instrument made a sound");
    assert!(p.hifi_table_count() > 0, "note-on prewarmed nothing");
    assert!(p.hifi_locked());
}

#[test]
fn hifi_is_locked_whichever_way_round_preview_and_hifi_are_switched_on() {
    let mut p = AhxPlayer::new(&fixture("karma.ahx"), RATE as u32, 2).unwrap();
    p.set_hifi(true);
    p.enable_preview();
    assert!(p.hifi_locked());
    // (What the song walk built before the switch stays; it is just unused.)
    assert!(p.preview_note_on(1, 30, 127));
    render(&mut p, 1.0);
    assert_eq!(p.hifi_miss_count(), 0.0);
}

#[test]
fn a_held_note_outlasts_the_envelope_and_a_released_one_does_not() {
    use cubed_ahx::engine::AhxEngine;
    use cubed_ahx::format::parse;

    fn peak_after(engine: &mut AhxEngine, seconds: f32) -> i32 {
        let mut buf = vec![0i16; (seconds * RATE as f32) as usize * 2];
        engine.render_block(&mut buf);
        buf.iter().map(|&x| (x as i32).abs()).max().unwrap()
    }

    // Instrument 1 of karma: a+d+s+r is 108 ticks (2.2 s at 50 Hz), so its own
    // envelope is over long before 4 s; only a key held down keeps it going.
    for instrument in [1usize, 2, 4] {
        // Sustain 0 releases right after the decay in a song; on the keyboard the
        // hold makes it a drone for as long as the key is down (a deliberate
        // difference, see `AhxEngine::live_tick`), so both must stay up.
        for s_frames in [None, Some(0u8)] {
            let mut song = parse(&fixture("karma.ahx")).unwrap();
            if let Some(s) = s_frames {
                song.instruments[instrument].envelope.s_frames = s;
            }
            let mut held = AhxEngine::new(song.clone(), RATE as u32, 2).unwrap();
            held.enable_live();
            held.live_note_on(instrument, 30, 127);
            assert!(
                peak_after(&mut held, 0.3) > 0,
                "instrument {instrument} is silent"
            );
            assert!(
                peak_after(&mut held, 3.7) > 0,
                "instrument {instrument} died while held (s_frames {s_frames:?})"
            );

            let mut released = AhxEngine::new(song, RATE as u32, 2).unwrap();
            released.enable_live();
            released.live_note_on(instrument, 30, 127);
            assert!(peak_after(&mut released, 0.3) > 0);
            released.live_note_off();
            peak_after(&mut released, 2.6);
            assert_eq!(
                peak_after(&mut released, 1.1),
                0,
                "instrument {instrument} kept sounding after note-off"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The PList playhead: `AhxEngine::live_plist_state` (and the two `AhxPlayer`
// getters over it) report the row the previewed note's voice ran last, for the
// editor's canvas. The expected traces below are worked out by hand from the
// per-tick PList block in `voice.rs` and are not produced by running anything:
//
//   * a trigger sets `perf_wait = 0`, `perf_speed = plist.speed`;
//   * every tick does `perf_wait -= 1` and runs the next row when the result,
//     as a signed byte, is <= 0, then sets `perf_wait = perf_speed`;
//   * so the first tick after a note-on always runs row 0, and with speed S
//     row r runs on tick `1 + r*S` (speed 0 runs a row every tick, as 1 does);
//   * Jump (command 5) sets the row that runs *next*; `F` (command 15) sets
//     both `perf_speed` and `perf_wait`, after the row has set `perf_wait`.
// ---------------------------------------------------------------------------

mod playhead {
    use super::*;
    use cubed_ahx::engine::AhxEngine;
    use cubed_ahx::format::{parse, Instrument, PList, PListEntry};

    /// A row that changes nothing but the note, so the trace is only about
    /// where the cursor goes.
    fn step() -> PListEntry {
        PListEntry {
            note: 1,
            ..PListEntry::default()
        }
    }

    fn cmd(fx: u8, param: u8) -> PListEntry {
        PListEntry {
            fx: [fx, 0],
            fx_param: [param, 0],
            ..step()
        }
    }

    fn plist(speed: u8, entries: Vec<PListEntry>) -> PList {
        PList { speed, entries }
    }

    /// karma with instrument `n` replaced by one that has `plists[n - 1]` and a
    /// release of `r_frames` (no hard cut), and an engine in live mode on it.
    fn live_engine(plists: Vec<PList>, r_frames: u8) -> AhxEngine {
        let mut song = parse(&fixture("karma.ahx")).unwrap();
        for (i, pl) in plists.into_iter().enumerate() {
            let ins: &mut Instrument = &mut song.instruments[i + 1];
            ins.plist = pl;
            ins.envelope.r_frames = r_frames;
            ins.hard_cut_release = false;
        }
        let mut e = AhxEngine::new(song, RATE as u32, 2).unwrap();
        e.enable_live();
        e
    }

    /// One engine tick (`play_irq` runs at the start of a tick's frames).
    fn tick(e: &mut AhxEngine) -> Option<(usize, usize)> {
        let mut buf = vec![0i16; e.samples_per_tick() * 2];
        e.render_block(&mut buf);
        e.live_plist_state()
    }

    /// The row reported after each of `n` ticks, `-1` for none.
    fn rows(e: &mut AhxEngine, n: usize) -> Vec<i32> {
        (0..n)
            .map(|_| tick(e).map_or(-1, |(_, row)| row as i32))
            .collect()
    }

    fn five_rows() -> Vec<PListEntry> {
        vec![step(); 5]
    }

    #[test]
    fn speed_1_runs_a_row_per_tick_from_the_first_tick_and_parks_on_the_last() {
        let mut e = live_engine(vec![plist(1, five_rows())], 0);
        assert_eq!(e.live_plist_state(), None, "no note yet");
        assert!(e.live_note_on(1, 30, 127));
        assert_eq!(
            e.live_plist_state(),
            None,
            "the note-on waits for the next tick"
        );
        // Rows 0..=4 on ticks 1..=5; the cursor is past the end after that, so
        // the last row that ran stays the answer.
        assert_eq!(rows(&mut e, 8), [0, 1, 2, 3, 4, 4, 4, 4]);
        assert_eq!(e.live_plist_state(), Some((1, 4)));
    }

    #[test]
    fn speed_0_runs_a_row_per_tick_like_speed_1() {
        // wait: 0 -> -1 (runs), then perf_wait = 0 -> -1 (runs) ...
        let mut e = live_engine(vec![plist(0, five_rows())], 0);
        e.live_note_on(1, 30, 127);
        assert_eq!(rows(&mut e, 6), [0, 1, 2, 3, 4, 4]);
    }

    #[test]
    fn speed_3_holds_each_row_for_three_ticks() {
        // Row r runs on tick 1 + 3r: 1, 4, 7, 10.
        let mut e = live_engine(vec![plist(3, vec![step(); 4])], 0);
        e.live_note_on(1, 30, 127);
        assert_eq!(rows(&mut e, 12), [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3]);
    }

    #[test]
    fn jump_reports_the_row_that_ran_not_the_one_it_jumped_to() {
        // [step, Jump 0]: 0, 1, 0, 1 ... the jump row is the one that ran.
        let mut e = live_engine(vec![plist(1, vec![step(), cmd(5, 0)])], 0);
        e.live_note_on(1, 30, 127);
        assert_eq!(rows(&mut e, 6), [0, 1, 0, 1, 0, 1]);

        // [step, step, step, Jump 1]: 0, 1, 2, 3, then 1, 2, 3, 1. On tick 4
        // row 3 ran and the cursor (`perf_current`, the row that runs next)
        // already holds the target 1: this is why the playhead has its own
        // field and is not `perf_current - 1`.
        let mut e = live_engine(vec![plist(1, vec![step(), step(), step(), cmd(5, 1)])], 0);
        e.live_note_on(1, 30, 127);
        assert_eq!(rows(&mut e, 3), [0, 1, 2]);
        assert_eq!(tick(&mut e), Some((1, 3)));
        assert_eq!(e.voice(0).perf_current, 1);
        assert_eq!(rows(&mut e, 4), [1, 2, 3, 1]);
    }

    #[test]
    fn the_f_speed_command_stretches_the_rows_after_it() {
        // Speed 1, row 0 is F03: it sets perf_speed = perf_wait = 3 after the
        // row set perf_wait = 1. Row 0 runs on tick 1 (wait 3), then wait
        // 2, 1, 0 on ticks 2..4: row 1 runs on tick 4 and reloads 3, so row 2
        // is on tick 7 and row 3 on tick 10.
        let mut e = live_engine(vec![plist(1, vec![cmd(15, 3), step(), step(), step()])], 0);
        e.live_note_on(1, 30, 127);
        assert_eq!(rows(&mut e, 11), [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3]);
    }

    #[test]
    fn a_retrigger_restarts_at_row_0_and_stamps_the_instrument() {
        let mut e = live_engine(
            vec![plist(1, vec![step(); 20]), plist(1, vec![step(); 3])],
            0,
        );
        e.live_note_on(1, 30, 127);
        assert_eq!(rows(&mut e, 5), [0, 1, 2, 3, 4]);
        assert_eq!(e.live_plist_state(), Some((1, 4)));

        // Same instrument again: until the next tick the old note is what
        // sounds; that tick runs row 0 of the new one.
        assert!(e.live_note_on(1, 32, 127));
        assert_eq!(e.live_plist_state(), Some((1, 4)));
        assert_eq!(rows(&mut e, 3), [0, 1, 2]);

        // Another instrument: the stamp changes with the row, its list is 3
        // rows long and parks on row 2.
        assert!(e.live_note_on(2, 30, 127));
        assert_eq!(
            e.live_plist_state(),
            Some((1, 2)),
            "still the old note until the tick"
        );
        assert_eq!(tick(&mut e), Some((2, 0)));
        assert_eq!(tick(&mut e), Some((2, 1)));
        assert_eq!(tick(&mut e), Some((2, 2)));
        assert_eq!(tick(&mut e), Some((2, 2)));
    }

    #[test]
    fn note_off_keeps_the_rows_running_until_the_release_is_over_then_reports_none() {
        // hard_cut_release(frames) then one envelope step in the same tick, and
        // the voice is cut when released with r_frames <= 0: a release of R
        // frames ends on the R-th tick after the note-off. With no release
        // frames the engine ramps over LIVE_CUT_FRAMES = 2.
        for (r_frames, ticks_to_cut) in [(1u8, 1usize), (3, 3), (5, 5), (0, 2)] {
            let mut e = live_engine(vec![plist(1, vec![step(); 40])], r_frames);
            e.live_note_on(1, 30, 127);
            assert_eq!(rows(&mut e, 4), [0, 1, 2, 3]);
            e.live_note_off();
            let after = rows(&mut e, ticks_to_cut + 3);
            for (i, row) in after.iter().enumerate() {
                if i + 1 < ticks_to_cut {
                    // Still releasing: the list carries on (tick 5 + i runs row 4 + i).
                    assert_eq!(
                        *row,
                        4 + i as i32,
                        "release {r_frames}, tick {} after the note-off",
                        i + 1
                    );
                } else {
                    assert_eq!(
                        *row,
                        -1,
                        "release {r_frames}, tick {} after the note-off",
                        i + 1
                    );
                }
            }
            assert_eq!(e.live_plist_state(), None);

            // A fresh note brings it back from row 0.
            assert!(e.live_note_on(1, 30, 127));
            assert_eq!(tick(&mut e), Some((1, 0)));
        }
    }

    #[test]
    fn a_shorter_list_under_a_sounding_note_keeps_the_last_row_that_ran() {
        let mut e = live_engine(vec![plist(1, vec![step(); 6])], 0);
        e.live_note_on(1, 30, 127);
        assert_eq!(rows(&mut e, 3), [0, 1, 2]);

        // The list shrinks to 2 rows while the cursor (3) is past its end: no
        // row runs any more, and what is reported is still row 2, which the
        // list no longer has. The engine says what ran; the editor's canvas
        // shows no bar for a row it does not have.
        let mut ins = e.song().instruments[1].clone();
        ins.plist.entries.truncate(2);
        assert!(e.replace_instrument(1, ins).is_some());
        assert_eq!(rows(&mut e, 3), [2, 2, 2]);

        // Grown again, it carries on from the cursor: rows 3, 4, 5.
        let mut ins = e.song().instruments[1].clone();
        ins.plist.entries = vec![step(); 6];
        e.replace_instrument(1, ins);
        assert_eq!(rows(&mut e, 4), [3, 4, 5, 5]);
    }

    #[test]
    fn an_instrument_with_no_rows_has_no_playhead() {
        let mut e = live_engine(vec![plist(1, vec![])], 0);
        assert!(e.live_note_on(1, 30, 127));
        assert_eq!(rows(&mut e, 5), [-1; 5]);
    }

    #[test]
    fn instrument_0_and_missing_instruments_never_report() {
        let mut e = live_engine(vec![plist(1, five_rows())], 0);
        assert!(!e.live_note_on(0, 30, 127));
        assert!(!e.live_note_on(200, 30, 127));
        assert_eq!(rows(&mut e, 3), [-1; 3], "refused note-ons start nothing");
        assert_eq!(e.voice(0).instrument_idx, 0);
        assert_eq!(e.voice(0).perf_row, -1);
    }

    #[test]
    fn song_mode_reports_nothing_whatever_its_voices_are_doing() {
        let song = parse(&fixture("karma.ahx")).unwrap();
        let mut e = AhxEngine::new(song, RATE as u32, 2).unwrap();
        assert!(!e.live_enabled());
        for _ in 0..200 {
            assert_eq!(tick(&mut e), None);
        }
    }

    #[test]
    fn the_player_getters_are_minus_one_and_zero_for_none_and_the_pair_otherwise() {
        let song_bytes = fixture("karma.ahx");
        let song = parse(&song_bytes).unwrap();
        let (instrument, len) = (1..=song.instrument_nr as usize)
            .map(|i| (i, song.instruments[i].plist.entries.len()))
            .find(|&(_, len)| len > 0)
            .expect("an instrument with a PList");

        // The song player: nothing to report.
        let mut p = AhxPlayer::new(&song_bytes, RATE as u32, 2).unwrap();
        p.play();
        render(&mut p, 1.0);
        assert_eq!(
            (p.preview_plist_row(), p.preview_plist_instrument()),
            (-1, 0)
        );

        // A preview player, idle and then playing.
        let mut p = preview_player("karma.ahx");
        assert_eq!(
            (p.preview_plist_row(), p.preview_plist_instrument()),
            (-1, 0)
        );
        assert!(p.preview_note_on(instrument, 30, 127));
        assert_eq!(
            (p.preview_plist_row(), p.preview_plist_instrument()),
            (-1, 0),
            "not before the tick"
        );
        render(&mut p, 0.001); // the first frame runs the tick
        assert_eq!(
            (p.preview_plist_row(), p.preview_plist_instrument()),
            (0, instrument as u32)
        );
        // Held long past any PList, it parks on the last row or loops within it.
        render(&mut p, 3.0);
        let (row, stamp) = (p.preview_plist_row(), p.preview_plist_instrument());
        assert!(row >= 0 && (row as usize) < len, "row {row} of {len}");
        assert_eq!(stamp, instrument as u32);
        // Released and run dry: back to none.
        p.preview_note_off();
        render(&mut p, 6.0);
        assert_eq!(
            (p.preview_plist_row(), p.preview_plist_instrument()),
            (-1, 0)
        );
    }
}
