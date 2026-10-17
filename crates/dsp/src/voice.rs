use crate::{
    graph::{ModulationTransformation, ModulationType},
    nodes::{EnvelopePhase, Lfo, LfoRetriggerMode},
    AudioGraph, AudioNode, Envelope, MacroManager, ModulationTarget, NodeId, PortId,
};

#[derive(Debug)]
pub struct Voice {
    pub id: usize,
    pub graph: AudioGraph,
    pub output_node: NodeId,

    // Voice state
    buffer_size: usize,
    pub current_gate: f32,
    pub current_frequency: f32,
    pub current_velocity: f32,
    pub active: bool,
    macro_manager: MacroManager,
}

impl Voice {
    pub fn new(id: usize, buffer_size: usize) -> Self {
        let buffer_size = buffer_size.max(1);
        let mut graph = AudioGraph::new(buffer_size);
        let macro_manager = MacroManager::new(4, &mut graph.buffer_pool, buffer_size);
        let output_node = NodeId::default();
        graph.set_output_node(output_node);

        Self {
            id,
            graph,
            output_node,
            buffer_size,
            current_gate: 0.0,
            current_frequency: 440.0,
            current_velocity: 1.0,
            active: false,
            macro_manager,
        }
    }

    pub fn clear(&mut self) {
        // Clear the graph
        self.graph.clear();

        // Clear stored node references
        self.output_node = NodeId::default();

        // Reset state
        self.current_gate = 0.0;
        self.current_frequency = 440.0;
        self.active = false;

        // Clear macro manager
        self.macro_manager.clear(&mut self.graph.buffer_pool);
    }

    pub fn set_output_node(&mut self, node: NodeId) {
        self.output_node = node;
        self.graph.set_output_node(node);
    }

    // pub fn add_oscillator(&mut self, sample_rate: f32) -> NodeId {
    //     let osc = ModulatableOscillator::new(sample_rate);
    //     let osc_id = self.graph.add_node(Box::new(osc));
    //     self.oscillators.push(osc_id);
    //     osc_id
    // }

    pub fn update_active_state(&mut self) {
        // For physical modeling synths like Karplus-Strong:
        // We need to keep processing even after envelopes finish
        // as the resonant structures (comb filters, etc.) need to decay naturally

        // A voice is active if:
        // 1. Gate is currently on (note is being held), OR
        // 2. There are active envelopes connected to the output path, OR
        // 3. There is significant audio output from the voice (for decay tails)

        let gate_active = self.current_gate > 0.0;
        let has_active_envelopes = self.has_active_envelopes();
        let has_audio_output = self.has_significant_audio_output();

        self.active = gate_active || has_active_envelopes || has_audio_output;
    }

    // Simple check for active envelopes (same as your original implementation)
    fn has_active_envelopes(&self) -> bool {
        self.graph.nodes.iter().any(|(_id, node)| {
            if let Some(env) = node.as_any().downcast_ref::<Envelope>() {
                env.is_active() && env.get_phase() == EnvelopePhase::Release
            } else {
                false
            }
        })
    }

    // Check if the voice is still producing significant audio output
    fn has_significant_audio_output(&self) -> bool {
        // This is a new method to detect if the voice is still producing sound
        // even after envelopes have completed

        // If we don't have an output node, we can't check
        if self.output_node == NodeId::default() {
            return false;
        }

        // Get the output buffer indices
        if let Some(&left_buffer_idx) = self
            .graph
            .node_buffers
            .get(&(self.output_node, PortId::AudioOutput0))
        {
            // Analyze the buffer to see if it contains significant audio
            let output_buffer = self.graph.buffer_pool.copy_out(left_buffer_idx);

            // Check if the output has any significant audio content
            // Using RMS to detect if there's still meaningful audio
            let mut sum_squared = 0.0;
            for &sample in output_buffer {
                sum_squared += sample * sample;
            }

            let rms = (sum_squared / output_buffer.len() as f32).sqrt();

            // -80dB threshold (very quiet but still audible)
            const SILENCE_THRESHOLD: f32 = 0.0001; // approximately -80dB

            return rms > SILENCE_THRESHOLD;
        }

        false
    }

    //this doesn't quite work yet, dont use
    pub fn has_active_lfos(&self) -> bool {
        self.graph.nodes.iter().any(|(_id, node)| {
            if let Some(lfo) = node.as_any().downcast_ref::<Lfo>() {
                lfo.is_active()
            } else {
                false
            }
        })
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn get_current_gate(&self) -> f32 {
        self.current_gate
    }

    pub fn get_current_frequency(&self) -> f32 {
        self.current_frequency
    }

    pub fn add_macro_modulation(
        &mut self,
        macro_index: usize,
        target_node: NodeId,
        target_port: PortId,
        amount: f32,
        modulation_type: ModulationType,
        modulation_transform: ModulationTransformation,
    ) -> Result<(), String> {
        // First add the modulation target to the macro manager to get the buffer
        self.macro_manager.add_modulation(
            macro_index,
            ModulationTarget {
                node_id: target_node,
                port_id: target_port,
                amount,
                modulation_type,
                modulation_transform,
            },
        )?;

        // Get the buffer index that the macro manager is using
        let buffer_idx = self
            .macro_manager
            .get_macro_buffer_idx(macro_index)
            .ok_or_else(|| "Failed to get macro buffer index".to_string())?;

        // Since the macro modulation isn't coming from a regular node,
        // we supply a reserved NodeId (for example, NodeId(usize::MAX)) as the source.
        self.graph
            .input_connections
            .entry(target_node)
            .or_default()
            .push((
                target_port,
                buffer_idx,
                amount,
                NodeId::default(), // Sentinel value for macro modulations (no source node)
                modulation_type,
                modulation_transform,
            ));

        Ok(())
    }

    pub fn remove_macro_route(
        &mut self,
        macro_index: usize,
        target_node: NodeId,
        target_port: PortId,
    ) -> Result<bool, String> {
        self.macro_manager
            .remove_target(macro_index, target_node, target_port)?;

        if let Some(buffer_idx) = self.macro_manager.get_macro_buffer_idx(macro_index) {
            self.graph
                .remove_macro_connection(buffer_idx, target_node, target_port);
        }

        Ok(true)
    }

    pub fn clear_macros(&mut self) {
        self.macro_manager.clear(&mut self.graph.buffer_pool);
    }

    pub fn update_macro(&mut self, macro_index: usize, values: &[f32]) -> Result<(), String> {
        self.macro_manager
            .update_macro(macro_index, values, &mut self.graph.buffer_pool)
    }

    pub fn process_audio(
        &mut self,
        gate_buffer: &[f32],
        frequency_buffer: &[f32],
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        // First, let's process the audio - we need to do this before checking
        // has_significant_audio_output to have valid output buffers to analyze

        let gate_present = gate_buffer.iter().any(|&g| g > 0.0);

        if self.is_active() || gate_present {
            // Normal processing path for active voices
            let single_gate = [self.current_gate];
            let gate_slice = if gate_buffer.is_empty() {
                &single_gate
            } else {
                gate_buffer
            };

            // Use full gate buffer so envelopes see per-sample changes.
            self.graph.set_gate(gate_slice);
            let single_frequency = [self.current_frequency];
            let frequency_slice = if frequency_buffer.is_empty() {
                &single_frequency
            } else {
                frequency_buffer
            };

            self.graph.set_frequency(frequency_slice);
            self.graph.set_velocity(&[self.current_velocity]);
            self.graph.process_audio_with_macros(
                Some(&self.macro_manager),
                output_left,
                output_right,
            );
        } else {
            // Only update free-running LFOs if we have them
            if self.has_free_running_lfos() {
                self.update_free_running_lfos();
            }
            // Clear the output
            output_left.fill(0.0);
            output_right.fill(0.0);
        }

        // Update the active state for the next cycle
        self.update_active_state();

        // Debug info can be helpful during development
        // if self.is_active() {
        //     web_sys::console::log_1(&format!(
        //         "Voice {} active: gate={}, has_env={}, has_output={}",
        //         self.id,
        //         self.current_gate > 0.0,
        //         self.has_active_envelopes(),
        //         self.has_significant_audio_output()
        //     ).into());
        // }
    }

    fn has_free_running_lfos(&self) -> bool {
        self.graph.nodes.iter().any(|(_id, node)| {
            if let Some(lfo) = node.as_any().downcast_ref::<Lfo>() {
                lfo.retrigger_mode == LfoRetriggerMode::FreeRunning
            } else {
                false
            }
        })
    }

    // New method to only update LFO phases
    fn update_free_running_lfos(&mut self) {
        for node in self.graph.nodes.values_mut() {
            if let Some(lfo) = node.as_any_mut().downcast_mut::<Lfo>() {
                if lfo.retrigger_mode == LfoRetriggerMode::FreeRunning {
                    // web_sys::console::log_1(&format!("LFO state advancing phase",).into());
                    // Only advance the phase
                    lfo.advance_phase_for_buffer(self.buffer_size);
                }
            }
        }
    }

    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }
}
