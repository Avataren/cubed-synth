use cubed_simd::Simd;
use rustc_hash::FxHashMap;
use rustfft::num_traits::Float;
use std::any::Any;
use std::cell::RefCell;
use std::f32::consts::PI;
use std::rc::Rc;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use web_sys::console;

use super::morph_wavetable::{WavetableMorphCollection, WavetableSynthBank};
use crate::graph::{ModulationProcessor, ModulationSource};
use crate::{AudioNode, PortId};
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "wasm", wasm_bindgen)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WavetableOscillatorStateUpdate {
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
    pub detune: f32,
    pub hard_sync: bool,
    pub gain: f32,
    pub active: bool,
    pub feedback_amount: f32,
    #[serde(default)]
    pub waveform: u32,
    pub unison_voices: u32,
    pub spread: f32,
    #[serde(default)]
    #[serde(rename = "wave_index")]
    pub wavetable_index: f32,
}

#[cfg(feature = "wasm")]
#[cfg_attr(feature = "wasm", wasm_bindgen)]
impl WavetableOscillatorStateUpdate {
    #[cfg_attr(feature = "wasm", wasm_bindgen(constructor))]
    pub fn new(
        phase_mod_amount: f32,
        detune: f32,
        hard_sync: bool,
        gain: f32,
        active: bool,
        feedback_amount: f32,
        unison_voices: u32,
        spread: f32,
        wavetable_index: f32,
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
            waveform: 0,
            unison_voices,
            spread,
            wavetable_index,
        }
    }
}

pub struct WavetableOscillator {
    smoothing_coeff: f32,

    // smoothed parameters
    target_gain: f32,
    target_feedback_amount: f32,
    target_phase_mod_amount: f32,
    target_detune: f32,
    target_spread: f32,
    target_wavetable_index: f32,

    smoothed_gain: f32,
    smoothed_feedback_amount: f32,
    smoothed_phase_mod_amount: f32,
    smoothed_spread: f32,
    smoothed_wavetable_index: f32,

    // live state
    active: bool,
    hard_sync: bool,
    last_gate_value: f32,
    frequency: f32,

    // unison
    unison_voices: usize,
    voice_phases: Vec<f32>,
    voice_last_outputs: Vec<f32>,
    voice_weights: Vec<f32>,
    voice_offsets: Vec<f32>,

    // wavetable lookup
    collection_name: String,
    wavetable_bank: Rc<RefCell<WavetableSynthBank>>,

    // constants
    two_pi_recip: f32,
    feedback_divisor: f32,
    cent_ratio: f32,
    semitone_ratio: f32,
    sample_rate_recip: f32,

    // scratch buffers
    mod_scratch_add: Vec<f32>,
    mod_scratch_mult: Vec<f32>,
    gate_buffer: Vec<f32>,
    scratch_freq: Vec<f32>,
    scratch_phase_mod: Vec<f32>,
    scratch_gain_mod: Vec<f32>,
    scratch_feedback_mod: Vec<f32>,
    scratch_mod_index: Vec<f32>,
    scratch_wavetable_index: Vec<f32>,
    scratch_detune_mod: Vec<f32>,
    global_freq_buffer: Vec<f32>,
}

impl ModulationProcessor for WavetableOscillator {}

impl WavetableOscillator {
    pub fn new(sample_rate: f32, bank: Rc<RefCell<WavetableSynthBank>>) -> Self {
        let initial_capacity = 128;
        let initial_frequency = 440.0;
        let initial_gain = 1.0;
        let initial_feedback = 0.0;
        let initial_phase_mod = 0.0;
        let initial_detune = 0.0;
        let initial_spread = 10.0;
        let initial_wt_index = 0.0;
        let max_spread_cents = 100.0;

        let smoothing_time_ms = 1.0;
        let smoothing_time_samples = sample_rate * (smoothing_time_ms / 1000.0);
        let smoothing_coeff = if smoothing_time_samples > 0.0 {
            1.0 - (-1.0 / smoothing_time_samples).exp()
        } else {
            1.0
        };

        let mut osc = Self {
            smoothing_coeff,

            target_gain: initial_gain,
            target_feedback_amount: initial_feedback,
            target_phase_mod_amount: initial_phase_mod,
            target_detune: initial_detune,
            target_spread: initial_spread.clamp(0.0, max_spread_cents),
            target_wavetable_index: initial_wt_index,

            smoothed_gain: initial_gain,
            smoothed_feedback_amount: initial_feedback,
            smoothed_phase_mod_amount: initial_phase_mod,
            smoothed_spread: initial_spread.clamp(0.0, max_spread_cents),
            smoothed_wavetable_index: initial_wt_index,

            active: true,
            hard_sync: false,
            last_gate_value: 0.0,
            frequency: initial_frequency,

            unison_voices: 1,
            voice_phases: vec![0.0; 1],
            voice_last_outputs: vec![0.0; 1],
            voice_weights: Vec::with_capacity(16),
            voice_offsets: Vec::with_capacity(16),

            collection_name: "default".to_string(),
            wavetable_bank: bank,

            two_pi_recip: 1.0 / (PI * 2.0),
            feedback_divisor: PI * 1.5,
            cent_ratio: 2.0_f32.powf(1.0 / 1200.0),
            semitone_ratio: 2.0_f32.powf(1.0 / 12.0),
            sample_rate_recip: 1.0 / sample_rate,

            mod_scratch_add: vec![0.0; initial_capacity],
            mod_scratch_mult: vec![1.0; initial_capacity],
            gate_buffer: vec![0.0; initial_capacity],
            scratch_freq: vec![initial_frequency; initial_capacity],
            scratch_phase_mod: vec![0.0; initial_capacity],
            scratch_gain_mod: vec![initial_gain; initial_capacity],
            scratch_feedback_mod: vec![initial_feedback; initial_capacity],
            scratch_mod_index: vec![initial_phase_mod; initial_capacity],
            scratch_wavetable_index: vec![initial_wt_index; initial_capacity],
            scratch_detune_mod: vec![0.0; initial_capacity],
            global_freq_buffer: vec![initial_frequency; initial_capacity],
        };

        osc.update_voice_unison_values(initial_spread);
        osc
    }

    fn ensure_scratch_buffers(&mut self, size: usize) {
        let resize_if_needed = |buf: &mut Vec<f32>, default_val: f32| {
            if buf.len() < size {
                buf.resize(size, default_val);
            }
        };

        resize_if_needed(&mut self.mod_scratch_add, 0.0);
        resize_if_needed(&mut self.mod_scratch_mult, 1.0);
        resize_if_needed(&mut self.gate_buffer, 0.0);
        resize_if_needed(&mut self.scratch_freq, self.frequency);
        resize_if_needed(&mut self.scratch_phase_mod, 0.0);
        resize_if_needed(&mut self.scratch_gain_mod, self.smoothed_gain);
        resize_if_needed(
            &mut self.scratch_feedback_mod,
            self.smoothed_feedback_amount,
        );
        resize_if_needed(&mut self.scratch_mod_index, self.smoothed_phase_mod_amount);
        resize_if_needed(
            &mut self.scratch_wavetable_index,
            self.smoothed_wavetable_index,
        );
        resize_if_needed(&mut self.scratch_detune_mod, 0.0);
        resize_if_needed(&mut self.global_freq_buffer, self.frequency);
    }

    pub fn set_current_wavetable(&mut self, collection_name: &str) {
        self.collection_name = collection_name.to_string();
    }

    pub fn update_params(&mut self, params: &WavetableOscillatorStateUpdate) {
        self.target_gain = params.gain;
        self.target_feedback_amount = params.feedback_amount;
        self.target_phase_mod_amount = params.phase_mod_amount;
        self.target_detune = params.detune;

        let max_spread_cents = 100.0;
        self.target_spread = params.spread.clamp(0.0, max_spread_cents);

        self.target_wavetable_index = params.wavetable_index;
        self.hard_sync = params.hard_sync;
        self.active = params.active;

        let new_voice_count = if params.unison_voices == 0 {
            1
        } else {
            params.unison_voices as usize
        };
        if new_voice_count != self.unison_voices {
            self.unison_voices = new_voice_count;
            self.voice_phases.resize(new_voice_count, 0.0);
            self.voice_last_outputs.resize(new_voice_count, 0.0);
            self.update_voice_unison_values(self.target_spread);
        }
    }

    fn update_voice_unison_values(&mut self, spread_cents: f32) {
        let n = self.unison_voices;
        let half = spread_cents / 2.0;

        self.voice_weights.resize(n, 1.0);
        self.voice_offsets.resize(n, 0.0);

        if n > 1 {
            for (i, offset) in self.voice_offsets.iter_mut().enumerate() {
                let norm = (i as f32 / (n - 1) as f32) * 2.0 - 1.0;
                *offset = (norm * half) / 100.0;
            }
        }
    }

    /// Original scalar gate‑check (for multi‑voice path).
    #[inline(always)]
    fn check_gate(&mut self, gate: f32) {
        if self.hard_sync && gate > 0.0 && self.last_gate_value <= 0.0 {
            for phase in self.voice_phases.iter_mut() {
                *phase = 0.0;
            }
        }
        self.last_gate_value = gate;
    }

    /// New SIMD gate‑check: also resets the local `phase`.
    #[inline(always)]
    fn check_gate_simd(&mut self, gate: f32, phase: &mut f32) {
        if self.hard_sync && gate > 0.0 && self.last_gate_value <= 0.0 {
            for ph in self.voice_phases.iter_mut() {
                *ph = 0.0;
            }
            *phase = 0.0;
        }
        self.last_gate_value = gate;
    }

    #[inline(always)]
    fn process_simd_single_voice(
        &mut self,
        output: &mut [f32],
        buf_size: usize,
        max_wt_index: f32,
        sr_recip: f32,
        sem_ratio: f32,
        two_pi_recip: f32,
        fb_div: f32,
        base_detune: f32,
        gate_buf: &[f32],
        coll: &Rc<WavetableMorphCollection>,
    ) {
        type F32x4 = Simd<f32, 4>;

        let mut phase = self.voice_phases[0];
        let mut last_out = self.voice_last_outputs[0];
        let lanes = 4;
        let chunks = buf_size / lanes;

        for chunk in 0..chunks {
            let base = chunk * lanes;

            // load SIMD lanes
            let freq_v = F32x4::from_array([
                self.scratch_freq[base],
                self.scratch_freq[base + 1],
                self.scratch_freq[base + 2],
                self.scratch_freq[base + 3],
            ]);
            let pm_v = F32x4::from_array([
                self.scratch_phase_mod[base],
                self.scratch_phase_mod[base + 1],
                self.scratch_phase_mod[base + 2],
                self.scratch_phase_mod[base + 3],
            ]);
            let idx_v = F32x4::from_array([
                self.scratch_mod_index[base],
                self.scratch_mod_index[base + 1],
                self.scratch_mod_index[base + 2],
                self.scratch_mod_index[base + 3],
            ]);
            let gain_v = F32x4::from_array([
                self.scratch_gain_mod[base],
                self.scratch_gain_mod[base + 1],
                self.scratch_gain_mod[base + 2],
                self.scratch_gain_mod[base + 3],
            ]);
            let fb_v = F32x4::from_array([
                self.scratch_feedback_mod[base],
                self.scratch_feedback_mod[base + 1],
                self.scratch_feedback_mod[base + 2],
                self.scratch_feedback_mod[base + 3],
            ]);
            let wt_v = F32x4::from_array([
                self.scratch_wavetable_index[base].clamp(0.0, max_wt_index),
                self.scratch_wavetable_index[base + 1].clamp(0.0, max_wt_index),
                self.scratch_wavetable_index[base + 2].clamp(0.0, max_wt_index),
                self.scratch_wavetable_index[base + 3].clamp(0.0, max_wt_index),
            ]);
            let det_v = F32x4::from_array([
                self.scratch_detune_mod[base],
                self.scratch_detune_mod[base + 1],
                self.scratch_detune_mod[base + 2],
                self.scratch_detune_mod[base + 3],
            ]);

            // compute semitone offsets
            let total_det = F32x4::splat(self.voice_offsets[0]) + det_v;
            let mut sems = [0.0; 4];
            for j in 0..lanes {
                sems[j] = sem_ratio.powf(total_det[j]);
            }
            let sem_v = F32x4::from_array(sems);

            // effective frequency & increment
            let eff_freq = freq_v * F32x4::splat(base_detune) * sem_v;
            let inc = eff_freq * F32x4::splat(sr_recip);

            let mut out_chunk = [0.0; 4];
            for j in 0..lanes {
                self.check_gate_simd(gate_buf[base + j], &mut phase);
                phase = (phase + inc[j]).rem_euclid(1.0);

                let offset = (pm_v[j] * idx_v[j]) * two_pi_recip;
                let voice_fb = (last_out * fb_v[j]) / fb_div;
                let lookup = (phase + offset + voice_fb).rem_euclid(1.0);

                let sample = coll.lookup_sample(lookup, wt_v[j], eff_freq[j]);
                last_out = sample;
                out_chunk[j] = sample * gain_v[j];
            }

            output[base..base + lanes].copy_from_slice(&out_chunk);
        }

        // handle remainder
        let rem_base = chunks * lanes;
        for i in 0..(buf_size % lanes) {
            let idx = rem_base + i;
            self.check_gate_simd(gate_buf[idx], &mut phase);

            let cf = self.scratch_freq[idx];
            let pm = self.scratch_phase_mod[idx];
            let idx_mod = self.scratch_mod_index[idx];
            let fb = self.scratch_feedback_mod[idx];
            let gain = self.scratch_gain_mod[idx];
            let wt_i = self.scratch_wavetable_index[idx].clamp(0.0, max_wt_index);
            let det = self.scratch_detune_mod[idx];

            let total_det = self.voice_offsets[0] + det;
            let sem = sem_ratio.powf(total_det);
            let ef = cf * base_detune * sem;
            let inc = ef * sr_recip;

            phase = (phase + inc).rem_euclid(1.0);
            let offset = (pm * idx_mod) * two_pi_recip;
            let voice_fb = (last_out * fb) / fb_div;
            let lookup = (phase + offset + voice_fb).rem_euclid(1.0);

            let sample = coll.lookup_sample(lookup, wt_i, ef);
            last_out = sample;
            output[idx] = sample * gain;
        }

        self.voice_phases[0] = phase;
        self.voice_last_outputs[0] = last_out;
    }

    pub fn process<'a>(
        &mut self,
        inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
        buffer_size: usize,
    ) {
        // inactive → clear and return
        if !self.active {
            if let Some(buf) = outputs.get_mut(&PortId::AudioOutput0) {
                buf[..buffer_size].fill(0.0);
            }
            if let Some(buf) = outputs.get_mut(&PortId::AudioOutput1) {
                buf[..buffer_size].fill(0.0);
            }
            return;
        }

        self.ensure_scratch_buffers(buffer_size);

        // Check that we have at least one output
        let has_out0 = outputs.contains_key(&PortId::AudioOutput0);
        let has_out1 = outputs.contains_key(&PortId::AudioOutput1);
        if !has_out0 && !has_out1 {
            return;
        }

        // — Parameter smoothing —
        let alpha = (self.smoothing_coeff * buffer_size as f32).min(1.0);
        self.smoothed_gain += alpha * (self.target_gain - self.smoothed_gain);
        self.smoothed_feedback_amount +=
            alpha * (self.target_feedback_amount - self.smoothed_feedback_amount);
        self.smoothed_phase_mod_amount +=
            alpha * (self.target_phase_mod_amount - self.smoothed_phase_mod_amount);
        let prev_spread = self.smoothed_spread;
        self.smoothed_spread += alpha * (self.target_spread - self.smoothed_spread);
        self.smoothed_wavetable_index +=
            alpha * (self.target_wavetable_index - self.smoothed_wavetable_index);

        // only recalc offsets on >0.5 cents change or voice‑count mismatch
        if (self.smoothed_spread - prev_spread).abs() > 0.5
            || self.voice_offsets.len() != self.unison_voices
        {
            self.update_voice_unison_values(self.smoothed_spread);
        }

        // — Per‑port modulation helper —
        let mut process_mod_input = |port: PortId, base: f32, target: &mut [f32]| {
            self.mod_scratch_add[..buffer_size].fill(0.0);
            self.mod_scratch_mult[..buffer_size].fill(1.0);

            if let Some(srcs) = inputs.get(&port).filter(|s| !s.is_empty()) {
                Self::accumulate_modulations_inplace(
                    buffer_size,
                    Some(srcs.as_slice()),
                    &mut self.mod_scratch_add,
                    &mut self.mod_scratch_mult,
                );
                Self::combine_modulation_inplace(
                    &mut target[..buffer_size],
                    buffer_size,
                    base,
                    &self.mod_scratch_add,
                    &self.mod_scratch_mult,
                );
            } else {
                target[..buffer_size].fill(base);
            }
        };

        process_mod_input(PortId::PhaseMod, 0.0, &mut self.scratch_phase_mod);
        process_mod_input(
            PortId::GainMod,
            self.smoothed_gain,
            &mut self.scratch_gain_mod,
        );
        process_mod_input(
            PortId::FeedbackMod,
            self.smoothed_feedback_amount,
            &mut self.scratch_feedback_mod,
        );
        process_mod_input(
            PortId::ModIndex,
            self.smoothed_phase_mod_amount,
            &mut self.scratch_mod_index,
        );
        process_mod_input(
            PortId::WavetableIndex,
            self.smoothed_wavetable_index,
            &mut self.scratch_wavetable_index,
        );
        process_mod_input(PortId::DetuneMod, 0.0, &mut self.scratch_detune_mod);

        // — Gate / hard‑sync —
        self.gate_buffer[..buffer_size].fill(0.0);
        if let Some(gates) = inputs.get(&PortId::GlobalGate) {
            for src in gates {
                Self::apply_add(
                    src.buffer,
                    &mut self.gate_buffer[..buffer_size],
                    src.amount,
                    src.transformation,
                );
            }
        }

        // — Frequency‑mod scratch —
        self.mod_scratch_add[..buffer_size].fill(0.0);
        self.mod_scratch_mult[..buffer_size].fill(1.0);
        if let Some(freqs) = inputs.get(&PortId::FrequencyMod) {
            if !freqs.is_empty() {
                Self::accumulate_modulations_inplace(
                    buffer_size,
                    Some(freqs.as_slice()),
                    &mut self.mod_scratch_add,
                    &mut self.mod_scratch_mult,
                );
            }
        }

        // — Global frequency input —
        if let Some(glob) = inputs.get(&PortId::GlobalFrequency) {
            if !glob.is_empty() && !glob[0].buffer.is_empty() {
                let src = &glob[0].buffer;
                let n = src.len().min(buffer_size);
                self.global_freq_buffer[..n].copy_from_slice(&src[..n]);
                if n < buffer_size {
                    let last = src[n - 1];
                    self.global_freq_buffer[n..buffer_size].fill(last);
                }
                Self::combine_modulation_inplace_varying_base(
                    &mut self.scratch_freq[..buffer_size],
                    buffer_size,
                    &self.global_freq_buffer,
                    &self.mod_scratch_add,
                    &self.mod_scratch_mult,
                );
            } else {
                Self::combine_modulation_inplace(
                    &mut self.scratch_freq[..buffer_size],
                    buffer_size,
                    self.frequency,
                    &self.mod_scratch_add,
                    &self.mod_scratch_mult,
                );
            }
        } else {
            Self::combine_modulation_inplace(
                &mut self.scratch_freq[..buffer_size],
                buffer_size,
                self.frequency,
                &self.mod_scratch_add,
                &self.mod_scratch_mult,
            );
        }

        // Correct unison if mismatched
        if self.voice_offsets.len() != self.unison_voices {
            #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
            console::warn_1(
                &format!(
                    "Correcting voice offset count ({} vs {})",
                    self.voice_offsets.len(),
                    self.unison_voices
                )
                .into(),
            );
            #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
            eprintln!(
                "Correcting voice offset count ({} vs {})",
                self.voice_offsets.len(),
                self.unison_voices
            );
            self.update_voice_unison_values(self.smoothed_spread);
        }

        let gate_buf = self.gate_buffer[..buffer_size].to_vec();

        // Precompute constants & fetch collection
        // Gracefully handle missing wavetable (output silence until it's loaded)
        let collection = {
            let bank_ref = self.wavetable_bank.borrow();
            match bank_ref.get_collection(&self.collection_name) {
                Some(coll) => coll.clone(),
                None => {
                    // Wavetable not loaded yet - output silence
                    if let Some(buf) = outputs.get_mut(&PortId::AudioOutput0) {
                        buf[..buffer_size].fill(0.0);
                    }
                    if let Some(buf) = outputs.get_mut(&PortId::AudioOutput1) {
                        buf[..buffer_size].fill(0.0);
                    }
                    return;
                }
            }
        };
        let max_wt_index = collection.num_tables() as f32 - 1.0001;
        let base_detune_factor = self.cent_ratio.powf(self.target_detune);
        let sr_recip = self.sample_rate_recip;
        let semitone_ratio = self.semitone_ratio;
        let two_pi_recip = self.two_pi_recip;
        let feedback_divisor = self.feedback_divisor;

        let total_weight: f32 = self.voice_weights.iter().sum();
        let norm_weight = if total_weight == 0.0 {
            1.0
        } else {
            1.0 / total_weight
        };

        // Initialize output buffers
        if let Some(buf) = outputs.get_mut(&PortId::AudioOutput0) {
            buf[..buffer_size].fill(0.0);
        }
        if let Some(buf) = outputs.get_mut(&PortId::AudioOutput1) {
            buf[..buffer_size].fill(0.0);
        }

        // Hot‑path
        if self.unison_voices == 1 {
            // For single voice, use a temporary buffer then copy to both channels
            let mut temp_buffer = vec![0.0f32; buffer_size];
            self.process_simd_single_voice(
                &mut temp_buffer,
                buffer_size,
                max_wt_index,
                sr_recip,
                semitone_ratio,
                two_pi_recip,
                feedback_divisor,
                base_detune_factor,
                &gate_buf,
                &collection,
            );
            // Copy mono to both stereo channels
            if let Some(buf) = outputs.get_mut(&PortId::AudioOutput0) {
                buf[..buffer_size].copy_from_slice(&temp_buffer);
            }
            if let Some(buf) = outputs.get_mut(&PortId::AudioOutput1) {
                buf[..buffer_size].copy_from_slice(&temp_buffer);
            }
        } else {
            // Multi-voice: pan voices across stereo field
            let total_voices = self.unison_voices as f32;

            for i in 0..buffer_size {
                self.check_gate(self.gate_buffer[i]);

                let cf = self.scratch_freq[i];
                let pm = self.scratch_phase_mod[i];
                let idx_mod = self.scratch_mod_index[i];
                let fb_amt = self.scratch_feedback_mod[i];
                let gain = self.scratch_gain_mod[i];
                let wt_i = self.scratch_wavetable_index[i].clamp(0.0, max_wt_index);
                let det_mod = self.scratch_detune_mod[i];

                let mut sum_l = 0.0;
                let mut sum_r = 0.0;

                for v in 0..self.unison_voices {
                    let offset = self.voice_offsets[v];
                    let sem_off = semitone_ratio.powf(offset + det_mod);
                    let eff_freq = cf * base_detune_factor * sem_off;
                    let inc = eff_freq * sr_recip;

                    let new_phase = (self.voice_phases[v] + inc).rem_euclid(1.0);
                    let fb_val = (self.voice_last_outputs[v] * fb_amt) / feedback_divisor;
                    let lookup =
                        (new_phase + (pm * idx_mod) * two_pi_recip + fb_val).rem_euclid(1.0);

                    let s = collection.lookup_sample(lookup, wt_i, eff_freq);

                    // Calculate pan position for this voice: -1 (left) to +1 (right)
                    let pan = if total_voices > 1.0 {
                        (v as f32 / (total_voices - 1.0)) * 2.0 - 1.0
                    } else {
                        0.0
                    };

                    // Equal power panning
                    let pan_norm = (pan + 1.0) * 0.5; // Normalize to 0..1
                    let gain_l = ((1.0 - pan_norm) * std::f32::consts::FRAC_PI_2).cos();
                    let gain_r = (pan_norm * std::f32::consts::FRAC_PI_2).cos();

                    sum_l += s * self.voice_weights[v] * gain_l;
                    sum_r += s * self.voice_weights[v] * gain_r;

                    self.voice_phases[v] = new_phase;
                    self.voice_last_outputs[v] = s;
                }

                let final_l = sum_l * norm_weight * gain;
                let final_r = sum_r * norm_weight * gain;

                if let Some(buf) = outputs.get_mut(&PortId::AudioOutput0) {
                    buf[i] = final_l;
                }
                if let Some(buf) = outputs.get_mut(&PortId::AudioOutput1) {
                    buf[i] = final_r;
                }
            }
        }
    }

    pub fn reset(&mut self) {
        self.last_gate_value = 0.0;
        for ph in &mut self.voice_phases {
            *ph = 0.0;
        }
        for out in &mut self.voice_last_outputs {
            *out = 0.0;
        }

        let initial_gain = 1.0;
        let initial_feedback = 0.0;
        let initial_phase_mod = 0.0;
        let initial_detune = 0.0;
        let initial_spread = 10.0;
        let initial_wt_index = 0.0;
        let max_spread_cents = 100.0;

        self.target_gain = initial_gain;
        self.target_feedback_amount = initial_feedback;
        self.target_phase_mod_amount = initial_phase_mod;
        self.target_detune = initial_detune;
        self.target_spread = initial_spread.clamp(0.0, max_spread_cents);
        self.target_wavetable_index = initial_wt_index;

        self.smoothed_gain = initial_gain;
        self.smoothed_feedback_amount = initial_feedback;
        self.smoothed_phase_mod_amount = initial_phase_mod;
        self.smoothed_spread = initial_spread.clamp(0.0, max_spread_cents);
        self.smoothed_wavetable_index = initial_wt_index;

        self.update_voice_unison_values(self.smoothed_spread);
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
        "Wavetable Oscillator"
    }
    fn node_type(&self) -> &str {
        "wavetable_oscillator"
    }
}

impl AudioNode for WavetableOscillator {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        [
            (PortId::GlobalFrequency, false),
            (PortId::FrequencyMod, false),
            (PortId::PhaseMod, false),
            (PortId::ModIndex, false),
            (PortId::WavetableIndex, false),
            (PortId::GainMod, false),
            (PortId::FeedbackMod, false),
            (PortId::DetuneMod, false),
            (PortId::GlobalGate, false),
            (PortId::AudioOutput0, true),
            (PortId::AudioOutput1, true),
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
        // call our internal process()
        self.process(inputs, outputs, buffer_size);
    }

    fn reset(&mut self) {
        self.reset();
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self.as_any_mut()
    }
    fn as_any(&self) -> &dyn Any {
        self.as_any()
    }
    fn is_active(&self) -> bool {
        self.is_active()
    }
    fn set_active(&mut self, active: bool) {
        self.set_active(active)
    }
    fn name(&self) -> &'static str {
        WavetableOscillator::name(self)
    }
    fn node_type(&self) -> &str {
        self.node_type()
    }
}
