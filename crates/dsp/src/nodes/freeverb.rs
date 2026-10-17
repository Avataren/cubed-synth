use super::copy_input;
use crate::traits::{AudioNode, PortId, StereoEffect};
use crate::traits::{Inputs, Outputs};
use cubed_simd::f32x4;
use cubed_simd::num::SimdFloat;
use rustc_hash::FxHashMap;
use std::any::Any;

/// A single comb filter with a one-pole lowpass in the feedback loop.
struct CombFilter {
    buffer: Vec<f32>,
    buffer_size: usize,
    index: usize,
    feedback: f32,
    filter_store: f32,
    damp1: f32, // Usually equal to the damp parameter (0..1)
    damp2: f32, // 1 - damp1
}

impl CombFilter {
    /// Create a new comb filter.
    pub fn new(delay_samples: usize, feedback: f32, damp: f32) -> Self {
        let damp1 = damp.clamp(0.0, 1.0);
        let damp2 = 1.0 - damp1;
        Self {
            buffer: vec![0.0; delay_samples],
            buffer_size: delay_samples,
            index: 0,
            feedback,
            filter_store: 0.0,
            damp1,
            damp2,
        }
    }

    /// Process a single sample.
    #[inline(always)]
    pub fn process(&mut self, input: f32) -> f32 {
        let output = self.buffer[self.index];
        // One-pole lowpass for internal damping.
        self.filter_store = output * self.damp2 + self.filter_store * self.damp1;
        // Write back new input plus feedback.
        self.buffer[self.index] = input + self.filter_store * self.feedback;
        self.index = (self.index + 1) % self.buffer_size;
        output
    }

    /// Reset internal state.
    pub fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.filter_store = 0.0;
        self.index = 0;
    }
}

/// A single allpass filter.
struct AllpassFilter {
    buffer: Vec<f32>,
    buffer_size: usize,
    index: usize,
    feedback: f32, // Typically 0.5 in Freeverb.
}

impl AllpassFilter {
    /// Create a new allpass filter.
    pub fn new(delay_samples: usize, feedback: f32) -> Self {
        Self {
            buffer: vec![0.0; delay_samples],
            buffer_size: delay_samples,
            index: 0,
            feedback,
        }
    }

    /// Process one sample.
    #[inline(always)]
    pub fn process(&mut self, input: f32) -> f32 {
        let bufout = self.buffer[self.index];
        let output = bufout - input;
        self.buffer[self.index] = input + bufout * self.feedback;
        self.index = (self.index + 1) % self.buffer_size;
        output
    }

    /// Reset the filter.
    pub fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.index = 0;
    }
}

/// The Freeverb reverb node.
pub struct Freeverb {
    enabled: bool,

    // Parameters
    room_size: f32, // 0..1; controls comb filter feedback.
    damp: f32,      // 0..1; dampening inside comb filters.
    wet: f32,       // Wet mix level.
    dry: f32,       // Dry mix level.
    width: f32,     // Stereo width (0 = narrow, 1 = wide).

    // 8 comb filters per channel.
    comb_filters_l: Vec<CombFilter>,
    comb_filters_r: Vec<CombFilter>,

    // 4 allpass filters per channel.
    allpass_filters_l: Vec<AllpassFilter>,
    allpass_filters_r: Vec<AllpassFilter>,

    // Precomputed stereo wet mix coefficients.
    wet1: f32,
    wet2: f32,
}

impl Freeverb {
    /// Create a new Freeverb node.
    ///
    /// * `sample_rate` - Audio sample rate in Hz.
    /// * `room_size` - 0 to 1 value controlling reverb time.
    /// * `damp` - 0 to 1 value controlling lowpass damping.
    /// * `wet` - Processed (wet) level.
    /// * `dry` - Unprocessed (dry) level.
    /// * `width` - Stereo width (0 = narrow, 1 = wide).
    pub fn new(
        sample_rate: f32,
        room_size: f32,
        damp: f32,
        wet: f32,
        dry: f32,
        width: f32,
    ) -> Self {
        let room_size = room_size.clamp(0.0, 1.0);
        let damp = damp.clamp(0.0, 1.0);
        let wet = wet.clamp(0.0, 1.0);
        let dry = dry.clamp(0.0, 1.0);
        let width = width.clamp(0.0, 1.0);

        // Original Freeverb delay lengths (for 44.1kHz)
        let comb_tunings_l = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
        // Right channel: slightly offset delays for stereo.
        let comb_tunings_r: Vec<i32> = comb_tunings_l.iter().map(|&t| t + 23).collect();
        let scale = sample_rate / 44100.0;
        let comb_filters_l: Vec<CombFilter> = comb_tunings_l
            .iter()
            .map(|&t| {
                let delay_samples = ((t as f32) * scale).round() as usize;
                CombFilter::new(delay_samples, room_size, damp)
            })
            .collect();
        let comb_filters_r: Vec<CombFilter> = comb_tunings_r
            .iter()
            .map(|&t| {
                let delay_samples = ((t as f32) * scale).round() as usize;
                CombFilter::new(delay_samples, room_size, damp)
            })
            .collect();

        let allpass_tunings = [556, 441, 341, 225];
        let allpass_filters_l: Vec<AllpassFilter> = allpass_tunings
            .iter()
            .map(|&t| {
                let delay_samples = ((t as f32) * scale).round() as usize;
                AllpassFilter::new(delay_samples, 0.5)
            })
            .collect();
        let allpass_filters_r: Vec<AllpassFilter> = allpass_tunings
            .iter()
            .map(|&t| {
                let delay_samples = ((t as f32) * scale).round() as usize;
                AllpassFilter::new(delay_samples, 0.5)
            })
            .collect();

        // Stereo wet mix coefficients.
        let wet1 = wet * (width / 2.0 + 0.5);
        let wet2 = wet * (0.5 - width / 2.0);

        Self {
            enabled: true,
            room_size,
            damp,
            wet,
            dry,
            width,
            comb_filters_l,
            comb_filters_r,
            allpass_filters_l,
            allpass_filters_r,
            wet1,
            wet2,
        }
    }

    /// Adjust the room size (affecting comb filter feedback).
    pub fn set_room_size(&mut self, room_size: f32) {
        self.room_size = room_size.clamp(0.0, 1.0);
        for comb in self.comb_filters_l.iter_mut() {
            comb.feedback = self.room_size;
        }
        for comb in self.comb_filters_r.iter_mut() {
            comb.feedback = self.room_size;
        }
    }

    /// Adjust the damping.
    pub fn set_damp(&mut self, damp: f32) {
        self.damp = damp.clamp(0.0, 1.0);
        for comb in self.comb_filters_l.iter_mut() {
            comb.damp1 = self.damp;
            comb.damp2 = 1.0 - self.damp;
        }
        for comb in self.comb_filters_r.iter_mut() {
            comb.damp1 = self.damp;
            comb.damp2 = 1.0 - self.damp;
        }
    }

    /// Adjust the wet mix.
    pub fn set_wet(&mut self, wet: f32) {
        self.wet = wet.clamp(0.0, 1.0);
        self.update_wet_mix();
    }

    /// Adjust the dry mix.
    pub fn set_dry(&mut self, dry: f32) {
        self.dry = dry.clamp(0.0, 1.0);
    }

    /// Adjust the stereo width.
    pub fn set_width(&mut self, width: f32) {
        self.width = width.clamp(0.0, 1.0);
        self.update_wet_mix();
    }

    fn update_wet_mix(&mut self) {
        self.wet1 = self.wet * (self.width / 2.0 + 0.5);
        self.wet2 = self.wet * (0.5 - self.width / 2.0);
    }

    /// Reset all filters.
    pub fn reset_filters(&mut self) {
        for comb in self.comb_filters_l.iter_mut() {
            comb.reset();
        }
        for comb in self.comb_filters_r.iter_mut() {
            comb.reset();
        }
        for ap in self.allpass_filters_l.iter_mut() {
            ap.reset();
        }
        for ap in self.allpass_filters_r.iter_mut() {
            ap.reset();
        }
    }
}

impl StereoEffect for Freeverb {
    fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let buffer_size = left.len().min(right.len());
        // Fixed gain constant to scale down each channel.
        let fixed_gain = 0.015;

        let simd_block = 4;
        let mut i = 0;
        while i < buffer_size {
            let block_size = if buffer_size - i >= simd_block {
                simd_block
            } else {
                buffer_size - i
            };

            let mut temp_reverb_l = [0.0f32; 4];
            let mut temp_reverb_r = [0.0f32; 4];

            // Process per sample in the current block.
            for j in 0..block_size {
                let idx = i + j;
                // Apply fixed gain to each channel separately.
                let input_l = left[idx] * fixed_gain;
                let input_r = right[idx] * fixed_gain;

                // --- Comb Filter Processing for Left Channel ---
                let c0 = self.comb_filters_l[0].process(input_l);
                let c1 = self.comb_filters_l[1].process(input_l);
                let c2 = self.comb_filters_l[2].process(input_l);
                let c3 = self.comb_filters_l[3].process(input_l);
                let c4 = self.comb_filters_l[4].process(input_l);
                let c5 = self.comb_filters_l[5].process(input_l);
                let c6 = self.comb_filters_l[6].process(input_l);
                let c7 = self.comb_filters_l[7].process(input_l);
                let vec_l1 = f32x4::from_array([c0, c1, c2, c3]);
                let vec_l2 = f32x4::from_array([c4, c5, c6, c7]);
                let comb_sum_l = vec_l1.reduce_sum() + vec_l2.reduce_sum();

                // --- Comb Filter Processing for Right Channel ---
                let cr0 = self.comb_filters_r[0].process(input_r);
                let cr1 = self.comb_filters_r[1].process(input_r);
                let cr2 = self.comb_filters_r[2].process(input_r);
                let cr3 = self.comb_filters_r[3].process(input_r);
                let cr4 = self.comb_filters_r[4].process(input_r);
                let cr5 = self.comb_filters_r[5].process(input_r);
                let cr6 = self.comb_filters_r[6].process(input_r);
                let cr7 = self.comb_filters_r[7].process(input_r);
                let vec_r1 = f32x4::from_array([cr0, cr1, cr2, cr3]);
                let vec_r2 = f32x4::from_array([cr4, cr5, cr6, cr7]);
                let comb_sum_r = vec_r1.reduce_sum() + vec_r2.reduce_sum();

                // --- Allpass Filter Processing ---
                let mut reverb_l = comb_sum_l;
                for ap in self.allpass_filters_l.iter_mut() {
                    reverb_l = ap.process(reverb_l);
                }
                let mut reverb_r = comb_sum_r;
                for ap in self.allpass_filters_r.iter_mut() {
                    reverb_r = ap.process(reverb_r);
                }

                // --- Apply phase decorrelation to the reverb outputs ---
                let phase_shift = 0.02;
                let new_reverb_r = reverb_r + reverb_l * phase_shift;
                let new_reverb_l = reverb_l - new_reverb_r * phase_shift;
                temp_reverb_l[j] = new_reverb_l;
                temp_reverb_r[j] = new_reverb_r;
            }

            // --- Final Mixing of Dry and Wet Signals ---
            if block_size == simd_block {
                let in_left_block = f32x4::from_slice(&left[i..i + simd_block]);
                let in_right_block = f32x4::from_slice(&right[i..i + simd_block]);
                let dry_left = in_left_block * f32x4::splat(self.dry);
                let dry_right = in_right_block * f32x4::splat(self.dry);
                let reverb_l_vec = f32x4::from_array(temp_reverb_l);
                let reverb_r_vec = f32x4::from_array(temp_reverb_r);
                let wet1_vec = f32x4::splat(self.wet1);
                let wet2_vec = f32x4::splat(self.wet2);

                let mixed_left = dry_left + wet1_vec * reverb_l_vec + wet2_vec * reverb_r_vec;
                let mixed_right = dry_right + wet1_vec * reverb_r_vec + wet2_vec * reverb_l_vec;
                mixed_left.copy_to_slice(&mut left[i..i + simd_block]);
                mixed_right.copy_to_slice(&mut right[i..i + simd_block]);
            } else {
                for j in 0..block_size {
                    let idx = i + j;
                    let dry_l = left[idx] * self.dry;
                    let dry_r = right[idx] * self.dry;
                    left[idx] = dry_l + self.wet1 * temp_reverb_l[j] + self.wet2 * temp_reverb_r[j];
                    right[idx] =
                        dry_r + self.wet1 * temp_reverb_r[j] + self.wet2 * temp_reverb_l[j];
                }
            }
            i += block_size;
        }
    }
}

impl AudioNode for Freeverb {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        let mut ports = FxHashMap::default();
        ports.insert(PortId::AudioInput0, false); // Left input
        ports.insert(PortId::AudioInput1, false); // Right input
        ports.insert(PortId::AudioOutput0, true); // Left output
        ports.insert(PortId::AudioOutput1, true); // Right output
        ports
    }

    fn process<'a>(&mut self, inputs: &Inputs<'a>, outputs: &mut Outputs<'_>, buffer_size: usize) {
        // Retrieve input buffers; if absent, use a zero buffer.
        let left_in = inputs
            .get(&PortId::AudioInput0)
            .and_then(|sources| sources.first())
            .map(|src| &src.buffer[..buffer_size])
            .unwrap_or_else(|| {
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
        self.reset_filters();
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
        "Reverb"
    }

    fn node_type(&self) -> &str {
        "freeverb"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ModulationSource, ModulationTransformation, ModulationType};
    use crate::traits::{AudioNode, PortId};
    use crate::PortMap;

    /// Test Freeverb when only the dry signal is active.
    #[test]
    fn test_freeverb_dry_only() {
        // Using a common sample rate.
        let sample_rate = 44100.0;
        // For a dry-only test: wet = 0, dry = 1.
        let room_size = 0.5;
        let damp = 0.5;
        let wet = 0.0;
        let dry = 1.0;
        let width = 0.5;

        let mut freeverb = Freeverb::new(sample_rate, room_size, damp, wet, dry, width);

        let buffer_size = 1024;
        // Create constant input buffers.
        let input_left = vec![1.0; buffer_size];
        let input_right = vec![1.0; buffer_size];

        let mut output_left = vec![0.0; buffer_size];
        let mut output_right = vec![0.0; buffer_size];

        // Build input modulation sources.
        let mut inputs: PortMap<Vec<ModulationSource>> = PortMap::default();
        inputs.insert(
            PortId::AudioInput0,
            vec![ModulationSource {
                buffer: &input_left,
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );
        inputs.insert(
            PortId::AudioInput1,
            vec![ModulationSource {
                buffer: &input_right,
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );

        // Create outputs map inside a scoped block so that the mutable borrows don't last longer than needed.
        {
            let mut outputs = Outputs::default();
            outputs.insert(PortId::AudioOutput0, output_left.as_mut_slice());
            outputs.insert(PortId::AudioOutput1, output_right.as_mut_slice());

            // Process one block.
            freeverb.process(&inputs.as_inputs(), &mut outputs, buffer_size);
        }

        // With wet=0 and dry=1, the processed output should equal the input exactly.
        for (i, sample) in output_left.iter().enumerate() {
            assert!(
                (sample - 1.0).abs() < 1e-6,
                "Left channel sample {} expected 1.0 but got {}",
                i,
                sample
            );
        }
        for (i, sample) in output_right.iter().enumerate() {
            assert!(
                (sample - 1.0).abs() < 1e-6,
                "Right channel sample {} expected 1.0 but got {}",
                i,
                sample
            );
        }
    }

    /// Test Freeverb with wet-only mode and multiple processing blocks to allow delay buffers to warm up.
    #[test]
    fn test_freeverb_wet_only_warmup() {
        let sample_rate = 44100.0;
        // Using a wet-only configuration (dry = 0, wet = 1)
        let room_size = 0.5;
        let damp = 0.5;
        let wet = 1.0;
        let dry = 0.0;
        let width = 0.5;

        let mut freeverb = Freeverb::new(sample_rate, room_size, damp, wet, dry, width);

        let buffer_size = 1024;
        // Constant input of ones.
        let input_left = vec![1.0; buffer_size];
        let input_right = vec![1.0; buffer_size];

        let mut output_left = vec![0.0; buffer_size];
        let mut output_right = vec![0.0; buffer_size];

        let mut inputs: PortMap<Vec<ModulationSource>> = PortMap::default();
        inputs.insert(
            PortId::AudioInput0,
            vec![ModulationSource {
                buffer: &input_left,
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );
        inputs.insert(
            PortId::AudioInput1,
            vec![ModulationSource {
                buffer: &input_right,
                amount: 1.0,
                mod_type: ModulationType::Additive,
                transformation: ModulationTransformation::None,
            }],
        );

        let num_blocks = 10;
        let mut accumulated_left = Vec::new();
        let mut accumulated_right = Vec::new();

        // For each processing block, create a fresh outputs map so the mutable borrow is limited.
        for _ in 0..num_blocks {
            {
                let mut outputs = Outputs::default();
                outputs.insert(PortId::AudioOutput0, output_left.as_mut_slice());
                outputs.insert(PortId::AudioOutput1, output_right.as_mut_slice());
                freeverb.process(&inputs.as_inputs(), &mut outputs, buffer_size);
            }
            // Now it is safe to borrow the output buffers immutably.
            accumulated_left.extend_from_slice(&output_left);
            accumulated_right.extend_from_slice(&output_right);
        }

        // With dry=0 and wet=1, after warming up the reverb chain should produce a nonzero sum.
        let left_sum: f32 = accumulated_left.iter().sum();
        let right_sum: f32 = accumulated_right.iter().sum();

        assert!(
            left_sum.abs() > 0.0,
            "Expected nonzero sum on left channel reverb, got {}",
            left_sum
        );
        assert!(
            right_sum.abs() > 0.0,
            "Expected nonzero sum on right channel reverb, got {}",
            right_sum
        );
    }

    /// Test the comb filter's warm-up behavior in isolation.
    #[test]
    fn test_comb_filter_warmup() {
        let delay_samples = 100;
        let feedback = 0.5;
        let damp = 0.5;
        let mut comb = CombFilter::new(delay_samples, feedback, damp);

        // For the first delay_samples calls the buffer (initialized with zeros) returns 0.
        for i in 0..delay_samples {
            let output = comb.process(1.0);
            assert!(
                (output - 0.0).abs() < 1e-6,
                "Comb filter at sample {} expected 0 but got {}",
                i,
                output
            );
        }
        // After the initial delay, the filter state should be nontrivial.
        let output = comb.process(1.0);
        // We don't know the exact value, but it should be a finite number.
        assert!(
            !output.is_nan(),
            "Comb filter output after delay wrapping should be a valid number"
        );
    }

    /// Test the allpass filter's response in isolation.
    /// Test the allpass filter's response in isolation.
    #[test]
    fn test_allpass_filter_response() {
        let delay_samples = 50;
        let feedback = 0.5;
        let mut allpass = AllpassFilter::new(delay_samples, feedback);

        // For the first delay_samples calls, the allpass filter outputs -1 (since each element is initially 0).
        for i in 0..delay_samples {
            let output = allpass.process(1.0);
            assert!(
                (output + 1.0).abs() < 1e-6,
                "Allpass filter at sample {} expected -1 but got {}",
                i,
                output
            );
        }
        // After a full cycle, the buffer has been warmed up:
        // At this point, the index wraps around. Now the output should be 1.0 (stored in the buffer)
        // minus the input 1.0, i.e. 0.
        let output = allpass.process(1.0);
        assert!(
            (output - 0.0).abs() < 1e-6,
            "Allpass filter at sample {} expected 0 but got {}",
            delay_samples,
            output
        );
    }
}
