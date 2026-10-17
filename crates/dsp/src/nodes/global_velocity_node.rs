use cubed_simd::num::SimdFloat;
use cubed_simd::Simd;
use std::any::Any;

use crate::graph::ModulationSource;
use crate::impulse_generator::fill_seed;
use crate::{AudioNode, PortId};
use rustc_hash::FxHashMap;

pub struct GlobalVelocityNode {
    base_velocity: Vec<f32>,
    sensitivity: f32,
    /// Interpolation factor between the sensitivity-adjusted base value and the random value.
    /// 0.0 means use only the sensitivity-adjusted value; 1.0 means fully using the random value.
    randomize: f32,
    /// Precomputed random numbers (each in [0, 1]).
    random_numbers: Vec<f32>,
    /// Index into the random_numbers vector.
    random_index: usize,
    /// The current random value, updated on gate events.
    current_random_value: f32,
    /// Holds the previous gate value to detect rising edges.
    prev_gate_value: f32,
}

impl GlobalVelocityNode {
    pub fn new(initial_freq: f32, buffer_size: usize) -> Self {
        // Precompute 1024 random numbers.
        let num_random = 1024;
        let mut random_numbers = vec![0f32; num_random];
        // 4 bytes per f32.
        let mut buf = vec![0u8; num_random * 4];
        fill_seed(&mut buf).expect("Fallback for random number generation failed");
        for i in 0..num_random {
            let start = i * 4;
            let bytes = [buf[start], buf[start + 1], buf[start + 2], buf[start + 3]];
            let num = u32::from_le_bytes(bytes);
            random_numbers[i] = (num as f32) / (u32::MAX as f32);
        }

        Self {
            base_velocity: vec![initial_freq; buffer_size],
            sensitivity: 1.0,
            randomize: 0.0, // default: no randomization
            random_numbers,
            random_index: 0,
            // Initialize with the first random number.
            current_random_value: 0.0,
            prev_gate_value: 0.0,
        }
    }

    pub fn set_velocity(&mut self, velocity: &[f32]) {
        if velocity.len() == 1 {
            self.base_velocity.fill(velocity[0]);
        } else if velocity.len() == self.base_velocity.len() {
            self.base_velocity.copy_from_slice(velocity);
        } else {
            self.base_velocity.fill(velocity[0]);
        }
    }

    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.sensitivity = sensitivity;
    }

    /// Sets the randomization amount (0.0 to 1.0).
    pub fn set_randomize(&mut self, randomize: f32) {
        self.randomize = randomize.clamp(0.0, 1.0);
    }

    /// Processes a segment of the buffer [start, end) using SIMD.
    fn process_segment(&self, start: usize, end: usize, exp: f32, output: &mut [f32]) {
        // We use an 8-lane SIMD vector.
        const LANES: usize = 8;
        let rand_val = self.current_random_value;
        let interp = Simd::splat(self.randomize);
        let one_minus_interp = Simd::splat(1.0 - self.randomize);
        let rand_simd = Simd::splat(rand_val);
        let zero = Simd::splat(0.0);
        let one = Simd::splat(1.0);
        let mut i = start;
        // Process in chunks of LANES.
        while i + LANES <= end {
            // Load a SIMD chunk from base_velocity.
            let base_chunk = Simd::<f32, LANES>::from_slice(&self.base_velocity[i..i + LANES]);
            // Apply sensitivity adjustment.
            let adjusted = if (self.sensitivity - 1.0).abs() < 1e-5 {
                base_chunk
            } else {
                let mut arr = base_chunk.to_array();
                for x in &mut arr {
                    *x = x.powf(exp);
                }
                Simd::from_array(arr)
            };
            // Interpolate between the sensitivity-adjusted value and the random value.
            let mixed = one_minus_interp * adjusted + interp * rand_simd;
            // Clamp to [0, 1].
            let clamped = mixed.simd_clamp(zero, one);
            // Instead of write_to_slice, convert to array and copy.
            let arr = clamped.to_array();
            output[i..i + LANES].copy_from_slice(&arr);
            i += LANES;
        }
        // Process any remaining samples.
        while i < end {
            let base_val = self.base_velocity[i];
            let adjusted = if (self.sensitivity - 1.0).abs() < 1e-5 {
                base_val
            } else {
                base_val.powf(exp)
            };
            let mixed = (1.0 - self.randomize) * adjusted + self.randomize * rand_val;
            output[i] = mixed.clamp(0.0, 1.0);
            i += 1;
        }
    }
}

impl AudioNode for GlobalVelocityNode {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        let mut ports = FxHashMap::default();
        ports.insert(PortId::AudioOutput0, true);
        // Declare a gate port. The false value indicates it isn't an audio output.
        ports.insert(PortId::GlobalGate, false);
        ports
    }

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        let output = outputs
            .get_mut(&PortId::AudioOutput0)
            .expect("Expected AudioOutput0 output port");

        // Build the gate buffer from all gate sources.
        let mut gate_buffer = vec![0.0; buffer_size];
        if let Some(sources) = inputs.get(&PortId::GlobalGate) {
            for source in sources {
                for (dest, &src) in gate_buffer.iter_mut().zip(source.buffer.iter()) {
                    *dest += src * source.amount;
                }
            }
        }

        // Calculate the exponent for sensitivity adjustment.
        let exp = 1.0 / self.sensitivity;
        let rnd_len = self.random_numbers.len();

        // Process the buffer in segments where the random value remains constant.
        let mut seg_start = 0;
        for i in 0..buffer_size {
            let gate_val = gate_buffer[i];
            let gate_on = gate_val > 0.5;
            let prev_gate_on = self.prev_gate_value > 0.5;
            // Rising edge: current gate is on, previous was off.
            if gate_on && !prev_gate_on {
                // Process the segment since the last random value update.
                if seg_start < i {
                    self.process_segment(seg_start, i, exp, output);
                }
                // Update the random value.
                self.random_index = (self.random_index + 1) % rnd_len;
                self.current_random_value = self.random_numbers[self.random_index];
                seg_start = i;
            }
            self.prev_gate_value = gate_val;
        }
        // Process any remaining samples.
        if seg_start < buffer_size {
            self.process_segment(seg_start, buffer_size, exp, output);
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
        "Global Velocity"
    }
    fn node_type(&self) -> &str {
        "global_velocity"
    }
}
