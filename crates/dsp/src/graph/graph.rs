use rustc_hash::{FxHashMap, FxHashSet};

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
use crate::{AudioNode, MacroManager, PortId};

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
        let macro_data = macro_manager.map(|m| m.prepare_macro_data(&self.buffer_pool));

        for &buffer_idx in self.node_buffers.values() {
            self.buffer_pool.clear(buffer_idx);
        }

        // Use FxFxHashMap if you added the dependency and want the potential speedup
        type InputsMap<'a> = FxHashMap<PortId, Vec<ModulationSource<'a>>>;
        type OutputsMap<'a> = FxHashMap<PortId, &'a mut [f32]>;

        let mut required_input_indices = FxHashSet::default();
        let mut input_indices: Vec<usize> = Vec::new();
        let mut output_indices: Vec<usize> = Vec::new();
        'node_loop: for &node_id in &self.processing_order {
            // Drop any borrows from the previous iteration before touching the buffer pool.
            required_input_indices.clear();
            input_indices.clear();
            output_indices.clear();

            // Added optional loop label
            let node = match self.nodes.get(&node_id) {
                Some(n) => n,
                None => continue,
            };

            if !node.should_process() {
                let node_ports = node.get_ports();
                for (port, is_output) in node_ports.iter() {
                    if *is_output {
                        if let Some(buffer_idx) = self.node_buffers.get(&(node_id, *port)) {
                            self.buffer_pool.clear(*buffer_idx);
                        }
                    }
                }
                continue;
            }

            let node_ports = node.get_ports().clone();

            // --- Gather Input and Output Buffer Indices ---
            // Collect required input indices
            if node_ports.contains_key(&PortId::GlobalGate) {
                required_input_indices.insert(self.gate_buffer_idx);
            }
            if let Some(connections) = self.input_connections.get(&node_id) {
                for &(_port, source_idx, _amount, _src_node, _mod_type, _mod_transform) in
                    connections
                {
                    required_input_indices.insert(source_idx);
                }
            }

            // Collect output buffer indices
            let mut output_indices_map: FxHashMap<PortId, usize> = FxHashMap::default();
            for (&port, &is_output) in node_ports.iter() {
                if is_output {
                    if let Some(&idx) = self.node_buffers.get(&(node_id, port)) {
                        output_indices_map.insert(port, idx);
                    }
                }
            }

            input_indices.extend(required_input_indices.iter().copied());
            output_indices.extend(output_indices_map.values().copied());

            // --- Get Buffers with Zero-Copy ---
            // Get both input (immutable) and output (mutable) buffers in a single call
            let (input_buffers, output_buffers) = match self
                .buffer_pool
                .get_mixed_buffers(&input_indices, &output_indices)
            {
                Ok((inputs, outputs)) => (inputs, outputs),
                Err(e) => {
                    log_error(&format!(
                        "Error getting buffers for node {:?}: {}",
                        node_id, e
                    ));
                    continue 'node_loop;
                }
            };

            // Build input map with buffer slices (zero-copy)
            let mut input_slices: FxHashMap<usize, &[f32]> = FxHashMap::default();
            for (idx, slice) in input_buffers {
                input_slices.insert(idx, slice);
            }

            let mut inputs: InputsMap = FxHashMap::default();

            // Global Gate
            if node_ports.contains_key(&PortId::GlobalGate) {
                if let Some(&slice) = input_slices.get(&self.gate_buffer_idx) {
                    inputs
                        .entry(PortId::GlobalGate)
                        .or_default()
                        .push(ModulationSource {
                            buffer: slice, // Zero-copy: use slice reference directly
                            amount: 1.0,
                            mod_type: ModulationType::Additive,
                            transformation: ModulationTransformation::None,
                        });
                }
            }

            // Connection Inputs
            if let Some(connections) = self.input_connections.get(&node_id) {
                for &(port, source_idx, amount, _src_node, mod_type, mod_transform) in connections {
                    if let Some(&slice) = input_slices.get(&source_idx) {
                        inputs.entry(port).or_default().push(ModulationSource {
                            buffer: slice, // Zero-copy: use slice reference directly
                            amount,
                            mod_type,
                            transformation: mod_transform,
                        });
                    }
                }
            }

            // Build output map
            let mut outputs: OutputsMap = FxHashMap::default();
            for (buffer_idx, buffer_slice) in output_buffers {
                if let Some((port, _)) = output_indices_map
                    .iter()
                    .find(|(_, &idx)| idx == buffer_idx)
                {
                    outputs.insert(*port, buffer_slice);
                }
            }

            // --- Process Node ---
            if !outputs.is_empty() {
                if let Some(node_mut) = self.nodes.get_mut(&node_id) {
                    node_mut.process(&inputs, &mut outputs, self.buffer_size);

                    // --- Apply Macro Modulation ---
                    if let (Some(mgr), Some(ref data)) = (macro_manager, &macro_data) {
                        if mgr.has_active_macros() {
                            mgr.apply_modulation(0, data, &mut outputs);
                        }
                    }
                }
            } else {
                // Node has no outputs, just process it (might have side effects?)
                let mut empty_outputs: OutputsMap = FxHashMap::default();
                if let Some(node_mut) = self.nodes.get_mut(&node_id) {
                    node_mut.process(&inputs, &mut empty_outputs, self.buffer_size);
                }
            }
        } // End node processing loop ('node_loop)

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
