//! A catalogue of the synth's node types: what a patch editor needs to know to
//! offer them (ports with their numbers and kinds, parameters with their
//! ranges, defaults and units, the eight effects, the modulation types).
//!
//! The data is Rust: port numbers come from [`PortId`], parameter keys are the
//! serde field names of the patch's state structs, and tests check both against
//! the code that really builds and loads nodes (every type the loader can build
//! is listed, every parameter key is in its state's serialized form, every
//! declared port is one the built node has). [`catalog_json`] is the shape an
//! editor reads; `tests/fixtures/catalog.json` is a copy of it that a test keeps
//! equal.
//!
//! Parameter keys are the keys of the node's state in a patch's `synthState`
//! (`oscillators`, `filters`, ...), so an editor reads and writes the patch
//! document directly. [`patch_edit`](crate::patch_edit) does that with the
//! catalogue's checks.

use serde_json::{json, Map, Value};

use crate::traits::PortId;

/// Bumped when the shape of [`catalog_json`] changes.
pub const VERSION: u32 = 1;
/// How many macros a patch has.
pub const MACRO_COUNT: usize = 4;
/// Effect node ids start here: `10000 + index` in the default chain.
pub const EFFECT_ID_BASE: usize = 10_000;

/// What a node does, for grouping in an editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Generator,
    Modulator,
    Filter,
    Mixer,
    System,
    Effect,
}

impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Category::Generator => "generator",
            Category::Modulator => "modulator",
            Category::Filter => "filter",
            Category::Mixer => "mixer",
            Category::System => "system",
            Category::Effect => "effect",
        }
    }
}

/// How a node's state is stored in `synthState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateShape {
    /// A map from node id to the node's state (`oscillators`, `filters`, ...).
    Map,
    /// One object for the node (`noise`, `velocity`).
    Single,
    /// The node has no state in a patch.
    None,
}

impl StateShape {
    pub fn name(self) -> &'static str {
        match self {
            StateShape::Map => "map",
            StateShape::Single => "single",
            StateShape::None => "none",
        }
    }
}

/// What travels through a port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortKind {
    /// A sound.
    Audio,
    /// A control signal (an envelope or LFO, a gate, a pitch).
    Control,
}

impl PortKind {
    pub fn name(self) -> &'static str {
        match self {
            PortKind::Audio => "audio",
            PortKind::Control => "control",
        }
    }
}

/// What a connection into a port does by default (the DAW's choice).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModulationType {
    /// `0`: the port's value is multiplied by the source times the amount.
    Vca,
    /// `1`: the value is scaled by `1 + source * amount`.
    Bipolar,
    /// `2`: the source times the amount is added to the value.
    Additive,
}

impl ModulationType {
    pub const ALL: [ModulationType; 3] = [
        ModulationType::Vca,
        ModulationType::Bipolar,
        ModulationType::Additive,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ModulationType::Vca => "vca",
            ModulationType::Bipolar => "bipolar",
            ModulationType::Additive => "additive",
        }
    }

    /// The number a patch stores.
    pub fn value(self) -> i32 {
        self as i32
    }
}

/// The transformations a connection can apply to its source.
pub const TRANSFORMS: [&str; 4] = ["none", "invert", "square", "cube"];

/// One port of a node.
#[derive(Clone, Copy, Debug)]
pub struct PortSpec {
    pub port: PortId,
    pub label: &'static str,
    pub kind: PortKind,
    /// An input that a modulation source (an LFO, an envelope) is meant to drive.
    pub modulatable: bool,
    /// The engine wires it itself (gate, global pitch): an editor does not offer it.
    pub system: bool,
}

impl PortSpec {
    /// How a connection into this port is made unless the user chooses.
    pub fn default_modulation(&self) -> ModulationType {
        match self.port {
            PortId::FrequencyMod => ModulationType::Bipolar,
            PortId::GainMod | PortId::FeedbackMod => ModulationType::Vca,
            _ => ModulationType::Additive,
        }
    }
}

const fn audio_in(port: PortId, label: &'static str) -> PortSpec {
    PortSpec {
        port,
        label,
        kind: PortKind::Audio,
        modulatable: false,
        system: false,
    }
}

const fn audio_out(port: PortId, label: &'static str) -> PortSpec {
    audio_in(port, label)
}

const fn control_out(port: PortId, label: &'static str) -> PortSpec {
    PortSpec {
        port,
        label,
        kind: PortKind::Control,
        modulatable: false,
        system: false,
    }
}

/// A control input a source can drive.
const fn modulated(port: PortId, label: &'static str) -> PortSpec {
    PortSpec {
        port,
        label,
        kind: PortKind::Control,
        modulatable: true,
        system: false,
    }
}

/// A control input the engine wires itself.
const fn system(port: PortId, label: &'static str) -> PortSpec {
    PortSpec {
        port,
        label,
        kind: PortKind::Control,
        modulatable: false,
        system: true,
    }
}

/// The type of a parameter's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    Float,
    Int,
    Bool,
    /// An integer that indexes `options`.
    Enum,
}

impl ParamKind {
    pub fn name(self) -> &'static str {
        match self {
            ParamKind::Float => "float",
            ParamKind::Int => "int",
            ParamKind::Bool => "bool",
            ParamKind::Enum => "enum",
        }
    }
}

/// How a knob should map its travel to the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    Linear,
    /// Logarithmic: equal travel is an equal ratio (frequencies, times).
    Log,
}

impl Curve {
    pub fn name(self) -> &'static str {
        match self {
            Curve::Linear => "linear",
            Curve::Log => "log",
        }
    }
}

/// One parameter of a node: a key of its state in the patch.
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    /// The serde key of the state field.
    pub key: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    /// Float and int ranges; unused for bool and enum.
    pub min: f64,
    pub max: f64,
    /// A bool is 0 or 1; an enum is an index into `options`.
    pub default: f64,
    pub step: f64,
    pub unit: &'static str,
    pub curve: Curve,
    /// The input port that modulates this parameter.
    pub modulated_by: Option<PortId>,
    pub options: &'static [&'static str],
}

impl ParamSpec {
    const fn base(key: &'static str, label: &'static str, kind: ParamKind) -> ParamSpec {
        ParamSpec {
            key,
            label,
            kind,
            min: 0.0,
            max: 1.0,
            default: 0.0,
            step: 0.0,
            unit: "",
            curve: Curve::Linear,
            modulated_by: None,
            options: &[],
        }
    }

    const fn float(
        key: &'static str,
        label: &'static str,
        min: f64,
        max: f64,
        default: f64,
    ) -> ParamSpec {
        let mut p = ParamSpec::base(key, label, ParamKind::Float);
        p.min = min;
        p.max = max;
        p.default = default;
        p
    }

    const fn int(
        key: &'static str,
        label: &'static str,
        min: f64,
        max: f64,
        default: f64,
    ) -> ParamSpec {
        let mut p = ParamSpec::base(key, label, ParamKind::Int);
        p.min = min;
        p.max = max;
        p.default = default;
        p.step = 1.0;
        p
    }

    const fn boolean(key: &'static str, label: &'static str, default: bool) -> ParamSpec {
        let mut p = ParamSpec::base(key, label, ParamKind::Bool);
        p.default = if default { 1.0 } else { 0.0 };
        p
    }

    const fn choice(
        key: &'static str,
        label: &'static str,
        options: &'static [&'static str],
        default: usize,
    ) -> ParamSpec {
        let mut p = ParamSpec::base(key, label, ParamKind::Enum);
        p.options = options;
        p.default = default as f64;
        p.max = (options.len() - 1) as f64;
        p
    }

    const fn unit(mut self, unit: &'static str) -> ParamSpec {
        self.unit = unit;
        self
    }

    const fn log(mut self) -> ParamSpec {
        self.curve = Curve::Log;
        self
    }

    const fn step(mut self, step: f64) -> ParamSpec {
        self.step = step;
        self
    }

    const fn modulated_by(mut self, port: PortId) -> ParamSpec {
        self.modulated_by = Some(port);
        self
    }

    /// Whether a JSON value is acceptable for this parameter, and the value as
    /// the patch stores it (clamped to the range; an int rounded; a bool as a
    /// bool; an enum as an index).
    pub fn check(&self, value: &Value) -> Result<Value, String> {
        let bad = |what: &str| format!("parameter '{}' takes {what}, got {value}", self.key);
        match self.kind {
            ParamKind::Bool => match value {
                Value::Bool(b) => Ok(Value::Bool(*b)),
                Value::Number(n) if n.as_f64() == Some(0.0) => Ok(Value::Bool(false)),
                Value::Number(n) if n.as_f64() == Some(1.0) => Ok(Value::Bool(true)),
                _ => Err(bad("true or false")),
            },
            ParamKind::Enum => {
                let n = value.as_f64().ok_or_else(|| bad("an option index"))?;
                if n.fract() != 0.0 || n < 0.0 || n as usize >= self.options.len() {
                    return Err(format!(
                        "parameter '{}' takes an option index 0..{}, got {value}",
                        self.key,
                        self.options.len() - 1
                    ));
                }
                Ok(json!(n as i64))
            }
            ParamKind::Int => {
                let n = value.as_f64().ok_or_else(|| bad("a number"))?;
                if !n.is_finite() {
                    return Err(bad("a finite number"));
                }
                Ok(json!(n.round().clamp(self.min, self.max) as i64))
            }
            ParamKind::Float => {
                let n = value.as_f64().ok_or_else(|| bad("a number"))?;
                if !n.is_finite() {
                    return Err(bad("a finite number"));
                }
                Ok(number(n.clamp(self.min, self.max)))
            }
        }
    }

    /// The default as the patch stores it.
    pub fn default_value(&self) -> Value {
        match self.kind {
            ParamKind::Bool => Value::Bool(self.default != 0.0),
            ParamKind::Int | ParamKind::Enum => json!(self.default as i64),
            ParamKind::Float => number(self.default),
        }
    }
}

/// A float as a JSON number, written the shortest way that reads back as the
/// same `f32` (the engine holds `f32`, a patch from the DAW holds JS numbers).
pub fn number(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        return json!(x as i64);
    }
    let narrowed = x as f32;
    if narrowed as f64 == x {
        // Exactly an f32 (a default like 0.5): keep it short.
        return json!(x);
    }
    let text = format!("{narrowed}");
    json!(text.parse::<f64>().unwrap_or(x))
}

/// One node type.
#[derive(Clone, Copy, Debug)]
pub struct NodeSpec {
    /// The key under `layout.canonicalVoice.nodes` and the type name in a connection.
    pub type_name: &'static str,
    pub title: &'static str,
    pub category: Category,
    /// The key of the node's state in `synthState`.
    pub state_map: Option<&'static str>,
    pub state_shape: StateShape,
    /// A patch holds at most one.
    pub singleton: bool,
    /// An editor may delete it (the voice's wiring nodes and its mixer stay).
    pub deletable: bool,
    pub inputs: &'static [PortSpec],
    pub outputs: &'static [PortSpec],
    pub params: &'static [ParamSpec],
    /// Effects only: the slot in the default chain (its id is `10000 + slot`).
    pub slot: Option<usize>,
    /// State fields that are not parameters (derived from them, or unused by
    /// the engine) with the values a new node's state starts with.
    pub extra_state: &'static [(&'static str, Extra)],
}

/// A value in [`NodeSpec::extra_state`].
#[derive(Clone, Copy, Debug)]
pub enum Extra {
    Num(f64),
    Bool(bool),
}

impl NodeSpec {
    pub fn param(&self, key: &str) -> Option<&'static ParamSpec> {
        self.params.iter().find(|p| p.key == key)
    }

    pub fn param_index(&self, key: &str) -> Option<usize> {
        self.params.iter().position(|p| p.key == key)
    }

    pub fn input(&self, port: PortId) -> Option<&'static PortSpec> {
        self.inputs.iter().find(|p| p.port == port)
    }

    pub fn output(&self, port: PortId) -> Option<&'static PortSpec> {
        self.outputs.iter().find(|p| p.port == port)
    }

    pub fn is_effect(&self) -> bool {
        self.category == Category::Effect
    }

    /// The node's state when it is created: every parameter at its default (the
    /// `id` is the caller's).
    pub fn default_state(&self, id: &str) -> Map<String, Value> {
        let mut state = Map::new();
        if self.state_shape == StateShape::Map {
            state.insert("id".into(), json!(id));
        }
        for param in self.params {
            state.insert(param.key.into(), param.default_value());
        }
        for (key, value) in self.extra_state {
            let value = match value {
                Extra::Num(x) => number(*x),
                Extra::Bool(b) => json!(b),
            };
            state.insert((*key).into(), value);
        }
        state
    }
}

// ---------------------------------------------------------------------------
// Option lists

const WAVEFORMS: &[&str] = &["Sine", "Triangle", "Saw", "Square"];
const LFO_WAVEFORMS: &[&str] = &["Sine", "Triangle", "Square", "Saw", "InverseSaw"];
const LFO_TRIGGERS: &[&str] = &["FreeRunning", "StartOnGate", "Retrigger", "OneShot"];
const LOOP_MODES: &[&str] = &["Off", "Loop", "PingPong"];
const SAMPLER_TRIGGERS: &[&str] = &["FreeRunning", "Gate", "OneShot"];
const FILTER_TYPES: &[&str] = &[
    "LowPass",
    "LowShelf",
    "Peaking",
    "HighShelf",
    "Notch",
    "HighPass",
    "Ladder",
    "Comb",
    "BandPass",
];
const FILTER_SLOPES: &[&str] = &["Db12", "Db24"];
const NOISE_TYPES: &[&str] = &["White", "Pink", "Brownian"];

// ---------------------------------------------------------------------------
// Ports

const STEREO_OUT: &[PortSpec] = &[
    audio_out(PortId::AudioOutput0, "Left"),
    audio_out(PortId::AudioOutput1, "Right"),
];

const OSCILLATOR_INPUTS: &[PortSpec] = &[
    system(PortId::GlobalFrequency, "Pitch"),
    system(PortId::GlobalGate, "Gate"),
    modulated(PortId::FrequencyMod, "Frequency mod"),
    modulated(PortId::PhaseMod, "Phase mod"),
    modulated(PortId::ModIndex, "Mod index"),
    modulated(PortId::GainMod, "Gain"),
    modulated(PortId::FeedbackMod, "Feedback"),
    modulated(PortId::DetuneMod, "Detune"),
];

const WAVETABLE_INPUTS: &[PortSpec] = &[
    system(PortId::GlobalFrequency, "Pitch"),
    system(PortId::GlobalGate, "Gate"),
    modulated(PortId::FrequencyMod, "Frequency mod"),
    modulated(PortId::PhaseMod, "Phase mod"),
    modulated(PortId::ModIndex, "Mod index"),
    modulated(PortId::WavetableIndex, "Wavetable index"),
    modulated(PortId::GainMod, "Gain"),
    modulated(PortId::FeedbackMod, "Feedback"),
    modulated(PortId::DetuneMod, "Detune"),
];

const SAMPLER_INPUTS: &[PortSpec] = &[
    system(PortId::GlobalFrequency, "Pitch"),
    system(PortId::GlobalGate, "Gate"),
    modulated(PortId::FrequencyMod, "Frequency mod"),
    modulated(PortId::GainMod, "Gain"),
    modulated(PortId::StereoPan, "Pan"),
    modulated(PortId::SampleOffset, "Sample offset"),
];

const NOISE_INPUTS: &[PortSpec] = &[
    modulated(PortId::CutoffMod, "Cutoff"),
    modulated(PortId::GainMod, "Gain"),
];

const ARPEGGIATOR_INPUTS: &[PortSpec] = &[system(PortId::GlobalGate, "Gate")];
const ARPEGGIATOR_OUTPUTS: &[PortSpec] = &[
    control_out(PortId::AudioOutput0, "Out"),
    control_out(PortId::ArpGate, "Arp gate"),
];

const ENVELOPE_INPUTS: &[PortSpec] = &[
    system(PortId::CombinedGate, "Gate"),
    modulated(PortId::AttackMod, "Attack"),
];
const CONTROL_OUT: &[PortSpec] = &[control_out(PortId::AudioOutput0, "Out")];

const LFO_INPUTS: &[PortSpec] = &[
    system(PortId::CombinedGate, "Gate"),
    modulated(PortId::FrequencyMod, "Frequency mod"),
    modulated(PortId::GainMod, "Gain"),
];

const FILTER_INPUTS: &[PortSpec] = &[
    audio_in(PortId::AudioInput0, "In"),
    system(PortId::GlobalFrequency, "Pitch"),
    system(PortId::Frequency, "Base frequency"),
    modulated(PortId::CutoffMod, "Cutoff"),
    modulated(PortId::ResonanceMod, "Resonance"),
];
const AUDIO_OUT: &[PortSpec] = &[audio_out(PortId::AudioOutput0, "Out")];

const MIXER_INPUTS: &[PortSpec] = &[
    audio_in(PortId::AudioInput0, "In"),
    modulated(PortId::GainMod, "Gain"),
    modulated(PortId::StereoPan, "Pan"),
];

const GLOBAL_FREQUENCY_INPUTS: &[PortSpec] = &[modulated(PortId::DetuneMod, "Detune")];
const GLOBAL_FREQUENCY_OUTPUTS: &[PortSpec] = &[control_out(PortId::GlobalFrequency, "Pitch")];

const GLIDE_INPUTS: &[PortSpec] = &[
    system(PortId::AudioInput0, "Pitch"),
    system(PortId::CombinedGate, "Gate"),
];

const VELOCITY_INPUTS: &[PortSpec] = &[system(PortId::GlobalGate, "Gate")];

const GATEMIXER_INPUTS: &[PortSpec] = &[
    system(PortId::GlobalGate, "Gate"),
    system(PortId::ArpGate, "Arp gate"),
];
const GATEMIXER_OUTPUTS: &[PortSpec] = &[control_out(PortId::CombinedGate, "Gate")];

// ---------------------------------------------------------------------------
// Parameters (keys are the patch's state keys; defaults are the DAW's)

const OSCILLATOR_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Active", true),
    ParamSpec::choice("waveform", "Waveform", WAVEFORMS, 0),
    ParamSpec::float("gain", "Gain", 0.0, 1.0, 0.5).modulated_by(PortId::GainMod),
    ParamSpec::float("phase_mod_amount", "Mod index", 0.0, 30.0, 0.0)
        .modulated_by(PortId::ModIndex),
    ParamSpec::float("feedback_amount", "Feedback", 0.0, 1.0, 0.0)
        .modulated_by(PortId::FeedbackMod),
    ParamSpec::int("detune_oct", "Octave", -5.0, 5.0, 0.0),
    ParamSpec::int("detune_semi", "Semitones", -12.0, 12.0, 0.0).unit("st"),
    ParamSpec::float("detune_cents", "Cents", -100.0, 100.0, 0.0)
        .unit("cents")
        .step(1.0),
    ParamSpec::int("unison_voices", "Unison", 1.0, 10.0, 1.0),
    ParamSpec::float("spread", "Spread", 0.0, 100.0, 0.0)
        .unit("cents")
        .step(1.0),
    ParamSpec::boolean("hard_sync", "Hard sync", false),
];

const WAVETABLE_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Active", true),
    ParamSpec::float("gain", "Gain", 0.0, 1.0, 0.5).modulated_by(PortId::GainMod),
    ParamSpec::float("wave_index", "Wave index", 0.0, 1.0, 0.0)
        .modulated_by(PortId::WavetableIndex),
    ParamSpec::float("phase_mod_amount", "Mod index", 0.0, 30.0, 0.0)
        .modulated_by(PortId::ModIndex),
    ParamSpec::float("feedback_amount", "Feedback", 0.0, 1.0, 0.0)
        .modulated_by(PortId::FeedbackMod),
    ParamSpec::int("detune_oct", "Octave", -5.0, 5.0, 0.0),
    ParamSpec::int("detune_semi", "Semitones", -12.0, 12.0, 0.0).unit("st"),
    ParamSpec::float("detune_cents", "Cents", -100.0, 100.0, 0.0)
        .unit("cents")
        .step(1.0),
    ParamSpec::int("unison_voices", "Unison", 1.0, 10.0, 1.0),
    ParamSpec::float("spread", "Spread", 0.0, 100.0, 0.0)
        .unit("cents")
        .step(1.0),
    ParamSpec::boolean("hard_sync", "Hard sync", false),
];

const SAMPLER_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Active", true),
    ParamSpec::float("gain", "Gain", 0.0, 1.5, 1.0).modulated_by(PortId::GainMod),
    ParamSpec::int("detune_oct", "Octave", -5.0, 5.0, 0.0),
    ParamSpec::int("detune_semi", "Semitones", -12.0, 12.0, 0.0).unit("st"),
    ParamSpec::float("detune_cents", "Cents", -100.0, 100.0, 0.0)
        .unit("cents")
        .step(1.0),
    ParamSpec::int("rootNote", "Root note", 0.0, 127.0, 60.0),
    ParamSpec::choice("loopMode", "Loop mode", LOOP_MODES, 0),
    ParamSpec::float("loopStart", "Loop start", 0.0, 1.0, 0.0),
    ParamSpec::float("loopEnd", "Loop end", 0.0, 1.0, 1.0),
    ParamSpec::choice("triggerMode", "Trigger", SAMPLER_TRIGGERS, 1),
];

const NOISE_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("is_enabled", "Enabled", false),
    ParamSpec::choice("noiseType", "Noise type", NOISE_TYPES, 0),
    ParamSpec::float("cutoff", "Cutoff", 0.0, 1.0, 1.0).modulated_by(PortId::CutoffMod),
    ParamSpec::float("gain", "Gain", 0.0, 1.0, 1.0).modulated_by(PortId::GainMod),
];

const ENVELOPE_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Active", true),
    ParamSpec::float("attack", "Attack", 0.0, 3.0, 0.0)
        .unit("s")
        .modulated_by(PortId::AttackMod),
    ParamSpec::float("decay", "Decay", 0.0, 10.0, 0.1).unit("s"),
    ParamSpec::float("sustain", "Sustain", 0.0, 1.0, 0.5),
    ParamSpec::float("release", "Release", 0.0, 3.0, 0.1).unit("s"),
    ParamSpec::float("attackCurve", "Attack curve", -10.0, 10.0, 0.0),
    ParamSpec::float("decayCurve", "Decay curve", -10.0, 10.0, 0.0),
    ParamSpec::float("releaseCurve", "Release curve", -10.0, 10.0, 0.0),
];

const LFO_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Active", true),
    ParamSpec::choice("waveform", "Waveform", LFO_WAVEFORMS, 0),
    ParamSpec::float("frequency", "Frequency", 0.01, 20.0, 1.0)
        .unit("Hz")
        .log()
        .modulated_by(PortId::FrequencyMod),
    ParamSpec::float("gain", "Gain", -5.0, 5.0, 1.0).modulated_by(PortId::GainMod),
    ParamSpec::float("phaseOffset", "Phase", 0.0, 1.0, 0.0),
    ParamSpec::boolean("useAbsolute", "Absolute", false),
    ParamSpec::boolean("useNormalized", "Normalized", false),
    ParamSpec::choice("triggerMode", "Trigger", LFO_TRIGGERS, 0),
    ParamSpec::choice("loopMode", "Loop mode", LOOP_MODES, 0),
    ParamSpec::float("loopStart", "Loop start", 0.0, 1.0, 0.5),
    ParamSpec::float("loopEnd", "Loop end", 0.0, 1.0, 1.0),
];

const FILTER_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Active", true),
    ParamSpec::choice("filter_type", "Type", FILTER_TYPES, 0),
    ParamSpec::choice("filter_slope", "Slope", FILTER_SLOPES, 0),
    ParamSpec::float("cutoff", "Cutoff", 20.0, 20_000.0, 20_000.0)
        .unit("Hz")
        .log()
        .modulated_by(PortId::CutoffMod),
    ParamSpec::float("resonance", "Resonance", 0.0, 1.0, 0.0).modulated_by(PortId::ResonanceMod),
    ParamSpec::float("keytracking", "Key tracking", 0.0, 1.0, 0.0),
    ParamSpec::float("gain", "Gain", 0.0, 2.0, 0.5),
    ParamSpec::float("comb_frequency", "Comb frequency", 55.0, 880.0, 220.0)
        .unit("Hz")
        .log(),
    ParamSpec::float("comb_dampening", "Comb damping", 0.0, 1.0, 0.5),
];

const GLIDE_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Portamento", false),
    ParamSpec::float("time", "Time", 0.0, 1.0, 0.0).unit("s"),
];

const VELOCITY_PARAMS: &[ParamSpec] = &[
    ParamSpec::float("sensitivity", "Sensitivity", 0.0, 2.0, 1.0),
    ParamSpec::float("randomize", "Randomize", 0.0, 1.0, 0.0),
];

const CHORUS_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Enabled", false),
    ParamSpec::float("baseDelayMs", "Delay", 0.1, 65.0, 15.0).unit("ms"),
    ParamSpec::float("depthMs", "Depth", 0.0, 25.0, 5.0).unit("ms"),
    ParamSpec::float("lfoRateHz", "Rate", 0.01, 10.0, 0.5)
        .unit("Hz")
        .log(),
    ParamSpec::float("feedback", "Feedback", 0.0, 0.98, 0.3),
    ParamSpec::float("feedback_filter", "Filter", 0.0, 1.0, 0.5),
    ParamSpec::float("mix", "Mix", 0.0, 1.0, 0.5),
    ParamSpec::float("stereoPhaseOffsetDeg", "Stereo phase", 0.0, 360.0, 90.0).unit("deg"),
];

const DELAY_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Enabled", false),
    ParamSpec::float("delayMs", "Delay", 0.0, 2000.0, 250.0).unit("ms"),
    ParamSpec::float("feedback", "Feedback", 0.0, 1.0, 0.5),
    ParamSpec::float("wetMix", "Mix", 0.0, 1.0, 0.1),
];

const REVERB_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Enabled", false),
    ParamSpec::float("room_size", "Room size", 0.0, 1.0, 0.95),
    ParamSpec::float("damp", "Damping", 0.0, 1.0, 0.5),
    ParamSpec::float("wet", "Wet", 0.0, 1.0, 0.3),
    ParamSpec::float("dry", "Dry", 0.0, 1.0, 0.7),
    ParamSpec::float("width", "Width", 0.0, 1.0, 1.0),
];

const CONVOLVER_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Enabled", false),
    ParamSpec::float("wetMix", "Mix", 0.0, 1.0, 0.1),
];

const COMPRESSOR_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Enabled", true),
    ParamSpec::float("thresholdDb", "Threshold", -60.0, 0.0, -12.0).unit("dB"),
    ParamSpec::float("ratio", "Ratio", 1.0, 20.0, 4.0),
    ParamSpec::float("attackMs", "Attack", 0.1, 200.0, 10.0)
        .unit("ms")
        .log(),
    ParamSpec::float("releaseMs", "Release", 5.0, 500.0, 80.0)
        .unit("ms")
        .log(),
    ParamSpec::float("makeupGainDb", "Makeup", 0.0, 24.0, 3.0).unit("dB"),
    ParamSpec::float("mix", "Mix", 0.0, 1.0, 0.5),
];

const SATURATION_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Enabled", false),
    ParamSpec::float("drive", "Drive", 0.5, 8.0, 2.0),
    ParamSpec::float("mix", "Mix", 0.0, 1.0, 0.5),
];

const BITCRUSHER_PARAMS: &[ParamSpec] = &[
    ParamSpec::boolean("active", "Enabled", false),
    ParamSpec::int("bits", "Bits", 2.0, 24.0, 12.0),
    ParamSpec::int("downsampleFactor", "Downsample", 1.0, 32.0, 4.0),
    ParamSpec::float("mix", "Mix", 0.0, 1.0, 0.5),
];

// ---------------------------------------------------------------------------
// Node types

#[allow(clippy::too_many_arguments)]
const fn node(
    type_name: &'static str,
    title: &'static str,
    category: Category,
    state_map: Option<&'static str>,
    state_shape: StateShape,
    inputs: &'static [PortSpec],
    outputs: &'static [PortSpec],
    params: &'static [ParamSpec],
) -> NodeSpec {
    NodeSpec {
        type_name,
        title,
        category,
        state_map,
        state_shape,
        singleton: false,
        deletable: true,
        inputs,
        outputs,
        params,
        slot: None,
        extra_state: &[],
    }
}

impl NodeSpec {
    const fn singleton(mut self) -> NodeSpec {
        self.singleton = true;
        self
    }

    /// A node the voice cannot do without.
    const fn fixed(mut self) -> NodeSpec {
        self.singleton = true;
        self.deletable = false;
        self
    }

    const fn slot(mut self, slot: usize) -> NodeSpec {
        self.slot = Some(slot);
        self
    }

    const fn extra(mut self, extra_state: &'static [(&'static str, Extra)]) -> NodeSpec {
        self.extra_state = extra_state;
        self
    }
}

/// The node types of a voice, in the order the loader creates them.
pub static NODES: &[NodeSpec] = &[
    node(
        "global_frequency",
        "Global frequency",
        Category::System,
        None,
        StateShape::None,
        GLOBAL_FREQUENCY_INPUTS,
        GLOBAL_FREQUENCY_OUTPUTS,
        &[],
    )
    .fixed(),
    node(
        "glide",
        "Glide",
        Category::System,
        Some("glides"),
        StateShape::Map,
        GLIDE_INPUTS,
        AUDIO_OUT,
        GLIDE_PARAMS,
    )
    .fixed(),
    node(
        "global_velocity",
        "Velocity",
        Category::System,
        Some("velocity"),
        StateShape::Single,
        VELOCITY_INPUTS,
        CONTROL_OUT,
        VELOCITY_PARAMS,
    )
    .extra(&[("active", Extra::Bool(true))])
    .fixed(),
    node(
        "gatemixer",
        "Gate mixer",
        Category::System,
        None,
        StateShape::None,
        GATEMIXER_INPUTS,
        GATEMIXER_OUTPUTS,
        &[],
    )
    .fixed(),
    node(
        "mixer",
        "Mixer",
        Category::Mixer,
        None,
        StateShape::None,
        MIXER_INPUTS,
        STEREO_OUT,
        &[],
    )
    .fixed(),
    node(
        "filter",
        "Filter",
        Category::Filter,
        Some("filters"),
        StateShape::Map,
        FILTER_INPUTS,
        AUDIO_OUT,
        FILTER_PARAMS,
    )
    .extra(&[("oversampling", Extra::Num(0.0))]),
    node(
        "oscillator",
        "Oscillator",
        Category::Generator,
        Some("oscillators"),
        StateShape::Map,
        OSCILLATOR_INPUTS,
        STEREO_OUT,
        OSCILLATOR_PARAMS,
    )
    .extra(&[
        ("detune", Extra::Num(0.0)),
        ("freq_mod_amount", Extra::Num(0.0)),
        ("wave_index", Extra::Num(0.0)),
    ]),
    node(
        "wavetable_oscillator",
        "Wavetable oscillator",
        Category::Generator,
        Some("wavetableOscillators"),
        StateShape::Map,
        WAVETABLE_INPUTS,
        STEREO_OUT,
        WAVETABLE_PARAMS,
    )
    .extra(&[
        ("detune", Extra::Num(0.0)),
        ("freq_mod_amount", Extra::Num(0.0)),
        ("waveform", Extra::Num(0.0)),
    ]),
    node(
        "sampler",
        "Sampler",
        Category::Generator,
        Some("samplers"),
        StateShape::Map,
        SAMPLER_INPUTS,
        STEREO_OUT,
        SAMPLER_PARAMS,
    )
    .extra(&[
        ("detune", Extra::Num(0.0)),
        ("frequency", Extra::Num(440.0)),
        ("sampleLength", Extra::Num(44_100.0)),
        ("sampleRate", Extra::Num(44_100.0)),
        ("channels", Extra::Num(1.0)),
    ]),
    node(
        "envelope",
        "Envelope",
        Category::Modulator,
        Some("envelopes"),
        StateShape::Map,
        ENVELOPE_INPUTS,
        CONTROL_OUT,
        ENVELOPE_PARAMS,
    ),
    node(
        "lfo",
        "LFO",
        Category::Modulator,
        Some("lfos"),
        StateShape::Map,
        LFO_INPUTS,
        CONTROL_OUT,
        LFO_PARAMS,
    ),
    node(
        "noise",
        "Noise",
        Category::Generator,
        Some("noise"),
        StateShape::Single,
        NOISE_INPUTS,
        STEREO_OUT,
        NOISE_PARAMS,
    )
    .singleton(),
    node(
        "arpeggiator_generator",
        "Arpeggiator",
        Category::Modulator,
        None,
        StateShape::None,
        ARPEGGIATOR_INPUTS,
        ARPEGGIATOR_OUTPUTS,
        &[],
    )
    .singleton(),
];

/// The effects, in the default chain's order (slot 0 first).
pub static EFFECTS: &[NodeSpec] = &[
    node(
        "chorus",
        "Chorus",
        Category::Effect,
        Some("choruses"),
        StateShape::Map,
        &[],
        &[],
        CHORUS_PARAMS,
    )
    .slot(0),
    node(
        "delay",
        "Delay",
        Category::Effect,
        Some("delays"),
        StateShape::Map,
        &[],
        &[],
        DELAY_PARAMS,
    )
    .slot(1),
    node(
        "freeverb",
        "Reverb",
        Category::Effect,
        Some("reverbs"),
        StateShape::Map,
        &[],
        &[],
        REVERB_PARAMS,
    )
    .slot(2),
    node(
        "convolver",
        "Convolution reverb",
        Category::Effect,
        Some("convolvers"),
        StateShape::Map,
        &[],
        &[],
        CONVOLVER_PARAMS,
    )
    .slot(3),
    node(
        "limiter",
        "Limiter",
        Category::Effect,
        None,
        StateShape::None,
        &[],
        &[],
        &[],
    )
    .slot(4),
    node(
        "compressor",
        "Compressor",
        Category::Effect,
        Some("compressors"),
        StateShape::Map,
        &[],
        &[],
        COMPRESSOR_PARAMS,
    )
    .slot(5),
    node(
        "saturation",
        "Saturation",
        Category::Effect,
        Some("saturations"),
        StateShape::Map,
        &[],
        &[],
        SATURATION_PARAMS,
    )
    .slot(6),
    node(
        "bitcrusher",
        "Bitcrusher",
        Category::Effect,
        Some("bitcrushers"),
        StateShape::Map,
        &[],
        &[],
        BITCRUSHER_PARAMS,
    )
    .slot(7),
];

/// A patch-level parameter: the instrument's output gain.
pub const INSTRUMENT_GAIN: ParamSpec = ParamSpec::float("instrumentGain", "Volume", 0.0, 2.0, 1.0);

/// The patch-level parameters (not a node's).
pub static PATCH_PARAMS: &[ParamSpec] = &[INSTRUMENT_GAIN];

/// A macro's value.
pub const MACRO_VALUE: ParamSpec = ParamSpec::float("macro", "Macro", 0.0, 1.0, 0.0);

/// The node type `type_name` (a voice node or an effect).
pub fn node_spec(type_name: &str) -> Option<&'static NodeSpec> {
    NODES
        .iter()
        .chain(EFFECTS.iter())
        .find(|n| n.type_name == type_name)
}

/// The effect type `type_name`.
pub fn effect_spec(type_name: &str) -> Option<&'static NodeSpec> {
    EFFECTS.iter().find(|n| n.type_name == type_name)
}

/// Every node type and effect.
pub fn all_specs() -> impl Iterator<Item = &'static NodeSpec> {
    NODES.iter().chain(EFFECTS.iter())
}

/// The index of `key` among the type's parameters, which is how a live edit
/// names it.
pub fn param_index(type_name: &str, key: &str) -> Option<usize> {
    node_spec(type_name)?.param_index(key)
}

// ---------------------------------------------------------------------------
// JSON

fn port_json(spec: &PortSpec, input: bool) -> Value {
    let mut port = json!({
        "port": spec.port.name(),
        "id": spec.port as u32,
        "label": spec.label,
        "kind": spec.kind.name(),
    });
    if input {
        port["modulatable"] = json!(spec.modulatable);
        port["system"] = json!(spec.system);
        port["default_modulation"] = json!(spec.default_modulation().name());
    }
    port
}

fn param_json(spec: &ParamSpec) -> Value {
    let mut param = json!({
        "key": spec.key,
        "label": spec.label,
        "kind": spec.kind.name(),
        "default": spec.default_value(),
    });
    match spec.kind {
        ParamKind::Float | ParamKind::Int => {
            param["min"] = number(spec.min);
            param["max"] = number(spec.max);
            param["curve"] = json!(spec.curve.name());
            if spec.step != 0.0 {
                param["step"] = number(spec.step);
            }
        }
        ParamKind::Enum => {
            param["options"] = json!(spec.options);
        }
        ParamKind::Bool => {}
    }
    if !spec.unit.is_empty() {
        param["unit"] = json!(spec.unit);
    }
    if let Some(port) = spec.modulated_by {
        param["modulated_by"] = json!(port.name());
    }
    param
}

fn node_json(spec: &NodeSpec) -> Value {
    let mut node = json!({
        "type": spec.type_name,
        "title": spec.title,
        "category": spec.category.name(),
        "state_map": spec.state_map,
        "state_shape": spec.state_shape.name(),
        "singleton": spec.singleton,
        "deletable": spec.deletable,
        "inputs": spec.inputs.iter().map(|p| port_json(p, true)).collect::<Vec<_>>(),
        "outputs": spec.outputs.iter().map(|p| port_json(p, false)).collect::<Vec<_>>(),
        "params": spec.params.iter().map(param_json).collect::<Vec<_>>(),
    });
    if let Some(slot) = spec.slot {
        node["slot"] = json!(slot);
        node["default_id"] = json!((EFFECT_ID_BASE + slot).to_string());
    }
    node
}

/// The catalogue as JSON, the shape an editor reads (see the module docs and
/// `docs/plans/synth_studio.md`):
///
/// ```text
/// { version, nodes: [..], effects: [..], modulation_types: [..], transforms: [..],
///   ports: [{id, name}], patch: {params: [..], macro_count} }
/// ```
pub fn catalog_json() -> Value {
    json!({
        "version": VERSION,
        "nodes": NODES.iter().map(node_json).collect::<Vec<_>>(),
        "effects": EFFECTS.iter().map(node_json).collect::<Vec<_>>(),
        "modulation_types": ModulationType::ALL.iter().map(|m| m.name()).collect::<Vec<_>>(),
        "transforms": TRANSFORMS,
        "ports": PortId::ALL
            .iter()
            .map(|p| json!({"id": *p as u32, "name": p.name()}))
            .collect::<Vec<_>>(),
        "patch": {
            "params": PATCH_PARAMS.iter().map(param_json).collect::<Vec<_>>(),
            "macro_count": MACRO_COUNT,
            "macro_value": param_json(&MACRO_VALUE),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_engine::patch::{
        BitcrusherState, ChorusState, CompressorState, ConvolverState, DelayState, FilterState,
        GlideState, LfoState, NoiseState, ReverbState, SamplerState, SaturationState,
        VelocityState,
    };
    use crate::audio_engine::patch_loader::NODE_CREATION_ORDER;
    use crate::nodes::{
        AnalogOscillatorStateUpdate, EnvelopeConfig, WavetableOscillatorStateUpdate,
    };
    use serde::de::DeserializeOwned;
    use serde::Serialize;
    use std::collections::BTreeSet;

    fn keys(value: &Value) -> BTreeSet<String> {
        value.as_object().unwrap().keys().cloned().collect()
    }

    /// The type's default state, read into the typed struct the engine reads
    /// patches into and written back: every catalogue key must be in what comes
    /// out (the field exists and its serde name is the key).
    fn check_state<T: DeserializeOwned + Serialize>(type_name: &str) {
        let spec = node_spec(type_name).unwrap();
        let state = Value::Object(spec.default_state("00000000-0000-0000-0000-000000000001"));
        let typed: T = serde_json::from_value(state.clone()).unwrap_or_else(|e| {
            panic!("the default {type_name} state does not deserialize: {e}\n{state}")
        });
        let out = serde_json::to_value(&typed).unwrap();
        let out_keys = keys(&out);
        for param in spec.params {
            assert!(
                out_keys.contains(param.key),
                "{type_name}: key '{}' is not in the serialized state {out_keys:?}",
                param.key
            );
        }
    }

    #[test]
    fn every_param_key_is_a_field_of_its_state_struct() {
        check_state::<AnalogOscillatorStateUpdate>("oscillator");
        check_state::<WavetableOscillatorStateUpdate>("wavetable_oscillator");
        check_state::<EnvelopeConfig>("envelope");
        check_state::<LfoState>("lfo");
        check_state::<FilterState>("filter");
        check_state::<SamplerState>("sampler");
        check_state::<GlideState>("glide");
        check_state::<ChorusState>("chorus");
        check_state::<DelayState>("delay");
        check_state::<ReverbState>("freeverb");
        check_state::<ConvolverState>("convolver");
        check_state::<CompressorState>("compressor");
        check_state::<SaturationState>("saturation");
        check_state::<BitcrusherState>("bitcrusher");
        check_state::<NoiseState>("noise");
        check_state::<VelocityState>("global_velocity");
    }

    #[test]
    fn every_node_the_loader_can_build_is_listed() {
        for node_type in NODE_CREATION_ORDER {
            let spec = node_spec(node_type)
                .unwrap_or_else(|| panic!("node type '{node_type}' is not in the catalogue"));
            assert!(!spec.is_effect());
        }
        // ... and the catalogue lists nothing the loader would refuse.
        for spec in NODES {
            assert!(
                NODE_CREATION_ORDER.contains(&spec.type_name),
                "'{}' is not a type the loader creates",
                spec.type_name
            );
        }
        let order: Vec<_> = NODES.iter().map(|n| n.type_name).collect();
        assert_eq!(order, NODE_CREATION_ORDER, "same order as the loader");
    }

    #[test]
    fn the_eight_effects_are_the_default_chain() {
        let names: Vec<_> = EFFECTS.iter().map(|e| e.type_name).collect();
        assert_eq!(
            names,
            [
                "chorus",
                "delay",
                "freeverb",
                "convolver",
                "limiter",
                "compressor",
                "saturation",
                "bitcrusher"
            ]
        );
        for (i, effect) in EFFECTS.iter().enumerate() {
            assert_eq!(effect.slot, Some(i));
            assert!(effect.is_effect());
            assert!(effect.inputs.is_empty() && effect.outputs.is_empty());
        }
    }

    #[test]
    fn declared_ports_are_the_ports_of_the_built_nodes() {
        use crate::audio_engine::native::AudioEngine;
        let mut engine = AudioEngine::new(48_000.0, 1);
        for spec in NODES {
            let node = engine.build_node_for_test(spec.type_name);
            let ports = node.get_ports();
            let mut declared: BTreeSet<(usize, bool)> = BTreeSet::new();
            for p in spec.inputs {
                assert!(
                    declared.insert((p.port as usize, false)),
                    "{}: input {:?} twice",
                    spec.type_name,
                    p.port
                );
            }
            for p in spec.outputs {
                assert!(
                    declared.insert((p.port as usize, true)),
                    "{}: output {:?} twice",
                    spec.type_name,
                    p.port
                );
            }
            let actual: BTreeSet<(usize, bool)> = ports
                .iter()
                .map(|(port, is_output)| (*port as usize, *is_output))
                .collect();
            assert_eq!(declared, actual, "ports of '{}'", spec.type_name);
        }
    }

    #[test]
    fn effect_nodes_are_the_effects_the_engine_builds() {
        use crate::audio_engine::native::AudioEngine;
        let engine = AudioEngine::new(48_000.0, 1);
        let kinds = engine.effect_kinds();
        let expected: Vec<_> = EFFECTS.iter().map(|e| e.type_name.to_string()).collect();
        assert_eq!(kinds, expected);
    }

    #[test]
    fn params_have_sane_ranges_and_defaults() {
        let mut seen_keys = BTreeSet::new();
        let patch_spec = NodeSpec {
            type_name: "patch",
            title: "",
            category: Category::System,
            state_map: None,
            state_shape: StateShape::None,
            singleton: true,
            deletable: false,
            inputs: &[],
            outputs: &[],
            params: PATCH_PARAMS,
            slot: None,
            extra_state: &[],
        };
        let mut specs: Vec<&NodeSpec> = all_specs().collect();
        specs.push(&patch_spec);
        for spec in specs {
            seen_keys.clear();
            for p in spec.params {
                assert!(
                    seen_keys.insert(p.key),
                    "{}: key {} twice",
                    spec.type_name,
                    p.key
                );
                match p.kind {
                    ParamKind::Float | ParamKind::Int => {
                        assert!(p.min < p.max, "{}.{}", spec.type_name, p.key);
                        assert!(
                            p.default >= p.min && p.default <= p.max,
                            "{}.{} default {} outside {}..{}",
                            spec.type_name,
                            p.key,
                            p.default,
                            p.min,
                            p.max
                        );
                    }
                    ParamKind::Enum => {
                        assert!(!p.options.is_empty());
                        assert!((p.default as usize) < p.options.len());
                    }
                    ParamKind::Bool => {}
                }
                // The default passes its own check.
                p.check(&p.default_value()).unwrap();
                if let Some(port) = p.modulated_by {
                    assert!(
                        spec.input(port).is_some_and(|i| i.modulatable),
                        "{}.{} is modulated by {:?}, which the node does not take",
                        spec.type_name,
                        p.key,
                        port
                    );
                }
            }
        }
    }

    #[test]
    fn param_checks_clamp_and_refuse() {
        let osc = node_spec("oscillator").unwrap();
        let gain = osc.param("gain").unwrap();
        assert_eq!(gain.check(&json!(3.0)).unwrap(), json!(1));
        assert!(gain.check(&json!("loud")).is_err());
        let unison = osc.param("unison_voices").unwrap();
        assert_eq!(unison.check(&json!(3.4)).unwrap(), json!(3));
        assert_eq!(unison.check(&json!(99)).unwrap(), json!(10));
        let wave = osc.param("waveform").unwrap();
        assert_eq!(wave.check(&json!(2)).unwrap(), json!(2));
        assert!(wave.check(&json!(4)).is_err());
        assert!(wave.check(&json!(1.5)).is_err());
        let active = osc.param("active").unwrap();
        assert_eq!(active.check(&json!(false)).unwrap(), json!(false));
        assert_eq!(active.check(&json!(1)).unwrap(), json!(true));
        assert!(active.check(&json!(0.5)).is_err());
    }

    #[test]
    fn modulation_types_and_transforms_match_the_graph() {
        use crate::graph::ModulationType as Graph;
        for m in ModulationType::ALL {
            assert_eq!(Graph::from_i32(m.value()) as u8 as i32, m.value());
        }
        use crate::graph::ModulationTransformation as Transform;
        for (i, name) in TRANSFORMS.iter().enumerate() {
            assert_eq!(Transform::from_i32(i as i32) as u32, i as u32, "{name}");
        }
        // The port numbers are the enum's.
        for port in PortId::ALL {
            assert_eq!(PortId::try_from_u32(port as u32), Some(port));
            assert_eq!(PortId::from_name(port.name()), Some(port));
            assert_eq!(PortId::from_u32(port as u32), port);
        }
    }

    #[test]
    fn json_has_the_contract_shape() {
        let json = catalog_json();
        assert_eq!(json["version"], 1);
        let nodes = json["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), NODES.len());
        let filter = nodes.iter().find(|n| n["type"] == "filter").unwrap();
        assert_eq!(filter["title"], "Filter");
        assert_eq!(filter["category"], "filter");
        assert_eq!(filter["state_map"], "filters");
        assert_eq!(filter["singleton"], false);
        assert_eq!(filter["deletable"], true);
        let cutoff_mod = filter["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["port"] == "CutoffMod")
            .unwrap();
        assert_eq!(cutoff_mod["id"], PortId::CutoffMod as u32);
        assert_eq!(cutoff_mod["kind"], "control");
        assert_eq!(cutoff_mod["modulatable"], true);
        let cutoff = filter["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "cutoff")
            .unwrap();
        assert_eq!(cutoff["kind"], "float");
        assert_eq!(cutoff["min"], 20);
        assert_eq!(cutoff["max"], 20000);
        assert_eq!(cutoff["default"], 20000);
        assert_eq!(cutoff["unit"], "Hz");
        assert_eq!(cutoff["curve"], "log");
        assert_eq!(cutoff["modulated_by"], "CutoffMod");
        let filter_type = filter["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "filter_type")
            .unwrap();
        assert_eq!(filter_type["kind"], "enum");
        assert_eq!(filter_type["options"][0], "LowPass");
        assert_eq!(filter_type["default"], 0);
        let effects = json["effects"].as_array().unwrap();
        assert_eq!(effects.len(), 8);
        assert_eq!(effects[0]["default_id"], "10000");
        assert_eq!(effects[7]["default_id"], "10007");
        assert_eq!(effects[0]["category"], "effect");
        assert_eq!(
            json["modulation_types"],
            json!(["vca", "bipolar", "additive"])
        );
        assert_eq!(
            json["transforms"],
            json!(["none", "invert", "square", "cube"])
        );
        assert_eq!(json["patch"]["macro_count"], 4);
    }

    /// `tests/fixtures/catalog.json` is the catalogue as the engine's game ships
    /// it. Rewrite it with `CUBED_UPDATE_CATALOG=1 cargo test -p cubed-dsp catalog`.
    #[test]
    fn the_fixture_is_the_catalogue() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("catalog.json");
        let text = serde_json::to_string_pretty(&catalog_json()).unwrap() + "\n";
        if std::env::var_os("CUBED_UPDATE_CATALOG").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &text).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            on_disk.replace("\r\n", "\n"),
            text,
            "tests/fixtures/catalog.json is out of date: run CUBED_UPDATE_CATALOG=1 cargo test -p cubed-dsp catalog"
        );
    }
}
