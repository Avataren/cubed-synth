use rustc_hash::FxHashMap;
use std::any::Any;

use crate::graph::ModulationSource;
use crate::traits::{AudioNode, PortId, StereoEffect};

// --- Helper Functions ---
#[inline(always)]
fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db * 0.05) // 0.05 is 1/20
}

#[inline(always)]
fn calculate_smooth_coeff(time_ms: f32, sample_rate: f32) -> f32 {
    if time_ms <= 0.0 || sample_rate <= 0.0 {
        return 1.0; // Instantaneous change
    }
    // Calculate time constant in samples
    let time_samples = time_ms * 0.001 * sample_rate;
    if time_samples < 1.0 {
        return 1.0; // Effectively instantaneous if time is less than one sample
    }
    // Standard one-pole smoothing coefficient formula
    (-1.0 / time_samples).exp()
}

// --- Limiter Struct Definition ---
pub struct Limiter {
    enabled: bool,
    sample_rate: f32,

    // --- Parameters (Targets) ---
    target_threshold_db: f32,
    target_attack_ms: f32,
    target_release_ms: f32,
    target_stereo_link: bool, // Apply same gain reduction to L/R?

    // --- Parameters (Internal/Smoothed) ---
    current_threshold_linear: f32,
    attack_coeff: f32,  // Coefficient for envelope attack
    release_coeff: f32, // Coefficient for envelope release

    // --- State Variables ---
    envelope_l: f32, // Current detected envelope level (linear)
    envelope_r: f32,
    gain_reduction_l: f32, // Current gain reduction factor applied (linear, <= 1.0)
    gain_reduction_r: f32,

    // --- Lookahead ---
    lookahead_buffer_l: Vec<f32>,
    lookahead_buffer_r: Vec<f32>,
    lookahead_write_index: usize,
    lookahead_read_delay: usize, // How many samples behind write index to read
}

impl Limiter {
    pub fn new(
        sample_rate: f32,
        threshold_db: f32, // E.g., -0.1 dBFS
        attack_ms: f32,    // E.g., 0.1 ms (very fast)
        release_ms: f32,   // E.g., 50.0 ms
        lookahead_ms: f32, // E.g., 1.5 ms
        stereo_link: bool, // E.g., true
    ) -> Self {
        assert!(sample_rate > 0.0, "Sample rate must be positive");
        assert!(attack_ms >= 0.0, "Attack time must be non-negative");
        assert!(release_ms > 0.0, "Release time must be positive"); // Release needs time to recover
        assert!(lookahead_ms >= 0.0, "Lookahead time must be non-negative");

        let attack_coeff = calculate_smooth_coeff(attack_ms, sample_rate);
        let release_coeff = calculate_smooth_coeff(release_ms, sample_rate);
        let current_threshold_linear = db_to_linear(threshold_db);

        // Calculate lookahead buffer size and indices
        let lookahead_samples = (lookahead_ms * 0.001 * sample_rate).ceil() as usize;
        let lookahead_buffer_size = if lookahead_samples > 0 {
            lookahead_samples + 1
        } else {
            1
        }; // Need at least size 1 even if no lookahead
        let lookahead_read_delay = lookahead_samples; // Read 'delay' samples behind write index

        Self {
            enabled: true,
            sample_rate,
            target_threshold_db: threshold_db,
            target_attack_ms: attack_ms,
            target_release_ms: release_ms,
            target_stereo_link: stereo_link,
            current_threshold_linear,
            attack_coeff,
            release_coeff,
            envelope_l: 0.0, // Start envelope at 0
            envelope_r: 0.0,
            gain_reduction_l: 1.0, // Start with no gain reduction
            gain_reduction_r: 1.0,
            lookahead_buffer_l: vec![0.0; lookahead_buffer_size],
            lookahead_buffer_r: vec![0.0; lookahead_buffer_size],
            lookahead_write_index: 0,
            lookahead_read_delay,
        }
    }

    // --- Parameter Setters ---
    pub fn set_threshold_db(&mut self, db: f32) {
        self.target_threshold_db = db;
        // Update linear threshold immediately - smoothing gain is enough
        self.current_threshold_linear = db_to_linear(db);
    }

    pub fn set_attack_ms(&mut self, ms: f32) {
        let safe_ms = ms.max(0.0); // Ensure non-negative
        self.target_attack_ms = safe_ms;
        self.attack_coeff = calculate_smooth_coeff(safe_ms, self.sample_rate);
    }

    pub fn set_release_ms(&mut self, ms: f32) {
        let safe_ms = ms.max(f32::EPSILON * 1000.0); // Ensure positive
        self.target_release_ms = safe_ms;
        self.release_coeff = calculate_smooth_coeff(safe_ms, self.sample_rate);
    }

    pub fn set_stereo_link(&mut self, link: bool) {
        self.target_stereo_link = link;
        // If unlinking, reset gains potentially? Or let them diverge naturally.
        // Let's let them diverge naturally for now. Reset on activation might be better.
    }

    // Note: Changing lookahead requires resizing buffers, which is complex.
    // Usually, lookahead is fixed after initialization or requires a full reset/recreation.
    // We'll omit a `set_lookahead_ms` for simplicity here. If needed, it would
    // call `reset_state` and reallocate/recalculate lookahead parameters.

    // --- MAIN PROCESS FUNCTION ---
    #[inline(never)]
    pub fn process_block<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        if buffer_size == 0 {
            return;
        }

        // Borrow both outputs at once (non-overlapping)
        let [mut l_opt, mut r_opt] =
            outputs.get_disjoint_mut([&PortId::AudioOutput0, &PortId::AudioOutput1]);

        // Handle missing outputs with early returns (reborrow via as_deref_mut)
        if l_opt.is_none() && r_opt.is_none() {
            eprintln!("Limiter Error: Missing both outputs");
            return;
        }
        if let (Some(l), None) = (l_opt.as_deref_mut(), r_opt.as_deref_mut()) {
            eprintln!("Limiter Error: Missing AudioOutput1");
            let fill_len = buffer_size.min(l.len());
            l[..fill_len].fill(0.0);
            return;
        }
        if let (None, Some(r)) = (l_opt.as_deref_mut(), r_opt.as_deref_mut()) {
            eprintln!("Limiter Error: Missing AudioOutput0");
            let fill_len = buffer_size.min(r.len());
            r[..fill_len].fill(0.0);
            return;
        }

        // From here both exist; reborrow as &mut [f32] without moving
        let out_left_full: &mut [f32] = l_opt.unwrap();
        let out_right_full: &mut [f32] = r_opt.unwrap();

        // Inputs
        let left_source = match inputs.get(&PortId::AudioInput0).and_then(|v| v.first()) {
            Some(src) => src,
            None => {
                let fill_len = buffer_size
                    .min(out_left_full.len())
                    .min(out_right_full.len());
                if fill_len > 0 {
                    out_left_full[..fill_len].fill(0.0);
                    out_right_full[..fill_len].fill(0.0);
                }
                if fill_len < out_left_full.len() {
                    out_left_full[fill_len..].fill(0.0);
                }
                if fill_len < out_right_full.len() {
                    out_right_full[fill_len..].fill(0.0);
                }
                return;
            }
        };
        let left_in_slice = left_source.buffer;

        let right_in_slice = inputs
            .get(&PortId::AudioInput1)
            .and_then(|v| v.first())
            .map_or(left_in_slice, |s| s.buffer);

        // Work length
        let process_len = buffer_size
            .min(out_left_full.len())
            .min(out_right_full.len())
            .min(left_in_slice.len())
            .min(right_in_slice.len());

        // Disabled passthrough: copy in→out, zero tail if any
        if !self.enabled {
            if process_len > 0 {
                out_left_full[..process_len].copy_from_slice(&left_in_slice[..process_len]);
                out_right_full[..process_len].copy_from_slice(&right_in_slice[..process_len]);
            }
            if process_len < out_left_full.len() {
                out_left_full[process_len..].fill(0.0);
            }
            if process_len < out_right_full.len() {
                out_right_full[process_len..].fill(0.0);
            }
            return;
        }

        if process_len == 0 {
            // Nothing to do; zero up to requested buffer_size if there’s space
            let fill_l = buffer_size.min(out_left_full.len());
            let fill_r = buffer_size.min(out_right_full.len());
            if fill_l > 0 {
                out_left_full[..fill_l].fill(0.0);
            }
            if fill_r > 0 {
                out_right_full[..fill_r].fill(0.0);
            }
            return;
        }

        let (out_left, out_right) = (
            &mut out_left_full[..process_len],
            &mut out_right_full[..process_len],
        );
        out_left.copy_from_slice(&left_in_slice[..process_len]);
        out_right.copy_from_slice(&right_in_slice[..process_len]);
        self.process_stereo(out_left, out_right);

        // Zero any remaining tail past process_len
        if process_len < out_left_full.len() {
            out_left_full[process_len..].fill(0.0);
        }
        if process_len < out_right_full.len() {
            out_right_full[process_len..].fill(0.0);
        }
    }

    /// Resets the internal state of the Limiter node.
    pub fn reset_state(&mut self) {
        self.envelope_l = 0.0;
        self.envelope_r = 0.0;
        self.gain_reduction_l = 1.0;
        self.gain_reduction_r = 1.0;

        // Reset lookahead buffer state
        self.lookahead_buffer_l.fill(0.0);
        self.lookahead_buffer_r.fill(0.0);
        self.lookahead_write_index = 0;

        // Recalculate coefficients and linear threshold based on current targets
        self.attack_coeff = calculate_smooth_coeff(self.target_attack_ms, self.sample_rate);
        self.release_coeff = calculate_smooth_coeff(self.target_release_ms, self.sample_rate);
        self.current_threshold_linear = db_to_linear(self.target_threshold_db);
        // Note: Lookahead samples/delay remain unchanged unless explicitly recalculated.
    }

    // --- Methods required by AudioNode trait implementation below ---
    fn node_type_str(&self) -> &str {
        "limiter"
    }
    fn is_node_active(&self) -> bool {
        self.enabled
    }
    fn set_node_active(&mut self, active: bool) {
        if active && !self.enabled {
            // Reset state when activating to avoid pops/clicks from old state
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
} // impl Limiter

impl StereoEffect for Limiter {
    fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let process_len = left.len().min(right.len());

        // --- Core limiter state pulled into locals ---
        let threshold_lin = self.current_threshold_linear;
        let attack_c = self.attack_coeff;
        let release_c = self.release_coeff;
        let stereo_link = self.target_stereo_link;

        let mut envelope_l = self.envelope_l;
        let mut envelope_r = self.envelope_r;
        let mut gain_reduction_l = self.gain_reduction_l;
        let mut gain_reduction_r = self.gain_reduction_r;

        let lookahead_buf_l = &mut self.lookahead_buffer_l;
        let lookahead_buf_r = &mut self.lookahead_buffer_r;
        let lookahead_buf_len = lookahead_buf_l.len();
        let mut write_idx = self.lookahead_write_index;
        let read_delay = self.lookahead_read_delay;
        let has_lookahead = read_delay > 0 && lookahead_buf_len > 1;

        for i in 0..process_len {
            let input_l = left[i];
            let input_r = right[i];

            // Lookahead
            let (delayed_l, delayed_r) = if has_lookahead {
                let read_idx = (write_idx + lookahead_buf_len - read_delay) % lookahead_buf_len;
                let dl = lookahead_buf_l[read_idx];
                let dr = lookahead_buf_r[read_idx];
                lookahead_buf_l[write_idx] = input_l;
                lookahead_buf_r[write_idx] = input_r;
                write_idx = (write_idx + 1) % lookahead_buf_len;
                (dl, dr)
            } else {
                (input_l, input_r)
            };

            // Peak envelope
            let peak_l = input_l.abs();
            let peak_r = input_r.abs();

            if peak_l > envelope_l {
                envelope_l = peak_l * (1.0 - attack_c) + envelope_l * attack_c;
            } else {
                envelope_l = peak_l * (1.0 - release_c) + envelope_l * release_c;
            }
            envelope_l += f32::EPSILON * 0.1;

            if peak_r > envelope_r {
                envelope_r = peak_r * (1.0 - attack_c) + envelope_r * attack_c;
            } else {
                envelope_r = peak_r * (1.0 - release_c) + envelope_r * release_c;
            }
            envelope_r += f32::EPSILON * 0.1;

            // Target gain(s)
            let (target_gain_l, target_gain_r) = if stereo_link {
                let max_env = envelope_l.max(envelope_r);
                if max_env > threshold_lin {
                    let g = (threshold_lin / max_env).min(1.0);
                    (g, g)
                } else {
                    (1.0, 1.0)
                }
            } else {
                let gl = if envelope_l > threshold_lin {
                    (threshold_lin / envelope_l).min(1.0)
                } else {
                    1.0
                };
                let gr = if envelope_r > threshold_lin {
                    (threshold_lin / envelope_r).min(1.0)
                } else {
                    1.0
                };
                (gl, gr)
            };

            // Smooth gain
            if target_gain_l < gain_reduction_l {
                gain_reduction_l = target_gain_l * (1.0 - attack_c) + gain_reduction_l * attack_c;
            } else {
                gain_reduction_l = target_gain_l * (1.0 - release_c) + gain_reduction_l * release_c;
            }
            if target_gain_r < gain_reduction_r {
                gain_reduction_r = target_gain_r * (1.0 - attack_c) + gain_reduction_r * attack_c;
            } else {
                gain_reduction_r = target_gain_r * (1.0 - release_c) + gain_reduction_r * release_c;
            }

            gain_reduction_l = gain_reduction_l.min(1.0);
            gain_reduction_r = gain_reduction_r.min(1.0);

            // Apply
            left[i] = delayed_l * gain_reduction_l;
            right[i] = delayed_r * gain_reduction_r;
        }

        // Write back state
        self.envelope_l = envelope_l;
        self.envelope_r = envelope_r;
        self.gain_reduction_l = gain_reduction_l;
        self.gain_reduction_r = gain_reduction_r;
        if has_lookahead {
            self.lookahead_write_index = write_idx;
        }
    }
}

// --- AudioNode Trait Implementation ---
impl AudioNode for Limiter {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        FxHashMap::from_iter([
            (PortId::AudioInput0, false),
            (PortId::AudioInput1, false),
            (PortId::AudioOutput0, true),
            (PortId::AudioOutput1, true),
        ])
    }

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
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
        "Limiter"
    }
    fn node_type(&self) -> &str {
        self.node_type_str()
    }
    fn as_stereo_effect(&mut self) -> Option<&mut dyn StereoEffect> {
        Some(self)
    }
}
