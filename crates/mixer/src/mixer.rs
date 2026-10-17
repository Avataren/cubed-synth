//! The mixer core: buses, voices and the block renderer.

use std::sync::Arc;

use crate::clip::{Clip, ClipVoice};
use crate::command::{
    BusId, BusSettings, Capture, ClipId, Command, Event, Play, PlaySource, Returned, SendSettings,
    SourceId, Start, VoiceId,
};
use crate::insert::Insert;
use crate::smooth::{balance, block_coeff, db_to_gain, GainRamp, Smoothed};
use crate::source::{Source, SourceVoice, Voice};
use crate::spatial::{add_scaled, Hearing, Listener, SpatialVoice};

/// Insert slots per bus.
pub const MAX_INSERTS: usize = 8;
/// Send slots per bus.
pub const MAX_SENDS: usize = 4;

/// How long a bus keeps running its inserts after its input falls silent,
/// so reverb and delay tails ring out. After that an idle bus costs nothing.
const TAIL_SECONDS: f32 = 5.0;
/// How fast a peak meter falls.
const PEAK_FALL_DB_PER_SECOND: f32 = 24.0;
/// The window of the RMS meter.
const RMS_SECONDS: f32 = 0.3;

/// Sizes fixed when the mixer is built: nothing grows while it runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixerConfig {
    pub sample_rate: f32,
    /// The longest block rendered at once; `process` splits longer requests.
    pub max_block: usize,
    pub max_voices: usize,
    pub max_buses: usize,
    pub max_clips: usize,
    /// Prepared-source slots (`Command::PrepareSource`).
    pub max_sources: usize,
    /// Events kept between `drain_events` calls; more are dropped and counted.
    pub max_events: usize,
    /// Released objects kept between `take_returned` calls; more are freed
    /// on the audio thread and counted.
    pub max_returned: usize,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            max_block: 128,
            max_voices: 128,
            max_buses: 32,
            max_clips: 4096,
            max_sources: 16,
            max_events: 512,
            max_returned: 256,
        }
    }
}

/// A bus's output level after its fader: the peak (falling slowly) and the
/// RMS over the last ~300 ms, per channel, as linear amplitudes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BusMeter {
    pub peak: [f32; 2],
    pub rms: [f32; 2],
}

/// Counters for things that went wrong without stopping the mix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Voices playing (or waiting for their start frame) now.
    pub voices: u32,
    /// Commands dropped: unknown ids, slots out of range, or a routing that
    /// would make a loop.
    pub rejected_commands: u64,
    /// Events lost because nobody drained them.
    pub dropped_events: u64,
    /// Objects freed on the audio thread because the returned queue was full.
    pub freed_on_audio_thread: u64,
}

struct InsertSlot {
    insert: Box<dyn Insert>,
    bypass: bool,
}

struct BusSend {
    to: usize,
    gain: Smoothed,
    pre_fader: bool,
}

struct Bus {
    used: bool,
    settings: BusSettings,
    /// The fader's gain with mute and solo applied.
    gain: Smoothed,
    pan: Smoothed,
    /// Whether mute and solo let the bus be heard.
    audible: bool,
    inserts: [Option<InsertSlot>; MAX_INSERTS],
    sends: [Option<BusSend>; MAX_SENDS],
    left: Vec<f32>,
    right: Vec<f32>,
    has_input: bool,
    silent_frames: usize,
    meter: BusMeter,
    /// Mean squares behind `meter.rms`.
    mean_square: [f32; 2],
    /// The last block's peak after the fader, for buses this one ducks.
    level: f32,
    /// How far ducking has the bus down at the end of this block and at its
    /// start (linear, 1 = not at all).
    duck_gain: f32,
    duck_start: f32,
}

impl Bus {
    fn new(max_block: usize) -> Bus {
        Bus {
            used: false,
            settings: BusSettings::default(),
            gain: Smoothed::new(1.0),
            pan: Smoothed::new(0.0),
            audible: true,
            inserts: std::array::from_fn(|_| None),
            sends: std::array::from_fn(|_| None),
            left: vec![0.0; max_block],
            right: vec![0.0; max_block],
            has_input: false,
            silent_frames: 0,
            meter: BusMeter::default(),
            mean_square: [0.0; 2],
            level: 0.0,
            duck_gain: 1.0,
            duck_start: 1.0,
        }
    }

    fn target_gain(&self) -> f32 {
        if self.audible && !self.settings.mute {
            db_to_gain(self.settings.gain_db)
        } else {
            0.0
        }
    }

    /// The buses this one feeds: its output and its sends.
    fn targets(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        let output = (index != 0).then_some(self.settings.output.0 as usize);
        output
            .into_iter()
            .chain(self.sends.iter().flatten().map(|s| s.to))
    }
}

struct VoiceSlot {
    id: VoiceId,
    bus: usize,
    priority: u8,
    /// Start order, for stealing the oldest of equal priority.
    serial: u64,
    start_frame: u64,
    voice: Voice,
}

/// The mixer: apply commands between blocks, render blocks with `process`.
pub struct Mixer {
    config: MixerConfig,
    frame: u64,
    buses: Vec<Bus>,
    /// Bus processing order: every bus before the buses it feeds, so master
    /// comes last.
    order: Vec<usize>,
    clips: Vec<Option<Arc<Clip>>>,
    sources: Vec<Option<Box<dyn Source>>>,
    voices: Vec<Option<VoiceSlot>>,
    serial: u64,
    scratch: Vec<f32>,
    scratch_right: Vec<f32>,
    events: Vec<Event>,
    returned: Vec<Returned>,
    capture: Option<Box<Capture>>,
    stats: Stats,
    listener: Listener,
    /// The frame of the listener's last update.
    listener_updated_at: u64,
    // Scratch for rebuilding the order.
    indegree: Vec<u16>,
    stack: Vec<usize>,
    new_order: Vec<usize>,
}

impl Mixer {
    /// A mixer with only the master bus. All its memory is allocated here.
    pub fn new(config: MixerConfig) -> Mixer {
        let config = MixerConfig {
            sample_rate: config.sample_rate.max(1.0),
            max_block: config.max_block.max(1),
            max_buses: config.max_buses.clamp(1, u16::MAX as usize),
            ..config
        };
        let mut buses: Vec<Bus> = (0..config.max_buses)
            .map(|_| Bus::new(config.max_block))
            .collect();
        buses[0].used = true;
        let mut order = Vec::with_capacity(config.max_buses);
        order.push(0);
        Mixer {
            config,
            frame: 0,
            buses,
            order,
            clips: (0..config.max_clips).map(|_| None).collect(),
            sources: (0..config.max_sources).map(|_| None).collect(),
            voices: (0..config.max_voices).map(|_| None).collect(),
            serial: 0,
            scratch: vec![0.0; config.max_block],
            scratch_right: vec![0.0; config.max_block],
            events: Vec::with_capacity(config.max_events),
            returned: Vec::with_capacity(config.max_returned),
            capture: None,
            stats: Stats::default(),
            listener: Listener::default(),
            listener_updated_at: 0,
            indegree: Vec::with_capacity(config.max_buses),
            stack: Vec::with_capacity(config.max_buses),
            new_order: Vec::with_capacity(config.max_buses),
        }
    }

    pub fn config(&self) -> &MixerConfig {
        &self.config
    }

    /// Frames rendered so far: the mixer's clock.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The bus's meter, if the bus exists.
    pub fn meter(&self, bus: BusId) -> Option<BusMeter> {
        self.buses
            .get(bus.0 as usize)
            .filter(|b| b.used)
            .map(|b| b.meter)
    }

    /// Every bus that exists, with its meter.
    pub fn meters(&self) -> impl Iterator<Item = (BusId, BusMeter)> + '_ {
        self.buses
            .iter()
            .enumerate()
            .filter(|(_, b)| b.used)
            .map(|(i, b)| (BusId(i as u16), b.meter))
    }

    pub fn is_playing(&self, voice: VoiceId) -> bool {
        self.find_voice(voice).is_some()
    }

    /// Hands every event since the last call to `f`, oldest first.
    pub fn drain_events(&mut self, mut f: impl FnMut(Event)) {
        for event in self.events.drain(..) {
            f(event);
        }
    }

    /// Hands back everything the mixer let go of, to be freed off the audio
    /// thread.
    pub fn take_returned(&mut self, mut f: impl FnMut(Returned)) {
        for item in self.returned.drain(..) {
            f(item);
        }
    }

    /// Applies one change. Never allocates; never panics on bad input.
    pub fn apply(&mut self, command: Command) {
        match command {
            Command::SetBus(bus, settings) => self.set_bus(bus, settings),
            Command::RemoveBus(bus) => self.remove_bus(bus),
            Command::SetBusGain(bus, db) => self.update_bus(bus, |s| s.gain_db = db),
            Command::SetBusPan(bus, pan) => self.update_bus(bus, |s| s.pan = pan),
            Command::SetBusMute(bus, mute) => self.update_bus(bus, |s| s.mute = mute),
            Command::SetBusSolo(bus, solo) => self.update_bus(bus, |s| s.solo = solo),
            Command::SetBusOutput(bus, output) => self.update_bus(bus, |s| s.output = output),
            Command::SetInsert { bus, slot, insert } => self.set_insert(bus, slot, insert),
            Command::SetInsertBypass { bus, slot, bypass } => {
                match self
                    .used_bus(bus)
                    .and_then(|b| self.buses[b].inserts.get_mut(slot as usize))
                {
                    Some(Some(insert)) => {
                        if insert.bypass && !bypass {
                            insert.insert.reset();
                        }
                        insert.bypass = bypass;
                    }
                    _ => self.stats.rejected_commands += 1,
                }
            }
            Command::SetInsertParam {
                bus,
                slot,
                param,
                value,
            } => match self
                .used_bus(bus)
                .and_then(|b| self.buses[b].inserts.get_mut(slot as usize))
            {
                Some(Some(insert)) => insert.insert.set_param(param as usize, value),
                _ => self.stats.rejected_commands += 1,
            },
            Command::SetSend { bus, slot, send } => self.set_send(bus, slot, send),
            Command::AddClip(id, clip) => self.add_clip(id, clip),
            Command::RemoveClip(id) => {
                let Some(old) = self.clips.get_mut(id.0 as usize).and_then(Option::take) else {
                    self.stats.rejected_commands += 1;
                    return;
                };
                self.end_voices(|v| v.voice.clip() == Some(id.0 as usize));
                self.give_back(Returned::Clip(old));
            }
            Command::Play(play) => self.play(play),
            Command::PrepareSource(id, source) => {
                let Some(slot) = self.sources.get_mut(id.0 as usize) else {
                    self.stats.rejected_commands += 1;
                    self.give_back(Returned::Source(source));
                    return;
                };
                if let Some(old) = slot.replace(source) {
                    self.give_back(Returned::Source(old));
                }
            }
            Command::PlaySource(play) => self.play_source(play),
            Command::SourceEvent { voice, event } => self.with_voice(voice, |v| {
                if let Some(source) = v.voice.source_mut() {
                    source.event(event);
                }
            }),
            Command::SetVoiceGain(id, gain) => self.with_voice(id, |v| {
                *v.voice.ramp() = None;
                v.voice.gain().set(gain.max(0.0));
            }),
            Command::FadeVoice {
                voice,
                gain,
                frames,
            } => self.with_voice(voice, |v| {
                if frames == 0 {
                    *v.voice.ramp() = None;
                    v.voice.gain().set(gain.max(0.0));
                } else {
                    *v.voice.ramp() = Some(GainRamp::new(gain, frames));
                }
            }),
            Command::SetVoicePitch(id, pitch) => self.with_voice(id, |v| v.voice.set_pitch(pitch)),
            Command::SetVoicePan(id, pan) => {
                self.with_voice(id, |v| v.voice.pan().set(pan.clamp(-1.0, 1.0)))
            }
            Command::Stop(id, fade) => self.with_voice(id, |v| v.voice.stop(fade)),
            Command::StopBus(bus, fade) => {
                let Some(b) = self.used_bus(bus) else {
                    self.stats.rejected_commands += 1;
                    return;
                };
                for slot in self.voices.iter_mut().flatten().filter(|v| v.bus == b) {
                    slot.voice.stop(fade);
                }
            }
            Command::StopAll(fade) => {
                for slot in self.voices.iter_mut().flatten() {
                    slot.voice.stop(fade);
                }
            }
            Command::SetListener(listener) => {
                self.listener = listener;
                self.listener_updated_at = self.frame;
            }
            Command::SetVoiceSpatial(id, spatial) => {
                let frame = self.frame;
                self.with_voice(id, |v| match (spatial, v.voice.spatial()) {
                    (Some(params), Some(state)) => {
                        state.params = params;
                        state.updated_at = frame;
                    }
                    (Some(params), state) => *state = Some(SpatialVoice::new(params, frame)),
                    (None, state) => *state = None,
                });
            }
            Command::SetVoicePosition {
                voice,
                position,
                velocity,
            } => {
                let frame = self.frame;
                self.with_voice(voice, |v| {
                    if let Some(state) = v.voice.spatial() {
                        state.params.position = position;
                        state.params.velocity = velocity;
                        state.updated_at = frame;
                    }
                });
            }
            Command::Capture(mut capture) => {
                capture.filled = 0;
                capture.start_frame = self.frame;
                if let Some(old) = self.capture.replace(capture) {
                    self.give_back(Returned::Capture(old));
                }
            }
        }
    }

    /// Renders the next frames of the mix into `left` and `right`.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let total = left.len().min(right.len());
        let mut done = 0;
        while done < total {
            let n = (total - done).min(self.config.max_block);
            self.render_block(&mut left[done..done + n], &mut right[done..done + n]);
            done += n;
        }
    }

    // --- Buses ---------------------------------------------------------

    /// The bus's index if it exists.
    fn used_bus(&self, bus: BusId) -> Option<usize> {
        let index = bus.0 as usize;
        self.buses.get(index).filter(|b| b.used).map(|_| index)
    }

    fn set_bus(&mut self, bus: BusId, settings: BusSettings) {
        let index = bus.0 as usize;
        let Some(slot) = self.buses.get_mut(index) else {
            self.stats.rejected_commands += 1;
            return;
        };
        let was_used = slot.used;
        let old = slot.settings;
        slot.used = true;
        slot.settings = settings;
        let duck_ok = settings
            .duck
            .is_none_or(|d| d.key.0 as usize != index && (d.key.0 as usize) < self.buses.len());
        if !duck_ok || !self.output_is_valid(index) || !self.rebuild_order() {
            let slot = &mut self.buses[index];
            slot.used = was_used;
            slot.settings = old;
            self.rebuild_order();
            self.stats.rejected_commands += 1;
            return;
        }
        let slot = &mut self.buses[index];
        if !was_used {
            slot.left.fill(0.0);
            slot.right.fill(0.0);
            slot.silent_frames = 0;
            slot.meter = BusMeter::default();
            slot.mean_square = [0.0; 2];
            slot.level = 0.0;
            slot.duck_gain = 1.0;
            slot.duck_start = 1.0;
            slot.pan.snap(settings.pan.clamp(-1.0, 1.0));
        } else {
            slot.pan.set(settings.pan.clamp(-1.0, 1.0));
        }
        self.update_audible();
        if !was_used {
            let slot = &mut self.buses[index];
            let gain = slot.target_gain();
            slot.gain.snap(gain);
        }
    }

    fn update_bus(&mut self, bus: BusId, change: impl FnOnce(&mut BusSettings)) {
        let Some(index) = self.used_bus(bus) else {
            self.stats.rejected_commands += 1;
            return;
        };
        let mut settings = self.buses[index].settings;
        change(&mut settings);
        self.set_bus(bus, settings);
    }

    /// Whether the bus's output names an existing bus other than itself.
    fn output_is_valid(&self, index: usize) -> bool {
        if index == 0 {
            return true;
        }
        let output = self.buses[index].settings.output.0 as usize;
        output != index && self.buses.get(output).is_some_and(|b| b.used)
    }

    fn remove_bus(&mut self, bus: BusId) {
        let Some(index) = self.used_bus(bus).filter(|&i| i != 0) else {
            self.stats.rejected_commands += 1;
            return;
        };
        self.end_voices(|v| v.bus == index);
        for slot in 0..MAX_INSERTS {
            if let Some(insert) = self.buses[index].inserts[slot].take() {
                self.give_back(Returned::Insert(insert.insert));
            }
        }
        let removed = &mut self.buses[index];
        removed.used = false;
        removed.sends = std::array::from_fn(|_| None);
        removed.settings = BusSettings::default();
        for other in self.buses.iter_mut().filter(|b| b.used) {
            if other.settings.output.0 as usize == index {
                other.settings.output = BusId::MASTER;
            }
            for send in other.sends.iter_mut() {
                if send.as_ref().is_some_and(|s| s.to == index) {
                    *send = None;
                }
            }
        }
        self.rebuild_order();
        self.update_audible();
    }

    fn set_insert(&mut self, bus: BusId, slot: u8, insert: Option<Box<dyn Insert>>) {
        let Some(index) = self.used_bus(bus).filter(|_| (slot as usize) < MAX_INSERTS) else {
            self.stats.rejected_commands += 1;
            if let Some(insert) = insert {
                self.give_back(Returned::Insert(insert));
            }
            return;
        };
        let new = insert.map(|insert| InsertSlot {
            insert,
            bypass: false,
        });
        if let Some(old) = std::mem::replace(&mut self.buses[index].inserts[slot as usize], new) {
            self.give_back(Returned::Insert(old.insert));
        }
        // An insert added to an idle bus starts from silence.
        self.buses[index].silent_frames = 0;
    }

    fn set_send(&mut self, bus: BusId, slot: u8, send: Option<SendSettings>) {
        let Some(index) = self.used_bus(bus).filter(|_| (slot as usize) < MAX_SENDS) else {
            self.stats.rejected_commands += 1;
            return;
        };
        let new = match send {
            None => None,
            Some(settings) => {
                let Some(to) = self.used_bus(settings.to).filter(|&to| to != index) else {
                    self.stats.rejected_commands += 1;
                    return;
                };
                let level = db_to_gain(settings.level_db);
                let gain = match &self.buses[index].sends[slot as usize] {
                    // Retargeting the same bus glides; a new send starts at its level.
                    Some(old) if old.to == to => {
                        let mut gain = old.gain;
                        gain.set(level);
                        gain
                    }
                    _ => Smoothed::new(level),
                };
                Some(BusSend {
                    to,
                    gain,
                    pre_fader: settings.pre_fader,
                })
            }
        };
        let old = std::mem::replace(&mut self.buses[index].sends[slot as usize], new);
        if !self.rebuild_order() {
            self.buses[index].sends[slot as usize] = old;
            self.rebuild_order();
            self.stats.rejected_commands += 1;
        }
    }

    /// Recomputes the processing order; false (and the old order kept) if
    /// the routing has a loop.
    fn rebuild_order(&mut self) -> bool {
        let n = self.buses.len();
        self.indegree.clear();
        self.indegree.resize(n, 0);
        for (i, bus) in self.buses.iter().enumerate().filter(|(_, b)| b.used) {
            for target in bus.targets(i) {
                if let Some(d) = self.indegree.get_mut(target) {
                    *d += 1;
                }
            }
        }
        self.stack.clear();
        self.new_order.clear();
        for (i, bus) in self.buses.iter().enumerate() {
            if bus.used && self.indegree[i] == 0 {
                self.stack.push(i);
            }
        }
        while let Some(i) = self.stack.pop() {
            self.new_order.push(i);
            for target in self.buses[i].targets(i) {
                if let Some(d) = self.indegree.get_mut(target) {
                    *d -= 1;
                    if *d == 0 {
                        self.stack.push(target);
                    }
                }
            }
        }
        let used = self.buses.iter().filter(|b| b.used).count();
        if self.new_order.len() != used {
            return false;
        }
        std::mem::swap(&mut self.order, &mut self.new_order);
        true
    }

    /// Applies mute and solo: with any bus soloed, only soloed buses, the
    /// buses feeding them and the buses they feed are heard.
    fn update_audible(&mut self) {
        let any_solo = self.buses.iter().any(|b| b.used && b.settings.solo);
        if !any_solo {
            for bus in self.buses.iter_mut() {
                bus.audible = true;
            }
        } else {
            for bus in self.buses.iter_mut() {
                bus.audible = false;
            }
            let limit = self.buses.len();
            for i in 0..limit {
                if !(self.buses[i].used && self.buses[i].settings.solo) {
                    continue;
                }
                // The soloed bus and its path to master.
                let mut b = i;
                for _ in 0..limit {
                    self.buses[b].audible = true;
                    if b == 0 {
                        break;
                    }
                    b = self.buses[b].settings.output.0 as usize;
                }
            }
            for i in 0..limit {
                if !self.buses[i].used {
                    continue;
                }
                // Buses whose path to master passes a soloed bus.
                let mut b = i;
                for _ in 0..limit {
                    if self.buses[b].settings.solo {
                        self.buses[i].audible = true;
                        break;
                    }
                    if b == 0 {
                        break;
                    }
                    b = self.buses[b].settings.output.0 as usize;
                }
            }
        }
        for bus in self.buses.iter_mut().filter(|b| b.used) {
            let target = bus.target_gain();
            bus.gain.set(target);
        }
    }

    // --- Clips and voices ----------------------------------------------

    fn add_clip(&mut self, id: ClipId, clip: Arc<Clip>) {
        let index = id.0 as usize;
        if index >= self.clips.len() {
            self.stats.rejected_commands += 1;
            self.give_back(Returned::Clip(clip));
            return;
        }
        if let Some(old) = self.clips[index].replace(clip) {
            self.end_voices(|v| v.voice.clip() == Some(index));
            self.give_back(Returned::Clip(old));
        }
    }

    fn play(&mut self, play: Play) {
        let clip = self
            .clips
            .get(play.clip.0 as usize)
            .and_then(|c| c.as_ref())
            .map(|c| c.sample_rate());
        let (Some(clip_rate), Some(bus)) = (clip, self.used_bus(play.bus)) else {
            self.stats.rejected_commands += 1;
            self.emit(Event::Rejected(play.voice));
            return;
        };
        let Some(slot) = self.allocate_voice(bus, play.priority, play.voice) else {
            return;
        };
        let start_frame = self.start_frame(play.start);
        let voice = ClipVoice::new(
            play.clip.0 as usize,
            clip_rate,
            self.config.sample_rate,
            play.gain,
            play.pitch,
            play.pan,
            play.looped,
            play.spatial.map(|s| (s, start_frame)),
        );
        self.place_voice(
            slot,
            play.voice,
            bus,
            play.priority,
            start_frame,
            Voice::Clip(voice),
        );
    }

    fn play_source(&mut self, play: PlaySource) {
        let ready = self
            .sources
            .get(play.source.0 as usize)
            .is_some_and(|s| s.is_some());
        let (true, Some(bus)) = (ready, self.used_bus(play.bus)) else {
            self.stats.rejected_commands += 1;
            self.emit(Event::Rejected(play.voice));
            return;
        };
        let slot = self.allocate_voice(bus, play.priority, play.voice);
        let source = self.sources[play.source.0 as usize]
            .take()
            .expect("checked");
        let Some(slot) = slot else {
            // It was prepared for this play: let it go.
            self.give_back(Returned::Source(source));
            return;
        };
        let start_frame = self.start_frame(play.start);
        let voice = SourceVoice::new(
            source,
            play.gain,
            play.pan,
            play.spatial.map(|s| (s, start_frame)),
        );
        self.place_voice(
            slot,
            play.voice,
            bus,
            play.priority,
            start_frame,
            Voice::Source(voice),
        );
    }

    /// Whether a prepared source is waiting in the slot.
    pub fn is_prepared(&self, source: SourceId) -> bool {
        self.sources
            .get(source.0 as usize)
            .is_some_and(|s| s.is_some())
    }

    fn start_frame(&self, start: Start) -> u64 {
        match start {
            Start::Now => self.frame,
            Start::AtFrame(frame) => frame.max(self.frame),
        }
    }

    /// A voice slot for a new voice on `bus`: a free one, or one stolen from
    /// a voice of no higher priority (the bus's own when it is at its
    /// limit). `None` (and the new voice rejected) when there is none.
    fn allocate_voice(&mut self, bus: usize, priority: u8, voice: VoiceId) -> Option<usize> {
        let limit = self.buses[bus].settings.voice_limit as usize;
        let mut on_bus = 0;
        let mut free = None;
        let mut weakest_on_bus: Option<(usize, u8, u64)> = None;
        let mut weakest: Option<(usize, u8, u64)> = None;
        for (i, slot) in self.voices.iter().enumerate() {
            match slot {
                None => {
                    free.get_or_insert(i);
                }
                Some(v) => {
                    let key = (i, v.priority, v.serial);
                    let weaker = |w: &Option<(usize, u8, u64)>| {
                        w.is_none_or(|(_, p, s)| (v.priority, v.serial) < (p, s))
                    };
                    if v.bus == bus {
                        on_bus += 1;
                        if weaker(&weakest_on_bus) {
                            weakest_on_bus = Some(key);
                        }
                    }
                    if weaker(&weakest) {
                        weakest = Some(key);
                    }
                }
            }
        }
        let slot = if on_bus >= limit {
            weakest_on_bus.filter(|&(_, p, _)| p <= priority)
        } else if free.is_some() {
            free.map(|i| (i, 0, 0))
        } else {
            weakest.filter(|&(_, p, _)| p <= priority)
        };
        let Some((slot, _, _)) = slot else {
            self.emit(Event::Rejected(voice));
            return None;
        };
        if let Some(stolen) = self.voices[slot].take() {
            let id = stolen.id;
            retire(&mut self.returned, &mut self.stats, stolen);
            self.emit(Event::Stolen(id));
        }
        Some(slot)
    }

    fn place_voice(
        &mut self,
        slot: usize,
        id: VoiceId,
        bus: usize,
        priority: u8,
        start_frame: u64,
        voice: Voice,
    ) {
        self.serial += 1;
        self.voices[slot] = Some(VoiceSlot {
            id,
            bus,
            priority,
            serial: self.serial,
            start_frame,
            voice,
        });
    }

    fn find_voice(&self, id: VoiceId) -> Option<usize> {
        self.voices
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|v| v.id == id))
    }

    fn with_voice(&mut self, id: VoiceId, f: impl FnOnce(&mut VoiceSlot)) {
        match self.find_voice(id) {
            Some(i) => f(self.voices[i].as_mut().expect("found above")),
            None => self.stats.rejected_commands += 1,
        }
    }

    /// Ends matching voices at once, reporting each.
    fn end_voices(&mut self, matches: impl Fn(&VoiceSlot) -> bool) {
        for i in 0..self.voices.len() {
            if self.voices[i].as_ref().is_some_and(&matches) {
                let voice = self.voices[i].take().expect("checked");
                let id = voice.id;
                retire(&mut self.returned, &mut self.stats, voice);
                self.emit(Event::Ended(id));
            }
        }
    }

    fn emit(&mut self, event: Event) {
        push_event(&mut self.events, &mut self.stats, event);
    }

    fn give_back(&mut self, item: Returned) {
        give_back(&mut self.returned, &mut self.stats, item);
    }

    // --- Rendering -----------------------------------------------------

    fn render_block(&mut self, out_left: &mut [f32], out_right: &mut [f32]) {
        let n = out_left.len();
        let sample_rate = self.config.sample_rate;
        let coeff = block_coeff(n, sample_rate);
        let since = self.frame.saturating_sub(self.listener_updated_at) as f32 / sample_rate;
        let hearing = Hearing {
            listener: &self.listener,
            ear: add_scaled(
                self.listener.position,
                self.listener.velocity,
                since.min(0.1),
            ),
            frame: self.frame,
            sample_rate,
        };
        let Mixer {
            buses,
            order,
            clips,
            voices,
            scratch,
            scratch_right,
            events,
            returned,
            stats,
            frame,
            ..
        } = self;

        for bus in buses.iter_mut().filter(|b| b.used) {
            bus.left[..n].fill(0.0);
            bus.right[..n].fill(0.0);
            bus.has_input = false;
        }

        // Voices into their buses.
        let mut playing = 0;
        for slot in voices.iter_mut() {
            let Some(v) = slot else { continue };
            let offset = v.start_frame.saturating_sub(*frame) as usize;
            if offset >= n {
                playing += 1;
                continue;
            }
            let bus = &mut buses[v.bus];
            let alive = match &mut v.voice {
                Voice::Clip(voice) => match clips.get(voice.clip).and_then(|c| c.as_deref()) {
                    Some(clip) => voice.render(
                        clip,
                        &mut bus.left[offset..n],
                        &mut bus.right[offset..n],
                        &mut scratch[..n - offset],
                        coeff,
                        &hearing,
                    ),
                    // Unreachable: a clip's voices end when it goes.
                    None => false,
                },
                Voice::Source(voice) => voice.render(
                    &mut bus.left[offset..n],
                    &mut bus.right[offset..n],
                    (&mut scratch[..n - offset], &mut scratch_right[..n - offset]),
                    coeff,
                    &hearing,
                ),
            };
            bus.has_input = true;
            if alive {
                playing += 1;
            } else {
                let ended = slot.take().expect("matched above");
                let id = ended.id;
                retire(returned, stats, ended);
                push_event(events, stats, Event::Ended(id));
            }
        }
        stats.voices = playing;

        // Ducking follows the key bus's level in the last block, so it does
        // not depend on which of the two renders first.
        for b in 0..buses.len() {
            let Some(duck) = buses[b].settings.duck.filter(|_| buses[b].used) else {
                buses[b].duck_gain = 1.0;
                continue;
            };
            let key_level = buses
                .get(duck.key.0 as usize)
                .filter(|k| k.used)
                .map_or(0.0, |k| k.level);
            let target = if key_level > db_to_gain(duck.threshold_db) {
                db_to_gain(-duck.depth_db.max(0.0))
            } else {
                1.0
            };
            let bus = &mut buses[b];
            let ms = if target < bus.duck_gain {
                duck.attack_ms
            } else {
                duck.release_ms
            };
            let keep = (-(n as f32) / (ms.max(0.1) * 0.001 * sample_rate)).exp();
            bus.duck_start = bus.duck_gain;
            bus.duck_gain = target + (bus.duck_gain - target) * keep;
        }

        // Buses, each before the buses it feeds.
        let tail_frames = (TAIL_SECONDS * sample_rate) as usize;
        let peak_fall = db_to_gain(-PEAK_FALL_DB_PER_SECOND * n as f32 / sample_rate);
        let rms_keep = (-(n as f32) / (RMS_SECONDS * sample_rate)).exp();
        for &b in order.iter() {
            let bus = &mut buses[b];
            if bus.has_input {
                bus.silent_frames = 0;
            } else {
                bus.silent_frames = bus.silent_frames.saturating_add(n);
            }
            let idle = bus.silent_frames > tail_frames;
            let (gain_start, gain_end) = bus.gain.advance(coeff);
            let (gain_start, gain_end) = if bus.settings.duck.is_some() {
                (gain_start * bus.duck_start, gain_end * bus.duck_gain)
            } else {
                (gain_start, gain_end)
            };
            let (pan_start, pan_end) = bus.pan.advance(coeff);
            let mut send_ramps = [(0.0f32, 0.0f32); MAX_SENDS];
            for (ramp, send) in send_ramps.iter_mut().zip(bus.sends.iter_mut()) {
                if let Some(send) = send {
                    *ramp = send.gain.advance(coeff);
                }
            }

            if !idle {
                for slot in bus.inserts.iter_mut().flatten() {
                    if !slot.bypass {
                        slot.insert.process(&mut bus.left[..n], &mut bus.right[..n]);
                    }
                }
                send(buses, b, n, &send_ramps, true);
                let bus = &mut buses[b];
                fader(
                    &mut bus.left[..n],
                    &mut bus.right[..n],
                    (gain_start, gain_end),
                    (pan_start, pan_end),
                );
                send(buses, b, n, &send_ramps, false);
            }

            let bus = &mut buses[b];
            meter(bus, n, peak_fall, rms_keep, idle);

            if b == 0 {
                out_left.copy_from_slice(&bus.left[..n]);
                out_right.copy_from_slice(&bus.right[..n]);
            } else if !idle {
                let to = bus.settings.output.0 as usize;
                if let Ok([from, into]) = buses.get_disjoint_mut([b, to]) {
                    mix_into(&from.left[..n], &mut into.left[..n], 1.0, 1.0);
                    mix_into(&from.right[..n], &mut into.right[..n], 1.0, 1.0);
                    into.has_input = true;
                }
            }
        }

        if let Some(capture) = self.capture.as_mut() {
            let room = capture.left.len().min(capture.right.len()) - capture.filled;
            let k = room.min(n);
            let at = capture.filled;
            capture.left[at..at + k].copy_from_slice(&out_left[..k]);
            capture.right[at..at + k].copy_from_slice(&out_right[..k]);
            capture.filled += k;
            if capture.filled >= capture.left.len().min(capture.right.len()) {
                let done = self.capture.take().expect("checked");
                self.give_back(Returned::Capture(done));
            }
        }
        self.frame += n as u64;
    }
}

fn give_back(returned: &mut Vec<Returned>, stats: &mut Stats, item: Returned) {
    if returned.len() < returned.capacity() {
        returned.push(item);
    } else {
        stats.freed_on_audio_thread += 1;
        drop(item);
    }
}

/// Lets go of an ended voice: a source goes back to the host.
fn retire(returned: &mut Vec<Returned>, stats: &mut Stats, mut slot: VoiceSlot) {
    if let Voice::Source(voice) = &mut slot.voice {
        if let Some(source) = voice.source.take() {
            give_back(returned, stats, Returned::Source(source));
        }
    }
}

fn push_event(events: &mut Vec<Event>, stats: &mut Stats, event: Event) {
    if events.len() < events.capacity() {
        events.push(event);
    } else {
        stats.dropped_events += 1;
    }
}

/// Adds the bus's pre- or post-fader sends into their targets.
fn send(buses: &mut [Bus], b: usize, n: usize, ramps: &[(f32, f32); MAX_SENDS], pre_fader: bool) {
    for s in 0..MAX_SENDS {
        let Some((to, pre)) = buses[b].sends[s].as_ref().map(|s| (s.to, s.pre_fader)) else {
            continue;
        };
        if pre != pre_fader {
            continue;
        }
        let (g0, g1) = ramps[s];
        if g0 == 0.0 && g1 == 0.0 {
            continue;
        }
        if let Ok([from, into]) = buses.get_disjoint_mut([b, to]) {
            mix_into(&from.left[..n], &mut into.left[..n], g0, g1);
            mix_into(&from.right[..n], &mut into.right[..n], g0, g1);
            into.has_input = true;
        }
    }
}

/// `into += from * gain`, the gain ramping from `g0` to `g1`.
fn mix_into(from: &[f32], into: &mut [f32], g0: f32, g1: f32) {
    if g0 == 1.0 && g1 == 1.0 {
        for (o, i) in into.iter_mut().zip(from) {
            *o += i;
        }
        return;
    }
    let step = (g1 - g0) / from.len() as f32;
    for (k, (o, i)) in into.iter_mut().zip(from).enumerate() {
        *o += i * (g0 + step * k as f32);
    }
}

/// The fader: gain and balance, each ramping across the block.
fn fader(left: &mut [f32], right: &mut [f32], gain: (f32, f32), pan: (f32, f32)) {
    let (l0, r0) = balance(pan.0);
    let (l1, r1) = balance(pan.1);
    let (a0, a1) = (gain.0 * l0, gain.1 * l1);
    let (b0, b1) = (gain.0 * r0, gain.1 * r1);
    if a0 == 1.0 && a1 == 1.0 && b0 == 1.0 && b1 == 1.0 {
        return;
    }
    let n = left.len() as f32;
    let (step_l, step_r) = ((a1 - a0) / n, (b1 - b0) / n);
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        *l *= a0 + step_l * i as f32;
        *r *= b0 + step_r * i as f32;
    }
}

fn meter(bus: &mut Bus, n: usize, peak_fall: f32, rms_keep: f32, idle: bool) {
    bus.level = 0.0;
    for (c, buffer) in [&bus.left[..n], &bus.right[..n]].into_iter().enumerate() {
        let (peak, sum) = if idle {
            (0.0, 0.0)
        } else {
            buffer
                .iter()
                .fold((0f32, 0f32), |(p, s), &x| (p.max(x.abs()), s + x * x))
        };
        bus.level = bus.level.max(peak);
        bus.meter.peak[c] = peak.max(bus.meter.peak[c] * peak_fall);
        let ms = sum / n as f32;
        bus.mean_square[c] = bus.mean_square[c] * rms_keep + ms * (1.0 - rms_keep);
        bus.meter.rms[c] = bus.mean_square[c].sqrt();
    }
}
