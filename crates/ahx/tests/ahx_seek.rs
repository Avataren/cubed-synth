//! Transport: seek, loop-position and pause/resume on the engine and the
//! wasm-facing player.
//!
//! The contract for `seek` is the strong one: an engine put at `(position,
//! row)` renders *sample for sample* what an engine that played the song from
//! the top to that row renders from there. The reference here is exactly that
//! (a fresh engine rendered tick by tick, its output thrown away), so nothing
//! in the seek path is trusted to check itself. Everything runs on the real
//! corpus, with hi-fi on and off and with capture on, across block sizes that
//! do not line up with ticks.

use cubed_ahx::engine::{AhxEngine, SeekKind};
use cubed_ahx::format;
use cubed_ahx::player::AhxPlayer;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

const RATE: u32 = 44100;
const BLOCKS: [usize; 5] = [128, 1, 333, 882, 4096];

fn bytes(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/songs")
            .join(name),
    )
    .unwrap()
}

fn engine(name: &str) -> AhxEngine {
    AhxEngine::new(format::parse(&bytes(name)).unwrap(), RATE, 2).unwrap()
}

fn render(e: &mut AhxEngine, frames: usize, blocks: &[usize]) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames * 2);
    let mut done = 0;
    let mut k = 0;
    while done < frames {
        let n = blocks[k % blocks.len()].min(frames - done);
        let at = out.len();
        out.resize(at + n * 2, 0);
        e.render_block(&mut out[at..]);
        done += n;
        k += 1;
    }
    out
}

/// A row top the song's own flow reaches: where it is and how many whole ticks
/// it took to get there from the top.
#[derive(Debug, Clone, Copy)]
struct RowTop {
    pos: usize,
    row: usize,
    ticks: usize,
}

/// Every row top of the first `max_ticks` ticks, in order, first visit only:
/// a boundary where `(pos, row)` differs from the last one (the transport only
/// moves at the end of a row's last tick).
fn row_tops(name: &str, max_ticks: usize) -> Vec<RowTop> {
    let mut e = engine(name);
    let tick = e.samples_per_tick();
    let mut buf = vec![0i16; tick * 2];
    let mut seen = HashSet::new();
    let mut tops = Vec::new();
    let mut last = (-1, -1);
    for ticks in 0..max_ticks {
        let here = (e.pos_nr(), e.note_nr());
        if here != last {
            last = here;
            if seen.insert(here) {
                tops.push(RowTop {
                    pos: here.0 as usize,
                    row: here.1 as usize,
                    ticks,
                });
            }
        }
        if e.song_end_reached() {
            break;
        }
        e.render_block(&mut buf);
    }
    tops
}

/// A fresh engine played `ticks` whole ticks, output discarded.
fn played_to(e: &mut AhxEngine, ticks: usize) {
    let mut buf = vec![0i16; e.samples_per_tick() * 2];
    for _ in 0..ticks {
        e.render_block(&mut buf);
    }
}

/// Six row tops spread over the first ~40 s (plus the very first): early,
/// mid-pattern, across position boundaries.
fn targets(name: &str) -> Vec<RowTop> {
    let tops = row_tops(name, 50 * 40);
    assert!(tops.len() > 8, "{name}: too short to seek in");
    let step = tops.len() / 6;
    (0..6)
        .map(|i| tops[i * step])
        .chain([tops[tops.len() - 1]])
        .collect()
}

const SONGS: [&str; 4] = [
    "karma.ahx",
    "robocop_iii_j_tel.ahx",
    "sunspots.hvl",
    "blondie.ahx",
];

fn setup(e: &mut AhxEngine, hifi: bool, capture: bool) {
    if hifi {
        e.set_hifi(true);
        e.prewarm_hifi();
    }
    e.enable_capture(capture);
}

#[test]
fn seek_renders_exactly_what_a_full_run_renders_from_there() {
    for name in SONGS {
        for (hifi, capture) in [(false, false), (true, false), (false, true)] {
            for t in targets(name) {
                let mut full = engine(name);
                setup(&mut full, hifi, capture);
                played_to(&mut full, t.ticks);

                let mut seeked = engine(name);
                setup(&mut seeked, hifi, capture);
                assert_eq!(
                    seeked.seek(t.pos, t.row),
                    Some(SeekKind::Exact),
                    "{name} {t:?}"
                );

                let what = format!("{name} hifi={hifi} capture={capture} {t:?}");
                assert_eq!(
                    (seeked.pos_nr(), seeked.note_nr()),
                    (t.pos as i32, t.row as i32),
                    "{what}"
                );
                assert_eq!(seeked.tempo(), full.tempo(), "{what}: speed");
                assert_eq!(
                    seeked.ticks_played(),
                    full.ticks_played(),
                    "{what}: tick counter"
                );
                // Two seconds, in block sizes that split ticks; it spans several rows.
                let want = render(&mut full, RATE as usize * 2, &BLOCKS);
                let got = render(&mut seeked, RATE as usize * 2, &BLOCKS);
                assert!(
                    want == got,
                    "{what}: render after the seek differs from the full run"
                );
                if capture {
                    for v in 0..full.channels() {
                        let (mut a, mut b) = (vec![0i16; 2048], vec![0i16; 2048]);
                        full.read_channel_snapshot(v, &mut a);
                        seeked.read_channel_snapshot(v, &mut b);
                        assert!(a == b, "{what}: voice {v} scope");
                    }
                }
            }
        }
    }
}

#[test]
fn seek_lands_on_the_first_sample_of_the_row() {
    // The row's own notes are what the first tick after the seek plays: the
    // tick counter is a whole number of ticks and no tick is half-consumed,
    // so the next tick starts by processing that row's step.
    let mut e = engine("karma.ahx");
    let t = row_tops("karma.ahx", 2000)[20];
    e.seek(t.pos, t.row).unwrap();
    assert_eq!(e.ticks_played() as usize, t.ticks);
    let mut one_tick = vec![0i16; e.samples_per_tick() * 2];
    e.render_block(&mut one_tick);
    assert_eq!(e.ticks_played() as usize, t.ticks + 1, "one tick rendered");
}

#[test]
fn seek_from_any_earlier_state_equals_seek_from_fresh() {
    for name in ["karma.ahx", "sunspots.hvl"] {
        let ts = targets(name);
        let t = ts[3];
        let mut fresh = engine(name);
        fresh.seek(t.pos, t.row).unwrap();
        let want = render(&mut fresh, RATE as usize, &BLOCKS);

        // Already played past the target, then a backward seek; and one that was
        // mid-tick (an odd block) with a note sounding.
        let mut back = engine(name);
        render(&mut back, RATE as usize * 15 + 17, &BLOCKS);
        back.seek(t.pos, t.row).unwrap();
        assert!(
            render(&mut back, RATE as usize, &BLOCKS) == want,
            "{name}: backward seek"
        );

        // Seek to a later target first, then to this one.
        let mut hop = engine(name);
        let far = ts[ts.len() - 1];
        hop.seek(far.pos, far.row).unwrap();
        render(&mut hop, 5000, &BLOCKS);
        hop.seek(t.pos, t.row).unwrap();
        assert!(
            render(&mut hop, RATE as usize, &BLOCKS) == want,
            "{name}: hop"
        );
    }
}

#[test]
fn seek_to_the_top_is_the_fresh_engine() {
    for name in SONGS {
        let mut fresh = engine(name);
        let want = render(&mut fresh, RATE as usize * 2, &BLOCKS);
        let mut e = engine(name);
        render(&mut e, 30_000, &BLOCKS);
        assert_eq!(e.seek(0, 0), Some(SeekKind::Exact));
        assert!(render(&mut e, RATE as usize * 2, &BLOCKS) == want, "{name}");
    }
}

#[test]
fn seek_out_of_range_changes_nothing() {
    let mut a = engine("karma.ahx");
    let mut b = engine("karma.ahx");
    render(&mut a, 40_000, &BLOCKS);
    render(&mut b, 40_000, &BLOCKS);
    let count = a.song().position_nr;
    let len = a.song().track_length as usize;
    assert_eq!(a.seek(count, 0), None);
    assert_eq!(a.seek(0, len), None);
    assert_eq!(a.seek(usize::MAX, usize::MAX), None);
    assert!(render(&mut a, 20_000, &BLOCKS) == render(&mut b, 20_000, &BLOCKS));
}

#[test]
fn seek_survives_mute_solo_hifi_and_capture_settings() {
    let mut e = engine("karma.ahx");
    e.set_mute_solo(0b0010, 0);
    e.set_hifi(true);
    e.prewarm_hifi();
    e.enable_capture(true);
    let t = targets("karma.ahx")[3];
    e.seek(t.pos, t.row).unwrap();
    assert_eq!(e.mute_solo(), (0b0010, 0));
    assert!(e.hifi_enabled() && e.hifi_locked() && e.capture_enabled());
    let mut reference = engine("karma.ahx");
    reference.set_hifi(true);
    reference.prewarm_hifi();
    reference.set_mute_solo(0b0010, 0);
    played_to(&mut reference, t.ticks);
    assert!(
        render(&mut e, RATE as usize, &BLOCKS) == render(&mut reference, RATE as usize, &BLOCKS)
    );
    assert_eq!(e.hifi_misses(), reference.hifi_misses());
}

/// Positions and rows the song's flow never lands on (a jump skips the
/// position, a break leaves before the row) start cold, at exactly that spot.
#[test]
fn an_unreachable_row_starts_cold_at_that_row() {
    let mut cold = 0;
    for name in SONGS.iter().copied().chain([
        "thats_the_wave_it_is.ahx",
        "the_fugitive.ahx",
        "wave_stepper.ahx",
    ]) {
        let reached: HashSet<(usize, usize)> = row_tops(name, 400_000)
            .iter()
            .map(|t| (t.pos, t.row))
            .collect();
        let mut e = engine(name);
        let (positions, len) = (e.song().position_nr, e.song().track_length as usize);
        let missing = (0..positions)
            .flat_map(|p| (0..len).map(move |r| (p, r)))
            .find(|k| !reached.contains(k));
        let Some((p, r)) = missing else { continue };
        assert_eq!(e.seek(p, r), Some(SeekKind::Cold), "{name} ({p},{r})");
        assert_eq!((e.pos_nr(), e.note_nr()), (p as i32, r as i32));
        assert_eq!(e.ticks_played(), 0);
        assert!(!e.song_end_reached());
        // It plays: the row's own step sounds and the transport moves on.
        render(&mut e, RATE as usize * 2, &BLOCKS);
        assert!(e.ticks_played() > 0);
        cold += 1;
    }
    assert!(
        cold > 0,
        "the corpus has no unreachable row to cover the cold path"
    );
}

/// `set_loop_position`: off is the reference transport; on, the position runs
/// its rows and starts over, on the same running clock.
#[test]
fn loop_position_repeats_one_position_on_a_running_clock() {
    for name in ["karma.ahx", "sunspots.hvl", "blondie.ahx"] {
        let t = row_tops(name, 4000)
            .into_iter()
            .find(|t| t.row == 0 && t.pos == 1)
            .expect("position 1 reached");
        let mut e = engine(name);
        e.set_loop_position(true);
        e.seek(t.pos, t.row).unwrap();
        assert!(e.loop_position(), "{name}: seek keeps the loop setting");
        let tick = e.samples_per_tick();
        let mut buf = vec![0i16; tick * 2];
        let (mut wraps, mut last_row, mut last_ticks) = (0, e.note_nr(), e.ticks_played());
        let mut heard = false;
        // Long enough for several trips round any single position.
        for _ in 0..(50 * 60) {
            e.render_block(&mut buf);
            assert_eq!(e.pos_nr(), t.pos as i32, "{name}: left the looped position");
            assert!(!e.song_end_reached());
            assert_eq!(e.ticks_played(), last_ticks + 1, "{name}: the clock ran on");
            last_ticks += 1;
            if e.note_nr() < last_row {
                wraps += 1;
            }
            last_row = e.note_nr();
            heard |= buf.iter().any(|&s| s != 0);
        }
        assert!(wraps >= 2, "{name}: only {wraps} trips");
        assert!(heard, "{name}: silent");
    }
}

#[test]
fn loop_position_is_the_reference_until_the_position_ends() {
    // Same bytes as the reference for as long as the reference stays on the
    // position; only the wrap differs.
    for name in ["karma.ahx", "sunspots.hvl"] {
        let t = row_tops(name, 4000)
            .into_iter()
            .find(|t| t.row == 0 && t.pos == 1)
            .unwrap();
        let mut plain = engine(name);
        plain.seek(t.pos, t.row).unwrap();
        let mut looped = engine(name);
        looped.set_loop_position(true);
        looped.seek(t.pos, t.row).unwrap();
        let tick = plain.samples_per_tick();
        let (mut a, mut b) = (vec![0i16; tick * 2], vec![0i16; tick * 2]);
        let mut compared = 0;
        while plain.pos_nr() == t.pos as i32 {
            plain.render_block(&mut a);
            looped.render_block(&mut b);
            assert!(
                a == b,
                "{name}: tick {compared} differs inside the position"
            );
            compared += 1;
        }
        assert!(compared > 10);
        // The reference has moved on; the loop is back on row 0 of the same position.
        assert_eq!((looped.pos_nr(), looped.note_nr()), (t.pos as i32, 0));
    }
}

#[test]
fn loop_position_off_again_moves_on() {
    let mut e = engine("karma.ahx");
    e.seek(1, 0).unwrap();
    e.set_loop_position(true);
    let tick = e.samples_per_tick();
    let mut buf = vec![0i16; tick * 2];
    for _ in 0..(50 * 40) {
        e.render_block(&mut buf);
    }
    assert_eq!(e.pos_nr(), 1);
    e.set_loop_position(false);
    for _ in 0..(50 * 40) {
        e.render_block(&mut buf);
    }
    assert_ne!(e.pos_nr(), 1, "released: the song moves on");
}

/// Pause and resume on the player: the clock does not move while paused and
/// the render carries on from the same sample, so the pause is a gap and
/// nothing else.
#[test]
fn pause_and_resume_is_only_a_gap() {
    for name in ["karma.ahx", "sunspots.hvl"] {
        for (hifi, capture) in [(false, false), (true, true)] {
            let mk = || {
                let mut p = AhxPlayer::new(&bytes(name), RATE, 2).unwrap();
                p.set_hifi(hifi);
                p.enable_capture(capture);
                p.play();
                p
            };
            let run =
                |p: &mut AhxPlayer, frames: usize, blocks: &[usize]| -> (Vec<f32>, Vec<f32>) {
                    let (mut l, mut r) = (Vec::new(), Vec::new());
                    let (mut done, mut k) = (0, 0);
                    while done < frames {
                        let n = blocks[k % blocks.len()].min(frames - done);
                        let (mut bl, mut br) = (vec![0f32; n], vec![0f32; n]);
                        p.render(&mut bl, &mut br);
                        l.extend(bl);
                        r.extend(br);
                        done += n;
                        k += 1;
                    }
                    (l, r)
                };

            let (first, second, gap) = (RATE as usize * 3 + 71, RATE as usize * 2, 128 * 300 + 5);
            let mut straight = mk();
            let (wl, wr) = run(&mut straight, first + second, &BLOCKS);

            let mut paused = mk();
            let (mut gl, mut gr) = run(&mut paused, first, &BLOCKS);
            let at = (paused.position(), paused.row(), paused.ticks());
            paused.pause();
            let (silence_l, silence_r) = run(&mut paused, gap, &[128, 999]);
            assert!(
                silence_l.iter().chain(&silence_r).all(|&s| s == 0.0),
                "{name}: paused output is silent"
            );
            assert_eq!(
                (paused.position(), paused.row(), paused.ticks()),
                at,
                "{name}: the clock moved while paused"
            );
            paused.play();
            let (rl, rr) = run(&mut paused, second, &[333, 128, 1]);
            gl.extend(rl);
            gr.extend(rr);
            assert!(
                gl == wl && gr == wr,
                "{name} hifi={hifi}: resumed render differs from the uninterrupted one"
            );
        }
    }
}

#[test]
fn player_seek_keeps_the_play_state_and_reports_position() {
    let mut p = AhxPlayer::new(&bytes("karma.ahx"), RATE, 2).unwrap();
    let t = row_tops("karma.ahx", 2000)[25];
    assert_eq!(p.seek(t.pos, t.row), 1);
    assert!(!p.is_playing(), "a paused player stays paused");
    assert_eq!((p.position(), p.row() as usize), (t.pos as i32, t.row));
    p.play();
    assert_eq!(p.seek(0, 0), 1);
    assert!(p.is_playing(), "a playing player keeps playing");
    assert_eq!(p.seek(9999, 0), 0);
    p.set_loop_position(true);
    assert!(p.loop_position());
    p.restart(0);
    assert!(p.loop_position(), "restart keeps the loop setting");
}

/// `prewarm_hifi` walks every subsong through `init_subsong`; the subsong a
/// seek replays must still be the one playback is on afterwards. (blondie has
/// a second subsong, so a prewarm that left the walk's last one behind sent
/// every later seek to the wrong song.)
#[test]
fn hifi_prewarm_does_not_change_which_subsong_a_seek_replays() {
    let mut e = engine("blondie.ahx");
    assert!(
        e.song().subsong_nr > 0,
        "the test needs a song with a subsong"
    );
    e.set_hifi(true);
    e.prewarm_hifi();
    assert_eq!(e.seek(0, 0), Some(SeekKind::Exact));
    let mut plain = engine("blondie.ahx");
    plain.set_hifi(true);
    plain.prewarm_hifi();
    assert!(render(&mut e, RATE as usize, &BLOCKS) == render(&mut plain, RATE as usize, &BLOCKS));
}

/// Every fixture (ring modulation, filter sweeps, square sweeps, 16-voice HVLs
/// all turn up somewhere in the corpus), one deep target each, hi-fi on.
#[test]
fn seek_is_exact_across_the_whole_corpus() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/songs");
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".ahx") || n.ends_with(".hvl"))
        .collect();
    names.sort();
    assert!(names.len() >= 24, "corpus went missing: {names:?}");
    for name in names {
        let tops = row_tops(&name, 50 * 90);
        let t = tops[tops.len() * 2 / 3];
        let mut full = engine(&name);
        setup(&mut full, true, false);
        played_to(&mut full, t.ticks);
        let mut seeked = engine(&name);
        setup(&mut seeked, true, false);
        assert_eq!(
            seeked.seek(t.pos, t.row),
            Some(SeekKind::Exact),
            "{name} {t:?}"
        );
        assert!(
            render(&mut full, RATE as usize, &BLOCKS)
                == render(&mut seeked, RATE as usize, &BLOCKS),
            "{name} {t:?}"
        );
    }
}
