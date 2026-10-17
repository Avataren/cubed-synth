// convolver.rs

use fft_convolver::FFTConvolver;
use rustc_hash::FxHashMap;
use std::any::Any;
use std::borrow::Cow;

// Removed: use web_sys::console; // No longer needed

// Import ModulationProcessor and ModulationSource from the graph module.
use crate::graph::{ModulationProcessor, ModulationSource};
use crate::traits::{AudioNode, PortId}; // Ensure these paths are correct

/// Helper function to ensure a Vec has at least a certain size, filling with a value if resizing,
/// and truncating if it's too long.
fn ensure_buffer_size_and_fill(buffer: &mut Vec<f32>, required_size: usize, fill_value: f32) {
    if buffer.len() < required_size {
        buffer.resize_with(required_size, || fill_value);
    }
    buffer.truncate(required_size);
}

/// A Convolver that uses FFTConvolver for fast convolution processing.
/// Assumes only audio inputs are connected, uses self.wet_level for mix.
pub struct Convolver {
    enabled: bool,
    convolvers: Vec<FFTConvolver<f32>>,
    tail_count: usize,
    pub wet_level: f32,
    pub partition_size: usize,
    pub sample_rate: f32,
    fallback_zero_buffer: Vec<f32>,
    temp_wet_l: Vec<f32>,
    temp_wet_r: Vec<f32>,
    /// Store the original impulse response for serialization
    original_impulse_response: Vec<Vec<f32>>,
}

impl Convolver {
    pub fn new(impulse_response: Vec<f32>, partition_size: usize, sample_rate: f32) -> Self {
        Self::new_multi_channel(vec![impulse_response], partition_size, sample_rate)
    }

    pub fn set_wet_level(&mut self, wet_level: f32) {
        self.wet_level = wet_level.clamp(0.0, 1.0);
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.set_active(enabled);
    }

    /// Get the original impulse response data and metadata for serialization
    pub fn get_impulse_response_data(&self) -> (Vec<Vec<f32>>, f32, usize) {
        (
            self.original_impulse_response.clone(),
            self.sample_rate,
            self.original_impulse_response.len(),
        )
    }

    pub fn new_multi_channel(
        impulse_response: Vec<Vec<f32>>,
        partition_size: usize,
        sample_rate: f32,
    ) -> Self {
        // --- Initialization Logic ---
        let num_ir_channels = impulse_response.len();
        assert!(
            !impulse_response.is_empty(),
            "Impulse response cannot be empty"
        );
        assert!(
            [1, 2, 4].contains(&num_ir_channels),
            "Impulse response must have 1, 2, or 4 channels (provided {})",
            num_ir_channels
        );
        let length = impulse_response.first().map_or(0, |ch| ch.len());
        assert!(length > 0, "Impulse response channels cannot be empty");

        let total_samples = impulse_response.iter().map(|ch| ch.len()).sum::<usize>();
        let mut power: f32 = impulse_response
            .iter()
            .flat_map(|ch| ch.iter())
            .map(|&s| s * s)
            .sum();
        if total_samples > 0 && power > 0.0 {
            power = (power / (total_samples as f32)).sqrt();
        } else {
            power = 0.0;
        }
        let min_power = 0.000125;
        if !power.is_finite() || power < min_power {
            power = min_power;
        }
        let gain_calibration = 0.00125;
        let gain_calibration_sample_rate = sample_rate;
        let mut scale = 1.0 / power;
        scale *= gain_calibration;
        if sample_rate > 0.0 && gain_calibration_sample_rate > 0.0 {
            scale *= gain_calibration_sample_rate / sample_rate;
        }

        let num_convolvers = 2;
        let mut convolvers = Vec::with_capacity(num_convolvers);
        for i in 0..num_convolvers {
            let ir_channel_index = if num_ir_channels == 1 { 0 } else { i };
            if ir_channel_index >= impulse_response.len() {
                panic!("Internal logic error: Invalid IR channel index calculation.");
            }
            let mut scaled_channel = impulse_response[ir_channel_index].clone();
            for sample in scaled_channel.iter_mut() {
                *sample *= scale;
            }

            let mut conv = FFTConvolver::<f32>::default();
            match conv.init(partition_size, &scaled_channel) {
                Ok(_) => convolvers.push(conv),
                Err(e) => {
                    // Panic for critical initialization errors
                    panic!(
                        "Unable to initialize convolver {} using IR channel {}: {:?}",
                        i, ir_channel_index, e
                    );
                }
            }
        }

        Self {
            enabled: true,
            convolvers,
            tail_count: 0,
            wet_level: 0.2,
            partition_size,
            sample_rate,
            fallback_zero_buffer: Vec::new(),
            temp_wet_l: Vec::new(),
            temp_wet_r: Vec::new(),
            original_impulse_response: impulse_response, // Store original IR
        }
    }

    // --- process_block_wet_signal (Cleaned) ---
    fn process_block_wet_signal(
        convolvers: &mut [FFTConvolver<f32>],
        tail_count: &mut usize,
        inputs: &[&[f32]],
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        // Basic checks remain
        if convolvers.len() < 2 {
            output_left.fill(0.0);
            output_right.fill(0.0);
            return;
        }
        let buffer_len = output_left.len();
        if output_right.len() != buffer_len {
            output_left.fill(0.0);
            output_right.fill(0.0);
            return;
        }
        if buffer_len == 0 {
            return;
        }

        // Determine input config
        let input_l_slice_opt = inputs.first().filter(|s| s.len() >= buffer_len);
        let input_r_slice_opt = inputs
            .get(1)
            .filter(|s| s.len() >= buffer_len)
            .or(input_l_slice_opt);

        // Perform convolution
        match (input_l_slice_opt, input_r_slice_opt) {
            (Some(in_l), Some(in_r)) => {
                let _ = convolvers[0].process(&in_l[..buffer_len], output_left);
                let _ = convolvers[1].process(&in_r[..buffer_len], output_right);
            }
            _ => {
                // No valid input / length mismatch
                output_left.fill(0.0);
                output_right.fill(0.0);
            }
        }

        // Tail handling
        let is_silent = inputs
            .iter()
            .all(|channel| channel.iter().all(|&s| s.abs() < 1e-10));
        if is_silent {
            *tail_count += buffer_len;
        } else {
            *tail_count = 0;
        }
    }

    // --- reset_state (Cleaned) ---
    pub fn reset_state(&mut self) {
        self.tail_count = 0;
    }

    /// Update the convolver's impulse response
    pub fn set_impulse_response(&mut self, impulse_response: Vec<f32>) {
        // Rebuild the convolver with the new impulse response
        let new_convolver = Self::new(impulse_response, self.partition_size, self.sample_rate);

        // Update all fields
        self.convolvers = new_convolver.convolvers;
        self.original_impulse_response = new_convolver.original_impulse_response;
        self.tail_count = 0; // Reset tail count
    }
}

// ============================================================
// Implement the AudioNode trait for graph integration
// ============================================================
impl AudioNode for Convolver {
    // --- get_ports (Cleaned) ---
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        [
            (PortId::AudioInput0, false),
            (PortId::AudioInput1, false),
            (PortId::AudioOutput0, true),
            (PortId::AudioOutput1, true),
        ]
        .iter()
        .cloned()
        .collect()
    }

    // --- process (Cleaned) ---
    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        // Removed size check log

        if !self.enabled || self.convolvers.is_empty() {
            // --- Disabled State (Pass-through dry signal) ---
            let maybe_in_l_buf = inputs
                .get(&PortId::AudioInput0)
                .and_then(|v| v.first())
                .map(|s| &s.buffer);
            let maybe_in_r_buf = inputs
                .get(&PortId::AudioInput1)
                .and_then(|v| v.first())
                .map(|s| &s.buffer);
            let fill_or_copy = |out_opt: Option<&mut &mut [f32]>, in_opt: Option<&&[f32]>| {
                if let Some(out_slice) = out_opt {
                    if let Some(in_buf_ref) = in_opt {
                        let in_buf = *in_buf_ref;
                        let copy_len = buffer_size.min(in_buf.len());
                        if copy_len > 0 {
                            out_slice[..copy_len].copy_from_slice(&in_buf[..copy_len]);
                        }
                        if copy_len < buffer_size {
                            out_slice[copy_len..buffer_size].fill(0.0);
                        }
                    } else {
                        out_slice[..buffer_size].fill(0.0);
                    }
                }
            };
            fill_or_copy(outputs.get_mut(&PortId::AudioOutput0), maybe_in_l_buf);
            let fallback_r_in = maybe_in_r_buf.or(maybe_in_l_buf);
            fill_or_copy(outputs.get_mut(&PortId::AudioOutput1), fallback_r_in);
            return;
        }

        // --- Prepare Buffers ---
        ensure_buffer_size_and_fill(&mut self.fallback_zero_buffer, buffer_size, 0.0);
        ensure_buffer_size_and_fill(&mut self.temp_wet_l, buffer_size, 0.0);
        ensure_buffer_size_and_fill(&mut self.temp_wet_r, buffer_size, 0.0);

        // --- Get Audio Input Sources ---
        let input_l_source_buffer_opt: Option<&[f32]> = inputs
            .get(&PortId::AudioInput0)
            .and_then(|s| s.first())
            .map(|s| s.buffer.get(..buffer_size).unwrap_or(s.buffer));
        let input_r_source_buffer_opt: Option<&[f32]> = inputs
            .get(&PortId::AudioInput1)
            .and_then(|s| s.first())
            .map(|s| s.buffer.get(..buffer_size).unwrap_or(s.buffer));

        // --- Determine final DRY slices using Cow ---
        let dry_l_signal: Cow<[f32]> = input_l_source_buffer_opt
            .map(Cow::Borrowed)
            .unwrap_or_else(|| Cow::Owned(self.fallback_zero_buffer[..buffer_size].to_vec()));
        let dry_r_signal: Cow<[f32]> = input_r_source_buffer_opt
            .map(Cow::Borrowed)
            .unwrap_or_else(|| dry_l_signal.clone());

        // --- Get Output Buffers ---
        let mut out_l_buffer_opt = outputs.remove(&PortId::AudioOutput0);
        let mut out_r_buffer_opt = outputs.remove(&PortId::AudioOutput1);

        // --- Process Block ---
        if let (Some(out_l_buffer), Some(out_r_buffer)) =
            (&mut out_l_buffer_opt, &mut out_r_buffer_opt)
        {
            let conv_input_l: &[f32] = &dry_l_signal;
            let conv_input_r: &[f32] = &dry_r_signal;
            let input_slices_for_conv: &[&[f32]] = &[conv_input_l, conv_input_r];
            let temp_wet_l_slice = &mut self.temp_wet_l[..buffer_size];
            let temp_wet_r_slice = &mut self.temp_wet_r[..buffer_size];

            // --- Calculate Wet Signal ---
            Convolver::process_block_wet_signal(
                &mut self.convolvers,
                &mut self.tail_count,
                input_slices_for_conv,
                temp_wet_l_slice,
                temp_wet_r_slice,
            );

            // --- Apply Wet/Dry Mix using self.wet_level ---
            let final_out_l_slice = &mut out_l_buffer[..buffer_size];
            let final_out_r_slice = &mut out_r_buffer[..buffer_size];
            let mix_factor = self.wet_level; // Use property directly

            // Removed pre-mix logs

            for i in 0..buffer_size {
                let wet_l = *self.temp_wet_l.get(i).unwrap_or(&0.0);
                let wet_r = *self.temp_wet_r.get(i).unwrap_or(&0.0);
                let dry_l = *dry_l_signal.get(i).unwrap_or(&0.0);
                let dry_r = *dry_r_signal.get(i).unwrap_or(&0.0);

                final_out_l_slice[i] = wet_l * mix_factor + dry_l * (1.0 - mix_factor);
                final_out_r_slice[i] = wet_r * mix_factor + dry_r * (1.0 - mix_factor);
            }
        } else {
            //todo: error handling
        }

        // --- Reinsert Output Buffers ---
        if let Some(out_l) = out_l_buffer_opt {
            outputs.insert(PortId::AudioOutput0, out_l);
        }
        if let Some(out_r) = out_r_buffer_opt {
            outputs.insert(PortId::AudioOutput1, out_r);
        }
    }

    // --- Other trait methods ---
    fn reset(&mut self) {
        self.reset_state();
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
        if !active && self.enabled {
            self.reset_state();
        }
        self.enabled = active;
    }
    fn name(&self) -> &'static str {
        "Convolver"
    }
    fn node_type(&self) -> &str {
        "convolver"
    }
}

impl ModulationProcessor for Convolver {}
