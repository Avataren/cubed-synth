use std::any::Any;

use rustc_hash::FxHashMap;

use crate::{
    graph::ModulationSource,
    traits::{AudioNode, PortId},
};

/// Simple stereo bitcrusher with sample-rate reduction.
pub struct Bitcrusher {
    enabled: bool,
    bits: u8,
    downsample_factor: usize,
    mix: f32,
    held_left: f32,
    held_right: f32,
    sample_hold_phase: usize,
}

impl Bitcrusher {
    pub fn new(bits: u8, downsample_factor: usize, mix: f32) -> Self {
        let mut crusher = Self {
            enabled: true,
            bits: 0,
            downsample_factor: 1,
            mix: 0.5,
            held_left: 0.0,
            held_right: 0.0,
            sample_hold_phase: 0,
        };
        crusher.set_bits(bits);
        crusher.set_downsample_factor(downsample_factor);
        crusher.set_mix(mix);
        crusher
    }

    pub fn set_bits(&mut self, bits: u8) {
        // Clamp to a sane range to avoid overflow and NaNs in step calculation.
        let clamped = bits.clamp(1, 24);
        self.bits = clamped;
    }

    pub fn set_downsample_factor(&mut self, factor: usize) {
        self.downsample_factor = factor.max(1);
    }

    pub fn set_mix(&mut self, mix: f32) {
        self.mix = mix.clamp(0.0, 1.0);
    }

    fn quantize(sample: f32, step: f32) -> f32 {
        // Map [-1, 1] into quantized steps then return to [-1, 1]
        let normalized = ((sample + 1.0) / step).round();
        let quantized = normalized * step - 1.0;
        quantized.clamp(-1.0, 1.0)
    }
}

impl AudioNode for Bitcrusher {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        let mut ports = FxHashMap::default();
        ports.insert(PortId::AudioInput0, false);
        ports.insert(PortId::AudioInput1, false);
        ports.insert(PortId::AudioOutput0, true);
        ports.insert(PortId::AudioOutput1, true);
        ports
    }

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        let left_in = inputs.get(&PortId::AudioInput0).unwrap()[0].buffer;
        let right_in = inputs.get(&PortId::AudioInput1).unwrap()[0].buffer;

        let outs = outputs.get_disjoint_mut([&PortId::AudioOutput0, &PortId::AudioOutput1]);
        let [Some(out_left), Some(out_right)] = outs else {
            panic!("Missing stereo output buffers for Bitcrusher");
        };
        let out_left: &mut [f32] = out_left;
        let out_right: &mut [f32] = out_right;

        let bits = self.bits.clamp(1, 24);
        let levels = 1u32.checked_shl(bits as u32).unwrap_or(0).max(2) as f32;
        let step = 2.0 / (levels - 1.0);
        let factor = self.downsample_factor.max(1);
        let dry_gain = 1.0 - self.mix;
        let wet_gain = self.mix;

        let mut phase = if factor == 1 {
            0
        } else {
            self.sample_hold_phase % factor
        };

        for i in 0..buffer_size {
            if phase == 0 {
                self.held_left = Self::quantize(left_in[i], step);
                self.held_right = Self::quantize(right_in[i], step);
            }

            out_left[i] = left_in[i] * dry_gain + self.held_left * wet_gain;
            out_right[i] = right_in[i] * dry_gain + self.held_right * wet_gain;

            if factor > 1 {
                phase += 1;
                if phase >= factor {
                    phase = 0;
                }
            }
        }

        if factor > 1 {
            self.sample_hold_phase = phase;
        } else {
            self.sample_hold_phase = 0;
        }
    }

    fn reset(&mut self) {
        self.sample_hold_phase = 0;
        self.held_left = 0.0;
        self.held_right = 0.0;
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn is_active(&self) -> bool {
        self.enabled
    }

    fn set_active(&mut self, active: bool) {
        self.enabled = active;
        if !active {
            self.reset();
        }
    }

    fn name(&self) -> &'static str {
        "Bitcrusher"
    }

    fn node_type(&self) -> &str {
        "bitcrusher"
    }
}
