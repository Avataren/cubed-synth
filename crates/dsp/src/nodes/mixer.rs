use cubed_simd::num::SimdFloat;
use cubed_simd::{Simd, StdFloat};
use std::any::Any; // Import Simd explicitly, StdFloat for sqrt

use rustc_hash::FxHashMap;

// Import necessary types
use crate::graph::{ModulationProcessor, ModulationSource};
use crate::traits::{AudioNode, PortId};

/// A simple stereo mixer node with gain and panning control.
/// It takes a mono audio input and applies gain and panning to produce stereo output.
pub struct Mixer {
    enabled: bool, // From AudioNode trait
    // base_gain: f32,
    // base_pan: f32,

    // === Scratch Buffers ===
    mod_scratch_add: Vec<f32>,
    mod_scratch_mult: Vec<f32>,
    audio_in_buffer: Vec<f32>,
    scratch_gain_add: Vec<f32>,
    scratch_gain_mult: Vec<f32>,
    scratch_pan_add: Vec<f32>,
    scratch_pan_mult: Vec<f32>,

    // === Temporary Output Buffers ===
    temp_out_l: Vec<f32>,
    temp_out_r: Vec<f32>,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    pub fn new() -> Self {
        let initial_capacity = 128;
        Self {
            enabled: true,
            // base_gain: 1.0,
            // base_pan: 0.0,
            mod_scratch_add: vec![0.0; initial_capacity],
            mod_scratch_mult: vec![1.0; initial_capacity],
            audio_in_buffer: vec![0.0; initial_capacity],
            scratch_gain_add: vec![0.0; initial_capacity],
            scratch_gain_mult: vec![1.0; initial_capacity],
            scratch_pan_add: vec![0.0; initial_capacity],
            scratch_pan_mult: vec![1.0; initial_capacity],

            // Initialize temporary output buffers
            temp_out_l: vec![0.0; initial_capacity],
            temp_out_r: vec![0.0; initial_capacity],
        }
    }

    /// Ensure all scratch buffers and temp output buffers have at least `size` capacity.
    fn ensure_buffers(&mut self, size: usize) {
        let resize_if_needed = |buf: &mut Vec<f32>, default_val: f32| {
            if buf.len() < size {
                buf.resize(size, default_val);
            }
        };
        resize_if_needed(&mut self.mod_scratch_add, 0.0);
        resize_if_needed(&mut self.mod_scratch_mult, 1.0);
        resize_if_needed(&mut self.audio_in_buffer, 0.0);
        resize_if_needed(&mut self.scratch_gain_add, 0.0);
        resize_if_needed(&mut self.scratch_gain_mult, 1.0);
        resize_if_needed(&mut self.scratch_pan_add, 0.0);
        resize_if_needed(&mut self.scratch_pan_mult, 1.0);
        // Resize temporary output buffers as well
        resize_if_needed(&mut self.temp_out_l, 0.0);
        resize_if_needed(&mut self.temp_out_r, 0.0);
    }

    // --- Optional Parameter Setters ---
    // pub fn set_base_gain(&mut self, gain: f32) { self.base_gain = gain.max(0.0); }
    // pub fn set_base_pan(&mut self, pan: f32) { self.base_pan = pan.clamp(-1.0, 1.0); }
}

// Implement the modulation processor trait
impl ModulationProcessor for Mixer {}

impl AudioNode for Mixer {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        [
            (PortId::AudioInput0, false), // Mono audio input
            (PortId::GainMod, false),     // Modulation for gain
            (PortId::StereoPan, false),   // Modulation for pan
            (PortId::AudioOutput0, true), // Left audio output
            (PortId::AudioOutput1, true), // Right audio output
        ]
        .iter()
        .cloned()
        .collect()
    }

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>, // Still takes mutable FxHashMap
        buffer_size: usize,
    ) {
        // --- 0) Early exit and Buffer Preparation ---
        if !self.enabled {
            // Important: Still need to potentially zero output buffers if they exist
            if let Some(out_l) = outputs.get_mut(&PortId::AudioOutput0) {
                out_l[..buffer_size].fill(0.0);
            }
            if let Some(out_r) = outputs.get_mut(&PortId::AudioOutput1) {
                out_r[..buffer_size].fill(0.0);
            }
            return;
        }

        // Ensure all internal buffers are sized correctly
        self.ensure_buffers(buffer_size);

        // Check if there's anywhere to write the final output
        let has_output_l = outputs.contains_key(&PortId::AudioOutput0);
        let has_output_r = outputs.contains_key(&PortId::AudioOutput1);
        if !has_output_l && !has_output_r {
            return; // Nothing to do if no output buffers requested
        }

        // --- 1) Process Inputs ---
        // (Input processing remains the same)
        self.audio_in_buffer[..buffer_size].fill(0.0);
        if let Some(audio_sources) = inputs.get(&PortId::AudioInput0) {
            for source in audio_sources {
                Self::apply_add(
                    source.buffer,
                    &mut self.audio_in_buffer[..buffer_size],
                    source.amount,
                    source.transformation,
                );
            }
        }

        let mut process_mod_input = |port_id: PortId,
                                     target_add: &mut [f32],
                                     target_mult: &mut [f32],
                                     default_add: f32,
                                     default_mult: f32| {
            let sources = inputs.get(&port_id);
            if sources.is_some_and(|s| !s.is_empty()) {
                Self::accumulate_modulations_inplace(
                    buffer_size,
                    sources.map(|v| v.as_slice()),
                    &mut self.mod_scratch_add,
                    &mut self.mod_scratch_mult,
                );
                target_add[..buffer_size].copy_from_slice(&self.mod_scratch_add[..buffer_size]);
                target_mult[..buffer_size].copy_from_slice(&self.mod_scratch_mult[..buffer_size]);
            } else {
                target_add[..buffer_size].fill(default_add);
                target_mult[..buffer_size].fill(default_mult);
            }
        };

        let base_gain = 1.0;
        let base_pan = 0.0;
        process_mod_input(
            PortId::GainMod,
            &mut self.scratch_gain_add,
            &mut self.scratch_gain_mult,
            0.0,
            1.0,
        );
        process_mod_input(
            PortId::StereoPan,
            &mut self.scratch_pan_add,
            &mut self.scratch_pan_mult,
            0.5, // Default to center (0.5) when no pan modulation is connected
            1.0,
        );

        // --- 2) Apply Gain and Panning (SIMD) -> Into Temporary Buffers ---
        const LANES: usize = 4;
        type Vf32 = Simd<f32, LANES>;

        let zero_simd = Vf32::splat(0.0);
        let one_simd = Vf32::splat(1.0);

        let chunks = buffer_size / LANES;
        for i in 0..chunks {
            let offset = i * LANES;

            // --- Load/Calculate values (same as before) ---
            let audio_in_simd = Vf32::from_slice(&self.audio_in_buffer[offset..offset + LANES]);
            let gain_add_simd = Vf32::from_slice(&self.scratch_gain_add[offset..offset + LANES]);
            let gain_mult_simd = Vf32::from_slice(&self.scratch_gain_mult[offset..offset + LANES]);
            let pan_add_simd = Vf32::from_slice(&self.scratch_pan_add[offset..offset + LANES]);
            let pan_mult_simd = Vf32::from_slice(&self.scratch_pan_mult[offset..offset + LANES]);

            let effective_gain_simd = (Vf32::splat(base_gain) + gain_add_simd) * gain_mult_simd;
            let gain_applied_input = audio_in_simd * effective_gain_simd.simd_max(zero_simd);

            // Treat StereoPan as 0..1 where 0 = left, 0.5 = center, 1 = right.
            let effective_pan_simd = (Vf32::splat(base_pan) + pan_add_simd) * pan_mult_simd;
            let clamped_pan_simd = effective_pan_simd.simd_clamp(zero_simd, one_simd);
            let gain_r_simd = clamped_pan_simd.sqrt();
            let gain_l_simd = (one_simd - clamped_pan_simd).sqrt();

            let output_l_simd = gain_applied_input * gain_l_simd;
            let output_r_simd = gain_applied_input * gain_r_simd;

            // --- Store results into temporary buffers ---
            output_l_simd.copy_to_slice(&mut self.temp_out_l[offset..offset + LANES]);
            output_r_simd.copy_to_slice(&mut self.temp_out_r[offset..offset + LANES]);
        } // End of SIMD loop

        // --- 3) Scalar Remainder -> Into Temporary Buffers ---
        let remainder_start = chunks * LANES;
        for i in remainder_start..buffer_size {
            let audio_in = self.audio_in_buffer[i];
            let gain_add = self.scratch_gain_add[i];
            let gain_mult = self.scratch_gain_mult[i];
            let pan_add = self.scratch_pan_add[i];
            let pan_mult = self.scratch_pan_mult[i];

            let effective_gain = (base_gain + gain_add) * gain_mult;
            let gain_applied_input = audio_in * effective_gain.max(0.0);

            // StereoPan interpreted as 0..1: 0 = left, 0.5 = center, 1 = right.
            let effective_pan = (base_pan + pan_add) * pan_mult;
            let clamped_pan = effective_pan.clamp(0.0, 1.0);
            let gain_r = clamped_pan.sqrt();
            let gain_l = (1.0 - clamped_pan).sqrt();

            // Write to temporary buffers
            self.temp_out_l[i] = gain_applied_input * gain_l;
            self.temp_out_r[i] = gain_applied_input * gain_r;
        } // End of scalar loop

        // --- 4) Copy Temporary Buffers to Actual Outputs ---
        // Now we borrow outputs mutably one at a time.
        if let Some(out_l) = outputs.get_mut(&PortId::AudioOutput0) {
            out_l[..buffer_size].copy_from_slice(&self.temp_out_l[..buffer_size]);
        }
        if let Some(out_r) = outputs.get_mut(&PortId::AudioOutput1) {
            out_r[..buffer_size].copy_from_slice(&self.temp_out_r[..buffer_size]);
        }
    } // End of process fn

    fn reset(&mut self) {
        // No internal state requiring reset
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
    }

    fn name(&self) -> &'static str {
        "Mixer"
    }

    fn node_type(&self) -> &str {
        "mixer"
    }
}
