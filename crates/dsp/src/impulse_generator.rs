#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
use getrandom::fill;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use js_sys::{self, Reflect};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use web_sys::console;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
fn log_error(message: &str) {
    console::error_1(&message.into());
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
fn log_error(message: &str) {
    eprintln!("{message}");
}

/// Fallback to JS crypto.getRandomValues if available, or use Math.random() as a last resort.
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub fn js_fallback_fill(seed: &mut [u8]) -> Result<(), String> {
    if let Some(window) = web_sys::window() {
        if let Ok(crypto) = window.crypto() {
            if crypto.get_random_values_with_u8_array(seed).is_ok() {
                return Ok(());
            }
        }
    }

    // AudioWorkletGlobalScope does not have a Window, but crypto is still exposed on globalThis.
    let global = js_sys::global();
    if let Ok(crypto_value) = Reflect::get(global.as_ref(), &JsValue::from_str("crypto")) {
        if let Ok(crypto) = crypto_value.dyn_into::<web_sys::Crypto>() {
            if crypto.get_random_values_with_u8_array(seed).is_ok() {
                return Ok(());
            }
        }
    }

    // web_sys::console::warn_1(
    //     &"Falling back to Math.random() for seed generation (insecure)".into(),
    // );
    for byte in seed.iter_mut() {
        *byte = (js_sys::Math::random() * 256.0) as u8;
    }
    Ok(())
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub fn js_fallback_fill(seed: &mut [u8]) -> Result<(), String> {
    let mut rng = rand::rng();
    for byte in seed.iter_mut() {
        *byte = rng.random();
    }
    Ok(())
}

/// Tries getrandom::fill first (on native), then falls back to js_fallback_fill.
///
/// On WebAssembly this skips getrandom entirely and only uses the JS/Math.random
/// fallback to avoid relying on the Web Crypto API in contexts where it is
/// unavailable (e.g. AudioWorklet global scope).
pub fn fill_seed(seed: &mut [u8]) -> Result<(), String> {
    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    {
        // In WASM we avoid getrandom completely to prevent noisy failures when
        // Web Crypto is not available. The js_fallback_fill implementation
        // already logs a warning and falls back to Math.random() if needed.
        js_fallback_fill(seed)
    }

    #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
    {
        match fill(seed) {
            Ok(()) => Ok(()),
            Err(e) => {
                log_error(&format!("getrandom failed: {:?}", e));
                js_fallback_fill(seed)
            }
        }
    }
}

/// A biquad low‑pass filter with a Butterworth‑like Q (~0.707) for a natural spectral roll‑off.
struct BiquadLPF {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl BiquadLPF {
    fn new(cutoff: f32, sample_rate: f32) -> Self {
        let q = 0.707; // Butterworth Q
        let cutoff = cutoff.clamp(20.0, 20_000.0);
        let sample_rate = sample_rate.clamp(1.0, 192_000.0);
        let w0 = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);
        let b0 = (1.0 - cos_w0) / 2.0;
        let b1 = 1.0 - cos_w0;
        let b2 = (1.0 - cos_w0) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

/// A simple all‑pass filter for diffusion.
struct AllPass {
    feedback: f32,
    buffer: Vec<f32>,
    index: usize,
}

impl AllPass {
    fn new(feedback: f32, delay_samples: usize) -> Self {
        Self {
            feedback,
            buffer: vec![0.0; delay_samples.max(1)],
            index: 0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let buffered = self.buffer[self.index];
        let output = -input + buffered;
        self.buffer[self.index] = input + buffered * self.feedback;
        self.index = (self.index + 1) % self.buffer.len();
        output
    }
}

/// A comb filter for the hall reverb.
struct CombFilter {
    feedback: f32,
    buffer: Vec<f32>,
    index: usize,
}

impl CombFilter {
    fn new(delay_samples: usize, feedback: f32) -> Self {
        Self {
            feedback,
            buffer: vec![0.0; delay_samples.max(1)],
            index: 0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let out = self.buffer[self.index];
        self.buffer[self.index] = input + out * self.feedback;
        self.index = (self.index + 1) % self.buffer.len();
        out
    }
}

/// Finalizes an impulse response by removing DC offset, energy normalization,
/// and then peak normalization.
fn finalize_ir(ir: &mut [f32]) {
    let sum: f32 = ir.iter().copied().sum();
    let dc_offset = sum / (ir.len() as f32);
    for sample in ir.iter_mut() {
        *sample -= dc_offset;
    }
    let energy: f32 = ir.iter().map(|x| x * x).sum();
    if energy > 0.0 {
        let compensation = 1.0 / energy.sqrt();
        for sample in ir.iter_mut() {
            *sample *= compensation;
        }
    }
    let max = ir.iter().map(|x| x.abs()).fold(0.0, f32::max);
    if max > 0.0 && max.is_finite() {
        let scale = 1.0 / max;
        for sample in ir.iter_mut() {
            *sample *= scale;
        }
    }
}

/// A public utility function that processes and normalizes an impulse response.
pub fn process_impulse_response(mut ir: Vec<f32>) -> Vec<f32> {
    finalize_ir(&mut ir);
    ir
}

/// The impulse response generator.
pub struct ImpulseResponseGenerator {
    sample_rate: f32,
}

impl ImpulseResponseGenerator {
    pub fn new(sample_rate: f32) -> Self {
        Self { sample_rate }
    }

    /// Generate an enhanced plate reverb impulse response.
    pub fn plate(&self, decay_time: f32, diffusion: f32) -> Vec<f32> {
        let decay_time = decay_time.clamp(0.1, 10.0);
        let diffusion = diffusion.clamp(0.0, 1.0);
        let sample_rate = self.sample_rate.clamp(1.0, 192_000.0);
        let num_samples = ((decay_time * sample_rate) as usize).clamp(1, 60 * 48000);

        let mut seed = [0u8; 32];
        if let Err(e) = fill_seed(&mut seed) {
            log_error(&format!("failed to generate random seed: {}", e));
            panic!("failed to generate random seed: {}", e);
        }
        let mut rng = StdRng::from_seed(seed);

        let mut ir: Vec<f32> = (0..num_samples)
            .map(|i| {
                let t = i as f32 / sample_rate;
                let envelope = (-4.0 * t / decay_time).exp();
                envelope * rng.random_range(-1.0_f32..1.0_f32)
            })
            .collect();

        if diffusion > 0.0 {
            let delay_times = [0.010, 0.012, 0.015, 0.017];
            let mut allpass_chain: Vec<AllPass> = delay_times
                .iter()
                .map(|&dt| AllPass::new(diffusion, (dt * sample_rate) as usize))
                .collect();
            for sample in ir.iter_mut() {
                let mut processed = *sample;
                for ap in allpass_chain.iter_mut() {
                    processed = ap.process(processed);
                }
                *sample = processed;
            }
        }

        let cutoff = (5000.0 - 4500.0 * diffusion).clamp(20.0, 20000.0);
        let mut lpf = BiquadLPF::new(cutoff, sample_rate);
        for sample in ir.iter_mut() {
            *sample = lpf.process(*sample);
        }

        let early_reflections = [
            ((0.005 * sample_rate) as usize, 0.7 * diffusion),
            ((0.012 * sample_rate) as usize, 0.5 * diffusion),
            ((0.020 * sample_rate) as usize, 0.3 * diffusion),
        ];
        for &(delay, gain) in early_reflections.iter() {
            for i in 0..ir.len().saturating_sub(delay) {
                ir[i + delay] += gain * ir[i];
            }
        }

        let fade_len = (num_samples as f32 * 0.1) as usize;
        for i in (num_samples - fade_len)..num_samples {
            let fade = (num_samples - i) as f32 / fade_len as f32;
            ir[i] *= fade;
        }

        finalize_ir(&mut ir);
        ir
    }

    /// Generate a hall reverb impulse response using parallel comb filters
    /// and a series all‑pass filter chain for diffusion.
    pub fn hall(&self, decay_time: f32, room_size: f32) -> Vec<f32> {
        let decay_time = decay_time.clamp(0.1, 10.0);
        let room_size = room_size.clamp(0.1, 1.0);
        let sample_rate = self.sample_rate.clamp(1.0, 192_000.0);
        let num_samples = ((decay_time * sample_rate) as usize).clamp(1, 60 * 48000);

        let mut seed = [0u8; 32];
        if let Err(e) = fill_seed(&mut seed) {
            log_error(&format!("failed to generate random seed: {}", e));
            panic!("failed to generate random seed: {}", e);
        }
        let mut rng = StdRng::from_seed(seed);

        let noise: Vec<f32> = (0..num_samples)
            .map(|i| {
                let t = i as f32 / sample_rate;
                let envelope = (-4.0 * t / decay_time).exp();
                envelope * rng.random_range(-1.0_f32..1.0_f32)
            })
            .collect();

        let base_delays = [0.0297, 0.0371, 0.0411, 0.0437];
        let comb_delays: Vec<usize> = base_delays
            .iter()
            .map(|&d| ((d * room_size) * sample_rate) as usize)
            .collect();
        let comb_filters: Vec<CombFilter> = comb_delays
            .iter()
            .map(|&delay_samples| {
                let delay_secs = delay_samples as f32 / sample_rate;
                let feedback = (-6.9078 * delay_secs / decay_time).exp();
                CombFilter::new(delay_samples, feedback)
            })
            .collect();

        let mut comb_outputs = vec![0.0_f32; num_samples];
        let mut combs: Vec<CombFilter> = comb_filters;
        for i in 0..num_samples {
            let input = noise[i];
            let mut sum = 0.0;
            for comb in combs.iter_mut() {
                sum += comb.process(input);
            }
            comb_outputs[i] = sum;
        }

        let allpass_delays = [0.005, 0.007];
        let mut allpass_chain: Vec<AllPass> = allpass_delays
            .iter()
            .map(|&dt| AllPass::new(0.5, (dt * sample_rate) as usize))
            .collect();
        for sample in comb_outputs.iter_mut() {
            let mut processed = *sample;
            for ap in allpass_chain.iter_mut() {
                processed = ap.process(processed);
            }
            *sample = processed;
        }

        let cutoff = (7000.0 - 6000.0 * (room_size - 0.1) / 0.9).clamp(200.0, 20000.0);
        let mut lpf = BiquadLPF::new(cutoff, sample_rate);
        for sample in comb_outputs.iter_mut() {
            *sample = lpf.process(*sample);
        }

        let fade_len = (num_samples as f32 * 0.1) as usize;
        for i in (num_samples - fade_len)..num_samples {
            let fade = (num_samples - i) as f32 / fade_len as f32;
            comb_outputs[i] *= fade;
        }

        finalize_ir(&mut comb_outputs);
        comb_outputs
    }
}
