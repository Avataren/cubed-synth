//! Parameter edits for a running engine: a [`ParamEdit`] (a node, a parameter,
//! a value) applied between blocks without allocating, smoothed so that it does
//! not click.
//!
//! The engine keeps a typed copy (a *shadow*) of the state of every node a patch
//! can edit, built when the patch loads. An edit sets one field of the shadow and
//! gives the whole state to the node's `update_*` method, the same path a patch
//! takes at load. How the change reaches the sound depends on the parameter:
//!
//! - a number whose step is small (at most [`RAMP_LIMIT`] of its range) glides
//!   to the new value over [`RAMP_BLOCKS`] blocks, a step per block;
//! - a bigger step, and any parameter that is not a smooth number (a waveform, an
//!   on/off, a filter type), ducks the output over one block, is applied while
//!   it is silent (a block later), and comes back over the next: about 8 ms in
//!   all;
//! - a pitch offset or a sync switch, which cannot click, is applied at once;
//! - the instrument gain and the macros glide per sample over the block (see
//!   `process_with_frame_instrument` and `fill_macro_values`).
//!
//! An engine with nothing in flight pays one comparison per block.

use rustc_hash::FxHashMap;
use serde_json::Value;

use super::*;
use crate::audio_engine::patch::{ConvolverState, VelocityState};
use crate::catalog::{self, Curve, NodeSpec, ParamKind, ParamSpec, StateShape};
use crate::live::{self, ParamEdit};

/// A number's change over this fraction of its range or less glides.
const RAMP_LIMIT: f32 = 0.3;
/// Blocks a glide takes (about 21 ms at 48 kHz in 128-frame blocks).
const RAMP_BLOCKS: u16 = 8;
/// Blocks a delay time glides over (about 85 ms).
const DELAY_RAMP_BLOCKS: u16 = 32;
/// Edits waiting for the output to duck, and glides in flight, at most.
const MAX_QUEUED: usize = 128;
const MAX_RAMPS: usize = 128;

/// Why an edit was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    /// The engine has no editable node with that id.
    UnknownNode,
    /// The node type has no parameter with that index.
    UnknownParam,
    /// Too many edits are waiting for the output to duck.
    QueueFull,
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EditError::UnknownNode => "no such node",
            EditError::UnknownParam => "no such parameter",
            EditError::QueueFull => "too many edits at once",
        })
    }
}

impl std::error::Error for EditError {}

/// A typed copy of a node's state.
enum Shadow {
    /// Taken out while it is being applied.
    Gone,
    Osc(AnalogOscillatorStateUpdate),
    Wave(WavetableOscillatorStateUpdate),
    Env(EnvelopeConfig),
    Lfo(LfoState),
    Filter(FilterState),
    Sampler(SamplerState),
    Glide(GlideState),
    Noise(NoiseState),
    Velocity(VelocityState),
    Chorus(ChorusState),
    Delay(DelayState),
    Reverb(ReverbState),
    Convolver(ConvolverState),
    Compressor(CompressorState),
    Saturation(SaturationState),
    Bitcrusher(BitcrusherState),
}

impl Shadow {
    fn type_name(&self) -> &'static str {
        match self {
            Shadow::Gone => "",
            Shadow::Osc(_) => "oscillator",
            Shadow::Wave(_) => "wavetable_oscillator",
            Shadow::Env(_) => "envelope",
            Shadow::Lfo(_) => "lfo",
            Shadow::Filter(_) => "filter",
            Shadow::Sampler(_) => "sampler",
            Shadow::Glide(_) => "glide",
            Shadow::Noise(_) => "noise",
            Shadow::Velocity(_) => "global_velocity",
            Shadow::Chorus(_) => "chorus",
            Shadow::Delay(_) => "delay",
            Shadow::Reverb(_) => "freeverb",
            Shadow::Convolver(_) => "convolver",
            Shadow::Compressor(_) => "compressor",
            Shadow::Saturation(_) => "saturation",
            Shadow::Bitcrusher(_) => "bitcrusher",
        }
    }

    fn spec(&self) -> Option<&'static NodeSpec> {
        catalog::node_spec(self.type_name())
    }

    /// A field of the state by its patch key.
    fn field(&mut self, key: &str) -> Option<Field<'_>> {
        match self {
            Shadow::Gone => None,
            Shadow::Osc(s) => osc_field(s, key),
            Shadow::Wave(s) => wave_field(s, key),
            Shadow::Env(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "attack" => Some(Field::F32(&mut s.attack)),
                "decay" => Some(Field::F32(&mut s.decay)),
                "sustain" => Some(Field::F32(&mut s.sustain)),
                "release" => Some(Field::F32(&mut s.release)),
                "attackCurve" => Some(Field::F32(&mut s.attack_curve)),
                "decayCurve" => Some(Field::F32(&mut s.decay_curve)),
                "releaseCurve" => Some(Field::F32(&mut s.release_curve)),
                _ => None,
            },
            Shadow::Lfo(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "waveform" => Some(Field::U8(&mut s.waveform)),
                "frequency" => Some(Field::F32(&mut s.frequency)),
                "gain" => Some(Field::F32(&mut s.gain)),
                "phaseOffset" => Some(Field::F32(&mut s.phase_offset)),
                "useAbsolute" => Some(Field::Bool(&mut s.use_absolute)),
                "useNormalized" => Some(Field::Bool(&mut s.use_normalized)),
                "triggerMode" => Some(Field::U8(&mut s.trigger_mode)),
                "loopMode" => Some(Field::Usize(&mut s.loop_mode)),
                "loopStart" => Some(Field::F32(&mut s.loop_start)),
                "loopEnd" => Some(Field::F32(&mut s.loop_end)),
                _ => None,
            },
            Shadow::Filter(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "filter_type" => Some(Field::I32(&mut s.filter_type)),
                "filter_slope" => Some(Field::Slope(&mut s.filter_slope)),
                "cutoff" => Some(Field::F32(&mut s.cutoff)),
                "resonance" => Some(Field::F32(&mut s.resonance)),
                "keytracking" => Some(Field::F32(&mut s.key_tracking)),
                "gain" => Some(Field::F32(&mut s.gain)),
                "comb_frequency" => Some(Field::F32(&mut s.comb_frequency)),
                "comb_dampening" => Some(Field::F32(&mut s.comb_dampening)),
                _ => None,
            },
            Shadow::Sampler(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "gain" => Some(Field::F32(&mut s.gain)),
                "detune_oct" => Some(Field::F32(&mut s.detune_oct)),
                "detune_semi" => Some(Field::F32(&mut s.detune_semi)),
                "detune_cents" => Some(Field::F32(&mut s.detune_cents)),
                "rootNote" => Some(Field::F32(&mut s.root_note)),
                "loopMode" => Some(Field::U8(&mut s.loop_mode)),
                "loopStart" => Some(Field::F32(&mut s.loop_start)),
                "loopEnd" => Some(Field::F32(&mut s.loop_end)),
                "triggerMode" => Some(Field::U8(&mut s.trigger_mode)),
                _ => None,
            },
            Shadow::Glide(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "time" => Some(Field::F32(&mut s.time)),
                _ => None,
            },
            Shadow::Noise(s) => match key {
                "is_enabled" => Some(Field::Bool(&mut s.is_enabled)),
                "noiseType" => Some(Field::U8(&mut s.noise_type)),
                "cutoff" => Some(Field::F32(&mut s.cutoff)),
                "gain" => Some(Field::F32(&mut s.gain)),
                _ => None,
            },
            Shadow::Velocity(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "sensitivity" => Some(Field::F32(&mut s.sensitivity)),
                "randomize" => Some(Field::F32(&mut s.randomize)),
                _ => None,
            },
            Shadow::Chorus(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "baseDelayMs" => Some(Field::F32(&mut s.base_delay_ms)),
                "depthMs" => Some(Field::F32(&mut s.depth_ms)),
                "lfoRateHz" => Some(Field::F32(&mut s.lfo_rate_hz)),
                "feedback" => Some(Field::F32(&mut s.feedback)),
                "feedback_filter" => Some(Field::F32(&mut s.feedback_filter)),
                "mix" => Some(Field::F32(&mut s.mix)),
                "stereoPhaseOffsetDeg" => Some(Field::F32(&mut s.stereo_phase_offset_deg)),
                _ => None,
            },
            Shadow::Delay(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "delayMs" => Some(Field::F32(&mut s.delay_ms)),
                "feedback" => Some(Field::F32(&mut s.feedback)),
                "wetMix" => Some(Field::F32(&mut s.wet_mix)),
                _ => None,
            },
            Shadow::Reverb(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "room_size" => Some(Field::F32(&mut s.room_size)),
                "damp" => Some(Field::F32(&mut s.damp)),
                "wet" => Some(Field::F32(&mut s.wet)),
                "dry" => Some(Field::F32(&mut s.dry)),
                "width" => Some(Field::F32(&mut s.width)),
                _ => None,
            },
            Shadow::Convolver(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "wetMix" => Some(Field::F32(&mut s.wet_mix)),
                _ => None,
            },
            Shadow::Compressor(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "thresholdDb" => Some(Field::F32(&mut s.threshold_db)),
                "ratio" => Some(Field::F32(&mut s.ratio)),
                "attackMs" => Some(Field::F32(&mut s.attack_ms)),
                "releaseMs" => Some(Field::F32(&mut s.release_ms)),
                "makeupGainDb" => Some(Field::F32(&mut s.makeup_gain_db)),
                "mix" => Some(Field::F32(&mut s.mix)),
                _ => None,
            },
            Shadow::Saturation(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "drive" => Some(Field::F32(&mut s.drive)),
                "mix" => Some(Field::F32(&mut s.mix)),
                _ => None,
            },
            Shadow::Bitcrusher(s) => match key {
                "active" => Some(Field::Bool(&mut s.active)),
                "bits" => Some(Field::U8(&mut s.bits)),
                "downsampleFactor" => Some(Field::Usize(&mut s.downsample_factor)),
                "mix" => Some(Field::F32(&mut s.mix)),
                _ => None,
            },
        }
    }

    /// Keeps the fields derived from others in step: the oscillators' and the
    /// sampler's detune in cents, the sampler's frequency.
    fn derive(&mut self) {
        let total = |oct: f32, semi: f32, cents: f32| oct * 1200.0 + semi * 100.0 + cents;
        match self {
            Shadow::Osc(s) => s.detune = total(s.detune_oct, s.detune_semi, s.detune_cents),
            Shadow::Wave(s) => s.detune = total(s.detune_oct, s.detune_semi, s.detune_cents),
            Shadow::Sampler(s) => {
                s.detune = total(s.detune_oct, s.detune_semi, s.detune_cents);
                s.frequency = 440.0 * 2f32.powf(s.detune / 1200.0);
            }
            _ => {}
        }
    }

    /// Whether the node makes sound (a node that is off can be edited at once).
    fn is_active(&mut self) -> bool {
        if let Some(field) = self.field("active") {
            return field.get() != 0.0;
        }
        if let Some(field) = self.field("is_enabled") {
            return field.get() != 0.0;
        }
        true
    }
}

fn osc_field<'a>(s: &'a mut AnalogOscillatorStateUpdate, key: &str) -> Option<Field<'a>> {
    match key {
        "active" => Some(Field::Bool(&mut s.active)),
        "waveform" => Some(Field::Waveform(&mut s.waveform)),
        "gain" => Some(Field::F32(&mut s.gain)),
        "phase_mod_amount" => Some(Field::F32(&mut s.phase_mod_amount)),
        "feedback_amount" => Some(Field::F32(&mut s.feedback_amount)),
        "detune_oct" => Some(Field::F32(&mut s.detune_oct)),
        "detune_semi" => Some(Field::F32(&mut s.detune_semi)),
        "detune_cents" => Some(Field::F32(&mut s.detune_cents)),
        "unison_voices" => Some(Field::U32(&mut s.unison_voices)),
        "spread" => Some(Field::F32(&mut s.spread)),
        "hard_sync" => Some(Field::Bool(&mut s.hard_sync)),
        _ => None,
    }
}

fn wave_field<'a>(s: &'a mut WavetableOscillatorStateUpdate, key: &str) -> Option<Field<'a>> {
    match key {
        "active" => Some(Field::Bool(&mut s.active)),
        "gain" => Some(Field::F32(&mut s.gain)),
        "wave_index" => Some(Field::F32(&mut s.wavetable_index)),
        "phase_mod_amount" => Some(Field::F32(&mut s.phase_mod_amount)),
        "feedback_amount" => Some(Field::F32(&mut s.feedback_amount)),
        "detune_oct" => Some(Field::F32(&mut s.detune_oct)),
        "detune_semi" => Some(Field::F32(&mut s.detune_semi)),
        "detune_cents" => Some(Field::F32(&mut s.detune_cents)),
        "unison_voices" => Some(Field::U32(&mut s.unison_voices)),
        "spread" => Some(Field::F32(&mut s.spread)),
        "hard_sync" => Some(Field::Bool(&mut s.hard_sync)),
        _ => None,
    }
}

/// A field of a node's state, read and written as an `f32` (a bool is 0 or 1, an
/// enum or an int its number).
enum Field<'a> {
    F32(&'a mut f32),
    U32(&'a mut u32),
    U8(&'a mut u8),
    Usize(&'a mut usize),
    I32(&'a mut i32),
    Bool(&'a mut bool),
    Waveform(&'a mut Waveform),
    Slope(&'a mut FilterSlope),
}

impl Field<'_> {
    fn get(&self) -> f32 {
        match self {
            Field::F32(v) => **v,
            Field::U32(v) => **v as f32,
            Field::U8(v) => **v as f32,
            Field::Usize(v) => **v as f32,
            Field::I32(v) => **v as f32,
            Field::Bool(v) => **v as u8 as f32,
            Field::Waveform(v) => **v as u32 as f32,
            Field::Slope(v) => **v as u8 as f32,
        }
    }

    fn set(&mut self, value: f32) {
        match self {
            Field::F32(v) => **v = value,
            Field::U32(v) => **v = value.round().max(0.0) as u32,
            Field::U8(v) => **v = value.round().clamp(0.0, 255.0) as u8,
            Field::Usize(v) => **v = value.round().max(0.0) as usize,
            Field::I32(v) => **v = value.round() as i32,
            Field::Bool(v) => **v = value != 0.0,
            Field::Waveform(v) => {
                **v = match value.round() as i32 {
                    1 => Waveform::Triangle,
                    2 => Waveform::Saw,
                    3 => Waveform::Square,
                    4 => Waveform::Custom,
                    _ => Waveform::Sine,
                }
            }
            Field::Slope(v) => {
                **v = if value >= 0.5 {
                    FilterSlope::Db24
                } else {
                    FilterSlope::Db12
                }
            }
        }
    }
}

/// A number gliding to its target, a step per block.
#[derive(Clone, Copy)]
struct Ramp {
    node: u128,
    param: u16,
    from: f32,
    to: f32,
    step: u16,
    steps: u16,
}

/// An edit waiting for the output to duck.
#[derive(Clone, Copy)]
struct Queued {
    node: u128,
    param: u16,
    value: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dip {
    /// Nothing is queued.
    Idle,
    /// The output went down over the last block: apply what is queued, and keep
    /// it silent for this block. (The limiter delays the sound by its lookahead,
    /// so the old sound is still leaving it for a while after the change.)
    Applying,
    /// Come back over this block.
    Release,
}

/// The edits in flight, and the typed state they act on.
pub(super) struct Live {
    shadows: FxHashMap<u128, Shadow>,
    ramps: Vec<Ramp>,
    queued: Vec<Queued>,
    dip: Dip,
    /// The instrument gain at the end of the last block, and the macros' values.
    gain_applied: f32,
    macros_applied: [f32; MACRO_COUNT],
}

impl Live {
    pub(super) fn new() -> Live {
        Live {
            shadows: FxHashMap::default(),
            ramps: Vec::with_capacity(MAX_RAMPS),
            queued: Vec::with_capacity(MAX_QUEUED),
            dip: Dip::Idle,
            gain_applied: 1.0,
            macros_applied: [0.0; MACRO_COUNT],
        }
    }

    /// Nothing is gliding or waiting.
    pub(super) fn is_idle(&self) -> bool {
        self.ramps.is_empty() && self.queued.is_empty() && self.dip == Dip::Idle
    }
}

/// How far a number moves, as a fraction of its range (in log steps for a log
/// curve).
fn relative_change(param: &ParamSpec, from: f32, to: f32) -> f32 {
    let (min, max) = (param.min as f32, param.max as f32);
    if param.curve == Curve::Log && min > 0.0 && from > 0.0 && to > 0.0 {
        ((to / from).ln() / (max / min).ln()).abs()
    } else {
        ((to - from) / (max - min).max(f32::EPSILON)).abs()
    }
}

/// A delay line's time: a step in it jumps the read position, so it glides
/// slowly however far it goes.
fn is_delay_time(key: &str) -> bool {
    matches!(key, "baseDelayMs" | "delayMs" | "depthMs")
}

/// Parameters that cannot click, applied at once: the pitch offsets (the phase
/// goes on) and sync.
fn is_direct(key: &str) -> bool {
    matches!(key, "detune_oct" | "detune_semi" | "hard_sync")
}

impl AudioEngine {
    /// Builds the shadows from the patch the engine was just built from.
    pub(super) fn build_live(&mut self, patch: PatchFile) {
        self.live = Live::new();
        self.live.gain_applied = self.instrument_gain;
        self.live.macros_applied = self.macro_values;
        let synth = patch.synth_state;
        let layout_nodes: Vec<(String, String)> = synth
            .layout
            .canonical_voice()
            .map(|voice| {
                voice
                    .nodes
                    .iter()
                    .flat_map(|(type_name, list)| {
                        list.iter().map(move |n| (type_name.clone(), n.id.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let add = |this: &mut AudioEngine, id: &str, shadow: Shadow| {
            if let Some(key) = live::node_key(id) {
                this.live.shadows.insert(key, shadow);
            }
        };
        for (id, state) in synth.oscillators {
            add(self, &id, Shadow::Osc(state));
        }
        for (id, state) in synth.wavetable_oscillators {
            add(self, &id, Shadow::Wave(state));
        }
        for (id, state) in synth.envelopes {
            add(self, &id, Shadow::Env(state));
        }
        for (id, state) in synth.lfos {
            add(self, &id, Shadow::Lfo(state));
        }
        for (id, state) in synth.filters {
            add(self, &id, Shadow::Filter(state));
        }
        for (id, state) in synth.samplers {
            add(self, &id, Shadow::Sampler(state));
        }
        for (id, state) in synth.glides {
            add(self, &id, Shadow::Glide(state));
        }
        for (id, state) in synth.choruses {
            add(self, &id, Shadow::Chorus(state));
        }
        for (id, state) in synth.delays {
            add(self, &id, Shadow::Delay(state));
        }
        for (id, state) in synth.reverbs {
            add(self, &id, Shadow::Reverb(state));
        }
        for (id, state) in synth.convolvers {
            add(self, &id, Shadow::Convolver(state));
        }
        for (id, state) in synth.compressors {
            add(self, &id, Shadow::Compressor(state));
        }
        for (id, state) in synth.saturations {
            add(self, &id, Shadow::Saturation(state));
        }
        for (id, state) in synth.bitcrushers {
            add(self, &id, Shadow::Bitcrusher(state));
        }
        // The two nodes with one state each.
        let mut noise = synth.noise;
        let mut velocity = synth.velocity;
        for (type_name, id) in &layout_nodes {
            match type_name.as_str() {
                "noise" => {
                    if let Some(state) = noise.take() {
                        add(self, id, Shadow::Noise(state));
                    }
                }
                "global_velocity" => {
                    if let Some(state) = velocity.take() {
                        add(self, id, Shadow::Velocity(state));
                    }
                }
                _ => {}
            }
        }
        // A node the patch has no state for gets the catalogue's defaults, so it
        // can be edited too.
        for (type_name, id) in &layout_nodes {
            let Some(key) = live::node_key(id) else {
                continue;
            };
            if self.live.shadows.contains_key(&key) {
                continue;
            }
            if let Some(shadow) = default_shadow(type_name, id) {
                self.live.shadows.insert(key, shadow);
            }
        }
        // ... and an effect of the chain.
        for index in 0..self.effect_ids.len() {
            let id = self.effect_ids[index];
            if self.live.shadows.contains_key(&(id as u128)) {
                continue;
            }
            let type_name = self.effect_stack.effects[index]
                .node
                .node_type()
                .to_string();
            if let Some(shadow) = default_shadow(&type_name, &id.to_string()) {
                self.live.shadows.insert(id as u128, shadow);
            }
        }
    }

    /// Takes a parameter edit. Call it between blocks, on the thread that renders
    /// them. It never allocates.
    pub fn edit_param(&mut self, edit: ParamEdit) -> Result<(), EditError> {
        if edit.node == live::PATCH {
            return self.edit_patch_param(edit.param, edit.value);
        }
        let key = edit.node;
        let (param, current, active) = {
            let shadow = self
                .live
                .shadows
                .get_mut(&key)
                .ok_or(EditError::UnknownNode)?;
            let spec = shadow.spec().ok_or(EditError::UnknownNode)?;
            let param = spec
                .params
                .get(edit.param as usize)
                .ok_or(EditError::UnknownParam)?;
            let active = shadow.is_active();
            let current = shadow
                .field(param.key)
                .ok_or(EditError::UnknownParam)?
                .get();
            (param, current, active)
        };
        let value = sanitize(param, edit.value);

        // A glide in flight for this parameter is replaced.
        let in_flight = self
            .live
            .ramps
            .iter()
            .position(|r| r.node == key && r.param == edit.param);
        if let Some(i) = in_flight {
            self.live.ramps.swap_remove(i);
        }
        // So is an edit waiting for the duck.
        if let Some(i) = self
            .live
            .queued
            .iter()
            .position(|q| q.node == key && q.param == edit.param)
        {
            self.live.queued.swap_remove(i);
        }

        if value == current {
            return Ok(());
        }
        let smooth = param.kind == ParamKind::Float;
        // A node that is off is edited at once, unless the edit turns it on.
        let silent = !active && !matches!(param.key, "active" | "is_enabled");
        if is_direct(param.key) || silent {
            self.set_and_apply(key, edit.param, value);
        } else if smooth
            && (is_delay_time(param.key) || relative_change(param, current, value) <= RAMP_LIMIT)
        {
            if self.live.ramps.len() < MAX_RAMPS {
                self.live.ramps.push(Ramp {
                    node: key,
                    param: edit.param,
                    from: current,
                    to: value,
                    step: 0,
                    steps: if is_delay_time(param.key) {
                        DELAY_RAMP_BLOCKS
                    } else {
                        RAMP_BLOCKS
                    },
                });
            } else {
                self.set_and_apply(key, edit.param, value);
            }
        } else if self.live.queued.len() < MAX_QUEUED {
            self.live.queued.push(Queued {
                node: key,
                param: edit.param,
                value,
            });
        } else {
            return Err(EditError::QueueFull);
        }
        Ok(())
    }

    /// The patch's own parameters: the output gain and the macros.
    fn edit_patch_param(&mut self, param: u16, value: f32) -> Result<(), EditError> {
        if !value.is_finite() {
            return Ok(());
        }
        match param {
            live::PATCH_GAIN => self.instrument_gain = value.clamp(0.0, 2.0),
            p if (live::PATCH_MACRO0..live::PATCH_MACRO0 + MACRO_COUNT as u16).contains(&p) => {
                self.macro_values[(p - live::PATCH_MACRO0) as usize] = value.clamp(0.0, 1.0);
            }
            _ => return Err(EditError::UnknownParam),
        }
        Ok(())
    }

    /// The value of a node's parameter as the engine holds it now (a glide in
    /// flight has not arrived), if the engine has the node.
    pub fn param_value(&mut self, node: u128, param: u16) -> Option<f32> {
        let shadow = self.live.shadows.get_mut(&node)?;
        let key = shadow.spec()?.params.get(param as usize)?.key;
        Some(shadow.field(key)?.get())
    }

    /// The nodes the engine can edit, by id.
    pub fn editable_nodes(&self) -> impl Iterator<Item = u128> + '_ {
        self.live.shadows.keys().copied()
    }

    /// Whether any edit is still gliding or waiting for the output to duck.
    pub fn edits_in_flight(&self) -> bool {
        !self.live.is_idle()
    }

    /// Sets one field of a node's state and gives the node the new state.
    fn set_and_apply(&mut self, node: u128, param: u16, value: f32) {
        let Some(slot) = self.live.shadows.get_mut(&node) else {
            return;
        };
        let mut shadow = std::mem::replace(slot, Shadow::Gone);
        if let Some(spec) = shadow.spec() {
            if let Some(param) = spec.params.get(param as usize) {
                if let Some(mut field) = shadow.field(param.key) {
                    field.set(value);
                }
                shadow.derive();
                self.apply_shadow(node, &shadow);
            }
        }
        if let Some(slot) = self.live.shadows.get_mut(&node) {
            *slot = shadow;
        }
    }

    /// Gives a node its whole state, as loading a patch does.
    fn apply_shadow(&mut self, node: u128, shadow: &Shadow) {
        let id = NodeId(Uuid::from_u128(node));
        // The nodes report a problem as a message; there is nothing to do with it
        // here but not to allocate it.
        match shadow {
            Shadow::Gone => {}
            Shadow::Osc(s) => {
                let _ = self.update_oscillator(id, s);
            }
            Shadow::Wave(s) => {
                let _ = self.update_wavetable_oscillator(id, s);
            }
            Shadow::Env(s) => {
                let _ = self.apply_envelope_state(id, s);
            }
            Shadow::Lfo(s) => self.update_lfo(id, s),
            Shadow::Filter(s) => {
                let _ = self.apply_filter_state(s);
            }
            Shadow::Sampler(s) => {
                let _ = self.apply_sampler_state(s);
            }
            Shadow::Glide(s) => {
                let _ = self.apply_glide_state(s);
            }
            Shadow::Noise(s) => {
                let _ = self.apply_noise_state(id, s);
            }
            Shadow::Velocity(s) => {
                let _ = self.update_velocity(id, s.sensitivity, s.randomize);
            }
            Shadow::Chorus(s) => self.apply_chorus_state(s),
            Shadow::Delay(s) => self.apply_delay_state(s),
            Shadow::Reverb(s) => self.apply_reverb_state(s),
            Shadow::Convolver(s) => self.update_convolver(node as usize, s.wet_mix, s.active),
            Shadow::Compressor(s) => {
                let _ = self.apply_compressor_state(s);
            }
            Shadow::Saturation(s) => {
                let _ = self.apply_saturation_state(s);
            }
            Shadow::Bitcrusher(s) => {
                let _ = self.apply_bitcrusher_state(s);
            }
        }
    }

    /// Steps the glides, and applies what waited for the output to duck. Returns
    /// the output's gain at the start and end of this block (1 and 1 unless an
    /// edit is ducking it). Called at the start of a block that has edits in
    /// flight.
    pub(super) fn begin_live_block(&mut self) -> (f32, f32) {
        // The glides: a step each. (Taken out so `set_and_apply` may borrow the
        // engine; `take` leaves an empty vector, which costs nothing.)
        let mut ramps = std::mem::take(&mut self.live.ramps);
        let mut i = 0;
        while i < ramps.len() {
            let ramp = &mut ramps[i];
            ramp.step += 1;
            let t = ramp.step as f32 / ramp.steps as f32;
            let value = if ramp.step >= ramp.steps {
                ramp.to
            } else {
                ramp.from + (ramp.to - ramp.from) * t
            };
            let (node, param, done) = (ramp.node, ramp.param, ramp.step >= ramp.steps);
            self.set_and_apply(node, param, value);
            if done {
                ramps.swap_remove(i);
            } else {
                i += 1;
            }
        }
        self.live.ramps = ramps;

        match self.live.dip {
            Dip::Idle if !self.live.queued.is_empty() => {
                self.live.dip = Dip::Applying;
                (1.0, 0.0)
            }
            Dip::Applying => {
                let mut queued = std::mem::take(&mut self.live.queued);
                for edit in queued.drain(..) {
                    self.set_and_apply(edit.node, edit.param, edit.value);
                }
                self.live.queued = queued;
                self.live.dip = Dip::Release;
                (0.0, 0.0)
            }
            Dip::Release => {
                self.live.dip = Dip::Idle;
                (0.0, 1.0)
            }
            Dip::Idle => (1.0, 1.0),
        }
    }

    /// The instrument gain's glide over this block, as (start, end).
    pub(super) fn instrument_gain_ramp(&mut self) -> (f32, f32) {
        let ramp = (self.live.gain_applied, self.instrument_gain);
        self.live.gain_applied = self.instrument_gain;
        ramp
    }

    /// Writes the macros' values into every voice's lanes of `frame`, which is
    /// how the engine hears them; one that moved since the last block glides
    /// there over the block.
    pub fn fill_macro_values(&mut self, frame: &mut AutomationFrame) {
        for index in 0..MACRO_COUNT {
            let (from, to) = (self.live.macros_applied[index], self.macro_values[index]);
            for voice in 0..frame.num_voices() {
                if from == to {
                    frame.set_macro_value(voice, index, to);
                } else {
                    frame.set_macro_ramp(voice, index, from, to);
                }
            }
            self.live.macros_applied[index] = to;
        }
    }
}

/// The value as the patch would keep it: a number clamped to its range, an int
/// rounded, a bool 0 or 1, an enum within its options.
fn sanitize(param: &ParamSpec, value: f32) -> f32 {
    if !value.is_finite() {
        return param.default as f32;
    }
    match param.kind {
        ParamKind::Float => value.clamp(param.min as f32, param.max as f32),
        ParamKind::Int => value.round().clamp(param.min as f32, param.max as f32),
        ParamKind::Bool => (value != 0.0) as u8 as f32,
        ParamKind::Enum => value
            .round()
            .clamp(0.0, param.options.len().saturating_sub(1) as f32),
    }
}

/// A node's state from the catalogue's defaults.
fn default_shadow(type_name: &str, id: &str) -> Option<Shadow> {
    let spec = catalog::node_spec(type_name)?;
    if spec.state_shape == StateShape::None {
        return None;
    }
    let value = Value::Object(spec.default_state(id));
    fn read<T: serde::de::DeserializeOwned>(value: Value) -> Option<T> {
        serde_json::from_value(value).ok()
    }
    Some(match type_name {
        "oscillator" => Shadow::Osc(read(value)?),
        "wavetable_oscillator" => Shadow::Wave(read(value)?),
        "envelope" => Shadow::Env(read(value)?),
        "lfo" => Shadow::Lfo(read(value)?),
        "filter" => Shadow::Filter(read(value)?),
        "sampler" => Shadow::Sampler(read(value)?),
        "glide" => Shadow::Glide(read(value)?),
        "noise" => Shadow::Noise(read(value)?),
        "global_velocity" => Shadow::Velocity(read(value)?),
        "chorus" => Shadow::Chorus(read(value)?),
        "delay" => Shadow::Delay(read(value)?),
        "freeverb" => Shadow::Reverb(read(value)?),
        "convolver" => Shadow::Convolver(read(value)?),
        "compressor" => Shadow::Compressor(read(value)?),
        "saturation" => Shadow::Saturation(read(value)?),
        "bitcrusher" => Shadow::Bitcrusher(read(value)?),
        _ => return None,
    })
}
