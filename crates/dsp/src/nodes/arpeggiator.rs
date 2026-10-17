use crate::traits::{Inputs, Outputs};
use cubed_simd::Simd;
use std::any::Any;

use rustc_hash::FxHashMap;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use web_sys::console;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
fn log_console(message: &str) {
    console::log_1(&message.into());
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
fn log_console(_message: &str) {}

use crate::graph::ModulationSource;
use crate::{AudioNode, PortId};

/// A single step in the arpeggiator pattern.
#[derive(Clone, Copy)]
pub struct PatternStep {
    /// The modulation value (in cents) for this step.
    pub value: f32,
    /// Whether this step is active (i.e. should trigger the gate).
    pub active: bool,
}

/// Modes for the arpeggiator.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArpeggiatorMode {
    /// Runs continuously.
    FreeRunning,
    /// Plays the pattern forward then in reverse.
    PingPong,
    /// Resets the progression on a gate trigger.
    Trigger,
}

/// A generator node that produces a modulation signal (in cents) according to an arpeggiator pattern.
/// In Trigger mode, a gate input is used to reset the progression on a rising edge.
/// If gate output is enabled, it writes a gate signal that is high for active steps (except during a brief gap)
/// and low for skipped steps.
pub struct ArpeggiatorGenerator {
    /// Whether the arpeggiator is enabled.
    enabled: bool,
    /// The arpeggiator pattern (each step holds a modulation value and active flag).
    pattern: Vec<PatternStep>,
    /// Number of samples per arpeggiator step.
    step_samples: usize,
    /// A counter tracking the current sample position.
    sample_counter: usize,
    /// The selected arpeggiator mode.
    mode: ArpeggiatorMode,
    /// The previous gate state (for Trigger mode edge detection).
    prev_gate_active: bool,
    /// Flag to control whether the node writes a gate signal.
    gate_output_enabled: bool,
    /// Field for storing the previous step index (for potential further extensions).
    prev_step: usize,
}

impl Default for ArpeggiatorGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl ArpeggiatorGenerator {
    /// Creates a new arpeggiator generator.
    pub fn new() -> Self {
        Self {
            enabled: false,
            pattern: Vec::new(),
            step_samples: 0,
            sample_counter: 0,
            mode: ArpeggiatorMode::Trigger,
            prev_gate_active: false,
            gate_output_enabled: false,
            prev_step: 0,
        }
    }

    /// Enable the arpeggiator with a given pattern and step duration (in samples).
    pub fn enable(&mut self, pattern: Vec<PatternStep>, step_samples: usize) {
        self.enabled = true;
        self.pattern = pattern;
        self.step_samples = step_samples;
        self.sample_counter = 0;
        self.prev_step = 0;
    }

    /// Disable the arpeggiator.
    pub fn disable(&mut self) {
        self.enabled = false;
    }

    /// Set the arpeggiator mode.
    pub fn set_mode(&mut self, mode: ArpeggiatorMode) {
        self.mode = mode;
    }

    /// Set the arpeggiator pattern and reset the progression.
    pub fn set_pattern(&mut self, pattern: Vec<PatternStep>) {
        self.pattern = pattern;
        self.sample_counter = 0;
        self.prev_step = 0;
    }

    // Measure 2 is written as measure-1 pitches `- 12.0`, including `12.0 - 12.0`.
    #[allow(clippy::eq_op)]
    pub fn create_test_pattern(&mut self, sample_rate: f32, arp_delay: f32) {
        // Two measures, each 16 notes, for a total of 32 steps.
        // Measure 1: a-c-f-g-a-c-f-g-a (up), then g-f-c-a-g-f-c (down)
        // Measure 2: f-a-d-e-f-a-d-e-f (up), then e-d-a-f-e-d-a (down)
        //
        // Offsets are relative to the FIRST A = 0 semitones.

        // Measure 1 (16 steps):
        let measure1 = [
            // Up (9)
            0.0,  // A
            3.0,  // C
            8.0,  // F
            10.0, // G
            12.0, // A (1 octave up)
            15.0, // C
            20.0, // F
            22.0, // G
            24.0, // A (2 octaves above the start)
            // Down (7)
            22.0, // G
            20.0, // F
            15.0, // C
            12.0, // A
            10.0, // G
            8.0,  // F
            3.0,  // C
        ];

        // Measure 2 (16 steps):
        let measure2 = [
            // Up (9)
            8.0 - 12.0,  // F
            12.0 - 12.0, // A
            17.0 - 12.0, // D
            19.0 - 12.0, // E
            20.0 - 12.0, // F
            24.0 - 12.0, // A
            29.0 - 12.0, // D
            31.0 - 12.0, // E
            32.0 - 12.0, // F (top)
            // Down (7)
            31.0 - 12.0, // E
            29.0 - 12.0, // D
            24.0 - 12.0, // A
            20.0 - 12.0, // F
            19.0 - 12.0, // E
            17.0 - 12.0, // D
            12.0 - 12.0, // A
        ];

        // Convert them to PatternStep structs with `active: true`
        let mut pattern = Vec::with_capacity(measure1.len() + measure2.len());
        for semitone in measure1.iter().chain(measure2.iter()) {
            pattern.push(PatternStep {
                value: *semitone,
                active: true,
            });
        }

        // Determine how many samples each note should last
        let step_samples = (sample_rate * arp_delay) as usize;

        // Enable the gate so the notes sound
        self.set_gate_output_enabled(true);

        // Load the combined pattern (32 steps)
        self.enable(pattern, step_samples);

        // Cycle through them
        self.set_mode(ArpeggiatorMode::Trigger);
        self.set_gate_output_enabled(true);
    }

    /// Set the delay time between steps (in samples) and reset the progression.
    pub fn set_delay_time(&mut self, delay_samples: usize) {
        self.step_samples = delay_samples;
        self.sample_counter = 0;
        self.prev_step = 0;
    }

    /// Enable or disable gate output.
    pub fn set_gate_output_enabled(&mut self, enabled: bool) {
        self.gate_output_enabled = enabled;
    }

    /// Computes the modulation value (in cents) for the given sample index.
    /// If the corresponding pattern step is inactive, it returns 0.0.
    #[inline]
    fn modulation_value(&self, sample_index: usize) -> f32 {
        if !self.enabled || self.pattern.is_empty() || self.step_samples == 0 {
            return 0.0;
        }
        let step_index = match self.mode {
            ArpeggiatorMode::FreeRunning | ArpeggiatorMode::Trigger => {
                (sample_index / self.step_samples) % self.pattern.len()
            }
            ArpeggiatorMode::PingPong => {
                let n = self.pattern.len();
                if n == 1 {
                    0
                } else {
                    let period = 2 * n - 2;
                    let pos = (sample_index / self.step_samples) % period;
                    if pos < n {
                        pos
                    } else {
                        period - pos
                    }
                }
            }
        };

        let step = self.pattern[step_index];
        if step.active {
            step.value
        } else {
            // For a skipped step, output zero modulation.
            0.0
        }
    }

    /// Process modulation from a gate input.
    /// If no gate input is provided, returns a vector filled with the default value.
    fn process_modulations(
        &self,
        buffer_size: usize,
        maybe_sources: Option<&[ModulationSource]>,
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

    /// Process the arpeggiator in modes that use SIMD block processing.
    fn process_simd(&mut self, outputs: &mut Outputs<'_>, buffer_size: usize) {
        let output = outputs
            .get_mut(&PortId::AudioOutput0)
            .expect("Expected AudioOutput0 output port");
        const LANES: usize = 4;
        type Vf32 = Simd<f32, LANES>;
        let mut i = 0;
        while i + LANES <= buffer_size {
            let global_index = self.sample_counter + i;
            let global_index_end = self.sample_counter + i + LANES - 1;
            // Determine if the entire SIMD block falls within the same arpeggiator step.
            let step_start = global_index / self.step_samples;
            let step_end = global_index_end / self.step_samples;
            if self.enabled && step_start == step_end {
                let value = self.modulation_value(global_index);
                let modulation_vec = Vf32::splat(value);
                output[i..i + LANES].copy_from_slice(&modulation_vec.to_array());
            } else {
                for j in i..i + LANES {
                    output[j] = self.modulation_value(self.sample_counter + j);
                }
            }
            i += LANES;
        }
        // Process any remaining samples.
        for j in i..buffer_size {
            output[j] = self.modulation_value(self.sample_counter + j);
        }

        // --- Debug Logging ---
        let num_debug_samples = std::cmp::min(buffer_size, 8);
        let _debug_samples = &output[0..num_debug_samples];
        // console::log_1(
        //     &format!(
        //         "Arpeggiator SIMD Debug: sample_counter = {}, first {} mod values = {:?}",
        //         self.sample_counter, num_debug_samples, debug_samples
        //     )
        //     .into(),
        // );

        // // Build a vector of unique modulation values (using an epsilon comparison).
        // {
        //     let mut unique_values: Vec<f32> = Vec::new();
        //     for &val in &output[..buffer_size] {
        //         if !unique_values
        //             .iter()
        //             .any(|&x| (x - val).abs() < std::f32::EPSILON)
        //         {
        //             unique_values.push(val);
        //         }
        //     }
        //     console::log_1(
        //         &format!(
        //             "Arpeggiator SIMD Debug: unique modulation values in buffer: {:?}",
        //             unique_values
        //         )
        //         .into(),
        //     );
        // }

        self.sample_counter += buffer_size;
    }

    fn process_trigger_mode<'a>(
        &mut self,
        inputs: &Inputs<'a>,
        outputs: &mut Outputs<'_>,
        buffer_size: usize,
    ) {
        let output = outputs
            .get_mut(&PortId::AudioOutput0)
            .expect("Expected AudioOutput0 output port");
        let gate_mod = self.process_modulations(buffer_size, inputs.get(&PortId::GlobalGate), 0.0);
        for j in 0..buffer_size {
            let current_gate = gate_mod[j] > 0.5;
            if !self.prev_gate_active && current_gate {
                log_console(&format!(
                    "Arpeggiator Trigger Debug: Rising edge detected at sample {}",
                    self.sample_counter + j
                ));
                self.sample_counter = 0;
                self.prev_step = 0;
            }
            self.prev_gate_active = current_gate;
            output[j] = self.modulation_value(self.sample_counter);
            self.sample_counter += 1;
        }

        // // --- Debug Logging ---
        // let num_debug_samples = std::cmp::min(buffer_size, 8);
        // let debug_samples = &output[0..num_debug_samples];
        // console::log_1(
        //     &format!(
        //         "Arpeggiator Trigger Debug: sample_counter = {}, first {} mod values = {:?}",
        //         self.sample_counter, num_debug_samples, debug_samples
        //     )
        //     .into(),
        // );

        // // Build a vector of unique modulation values using an epsilon comparison.
        // {
        //     let mut unique_values: Vec<f32> = Vec::new();
        //     for &val in &output[..buffer_size] {
        //         if !unique_values
        //             .iter()
        //             .any(|&x| (x - val).abs() < std::f32::EPSILON)
        //         {
        //             unique_values.push(val);
        //         }
        //     }
        //     console::log_1(
        //         &format!(
        //             "Arpeggiator Trigger Debug: unique modulation values in buffer: {:?}",
        //             unique_values
        //         )
        //         .into(),
        //     );
        // }
    }

    /// Process the node while optionally writing a gate signal.
    ///
    /// For each step, if the step is active the gate output is high (except for a brief gap at the end of the step);
    /// if the step is skipped, the gate output remains low (0.0) for the entire duration.
    fn process_with_optional_gate<'a>(
        &mut self,
        inputs: &Inputs<'a>,
        outputs: &mut Outputs<'_>,
        buffer_size: usize,
    ) {
        // Process modulation output according to mode.
        if self.mode == ArpeggiatorMode::Trigger {
            self.process_trigger_mode(inputs, outputs, buffer_size);
        } else {
            self.process_simd(outputs, buffer_size);
        }

        // If gate output is enabled, write the gate signal.
        if self.gate_output_enabled {
            if let Some(gate_output) = outputs.get_mut(&PortId::ArpGate) {
                // Define a gap duration (in samples) at the end of an active step.
                let gap_samples = 2;
                // Compute the starting index for the current block.
                let block_start = self.sample_counter - buffer_size;
                for j in 0..buffer_size {
                    let global_index = block_start + j;
                    let relative = global_index % self.step_samples;
                    let step_index = (global_index / self.step_samples) % self.pattern.len();
                    let pattern_step = self.pattern[step_index];
                    // If the step is inactive, the gate remains off.
                    if !pattern_step.active {
                        gate_output[j] = 0.0;
                    } else {
                        // Otherwise, gate is high for most of the step except during the gap.
                        gate_output[j] = if relative >= self.step_samples - gap_samples {
                            0.0
                        } else {
                            1.0
                        };
                    }
                }
            }
        }
    }
}

impl AudioNode for ArpeggiatorGenerator {
    /// Define the node's ports.
    ///
    /// - PortId::AudioOutput0: modulation output (in cents).
    /// - PortId::GlobalGate: optional gate input (for Trigger mode).
    /// - PortId::ArpGate: optional gate trigger output.
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        let mut ports = FxHashMap::default();
        ports.insert(PortId::AudioOutput0, true);
        ports.insert(PortId::GlobalGate, false);
        ports.insert(PortId::ArpGate, true);
        ports
    }

    fn process<'a>(&mut self, inputs: &Inputs<'a>, outputs: &mut Outputs<'_>, buffer_size: usize) {
        self.process_with_optional_gate(inputs, outputs, buffer_size);
    }

    fn reset(&mut self) {
        self.sample_counter = 0;
        self.prev_gate_active = false;
        self.prev_step = 0;
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn is_active(&self) -> bool {
        true
    }
    fn set_active(&mut self, _active: bool) {
        // Always active.
    }
    fn name(&self) -> &'static str {
        "Arpeggiator"
    }
    fn node_type(&self) -> &str {
        "arpeggiator_generator"
    }
}
