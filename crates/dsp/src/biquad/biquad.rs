use std::f64::consts::PI as PI64;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::wasm_bindgen;

#[cfg_attr(feature = "wasm", wasm_bindgen)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterType {
    LowPass,
    LowShelf,
    Peaking,
    HighShelf,
    Notch,
    HighPass,
    Ladder,
    Comb,
    BandPass,
}

pub trait Filter {
    fn process(&mut self, input: f32) -> f32;
    fn reset(&mut self);
}

#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    pub filter_type: FilterType,
    pub sample_rate: f32,
    pub frequency: f32,
    pub q: f32,
    pub gain_db: f32,
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    pub fn new(
        filter_type: FilterType,
        sample_rate: f32,
        frequency: f32,
        q: f32,
        gain_db: f32,
    ) -> Self {
        let mut filter = Self {
            filter_type,
            sample_rate: sample_rate.max(1.0),
            frequency: frequency.clamp(10.0, sample_rate * 0.4999),
            q: q.max(0.01),
            gain_db,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        };
        filter.update_coefficients();
        filter
    }

    #[allow(dead_code)]
    fn normalize_coeffs(&mut self) {
        let norm_factor = match self.filter_type {
            FilterType::LowPass | FilterType::Notch => {
                (self.b0 + self.b1 + self.b2) / (1.0 + self.a1 + self.a2)
            }
            FilterType::HighPass => (self.b0 - self.b1 + self.b2) / (1.0 - self.a1 + self.a2),
            _ => 1.0,
        };
        if norm_factor.abs() > 1e-8 {
            self.b0 /= norm_factor;
            self.b1 /= norm_factor;
            self.b2 /= norm_factor;
        }
    }

    pub fn update_coefficients(&mut self) {
        let sr64 = self.sample_rate as f64;
        let freq64 = self.frequency.clamp(1.0, self.sample_rate * 0.49999) as f64;
        let q64 = self.q.max(0.001) as f64;
        let gain_db64 = self.gain_db as f64;
        let a_lin64 = 10.0_f64.powf(gain_db64 / 20.0);
        let a_sqrt64 = 10.0_f64.powf(gain_db64 / 40.0);

        let omega64 = 2.0 * PI64 * freq64 / sr64;
        let sn64 = omega64.sin();
        let cs64 = omega64.cos();
        let alpha64 = sn64 / (2.0 * q64);
        let two_cs64 = 2.0 * cs64;

        // Initialize f64 temp variables to satisfy the compiler
        let mut b0_64: f64 = 0.0;
        let mut b1_64: f64 = 0.0;
        let mut b2_64: f64 = 0.0;
        let mut a0_64: f64 = 1.0; // Initialize a0 to 1.0 to avoid division by zero if path not taken
        let mut a1_64: f64 = 0.0;
        let mut a2_64: f64 = 0.0;

        let mut use_direct_assignment = false;

        match self.filter_type {
            FilterType::LowPass => {
                let omega_half64 = omega64 / 2.0;
                let sn_half64 = omega_half64.sin();
                let a0_val = 1.0 + alpha64; // Calculate a0 for normalization factor
                if a0_val.abs() < 1e-12 {
                    // Handle error case directly, maybe set to pass-through
                    self.b0 = 1.0;
                    self.b1 = 0.0;
                    self.b2 = 0.0;
                    self.a1 = 0.0;
                    self.a2 = 0.0;
                } else {
                    let scale = 1.0 / a0_val;
                    let sn_half_sq = sn_half64 * sn_half64;
                    self.b0 = (sn_half_sq * scale) as f32;
                    self.b1 = (2.0 * sn_half_sq * scale) as f32;
                    self.b2 = (sn_half_sq * scale) as f32;
                    self.a1 = ((-two_cs64) * scale) as f32;
                    self.a2 = ((1.0 - alpha64) * scale) as f32;
                }
                use_direct_assignment = true;
            }
            FilterType::HighPass => {
                let omega_half64 = omega64 / 2.0;
                let cs_half64 = omega_half64.cos();
                let a0_val = 1.0 + alpha64; // Calculate a0 for normalization factor
                if a0_val.abs() < 1e-12 {
                    // Handle error case directly
                    self.b0 = 1.0;
                    self.b1 = 0.0;
                    self.b2 = 0.0;
                    self.a1 = 0.0;
                    self.a2 = 0.0;
                } else {
                    let scale = 1.0 / a0_val;
                    let cs_half_sq = cs_half64 * cs_half64;
                    self.b0 = (cs_half_sq * scale) as f32;
                    self.b1 = (-2.0 * cs_half_sq * scale) as f32;
                    self.b2 = (cs_half_sq * scale) as f32;
                    self.a1 = ((-two_cs64) * scale) as f32;
                    self.a2 = ((1.0 - alpha64) * scale) as f32;
                }
                use_direct_assignment = true;
            }
            FilterType::BandPass => {
                b0_64 = alpha64;
                b1_64 = 0.0;
                b2_64 = -alpha64;
                a0_64 = 1.0 + alpha64;
                a1_64 = -two_cs64;
                a2_64 = 1.0 - alpha64;
            }
            FilterType::Notch => {
                b0_64 = 1.0;
                b1_64 = -two_cs64;
                b2_64 = 1.0;
                a0_64 = 1.0 + alpha64;
                a1_64 = -two_cs64;
                a2_64 = 1.0 - alpha64;
            }
            FilterType::Peaking => {
                b0_64 = 1.0 + alpha64 * a_lin64;
                b1_64 = -two_cs64;
                b2_64 = 1.0 - alpha64 * a_lin64;
                a0_64 = 1.0 + alpha64 / a_lin64;
                a1_64 = -two_cs64;
                a2_64 = 1.0 - alpha64 / a_lin64;
            }
            FilterType::LowShelf => {
                let beta64 = 2.0 * a_sqrt64 * alpha64;
                let a_plus_1 = a_lin64 + 1.0;
                let a_minus_1 = a_lin64 - 1.0;

                b0_64 = a_lin64 * (a_plus_1 - a_minus_1 * cs64 + beta64);
                b1_64 = 2.0 * a_lin64 * (a_minus_1 - a_plus_1 * cs64);
                b2_64 = a_lin64 * (a_plus_1 - a_minus_1 * cs64 - beta64);
                a0_64 = a_plus_1 + a_minus_1 * cs64 + beta64;
                a1_64 = -2.0 * (a_minus_1 + a_plus_1 * cs64);
                a2_64 = a_plus_1 + a_minus_1 * cs64 - beta64;
            }
            FilterType::HighShelf => {
                let beta64 = 2.0 * a_sqrt64 * alpha64;
                let a_plus_1 = a_lin64 + 1.0;
                let a_minus_1 = a_lin64 - 1.0;

                b0_64 = a_lin64 * (a_plus_1 + a_minus_1 * cs64 + beta64);
                b1_64 = -2.0 * a_lin64 * (a_minus_1 + a_plus_1 * cs64);
                b2_64 = a_lin64 * (a_plus_1 + a_minus_1 * cs64 - beta64);
                a0_64 = a_plus_1 - a_minus_1 * cs64 + beta64;
                a1_64 = 2.0 * (a_minus_1 - a_plus_1 * cs64);
                a2_64 = a_plus_1 - a_minus_1 * cs64 - beta64;
            }
            FilterType::Ladder | FilterType::Comb => {
                // Set to pass-through (will be handled by the final check anyway)
                self.b0 = 1.0;
                self.b1 = 0.0;
                self.b2 = 0.0;
                self.a1 = 0.0;
                self.a2 = 0.0;
                use_direct_assignment = true; // Skip normalization path for these
            }
        }

        if !use_direct_assignment {
            if a0_64.abs() < 1e-12 {
                self.b0 = 1.0;
                self.b1 = 0.0;
                self.b2 = 0.0;
                self.a1 = 0.0;
                self.a2 = 0.0;
            } else {
                let a0_inv = 1.0 / a0_64;
                self.b0 = (b0_64 * a0_inv) as f32;
                self.b1 = (b1_64 * a0_inv) as f32;
                self.b2 = (b2_64 * a0_inv) as f32;
                self.a1 = (a1_64 * a0_inv) as f32;
                self.a2 = (a2_64 * a0_inv) as f32;
            }
        }

        // Final safety check applies to all paths
        if !self.b0.is_finite()
            || !self.b1.is_finite()
            || !self.b2.is_finite()
            || !self.a1.is_finite()
            || !self.a2.is_finite()
        {
            self.b0 = 1.0;
            self.b1 = 0.0;
            self.b2 = 0.0;
            self.a1 = 0.0;
            self.a2 = 0.0;
        }
    }

    #[inline(always)]
    pub fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;

        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        self.y1 = if output.abs() < 1e-18 { 0.0 } else { output };

        self.y1
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

impl Filter for Biquad {
    #[inline(always)]
    fn process(&mut self, input: f32) -> f32 {
        self.process(input)
    }
    fn reset(&mut self) {
        self.reset()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CascadedBiquad {
    pub first: Biquad,
    pub second: Biquad,
}

impl CascadedBiquad {
    pub fn new(
        filter_type: FilterType,
        sample_rate: f32,
        frequency: f32,
        q: f32,
        gain_db: f32,
    ) -> Self {
        let q_stage = q.max(0.501);
        Self {
            first: Biquad::new(filter_type, sample_rate, frequency, q_stage, 0.0),
            second: Biquad::new(filter_type, sample_rate, frequency, q_stage, gain_db),
        }
    }

    pub fn new_with_gain_split(
        filter_type: FilterType,
        sample_rate: f32,
        frequency: f32,
        q: f32,
        gain_db1: f32,
        gain_db2: f32,
    ) -> Self {
        let q_stage = q.max(0.501);
        Self {
            first: Biquad::new(filter_type, sample_rate, frequency, q_stage, gain_db1),
            second: Biquad::new(filter_type, sample_rate, frequency, q_stage, gain_db2),
        }
    }
}

impl Filter for CascadedBiquad {
    #[inline(always)]
    fn process(&mut self, input: f32) -> f32 {
        self.second.process(self.first.process(input))
    }
    fn reset(&mut self) {
        self.first.reset();
        self.second.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const TEST_SAMPLE_RATE: f32 = 48000.0;
    const F32_EPSILON: f32 = 1e-6;

    #[test]
    fn test_biquad_coeff_update_precision() {
        let freq = 1000.0;
        let q = 0.707;
        let gain = 0.0;
        let os_factor = 16;
        let effective_sr = TEST_SAMPLE_RATE * os_factor as f32;

        let mut bq1 = Biquad::new(FilterType::LowPass, TEST_SAMPLE_RATE, freq, q, gain);
        bq1.sample_rate = TEST_SAMPLE_RATE;
        bq1.frequency = freq;
        bq1.q = q;
        bq1.gain_db = gain;
        bq1.update_coefficients();

        println!(
            "Base SR ({:.1} Hz) Coeffs for {} Hz:",
            TEST_SAMPLE_RATE, freq
        );
        println!(
            "  b0={}, b1={}, b2={}, a1={}, a2={}",
            bq1.b0, bq1.b1, bq1.b2, bq1.a1, bq1.a2
        );

        assert!(bq1.b0.is_finite(), "Base b0 NaN/Inf");
        assert!(bq1.b1.is_finite(), "Base b1 NaN/Inf");
        assert!(bq1.b2.is_finite(), "Base b2 NaN/Inf");
        assert!(bq1.a1.is_finite(), "Base a1 NaN/Inf");
        assert!(bq1.a2.is_finite(), "Base a2 NaN/Inf");
        let dc_gain = (bq1.b0 + bq1.b1 + bq1.b2) / (1.0 + bq1.a1 + bq1.a2);
        assert!(
            (dc_gain - 1.0).abs() < 0.01,
            "Base DC gain not close to 1: {}",
            dc_gain
        );

        let mut bq_os = Biquad::new(FilterType::LowPass, effective_sr, freq, q, gain);
        bq_os.sample_rate = effective_sr;
        bq_os.frequency = freq;
        bq_os.q = q;
        bq_os.gain_db = gain;
        bq_os.update_coefficients();

        println!(
            "Effective SR ({:.1} Hz) Coeffs for {} Hz:",
            effective_sr, freq
        );
        println!(
            "  b0={}, b1={}, b2={}, a1={}, a2={}",
            bq_os.b0, bq_os.b1, bq_os.b2, bq_os.a1, bq_os.a2
        );

        assert!(bq_os.b0.is_finite(), "OS b0 NaN/Inf");
        assert!(bq_os.b1.is_finite(), "OS b1 NaN/Inf");
        assert!(bq_os.b2.is_finite(), "OS b2 NaN/Inf");
        assert!(bq_os.a1.is_finite(), "OS a1 NaN/Inf");
        assert!(bq_os.a2.is_finite(), "OS a2 NaN/Inf");

        assert!(
            (bq1.b0 - bq_os.b0).abs() > F32_EPSILON,
            "b0 coeffs are unexpectedly the same"
        );
        assert!(
            (bq1.a1 - bq_os.a1).abs() > F32_EPSILON,
            "a1 coeffs are unexpectedly the same"
        );
        assert!(
            (bq1.a2 - bq_os.a2).abs() > F32_EPSILON,
            "a2 coeffs are unexpectedly the same"
        );

        let os_dc_gain = (bq_os.b0 + bq_os.b1 + bq_os.b2) / (1.0 + bq_os.a1 + bq_os.a2);
        assert!(
            (os_dc_gain - 1.0).abs() < 0.01,
            "OS DC gain not close to 1: {}",
            os_dc_gain
        );

        // Expected values for LPF, 768kHz SR, 1kHz F, 0.707 Q using the current coefficient formulas
        // Expected approx: b0=1.66e-5, b1=3.33e-5, b2=1.66e-5, a1=-1.9884, a2=0.9885
        assert!(
            bq_os.b0 > 1.5e-5 && bq_os.b0 < 1.8e-5,
            "OS b0 unexpected value: {}",
            bq_os.b0
        );
        assert!(
            bq_os.b1 > 3.1e-5 && bq_os.b1 < 3.5e-5,
            "OS b1 unexpected value: {}",
            bq_os.b1
        );
        assert!(
            bq_os.a1 < -1.98 && bq_os.a1 > -1.995,
            "OS a1 unexpected value: {}",
            bq_os.a1
        );
        assert!(
            bq_os.a2 > 0.988 && bq_os.a2 < 0.9895,
            "OS a2 unexpected value: {}",
            bq_os.a2
        );
    }

    #[test]
    fn test_biquad_process_impulse() {
        let freq = 1000.0;
        let q = 0.707;
        let mut bq = Biquad::new(FilterType::LowPass, TEST_SAMPLE_RATE, freq, q, 0.0);

        let impulse = [1.0, 0.0, 0.0, 0.0, 0.0];
        let mut output = [0.0; 5];

        for i in 0..impulse.len() {
            output[i] = bq.process(impulse[i]);
        }

        println!("Impulse Response (first 5): {:?}", output);

        assert!(
            (output[0] - bq.b0).abs() < F32_EPSILON,
            "First output sample mismatch"
        );
        assert!(
            output[1].abs() > F32_EPSILON,
            "Second output sample is zero"
        );
        assert!(
            (output[1] - output[0]).abs() > F32_EPSILON,
            "Second sample same as first"
        );
    }

    #[test]
    fn test_biquad_dc_blocking() {
        // DC signal should be blocked by highpass
        let mut bq = Biquad::new(FilterType::HighPass, TEST_SAMPLE_RATE, 100.0, 0.707, 0.0);

        // Process DC signal (constant 1.0)
        let mut output_sum = 0.0;
        for _ in 0..1000 {
            output_sum += bq.process(1.0).abs();
        }
        let avg_output = output_sum / 1000.0;

        // After settling, DC should be heavily attenuated
        assert!(
            avg_output < 0.1,
            "HighPass should block DC signal, got {}",
            avg_output
        );
    }

    #[test]
    fn test_biquad_stability() {
        // Test that filter doesn't explode with extreme inputs
        for filter_type in [
            FilterType::LowPass,
            FilterType::HighPass,
            FilterType::BandPass,
            FilterType::Notch,
        ] {
            let mut bq = Biquad::new(filter_type, TEST_SAMPLE_RATE, 1000.0, 0.707, 0.0);

            // Feed in extreme values
            for _ in 0..100 {
                let output = bq.process(100.0);
                assert!(
                    output.is_finite(),
                    "Filter {:?} produced non-finite output",
                    filter_type
                );
                assert!(
                    output.abs() < 1000.0,
                    "Filter {:?} output exploded: {}",
                    filter_type,
                    output
                );
            }

            // Reset and test with negative extreme
            bq.reset();
            for _ in 0..100 {
                let output = bq.process(-100.0);
                assert!(
                    output.is_finite(),
                    "Filter {:?} produced non-finite output with negative input",
                    filter_type
                );
                assert!(
                    output.abs() < 1000.0,
                    "Filter {:?} output exploded with negative input: {}",
                    filter_type,
                    output
                );
            }
        }
    }

    #[test]
    fn test_biquad_reset() {
        let mut bq = Biquad::new(FilterType::LowPass, TEST_SAMPLE_RATE, 1000.0, 0.707, 0.0);

        // Process some samples to build up state
        for _ in 0..10 {
            bq.process(1.0);
        }

        // Reset
        bq.reset();

        // State variables should be zero
        assert_eq!(bq.x1, 0.0);
        assert_eq!(bq.x2, 0.0);
        assert_eq!(bq.y1, 0.0);
        assert_eq!(bq.y2, 0.0);
    }

    #[test]
    fn test_biquad_frequency_clamping() {
        // Test that extreme frequencies are clamped properly
        let mut bq = Biquad::new(FilterType::LowPass, TEST_SAMPLE_RATE, 100000.0, 0.707, 0.0);
        assert!(
            bq.frequency < TEST_SAMPLE_RATE * 0.5,
            "Frequency should be clamped below Nyquist"
        );

        bq = Biquad::new(FilterType::LowPass, TEST_SAMPLE_RATE, -100.0, 0.707, 0.0);
        assert!(
            bq.frequency >= 10.0,
            "Frequency should be clamped above minimum"
        );

        // Update coefficients with extreme values
        bq.frequency = 100000.0;
        bq.update_coefficients();
        assert!(bq.b0.is_finite() && bq.b1.is_finite() && bq.b2.is_finite());
        assert!(bq.a1.is_finite() && bq.a2.is_finite());
    }

    #[test]
    fn test_biquad_q_clamping() {
        // Test that Q values are handled correctly
        let mut bq = Biquad::new(FilterType::LowPass, TEST_SAMPLE_RATE, 1000.0, -1.0, 0.0);
        assert!(bq.q > 0.0, "Q should be positive");

        bq = Biquad::new(FilterType::LowPass, TEST_SAMPLE_RATE, 1000.0, 0.0, 0.0);
        assert!(bq.q > 0.0, "Q should be positive even when set to 0");
    }

    #[test]
    fn test_biquad_all_filter_types() {
        // Ensure all filter types can be created and process without panicking
        for filter_type in [
            FilterType::LowPass,
            FilterType::HighPass,
            FilterType::BandPass,
            FilterType::Notch,
            FilterType::Peaking,
            FilterType::LowShelf,
            FilterType::HighShelf,
            FilterType::Ladder,
            FilterType::Comb,
        ] {
            let mut bq = Biquad::new(filter_type, TEST_SAMPLE_RATE, 1000.0, 0.707, 6.0);

            // Process a sine wave
            for i in 0..100 {
                let phase = 2.0 * PI * 100.0 * i as f32 / TEST_SAMPLE_RATE;
                let input = phase.sin();
                let output = bq.process(input);
                assert!(
                    output.is_finite(),
                    "Filter {:?} produced non-finite output",
                    filter_type
                );
            }
        }
    }

    #[test]
    fn test_cascaded_biquad() {
        let mut cascaded =
            CascadedBiquad::new(FilterType::LowPass, TEST_SAMPLE_RATE, 1000.0, 1.0, 0.0);

        // Process impulse
        let output1 = cascaded.process(1.0);
        let output2 = cascaded.process(0.0);

        assert!(output1.is_finite());
        assert!(output2.is_finite());

        // Reset should clear state
        cascaded.reset();
        let output_after_reset = cascaded.process(1.0);
        assert!(
            (output_after_reset - output1).abs() < F32_EPSILON,
            "Reset should restore initial state"
        );
    }

    #[test]
    fn test_biquad_gain() {
        // Test that gain affects shelving and peaking filters
        let mut bq_pos = Biquad::new(FilterType::LowShelf, TEST_SAMPLE_RATE, 1000.0, 0.707, 6.0);
        let mut bq_neg = Biquad::new(FilterType::LowShelf, TEST_SAMPLE_RATE, 1000.0, 0.707, -6.0);
        let mut bq_zero = Biquad::new(FilterType::LowShelf, TEST_SAMPLE_RATE, 1000.0, 0.707, 0.0);

        // Process low frequency signal
        let mut sum_pos = 0.0;
        let mut sum_neg = 0.0;
        let mut sum_zero = 0.0;

        for i in 0..100 {
            let phase = 2.0 * PI * 50.0 * i as f32 / TEST_SAMPLE_RATE;
            let input = phase.sin();
            sum_pos += bq_pos.process(input).abs();
            sum_neg += bq_neg.process(input).abs();
            sum_zero += bq_zero.process(input).abs();
        }

        // Positive gain should amplify, negative should attenuate
        assert!(
            sum_pos > sum_zero,
            "Positive gain should amplify low frequencies"
        );
        assert!(
            sum_neg < sum_zero,
            "Negative gain should attenuate low frequencies"
        );
    }

    #[test]
    fn test_biquad_denormal_prevention() {
        // Test that very small values are properly handled
        let mut bq = Biquad::new(FilterType::LowPass, TEST_SAMPLE_RATE, 1000.0, 0.707, 0.0);

        // Process very small values
        for _ in 0..1000 {
            let output = bq.process(1e-20);
            // Should either be zero or a small value, not accumulate denormals
            assert!(output.abs() < 1e-15 || output == 0.0);
        }
    }
}
