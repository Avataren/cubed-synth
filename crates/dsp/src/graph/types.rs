#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use crate::impulse_generator::fill_seed;
use crate::PortId;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use rand::rngs::StdRng;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

// Zero-copy buffer passing: uses borrowed slices instead of owned Vec
// This eliminates expensive buffer copies in the audio processing hot path
pub struct ModulationSource<'a> {
    pub buffer: &'a [f32], // Borrowed slice (zero-copy)
    pub amount: f32,
    pub mod_type: ModulationType,
    pub transformation: ModulationTransformation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub Uuid);

impl Default for NodeId {
    fn default() -> Self {
        NodeId(Uuid::nil())
    }
}

impl NodeId {
    pub fn new() -> Self {
        #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
        {
            // In WASM we avoid relying directly on `Uuid::new_v4()` because it
            // uses `getrandom` internally, which can fail in environments like
            // AudioWorklets where `crypto.getRandomValues` is not available.
            //
            // Instead we reuse the crate's own seeding helper that already
            // knows how to fall back to JS and Math.random() as needed.
            let mut seed = [0u8; 32];
            // Errors are already logged and a JS fallback is used, so ignore
            // the result here – we always end up with some seed bytes.
            let _ = fill_seed(&mut seed);
            let mut rng = StdRng::from_seed(seed);
            let random_u128: u128 = rng.random();
            NodeId(Uuid::from_u128(random_u128))
        }

        #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
        {
            NodeId(Uuid::new_v4())
        }
    }

    pub fn from_string(s: &str) -> Result<Self, uuid::Error> {
        Ok(NodeId(Uuid::parse_str(s)?))
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
pub struct WasmNodeId {
    inner: NodeId,
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
impl WasmNodeId {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        WasmNodeId {
            inner: NodeId::new(),
        }
    }

    #[wasm_bindgen(js_name = fromString)]
    pub fn from_string(s: &str) -> Result<WasmNodeId, JsValue> {
        NodeId::from_string(s)
            .map(|inner| WasmNodeId { inner })
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = toString)]
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        self.inner.to_string()
    }
}

#[cfg(feature = "wasm")]
impl From<NodeId> for WasmNodeId {
    fn from(inner: NodeId) -> Self {
        WasmNodeId { inner }
    }
}

#[cfg(feature = "wasm")]
impl From<WasmNodeId> for NodeId {
    fn from(wasm: WasmNodeId) -> Self {
        wasm.inner
    }
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
#[derive(Default)]
pub enum ModulationType {
    VCA = 0,
    Bipolar = 1,
    #[default]
    Additive = 2,
}

impl ModulationType {
    pub fn from_i32(value: i32) -> Self {
        match value {
            0 => ModulationType::VCA,
            1 => ModulationType::Bipolar,
            2 => ModulationType::Additive,
            _ => ModulationType::Additive,
        }
    }
}

#[derive(Copy, Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "wasm", wasm_bindgen)]
#[repr(u32)]
pub enum ModulationTransformation {
    None,
    Invert,
    Square,
    Cube,
}

impl ModulationTransformation {
    #[inline]
    pub fn apply(&self, x: f32) -> f32 {
        match self {
            ModulationTransformation::None => x,
            ModulationTransformation::Invert => 1.0 - x,
            ModulationTransformation::Square => x * x,
            ModulationTransformation::Cube => x * x * x,
        }
    }

    pub fn from_i32(value: i32) -> Self {
        match value {
            0 => ModulationTransformation::None,
            1 => ModulationTransformation::Invert,
            2 => ModulationTransformation::Square,
            3 => ModulationTransformation::Cube,
            _ => ModulationTransformation::None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Connection {
    pub from_node: NodeId,
    pub from_port: PortId,
    pub to_node: NodeId,
    pub to_port: PortId,
    pub amount: f32,
    pub modulation_type: ModulationType,
    pub modulation_transform: ModulationTransformation,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ConnectionKey {
    pub from_node: NodeId,
    pub from_port: PortId,
    pub to_node: NodeId,
    pub to_port: PortId,
}

impl ConnectionKey {
    pub fn new(from_node: NodeId, from_port: PortId, to_node: NodeId, to_port: PortId) -> Self {
        Self {
            from_node,
            from_port,
            to_node,
            to_port,
        }
    }
}
