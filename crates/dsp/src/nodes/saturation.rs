use cubed_simd::f32x4;
use std::any::Any;

use rustc_hash::FxHashMap;

use crate::graph::ModulationSource;
use crate::traits::{AudioNode, PortId};

/// A saturation node implementing soft clipping using tanh.
pub struct Saturation {
    enabled: bool,
    drive: f32, // Determines the amount of saturation. Higher values result in more saturation.
    mix: f32,   // Mix amount: 0.0 = fully dry, 1.0 = fully saturated (wet)
}

impl Saturation {
    /// Creates a new Saturation node.
    ///
    /// * `drive` - The drive amount for the saturation effect.
    /// * `mix` - The mix amount (0.0 = fully dry, 1.0 = fully saturated).
    pub fn new(drive: f32, mix: f32) -> Self {
        Self {
            enabled: true,
            drive,
            mix: mix.clamp(0.0, 1.0),
        }
    }

    /// Sets the drive amount.
    pub fn set_drive(&mut self, drive: f32) {
        self.drive = drive;
    }

    /// Sets the mix amount (0.0 = fully dry, 1.0 = fully saturated).
    pub fn set_mix(&mut self, mix: f32) {
        self.mix = mix.clamp(0.0, 1.0);
    }
}

impl AudioNode for Saturation {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        let mut ports = FxHashMap::default();
        // Stereo inputs (each a vector of ModulationSource):
        ports.insert(PortId::AudioInput0, false); // Left input
        ports.insert(PortId::AudioInput1, false); // Right input

        // Stereo outputs:
        ports.insert(PortId::AudioOutput0, true); // Left output
        ports.insert(PortId::AudioOutput1, true); // Right output

        ports
    }

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        // Extract the input buffers from the first modulation source for each channel.
        let left_in = inputs.get(&PortId::AudioInput0).unwrap()[0].buffer;
        let right_in = inputs.get(&PortId::AudioInput1).unwrap()[0].buffer;

        // Retrieve output buffers using nightly's get_disjoint_mut.
        let outs = outputs.get_disjoint_mut([&PortId::AudioOutput0, &PortId::AudioOutput1]);
        let [Some(out_left), Some(out_right)] = outs else {
            panic!("Missing stereo output buffers");
        };
        let out_left: &mut [f32] = out_left;
        let out_right: &mut [f32] = out_right;

        // Avoid division by zero: if drive is nearly zero, clamp it.
        let drive = if self.drive.abs() < 0.0001 {
            0.0001
        } else {
            self.drive
        };
        // Normalization factor to keep the output within -1.0 to 1.0.
        let norm = drive.tanh();

        // Precompute dry/wet levels.
        let dry_level = f32x4::splat(1.0 - self.mix);
        let wet_level = f32x4::splat(self.mix);

        // Process the buffer in chunks of 4 samples (SIMD width).
        let mut i = 0;
        while i < buffer_size {
            let chunk_len = (buffer_size - i).min(4);

            // Load up to 4 samples from left and right inputs.
            let mut in_left_arr = [0.0; 4];
            let mut in_right_arr = [0.0; 4];
            in_left_arr[..chunk_len].copy_from_slice(&left_in[i..i + chunk_len]);
            in_right_arr[..chunk_len].copy_from_slice(&right_in[i..i + chunk_len]);
            let in_left_vec = f32x4::from_array(in_left_arr);
            let in_right_vec = f32x4::from_array(in_right_arr);

            // Apply soft clipping per sample.
            let mut sat_left_arr = [0.0; 4];
            let mut sat_right_arr = [0.0; 4];
            for j in 0..chunk_len {
                // Soft saturation: y = tanh(x * drive) / tanh(drive)
                sat_left_arr[j] = ((in_left_arr[j] * drive).tanh()) / norm;
                sat_right_arr[j] = ((in_right_arr[j] * drive).tanh()) / norm;
            }
            let sat_left_vec = f32x4::from_array(sat_left_arr);
            let sat_right_vec = f32x4::from_array(sat_right_arr);

            // Mix the dry (original) and wet (saturated) signals.
            let mixed_left_vec = in_left_vec * dry_level + sat_left_vec * wet_level;
            let mixed_right_vec = in_right_vec * dry_level + sat_right_vec * wet_level;
            let mixed_left_arr = mixed_left_vec.to_array();
            let mixed_right_arr = mixed_right_vec.to_array();
            out_left[i..i + chunk_len].copy_from_slice(&mixed_left_arr[..chunk_len]);
            out_right[i..i + chunk_len].copy_from_slice(&mixed_right_arr[..chunk_len]);

            i += chunk_len;
        }
    }

    fn reset(&mut self) {
        // No internal buffers to reset.
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
        "Saturation"
    }

    fn node_type(&self) -> &str {
        "saturation"
    }
}
