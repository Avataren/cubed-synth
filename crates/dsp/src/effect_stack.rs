use rustc_hash::FxHashMap;

use crate::{
    graph::{ModulationSource, ModulationTransformation, ModulationType},
    AudioNode, PortId,
};

pub struct Effect {
    pub node: Box<dyn AudioNode>,
}

pub struct EffectStack {
    pub effects: Vec<Effect>,
    work_left_a: Vec<f32>,
    work_right_a: Vec<f32>,
    work_left_b: Vec<f32>,
    work_right_b: Vec<f32>,
}

impl EffectStack {
    pub fn new(_buffer_size: usize) -> Self {
        Self {
            effects: Vec::new(),
            work_left_a: Vec::new(),
            work_right_a: Vec::new(),
            work_left_b: Vec::new(),
            work_right_b: Vec::new(),
        }
    }

    fn ensure_capacity(&mut self, len: usize) {
        if self.work_left_a.len() < len {
            self.work_left_a.resize(len, 0.0);
        }
        if self.work_right_a.len() < len {
            self.work_right_a.resize(len, 0.0);
        }
        if self.work_left_b.len() < len {
            self.work_left_b.resize(len, 0.0);
        }
        if self.work_right_b.len() < len {
            self.work_right_b.resize(len, 0.0);
        }
    }

    pub fn add_effect(&mut self, effect: Box<dyn AudioNode>) -> usize {
        let index = self.effects.len();
        self.effects.push(Effect { node: effect });
        index
    }

    pub fn remove_effect(&mut self, index: usize) {
        if index < self.effects.len() {
            self.effects.remove(index);
        }
    }

    pub fn reorder_effects(&mut self, from: usize, to: usize) {
        if from < self.effects.len() && to < self.effects.len() {
            let effect = self.effects.remove(from);
            self.effects.insert(to, effect);
        }
    }

    pub fn set_effect_enabled(&mut self, index: usize, enabled: bool) {
        if let Some(effect) = self.effects.get_mut(index) {
            effect.node.set_active(enabled);
        }
    }

    pub fn get_effect_count(&self) -> usize {
        self.effects.len()
    }

    pub fn process_audio(
        &mut self,
        input_left: &[f32],
        input_right: &[f32],
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        // FIXED: Use actual buffer lengths instead of self.buffer_size
        let actual_buffer_size = input_left
            .len()
            .min(input_right.len())
            .min(output_left.len())
            .min(output_right.len());

        if actual_buffer_size == 0 {
            return;
        }

        // If all effects are disabled, bypass processing entirely.
        if self.effects.iter().all(|e| !e.node.is_active()) {
            output_left[..actual_buffer_size].copy_from_slice(&input_left[..actual_buffer_size]);
            output_right[..actual_buffer_size].copy_from_slice(&input_right[..actual_buffer_size]);
            return;
        }

        if self.effects.iter().all(|e| !e.node.is_active()) {
            output_left[..actual_buffer_size].copy_from_slice(&input_left[..actual_buffer_size]);
            output_right[..actual_buffer_size].copy_from_slice(&input_right[..actual_buffer_size]);
            return;
        }

        self.ensure_capacity(actual_buffer_size);

        self.work_left_a[..actual_buffer_size].copy_from_slice(&input_left[..actual_buffer_size]);
        self.work_right_a[..actual_buffer_size].copy_from_slice(&input_right[..actual_buffer_size]);

        let mut current_is_a = true;
        let mut had_active_effect = false;

        for effect in &mut self.effects {
            if !effect.node.is_active() {
                continue;
            }
            had_active_effect = true;

            let (current_left, current_right, next_left, next_right) = if current_is_a {
                (
                    &mut self.work_left_a,
                    &mut self.work_right_a,
                    &mut self.work_left_b,
                    &mut self.work_right_b,
                )
            } else {
                (
                    &mut self.work_left_b,
                    &mut self.work_right_b,
                    &mut self.work_left_a,
                    &mut self.work_right_a,
                )
            };

            next_left[..actual_buffer_size].fill(0.0);
            next_right[..actual_buffer_size].fill(0.0);

            // Zero-copy: Create ModulationSource with references to current buffers
            let mut inputs = FxHashMap::with_capacity_and_hasher(2, Default::default());

            let left_source = ModulationSource {
                buffer: &current_left[..actual_buffer_size],
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            };

            let right_source = ModulationSource {
                buffer: &current_right[..actual_buffer_size],
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            };

            inputs.insert(PortId::AudioInput0, vec![left_source]);
            inputs.insert(PortId::AudioInput1, vec![right_source]);

            let mut outputs = FxHashMap::with_capacity_and_hasher(2, Default::default());
            outputs.insert(PortId::AudioOutput0, &mut next_left[..actual_buffer_size]);
            outputs.insert(PortId::AudioOutput1, &mut next_right[..actual_buffer_size]);

            effect
                .node
                .process(&inputs, &mut outputs, actual_buffer_size);

            // No need to store ModulationSource instances - they're created on the fly
            current_is_a = !current_is_a;
        }

        let (final_left, final_right) = if had_active_effect {
            if current_is_a {
                (&self.work_left_a, &self.work_right_a)
            } else {
                (&self.work_left_b, &self.work_right_b)
            }
        } else {
            (&self.work_left_a, &self.work_right_a)
        };

        output_left[..actual_buffer_size].copy_from_slice(&final_left[..actual_buffer_size]);
        output_right[..actual_buffer_size].copy_from_slice(&final_right[..actual_buffer_size]);

        // CRITICAL: Zero out any remaining samples to prevent garbage data
        if actual_buffer_size < output_left.len() {
            output_left[actual_buffer_size..].fill(0.0);
        }
        if actual_buffer_size < output_right.len() {
            output_right[actual_buffer_size..].fill(0.0);
        }
    }
}
