use crate::traits::{Inputs, Outputs};
use cubed_simd::num::SimdFloat;
use cubed_simd::Simd;
use rustc_hash::FxHashMap;
use std::any::Any;
use std::f32::consts::{PI, TAU};

// Parameter Clamp Constants
const MIN_ALPHA: f32 = 0.9;
const MAX_ALPHA: f32 = 0.99999;
const MIN_FEEDBACK: f32 = -0.98;
const MAX_FEEDBACK: f32 = 0.98;
const MIN_MIX: f32 = 0.0;
const MAX_MIX: f32 = 1.0;

// Assuming these paths are correct for your project
use crate::traits::{AudioNode, PortId, StereoEffect};

const SIMD_WIDTH: usize = 4; // web wasm guaranteed supported

struct FeedbackFilter {
    state: f32,
    alpha: f32, // coefficient between 0 and 1, where smaller values mean more low-pass effect
}

impl FeedbackFilter {
    fn new(alpha: f32) -> Self {
        Self { state: 0.0, alpha }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        // Simple one-pole filter: y[n] = (1 - alpha) * input + alpha * y[n - 1]
        self.state = (1.0 - self.alpha) * input + self.alpha * self.state;

        // SAFETY: Clamp state to prevent unbounded growth and check for NaN
        const MAX_STATE: f32 = 100.0;
        self.state = if self.state.is_finite() {
            self.state.clamp(-MAX_STATE, MAX_STATE)
        } else {
            0.0
        };

        self.state
    }

    fn reset(&mut self) {
        self.state = 0.0;
    }
}

#[derive(Clone)]
struct FirFilter {
    coefficients: Vec<f32>,
    /// A double–sized buffer so that the FIR window is always contiguous.
    buffer: Vec<f32>,
    buffer_pos: usize,
    taps: usize,
}

impl FirFilter {
    fn new(coefficients: Vec<f32>) -> Self {
        let taps = coefficients.len();
        assert!(taps > 0, "FIR filter must have at least one tap.");
        // Create a buffer with double the taps. (Initialize to 0.)
        Self {
            coefficients,
            buffer: vec![0.0; 2 * taps],
            buffer_pos: 0,
            taps,
        }
    }

    /// Process one input sample and return the FIR output.
    /// This version uses SIMD (with a fallback loop over any remainder).
    #[inline(always)]
    fn process(&mut self, input: f32) -> f32 {
        // Write the new input sample at the current write position...
        self.buffer[self.buffer_pos] = input;
        // Also duplicate the sample to the mirror position to guarantee contiguity.
        self.buffer[self.buffer_pos + self.taps] = input;

        // The contiguous segment for the FIR is now at:
        //   self.buffer[self.buffer_pos .. self.buffer_pos + self.taps]
        let slice = &self.buffer[self.buffer_pos..self.buffer_pos + self.taps];
        let coeffs = &self.coefficients;

        // SIMD dot–product on the contiguous slice.
        let mut acc = Simd::<f32, SIMD_WIDTH>::splat(0.0);
        let chunk_count = self.taps / SIMD_WIDTH;
        let remainder = self.taps % SIMD_WIDTH;

        // Process full SIMD chunks.
        for i in 0..chunk_count {
            let start = i * SIMD_WIDTH;
            let sample_vec = Simd::from_slice(&slice[start..start + SIMD_WIDTH]);
            let coeff_vec = Simd::from_slice(&coeffs[start..start + SIMD_WIDTH]);
            acc += sample_vec * coeff_vec;
        }
        let mut result = acc.reduce_sum();

        // Process any remaining coefficients.
        if remainder != 0 {
            let start = chunk_count * SIMD_WIDTH;
            for j in 0..remainder {
                result += slice[start + j] * coeffs[start + j];
            }
        }

        // Advance the circular pointer.
        self.buffer_pos = (self.buffer_pos + 1) % self.taps;
        result
    }

    /// Reset the filter state.
    fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.buffer_pos = 0;
    }
}

fn generate_fir_coeffs(
    num_taps: usize,
    cutoff_normalized: f32,
    window: fn(usize, usize) -> f32,
) -> Vec<f32> {
    assert!(num_taps > 0, "Number of taps must be positive.");
    assert!(
        !num_taps.is_multiple_of(2),
        "Use an odd number of taps for a Type 1 linear phase FIR (zero delay at center)."
    );
    assert!(
        cutoff_normalized > 0.0 && cutoff_normalized < 0.5,
        "Normalized cutoff must be between 0 and 0.5"
    );
    let mut coeffs = vec![0.0; num_taps];
    let center = (num_taps / 2) as isize;
    let mut sum = 0.0;
    for i in 0..num_taps {
        let n = i as isize - center;
        let val = if n == 0 {
            2.0 * cutoff_normalized
        } else {
            (TAU * cutoff_normalized * n as f32).sin() / (PI * n as f32)
        };
        let win_val = window(i, num_taps);
        coeffs[i] = val * win_val;
        sum += coeffs[i];
    }
    if sum.abs() > 1e-6 {
        let inv_sum = 1.0 / sum;
        for c in coeffs.iter_mut() {
            *c *= inv_sum;
        }
    } else {
        eprintln!("Warning: FIR filter coefficient sum is near zero. Check parameters. Setting center tap to 1.");
        coeffs.fill(0.0);
        if num_taps > 0 {
            coeffs[center as usize] = 1.0;
        }
    }
    coeffs
}

fn blackman_window(n: usize, num_taps: usize) -> f32 {
    if num_taps <= 1 {
        return 1.0;
    }
    let m = (num_taps - 1) as f32;
    let nn = n as f32;
    0.42 - 0.5 * (TAU * nn / m).cos() + 0.08 * (2.0 * TAU * nn / m).cos()
}

#[derive(Clone, Copy, Debug)]
struct DcBlocker {
    x_prev: f32,
    y_prev: f32,
    alpha: f32,
}

const DENORMAL_PREVENTION: f32 = 1.0e-20;

impl DcBlocker {
    fn new(alpha: f32) -> Self {
        Self {
            x_prev: 0.0,
            y_prev: 0.0,
            alpha: alpha.clamp(MIN_ALPHA, MAX_ALPHA),
        }
    }

    #[inline(always)]
    fn process(&mut self, input: f32) -> f32 {
        let output = input - self.x_prev + self.alpha * self.y_prev;
        self.x_prev = input;
        // Add tiny value to prevent denormals
        self.y_prev = output + DENORMAL_PREVENTION;

        // SAFETY: Check for NaN or Inf and reset if detected
        if !output.is_finite() {
            self.x_prev = 0.0;
            self.y_prev = 0.0;
            return 0.0;
        }

        output
    }

    fn reset(&mut self) {
        self.x_prev = 0.0;
        self.y_prev = 0.0;
    }
}

fn dc_blocker_alpha(cutoff_hz: f32, sample_rate: f32) -> f32 {
    if cutoff_hz <= 0.0 || sample_rate <= 0.0 {
        return 1.0;
    }
    let rc = 1.0 / (TAU * cutoff_hz);
    let dt = 1.0 / sample_rate;
    rc / (rc + dt)
}

#[inline(always)]
fn smooth_parameter(current: f32, target: f32, coefficient: f32) -> f32 {
    current * coefficient + target * (1.0 - coefficient)
}

const OVERSAMPLE: usize = 4;
const INTERPOLATION_MARGIN: usize = 3;
const DC_BLOCKER_CUTOFF_HZ: f32 = 10.0;

pub struct Chorus {
    enabled: bool,
    internal_sample_rate: f32,
    inv_internal_sample_rate: f32,
    delay_buffer_left: Vec<f32>,
    delay_buffer_right: Vec<f32>,
    upsample_stage1_left: Vec<f32>,
    upsample_stage1_right: Vec<f32>,
    upsampled_input_left: Vec<f32>,
    upsampled_input_right: Vec<f32>,
    processed_oversampled_left: Vec<f32>,
    processed_oversampled_right: Vec<f32>,
    write_index: usize,
    max_delay_samples: usize,
    max_safe_read_delay: f32,
    lfo_phase_left: f32,
    lfo_phase_right: f32,
    target_base_delay_samples: f32,
    target_depth_samples: f32,
    target_lfo_rate_hz: f32,
    target_feedback: f32,
    target_mix: f32,
    target_lfo_stereo_phase_offset_rad: f32,
    current_base_delay_samples: f32,
    current_depth_samples: f32,
    current_lfo_phase_increment: f32,
    current_feedback: f32,
    current_mix: f32,
    param_smooth_coeff: f32,
    upsample_filter1_left: FirFilter,
    upsample_filter1_right: FirFilter,
    upsample_filter2_left: FirFilter,
    upsample_filter2_right: FirFilter,
    downsample_filter1_left: FirFilter,
    downsample_filter1_right: FirFilter,
    downsample_filter2_left: FirFilter,
    downsample_filter2_right: FirFilter,
    output_dc_blocker_l: DcBlocker,
    output_dc_blocker_r: DcBlocker,
    scratch_downsample_stage: Vec<f32>,
    scratch_downsampled: Vec<f32>,
    scratch_final_left: Vec<f32>,
    scratch_final_right: Vec<f32>,
    feedback_filter_l: FeedbackFilter,
    feedback_filter_r: FeedbackFilter,
    target_feedback_filter_cutoff: f32,
    current_feedback_filter_cutoff: f32,
}

impl Chorus {
    #[inline]
    fn ensure_len(buf: &mut Vec<f32>, len: usize) {
        if buf.len() < len {
            buf.resize(len, 0.0);
        }
    }

    fn ensure_buffer_capacity(&mut self, buffer_size: usize) {
        let oversampled = buffer_size * OVERSAMPLE;

        Self::ensure_len(&mut self.upsample_stage1_left, oversampled);
        Self::ensure_len(&mut self.upsample_stage1_right, oversampled);
        Self::ensure_len(&mut self.upsampled_input_left, oversampled);
        Self::ensure_len(&mut self.upsampled_input_right, oversampled);
        Self::ensure_len(&mut self.processed_oversampled_left, oversampled);
        Self::ensure_len(&mut self.processed_oversampled_right, oversampled);
        Self::ensure_len(&mut self.scratch_downsample_stage, oversampled);
        Self::ensure_len(&mut self.scratch_downsampled, oversampled);
        Self::ensure_len(&mut self.scratch_final_left, buffer_size);
        Self::ensure_len(&mut self.scratch_final_right, buffer_size);
    }

    pub fn new(
        sample_rate: f32,
        max_base_delay_ms: f32,
        base_delay_ms: f32,
        depth_ms: f32,
        lfo_rate_hz: f32,
        feedback: f32,
        mix: f32,
        stereo_phase_offset_deg: f32,
    ) -> Self {
        assert!(sample_rate > 0.0, "Sample rate must be positive");
        let internal_sample_rate = sample_rate * OVERSAMPLE as f32;
        let inv_internal_sample_rate = 1.0 / internal_sample_rate;

        let max_modulated_delay_ms = max_base_delay_ms + 20.0;
        let required_samples_for_delay =
            (max_modulated_delay_ms / 1000.0 * internal_sample_rate).ceil() as usize;
        let max_delay_samples = required_samples_for_delay + INTERPOLATION_MARGIN;
        let max_safe_read_delay = (max_delay_samples - INTERPOLATION_MARGIN) as f32;

        let initial_base_delay_samples = (base_delay_ms / 1000.0 * internal_sample_rate).max(0.0);
        let initial_depth_samples = (depth_ms / 1000.0 * internal_sample_rate * 0.5).abs();
        let initial_lfo_rate_hz = lfo_rate_hz.max(0.0);
        let initial_lfo_phase_increment = TAU * initial_lfo_rate_hz * inv_internal_sample_rate;
        let initial_feedback = feedback.clamp(MIN_FEEDBACK, MAX_FEEDBACK);
        let initial_mix = mix.clamp(MIN_MIX, MAX_MIX);
        let initial_lfo_stereo_phase_offset_rad = stereo_phase_offset_deg.to_radians();

        let smoothing_time_ms = 0.1;
        let param_smooth_coeff = if internal_sample_rate > 0.0 {
            let smoothing_samples = smoothing_time_ms * 0.001 * internal_sample_rate;
            if smoothing_samples > 1.0 {
                (-1.0 / smoothing_samples).exp()
            } else {
                0.0
            }
        } else {
            0.0
        };

        let num_taps = 31;
        let normalized_cutoff = 0.5 / OVERSAMPLE as f32;
        let filter_cutoff = normalized_cutoff * 0.90;
        let base_coeffs = generate_fir_coeffs(num_taps, filter_cutoff, blackman_window);
        let mut up_coeffs = base_coeffs.clone();
        for c in up_coeffs.iter_mut() {
            *c *= 2.0;
        }
        let down_coeffs = base_coeffs;

        const INITIAL_CAPACITY: usize = 128;
        let initial_oversampled = INITIAL_CAPACITY * OVERSAMPLE;

        Self {
            enabled: true,
            internal_sample_rate,
            inv_internal_sample_rate,
            delay_buffer_left: vec![0.0; max_delay_samples],
            delay_buffer_right: vec![0.0; max_delay_samples],
            upsample_stage1_left: vec![0.0; initial_oversampled],
            upsample_stage1_right: vec![0.0; initial_oversampled],
            upsampled_input_left: vec![0.0; initial_oversampled],
            upsampled_input_right: vec![0.0; initial_oversampled],
            processed_oversampled_left: vec![0.0; initial_oversampled],
            processed_oversampled_right: vec![0.0; initial_oversampled],
            write_index: 0,
            max_delay_samples,
            max_safe_read_delay,
            target_base_delay_samples: initial_base_delay_samples,
            target_depth_samples: initial_depth_samples,
            target_lfo_rate_hz: initial_lfo_rate_hz,
            target_feedback: initial_feedback,
            target_mix: initial_mix,
            target_lfo_stereo_phase_offset_rad: initial_lfo_stereo_phase_offset_rad,
            current_base_delay_samples: initial_base_delay_samples,
            current_depth_samples: initial_depth_samples,
            current_lfo_phase_increment: initial_lfo_phase_increment,
            current_feedback: initial_feedback,
            current_mix: initial_mix,
            lfo_phase_left: 0.0,
            lfo_phase_right: initial_lfo_stereo_phase_offset_rad.rem_euclid(TAU),
            param_smooth_coeff,
            upsample_filter1_left: FirFilter::new(up_coeffs.clone()),
            upsample_filter1_right: FirFilter::new(up_coeffs.clone()),
            upsample_filter2_left: FirFilter::new(up_coeffs.clone()),
            upsample_filter2_right: FirFilter::new(up_coeffs.clone()),
            downsample_filter1_left: FirFilter::new(down_coeffs.clone()),
            downsample_filter1_right: FirFilter::new(down_coeffs.clone()),
            downsample_filter2_left: FirFilter::new(down_coeffs.clone()),
            downsample_filter2_right: FirFilter::new(down_coeffs),
            output_dc_blocker_l: DcBlocker::new(dc_blocker_alpha(
                DC_BLOCKER_CUTOFF_HZ,
                sample_rate,
            )),
            output_dc_blocker_r: DcBlocker::new(dc_blocker_alpha(
                DC_BLOCKER_CUTOFF_HZ,
                sample_rate,
            )),
            scratch_downsample_stage: vec![0.0; initial_oversampled],
            scratch_downsampled: vec![0.0; initial_oversampled],
            scratch_final_left: vec![0.0; INITIAL_CAPACITY],
            scratch_final_right: vec![0.0; INITIAL_CAPACITY],
            feedback_filter_l: FeedbackFilter::new(0.5),
            feedback_filter_r: FeedbackFilter::new(0.5),
            target_feedback_filter_cutoff: 10000.0,
            current_feedback_filter_cutoff: 10000.0,
        }
    }

    // Parameter setters remain the same except for using the centralized clamps.
    pub fn set_base_delay_ms(&mut self, delay_ms: f32) {
        self.target_base_delay_samples = (delay_ms * 0.001 * self.internal_sample_rate).max(0.0);
    }
    pub fn set_depth_ms(&mut self, depth_ms: f32) {
        self.target_depth_samples = (depth_ms * 0.001 * self.internal_sample_rate * 0.5).abs();
    }
    pub fn set_rate_hz(&mut self, rate_hz: f32) {
        self.target_lfo_rate_hz = rate_hz.max(0.0);
    }
    pub fn set_feedback(&mut self, feedback: f32) {
        self.target_feedback = feedback.clamp(MIN_FEEDBACK, MAX_FEEDBACK);
    }
    pub fn set_mix(&mut self, mix: f32) {
        self.target_mix = mix.clamp(MIN_MIX, MAX_MIX);
    }
    pub fn set_stereo_phase_offset_deg(&mut self, offset_deg: f32) {
        self.target_lfo_stereo_phase_offset_rad = offset_deg.to_radians();
        self.lfo_phase_right =
            (self.lfo_phase_left + self.target_lfo_stereo_phase_offset_rad).rem_euclid(TAU);
    }
    pub fn set_feedback_filter_cutoff(&mut self, cutoff_hz: f32) {
        // You might want to clamp the cutoff between some reasonable min/max values.
        // Here we assume a minimum of 20 Hz and maximum of, say, 20000 Hz.
        self.target_feedback_filter_cutoff = cutoff_hz.clamp(20.0, 20000.0);
    }
    /// Cubic interpolation for delay sample estimation.
    /// Uses four surrounding points for smooth interpolation.
    #[inline(always)]
    fn read_cubic_interpolated(
        buffer: &[f32],
        delay_samples: f32,
        write_index: usize,
        max_delay_samples: usize,
    ) -> f32 {
        let read_pos_float = (write_index as f32 - delay_samples + max_delay_samples as f32)
            % max_delay_samples as f32;
        let index_frac = read_pos_float.fract();
        let index_int = read_pos_float.floor() as usize;
        let i_1 = (index_int + max_delay_samples - 1) % max_delay_samples;
        let i0 = index_int;
        let i1 = (index_int + 1) % max_delay_samples;
        let i2 = (index_int + 2) % max_delay_samples;
        let p0 = buffer[i_1];
        let p1 = buffer[i0];
        let p2 = buffer[i1];
        let p3 = buffer[i2];
        let t = index_frac;
        let t2 = t * t;
        let t3 = t2 * t;
        0.5 * ((2.0 * p1)
            + (-p0 + p2) * t
            + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
            + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
    }

    #[inline(always)]
    fn calculate_filter_alpha(cutoff: f32, sample_rate: f32) -> f32 {
        // A typical relationship is: alpha = exp(-2π * cutoff / sample_rate)
        (-2.0 * PI * cutoff / sample_rate).exp()
    }

    #[inline(never)]
    pub fn process_block(
        &mut self,
        inputs: &Inputs<'_>,
        outputs: &mut Outputs<'_>,
        buffer_size: usize,
    ) {
        if buffer_size == 0 {
            return;
        }

        // Obtain the output slices by temporarily removing them from the map.
        // (This pattern is safe because the hash map has only two entries.)
        let mut out_left = outputs.remove(&PortId::AudioOutput0);
        let mut out_right = outputs.remove(&PortId::AudioOutput1);
        if out_left.is_none() || out_right.is_none() {
            // If one output is missing, zero any present and reinsert if necessary.
            if let Some(out) = out_left.as_mut().or(out_right.as_mut()) {
                let fill_len = buffer_size.min(out.len());
                out[..fill_len].fill(0.0);
            }
            if let Some(o) = out_left.take() {
                outputs.insert(PortId::AudioOutput0, o);
            }
            if let Some(o) = out_right.take() {
                outputs.insert(PortId::AudioOutput1, o);
            }
            return;
        }
        // Now we have exclusive mutable access to both output slices.
        let out_left_slice: &mut [f32] = out_left.as_mut().unwrap();
        let out_right_slice: &mut [f32] = out_right.as_mut().unwrap();

        // Retrieve input buffers.
        let left_in_slice = match inputs.get(&PortId::AudioInput0).and_then(|v| v.first()) {
            Some(src) => src.buffer,
            None => {
                let fill_len = buffer_size
                    .min(out_left_slice.len())
                    .min(out_right_slice.len());
                if fill_len > 0 {
                    out_left_slice[..fill_len].fill(0.0);
                    out_right_slice[..fill_len].fill(0.0);
                }
                if fill_len < out_left_slice.len() {
                    out_left_slice[fill_len..].fill(0.0);
                }
                if fill_len < out_right_slice.len() {
                    out_right_slice[fill_len..].fill(0.0);
                }
                outputs.insert(PortId::AudioOutput0, out_left.unwrap());
                outputs.insert(PortId::AudioOutput1, out_right.unwrap());
                return;
            }
        };
        let right_in_slice = inputs
            .get(&PortId::AudioInput1)
            .and_then(|v| v.first())
            .map_or(left_in_slice, |s| s.buffer);

        // Disabled state: passthrough
        if !self.enabled {
            let proc_len = buffer_size
                .min(left_in_slice.len())
                .min(right_in_slice.len())
                .min(out_left_slice.len())
                .min(out_right_slice.len());

            out_left_slice[..proc_len].copy_from_slice(&left_in_slice[..proc_len]);
            out_right_slice[..proc_len].copy_from_slice(&right_in_slice[..proc_len]);

            if proc_len < out_left_slice.len() {
                out_left_slice[proc_len..].fill(0.0);
            }
            if proc_len < out_right_slice.len() {
                out_right_slice[proc_len..].fill(0.0);
            }
            outputs.insert(PortId::AudioOutput0, out_left.unwrap());
            outputs.insert(PortId::AudioOutput1, out_right.unwrap());
            return;
        }
        let process_len = buffer_size
            .min(left_in_slice.len())
            .min(right_in_slice.len())
            .min(out_left_slice.len())
            .min(out_right_slice.len());
        if process_len == 0 {
            let len_left = buffer_size.min(out_left_slice.len());
            out_left_slice[..len_left].fill(0.0);
            let len_right = buffer_size.min(out_right_slice.len());
            out_right_slice[..len_right].fill(0.0);
            outputs.insert(PortId::AudioOutput0, out_left.unwrap());
            outputs.insert(PortId::AudioOutput1, out_right.unwrap());
            return;
        }

        out_left_slice[..process_len].copy_from_slice(&left_in_slice[..process_len]);
        out_right_slice[..process_len].copy_from_slice(&right_in_slice[..process_len]);
        self.process_stereo(
            &mut out_left_slice[..process_len],
            &mut out_right_slice[..process_len],
        );
        if process_len < out_left_slice.len() {
            out_left_slice[process_len..].fill(0.0);
        }
        if process_len < out_right_slice.len() {
            out_right_slice[process_len..].fill(0.0);
        }

        // Finally, reinsert the output slices back into the outputs map.
        outputs.insert(PortId::AudioOutput0, out_left.unwrap());
        outputs.insert(PortId::AudioOutput1, out_right.unwrap());
    }

    pub fn reset_state(&mut self) {
        self.delay_buffer_left.fill(0.0);
        self.delay_buffer_right.fill(0.0);
        self.write_index = 0;
        self.lfo_phase_left = 0.0;
        self.lfo_phase_right = self.target_lfo_stereo_phase_offset_rad.rem_euclid(TAU);
        self.upsample_filter1_left.reset();
        self.upsample_filter1_right.reset();
        self.upsample_filter2_left.reset();
        self.upsample_filter2_right.reset();
        self.downsample_filter1_left.reset();
        self.downsample_filter1_right.reset();
        self.downsample_filter2_left.reset();
        self.downsample_filter2_right.reset();
        self.output_dc_blocker_l.reset();
        self.output_dc_blocker_r.reset();
        self.current_base_delay_samples = self.target_base_delay_samples;
        self.current_depth_samples = self.target_depth_samples;
        self.current_lfo_phase_increment =
            TAU * self.target_lfo_rate_hz * self.inv_internal_sample_rate;
        self.current_feedback = self.target_feedback;
        self.current_mix = self.target_mix;
        self.upsample_stage1_left.fill(0.0);
        self.upsample_stage1_right.fill(0.0);
        self.upsampled_input_left.fill(0.0);
        self.upsampled_input_right.fill(0.0);
        self.processed_oversampled_left.fill(0.0);
        self.processed_oversampled_right.fill(0.0);
        self.feedback_filter_l.reset();
        self.feedback_filter_r.reset();
    }

    fn node_type_str(&self) -> &str {
        "chorus"
    }
    fn is_node_active(&self) -> bool {
        self.enabled
    }
    fn set_node_active(&mut self, active: bool) {
        if active && !self.enabled {
            self.reset_state();
        }
        self.enabled = active;
    }
    fn as_any_internal_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn as_any_internal(&self) -> &dyn Any {
        self
    }
}

impl StereoEffect for Chorus {
    fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let process_len = left.len().min(right.len());
        self.ensure_buffer_capacity(process_len);

        // Smooth parameters (as before)
        let coeff = self.param_smooth_coeff;
        self.current_base_delay_samples = smooth_parameter(
            self.current_base_delay_samples,
            self.target_base_delay_samples,
            coeff,
        );
        self.current_depth_samples =
            smooth_parameter(self.current_depth_samples, self.target_depth_samples, coeff);
        self.current_feedback =
            smooth_parameter(self.current_feedback, self.target_feedback, coeff);
        self.current_mix = smooth_parameter(self.current_mix, self.target_mix, coeff);
        let target_lfo_phase_increment =
            TAU * self.target_lfo_rate_hz * self.inv_internal_sample_rate;
        self.current_lfo_phase_increment = smooth_parameter(
            self.current_lfo_phase_increment,
            target_lfo_phase_increment,
            coeff,
        );

        self.current_feedback_filter_cutoff = smooth_parameter(
            self.current_feedback_filter_cutoff,
            self.target_feedback_filter_cutoff,
            self.param_smooth_coeff,
        );
        // Update the filter’s alpha for tone shaping.
        self.feedback_filter_l.alpha = Self::calculate_filter_alpha(
            self.current_feedback_filter_cutoff,
            self.internal_sample_rate,
        );
        self.feedback_filter_r.alpha = Self::calculate_filter_alpha(
            self.current_feedback_filter_cutoff,
            self.internal_sample_rate,
        );

        // UPSAMPLING (multipass FIR cascaded)
        let internal_buffer_len = process_len * OVERSAMPLE;
        {
            let mut up_idx = 0;
            let filter1_l = &mut self.upsample_filter1_left;
            let filter1_r = &mut self.upsample_filter1_right;
            for i in 0..process_len {
                self.upsample_stage1_left[up_idx] = filter1_l.process(left[i]);
                self.upsample_stage1_right[up_idx] = filter1_r.process(right[i]);
                up_idx += 1;
                self.upsample_stage1_left[up_idx] = filter1_l.process(0.0);
                self.upsample_stage1_right[up_idx] = filter1_r.process(0.0);
                up_idx += 1;
                self.upsample_stage1_left[up_idx] = filter1_l.process(0.0);
                self.upsample_stage1_right[up_idx] = filter1_r.process(0.0);
                up_idx += 1;
                self.upsample_stage1_left[up_idx] = filter1_l.process(0.0);
                self.upsample_stage1_right[up_idx] = filter1_r.process(0.0);
                up_idx += 1;
            }
        }

        for i in 0..internal_buffer_len {
            self.upsampled_input_left[i] = self
                .upsample_filter2_left
                .process(self.upsample_stage1_left[i]);
            self.upsampled_input_right[i] = self
                .upsample_filter2_right
                .process(self.upsample_stage1_right[i]);
        }

        // CORE CHORUS PROCESSING (in oversampled domain)
        {
            let mut lfo_phase_left = self.lfo_phase_left;
            let mut lfo_phase_right = self.lfo_phase_right;
            let phase_inc = self.current_lfo_phase_increment;
            let base_delay = self.current_base_delay_samples;
            let depth = self.current_depth_samples;
            let feedback = self.current_feedback;
            let mix = self.current_mix;
            let dry_level = 1.0 - mix;
            let wet_level = mix;
            let max_delay_samples = self.max_delay_samples;
            let max_safe_read_delay = self.max_safe_read_delay;
            let mut current_write_index = self.write_index;
            let delay_buf_l = &mut self.delay_buffer_left;
            let delay_buf_r = &mut self.delay_buffer_right;
            const MIN_DELAY_SAMPLES_CUBIC: f32 = 2.0;
            let min_target_delay = base_delay - depth;
            let effective_base_delay = if min_target_delay < MIN_DELAY_SAMPLES_CUBIC {
                base_delay + (MIN_DELAY_SAMPLES_CUBIC - min_target_delay)
            } else {
                base_delay
            };

            for i in 0..internal_buffer_len {
                let lfo_val_left = lfo_phase_left.sin();
                let lfo_val_right = lfo_phase_right.sin();
                let target_delay_l = effective_base_delay + lfo_val_left * depth;
                let target_delay_r = effective_base_delay + lfo_val_right * depth;
                let delay_smpls_left = target_delay_l.min(max_safe_read_delay).max(0.0);
                let delay_smpls_right = target_delay_r.min(max_safe_read_delay).max(0.0);
                // let delayed_left = Self::read_cubic_interpolated(
                //     delay_buf_l,
                //     delay_smpls_left,
                //     current_write_index,
                //     max_delay_samples,
                // );
                // let delayed_right = Self::read_cubic_interpolated(
                //     delay_buf_r,
                //     delay_smpls_right,
                //     current_write_index,
                //     max_delay_samples,
                // );
                // Read the delayed values
                let delayed_left = Self::read_cubic_interpolated(
                    delay_buf_l,
                    delay_smpls_left,
                    current_write_index,
                    max_delay_samples,
                );
                let delayed_right = Self::read_cubic_interpolated(
                    delay_buf_r,
                    delay_smpls_right,
                    current_write_index,
                    max_delay_samples,
                );

                // SAFETY: Check for NaN or Inf in delayed signals
                let delayed_left = if delayed_left.is_finite() {
                    delayed_left
                } else {
                    0.0
                };
                let delayed_right = if delayed_right.is_finite() {
                    delayed_right
                } else {
                    0.0
                };

                // Process through the feedback filter
                let filtered_left = self.feedback_filter_l.process(delayed_left);
                let filtered_right = self.feedback_filter_r.process(delayed_right);

                // SAFETY: Check for NaN or Inf in filtered signals
                let filtered_left = if filtered_left.is_finite() {
                    filtered_left
                } else {
                    0.0
                };
                let filtered_right = if filtered_right.is_finite() {
                    filtered_right
                } else {
                    0.0
                };

                // Compute the feedback terms using the filtered signals
                let feedback_term_l = feedback * filtered_left;
                let feedback_term_r = feedback * filtered_right;
                let current_input_l = self.upsampled_input_left[i];
                let current_input_r = self.upsampled_input_right[i];
                let write_val_left = current_input_l + feedback_term_l;
                let write_val_right = current_input_r + feedback_term_r;

                // SAFETY: Clamp values to prevent unbounded growth
                const MAX_SAMPLE_VALUE: f32 = 10.0; // Allow some headroom beyond [-1, 1]
                let write_val_left = write_val_left.clamp(-MAX_SAMPLE_VALUE, MAX_SAMPLE_VALUE);
                let write_val_right = write_val_right.clamp(-MAX_SAMPLE_VALUE, MAX_SAMPLE_VALUE);

                delay_buf_l[current_write_index] = write_val_left;
                delay_buf_r[current_write_index] = write_val_right;
                self.processed_oversampled_left[i] =
                    current_input_l * dry_level + delayed_left * wet_level;
                self.processed_oversampled_right[i] =
                    current_input_r * dry_level + delayed_right * wet_level;
                lfo_phase_left = (lfo_phase_left + phase_inc).rem_euclid(TAU);
                lfo_phase_right = (lfo_phase_right + phase_inc).rem_euclid(TAU);
                current_write_index = (current_write_index + 1) % max_delay_samples;
            }
            self.lfo_phase_left = lfo_phase_left;
            self.lfo_phase_right = lfo_phase_right;
            self.write_index = current_write_index;
        }

        // DOWNSAMPLING (multipass FIR cascaded)
        {
            let downsample_stage = &mut self.scratch_downsample_stage[..internal_buffer_len];
            let filter1_l = &mut self.downsample_filter1_left;
            let filter1_r = &mut self.downsample_filter1_right;
            for i in 0..internal_buffer_len {
                downsample_stage[i] = filter1_l.process(self.processed_oversampled_left[i]);
                self.processed_oversampled_right[i] =
                    filter1_r.process(self.processed_oversampled_right[i]);
            }
        }
        {
            let downsampled = &mut self.scratch_downsampled[..internal_buffer_len];
            let filter2_l = &mut self.downsample_filter2_left;
            let filter2_r = &mut self.downsample_filter2_right;
            for i in 0..internal_buffer_len {
                downsampled[i] = filter2_l.process(self.scratch_downsample_stage[i]);
                self.processed_oversampled_right[i] =
                    filter2_r.process(self.processed_oversampled_right[i]);
            }
        }

        // Write final decimated results directly into the preallocated scratch buffers.
        let final_left = &mut self.scratch_final_left;
        let final_right = &mut self.scratch_final_right;
        let mut out_idx = 0;
        for i in (0..internal_buffer_len).step_by(OVERSAMPLE) {
            if out_idx < process_len {
                final_left[out_idx] = self.scratch_downsampled[i];
                final_right[out_idx] = self.processed_oversampled_right[i];
                out_idx += 1;
            } else {
                eprintln!("Chorus Error: Output buffer overrun during downsampling!");
                break;
            }
        }
        assert_eq!(
            out_idx, process_len,
            "Downsampling mismatch: expected {} samples, got {}",
            process_len, out_idx
        );

        // Apply DC blockers in place on the scratch final buffers.
        for i in 0..process_len {
            final_left[i] = self.output_dc_blocker_l.process(final_left[i]);
            final_right[i] = self.output_dc_blocker_r.process(final_right[i]);
        }

        // Copy from our scratch final buffers into the block, in place.
        left[..process_len].copy_from_slice(&final_left[..process_len]);
        right[..process_len].copy_from_slice(&final_right[..process_len]);
    }
}

impl AudioNode for Chorus {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        FxHashMap::from_iter([
            (PortId::AudioInput0, false),
            (PortId::AudioInput1, false),
            (PortId::AudioOutput0, true),
            (PortId::AudioOutput1, true),
        ])
    }

    fn process(&mut self, inputs: &Inputs<'_>, outputs: &mut Outputs<'_>, buffer_size: usize) {
        self.process_block(inputs, outputs, buffer_size);
    }

    fn reset(&mut self) {
        self.reset_state();
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self.as_any_internal_mut()
    }
    fn as_any(&self) -> &dyn Any {
        self.as_any_internal()
    }
    fn is_active(&self) -> bool {
        self.is_node_active()
    }
    fn set_active(&mut self, active: bool) {
        self.set_node_active(active);
    }
    fn name(&self) -> &'static str {
        "Chorus"
    }
    fn node_type(&self) -> &str {
        self.node_type_str()
    }
    fn as_stereo_effect(&mut self) -> Option<&mut dyn StereoEffect> {
        Some(self)
    }
}
