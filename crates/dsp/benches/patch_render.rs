//! Synth render throughput: a typical subtractive/FM voice on the native
//! `AudioEngine` (two analog oscillators, the carrier with 4-voice unison and
//! phase modulation, a 24 dB low-pass, amp and mod envelopes), every voice
//! held on a different note, rendered in 128-frame blocks (the worklet's
//! quantum), dry and with chorus, delay and reverb. Reports the realtime
//! factor and the cost of one block against its deadline. The graph's
//! modulation and several nodes use the crate's SIMD path, so this is also the
//! check that the stable `cubed-simd` fallback keeps up with `std::simd`
//! (`--features nightly-simd` on a nightly toolchain).
//!
//! cargo bench -p cubed-dsp --bench patch_render

use std::hint::black_box;
use std::time::Instant;

use cubed_dsp::audio_engine::AudioEngine;
use cubed_dsp::biquad::FilterType;
use cubed_dsp::graph::{ModulationTransformation, ModulationType};
use cubed_dsp::nodes::{AnalogOscillatorStateUpdate, FilterSlope, Waveform};
use cubed_dsp::traits::PortId;
use cubed_dsp::AutomationFrame;

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;
const VOICES: usize = 8;
/// The engine's macro lanes per voice (`audio_engine::native::MACRO_COUNT`).
const MACROS: usize = 4;
const SECONDS: f64 = 20.0;
const RUNS: usize = 3;

fn engine(effects: bool) -> Result<AudioEngine, String> {
    let mut engine = AudioEngine::new_with_block_size(RATE, VOICES, BLOCK);
    engine.init(RATE, VOICES);

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
    engine.connect_nodes(
        modulator,
        PortId::AudioOutput0,
        carrier,
        PortId::PhaseMod,
        1.0,
        add,
        none,
    )?;
    engine.connect_nodes(
        mod_env,
        PortId::AudioOutput0,
        carrier,
        PortId::ModIndex,
        8.0,
        add,
        none,
    )?;
    engine.connect_nodes(
        carrier,
        PortId::AudioOutput0,
        filter,
        PortId::AudioInput0,
        0.8,
        add,
        none,
    )?;
    engine.connect_nodes(
        filter,
        PortId::AudioOutput0,
        mixer,
        PortId::AudioInput0,
        1.0,
        add,
        none,
    )?;
    engine.connect_nodes(
        amp_env,
        PortId::AudioOutput0,
        mixer,
        PortId::GainMod,
        1.0,
        ModulationType::VCA,
        none,
    )?;

    engine.set_chorus_active(effects);
    engine.set_delay_active(effects);
    engine.set_reverb_active(effects);
    Ok(engine)
}

/// Seconds to render `SECONDS`, the best of `RUNS`, and the peak level.
fn time_voices(effects: bool) -> (f64, f32) {
    let blocks = (RATE as f64 * SECONDS) as usize / BLOCK;
    let mut frame = AutomationFrame::with_dimensions(VOICES, MACROS, BLOCK);
    for voice in 0..VOICES {
        let note = 48.0 + 3.0 * voice as f32;
        let hz = 440.0 * 2f32.powf((note - 69.0) / 12.0);
        frame.set_voice_values(voice, 1.0, hz, 1.0, 1.0);
    }
    let mut best = f64::INFINITY;
    let mut peak = 0f32;
    for _ in 0..RUNS {
        let mut engine = engine(effects).expect("voice builds");
        let (mut left, mut right) = ([0f32; BLOCK], [0f32; BLOCK]);
        let start = Instant::now();
        for _ in 0..blocks {
            engine.process_with_frame(&frame, 1.0, &mut left, &mut right);
            black_box((&left, &right));
            peak = left.iter().chain(&right).fold(peak, |p, s| p.max(s.abs()));
        }
        best = best.min(start.elapsed().as_secs_f64());
    }
    (best, peak)
}

fn main() {
    let deadline_us = BLOCK as f64 / RATE as f64 * 1e6;
    let blocks = (RATE as f64 * SECONDS) as usize / BLOCK;
    for effects in [false, true] {
        let (secs, peak) = time_voices(effects);
        assert!(peak > 1e-3, "the voices are silent");
        let block_us = secs / blocks as f64 * 1e6;
        println!(
            "{VOICES} voices {:<8}: {:>6.1}x realtime, {:>7.2} us/block ({:.2}% of {:.0} us), peak {:.3}",
            if effects { "+effects" } else { "dry" },
            SECONDS / secs,
            block_us,
            block_us / deadline_us * 100.0,
            deadline_us,
            peak,
        );
    }
}
