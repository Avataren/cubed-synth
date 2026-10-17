//! What the host sends the mixer, and what it gets back.

use std::sync::Arc;

use crate::clip::Clip;
use crate::insert::Insert;
use crate::source::Source;
use crate::spatial::{Listener, Spatial};

/// A bus slot. The host names its buses and maps the names to slots; slot 0
/// is the master bus, which feeds the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BusId(pub u16);

impl BusId {
    pub const MASTER: BusId = BusId(0);
}

/// A clip slot, below `MixerConfig::max_clips`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClipId(pub u32);

/// A prepared-source slot, below `MixerConfig::max_sources`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(pub u32);

/// A voice, named by the host when it starts one. Ids should not repeat
/// while a voice with that id may still be playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VoiceId(pub u64);

/// A bus's settings. Gains are in dB; at or below [`crate::MIN_DB`] the bus is
/// silent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BusSettings {
    /// Where the bus's output goes; ignored for the master bus.
    pub output: BusId,
    pub gain_db: f32,
    /// Balance, -1 (left) to 1 (right).
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    /// The most voices that play on the bus at once; a new one beyond it
    /// steals the bus's lowest-priority, oldest voice.
    pub voice_limit: u16,
    /// Turns the bus down while another bus is loud.
    pub duck: Option<Duck>,
}

impl Default for BusSettings {
    fn default() -> Self {
        Self {
            output: BusId::MASTER,
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            voice_limit: u16::MAX,
            duck: None,
        }
    }
}

/// Ducking: a bus goes down by `depth_db` while its key bus's output peaks
/// above `threshold_db` (music under effects or dialogue).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Duck {
    /// The bus whose level ducks this one (after its fader).
    pub key: BusId,
    /// How far down, in dB (positive).
    pub depth_db: f32,
    pub threshold_db: f32,
    /// How fast it goes down, and comes back once the key is quiet.
    pub attack_ms: f32,
    pub release_ms: f32,
}

impl Duck {
    pub fn new(key: BusId, depth_db: f32) -> Duck {
        Duck {
            key,
            depth_db,
            threshold_db: -40.0,
            attack_ms: 20.0,
            release_ms: 400.0,
        }
    }
}

/// A send from one bus to another (e.g. to a reverb return).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SendSettings {
    pub to: BusId,
    pub level_db: f32,
    /// Taken before the bus's fader (and mute) instead of after it.
    pub pre_fader: bool,
}

/// When a voice starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// At the start of the next block.
    Now,
    /// At this output frame (see `Mixer::frame`); a frame already past
    /// starts now.
    AtFrame(u64),
}

/// A clip to play.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Play {
    pub voice: VoiceId,
    pub clip: ClipId,
    pub bus: BusId,
    /// Linear gain.
    pub gain: f32,
    /// Playback speed, which also shifts the pitch; 1 is unchanged.
    pub pitch: f32,
    /// Balance, -1 (left) to 1 (right).
    pub pan: f32,
    pub looped: bool,
    /// Who loses when a bus runs out of voices: lower priorities first.
    pub priority: u8,
    pub start: Start,
    /// Where it is heard from; `None` plays it as it is (music, UI).
    pub spatial: Option<Spatial>,
}

impl Play {
    pub fn new(voice: VoiceId, clip: ClipId, bus: BusId) -> Play {
        Play {
            voice,
            clip,
            bus,
            gain: 1.0,
            pitch: 1.0,
            pan: 0.0,
            looped: false,
            priority: 128,
            start: Start::Now,
            spatial: None,
        }
    }
}

/// A prepared source to play (see `Command::PrepareSource`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaySource {
    pub voice: VoiceId,
    pub source: SourceId,
    pub bus: BusId,
    /// Linear gain.
    pub gain: f32,
    /// Balance, -1 (left) to 1 (right).
    pub pan: f32,
    pub priority: u8,
    pub start: Start,
    pub spatial: Option<Spatial>,
}

impl PlaySource {
    pub fn new(voice: VoiceId, source: SourceId, bus: BusId) -> PlaySource {
        PlaySource {
            voice,
            source,
            bus,
            gain: 1.0,
            pan: 0.0,
            priority: 128,
            start: Start::Now,
            spatial: None,
        }
    }
}

/// A recording of the master output, filled block by block until full and
/// then handed back as [`Returned::Capture`].
#[derive(Debug)]
pub struct Capture {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Frames written so far.
    pub filled: usize,
    /// The output frame the recording started at.
    pub start_frame: u64,
}

impl Capture {
    /// A recording of `frames` frames. Allocate it off the audio thread.
    pub fn new(frames: usize) -> Capture {
        Capture {
            left: vec![0.0; frames],
            right: vec![0.0; frames],
            filled: 0,
            start_frame: 0,
        }
    }
}

/// A change to the mix. The mixer applies commands between blocks and never
/// allocates doing so; anything it lets go of comes back as [`Returned`].
pub enum Command {
    /// Creates the bus or changes all its settings.
    SetBus(BusId, BusSettings),
    /// Removes the bus; its voices stop and buses feeding it move to master.
    RemoveBus(BusId),
    SetBusGain(BusId, f32),
    SetBusPan(BusId, f32),
    SetBusMute(BusId, bool),
    SetBusSolo(BusId, bool),
    SetBusOutput(BusId, BusId),
    /// Puts an effect in insert `slot` of the bus (`None` empties it).
    SetInsert {
        bus: BusId,
        slot: u8,
        insert: Option<Box<dyn Insert>>,
    },
    SetInsertBypass {
        bus: BusId,
        slot: u8,
        bypass: bool,
    },
    /// Changes a parameter of the insert in `slot` (`Insert::set_param`).
    SetInsertParam {
        bus: BusId,
        slot: u8,
        param: u8,
        value: f32,
    },
    /// Sets send `slot` of the bus (`None` removes it).
    SetSend {
        bus: BusId,
        slot: u8,
        send: Option<SendSettings>,
    },
    /// Makes a clip playable under `ClipId`; a clip already there is
    /// replaced (its voices stop).
    AddClip(ClipId, Arc<Clip>),
    /// Stops the clip's voices and lets it go.
    RemoveClip(ClipId),
    Play(Play),
    /// Puts a source in a slot for `PlaySource` (a source already there is
    /// handed back). Sources are objects, so the host builds them where the
    /// mixer runs (the worklet in its message handler) and plays them by id.
    PrepareSource(SourceId, Box<dyn Source>),
    /// Plays the prepared source; the slot is empty afterwards.
    PlaySource(PlaySource),
    SetVoiceGain(VoiceId, f32),
    /// Glides the voice's gain to `gain` over `frames` (a fade-in or a
    /// crossfade); `SetVoiceGain` cancels it.
    FadeVoice {
        voice: VoiceId,
        gain: f32,
        frames: u32,
    },
    SetVoicePitch(VoiceId, f32),
    SetVoicePan(VoiceId, f32),
    /// Fades the voice out over that many frames, then ends it.
    Stop(VoiceId, u32),
    /// Fades out every voice on the bus.
    StopBus(BusId, u32),
    /// Fades out every voice.
    StopAll(u32),
    /// Starts recording the master output into the buffer.
    Capture(Box<Capture>),
    /// Moves the listener (once per game frame, with its velocity).
    SetListener(Listener),
    /// Makes the voice spatial, changes its spatial settings, or (`None`)
    /// plays it as it is from now on.
    SetVoiceSpatial(VoiceId, Option<Spatial>),
    /// Moves a spatial voice (once per game frame, with its velocity).
    SetVoicePosition {
        voice: VoiceId,
        position: [f32; 3],
        velocity: [f32; 3],
    },
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::SetBus(bus, settings) => write!(f, "SetBus({bus:?}, {settings:?})"),
            Command::RemoveBus(bus) => write!(f, "RemoveBus({bus:?})"),
            Command::SetBusGain(bus, db) => write!(f, "SetBusGain({bus:?}, {db})"),
            Command::SetBusPan(bus, pan) => write!(f, "SetBusPan({bus:?}, {pan})"),
            Command::SetBusMute(bus, on) => write!(f, "SetBusMute({bus:?}, {on})"),
            Command::SetBusSolo(bus, on) => write!(f, "SetBusSolo({bus:?}, {on})"),
            Command::SetBusOutput(bus, to) => write!(f, "SetBusOutput({bus:?}, {to:?})"),
            Command::SetInsert { bus, slot, insert } => write!(
                f,
                "SetInsert {{ bus: {bus:?}, slot: {slot}, insert: {} }}",
                if insert.is_some() { "Some(..)" } else { "None" }
            ),
            Command::SetInsertBypass { bus, slot, bypass } => write!(
                f,
                "SetInsertBypass {{ bus: {bus:?}, slot: {slot}, bypass: {bypass} }}"
            ),
            Command::SetInsertParam {
                bus,
                slot,
                param,
                value,
            } => write!(
                f,
                "SetInsertParam {{ bus: {bus:?}, slot: {slot}, param: {param}, value: {value} }}"
            ),
            Command::SetSend { bus, slot, send } => {
                write!(
                    f,
                    "SetSend {{ bus: {bus:?}, slot: {slot}, send: {send:?} }}"
                )
            }
            Command::AddClip(id, clip) => write!(f, "AddClip({id:?}, {} frames)", clip.frames()),
            Command::RemoveClip(id) => write!(f, "RemoveClip({id:?})"),
            Command::Play(play) => write!(f, "Play({play:?})"),
            Command::PrepareSource(id, _) => write!(f, "PrepareSource({id:?}, ..)"),
            Command::PlaySource(play) => write!(f, "PlaySource({play:?})"),
            Command::FadeVoice {
                voice,
                gain,
                frames,
            } => write!(f, "FadeVoice({voice:?}, {gain}, {frames})"),
            Command::SetVoiceGain(v, x) => write!(f, "SetVoiceGain({v:?}, {x})"),
            Command::SetVoicePitch(v, x) => write!(f, "SetVoicePitch({v:?}, {x})"),
            Command::SetVoicePan(v, x) => write!(f, "SetVoicePan({v:?}, {x})"),
            Command::Stop(v, fade) => write!(f, "Stop({v:?}, {fade})"),
            Command::StopBus(bus, fade) => write!(f, "StopBus({bus:?}, {fade})"),
            Command::StopAll(fade) => write!(f, "StopAll({fade})"),
            Command::Capture(c) => write!(f, "Capture({} frames)", c.left.len()),
            Command::SetListener(l) => write!(f, "SetListener({l:?})"),
            Command::SetVoiceSpatial(v, s) => write!(f, "SetVoiceSpatial({v:?}, {s:?})"),
            Command::SetVoicePosition {
                voice,
                position,
                velocity,
            } => write!(f, "SetVoicePosition({voice:?}, {position:?}, {velocity:?})"),
        }
    }
}

/// Something the mixer reports back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The voice finished: its clip or source ran out, a stop's fade ended,
    /// or its clip or bus went away.
    Ended(VoiceId),
    /// The voice was cut to make room for a higher-priority one.
    Stolen(VoiceId),
    /// The voice did not start: its bus was full of higher-priority voices,
    /// or its clip, source or bus does not exist.
    Rejected(VoiceId),
}

/// Things the mixer no longer needs, handed back so the host frees them off
/// the audio thread.
pub enum Returned {
    Clip(Arc<Clip>),
    Insert(Box<dyn Insert>),
    /// A source whose voice ended (or that never played).
    Source(Box<dyn Source>),
    /// A finished (or cancelled) recording.
    Capture(Box<Capture>),
}
