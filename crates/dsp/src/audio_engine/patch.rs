#![cfg_attr(
    not(any(feature = "native-host", all(feature = "wasm", target_arch = "wasm32"))),
    allow(dead_code)
)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::nodes::{
    AnalogOscillatorStateUpdate, EnvelopeConfig, FilterSlope, WavetableOscillatorStateUpdate,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct PatchFile {
    pub metadata: PatchMetadata,
    #[serde(rename = "synthState")]
    pub synth_state: SynthState,
    #[serde(rename = "audioAssets", default)]
    pub audio_assets: HashMap<String, AudioAsset>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PatchMetadata {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SynthState {
    pub layout: Layout,
    #[serde(default)]
    pub oscillators: HashMap<String, AnalogOscillatorStateUpdate>,
    #[serde(default, rename = "wavetableOscillators")]
    pub wavetable_oscillators: HashMap<String, WavetableOscillatorStateUpdate>,
    #[serde(default)]
    pub envelopes: HashMap<String, EnvelopeConfig>,
    #[serde(default)]
    pub lfos: HashMap<String, LfoState>,
    #[serde(default)]
    pub filters: HashMap<String, FilterState>,
    #[serde(default)]
    pub samplers: HashMap<String, SamplerState>,
    #[serde(default)]
    pub glides: HashMap<String, GlideState>,
    #[serde(default)]
    pub convolvers: HashMap<String, ConvolverState>,
    #[serde(default)]
    pub delays: HashMap<String, DelayState>,
    #[serde(default)]
    pub choruses: HashMap<String, ChorusState>,
    #[serde(default)]
    pub reverbs: HashMap<String, ReverbState>,
    #[serde(default)]
    pub compressors: HashMap<String, CompressorState>,
    #[serde(default)]
    pub saturations: HashMap<String, SaturationState>,
    #[serde(default)]
    pub bitcrushers: HashMap<String, BitcrusherState>,
    #[serde(default)]
    pub noise: Option<NoiseState>,
    #[serde(default)]
    pub velocity: Option<VelocityState>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Layout {
    #[serde(default, rename = "voiceCount")]
    pub voice_count: Option<usize>,
    #[serde(default, rename = "canonicalVoice")]
    pub canonical_voice: Option<VoiceLayout>,
    #[serde(default)]
    pub voices: Vec<VoiceLayout>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VoiceLayout {
    pub id: usize,
    #[serde(default)]
    pub nodes: HashMap<String, Vec<PatchNode>>,
    #[serde(default)]
    pub connections: Vec<PatchConnection>,
}

impl Layout {
    /// Returns the declared voice count, falling back to legacy serialized voices
    /// or to 1 if we have a canonical voice but no explicit count.
    pub fn resolved_voice_count(&self) -> usize {
        if let Some(count) = self.voice_count {
            if count > 0 {
                return count;
            }
        }

        if !self.voices.is_empty() {
            return self.voices.len();
        }

        if self.canonical_voice.is_some() {
            return 1;
        }

        0
    }

    /// Returns the canonical voice layout, falling back to the first legacy voice.
    pub fn canonical_voice(&self) -> Option<&VoiceLayout> {
        if let Some(ref canonical) = self.canonical_voice {
            return Some(canonical);
        }
        self.voices.first()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PatchNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PatchConnection {
    #[serde(rename = "fromId")]
    pub from_id: String,
    #[serde(rename = "toId")]
    pub to_id: String,
    pub target: u32,
    pub amount: f32,
    #[serde(rename = "modulationType")]
    pub modulation_type: i32,
    #[serde(rename = "modulationTransformation")]
    pub modulation_transform: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfoState {
    #[serde(rename = "id")]
    pub lfo_id: String,
    pub frequency: f32,
    #[serde(rename = "phaseOffset")]
    pub phase_offset: f32,
    pub waveform: u8,
    #[serde(rename = "useAbsolute")]
    pub use_absolute: bool,
    #[serde(rename = "useNormalized")]
    pub use_normalized: bool,
    #[serde(rename = "triggerMode")]
    pub trigger_mode: u8,
    pub gain: f32,
    pub active: bool,
    #[serde(rename = "loopMode")]
    pub loop_mode: usize,
    #[serde(rename = "loopStart")]
    pub loop_start: f32,
    #[serde(rename = "loopEnd")]
    pub loop_end: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FilterState {
    pub id: String,
    pub cutoff: f32,
    pub resonance: f32,
    #[serde(rename = "keytracking")]
    pub key_tracking: f32,
    #[serde(rename = "comb_frequency")]
    pub comb_frequency: f32,
    #[serde(rename = "comb_dampening")]
    pub comb_dampening: f32,
    pub oversampling: u32,
    pub gain: f32,
    #[serde(rename = "filter_type")]
    pub filter_type: i32,
    #[serde(rename = "filter_slope")]
    pub filter_slope: FilterSlope,
    #[serde(default)]
    pub active: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SamplerState {
    pub id: String,
    pub frequency: f32,
    pub gain: f32,
    #[serde(rename = "loopMode")]
    pub loop_mode: u8,
    #[serde(rename = "loopStart")]
    pub loop_start: f32,
    #[serde(rename = "loopEnd")]
    pub loop_end: f32,
    #[serde(rename = "sampleLength", default)]
    pub sample_length: f32,
    #[serde(rename = "rootNote")]
    pub root_note: f32,
    #[serde(rename = "triggerMode")]
    pub trigger_mode: u8,
    #[serde(default)]
    pub active: bool,
    #[serde(rename = "sampleRate", default)]
    pub sample_rate: f32,
    #[serde(default)]
    pub channels: u32,
    #[serde(rename = "fileName", default)]
    pub file_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConvolverState {
    pub id: String,
    #[serde(rename = "wetMix")]
    pub wet_mix: f32,
    pub active: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DelayState {
    pub id: String,
    #[serde(rename = "delayMs")]
    pub delay_ms: f32,
    pub feedback: f32,
    #[serde(rename = "wetMix")]
    pub wet_mix: f32,
    pub active: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChorusState {
    pub id: String,
    pub active: bool,
    #[serde(rename = "baseDelayMs")]
    pub base_delay_ms: f32,
    #[serde(rename = "depthMs")]
    pub depth_ms: f32,
    #[serde(rename = "lfoRateHz")]
    pub lfo_rate_hz: f32,
    pub feedback: f32,
    #[serde(rename = "feedback_filter")]
    pub feedback_filter: f32,
    pub mix: f32,
    #[serde(rename = "stereoPhaseOffsetDeg")]
    pub stereo_phase_offset_deg: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompressorState {
    pub id: String,
    pub active: bool,
    #[serde(rename = "thresholdDb")]
    pub threshold_db: f32,
    pub ratio: f32,
    #[serde(rename = "attackMs")]
    pub attack_ms: f32,
    #[serde(rename = "releaseMs")]
    pub release_ms: f32,
    #[serde(rename = "makeupGainDb")]
    pub makeup_gain_db: f32,
    pub mix: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SaturationState {
    pub id: String,
    pub active: bool,
    pub drive: f32,
    pub mix: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BitcrusherState {
    pub id: String,
    pub active: bool,
    pub bits: u8,
    #[serde(rename = "downsampleFactor")]
    pub downsample_factor: usize,
    pub mix: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReverbState {
    pub id: String,
    pub active: bool,
    #[serde(rename = "room_size")]
    pub room_size: f32,
    pub damp: f32,
    pub wet: f32,
    pub dry: f32,
    pub width: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NoiseState {
    #[serde(rename = "noiseType")]
    pub noise_type: u8,
    pub cutoff: f32,
    pub gain: f32,
    #[serde(rename = "is_enabled")]
    pub is_enabled: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VelocityState {
    pub sensitivity: f32,
    pub randomize: f32,
    pub active: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GlideState {
    #[serde(rename = "id")]
    pub glide_id: String,
    #[serde(rename = "time", default)]
    pub time: f32,
    #[serde(rename = "riseTime", default, skip_serializing_if = "Option::is_none")]
    pub rise_time: Option<f32>,
    #[serde(rename = "fallTime", default, skip_serializing_if = "Option::is_none")]
    pub fall_time: Option<f32>,
    #[serde(default)]
    pub active: bool,
}

impl GlideState {
    /// Returns the single glide time, falling back to legacy rise/fall fields if necessary.
    pub fn resolved_time(&self) -> f32 {
        // If a modern time value is present (including 0.0), prefer it.
        if self.rise_time.is_none() && self.fall_time.is_none() {
            return self.time;
        }

        // Legacy patches may contain rise/fall without the unified time field.
        if self.time == 0.0 {
            return self
                .rise_time
                .unwrap_or(0.0)
                .max(self.fall_time.unwrap_or(0.0));
        }

        self.time
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AudioAsset {
    pub id: String,
    #[serde(rename = "type")]
    pub asset_type: AudioAssetType,
    #[serde(rename = "base64Data")]
    pub base64_data: String,
    #[serde(rename = "sampleRate")]
    pub sample_rate: f32,
    pub channels: u32,
    #[serde(rename = "rootNote", default)]
    pub root_note: Option<f32>,
    #[serde(rename = "fileName", default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub duration: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioAssetType {
    Sample,
    ImpulseResponse,
    Wavetable,
}
