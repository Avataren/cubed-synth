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
    work_left: Vec<f32>,
    work_right: Vec<f32>,
}

impl EffectStack {
    pub fn new(_buffer_size: usize) -> Self {
        Self {
            effects: Vec::new(),
            work_left: Vec::new(),
            work_right: Vec::new(),
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

    /// Runs the input through every active effect in order. Stereo effects
    /// process the output buffers in place; any other node goes through its
    /// port-map `process`, which allocates.
    pub fn process_audio(
        &mut self,
        input_left: &[f32],
        input_right: &[f32],
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        let actual_buffer_size = input_left
            .len()
            .min(input_right.len())
            .min(output_left.len())
            .min(output_right.len());

        if actual_buffer_size == 0 {
            return;
        }

        let (out_left, left_tail) = output_left.split_at_mut(actual_buffer_size);
        let (out_right, right_tail) = output_right.split_at_mut(actual_buffer_size);
        out_left.copy_from_slice(&input_left[..actual_buffer_size]);
        out_right.copy_from_slice(&input_right[..actual_buffer_size]);

        for effect in &mut self.effects {
            if !effect.node.is_active() {
                continue;
            }
            if let Some(stereo) = effect.node.as_stereo_effect() {
                stereo.process_stereo(out_left, out_right);
            } else {
                Self::process_ports(
                    effect.node.as_mut(),
                    &mut self.work_left,
                    &mut self.work_right,
                    out_left,
                    out_right,
                );
            }
        }

        // CRITICAL: Zero out any remaining samples to prevent garbage data
        left_tail.fill(0.0);
        right_tail.fill(0.0);
    }

    /// Runs a node that is not a `StereoEffect` through its port maps, using
    /// the work buffers as its input.
    fn process_ports(
        node: &mut dyn AudioNode,
        work_left: &mut Vec<f32>,
        work_right: &mut Vec<f32>,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let len = left.len();
        work_left.clear();
        work_left.extend_from_slice(left);
        work_right.clear();
        work_right.extend_from_slice(right);
        left.fill(0.0);
        right.fill(0.0);

        let source = |buffer| ModulationSource {
            buffer,
            amount: 1.0,
            mod_type: ModulationType::Additive,
            transformation: ModulationTransformation::None,
        };
        let mut inputs = FxHashMap::with_capacity_and_hasher(2, Default::default());
        inputs.insert(PortId::AudioInput0, vec![source(&work_left[..])]);
        inputs.insert(PortId::AudioInput1, vec![source(&work_right[..])]);

        let mut outputs = FxHashMap::with_capacity_and_hasher(2, Default::default());
        outputs.insert(PortId::AudioOutput0, left);
        outputs.insert(PortId::AudioOutput1, right);

        node.process(&inputs, &mut outputs, len);
    }
}
