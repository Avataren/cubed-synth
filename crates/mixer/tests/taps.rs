//! Taps on buses: the latest frames after the fader, for a scope and a
//! spectrum; switched on and off by command; read through `Analysis`.

use std::sync::Arc;

use cubed_mixer::{
    Analysis, BusId, BusSettings, Clip, ClipId, Command, Mixer, MixerConfig, Play, Returned,
    Spectrum, Tap, TapSet, VoiceId,
};

const RATE: f32 = 48_000.0;
const MUSIC: BusId = BusId(1);
const OTHER: BusId = BusId(2);

fn mixer() -> Mixer {
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE,
        ..MixerConfig::default()
    });
    for bus in [MUSIC, OTHER] {
        mixer.apply(Command::SetBus(bus, BusSettings::default()));
    }
    mixer
}

/// A looping sine of amplitude 0.5 at `hz` on `bus`, whole cycles only.
fn play_sine(mixer: &mut Mixer, hz: f32, bus: BusId, voice: u64) {
    let cycles = (hz * 0.5).round().max(1.0);
    let frames = (RATE * cycles / hz).round() as usize;
    let samples: Vec<f32> = (0..frames)
        .map(|i| 0.5 * (std::f32::consts::TAU * cycles * i as f32 / frames as f32).sin())
        .collect();
    mixer.apply(Command::AddClip(
        ClipId(voice as u32),
        Arc::new(Clip::mono(RATE, samples)),
    ));
    mixer.apply(Command::Play(Play {
        looped: true,
        ..Play::new(VoiceId(voice), ClipId(voice as u32), bus)
    }));
}

fn render(mixer: &mut Mixer, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    for (lc, rc) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
        mixer.process(lc, rc);
    }
    (l, r)
}

#[test]
fn the_master_tap_holds_what_the_mixer_played() {
    let mut mixer = mixer();
    let mut taps = TapSet::new(mixer.config().max_buses);
    let tap = taps.enable(BusId::MASTER, 4096).unwrap();
    mixer.apply(Command::SetTap(BusId::MASTER, Some(tap)));
    play_sine(&mut mixer, 1_000.0, MUSIC, 1);
    let (l, r) = render(&mut mixer, 10_000);

    let (mut sl, mut sr) = (vec![0.0; 4096], vec![0.0; 4096]);
    assert_eq!(taps.scope(BusId::MASTER, &mut sl, &mut sr), 4096);
    // The scope is the last 4096 frames of the output, in order.
    assert_eq!(sl[..], l[10_000 - 4096..]);
    assert_eq!(sr[..], r[10_000 - 4096..]);
    assert_eq!(taps.position(BusId::MASTER), 10_000);
    assert!(sl.iter().any(|&s| s.abs() > 0.1));
}

#[test]
fn a_bus_tap_is_after_the_fader_and_other_buses_have_none() {
    let mut mixer = mixer();
    let mut taps = TapSet::new(mixer.config().max_buses);
    let tap = taps.enable(MUSIC, 2048).unwrap();
    mixer.apply(Command::SetTap(MUSIC, Some(tap)));
    play_sine(&mut mixer, 500.0, MUSIC, 1);
    play_sine(&mut mixer, 700.0, OTHER, 2);
    // The music bus at -6 dB.
    mixer.apply(Command::SetBusGain(MUSIC, -6.0206));
    render(&mut mixer, 48_000);
    let (mut sl, mut sr) = (vec![0.0; 1024], vec![0.0; 1024]);
    assert_eq!(taps.scope(MUSIC, &mut sl, &mut sr), 1024);
    let peak = sl.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    // 0.5 sine, bus at half: about 0.25 (the clip's own pan law is in it too).
    assert!(peak > 0.1 && peak < 0.3, "{peak}");
    assert_eq!(
        taps.scope(OTHER, &mut sl, &mut sr),
        0,
        "no tap on the other bus"
    );
    // The meter agrees on the level.
    let meter = mixer.meter(MUSIC).unwrap();
    assert!(meter.peak[0] >= peak * 0.95);
}

#[test]
fn a_tap_that_is_replaced_removed_or_on_a_bus_that_is_gone_comes_back() {
    let mut mixer = mixer();
    let first = Tap::new(256);
    let second = Tap::new(256);
    mixer.apply(Command::SetTap(MUSIC, Some(first.clone())));
    mixer.apply(Command::SetTap(MUSIC, Some(second.clone())));
    let mut returned = Vec::new();
    mixer.take_returned(|r| returned.push(r));
    assert!(matches!(&returned[..], [Returned::Tap(t)] if Arc::ptr_eq(t, &first)));
    returned.clear();

    mixer.apply(Command::SetTap(MUSIC, None));
    mixer.take_returned(|r| returned.push(r));
    assert!(matches!(&returned[..], [Returned::Tap(t)] if Arc::ptr_eq(t, &second)));
    returned.clear();

    // A bus that does not exist: rejected, the tap given back.
    let before = mixer.stats().rejected_commands;
    mixer.apply(Command::SetTap(BusId(9), Some(Tap::new(64))));
    assert_eq!(mixer.stats().rejected_commands, before + 1);
    mixer.take_returned(|r| returned.push(r));
    assert_eq!(returned.len(), 1);
    returned.clear();

    // Removing a bus with a tap hands it back too.
    mixer.apply(Command::SetTap(OTHER, Some(Tap::new(64))));
    mixer.apply(Command::RemoveBus(OTHER));
    mixer.take_returned(|r| returned.push(r));
    assert!(matches!(&returned[..], [Returned::Tap(_)]));
}

#[test]
fn a_switched_off_tap_stops_and_an_idle_bus_reads_silence() {
    let mut mixer = mixer();
    let tap = Tap::new(1024);
    mixer.apply(Command::SetTap(MUSIC, Some(tap.clone())));
    render(&mut mixer, 1_280);
    assert_eq!(tap.written(), 1_280, "written even while silent");
    let (mut l, mut r) = (vec![1.0; 512], vec![1.0; 512]);
    assert_eq!(tap.read(&mut l, &mut r), 512);
    assert!(l.iter().chain(&r).all(|&s| s == 0.0));
    mixer.apply(Command::SetTap(MUSIC, None));
    render(&mut mixer, 1_280);
    assert_eq!(
        tap.written(),
        1_280,
        "no more frames after it was taken off"
    );
}

/// A sine through the mixer, tapped, analysed: the bar of its frequency is lit.
#[test]
fn a_played_sine_shows_in_the_spectrum_through_the_reader() {
    let mut mixer = mixer();
    let mut taps = TapSet::new(mixer.config().max_buses);
    let tap = taps.enable(BusId::MASTER, 4096).unwrap();
    mixer.apply(Command::SetTap(BusId::MASTER, Some(tap)));
    play_sine(&mut mixer, 2_000.0, MUSIC, 1);
    render(&mut mixer, 20_000);

    struct Reader<'a>(&'a TapSet, &'a Mixer);
    impl Analysis for Reader<'_> {
        fn sample_rate(&self) -> f32 {
            RATE
        }
        fn scope(&self, bus: BusId, l: &mut [f32], r: &mut [f32]) -> usize {
            self.0.scope(bus, l, r)
        }
        fn scope_position(&self, bus: BusId) -> u64 {
            self.0.position(bus)
        }
        fn levels(&self, bus: BusId) -> Option<cubed_mixer::BusMeter> {
            self.1.meter(bus)
        }
    }
    let reader = Reader(&taps, &mixer);
    let mut spectrum = Spectrum::new(2048);
    let (mut l, mut r) = (vec![0.0; 2048], vec![0.0; 2048]);
    assert_eq!(reader.scope(BusId::MASTER, &mut l, &mut r), 2048);
    let mut bins = [0.0; 64];
    spectrum.log_bins(
        &l,
        &r,
        reader.sample_rate(),
        &mut bins,
        20.0,
        20_000.0,
        -90.0,
    );
    let loudest = bins
        .iter()
        .enumerate()
        .fold((0, 0.0f32), |a, (i, &v)| if v > a.1 { (i, v) } else { a })
        .0;
    let expected = ((2_000f32 / 20.0).ln() / 1000f32.ln() * 64.0) as usize;
    assert!(loudest.abs_diff(expected) <= 1, "{loudest} vs {expected}");
    let levels = reader.levels(BusId::MASTER).unwrap();
    assert!(levels.rms[0] > 0.1 && levels.peak[0] > 0.2, "{levels:?}");
}
