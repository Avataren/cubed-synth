use crate::traits::{Inputs, Outputs};
use cubed_simd::f32x4;
use std::any::Any;

use rustc_hash::FxHashMap;

use super::copy_input;
use crate::traits::{AudioNode, PortId, StereoEffect};

pub struct Delay {
    enabled: bool,
    delay_buffer_left: Vec<f32>,
    delay_buffer_right: Vec<f32>,
    write_index: usize,
    max_delay_samples: usize,
    delay_samples: usize, // current delay time in samples
    feedback: f32,
    mix: f32, // mix amount: 0.0 = fully dry, 1.0 = fully wet
    sample_rate: f32,
}

impl Delay {
    /// Creates a new Delay node.
    ///
    /// * `sample_rate` - The sample rate in Hz.
    /// * `max_delay_ms` - The maximum delay time in milliseconds.
    /// * `delay_ms` - The initial delay time in milliseconds (clamped to max_delay_ms).
    /// * `feedback` - The feedback coefficient (usually between 0.0 and 1.0).
    /// * `mix` - The mix amount (0.0 = fully dry, 1.0 = fully wet).
    pub fn new(
        sample_rate: f32,
        max_delay_ms: f32,
        delay_ms: f32,
        feedback: f32,
        mix: f32,
    ) -> Self {
        let max_delay_samples = ((max_delay_ms / 1000.0) * sample_rate).ceil() as usize;
        let delay_samples =
            (((delay_ms / 1000.0) * sample_rate).ceil() as usize).min(max_delay_samples);
        Self {
            enabled: true,
            delay_buffer_left: vec![0.0; max_delay_samples],
            delay_buffer_right: vec![0.0; max_delay_samples],
            write_index: 0,
            max_delay_samples,
            delay_samples,
            feedback,
            mix: mix.clamp(0.0, 1.0),
            sample_rate,
        }
    }

    /// Sets the delay time in milliseconds.
    pub fn set_delay_ms(&mut self, delay_ms: f32) {
        self.delay_samples =
            (((delay_ms / 1000.0) * self.sample_rate).ceil() as usize).min(self.max_delay_samples);
    }

    /// Sets the feedback amount.
    pub fn set_feedback(&mut self, feedback: f32) {
        self.feedback = feedback;
    }

    /// Sets the mix amount (0.0 = fully dry, 1.0 = fully wet).
    pub fn set_mix(&mut self, mix: f32) {
        self.mix = mix.clamp(0.0, 1.0);
    }
}

// If the modulation trait is no longer required, you can remove this implementation.
// impl ModulationProcessor for Delay {}

impl StereoEffect for Delay {
    fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let buffer_size = left.len().min(right.len());
        let mut i = 0;
        while i < buffer_size {
            let chunk_len = (buffer_size - i).min(4);

            // Load up to 4 samples from left and right inputs.
            let mut in_left_arr = [0.0; 4];
            let mut in_right_arr = [0.0; 4];
            in_left_arr[..chunk_len].copy_from_slice(&left[i..i + chunk_len]);
            in_right_arr[..chunk_len].copy_from_slice(&right[i..i + chunk_len]);
            let in_left_vec = f32x4::from_array(in_left_arr);
            let in_right_vec = f32x4::from_array(in_right_arr);

            // Calculate the base read index for the delay with wrap-around.
            let base = (self.write_index + self.max_delay_samples - self.delay_samples)
                % self.max_delay_samples;

            // Load delayed samples from the delay buffers (handling wrap-around).
            let (delayed_left_vec, delayed_right_vec) =
                if base + chunk_len <= self.max_delay_samples {
                    let mut left_delay_arr = [0.0; 4];
                    let mut right_delay_arr = [0.0; 4];
                    left_delay_arr[..chunk_len]
                        .copy_from_slice(&self.delay_buffer_left[base..base + chunk_len]);
                    right_delay_arr[..chunk_len]
                        .copy_from_slice(&self.delay_buffer_right[base..base + chunk_len]);
                    (
                        f32x4::from_array(left_delay_arr),
                        f32x4::from_array(right_delay_arr),
                    )
                } else {
                    let first_part = self.max_delay_samples - base;
                    let second_part = chunk_len - first_part;
                    let mut left_delay_arr = [0.0; 4];
                    let mut right_delay_arr = [0.0; 4];
                    left_delay_arr[..first_part]
                        .copy_from_slice(&self.delay_buffer_left[base..self.max_delay_samples]);
                    left_delay_arr[first_part..chunk_len]
                        .copy_from_slice(&self.delay_buffer_left[0..second_part]);
                    right_delay_arr[..first_part]
                        .copy_from_slice(&self.delay_buffer_right[base..self.max_delay_samples]);
                    right_delay_arr[first_part..chunk_len]
                        .copy_from_slice(&self.delay_buffer_right[0..second_part]);
                    (
                        f32x4::from_array(left_delay_arr),
                        f32x4::from_array(right_delay_arr),
                    )
                };

            // Compute new samples: new_sample = input + (delayed * feedback)
            let fb_vec = f32x4::splat(self.feedback);
            let new_left_vec = in_left_vec + delayed_left_vec * fb_vec;
            let new_right_vec = in_right_vec + delayed_right_vec * fb_vec;

            // Write new samples into the delay buffers (handling wrap-around).
            if self.write_index + chunk_len <= self.max_delay_samples {
                self.delay_buffer_left[self.write_index..self.write_index + chunk_len]
                    .copy_from_slice(&new_left_vec.to_array()[..chunk_len]);
                self.delay_buffer_right[self.write_index..self.write_index + chunk_len]
                    .copy_from_slice(&new_right_vec.to_array()[..chunk_len]);
            } else {
                let first_part = self.max_delay_samples - self.write_index;
                let second_part = chunk_len - first_part;
                self.delay_buffer_left[self.write_index..self.max_delay_samples]
                    .copy_from_slice(&new_left_vec.to_array()[..first_part]);
                self.delay_buffer_left[0..second_part]
                    .copy_from_slice(&new_left_vec.to_array()[first_part..chunk_len]);
                self.delay_buffer_right[self.write_index..self.max_delay_samples]
                    .copy_from_slice(&new_right_vec.to_array()[..first_part]);
                self.delay_buffer_right[0..second_part]
                    .copy_from_slice(&new_right_vec.to_array()[first_part..chunk_len]);
            }

            // Calculate mix levels.
            // dry_level = 1.0 - mix, wet_level = mix
            let dry_level = f32x4::splat(1.0 - self.mix);
            let wet_level = f32x4::splat(self.mix);

            // Mix the dry (original) and wet (delayed) signals.
            let mixed_left_vec = in_left_vec * dry_level + delayed_left_vec * wet_level;
            let mixed_right_vec = in_right_vec * dry_level + delayed_right_vec * wet_level;
            let mixed_left_arr = mixed_left_vec.to_array();
            let mixed_right_arr = mixed_right_vec.to_array();
            left[i..i + chunk_len].copy_from_slice(&mixed_left_arr[..chunk_len]);
            right[i..i + chunk_len].copy_from_slice(&mixed_right_arr[..chunk_len]);

            // Advance the write pointer and the processing index.
            self.write_index = (self.write_index + chunk_len) % self.max_delay_samples;
            i += chunk_len;
        }
    }
}

impl AudioNode for Delay {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        let mut ports = FxHashMap::default();
        // Stereo inputs:
        ports.insert(PortId::AudioInput0, false); // Left input
        ports.insert(PortId::AudioInput1, false); // Right input

        // Stereo outputs:
        ports.insert(PortId::AudioOutput0, true); // Left output
        ports.insert(PortId::AudioOutput1, true); // Right output

        ports
    }

    fn process<'a>(&mut self, inputs: &Inputs<'a>, outputs: &mut Outputs<'_>, buffer_size: usize) {
        // Instead of calling process_modulations we directly take the audio input from the
        // first modulation source for each port.
        // (It is assumed that each input port always provides at least one source.)
        let left_in = inputs
            .get(&PortId::AudioInput0)
            .and_then(|sources| sources.first())
            .map(|src| &src.buffer[..buffer_size])
            .unwrap_or_else(|| {
                // In a real-time context, try to avoid heap allocation on every call.
                // Here we create a temporary zero-buffer if needed.
                static ZERO_BUFFER: [f32; 1024] = [0.0; 1024];
                &ZERO_BUFFER[..buffer_size.min(ZERO_BUFFER.len())]
            });

        let right_in = inputs
            .get(&PortId::AudioInput1)
            .and_then(|sources| sources.first())
            .map(|src| &src.buffer[..buffer_size])
            .unwrap_or_else(|| {
                static ZERO_BUFFER: [f32; 1024] = [0.0; 1024];
                &ZERO_BUFFER[..buffer_size.min(ZERO_BUFFER.len())]
            });

        // Use get_disjoint_mut to retrieve both outputs at once.
        let outs = outputs.get_disjoint_mut([&PortId::AudioOutput0, &PortId::AudioOutput1]);
        let [Some(out_left), Some(out_right)] = outs else {
            panic!("Missing stereo output buffers");
        };

        let out_left = &mut out_left[..buffer_size];
        let out_right = &mut out_right[..buffer_size];
        copy_input(out_left, left_in);
        copy_input(out_right, right_in);
        self.process_stereo(out_left, out_right);
    }

    fn as_stereo_effect(&mut self) -> Option<&mut dyn StereoEffect> {
        Some(self)
    }

    fn reset(&mut self) {
        self.delay_buffer_left.fill(0.0);
        self.delay_buffer_right.fill(0.0);
        self.write_index = 0;
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
        if active {
            self.reset();
        }
    }

    fn name(&self) -> &'static str {
        "Delay"
    }

    fn node_type(&self) -> &str {
        "delay"
    }
}
