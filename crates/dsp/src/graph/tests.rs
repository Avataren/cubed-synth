// use super::graph::AudioGraph;
// use super::types::Connection;
// use crate::{AudioNode, PortId};
// use std::{any::Any, collections::FxHashMap};

// // Mock AudioNode implementation for testing
// struct MockNode {
//     ports: FxHashMap<PortId, bool>,
//     id: usize,
// }

// impl MockNode {
//     fn new(id: usize, inputs: Vec<PortId>, outputs: Vec<PortId>) -> Self {
//         let mut ports = FxHashMap::new();
//         for port in inputs {
//             ports.insert(port, false);
//         }
//         for port in outputs {
//             ports.insert(port, true);
//         }
//         Self { ports, id }
//     }
// }

// impl AudioNode for MockNode {
//     fn get_ports(&self) -> FxHashMap<PortId, bool> {
//         self.ports.clone()
//     }

//     fn process(
//         &mut self,
//         inputs: &FxHashMap<PortId, &[f32]>,
//         outputs: &mut FxHashMap<PortId, &mut [f32]>,
//         _buffer_size: usize,
//     ) {
//         // Copy gate to output for test
//         if let Some(gate_input) = inputs.get(&PortId::Gate) {
//             if let Some(output) = outputs.get_mut(&PortId::AudioOutput0) {
//                 output.copy_from_slice(gate_input);
//             }
//         }

//         // Pass through audio input if it exists
//         if let Some(audio_input) = inputs.get(&PortId::AudioInput0) {
//             if let Some(output) = outputs.get_mut(&PortId::AudioOutput0) {
//                 output.copy_from_slice(audio_input);
//             }
//         }
//     }

//     fn reset(&mut self) {}

//     fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
//         self
//     }

//     fn as_any(&self) -> &dyn Any {
//         self
//     }

//     fn is_active(&self) -> bool {
//         true
//     }

//     fn set_active(&mut self, _active: bool) {}
// }

// #[cfg(test)]
// mod tests {
//     use super::*;

//     #[test]
//     fn test_buffer_pool_management() {
//         let buffer_size = 128;
//         let mut graph = AudioGraph::new(buffer_size);

//         // Create a simple node with one input and one output
//         let node = Box::new(MockNode::new(
//             0,
//             vec![PortId::AudioInput0],
//             vec![PortId::AudioOutput0],
//         ));

//         let node_id = graph.add_node(node);

//         // Verify buffer allocation
//         assert!(graph
//             .node_buffers
//             .contains_key(&(node_id, PortId::AudioInput0)));
//         assert!(graph
//             .node_buffers
//             .contains_key(&(node_id, PortId::AudioOutput0)));
//     }

//     #[test]
//     fn test_node_connections() {
//         let buffer_size = 128;
//         let mut graph = AudioGraph::new(buffer_size);

//         // Create two nodes
//         let node1 = Box::new(MockNode::new(0, vec![], vec![PortId::AudioOutput0]));
//         let node2 = Box::new(MockNode::new(
//             1,
//             vec![PortId::AudioInput0],
//             vec![PortId::AudioOutput0],
//         ));

//         let node1_id = graph.add_node(node1);
//         let node2_id = graph.add_node(node2);

//         // Connect them
//         let connection = Connection {
//             from_node: node1_id,
//             from_port: PortId::AudioOutput0,
//             to_node: node2_id,
//             to_port: PortId::AudioInput0,
//             amount: 1.0,
//         };

//         let conn_id = graph.connect(connection);

//         // Verify connection
//         assert!(graph.connections.contains_key(&conn_id));
//         assert_eq!(graph.processing_order, vec![*node1_id, *node2_id]);
//     }

//     #[test]
//     fn test_processing_order() {
//         let buffer_size = 128;
//         let mut graph = AudioGraph::new(buffer_size);

//         // Create three nodes in a chain
//         let node1 = Box::new(MockNode::new(0, vec![], vec![PortId::AudioOutput0]));
//         let node2 = Box::new(MockNode::new(
//             1,
//             vec![PortId::AudioInput0],
//             vec![PortId::AudioOutput0],
//         ));
//         let node3 = Box::new(MockNode::new(
//             2,
//             vec![PortId::AudioInput0],
//             vec![PortId::AudioOutput0],
//         ));

//         let node1_id = graph.add_node(node1);
//         let node2_id = graph.add_node(node2);
//         let node3_id = graph.add_node(node3);

//         // Connect 1->2->3
//         graph.connect(Connection {
//             from_node: node1_id,
//             from_port: PortId::AudioOutput0,
//             to_node: node2_id,
//             to_port: PortId::AudioInput0,
//             amount: 1.0,
//         });

//         graph.connect(Connection {
//             from_node: node2_id,
//             from_port: PortId::AudioOutput0,
//             to_node: node3_id,
//             to_port: PortId::AudioInput0,
//             amount: 1.0,
//         });

//         // Verify correct processing order
//         assert_eq!(
//             graph.processing_order,
//             vec![*node1_id, *node2_id, *node3_id]
//         );
//     }

//     #[test]
//     fn test_audio_processing() {
//         let buffer_size = 128;
//         let mut graph = AudioGraph::new(buffer_size);

//         // Create a gate-controlled node
//         let node = Box::new(MockNode::new(
//             0,
//             vec![PortId::Gate],
//             vec![PortId::AudioOutput0],
//         ));

//         graph.add_node(node);

//         // Test with gate on
//         let gate_on = vec![1.0; buffer_size];
//         let mut output_left = vec![0.0; buffer_size];
//         let mut output_right = vec![0.0; buffer_size];

//         graph.set_gate(&gate_on);
//         graph.process_audio(&mut output_left, &mut output_right);

//         // Verify output matches gate input
//         assert_eq!(output_left, gate_on);
//         assert_eq!(output_right, gate_on);

//         // Test with gate off
//         let gate_off = vec![0.0; buffer_size];
//         graph.set_gate(&gate_off);
//         graph.process_audio(&mut output_left, &mut output_right);

//         // Verify output matches gate input
//         assert_eq!(output_left, gate_off);
//         assert_eq!(output_right, gate_off);
//     }

//     #[test]
//     fn test_frequency_control() {
//         let buffer_size = 128;
//         let mut graph = AudioGraph::new(buffer_size);

//         // Create a frequency-controlled node
//         let node = Box::new(MockNode::new(
//             0,
//             vec![PortId::Frequency],
//             vec![PortId::AudioOutput0],
//         ));

//         graph.add_node(node);

//         // Test with single frequency value
//         let freq = vec![440.0];
//         graph.set_frequency(&freq);

//         // Verify the frequency buffer is filled correctly
//         let freq_buffer = graph.buffer_pool.copy_out(graph.freq_buffer_idx);
//         assert!(freq_buffer.iter().all(|&x| x == 440.0));

//         // Test with buffer of frequencies
//         let freq_buffer = (0..buffer_size).map(|i| i as f32).collect::<Vec<_>>();
//         graph.set_frequency(&freq_buffer);

//         // Verify the frequency buffer matches input
//         let result_buffer = graph.buffer_pool.copy_out(graph.freq_buffer_idx);
//         assert_eq!(&freq_buffer[..], result_buffer);
//     }
// }
