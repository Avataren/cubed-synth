use rustc_hash::FxHashMap;
use std::any::Any;
// src/traits/mod.rs

#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

use crate::graph::ModulationSource;

#[cfg_attr(feature = "wasm", wasm_bindgen)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PortId {
    #[default]
    AudioInput0,
    AudioInput1,
    AudioInput2,
    AudioInput3,
    AudioOutput0,
    AudioOutput1,
    AudioOutput2,
    AudioOutput3,
    GlobalGate,
    GlobalFrequency,
    GlobalVelocity,
    Frequency,
    FrequencyMod,
    PhaseMod,
    ModIndex,
    CutoffMod,
    ResonanceMod,
    GainMod,
    EnvelopeMod,
    StereoPan,
    FeedbackMod,
    DetuneMod,
    WavetableIndex,
    WetDryMix,
    AttackMod,
    ArpGate,
    CombinedGate,
    SampleOffset,
}

impl PortId {
    pub fn is_audio_input(&self) -> bool {
        matches!(
            self,
            PortId::AudioInput0 | PortId::AudioInput1 | PortId::AudioInput2 | PortId::AudioInput3
        )
    }

    pub fn is_audio_output(&self) -> bool {
        matches!(
            self,
            PortId::AudioOutput0
                | PortId::AudioOutput1
                | PortId::AudioOutput2
                | PortId::AudioOutput3
        )
    }

    pub fn is_modulation_input(&self) -> bool {
        matches!(
            self,
            PortId::FrequencyMod
                | PortId::PhaseMod
                | PortId::GainMod
                | PortId::ModIndex
                | PortId::StereoPan
                | PortId::CutoffMod
                | PortId::ResonanceMod
                | PortId::EnvelopeMod
        )
    }

    pub fn to_input_index(&self) -> Option<usize> {
        match self {
            PortId::AudioInput0 => Some(0),
            PortId::AudioInput1 => Some(1),
            PortId::AudioInput2 => Some(2),
            PortId::AudioInput3 => Some(3),
            _ => None,
        }
    }

    pub fn to_output_index(&self) -> Option<usize> {
        match self {
            PortId::AudioOutput0 => Some(0),
            PortId::AudioOutput1 => Some(1),
            PortId::AudioOutput2 => Some(2),
            PortId::AudioOutput3 => Some(3),
            _ => None,
        }
    }

    pub fn from_input_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(PortId::AudioInput0),
            1 => Some(PortId::AudioInput1),
            2 => Some(PortId::AudioInput2),
            3 => Some(PortId::AudioInput3),
            _ => None,
        }
    }

    pub fn from_output_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(PortId::AudioOutput0),
            1 => Some(PortId::AudioOutput1),
            2 => Some(PortId::AudioOutput2),
            3 => Some(PortId::AudioOutput3),
            _ => None,
        }
    }

    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => PortId::AudioInput0,
            1 => PortId::AudioInput1,
            2 => PortId::AudioInput2,
            3 => PortId::AudioInput3,
            4 => PortId::AudioOutput0,
            5 => PortId::AudioOutput1,
            6 => PortId::AudioOutput2,
            7 => PortId::AudioOutput3,
            8 => PortId::GlobalGate,
            9 => PortId::GlobalFrequency,
            10 => PortId::GlobalVelocity,
            11 => PortId::Frequency,
            12 => PortId::FrequencyMod,
            13 => PortId::PhaseMod,
            14 => PortId::ModIndex,
            15 => PortId::CutoffMod,
            16 => PortId::ResonanceMod,
            17 => PortId::GainMod,
            18 => PortId::EnvelopeMod,
            19 => PortId::StereoPan,
            20 => PortId::FeedbackMod,
            21 => PortId::DetuneMod,
            22 => PortId::WavetableIndex,
            23 => PortId::WetDryMix,
            24 => PortId::AttackMod,
            25 => PortId::ArpGate,
            26 => PortId::CombinedGate,
            27 => PortId::SampleOffset,
            _ => PortId::AudioInput0, // Default or error case
        }
    }
}

pub trait AudioNode: Any {
    fn get_ports(&self) -> FxHashMap<PortId, bool>;

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>, // Zero-copy: borrowed slices
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    );

    fn reset(&mut self);

    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn as_any(&self) -> &dyn Any;

    // Active state management
    fn is_active(&self) -> bool;
    fn set_active(&mut self, active: bool);

    // Optional method to handle state changes
    fn on_active_changed(&mut self) {}

    // Helper to determine if node should be processed
    fn should_process(&self) -> bool {
        self.is_active()
    }

    fn name(&self) -> &'static str;

    fn node_type(&self) -> &str {
        "unknown" // Default implementation returns "unknown"
    }
}
