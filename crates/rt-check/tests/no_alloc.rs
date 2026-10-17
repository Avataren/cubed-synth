//! Every source the engine's mixer will host renders its blocks without
//! allocating or freeing: after a warm-up, each 128-frame block is checked.
//!
//! cargo test -p cubed-rt-check
//!
//! With `CUBED_RT_TRACE=1` every counted allocation prints a backtrace.

use cubed_ahx::player::AhxPlayer;
use cubed_dsp::audio_engine::AudioEngine;
use cubed_dsp::biquad::FilterType;
use cubed_dsp::effect_stack::EffectStack;
use cubed_dsp::graph::{ModulationTransformation, ModulationType};
use cubed_dsp::nodes::{
    AnalogOscillatorStateUpdate, Bitcrusher, Chorus, Compressor, Convolver, Delay, FilterSlope,
    Freeverb, Limiter, Saturation, Waveform,
};
use cubed_dsp::{AutomationFrame, PortId};
use cubed_mixer::songs::{open_song, SongFormat, SongOptions};
use cubed_mixer::{
    BusId, BusSettings, Clip, ClipId, Command, DspInsert, Duck, Effect, EffectKind, Listener,
    Mixer, MixerConfig, Play, PlaySource, SendSettings, SourceId, Spatial, SpatialMode, Start,
    VoiceId,
};
use cubed_opl::a2::player::A2Player;
use cubed_rt_check::{allocations_during, trace_allocations, CountingAllocator};
use cubed_sid::stream::SidChipPlayer;
use cubed_sid::{SidModel, SidSong, SidSongPlayer};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
const BLOCKS_PER_SECOND: usize = RATE as usize / BLOCK;
/// Blocks rendered before counting, so one-time setup (tables built on first
/// use, buffers sized to the first block) is not counted.
const WARMUP_BLOCKS: usize = BLOCKS_PER_SECOND;

/// Calls `render(block)` for a second of warm-up, then for `seconds` more,
/// failing at the first of those blocks that allocates or frees.
fn assert_no_alloc(name: &str, seconds: usize, mut render: impl FnMut(usize)) {
    trace_allocations(std::env::var_os("CUBED_RT_TRACE").is_some());
    for block in 0..WARMUP_BLOCKS {
        render(block);
    }
    for block in WARMUP_BLOCKS..WARMUP_BLOCKS + seconds * BLOCKS_PER_SECOND {
        let events = allocations_during(|| render(block));
        assert_eq!(
            events, 0,
            "{name}: block {block} made {events} allocations or frees"
        );
    }
}

/// A fixture file from another crate's `tests/` directory.
fn fixture(path: &str) -> Vec<u8> {
    let path = format!("{}/../{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn sid_song_player() {
    for path in ["s3-chain.asid", "s59-drum-example.asid"] {
        for model in [SidModel::Sid8580, SidModel::Sid6581] {
            let song = SidSong::parse(&fixture(&format!("sid/tests/fixtures/sid/{path}")))
                .expect("fixture parses");
            let mut player =
                SidSongPlayer::with_model(song, model, RATE as f64, 0).expect("player builds");
            let mut out = [0f32; BLOCK];
            let (mut a, mut b, mut c) = ([0f32; BLOCK], [0f32; BLOCK], [0f32; BLOCK]);
            assert_no_alloc(&format!("{path} {model:?}"), 5, |_| {
                player.render_taps(&mut out, [&mut a, &mut b, &mut c]);
            });
        }
    }
}

/// The register-stream player that PSID playback feeds: a few timed writes
/// per block, as a 50 Hz play routine spreads them.
#[test]
fn sid_chip_player() {
    let mut player = SidChipPlayer::new(true, RATE as f64, 985_248.0).expect("player builds");
    player.write(24, 0x0f); // volume
    player.write(5, 0x09); // voice 1 attack/decay
    player.write(6, 0xa0); // voice 1 sustain/release
    let mut out = [0f32; BLOCK];
    let (mut a, mut b, mut c) = ([0f32; BLOCK], [0f32; BLOCK], [0f32; BLOCK]);
    assert_no_alloc("SidChipPlayer", 5, |block| {
        let freq = (block * 37 % 0xffff) as u16;
        player.write_after(10, 0, freq as u8);
        player.write_after(20, 1, (freq >> 8) as u8);
        player.write_after(1_000, 4, if block % 40 < 20 { 0x41 } else { 0x40 });
        player.render(&mut out, &mut a, &mut b, &mut c);
    });
}

#[test]
fn ahx_player() {
    for path in ["a_new_beginning.ahx", "a_little_cyberfunk.hvl"] {
        for hifi in [false, true] {
            let bytes = fixture(&format!("ahx/tests/fixtures/songs/{path}"));
            let mut player = AhxPlayer::new(&bytes, RATE, 2).expect("fixture parses");
            player.set_hifi(hifi);
            player.play();
            let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
            assert_no_alloc(&format!("{path} hifi={hifi}"), 10, |_| {
                player.render(&mut left, &mut right);
            });
        }
    }
}

#[test]
fn a2m_player() {
    for path in [
        "NAB622/corridors of time.a2m",
        "Kvee/mega man 3 title.a2m",
        "Kvee/ym3812 funk.a2m",
    ] {
        let bytes = fixture(&format!("opl/tests/fixtures/a2m/{path}"));
        let mut player = A2Player::new(&bytes, RATE as f64).expect("player builds");
        player.play();
        let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
        assert_no_alloc(path, 10, |_| {
            player.render(&mut left, &mut right);
        });
    }
}

/// The DAW's synth engine: 8 voices of an FM-into-subtractive patch (two
/// oscillators, 4-voice unison, a 24 dB low-pass, two envelopes; the
/// `patch_render` bench's) through chorus, delay and reverb, with notes
/// starting and stopping. The native engine cannot load `real_patch.json`,
/// whose sampler it does not support, so the patch is built here.
///
/// Ignored until the graph is reworked: `AudioGraph::process_audio_with_macros`
/// builds hash maps and `Vec`s for every node of every voice, about 1,700
/// allocations and frees per block here. `cargo test -- --ignored` runs it.
#[test]
#[ignore = "AudioGraph allocates per node per block; see the doc comment"]
fn synth_engine() {
    const VOICES: usize = 8;
    let mut engine = synth_patch(VOICES).expect("patch builds");
    let mut frame = AutomationFrame::with_dimensions(VOICES, 4, BLOCK);
    let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
    assert_no_alloc("AudioEngine", 5, |block| {
        // Each voice holds a note for a quarter second, then rests as long.
        for voice in 0..VOICES {
            let gate = if (block / 47 + voice) % 2 == 0 {
                1.0
            } else {
                0.0
            };
            let hz = 110.0 * 2f32.powf(voice as f32 / 4.0);
            frame.set_voice_values(voice, gate, hz, 1.0, 1.0);
        }
        engine.process_with_frame(&frame, 1.0, &mut left, &mut right);
    });
}

fn synth_patch(voices: usize) -> Result<AudioEngine, String> {
    let rate = RATE as f32;
    let mut engine = AudioEngine::new_with_block_size(rate, voices, BLOCK);
    engine.init(rate, voices);

    let mixer = engine.create_mixer_node()?;
    let filter = engine.create_filter_node()?;
    let carrier = engine.create_oscillator_node()?;
    let modulator = engine.create_oscillator_node()?;
    let amp_env = engine.create_envelope_node()?;
    let mod_env = engine.create_envelope_node()?;

    let osc = |waveform, detune, unison_voices| AnalogOscillatorStateUpdate {
        id: None,
        phase_mod_amount: 0.0,
        freq_mod_amount: 0.0,
        detune_oct: 0.0,
        detune_semi: 0.0,
        detune_cents: 0.0,
        detune,
        wave_index: 0.0,
        hard_sync: false,
        gain: 1.0,
        active: true,
        feedback_amount: 0.0,
        waveform,
        unison_voices,
        spread: 10.0,
    };
    engine.update_oscillator(carrier, &osc(Waveform::Saw, 0.0, 4))?;
    engine.update_oscillator(modulator, &osc(Waveform::Sine, -1200.0, 1))?;
    engine.update_envelope_node(amp_env, 0.005, 0.2, 0.8, 0.3, 1.0, 1.0, 1.0, true)?;
    engine.update_envelope_node(mod_env, 0.05, 0.7, 0.25, 0.2, 1.0, -2.3, 1.0, true)?;
    engine.update_filters(
        filter,
        1800.0,
        0.4,
        1.0,
        0.0,
        220.0,
        0.5,
        1,
        FilterType::LowPass,
        FilterSlope::Db24,
    )?;

    let add = ModulationType::Additive;
    let none = ModulationTransformation::None;
    let links = [
        (modulator, carrier, PortId::PhaseMod, 1.0, add),
        (mod_env, carrier, PortId::ModIndex, 8.0, add),
        (carrier, filter, PortId::AudioInput0, 0.8, add),
        (filter, mixer, PortId::AudioInput0, 1.0, add),
        (amp_env, mixer, PortId::GainMod, 1.0, ModulationType::VCA),
    ];
    for (from, to, port, amount, kind) in links {
        engine.connect_nodes(from, PortId::AudioOutput0, to, port, amount, kind, none)?;
    }

    engine.set_chorus_active(true);
    engine.set_delay_active(true);
    engine.set_reverb_active(true);
    Ok(engine)
}

/// The effects as bus inserts: an `EffectStack` of every stereo effect.
#[test]
fn effect_stack() {
    let rate = RATE as f32;
    let ir: Vec<f32> = (0..4_800)
        .map(|i| (i as f32 * 0.37).sin() * (-(i as f32) / 900.0).exp())
        .collect();
    let mut stack = EffectStack::new(BLOCK);
    stack.add_effect(Box::new(Chorus::new(
        rate, 65.0, 15.0, 5.0, 0.5, 0.3, 0.5, 90.0,
    )));
    stack.add_effect(Box::new(Delay::new(rate, 2000.0, 500.0, 0.5, 0.1)));
    stack.add_effect(Box::new(Freeverb::new(rate, 0.95, 0.5, 0.3, 0.7, 1.0)));
    stack.add_effect(Box::new(Convolver::new(ir, BLOCK, rate)));
    stack.add_effect(Box::new(Limiter::new(rate, -0.5, 0.1, 50.0, 1.5, true)));
    stack.add_effect(Box::new(Compressor::new(
        rate, -12.0, 4.0, 10.0, 80.0, 3.0, 0.5,
    )));
    stack.add_effect(Box::new(Saturation::new(2.0, 0.5)));
    stack.add_effect(Box::new(Bitcrusher::new(12, 4, 0.5)));

    let mut input = [0f32; BLOCK];
    let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
    assert_no_alloc("EffectStack", 5, |block| {
        for (i, s) in input.iter_mut().enumerate() {
            let t = (block * BLOCK + i) as f32 / rate;
            *s = 0.5 * (std::f32::consts::TAU * 220.0 * t).sin();
        }
        stack.process_audio(&input, &input, &mut left, &mut right);
    });
}

/// The game mixer: six buses, a reverb on a return fed by sends, 48 voices
/// of looping and one-shot clips (most of them spatial, in 2D and 3D), and
/// every block a few plays, stops, fader moves, pan changes and listener and
/// emitter moves applied as commands.
#[test]
fn mixer() {
    let rate = RATE as f32;
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: rate,
        ..MixerConfig::default()
    });
    let reverb = BusId(6);
    for bus in 1..=6 {
        mixer.apply(Command::SetBus(BusId(bus), BusSettings::default()));
    }
    let mut verb = DspInsert::new(Freeverb::new(rate, 0.9, 0.5, 1.0, 0.0, 1.0));
    verb.node_mut().set_active(true);
    mixer.apply(Command::SetInsert {
        bus: reverb,
        slot: 0,
        insert: Some(Box::new(verb)),
    });
    for bus in 1..=3 {
        mixer.apply(Command::SetSend {
            bus: BusId(bus),
            slot: 0,
            send: Some(SendSettings {
                to: reverb,
                level_db: -12.0,
                pre_fader: false,
            }),
        });
    }
    for clip in 0..4u32 {
        let frames = 2_000 + clip as usize * 7_000;
        let samples = (0..frames).map(|i| (i as f32 * 0.05).sin() * 0.2).collect();
        let pcm = if clip % 2 == 0 {
            Clip::mono(22_050.0 + clip as f32 * 9_000.0, samples)
        } else {
            Clip::stereo(44_100.0, samples, vec![0.1; frames])
        };
        mixer.apply(Command::AddClip(ClipId(clip), std::sync::Arc::new(pcm)));
    }

    let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
    let mut next_voice = 0u64;
    assert_no_alloc("Mixer", 10, |block| {
        for _ in 0..2 {
            next_voice += 1;
            let v = next_voice;
            let mode = if v.is_multiple_of(2) {
                SpatialMode::Full
            } else {
                SpatialMode::Planar
            };
            let spatial = (!v.is_multiple_of(4)).then(|| Spatial {
                velocity: [3.0, 0.0, -1.0],
                ..Spatial::new(mode, [(v % 9) as f32 - 4.0, 0.0, -((v % 13) as f32)])
            });
            mixer.apply(Command::Play(Play {
                pitch: 0.5 + (v % 7) as f32 * 0.25,
                pan: (v % 5) as f32 * 0.5 - 1.0,
                looped: v.is_multiple_of(3),
                start: Start::AtFrame(mixer.frame() + v % 200),
                spatial,
                ..Play::new(
                    VoiceId(v),
                    ClipId((v % 4) as u32),
                    BusId(1 + (v % 5) as u16),
                )
            }));
            if v > 40 {
                mixer.apply(Command::Stop(VoiceId(v - 40), 2_000));
            }
        }
        let t = block as f32 * 0.01;
        mixer.apply(Command::SetListener(Listener {
            position: [t.sin() * 3.0, 0.0, t.cos() * 3.0],
            velocity: [1.0, 0.0, 0.0],
            ..Listener::default()
        }));
        for v in next_voice.saturating_sub(30)..next_voice {
            mixer.apply(Command::SetVoicePosition {
                voice: VoiceId(v),
                position: [
                    (v as f32 + t).sin() * 20.0,
                    0.0,
                    (v as f32 + t).cos() * 20.0,
                ],
                velocity: [5.0, 0.0, 5.0],
            });
        }
        let bus = BusId(1 + (block % 5) as u16);
        mixer.apply(Command::SetBusGain(bus, -((block % 24) as f32)));
        mixer.apply(Command::SetBusPan(bus, ((block % 9) as f32 - 4.0) / 4.0));
        mixer.process(&mut left, &mut right);
        mixer.drain_events(|_| {});
        mixer.take_returned(drop);
    });
}

/// Songs through the mixer, counted from their first block (the songs are
/// built and warmed up off the audio thread): two at a time crossfading on
/// a music bus that ducks under effects, through every built-in effect with
/// its parameters moving.
#[test]
fn mixer_songs() {
    let rate = RATE as f32;
    let songs = [
        (
            "ahx/tests/fixtures/songs/a_new_beginning.ahx",
            SongFormat::Ahx,
        ),
        (
            "ahx/tests/fixtures/songs/a_little_cyberfunk.hvl",
            SongFormat::Ahx,
        ),
        (
            "opl/tests/fixtures/a2m/NAB622/corridors of time.a2m",
            SongFormat::A2m,
        ),
        ("sid/tests/fixtures/sid/s3-chain.asid", SongFormat::Sid),
    ];
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: rate,
        ..MixerConfig::default()
    });
    let (sfx, music) = (BusId(1), BusId(2));
    mixer.apply(Command::SetBus(sfx, BusSettings::default()));
    mixer.apply(Command::SetBus(
        music,
        BusSettings {
            duck: Some(Duck::new(sfx, 9.0)),
            ..BusSettings::default()
        },
    ));
    mixer.apply(Command::AddClip(
        ClipId(0),
        std::sync::Arc::new(Clip::mono(rate, vec![0.3; 6_000])),
    ));
    for (slot, kind) in EffectKind::ALL.into_iter().take(8).enumerate() {
        mixer.apply(Command::SetInsert {
            bus: music,
            slot: slot as u8,
            insert: Some(Effect::new(kind).build(rate, BLOCK)),
        });
    }
    trace_allocations(std::env::var_os("CUBED_RT_TRACE").is_some());
    let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
    let fade = RATE / 2;
    for (i, (path, format)) in songs.into_iter().enumerate() {
        let song = open_song(format, &fixture(path), rate, SongOptions::default())
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let voice = VoiceId(100 + i as u64);
        mixer.apply(Command::PrepareSource(SourceId(i as u32 % 2), song));
        let events = allocations_during(|| {
            mixer.apply(Command::PlaySource(PlaySource {
                gain: 0.0,
                ..PlaySource::new(voice, SourceId(i as u32 % 2), music)
            }));
            mixer.apply(Command::FadeVoice {
                voice,
                gain: 1.0,
                frames: fade,
            });
            if i > 0 {
                mixer.apply(Command::Stop(VoiceId(99 + i as u64), fade));
            }
            for block in 0..3 * BLOCKS_PER_SECOND {
                let sweep = (block % 100) as f32 / 100.0;
                for slot in 0..8 {
                    mixer.apply(Command::SetInsertParam {
                        bus: music,
                        slot,
                        param: (block % 3) as u8,
                        value: 0.2 + sweep * 900.0,
                    });
                }
                if block % 50 == 0 {
                    mixer.apply(Command::Play(Play::new(
                        VoiceId(block as u64),
                        ClipId(0),
                        sfx,
                    )));
                }
                mixer.process(&mut left, &mut right);
                mixer.drain_events(|_| {});
            }
        });
        assert_eq!(events, 0, "{path}: {events} allocations or frees");
        // Ended songs come back to be freed here, off the audio thread.
        mixer.take_returned(drop);
    }
}

/// A stream through the mixer while another thread pushes frames into it.
#[test]
fn mixer_stream() {
    let rate = RATE as f32;
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: rate,
        ..MixerConfig::default()
    });
    mixer.apply(Command::SetBus(BusId(1), BusSettings::default()));
    let (mut writer, stream) = cubed_mixer::stream::stream(48_000, 44_100.0, rate);
    let feeder = std::thread::spawn(move || {
        let chunk: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.01).sin() * 0.3).collect();
        let mut pushed = 0;
        while pushed < 44_100 * 6 && !writer.is_abandoned() {
            let n = writer.push(&chunk);
            pushed += n;
            if n == 0 {
                std::thread::yield_now();
            }
        }
        writer.finish();
    });
    mixer.apply(Command::PrepareSource(SourceId(0), Box::new(stream)));
    mixer.apply(Command::PlaySource(PlaySource::new(
        VoiceId(1),
        SourceId(0),
        BusId(1),
    )));
    let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
    // Counted on this thread only (the counter is per thread).
    let mut ended = false;
    while !ended {
        let events = allocations_during(|| mixer.process(&mut left, &mut right));
        assert_eq!(events, 0, "a stream block allocated or freed");
        mixer.drain_events(|e| ended |= e == cubed_mixer::Event::Ended(VoiceId(1)));
        assert!(mixer.frame() < RATE as u64 * 20, "the stream never ended");
    }
    mixer.take_returned(drop);
    feeder.join().unwrap();
}
