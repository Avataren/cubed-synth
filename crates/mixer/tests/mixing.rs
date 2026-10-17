//! The mixer rendered offline: routing, faders, mute and solo, sends,
//! inserts, voices and recordings.

use std::sync::Arc;

use cubed_mixer::{
    BusId, BusSettings, Capture, Clip, ClipId, Command, Event, Insert, Mixer, MixerConfig, Play,
    Returned, SendSettings, Start, VoiceId,
};

const RATE: f32 = 48_000.0;
const SFX: BusId = BusId(1);
const MUSIC: BusId = BusId(2);
const REVERB: BusId = BusId(3);
const CLIP: ClipId = ClipId(0);

fn mixer() -> Mixer {
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE,
        ..MixerConfig::default()
    });
    for bus in [SFX, MUSIC, REVERB] {
        mixer.apply(Command::SetBus(bus, BusSettings::default()));
    }
    mixer
}

/// A constant 0.5 on both channels, `frames` long.
fn dc_clip(mixer: &mut Mixer, id: ClipId, frames: usize) {
    mixer.apply(Command::AddClip(
        id,
        Arc::new(Clip::mono(RATE, vec![0.5; frames])),
    ));
}

fn play(mixer: &mut Mixer, voice: u64, bus: BusId, looped: bool) {
    mixer.apply(Command::Play(Play {
        looped,
        ..Play::new(VoiceId(voice), CLIP, bus)
    }));
}

fn render(mixer: &mut Mixer, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    mixer.process(&mut l, &mut r);
    (l, r)
}

fn events(mixer: &mut Mixer) -> Vec<Event> {
    let mut out = Vec::new();
    mixer.drain_events(|e| out.push(e));
    out
}

/// The last sample of a long render: the level once every glide settled.
fn settled(mixer: &mut Mixer) -> (f32, f32) {
    let (l, r) = render(mixer, 48_000);
    (*l.last().unwrap(), *r.last().unwrap())
}

#[test]
fn a_clip_at_unity_reaches_the_output_unchanged() {
    let mut mixer = mixer();
    let samples: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin() * 0.8).collect();
    mixer.apply(Command::AddClip(
        CLIP,
        Arc::new(Clip::mono(RATE, samples.clone())),
    ));
    play(&mut mixer, 1, SFX, false);
    let (l, r) = render(&mut mixer, 1200);
    assert_eq!(&l[..1000], &samples[..]);
    assert_eq!(&r[..1000], &samples[..]);
    assert_eq!(events(&mut mixer), vec![Event::Ended(VoiceId(1))]);
    assert_eq!(mixer.stats().voices, 0);
}

#[test]
fn bus_and_master_faders_multiply_and_glide() {
    let mut mixer = mixer();
    dc_clip(&mut mixer, CLIP, 10);
    play(&mut mixer, 1, SFX, true);
    mixer.apply(Command::SetBusGain(SFX, -6.0));
    mixer.apply(Command::SetBusGain(BusId::MASTER, -6.0));
    let (l, _) = render(&mut mixer, 512);
    // No jump: the first block still starts near the old level.
    assert!((l[0] - 0.5).abs() < 0.01, "{}", l[0]);
    let (l, r) = settled(&mut mixer);
    let expected = 0.5 * 10f32.powf(-12.0 / 20.0);
    assert!((l - expected).abs() < 1e-4 && (r - expected).abs() < 1e-4);
}

#[test]
fn mute_and_solo() {
    let mut mixer = mixer();
    dc_clip(&mut mixer, CLIP, 10);
    play(&mut mixer, 1, SFX, true);
    play(&mut mixer, 2, MUSIC, true);
    assert_eq!(settled(&mut mixer).0, 1.0);

    mixer.apply(Command::SetBusMute(MUSIC, true));
    assert_eq!(settled(&mut mixer).0, 0.5);

    mixer.apply(Command::SetBusMute(MUSIC, false));
    mixer.apply(Command::SetBusSolo(MUSIC, true));
    assert_eq!(settled(&mut mixer).0, 0.5, "only music");

    // A bus routed into the soloed one is heard too.
    mixer.apply(Command::SetBusOutput(SFX, MUSIC));
    assert_eq!(settled(&mut mixer).0, 1.0);

    mixer.apply(Command::SetBusSolo(MUSIC, false));
    mixer.apply(Command::SetBusOutput(SFX, BusId::MASTER));
    assert_eq!(settled(&mut mixer).0, 1.0);
}

#[test]
fn balance_moves_a_bus_to_one_side() {
    let mut mixer = mixer();
    dc_clip(&mut mixer, CLIP, 10);
    play(&mut mixer, 1, SFX, true);
    mixer.apply(Command::SetBusPan(SFX, -1.0));
    let (l, r) = settled(&mut mixer);
    assert_eq!(l, 0.5);
    assert!(r.abs() < 1e-6);
}

#[test]
fn routing_loops_are_refused() {
    let mut mixer = mixer();
    mixer.apply(Command::SetBusOutput(MUSIC, SFX));
    assert_eq!(mixer.stats().rejected_commands, 0);
    mixer.apply(Command::SetBusOutput(SFX, MUSIC));
    mixer.apply(Command::SetSend {
        bus: SFX,
        slot: 0,
        send: Some(SendSettings {
            to: MUSIC,
            level_db: 0.0,
            pre_fader: false,
        }),
    });
    mixer.apply(Command::SetBusOutput(SFX, SFX));
    assert_eq!(mixer.stats().rejected_commands, 3);

    // The mix still runs, through the routing that was accepted.
    dc_clip(&mut mixer, CLIP, 10);
    play(&mut mixer, 1, MUSIC, true);
    assert_eq!(settled(&mut mixer).0, 0.5);
}

/// Multiplies by a fixed gain and counts its blocks.
struct Scale(f32, Arc<std::sync::atomic::AtomicUsize>);

impl Insert for Scale {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        left.iter_mut()
            .chain(right.iter_mut())
            .for_each(|s| *s *= self.0);
    }
}

#[test]
fn sends_feed_a_return_bus_before_or_after_the_fader() {
    let mut mixer = mixer();
    dc_clip(&mut mixer, CLIP, 10);
    play(&mut mixer, 1, SFX, true);
    let calls = Arc::new(Default::default());
    mixer.apply(Command::SetInsert {
        bus: REVERB,
        slot: 0,
        insert: Some(Box::new(Scale(0.5, Arc::clone(&calls)))),
    });
    let send = |pre_fader| Command::SetSend {
        bus: SFX,
        slot: 0,
        send: Some(SendSettings {
            to: REVERB,
            level_db: 0.0,
            pre_fader,
        }),
    };
    mixer.apply(send(false));
    // Dry 0.5 plus the return's 0.5 * 0.5.
    assert!((settled(&mut mixer).0 - 0.75).abs() < 1e-5);

    // Muted, a post-fader send goes quiet with the bus; a pre-fader one not.
    mixer.apply(Command::SetBusMute(SFX, true));
    assert!(settled(&mut mixer).0.abs() < 1e-5);
    mixer.apply(send(true));
    assert!((settled(&mut mixer).0 - 0.25).abs() < 1e-5);
}

#[test]
fn an_idle_bus_stops_running_its_inserts_after_the_tail() {
    let mut mixer = mixer();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    mixer.apply(Command::SetInsert {
        bus: REVERB,
        slot: 0,
        insert: Some(Box::new(Scale(1.0, Arc::clone(&calls)))),
    });
    render(&mut mixer, 48_000 * 4);
    let after_4s = calls.load(std::sync::atomic::Ordering::Relaxed);
    render(&mut mixer, 48_000 * 4);
    let after_8s = calls.load(std::sync::atomic::Ordering::Relaxed);
    assert!(after_4s > 0);
    assert!(after_8s < after_4s * 2, "{after_4s} then {after_8s} blocks");

    // A replaced insert comes back to be freed.
    mixer.apply(Command::SetInsert {
        bus: REVERB,
        slot: 0,
        insert: None,
    });
    let mut returned = 0;
    mixer.take_returned(|item| {
        assert!(matches!(item, Returned::Insert(_)));
        returned += 1;
    });
    assert_eq!(returned, 1);
}

#[test]
fn a_full_bus_steals_its_weakest_oldest_voice() {
    let mut mixer = mixer();
    dc_clip(&mut mixer, CLIP, 10);
    mixer.apply(Command::SetBus(
        SFX,
        BusSettings {
            voice_limit: 2,
            ..BusSettings::default()
        },
    ));
    let start = |mixer: &mut Mixer, voice, priority| {
        mixer.apply(Command::Play(Play {
            looped: true,
            priority,
            ..Play::new(VoiceId(voice), CLIP, SFX)
        }))
    };
    start(&mut mixer, 1, 100);
    start(&mut mixer, 2, 100);
    start(&mut mixer, 3, 100);
    assert_eq!(events(&mut mixer), vec![Event::Stolen(VoiceId(1))]);
    start(&mut mixer, 4, 50);
    assert_eq!(events(&mut mixer), vec![Event::Rejected(VoiceId(4))]);
    start(&mut mixer, 5, 200);
    assert_eq!(events(&mut mixer), vec![Event::Stolen(VoiceId(2))]);
    assert!(mixer.is_playing(VoiceId(3)) && mixer.is_playing(VoiceId(5)));
}

#[test]
fn a_scheduled_voice_starts_on_its_frame() {
    let mut mixer = mixer();
    dc_clip(&mut mixer, CLIP, 10_000);
    render(&mut mixer, 100);
    mixer.apply(Command::Play(Play {
        start: Start::AtFrame(300),
        ..Play::new(VoiceId(1), CLIP, SFX)
    }));
    let (l, _) = render(&mut mixer, 400);
    let first = l.iter().position(|&s| s != 0.0).unwrap();
    assert_eq!(first, 200, "frame 300 is sample 200 of this render");
}

#[test]
fn stops_fade_out_and_removed_clips_end_their_voices() {
    let mut mixer = mixer();
    dc_clip(&mut mixer, CLIP, 10);
    play(&mut mixer, 1, SFX, true);
    play(&mut mixer, 2, SFX, true);
    render(&mut mixer, 128);
    mixer.apply(Command::Stop(VoiceId(1), 480));
    let (l, _) = render(&mut mixer, 480);
    assert!(l[0] > 0.9 && (l[479] - 0.5).abs() < 0.01);
    assert_eq!(events(&mut mixer), vec![Event::Ended(VoiceId(1))]);

    mixer.apply(Command::RemoveClip(CLIP));
    assert_eq!(events(&mut mixer), vec![Event::Ended(VoiceId(2))]);
    let mut clips = 0;
    mixer.take_returned(|item| clips += matches!(item, Returned::Clip(_)) as usize);
    assert_eq!(clips, 1);
    play(&mut mixer, 3, SFX, false);
    assert_eq!(events(&mut mixer), vec![Event::Rejected(VoiceId(3))]);
}

#[test]
fn removing_a_bus_ends_its_voices_and_reroutes_its_inputs() {
    let mut mixer = mixer();
    dc_clip(&mut mixer, CLIP, 10);
    mixer.apply(Command::SetBusOutput(SFX, MUSIC));
    play(&mut mixer, 1, SFX, true);
    play(&mut mixer, 2, MUSIC, true);
    mixer.apply(Command::RemoveBus(MUSIC));
    assert_eq!(events(&mut mixer), vec![Event::Ended(VoiceId(2))]);
    assert_eq!(settled(&mut mixer).0, 0.5, "SFX now feeds master");
    assert!(mixer.meter(MUSIC).is_none());
}

#[test]
fn meters_follow_the_level_and_fall_back() {
    let mut mixer = mixer();
    dc_clip(&mut mixer, CLIP, 48_000 * 3);
    play(&mut mixer, 1, SFX, false);
    // Two seconds: the ~300 ms RMS average has settled.
    render(&mut mixer, 48_000 * 2);
    let meter = mixer.meter(SFX).unwrap();
    assert_eq!(meter.peak, [0.5, 0.5]);
    assert!((meter.rms[0] - 0.5).abs() < 0.01, "{}", meter.rms[0]);
    // The clip ends at 3 s; two seconds later the peak has fallen 48 dB.
    render(&mut mixer, 48_000 * 3);
    let meter = mixer.meter(SFX).unwrap();
    assert!(meter.peak[0] < 0.5 * 0.5, "{}", meter.peak[0]);
}

#[test]
fn a_capture_records_the_master_output() {
    let mut mixer = mixer();
    dc_clip(&mut mixer, CLIP, 1000);
    mixer.apply(Command::Capture(Box::new(Capture::new(500))));
    play(&mut mixer, 1, SFX, false);
    let (l, _) = render(&mut mixer, 1000);
    let mut capture = None;
    mixer.take_returned(|item| {
        if let Returned::Capture(c) = item {
            capture = Some(c);
        }
    });
    let capture = capture.expect("returned once full");
    assert_eq!(capture.filled, 500);
    assert_eq!(capture.left, l[..500]);
}

#[test]
fn bad_commands_are_counted_not_fatal() {
    let mut mixer = mixer();
    mixer.apply(Command::SetBusGain(BusId(31), 0.0));
    mixer.apply(Command::SetBusGain(BusId(999), 0.0));
    mixer.apply(Command::RemoveBus(BusId::MASTER));
    mixer.apply(Command::Stop(VoiceId(42), 0));
    mixer.apply(Command::AddClip(
        ClipId(1 << 30),
        Arc::new(Clip::mono(RATE, vec![])),
    ));
    assert_eq!(mixer.stats().rejected_commands, 5);
    let mut returned = 0;
    mixer.take_returned(|_| returned += 1);
    assert_eq!(returned, 1, "the refused clip comes back");
    render(&mut mixer, 256);
}
