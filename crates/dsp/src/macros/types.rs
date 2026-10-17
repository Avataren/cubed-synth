use crate::{graph::ModulationTransformation, graph::ModulationType, NodeId, PortId};

#[derive(Debug, Clone)]
pub struct ModulationTarget {
    pub node_id: NodeId,
    pub port_id: PortId,
    pub amount: f32,
    pub modulation_type: ModulationType,
    pub modulation_transform: ModulationTransformation,
}

#[derive(Debug)]
pub struct ModulationMacro {
    value_buffer_idx: usize,
    targets: Vec<ModulationTarget>,
}

impl ModulationMacro {
    pub fn new(buffer_idx: usize) -> Self {
        Self {
            value_buffer_idx: buffer_idx,
            targets: Vec::with_capacity(4), // Reasonable default capacity
        }
    }

    pub fn add_target(&mut self, target: ModulationTarget) {
        self.targets.push(target);
    }

    pub fn remove_target(&mut self, node_id: NodeId, port_id: PortId) -> bool {
        let before = self.targets.len();
        self.targets
            .retain(|t| !(t.node_id == node_id && t.port_id == port_id));
        before != self.targets.len()
    }

    pub fn clear_targets(&mut self) {
        self.targets.clear();
    }

    pub fn get_targets(&self) -> &[ModulationTarget] {
        &self.targets
    }

    pub fn get_value_buffer_idx(&self) -> usize {
        self.value_buffer_idx
    }
}
