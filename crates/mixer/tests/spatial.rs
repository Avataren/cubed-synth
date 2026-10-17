//! Spatial voices rendered offline: a moving emitter pans, fades and shifts
//! in pitch as it passes the listener, in 3D and in 2D.

use std::sync::Arc;

use cubed_mixer::{
    BusId, BusSettings, Clip, ClipId, Command, Listener, Mixer, MixerConfig, Play, Rolloff,
    Spatial, SpatialMode, VoiceId,
};

const RATE: f32 = 48_000.0;
const BUS: BusId = BusId(1);
const VOICE: VoiceId = VoiceId(1);
/// Positions are sent once per 60 Hz game frame, as an engine does.
const FRAME: usize = 800;
const TONE_HZ: f32 = 1_000.0;

fn mixer() -> Mixer {
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE,
        ..MixerConfig::default()
    });
    mixer.apply(Command::SetBus(BUS, BusSettings::default()));
    // A looping 1 kHz tone: exactly 48 cycles in 2,304 frames.
    let tone = (0..2_304)
        .map(|i| 0.5 * (std::f32::consts::TAU * TONE_HZ * i as f32 / RATE).sin())
        .collect();
    mixer.apply(Command::AddClip(
        ClipId(0),
        Arc::new(Clip::mono(RATE, tone)),
    ));
    mixer
}

fn play(mixer: &mut Mixer, spatial: Spatial) {
    mixer.apply(Command::Play(Play {
        looped: true,
        spatial: Some(spatial),
        ..Play::new(VOICE, ClipId(0), BUS)
    }));
}

/// Renders `seconds`, moving the voice along `path(t)` with its velocity
/// every game frame. Returns the left and right channels.
fn fly(
    mixer: &mut Mixer,
    seconds: f32,
    path: impl Fn(f32) -> ([f32; 3], [f32; 3]),
) -> (Vec<f32>, Vec<f32>) {
    let frames = (seconds * RATE) as usize;
    let (mut left, mut right) = (vec![0.0; frames], vec![0.0; frames]);
    let mut at = 0;
    while at < frames {
        let (position, velocity) = path(at as f32 / RATE);
        mixer.apply(Command::SetVoicePosition {
            voice: VOICE,
            position,
            velocity,
        });
        let n = FRAME.min(frames - at);
        mixer.process(&mut left[at..at + n], &mut right[at..at + n]);
        at += n;
    }
    (left, right)
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// The frequency of a tone, from its upward zero crossings.
fn frequency(samples: &[f32]) -> f32 {
    let crossings = samples
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    crossings as f32 * RATE / samples.len() as f32
}

/// A quarter-second window starting at `t` seconds.
fn window(samples: &[f32], t: f32) -> &[f32] {
    let start = (t * RATE) as usize;
    &samples[start..start + (RATE / 4.0) as usize]
}

#[test]
fn a_3d_fly_by_pans_left_to_right_peaks_when_closest_and_drops_in_pitch() {
    let mut mixer = mixer();
    // The listener at the origin looking down -Z; the emitter passes 2 m in
    // front, from 20 m left to 20 m right at 10 m/s.
    mixer.apply(Command::SetListener(Listener::default()));
    let start = Spatial {
        min_distance: 1.0,
        max_distance: 100.0,
        ..Spatial::new(SpatialMode::Full, [-20.0, 0.0, -2.0])
    };
    play(&mut mixer, start);
    let speed = 10.0;
    let (left, right) = fly(&mut mixer, 4.0, |t| {
        ([-20.0 + speed * t, 0.0, -2.0], [speed, 0.0, 0.0])
    });

    let (l0, r0) = (rms(window(&left, 0.25)), rms(window(&right, 0.25)));
    let (l3, r3) = (rms(window(&left, 3.5)), rms(window(&right, 3.5)));
    assert!(l0 > 4.0 * r0, "starts on the left: {l0} vs {r0}");
    assert!(r3 > 4.0 * l3, "ends on the right: {l3} vs {r3}");

    let total = |t| rms(window(&left, t)) + rms(window(&right, t));
    let middle = total(1.875);
    assert!(
        middle > 3.0 * total(0.25),
        "loudest when closest: {middle} vs {}",
        total(0.25)
    );
    assert!(middle > 3.0 * total(3.5));

    // Approaching: 343 / (343 - ~10) = ~1.03; receding: ~0.97.
    let mono: Vec<f32> = left.iter().zip(&right).map(|(l, r)| l + r).collect();
    let early = frequency(&mono[..(0.75 * RATE) as usize]);
    let late = frequency(&mono[(3.25 * RATE) as usize..]);
    assert!(
        early > TONE_HZ * 1.02 && early < TONE_HZ * 1.04,
        "approaching {early} Hz"
    );
    assert!(
        late < TONE_HZ * 0.98 && late > TONE_HZ * 0.96,
        "receding {late} Hz"
    );
}

#[test]
fn a_2d_emitter_ignores_depth_pans_by_offset_and_shifts_in_pitch() {
    let mut mixer = mixer();
    // A side view: the camera at z = 10 looks down -Z, so depth (z) does not
    // count; a pan width of 10 puts x = +-10 fully to a side.
    mixer.apply(Command::SetListener(Listener {
        position: [0.0, 0.0, 10.0],
        pan_width: 10.0,
        ..Listener::default()
    }));
    play(
        &mut mixer,
        Spatial {
            min_distance: 2.0,
            max_distance: 40.0,
            ..Spatial::new(SpatialMode::Planar, [-10.0, 0.0, -50.0])
        },
    );
    let speed = 5.0;
    let (left, right) = fly(&mut mixer, 4.0, |t| {
        ([-10.0 + speed * t, 0.0, -50.0], [speed, 0.0, 0.0])
    });

    // At x = -10 the sound is all left, at +10 all right (the first and last
    // 50 ms, while it is within a quarter unit of the edge).
    let edge = (0.05 * RATE) as usize;
    let (l0, r0) = (rms(&left[..edge]), rms(&right[..edge]));
    assert!(r0 < 0.05 * l0, "fully left: {l0} vs {r0}");
    let end = left.len() - edge;
    let (l3, r3) = (rms(&left[end..]), rms(&right[end..]));
    assert!(l3 < 0.05 * r3, "fully right: {l3} vs {r3}");

    // Within min_distance of the listener's line (x near 0) it plays at full
    // level although it is 60 units deep: equal-power centre, 0.5 * 0.707.
    let centre = rms(window(&left, 1.9));
    let full = 0.5 * std::f32::consts::FRAC_1_SQRT_2 * std::f32::consts::FRAC_1_SQRT_2;
    assert!((centre - full).abs() < 0.03, "{centre} vs {full}");

    let mono: Vec<f32> = left.iter().zip(&right).map(|(l, r)| l + r).collect();
    let early = frequency(&mono[(0.25 * RATE) as usize..(1.25 * RATE) as usize]);
    let late = frequency(&mono[(2.75 * RATE) as usize..(3.75 * RATE) as usize]);
    assert!(
        early > late * 1.02,
        "approaching {early} Hz, receding {late} Hz"
    );
}

#[test]
fn inverse_rolloff_halves_the_level_each_time_the_distance_doubles() {
    let mut mixer = mixer();
    mixer.apply(Command::SetListener(Listener::default()));
    play(
        &mut mixer,
        Spatial::new(SpatialMode::Full, [0.0, 0.0, -1.0]),
    );
    let mut levels = Vec::new();
    for distance in [1.0, 2.0, 4.0, 8.0] {
        let (left, _) = fly(&mut mixer, 0.5, |_| ([0.0, 0.0, -distance], [0.0; 3]));
        levels.push(rms(&left[left.len() / 2..]));
    }
    for pair in levels.windows(2) {
        let ratio = pair[1] / pair[0];
        assert!((ratio - 0.5).abs() < 0.02, "{levels:?}");
    }
}

#[test]
fn out_of_range_voices_go_virtual_and_keep_their_place() {
    let mut mixer = mixer();
    mixer.apply(Command::SetListener(Listener::default()));
    let linear = Spatial {
        rolloff: Rolloff::Linear,
        min_distance: 1.0,
        max_distance: 10.0,
        ..Spatial::new(SpatialMode::Full, [0.0, 0.0, -50.0])
    };
    play(&mut mixer, linear);
    let (left, right) = fly(&mut mixer, 0.5, |_| ([0.0, 0.0, -50.0], [0.0; 3]));
    assert!(
        left.iter().chain(&right).all(|&s| s == 0.0),
        "silent beyond max_distance"
    );
    assert!(mixer.is_playing(VOICE));
    // Back in range it sounds again: a 0.5 sine, -3 dB in the centre.
    let (left, _) = fly(&mut mixer, 0.5, |_| ([0.0, 0.0, -1.0], [0.0; 3]));
    assert!((rms(&left[left.len() / 2..]) - 0.25).abs() < 0.01);
}

#[test]
fn doppler_switches_off_for_the_project_and_per_voice() {
    for (listener_doppler, voice_doppler) in [(false, 1.0), (true, 0.0)] {
        let mut mixer = mixer();
        mixer.apply(Command::SetListener(Listener {
            doppler: listener_doppler,
            ..Listener::default()
        }));
        play(
            &mut mixer,
            Spatial {
                doppler: voice_doppler,
                max_distance: 100.0,
                ..Spatial::new(SpatialMode::Full, [0.0, 0.0, -60.0])
            },
        );
        let (left, _) = fly(&mut mixer, 1.0, |t| {
            ([0.0, 0.0, -60.0 + 30.0 * t], [0.0, 0.0, 30.0])
        });
        let f = frequency(&left[(0.25 * RATE) as usize..]);
        assert!((f - TONE_HZ).abs() < 3.0, "{f} Hz");
    }
}

#[test]
fn a_voice_can_become_spatial_and_stop_being_it() {
    let mut mixer = mixer();
    mixer.apply(Command::SetListener(Listener::default()));
    mixer.apply(Command::Play(Play {
        looped: true,
        ..Play::new(VOICE, ClipId(0), BUS)
    }));
    let (left, _) = fly(&mut mixer, 0.25, |_| ([0.0; 3], [0.0; 3]));
    assert!((rms(&left[6000..]) - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);
    mixer.apply(Command::SetVoiceSpatial(
        VOICE,
        Some(Spatial::new(SpatialMode::Full, [4.0, 0.0, 0.0])),
    ));
    let (left, right) = fly(&mut mixer, 0.5, |_| ([4.0, 0.0, 0.0], [0.0; 3]));
    assert!(
        rms(&right[12000..]) > 10.0 * rms(&left[12000..]),
        "now on the right, a quarter as loud"
    );
    mixer.apply(Command::SetVoiceSpatial(VOICE, None));
    let (left, right) = fly(&mut mixer, 0.25, |_| ([0.0; 3], [0.0; 3]));
    assert!((rms(&left[6000..]) - rms(&right[6000..])).abs() < 1e-3);
}
