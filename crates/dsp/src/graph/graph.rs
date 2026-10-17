use rustc_hash::FxHashMap;

/// AudioGraph is a flexible audio processing system that manages interconnected audio nodes and their buffer routing.
///
/// Core concepts:
/// - Nodes: Audio processing units (oscillators, envelopes, etc.) that can have multiple inputs and outputs
/// - Connections: Routes that carry audio/control signals between nodes' ports
/// - Buffer Pool: A memory manager that pre-allocates and reuses audio buffers
/// - Processing Order: A topologically sorted sequence of nodes ensuring correct signal flow
///
/// The system works by:
/// 1. Construction:
///    - Nodes are added to the graph and assigned unique IDs
///    - Each node's ports get dedicated buffers from the buffer pool
///    - Connections are established between nodes, recording source/destination ports
///    - Processing order is computed to handle dependencies correctly
///
/// 2. Runtime Processing:
///    - Global inputs (gate, frequency) are written to their dedicated buffers
///    - Nodes are processed in topological order
///    - For each node:
///      - Input buffers are prepared (either from source nodes or global inputs)
///      - Node processes its inputs and writes to its output buffers
///      - Output buffers are available for downstream nodes
///    - Final node's output is copied to the main output buffers
///
/// Key optimizations:
/// - Buffer pool prevents audio buffer allocations during processing
/// - Pre-computed processing order eliminates runtime dependency checks
/// - Vec-based storage enables efficient node lookup
/// - Connection information is cached to minimize lookup overhead
/// - Temporary buffers are reused across processing cycles
///
/// The system maintains thread safety through Rust's ownership system and provides
/// real-time safety by avoiding allocations in the processing path. It supports
/// arbitrary node graphs as long as they don't contain feedback loops.
///
use super::{
    buffer_pool::AudioBufferPool,
    types::{Connection, ConnectionKey, ModulationTransformation, NodeId},
    ModulationSource,
};
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use web_sys::console;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
fn log_console(message: &str) {
    console::log_1(&message.into());
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
fn log_console(_message: &str) {}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
fn log_error(message: &str) {
    console::error_1(&message.into());
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
fn log_error(message: &str) {
    eprintln!("{message}");
}
use crate::{
    graph::ModulationType,
    nodes::{GateMixer, GlobalFrequencyNode, GlobalVelocityNode},
};
use crate::{AudioNode, Inputs, MacroManager, Outputs, PortId};

pub struct AudioGraph {
    pub(crate) nodes: FxHashMap<NodeId, Box<dyn AudioNode>>,
    pub(crate) connections: FxHashMap<ConnectionKey, Connection>,
    pub(crate) processing_order: Vec<NodeId>,
    pub(crate) buffer_size: usize,
    pub(crate) buffer_pool: AudioBufferPool,
    pub(crate) node_buffers: FxHashMap<(NodeId, PortId), usize>,
    pub(crate) gate_buffer_idx: usize,
    // We now include the source node in the tuple so we can identify which connection to remove.
    pub(crate) input_connections: FxHashMap<
        NodeId,
        Vec<(
            PortId,
            usize,
            f32,
            NodeId,
            ModulationType,
            ModulationTransformation,
        )>,
    >,
    pub(crate) temp_buffer_indices: Vec<usize>,
    /// What each node in processing order reads and writes, worked out when
    /// the graph changes so a block only follows it.
    plan: Vec<NodePlan>,
    /// The graph changed since `plan` was made.
    plan_dirty: bool,
    /// The sources handed to nodes, kept between blocks (empty) so building
    /// them never allocates.
    source_scratch: Vec<ModulationSource<'static>>,
    /// This block's macro values and each macro's span in them.
    macro_values: Vec<f32>,
    macro_spans: Vec<std::ops::Range<usize>>,
    pub(crate) global_frequency_node: Option<NodeId>,
    pub(crate) global_glide_node: Option<NodeId>,
    pub(crate) global_velocity_node: Option<NodeId>,
    pub(crate) global_gatemixer_node: Option<NodeId>,
    pub(crate) output_node: Option<NodeId>,
}

impl AudioGraph {
    pub fn new(buffer_size: usize) -> Self {
        let mut buffer_pool = AudioBufferPool::new(buffer_size, 32);
        let gate_buffer_idx = buffer_pool.acquire(buffer_size);

        let mut graph = Self {
            nodes: FxHashMap::default(),
            connections: FxHashMap::default(),
            processing_order: Vec::new(),
            buffer_size,
            buffer_pool,
            node_buffers: FxHashMap::default(),
            gate_buffer_idx,
            input_connections: FxHashMap::default(),
            temp_buffer_indices: Vec::new(),
            plan: Vec::new(),
            plan_dirty: true,
            source_scratch: Vec::new(),
            macro_values: Vec::new(),
            macro_spans: Vec::new(),
            global_frequency_node: None,
            global_glide_node: None,
            global_velocity_node: None,
            global_gatemixer_node: None,
            output_node: None,
        };

        // Create and add the GlobalVelocityNode:
        let global_velocity_node = Box::new(GlobalVelocityNode::new(1.0, buffer_size));
        let global_velocity_node_id = graph.add_node(global_velocity_node);
        graph.global_velocity_node = Some(global_velocity_node_id);
        // Create and add the GlobalFrequencyNode:
        let global_node = Box::new(GlobalFrequencyNode::new(440.0, buffer_size));
        let global_node_id = graph.add_node(global_node);
        graph.global_frequency_node = Some(global_node_id);

        let gate_mixer = Box::new(GateMixer::new());
        let gate_mixer_id = graph.add_node(gate_mixer);
        graph.global_gatemixer_node = Some(gate_mixer_id);
        graph
    }

    pub fn clear(&mut self) {
        // Release all buffers owned by nodes and temporary processing.
        for &buffer_idx in self.node_buffers.values() {
            self.buffer_pool.release(buffer_idx);
        }
        for &buffer_idx in &self.temp_buffer_indices {
            self.buffer_pool.release(buffer_idx);
        }

        // Clear graph structures.
        self.connections.clear();
        self.input_connections.clear();
        self.nodes.clear();
        self.processing_order.clear();
        self.node_buffers.clear();
        self.temp_buffer_indices.clear();
        self.plan.clear();
        self.plan_dirty = true;

        // Reset global node references and output node.
        self.global_frequency_node = None;
        self.global_glide_node = None;
        self.global_velocity_node = None;
        self.global_gatemixer_node = None;
        self.output_node = None;

        // IMPORTANT: We intentionally do NOT release the dedicated gate buffer or
        // any macro buffers here. The gate buffer index remains valid and
        // reserved, and MacroManager continues to own its buffers. New nodes
        // will acquire fresh buffers from the pool without aliasing these.
    }

    pub fn set_output_node(&mut self, node: NodeId) {
        self.output_node = Some(node);
        self.update_processing_order();
    }

    /// Adds a node to the graph using an explicit NodeId.
    ///
    /// This is primarily used by the audio engine to ensure that
    /// corresponding nodes across different voices share the same
    /// logical identifier, while still keeping the graph's internal
    /// representation based on NodeId keys.
    pub fn add_node_with_id(&mut self, id: NodeId, node: Box<dyn AudioNode>) {
        // Allocate buffers for each port.
        let ports = node.get_ports();
        for port in ports.keys() {
            let buffer_idx = self.buffer_pool.acquire(self.buffer_size);
            self.node_buffers.insert((id, *port), buffer_idx);
        }

        self.nodes.insert(id, node);
        self.update_processing_order();

        // Auto-connect the GlobalFrequency node if the new node accepts it.
        if ports.contains_key(&PortId::GlobalFrequency) {
            let source_node = self.global_glide_node.or(self.global_frequency_node);
            if let Some(global_node_id) = source_node {
                let from_port = if Some(global_node_id) == self.global_glide_node {
                    PortId::AudioOutput0
                } else {
                    PortId::GlobalFrequency
                };
                // Create a connection from the global frequency node's output
                self.add_connection(Connection {
                    from_node: global_node_id,
                    from_port,
                    to_node: id,
                    to_port: PortId::GlobalFrequency,
                    amount: 1.0,
                    modulation_type: ModulationType::Additive,
                    modulation_transform: ModulationTransformation::None,
                });
            }
        }

        // Auto-connect the GateMixer node if the new node accepts CombinedGate.
        if ports.contains_key(&PortId::CombinedGate) {
            if let Some(gate_mixer_node_id) = self.global_gatemixer_node {
                self.add_connection(Connection {
                    from_node: gate_mixer_node_id,
                    from_port: PortId::CombinedGate,
                    to_node: id,
                    to_port: PortId::CombinedGate,
                    amount: 1.0,
                    modulation_type: ModulationType::Additive,
                    modulation_transform: ModulationTransformation::None,
                });
            }
        }
    }

    pub fn add_node(&mut self, node: Box<dyn AudioNode>) -> NodeId {
        let id = NodeId::new();
        self.add_node_with_id(id, node);
        id
    }

    pub fn delete_node(&mut self, node_id: NodeId) {
        // Remove all connections involving this node
        self.connections
            .retain(|_, conn| conn.from_node != node_id && conn.to_node != node_id);

        // Remove from input_connections
        self.input_connections.remove(&node_id);
        for inputs in self.input_connections.values_mut() {
            inputs.retain(|(_, _, _, from_node, _, _)| *from_node != node_id);
        }

        // Release buffers
        let buffers_to_release: Vec<_> = self
            .node_buffers
            .iter()
            .filter_map(|((id, _port), &buffer_idx)| {
                if *id == node_id {
                    Some(buffer_idx)
                } else {
                    None
                }
            })
            .collect();

        for buffer_idx in buffers_to_release {
            self.buffer_pool.release(buffer_idx);
        }

        // Remove node buffers
        self.node_buffers.retain(|(id, _), _| *id != node_id);

        // Remove from processing order
        self.processing_order.retain(|&id| id != node_id);

        // Remove the node itself
        self.nodes.remove(&node_id);

        // Update processing order
        self.update_processing_order();
    }

    pub fn add_connection(&mut self, connection: Connection) {
        let key = ConnectionKey::new(
            connection.from_node,
            connection.from_port,
            connection.to_node,
            connection.to_port,
        );

        let source_buffer_idx = match self
            .node_buffers
            .get(&(connection.from_node, connection.from_port))
        {
            Some(&idx) => idx,
            None => {
                log_error(&format!(
                    "add_connection: source buffer not found for node {:?}, port {:?}",
                    connection.from_node, connection.from_port
                ));
                return;
            }
        };
        let to_port = connection.to_port;
        let to_node = connection.to_node;
        let amount = connection.amount;

        self.connections.insert(key, connection.clone());

        // Update input_connections with the source node included.
        self.input_connections.entry(to_node).or_default().push((
            to_port,
            source_buffer_idx,
            amount,
            connection.from_node,
            connection.modulation_type,
            connection.modulation_transform,
        ));

        self.update_processing_order();
    }

    pub fn remove_macro_connection(
        &mut self,
        buffer_idx: usize,
        target_node: NodeId,
        target_port: PortId,
    ) {
        if let Some(inputs) = self.input_connections.get_mut(&target_node) {
            inputs.retain(|(port, idx, _, _, _, _)| !(*port == target_port && *idx == buffer_idx));
            if inputs.is_empty() {
                self.input_connections.remove(&target_node);
            }
        }
        self.rebuild_plan();
    }

    /// Routes a macro's value buffer into `target_port` of `target_node`.
    pub fn add_macro_connection(
        &mut self,
        buffer_idx: usize,
        target_node: NodeId,
        target_port: PortId,
        amount: f32,
        modulation_type: ModulationType,
        modulation_transform: ModulationTransformation,
    ) {
        self.input_connections
            .entry(target_node)
            .or_default()
            .push((
                target_port,
                buffer_idx,
                amount,
                // No source node: a macro.
                NodeId::default(),
                modulation_type,
                modulation_transform,
            ));
        self.rebuild_plan();
    }

    pub fn debug_connections(&self) -> Vec<(ConnectionKey, Connection)> {
        self.connections
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }

    /// Removes a connection given the source node, target node, and target port.
    pub fn remove_specific_connection(
        &mut self,
        from_node: NodeId,
        to_node: NodeId,
        to_port: PortId,
    ) {
        // web_sys::console::log_1(
        //     &format!(
        //         "remove_specific_connection - Before remove: connections={:?}, inputs={:?}",
        //         self.connections, self.input_connections
        //     )
        //     .into(),
        // );

        // Remove matching connection(s) from the connections map.
        let to_remove: Vec<_> = self
            .connections
            .iter()
            .filter(|(_, conn)| {
                conn.from_node == from_node && conn.to_node == to_node && conn.to_port == to_port
            })
            .map(|(k, _)| *k)
            .collect();

        for key in to_remove {
            self.connections.remove(&key);
        }

        // Gather all buffer indices corresponding to outputs from `from_node`.
        let from_node_buffer_indices: Vec<usize> = self
            .node_buffers
            .iter()
            .filter(|((node_id, _), _)| *node_id == from_node)
            .map(|(_, &buf_idx)| buf_idx)
            .collect();

        // Update input_connections by removing entries matching the criteria.
        if let Some(inputs) = self.input_connections.get_mut(&to_node) {
            inputs.retain(
                |(port, buffer_idx, _amount, src_node, _mod_type, _mod_transform)| {
                    !(*port == to_port
                        && *src_node == from_node
                        && from_node_buffer_indices.contains(buffer_idx))
                },
            );
            if inputs.is_empty() {
                self.input_connections.remove(&to_node);
            }
        }

        // if let Some(inputs) = self.input_connections.get_mut(&to_node) {
        //     inputs.retain(|(port, buffer_idx, _amount, src_node)| {
        //         !(*port == to_port
        //             && *src_node == from_node
        //             && from_node_buffer_indices.contains(buffer_idx))
        //     });
        //     if inputs.is_empty() {
        //         self.input_connections.remove(&to_node);
        //     }
        // }

        // web_sys::console::log_1(
        //     &format!(
        //         "remove_specific_connection - After remove: connections={:?}, inputs={:?}",
        //         self.connections, self.input_connections
        //     )
        //     .into(),
        // );
        self.rebuild_plan();
    }

    /// Removes a connection based on an entire Connection struct.
    pub fn remove_connection(&mut self, connection: &Connection) {
        // Remove the matching connection from the connections map.
        self.connections.retain(|_, existing| {
            !(existing.from_node == connection.from_node
                && existing.to_node == connection.to_node
                && existing.from_port == connection.from_port
                && existing.to_port == connection.to_port)
        });

        // Gather all buffer indices corresponding to outputs from connection.from_node.
        let from_node_buffer_indices: Vec<usize> = self
            .node_buffers
            .iter()
            .filter(|((node_id, _), _)| *node_id == connection.from_node)
            .map(|(_, &buf_idx)| buf_idx)
            .collect();

        // Update input_connections accordingly.
        if let Some(inputs) = self.input_connections.get_mut(&connection.to_node) {
            inputs.retain(
                |(port, buffer_idx, _amount, src_node, _mod_type, _mod_transform)| {
                    !(*port == connection.to_port
                        && *src_node == connection.from_node
                        && from_node_buffer_indices.contains(buffer_idx))
                },
            );
            if inputs.is_empty() {
                self.input_connections.remove(&connection.to_node);
            }
        }

        // if let Some(inputs) = self.input_connections.get_mut(&connection.to_node) {
        //     inputs.retain(|(port, buffer_idx, _amount, src_node)| {
        //         !(*port == connection.to_port
        //             && *src_node == connection.from_node
        //             && from_node_buffer_indices.contains(buffer_idx))
        //     });
        //     if inputs.is_empty() {
        //         self.input_connections.remove(&connection.to_node);
        //     }
        // }

        self.update_processing_order();
    }

    pub fn connect(&mut self, connection: Connection) -> ConnectionKey {
        log_console(&format!(
            "connect - Before connect: connections={:?}, inputs={:?}",
            self.connections, self.input_connections
        ));

        let key = ConnectionKey::new(
            connection.from_node,
            connection.from_port,
            connection.to_node,
            connection.to_port,
        );

        self.connections.insert(key, connection.clone());

        let source_buffer_idx = self.node_buffers[&(connection.from_node, connection.from_port)];
        let inputs = self
            .input_connections
            .entry(connection.to_node)
            .or_default();

        // Update an existing connection if one exists.
        let existing_idx = inputs.iter().position(|(port, _, _, src_node, _, _)| {
            *port == connection.to_port && *src_node == connection.from_node
        });

        if let Some(idx) = existing_idx {
            inputs[idx] = (
                connection.to_port,
                source_buffer_idx,
                connection.amount,
                connection.from_node,
                connection.modulation_type,
                connection.modulation_transform,
            );
        } else {
            inputs.push((
                connection.to_port,
                source_buffer_idx,
                connection.amount,
                connection.from_node,
                connection.modulation_type,
                connection.modulation_transform,
            ));
        }

        // let existing_idx = inputs.iter().position(|(port, _, _, src_node)| {
        //     *port == connection.to_port && *src_node == connection.from_node
        // });

        // if let Some(idx) = existing_idx {
        //     web_sys::console::log_1(
        //         &format!(
        //             "Updating existing connection at idx {}: old={:?}, new=({:?}, {:?}, {:?})",
        //             idx, inputs[idx], connection.to_port, source_buffer_idx, connection.amount
        //         )
        //         .into(),
        //     );
        //     inputs[idx] = (
        //         connection.to_port,
        //         source_buffer_idx,
        //         connection.amount,
        //         connection.from_node,
        //     );
        // } else {
        //     web_sys::console::log_1(
        //         &format!(
        //             "Adding new connection: ({:?}, {:?}, {:?})",
        //             connection.to_port, source_buffer_idx, connection.amount
        //         )
        //         .into(),
        //     );
        //     inputs.push((
        //         connection.to_port,
        //         source_buffer_idx,
        //         connection.amount,
        //         connection.from_node,
        //     ));
        // }

        log_console(&format!(
            "connect - After connect: connections={:?}, inputs={:?}",
            self.connections, self.input_connections
        ));

        self.update_processing_order();
        key
    }

    pub fn get_node(&self, node_id: NodeId) -> Option<&dyn AudioNode> {
        self.nodes.get(&node_id).map(|node| node.as_ref())
    }

    pub fn get_node_mut(&mut self, node_id: NodeId) -> Option<&mut Box<dyn AudioNode>> {
        self.nodes.get_mut(&node_id)
    }

    fn update_processing_order(&mut self) {
        let mut in_degree: FxHashMap<NodeId, usize> = FxHashMap::default();

        // Initialize in-degrees for all nodes to 0
        for &node_id in self.nodes.keys() {
            in_degree.insert(node_id, 0);
        }

        // Compute in-degrees for each node.
        for conn in self.connections.values() {
            *in_degree.entry(conn.to_node).or_insert(0) += 1;
        }

        // Start with all nodes that have no incoming connections.
        let mut queue: Vec<NodeId> = in_degree
            .iter()
            .filter_map(|(&node_id, &deg)| if deg == 0 { Some(node_id) } else { None })
            .collect();

        self.processing_order.clear();

        // Process nodes in order.
        while let Some(node_id) = queue.pop() {
            self.processing_order.push(node_id);

            // For each connection from this node, reduce the in-degree of its destination.
            for conn in self.connections.values() {
                if conn.from_node == node_id {
                    let dest = conn.to_node;
                    if let Some(degree) = in_degree.get_mut(&dest) {
                        *degree -= 1;
                        if *degree == 0 {
                            queue.push(dest);
                        }
                    }
                }
            }
        }

        // If there is a cycle (or some nodes were not reached), add any remaining nodes.
        if self.processing_order.len() < self.nodes.len() {
            for &node_id in self.nodes.keys() {
                if !self.processing_order.contains(&node_id) {
                    self.processing_order.push(node_id);
                }
            }
        }
        // Changes come from the host thread (a patch loading): plan now, so
        // the audio thread never has to.
        self.rebuild_plan();
    }

    pub fn set_gate(&mut self, gate: &[f32]) {
        let buffer = &mut self.buffer_pool.buffers[self.gate_buffer_idx];

        if gate.is_empty() {
            buffer.fill(0.0);
            return;
        }

        if gate.len() == 1 {
            buffer.fill(gate[0]);
            return;
        }

        let copy_len = gate.len().min(buffer.len());
        buffer[..copy_len].copy_from_slice(&gate[..copy_len]);

        if copy_len < buffer.len() {
            let last = gate[copy_len - 1];
            buffer[copy_len..].fill(last);
        }
    }

    pub fn set_frequency(&mut self, freq: &[f32]) {
        if let Some(global_node_id) = self.global_frequency_node {
            if let Some(node) = self.get_node_mut(global_node_id) {
                if let Some(global_freq_node) =
                    node.as_any_mut().downcast_mut::<GlobalFrequencyNode>()
                {
                    global_freq_node.set_base_frequency(freq);
                }
            }
        }
    }

    pub fn set_velocity(&mut self, velocity: &[f32]) {
        if let Some(global_node_id) = self.global_velocity_node {
            if let Some(node) = self.get_node_mut(global_node_id) {
                if let Some(global_vel_node) =
                    node.as_any_mut().downcast_mut::<GlobalVelocityNode>()
                {
                    global_vel_node.set_velocity(velocity);
                }
            }
        }
    }

    pub fn process_audio(&mut self, output_left: &mut [f32], output_right: &mut [f32]) {
        self.process_audio_with_macros(None, output_left, output_right);
    }

    pub fn process_audio_with_macros(
        &mut self,
        macro_manager: Option<&MacroManager>,
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        if self.plan_dirty {
            self.rebuild_plan();
        }
        let macros = macro_manager.filter(|m| m.has_active_macros());
        if let Some(manager) = macros {
            manager.copy_values(
                &self.buffer_pool,
                &mut self.macro_values,
                &mut self.macro_spans,
            );
        }

        for &buffer_idx in self.node_buffers.values() {
            self.buffer_pool.clear(buffer_idx);
        }

        // Every slice below comes from these buffers, which do not move while
        // the block renders. A node's inputs and outputs are distinct buffers
        // (`NodePlan::conflict`), so its shared and mutable slices never alias.
        let buffers = self.buffer_pool.buffers.as_mut_ptr();
        let mut sources: Vec<ModulationSource<'_>> = std::mem::take(&mut self.source_scratch);
        for plan in &self.plan {
            let Some(node) = self.nodes.get_mut(&plan.id) else {
                continue;
            };
            if !node.should_process() {
                for &(_, buffer_idx) in &plan.outputs {
                    self.buffer_pool.clear(buffer_idx);
                }
                continue;
            }
            if plan.conflict {
                continue;
            }
            sources.clear();
            for source in &plan.sources {
                // SAFETY: an index the plan took from this graph's buffers.
                let buffer = unsafe { (*buffers.add(source.buffer)).as_slice() };
                sources.push(ModulationSource {
                    buffer,
                    amount: source.amount,
                    mod_type: source.mod_type,
                    transformation: source.transformation,
                });
            }
            let mut inputs = Inputs::new();
            for (port, range) in &plan.ports {
                inputs.insert(*port, &sources[range.clone()]);
            }
            let mut outputs = Outputs::new();
            for &(port, buffer_idx) in &plan.outputs {
                // SAFETY: as above; no input of this node is an output of it.
                let buffer = unsafe { (*buffers.add(buffer_idx)).as_mut_slice() };
                outputs.insert(port, buffer);
            }
            node.process(&inputs, &mut outputs, self.buffer_size);
            if let (Some(manager), false) = (macros, outputs.is_empty()) {
                manager.apply_modulation(0, &self.macro_values, &self.macro_spans, &mut outputs);
            }
        }
        self.source_scratch = recycle(sources);

        // --- Final Output ---
        if let Some(output_node_id) = self.output_node {
            // Check if the output node itself was processed (i.e., is active)
            let output_node_active = self
                .nodes
                .get(&output_node_id)
                .is_some_and(|n| n.is_active());

            if output_node_active {
                if let Some(&left_buffer_idx) = self
                    .node_buffers
                    .get(&(output_node_id, PortId::AudioOutput0))
                {
                    let left_buffer = self.buffer_pool.copy_out(left_buffer_idx);
                    let len = output_left.len().min(left_buffer.len());

                    // Copy valid data
                    output_left[..len].copy_from_slice(&left_buffer[..len]);

                    // IMPROVED: Use fill instead of loop for remainder
                    if len < output_left.len() {
                        output_left[len..].fill(0.0);
                    }

                    if let Some(&right_buffer_idx) = self
                        .node_buffers
                        .get(&(output_node_id, PortId::AudioOutput1))
                    {
                        let right_buffer = self.buffer_pool.copy_out(right_buffer_idx);
                        let len = output_right.len().min(right_buffer.len());

                        output_right[..len].copy_from_slice(&right_buffer[..len]);

                        if len < output_right.len() {
                            output_right[len..].fill(0.0);
                        }
                    } else {
                        // Mono output case: copy left to right
                        let len = output_right.len().min(left_buffer.len());
                        output_right[..len].copy_from_slice(&left_buffer[..len]);

                        if len < output_right.len() {
                            output_right[len..].fill(0.0);
                        }
                    }
                } else {
                    // Output node active but required buffer not found? Fill with silence.
                    output_left.fill(0.0);
                    output_right.fill(0.0);
                }
            } else {
                // Output node is not active, fill with silence
                output_left.fill(0.0);
                output_right.fill(0.0);
            }
        } else {
            // No output node set, fill with silence
            output_left.fill(0.0);
            output_right.fill(0.0);
        }
    } // End process_audio_with_macros
}

impl AudioGraph {
    /// Works out, for every node in processing order, its output buffers and
    /// the sources reaching its inputs, as the block renderer reads them:
    /// the global gate first for a node that takes it, then the connections
    /// in the order they were made, grouped by port.
    fn rebuild_plan(&mut self) {
        self.plan.clear();
        for &id in &self.processing_order {
            let Some(node) = self.nodes.get(&id) else {
                continue;
            };
            let ports = node.get_ports();
            let mut outputs: Vec<(PortId, usize)> = ports
                .iter()
                .filter(|(_, &is_output)| is_output)
                .filter_map(|(&port, _)| self.node_buffers.get(&(id, port)).map(|&b| (port, b)))
                .collect();
            outputs.sort_by_key(|(port, _)| port.index());

            let mut by_port: Vec<(PortId, Vec<PlannedSource>)> = Vec::new();
            let mut add = |port: PortId, source: PlannedSource| match by_port
                .iter_mut()
                .find(|(p, _)| *p == port)
            {
                Some((_, list)) => list.push(source),
                None => by_port.push((port, vec![source])),
            };
            if ports.contains_key(&PortId::GlobalGate) {
                add(
                    PortId::GlobalGate,
                    PlannedSource {
                        buffer: self.gate_buffer_idx,
                        amount: 1.0,
                        mod_type: ModulationType::Additive,
                        transformation: ModulationTransformation::None,
                    },
                );
            }
            for &(port, buffer, amount, _from, mod_type, transformation) in
                self.input_connections.get(&id).into_iter().flatten()
            {
                add(
                    port,
                    PlannedSource {
                        buffer,
                        amount,
                        mod_type,
                        transformation,
                    },
                );
            }

            let mut sources = Vec::new();
            let mut fed = Vec::new();
            for (port, list) in by_port {
                let start = sources.len();
                sources.extend(list);
                fed.push((port, start..sources.len()));
            }
            let conflict = sources
                .iter()
                .any(|s| outputs.iter().any(|&(_, b)| b == s.buffer));
            if conflict {
                log_error(&format!(
                    "node {id:?} reads one of its own output buffers; it is skipped"
                ));
            }
            self.plan.push(NodePlan {
                id,
                outputs,
                sources,
                ports: fed,
                conflict,
            });
        }
        // Room for the most sources any node takes, so a block never grows it.
        let most = self.plan.iter().map(|p| p.sources.len()).max().unwrap_or(0);
        self.source_scratch.reserve(most);
        self.plan_dirty = false;
    }
}

/// One node's part of a block, worked out ahead.
struct NodePlan {
    id: NodeId,
    /// Its output ports and their buffers.
    outputs: Vec<(PortId, usize)>,
    /// Every source reaching it, grouped by input port.
    sources: Vec<PlannedSource>,
    /// Each fed input port and its range in `sources`.
    ports: Vec<(PortId, std::ops::Range<usize>)>,
    /// One of its inputs is one of its own output buffers: it cannot be
    /// handed both, so it is skipped.
    conflict: bool,
}

struct PlannedSource {
    buffer: usize,
    amount: f32,
    mod_type: ModulationType,
    transformation: ModulationTransformation,
}

/// The emptied scratch, with its allocation, ready for the next block.
fn recycle(mut sources: Vec<ModulationSource<'_>>) -> Vec<ModulationSource<'static>> {
    sources.clear();
    // Same layout in and out, so std collects in place: no new allocation
    // (cubed-rt-check's synth_engine checks it).
    sources
        .into_iter()
        .map(|_| -> ModulationSource<'static> { unreachable!("the scratch was cleared") })
        .collect()
}

impl Drop for AudioGraph {
    fn drop(&mut self) {
        // Clean up all allocated buffers.
        for &buffer_idx in self.node_buffers.values() {
            self.buffer_pool.release(buffer_idx);
        }
        for &buffer_idx in &self.temp_buffer_indices {
            self.buffer_pool.release(buffer_idx);
        }
        self.buffer_pool.release(self.gate_buffer_idx);
        // self.buffer_pool.release(self.freq_buffer_idx);
    }
}

impl std::fmt::Debug for AudioGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioGraph")
            .field("buffer_size", &self.buffer_size)
            .field("num_nodes", &self.nodes.len())
            .field("num_connections", &self.connections.len())
            .finish()
    }
}
