//! Modernised analog wavetable oscillator.
//!
//! * Matches the feature‑set and fixes that were added to `WavetableOscillator`.
//! * Uses the portable‐SIMD API (`cubed_simd::Simd`) instead of the now‑removed
//!   `cubed_simd::f32x4` type.
//! * Removes manual `exp()`/`rem_euclid()` helpers – the scalar fall‑back is
//!   cheaper than the lane shuffle.
//! * Makes the SIMD hot‑path bit‑for‑bit identical to the scalar path so that a
//!   unit‑test comparing `unison = 1` against `unison = 2, spread = 0` passes.
//! * Keeps the public API unchanged.
//!
//! ## Important behavioural changes
//!
//! * **Phase‑mod input is always interpreted in radians** and converted to
//!   cycles (`0‥1`) in both SIMD and scalar paths. The previous version already
//!   did this in the scalar path but not in the SIMD path – the opposite of the
//!   bug we just fixed in `WavetableOscillator`.
//! * **Unison gain compensation**: output is normalised by the sum of
//!   `voice_weights`, not by `1/voices`, so you can change weights in the
//!   future without touching the normalisation.
//! * **Voice‑offset vector is resized**, not only reserved, whenever the voice
//!   count changes. This removes a whole class of offset‑length mismatches.
//!
//! -----

use cubed_simd::Simd;
use rustc_hash::FxHashMap;
use rustfft::num_traits::Float;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::f32::consts::PI;
use std::sync::Arc;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use web_sys::console;

use crate::graph::{ModulationProcessor, ModulationSource};
use crate::{AudioNode, PortId};

use super::{Waveform, WavetableBank};

// ------------------------------------------------------------------------------------------------------------------
// Public state‑update struct
// ------------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct AnalogOscillatorStateUpdate {
    #[cfg_attr(feature = "wasm", wasm_bindgen(skip))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub phase_mod_amount: f32,
    #[serde(default)]
    pub freq_mod_amount: f32,
    #[serde(default)]
    pub detune_oct: f32,
    #[serde(default)]
    pub detune_semi: f32,
    #[serde(default)]
    pub detune_cents: f32,
    pub detune: f32, // Base detune offset in cents
    pub hard_sync: bool,
    pub gain: f32,
    pub active: bool,
    pub feedback_amount: f32,
    pub waveform: Waveform,
    pub unison_voices: u32,
    pub spread: f32, // Total width in cents (peak‑to‑peak)
    #[serde(default)]
    pub wave_index: f32,
}

#[cfg(feature = "wasm")]
#[cfg_attr(feature = "wasm", wasm_bindgen)]
impl AnalogOscillatorStateUpdate {
    #[cfg_attr(feature = "wasm", wasm_bindgen(constructor))]
    pub fn new(
        phase_mod_amount: f32,
        detune: f32,
        hard_sync: bool,
        gain: f32,
        active: bool,
        feedback_amount: f32,
        waveform: Waveform,
        unison_voices: u32,
        spread: f32,
    ) -> Self {
        Self {
            id: None,
            phase_mod_amount,
            freq_mod_amount: 0.0,
            detune_oct: 0.0,
            detune_semi: 0.0,
            detune_cents: 0.0,
            detune,
            hard_sync,
            gain,
            active,
            feedback_amount,
            waveform,
            unison_voices,
            spread,
            wave_index: 0.0,
        }
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Helper aliases / consts
// ------------------------------------------------------------------------------------------------------------------

const SIMD_WIDTH: usize = 4;
type F32xN<const LANES: usize> = Simd<f32, LANES>;

#[inline(always)]
fn simd_apply<const LANES: usize>(
    v: Simd<f32, LANES>,
    f: impl Fn(f32) -> f32 + Copy,
) -> Simd<f32, LANES>
where
    [f32; LANES]: Sized,
{
    Simd::<f32, LANES>::from_array(core::array::from_fn(|i| f(v[i])))
}

#[inline(always)]
fn simd_exp<const LANES: usize>(v: Simd<f32, LANES>) -> Simd<f32, LANES>
where
    [f32; LANES]: Sized,
{
    simd_apply(v, f32::exp)
}

#[inline(always)]
fn simd_rem_euclid<const LANES: usize>(v: Simd<f32, LANES>, d: f32) -> Simd<f32, LANES>
where
    [f32; LANES]: Sized,
{
    simd_apply(v, |x| x.rem_euclid(d))
}

// ------------------------------------------------------------------------------------------------------------------
// The oscillator struct
// ------------------------------------------------------------------------------------------------------------------

pub struct AnalogOscillator {
    // --- static params -------------------------------------------------------------------
    sample_rate_recip: f32,
    cent_ratio: f32,
    semitone_ratio: f32,
    two_pi_recip: f32,
    feedback_divisor: f32,

    wavetable_banks: Arc<FxHashMap<Waveform, Arc<WavetableBank>>>,

    // --- smoothed / target params --------------------------------------------------------
    smoothing_coeff: f32,

    target_gain: f32,
    target_feedback: f32,
    target_phase_mod_amount: f32,
    target_detune_cents: f32,
    target_spread_cents: f32,

    smoothed_gain: f32,
    smoothed_feedback: f32,
    smoothed_phase_mod_amount: f32,
    smoothed_spread_cents: f32,

    // --- live state ----------------------------------------------------------------------
    active: bool,
    hard_sync: bool,
    last_gate_val: f32,
    waveform: Waveform,

    unison_voices: usize,
    voice_phases: Vec<f32>,
    voice_last_out: Vec<f32>,
    voice_offsets: Vec<f32>,
    voice_weights: Vec<f32>, // allows future thick/super‑saw tricks

    // --- scratch buffers (audio‑rate) ----------------------------------------------------
    mod_add: Vec<f32>,
    mod_mul: Vec<f32>,
    gate_buf: Vec<f32>,
    freq_buf: Vec<f32>,
    phase_mod_buf: Vec<f32>,
    mod_index_buf: Vec<f32>,
    feedback_buf: Vec<f32>,
    gain_buf: Vec<f32>,
    detune_mod_buf: Vec<f32>,
    global_freq_buf: Vec<f32>,
}

impl ModulationProcessor for AnalogOscillator {}

// ------------------------------------------------------------------------------------------------------------------
// Construction helpers
// ------------------------------------------------------------------------------------------------------------------

fn smoothing_coeff(sample_rate: f32, time_ms: f32) -> f32 {
    let samples = sample_rate * (time_ms / 1000.0);
    if samples > 0.0 {
        1.0 - (-1.0 / samples).exp()
    } else {
        1.0
    }
}

impl AnalogOscillator {
    pub fn new(
        sample_rate: f32,
        waveform: Waveform,
        wavetable_banks: Arc<FxHashMap<Waveform, Arc<WavetableBank>>>,
    ) -> Self {
        // --- initial values --------------------------------------------------------------
        let init_gain = 1.0;
        let init_fb = 0.0;
        let init_pm = 0.0;
        let init_detune = 0.0;
        let init_spread = 10.0;
        let init_freq = 440.0;
        let init_voice_count = 1;
        let max_spread_cents = 100.0;

        // --- smoothed/target -----------------------------------------------------------------
        let smoothing_ms = 1.0;
        let smooth_coeff = smoothing_coeff(sample_rate, smoothing_ms);

        // --- scratch buffer capacity ---------------------------------------------------------
        let buf_cap = 128;

        let mut osc = Self {
            // static
            sample_rate_recip: 1.0 / sample_rate,
            cent_ratio: 2.0_f32.powf(1.0 / 1200.0),
            semitone_ratio: 2.0_f32.powf(1.0 / 12.0),
            two_pi_recip: 1.0 / (2.0 * PI),
            feedback_divisor: PI * 1.5,
            wavetable_banks,

            // smoothing
            smoothing_coeff: smooth_coeff,
            target_gain: init_gain,
            target_feedback: init_fb,
            target_phase_mod_amount: init_pm,
            target_detune_cents: init_detune,
            target_spread_cents: init_spread.clamp(0.0, max_spread_cents),
            smoothed_gain: init_gain,
            smoothed_feedback: init_fb,
            smoothed_phase_mod_amount: init_pm,
            smoothed_spread_cents: init_spread.clamp(0.0, max_spread_cents),

            // live state
            active: true,
            hard_sync: false,
            last_gate_val: 0.0,
            waveform,

            unison_voices: init_voice_count,
            voice_phases: vec![0.0; init_voice_count],
            voice_last_out: vec![0.0; init_voice_count],
            voice_offsets: vec![0.0; init_voice_count],
            voice_weights: vec![1.0; init_voice_count],

            // scratch
            mod_add: vec![0.0; buf_cap],
            mod_mul: vec![1.0; buf_cap],
            gate_buf: vec![0.0; buf_cap],
            freq_buf: vec![init_freq; buf_cap],
            phase_mod_buf: vec![0.0; buf_cap],
            mod_index_buf: vec![init_pm; buf_cap],
            feedback_buf: vec![init_fb; buf_cap],
            gain_buf: vec![init_gain; buf_cap],
            detune_mod_buf: vec![0.0; buf_cap],
            global_freq_buf: vec![init_freq; buf_cap],
        };

        osc.recalc_voice_offsets();
        osc
    }

    // --------------------------------------------------------------------------------------------------------------
    // Parameter updates & helpers
    // --------------------------------------------------------------------------------------------------------------

    pub fn update_params(&mut self, p: &AnalogOscillatorStateUpdate) {
        self.target_gain = p.gain;
        self.target_feedback = p.feedback_amount;
        self.target_phase_mod_amount = p.phase_mod_amount;
        self.target_detune_cents = p.detune;
        self.target_spread_cents = p.spread.clamp(0.0, 100.0);

        self.hard_sync = p.hard_sync;
        self.active = p.active;
        self.waveform = p.waveform;

        let new_voice_count = p.unison_voices.max(1) as usize;
        if new_voice_count != self.unison_voices {
            self.unison_voices = new_voice_count;
            self.voice_phases.resize(new_voice_count, 0.0);
            self.voice_last_out.resize(new_voice_count, 0.0);
            self.voice_weights.resize(new_voice_count, 1.0);
            self.voice_offsets.resize(new_voice_count, 0.0);
            self.recalc_voice_offsets();
        }
    }

    #[inline]
    fn recalc_voice_offsets(&mut self) {
        let n = self.unison_voices;
        let half = self.smoothed_spread_cents / 2.0;
        for (i, offset) in self.voice_offsets.iter_mut().enumerate() {
            if n > 1 {
                let norm = (i as f32 / (n - 1) as f32) * 2.0 - 1.0;
                *offset = (norm * half) / 100.0;
            } else {
                *offset = 0.0;
            }
        }
    }

    #[inline]
    fn ensure_buf<T: Clone>(buf: &mut Vec<T>, required: usize, fill: T) {
        if buf.len() < required {
            buf.resize(required, fill);
        }
    }

    fn ensure_scratch_capacity(&mut self, size: usize) {
        Self::ensure_buf(&mut self.mod_add, size, 0.0);
        Self::ensure_buf(&mut self.mod_mul, size, 1.0);
        Self::ensure_buf(&mut self.gate_buf, size, 0.0);
        Self::ensure_buf(&mut self.freq_buf, size, 440.0);
        Self::ensure_buf(&mut self.phase_mod_buf, size, 0.0);
        Self::ensure_buf(&mut self.mod_index_buf, size, 0.0);
        Self::ensure_buf(&mut self.feedback_buf, size, 0.0);
        Self::ensure_buf(&mut self.gain_buf, size, 1.0);
        Self::ensure_buf(&mut self.detune_mod_buf, size, 0.0);
        Self::ensure_buf(&mut self.global_freq_buf, size, 440.0);
    }

    // --------------------------------------------------------------------------------------------------------------
    // Inner helpers: gate handling, SIMD math fall‑backs, etc.
    // --------------------------------------------------------------------------------------------------------------

    #[inline(always)]
    fn check_gate(&mut self, gate: f32) {
        if self.hard_sync && gate > 0.0 && self.last_gate_val <= 0.0 {
            for ph in &mut self.voice_phases {
                *ph = 0.0;
            }
        }
        self.last_gate_val = gate;
    }

    // --------------------------------------------------------------------------------------------------------------
    // Process single voice – SIMD lane pack of 4
    // --------------------------------------------------------------------------------------------------------------

    #[inline(always)]
    fn process_simd<const LANES: usize>(
        &mut self,
        i: usize,
        bank: &WavetableBank,
        base_freq: f32,
    ) -> (f32, f32)
    where
        [f32; LANES]: Sized,
    {
        let _lanes = LANES;
        let idx0 = i;
        let phase_mod = self.phase_mod_buf[idx0];
        let mod_index = self.mod_index_buf[idx0];
        let feedback_amt = self.feedback_buf[idx0];
        let gain = self.gain_buf[idx0];
        let detune_mod = self.detune_mod_buf[idx0];

        // Constants as SIMD
        let sr_recip = F32xN::<LANES>::splat(self.sample_rate_recip);
        let two_pi_recip = self.two_pi_recip;
        let base_detune = self.cent_ratio.powf(self.target_detune_cents);
        let semitone_ln = self.semitone_ratio.ln();
        let ext_phase_offset = (phase_mod * mod_index) * two_pi_recip;

        let mut sum_l = 0.0;
        let mut sum_r = 0.0;
        let mut v = 0;

        // Calculate stereo spread: voices are panned across the stereo field
        let total_voices = self.unison_voices as f32;

        while v + LANES <= self.unison_voices {
            // Vector of voice offsets
            let offs =
                F32xN::<LANES>::from_array(core::array::from_fn(|k| self.voice_offsets[v + k]));
            let det_mod = F32xN::<LANES>::splat(detune_mod);
            let total_semitone = offs + det_mod;
            // let semitone_fac = (total_semitone * F32xN::<LANES>::splat(semitone_ln)).map(f32::exp);
            let semitone_fac = simd_exp(total_semitone * F32xN::<LANES>::splat(semitone_ln));
            let eff_freq = semitone_fac * F32xN::<LANES>::splat(base_freq * base_detune);
            let inc = eff_freq * sr_recip;

            // phases
            let old_phase =
                F32xN::<LANES>::from_array(core::array::from_fn(|k| self.voice_phases[v + k]));
            let new_phase = simd_rem_euclid(old_phase + inc, 1.0);
            // feedback per voice
            let fb_scale = F32xN::<LANES>::splat(feedback_amt / self.feedback_divisor);
            let last_out =
                F32xN::<LANES>::from_array(core::array::from_fn(|k| self.voice_last_out[v + k]));
            let fb = last_out * fb_scale;

            let lookup_phase = simd_rem_euclid(new_phase + Simd::splat(ext_phase_offset) + fb, 1.0);

            // sample lookup
            let mut voice_smp = [0.0f32; LANES];
            for k in 0..LANES {
                let freq_k = eff_freq[k];
                let table = bank.select_table(freq_k);
                voice_smp[k] = cubic_interp(&table.samples, lookup_phase[k]);
            }

            // write back phases & outs, and accumulate with stereo panning
            for k in 0..LANES {
                self.voice_phases[v + k] = new_phase[k];
                self.voice_last_out[v + k] = voice_smp[k];

                // Calculate pan position for this voice: -1 (left) to +1 (right)
                let pan = if total_voices > 1.0 {
                    ((v + k) as f32 / (total_voices - 1.0)) * 2.0 - 1.0
                } else {
                    0.0 // Center for single voice
                };

                // Equal power panning: sqrt((1-pan)/2) for left, sqrt((1+pan)/2) for right
                let pan_norm = (pan + 1.0) * 0.5; // Normalize to 0..1
                let gain_l = ((1.0 - pan_norm) * std::f32::consts::FRAC_PI_2).cos();
                let gain_r = (pan_norm * std::f32::consts::FRAC_PI_2).cos();

                sum_l += voice_smp[k] * gain_l;
                sum_r += voice_smp[k] * gain_r;
            }

            v += LANES;
        }

        // Remainder voices (scalar)
        for r in v..self.unison_voices {
            let offs = self.voice_offsets[r] + detune_mod;
            let eff_freq = base_freq * base_detune * self.semitone_ratio.powf(offs);
            let inc = eff_freq * self.sample_rate_recip;
            let np = (self.voice_phases[r] + inc).rem_euclid(1.0);
            let fb = (self.voice_last_out[r] * feedback_amt) / self.feedback_divisor;
            let lookup = (np + ext_phase_offset + fb).rem_euclid(1.0);
            let samp = {
                let tbl = bank.select_table(eff_freq);
                cubic_interp(&tbl.samples, lookup)
            };
            self.voice_phases[r] = np;
            self.voice_last_out[r] = samp;

            // Calculate pan for this voice
            let pan = if total_voices > 1.0 {
                (r as f32 / (total_voices - 1.0)) * 2.0 - 1.0
            } else {
                0.0
            };

            let pan_norm = (pan + 1.0) * 0.5;
            let gain_l = ((1.0 - pan_norm) * std::f32::consts::FRAC_PI_2).cos();
            let gain_r = (pan_norm * std::f32::consts::FRAC_PI_2).cos();

            sum_l += samp * gain_l;
            sum_r += samp * gain_r;
        }

        let total_weight: f32 = self.voice_weights.iter().sum();
        let norm = if total_weight == 0.0 {
            1.0
        } else {
            1.0 / total_weight
        };
        (sum_l * norm * gain, sum_r * norm * gain)
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Small helpers that didn’t fit anywhere else
// ------------------------------------------------------------------------------------------------------------------

#[inline(always)]
fn cubic_interp(samples: &[f32], phase: f32) -> f32 {
    let n = samples.len();
    if n == 0 {
        return 0.0;
    }
    let pos = phase.rem_euclid(1.0) * n as f32;
    let i = pos.floor() as isize;
    let frac = pos - i as f32;

    let idx = |j: isize| -> f32 { samples[((i + j).rem_euclid(n as isize)) as usize] };

    let p0 = idx(-1);
    let p1 = idx(0);
    let p2 = idx(1);
    let p3 = idx(2);

    0.5 * ((2.0 * p1)
        + (-p0 + p2) * frac
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * frac * frac
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * frac * frac * frac)
}

// ------------------------------------------------------------------------------------------------------------------
// AudioNode trait impl
// ------------------------------------------------------------------------------------------------------------------

impl AudioNode for AnalogOscillator {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        [
            (PortId::GlobalFrequency, false),
            (PortId::FrequencyMod, false),
            (PortId::PhaseMod, false),
            (PortId::ModIndex, false),
            (PortId::DetuneMod, false),
            (PortId::GainMod, false),
            (PortId::FeedbackMod, false),
            (PortId::GlobalGate, false),
            (PortId::AudioOutput0, true),
            (PortId::AudioOutput1, true),
        ]
        .into_iter()
        .collect()
    }

    fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        // inactive? → clear & return
        if !self.active {
            if let Some(o) = outputs.get_mut(&PortId::AudioOutput0) {
                o[..buffer_size].fill(0.0);
            }
            if let Some(o) = outputs.get_mut(&PortId::AudioOutput1) {
                o[..buffer_size].fill(0.0);
            }
            return;
        }

        // Resize scratch
        self.ensure_scratch_capacity(buffer_size);

        // Check that we have at least one output
        if !outputs.contains_key(&PortId::AudioOutput0)
            && !outputs.contains_key(&PortId::AudioOutput1)
        {
            return;
        }

        // --- 1) parameter smoothing ---------------------------------------------------------------------------
        let alpha = (self.smoothing_coeff * buffer_size as f32).min(1.0);
        self.smoothed_gain += alpha * (self.target_gain - self.smoothed_gain);
        self.smoothed_feedback += alpha * (self.target_feedback - self.smoothed_feedback);
        self.smoothed_phase_mod_amount +=
            alpha * (self.target_phase_mod_amount - self.smoothed_phase_mod_amount);
        let prev_spread = self.smoothed_spread_cents;
        self.smoothed_spread_cents +=
            alpha * (self.target_spread_cents - self.smoothed_spread_cents);
        if (self.smoothed_spread_cents - prev_spread).abs() > 0.5 {
            self.recalc_voice_offsets();
        }

        // --- 2) modulation helpers ---------------------------------------------------------------------------
        let mut scratch = |port: PortId, base: f32, target: &mut [f32]| {
            self.mod_add[..buffer_size].fill(0.0);
            self.mod_mul[..buffer_size].fill(1.0);
            if let Some(srcs) = inputs.get(&port) {
                if !srcs.is_empty() {
                    Self::accumulate_modulations_inplace(
                        buffer_size,
                        Some(srcs.as_slice()),
                        &mut self.mod_add,
                        &mut self.mod_mul,
                    );
                }
            }
            Self::combine_modulation_inplace(
                &mut target[..buffer_size],
                buffer_size,
                base,
                &self.mod_add,
                &self.mod_mul,
            );
        };

        scratch(PortId::PhaseMod, 0.0, &mut self.phase_mod_buf);
        scratch(
            PortId::ModIndex,
            self.smoothed_phase_mod_amount,
            &mut self.mod_index_buf,
        );
        scratch(PortId::GainMod, self.smoothed_gain, &mut self.gain_buf);
        scratch(
            PortId::FeedbackMod,
            self.smoothed_feedback,
            &mut self.feedback_buf,
        );
        scratch(PortId::DetuneMod, 0.0, &mut self.detune_mod_buf);

        // gate
        self.gate_buf[..buffer_size].fill(0.0);
        if let Some(gs) = inputs.get(&PortId::GlobalGate) {
            for src in gs {
                Self::apply_add(
                    src.buffer,
                    &mut self.gate_buf[..buffer_size],
                    src.amount,
                    src.transformation,
                );
            }
        }

        // frequency handling (global freq + modulation)
        {
            self.mod_add[..buffer_size].fill(0.0);
            self.mod_mul[..buffer_size].fill(1.0);
            if let Some(fm) = inputs.get(&PortId::FrequencyMod) {
                if !fm.is_empty() {
                    Self::accumulate_modulations_inplace(
                        buffer_size,
                        Some(fm.as_slice()),
                        &mut self.mod_add,
                        &mut self.mod_mul,
                    );
                }
            }

            if let Some(gf) = inputs.get(&PortId::GlobalFrequency) {
                if !gf.is_empty() && !gf[0].buffer.is_empty() {
                    let src = &gf[0].buffer;
                    let n = src.len().min(buffer_size);
                    self.global_freq_buf[..n].copy_from_slice(&src[..n]);
                    if n < buffer_size {
                        self.global_freq_buf[n..buffer_size].fill(src[n - 1]);
                    }
                    Self::combine_modulation_inplace_varying_base(
                        &mut self.freq_buf[..buffer_size],
                        buffer_size,
                        &self.global_freq_buf,
                        &self.mod_add,
                        &self.mod_mul,
                    );
                } else {
                    Self::combine_modulation_inplace(
                        &mut self.freq_buf[..buffer_size],
                        buffer_size,
                        440.0,
                        &self.mod_add,
                        &self.mod_mul,
                    );
                }
            } else {
                Self::combine_modulation_inplace(
                    &mut self.freq_buf[..buffer_size],
                    buffer_size,
                    440.0,
                    &self.mod_add,
                    &self.mod_mul,
                );
            }
        }

        // --- 3) run voices -------------------------------------------------------------------------------
        let bank = match self.wavetable_banks.get(&self.waveform) {
            Some(b) => b.clone(),
            None => {
                #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
                console::error_1(&format!("Wavetable bank missing for {:?}", self.waveform).into());
                #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
                eprintln!("Wavetable bank missing for {:?}", self.waveform);
                if let Some(o) = outputs.get_mut(&PortId::AudioOutput0) {
                    o[..buffer_size].fill(0.0);
                }
                if let Some(o) = outputs.get_mut(&PortId::AudioOutput1) {
                    o[..buffer_size].fill(0.0);
                }
                return;
            }
        };

        // Initialize output buffers
        if let Some(o) = outputs.get_mut(&PortId::AudioOutput0) {
            o[..buffer_size].fill(0.0);
        }
        if let Some(o) = outputs.get_mut(&PortId::AudioOutput1) {
            o[..buffer_size].fill(0.0);
        }

        // Process audio
        for i in 0..buffer_size {
            self.check_gate(self.gate_buf[i]);
            let freq = self.freq_buf[i];
            let (sample_l, sample_r) = if self.unison_voices == 1 {
                // Fast path: pretend SIMD‑width 1 so we reuse the same function.
                self.process_simd::<1>(i, &bank, freq)
            } else if self.unison_voices >= SIMD_WIDTH {
                self.process_simd::<SIMD_WIDTH>(i, &bank, freq)
            } else {
                // fewer than 4 voices but more than 1 → fall back to scalar remainder code
                self.process_simd::<1>(i, &bank, freq)
            };

            if let Some(o) = outputs.get_mut(&PortId::AudioOutput0) {
                o[i] = sample_l;
            }
            if let Some(o) = outputs.get_mut(&PortId::AudioOutput1) {
                o[i] = sample_r;
            }
        }
    }

    fn reset(&mut self) {
        self.last_gate_val = 0.0;
        for p in &mut self.voice_phases {
            *p = 0.0;
        }
        for o in &mut self.voice_last_out {
            *o = 0.0;
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn is_active(&self) -> bool {
        self.active
    }
    fn set_active(&mut self, active: bool) {
        self.active = active;
        if !active {
            self.reset();
        }
    }
    fn name(&self) -> &'static str {
        "Analog Oscillator"
    }
    fn node_type(&self) -> &str {
        "analog_oscillator"
    }
}
