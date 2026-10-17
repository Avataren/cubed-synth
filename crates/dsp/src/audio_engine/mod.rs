mod patch;
mod patch_loader;
#[cfg(any(all(feature = "wasm", target_arch = "wasm32"), test))]
mod wav_decode;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub mod wasm;

#[cfg(any(feature = "native-host", test))]
pub mod native;

#[cfg(all(feature = "wasm", not(feature = "native-host"), target_arch = "wasm32"))]
pub use wasm::*;

#[cfg(feature = "native-host")]
pub use native::*;

#[cfg(all(
    feature = "wasm",
    not(feature = "native-host"),
    not(target_arch = "wasm32")
))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmNoiseType {
    White = 0,
    Pink = 1,
    Brownian = 2,
}

#[cfg(all(
    feature = "wasm",
    not(feature = "native-host"),
    not(target_arch = "wasm32")
))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmModulationType {
    VCA = 0,
    Bipolar = 1,
    Additive = 2,
}

// Re-export common types for both
pub use crate::graph::{ModulationTransformation, ModulationType};
pub use crate::traits::PortId;
