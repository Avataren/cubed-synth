// --- IMPORTS and other parts remain the same ---
use std::any::Any;
use std::sync::OnceLock;

use rustc_hash::FxHashMap;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::wasm_bindgen;

use crate::graph::{ModulationProcessor, ModulationSource};
use crate::traits::{AudioNode, PortId};
use serde::{Deserialize, Serialize};

// --- Enums, LfoTables, Constants remain the same ---
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LfoWaveform {
    Sine,
    Triangle,
    Square,
    Saw,
    InverseSaw,
}
impl LfoWaveform {
    #[inline(always)]
    fn normalized_phase_offset(self) -> f32 {
        match self {
            LfoWaveform::Sine | LfoWaveform::Triangle => -0.25,
            _ => 0.0,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LfoRetriggerMode {
    FreeRunning,
    StartOnGate,
    Retrigger,
    OneShot,
}
impl LfoRetriggerMode {
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => LfoRetriggerMode::FreeRunning,
            1 => LfoRetriggerMode::StartOnGate,
            2 => LfoRetriggerMode::Retrigger,
            3 => LfoRetriggerMode::OneShot,
            _ => LfoRetriggerMode::FreeRunning,
        }
    }
}
#[cfg_attr(feature = "wasm", wasm_bindgen)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LfoLoopMode {
    Off = 0,      // Repeats full 0..1 waveform (unless OneShot)
    Loop = 1,     // Starts at 0.0, runs to loop_end, then loops between loop_start and loop_end
    PingPong = 2, // Starts at 0.0, runs to loop_end, then bounces between loop_start and loop_end
}
struct LfoTables {
    sine: Vec<f32>,
    triangle: Vec<f32>,
    square: Vec<f32>,
    saw: Vec<f32>,
    inverse_saw: Vec<f32>,
}
const TABLE_SIZE: usize = 1024;
const TABLE_MASK: usize = TABLE_SIZE - 1;
const TABLE_SIZE_F32: f32 = TABLE_SIZE as f32;
static LFO_TABLES: OnceLock<LfoTables> = OnceLock::new();
impl LfoTables {
    fn new() -> Self {
        let mut sine = vec![0.0; TABLE_SIZE];
        let mut triangle = vec![0.0; TABLE_SIZE];
        let mut square = vec![0.0; TABLE_SIZE];
        let mut saw = vec![0.0; TABLE_SIZE];
        let mut inverse_saw = vec![0.0; TABLE_SIZE];

        for i in 0..TABLE_SIZE {
            let phase_rad = 2.0 * std::f32::consts::PI * (i as f32) / TABLE_SIZE_F32;
            let phase_norm = i as f32 / TABLE_SIZE_F32;

            sine[i] = phase_rad.sin();
            triangle[i] = 2.0 * (2.0 * (phase_norm + 0.25).fract() - 1.0).abs() - 1.0;
            square[i] = if phase_norm < 0.5 { 1.0 } else { -1.0 };
            saw[i] = 2.0 * phase_norm - 1.0;
            inverse_saw[i] = 1.0 - 2.0 * phase_norm;
        }

        Self {
            sine,
            triangle,
            square,
            saw,
            inverse_saw,
        }
    }
    #[inline(always)]
    fn get_table(&self, waveform: LfoWaveform) -> &[f32] {
        match waveform {
            LfoWaveform::Sine => &self.sine,
            LfoWaveform::Triangle => &self.triangle,
            LfoWaveform::Square => &self.square,
            LfoWaveform::Saw => &self.saw,
            LfoWaveform::InverseSaw => &self.inverse_saw,
        }
    }
}

// --- LFO Node Struct ---
pub struct Lfo {
    // Parameters
    sample_rate: f32,
    base_frequency: f32,
    base_gain: f32,
    waveform: LfoWaveform,
    phase_offset: f32, // Applied *before* table lookup
    use_absolute: bool,
    use_normalized: bool,
    pub retrigger_mode: LfoRetriggerMode,
    loop_mode: LfoLoopMode,
    loop_start: f32, // Normalized phase [0.0, 1.0) - Target after first loop_end hit
    loop_end: f32,   // Normalized phase (0.0, 1.0] - Point where looping begins
    active: bool,

    // State
    phase: f32,              // Current phase [0.0, 1.0)
    direction: f32,          // 1.0 or -1.0 (only for PingPong *after* initial run)
    last_gate: f32,          // Stores the gate value from the *previous sample* processed
    is_running: bool, // Tracks if phase should advance (esp. for OneShot completion/Retrigger gating)
    oneshot_held_value: f32, // Value held after OneShot completes
    // NEW state: Tracks if the initial run from 0.0 up to loop_end has completed
    has_reached_loop_end_once: bool,

    // === Scratch Buffers ===
    mod_scratch_add: Vec<f32>,
    mod_scratch_mult: Vec<f32>,
    gate_buffer: Vec<f32>,
    scratch_freq_add: Vec<f32>,
    scratch_freq_mult: Vec<f32>,
    scratch_gain_add: Vec<f32>,
    scratch_gain_mult: Vec<f32>,
}

impl Lfo {
    pub fn new(sample_rate: f32) -> Self {
        LFO_TABLES.get_or_init(LfoTables::new);
        let initial_capacity = 128; // Default buffer size

        Self {
            sample_rate,
            base_frequency: 1.0,
            base_gain: 1.0,
            waveform: LfoWaveform::Sine,
            phase_offset: 0.0,
            use_absolute: false,
            use_normalized: false,
            retrigger_mode: LfoRetriggerMode::FreeRunning,
            loop_mode: LfoLoopMode::Off, // Default to Off
            loop_start: 0.0,
            loop_end: 1.0,
            active: true,
            phase: 0.0,     // Always start at 0.0 initially
            direction: 1.0, // Always start going forwards
            last_gate: 0.0,
            is_running: true, // Start running in FreeRunning mode
            oneshot_held_value: 0.0,
            has_reached_loop_end_once: false, // Start before the loop point is hit
            // Initialize scratch buffers
            mod_scratch_add: vec![0.0; initial_capacity],
            mod_scratch_mult: vec![1.0; initial_capacity],
            gate_buffer: vec![0.0; initial_capacity],
            scratch_freq_add: vec![0.0; initial_capacity],
            scratch_freq_mult: vec![1.0; initial_capacity],
            scratch_gain_add: vec![0.0; initial_capacity],
            scratch_gain_mult: vec![1.0; initial_capacity],
        }
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
        resize_if_needed(&mut self.scratch_freq_add, 0.0);
        resize_if_needed(&mut self.scratch_freq_mult, 1.0);
        resize_if_needed(&mut self.scratch_gain_add, 0.0);
        resize_if_needed(&mut self.scratch_gain_mult, 1.0);
    }

    // --- Parameter Setters ---
    pub fn set_gain(&mut self, gain: f32) {
        self.base_gain = gain.max(0.0);
    }
    pub fn set_frequency(&mut self, freq: f32) {
        self.base_frequency = freq.max(0.0);
    }
    pub fn set_waveform(&mut self, waveform: LfoWaveform) {
        self.waveform = waveform;
    }
    pub fn set_phase_offset(&mut self, offset: f32) {
        self.phase_offset = offset;
    }
    pub fn set_use_absolute(&mut self, use_absolute: bool) {
        self.use_absolute = use_absolute;
    }
    pub fn set_use_normalized(&mut self, use_normalized: bool) {
        self.use_normalized = use_normalized;
    }

    pub fn set_retrigger_mode(&mut self, mode: LfoRetriggerMode) {
        if mode != self.retrigger_mode {
            self.retrigger_mode = mode;
            // Reset state based on new mode and current gate, always starting at phase 0
            self.reset_state_for_mode(self.last_gate > 0.0);
        }
    }

    pub fn set_loop_mode(&mut self, loop_mode: LfoLoopMode) {
        if loop_mode != self.loop_mode {
            self.loop_mode = loop_mode;
            // Changing loop mode forces a restart from phase 0.0 and resets the loop flag
            // Determine initial running state based on current gate and new mode.
            self.reset_state_for_mode(self.last_gate > 0.0);
        }
    }

    pub fn set_loop_start(&mut self, start: f32) {
        let clamped_start = start.clamp(0.0, 1.0);
        // Ensure start <= end, cannot be exactly equal if loop_end is 0.0
        self.loop_start = clamped_start.min(self.loop_end);
        // Clamp current phase ONLY if we are *already* in the looping segment (past the initial run)
        if self.has_reached_loop_end_once && self.loop_mode != LfoLoopMode::Off {
            self.phase = self.phase.clamp(self.loop_start, self.loop_end);
        }
    }

    pub fn set_loop_end(&mut self, end: f32) {
        let clamped_end = end.clamp(0.0, 1.0);
        // Ensure end >= start
        self.loop_end = clamped_end.max(self.loop_start);
        // If loop_start == loop_end, looping behavior might be static or jumpy, ensure loop_end > 0 if loop_start is 0?
        // Current logic handles loop_start == loop_end (results in static phase or immediate jump/reflection).

        // Clamp current phase ONLY if we are *already* in the looping segment
        if self.has_reached_loop_end_once && self.loop_mode != LfoLoopMode::Off {
            self.phase = self.phase.clamp(self.loop_start, self.loop_end);
        }
        // If we haven't reached loop end yet, changing loop_end could affect when the transition happens.
        // If the new loop_end is now *below* the current phase during the initial run, we should arguably
        // trigger the loop transition immediately.
        else if !self.has_reached_loop_end_once
            && self.loop_mode != LfoLoopMode::Off
            && self.phase >= self.loop_end
        {
            // Trigger the first loop transition manually
            self.trigger_first_loop_transition(0.0); // Assume zero overflow/overshoot for simplicity
        }
    }

    /// Internal helper to handle the transition logic when phase first reaches loop_end.
    /// `overflow_or_overshoot` is the amount phase went past loop_end.
    #[inline(always)]
    fn trigger_first_loop_transition(&mut self, overflow_or_overshoot: f32) {
        if self.has_reached_loop_end_once {
            return;
        } // Should not happen, but safety check

        let loop_width = (self.loop_end - self.loop_start).max(f32::EPSILON);
        self.has_reached_loop_end_once = true; // Engage looping behavior

        match self.loop_mode {
            LfoLoopMode::Loop => {
                // Jump to loop_start + overflow within the loop segment
                self.phase = self.loop_start + overflow_or_overshoot.rem_euclid(loop_width);
                // Ensure phase is clamped within loop bounds after the jump
                self.phase = self.phase.clamp(self.loop_start, self.loop_end);
            }
            LfoLoopMode::PingPong => {
                // Reflect back from loop_end
                self.phase = self.loop_end - overflow_or_overshoot.rem_euclid(loop_width); // Use rem_euclid for safety
                self.direction = -1.0; // Start moving backwards
                                       // Clamp in case of large overshoot or loop_width issues
                self.phase = self.phase.max(self.loop_start);
            }
            LfoLoopMode::Off => {
                // This function shouldn't be called for Off mode, but handle defensively
                self.has_reached_loop_end_once = false; // Reset flag for Off mode
            }
        }
    }

    /// Internal state reset logic. Sets phase to 0, resets flags, and sets initial running state.
    /// `should_run_now` indicates if the LFO should start in a running state immediately.
    fn reset_state_for_mode(&mut self, should_run_now: bool) {
        self.phase = 0.0; // ALWAYS reset phase to 0.0
        self.direction = 1.0; // Always start going forwards
        self.has_reached_loop_end_once = false; // Reset the loop detection flag
        self.is_running = match self.retrigger_mode {
            LfoRetriggerMode::FreeRunning => true, // Always running
            LfoRetriggerMode::Retrigger | LfoRetriggerMode::OneShot => should_run_now, // Run only if triggered now
            LfoRetriggerMode::StartOnGate => should_run_now, // Start running if triggered now (and will stay running)
        };
        // For OneShot, calculate initial value in case it finishes immediately (e.g., freq=0)
        if self.retrigger_mode == LfoRetriggerMode::OneShot && !self.is_running {
            self.oneshot_held_value = self.lookup_sample_at_phase(0.0); // Or calculate based on phase=1.0? Let's stick to 0.0 for consistency.
        } else if self.retrigger_mode == LfoRetriggerMode::OneShot && self.is_running {
            // Reset held value when triggered
            self.oneshot_held_value = 0.0; // Or maybe lookup_sample_at_phase(1.0)? Holding 0 seems safer until it finishes.
        }

        // NOTE: For OneShot, the held value is calculated *when it stops* inside internal_advance_phase.
        // This setup ensures `is_running` is correct initially.
    }

    #[inline(always)]
    fn lookup_sample_at_phase(&self, phase_value: f32) -> f32 {
        // ... (lookup remains the same) ...
        let tables = LFO_TABLES.get().expect("LFO tables not initialized");
        let table = tables.get_table(self.waveform);

        let extra_phase = if self.use_normalized {
            self.waveform.normalized_phase_offset()
        } else {
            0.0
        };

        // Apply phase offset and wrap phase to [0.0, 1.0) for table lookup
        let effective_phase = (phase_value + self.phase_offset + extra_phase).rem_euclid(1.0);

        let table_index_f = effective_phase * TABLE_SIZE_F32;
        let index1 = (table_index_f as usize) & TABLE_MASK;
        let index2 = (index1 + 1) & TABLE_MASK;
        let fraction = table_index_f - table_index_f.floor();

        // Linear interpolation
        let sample1 = table[index1];
        let sample2 = table[index2];
        let mut sample = sample1 + (sample2 - sample1) * fraction;

        if self.use_absolute {
            sample = sample.abs();
        }
        if self.use_normalized {
            // Assuming bipolar input [-1, 1] -> unipolar [0, 1]
            sample = (sample + 1.0) * 0.5;
        }
        sample
    }

    /// Advances phase for one sample based on mode. Manages loop transitions and OneShot.
    #[inline(always)]
    fn internal_advance_phase(&mut self, phase_increment: f32) {
        if !self.is_running {
            return;
        }

        let loop_width = (self.loop_end - self.loop_start).max(f32::EPSILON);

        // --- Check for initial transition ---
        if !self.has_reached_loop_end_once && self.loop_mode != LfoLoopMode::Off {
            let next_phase = self.phase + phase_increment; // Calculate potential next phase
            if next_phase >= self.loop_end {
                // Reached or exceeded loop_end on this step
                let overflow_or_overshoot = next_phase - self.loop_end;
                self.trigger_first_loop_transition(overflow_or_overshoot);
                // Phase is now set correctly for the start of the loop/pingpong segment, exit advance logic for this sample
                return;
            } else {
                // Still in the initial run phase, just update phase
                self.phase = next_phase;
                // Phase might need clamping to 1.0 if loop_end is 1.0? No, rem_euclid handles it later if needed.
                return; // Finished advancing for this sample
            }
        }

        // --- Apply standard phase advancement based on current state ---
        // (Either LoopMode::Off, or already past the initial run for Loop/PingPong)
        match self.loop_mode {
            LfoLoopMode::Off => {
                // Standard 0..1 behavior (wrap or stop)
                if self.retrigger_mode == LfoRetriggerMode::OneShot {
                    self.phase += phase_increment;
                    if self.phase >= 1.0 {
                        self.phase = 1.0; // Clamp at end
                        self.is_running = false; // Stop
                        self.oneshot_held_value = self.lookup_sample_at_phase(self.phase);
                        // Hold final value
                    }
                } else {
                    // Continuous wrap 0..1
                    self.phase = (self.phase + phase_increment).rem_euclid(1.0);
                }
            }

            LfoLoopMode::Loop => {
                // Assumes has_reached_loop_end_once is true here
                self.phase += phase_increment; // Direction is always 1.0 for Loop
                if self.phase >= self.loop_end {
                    let overflow = self.phase - self.loop_end;
                    self.phase = self.loop_start + overflow % loop_width;
                } else if self.phase < self.loop_start {
                    // Handle negative freq mod during loop
                    let underflow = self.loop_start - self.phase;
                    self.phase = self.loop_end - underflow % loop_width;
                    // Clamp just in case
                    self.phase = self.phase.clamp(self.loop_start, self.loop_end);
                }
                // Final clamp might be redundant but safe
                // self.phase = self.phase.clamp(self.loop_start, self.loop_end);
            }

            LfoLoopMode::PingPong => {
                // Assumes has_reached_loop_end_once is true here
                self.phase += phase_increment * self.direction;
                if self.direction > 0.0 && self.phase >= self.loop_end {
                    let overshoot = self.phase - self.loop_end;
                    self.phase = self.loop_end - overshoot; // Reflect
                    self.direction = -1.0;
                    self.phase = self.phase.max(self.loop_start); // Clamp after reflection
                } else if self.direction < 0.0 && self.phase <= self.loop_start {
                    let undershoot = self.loop_start - self.phase;
                    self.phase = self.loop_start + undershoot; // Reflect
                    self.direction = 1.0;
                    self.phase = self.phase.min(self.loop_end); // Clamp after reflection
                }
                // Final clamp just to be safe
                // self.phase = self.phase.clamp(self.loop_start, self.loop_end);
            }
        }
    }

    /// Public method to advance phase for a buffer when inactive (FreeRunning only).
    /// NOTE: Approximation accuracy for the new "start at 0" logic is limited.
    pub fn advance_phase_for_buffer(&mut self, buffer_size: usize) {
        if self.retrigger_mode != LfoRetriggerMode::FreeRunning || !self.is_running {
            return;
        }
        let total_phase_increment = (self.base_frequency * buffer_size as f32) / self.sample_rate;

        match self.loop_mode {
            LfoLoopMode::Off => {
                // Simple wrap for Off mode
                self.phase = (self.phase + total_phase_increment).rem_euclid(1.0);
            }
            LfoLoopMode::Loop | LfoLoopMode::PingPong => {
                if !self.has_reached_loop_end_once {
                    // --- Approximating initial run ---
                    let phase_after_inc = self.phase + total_phase_increment;
                    if phase_after_inc >= self.loop_end {
                        // We likely crossed the loop_end threshold during this buffer
                        // Trigger the transition based on estimated overflow
                        let overflow = phase_after_inc - self.loop_end;
                        self.trigger_first_loop_transition(overflow);
                        // Note: This assumes the buffer didn't also cross loop_start going backwards in PingPong mode,
                        // which is unlikely but possible with large increments. The approximation is limited here.
                    } else {
                        // Didn't reach loop_end yet
                        self.phase = phase_after_inc;
                    }
                } else {
                    // --- Approximating behavior *within* the loop segment ---
                    let loop_width = (self.loop_end - self.loop_start).max(f32::EPSILON);
                    if self.loop_mode == LfoLoopMode::Loop {
                        // Standard loop approximation
                        self.phase = self.loop_start
                            + (self.phase - self.loop_start + total_phase_increment)
                                .rem_euclid(loop_width);
                    } else {
                        // PingPong approximation
                        if loop_width <= f32::EPSILON {
                            return;
                        } // Avoid div by zero

                        let mut remaining_increment = total_phase_increment;
                        let mut current_phase = self.phase;
                        let mut current_direction = self.direction;

                        // Estimate traversals - this is complex to get perfect
                        while remaining_increment > 0.0 {
                            let distance_to_boundary = if current_direction > 0.0 {
                                (self.loop_end - current_phase).max(0.0)
                            } else {
                                (current_phase - self.loop_start).max(0.0)
                            };

                            let increment_this_step = remaining_increment.min(distance_to_boundary);
                            current_phase += increment_this_step * current_direction;
                            remaining_increment -= increment_this_step;

                            if remaining_increment > f32::EPSILON {
                                // Hit a boundary, reverse direction
                                current_direction *= -1.0;
                                // Consume tiny amount to avoid infinite loops at boundary
                                remaining_increment -= f32::EPSILON;
                            }
                        }
                        self.phase = current_phase.clamp(self.loop_start, self.loop_end);
                        self.direction = current_direction;
                    }
                }
            }
        }
        // Final safety clamp? Might hide bugs in approximation.
        // self.phase = self.phase.clamp(0.0, 1.0);
    }

    // --- get_waveform_data remains the same ---
    pub fn get_waveform_data(
        waveform: LfoWaveform,
        phase_offset: f32,
        cycles_per_buffer: f32, // Frequency interpretation for visualization
        buffer_size: usize,
        use_absolute: bool,
        use_normalized: bool,
    ) -> Vec<f32> {
        // Ensure tables are initialized
        let tables = LFO_TABLES.get_or_init(LfoTables::new);
        let table = tables.get_table(waveform);
        let mut buffer = vec![0.0; buffer_size];
        if buffer_size == 0 {
            return buffer;
        } // Handle zero size

        // Calculate phase increment per sample for visualization
        let phase_increment = cycles_per_buffer / (buffer_size as f32);
        let mut current_phase = 0.0;

        let extra_phase_offset = if use_normalized {
            waveform.normalized_phase_offset()
        } else {
            0.0
        } + phase_offset;

        for i in 0..buffer_size {
            // Use the standard lookup function which handles phase offset and wrapping
            let effective_phase = (current_phase + extra_phase_offset).rem_euclid(1.0);
            let table_index_f = effective_phase * TABLE_SIZE_F32;
            let index1 = (table_index_f as usize) & TABLE_MASK;
            let index2 = (index1 + 1) & TABLE_MASK;
            let fraction = table_index_f - table_index_f.floor();

            let sample1 = table[index1];
            let sample2 = table[index2];
            let mut sample = sample1 + (sample2 - sample1) * fraction;

            if use_absolute {
                sample = sample.abs();
            }
            if use_normalized {
                sample = (sample + 1.0) * 0.5;
            }

            buffer[i] = sample;

            current_phase += phase_increment;
            // No need to wrap current_phase here, rem_euclid handles it
        }
        buffer
    }
} // End impl Lfo

// --- ModulationProcessor Implementation remains the same ---
impl ModulationProcessor for Lfo {}

// --- AudioNode Implementation ---
impl AudioNode for Lfo {
    fn get_ports(&self) -> FxHashMap<PortId, bool> {
        [
            (PortId::FrequencyMod, false), // Modulation for frequency
            (PortId::GainMod, false),      // Modulation for output gain/amplitude
            (PortId::CombinedGate, false), // Gate input for retriggering/one-shot
            (PortId::AudioOutput0, true),  // LFO output signal
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
        // --- 0) Early exit and Buffer Preparation ---
        if !self.active {
            if let Some(output_buffer) = outputs.get_mut(&PortId::AudioOutput0) {
                // Fill with value at phase 0 when inactive? Or just 0.0? Let's use 0.0 for simplicity.
                output_buffer[..buffer_size].fill(0.0);
                // Alternative: Fill with lookup_sample_at_phase(0.0) * base_gain
                // let inactive_val = self.lookup_sample_at_phase(0.0) * self.base_gain.max(0.0);
                // output_buffer[..buffer_size].fill(inactive_val);
            }
            self.advance_phase_for_buffer(buffer_size); // Still advance FreeRunning LFOs
            return;
        }
        self.ensure_scratch_buffers(buffer_size);
        let output_buffer = match outputs.get_mut(&PortId::AudioOutput0) {
            Some(buffer) => buffer,
            None => return, // No output connected
        };

        // --- 1) Process Modulation Inputs (gate, freq, gain) ---
        // --- (Identical modulation processing) ---
        self.gate_buffer[..buffer_size].fill(0.0);
        if let Some(gate_sources) = inputs.get(&PortId::CombinedGate) {
            for source in gate_sources {
                Self::apply_add(
                    source.buffer,
                    &mut self.gate_buffer[..buffer_size],
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
                // Reset scratch buffers for accumulation
                self.mod_scratch_add[..buffer_size].fill(default_add);
                self.mod_scratch_mult[..buffer_size].fill(default_mult);
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
        process_mod_input(
            PortId::FrequencyMod,
            &mut self.scratch_freq_add,
            &mut self.scratch_freq_mult,
            0.0,
            1.0,
        );
        process_mod_input(
            PortId::GainMod,
            &mut self.scratch_gain_add,
            &mut self.scratch_gain_mult,
            0.0,
            1.0,
        );
        // --- End Modulation Input Processing ---

        // --- 2) Main Processing Loop (Sample by Sample) ---
        for i in 0..buffer_size {
            // --- Handle Gate Input & Retriggering ---
            let current_gate = self.gate_buffer[i];
            let is_gate_high = current_gate > 0.0;
            let rising_edge = is_gate_high && self.last_gate <= 0.0;

            if rising_edge {
                match self.retrigger_mode {
                    // Modes that reset on rising edge:
                    LfoRetriggerMode::Retrigger
                    | LfoRetriggerMode::OneShot
                    | LfoRetriggerMode::StartOnGate => {
                        // Reset state: phase 0.0, clear loop flag, set running=true
                        self.reset_state_for_mode(true);
                    }
                    LfoRetriggerMode::FreeRunning => {} // No action on gate
                }
            }

            // --- Determine if LFO should run *this sample* ---
            // This now depends more complexly on the mode and potentially the gate.
            let should_advance_phase = match self.retrigger_mode {
                LfoRetriggerMode::FreeRunning => true, // Always advances if active
                LfoRetriggerMode::Retrigger => is_gate_high, // Advances only when gate is high
                LfoRetriggerMode::OneShot => self.is_running, // Advances only if the one-shot cycle hasn't completed
                LfoRetriggerMode::StartOnGate => self.is_running, // Advances if it has been triggered at least once
            };

            // --- Update internal `is_running` state specifically for modes that can stop ---
            // Retrigger stops when gate goes low.
            if self.retrigger_mode == LfoRetriggerMode::Retrigger && !is_gate_high {
                self.is_running = false;
            }
            // OneShot stops itself via internal_advance_phase when phase >= 1.0 (or loop_end?). Let's assume 1.0 for now.
            // StartOnGate never stops itself based on the gate signal after the initial trigger.
            // FreeRunning never stops itself.

            // Store the gate value for the next sample's rising edge detection
            self.last_gate = current_gate;

            // --- Calculate Per-Sample Parameters ---
            let current_freq =
                (self.base_frequency + self.scratch_freq_add[i]) * self.scratch_freq_mult[i];
            let current_gain =
                (self.base_gain + self.scratch_gain_add[i]) * self.scratch_gain_mult[i];
            let phase_increment = (current_freq / self.sample_rate).max(0.0);

            // --- Calculate Output Sample AND Advance Phase ---
            // Get the phase value *before* any potential advancement this sample.
            let current_phase_value = self.phase;
            let output_sample;

            if should_advance_phase {
                // LFO is running/advancing this sample.
                output_sample = self.lookup_sample_at_phase(current_phase_value);

                // Advance the phase state for the *next* sample.
                // This also handles OneShot stopping by setting self.is_running = false internally.
                self.internal_advance_phase(phase_increment);
            } else {
                // LFO is NOT advancing phase this sample.
                if self.retrigger_mode == LfoRetriggerMode::OneShot
                /* && !self.is_running is implied by should_advance_phase being false */
                {
                    // OneShot has finished and is holding its final value.
                    output_sample = self.oneshot_held_value;
                } else if self.retrigger_mode == LfoRetriggerMode::Retrigger
                /* && !is_gate_high is implied */
                {
                    // Retrigger mode is paused (gate is low). Hold the value at the paused phase.
                    output_sample = self.lookup_sample_at_phase(current_phase_value);
                } else {
                    // Default case for not advancing (e.g., StartOnGate before first trigger,
                    // or potentially FreeRunning if somehow self.is_running became false externally?)
                    // Output the value at the current phase (which isn't changing).
                    output_sample = self.lookup_sample_at_phase(current_phase_value);
                    // Could also output 0.0 or lookup_sample_at_phase(0.0) if preferred for pre-trigger state.
                    // Let's stick to current_phase_value for consistency.
                    // output_sample = self.lookup_sample_at_phase(0.0); // Alternative if you want 0 before first trigger
                }
                // Do NOT call internal_advance_phase here.
            }

            // Apply final gain and write to output
            output_buffer[i] = output_sample * current_gain.max(0.0);
        }
    }

    fn reset(&mut self) {
        // Reset state based on mode, assume gate is low initially
        self.reset_state_for_mode(false); // Pass false, requires trigger if not FreeRunning
        self.last_gate = 0.0;
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
        if active == self.active {
            return;
        }
        self.active = active;

        if active {
            // --- LFO is becoming ACTIVE ---
            // Reset state based on mode and *last known* gate state.
            // reset_state_for_mode now correctly sets phase=0.0 and resets the flag.
            self.reset_state_for_mode(self.last_gate > 0.0);
        } else {
            // --- LFO is becoming INACTIVE ---
            // No state change needed other than setting self.active.
            // Phase advancement for FreeRunning is handled in process().
            // Optionally reset state completely when deactivated?
            // self.reset(); // Uncomment to fully reset LFO when deactivated
        }
    }

    fn name(&self) -> &'static str {
        "LFO"
    }

    fn node_type(&self) -> &str {
        "lfo"
    }
} // End impl AudioNode for Lfo
