use cubed_simd::Simd;
use cubed_simd::StdFloat;
use std::any::Any;

use rustc_hash::FxHashMap;

// Import necessary types
use crate::graph::{ModulationProcessor, ModulationSource};
use crate::{AudioNode, PortId};

/// GlobalFrequencyNode encapsulates the base frequency buffer and applies a detune factor.
/// The base frequency can be updated externally.
/// The detune parameter (in cents) is modulated via the PortId::DetuneMod input.
/// Additive modulation on DetuneMod is interpreted in *semitones*.
pub struct GlobalFrequencyNode {
    base_frequency: Vec<f32>,
    /// Base detune value in cents.
    detune: f32,

    // === Scratch Buffers ===
    mod_scratch_add: Vec<f32>,
    mod_scratch_mult: Vec<f32>,
    // Final modulation results per sample
    scratch_detune_add_semitones: Vec<f32>,
    scratch_detune_mult: Vec<f32>,
}

impl GlobalFrequencyNode {
    /// Creates a new GlobalFrequencyNode with the given initial frequency and buffer size.
    pub fn new(initial_freq: f32, buffer_size: usize) -> Self {
        Self {
            base_frequency: vec![initial_freq; buffer_size],
            detune: 0.0,
            // Initialize scratch buffers
            mod_scratch_add: vec![0.0; buffer_size],
            mod_scratch_mult: vec![1.0; buffer_size],
            scratch_detune_add_semitones: vec![0.0; buffer_size],
            scratch_detune_mult: vec![1.0; buffer_size],
        }
    }

    /// Ensure all scratch buffers have at least `size` capacity.
    fn ensure_scratch_buffers(&mut self, size: usize) {
        let resize_if_needed = |buf: &mut Vec<f32>, default_val: f32| {
            if buf.len() < size {
                buf.resize(size, default_val);
            }
        };
        // Check base frequency buffer size as well
        if self.base_frequency.len() < size {
            // If resizing base_frequency, decide how to fill new elements (e.g., last value)
            let last_val = self.base_frequency.last().cloned().unwrap_or(440.0);
            self.base_frequency.resize(size, last_val);
        }

        resize_if_needed(&mut self.mod_scratch_add, 0.0);
        resize_if_needed(&mut self.mod_scratch_mult, 1.0);
        resize_if_needed(&mut self.scratch_detune_add_semitones, 0.0);
        resize_if_needed(&mut self.scratch_detune_mult, 1.0);
    }

    /// Sets the base (static) detune parameter in cents.
    pub fn set_detune(&mut self, detune: f32) {
        self.detune = detune;
    }

    /// Updates the base frequency buffer (e.g., from host).
    pub fn set_base_frequency(&mut self, freq: &[f32]) {
        let current_len = self.base_frequency.len();
        if freq.is_empty() {
            // Maybe log a warning or keep current values? For now, do nothing.
            return;
        }

        if freq.len() == 1 {
            // Fill entire buffer with the single value
            self.base_frequency.fill(freq[0]);
        } else if freq.len() >= current_len {
            // If input is same size or larger, copy the relevant part
            self.base_frequency.copy_from_slice(&freq[..current_len]);
        } else {
            // If input is smaller, copy what's provided and fill rest with last value
            self.base_frequency[..freq.len()].copy_from_slice(freq);
            let last_val = freq[freq.len() - 1];
            self.base_frequency[freq.len()..].fill(last_val);
        }
    }
}

impl AudioNode for GlobalFrequencyNode {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        [
            (PortId::GlobalFrequency, true), // Output: The calculated frequency signal
            (PortId::DetuneMod, false),      // Input: Modulation for detune (additive in semitones)
        ]
        .iter()
        .cloned()
        .collect()
    }

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        // --- 0) Buffer Prep ---
        self.ensure_scratch_buffers(buffer_size);

        let output = match outputs.get_mut(&PortId::GlobalFrequency) {
            Some(buf) => buf,
            None => return, // No output requested
        };

        // --- 1) Process Detune Modulation ---
        let detune_mod_sources = inputs.get(&PortId::DetuneMod);
        if detune_mod_sources.is_some_and(|s| !s.is_empty()) {
            // Accumulate modulation into shared scratch buffers
            Self::accumulate_modulations_inplace(
                buffer_size,
                detune_mod_sources.map(|v| v.as_slice()),
                &mut self.mod_scratch_add, // Holds additive part (semitones)
                &mut self.mod_scratch_mult, // Holds multiplicative part (factor)
            );
            // Copy results to dedicated scratch buffers. No base value combination needed here,
            // as the modulation inputs start from 0 (add) and 1 (mult).
            self.scratch_detune_add_semitones[..buffer_size]
                .copy_from_slice(&self.mod_scratch_add[..buffer_size]);
            self.scratch_detune_mult[..buffer_size]
                .copy_from_slice(&self.mod_scratch_mult[..buffer_size]);
        } else {
            // No modulation, set defaults
            self.scratch_detune_add_semitones[..buffer_size].fill(0.0);
            self.scratch_detune_mult[..buffer_size].fill(1.0);
        }

        // --- 2) Calculate Output Frequency (SIMD) ---
        const LANES: usize = 4; // Use f32x4
        type Vf32 = Simd<f32, LANES>;

        // Pre-splat constants
        let static_detune_cents_simd = Vf32::splat(self.detune);
        let cents_per_semitone_simd = Vf32::splat(100.0);
        let inv_cents_per_octave_simd = Vf32::splat(1.0 / 1200.0);

        let chunks = buffer_size / LANES;
        for i in 0..chunks {
            let offset = i * LANES;

            // Load base frequency and modulation values for the chunk
            let base_freq_simd = Vf32::from_slice(&self.base_frequency[offset..offset + LANES]);
            let mod_add_semitones_simd =
                Vf32::from_slice(&self.scratch_detune_add_semitones[offset..offset + LANES]);
            let mod_mult_simd = Vf32::from_slice(&self.scratch_detune_mult[offset..offset + LANES]);

            // Calculate total detune in cents
            let mod_add_cents_simd = mod_add_semitones_simd * cents_per_semitone_simd;
            let total_detune_cents_simd = static_detune_cents_simd + mod_add_cents_simd;

            // Calculate detune factor: 2^(total_cents / 1200)
            let exp_arg_simd = total_detune_cents_simd * inv_cents_per_octave_simd;
            let detune_factor_simd = exp_arg_simd.exp2(); // Use SIMD exp2

            // Apply factors
            let final_factor_simd = detune_factor_simd * mod_mult_simd;
            let result_simd = base_freq_simd * final_factor_simd;

            // Store result
            result_simd.copy_to_slice(&mut output[offset..offset + LANES]);
        }

        // --- 3) Scalar Remainder ---
        let remainder_start = chunks * LANES;
        for i in remainder_start..buffer_size {
            let base_freq = self.base_frequency[i];
            let mod_add_semitones = self.scratch_detune_add_semitones[i];
            let mod_mult = self.scratch_detune_mult[i];

            let total_detune_cents = self.detune + mod_add_semitones * 100.0;
            let detune_factor = 2.0f32.powf(total_detune_cents / 1200.0);
            let final_factor = detune_factor * mod_mult;

            output[i] = base_freq * final_factor;
        }
    }

    fn reset(&mut self) {
        // No dynamic state directly within this node (base_frequency is managed externally).
        // Scratch buffers are overwritten each process call.
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn is_active(&self) -> bool {
        // This node is typically always active as it provides a fundamental signal.
        true
    }

    fn set_active(&mut self, _active: bool) {
        // Activation state typically doesn't apply or is ignored.
    }

    fn name(&self) -> &'static str {
        "Global Frequency"
    }

    fn node_type(&self) -> &str {
        "global_frequency"
    }
}

// Implement the modulation trait to use its helpers
impl ModulationProcessor for GlobalFrequencyNode {}
