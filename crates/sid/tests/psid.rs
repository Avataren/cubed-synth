//! `.sid` playback end to end: the synthetic tunes of `tests/fixtures/psid/`
//! (made by `make_fixtures.py`: no real C64 tune is ever committed), and, with
//! the environment variables below, the DAW's own runner as an oracle and a
//! smoke run over a corpus of real tunes.
//!
//!   CUBED_PSID=<dir>            a folder of `.sid` files, searched recursively
//!   CUBED_PSID_SECONDS=10       how long each tune runs (default 10)
//!   CUBED_PSID_STRIDE=1         every n-th file only (default 1: all of them)
//!   CUBED_PSID_AUDIO=25         render every n-th tune for real (default 25)
//!   CUBED_PSID_ORACLE=<json>    what `fixtures/psid/oracle.ts` recorded for a list of tunes
//!
//! cargo test --release -p cubed-sid --test psid -- --nocapture

use cubed_sid::psid::{EndReason, PsidFile, PsidOptions, PsidPlayer, PsidRunner, RunMode};
use cubed_sid::{Chip, SidModel};
use std::path::{Path, PathBuf};

const ARP: &[u8] = include_bytes!("fixtures/psid/arp.sid");
const IRQ: &[u8] = include_bytes!("fixtures/psid/irq.sid");
const RASTER: &[u8] = include_bytes!("fixtures/psid/raster.sid");
const TWO: &[u8] = include_bytes!("fixtures/psid/two.sid");
const CIA2X: &[u8] = include_bytes!("fixtures/psid/cia2x.sid");

const RATE: f64 = 44_100.0;
const FRAME: u64 = 312 * 63;

type W = (u64, u8, u8);

/// The first chip's writes in the first `cycles` cycles.
fn writes(runner: &mut PsidRunner, cycles: u64) -> Vec<W> {
    runner.advance(cycles);
    let mut out = vec![];
    runner.drain(cycles, 1, |w| out.push((w.cycle, w.reg, w.value)));
    out
}

fn runner_of(bytes: &[u8], subsong: usize) -> PsidRunner {
    PsidRunner::new(&PsidFile::parse(bytes).unwrap(), subsong).unwrap()
}

fn power(x: &[f32], hz: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * hz / RATE;
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
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

fn render(player: &mut PsidPlayer, n: usize, block: usize) -> Vec<f32> {
    let mut out = vec![0f32; n];
    for chunk in out.chunks_mut(block) {
        player.render(chunk);
    }
    out
}

// --- The synthetic tunes: what they write, and when ---------------------------

#[test]
fn the_fixtures_parse_as_what_they_are() {
    let arp = PsidFile::parse(ARP).unwrap();
    assert_eq!(
        (arp.songs, arp.start_song, arp.play_address),
        (2, 1, 0x1026)
    );
    assert_eq!(arp.name, "Arp (test)");
    let irq = PsidFile::parse(IRQ).unwrap();
    assert_eq!(irq.kind, cubed_sid::psid::PsidType::Rsid);
    assert_eq!(irq.play_address, 0);
    assert_eq!(PsidFile::parse(TWO).unwrap().extra_sids, [0xd420]);
    assert!(PsidFile::parse(CIA2X).unwrap().uses_cia(1));
    assert!(PsidFile::parse(RASTER).is_ok());
}

#[test]
fn the_arpeggio_writes_what_its_code_says_at_the_cycles_its_instructions_take() {
    let mut r = runner_of(ARP, 0);
    assert_eq!(r.mode(), RunMode::Play);
    // What init wrote is the state at cycle 0, in order.
    let mut init = vec![];
    r.drain(0, 1, |w| init.push((w.cycle, w.reg, w.value)));
    assert_eq!(
        init,
        [
            (0, 0x18, 0x0f),
            (0, 0x05, 0x09),
            (0, 0x06, 0xf0),
            (0, 0x03, 0x08)
        ]
    );
    let w = writes(&mut r, 20 * FRAME);
    // Frame 0 starts a note: LDA zp 3, AND # 2, BNE not taken 2, LDX zp 3, LDA abs,X 4 -> STA $D400 starts at 14;
    // LDA abs,X 4 -> STA $D401 at 22; LDA # 2 -> STA $D404 at 28.
    assert_eq!(
        w[..3],
        [(14, 0x00, 0xe0), (22, 0x01, 0x0b), (28, 0x04, 0x21)]
    );
    // Frame 6: LDA zp 3, AND # 2, BNE taken 3, CMP # 2, BNE not taken 2, LDA # 2 -> STA $D404 at 14: gate off.
    assert_eq!(w[3], (6 * FRAME + 14, 0x04, 0x20));
    // Frame 8: the next note; frame 16: the third; frame 14 and 22: gate off.
    assert_eq!(
        w[4..7],
        [
            (8 * FRAME + 14, 0x00, 0x7d),
            (8 * FRAME + 22, 0x01, 0x0e),
            (8 * FRAME + 28, 0x04, 0x21)
        ]
    );
    assert_eq!(w[7], (14 * FRAME + 14, 0x04, 0x20));
    assert_eq!(
        w[8..11],
        [
            (16 * FRAME + 14, 0x00, 0xba),
            (16 * FRAME + 22, 0x01, 0x11),
            (16 * FRAME + 28, 0x04, 0x21)
        ]
    );
    assert_eq!(w.len(), 11, "and nothing else in 20 frames: {w:?}");
}

#[test]
fn the_second_subsong_plays_its_own_notes_and_wraps_around_them() {
    let mut r = runner_of(ARP, 1);
    let w = writes(&mut r, 30 * FRAME);
    let notes: Vec<_> = w.iter().filter(|w| w.1 == 0).map(|w| w.2).collect();
    assert_eq!(
        notes,
        [0xc0, 0x2c, 0x85, 0xc0],
        "subsong 2: notes 3 to 5, then round again"
    );
    // The first subsong wraps too.
    let notes: Vec<_> = writes(&mut runner_of(ARP, 0), 30 * FRAME)
        .iter()
        .filter(|w| w.1 == 0)
        .map(|w| w.2)
        .collect();
    assert_eq!(notes, [0xe0, 0x7d, 0xba, 0xe0]);
}

#[test]
fn the_cia_interrupt_tune_fires_every_20001_cycles_through_the_kernal_vector() {
    let mut r = runner_of(IRQ, 0);
    assert_eq!(r.mode(), RunMode::Irq);
    let w = writes(&mut r, 400_000);
    let steps: Vec<_> = w.iter().filter(|w| w.1 == 1 && w.0 > 0).collect();
    assert!(steps.len() >= 19, "{} handler runs", steps.len());
    assert_eq!(
        steps.iter().map(|w| w.2).take(5).collect::<Vec<_>>(),
        [8, 16, 24, 32, 40]
    );
    for pair in steps.windows(2) {
        assert_eq!(
            pair[1].0 - pair[0].0,
            20_001,
            "the timer's period is its latch + 1"
        );
    }
    // Init's own writes came first, at cycle 0 or soon after.
    assert!(w.iter().take(3).all(|w| w.0 < 1000));
    assert_eq!(r.ended(), None);
}

#[test]
fn the_raster_interrupt_tune_fires_once_a_frame_from_line_100() {
    let mut r = runner_of(RASTER, 0);
    let w = writes(&mut r, 10 * FRAME);
    let hits: Vec<_> = w.iter().filter(|w| w.1 == 0).collect();
    assert!(hits.len() >= 9, "{hits:?}");
    assert_eq!(
        hits.iter().map(|w| w.2).take(4).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    for pair in hits.windows(2) {
        assert_eq!(pair[1].0 - pair[0].0, FRAME);
    }
    // Line 100 starts on cycle 6300; entering the interrupt takes 7 cycles; the stand-in KERNAL's entry (PHA, TXA, PHA,
    // TYA, PHA, TSX, LDA abs,X, AND #, BEQ taken, JMP (ind)) 3+2+3+2+3+2+4+2+3+5 = 29; the handler's LDA #, STA abs, INC zp,
    // LDA zp is 2+4+5+3 = 14 before the STA $D400.
    let offset = hits[0].0 % FRAME;
    assert_eq!(offset, 6300 + 7 + 29 + 14);
}

#[test]
fn a_two_sid_tune_writes_to_each_chip() {
    let file = PsidFile::parse(TWO).unwrap();
    let mut r = PsidRunner::new(&file, 0).unwrap();
    assert_eq!(r.sid_count(), 2);
    r.advance(5 * FRAME);
    let mut by_chip = [vec![], vec![]];
    r.drain(5 * FRAME, 2, |w| {
        by_chip[w.chip as usize].push((w.reg, w.value))
    });
    assert_eq!(
        by_chip[0],
        [
            (0x18, 0x0f),
            (0x06, 0xf0),
            (0x04, 0x21),
            (1, 1),
            (1, 2),
            (1, 3),
            (1, 4),
            (1, 5)
        ]
    );
    assert_eq!(
        by_chip[1],
        [
            (0x18, 0x0f),
            (0x06, 0xf0),
            (0x04, 0x21),
            (1, 0xfe),
            (1, 0xfd),
            (1, 0xfc),
            (1, 0xfb),
            (1, 0xfa)
        ]
    );
    assert_eq!(r.extra_sid_writes(), 0);
    // Played with a chip each: both are heard; asked for one, the second's writes are counted, not played.
    let mut p = PsidPlayer::new(TWO, RATE, PsidOptions::default()).unwrap();
    assert_eq!(p.sid_count(), 2);
    let _ = render(&mut p, 4410, 128);
    assert_eq!(p.extra_sid_writes(), 0);
}

#[test]
fn a_cia_timed_tune_ticks_on_the_timers_period() {
    let mut r = runner_of(CIA2X, 0);
    assert_eq!(r.tick_cycles(), 9830);
    let w = writes(&mut r, 20 * 9830);
    let d400: Vec<_> = w.iter().filter(|w| w.1 == 0).collect();
    assert_eq!(d400.len(), 20);
    for pair in d400.windows(2) {
        assert_eq!(pair[1].0 - pair[0].0, 9830);
    }
}

// --- The player: audio ------------------------------------------------------------

/// The reference: the writes the runner makes, scheduled on a bare chip all at once, rendered in one block.
fn reference(bytes: &[u8], subsong: usize, model: SidModel, samples: usize) -> Vec<f32> {
    let file = PsidFile::parse(bytes).unwrap();
    let mut r = PsidRunner::new(&file, subsong).unwrap();
    let mut chip = Chip::with_sample_rate(model, RATE).unwrap();
    chip.set_clock_hz(r.clock_hz() as f64);
    let total = chip.cycles_in_next(samples);
    r.advance(total);
    chip.reserve_writes(10_000);
    r.drain(total, 1, |w| chip.write_after(w.cycle, w.reg, w.value));
    let mut out = vec![0f32; samples];
    chip.render(&mut out);
    out
}

#[test]
fn the_player_can_move_to_the_audio_thread() {
    fn send<T: Send>() {}
    send::<PsidPlayer>();
    send::<PsidRunner>();
}

#[test]
fn two_chips_are_mixed_as_their_average() {
    let file = PsidFile::parse(TWO).unwrap();
    let mut r = PsidRunner::new(&file, 0).unwrap();
    let mut chips = [
        Chip::with_sample_rate(SidModel::Sid6581, RATE).unwrap(),
        Chip::with_sample_rate(SidModel::Sid6581, RATE).unwrap(),
    ];
    let samples = 30_000;
    let total = chips[0].cycles_in_next(samples);
    r.advance(total);
    for c in &mut chips {
        c.reserve_writes(10_000);
        c.set_clock_hz(r.clock_hz() as f64);
    }
    r.drain(total, 2, |w| {
        chips[w.chip as usize].write_after(w.cycle, w.reg, w.value)
    });
    let (mut a, mut b) = (vec![0f32; samples], vec![0f32; samples]);
    chips[0].render(&mut a);
    chips[1].render(&mut b);
    // The first chip alone is not the mix: the second contributes.
    assert!(rms(&b) > 0.01 && rms(&a) > 0.01);
    let want: Vec<f32> = a.iter().zip(&b).map(|(x, y)| (x + y) * 0.5).collect();
    let mut p = PsidPlayer::new(TWO, RATE, PsidOptions::default()).unwrap();
    let got = render(&mut p, samples, 128);
    assert!(got == want, "the player's mix is (chip 1 + chip 2) / 2");
}

#[test]
fn the_player_renders_exactly_what_the_chip_does_with_the_same_writes() {
    for (bytes, subsong) in [(ARP, 0), (ARP, 1), (IRQ, 0), (RASTER, 0), (CIA2X, 0)] {
        let want = reference(bytes, subsong, SidModel::Sid6581, 44_100);
        for block in [1usize, 7, 128, 4410, 44_100] {
            let mut p = PsidPlayer::new(
                bytes,
                RATE,
                PsidOptions {
                    subsong: Some(subsong),
                    model: None,
                },
            )
            .unwrap();
            let got = render(&mut p, 44_100, block);
            assert!(
                got == want,
                "tune {subsong} rendered in blocks of {block} differs from the reference"
            );
        }
    }
}

#[test]
fn the_arpeggio_sounds_its_notes() {
    let mut p = PsidPlayer::new(
        ARP,
        RATE,
        PsidOptions {
            subsong: Some(0),
            model: Some(SidModel::Sid8580),
        },
    )
    .unwrap();
    let out = render(&mut p, 44_100, 128);
    assert!(out.iter().all(|x| x.is_finite()));
    assert!(peak(&out) > 0.05 && peak(&out) < 1.0, "peak {}", peak(&out));
    // Frame 0..6 sound the first note (freq reg $0BE0 = 3040: 178.5 Hz), and the gate is off after frame 6.
    let hz = |reg: f64| reg * 985_248.0 / 16_777_216.0;
    let frame = (RATE * FRAME as f64 / 985_248.0) as usize;
    let first = &out[frame..6 * frame];
    let f0 = hz(3040.0);
    assert!(
        power(first, f0) > 5.0 * power(first, f0 * 1.19),
        "note 1 is {f0:.1} Hz"
    );
    assert!(power(first, f0) > 5.0 * power(first, f0 * 0.84));
    // Frames 8..14 sound the second note ($0E7D = 3709: 217.5 Hz).
    let second = &out[9 * frame..14 * frame];
    let f1 = hz(3709.0);
    assert!(
        power(second, f1) > 5.0 * power(second, f0),
        "note 2 is {f1:.1} Hz"
    );
    // The gate falls in frame 6; the release starts up to 32 ms late (the envelope's rate counter: the ADSR delay bug)
    // and the next note comes in frame 8: the envelope dips in between.
    let mut q = PsidPlayer::new(
        ARP,
        RATE,
        PsidOptions {
            subsong: Some(0),
            model: Some(SidModel::Sid8580),
        },
    )
    .unwrap();
    let _ = render(&mut q, 5 * frame, 128);
    assert_eq!(
        q.chip(0).voice(0).envelope_level(),
        255,
        "sustaining before the gate falls"
    );
    let mut lowest = 255;
    for _ in 0..(3 * frame / 32) {
        let _ = render(&mut q, 32, 32);
        lowest = lowest.min(q.chip(0).voice(0).envelope_level());
    }
    assert!(lowest < 255, "releasing after it");
    // The taps: only voice 1 plays.
    let mut p = PsidPlayer::new(ARP, RATE, PsidOptions::default()).unwrap();
    let (mut o, mut a, mut b, mut c) = (
        vec![0f32; 4410],
        vec![0f32; 4410],
        vec![0f32; 4410],
        vec![0f32; 4410],
    );
    p.render_taps(&mut o, [&mut a, &mut b, &mut c]);
    assert!(rms(&a) > 0.01 && rms(&b) < 1e-6 && rms(&c) < 1e-6);
}

#[test]
fn render_taps_matches_render_and_mute_silences() {
    let mut p1 = PsidPlayer::new(ARP, RATE, PsidOptions::default()).unwrap();
    let mut p2 = PsidPlayer::new(ARP, RATE, PsidOptions::default()).unwrap();
    let plain = render(&mut p1, 10_000, 128);
    let mut tapped = vec![0f32; 10_000];
    let (mut a, mut b, mut c) = (vec![0f32; 128], vec![0f32; 128], vec![0f32; 128]);
    for chunk in tapped.chunks_mut(128) {
        let n = chunk.len();
        p2.render_taps(chunk, [&mut a[..n], &mut b[..n], &mut c[..n]]);
    }
    assert!(plain == tapped);
    // (The output's DC blocker rings for a few milliseconds after a voice is cut: look at the tail.)
    let mut p = PsidPlayer::new(ARP, RATE, PsidOptions::default()).unwrap();
    p.set_mute_solo(1, 0);
    assert!(rms(&render(&mut p, 10_000, 128)[5000..]) < 1e-6);
    p.set_mute_solo(0, 0);
    assert!(rms(&render(&mut p, 10_000, 128)) > 0.01);
    p.set_mute_solo(0, 4); // only voice 3: silent here
    assert!(rms(&render(&mut p, 10_000, 128)[5000..]) < 1e-6);
    p.set_gain(0.0);
    p.set_mute_solo(0, 0);
    assert_eq!(rms(&render(&mut p, 1000, 128)), 0.0);
}

#[test]
fn stereo_render_is_the_mono_signal_twice() {
    let mut p1 = PsidPlayer::new(ARP, RATE, PsidOptions::default()).unwrap();
    let mut p2 = PsidPlayer::new(ARP, RATE, PsidOptions::default()).unwrap();
    let mono = render(&mut p1, 5000, 100);
    let (mut l, mut r) = (vec![0f32; 5000], vec![0f32; 5000]);
    for (cl, cr) in l.chunks_mut(100).zip(r.chunks_mut(100)) {
        p2.render_stereo(cl, cr);
    }
    assert!(l == mono && r == mono);
}

#[test]
fn the_chip_model_comes_from_the_file_or_the_options() {
    // The fixtures say 6581 (flags $14); the options override it.
    let p = PsidPlayer::new(ARP, RATE, PsidOptions::default()).unwrap();
    assert_eq!(p.chip(0).model(), SidModel::Sid6581);
    let p = PsidPlayer::new(
        ARP,
        RATE,
        PsidOptions {
            model: Some(SidModel::Sid8580),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(p.chip(0).model(), SidModel::Sid8580);
    // A file that says nothing is a 6581.
    let mut b = ARP.to_vec();
    b[0x77] = 0x00;
    let p = PsidPlayer::new(&b, RATE, PsidOptions::default()).unwrap();
    assert_eq!(p.chip(0).model(), SidModel::Sid6581);
    b[0x77] = 0x20; // 8580
    let p = PsidPlayer::new(&b, RATE, PsidOptions::default()).unwrap();
    assert_eq!(p.chip(0).model(), SidModel::Sid8580);
}

#[test]
fn ntsc_tunes_play_at_the_ntsc_clock() {
    let mut b = ARP.to_vec();
    b[0x77] = 0x18; // NTSC, 6581
    let p = PsidPlayer::new(&b, RATE, PsidOptions::default()).unwrap();
    assert_eq!(p.runner().clock_hz(), 1_022_727);
    let w = writes(
        &mut PsidRunner::new(&PsidFile::parse(&b).unwrap(), 0).unwrap(),
        10 * 263 * 65,
    );
    // After init's four writes at cycle 0: the note (14, 22, 28), then the gate-off in frame 6.
    assert_eq!(w[4].0, 14);
    assert_eq!(w[7].0, 6 * 263 * 65 + 14);
}

#[test]
fn restart_and_subsong_choice() {
    let mut p = PsidPlayer::new(ARP, RATE, PsidOptions::default()).unwrap();
    assert_eq!((p.subsong(), p.subsongs()), (0, 2));
    let first = render(&mut p, 20_000, 128);
    assert!(p.position_seconds() > 0.4);
    p.restart(None).unwrap();
    assert_eq!(p.frames(), 0);
    let again = render(&mut p, 20_000, 128);
    assert!(first == again, "a restart plays the same again");
    p.restart(Some(1)).unwrap();
    assert_eq!(p.subsong(), 1);
    let other = render(&mut p, 20_000, 128);
    assert!(other != first);
    // An out-of-range subsong is the last one.
    p.restart(Some(77)).unwrap();
    assert_eq!(p.subsong(), 1);
    let q = PsidPlayer::new(
        ARP,
        RATE,
        PsidOptions {
            subsong: Some(9),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(q.subsong(), 1);
}

#[test]
fn seeking_runs_the_tune_to_there_and_a_seek_back_starts_again() {
    let mut p = PsidPlayer::new(ARP, RATE, PsidOptions::default()).unwrap();
    p.seek(5.0).unwrap();
    assert!(
        (p.position_seconds() - 5.0).abs() < 0.01,
        "{}",
        p.position_seconds()
    );
    assert!((p.frames() as i64 - 5 * 44_100).abs() < 10);
    // It plays on from there: notes keep coming (the arpeggio is 8 frames a note).
    let out = render(&mut p, 44_100, 128);
    assert!(rms(&out) > 0.01);
    assert!((p.position_seconds() - 6.0).abs() < 0.01);
    p.seek(0.0).unwrap();
    assert_eq!(p.frames(), 0);
    assert!(p.position_seconds() < 0.001);
    let a = render(&mut p, 20_000, 128);
    p.restart(None).unwrap();
    assert!(
        a == render(&mut p, 20_000, 128),
        "seeking to 0 is a restart"
    );
    assert!(p.seek(-1.0).is_err());
    assert!(p.seek(1.0e9).is_err());
    assert!(p.seek(f64::NAN).is_err());
}

#[test]
fn a_tune_that_stops_reports_why_and_the_chip_goes_on_with_what_it_holds() {
    // init: set voice 1 to a sustained note; play: JAM.
    let code: [u8; 20] = [
        0xa9, 0x0f, 0x8d, 0x18, 0xd4, 0xa9, 0x40, 0x8d, 0x01, 0xd4, 0xa9, 0xf0, 0x8d, 0x06, 0xd4,
        0xa9, 0x11, 0x8d, 0x04, 0xd4,
    ];
    let mut data = code.to_vec();
    data.push(0x60); // rts: end of init
    data.push(0x02); // play: JAM
    let mut bytes = vec![0u8; 0x7c];
    bytes[..4].copy_from_slice(b"PSID");
    bytes[5] = 2;
    bytes[7] = 0x7c;
    bytes[8..10].copy_from_slice(&0x1000u16.to_be_bytes());
    bytes[10..12].copy_from_slice(&0x1000u16.to_be_bytes());
    bytes[12..14].copy_from_slice(&0x1015u16.to_be_bytes());
    bytes[15] = 1;
    bytes[17] = 1;
    bytes[0x77] = 0x14;
    bytes.extend_from_slice(&data);
    let mut p = PsidPlayer::new(&bytes, RATE, PsidOptions::default()).unwrap();
    assert_eq!(p.ended(), None);
    let out = render(&mut p, 44_100, 128);
    assert_eq!(
        p.ended(),
        Some(EndReason::PlayJam {
            addr: 0x1015,
            pc: 0x1015
        })
    );
    assert!(p.song_end_reached());
    assert!(rms(&out[40_000..]) > 0.01, "the held note goes on sounding");
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    assert!(PsidPlayer::new(b"", RATE, PsidOptions::default()).is_err());
    // A file cut inside its code: its init runs into zeros (BRK).
    assert!(PsidPlayer::new(&ARP[..0x80], RATE, PsidOptions::default()).is_err());
    assert!(PsidPlayer::new(ARP, 1.0, PsidOptions::default()).is_err());
    let mut garbage = ARP.to_vec();
    for b in garbage[0x7c..].iter_mut() {
        *b = 0xff;
    }
    // Code that is all $FF (ISC abs,X ...) runs without a panic; whether it jams is up to it.
    if let Ok(mut p) = PsidPlayer::new(&garbage, RATE, PsidOptions::default()) {
        let _ = render(&mut p, 44_100, 128);
    }
}

// --- A corpus of real tunes and the DAW's runner as an oracle -------------------------

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn sid_files(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("sid"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// What one tune did in the first `seconds`.
#[derive(Debug, Default)]
struct Outcome {
    refused: Option<String>,
    ended: Option<String>,
    writes: usize,
    silent: Option<bool>,
}

fn run_tune(path: &Path, seconds: f64, audio: bool) -> Outcome {
    let mut o = Outcome::default();
    let Ok(bytes) = std::fs::read(path) else {
        o.refused = Some("unreadable".into());
        return o;
    };
    let file = match PsidFile::parse(&bytes) {
        Ok(f) => f,
        Err(e) => {
            o.refused = Some(e.to_string());
            return o;
        }
    };
    if audio {
        let mut p = match PsidPlayer::from_file(file, RATE, PsidOptions::default()) {
            Ok(p) => p,
            Err(e) => {
                o.refused = Some(e.to_string());
                return o;
            }
        };
        let n = (seconds * RATE) as usize;
        let mut out = vec![0f32; n];
        for chunk in out.chunks_mut(512) {
            p.render(chunk);
        }
        o.ended = p.ended().map(|e| e.to_string());
        let finite = out.iter().all(|x| x.is_finite());
        o.silent = Some(!finite || peak(&out) <= 1e-4);
        // Writes: whatever the chip was told is not counted here.
        o.writes = 1;
        return o;
    }
    let mut r = match PsidRunner::new(&file, file.start_subsong()) {
        Ok(r) => r,
        Err(e) => {
            o.refused = Some(e.to_string());
            return o;
        }
    };
    let cycles = (r.clock_hz() as f64 * seconds) as u64;
    let mut t = 0;
    while t < cycles {
        t = (t + 100_000).min(cycles);
        r.advance(t);
        r.drain(t, 3, |_| o.writes += 1);
    }
    o.ended = r.ended().map(|e| e.to_string());
    o
}

#[test]
fn corpus_smoke() {
    let Some(root) = std::env::var_os("CUBED_PSID") else {
        eprintln!("CUBED_PSID is not set: no corpus to run");
        return;
    };
    let seconds = env_f64("CUBED_PSID_SECONDS", 10.0);
    let stride = env_f64("CUBED_PSID_STRIDE", 1.0).max(1.0) as usize;
    let audio_every = env_f64("CUBED_PSID_AUDIO", 25.0).max(1.0) as usize;
    let files: Vec<PathBuf> = sid_files(Path::new(&root))
        .into_iter()
        .step_by(stride)
        .collect();
    assert!(!files.is_empty(), "no .sid files under {root:?}");
    let threads = 4;
    let results: Vec<(PathBuf, Outcome)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|k| {
                let files = &files;
                s.spawn(move || {
                    files
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| i % threads == k)
                        .map(|(i, f)| (f.clone(), run_tune(f, seconds, i % audio_every == 0)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a tune panicked"))
            .collect()
    });
    let mut classes: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (path, o) in &results {
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(why) = &o.refused {
            // Group the init failures by their kind, with addresses blanked.
            let key: String = why.split('$').next().unwrap_or(why).to_string();
            classes
                .entry(format!("refused: {key}"))
                .or_default()
                .push(format!("{name}: {why}"));
        } else if let Some(why) = &o.ended {
            classes
                .entry("ended early".into())
                .or_default()
                .push(format!("{name}: {why}"));
        } else if o.writes == 0 {
            classes
                .entry("wrote nothing to a SID".into())
                .or_default()
                .push(name);
        } else if o.silent == Some(true) {
            classes
                .entry("silent when rendered".into())
                .or_default()
                .push(name);
        }
    }
    eprintln!(
        "corpus: {} tunes, {seconds} s each, stride {stride}",
        results.len()
    );
    for (class, names) in &classes {
        eprintln!("  {class}: {}", names.len());
        for n in names.iter().take(12) {
            eprintln!("      {n}");
        }
    }
    if std::env::var_os("CUBED_PSID_STRICT").is_some() {
        let bad: usize = classes
            .iter()
            .filter(|(k, _)| !k.starts_with("refused"))
            .map(|(_, v)| v.len())
            .sum();
        assert_eq!(bad, 0, "tunes that fail to play");
    }
}

fn fnv(writes: &[(u64, u8, u8)]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &(c, r, v) in writes {
        for b in [
            c as u32 as u8,
            (c >> 8) as u8,
            (c >> 16) as u8,
            (c >> 24) as u8,
            r,
            v,
        ] {
            h = (h ^ b as u32).wrapping_mul(0x0100_0193);
        }
    }
    h
}

#[test]
fn oracle_the_daws_runner_writes_the_same() {
    let Some(json) = std::env::var_os("CUBED_PSID_ORACLE") else {
        eprintln!("CUBED_PSID_ORACLE is not set: nothing to compare with");
        return;
    };
    let seconds = env_f64("CUBED_PSID_SECONDS", 10.0);
    let oracle: serde_json::Value = serde_json::from_slice(&std::fs::read(json).unwrap()).unwrap();
    let entries: Vec<(&String, &serde_json::Value)> = oracle.as_object().unwrap().iter().collect();
    let threads = 4;
    let mismatches: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|k| {
                let entries = &entries;
                s.spawn(move || {
                    let mut bad = vec![];
                    for (path, want) in entries.iter().skip(k).step_by(threads) {
                        if let Some(m) = compare_with_oracle(path, want, seconds) {
                            bad.push(format!("{path}: {m}"));
                        }
                    }
                    bad
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    eprintln!(
        "oracle: {} tunes compared, {} differ",
        entries.len(),
        mismatches.len()
    );
    for m in mismatches.iter().take(40) {
        eprintln!("  {m}");
    }
    assert!(
        mismatches.is_empty(),
        "{} tunes differ from the DAW's runner",
        mismatches.len()
    );
}

fn compare_with_oracle(path: &str, want: &serde_json::Value, seconds: f64) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    // The DAW's messages say "this importer" where this player says "this player".
    let mut want = want.clone();
    if let Some(reason) = want["reason"].as_str() {
        want["reason"] = reason.replace("this importer", "this player").into();
    }
    let want = &want;
    let file = match PsidFile::parse(&bytes) {
        Ok(f) => f,
        Err(e) => {
            return if want["ok"] == false && want["reason"] == e.to_string() {
                None
            } else {
                Some(format!("parse: {e} vs the DAW's {want}"))
            };
        }
    };
    let mut r = match PsidRunner::new(&file, file.start_subsong()) {
        Ok(r) => r,
        Err(e) => {
            return if want["ok"] == false && want["reason"] == e.to_string() {
                None
            } else {
                Some(format!("init: {e} vs the DAW's {want}"))
            };
        }
    };
    if want["ok"] != true {
        return Some(format!("runs here, the DAW says {}", want["reason"]));
    }
    let cycles = (r.clock_hz() as f64 * seconds).floor() as u64;
    let mode = if r.mode() == RunMode::Play {
        "play"
    } else {
        "irq"
    };
    if want["mode"] != mode {
        return Some(format!("mode {mode} vs {}", want["mode"]));
    }
    r.advance(cycles);
    let mut w = vec![];
    r.drain(cycles, 1, |x| w.push((x.cycle, x.reg, x.value)));
    // (The oracle's tick period is read after its run, when the DAW's runner has finished the play call
    // that straddles the end; this runner stops that call where the time ends. The writes below are the proof.)
    let ended = r.ended().map(|e| e.to_string());
    let want_ended = want["ended"].as_str().map(str::to_string);
    if ended != want_ended {
        return Some(format!("ended {ended:?} vs {want_ended:?}"));
    }
    if want["count"].as_u64() != Some(w.len() as u64) {
        let first_diff = want["first"]
            .as_array()
            .unwrap()
            .iter()
            .zip(&w)
            .position(|(a, b)| a[0] != b.0 || a[1] != b.1 || a[2] != b.2);
        return Some(format!(
            "{} writes vs {} (first difference at {first_diff:?})",
            w.len(),
            want["count"]
        ));
    }
    if want["hash"].as_u64() != Some(fnv(&w) as u64) {
        let first_diff = want["first"]
            .as_array()
            .unwrap()
            .iter()
            .zip(&w)
            .position(|(a, b)| a[0] != b.0 || a[1] != b.1 || a[2] != b.2);
        return Some(format!(
            "same count, different writes (first difference in the first 32: {first_diff:?})"
        ));
    }
    None
}
