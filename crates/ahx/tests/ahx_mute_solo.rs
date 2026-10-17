//! Per-voice mute/solo (`AhxEngine::set_mute_solo`): with nothing set the mix
//! is untouched (the corpus-wide proof is the render goldens, which also run
//! a set-then-cleared engine); with something set a silenced voice is exactly
//! absent from the mix and nothing else about the song changes.

use cubed_ahx::engine::{AhxEngine, CAPTURE_FRAMES};
use cubed_ahx::format;
use cubed_ahx::player::AhxPlayer;
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

fn snapshot(e: &AhxEngine, voice: usize) -> Vec<i16> {
    let mut s = vec![0i16; CAPTURE_FRAMES];
    assert_eq!(e.read_channel_snapshot(voice, &mut s), CAPTURE_FRAMES);
    s
}

const SONGS: [&str; 2] = ["karma.ahx", "sunspots.hvl"];

#[test]
fn nothing_muted_or_soloed_is_bit_identical() {
    for name in SONGS {
        let frames = RATE as usize * 6;
        let plain = render(&mut engine(name), frames, &BLOCKS);

        // Explicitly set to "nothing".
        let mut zero = engine(name);
        zero.set_mute_solo(0, 0);
        assert_eq!(
            render(&mut zero, frames, &BLOCKS),
            plain,
            "{name}: explicit 0/0"
        );

        // Bits above the song's channels change nothing either.
        let mut high = engine(name);
        high.set_mute_solo(0xffff_0000, 0);
        assert_eq!(
            render(&mut high, frames, &BLOCKS),
            plain,
            "{name}: mute bits past the channels"
        );

        // Set and cleared before the first sample.
        let mut cleared = engine(name);
        cleared.set_mute_solo(0xffff, 0xff);
        cleared.set_mute_solo(0, 0);
        assert_eq!(
            render(&mut cleared, frames, &BLOCKS),
            plain,
            "{name}: set then cleared"
        );
    }
}

#[test]
fn muting_every_voice_is_digital_silence() {
    for name in SONGS {
        let mut e = engine(name);
        let all = (1u32 << e.channels()) - 1;
        e.set_mute_solo(all, 0);
        e.enable_capture(true);
        let out = render(&mut e, RATE as usize * 4, &BLOCKS);
        assert!(
            out.iter().all(|&s| s == 0),
            "{name}: mute-all is not silent"
        );
        for v in 0..e.channels() {
            assert!(
                snapshot(&e, v).iter().all(|&s| s == 0),
                "{name}: voice {v} scope not flat"
            );
        }
    }
}

#[test]
fn soloing_a_voice_that_is_also_muted_is_silence_and_soloing_nothing_real_too() {
    let mut e = engine("karma.ahx");
    e.set_mute_solo(0b0010, 0b0010);
    assert!(render(&mut e, 20000, &BLOCKS).iter().all(|&s| s == 0));
    // Solo on a voice the song does not have: every real voice is non-solo.
    let mut e = engine("karma.ahx");
    e.set_mute_solo(0, 1 << 9);
    assert!(render(&mut e, 20000, &BLOCKS).iter().all(|&s| s == 0));
}

/// `L = ((j * panl) >> 7) * mixgain >> 8` for one voice's contribution `j`.
fn one_voice_mix(j: i32, pan: i32, mixgain: i32) -> i32 {
    (((j * pan) >> 7).wrapping_mul(mixgain) >> 8).clamp(-0x8000, 0x7fff)
}

#[test]
fn solo_one_is_exactly_that_voices_captured_contribution() {
    for name in SONGS {
        let plain_engine = engine(name);
        let n = plain_engine.channels();
        for solo in 0..n {
            // Reference run: everything audible, capture on. Ten whole ticks,
            // so the run ends on a tick edge and the pan values are the last
            // tick's (pan and volume are per-tick).
            let mut plain = engine(name);
            plain.enable_capture(true);
            let tick = plain.samples_per_tick();
            let plain_out = render(&mut plain, tick * 10, &BLOCKS);

            let mut soloed = engine(name);
            soloed.enable_capture(true);
            soloed.set_mute_solo(0, 1 << solo);
            let solo_out = render(&mut soloed, tick * 10, &BLOCKS);

            // Song timing and voice state are untouched by the gate.
            assert_eq!(
                (soloed.pos_nr(), soloed.note_nr(), soloed.ticks_played()),
                (plain.pos_nr(), plain.note_nr(), plain.ticks_played()),
                "{name} solo {solo}: transport"
            );

            // The soloed voice's trace is what it was with everyone playing;
            // every other voice's is flat.
            assert_eq!(
                snapshot(&soloed, solo),
                snapshot(&plain, solo),
                "{name} solo {solo}: its capture"
            );
            for v in (0..n).filter(|&v| v != solo) {
                assert!(
                    snapshot(&soloed, v).iter().all(|&s| s == 0),
                    "{name} solo {solo}: voice {v} not flat"
                );
            }

            // And the mix is that contribution, panned and gained, and nothing else.
            let snap = snapshot(&plain, solo);
            let (panl, panr) = (
                plain.voice(solo).pan_mult_left,
                plain.voice(solo).pan_mult_right,
            );
            let g = plain.mix_gain();
            let tail = &solo_out[solo_out.len() - tick * 2..];
            let snap_tail = &snap[CAPTURE_FRAMES - tick..];
            for f in 0..tick {
                let j = snap_tail[f] as i32;
                assert_eq!(
                    tail[f * 2] as i32,
                    one_voice_mix(j, panl, g),
                    "{name} solo {solo} L frame {f}"
                );
                assert_eq!(
                    tail[f * 2 + 1] as i32,
                    one_voice_mix(j, panr, g),
                    "{name} solo {solo} R frame {f}"
                );
            }
            assert_eq!(plain_out.len(), solo_out.len());
        }
    }
}

#[test]
fn a_solo_run_is_the_all_voices_run_minus_the_others_for_the_whole_song() {
    // Stronger than one tick: rendering with everything but voice k muted is
    // the same audio as soloing voice k (two spellings of one state), and it
    // differs from the full mix (something else was playing).
    let name = "karma.ahx";
    let frames = RATE as usize * 3;
    let full = render(&mut engine(name), frames, &BLOCKS);
    let mut a = engine(name);
    a.set_mute_solo(0, 0b0001);
    let solo = render(&mut a, frames, &BLOCKS);
    let mut b = engine(name);
    b.set_mute_solo(0b1110, 0);
    assert_eq!(render(&mut b, frames, &BLOCKS), solo);
    assert_ne!(solo, full);
}

#[test]
fn unmuting_restores_identical_samples() {
    for name in SONGS {
        let frames = RATE as usize * 6;
        let plain = render(&mut engine(name), frames, &BLOCKS);

        // Mute everything for a stretch (at block edges that are not tick
        // edges), then release: the samples after must be the untouched
        // engine's, because a silenced voice keeps running.
        let mut e = engine(name);
        let all = (1u32 << e.channels()) - 1;
        let (a, b) = (RATE as usize, RATE as usize * 3 + 17);
        let mut got = render(&mut e, a, &BLOCKS);
        e.set_mute_solo(all, 0);
        let muted = render(&mut e, b - a, &BLOCKS);
        assert!(muted.iter().all(|&s| s == 0));
        e.set_mute_solo(0, 0);
        got.extend(muted);
        got.extend(render(&mut e, frames - b, &BLOCKS));

        assert_eq!(got[..a * 2], plain[..a * 2], "{name}: before the mute");
        assert_eq!(got[b * 2..], plain[b * 2..], "{name}: after the un-mute");

        // Same for solo -> clear, and for a rewind, which keeps the state.
        let mut e = engine(name);
        e.set_mute_solo(0, 0b0010);
        render(&mut e, 5000, &BLOCKS);
        assert_eq!(e.mute_solo(), (0, 0b0010));
        assert!(e.init_subsong(0));
        assert_eq!(
            e.mute_solo(),
            (0, 0b0010),
            "{name}: a rewind keeps mute/solo"
        );
        e.set_mute_solo(0, 0);
        let restarted = render(&mut e, 30000, &BLOCKS);
        assert_eq!(
            restarted[..],
            plain[..30000 * 2],
            "{name}: cleared after a rewind is a plain start"
        );
    }
}

#[test]
fn player_exposes_mute_solo() {
    let mut p = AhxPlayer::new(&bytes("karma.ahx"), RATE, 2).unwrap();
    p.play();
    p.set_mute_solo(0b1111, 0);
    let (mut l, mut r) = ([0f32; 512], [0f32; 512]);
    for _ in 0..100 {
        p.render(&mut l, &mut r);
        assert!(l.iter().chain(r.iter()).all(|&s| s == 0.0));
    }
    p.set_mute_solo(0, 0);
    let mut heard = false;
    for _ in 0..100 {
        p.render(&mut l, &mut r);
        heard |= l.iter().any(|&s| s != 0.0);
    }
    assert!(heard);
}
