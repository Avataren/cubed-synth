//! The synth engine's output, pinned: two patches rendered through voices,
//! graphs and effects with notes starting and stopping, hashed bit for bit.
//! A rework of the graph's block processing must not change a sample.
//!
//! One test in its own binary, because it sets the global random seed.
#![cfg(feature = "native-host")]

use cubed_dsp::audio_engine::native::AudioEngine;
use cubed_dsp::automation::AutomationFrame;
use cubed_dsp::biquad::FilterType;
use cubed_dsp::graph::{ModulationTransformation, ModulationType};
use cubed_dsp::nodes::{AnalogOscillatorStateUpdate, FilterSlope, Waveform};
use cubed_dsp::traits::PortId;

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;
const VOICES: usize = 8;
const BLOCKS: usize = 600;

fn osc(waveform: Waveform, detune: f32, unison_voices: u32) -> AnalogOscillatorStateUpdate {
    AnalogOscillatorStateUpdate {
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
    }
}

/// Two oscillators (FM into a unison saw), a filter, two envelopes and a
/// mixer per voice; `glide` puts a glide on the global frequency.
fn patch(
    waveform: Waveform,
    filter_type: FilterType,
    slope: FilterSlope,
    glide: bool,
    effects: bool,
) -> AudioEngine {
    let mut engine = AudioEngine::new_with_block_size(RATE, VOICES, BLOCK);
    engine.init(RATE, VOICES);
    let mixer = engine.create_mixer_node().unwrap();
    let filter = engine.create_filter_node().unwrap();
    let carrier = engine.create_oscillator_node().unwrap();
    let modulator = engine.create_oscillator_node().unwrap();
    let amp_env = engine.create_envelope_node().unwrap();
    let mod_env = engine.create_envelope_node().unwrap();
    engine
        .update_oscillator(carrier, &osc(waveform, 0.0, 4))
        .unwrap();
    engine
        .update_oscillator(modulator, &osc(Waveform::Sine, -1200.0, 1))
        .unwrap();
    engine
        .update_envelope_node(amp_env, 0.005, 0.2, 0.8, 0.3, 1.0, 1.0, 1.0, true)
        .unwrap();
    engine
        .update_envelope_node(mod_env, 0.05, 0.7, 0.25, 0.2, 1.0, -2.3, 1.0, true)
        .unwrap();
    engine
        .update_filters(
            filter,
            1800.0,
            0.4,
            1.0,
            0.0,
            220.0,
            0.5,
            1,
            filter_type,
            slope,
        )
        .unwrap();
    let add = ModulationType::Additive;
    let none = ModulationTransformation::None;
    let links = [
        (modulator, carrier, PortId::PhaseMod, 1.0, add),
        (mod_env, carrier, PortId::ModIndex, 8.0, add),
        (mod_env, filter, PortId::CutoffMod, 0.6, add),
        (carrier, filter, PortId::AudioInput0, 0.8, add),
        (modulator, filter, PortId::AudioInput0, 0.1, add),
        (filter, mixer, PortId::AudioInput0, 1.0, add),
        (amp_env, mixer, PortId::GainMod, 1.0, ModulationType::VCA),
    ];
    for (from, to, port, amount, kind) in links {
        engine
            .connect_nodes(from, PortId::AudioOutput0, to, port, amount, kind, none)
            .unwrap();
    }
    if glide {
        let glide = engine.create_glide_node(0.08).unwrap();
        engine
            .insert_glide_on_global_frequency(glide, carrier)
            .unwrap();
    }
    engine.set_chorus_active(effects);
    engine.set_delay_active(effects);
    engine.set_reverb_active(effects);
    engine
}

/// FNV-1a over the bits of every sample, with each voice holding a note for
/// a while, resting, and coming back at another pitch.
fn render_hash(engine: &mut AudioEngine) -> u64 {
    let mut frame = AutomationFrame::with_dimensions(VOICES, 4, BLOCK);
    let (mut left, mut right) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for block in 0..BLOCKS {
        for voice in 0..VOICES {
            let phase = block / (31 + voice * 7) + voice;
            let gate = if phase % 2 == 0 { 1.0 } else { 0.0 };
            let hz = 110.0 * 2f32.powf(((voice * 5 + phase * 3) % 24) as f32 / 12.0);
            frame.set_voice_values(voice, gate, hz, 0.4 + voice as f32 * 0.07, 1.0);
        }
        engine.process_with_frame(&frame, 0.8, &mut left, &mut right);
        for s in left.iter().chain(&right) {
            for byte in s.to_bits().to_le_bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    hash
}

#[test]
fn patches_render_exactly_as_before() {
    cubed_dsp::seed::set_seed(7);
    let cases = [
        (
            "fm saw, 24 dB low-pass, effects",
            patch(
                Waveform::Saw,
                FilterType::LowPass,
                FilterSlope::Db24,
                false,
                true,
            ),
        ),
        (
            "square with glide, 12 dB high-pass, dry",
            patch(
                Waveform::Square,
                FilterType::HighPass,
                FilterSlope::Db12,
                true,
                false,
            ),
        ),
    ];
    let mut hashes = Vec::new();
    for (name, mut engine) in cases {
        let hash = render_hash(&mut engine);
        println!("{name}: {hash:#018x}");
        hashes.push(hash);
    }
    // Recorded before the graph's block processing was reworked.
    assert_eq!(hashes, [0xb893_78a6_8abf_a3c4, 0x6868_0064_f5e2_a355]);
}
