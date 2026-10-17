use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;
use std::any::Any;

use crate::graph::ModulationSource;
use crate::traits::{AudioNode, PortId};

pub struct Glide {
    sample_rate: f32,
    glide_time: f32,
    alpha: f32,
    current: f32,
    active: bool,
    last_gate_value: f32,
    initialized: bool,
}

static PORT_MAP: Lazy<FxHashMap<PortId, bool>> = Lazy::new(|| {
    [
        (PortId::AudioInput0, false),
        (PortId::CombinedGate, false),
        (PortId::AudioOutput0, true),
    ]
    .into_iter()
    .collect()
});

impl Glide {
    pub fn new(sample_rate: f32, glide_time: f32) -> Self {
        let sample_rate = sample_rate.max(1.0);
        let glide_time = glide_time.max(0.0);

        let alpha = Self::time_to_alpha(sample_rate, glide_time);

        Self {
            sample_rate,
            glide_time,
            alpha,
            current: 0.0,
            active: true,
            last_gate_value: 0.0,
            initialized: false,
        }
    }

    fn time_to_alpha(sample_rate: f32, time_sec: f32) -> f32 {
        if time_sec <= 0.0 {
            1.0
        } else {
            let tau_samples = (time_sec * sample_rate).max(1.0);
            1.0 - (-1.0 / tau_samples).exp()
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.alpha = Self::time_to_alpha(self.sample_rate, self.glide_time);
    }

    pub fn set_time(&mut self, glide_time: f32) {
        self.glide_time = glide_time.max(0.0);
        self.alpha = Self::time_to_alpha(self.sample_rate, self.glide_time);
    }

    #[inline]
    fn next_value(&mut self, target: f32) -> f32 {
        let delta = target - self.current;
        self.current += self.alpha * delta;
        self.current
    }
}

impl AudioNode for Glide {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        PORT_MAP.clone()
    }

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        let input = inputs
            .get(&PortId::AudioInput0)
            .and_then(|sources| sources.first())
            .map(|src| &src.buffer[..buffer_size])
            .unwrap_or(&[][..]);

        let output = match outputs.get_mut(&PortId::AudioOutput0) {
            Some(buf) => buf,
            None => return,
        };

        if !self.active {
            // Bypass: copy input through unchanged
            let len = input.len().min(buffer_size);
            output[..len].copy_from_slice(&input[..len]);
            if len < buffer_size {
                output[len..buffer_size].fill(self.current);
            }
            return;
        }

        // Optional gate input (CombinedGate) - used to disable glide when gate is off
        // and to avoid sliding after note releases.
        let gate_buffer = inputs
            .get(&PortId::CombinedGate)
            .and_then(|sources| sources.first())
            .map(|src| &src.buffer[..buffer_size]);

        if input.is_empty() {
            output[..buffer_size].fill(self.current);
            if let Some(gates) = gate_buffer {
                if !gates.is_empty() {
                    self.last_gate_value = *gates.last().unwrap_or(&self.last_gate_value);
                }
            }
            return;
        }

        let gate_present = gate_buffer.is_some();
        let gate_now = gate_buffer.and_then(|g| g.first()).copied().unwrap_or(1.0);
        let gate_last_sample = gate_buffer
            .and_then(|g| g.last())
            .copied()
            .unwrap_or(gate_now);
        let was_open = if gate_present {
            self.last_gate_value > 0.5
        } else {
            true
        };
        let is_open = if gate_present { gate_now > 0.5 } else { true };

        // If the gate is closed, bypass glide and latch to the current input.
        if !is_open {
            let len = input.len().min(buffer_size);
            output[..len].copy_from_slice(&input[..len]);
            if len > 0 {
                self.current = input[len - 1];
                self.initialized = true;
            }
            if len < buffer_size {
                output[len..buffer_size].fill(self.current);
            }
            self.last_gate_value = if gate_present { gate_last_sample } else { 1.0 };
            return;
        }

        // On first activation or after a gate-off period, snap to the input
        // so portamento only engages while the gate stays high.
        if !was_open || !self.initialized {
            let len = input.len().min(buffer_size);
            output[..len].copy_from_slice(&input[..len]);
            if len > 0 {
                self.current = input[len - 1];
                self.initialized = true;
            }
            if len < buffer_size {
                output[len..buffer_size].fill(self.current);
            }
            self.last_gate_value = if gate_present { gate_last_sample } else { 1.0 };
            return;
        }

        let len = input.len().min(buffer_size);
        for i in 0..len {
            output[i] = self.next_value(input[i]);
        }
        if len < buffer_size {
            output[len..buffer_size].fill(self.current);
        }
        self.last_gate_value = if gate_present { gate_last_sample } else { 1.0 };
    }

    fn reset(&mut self) {
        self.current = 0.0;
        self.last_gate_value = 0.0;
        self.initialized = false;
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

    fn set_active(&mut self, active: bool) {
        self.active = active;
        if !active {
            self.reset();
        }
    }

    fn name(&self) -> &'static str {
        "Glide"
    }

    fn node_type(&self) -> &str {
        "glide"
    }

    fn should_process(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ModulationTransformation, ModulationType};

    #[test]
    fn glide_zero_time_is_passthrough() {
        let sample_rate = 48_000.0;
        let mut node = Glide::new(sample_rate, 0.0);
        const BUFFER_SIZE: usize = 16;

        let input: Vec<f32> = (0..BUFFER_SIZE).map(|i| i as f32).collect();

        let mut inputs: FxHashMap<PortId, Vec<ModulationSource>> = FxHashMap::default();
        inputs.insert(
            PortId::AudioInput0,
            vec![ModulationSource {
                buffer: &input,
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );
        inputs.insert(
            PortId::CombinedGate,
            vec![ModulationSource {
                buffer: &[1.0; BUFFER_SIZE],
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );

        let mut out_buf = vec![0.0; BUFFER_SIZE];
        let mut outputs: FxHashMap<PortId, &mut [f32]> = FxHashMap::default();
        outputs.insert(PortId::AudioOutput0, &mut out_buf[..]);

        node.process(&inputs, &mut outputs, BUFFER_SIZE);

        assert_eq!(out_buf, input);
    }

    #[test]
    fn glide_smooths_step_input() {
        let sample_rate = 10.0;
        let glide_time = 0.5;
        let mut node = Glide::new(sample_rate, glide_time);
        const BUFFER_SIZE: usize = 8;

        let input = vec![1.0; BUFFER_SIZE];

        let mut inputs: FxHashMap<PortId, Vec<ModulationSource>> = FxHashMap::default();
        inputs.insert(
            PortId::AudioInput0,
            vec![ModulationSource {
                buffer: &input,
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );
        inputs.insert(
            PortId::CombinedGate,
            vec![ModulationSource {
                buffer: &[1.0; BUFFER_SIZE],
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );

        let mut out_buf = vec![0.0; BUFFER_SIZE];
        let mut outputs: FxHashMap<PortId, &mut [f32]> = FxHashMap::default();
        outputs.insert(PortId::AudioOutput0, &mut out_buf[..]);

        let mut gate_off_buf = [0.0; BUFFER_SIZE];
        let mut gate_off_outputs: FxHashMap<PortId, &mut [f32]> = FxHashMap::default();
        gate_off_outputs.insert(PortId::AudioOutput0, &mut gate_off_buf[..]);
        node.process(&inputs, &mut gate_off_outputs, BUFFER_SIZE);

        node.process(&inputs, &mut outputs, BUFFER_SIZE);

        assert_eq!(out_buf, input);
    }

    #[test]
    fn glide_stops_sliding_when_gate_is_off() {
        let sample_rate = 10.0;
        let mut node = Glide::new(sample_rate, 0.5);
        const BUFFER_SIZE: usize = 4;

        // Block 1: initialize with gate on and steady input
        let mut inputs: FxHashMap<PortId, Vec<ModulationSource>> = FxHashMap::default();
        inputs.insert(
            PortId::AudioInput0,
            vec![ModulationSource {
                buffer: &[1.0; BUFFER_SIZE],
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );
        inputs.insert(
            PortId::CombinedGate,
            vec![ModulationSource {
                buffer: &[1.0; BUFFER_SIZE],
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );
        let mut out_buf = [0.0; BUFFER_SIZE];
        let mut outputs: FxHashMap<PortId, &mut [f32]> = FxHashMap::default();
        outputs.insert(PortId::AudioOutput0, &mut out_buf[..]);
        node.process(&inputs, &mut outputs, BUFFER_SIZE);

        // Block 2: gate still on, target drops to 0 -> should glide (not jump)
        let mut inputs2: FxHashMap<PortId, Vec<ModulationSource>> = FxHashMap::default();
        inputs2.insert(
            PortId::AudioInput0,
            vec![ModulationSource {
                buffer: &[0.0; BUFFER_SIZE],
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );
        inputs2.insert(
            PortId::CombinedGate,
            vec![ModulationSource {
                buffer: &[1.0; BUFFER_SIZE],
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );
        let mut out_buf2 = [0.0; BUFFER_SIZE];
        let mut outputs2: FxHashMap<PortId, &mut [f32]> = FxHashMap::default();
        outputs2.insert(PortId::AudioOutput0, &mut out_buf2[..]);
        node.process(&inputs2, &mut outputs2, BUFFER_SIZE);
        assert!(out_buf2[0] < 1.0 && out_buf2[0] > 0.0);

        // Block 3: gate off, new target 0.5 should be passed through without glide
        let mut inputs3: FxHashMap<PortId, Vec<ModulationSource>> = FxHashMap::default();
        inputs3.insert(
            PortId::AudioInput0,
            vec![ModulationSource {
                buffer: &[0.5; BUFFER_SIZE],
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );
        inputs3.insert(
            PortId::CombinedGate,
            vec![ModulationSource {
                buffer: &[0.0; BUFFER_SIZE],
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );
        let mut out_buf3 = vec![0.0; BUFFER_SIZE];
        let mut outputs3: FxHashMap<PortId, &mut [f32]> = FxHashMap::default();
        outputs3.insert(PortId::AudioOutput0, &mut out_buf3[..]);
        node.process(&inputs3, &mut outputs3, BUFFER_SIZE);

        for sample in out_buf3 {
            assert!((sample - 0.5).abs() < 1e-6);
        }
    }
}
