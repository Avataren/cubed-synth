use cubed_dsp::audio_engine::native::{AudioEngine, EFFECT_NODE_ID_OFFSET};
use cubed_dsp::automation::AutomationFrame;
use cubed_dsp::biquad::FilterType;
use cubed_dsp::graph::{ModulationTransformation, ModulationType};
use cubed_dsp::nodes::{AnalogOscillatorStateUpdate, FilterSlope, Waveform};
use cubed_dsp::traits::PortId;

const SAMPLE_RATE: f32 = 48_000.0;
const EFFECT_CONVOLVER_ID: usize = EFFECT_NODE_ID_OFFSET + 3;
const EFFECT_COMPRESSOR_ID: usize = EFFECT_NODE_ID_OFFSET + 5;
const EFFECT_SATURATION_ID: usize = EFFECT_NODE_ID_OFFSET + 6;
const EFFECT_BITCRUSHER_ID: usize = EFFECT_NODE_ID_OFFSET + 7;

fn frame(block: usize) -> AutomationFrame {
    AutomationFrame::with_dimensions(1, 4, block)
}

fn render(engine: &mut AudioEngine) -> Vec<f32> {
    render_gated(engine, false)
}

fn render_gated(engine: &mut AudioEngine, gate_on: bool) -> Vec<f32> {
    let mut left = vec![0.0; engine.block_size()];
    let mut right = vec![0.0; engine.block_size()];
    let mut automation = frame(engine.block_size());
    if gate_on {
        automation.gates_mut().fill(1.0);
    }
    engine.process_with_frame(&automation, 1.0, &mut left, &mut right);
    left
}

#[test]
#[cfg(feature = "native-host")]
fn oscillator_through_mixer_and_filter_produces_audio() {
    let mut engine = AudioEngine::new(SAMPLE_RATE, 1);
    let mixer = engine.create_mixer_node().unwrap();
    let osc = engine.create_oscillator_node().unwrap();
    let filter = engine.create_filter_node().unwrap();

    engine
        .update_oscillator(
            osc,
            &AnalogOscillatorStateUpdate {
                id: None,
                phase_mod_amount: 0.0,
                freq_mod_amount: 0.0,
                detune_oct: 0.0,
                detune_semi: 0.0,
                detune_cents: 0.0,
                detune: 0.0,
                hard_sync: false,
                gain: 1.0,
                feedback_amount: 0.0,
                waveform: Waveform::Sine,
                unison_voices: 1,
                spread: 0.0,
                active: true,
                wave_index: 0.0,
            },
        )
        .unwrap();
    engine
        .update_filters(
            filter,
            SAMPLE_RATE / 4.0,
            0.0,
            0.5,
            0.0,
            220.0,
            0.5,
            0,
            FilterType::LowPass,
            FilterSlope::Db12,
        )
        .unwrap();
    engine
        .connect_nodes(
            osc,
            PortId::AudioOutput0,
            filter,
            PortId::AudioInput0,
            1.0,
            ModulationType::Additive,
            ModulationTransformation::None,
        )
        .unwrap();
    engine
        .connect_nodes(
            filter,
            PortId::AudioOutput0,
            mixer,
            PortId::AudioInput0,
            1.0,
            ModulationType::Additive,
            ModulationTransformation::None,
        )
        .unwrap();

    let output = render_gated(&mut engine, true);
    assert_peak(output, 0.001, "oscillator/filter/mixer");
}

#[test]
#[cfg(feature = "native-host")]
fn envelope_modulates_mixer_gain() {
    let mut engine = AudioEngine::new(SAMPLE_RATE, 1);
    let mixer = engine.create_mixer_node().unwrap();
    let osc = engine.create_oscillator_node().unwrap();
    let envelope = engine.create_envelope_node().unwrap();

    engine
        .update_envelope(envelope, 0.0, 0.05, 1.0, 0.05, 0.0, 0.0, 0.0, true)
        .unwrap();
    engine
        .connect_nodes(
            osc,
            PortId::AudioOutput0,
            mixer,
            PortId::AudioInput0,
            1.0,
            ModulationType::Additive,
            ModulationTransformation::None,
        )
        .unwrap();
    engine
        .connect_nodes(
            envelope,
            PortId::AudioOutput0,
            mixer,
            PortId::GainMod,
            1.0,
            ModulationType::VCA,
            ModulationTransformation::None,
        )
        .unwrap();

    // Warm up and then gate on.
    let _ = render(&mut engine);
    let mut gated = frame(128);
    gated.gates_mut()[..128].fill(1.0);
    let mut left = vec![0.0; 128];
    let mut right = vec![0.0; 128];
    engine.process_with_frame(&gated, 1.0, &mut left, &mut right);

    assert_peak(left, 0.0005, "envelope gain modulation");
}

#[test]
#[cfg(feature = "native-host")]
fn active_convolver_changes_signal() {
    let mut engine = AudioEngine::new(SAMPLE_RATE, 1);
    let mixer = engine.create_mixer_node().unwrap();
    let osc = engine.create_oscillator_node().unwrap();
    engine
        .connect_nodes(
            osc,
            PortId::AudioOutput0,
            mixer,
            PortId::AudioInput0,
            1.0,
            ModulationType::Additive,
            ModulationTransformation::None,
        )
        .unwrap();

    // Warm up with the default convolver disabled.
    let _ = render_gated(&mut engine, true);
    let dry_left = render_gated(&mut engine, true);

    engine
        .set_convolver_active(EFFECT_CONVOLVER_ID, true, 1.0)
        .unwrap();
    engine
        .update_effect_impulse(3, vec![1.0, 0.75, 0.5, 0.25])
        .unwrap();
    let wet_left = render_gated(&mut engine, true);

    let tail_left = render_gated(&mut engine, false);

    assert_peak(dry_left.clone(), 0.0005, "convolver dry source");
    assert!(
        wet_left.iter().any(|&v: &f32| v.abs() > 1e-6),
        "convolver produced no reverb tail"
    );
    assert_ne!(
        rms(&dry_left),
        rms(&wet_left),
        "active convolver did not alter the signal"
    );
    assert!(
        tail_left.iter().any(|&v: &f32| v.abs() > 1e-7),
        "convolver produced no reverb tail"
    );
}

#[test]
#[cfg(feature = "native-host")]
fn all_active_effects_pass_non_silent_input_without_panicking() {
    let mut engine = AudioEngine::new(SAMPLE_RATE, 1);
    let mixer = engine.create_mixer_node().unwrap();
    let osc = engine.create_oscillator_node().unwrap();
    engine
        .connect_nodes(
            osc,
            PortId::AudioOutput0,
            mixer,
            PortId::AudioInput0,
            1.0,
            ModulationType::Additive,
            ModulationTransformation::None,
        )
        .unwrap();

    engine.set_chorus_active(true);
    engine.set_delay_active(true);
    engine.set_reverb_active(true);
    engine
        .set_convolver_active(EFFECT_CONVOLVER_ID, true, 1.0)
        .unwrap();
    engine
        .update_compressor(EFFECT_COMPRESSOR_ID, true, -12.0, 3.0, 5.0, 50.0, 0.0, 0.5)
        .unwrap();
    engine
        .update_saturation(EFFECT_SATURATION_ID, 2.0, 0.5, true)
        .unwrap();
    engine
        .update_bitcrusher(EFFECT_BITCRUSHER_ID, 8, 2, 0.5, true)
        .unwrap();

    let output = render(&mut engine);
    assert!(
        output.iter().all(|value| value.is_finite()),
        "non-finite effect output"
    );
    assert_peak(output, 0.0005, "all active effects");
}

#[test]
#[cfg(feature = "native-host")]
fn glide_connection_does_not_disconnect_pitch() {
    let mut engine = AudioEngine::new(SAMPLE_RATE, 1);
    let mixer = engine.create_mixer_node().unwrap();
    let osc = engine.create_oscillator_node().unwrap();
    let glide = engine.create_glide_node(0.02).unwrap();
    engine.insert_glide_on_global_frequency(glide, osc).unwrap();
    engine
        .connect_nodes(
            osc,
            PortId::AudioOutput0,
            mixer,
            PortId::AudioInput0,
            1.0,
            ModulationType::Additive,
            ModulationTransformation::None,
        )
        .unwrap();

    assert_peak(render_gated(&mut engine, true), 0.0005, "glide path");
}

#[cfg_attr(not(feature = "native-host"), allow(dead_code))]
fn rms(buffer: &[f32]) -> f32 {
    (buffer.iter().map(|v| v * v).sum::<f32>() / buffer.len().max(1) as f32).sqrt()
}

#[cfg_attr(not(feature = "native-host"), allow(dead_code))]
fn assert_peak(buffer: Vec<f32>, minimum: f32, label: &str) {
    let peak = buffer
        .iter()
        .fold(0.0f32, |peak, value| peak.max(value.abs()));
    assert!(peak >= minimum, "{label} was silent (peak {peak})");
}
