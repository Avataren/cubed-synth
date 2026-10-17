// Several helpers are only called from the wasm bindings (`wasm.rs`).
#![cfg_attr(not(all(feature = "wasm", target_arch = "wasm32")), allow(dead_code))]

// Shared patch loading logic for both native and wasm builds

use crate::audio_engine::patch::{PatchNode, VoiceLayout as PatchVoiceLayout};
use crate::biquad::FilterType;
use crate::graph::{ModulationTransformation, ModulationType, NodeId};
use crate::traits::PortId;
use uuid::Uuid;

/// Result type that can be adapted for different error types
pub type PatchLoaderResult<T> = Result<T, String>;

/// Parse a node ID string as a UUID
pub fn parse_node_id(id_str: &str) -> PatchLoaderResult<NodeId> {
    Uuid::parse_str(id_str)
        .map(NodeId)
        .map_err(|e| format!("Invalid node UUID '{}': {}", id_str, e))
}

/// Parse a port ID from u32
pub fn port_id_from_u32(value: u32) -> PatchLoaderResult<PortId> {
    match value {
        0 => Ok(PortId::AudioInput0),
        1 => Ok(PortId::AudioInput1),
        2 => Ok(PortId::AudioInput2),
        3 => Ok(PortId::AudioInput3),
        4 => Ok(PortId::AudioOutput0),
        5 => Ok(PortId::AudioOutput1),
        6 => Ok(PortId::AudioOutput2),
        7 => Ok(PortId::AudioOutput3),
        8 => Ok(PortId::GlobalGate),
        9 => Ok(PortId::GlobalFrequency),
        10 => Ok(PortId::GlobalVelocity),
        11 => Ok(PortId::Frequency),
        12 => Ok(PortId::FrequencyMod),
        13 => Ok(PortId::PhaseMod),
        14 => Ok(PortId::ModIndex),
        15 => Ok(PortId::CutoffMod),
        16 => Ok(PortId::ResonanceMod),
        17 => Ok(PortId::GainMod),
        18 => Ok(PortId::EnvelopeMod),
        19 => Ok(PortId::StereoPan),
        20 => Ok(PortId::FeedbackMod),
        21 => Ok(PortId::DetuneMod),
        22 => Ok(PortId::WavetableIndex),
        23 => Ok(PortId::WetDryMix),
        24 => Ok(PortId::AttackMod),
        25 => Ok(PortId::ArpGate),
        26 => Ok(PortId::CombinedGate),
        _ => Err(format!("Unknown port id value {}", value)),
    }
}

/// Parse modulation type from i32
pub fn modulation_type_from_i32(value: i32) -> PatchLoaderResult<ModulationType> {
    match value {
        0 => Ok(ModulationType::VCA),
        1 => Ok(ModulationType::Bipolar),
        2 => Ok(ModulationType::Additive),
        _ => Err(format!("Unknown modulation type {}", value)),
    }
}

/// Parse modulation transformation from i32
pub fn modulation_transform_from_i32(value: i32) -> PatchLoaderResult<ModulationTransformation> {
    match value {
        0 => Ok(ModulationTransformation::None),
        1 => Ok(ModulationTransformation::Invert),
        2 => Ok(ModulationTransformation::Square),
        3 => Ok(ModulationTransformation::Cube),
        _ => Err(format!("Unknown modulation transform {}", value)),
    }
}

/// Parse filter type from i32
pub fn filter_type_from_i32(value: i32) -> PatchLoaderResult<FilterType> {
    match value {
        0 => Ok(FilterType::LowPass),
        1 => Ok(FilterType::LowShelf),
        2 => Ok(FilterType::Peaking),
        3 => Ok(FilterType::HighShelf),
        4 => Ok(FilterType::Notch),
        5 => Ok(FilterType::HighPass),
        6 => Ok(FilterType::Ladder),
        7 => Ok(FilterType::Comb),
        8 => Ok(FilterType::BandPass),
        _ => Err(format!("Unknown filter type {}", value)),
    }
}

/// Find the first node ID of a specific type in the voice layout
pub fn find_node_id(voice_layout: &PatchVoiceLayout, node_type: &str) -> Option<String> {
    voice_layout
        .nodes
        .get(node_type)
        .and_then(|nodes| nodes.first())
        .map(|node| node.id.clone())
}

/// Parse audio asset ID to extract node type and node ID
pub fn parse_audio_asset_id(asset_id: &str) -> Option<(String, String)> {
    // Expected formats:
    // - "sample_<node_id>" for sampler samples
    // - "impulse_response_<effect_id>" for convolver impulse responses
    // - "wavetable_<node_id>" for wavetable data

    if let Some(node_id) = asset_id.strip_prefix("sample_") {
        return Some(("sample".to_string(), node_id.to_string()));
    }

    if let Some(effect_id) = asset_id.strip_prefix("impulse_response_") {
        return Some(("impulse_response".to_string(), effect_id.to_string()));
    }

    if let Some(node_id) = asset_id.strip_prefix("wavetable_") {
        return Some(("wavetable".to_string(), node_id.to_string()));
    }

    None
}

/// Node creation order - ensures dependencies are created first
pub const NODE_CREATION_ORDER: [&str; 13] = [
    "global_frequency",
    "glide",
    "global_velocity",
    "gatemixer",
    "mixer",
    "filter",
    "oscillator",
    "wavetable_oscillator",
    "sampler",
    "envelope",
    "lfo",
    "noise",
    "arpeggiator_generator",
];

/// Iterate nodes in creation order, invoking the callback for each node.
/// Shared by native and wasm loaders to keep ordering in sync.
pub fn for_each_node_in_creation_order<F, E>(
    voice_layout: &PatchVoiceLayout,
    mut callback: F,
) -> Result<(), E>
where
    F: FnMut(&str, &PatchNode) -> Result<(), E>,
{
    for node_type in NODE_CREATION_ORDER {
        if let Some(nodes) = voice_layout.nodes.get(node_type) {
            for patch_node in nodes {
                callback(node_type, patch_node)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_audio_asset_id() {
        assert_eq!(
            parse_audio_asset_id("sample_abc123"),
            Some(("sample".to_string(), "abc123".to_string()))
        );

        assert_eq!(
            parse_audio_asset_id("impulse_response_10003"),
            Some(("impulse_response".to_string(), "10003".to_string()))
        );

        assert_eq!(
            parse_audio_asset_id("wavetable_xyz789"),
            Some(("wavetable".to_string(), "xyz789".to_string()))
        );

        assert_eq!(parse_audio_asset_id("invalid"), None);
    }

    #[test]
    fn test_parse_node_id() {
        let valid_uuid = "e3957b9b-9e2d-5999-bc84-93c4cd9d31b5";
        assert!(parse_node_id(valid_uuid).is_ok());

        let invalid_uuid = "not-a-uuid";
        assert!(parse_node_id(invalid_uuid).is_err());
    }
}
