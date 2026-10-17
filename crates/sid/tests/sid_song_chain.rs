//! S3 end-to-end proof (`.ai/plan-sid-tracking.md` S3): a SID song made in the
//! app, through the app's real chain, plays on the real S1/S2 chip.
//!
//! `fixtures/sid/s3-chain.asid` is not hand-written: it is the SID song file
//! the app's chain saved (`src/tests/sid-format-chain.test.ts`: a SidDoc built
//! with the doc's ops -> a real tracker store -> the real .cmod save ->
//! the real .cmod load into a second store -> that store's save), and that
//! test fails unless its output equals this file byte for byte. The song's
//! layout is documented in `src/tests/helpers/sid-chain-song.ts`; every
//! expected value below is derived from it and the player's documented
//! semantics (`src/sid/player.rs` header), in the comment above each check.
//!
//! Timing: tempo 6 at the PAL frame rate (50.1245 Hz), so row r starts at
//! frame 6r, and a frame is 879.8 samples at 44.1 kHz (879 or 880).
//! `frame()` renders exactly one frame, whose register writes land before
//! its samples.

use cubed_sid::envelope::Stage;
use cubed_sid::{gt_note_freq_reg, SidModel, SidSong, SidSongPlayer, DEFAULT_SAMPLE_RATE};

const FIXTURE: &[u8] = include_bytes!("fixtures/sid/s3-chain.asid");
/// One PAL frame at 44.1 kHz is 879.8 samples (`player::frame_cycles`, GT
/// parity 0925b): a buffer that holds one; `samples_in_next_frame` says how
/// much of it a frame is.
const SPF: usize = 880;

fn song() -> SidSong {
    SidSong::parse(FIXTURE).expect("the app's file parses")
}

fn frame(p: &mut SidSongPlayer) -> Vec<f32> {
    let mut out = vec![0.0f32; SPF];
    let n = p.samples_in_next_frame();
    p.render(&mut out[..n]);
    out.truncate(n);
    out
}

/// Plays `n` frames, calling `probe(frame index, player)` after each.
fn play(p: &mut SidSongPlayer, n: usize, mut probe: impl FnMut(usize, &SidSongPlayer)) -> Vec<f32> {
    let mut all = Vec::with_capacity(n * SPF);
    for i in 0..n {
        all.extend(frame(p));
        probe(i, p);
    }
    all
}

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

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

#[test]
fn the_apps_file_round_trips_byte_exact_in_rust() {
    let s = song();
    // Rust writes back exactly the bytes the app wrote.
    assert_eq!(s.to_bytes(), FIXTURE);
    // What the app's doc holds (sid-chain-song.ts).
    assert_eq!(s.name, b"S3 chain proof");
    assert_eq!(s.author, b"OpenClaw");
    assert_eq!(s.copyright, b"2026");
    assert_eq!(
        s.model,
        SidModel::Sid6581,
        "the per-song tag set by setSidChipModel"
    );
    assert_eq!((s.channels, s.speed_multiplier, s.tempo), (3, 1, 6));
    assert_eq!(s.subsongs.len(), 2);
    assert_eq!(
        s.patterns.iter().map(|p| p.rows.len()).collect::<Vec<_>>(),
        vec![32, 32, 16]
    );
    let names: Vec<&[u8]> = s.instruments.iter().map(|i| i.name.as_slice()).collect();
    assert_eq!(
        names,
        vec![&b"Tri lead"[..], b"Arp pulse", b"Filt saw", b"Vib lead"]
    );
    let t = &s.tables;
    assert_eq!(
        (t.wave.len(), t.pulse.len(), t.filter.len(), t.speed.len()),
        (6, 4, 4, 2)
    );
    assert_eq!(s.subsongs[0].orderlists[2].entries[0].transpose, 5);
    assert_eq!(s.subsongs[0].orderlists[2].entries[0].repeat, 2);
}

#[test]
fn the_song_plays_on_a_chip_of_its_own_model_frame_by_frame() {
    let mut p = SidSongPlayer::new(song(), DEFAULT_SAMPLE_RATE).unwrap();
    assert_eq!(p.chip().model(), SidModel::Sid6581);
    // A PAL frame, 19 656 cycles (was a flat 882 at 50 Hz).
    assert!((p.samples_per_frame() - 879.809).abs() < 1e-3);

    let (c4, e4, g4) = (
        gt_note_freq_reg(48),
        gt_note_freq_reg(52),
        gt_note_freq_reg(55),
    );
    let a3 = gt_note_freq_reg(45);
    let c5 = gt_note_freq_reg(60);
    let mut checked = 0;
    let out = play(&mut p, 192, |f, p| {
        let chip = p.chip();
        let (v1, v2, v3) = (chip.voice(0), chip.voice(1), chip.voice(2));
        match f {
            // Row 0: voice 1 A-4 on "Tri lead" (triangle + gate); voices 2-3 silent (no waveform, no gate).
            0 => {
                assert_eq!(v1.frequency(), 7494);
                assert_eq!(v1.control(), 0x11);
                assert_eq!((v2.control(), v3.control()), (0, 0));
            }
            // Row 8 = frame 48: voice 2 C-4 "Arp pulse": its first-frame byte
            // $41 (pulse + gate) and no pitch yet (GT leaves the pitch to the
            // wave table, gplay.c:509-512; a fresh channel's is 0); from frame
            // 49 the wave table walks C, E, G, jumps back, one row a frame.
            48 => assert_eq!((v2.frequency(), v2.control()), (0, 0x41)),
            49..=54 => {
                let arp = [c4, e4, g4][(f - 49) % 3];
                assert_eq!((v2.frequency(), v2.control()), (arp, 0x41), "frame {f}");
            }
            _ => {}
        }
        // The pulse table: the note's frame skips it as GT's does (S5.19), so
        // a fresh channel's width 0 there; the table's 0x400 on frame 49,
        // then +0x10 a frame for 32 frames (0x600 at frame 81), then -0x10
        // (0x5F0 at frame 82).
        match f {
            48 => assert_eq!(v2.pulse_width(), 0),
            49..=53 => assert_eq!(
                v2.pulse_width(),
                0x400 + 0x10 * (f as u16 - 49),
                "frame {f}"
            ),
            81 => assert_eq!(v2.pulse_width(), 0x600),
            82 => assert_eq!(v2.pulse_width(), 0x5F0),
            _ => {}
        }
        // Row 10 = frame 60: voice 3 E-3 + 5 = A-3 on "Filt saw"; its filter
        // table, heard from the frame after the note's (the filter registers
        // are the frame's first, GT's order, S5.19): LP, $17 = 0xC4 (resonance
        // 12, voice 3 routed) and, on the same frame 61, the cutoff-set row
        // after it (GT combines them, gplay.c:271-275: 0x20<<3), then +2<<3 a
        // frame for 64 frames from frame 62 (0x500 at 125), then the table stops.
        match f {
            60 => assert_eq!((v3.frequency(), v3.control()), (a3, 0x21)),
            61 => {
                let fl = chip.filter();
                assert_eq!(
                    (fl.mode() & 0x70, fl.resonance(), fl.cutoff_reg()),
                    (0x10, 12, 0x100)
                );
            }
            62..=64 => assert_eq!(
                chip.filter().cutoff_reg(),
                0x100 + 0x10 * (f as u16 - 61),
                "frame {f}"
            ),
            124 => assert_eq!(chip.filter().cutoff_reg(), 0x4F0),
            125 | 126 | 131 => assert_eq!(chip.filter().cutoff_reg(), 0x500, "frame {f}"),
            _ => {}
        }
        // Row 16 = frame 96: key off; the envelope releases.
        if f == 96 {
            assert_eq!(v1.control(), 0x10);
            assert_eq!(v1.envelope_stage(), Stage::Release);
        }
        // Row 20 = frame 120: C-5 on "Vib lead": first-frame waveform 0x09
        // (test + gate) at voice 1's old pitch (A-4: the wave table sets the
        // note), then wave row 5, triangle + gate at C-5. S5.10, GoatTracker's vibrato
        // (gplay.c:767-800): delay 10 counts down on the tick-N frames that
        // run effects only (122-125, 127-131: 126 is tick 0, and 121's wave
        // step set the note, which ends GT's frame before them, gplay.c:722),
        // swings from 133 (132 is tick 0) at 0x28 a frame, turn value 4: 3
        // frames up, then 6 each way, tick-0 frames (138, 144) holding.
        match f {
            120 => assert_eq!((v1.frequency(), v1.control()), (7494, 0x09)),
            121..=132 => assert_eq!((v1.frequency(), v1.control()), (c5, 0x11), "frame {f}"),
            133..=144 => {
                let off = [40i32, 80, 120, 80, 40, 40, 0, -40, -80, -120, -80, -80][f - 133];
                assert_eq!(v1.frequency() as i32, c5 as i32 + off, "frame {f}");
            }
            _ => {}
        }
        // Row 28 = frame 168: porta up at speed-table row 2 (0x0040) from
        // where the vibrato left the register (S5.10: GT's vibrato moves the
        // frequency itself; +0 at frame 167). Tick 0 (168) holds and ticks
        // 1-5 slide (S5.12: GT skips every row's tick-0 effects, gplay.c:728).
        // Row 29 has command 0: tick 0 (174) holds, then the instrument
        // vibrato runs on (GT's command 0 falls through to it,
        // gplay.c:767-772; the portamento restarted its phase, gplay.c:413):
        // +0x28 at 175.
        let slid = c5 + 0x40 * 5;
        match f {
            168..=173 => assert_eq!(p.channel_freq(0), c5 + 0x40 * (f as u16 - 168), "frame {f}"),
            174 => assert_eq!(p.channel_freq(0), slid),
            175 => assert_eq!(p.channel_freq(0), slid + 0x28),
            _ => {}
        }
        // Row 31, tick 6 - 2 = frame 190: "Vib lead"'s gate timer (2) sees
        // the next row (the pattern's loop to row 0) trigger A-4, so the gate
        // is cleared early (hard restart).
        if f == 190 {
            assert_eq!(v1.control() & 0x01, 0);
            assert_eq!(v1.envelope_stage(), Stage::Release);
        }
        checked += 1;
    });
    assert_eq!(checked, 192);

    // Row 26 = the repeat of voice 3's pattern: A-3 again. 32 rows = 192
    // frames: every orderlist has wrapped, all three back at entry 0 row 0.
    assert!(p.looped());
    for c in 0..3 {
        assert_eq!(p.position(c), (0, 0));
    }
    assert_eq!(p.frames(), 192);

    // It sounds: finite, in range, not silent.
    assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
    assert!(rms(&out) > 0.01, "rms {}", rms(&out));
    // Rows 0-7 are voice 1 alone: its A-4 (7493 * 985248 / 2^24 = 440.03 Hz)
    // dominates a semitone away (466.16 Hz) by far.
    let solo = &out[5 * SPF..48 * SPF];
    assert!(power(solo, 440.03) > 100.0 * power(solo, 466.16));
}

#[test]
fn both_chip_models_play_it_and_they_differ() {
    let render = |model| {
        let mut p = SidSongPlayer::with_model(song(), model, DEFAULT_SAMPLE_RATE, 0).unwrap();
        assert_eq!(p.chip().model(), model);
        let mut out = vec![0.0f32; 192 * SPF];
        p.render(&mut out);
        out
    };
    let a = render(SidModel::Sid6581);
    let b = render(SidModel::Sid8580);
    assert!(rms(&a) > 0.01 && rms(&b) > 0.01);
    assert!(a.iter().zip(&b).any(|(x, y)| (x - y).abs() > 1e-3));
}

#[test]
fn subsong_one_plays_its_own_orderlists() {
    // Subsong 1: voice 1 plays P1 (silent until row 8), voice 3 plays P0 up
    // an octave: A-4 + 12 = A-5 on frame 0.
    let mut p =
        SidSongPlayer::with_model(song(), SidModel::Sid6581, DEFAULT_SAMPLE_RATE, 1).unwrap();
    frame(&mut p);
    assert_eq!(p.chip().voice(2).frequency(), gt_note_freq_reg(69));
    assert_eq!(p.chip().voice(2).control(), 0x11);
    assert_eq!(p.chip().voice(0).control(), 0);
}
