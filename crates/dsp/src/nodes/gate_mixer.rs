use crate::traits::{Inputs, Outputs};
use rustc_hash::FxHashMap;

use crate::graph::ModulationSource;
use crate::{AudioNode, PortId};
use std::any::Any;

pub struct GateMixer {}

impl Default for GateMixer {
    fn default() -> Self {
        Self::new()
    }
}

impl GateMixer {
    pub fn new() -> Self {
        Self {}
    }

    /// The first source's buffer, when it covers the block.
    fn gate<'a>(
        maybe_sources: Option<&'a [ModulationSource]>,
        buffer_size: usize,
    ) -> Option<&'a [f32]> {
        let buffer = maybe_sources?.first()?.buffer;
        (buffer.len() >= buffer_size).then(|| &buffer[..buffer_size])
    }
}

impl AudioNode for GateMixer {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        let mut ports = FxHashMap::default();
        // Define two inputs and one output.
        ports.insert(PortId::GlobalGate, false);
        ports.insert(PortId::ArpGate, false);
        ports.insert(PortId::CombinedGate, true);
        ports
    }

    fn process<'a>(&mut self, inputs: &Inputs<'a>, outputs: &mut Outputs<'_>, buffer_size: usize) {
        // If an input is missing, default to 1.0 (neutral for multiplication).
        // A missing gate counts as open (1.0, neutral for the product).
        let global_gate = Self::gate(inputs.get(&PortId::GlobalGate), buffer_size);
        let arp_gate = Self::gate(inputs.get(&PortId::ArpGate), buffer_size);

        if let Some(out_buffer) = outputs.get_mut(&PortId::CombinedGate) {
            for i in 0..buffer_size {
                let g = global_gate.map_or(1.0, |b| b[i]);
                let a = arp_gate.map_or(1.0, |b| b[i]);
                out_buffer[i] = g * a;
            }
        }
    }

    fn reset(&mut self) {}
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn is_active(&self) -> bool {
        true
    }
    fn set_active(&mut self, _active: bool) {}
    fn name(&self) -> &'static str {
        "Gate Mixer"
    }
    fn node_type(&self) -> &str {
        "gatemixer"
    }
}
