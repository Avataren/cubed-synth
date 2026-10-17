use super::patch::{AudioAsset, PatchFile, VoiceLayout as PatchVoiceLayout};
use super::patch_loader::{
    filter_type_from_i32, find_node_id, for_each_node_in_creation_order,
    modulation_transform_from_i32, modulation_type_from_i32, parse_audio_asset_id, parse_node_id,
    port_id_from_u32,
};
use crate::automation::AutomationFrame;
use crate::biquad::FilterType;
use crate::effect_stack::EffectStack;
use crate::graph::{Connection, ModulationTransformation, ModulationType, NodeId};
use crate::impulse_generator::ImpulseResponseGenerator;
use crate::nodes::morph_wavetable::{
    MipmappedWavetable, WavetableMorphCollection, WavetableSynthBank,
};
use crate::nodes::{
    generate_mipmapped_bank_dynamic, AnalogOscillator, AnalogOscillatorStateUpdate,
    ArpeggiatorGenerator, Bitcrusher, Chorus, Compressor, Convolver, Delay, Envelope,
    EnvelopeConfig, FilterCollection, FilterSlope, Freeverb, GateMixer, Glide, GlobalFrequencyNode,
    GlobalVelocityNode, Lfo, LfoLoopMode, LfoRetriggerMode, LfoWaveform, Limiter, Mixer,
    NoiseGenerator, NoiseType, NoiseUpdate, SampleData, Sampler, SamplerLoopMode,
    SamplerTriggerMode, Saturation, Waveform, WavetableBank, WavetableOscillator,
    WavetableOscillatorStateUpdate,
};
use crate::traits::{AudioNode, PortId};
use crate::voice::Voice;
use base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use base64::Engine as _;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use serde_json;
use std::{cell::RefCell, collections::HashMap, io::Cursor, rc::Rc, sync::Arc};

#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;
#[cfg(feature = "wasm")]
use web_sys::{console, js_sys};

use super::wav_decode::read_wav_samples_f32;
use hound;
use std::error::Error;
const EFFECT_NODE_ID_OFFSET: usize = 10_000;

#[cfg(target_arch = "wasm32")]
fn log_console(message: &str) {
    console::log_1(&message.into());
}

#[cfg(not(target_arch = "wasm32"))]
fn log_console(_message: &str) {}

fn import_wav_hound_reader<R: std::io::Read>(
    reader: R,
    base_size: usize,
) -> Result<WavetableMorphCollection, Box<dyn Error>> {
    let mut wav_reader = hound::WavReader::new(reader)?;
    let spec = wav_reader.spec();
    let sample_rate = spec.sample_rate as f32;

    // Read samples from the WAV (handling various bit depths/formats).
    let samples = read_wav_samples_f32(&mut wav_reader)?;

    let total_samples = samples.len();
    if total_samples % base_size != 0 {
        eprintln!(
            "Warning: {} extra samples will be ignored (not a complete wavetable)",
            total_samples % base_size
        );
    }
    let num_cycles = total_samples / base_size;
    log_console(&format!("Number of complete wavetables: {}", num_cycles));

    let mut collection = WavetableMorphCollection::new();

    for i in 0..num_cycles {
        let start = i * base_size;
        let end = start + base_size;
        let cycle_samples = samples[start..end].to_vec();

        // Generate a mipmapped bank using our dynamic method.
        let bank = generate_mipmapped_bank_dynamic(cycle_samples, base_size, sample_rate)?;
        // Wrap the bank in your collection’s expected type (for example, MipmappedWavetable).
        let mipmapped = MipmappedWavetable { bank };
        collection.add_wavetable(mipmapped);
    }

    Ok(collection)
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WasmNoiseType {
    White = 0,
    Pink = 1,
    Brownian = 2,
}

impl From<WasmNoiseType> for NoiseType {
    fn from(wasm_type: WasmNoiseType) -> Self {
        match wasm_type {
            WasmNoiseType::White => NoiseType::White,
            WasmNoiseType::Pink => NoiseType::Pink,
            WasmNoiseType::Brownian => NoiseType::Brownian,
        }
    }
}

#[derive(Deserialize)]
struct JsEnvelopeConfig {
    active: bool,
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    #[serde(rename = "attackCurve")]
    attack_curve: f32,
    #[serde(rename = "decayCurve")]
    decay_curve: f32,
    #[serde(rename = "releaseCurve")]
    release_curve: f32,
}
impl From<JsEnvelopeConfig> for EnvelopeConfig {
    fn from(js_conf: JsEnvelopeConfig) -> Self {
        EnvelopeConfig {
            attack: js_conf.attack,
            decay: js_conf.decay,
            sustain: js_conf.sustain,
            release: js_conf.release,
            attack_curve: js_conf.attack_curve,
            decay_curve: js_conf.decay_curve,
            release_curve: js_conf.release_curve,
            attack_smoothing_samples: 16, // a sensible default
            active: js_conf.active,
        }
    }
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct NoiseUpdateParams {
    pub noise_type: WasmNoiseType,
    pub cutoff: f32,
    pub gain: f32,
    pub enabled: bool,
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
impl NoiseUpdateParams {
    #[cfg_attr(feature = "wasm", wasm_bindgen(constructor))]
    pub fn new(
        noise_type: WasmNoiseType,
        cutoff: f32,
        gain: f32,
        enabled: bool,
    ) -> NoiseUpdateParams {
        NoiseUpdateParams {
            noise_type,
            cutoff,
            gain,
            enabled,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub enum WasmModulationType {
    VCA = 0,
    Bipolar = 1,
    Additive = 2,
}

impl From<WasmModulationType> for ModulationType {
    fn from(wasm_type: WasmModulationType) -> Self {
        match wasm_type {
            WasmModulationType::VCA => ModulationType::VCA,
            WasmModulationType::Bipolar => ModulationType::Bipolar,
            WasmModulationType::Additive => ModulationType::Additive,
        }
    }
}

impl From<ModulationType> for WasmModulationType {
    fn from(mod_type: ModulationType) -> Self {
        match mod_type {
            ModulationType::VCA => WasmModulationType::VCA,
            ModulationType::Bipolar => WasmModulationType::Bipolar,
            ModulationType::Additive => WasmModulationType::Additive,
        }
    }
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct AudioEngine {
    voices: Vec<Voice>,
    sample_rate: f32,
    num_voices: usize,
    wavetable_synthbank: Rc<RefCell<WavetableSynthBank>>,
    wavetable_banks: Arc<FxHashMap<Waveform, Arc<WavetableBank>>>,
    effect_stack: EffectStack,
    ir_generator: ImpulseResponseGenerator,
    cpu_time_accum: f64,   // accumulated processing time (seconds)
    audio_time_accum: f64, // accumulated quantum time (seconds)
    last_cpu_usage: f32,   // last computed average (%)
    block_size: usize,
    /// Scratch buffers for `process_audio`, kept across calls.
    ///
    /// These used to be four `vec![0.0; len]` allocations per render
    /// quantum -- heap traffic on the audio thread, ~375 times a second per
    /// engine at 48 kHz and 750 at 96 kHz, which is exactly where an
    /// allocator stall turns into an audible dropout. The native engine
    /// already reused preallocated buffers; this is the same fix on the path
    /// the browser actually runs. Resized in place when the block size
    /// changes, so the steady state allocates nothing.
    mix_left: Vec<f32>,
    mix_right: Vec<f32>,
    voice_left: Vec<f32>,
    voice_right: Vec<f32>,
}

/// Internal representation of LFO update parameters used by the engine.
#[derive(Clone)]
pub struct LfoUpdateParams {
    pub lfo_id: String,
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

#[derive(Serialize, Debug)]
struct EngineState {
    voices: Vec<VoiceState>,
}

#[derive(Serialize, Debug)]
struct VoiceState {
    id: usize,
    nodes: Vec<NodeState>,
    connections: Vec<ConnectionState>,
}

#[derive(Serialize, Debug)]
struct NodeState {
    id: String,
    node_type: String,
    name: String,
}

#[derive(Serialize, Debug)]
struct ConnectionState {
    from_id: String,
    to_id: String,
    target: u32,
    amount: f32,
    modulation_type: WasmModulationType,
    modulation_transform: ModulationTransformation,
}

/// Wasm-facing LFO params struct with only wasm-bindgen-safe field types.
#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct WasmLfoUpdateParams {
    /// UUID string for the target LFO node.
    lfo_id: String,
    frequency: f32,
    phase_offset: f32,
    waveform: u8,
    use_absolute: bool,
    use_normalized: bool,
    trigger_mode: u8,
    gain: f32,
    active: bool,
    loop_mode: usize,
    loop_start: f32,
    loop_end: f32,
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
impl WasmLfoUpdateParams {
    #[cfg_attr(feature = "wasm", wasm_bindgen(constructor))]
    pub fn new(
        lfo_id: String,
        frequency: f32,
        phase_offset: f32,
        waveform: u8,
        use_absolute: bool,
        use_normalized: bool,
        trigger_mode: u8,
        gain: f32,
        active: bool,
        loop_mode: usize,
        loop_start: f32,
        loop_end: f32,
    ) -> WasmLfoUpdateParams {
        WasmLfoUpdateParams {
            lfo_id,
            frequency,
            phase_offset,
            waveform,
            use_absolute,
            use_normalized,
            trigger_mode,
            gain,
            active,
            loop_mode,
            loop_start,
            loop_end,
        }
    }
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
impl AudioEngine {
    fn generate_default_sampler_data(sample_rate: f32) -> Vec<f32> {
        let duration_seconds = 0.5f32;
        let total_samples = (sample_rate * duration_seconds).max(1.0) as usize;
        let frequency = 220.0f32;
        let mut data = Vec::with_capacity(total_samples);
        for n in 0..total_samples {
            let t = n as f32 / sample_rate;
            let value = (2.0 * std::f32::consts::PI * frequency * t).sin();
            data.push(value);
        }
        data
    }
    #[cfg_attr(feature = "wasm", wasm_bindgen(constructor))]
    pub fn new(sample_rate: f32) -> Self {
        let num_voices = 8;
        let max_table_size = 2048;
        let buffer_size = 128;
        log_console(&format!(
            "INITIALIZING AUDIO ENGINE WITH {} VOICES",
            num_voices
        ));
        log_console("Creating WavetableSynthBank");
        let wavetable_synthbank = Rc::new(RefCell::new(WavetableSynthBank::new(sample_rate)));
        let mut banks = FxHashMap::default();
        log_console("Creating Sine");
        banks.insert(
            Waveform::Sine,
            Arc::new(
                WavetableBank::new(Waveform::Sine, max_table_size, sample_rate)
                    .expect("Failed to create Sine wavetable bank"),
            ),
        );
        log_console("Creating Saw");
        banks.insert(
            Waveform::Saw,
            Arc::new(
                WavetableBank::new(Waveform::Saw, max_table_size, sample_rate)
                    .expect("Failed to create Saw wavetable bank"),
            ),
        );
        banks.insert(
            Waveform::Square,
            Arc::new(
                WavetableBank::new(Waveform::Square, max_table_size, sample_rate)
                    .expect("Failed to create Square wavetable bank"),
            ),
        );
        banks.insert(
            Waveform::Triangle,
            Arc::new(
                WavetableBank::new(Waveform::Triangle, max_table_size, sample_rate)
                    .expect("Failed to create Triangle wavetable bank"),
            ),
        );

        Self {
            voices: Vec::new(),
            sample_rate,
            num_voices,
            wavetable_synthbank,
            wavetable_banks: Arc::new(banks),
            effect_stack: EffectStack::new(buffer_size),
            ir_generator: ImpulseResponseGenerator::new(sample_rate),
            cpu_time_accum: 0.0,
            audio_time_accum: 0.0,
            last_cpu_usage: 0.0,
            block_size: buffer_size,
            mix_left: vec![0.0; buffer_size],
            mix_right: vec![0.0; buffer_size],
            voice_left: vec![0.0; buffer_size],
            voice_right: vec![0.0; buffer_size],
        }
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn init(&mut self, sample_rate: f32, num_voices: usize) {
        self.sample_rate = sample_rate;
        self.num_voices = num_voices;

        self.voices = (0..num_voices)
            .map(|id| Voice::new(id, self.block_size))
            .collect();
        self.add_chorus().unwrap();
        self.add_delay(2000.0, 500.0, 0.5, 0.1).unwrap();
        self.add_freeverb(0.95, 0.5, 0.3, 0.7, 1.0).unwrap();
        self.add_plate_reverb(2.0, 0.6, sample_rate).unwrap();
        self.add_limiter().unwrap();
        self.add_compressor(-12.0, 4.0, 10.0, 80.0, 3.0, 0.5)
            .unwrap();
        self.add_saturation(2.0, 0.5, false).unwrap();
        self.add_bitcrusher(12, 4, 0.5, false).unwrap();
        //self.add_hall_reverb(2.0, 0.8, sample_rate).unwrap();
        log_console("plate reverb added");
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen(js_name = initWithPatch))]
    pub fn init_with_patch(&mut self, patch_json: &str) -> Result<usize, JsValue> {
        log_console(&format!(
            "Received patch JSON, byte length: {}, char length: {}",
            patch_json.len(),
            patch_json.chars().count()
        ));

        // Log a snippet around the problematic position
        if patch_json.len() > 26412 {
            let chars: Vec<char> = patch_json.chars().collect();
            log_console(&format!("Character count: {}", chars.len()));
            if chars.len() > 26412 {
                let start = 26400;
                let end = (26450).min(chars.len());
                let snippet: String = chars[start..end].iter().collect();
                log_console(&format!("JSON around char position 26412: {}", snippet));

                // Also log bytes
                let byte_snippet = &patch_json.as_bytes()
                    [26400.min(patch_json.len())..26450.min(patch_json.len())];
                log_console(&format!("Bytes: {:?}", byte_snippet));
            }
        }

        // First try to parse to see where it fails, and log an exact path on error.
        let mut deserializer = serde_json::Deserializer::from_str(patch_json);
        let patch: PatchFile = match serde_path_to_error::deserialize(&mut deserializer) {
            Ok(patch) => patch,
            Err(path_err) => {
                let path = path_err.path().to_string();
                let e = path_err.into_inner();

                log_console(&format!("Serde error at path {}: {}", path, e));
                log_console(&format!(
                    "Error line: {}, column (1-based): {}",
                    e.line(),
                    e.column()
                ));

                // Try to show a wide context around the error.
                let len = patch_json.len();
                if len > 0 {
                    // serde_json reports columns as 1-based; convert to 0-based for slicing.
                    let col0 = e.column().saturating_sub(1);
                    let start = col0.saturating_sub(200);
                    let end = (col0 + 200).min(len);

                    // Safe here because our JSON is plain ASCII (generated by JSON.stringify)
                    let context = &patch_json[start..end];
                    log_console(&format!(
                        "Context around error (bytes {}..{}): {}",
                        start, end, context
                    ));

                    let bytes = &patch_json.as_bytes()[start..end];
                    log_console(&format!(
                        "Byte values around error ({}..{}): {:?}",
                        start, end, bytes
                    ));

                    if col0 < len {
                        let b = patch_json.as_bytes()[col0];
                        log_console(&format!(
                            "Byte at error column (0-based idx {}): {} ('{}')",
                            col0, b, b as char
                        ));
                    }
                }

                return Err(JsValue::from_str(&format!(
                    "Failed to parse patch JSON at {}: {}",
                    path, e
                )));
            }
        };

        // Ensure mutable so we can inject defaults like glide
        let mut patch = patch;

        // Ensure a glide node exists to route global frequency through
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
                    .push(super::patch::PatchNode {
                        id: new_id.clone(),
                        node_type: "glide".to_string(),
                        name: "Glide".to_string(),
                    });
                patch.synth_state.glides.entry(new_id.clone()).or_insert(
                    super::patch::GlideState {
                        glide_id: new_id,
                        time: 0.0,
                        rise_time: None,
                        fall_time: None,
                        active: false,
                    },
                );
            }
        }

        let layout = &patch.synth_state.layout;
        let voice_count = layout.resolved_voice_count();
        if voice_count == 0 {
            return Err(JsValue::from_str("Patch contains no voices"));
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
        self.add_chorus()?;
        self.add_delay(2000.0, 500.0, 0.5, 0.1)?;
        self.add_freeverb(0.95, 0.5, 0.3, 0.7, 1.0)?;
        self.add_plate_reverb(2.0, 0.6, self.sample_rate)?;
        self.add_limiter()?;
        self.add_compressor(-12.0, 4.0, 10.0, 80.0, 3.0, 0.5)?;
        self.add_saturation(2.0, 0.5, false)?;
        self.add_bitcrusher(12, 4, 0.5, false)?;

        let canonical_voice = layout
            .canonical_voice()
            .ok_or_else(|| JsValue::from_str("Patch layout missing voice data"))?;

        self.build_nodes_from_canonical_voice(canonical_voice)?;
        self.connect_from_canonical_voice(canonical_voice)?;

        // DEBUG: inspect sampler-related connections after building from patch
        if let (Some(sampler_id_str), Some(filter_id_str)) = (
            find_node_id(canonical_voice, "sampler"),
            find_node_id(canonical_voice, "filter"),
        ) {
            match (
                NodeId::from_string(&sampler_id_str),
                NodeId::from_string(&filter_id_str),
            ) {
                (Ok(sampler_node_id), Ok(filter_node_id)) => {
                    if let Some(voice0) = self.voices.first() {
                        let all_conns = voice0.graph.debug_connections();
                        log_console(&format!(
                            "[init_with_patch] Voice 0 has {} total connections",
                            all_conns.len()
                        ));

                        for (_key, conn) in all_conns.into_iter() {
                            if conn.from_node == sampler_node_id || conn.to_node == sampler_node_id
                            {
                                log_console(&format!(
                                    "[init_with_patch] Sampler conn: from={} to={} from_port={:?} to_port={:?} amount={} mod_type={:?} mod_transform={:?} (filter_id={})",
                                    conn.from_node,
                                    conn.to_node,
                                    conn.from_port,
                                    conn.to_port,
                                    conn.amount,
                                    conn.modulation_type,
                                    conn.modulation_transform,
                                    filter_node_id,
                                ));
                            }
                        }
                    } else {
                        log_console("[init_with_patch] No voice 0 available when logging sampler connections");
                    }
                }
                (Err(e_sampler), Err(e_filter)) => {
                    log_console(&format!(
                        "[init_with_patch] Failed to parse sampler ({}) and filter ({}) UUIDs: sampler_err={}, filter_err={}",
                        sampler_id_str, filter_id_str, e_sampler, e_filter
                    ));
                }
                (Err(e_sampler), _) => {
                    log_console(&format!(
                        "[init_with_patch] Failed to parse sampler UUID {}: {}",
                        sampler_id_str, e_sampler
                    ));
                }
                (_, Err(e_filter)) => {
                    log_console(&format!(
                        "[init_with_patch] Failed to parse filter UUID {}: {}",
                        filter_id_str, e_filter
                    ));
                }
            }
        } else {
            log_console(
                "[init_with_patch] No sampler or filter node found in canonical voice; skipping sampler connection debug",
            );
        }

        self.apply_patch_states(&patch, canonical_voice)?;
        self.import_audio_assets(&patch.audio_assets)?;

        Ok(voice_count)
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn remove_connection(
        &mut self,
        from_node: &str,
        from_port: PortId,
        to_node: &str,
        to_port: PortId,
    ) -> Result<(), JsValue> {
        let from_node_id = NodeId::from_string(from_node)
            .map_err(|e| JsValue::from_str(&format!("Invalid from_node UUID: {}", e)))?;
        let to_node_id = NodeId::from_string(to_node)
            .map_err(|e| JsValue::from_str(&format!("Invalid to_node UUID: {}", e)))?;

        for voice in self.voices.iter_mut() {
            voice.graph.remove_connection(&Connection {
                from_node: from_node_id,
                from_port,
                to_node: to_node_id,
                to_port,
                amount: 0.0,
                modulation_type: ModulationType::VCA,
                modulation_transform: ModulationTransformation::None,
            });
        }
        Ok(())
    }

    #[cfg(feature = "wasm")]
    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn get_current_state(&self) -> JsValue {
        // Use voice 0 as the canonical layout.
        if let Some(voice) = self.voices.first() {
            let mut nodes: Vec<NodeState> = voice
                .graph
                .nodes
                .iter()
                .map(|(node_id, node)| NodeState {
                    id: node_id.to_string(),
                    node_type: node.node_type().to_string(),
                    name: node.name().to_string(),
                })
                .collect();

            // Append the effect stack's nodes with an offset.
            let effect_nodes: Vec<NodeState> = self
                .effect_stack
                .effects
                .iter()
                .enumerate()
                .map(|(i, effect)| NodeState {
                    id: (EFFECT_NODE_ID_OFFSET + i).to_string(), // unique pseudo-ID for effects
                    node_type: effect.node.node_type().to_string(),
                    name: effect.node.name().to_string(),
                })
                .collect();
            nodes.extend(effect_nodes);

            // Collect the voice's connections as before.
            let connections: Vec<ConnectionState> = voice
                .graph
                .connections
                .values()
                .map(|conn| ConnectionState {
                    from_id: conn.from_node.to_string(),
                    to_id: conn.to_node.to_string(),
                    target: conn.to_port as u32,
                    amount: conn.amount,
                    modulation_type: match conn.modulation_type {
                        ModulationType::VCA => WasmModulationType::VCA,
                        ModulationType::Bipolar => WasmModulationType::Bipolar,
                        ModulationType::Additive => WasmModulationType::Additive,
                    },
                    modulation_transform: conn.modulation_transform,
                })
                .collect();

            let canonical_voice = VoiceState {
                id: 0,
                nodes,
                connections,
            };

            let engine_state = EngineState {
                voices: vec![canonical_voice],
            };

            serde_wasm_bindgen::to_value(&engine_state).unwrap()
        } else {
            let engine_state = EngineState { voices: vec![] };
            serde_wasm_bindgen::to_value(&engine_state).unwrap()
        }
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
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
        // Begin CPU measurement:
        #[cfg(feature = "wasm")]
        let start = js_sys::Date::now();
        #[cfg(not(feature = "wasm"))]
        let start = std::time::Instant::now();
        // Reuse the preallocated scratch buffers; only a block-size change
        // touches the allocator.
        let frames = output_left.len();
        for buffer in [
            &mut self.mix_left,
            &mut self.mix_right,
            &mut self.voice_left,
            &mut self.voice_right,
        ] {
            buffer.resize(frames, 0.0);
        }
        self.mix_left.fill(0.0);
        self.mix_right.fill(0.0);

        let block_len = frames.max(1);
        // Parameter voice count is dictated by the automation adapter (fixed to descriptors, usually 8).
        let param_voice_count = if block_len > 0 && !gates.is_empty() {
            (gates.len() / block_len).max(1)
        } else {
            self.voices.len().max(1)
        };

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

        let macro_buffer_len = if !macro_values.is_empty() && param_voice_count > 0 {
            macro_values
                .len()
                .checked_div(param_voice_count * 4)
                .unwrap_or(0)
        } else {
            0
        };
        let voice_macro_stride = 4 * macro_buffer_len;

        // Process all voices and mix them
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

            // Update macro values
            if macro_buffer_len > 0 && i < param_voice_count {
                for macro_idx in 0..4 {
                    let macro_start = i * voice_macro_stride + (macro_idx * macro_buffer_len);
                    if macro_start + macro_buffer_len <= macro_values.len() {
                        let values = &macro_values[macro_start..macro_start + macro_buffer_len];
                        let _ = voice.update_macro(macro_idx, values);
                    }
                }
            }

            self.voice_left.fill(0.0);
            self.voice_right.fill(0.0);

            // Process voice audio
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

            // Mix voice into main mix buffers with gain
            for (i, (left, right)) in self
                .voice_left
                .iter()
                .zip(self.voice_right.iter())
                .enumerate()
            {
                self.mix_left[i] += left * gain;
                self.mix_right[i] += right * gain;
            }
        }

        // Process through effect stack
        self.effect_stack
            .process_audio(&self.mix_left, &self.mix_right, output_left, output_right);

        // Apply master gain after effects
        if master_gain != 1.0 {
            for sample in output_left.iter_mut() {
                *sample *= master_gain;
            }
            for sample in output_right.iter_mut() {
                *sample *= master_gain;
            }
        }

        #[cfg(feature = "wasm")]
        let elapsed_sec = {
            let end = js_sys::Date::now();
            (end - start) / 1000.0
        };

        #[cfg(not(feature = "wasm"))]
        let elapsed_sec = start.elapsed().as_secs_f64();
        // The available time for this block (`frames` samples) is:
        let quantum_sec = frames as f64 / self.sample_rate as f64;
        // Accumulate processing and quantum times:
        self.cpu_time_accum += elapsed_sec;
        self.audio_time_accum += quantum_sec;
        // Update average once enough quantum time has passed (e.g. 100ms):
        if self.audio_time_accum >= 0.1 {
            self.last_cpu_usage = ((self.cpu_time_accum / self.audio_time_accum) * 100.0) as f32;
            self.cpu_time_accum = 0.0;
            self.audio_time_accum = 0.0;
        }
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn process_with_frame(
        &mut self,
        frame: &AutomationFrame,
        master_gain: f32,
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        self.process_audio(
            frame.gates(),
            frame.frequencies(),
            frame.gains(),
            frame.velocities(),
            frame.macro_buffers(),
            master_gain,
            output_left,
            output_right,
        );
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn get_cpu_usage(&self) -> f32 {
        self.last_cpu_usage
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn add_saturation(&mut self, drive: f32, mix: f32, active: bool) -> Result<usize, JsValue> {
        let mut saturation = Saturation::new(drive, mix);
        saturation.set_active(active);
        Ok(self.effect_stack.add_effect(Box::new(saturation)))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn add_bitcrusher(
        &mut self,
        bits: u8,
        downsample_factor: usize,
        mix: f32,
        active: bool,
    ) -> Result<usize, JsValue> {
        let mut crusher = Bitcrusher::new(bits, downsample_factor, mix);
        crusher.set_active(active);
        Ok(self.effect_stack.add_effect(Box::new(crusher)))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn add_delay(
        &mut self,
        max_delay_ms: f32,
        delay_ms: f32,
        feedback: f32,
        mix: f32,
    ) -> Result<usize, JsValue> {
        let delay = Delay::new(self.sample_rate, max_delay_ms, delay_ms, feedback, mix);
        Ok(self.effect_stack.add_effect(Box::new(delay)))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn add_freeverb(
        &mut self,
        room_size: f32,
        damp: f32,
        wet: f32,
        dry: f32,
        width: f32,
    ) -> Result<usize, JsValue> {
        let reverb = Freeverb::new(self.sample_rate, room_size, damp, wet, dry, width);
        Ok(self.effect_stack.add_effect(Box::new(reverb)))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn add_hall_reverb(
        &mut self,
        decay_time: f32,
        room_size: f32,
        sample_rate: f32,
    ) -> Result<usize, JsValue> {
        // Validate parameters before processing
        let decay_time = decay_time.clamp(0.1, 10.0);
        let rsize = room_size.clamp(0.0, 1.0);
        // Generate hall reverb impulse response
        let ir = self.ir_generator.hall(decay_time, rsize);

        if ir.is_empty() {
            return Err(JsValue::from_str("Generated impulse response is empty"));
        }

        // Create convolver with bounds checking
        let mut convolver = Convolver::new(ir, 128, sample_rate);
        convolver.set_wet_level(0.1);
        Ok(self.effect_stack.add_effect(Box::new(convolver)))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn add_plate_reverb(
        &mut self,
        decay_time: f32,
        diffusion: f32,
        sample_rate: f32,
    ) -> Result<usize, JsValue> {
        // Validate parameters before processing
        let decay_time = decay_time.clamp(0.1, 10.0);
        let diffusion = diffusion.clamp(0.0, 1.0);
        // Generate plate reverb impulse response
        let ir = self.ir_generator.plate(decay_time, diffusion);

        if ir.is_empty() {
            return Err(JsValue::from_str("Generated impulse response is empty"));
        }

        // Create convolver with bounds checking
        let mut convolver = Convolver::new(ir, 128, sample_rate);
        convolver.set_wet_level(0.1);
        convolver.set_enabled(false);
        Ok(self.effect_stack.add_effect(Box::new(convolver)))
    }

    /// Generate a hall reverb impulse response and return it as a Vec<f32>
    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn generate_hall_impulse(&self, decay_time: f32, room_size: f32) -> Vec<f32> {
        let decay_time = decay_time.clamp(0.1, 10.0);
        let room_size = room_size.clamp(0.0, 1.0);
        self.ir_generator.hall(decay_time, room_size)
    }

    /// Generate a plate reverb impulse response and return it as a Vec<f32>
    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn generate_plate_impulse(&self, decay_time: f32, diffusion: f32) -> Vec<f32> {
        let decay_time = decay_time.clamp(0.1, 10.0);
        let diffusion = diffusion.clamp(0.0, 1.0);
        self.ir_generator.plate(decay_time, diffusion)
    }

    /// Update an existing effect's impulse response (for effects that are Convolvers)
    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn update_effect_impulse(
        &mut self,
        effect_index: usize,
        impulse_response: Vec<f32>,
    ) -> Result<(), JsValue> {
        if effect_index >= self.effect_stack.effects.len() {
            return Err(JsValue::from_str("Effect index out of bounds"));
        }

        if impulse_response.is_empty() {
            return Err(JsValue::from_str("Impulse response cannot be empty"));
        }

        // Get mutable reference to the effect
        if let Some(effect) = self.effect_stack.effects.get_mut(effect_index) {
            // Try to downcast to Convolver
            if let Some(convolver) = effect.node.as_any_mut().downcast_mut::<Convolver>() {
                convolver.set_impulse_response(impulse_response);
                Ok(())
            } else {
                Err(JsValue::from_str("Effect is not a Convolver"))
            }
        } else {
            Err(JsValue::from_str("Failed to get effect"))
        }
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn reorder_effects(&mut self, from_idx: usize, to_idx: usize) -> Result<(), JsValue> {
        self.effect_stack.reorder_effects(from_idx, to_idx);
        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn remove_effect(&mut self, index: usize) -> Result<(), JsValue> {
        self.effect_stack.remove_effect(index);
        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn delete_node(&mut self, node_id_str: &str) -> Result<(), JsValue> {
        let node_id = NodeId::from_string(node_id_str)
            .map_err(|e| JsValue::from_str(&format!("Invalid node UUID: {}", e)))?;

        log_console(&format!("Attempting to delete node with ID: {:?}", node_id));

        // Check if this is a special node that shouldn't be deleted
        if let Some(voice) = self.voices.first() {
            // Log the global node IDs for debugging
            log_console(&format!(
                "Global node IDs - Frequency: {:?}, Velocity: {:?}, GateMixer: {:?}",
                voice.graph.global_frequency_node,
                voice.graph.global_velocity_node,
                voice.graph.global_gatemixer_node
            ));

            if voice.graph.global_frequency_node == Some(node_id)
                || voice.graph.global_velocity_node == Some(node_id)
                || voice.graph.global_gatemixer_node == Some(node_id)
            {
                return Err(JsValue::from_str(
                    "Cannot delete node as it is a system node",
                ));
            }

            // Check if this is the output node
            if voice.graph.output_node == Some(node_id) {
                return Err(JsValue::from_str(
                    "Cannot delete node as it is the output node",
                ));
            }
        }

        // Log node type information for debugging
        if let Some(voice) = self.voices.first() {
            if let Some(node) = voice.graph.get_node(node_id) {
                log_console(&format!(
                    "Node {:?} is of type: {}, with {} connections",
                    node_id,
                    node.node_type(),
                    voice
                        .graph
                        .connections
                        .values()
                        .filter(|conn| conn.from_node == node_id || conn.to_node == node_id)
                        .count()
                ));
            }
        }

        // Delete the node from all voices using the simplified delete_node method
        for voice in &mut self.voices {
            voice.graph.delete_node(node_id);
        }

        log_console(&format!(
            "Node {:?} successfully deleted from all voices",
            node_id
        ));
        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn update_noise(
        &mut self,
        noise_id: &str,
        params: &NoiseUpdateParams,
    ) -> Result<(), JsValue> {
        let noise_node_id = NodeId::from_string(noise_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid noise_id UUID: {}", e)))?;

        for voice in &mut self.voices {
            if let Some(node) = voice.graph.get_node_mut(noise_node_id) {
                if let Some(noise) = node.as_any_mut().downcast_mut::<NoiseGenerator>() {
                    noise.update(NoiseUpdate {
                        noise_type: params.noise_type.into(),
                        cutoff: params.cutoff * self.sample_rate,
                        gain: params.gain,
                        enabled: params.enabled,
                    });
                } else {
                    return Err(JsValue::from_str("Node is not a NoiseGenerator"));
                }
            } else {
                return Err(JsValue::from_str("Node not found"));
            }
        }
        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn update_velocity(
        &mut self,
        node_id: &str,
        sensitivity: f32,
        randomize: f32,
    ) -> Result<(), JsValue> {
        let requested_node_id = NodeId::from_string(node_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid node_id UUID: {}", e)))?;

        let mut updated_any = false;

        for voice in &mut self.voices {
            let target_id = if voice.graph.get_node(requested_node_id).is_some() {
                Some(requested_node_id)
            } else {
                voice.graph.global_velocity_node
            };

            let Some(target_id) = target_id else {
                continue;
            };

            if let Some(node) = voice.graph.get_node_mut(target_id) {
                if let Some(velocity) = node.as_any_mut().downcast_mut::<GlobalVelocityNode>() {
                    velocity.set_sensitivity(sensitivity);
                    velocity.set_randomize(randomize);
                    updated_any = true;
                } else {
                    return Err(JsValue::from_str("Node is not a GlobalVelocityNode"));
                }
            }
        }

        if updated_any {
            Ok(())
        } else {
            Err(JsValue::from_str("Node not found"))
        }
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn get_gate_mixer_node_id(&mut self) -> Option<String> {
        self.voices
            .first()
            .and_then(|voice| voice.graph.global_gatemixer_node)
            .map(|id| id.to_string())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn update_envelope(
        &mut self,
        node_id: &str,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
        attack_curve: f32,
        decay_curve: f32,
        release_curve: f32,
        active: bool,
    ) -> Result<(), JsValue> {
        let mut errors: Vec<String> = Vec::new();

        let node_id = NodeId::from_string(node_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid node_id UUID: {}", e)))?;

        let mut found_any = false;
        // Iterate over all voices and attempt to update the envelope.
        for (i, voice) in self.voices.iter_mut().enumerate() {
            if let Some(node) = voice.graph.get_node_mut(node_id) {
                if let Some(env) = node.as_any_mut().downcast_mut::<Envelope>() {
                    found_any = true;
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

        if found_any && errors.is_empty() {
            Ok(())
        } else {
            Err(JsValue::from_str(&errors.join("; ")))
        }
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn get_envelope_preview(
        sample_rate: f32,
        js_config: JsValue,
        preview_duration: f32,
    ) -> Result<js_sys::Float32Array, JsValue> {
        // Deserialize the JS object into our helper struct.
        let js_conf: JsEnvelopeConfig = serde_wasm_bindgen::from_value(js_config)
            .map_err(|err| JsValue::from_str(&err.to_string()))?;

        // Convert it into our internal EnvelopeConfig.
        let config: EnvelopeConfig = js_conf.into();

        // Create a temporary envelope and generate the preview.
        let envelope = Envelope::new(sample_rate, config);
        let preview_values = envelope.preview(preview_duration);

        // Convert Vec<f32> into a Float32Array.
        let array = js_sys::Float32Array::new_with_length(preview_values.len() as u32);
        for (i, &value) in preview_values.iter().enumerate() {
            array.set_index(i as u32, value);
        }
        Ok(array)
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn update_wavetable_oscillator(
        &mut self,
        oscillator_id: &str,
        params: &WavetableOscillatorStateUpdate,
    ) -> Result<(), JsValue> {
        let oscillator_id = NodeId::from_string(oscillator_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid oscillator_id UUID: {}", e)))?;

        for voice in &mut self.voices {
            let node = voice
                .graph
                .get_node_mut(oscillator_id)
                .ok_or_else(|| JsValue::from_str("Node not found in one of the voices"))?;
            let osc = node
                .as_any_mut()
                .downcast_mut::<WavetableOscillator>()
                .ok_or_else(|| {
                    JsValue::from_str("Node is not a WavetableOscillator in one of the voices")
                })?;
            osc.update_params(params);
        }
        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn import_wave_impulse(&mut self, effect_id: usize, data: &[u8]) -> Result<(), JsValue> {
        use rubato::{
            Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
            WindowFunction,
        };
        use std::io::Cursor;

        log_console("Starting import_wave_impulse");

        // Create a hound reader from the data.
        let cursor = Cursor::new(data);
        let mut reader =
            hound::WavReader::new(cursor).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let spec = reader.spec();
        log_console(&format!(
            "WAV spec: sample_rate={}, channels={}, bits_per_sample={}, sample_format={:?}",
            spec.sample_rate, spec.channels, spec.bits_per_sample, spec.sample_format
        ));
        let num_channels = spec.channels;

        // Read the samples in f32 form.
        let samples = read_wav_samples_f32(&mut reader).map_err(|e| JsValue::from_str(&e))?;

        log_console(&format!("Read {} samples", samples.len()));

        // If the WAV is multi‑channel, average the channels to obtain a mono impulse response.
        let mut ir = if num_channels == 1 {
            samples
        } else {
            let mut mono = Vec::with_capacity(samples.len() / num_channels as usize);
            for chunk in samples.chunks(num_channels as usize) {
                let avg = chunk.iter().sum::<f32>() / (num_channels as f32);
                mono.push(avg);
            }
            mono
        };

        log_console(&format!("Mono IR length: {}", ir.len()));
        if ir.is_empty() {
            return Err(JsValue::from_str("Impulse response is empty"));
        }

        // Locate the effect in the effect stack using the given id (subtracting the offset).
        let index = effect_id
            .checked_sub(EFFECT_NODE_ID_OFFSET)
            .ok_or_else(|| JsValue::from_str("Invalid effect id"))?;
        if index >= self.effect_stack.effects.len() {
            return Err(JsValue::from_str("Effect id not found in effect stack"));
        }

        // Attempt to downcast the effect node to a Convolver.
        let effect = &mut self.effect_stack.effects[index];
        if let Some(convolver) = effect.node.as_any_mut().downcast_mut::<Convolver>() {
            let partition_size = convolver.partition_size;
            let target_sample_rate = convolver.sample_rate;
            let wet_level = convolver.wet_level;
            log_console(&format!(
                "Convolver target sample rate: {}",
                target_sample_rate
            ));

            // If the IR's sample rate doesn't match the target sample rate, resample it.
            if spec.sample_rate as f32 != target_sample_rate {
                log_console(&format!(
                    "Resampling needed: IR sample rate {} != target {}",
                    spec.sample_rate, target_sample_rate
                ));
                let conversion_ratio = target_sample_rate as f64 / spec.sample_rate as f64;
                log_console(&format!("Conversion ratio: {}", conversion_ratio));
                let chunk_size = 1024; // fixed chunk size
                log_console(&format!("Using chunk size: {}", chunk_size));

                // Set up the interpolation parameters per the rubato example.
                let params = SincInterpolationParameters {
                    sinc_len: 256,
                    f_cutoff: 0.95,
                    interpolation: SincInterpolationType::Linear,
                    oversampling_factor: 256,
                    window: WindowFunction::BlackmanHarris2,
                };

                // Create a SincFixedIn resampler for 1 channel.
                let mut resampler =
                    SincFixedIn::<f64>::new(conversion_ratio, 2.0, params, chunk_size, 1).map_err(
                        |e| JsValue::from_str(&format!("Resampler creation error: {:?}", e)),
                    )?;

                // Convert the IR to f64.
                let ir_f64: Vec<f64> = ir.iter().map(|&x| x as f64).collect();
                let mut resampled_output: Vec<f64> = Vec::new();

                // Process the IR in blocks of `chunk_size`.
                let mut pos = 0;
                while pos < ir_f64.len() {
                    let end = (pos + chunk_size).min(ir_f64.len());
                    let mut block = ir_f64[pos..end].to_vec();
                    if block.len() < chunk_size {
                        // Pad with zeros to fill the chunk.
                        block.resize(chunk_size, 0.0);
                    }
                    let input_block = vec![block]; // one channel
                    let out = resampler.process(&input_block, None).map_err(|e| {
                        JsValue::from_str(&format!("Resampling process error: {:?}", e))
                    })?;
                    // Append the output (first channel) to our accumulator.
                    resampled_output.extend(out[0].iter());
                    pos += chunk_size;
                }

                // Now flush any remaining samples using process_partial.
                let partial = resampler
                    .process_partial::<Vec<f64>>(None, None)
                    .map_err(|e| JsValue::from_str(&format!("Resampler flush error: {:?}", e)))?;
                resampled_output.extend(partial[0].iter());
                log_console(&format!(
                    "Resampled output length: {}",
                    resampled_output.len()
                ));

                // Replace IR with the resampled output (convert back to f32).
                ir = resampled_output.into_iter().map(|x| x as f32).collect();
                log_console(&format!("Final IR length after resampling: {}", ir.len()));
            } else {
                log_console("No resampling needed");
            }

            // Create a new convolver using the (resampled) impulse response.
            let new_convolver = Convolver::new(ir, partition_size, target_sample_rate);
            effect.node = Box::new(new_convolver);
            // Restore the original wet level.
            if let Some(new_conv) = effect.node.as_any_mut().downcast_mut::<Convolver>() {
                new_conv.set_wet_level(wet_level);
            }
            log_console("Impulse response imported successfully");
            Ok(())
        } else {
            Err(JsValue::from_str("Effect is not a Convolver"))
        }
    }

    /// Refactored import_wavetable function that uses the hound-based helper.
    /// It accepts the WAV data as a byte slice, uses a Cursor to create a reader,
    /// builds a new morph collection from the data, adds it to the synth bank under
    /// the name "imported", and then updates all wavetable oscillators to use it.
    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn import_wavetable(
        &mut self,
        node_id: &str,
        data: &[u8],
        base_size: usize,
    ) -> Result<(), JsValue> {
        let collection_name = format!("wt_{}", node_id);
        // Wrap the incoming data in a Cursor and call the hound helper.
        let cursor = Cursor::new(data);
        let collection = import_wav_hound_reader(cursor, base_size)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        // Keep existing collections (including the default) and register this one by node.
        {
            let mut bank = self.wavetable_synthbank.borrow_mut();
            if !bank.collections.contains_key("default") {
                bank.add_collection(
                    "default",
                    WavetableMorphCollection::generate_test_collection(self.sample_rate),
                );
            }
            bank.add_collection(&collection_name, collection);
        }

        // Parse the target oscillator ID.
        let node_id = NodeId::from_string(node_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid node_id UUID: {}", e)))?;

        let collection_name_clone = collection_name.clone();
        // Update the oscillator's active wavetable to the newly imported collection.
        for voice in &mut self.voices {
            let node = voice
                .graph
                .get_node_mut(node_id)
                .ok_or_else(|| JsValue::from_str("Node not found in one of the voices"))?;
            let osc = node
                .as_any_mut()
                .downcast_mut::<WavetableOscillator>()
                .ok_or_else(|| {
                    JsValue::from_str("Node is not a WavetableOscillator in one of the voices")
                })?;
            osc.set_current_wavetable(&collection_name_clone);
        }

        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn update_oscillator(
        &mut self,
        oscillator_id: &str,
        params: &AnalogOscillatorStateUpdate,
    ) -> Result<(), JsValue> {
        let oscillator_id = NodeId::from_string(oscillator_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid oscillator_id UUID: {}", e)))?;

        for voice in &mut self.voices {
            let node = voice
                .graph
                .get_node_mut(oscillator_id)
                .ok_or_else(|| JsValue::from_str("Node not found in one of the voices"))?;
            let osc = node
                .as_any_mut()
                .downcast_mut::<AnalogOscillator>()
                .ok_or_else(|| {
                    JsValue::from_str("Node is not an AnalogOscillator in one of the voices")
                })?;
            osc.update_params(params);
        }
        Ok(())
    }

    pub fn update_delay(
        &mut self,
        node_id: usize,
        delay_ms: f32,
        feedback: f32,
        wet_mix: f32,
        enabled: bool,
    ) {
        // Calculate the effect index based on the provided node_id.
        let effect_id = node_id - EFFECT_NODE_ID_OFFSET;

        // Try to get a mutable reference to the effect at that index.
        if let Some(effect) = self.effect_stack.effects.get_mut(effect_id) {
            // Attempt to downcast the boxed AudioNode to a Convolver.
            if let Some(delay) = effect.node.as_any_mut().downcast_mut::<Delay>() {
                delay.set_delay_ms(delay_ms);
                delay.set_feedback(feedback);
                delay.set_mix(wet_mix);
                delay.set_active(enabled);
            } else {
                // Log a warning if the node at that index isn't a Convolver.
                log_console(&format!("Effect at index {} is not a Delay", effect_id));
            }
        } else {
            // Log a warning if there is no effect at that index.
            log_console(&format!("No effect found at index {}", effect_id));
        }
    }

    pub fn update_saturation(&mut self, node_id: usize, drive: f32, mix: f32, active: bool) {
        let Some(effect_id) = node_id.checked_sub(EFFECT_NODE_ID_OFFSET) else {
            log_console(&format!(
                "Invalid saturation node id {}; expected offset {}",
                node_id, EFFECT_NODE_ID_OFFSET
            ));
            return;
        };

        if let Some(effect) = self.effect_stack.effects.get_mut(effect_id) {
            if let Some(saturation) = effect.node.as_any_mut().downcast_mut::<Saturation>() {
                saturation.set_drive(drive);
                saturation.set_mix(mix);
                saturation.set_active(active);
            } else {
                log_console(&format!(
                    "Effect at index {} is not a Saturation",
                    effect_id
                ));
            }
        } else {
            log_console(&format!("No effect found at index {}", effect_id));
        }
    }

    pub fn update_bitcrusher(
        &mut self,
        node_id: usize,
        bits: u8,
        downsample_factor: usize,
        mix: f32,
        active: bool,
    ) {
        let Some(effect_id) = node_id.checked_sub(EFFECT_NODE_ID_OFFSET) else {
            log_console(&format!(
                "Invalid bitcrusher node id {}; expected offset {}",
                node_id, EFFECT_NODE_ID_OFFSET
            ));
            return;
        };

        if let Some(effect) = self.effect_stack.effects.get_mut(effect_id) {
            if let Some(crusher) = effect.node.as_any_mut().downcast_mut::<Bitcrusher>() {
                crusher.set_bits(bits);
                crusher.set_downsample_factor(downsample_factor);
                crusher.set_mix(mix);
                crusher.set_active(active);
            } else {
                log_console(&format!(
                    "Effect at index {} is not a Bitcrusher",
                    effect_id
                ));
            }
        } else {
            log_console(&format!("No effect found at index {}", effect_id));
        }
    }

    pub fn update_convolver(&mut self, node_id: usize, wet_mix: f32, enabled: bool) {
        // Calculate the effect index based on the provided node_id.
        let effect_id = node_id - EFFECT_NODE_ID_OFFSET;

        // Try to get a mutable reference to the effect at that index.
        if let Some(effect) = self.effect_stack.effects.get_mut(effect_id) {
            // Attempt to downcast the boxed AudioNode to a Convolver.
            if let Some(convolver) = effect.node.as_any_mut().downcast_mut::<Convolver>() {
                convolver.set_wet_level(wet_mix);
                convolver.set_enabled(enabled);
            } else {
                // Log a warning if the node at that index isn't a Convolver.
                log_console(&format!("Effect at index {} is not a Convolver", effect_id));
            }
        } else {
            // Log a warning if there is no effect at that index.
            log_console(&format!("No effect found at index {}", effect_id));
        }
    }
    #[cfg_attr(feature = "wasm", wasm_bindgen)]

    pub fn add_limiter(&mut self) -> Result<usize, JsValue> {
        let mut limiter = Limiter::new(self.sample_rate, -0.5, 0.1, 50.0, 1.5, true);
        limiter.set_active(true);
        Ok(self.effect_stack.add_effect(Box::new(limiter)))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn add_compressor(
        &mut self,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_gain_db: f32,
        mix: f32,
    ) -> Result<usize, JsValue> {
        let mut compressor = Compressor::new(
            self.sample_rate,
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_gain_db,
            mix,
        );
        compressor.set_active(true);
        Ok(self.effect_stack.add_effect(Box::new(compressor)))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]

    pub fn add_chorus(&mut self) -> Result<usize, JsValue> {
        let mut chorus = Chorus::new(
            self.sample_rate,
            // max_base_delay_ms: Needs to accommodate the base delay + depth.
            // max base delay in ms:
            65.0,
            // base_delay_ms: The central delay time. 20-30ms is typical for chorus.
            15.0,
            // depth_ms: How much the delay time varies. 2-7ms is a common range.
            5.0,
            // lfo_rate_hz: Speed of the warble. Slow rates are classic chorus.
            // 0.3 - 0.8 Hz is a good starting range.
            0.5,
            // feedback: Usually low or zero for standard chorus. High feedback -> flanger.
            0.3,
            // mix: 0.5 gives an equal blend of dry and wet signal. Standard starting point.
            0.5,
            // stereo_phase_offset_deg: Creates stereo width. 90 degrees is common
            // and effective. 0 would be mono LFO. 180 is also common.
            90.0,
        );
        chorus.set_active(false);
        Ok(self.effect_stack.add_effect(Box::new(chorus)))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
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
        let effect_id = node_id - EFFECT_NODE_ID_OFFSET;

        // Try to get a mutable reference to the effect at that index.
        if let Some(effect) = self.effect_stack.effects.get_mut(effect_id) {
            // Attempt to downcast the boxed AudioNode to a Convolver.
            if let Some(reverb) = effect.node.as_any_mut().downcast_mut::<Freeverb>() {
                reverb.set_room_size(room_size);
                reverb.set_damp(damp);
                reverb.set_wet(wet);
                reverb.set_dry(dry);
                reverb.set_width(width);
                reverb.set_active(active);
            } else {
                // Log a warning if the node at that index isn't a Convolver.
                log_console(&format!("Effect at index {} is not a reverb", effect_id));
            }
        } else {
            // Log a warning if there is no effect at that index.
            log_console(&format!("No effect found at index {}", effect_id));
        }
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
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
    ) {
        let effect_id = node_id - EFFECT_NODE_ID_OFFSET;

        if let Some(effect) = self.effect_stack.effects.get_mut(effect_id) {
            if let Some(compressor) = effect.node.as_any_mut().downcast_mut::<Compressor>() {
                compressor.set_threshold_db(threshold_db);
                compressor.set_ratio(ratio);
                compressor.set_attack_ms(attack_ms);
                compressor.set_release_ms(release_ms);
                compressor.set_makeup_gain_db(makeup_gain_db);
                compressor.set_mix(mix);
                compressor.set_active(active);
            } else {
                log_console(&format!(
                    "Effect at index {} is not a compressor",
                    effect_id
                ));
            }
        } else {
            log_console(&format!("No effect found at index {}", effect_id));
        }
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
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
        let effect_id = node_id - EFFECT_NODE_ID_OFFSET;

        // Try to get a mutable reference to the effect at that index.
        if let Some(effect) = self.effect_stack.effects.get_mut(effect_id) {
            // Attempt to downcast the boxed AudioNode to a Convolver.
            if let Some(chorus) = effect.node.as_any_mut().downcast_mut::<Chorus>() {
                chorus.set_base_delay_ms(base_delay_ms);
                chorus.set_depth_ms(depth_ms);
                chorus.set_rate_hz(lfo_rate_hz);
                chorus.set_feedback(feedback);
                chorus.set_mix(mix);
                chorus.set_stereo_phase_offset_deg(stereo_phase_offset_deg);
                chorus.set_feedback_filter_cutoff(feedback_filter * self.sample_rate);
                chorus.set_active(active);
            } else {
                // Log a warning if the node at that index isn't a Convolver.
                log_console(&format!("Effect at index {} is not a chorus", effect_id));
            }
        } else {
            // Log a warning if there is no effect at that index.
            log_console(&format!("No effect found at index {}", effect_id));
        }
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn create_envelope(&mut self) -> Result<JsValue, JsValue> {
        let envelope_id = NodeId::new();
        for voice in &mut self.voices {
            voice.graph.add_node_with_id(
                envelope_id,
                Box::new(Envelope::new(self.sample_rate, EnvelopeConfig::default())),
            );
        }
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(
            &obj,
            &"envelopeId".into(),
            &JsValue::from_str(&envelope_id.to_string()),
        )?;

        Ok(obj.into())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn create_arpeggiator(&mut self) -> Result<JsValue, JsValue> {
        let arp_id = NodeId::new();
        for voice in &mut self.voices {
            let mut arp = ArpeggiatorGenerator::new();
            arp.create_test_pattern(self.sample_rate, 0.225);
            voice.graph.add_node_with_id(arp_id, Box::new(arp));

            if let Some(gate_mixer_id) = voice.graph.global_gatemixer_node {
                voice.graph.add_connection(Connection {
                    from_node: arp_id,
                    from_port: PortId::ArpGate,
                    to_node: gate_mixer_id,
                    to_port: PortId::ArpGate,
                    amount: 1.0,
                    modulation_type: ModulationType::VCA,
                    modulation_transform: ModulationTransformation::None,
                });
            }
        }
        Ok(JsValue::from_str(&arp_id.to_string()))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn create_mixer(&mut self) -> Result<JsValue, JsValue> {
        let mixer_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(mixer_id, Box::new(Mixer::new()));
            voice.graph.set_output_node(mixer_id);
        }
        // Just return the ID directly like other nodes
        Ok(JsValue::from_str(&mixer_id.to_string()))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn create_lfo(&mut self) -> Result<JsValue, JsValue> {
        let lfo_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(lfo_id, Box::new(Lfo::new(self.sample_rate)));
        }
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(
            &obj,
            &"lfoId".into(),
            &JsValue::from_str(&lfo_id.to_string()),
        )?;

        Ok(obj.into())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn create_filter(&mut self) -> Result<String, JsValue> {
        let filter_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(filter_id, Box::new(FilterCollection::new(self.sample_rate)));
        }
        Ok(filter_id.to_string())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn create_noise(&mut self) -> Result<String, JsValue> {
        let noise_id = NodeId::new();
        for voice in &mut self.voices {
            voice
                .graph
                .add_node_with_id(noise_id, Box::new(NoiseGenerator::new(self.sample_rate)));
        }
        Ok(noise_id.to_string())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn create_oscillator(&mut self) -> Result<String, JsValue> {
        let osc_id = NodeId::new();
        for voice in &mut self.voices {
            voice.graph.add_node_with_id(
                osc_id,
                Box::new(AnalogOscillator::new(
                    self.sample_rate,
                    Waveform::Sine,
                    self.wavetable_banks.clone(), // pass the shared banks
                )),
            );
        }
        Ok(osc_id.to_string())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn create_wavetable_oscillator(&mut self) -> Result<String, JsValue> {
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
        Ok(osc_id.to_string())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn create_sampler(&mut self) -> Result<String, JsValue> {
        let sample_data = Rc::new(RefCell::new(SampleData::new()));
        {
            let default_samples = Self::generate_default_sampler_data(self.sample_rate);
            let mut data_ref = sample_data.borrow_mut();
            data_ref.load_from_wav(default_samples, 1, self.sample_rate);
            data_ref.root_note = 69.0;
        }
        let sampler_id = NodeId::new();
        for voice in &mut self.voices {
            let mut sampler = Sampler::new(self.sample_rate);
            sampler.set_sample_data(sample_data.clone());
            voice.graph.add_node_with_id(sampler_id, Box::new(sampler));
        }
        Ok(sampler_id.to_string())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn import_sample(&mut self, sampler_id: &str, data: &[u8]) -> Result<(), JsValue> {
        use std::io::Cursor;

        log_console("Starting import_sample");

        // Create a hound reader from the data
        let cursor = Cursor::new(data);
        let mut reader =
            hound::WavReader::new(cursor).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let spec = reader.spec();
        log_console(&format!(
            "WAV spec: sample_rate={}, channels={}, bits_per_sample={}, sample_format={:?}",
            spec.sample_rate, spec.channels, spec.bits_per_sample, spec.sample_format
        ));

        // Read the samples in f32 form
        let samples = read_wav_samples_f32(&mut reader).map_err(|e| JsValue::from_str(&e))?;

        log_console(&format!("Read {} samples", samples.len()));

        // Create new sample data
        let sample_data = Rc::new(RefCell::new(SampleData::new()));
        sample_data.borrow_mut().load_from_wav(
            samples,
            spec.channels as usize,
            spec.sample_rate as f32,
        );

        // Parse sampler UUID
        let sampler_id = NodeId::from_string(sampler_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid sampler_id UUID: {}", e)))?;

        // Update all sampler nodes with the new sample data
        for voice in &mut self.voices {
            if let Some(node) = voice.graph.get_node_mut(sampler_id) {
                if let Some(sampler) = node.as_any_mut().downcast_mut::<Sampler>() {
                    sampler.set_sample_data(sample_data.clone());
                } else {
                    return Err(JsValue::from_str("Node is not a Sampler"));
                }
            } else {
                return Err(JsValue::from_str("Node not found"));
            }
        }

        log_console("Sample imported successfully");
        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn update_glide(
        &mut self,
        glide_id: &str,
        glide_time: f32,
        active: bool,
    ) -> Result<(), JsValue> {
        let glide_id = NodeId::from_string(glide_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid glide_id UUID: {}", e)))?;

        for voice in &mut self.voices {
            if let Some(node) = voice.graph.get_node_mut(glide_id) {
                if let Some(glide) = node.as_any_mut().downcast_mut::<Glide>() {
                    glide.set_time(glide_time);
                    glide.set_active(active);
                } else {
                    return Err(JsValue::from_str("Node is not a Glide"));
                }
            } else {
                return Err(JsValue::from_str("Node not found"));
            }
        }

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

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn update_sampler(
        &mut self,
        sampler_id: &str,
        frequency: f32,
        gain: f32,
        loop_mode: u8,
        loop_start: f32,
        loop_end: f32,
        root_note: f32,
        trigger_mode: u8,
        active: bool,
    ) -> Result<(), JsValue> {
        let loop_mode = match loop_mode {
            0 => SamplerLoopMode::Off,
            1 => SamplerLoopMode::Loop,
            2 => SamplerLoopMode::PingPong,
            _ => SamplerLoopMode::Off,
        };

        let trigger_mode = match trigger_mode {
            0 => SamplerTriggerMode::FreeRunning,
            1 => SamplerTriggerMode::Gate,
            2 => SamplerTriggerMode::OneShot,
            _ => SamplerTriggerMode::Gate,
        };

        let sampler_id = NodeId::from_string(sampler_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid sampler_id UUID: {}", e)))?;

        for voice in &mut self.voices {
            if let Some(node) = voice.graph.get_node_mut(sampler_id) {
                if let Some(sampler) = node.as_any_mut().downcast_mut::<Sampler>() {
                    sampler.set_base_frequency(frequency);
                    sampler.set_base_gain(gain);
                    sampler.set_loop_mode(loop_mode);
                    sampler.set_loop_start(loop_start);
                    sampler.set_loop_end(loop_end);
                    sampler.set_root_note(root_note);
                    sampler.set_trigger_mode(trigger_mode);
                    sampler.set_active(active);
                } else {
                    return Err(JsValue::from_str("Node is not a Sampler"));
                }
            } else {
                return Err(JsValue::from_str("Node not found"));
            }
        }
        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn get_sampler_waveform(
        &self,
        sampler_id: &str,
        max_points: usize,
    ) -> Result<Vec<f32>, JsValue> {
        let voice = self
            .voices
            .first()
            .ok_or_else(|| JsValue::from_str("No voices available"))?;
        let node = voice
            .graph
            .get_node(
                NodeId::from_string(sampler_id)
                    .map_err(|e| JsValue::from_str(&format!("Invalid sampler_id UUID: {}", e)))?,
            )
            .ok_or_else(|| JsValue::from_str("Sampler node not found"))?;
        let sampler = node
            .as_any()
            .downcast_ref::<Sampler>()
            .ok_or_else(|| JsValue::from_str("Node is not a Sampler"))?;
        let sample_data = sampler.get_sample_data();
        let data_ref = sample_data.borrow();
        let channels = data_ref.channels.max(1);
        let total_frames = data_ref.len();
        if total_frames == 0 {
            return Ok(Vec::new());
        }

        let target_points = max_points.max(32).min(total_frames);
        let mut waveform = Vec::with_capacity(target_points);
        let step = total_frames as f32 / target_points as f32;

        for i in 0..target_points {
            let frame = ((i as f32 * step).floor() as usize).min(total_frames - 1);
            let mut value = 0.0;
            for ch in 0..channels {
                let idx = frame * channels + ch;
                value += data_ref.samples[idx];
            }
            waveform.push(value / channels as f32);
        }

        Ok(waveform)
    }

    /// Export raw sample data with metadata for serialization
    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn export_sample_data(&self, sampler_id: &str) -> Result<js_sys::Object, JsValue> {
        let voice = self
            .voices
            .first()
            .ok_or_else(|| JsValue::from_str("No voices available"))?;
        let node = voice
            .graph
            .get_node(
                NodeId::from_string(sampler_id)
                    .map_err(|e| JsValue::from_str(&format!("Invalid sampler_id UUID: {}", e)))?,
            )
            .ok_or_else(|| JsValue::from_str("Sampler node not found"))?;
        let sampler = node
            .as_any()
            .downcast_ref::<Sampler>()
            .ok_or_else(|| JsValue::from_str("Node is not a Sampler"))?;

        let sample_data = sampler.get_sample_data();
        let data_ref = sample_data.borrow();

        // Create a JavaScript object with metadata and data
        let result = js_sys::Object::new();

        // Set metadata
        js_sys::Reflect::set(
            &result,
            &JsValue::from_str("sampleRate"),
            &JsValue::from_f64(data_ref.sample_rate as f64),
        )?;
        js_sys::Reflect::set(
            &result,
            &JsValue::from_str("channels"),
            &JsValue::from_f64(data_ref.channels as f64),
        )?;
        js_sys::Reflect::set(
            &result,
            &JsValue::from_str("rootNote"),
            &JsValue::from_f64(data_ref.root_note as f64),
        )?;

        // Convert samples vec to JavaScript Float32Array
        let samples_array = js_sys::Float32Array::from(&data_ref.samples[..]);
        js_sys::Reflect::set(&result, &JsValue::from_str("samples"), &samples_array)?;

        Ok(result)
    }

    /// Export raw convolver impulse response with metadata for serialization
    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn export_convolver_data(&self, convolver_id: &str) -> Result<js_sys::Object, JsValue> {
        // Try to resolve the ID as a graph node UUID first.
        let mut convolver_ref: Option<&Convolver> = None;

        if let Some(voice) = self.voices.first() {
            if let Ok(node_uuid) = NodeId::from_string(convolver_id) {
                if let Some(node) = voice.graph.get_node(node_uuid) {
                    if let Some(conv) = node.as_any().downcast_ref::<Convolver>() {
                        convolver_ref = Some(conv);
                    }
                }
            }
        }

        // If that failed, fall back to treating the ID as an effect-stack ID
        // (e.g. "10003" for EFFECT_NODE_ID_OFFSET + index).
        if convolver_ref.is_none() {
            let effect_id: usize = convolver_id
                .parse()
                .map_err(|_| JsValue::from_str("Invalid convolver_id: not UUID or effect id"))?;

            let index = effect_id
                .checked_sub(EFFECT_NODE_ID_OFFSET)
                .ok_or_else(|| JsValue::from_str("Invalid convolver_id: below effect offset"))?;

            let effect = self
                .effect_stack
                .effects
                .get(index)
                .ok_or_else(|| JsValue::from_str("Convolver effect not found"))?;

            let conv = effect
                .node
                .as_any()
                .downcast_ref::<Convolver>()
                .ok_or_else(|| JsValue::from_str("Effect is not a Convolver"))?;

            convolver_ref = Some(conv);
        }

        let convolver =
            convolver_ref.ok_or_else(|| JsValue::from_str("Convolver not found for given id"))?;

        let (ir_channels, sample_rate, num_channels) = convolver.get_impulse_response_data();

        // Create a JavaScript object with metadata and data
        let result = js_sys::Object::new();

        // Set metadata
        js_sys::Reflect::set(
            &result,
            &JsValue::from_str("sampleRate"),
            &JsValue::from_f64(sample_rate as f64),
        )?;
        js_sys::Reflect::set(
            &result,
            &JsValue::from_str("channels"),
            &JsValue::from_f64(num_channels as f64),
        )?;

        // Flatten the multi-channel IR into a single interleaved buffer
        // For stereo: [L0, R0, L1, R1, L2, R2, ...]
        let ir_length = ir_channels.first().map_or(0, |ch| ch.len());
        let mut interleaved = Vec::with_capacity(ir_length * num_channels);

        for i in 0..ir_length {
            for ch in 0..num_channels {
                if let Some(channel_data) = ir_channels.get(ch) {
                    if let Some(&sample) = channel_data.get(i) {
                        interleaved.push(sample);
                    } else {
                        interleaved.push(0.0);
                    }
                } else {
                    interleaved.push(0.0);
                }
            }
        }

        // Convert to JavaScript Float32Array
        let samples_array = js_sys::Float32Array::from(&interleaved[..]);
        js_sys::Reflect::set(&result, &JsValue::from_str("samples"), &samples_array)?;

        Ok(result)
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn update_filters(
        &mut self,
        filter_id: &str,
        cutoff: f32,
        resonance: f32,
        gain: f32,
        key_tracking: f32,
        comb_frequency: f32,
        comb_dampening: f32,
        _oversampling: u32,
        filter_type: FilterType,
        filter_slope: FilterSlope,
    ) -> Result<(), JsValue> {
        let filter_id = NodeId::from_string(filter_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid filter_id UUID: {}", e)))?;

        for voice in &mut self.voices {
            if let Some(node) = voice.graph.get_node_mut(filter_id) {
                if let Some(filter) = node.as_any_mut().downcast_mut::<FilterCollection>() {
                    filter.set_filter_type(filter_type);
                    filter.set_filter_slope(filter_slope);
                    //if (cutoff > 0.01) {
                    filter.set_params(cutoff, resonance);
                    //} else {
                    //filter.set_params(comb_frequency, resonance);
                    //}
                    filter.set_comb_target_frequency(comb_frequency);
                    filter.set_comb_dampening(comb_dampening);
                    filter.set_gain_db(gain * 24.0 - 12.0);
                    //log key_tracking
                    // log_console(&format!("key_tracking is {}", key_tracking));
                    filter.set_keyboard_tracking_sensitivity(key_tracking);
                    //filter.set_oversampling_factor(oversampling);
                } else {
                    return Err(JsValue::from_str("Node is not a Filter"));
                }
            } else {
                return Err(JsValue::from_str("Node not found"));
            }
        }
        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn get_filter_ir_waveform(
        &mut self,
        node_id: &str,
        waveform_length: usize,
    ) -> Result<Vec<f32>, JsValue> {
        let node_id = NodeId::from_string(node_id)
            .map_err(|e| JsValue::from_str(&format!("Invalid node_id UUID: {}", e)))?;

        // Every voice holds the same patch, so the first one answers.
        let Some(voice) = self.voices.first_mut() else {
            return Ok(vec![]);
        };
        let node = voice
            .graph
            .get_node_mut(node_id)
            .ok_or_else(|| JsValue::from_str("Node not found"))?;
        node.as_any_mut()
            .downcast_mut::<FilterCollection>()
            .map(|filter| filter.generate_frequency_response(waveform_length))
            .ok_or_else(|| JsValue::from_str("Node is not a Filter"))
    }

    /// Update all LFOs across all voices. This is called by the host when the user
    /// changes an LFO's settings.
    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn update_lfos(&mut self, params: WasmLfoUpdateParams) {
        let lfo_id = match NodeId::from_string(&params.lfo_id) {
            Ok(id) => id,
            Err(e) => {
                log_console(&format!("Invalid LFO UUID in update_lfos: {}", e));
                return;
            }
        };

        for voice in &mut self.voices {
            if let Some(node) = voice.graph.get_node_mut(lfo_id) {
                if let Some(lfo) = node.as_any_mut().downcast_mut::<Lfo>() {
                    // Convert u8 to LfoWaveform
                    //log_console(&format!("waveform is {}", params.waveform));
                    let waveform = match params.waveform {
                        0 => LfoWaveform::Sine,
                        1 => LfoWaveform::Triangle,
                        2 => LfoWaveform::Square,
                        3 => LfoWaveform::Saw,
                        4 => LfoWaveform::InverseSaw,
                        _ => LfoWaveform::Sine,
                    };

                    let loopmode = match params.loop_mode {
                        0 => LfoLoopMode::Off,
                        1 => LfoLoopMode::Loop,
                        2 => LfoLoopMode::PingPong,
                        _ => LfoLoopMode::Off,
                    };

                    lfo.set_gain(params.gain);
                    lfo.set_phase_offset(params.phase_offset);
                    lfo.set_frequency(params.frequency);
                    lfo.set_waveform(waveform);
                    lfo.set_use_absolute(params.use_absolute);
                    lfo.set_use_normalized(params.use_normalized);
                    lfo.set_retrigger_mode(LfoRetriggerMode::from_u8(params.trigger_mode));
                    lfo.set_active(params.active);
                    lfo.set_loop_mode(loopmode);
                    lfo.set_loop_start(params.loop_start);
                    lfo.set_loop_end(params.loop_end);
                }
            }
        }
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn get_lfo_waveform(
        &mut self,
        waveform: u8,
        phase_offset: f32,
        frequency: f32,
        buffer_size: usize,
        use_absolute: bool,
        use_normalized: bool,
    ) -> Result<Vec<f32>, JsValue> {
        let waveform = match waveform {
            0 => LfoWaveform::Sine,
            1 => LfoWaveform::Triangle,
            2 => LfoWaveform::Square,
            3 => LfoWaveform::Saw,
            4 => LfoWaveform::InverseSaw,
            _ => return Err(JsValue::from_str("Invalid waveform type")),
        };

        Ok(Lfo::get_waveform_data(
            waveform,
            phase_offset,
            frequency,
            buffer_size,
            use_absolute,
            use_normalized,
        ))
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn connect_nodes(
        &mut self,
        from_node: &str,
        from_port: PortId,
        to_node: &str,
        to_port: PortId,
        amount: f32,
        modulation_type: Option<WasmModulationType>,
        modulation_transform: ModulationTransformation,
    ) -> Result<(), JsValue> {
        // console::log_1(
        //     &format!(
        //         "RUST: Connecting nodes: from={}, to={}, modulation type={:?}",
        //         from_node, to_node, modulation_type
        //     )
        //     .into(),
        // );
        let from_node_id = NodeId::from_string(from_node)
            .map_err(|e| JsValue::from_str(&format!("Invalid from_node UUID: {}", e)))?;
        let to_node_id = NodeId::from_string(to_node)
            .map_err(|e| JsValue::from_str(&format!("Invalid to_node UUID: {}", e)))?;

        let connection = Connection {
            from_node: from_node_id,
            from_port,
            to_node: to_node_id,
            to_port,
            amount,
            modulation_type: modulation_type
                .map(ModulationType::from)
                .unwrap_or_default(),
            modulation_transform,
        };

        for voice in &mut self.voices {
            voice.graph.add_connection(connection.clone());
        }
        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn remove_specific_connection(
        &mut self,
        from_node: &str,
        to_node: &str,
        to_port: PortId,
    ) -> Result<(), JsValue> {
        log_console(&format!(
            "Removing connection: from={}, to={}, port={:?}",
            from_node, to_node, to_port
        ));

        let from_node_id = NodeId::from_string(from_node)
            .map_err(|e| JsValue::from_str(&format!("Invalid from_node UUID: {}", e)))?;
        let to_node_id = NodeId::from_string(to_node)
            .map_err(|e| JsValue::from_str(&format!("Invalid to_node UUID: {}", e)))?;

        for voice in &mut self.voices {
            voice
                .graph
                .remove_specific_connection(from_node_id, to_node_id, to_port);
        }

        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn connect_macro(
        &mut self,
        voice_index: usize,
        macro_index: usize,
        target_node: &str,
        target_port: PortId,
        amount: f32,
        modulation_type: Option<WasmModulationType>,
        modulation_transform: ModulationTransformation,
    ) -> Result<(), JsValue> {
        // log_console(&format!(
        //     "Connecting macro: voice={}, macro={}, node={}, port={:?}, amount={}, mod_type={:?}, mod_transform={:?}",
        //     voice_index, macro_index, target_node, target_port, amount, modulation_type, modulation_transform
        // ));

        let voice = self
            .voices
            .get_mut(voice_index)
            .ok_or_else(|| JsValue::from_str("Invalid voice index"))?;

        let target_node_id = NodeId::from_string(target_node)
            .map_err(|e| JsValue::from_str(&format!("Invalid target_node UUID: {}", e)))?;

        // Remove any existing route for this macro/target pairing before adding
        let _ = voice.remove_macro_route(macro_index, target_node_id, target_port);

        if amount <= 0.0 {
            // Treat non-positive amounts as removal only
            return Ok(());
        }

        voice
            .add_macro_modulation(
                macro_index,
                target_node_id,
                target_port,
                amount,
                modulation_type
                    .map(ModulationType::from)
                    .unwrap_or_default(),
                modulation_transform,
            )
            .map_err(|e| JsValue::from_str(&e))
    }

    fn build_nodes_from_canonical_voice(
        &mut self,
        voice_layout: &PatchVoiceLayout,
    ) -> Result<(), JsValue> {
        let mut sampler_cache: HashMap<NodeId, Rc<RefCell<SampleData>>> = HashMap::new();

        for_each_node_in_creation_order(voice_layout, |node_type, node| {
            let node_id = parse_node_id(&node.id).map_err(|e| JsValue::from_str(&e))?;
            self.instantiate_node(node_type, node_id, &mut sampler_cache)
        })
    }

    fn instantiate_node(
        &mut self,
        node_type: &str,
        node_id: NodeId,
        sampler_cache: &mut HashMap<NodeId, Rc<RefCell<SampleData>>>,
    ) -> Result<(), JsValue> {
        match node_type {
            "global_frequency" => {
                for voice in &mut self.voices {
                    voice.graph.add_node_with_id(
                        node_id,
                        Box::new(GlobalFrequencyNode::new(440.0, self.block_size)),
                    );
                    voice.graph.global_frequency_node = Some(node_id);
                }
            }
            "glide" => {
                for voice in &mut self.voices {
                    let mut glide = Glide::new(self.sample_rate, 0.0);
                    glide.set_active(false); // default bypass
                    voice.graph.add_node_with_id(node_id, Box::new(glide));
                    voice.graph.global_glide_node = Some(node_id);

                    if let Some(global_freq) = voice.graph.global_frequency_node {
                        voice.graph.add_connection(Connection {
                            from_node: global_freq,
                            from_port: PortId::GlobalFrequency,
                            to_node: node_id,
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
                            to_node: node_id,
                            to_port: PortId::CombinedGate,
                            amount: 1.0,
                            modulation_type: ModulationType::Additive,
                            modulation_transform: ModulationTransformation::None,
                        });
                    }
                }
            }
            "global_velocity" => {
                for voice in &mut self.voices {
                    voice.graph.add_node_with_id(
                        node_id,
                        Box::new(GlobalVelocityNode::new(1.0, self.block_size)),
                    );
                    voice.graph.global_velocity_node = Some(node_id);
                }
            }
            "gatemixer" => {
                for voice in &mut self.voices {
                    voice
                        .graph
                        .add_node_with_id(node_id, Box::new(GateMixer::new()));
                    voice.graph.global_gatemixer_node = Some(node_id);
                }
            }
            "oscillator" => {
                for voice in &mut self.voices {
                    voice.graph.add_node_with_id(
                        node_id,
                        Box::new(AnalogOscillator::new(
                            self.sample_rate,
                            Waveform::Sine,
                            self.wavetable_banks.clone(),
                        )),
                    );
                }
            }
            "wavetable_oscillator" => {
                for voice in &mut self.voices {
                    voice.graph.add_node_with_id(
                        node_id,
                        Box::new(WavetableOscillator::new(
                            self.sample_rate,
                            self.wavetable_synthbank.clone(),
                        )),
                    );
                }
            }
            "sampler" => {
                let data = sampler_cache.entry(node_id).or_insert_with(|| {
                    let mut sample = SampleData::new();
                    let defaults = Self::generate_default_sampler_data(self.sample_rate);
                    sample.load_from_wav(defaults, 1, self.sample_rate);
                    sample.root_note = 69.0;
                    Rc::new(RefCell::new(sample))
                });
                log_console(&format!(
                    "[instantiate_node] Creating sampler node with id {}",
                    node_id
                ));
                for voice in &mut self.voices {
                    let mut sampler = Sampler::new(self.sample_rate);
                    sampler.set_sample_data(data.clone());
                    voice.graph.add_node_with_id(node_id, Box::new(sampler));
                }
            }
            "envelope" => {
                for voice in &mut self.voices {
                    voice.graph.add_node_with_id(
                        node_id,
                        Box::new(Envelope::new(self.sample_rate, EnvelopeConfig::default())),
                    );
                }
            }
            "lfo" => {
                for voice in &mut self.voices {
                    voice
                        .graph
                        .add_node_with_id(node_id, Box::new(Lfo::new(self.sample_rate)));
                }
            }
            "filter" => {
                for voice in &mut self.voices {
                    voice.graph.add_node_with_id(
                        node_id,
                        Box::new(FilterCollection::new(self.sample_rate)),
                    );
                }
            }
            "mixer" => {
                for voice in &mut self.voices {
                    voice
                        .graph
                        .add_node_with_id(node_id, Box::new(Mixer::new()));
                    voice.set_output_node(node_id);
                }
            }
            "noise" => {
                for voice in &mut self.voices {
                    voice
                        .graph
                        .add_node_with_id(node_id, Box::new(NoiseGenerator::new(self.sample_rate)));
                }
            }
            "arpeggiator_generator" => {
                for voice in &mut self.voices {
                    let mut arp = ArpeggiatorGenerator::new();
                    arp.create_test_pattern(self.sample_rate, 0.225);
                    voice.graph.add_node_with_id(node_id, Box::new(arp));
                }
            }
            // Effect nodes exist in the effect stack.
            "chorus" | "delay" | "freeverb" | "convolver" | "limiter" | "compressor"
            | "saturation" | "bitcrusher" => {}
            other => log_console(&format!("Skipping unsupported node type {}", other)),
        }
        Ok(())
    }

    fn connect_from_canonical_voice(
        &mut self,
        voice_layout: &PatchVoiceLayout,
    ) -> Result<(), JsValue> {
        for connection in &voice_layout.connections {
            let to_port = port_id_from_u32(connection.target).map_err(|e| JsValue::from_str(&e))?;
            let base_modulation_type = modulation_type_from_i32(connection.modulation_type)
                .map_err(|e| JsValue::from_str(&e))?;
            let modulation_type = Some(WasmModulationType::from(base_modulation_type));
            let modulation_transform =
                modulation_transform_from_i32(connection.modulation_transform)
                    .map_err(|e| JsValue::from_str(&e))?;

            // Determine the appropriate output port on the source node.
            let from_port = self
                .voices
                .first()
                .and_then(|voice| {
                    NodeId::from_string(&connection.from_id)
                        .ok()
                        .and_then(|node_id| voice.graph.get_node(node_id))
                        .map(|node| {
                            let ports = node.get_ports();
                            ports
                                .iter()
                                .find(|(_, &is_output)| is_output)
                                .map(|(p, _)| *p)
                                .unwrap_or(PortId::AudioOutput0)
                        })
                })
                .unwrap_or(PortId::AudioOutput0);

            let mut from_id = connection.from_id.clone();
            let mut effective_from_port = from_port;

            if to_port == PortId::GlobalFrequency {
                if let Some(glide_node) =
                    self.voices.first().and_then(|v| v.graph.global_glide_node)
                {
                    from_id = glide_node.to_string();
                    effective_from_port = PortId::AudioOutput0;
                }
            }

            self.connect_nodes(
                &from_id,
                effective_from_port,
                &connection.to_id,
                to_port,
                connection.amount,
                modulation_type,
                modulation_transform,
            )?;
        }
        Ok(())
    }

    fn apply_patch_states(
        &mut self,
        patch: &PatchFile,
        canonical_voice: &PatchVoiceLayout,
    ) -> Result<(), JsValue> {
        for (id, state) in &patch.synth_state.oscillators {
            self.update_oscillator(id, state)?;
        }

        for (id, state) in &patch.synth_state.wavetable_oscillators {
            self.update_wavetable_oscillator(id, state)?;
        }

        for (id, config) in &patch.synth_state.envelopes {
            self.update_envelope(
                id,
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
            let params = WasmLfoUpdateParams::new(
                state.lfo_id.clone(),
                state.frequency,
                state.phase_offset,
                state.waveform,
                state.use_absolute,
                state.use_normalized,
                state.trigger_mode,
                state.gain,
                state.active,
                state.loop_mode,
                state.loop_start,
                state.loop_end,
            );
            self.update_lfos(params);
        }

        for filter in patch.synth_state.filters.values() {
            let filter_type =
                filter_type_from_i32(filter.filter_type).map_err(|e| JsValue::from_str(&e))?;
            self.update_filters(
                &filter.id,
                filter.cutoff,
                filter.resonance,
                filter.gain,
                filter.key_tracking,
                filter.comb_frequency,
                filter.comb_dampening,
                filter.oversampling,
                filter_type,
                filter.filter_slope,
            )?;
        }

        for sampler in patch.synth_state.samplers.values() {
            self.update_sampler(
                &sampler.id,
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

        for glide in patch.synth_state.glides.values() {
            self.update_glide(&glide.glide_id, glide.resolved_time(), glide.active)?;
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

        for compressor in patch.synth_state.compressors.values() {
            if let Ok(node_id) = compressor.id.parse::<usize>() {
                self.update_compressor(
                    node_id,
                    compressor.active,
                    compressor.threshold_db,
                    compressor.ratio,
                    compressor.attack_ms,
                    compressor.release_ms,
                    compressor.makeup_gain_db,
                    compressor.mix,
                );
            }
        }

        for saturation in patch.synth_state.saturations.values() {
            if let Ok(node_id) = saturation.id.parse::<usize>() {
                self.update_saturation(
                    node_id,
                    saturation.drive,
                    saturation.mix,
                    saturation.active,
                );
            }
        }

        for bitcrusher in patch.synth_state.bitcrushers.values() {
            if let Ok(node_id) = bitcrusher.id.parse::<usize>() {
                self.update_bitcrusher(
                    node_id,
                    bitcrusher.bits,
                    bitcrusher.downsample_factor,
                    bitcrusher.mix,
                    bitcrusher.active,
                );
            }
        }

        for convolver in patch.synth_state.convolvers.values() {
            if let Ok(node_id) = convolver.id.parse::<usize>() {
                self.update_convolver(node_id, convolver.wet_mix, convolver.active);
            }
        }

        if let Some(noise_state) = &patch.synth_state.noise {
            if let Some(noise_id) = find_node_id(canonical_voice, "noise") {
                let params = NoiseUpdateParams::new(
                    match noise_state.noise_type {
                        1 => WasmNoiseType::Pink,
                        2 => WasmNoiseType::Brownian,
                        _ => WasmNoiseType::White,
                    },
                    noise_state.cutoff,
                    noise_state.gain,
                    noise_state.is_enabled,
                );
                self.update_noise(&noise_id, &params)?;
            }
        }

        if let Some(velocity_state) = &patch.synth_state.velocity {
            if let Some(velocity_id) = find_node_id(canonical_voice, "global_velocity") {
                self.update_velocity(
                    &velocity_id,
                    velocity_state.sensitivity,
                    velocity_state.randomize,
                )?;
            }
        }

        Ok(())
    }

    fn import_audio_assets(&mut self, assets: &HashMap<String, AudioAsset>) -> Result<(), JsValue> {
        for asset in assets.values() {
            if let Some((asset_type, node_id)) = parse_audio_asset_id(&asset.id) {
                let data = BASE64_ENGINE
                    .decode(asset.base64_data.as_bytes())
                    .map_err(|e| {
                        JsValue::from_str(&format!("Failed to decode asset {}: {}", asset.id, e))
                    })?;

                match asset_type.as_str() {
                    "sample" => {
                        self.import_sample(&node_id, &data)?;
                    }
                    "impulse_response" => {
                        let effect_index = node_id.parse::<usize>().map_err(|e| {
                            JsValue::from_str(&format!(
                                "Invalid impulse response effect ID '{}': {}",
                                node_id, e
                            ))
                        })?;
                        self.import_wave_impulse(effect_index, &data)?;
                    }
                    "wavetable" => {
                        log_console("Wavetable asset import not yet implemented");
                    }
                    _ => {
                        log_console(&format!("Unknown asset type: {}", asset_type));
                    }
                }
            } else {
                log_console(&format!("Invalid asset ID format: {}", asset.id));
            }
        }
        Ok(())
    }

    #[cfg_attr(feature = "wasm", wasm_bindgen)]
    pub fn reset(&mut self) {
        // Clear all voices
        for voice in &mut self.voices {
            voice.clear();
        }
    }
}
