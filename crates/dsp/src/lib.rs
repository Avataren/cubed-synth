//! The synth engine of another_synth: the audio graph, its nodes (oscillators,
//! envelopes, LFOs, filters, effects), voices, automation and patch loading.

#![cfg_attr(feature = "nightly-simd", feature(portable_simd))]

pub mod audio;
pub mod audio_engine;
pub mod automation;
pub mod biquad;
pub mod effect_stack;
pub mod graph;
pub mod impulse_generator;
pub mod macros;
pub mod nodes;
pub mod processing;
pub mod seed;
pub mod traits;
pub mod utils;
pub mod voice;

pub use automation::{AutomationFrame, ConnectionUpdate};
pub use graph::AudioGraph;
pub use graph::{Connection, ConnectionId, NodeId};
pub use macros::{MacroManager, ModulationTarget};
pub use nodes::{Envelope, EnvelopeConfig};
pub use traits::{AudioNode, PortId, StereoEffect};
pub use utils::*;
pub use voice::Voice;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub use audio_engine::{AudioEngine, NoiseUpdateParams, WasmLfoUpdateParams};

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub use automation::{apply_connection_update as apply_modulation_update, AutomationAdapter};

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub use audio_engine::{WasmModulationType, WasmNoiseType};
