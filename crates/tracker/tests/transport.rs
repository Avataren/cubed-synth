//! Live control of a playing song: stop and play, seek, loops, mute and solo,
//! the note preview, and what a probe shows (position, levels, channel
//! scopes). Played over the synthetic MOD, S3M and XM demos.

use std::sync::Arc;

use cubed_tracker::{
    Loop, Module, Observer, Player, PlayerOptions, Probe, LEVEL_SLOTS, SCOPE_FRAMES,
};

const RATE: f32 = 48_000.0;

fn load(name: &str) -> Module {
    let bytes = std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    cubed_tracker::load(&bytes).unwrap()
}

fn player(name: &str) -> Player {
    Player::new(load(name), RATE, PlayerOptions::default())
}

#[derive(Default)]
struct Rows(Vec<(usize, usize)>);

impl Observer for Rows {
    fn row(&mut self, _time: f64, order: usize, row: usize) {
        self.0.push((order, row));
    }
}

/// Renders `seconds` in blocks of `block` frames, returning the rows that
/// started and the audio (left then right interleaved by block is not needed:
/// the two channels are summed into one mono track for energy checks).
fn run(p: &mut Player, seconds: f64, block: usize) -> (Vec<(usize, usize)>, Vec<f32>) {
    let mut rows = Rows::default();
    let (mut l, mut r) = (vec![0.0f32; block], vec![0.0f32; block]);
    let mut audio = Vec::new();
    let frames = (seconds * RATE as f64) as usize;
    for _ in 0..frames / block {
        p.render_observed(&mut l, &mut r, &mut rows);
        audio.extend(l.iter().zip(&r).map(|(a, b)| a + b));
    }
    (rows.0, audio)
}

fn energy(x: &[f32]) -> f32 {
    x.iter().map(|s| s * s).sum()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, s| m.max(s.abs()))
}

#[test]
fn a_stopped_player_holds_its_position_and_is_silent() {
    let mut p = player("demo.mod");
    let (_, audio) = run(&mut p, 1.0, 128);
    assert!(energy(&audio) > 0.0, "playing by default");
    p.stop();
    assert!(!p.playing());
    let at = p.position();
    // The notes die away in a few milliseconds, then nothing.
    let (rows, audio) = run(&mut p, 1.0, 128);
    assert!(rows.is_empty(), "a stopped song starts no rows");
    assert_eq!(p.position(), at, "and stays where it was");
    assert!(
        peak(&audio[(0.05 * RATE) as usize..]) == 0.0,
        "silent after the tails"
    );
    assert!(!p.status().playing);
    // Playing again starts from that row.
    p.play();
    let (rows, audio) = run(&mut p, 0.5, 128);
    assert_eq!((rows[0].0, rows[0].1), (at.order, at.row));
    assert!(energy(&audio) > 0.0);
    assert!(p.status().playing);
}

#[test]
fn seeking_works_while_stopped_and_play_starts_there() {
    let mut p = player("demo.mod");
    p.stop();
    p.seek(2, 8);
    let s = p.status();
    assert_eq!((s.order, s.pattern, s.row), (2, 2, 8));
    assert!(!s.playing);
    // Seeking past the end clamps to the last order and row.
    p.seek(99, 999);
    let s = p.status();
    assert_eq!((s.order as usize, s.row as usize), (3, 63));
    p.seek(2, 8);
    p.play();
    let (rows, _) = run(&mut p, 1.0, 128);
    assert_eq!(rows[0], (2, 8));
    assert_eq!(rows[1], (2, 9));
}

#[test]
fn seeking_while_playing_fades_the_notes_instead_of_cutting_them() {
    let mut p = player("demo.mod");
    let (_, before) = run(&mut p, 0.5, 64);
    assert!(peak(&before) > 0.1);
    // The last frame before the seek is not silent: note it, seek, and the first
    // frames after do not jump to a different level in one sample.
    let tail = *before.last().unwrap();
    p.seek(1, 0);
    let (_, after) = run(&mut p, 0.01, 64);
    let step = (after[0] - tail).abs();
    assert!(step < 0.3, "a click of {step}");
}

#[test]
fn the_status_follows_the_row_the_tick_and_the_tempo() {
    let mut p = player("demo.mod");
    let probe = Probe::new(4);
    p.attach_probe(probe.clone());
    let mut seen_ticks = std::collections::BTreeSet::new();
    let mut last_rows = 0;
    let (mut l, mut r) = ([0.0f32; 64], [0.0f32; 64]);
    for _ in 0..(RATE as usize * 2 / 64) {
        p.render(&mut l, &mut r);
        let s = probe.status();
        assert_eq!(s, probe.status());
        assert!(s.tempo >= 32 && s.tempo <= 255, "tempo {}", s.tempo);
        assert!(s.tick < s.speed.max(1), "tick {} of {}", s.tick, s.speed);
        assert!(s.rows >= last_rows);
        last_rows = s.rows;
        seen_ticks.insert(s.tick);
        assert!(s.frames > 0);
    }
    // The demo runs at speeds of 4 to 6: the ticks of a row all show up.
    assert!(seen_ticks.len() >= 4, "{seen_ticks:?}");
    assert!(last_rows >= 10, "rows counted: {last_rows}");
    assert!(probe.status().playing);
    assert!(probe.alive());
    drop(p);
    assert!(!probe.alive(), "the probe sees the player go");
}

#[test]
fn a_pattern_loop_repeats_and_ignores_jumps_and_breaks() {
    let mut p = player("demo.mod");
    // Order 2 ends in B03 and D16 on row 30, which would leave for order 3.
    p.seek(2, 0);
    p.set_loop(Loop::Pattern(2));
    assert_eq!(p.loop_region(), Loop::Pattern(2));
    let (rows, _) = run(&mut p, 20.0, 128);
    assert!(rows.iter().all(|r| r.0 == 2), "never leaves the pattern");
    let at30 = rows.iter().position(|r| r.1 == 30).expect("reaches row 30");
    // The jump (which beats the break beside it) is ignored: the next row plays.
    assert_eq!(rows[at30 + 1], (2, 31));
    let starts = rows.iter().filter(|r| r.1 == 0).count();
    assert!(starts >= 2, "went round more than once ({starts})");
    // With the jump taken out, the break goes to its row in the same pattern:
    // D16 is row 16.
    let mut module = load("demo.mod");
    for ch in 0..module.channels {
        let cell = module.patterns[2].cell_mut(30, ch);
        if cell.effect == 0x0b {
            *cell = Default::default();
        }
    }
    let mut p = Player::new(module, RATE, PlayerOptions::default());
    p.seek(2, 0);
    p.set_loop(Loop::Pattern(2));
    let (rows, _) = run(&mut p, 20.0, 128);
    let at30 = rows.iter().position(|r| r.1 == 30).expect("reaches row 30");
    assert_eq!(rows[at30 + 1], (2, 16));
    let mut p = player("demo.mod");
    p.seek(2, 0);
    p.set_loop(Loop::Pattern(2));
    run(&mut p, 1.0, 128);
    // Without the loop it moves on.
    p.set_loop(Loop::None);
    let (rows, _) = run(&mut p, 5.0, 128);
    assert!(rows.iter().any(|r| r.0 != 2));
}

#[test]
fn a_range_loop_wraps_inside_its_rows() {
    let mut p = player("demo.mod");
    p.seek(0, 4);
    p.set_loop(Loop::Range {
        order: 0,
        row0: 4,
        row1: 9,
    });
    let (rows, _) = run(&mut p, 6.0, 128);
    assert!(rows.len() > 18);
    assert!(rows.iter().all(|r| r.0 == 0 && (4..=9).contains(&r.1)));
    for w in rows.windows(2) {
        let next = if w[0].1 == 9 { 4 } else { w[0].1 + 1 };
        assert_eq!(w[1].1, next, "{w:?}");
    }
    // A loop does not move the playhead: set elsewhere, the song plays on until it
    // reaches the loop's pattern.
    let mut p = player("demo.mod");
    p.set_loop(Loop::Range {
        order: 2,
        row0: 4,
        row1: 5,
    });
    let (rows, _) = run(&mut p, 40.0, 128);
    let first = rows.iter().position(|r| r.0 == 2).unwrap();
    assert!(rows[..first].iter().all(|r| r.0 < 2));
    assert!(rows[first..].iter().all(|r| r.0 == 2));
    let tail: Vec<_> = rows.iter().rev().take(6).map(|r| r.1).collect();
    assert!(tail.iter().all(|&r| r == 4 || r == 5), "{tail:?}");
}

#[test]
fn a_row_sought_to_beyond_the_loop_plays_once_then_loops() {
    let mut p = player("demo.mod");
    p.set_loop(Loop::Range {
        order: 0,
        row0: 4,
        row1: 9,
    });
    p.seek(0, 20);
    let (rows, _) = run(&mut p, 2.0, 128);
    assert_eq!(rows[0], (0, 20));
    assert_eq!(rows[1], (0, 4));
}

#[test]
fn a_loop_on_the_last_rows_wraps_at_the_pattern_end() {
    let mut p = player("demo.xm");
    // XM pattern 1 has 48 rows; a range that reaches past them stops at the last.
    p.seek(1, 40);
    p.set_loop(Loop::Range {
        order: 1,
        row0: 44,
        row1: 200,
    });
    let (rows, _) = run(&mut p, 4.0, 128);
    assert!(rows.iter().all(|r| r.0 == 1));
    assert!(rows.iter().any(|r| r.1 == 47));
    let after_last = rows.windows(2).find(|w| w[0].1 == 47).unwrap();
    assert_eq!(after_last[1].1, 44);
}

#[test]
fn solo_hears_only_the_soloed_and_mute_only_the_muted() {
    let all = {
        let mut p = player("demo.mod");
        energy(&run(&mut p, 3.0, 128).1)
    };
    let mut solo2 = player("demo.mod");
    solo2.set_solo(2, true);
    assert_eq!(solo2.solo_mask(), 0b0100);
    let solo_only = energy(&run(&mut solo2, 3.0, 128).1);
    let mut others = player("demo.mod");
    others.set_mute(2, true);
    let without = energy(&run(&mut others, 3.0, 128).1);
    assert!(solo_only > 0.0 && solo_only < all * 0.8);
    assert!(without > 0.0 && without < all);
    // The mask API says the same, and a solo beats a mute: soloing a muted
    // channel hears it, as the DAW does.
    let mut q = player("demo.mod");
    q.set_mute_mask(0b0100);
    q.set_solo_mask(0b0100);
    assert!(q.channel_audible(2) && !q.channel_audible(0));
    let same = energy(&run(&mut q, 3.0, 128).1);
    assert!(
        (same - solo_only).abs() <= solo_only * 1e-3,
        "{same} vs {solo_only}"
    );
    q.set_solo_mask(0);
    assert!(!q.channel_audible(2), "the mute is still there");
    q.set_mute(2, false);
    assert!(q.channel_audible(2) && q.channel_audible(0));
    assert_eq!(q.mute_mask(), 0);
}

#[test]
fn channel_levels_read_each_channels_own_sound() {
    let mut p = player("demo.mod");
    let probe = Probe::new(4);
    p.attach_probe(probe.clone());
    p.set_mute(1, true);
    let (mut l, mut r) = ([0.0f32; 128], [0.0f32; 128]);
    let mut best = [0.0f32; LEVEL_SLOTS];
    let mut out = [0.0f32; LEVEL_SLOTS];
    for _ in 0..(RATE as usize * 3 / 128) {
        p.render(&mut l, &mut r);
        let n = probe.levels(&mut out);
        assert_eq!(n, 5, "four channels and the preview");
        for i in 0..n {
            best[i] = best[i].max(out[i]);
        }
    }
    assert_eq!(best[1], 0.0, "a muted channel shows nothing");
    assert!(
        best[0] > 0.01 && best[2] > 0.01 && best[3] > 0.0,
        "{best:?}"
    );
    assert_eq!(best[4], 0.0, "nothing previewed");
    // The loudest channel is no louder than the whole mix.
    assert!(best[..4].iter().all(|&v| v <= 2.0));
    // They fall away once the song stops.
    p.stop();
    for _ in 0..(RATE as usize * 4 / 128) {
        p.render(&mut l, &mut r);
    }
    probe.levels(&mut out);
    assert!(out[..5].iter().all(|&v| v == 0.0), "{out:?}");
}

#[test]
fn channel_scopes_only_run_when_asked_and_show_the_channel() {
    let mut p = player("demo.mod");
    let probe = Probe::new(4);
    p.attach_probe(probe.clone());
    let (mut l, mut r) = ([0.0f32; 128], [0.0f32; 128]);
    for _ in 0..200 {
        p.render(&mut l, &mut r);
    }
    assert_eq!(probe.scope_position(0), 0, "off: nothing written");
    let mut ring = [0.0f32; SCOPE_FRAMES];
    assert_eq!(probe.scope(0, &mut ring), 0);
    probe.set_scopes(true);
    assert!(probe.scopes_enabled());
    for _ in 0..200 {
        p.render(&mut l, &mut r);
    }
    assert_eq!(probe.scope_position(0), 200 * 128);
    assert_eq!(probe.scope(0, &mut ring), SCOPE_FRAMES);
    let alive = (0..4).filter(|&c| {
        let mut buf = [0.0f32; SCOPE_FRAMES];
        probe.scope(c, &mut buf);
        peak(&buf) > 0.01
    });
    assert!(alive.count() >= 2, "the channels have their own traces");
    // The sum of the channels' traces is the song's output (before the master
    // gain and the Amiga filter, so only roughly): a muted one is flat.
    p.set_mute(0, true);
    for _ in 0..200 {
        p.render(&mut l, &mut r);
    }
    probe.scope(0, &mut ring);
    assert_eq!(peak(&ring), 0.0);
    probe.set_scopes(false);
    let at = probe.scope_position(2);
    p.render(&mut l, &mut r);
    assert_eq!(probe.scope_position(2), at, "off again: not written");
}

#[test]
fn a_preview_plays_over_a_stopped_song_and_lets_go() {
    let mut p = player("demo.mod");
    let probe = Probe::new(4);
    p.attach_probe(probe.clone());
    p.stop();
    run(&mut p, 0.1, 128);
    assert!(p.preview_on(1, 48, 1.0, 0));
    let (rows, audio) = run(&mut p, 0.3, 128);
    assert!(rows.is_empty(), "the song stays stopped");
    assert!(energy(&audio) > 0.01);
    assert!(probe.status().preview);
    let mut levels = [0.0; LEVEL_SLOTS];
    probe.levels(&mut levels);
    assert!(levels[4] > 0.01, "the preview has a level of its own");
    assert_eq!(levels[..4], [0.0; 4]);
    p.preview_off();
    let (_, audio) = run(&mut p, 0.5, 128);
    assert_eq!(peak(&audio[(0.1 * RATE) as usize..]), 0.0, "released");
    assert!(!probe.status().preview);
    // Sample 1 loops; the demo's sample 4 does not, and ends by itself.
    assert!(p.preview_on(4, 48, 1.0, 0));
    run(&mut p, 2.0, 128);
    assert!(!probe.status().preview);
    // No sample, no note.
    assert!(!p.preview_on(0, 48, 1.0, 0));
    assert!(!p.preview_on(99, 48, 1.0, 0));
}

#[test]
fn a_preview_note_lasts_its_duration_and_sounds_over_a_playing_muted_song() {
    let mut p = player("demo.mod");
    p.set_mute_mask(0b1111);
    let (_, quiet) = run(&mut p, 0.5, 128);
    assert_eq!(peak(&quiet), 0.0, "everything is muted");
    assert!(p.preview_on(1, 60, 1.0, 9_600));
    let (_, audio) = run(&mut p, 0.5, 128);
    assert!(
        energy(&audio) > 0.01,
        "mute and solo leave the preview alone"
    );
    // 0.2 s long, then a 10 ms release.
    assert!(peak(&audio[..(0.19 * RATE) as usize]) > 0.01);
    assert_eq!(peak(&audio[(0.25 * RATE) as usize..]), 0.0);
    // Notes follow the pitch: an octave up has twice the zero crossings.
    let crossings = |note: u8| {
        let mut p = player("demo.mod");
        p.stop();
        assert!(p.preview_on(1, note, 1.0, 0));
        let (_, a) = run(&mut p, 0.2, 128);
        a.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() as f32
    };
    let ratio = crossings(48) / crossings(36);
    assert!((ratio - 2.0).abs() < 0.2, "octave ratio {ratio}");
}

#[test]
fn a_louder_velocity_is_a_louder_preview() {
    let level = |velocity: f32| {
        let mut p = player("demo.mod");
        p.stop();
        assert!(p.preview_on(1, 48, velocity, 0));
        peak(&run(&mut p, 0.2, 128).1)
    };
    let (soft, loud) = (level(0.25), level(1.0));
    assert!((loud / soft - 4.0).abs() < 0.4, "{soft} {loud}");
}

#[test]
fn an_xm_preview_picks_its_sample_by_the_keymap_and_runs_the_envelopes() {
    let preview = |instrument: u8, note: u8| {
        let mut p = player("demo.xm");
        p.stop();
        assert!(p.preview_on(instrument, note, 1.0, 0));
        let (_, a) = run(&mut p, 0.3, 128);
        (a, p)
    };
    // Instrument 2: the square below MIDI 60, the ping-pong sine from it up: the
    // sine is centred a little right (pan 192), the square left (pan 64).
    let pan_of = |note: u8| {
        let mut p = player("demo.xm");
        p.stop();
        assert!(p.preview_on(2, note, 1.0, 0));
        let (mut l, mut r) = ([0.0f32; 4096], [0.0f32; 4096]);
        p.render(&mut l, &mut r);
        energy(&r) / energy(&l)
    };
    assert!(pan_of(59) < 1.0 && pan_of(60) > 1.0);
    // The pad's volume envelope rises from 0 over 6 ticks.
    let (pad, mut p) = preview(1, 60);
    let start = peak(&pad[..(0.01 * RATE) as usize]);
    let held = peak(&pad[(0.2 * RATE) as usize..]);
    assert!(held > start * 2.0, "{start} then {held}");
    // Key-off goes through the envelope's release and fadeout, not a click.
    p.preview_off();
    let (_, tail) = run(&mut p, 3.0, 128);
    assert!(peak(&tail[..128]) > 0.0);
    assert_eq!(peak(&tail[tail.len() - 1000..]), 0.0, "faded away");
    let mut hat = player("demo.xm");
    hat.stop();
    assert!(hat.preview_on(4, 60, 1.0, 0));
    run(&mut hat, 1.0, 128);
    assert!(!hat.status().preview, "its envelope runs out");
    assert!(!hat.preview_on(9, 60, 1.0, 0), "no instrument 9");
}

#[test]
fn an_s3m_adlib_instrument_previews_on_the_chip() {
    let mut p = player("demo.s3m");
    p.stop();
    run(&mut p, 0.2, 128);
    // Instrument 5 is the AdLib organ.
    assert!(p.preview_on(5, 60, 1.0, 0));
    assert!(p.status().preview);
    let (_, audio) = run(&mut p, 0.5, 128);
    assert!(energy(&audio) > 1e-4, "the chip plays");
    p.preview_off();
    assert!(!p.status().preview);
    run(&mut p, 1.0, 128);
    // A PCM instrument right after is a PCM note.
    assert!(p.preview_on(1, 60, 1.0, 0));
    let (_, audio) = run(&mut p, 0.3, 128);
    assert!(energy(&audio) > 0.01);
}

#[test]
fn an_s3m_adlib_preview_while_the_song_plays_leaves_the_songs_channels() {
    let mut p = player("demo.s3m");
    let probe = Probe::new(5);
    p.attach_probe(probe.clone());
    run(&mut p, 0.3, 128);
    assert!(p.preview_on(5, 72, 1.0, 4_800));
    run(&mut p, 0.5, 128);
    assert!(!probe.status().preview, "released by its duration");
    assert!(probe.status().playing);
}

#[test]
fn attaching_a_probe_after_a_seek_shows_the_position_at_once() {
    let mut p = player("demo.mod");
    p.stop();
    p.seek(2, 12);
    let probe: Arc<Probe> = Probe::new(4);
    p.attach_probe(probe.clone());
    let s = probe.status();
    assert_eq!((s.order, s.pattern, s.row, s.playing), (2, 2, 12, false));
}

#[test]
fn a_live_cell_edit_is_played_the_next_time_it_is_reached() {
    let mut p = player("demo.mod");
    p.stop();
    // Empty the first four rows of pattern 0: a silent start.
    for row in 0..4 {
        for ch in 0..4 {
            assert!(p.set_cell(0, row, ch, Default::default()));
        }
    }
    assert!(!p.set_cell(0, 64, 0, Default::default()), "no row 64");
    assert!(!p.set_cell(0, 0, 4, Default::default()), "no channel 4");
    assert!(!p.set_cell(9, 0, 0, Default::default()), "no pattern 9");
    p.seek(0, 0);
    p.play();
    let (_, silent) = run(&mut p, 0.3, 128);
    assert_eq!(peak(&silent), 0.0);
    // A note on channel 1, row 1: C-2 (MIDI 36, period 428) on sample 1.
    let cell = cubed_tracker::Cell {
        note: cubed_tracker::Note::On(36),
        period: 428,
        instrument: 1,
        ..Default::default()
    };
    assert!(p.set_cell(0, 1, 1, cell));
    assert_eq!(p.module().patterns[0].cell(1, 1), &cell);
    p.seek(0, 0);
    let (_, sound) = run(&mut p, 0.3, 128);
    assert!(energy(&sound) > 0.01, "the new note plays");
}
