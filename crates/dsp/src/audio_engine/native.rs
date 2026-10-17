use crate::audio_engine::patch::{
    GlideState, PatchFile, PatchNode, VoiceLayout as PatchVoiceLayout,
};
use crate::audio_engine::patch_loader::for_each_node_in_creation_order;
use crate::audio_engine::patch_loader::parse_node_id;
use crate::automation::AutomationFrame;
use crate::biquad::FilterType;
use crate::effect_stack::EffectStack;
use crate::graph::{Connection, ModulationTransformation, ModulationType};
use crate::impulse_generator::ImpulseResponseGenerator;
use crate::nodes::morph_wavetable::WavetableSynthBank;
use crate::nodes::{
    AnalogOscillator, AnalogOscillatorStateUpdate, Bitcrusher, Chorus, Compressor, Convolver,
    Delay, Envelope, EnvelopeConfig, FilterCollection, FilterSlope, Freeverb, GateMixer, Glide,
    GlobalFrequencyNode, GlobalVelocityNode, Lfo, Limiter, Mixer, Saturation, Waveform,
    WavetableBank, WavetableOscillator, WavetableOscillatorStateUpdate,
};
//NoiseGenerator, NoiseUpdate,
use crate::traits::{AudioNode, PortId};
use crate::voice::Voice;
use crate::NodeId;
use rustc_hash::FxHashMap;
use std::{cell::RefCell, rc::Rc, sync::Arc, time::Instant};
use uuid::Uuid;

const DEFAULT_NUM_VOICES: usize = 8;
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

        Ok(voice_count)
    }

    fn build_nodes_from_canonical_voice(
        &mut self,
        canonical_voice: &PatchVoiceLayout,
    ) -> Result<(), String> {
        // Use the same creation order as wasm to ensure consistency
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
        &self,
        node_type: &str,
        _id: &NodeId,
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
            // "noise" => Ok(Box::new(NoiseGenerator::new(self.sample_rate))),
            // "sampler" => {
            //     let sample_data = Rc::new(RefCell::new(SampleData::new()));
            //     let mut sampler = Sampler::new(self.sample_rate);
            //     sampler.set_sample_data(sample_data);
            //     Ok(Box::new(sampler))
            // }
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

        let start = Instant::now();

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

        let elapsed_sec = start.elapsed().as_secs_f64();
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
