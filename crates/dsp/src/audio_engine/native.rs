use crate::audio_engine::patch::{
    AudioAsset, GlideState, LfoState, PatchFile, PatchNode, VoiceLayout as PatchVoiceLayout,
};
use crate::audio_engine::patch_loader::for_each_node_in_creation_order;
use crate::audio_engine::patch_loader::{
    filter_type_from_i32, find_node_id, parse_audio_asset_id, parse_node_id,
};
use crate::audio_engine::wav_decode::{decode_wav, impulse_from_wav};
use crate::automation::AutomationFrame;
use crate::biquad::FilterType;
use crate::effect_stack::EffectStack;
use crate::graph::{Connection, ModulationTransformation, ModulationType};
use crate::impulse_generator::ImpulseResponseGenerator;
use crate::nodes::morph_wavetable::WavetableSynthBank;
use crate::nodes::{
    AnalogOscillator, AnalogOscillatorStateUpdate, ArpeggiatorGenerator, Bitcrusher, Chorus,
    Compressor, Convolver, Delay, Envelope, EnvelopeConfig, FilterCollection, FilterSlope,
    Freeverb, GateMixer, Glide, GlobalFrequencyNode, GlobalVelocityNode, Lfo, LfoLoopMode,
    LfoRetriggerMode, LfoWaveform, Limiter, Mixer, NoiseGenerator, NoiseType, NoiseUpdate,
    SampleData, Sampler, SamplerLoopMode, SamplerTriggerMode, Saturation, Waveform, WavetableBank,
    WavetableOscillator, WavetableOscillatorStateUpdate,
};
use crate::traits::{AudioNode, PortId};
use crate::voice::Voice;
use crate::NodeId;
use base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use base64::Engine as _;
use rustc_hash::FxHashMap;
use std::{cell::RefCell, rc::Rc, sync::Arc};
use uuid::Uuid;

const DEFAULT_NUM_VOICES: usize = 8;

/// A sampler's sound until a sample is loaded: half a second of a 220 Hz
/// sine, as in the wasm engine.
fn default_sampler_data(sample_rate: f32) -> Vec<f32> {
    let frames = (sample_rate * 0.5).max(1.0) as usize;
    (0..frames)
        .map(|n| (2.0 * std::f32::consts::PI * 220.0 * n as f32 / sample_rate).sin())
        .collect()
}
const MAX_TABLE_SIZE: usize = 2048;
const DEFAULT_BLOCK_SIZE: usize = 128;
const MACRO_COUNT: usize = 4;
pub const EFFECT_NODE_ID_OFFSET: usize = 10_000;

pub struct AudioEngine {
    voices: Vec<Voice>,
    sample_rate: f32,
    num_voices: usize,
    wavetable_synthbank: Rc<RefCell<WavetableSynthBank>>,
    wavetable_banks: Arc<FxHashMap<Waveform, Arc<WavetableBank>>>,
    effect_stack: EffectStack,
    ir_generator: ImpulseResponseGenerator,
    cpu_time_accum: f64,
    audio_time_accum: f64,
    last_cpu_usage: f32,
    block_size: usize,
    mix_left: Vec<f32>,
    mix_right: Vec<f32>,
    voice_left: Vec<f32>,
    voice_right: Vec<f32>,
    effect_left: Vec<f32>,
    effect_right: Vec<f32>,
    /// Each sampler's sample, shared by its node in every voice.
    sampler_data: FxHashMap<NodeId, Rc<RefCell<SampleData>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmNoiseType {
    White,
    Pink,
    Brownian,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmModulationType {
    VCA,
    Bipolar,
    Additive,
}

#[derive(Debug, Clone, Copy)]
pub struct NoiseUpdateParams {
    pub noise_type: WasmNoiseType,
    pub cutoff: f32,
    pub gain: f32,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct LfoUpdateParams {
    pub lfo_id: usize,
    pub frequency: f32,
    pub phase_offset: f32,
    pub waveform: u8,
    pub use_absolute: bool,
    pub use_normalized: bool,
    pub trigger_mode: u8,
    pub gain: f32,
    pub active: bool,
    pub loop_mode: usize,
    pub loop_start: f32,
    pub loop_end: f32,
}

impl AudioEngine {
    pub fn new(sample_rate: f32, num_voices: usize) -> Self {
        Self::new_with_block_size(sample_rate, num_voices, DEFAULT_BLOCK_SIZE)
    }

    pub fn new_with_block_size(sample_rate: f32, num_voices: usize, block_size: usize) -> Self {
        let block_size = block_size.max(1);
        let wavetable_synthbank = Rc::new(RefCell::new(WavetableSynthBank::new(sample_rate)));

        let mut banks = FxHashMap::default();
        banks.insert(
            Waveform::Sine,
            Arc::new(
                WavetableBank::new(Waveform::Sine, MAX_TABLE_SIZE, sample_rate)
                    .expect("Failed to create Sine wavetable bank"),
            ),
        );
        banks.insert(
            Waveform::Saw,
            Arc::new(
                WavetableBank::new(Waveform::Saw, MAX_TABLE_SIZE, sample_rate)
                    .expect("Failed to create Saw wavetable bank"),
            ),
        );
        banks.insert(
            Waveform::Square,
            Arc::new(
                WavetableBank::new(Waveform::Square, MAX_TABLE_SIZE, sample_rate)
                    .expect("Failed to create Square wavetable bank"),
            ),
        );
        banks.insert(
            Waveform::Triangle,
            Arc::new(
                WavetableBank::new(Waveform::Triangle, MAX_TABLE_SIZE, sample_rate)
                    .expect("Failed to create Triangle wavetable bank"),
            ),
        );

        let initial_voice_count = if num_voices == 0 {
            DEFAULT_NUM_VOICES
        } else {
            num_voices
        };

        let mut engine = Self {
            voices: Vec::new(),
            sample_rate,
            num_voices: initial_voice_count,
            wavetable_synthbank,
            wavetable_banks: Arc::new(banks),
            effect_stack: EffectStack::new(block_size),
            ir_generator: ImpulseResponseGenerator::new(sample_rate),
            cpu_time_accum: 0.0,
            audio_time_accum: 0.0,
            last_cpu_usage: 0.0,
            block_size,
            mix_left: vec![0.0; block_size],
            mix_right: vec![0.0; block_size],
            voice_left: vec![0.0; block_size],
            voice_right: vec![0.0; block_size],
            effect_left: vec![0.0; block_size],
            effect_right: vec![0.0; block_size],
            sampler_data: FxHashMap::default(),
        };
        engine.init(sample_rate, initial_voice_count);
        engine
    }

    pub fn init(&mut self, sample_rate: f32, num_voices: usize) {
        let voice_count = if num_voices == 0 {
            DEFAULT_NUM_VOICES
        } else {
            num_voices
        };

        self.sample_rate = sample_rate;
        self.num_voices = voice_count;
        self.voices = (0..voice_count)
            .map(|id| Voice::new(id, self.block_size))
            .collect();

        self.effect_stack = EffectStack::new(self.block_size);
        self.ir_generator = ImpulseResponseGenerator::new(sample_rate);

        let mut chorus = Chorus::new(sample_rate, 65.0, 15.0, 5.0, 0.5, 0.3, 0.5, 90.0);
        chorus.set_active(false);
        self.effect_stack.add_effect(Box::new(chorus));

        let mut delay = Delay::new(sample_rate, 2000.0, 500.0, 0.5, 0.1);
        delay.set_active(false);
        self.effect_stack.add_effect(Box::new(delay));

        let mut reverb = Freeverb::new(sample_rate, 0.95, 0.5, 0.3, 0.7, 1.0);
        reverb.set_active(false);
        self.effect_stack.add_effect(Box::new(reverb));

        let plate_ir = self.ir_generator.plate(2.0, 0.6);
        let partition_size = self.block_size.next_power_of_two().max(32);
        let mut plate = Convolver::new(plate_ir, partition_size, sample_rate);
        plate.set_wet_level(0.1);
        plate.set_enabled(false);
        self.effect_stack.add_effect(Box::new(plate));

        let mut limiter = Limiter::new(sample_rate, -0.5, 0.1, 50.0, 1.5, true);
        limiter.set_active(true);
        self.effect_stack.add_effect(Box::new(limiter));

        let mut compressor = Compressor::new(sample_rate, -12.0, 4.0, 10.0, 80.0, 3.0, 0.5);
        compressor.set_active(true);
        self.effect_stack.add_effect(Box::new(compressor));

        let mut saturation = Saturation::new(2.0, 0.5);
        saturation.set_active(false);
        self.effect_stack.add_effect(Box::new(saturation));

        let mut bitcrusher = Bitcrusher::new(12, 4, 0.5);
        bitcrusher.set_active(false);
        self.effect_stack.add_effect(Box::new(bitcrusher));

        // Voice graphs own global frequency/velocity/gate nodes; effects remain
        // separate. `init` only needs to rebuild the shared effect chain.
    }

    pub fn init_with_patch(&mut self, patch_json: &str) -> Result<usize, String> {
        let mut patch: PatchFile = serde_json::from_str(patch_json)
            .map_err(|e| format!("Failed to parse patch JSON: {}", e))?;

        if let Some(canonical) = patch.synth_state.layout.canonical_voice.as_mut() {
            let has_glide = canonical
                .nodes
                .get("glide")
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            if !has_glide {
                let new_id = NodeId::new().0.to_string();
                canonical
                    .nodes
                    .entry("glide".to_string())
                    .or_default()
                    .push(PatchNode {
                        id: new_id.clone(),
                        node_type: "glide".to_string(),
                        name: "Glide".to_string(),
                    });
                patch
                    .synth_state
                    .glides
                    .entry(new_id.clone())
                    .or_insert(GlideState {
                        glide_id: new_id,
                        time: 0.0,
                        rise_time: None,
                        fall_time: None,
                        active: false,
                    });
            }
        }

        let layout = &patch.synth_state.layout;
        let voice_count = layout.resolved_voice_count();
        if voice_count == 0 {
            return Err("Patch contains no voices".to_string());
        }

        self.num_voices = voice_count;
        self.voices = (0..voice_count)
            .map(|id| Voice::new(id, self.block_size))
            .collect();

        for voice in &mut self.voices {
            voice.clear();
            voice.graph.global_frequency_node = None;
            voice.graph.global_velocity_node = None;
            voice.graph.global_gatemixer_node = None;
        }

        self.effect_stack = EffectStack::new(self.block_size);
        self.ir_generator = ImpulseResponseGenerator::new(self.sample_rate);
        let mut chorus = Chorus::new(self.sample_rate, 65.0, 15.0, 5.0, 0.5, 0.3, 0.5, 90.0);
        chorus.set_active(false);
        self.effect_stack.add_effect(Box::new(chorus));

        let mut delay = Delay::new(self.sample_rate, 2000.0, 500.0, 0.5, 0.1);
        delay.set_active(false);
        self.effect_stack.add_effect(Box::new(delay));

        let mut reverb = Freeverb::new(self.sample_rate, 0.95, 0.5, 0.3, 0.7, 1.0);
        reverb.set_active(false);
        self.effect_stack.add_effect(Box::new(reverb));

        let plate_ir = self.ir_generator.plate(2.0, 0.6);
        let partition_size = self.block_size.next_power_of_two().max(32);
        let mut plate = Convolver::new(plate_ir, partition_size, self.sample_rate);
        plate.set_wet_level(0.1);
        plate.set_enabled(false);
        self.effect_stack.add_effect(Box::new(plate));

        let mut limiter = Limiter::new(self.sample_rate, -0.5, 0.1, 50.0, 1.5, true);
        limiter.set_active(true);
        self.effect_stack.add_effect(Box::new(limiter));

        let mut compressor = Compressor::new(self.sample_rate, -12.0, 4.0, 10.0, 80.0, 3.0, 0.5);
        compressor.set_active(true);
        self.effect_stack.add_effect(Box::new(compressor));

        let mut saturation = Saturation::new(2.0, 0.5);
        saturation.set_active(false);
        self.effect_stack.add_effect(Box::new(saturation));

        let mut bitcrusher = Bitcrusher::new(12, 4, 0.5);
        bitcrusher.set_active(false);
        self.effect_stack.add_effect(Box::new(bitcrusher));

        let canonical_voice = layout
            .canonical_voice()
            .ok_or_else(|| "Patch layout missing voice data".to_string())?;

        self.build_nodes_from_canonical_voice(canonical_voice)?;
        self.connect_from_canonical_voice(canonical_voice)?;
        self.apply_patch_states(&patch, canonical_voice)?;
        self.import_audio_assets(&patch.audio_assets)?;

        Ok(voice_count)
    }

    /// The patch's embedded samples and impulse responses (base64 WAVs keyed
    /// `sample_<node id>` / `impulse_response_<effect id>`).
    fn import_audio_assets(
        &mut self,
        assets: &std::collections::HashMap<String, AudioAsset>,
    ) -> Result<(), String> {
        for asset in assets.values() {
            let Some((kind, node_id)) = parse_audio_asset_id(&asset.id) else {
                eprintln!("Invalid audio asset id: {}", asset.id);
                continue;
            };
            let data = BASE64_ENGINE
                .decode(asset.base64_data.as_bytes())
                .map_err(|e| format!("Failed to decode asset {}: {e}", asset.id))?;
            match kind.as_str() {
                "sample" => self.import_sample(parse_node_id(&node_id)?, &data)?,
                "impulse_response" => {
                    let effect = node_id.parse::<usize>().map_err(|e| {
                        format!("Invalid impulse response effect id {node_id}: {e}")
                    })?;
                    self.import_wave_impulse(effect, &data)?;
                }
                // The wasm engine does not import wavetables from patches either.
                "wavetable" => {}
                other => eprintln!("Unknown audio asset type: {other}"),
            }
        }
        Ok(())
    }

    /// Loads a WAV as the sample of a sampler node (in every voice).
    pub fn import_sample(&mut self, sampler_id: NodeId, wav: &[u8]) -> Result<(), String> {
        let (samples, channels, rate) = decode_wav(wav)?;
        let data = Rc::new(RefCell::new(SampleData::new()));
        data.borrow_mut()
            .load_from_wav(samples, channels, rate as f32);
        for voice in &mut self.voices {
            let node = voice
                .graph
                .get_node_mut(sampler_id)
                .ok_or("Node not found")?;
            let sampler = node
                .as_any_mut()
                .downcast_mut::<Sampler>()
                .ok_or("Node is not a Sampler")?;
            sampler.set_sample_data(data.clone());
        }
        self.sampler_data.insert(sampler_id, data);
        Ok(())
    }

    /// Replaces the convolver at `effect_id` with one of this WAV's impulse
    /// response (mono, resampled to the engine's rate), keeping its wet level.
    pub fn import_wave_impulse(&mut self, effect_id: usize, wav: &[u8]) -> Result<(), String> {
        let index = effect_id
            .checked_sub(EFFECT_NODE_ID_OFFSET)
            .ok_or("Invalid effect id")?;
        let effect = self
            .effect_stack
            .effects
            .get_mut(index)
            .ok_or("Effect id not found in effect stack")?;
        let convolver = effect
            .node
            .as_any_mut()
            .downcast_mut::<Convolver>()
            .ok_or("Effect is not a Convolver")?;
        let (partition_size, rate, wet) = (
            convolver.partition_size,
            convolver.sample_rate,
            convolver.wet_level,
        );
        let ir = impulse_from_wav(wav, rate)?;
        let mut new_convolver = Convolver::new(ir, partition_size, rate);
        new_convolver.set_wet_level(wet);
        effect.node = Box::new(new_convolver);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_sampler(
        &mut self,
        sampler_id: NodeId,
        frequency: f32,
        gain: f32,
        loop_mode: u8,
        loop_start: f32,
        loop_end: f32,
        root_note: f32,
        trigger_mode: u8,
        active: bool,
    ) -> Result<(), String> {
        let loop_mode = match loop_mode {
            1 => SamplerLoopMode::Loop,
            2 => SamplerLoopMode::PingPong,
            _ => SamplerLoopMode::Off,
        };
        let trigger_mode = match trigger_mode {
            0 => SamplerTriggerMode::FreeRunning,
            2 => SamplerTriggerMode::OneShot,
            _ => SamplerTriggerMode::Gate,
        };
        for voice in &mut self.voices {
            let node = voice
                .graph
                .get_node_mut(sampler_id)
                .ok_or("Node not found")?;
            let sampler = node
                .as_any_mut()
                .downcast_mut::<Sampler>()
                .ok_or("Node is not a Sampler")?;
            sampler.set_base_frequency(frequency);
            sampler.set_base_gain(gain);
            sampler.set_loop_mode(loop_mode);
            sampler.set_loop_start(loop_start);
            sampler.set_loop_end(loop_end);
            sampler.set_root_note(root_note);
            sampler.set_trigger_mode(trigger_mode);
            sampler.set_active(active);
        }
        Ok(())
    }

    /// Applies an LFO's patch settings in every voice.
    pub fn update_lfo(&mut self, lfo_id: NodeId, state: &LfoState) {
        let waveform = match state.waveform {
            1 => LfoWaveform::Triangle,
            2 => LfoWaveform::Square,
            3 => LfoWaveform::Saw,
            4 => LfoWaveform::InverseSaw,
            _ => LfoWaveform::Sine,
        };
        let loop_mode = match state.loop_mode {
            1 => LfoLoopMode::Loop,
            2 => LfoLoopMode::PingPong,
            _ => LfoLoopMode::Off,
        };
        for voice in &mut self.voices {
            if let Some(lfo) = voice
                .graph
                .get_node_mut(lfo_id)
                .and_then(|node| node.as_any_mut().downcast_mut::<Lfo>())
            {
                lfo.set_gain(state.gain);
                lfo.set_phase_offset(state.phase_offset);
                lfo.set_frequency(state.frequency);
                lfo.set_waveform(waveform);
                lfo.set_use_absolute(state.use_absolute);
                lfo.set_use_normalized(state.use_normalized);
                lfo.set_retrigger_mode(LfoRetriggerMode::from_u8(state.trigger_mode));
                lfo.set_active(state.active);
                lfo.set_loop_mode(loop_mode);
                lfo.set_loop_start(state.loop_start);
                lfo.set_loop_end(state.loop_end);
            }
        }
    }

    /// `cutoff` is a fraction of the sample rate, as patches store it.
    pub fn update_noise(
        &mut self,
        noise_id: NodeId,
        noise_type: NoiseType,
        cutoff: f32,
        gain: f32,
        enabled: bool,
    ) -> Result<(), String> {
        let sample_rate = self.sample_rate;
        for voice in &mut self.voices {
            let node = voice.graph.get_node_mut(noise_id).ok_or("Node not found")?;
            let noise = node
                .as_any_mut()
                .downcast_mut::<NoiseGenerator>()
                .ok_or("Node is not a NoiseGenerator")?;
            noise.update(NoiseUpdate {
                noise_type,
                cutoff: cutoff * sample_rate,
                gain,
                enabled,
            });
        }
        Ok(())
    }

    /// Velocity sensitivity and randomness, on the named node or each
    /// voice's global velocity node.
    pub fn update_velocity(
        &mut self,
        node_id: NodeId,
        sensitivity: f32,
        randomize: f32,
    ) -> Result<(), String> {
        let mut updated = false;
        for voice in &mut self.voices {
            let target = if voice.graph.get_node(node_id).is_some() {
                Some(node_id)
            } else {
                voice.graph.global_velocity_node
            };
            let Some(target) = target else { continue };
            if let Some(node) = voice.graph.get_node_mut(target) {
                let velocity = node
                    .as_any_mut()
                    .downcast_mut::<GlobalVelocityNode>()
                    .ok_or("Node is not a GlobalVelocityNode")?;
                velocity.set_sensitivity(sensitivity);
                velocity.set_randomize(randomize);
                updated = true;
            }
        }
        if updated {
            Ok(())
        } else {
            Err("Node not found".into())
        }
    }

    pub fn update_delay(
        &mut self,
        node_id: usize,
        delay_ms: f32,
        feedback: f32,
        wet_mix: f32,
        enabled: bool,
    ) {
        if let Some(delay) = self.effect_mut::<Delay>(node_id) {
            delay.set_delay_ms(delay_ms);
            delay.set_feedback(feedback);
            delay.set_mix(wet_mix);
            delay.set_active(enabled);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_reverb(
        &mut self,
        node_id: usize,
        active: bool,
        room_size: f32,
        damp: f32,
        wet: f32,
        dry: f32,
        width: f32,
    ) {
        if let Some(reverb) = self.effect_mut::<Freeverb>(node_id) {
            reverb.set_room_size(room_size);
            reverb.set_damp(damp);
            reverb.set_wet(wet);
            reverb.set_dry(dry);
            reverb.set_width(width);
            reverb.set_active(active);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_chorus(
        &mut self,
        node_id: usize,
        active: bool,
        base_delay_ms: f32,
        depth_ms: f32,
        lfo_rate_hz: f32,
        feedback: f32,
        feedback_filter: f32,
        mix: f32,
        stereo_phase_offset_deg: f32,
    ) {
        let sample_rate = self.sample_rate;
        if let Some(chorus) = self.effect_mut::<Chorus>(node_id) {
            chorus.set_base_delay_ms(base_delay_ms);
            chorus.set_depth_ms(depth_ms);
            chorus.set_rate_hz(lfo_rate_hz);
            chorus.set_feedback(feedback);
            chorus.set_mix(mix);
            chorus.set_stereo_phase_offset_deg(stereo_phase_offset_deg);
            chorus.set_feedback_filter_cutoff(feedback_filter * sample_rate);
            chorus.set_active(active);
        }
    }

    pub fn update_convolver(&mut self, node_id: usize, wet_mix: f32, enabled: bool) {
        if let Some(convolver) = self.effect_mut::<Convolver>(node_id) {
            convolver.set_wet_level(wet_mix);
            convolver.set_enabled(enabled);
        }
    }

    /// The effect-stack node with this patch id (`EFFECT_NODE_ID_OFFSET` +
    /// its index), if it is a `T`; otherwise a warning.
    fn effect_mut<T: 'static>(&mut self, node_id: usize) -> Option<&mut T> {
        let index = node_id.checked_sub(EFFECT_NODE_ID_OFFSET)?;
        let found = self
            .effect_stack
            .effects
            .get_mut(index)
            .and_then(|effect| effect.node.as_any_mut().downcast_mut::<T>());
        if found.is_none() {
            eprintln!("No {} at effect index {index}", std::any::type_name::<T>());
        }
        found
    }

    fn build_nodes_from_canonical_voice(
        &mut self,
        canonical_voice: &PatchVoiceLayout,
    ) -> Result<(), String> {
        // Use the same creation order as wasm to ensure consistency
        self.sampler_data.clear();
        for_each_node_in_creation_order(canonical_voice, |node_type, patch_node| {
            let id = parse_node_id(&patch_node.id)?;

            // IMPORTANT: Call create_node_from_type *before* mutably borrowing self.voices
            // to avoid aliasing (&self for creation vs &mut self.voices for insertion).
            for voice_index in 0..self.voices.len() {
                let node = self.create_node_from_type(node_type, &id)?;

                {
                    let voice = &mut self.voices[voice_index];
                    voice.graph.add_node_with_id(id, node);

                    // If the node is a global node, store its ID for this voice
                    match node_type {
                        "global_frequency" => voice.graph.global_frequency_node = Some(id),
                        "glide" => {
                            voice.graph.global_glide_node = Some(id);
                            if let Some(global_freq) = voice.graph.global_frequency_node {
                                voice.graph.add_connection(Connection {
                                    from_node: global_freq,
                                    from_port: PortId::GlobalFrequency,
                                    to_node: id,
                                    to_port: PortId::AudioInput0,
                                    amount: 1.0,
                                    modulation_type: ModulationType::Additive,
                                    modulation_transform: ModulationTransformation::None,
                                });
                            }
                            if let Some(gate_mixer) = voice.graph.global_gatemixer_node {
                                voice.graph.add_connection(Connection {
                                    from_node: gate_mixer,
                                    from_port: PortId::CombinedGate,
                                    to_node: id,
                                    to_port: PortId::CombinedGate,
                                    amount: 1.0,
                                    modulation_type: ModulationType::Additive,
                                    modulation_transform: ModulationTransformation::None,
                                });
                            }
                        }
                        "global_velocity" => voice.graph.global_velocity_node = Some(id),
                        "gatemixer" => voice.graph.global_gatemixer_node = Some(id),
                        "mixer" => voice.graph.set_output_node(id),
                        _ => {}
                    }
                }
            }

            Ok::<(), String>(())
        })?;
        // Ensure the glide hears the combined gate even though the gate mixer is created later.
        for voice in &mut self.voices {
            if let (Some(glide_id), Some(gate_mixer_id)) = (
                voice.graph.global_glide_node,
                voice.graph.global_gatemixer_node,
            ) {
                voice.graph.add_connection(Connection {
                    from_node: gate_mixer_id,
                    from_port: PortId::CombinedGate,
                    to_node: glide_id,
                    to_port: PortId::CombinedGate,
                    amount: 1.0,
                    modulation_type: ModulationType::Additive,
                    modulation_transform: ModulationTransformation::None,
                });
            }
        }

        Ok(())
    }

    fn create_node_from_type(
        &mut self,
        node_type: &str,
        id: &NodeId,
    ) -> Result<Box<dyn AudioNode>, String> {
        match node_type {
            "oscillator" => Ok(Box::new(AnalogOscillator::new(
                self.sample_rate,
                Waveform::Sine,
                self.wavetable_banks.clone(),
            ))),
            "wavetable_oscillator" => Ok(Box::new(WavetableOscillator::new(
                self.sample_rate,
                self.wavetable_synthbank.clone(),
            ))),
            "filter" => Ok(Box::new(FilterCollection::new(self.sample_rate))),
            "envelope" => Ok(Box::new(Envelope::new(
                self.sample_rate,
                Default::default(),
            ))),
            "mixer" => Ok(Box::new(Mixer::new())),
            "lfo" => Ok(Box::new(Lfo::new(self.sample_rate))),
            "global_frequency" => Ok(Box::new(GlobalFrequencyNode::new(440.0, self.block_size))),
            "global_velocity" => Ok(Box::new(GlobalVelocityNode::new(1.0, self.block_size))),
            "gatemixer" => Ok(Box::new(GateMixer::new())),
            "glide" => {
                let mut glide = Glide::new(self.sample_rate, 0.0);
                glide.set_active(false);
                Ok(Box::new(glide))
            }
            "noise" => Ok(Box::new(NoiseGenerator::new(self.sample_rate))),
            "sampler" => {
                // One sample for the node in every voice; a patch's audio
                // asset replaces this placeholder (as in the wasm engine).
                let sample_rate = self.sample_rate;
                let data = self
                    .sampler_data
                    .entry(*id)
                    .or_insert_with(|| {
                        let mut sample = SampleData::new();
                        sample.load_from_wav(default_sampler_data(sample_rate), 1, sample_rate);
                        sample.root_note = 69.0;
                        Rc::new(RefCell::new(sample))
                    })
                    .clone();
                let mut sampler = Sampler::new(self.sample_rate);
                sampler.set_sample_data(data);
                Ok(Box::new(sampler))
            }
            "arpeggiator_generator" => {
                let mut arp = ArpeggiatorGenerator::new();
                arp.create_test_pattern(self.sample_rate, 0.225);
                Ok(Box::new(arp))
            }
            _ => Err(format!("Unknown node type: {}", node_type)),
        }
    }

    fn connect_from_canonical_voice(
        &mut self,
        canonical_voice: &PatchVoiceLayout,
    ) -> Result<(), String> {
        for conn_data in &canonical_voice.connections {
            let from_node = parse_node_id(&conn_data.from_id)?;
            let to_node = parse_node_id(&conn_data.to_id)?;

            for voice in &mut self.voices {
                let from_port = voice
                    .graph
                    .nodes
                    .get(&from_node)
                    .and_then(|n| {
                        n.get_ports()
                            .iter()
                            .find(|(_, &is_output)| is_output)
                            .map(|(p, _)| *p)
                    })
                    .unwrap_or(PortId::AudioOutput0);

                let mut effective_from_node = from_node;
                let mut effective_from_port = from_port;
                let to_port = PortId::from_u32(conn_data.target);

                if to_port == PortId::GlobalFrequency {
                    if let Some(glide_node) = voice.graph.global_glide_node {
                        effective_from_node = glide_node;
                        effective_from_port = PortId::AudioOutput0;
                    }
                }

                let connection = Connection {
                    from_node: effective_from_node,
                    from_port: effective_from_port,
                    to_node,
                    to_port,
                    amount: conn_data.amount,
                    modulation_type: ModulationType::from_i32(conn_data.modulation_type),
                    modulation_transform: ModulationTransformation::from_i32(
                        conn_data.modulation_transform,
                    ),
                };
                voice.graph.add_connection(connection);
            }
        }
        Ok(())
    }

    fn apply_patch_states(
        &mut self,
        patch: &PatchFile,
        _canonical_voice: &PatchVoiceLayout,
    ) -> Result<(), String> {
        for (id, params) in &patch.synth_state.oscillators {
            let node_id = parse_node_id(id)?;
            self.update_oscillator(node_id, params)?;
        }
        for (id, params) in &patch.synth_state.wavetable_oscillators {
            let node_id = parse_node_id(id)?;
            self.update_wavetable_oscillator(node_id, params)?;
        }
        for (id, config) in &patch.synth_state.envelopes {
            let node_id = parse_node_id(id)?;
            self.update_envelope(
                node_id,
                config.attack,
                config.decay,
                config.sustain,
                config.release,
                config.attack_curve,
                config.decay_curve,
                config.release_curve,
                config.active,
            )?;
        }
        for state in patch.synth_state.lfos.values() {
            self.update_lfo(parse_node_id(&state.lfo_id)?, state);
        }
        for filter in patch.synth_state.filters.values() {
            self.update_filters(
                parse_node_id(&filter.id)?,
                filter.cutoff,
                filter.resonance,
                filter.gain,
                filter.key_tracking,
                filter.comb_frequency,
                filter.comb_dampening,
                filter.oversampling,
                filter_type_from_i32(filter.filter_type)?,
                filter.filter_slope,
            )?;
        }
        for sampler in patch.synth_state.samplers.values() {
            self.update_sampler(
                parse_node_id(&sampler.id)?,
                sampler.frequency,
                sampler.gain,
                sampler.loop_mode,
                sampler.loop_start,
                sampler.loop_end,
                sampler.root_note,
                sampler.trigger_mode,
                sampler.active,
            )?;
        }
        for chorus in patch.synth_state.choruses.values() {
            if let Ok(node_id) = chorus.id.parse::<usize>() {
                self.update_chorus(
                    node_id,
                    chorus.active,
                    chorus.base_delay_ms,
                    chorus.depth_ms,
                    chorus.lfo_rate_hz,
                    chorus.feedback,
                    chorus.feedback_filter,
                    chorus.mix,
                    chorus.stereo_phase_offset_deg,
                );
            }
        }
        for delay in patch.synth_state.delays.values() {
            if let Ok(node_id) = delay.id.parse::<usize>() {
                self.update_delay(
                    node_id,
                    delay.delay_ms,
                    delay.feedback,
                    delay.wet_mix,
                    delay.active,
                );
            }
        }
        for reverb in patch.synth_state.reverbs.values() {
            if let Ok(node_id) = reverb.id.parse::<usize>() {
                self.update_reverb(
                    node_id,
                    reverb.active,
                    reverb.room_size,
                    reverb.damp,
                    reverb.wet,
                    reverb.dry,
                    reverb.width,
                );
            }
        }
        for convolver in patch.synth_state.convolvers.values() {
            if let Ok(node_id) = convolver.id.parse::<usize>() {
                self.update_convolver(node_id, convolver.wet_mix, convolver.active);
            }
        }
        if let Some(noise) = &patch.synth_state.noise {
            if let Some(noise_id) = find_node_id(_canonical_voice, "noise") {
                let noise_type = match noise.noise_type {
                    1 => NoiseType::Pink,
                    2 => NoiseType::Brownian,
                    _ => NoiseType::White,
                };
                self.update_noise(
                    parse_node_id(&noise_id)?,
                    noise_type,
                    noise.cutoff,
                    noise.gain,
                    noise.is_enabled,
                )?;
            }
        }
        if let Some(velocity) = &patch.synth_state.velocity {
            if let Some(velocity_id) = find_node_id(_canonical_voice, "global_velocity") {
                self.update_velocity(
                    parse_node_id(&velocity_id)?,
                    velocity.sensitivity,
                    velocity.randomize,
                )?;
            }
        }

        for glide in patch.synth_state.glides.values() {
            let glide_id = parse_node_id(&glide.glide_id)?;
            for voice in &mut self.voices {
                if let Some(node) = voice.graph.get_node_mut(glide_id) {
                    if let Some(glide_node) = node.as_any_mut().downcast_mut::<Glide>() {
                        glide_node.set_time(glide.resolved_time());
                        glide_node.set_active(glide.active);
                    }
                }
            }
        }

        for compressor in patch.synth_state.compressors.values() {
            if let Ok(node_id) = compressor.id.parse::<usize>() {
                if let Err(err) = self.update_compressor(
                    node_id,
                    compressor.active,
                    compressor.threshold_db,
                    compressor.ratio,
                    compressor.attack_ms,
                    compressor.release_ms,
                    compressor.makeup_gain_db,
                    compressor.mix,
                ) {
                    eprintln!("Failed to apply compressor state: {}", err);
                }
            }
        }

        for saturation in patch.synth_state.saturations.values() {
            if let Ok(node_id) = saturation.id.parse::<usize>() {
                if let Err(err) = self.update_saturation(
                    node_id,
                    saturation.drive,
                    saturation.mix,
                    saturation.active,
                ) {
                    eprintln!("Failed to apply saturation state: {}", err);
                }
            }
        }

        for bitcrusher in patch.synth_state.bitcrushers.values() {
            if let Ok(node_id) = bitcrusher.id.parse::<usize>() {
                if let Err(err) = self.update_bitcrusher(
                    node_id,
                    bitcrusher.bits,
                    bitcrusher.downsample_factor,
                    bitcrusher.mix,
                    bitcrusher.active,
                ) {
                    eprintln!("Failed to apply bitcrusher state: {}", err);
                }
            }
        }
        // ... and so on for other state types (LFOs, filters, etc.)
        Ok(())
    }

    pub fn process_audio(
        &mut self,
        gates: &[f32],
        frequencies: &[f32],
        gains: &[f32],
        velocities: &[f32],
        macro_values: &[f32],
        master_gain: f32,
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        let voice_macro_span = self.voices.len().saturating_mul(MACRO_COUNT);
        let macro_buffer_len = macro_values
            .len()
            .checked_div(voice_macro_span)
            .unwrap_or(0);
        self.process_audio_internal(
            gates,
            frequencies,
            gains,
            velocities,
            macro_values,
            macro_buffer_len,
            master_gain,
            output_left,
            output_right,
        );
    }

    fn process_audio_internal(
        &mut self,
        gates: &[f32],
        frequencies: &[f32],
        gains: &[f32],
        velocities: &[f32],
        macro_values: &[f32],
        macro_buffer_len: usize,
        master_gain: f32,
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        #[cfg(debug_assertions)]
        {
            use std::sync::atomic::{AtomicUsize, Ordering};
            // Debug logging is expensive; keep it out of release builds.
            static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
            let call_count = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            if call_count.is_multiple_of(1000) {
                eprintln!("ENGINE process_audio_internal call {}:", call_count);
                eprintln!(
                    "  output_left.len()={}, output_right.len()={}",
                    output_left.len(),
                    output_right.len()
                );
                eprintln!("  self.block_size={}", self.block_size);
            }
        }

        // wasm32-unknown-unknown has no clock (`Instant::now` panics there,
        // e.g. in the mixer's AudioWorklet): the CPU meter stays at 0.
        #[cfg(not(target_arch = "wasm32"))]
        let start = std::time::Instant::now();

        // Reuse preallocated buffers sized to the engine's block size.
        let block_len = self.block_size.max(1);
        self.mix_left.resize(block_len, 0.0);
        self.mix_right.resize(block_len, 0.0);
        self.voice_left.resize(block_len, 0.0);
        self.voice_right.resize(block_len, 0.0);
        self.effect_left.resize(block_len, 0.0);
        self.effect_right.resize(block_len, 0.0);

        self.mix_left.fill(0.0);
        self.mix_right.fill(0.0);
        self.voice_left.fill(0.0);
        self.voice_right.fill(0.0);
        self.effect_left.fill(0.0);
        self.effect_right.fill(0.0);

        let param_voice_count = if block_len > 0 && !gates.is_empty() {
            (gates.len() / block_len).max(1)
        } else {
            self.voices.len().max(1)
        };
        let voice_macro_stride = MACRO_COUNT * macro_buffer_len;
        let gate_buffer_len = block_len;
        let frequency_buffer_len = if !frequencies.is_empty() && param_voice_count > 0 {
            frequencies
                .len()
                .checked_div(param_voice_count)
                .unwrap_or(block_len)
                .min(block_len)
                .max(1)
        } else {
            block_len
        };

        for (i, voice) in self.voices.iter_mut().enumerate() {
            let gate_slice = if gate_buffer_len > 0 && i < param_voice_count {
                let start = i.saturating_mul(gate_buffer_len);
                let end = (start + gate_buffer_len).min(gates.len());
                gates.get(start..end).unwrap_or(&[])
            } else {
                &[]
            };

            let gate = if gate_slice.is_empty() {
                gates.get(i).copied().unwrap_or(0.0)
            } else {
                gate_slice.iter().copied().fold(0.0_f32, f32::max)
            };
            let frequency_slice = if frequency_buffer_len > 0 && i < param_voice_count {
                let start = i.saturating_mul(frequency_buffer_len);
                let end = (start + frequency_buffer_len).min(frequencies.len());
                frequencies.get(start..end).unwrap_or(&[])
            } else {
                &[]
            };
            let frequency = if frequency_slice.is_empty() {
                frequencies.get(i).copied().unwrap_or(440.0)
            } else {
                *frequency_slice.first().unwrap_or(&440.0)
            };
            let gain = gains.get(i).copied().unwrap_or(1.0);
            let velocity = velocities.get(i).copied().unwrap_or(0.0);

            voice.current_gate = gate;
            voice.current_frequency = frequency;
            voice.current_velocity = velocity;

            if macro_buffer_len > 0 {
                for macro_idx in 0..MACRO_COUNT {
                    let macro_start = i * voice_macro_stride + macro_idx * macro_buffer_len;
                    if macro_start + macro_buffer_len <= macro_values.len() {
                        let values = &macro_values[macro_start..macro_start + macro_buffer_len];
                        let _ = voice.update_macro(macro_idx, values);
                    }
                }
            }

            self.voice_left.fill(0.0);
            self.voice_right.fill(0.0);

            // Process the full block_size
            let zero_gate = [0.0_f32];
            let gate_buffer = if gate_slice.is_empty() {
                &zero_gate
            } else {
                gate_slice
            };
            let zero_frequency = [frequency];
            let frequency_buffer = if frequency_slice.is_empty() {
                &zero_frequency
            } else {
                frequency_slice
            };
            voice.process_audio(
                gate_buffer,
                frequency_buffer,
                &mut self.voice_left,
                &mut self.voice_right,
            );

            // Mix voices together
            for (sample_idx, (left, right)) in self
                .voice_left
                .iter()
                .zip(self.voice_right.iter())
                .enumerate()
            {
                self.mix_left[sample_idx] += left * gain;
                self.mix_right[sample_idx] += right * gain;
            }
        }

        // Process effects with full block_size buffers
        self.effect_stack.process_audio(
            &self.mix_left,
            &self.mix_right,
            &mut self.effect_left,
            &mut self.effect_right,
        );

        // Apply master gain
        if master_gain != 1.0 {
            for sample in self.effect_left.iter_mut() {
                *sample *= master_gain;
            }
            for sample in self.effect_right.iter_mut() {
                *sample *= master_gain;
            }
        }

        // CRITICAL: Only copy the requested number of samples to the output
        let copy_len = output_left.len().min(self.block_size);
        output_left[..copy_len].copy_from_slice(&self.effect_left[..copy_len]);
        output_right[..copy_len].copy_from_slice(&self.effect_right[..copy_len]);

        // Zero any remaining output if output buffers are longer than what we produced
        if copy_len < output_left.len() {
            output_left[copy_len..].fill(0.0);
        }
        if copy_len < output_right.len() {
            output_right[copy_len..].fill(0.0);
        }

        #[cfg(not(target_arch = "wasm32"))]
        let elapsed_sec = start.elapsed().as_secs_f64();
        #[cfg(target_arch = "wasm32")]
        let elapsed_sec = 0.0;
        let quantum_sec = self.block_size as f64 / self.sample_rate as f64;
        self.cpu_time_accum += elapsed_sec;
        self.audio_time_accum += quantum_sec;
        if self.audio_time_accum >= 0.1 {
            self.last_cpu_usage = ((self.cpu_time_accum / self.audio_time_accum) * 100.0) as f32;
            self.cpu_time_accum = 0.0;
            self.audio_time_accum = 0.0;
        }
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn process_with_frame(
        &mut self,
        frame: &AutomationFrame,
        master_gain: f32,
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        self.process_audio_internal(
            frame.gates(),
            frame.frequencies(),
            frame.gains(),
            frame.velocities(),
            frame.macro_buffers(),
            frame.macro_buffer_len(),
            master_gain,
            output_left,
            output_right,
        );
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn num_voices(&self) -> usize {
        self.num_voices
    }

    fn set_effect_active(&mut self, index: usize, active: bool) {
        if let Some(effect) = self.effect_stack.effects.get_mut(index) {
            effect.node.set_active(active);
        }
    }

    pub fn set_chorus_active(&mut self, active: bool) {
        self.set_effect_active(0, active);
    }

    pub fn set_delay_active(&mut self, active: bool) {
        self.set_effect_active(1, active);
    }

    pub fn set_reverb_active(&mut self, active: bool) {
        self.set_effect_active(2, active);
    }

    pub fn set_convolver_active(
        &mut self,
        node_id: usize,
        active: bool,
        wet_level: f32,
    ) -> Result<(), String> {
        let effect_id = node_id
            .checked_sub(EFFECT_NODE_ID_OFFSET)
            .ok_or_else(|| "Invalid convolver node id".to_string())?;

        let effect = self
            .effect_stack
            .effects
            .get_mut(effect_id)
            .ok_or_else(|| format!("No effect found at index {}", effect_id))?;

        if let Some(convolver) = effect.node.as_any_mut().downcast_mut::<Convolver>() {
            convolver.set_wet_level(wet_level.clamp(0.0, 1.0));
            convolver.set_enabled(active);
            Ok(())
        } else {
            Err(format!("Effect at index {} is not a Convolver", effect_id))
        }
    }

    pub fn generate_hall_impulse(&self, decay_time: f32, room_size: f32) -> Vec<f32> {
        self.ir_generator
            .hall(decay_time.clamp(0.1, 10.0), room_size.clamp(0.0, 1.0))
    }

    pub fn generate_plate_impulse(&self, decay_time: f32, diffusion: f32) -> Vec<f32> {
        self.ir_generator
            .plate(decay_time.clamp(0.1, 10.0), diffusion.clamp(0.0, 1.0))
    }

    pub fn update_effect_impulse(
        &mut self,
        effect_index: usize,
        impulse_response: Vec<f32>,
    ) -> Result<(), String> {
        if effect_index >= self.effect_stack.effects.len() {
            return Err("Effect index out of bounds".to_string());
        }
        if impulse_response.is_empty() {
            return Err("Impulse response cannot be empty".to_string());
        }

        let effect = self
            .effect_stack
            .effects
            .get_mut(effect_index)
            .ok_or_else(|| "Failed to get effect".to_string())?;

        if let Some(convolver) = effect.node.as_any_mut().downcast_mut::<Convolver>() {
            convolver.set_impulse_response(impulse_response);
            Ok(())
        } else {
            Err("Effect is not a Convolver".to_string())
        }
    }

    pub fn preview_envelope_from_js(
        &self,
        js_config: &[u8],
        preview_duration: f32,
    ) -> Result<Vec<f32>, String> {
        let config: crate::nodes::envelope::EnvelopeConfig = serde_json::from_slice(js_config)
            .map_err(|err| format!("Invalid envelope config: {err}"))?;

        Ok(Envelope::new(self.sample_rate, config).preview(preview_duration))
    }

    pub fn update_compressor(
        &mut self,
        node_id: usize,
        active: bool,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_gain_db: f32,
        mix: f32,
    ) -> Result<(), String> {
        let effect_id = node_id
            .checked_sub(EFFECT_NODE_ID_OFFSET)
            .ok_or_else(|| "Invalid compressor node id".to_string())?;

        let effect = self
            .effect_stack
            .effects
            .get_mut(effect_id)
            .ok_or_else(|| format!("No effect found at index {}", effect_id))?;

        if let Some(comp) = effect.node.as_any_mut().downcast_mut::<Compressor>() {
            comp.set_threshold_db(threshold_db);
            comp.set_ratio(ratio);
            comp.set_attack_ms(attack_ms);
            comp.set_release_ms(release_ms);
            comp.set_makeup_gain_db(makeup_gain_db);
            comp.set_mix(mix);
            comp.set_active(active);
            Ok(())
        } else {
            Err(format!("Effect at index {} is not a compressor", effect_id))
        }
    }

    pub fn update_saturation(
        &mut self,
        node_id: usize,
        drive: f32,
        mix: f32,
        active: bool,
    ) -> Result<(), String> {
        let effect_id = node_id
            .checked_sub(EFFECT_NODE_ID_OFFSET)
            .ok_or_else(|| "Invalid saturation node id".to_string())?;

        let effect = self
            .effect_stack
            .effects
            .get_mut(effect_id)
            .ok_or_else(|| format!("No effect found at index {}", effect_id))?;

        if let Some(saturation) = effect.node.as_any_mut().downcast_mut::<Saturation>() {
            saturation.set_drive(drive);
            saturation.set_mix(mix);
            saturation.set_active(active);
            Ok(())
        } else {
            Err(format!(
                "Effect at index {} is not a saturation effect",
                effect_id
            ))
        }
    }

    pub fn update_bitcrusher(
        &mut self,
        node_id: usize,
        bits: u8,
        downsample_factor: usize,
        mix: f32,
        active: bool,
    ) -> Result<(), String> {
        let effect_id = node_id
            .checked_sub(EFFECT_NODE_ID_OFFSET)
            .ok_or_else(|| "Invalid bitcrusher node id".to_string())?;

        let effect = self
            .effect_stack
            .effects
            .get_mut(effect_id)
            .ok_or_else(|| format!("No effect found at index {}", effect_id))?;

        if let Some(bitcrusher) = effect.node.as_any_mut().downcast_mut::<Bitcrusher>() {
            bitcrusher.set_bits(bits);
            bitcrusher.set_downsample_factor(downsample_factor);
            bitcrusher.set_mix(mix);
            bitcrusher.set_active(active);
            Ok(())
        } else {
            Err(format!(
                "Effect at index {} is not a bitcrusher effect",
                effect_id
            ))
        }
    }

    fn ensure_global_nodes(&mut self) -> Result<(), String> {
        if !self.voices.is_empty() {
            return Ok(());
        }

        let voice_count = 1;
        self.voices = (0..voice_count)
            .map(|id| Voice::new(id, self.block_size))
            .collect();
        Ok(())
    }

    // Node creation methods
    pub fn create_oscillator(&mut self) -> Result<usize, String> {
        self.ensure_global_nodes()?;
        let osc_id = NodeId::new();
        for voice in &mut self.voices {
            voice.graph.add_node_with_id(
                osc_id,
                Box::new(AnalogOscillator::new(
                    self.sample_rate,
                    Waveform::Sine,
                    self.wavetable_banks.clone(),
                )),
            );
        }
        Ok(osc_id.0.as_u128() as usize)
    }

    pub fn create_oscillator_node(&mut self) -> Result<NodeId, String> {
        self.ensure_global_nodes()?;
        let osc_id = NodeId::new();
        for voice in &mut self.voices {
            voice.graph.add_node_with_id(
                osc_id,
                Box::new(AnalogOscillator::new(
                    self.sample_rate,
                    Waveform::Sine,
                    self.wavetable_banks.clone(),
                )),
            );
        }
        Ok(osc_id)
    }

    pub fn create_wavetable_oscillator(&mut self) -> Result<usize, String> {
        let osc_id = NodeId::new();
        for voice in &mut self.voices {
            voice.graph.add_node_with_id(
                osc_id,
                Box::new(WavetableOscillator::new(
                    self.sample_rate,
                    self.wavetable_synthbank.clone(),
                )),
            );
        }
        Ok(osc_id.0.as_u128() as usize)
    }

    pub fn create_mixer(&mut self) -> Result<usize, String> {
        self.ensure_global_nodes()?;
        let mixer_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(mixer_id, Box::new(Mixer::new()));
            voice.graph.set_output_node(mixer_id);
        }
        Ok(mixer_id.0.as_u128() as usize)
    }

    pub fn create_mixer_node(&mut self) -> Result<NodeId, String> {
        self.ensure_global_nodes()?;
        let mixer_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(mixer_id, Box::new(Mixer::new()));
            voice.graph.set_output_node(mixer_id);
        }
        Ok(mixer_id)
    }

    pub fn create_envelope(&mut self) -> Result<usize, String> {
        self.ensure_global_nodes()?;
        let envelope_id = NodeId::new();
        for voice in &mut self.voices {
            voice.graph.add_node_with_id(
                envelope_id,
                Box::new(Envelope::new(self.sample_rate, EnvelopeConfig::default())),
            );
        }
        Ok(envelope_id.0.as_u128() as usize)
    }

    pub fn create_envelope_node(&mut self) -> Result<NodeId, String> {
        self.ensure_global_nodes()?;
        let envelope_id = NodeId::new();
        for voice in &mut self.voices {
            voice.graph.add_node_with_id(
                envelope_id,
                Box::new(Envelope::new(self.sample_rate, EnvelopeConfig::default())),
            );
        }
        Ok(envelope_id)
    }

    pub fn create_lfo(&mut self) -> Result<usize, String> {
        let lfo_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(lfo_id, Box::new(Lfo::new(self.sample_rate)));
        }
        Ok(lfo_id.0.as_u128() as usize)
    }

    pub fn create_filter(&mut self) -> Result<usize, String> {
        self.ensure_global_nodes()?;
        let filter_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(filter_id, Box::new(FilterCollection::new(self.sample_rate)));
        }
        Ok(filter_id.0.as_u128() as usize)
    }

    pub fn create_filter_node(&mut self) -> Result<NodeId, String> {
        self.ensure_global_nodes()?;
        let filter_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(filter_id, Box::new(FilterCollection::new(self.sample_rate)));
        }
        Ok(filter_id)
    }

    pub fn create_glide(&mut self, glide_time: f32) -> Result<usize, String> {
        self.ensure_global_nodes()?;
        let glide_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(glide_id, Box::new(Glide::new(self.sample_rate, glide_time)));
            voice.graph.global_glide_node = Some(glide_id);
            if let Some(global_freq) = voice.graph.global_frequency_node {
                voice.graph.add_connection(Connection {
                    from_node: global_freq,
                    from_port: PortId::GlobalFrequency,
                    to_node: glide_id,
                    to_port: PortId::AudioInput0,
                    amount: 1.0,
                    modulation_type: ModulationType::Additive,
                    modulation_transform: ModulationTransformation::None,
                });
            }
            if let Some(gate_mixer_id) = voice.graph.global_gatemixer_node {
                voice.graph.add_connection(Connection {
                    from_node: gate_mixer_id,
                    from_port: PortId::CombinedGate,
                    to_node: glide_id,
                    to_port: PortId::CombinedGate,
                    amount: 1.0,
                    modulation_type: ModulationType::Additive,
                    modulation_transform: ModulationTransformation::None,
                });
            }
        }
        Ok(glide_id.0.as_u128() as usize)
    }

    pub fn create_glide_node(&mut self, glide_time: f32) -> Result<NodeId, String> {
        self.ensure_global_nodes()?;
        let glide_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(glide_id, Box::new(Glide::new(self.sample_rate, glide_time)));
            voice.graph.global_glide_node = Some(glide_id);
            if let Some(global_freq) = voice.graph.global_frequency_node {
                voice.graph.add_connection(Connection {
                    from_node: global_freq,
                    from_port: PortId::GlobalFrequency,
                    to_node: glide_id,
                    to_port: PortId::AudioInput0,
                    amount: 1.0,
                    modulation_type: ModulationType::Additive,
                    modulation_transform: ModulationTransformation::None,
                });
            }
            if let Some(gate_mixer_id) = voice.graph.global_gatemixer_node {
                voice.graph.add_connection(Connection {
                    from_node: gate_mixer_id,
                    from_port: PortId::CombinedGate,
                    to_node: glide_id,
                    to_port: PortId::CombinedGate,
                    amount: 1.0,
                    modulation_type: ModulationType::Additive,
                    modulation_transform: ModulationTransformation::None,
                });
            }
        }
        Ok(glide_id)
    }

    /// Insert a Glide node between the global frequency source and the target node's
    /// GlobalFrequency input, so that pitch changes are slewed.
    pub fn insert_glide_on_global_frequency_by_index(
        &mut self,
        glide_id: usize,
        target_node: usize,
    ) -> Result<(), String> {
        self.insert_glide_on_global_frequency(
            NodeId(Uuid::from_u128(glide_id as u128)),
            NodeId(Uuid::from_u128(target_node as u128)),
        )
    }

    pub fn insert_glide_on_global_frequency(
        &mut self,
        glide_id: NodeId,
        target_node: NodeId,
    ) -> Result<(), String> {
        let glide_node = glide_id;
        let target_node_id = target_node;

        for voice in &mut self.voices {
            let global_freq_id = voice
                .graph
                .global_frequency_node
                .ok_or_else(|| "GlobalFrequencyNode not found in voice graph".to_string())?;

            // Remove the direct GlobalFrequency -> target connection, if present.
            voice.graph.remove_specific_connection(
                global_freq_id,
                target_node_id,
                PortId::GlobalFrequency,
            );

            // Connect GlobalFrequency -> Glide input.
            voice.graph.add_connection(Connection {
                from_node: global_freq_id,
                from_port: PortId::GlobalFrequency,
                to_node: glide_node,
                to_port: PortId::AudioInput0,
                amount: 1.0,
                modulation_type: ModulationType::Additive,
                modulation_transform: ModulationTransformation::None,
            });

            // Connect Glide output -> target GlobalFrequency input.
            voice.graph.add_connection(Connection {
                from_node: glide_node,
                from_port: PortId::AudioOutput0,
                to_node: target_node_id,
                to_port: PortId::GlobalFrequency,
                amount: 1.0,
                modulation_type: ModulationType::Additive,
                modulation_transform: ModulationTransformation::None,
            });
        }

        Ok(())
    }

    // pub fn create_noise(&mut self) -> Result<usize, String> {
    //     let mut noise_id = NodeId(0);
    //     for voice in &mut self.voices {
    //         noise_id = voice
    //             .graph
    //             .add_node(Box::new(NoiseGenerator::new(self.sample_rate)));
    //     }
    //     Ok(noise_id.0)
    // }

    // Connection methods
    pub fn connect_nodes_by_index(
        &mut self,
        from_node: usize,
        from_port: PortId,
        to_node: usize,
        to_port: PortId,
        amount: f32,
        modulation_type: ModulationType,
        modulation_transform: ModulationTransformation,
    ) -> Result<(), String> {
        self.connect_nodes(
            NodeId(Uuid::from_u128(from_node as u128)),
            from_port,
            NodeId(Uuid::from_u128(to_node as u128)),
            to_port,
            amount,
            modulation_type,
            modulation_transform,
        )
    }

    pub fn connect_nodes(
        &mut self,
        from_node: NodeId,
        from_port: PortId,
        to_node: NodeId,
        to_port: PortId,
        amount: f32,
        modulation_type: ModulationType,
        modulation_transform: ModulationTransformation,
    ) -> Result<(), String> {
        let connection = Connection {
            from_node,
            from_port,
            to_node,
            to_port,
            amount,
            modulation_type,
            modulation_transform,
        };

        for voice in &mut self.voices {
            voice.graph.add_connection(connection.clone());
        }
        Ok(())
    }

    // Parameter update methods
    pub fn update_oscillator(
        &mut self,
        oscillator_id: NodeId,
        params: &AnalogOscillatorStateUpdate,
    ) -> Result<(), String> {
        for voice in &mut self.voices {
            let node = voice
                .graph
                .get_node_mut(oscillator_id)
                .ok_or_else(|| "Node not found in one of the voices".to_string())?;
            let osc = node
                .as_any_mut()
                .downcast_mut::<AnalogOscillator>()
                .ok_or_else(|| {
                    "Node is not an AnalogOscillator in one of the voices".to_string()
                })?;
            osc.update_params(params);
        }
        Ok(())
    }

    pub fn update_wavetable_oscillator(
        &mut self,
        oscillator_id: NodeId,
        params: &WavetableOscillatorStateUpdate,
    ) -> Result<(), String> {
        for voice in &mut self.voices {
            let node = voice
                .graph
                .get_node_mut(oscillator_id)
                .ok_or_else(|| "Node not found in one of the voices".to_string())?;
            let osc = node
                .as_any_mut()
                .downcast_mut::<WavetableOscillator>()
                .ok_or_else(|| {
                    "Node is not a WavetableOscillator in one of the voices".to_string()
                })?;
            osc.update_params(params);
        }
        Ok(())
    }

    pub fn update_envelope(
        &mut self,
        node_id: NodeId,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
        attack_curve: f32,
        decay_curve: f32,
        release_curve: f32,
        active: bool,
    ) -> Result<(), String> {
        self.update_envelope_node(
            node_id,
            attack,
            decay,
            sustain,
            release,
            attack_curve,
            decay_curve,
            release_curve,
            active,
        )
    }

    pub fn update_envelope_by_index(
        &mut self,
        node_id: usize,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
        attack_curve: f32,
        decay_curve: f32,
        release_curve: f32,
        active: bool,
    ) -> Result<(), String> {
        self.update_envelope_node(
            NodeId(Uuid::from_u128(node_id as u128)),
            attack,
            decay,
            sustain,
            release,
            attack_curve,
            decay_curve,
            release_curve,
            active,
        )
    }

    pub fn update_envelope_node(
        &mut self,
        node_id: NodeId,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
        attack_curve: f32,
        decay_curve: f32,
        release_curve: f32,
        active: bool,
    ) -> Result<(), String> {
        let mut errors: Vec<String> = Vec::new();

        for (i, voice) in self.voices.iter_mut().enumerate() {
            if let Some(node) = voice.graph.get_node_mut(node_id) {
                if let Some(env) = node.as_any_mut().downcast_mut::<Envelope>() {
                    let config = EnvelopeConfig {
                        attack,
                        decay,
                        sustain,
                        release,
                        attack_curve,
                        decay_curve,
                        release_curve,
                        attack_smoothing_samples: 16,
                        active,
                    };
                    env.update_config(config);
                    env.set_active(active);
                } else {
                    errors.push(format!("Voice {}: Node is not an Envelope", i));
                }
            } else {
                errors.push(format!("Voice {}: Node not found", i));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub fn update_filters_by_index(
        &mut self,
        filter_id: usize,
        cutoff: f32,
        resonance: f32,
        gain: f32,
        key_tracking: f32,
        comb_frequency: f32,
        comb_dampening: f32,
        _oversampling: u32,
        filter_type: FilterType,
        filter_slope: FilterSlope,
    ) -> Result<(), String> {
        self.update_filters(
            NodeId(Uuid::from_u128(filter_id as u128)),
            cutoff,
            resonance,
            gain,
            key_tracking,
            comb_frequency,
            comb_dampening,
            _oversampling,
            filter_type,
            filter_slope,
        )
    }

    pub fn update_filters(
        &mut self,
        filter_id: NodeId,
        cutoff: f32,
        resonance: f32,
        gain: f32,
        key_tracking: f32,
        comb_frequency: f32,
        comb_dampening: f32,
        _oversampling: u32,
        filter_type: FilterType,
        filter_slope: FilterSlope,
    ) -> Result<(), String> {
        for voice in &mut self.voices {
            if let Some(node) = voice.graph.get_node_mut(filter_id) {
                if let Some(filter) = node.as_any_mut().downcast_mut::<FilterCollection>() {
                    filter.set_filter_type(filter_type);
                    filter.set_filter_slope(filter_slope);
                    filter.set_params(cutoff, resonance);
                    filter.set_comb_target_frequency(comb_frequency);
                    filter.set_comb_dampening(comb_dampening);
                    filter.set_gain_db(gain * 24.0 - 12.0);
                    filter.set_keyboard_tracking_sensitivity(key_tracking);
                } else {
                    return Err("Node is not a Filter".to_string());
                }
            } else {
                return Err("Node not found".to_string());
            }
        }
        Ok(())
    }

    // pub fn update_noise(
    //     &mut self,
    //     noise_id: usize,
    //     params: &NoiseUpdateParams,
    // ) -> Result<(), String> {
    //     for voice in &mut self.voices {
    //         if let Some(node) = voice.graph.get_node_mut(NodeId(noise_id)) {
    //             if let Some(noise) = node.as_any_mut().downcast_mut::<NoiseGenerator>() {
    //                 noise.update(NoiseUpdate {
    //                     noise_type: params.noise_type.into(),
    //                     cutoff: params.cutoff * self.sample_rate,
    //                     gain: params.gain,
    //                     enabled: params.enabled,
    //                 });
    //             } else {
    //                 return Err("Node is not a NoiseGenerator".to_string());
    //             }
    //         } else {
    //             return Err("Node not found".to_string());
    //         }
    //     }
    //     Ok(())
    // }
}

unsafe impl Send for AudioEngine {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "wasm"))]
    #[test]
    fn process_with_frame_outputs_audio_when_gate_is_active() {
        let sample_rate = 48_000.0;
        let mut engine = AudioEngine::new(sample_rate, 1);
        engine.init(sample_rate, 1);

        let voice = engine.voices.get_mut(0).expect("voice should exist");
        let osc_id = voice.graph.add_node(Box::new(AnalogOscillator::new(
            sample_rate,
            Waveform::Sine,
            engine.wavetable_banks.clone(),
        )));
        let mixer_id = voice.graph.add_node(Box::new(Mixer::new()));
        voice.graph.set_output_node(mixer_id);

        voice.graph.add_connection(Connection {
            from_node: osc_id,
            from_port: PortId::AudioOutput0,
            to_node: mixer_id,
            to_port: PortId::AudioInput0,
            amount: 1.0,
            modulation_type: ModulationType::Additive,
            modulation_transform: ModulationTransformation::None,
        });

        let mut frame = AutomationFrame::with_dimensions(engine.num_voices(), MACRO_COUNT, 64);
        frame.set_voice_values(0, 1.0, 440.0, 1.0, 1.0);

        let mut left = [0.0f32; 128];
        let mut right = [0.0f32; 128];

        engine.process_with_frame(&frame, 1.0, &mut left, &mut right);
        engine.process_with_frame(&frame, 1.0, &mut left, &mut right);

        let has_signal = left
            .iter()
            .chain(right.iter())
            .any(|&sample| sample.abs() > 1e-6);

        assert!(has_signal, "expected audio output after gate activation");
    }
}
