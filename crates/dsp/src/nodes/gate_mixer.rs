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

    fn process_modulations(
        buffer_size: usize,
        maybe_sources: Option<&Vec<ModulationSource>>,
        default: f32,
    ) -> Vec<f32> {
        if let Some(sources) = maybe_sources {
            let buf = &sources[0].buffer;
            if buf.len() >= buffer_size {
                buf[..buffer_size].to_vec()
            } else {
                vec![default; buffer_size]
            }
        } else {
            vec![default; buffer_size]
        }
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

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        // If an input is missing, default to 1.0 (neutral for multiplication).
        let global_gate =
            Self::process_modulations(buffer_size, inputs.get(&PortId::GlobalGate), 1.0);
        let arp_gate = Self::process_modulations(buffer_size, inputs.get(&PortId::ArpGate), 1.0);

        if let Some(out_buffer) = outputs.get_mut(&PortId::CombinedGate) {
            for i in 0..buffer_size {
                out_buffer[i] = global_gate[i] * arp_gate[i];
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
