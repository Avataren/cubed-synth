#[cfg(test)]
mod tests {
    use crate::audio_engine::patch::{
        Layout, PatchConnection, PatchFile, PatchMetadata, PatchNode, SynthState, VoiceLayout,
    };
    use crate::graph::{AudioGraph, Connection, NodeId};
    use crate::nodes::{
        AnalogOscillator, GateMixer, GlobalFrequencyNode, GlobalVelocityNode, Mixer, Waveform,
    };
    use crate::traits::PortId;
    use rustc_hash::FxHashMap;
    use uuid::Uuid;

    // This function will serialize an AudioGraph to a PatchFile struct.
    fn serialize_graph_to_patch(graph: &AudioGraph) -> PatchFile {
        let mut nodes_by_type = FxHashMap::default();
        let mut connections = Vec::new();

        for (node_id, node) in &graph.nodes {
            let (node_type, node_name) = if node.as_any().is::<AnalogOscillator>() {
                ("oscillator", "Analog Oscillator")
            } else if node.as_any().is::<Mixer>() {
                ("mixer", "Mixer")
            } else if node.as_any().is::<GlobalFrequencyNode>() {
                ("global_frequency", "Global Frequency")
            } else if node.as_any().is::<GlobalVelocityNode>() {
                ("global_velocity", "Global Velocity")
            } else if node.as_any().is::<GateMixer>() {
                ("gatemixer", "Gate Mixer")
            } else {
                continue; // Skip unknown nodes
            };

            let patch_node = PatchNode {
                id: node_id.to_string(),
                node_type: node_type.to_string(),
                name: node_name.to_string(),
            };
            nodes_by_type
                .entry(node_type.to_string())
                .or_insert_with(Vec::new)
                .push(patch_node);
        }

        for conn in graph.connections.values() {
            let patch_conn = PatchConnection {
                from_id: conn.from_node.to_string(),
                to_id: conn.to_node.to_string(),
                target: conn.to_port as u32,
                amount: conn.amount,
                modulation_type: conn.modulation_type as i32,
                modulation_transform: conn.modulation_transform as i32,
            };
            connections.push(patch_conn);
        }

        // Sort connections to ensure consistent serialization
        connections.sort_by_key(|c| (c.from_id.clone(), c.to_id.clone(), c.target));

        let voice_layout = VoiceLayout {
            id: 0,
            nodes: nodes_by_type,
            connections,
        };

        let layout = Layout {
            voice_count: Some(1),
            canonical_voice: Some(voice_layout),
            voices: Vec::new(),
        };

        let synth_state = SynthState {
            layout,
            oscillators: Default::default(),
            wavetable_oscillators: Default::default(),
            envelopes: Default::default(),
            lfos: Default::default(),
            filters: Default::default(),
            samplers: Default::default(),
            convolvers: Default::default(),
            delays: Default::default(),
            choruses: Default::default(),
            reverbs: Default::default(),
            compressors: Default::default(),
            saturations: Default::default(),
            bitcrushers: Default::default(),
            noise: Default::default(),
            velocity: Default::default(),
        };

        let metadata = PatchMetadata {
            id: "test-patch".to_string(),
            name: "Test Patch".to_string(),
            version: 1,
        };

        PatchFile {
            metadata,
            synth_state,
            audio_assets: Default::default(),
        }
    }

    // This function will apply a patch to an AudioGraph.
    fn apply_patch_to_graph(
        graph: &mut AudioGraph,
        patch: &PatchFile,
        sample_rate: f32,
        block_size: usize,
    ) {
        // Remove all nodes and connections, including default global nodes
        graph.clear();

        // Re-add the global nodes that are part of the AudioGraph constructor,
        // so they can be auto-connected if needed.
        // A better approach would be a graph.from_patch() method.
        let global_velocity_node = Box::new(GlobalVelocityNode::new(1.0, block_size));
        let global_velocity_node_id = graph.add_node(global_velocity_node);
        graph.global_velocity_node = Some(global_velocity_node_id);
        let global_node = Box::new(GlobalFrequencyNode::new(440.0, block_size));
        let global_node_id = graph.add_node(global_node);
        graph.global_frequency_node = Some(global_node_id);
        let gate_mixer = Box::new(GateMixer::new());
        let gate_mixer_id = graph.add_node(gate_mixer);
        graph.global_gatemixer_node = Some(gate_mixer_id);

        let voice_layout = patch
            .synth_state
            .layout
            .canonical_voice()
            .expect("Patch missing canonical voice");

        // Create nodes
        for (node_type, nodes) in &voice_layout.nodes {
            for patch_node in nodes {
                let id = NodeId(Uuid::parse_str(&patch_node.id).unwrap());

                // Skip global nodes as they are already created
                if node_type == "global_frequency"
                    || node_type == "global_velocity"
                    || node_type == "gatemixer"
                {
                    continue;
                }

                let new_node: Box<dyn crate::traits::AudioNode> = match node_type.as_str() {
                    "oscillator" => Box::new(AnalogOscillator::new(
                        sample_rate,
                        Waveform::Sine,
                        Default::default(),
                    )),
                    "mixer" => Box::new(Mixer::new()),
                    _ => continue,
                };
                graph.add_node_with_id(id, new_node);
            }
        }

        // Create connections
        for patch_conn in &voice_layout.connections {
            let from_id = NodeId(Uuid::parse_str(&patch_conn.from_id).unwrap());
            let to_id = NodeId(Uuid::parse_str(&patch_conn.to_id).unwrap());

            // Find the from_port. This is tricky as it's not stored in the connection.
            // For this test, we assume the first output port.
            let from_port = graph
                .nodes
                .get(&from_id)
                .and_then(|n| {
                    n.get_ports()
                        .iter()
                        .find(|(_, &is_output)| is_output)
                        .map(|(p, _)| *p)
                })
                .unwrap_or(PortId::AudioOutput0);

            graph.add_connection(Connection {
                from_node: from_id,
                from_port,
                to_node: to_id,
                to_port: PortId::from_u32(patch_conn.target),
                amount: patch_conn.amount,
                modulation_type: crate::graph::ModulationType::from_i32(patch_conn.modulation_type),
                modulation_transform: crate::graph::ModulationTransformation::from_i32(
                    patch_conn.modulation_transform,
                ),
            });
        }
    }

    #[test]
    fn test_patch_serialization_roundtrip() {
        let sample_rate = 44100.0;
        let block_size = 128;

        // 1. Create an initial AudioGraph
        let mut original_graph = AudioGraph::new(block_size);
        let osc_id = original_graph.add_node(Box::new(AnalogOscillator::new(
            sample_rate,
            Waveform::Sine,
            Default::default(),
        )));
        let mixer_id = original_graph.add_node(Box::new(Mixer::new()));
        original_graph.add_connection(Connection {
            from_node: osc_id,
            from_port: PortId::AudioOutput0,
            to_node: mixer_id,
            to_port: PortId::AudioInput0,
            amount: 1.0,
            modulation_type: crate::graph::ModulationType::Additive,
            modulation_transform: crate::graph::ModulationTransformation::None,
        });
        original_graph.set_output_node(mixer_id);

        // 2. Serialize to PatchFile
        let original_patch = serialize_graph_to_patch(&original_graph);
        let json = serde_json::to_string_pretty(&original_patch).unwrap();

        println!("Serialized JSON:\n{}", json);

        // 3. Deserialize from JSON
        let deserialized_patch: PatchFile = serde_json::from_str(&json).unwrap();

        // 4. Apply patch to a new graph
        let mut new_graph = AudioGraph::new(block_size);
        apply_patch_to_graph(&mut new_graph, &deserialized_patch, sample_rate, block_size);

        // 5. Serialize the new graph and compare
        let new_patch = serialize_graph_to_patch(&new_graph);

        // We can't directly compare the patches because some fields are not serialized/deserialized (e.g. node parameters).
        // For this test, we'll compare the structure of the layout.

        let original_layout = &original_patch.synth_state.layout;
        let new_layout = &new_patch.synth_state.layout;

        assert_eq!(
            original_layout.resolved_voice_count(),
            new_layout.resolved_voice_count(),
            "Voice count should be equal"
        );

        let original_voice = original_layout
            .canonical_voice()
            .expect("Original layout missing canonical voice");
        let new_voice = new_layout
            .canonical_voice()
            .expect("New layout missing canonical voice");

        let original_node_count: usize = original_voice.nodes.values().map(|v| v.len()).sum();
        let new_node_count: usize = new_voice.nodes.values().map(|v| v.len()).sum();
        assert_eq!(
            original_node_count, new_node_count,
            "Node count should be equal"
        );

        assert_eq!(
            original_voice.connections.len(),
            new_voice.connections.len(),
            "Connection count should be equal"
        );

        // More detailed comparison could be done here if PartialEq is implemented on the patch structs.
    }

    #[test]
    fn test_apply_patch_after_deserialization() {
        use crate::audio_engine::AudioEngine;
        use crate::graph::ModulationTransformation;
        use crate::graph::ModulationType;

        let sample_rate = 44100.0;
        let block_size = 128;

        // 1. Create an initial engine and build a simple graph in its first voice
        let mut initial_engine = AudioEngine::new_with_block_size(sample_rate, 1, block_size);
        initial_engine.init(sample_rate, 1);

        let osc_id;
        let global_freq_id;
        {
            let voice_graph = &mut initial_engine.voices[0].graph;
            let osc = Box::new(AnalogOscillator::new(
                sample_rate,
                Waveform::Sine,
                Default::default(),
            ));
            osc_id = voice_graph.add_node(osc);

            global_freq_id = voice_graph.global_frequency_node.unwrap();

            // Connect global frequency to oscillator frequency
            voice_graph.add_connection(Connection {
                from_node: global_freq_id,
                from_port: PortId::GlobalFrequency,
                to_node: osc_id,
                to_port: PortId::Frequency,
                amount: 1.0,
                modulation_type: ModulationType::Additive,
                modulation_transform: ModulationTransformation::None,
            });
        } // Release mutable borrow

        // 2. Serialize the graph state to a patch
        let patch = serialize_graph_to_patch(&initial_engine.voices[0].graph);
        let json = serde_json::to_string(&patch).unwrap();

        // 3. Create a new engine and initialize it with the patch
        let mut new_engine = AudioEngine::new_with_block_size(sample_rate, 1, block_size);
        new_engine.init_with_patch(&json).unwrap();

        // 4. Try to modify the deserialized graph by adding a new node and connection
        let mixer_id;
        {
            let new_voice_graph = &mut new_engine.voices[0].graph;
            let mixer = Box::new(Mixer::new());
            mixer_id = new_voice_graph.add_node(mixer);
            new_voice_graph.set_output_node(mixer_id);

            // This is the critical part: try to connect the *deserialized* oscillator to the *new* mixer.
            // This will fail if the osc_id from the old graph isn't correctly mapped in the new one.
            new_voice_graph.add_connection(Connection {
                from_node: osc_id, // Using the ID from the original graph
                from_port: PortId::AudioOutput0,
                to_node: mixer_id,
                to_port: PortId::AudioInput0,
                amount: 1.0,
                modulation_type: ModulationType::Additive,
                modulation_transform: ModulationTransformation::None,
            });
        } // Release mutable borrow

        // 5. Verify that the new connection was made
        let final_voice_graph = &new_engine.voices[0].graph;
        let connections_to_mixer = final_voice_graph.input_connections.get(&mixer_id).unwrap();

        assert_eq!(
            connections_to_mixer.len(),
            1,
            "The new mixer should have one input connection."
        );

        let (port, _buffer_idx, _amount, from_node, _mod_type, _mod_transform) =
            connections_to_mixer[0];
        assert_eq!(
            from_node, osc_id,
            "The connection should come from the original oscillator's ID."
        );
        assert_eq!(
            port,
            PortId::AudioInput0,
            "The connection should go to the mixer's audio input."
        );
    }

    #[test]
    fn test_real_patch_json_parses() {
        use crate::audio_engine::patch::PatchFile;
        use std::fs;

        let json =
            fs::read_to_string("tests/real_patch.json").expect("failed to read real_patch.json");

        let parsed: Result<PatchFile, serde_json::Error> = serde_json::from_str(&json);
        assert!(
            parsed.is_ok(),
            "serde_json failed to parse real_patch.json: {:?}",
            parsed.err()
        );
    }
}
