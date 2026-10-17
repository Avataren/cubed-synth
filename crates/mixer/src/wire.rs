//! A byte encoding of [`Command`]s and [`Event`]s, for hosts whose game and
//! audio thread only share bytes: the browser's MessagePort between a game
//! and its AudioWorklet.
//!
//! Each message is a tag byte and fixed little-endian fields. Commands that
//! carry objects (clips, inserts, sources, recordings) have no encoding; a
//! host sends their data its own way (the worklet takes clip PCM and song
//! files through its own exports). Decoding never allocates.

use crate::command::{
    BusId, BusSettings, ClipId, Command, Duck, Event, Play, PlaySource, SendSettings, SourceId,
    Start, VoiceId,
};
use crate::source::SourceEvent;
use crate::spatial::{Listener, Rolloff, Spatial, SpatialMode};

mod tag {
    pub const SET_BUS: u8 = 1;
    pub const REMOVE_BUS: u8 = 2;
    pub const BUS_GAIN: u8 = 3;
    pub const BUS_PAN: u8 = 4;
    pub const BUS_MUTE: u8 = 5;
    pub const BUS_SOLO: u8 = 6;
    pub const BUS_OUTPUT: u8 = 7;
    pub const SET_SEND: u8 = 8;
    pub const INSERT_BYPASS: u8 = 9;
    pub const REMOVE_CLIP: u8 = 10;
    pub const PLAY: u8 = 11;
    pub const VOICE_GAIN: u8 = 12;
    pub const VOICE_PITCH: u8 = 13;
    pub const VOICE_PAN: u8 = 14;
    pub const STOP: u8 = 15;
    pub const STOP_BUS: u8 = 16;
    pub const STOP_ALL: u8 = 17;
    pub const SET_LISTENER: u8 = 18;
    pub const VOICE_SPATIAL: u8 = 19;
    pub const VOICE_POSITION: u8 = 20;
    pub const PLAY_SOURCE: u8 = 21;
    pub const FADE_VOICE: u8 = 22;
    pub const INSERT_PARAM: u8 = 23;
    pub const SOURCE_EVENT: u8 = 24;

    pub const ENDED: u8 = 1;
    pub const STOLEN: u8 = 2;
    pub const REJECTED: u8 = 3;
}

/// Bytes per encoded event.
pub const EVENT_SIZE: usize = 9;

/// Appends `command` to `out`. Returns false (and writes nothing) for the
/// commands that carry objects: `SetInsert`, `AddClip`, `PrepareSource` and
/// `Capture`.
pub fn encode(command: &Command, out: &mut Vec<u8>) -> bool {
    let mut w = Writer(out);
    match command {
        Command::SetBus(bus, s) => {
            w.u8(tag::SET_BUS);
            w.u16(bus.0);
            w.u16(s.output.0);
            w.f32(s.gain_db);
            w.f32(s.pan);
            w.u8(s.mute as u8 | (s.solo as u8) << 1);
            w.u16(s.voice_limit);
            match &s.duck {
                None => w.u8(0),
                Some(d) => {
                    w.u8(1);
                    w.u16(d.key.0);
                    w.f32(d.depth_db);
                    w.f32(d.threshold_db);
                    w.f32(d.attack_ms);
                    w.f32(d.release_ms);
                }
            }
        }
        Command::RemoveBus(bus) => {
            w.u8(tag::REMOVE_BUS);
            w.u16(bus.0);
        }
        Command::SetBusGain(bus, db) => w.bus_f32(tag::BUS_GAIN, *bus, *db),
        Command::SetBusPan(bus, pan) => w.bus_f32(tag::BUS_PAN, *bus, *pan),
        Command::SetBusMute(bus, on) => w.bus_u8(tag::BUS_MUTE, *bus, *on as u8),
        Command::SetBusSolo(bus, on) => w.bus_u8(tag::BUS_SOLO, *bus, *on as u8),
        Command::SetBusOutput(bus, output) => {
            w.u8(tag::BUS_OUTPUT);
            w.u16(bus.0);
            w.u16(output.0);
        }
        Command::SetSend { bus, slot, send } => {
            w.u8(tag::SET_SEND);
            w.u16(bus.0);
            w.u8(*slot);
            let s = send.unwrap_or(SendSettings {
                to: BusId::MASTER,
                level_db: 0.0,
                pre_fader: false,
            });
            w.u8(send.is_some() as u8);
            w.u16(s.to.0);
            w.f32(s.level_db);
            w.u8(s.pre_fader as u8);
        }
        Command::SetInsertBypass { bus, slot, bypass } => {
            w.u8(tag::INSERT_BYPASS);
            w.u16(bus.0);
            w.u8(*slot);
            w.u8(*bypass as u8);
        }
        Command::SetInsertParam {
            bus,
            slot,
            param,
            value,
        } => {
            w.u8(tag::INSERT_PARAM);
            w.u16(bus.0);
            w.u8(*slot);
            w.u8(*param);
            w.f32(*value);
        }
        Command::RemoveClip(clip) => {
            w.u8(tag::REMOVE_CLIP);
            w.u32(clip.0);
        }
        Command::Play(p) => {
            w.u8(tag::PLAY);
            w.u64(p.voice.0);
            w.u32(p.clip.0);
            w.u16(p.bus.0);
            w.f32(p.gain);
            w.f32(p.pitch);
            w.f32(p.pan);
            w.u8(p.looped as u8);
            w.u8(p.priority);
            w.start(p.start);
            w.spatial(p.spatial.as_ref());
        }
        Command::PlaySource(p) => {
            w.u8(tag::PLAY_SOURCE);
            w.u64(p.voice.0);
            w.u32(p.source.0);
            w.u16(p.bus.0);
            w.f32(p.gain);
            w.f32(p.pan);
            w.u8(p.priority);
            w.start(p.start);
            w.spatial(p.spatial.as_ref());
        }
        Command::SourceEvent { voice, event } => {
            w.u8(tag::SOURCE_EVENT);
            w.u64(voice.0);
            match *event {
                SourceEvent::NoteOn {
                    note,
                    velocity,
                    duration,
                } => {
                    w.u8(0);
                    w.u8(note);
                    w.f32(velocity);
                    w.u32(duration);
                }
                SourceEvent::NoteOff { note } => {
                    w.u8(1);
                    w.u8(note);
                    w.f32(0.0);
                    w.u32(0);
                }
                SourceEvent::AllNotesOff => {
                    w.u8(2);
                    w.u8(0);
                    w.f32(0.0);
                    w.u32(0);
                }
            }
        }
        Command::FadeVoice {
            voice,
            gain,
            frames,
        } => {
            w.u8(tag::FADE_VOICE);
            w.u64(voice.0);
            w.f32(*gain);
            w.u32(*frames);
        }
        Command::SetListener(l) => {
            w.u8(tag::SET_LISTENER);
            w.vec3(l.position);
            w.vec3(l.forward);
            w.vec3(l.up);
            w.vec3(l.velocity);
            w.f32(l.pan_width);
            w.f32(l.speed_of_sound);
            w.u8(l.doppler as u8);
        }
        Command::SetVoiceSpatial(voice, spatial) => {
            w.u8(tag::VOICE_SPATIAL);
            w.u64(voice.0);
            w.spatial(spatial.as_ref());
        }
        Command::SetVoicePosition {
            voice,
            position,
            velocity,
        } => {
            w.u8(tag::VOICE_POSITION);
            w.u64(voice.0);
            w.vec3(*position);
            w.vec3(*velocity);
        }
        Command::SetVoiceGain(v, x) => w.voice_f32(tag::VOICE_GAIN, *v, *x),
        Command::SetVoicePitch(v, x) => w.voice_f32(tag::VOICE_PITCH, *v, *x),
        Command::SetVoicePan(v, x) => w.voice_f32(tag::VOICE_PAN, *v, *x),
        Command::Stop(v, fade) => {
            w.u8(tag::STOP);
            w.u64(v.0);
            w.u32(*fade);
        }
        Command::StopBus(bus, fade) => {
            w.u8(tag::STOP_BUS);
            w.u16(bus.0);
            w.u32(*fade);
        }
        Command::StopAll(fade) => {
            w.u8(tag::STOP_ALL);
            w.u32(*fade);
        }
        Command::SetInsert { .. }
        | Command::AddClip(..)
        | Command::PrepareSource(..)
        | Command::Capture(_) => return false,
    }
    true
}

/// Decodes the commands in `bytes`, in order. Stops at the first message it
/// cannot read (an unknown tag or a truncated one) and yields an error then.
pub fn decode(bytes: &[u8]) -> impl Iterator<Item = Result<Command, WireError>> + '_ {
    let mut r = Reader { bytes, at: 0 };
    let mut failed = false;
    std::iter::from_fn(move || {
        if failed || r.at >= r.bytes.len() {
            return None;
        }
        let command = r.command();
        failed = command.is_err();
        Some(command)
    })
}

/// Appends an event to `out`.
pub fn encode_event(event: Event, out: &mut Vec<u8>) {
    let (tag, voice) = match event {
        Event::Ended(v) => (tag::ENDED, v),
        Event::Stolen(v) => (tag::STOLEN, v),
        Event::Rejected(v) => (tag::REJECTED, v),
    };
    out.push(tag);
    out.extend_from_slice(&voice.0.to_le_bytes());
}

/// Decodes events written by [`encode_event`]; unknown or partial ones are skipped.
pub fn decode_events(bytes: &[u8]) -> impl Iterator<Item = Event> + '_ {
    bytes.as_chunks::<EVENT_SIZE>().0.iter().filter_map(|e| {
        let voice = VoiceId(u64::from_le_bytes(e[1..9].try_into().ok()?));
        match e[0] {
            tag::ENDED => Some(Event::Ended(voice)),
            tag::STOLEN => Some(Event::Stolen(voice)),
            tag::REJECTED => Some(Event::Rejected(voice)),
            _ => None,
        }
    })
}

/// Why a command could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    UnknownTag(u8),
    Truncated,
}

struct Writer<'a>(&'a mut Vec<u8>);

impl Writer<'_> {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bus_f32(&mut self, tag: u8, bus: BusId, v: f32) {
        self.u8(tag);
        self.u16(bus.0);
        self.f32(v);
    }
    fn bus_u8(&mut self, tag: u8, bus: BusId, v: u8) {
        self.u8(tag);
        self.u16(bus.0);
        self.u8(v);
    }
    fn voice_f32(&mut self, tag: u8, voice: VoiceId, v: f32) {
        self.u8(tag);
        self.u64(voice.0);
        self.f32(v);
    }
    fn vec3(&mut self, v: [f32; 3]) {
        v.iter().for_each(|&x| self.f32(x));
    }
    fn start(&mut self, start: Start) {
        match start {
            Start::Now => {
                self.u8(0);
                self.u64(0);
            }
            Start::AtFrame(frame) => {
                self.u8(1);
                self.u64(frame);
            }
        }
    }
    /// A presence byte, then the settings.
    fn spatial(&mut self, spatial: Option<&Spatial>) {
        let Some(s) = spatial else {
            self.u8(0);
            return;
        };
        self.u8(1);
        self.u8(match s.mode {
            SpatialMode::Planar => 0,
            SpatialMode::Full => 1,
        });
        self.vec3(s.position);
        self.vec3(s.velocity);
        self.f32(s.min_distance);
        self.f32(s.max_distance);
        self.u8(match s.rolloff {
            Rolloff::Inverse => 0,
            Rolloff::Linear => 1,
        });
        self.f32(s.doppler);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let end = self.at + N;
        let bytes = self.bytes.get(self.at..end).ok_or(WireError::Truncated)?;
        self.at = end;
        Ok(bytes.try_into().expect("N bytes"))
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take::<1>()?[0])
    }
    fn bool(&mut self) -> Result<bool, WireError> {
        Ok(self.u8()? != 0)
    }
    fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn f32(&mut self) -> Result<f32, WireError> {
        Ok(f32::from_le_bytes(self.take()?))
    }
    fn bus(&mut self) -> Result<BusId, WireError> {
        Ok(BusId(self.u16()?))
    }
    fn voice(&mut self) -> Result<VoiceId, WireError> {
        Ok(VoiceId(self.u64()?))
    }
    fn vec3(&mut self) -> Result<[f32; 3], WireError> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
    fn start(&mut self) -> Result<Start, WireError> {
        let kind = self.u8()?;
        let frame = self.u64()?;
        Ok(if kind == 1 {
            Start::AtFrame(frame)
        } else {
            Start::Now
        })
    }
    fn duck(&mut self) -> Result<Option<Duck>, WireError> {
        if !self.bool()? {
            return Ok(None);
        }
        Ok(Some(Duck {
            key: self.bus()?,
            depth_db: self.f32()?,
            threshold_db: self.f32()?,
            attack_ms: self.f32()?,
            release_ms: self.f32()?,
        }))
    }
    fn spatial(&mut self) -> Result<Option<Spatial>, WireError> {
        if !self.bool()? {
            return Ok(None);
        }
        let mode = if self.u8()? == 0 {
            SpatialMode::Planar
        } else {
            SpatialMode::Full
        };
        Ok(Some(Spatial {
            mode,
            position: self.vec3()?,
            velocity: self.vec3()?,
            min_distance: self.f32()?,
            max_distance: self.f32()?,
            rolloff: if self.u8()? == 0 {
                Rolloff::Inverse
            } else {
                Rolloff::Linear
            },
            doppler: self.f32()?,
        }))
    }

    fn command(&mut self) -> Result<Command, WireError> {
        Ok(match self.u8()? {
            tag::SET_BUS => {
                let bus = self.bus()?;
                let output = self.bus()?;
                let gain_db = self.f32()?;
                let pan = self.f32()?;
                let flags = self.u8()?;
                let voice_limit = self.u16()?;
                let duck = self.duck()?;
                Command::SetBus(
                    bus,
                    BusSettings {
                        output,
                        gain_db,
                        pan,
                        mute: flags & 1 != 0,
                        solo: flags & 2 != 0,
                        voice_limit,
                        duck,
                    },
                )
            }
            tag::REMOVE_BUS => Command::RemoveBus(self.bus()?),
            tag::BUS_GAIN => Command::SetBusGain(self.bus()?, self.f32()?),
            tag::BUS_PAN => Command::SetBusPan(self.bus()?, self.f32()?),
            tag::BUS_MUTE => Command::SetBusMute(self.bus()?, self.bool()?),
            tag::BUS_SOLO => Command::SetBusSolo(self.bus()?, self.bool()?),
            tag::BUS_OUTPUT => Command::SetBusOutput(self.bus()?, self.bus()?),
            tag::SET_SEND => {
                let bus = self.bus()?;
                let slot = self.u8()?;
                let present = self.bool()?;
                let to = self.bus()?;
                let level_db = self.f32()?;
                let pre_fader = self.bool()?;
                Command::SetSend {
                    bus,
                    slot,
                    send: present.then_some(SendSettings {
                        to,
                        level_db,
                        pre_fader,
                    }),
                }
            }
            tag::INSERT_BYPASS => Command::SetInsertBypass {
                bus: self.bus()?,
                slot: self.u8()?,
                bypass: self.bool()?,
            },
            tag::INSERT_PARAM => Command::SetInsertParam {
                bus: self.bus()?,
                slot: self.u8()?,
                param: self.u8()?,
                value: self.f32()?,
            },
            tag::REMOVE_CLIP => Command::RemoveClip(ClipId(self.u32()?)),
            tag::PLAY => {
                let voice = self.voice()?;
                let clip = ClipId(self.u32()?);
                let bus = self.bus()?;
                let gain = self.f32()?;
                let pitch = self.f32()?;
                let pan = self.f32()?;
                let looped = self.bool()?;
                let priority = self.u8()?;
                let start = self.start()?;
                let spatial = self.spatial()?;
                Command::Play(Play {
                    voice,
                    clip,
                    bus,
                    gain,
                    pitch,
                    pan,
                    looped,
                    priority,
                    start,
                    spatial,
                })
            }
            tag::PLAY_SOURCE => Command::PlaySource(PlaySource {
                voice: self.voice()?,
                source: SourceId(self.u32()?),
                bus: self.bus()?,
                gain: self.f32()?,
                pan: self.f32()?,
                priority: self.u8()?,
                start: self.start()?,
                spatial: self.spatial()?,
            }),
            tag::SOURCE_EVENT => {
                let voice = self.voice()?;
                let kind = self.u8()?;
                let note = self.u8()?;
                let velocity = self.f32()?;
                let duration = self.u32()?;
                Command::SourceEvent {
                    voice,
                    event: match kind {
                        0 => SourceEvent::NoteOn {
                            note,
                            velocity,
                            duration,
                        },
                        1 => SourceEvent::NoteOff { note },
                        _ => SourceEvent::AllNotesOff,
                    },
                }
            }
            tag::FADE_VOICE => Command::FadeVoice {
                voice: self.voice()?,
                gain: self.f32()?,
                frames: self.u32()?,
            },
            tag::VOICE_GAIN => Command::SetVoiceGain(self.voice()?, self.f32()?),
            tag::VOICE_PITCH => Command::SetVoicePitch(self.voice()?, self.f32()?),
            tag::VOICE_PAN => Command::SetVoicePan(self.voice()?, self.f32()?),
            tag::STOP => Command::Stop(self.voice()?, self.u32()?),
            tag::STOP_BUS => Command::StopBus(self.bus()?, self.u32()?),
            tag::STOP_ALL => Command::StopAll(self.u32()?),
            tag::SET_LISTENER => Command::SetListener(Listener {
                position: self.vec3()?,
                forward: self.vec3()?,
                up: self.vec3()?,
                velocity: self.vec3()?,
                pan_width: self.f32()?,
                speed_of_sound: self.f32()?,
                doppler: self.bool()?,
            }),
            tag::VOICE_SPATIAL => Command::SetVoiceSpatial(self.voice()?, self.spatial()?),
            tag::VOICE_POSITION => Command::SetVoicePosition {
                voice: self.voice()?,
                position: self.vec3()?,
                velocity: self.vec3()?,
            },
            other => return Err(WireError::UnknownTag(other)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(command: Command) {
        let mut bytes = Vec::new();
        assert!(encode(&command, &mut bytes));
        let decoded: Vec<_> = decode(&bytes).collect();
        assert_eq!(decoded.len(), 1);
        let back = decoded.into_iter().next().unwrap().unwrap();
        let mut again = Vec::new();
        encode(&back, &mut again);
        assert_eq!(bytes, again, "{command:?}");
    }

    #[test]
    fn every_byte_command_round_trips() {
        let bus = BusId(3);
        let voice = VoiceId(0x0102_0304_0506_0708);
        for command in [
            Command::SetBus(
                bus,
                BusSettings {
                    output: BusId(1),
                    gain_db: -6.5,
                    pan: 0.25,
                    mute: true,
                    solo: false,
                    voice_limit: 12,
                    duck: None,
                },
            ),
            Command::SetBus(
                bus,
                BusSettings {
                    duck: Some(Duck {
                        threshold_db: -30.0,
                        ..Duck::new(BusId(2), 9.0)
                    }),
                    ..BusSettings::default()
                },
            ),
            Command::PlaySource(PlaySource {
                gain: 0.0,
                pan: 0.5,
                priority: 200,
                start: Start::AtFrame(99),
                spatial: Some(Spatial::new(SpatialMode::Full, [1.0, 2.0, 3.0])),
                ..PlaySource::new(voice, SourceId(3), bus)
            }),
            Command::PlaySource(PlaySource::new(voice, SourceId(0), BusId(1))),
            Command::FadeVoice {
                voice,
                gain: 0.75,
                frames: 96_000,
            },
            Command::SourceEvent {
                voice,
                event: SourceEvent::NoteOn {
                    note: 64,
                    velocity: 0.5,
                    duration: 24_000,
                },
            },
            Command::SourceEvent {
                voice,
                event: SourceEvent::NoteOff { note: 64 },
            },
            Command::SourceEvent {
                voice,
                event: SourceEvent::AllNotesOff,
            },
            Command::RemoveBus(bus),
            Command::SetBusGain(bus, -3.0),
            Command::SetBusPan(bus, -1.0),
            Command::SetBusMute(bus, true),
            Command::SetBusSolo(bus, true),
            Command::SetBusOutput(bus, BusId(2)),
            Command::SetSend {
                bus,
                slot: 1,
                send: Some(SendSettings {
                    to: BusId(5),
                    level_db: -12.0,
                    pre_fader: true,
                }),
            },
            Command::SetSend {
                bus,
                slot: 0,
                send: None,
            },
            Command::SetInsertBypass {
                bus,
                slot: 2,
                bypass: true,
            },
            Command::SetInsertParam {
                bus,
                slot: 3,
                param: 1,
                value: 0.4,
            },
            Command::RemoveClip(ClipId(77)),
            Command::Play(Play {
                gain: 0.5,
                pitch: 1.5,
                pan: -0.5,
                looped: true,
                priority: 9,
                start: Start::AtFrame(1 << 40),
                ..Play::new(voice, ClipId(4), bus)
            }),
            Command::Play(Play::new(voice, ClipId(4), bus)),
            Command::SetVoiceGain(voice, 0.25),
            Command::SetVoicePitch(voice, 2.0),
            Command::SetVoicePan(voice, 1.0),
            Command::Stop(voice, 240),
            Command::StopBus(bus, 10),
            Command::StopAll(0),
            Command::Play(Play {
                spatial: Some(Spatial {
                    velocity: [1.0, -2.0, 3.5],
                    min_distance: 2.0,
                    max_distance: 40.0,
                    rolloff: Rolloff::Linear,
                    doppler: 0.5,
                    ..Spatial::new(SpatialMode::Planar, [4.0, 5.0, 6.0])
                }),
                ..Play::new(voice, ClipId(1), bus)
            }),
            Command::SetListener(Listener {
                position: [1.0, 2.0, 3.0],
                velocity: [0.5, 0.0, 0.0],
                pan_width: 12.0,
                doppler: false,
                ..Listener::default()
            }),
            Command::SetVoiceSpatial(
                voice,
                Some(Spatial::new(SpatialMode::Full, [1.0, 0.0, 0.0])),
            ),
            Command::SetVoiceSpatial(voice, None),
            Command::SetVoicePosition {
                voice,
                position: [9.0, 8.0, 7.0],
                velocity: [-1.0, 0.0, 1.0],
            },
        ] {
            round_trip(command);
        }
    }

    #[test]
    fn a_batch_decodes_in_order_and_stops_at_garbage() {
        let mut bytes = Vec::new();
        encode(&Command::StopAll(1), &mut bytes);
        encode(&Command::SetBusGain(BusId(1), -6.0), &mut bytes);
        bytes.push(200);
        let decoded: Vec<_> = decode(&bytes).collect();
        assert!(matches!(decoded[0], Ok(Command::StopAll(1))));
        assert!(matches!(decoded[1], Ok(Command::SetBusGain(BusId(1), _))));
        assert_eq!(decoded[2].as_ref().err(), Some(&WireError::UnknownTag(200)));
        assert_eq!(decoded.len(), 3);
        assert_eq!(
            decode(&bytes[..3]).next().unwrap().err(),
            Some(WireError::Truncated)
        );
    }

    #[test]
    fn events_round_trip() {
        let mut bytes = Vec::new();
        for e in [
            Event::Ended(VoiceId(1)),
            Event::Stolen(VoiceId(u64::MAX)),
            Event::Rejected(VoiceId(7)),
        ] {
            encode_event(e, &mut bytes);
        }
        let back: Vec<_> = decode_events(&bytes).collect();
        assert_eq!(
            back,
            [
                Event::Ended(VoiceId(1)),
                Event::Stolen(VoiceId(u64::MAX)),
                Event::Rejected(VoiceId(7))
            ]
        );
    }
}
