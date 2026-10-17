//! A byte encoding of [`Command`]s and [`Event`]s, for hosts whose game and
//! audio thread only share bytes: the browser's MessagePort between a game
//! and its AudioWorklet.
//!
//! Each message is a tag byte and fixed little-endian fields. Commands that
//! carry objects (clips, inserts, recordings) have no encoding; a host sends
//! their data its own way (the worklet takes clip PCM through its own
//! export). Decoding never allocates.

use crate::command::{
    BusId, BusSettings, ClipId, Command, Event, Play, SendSettings, Start, VoiceId,
};

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

    pub const ENDED: u8 = 1;
    pub const STOLEN: u8 = 2;
    pub const REJECTED: u8 = 3;
}

/// Bytes per encoded event.
pub const EVENT_SIZE: usize = 9;

/// Appends `command` to `out`. Returns false (and writes nothing) for the
/// commands that carry objects: `SetInsert`, `AddClip` and `Capture`.
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
            match p.start {
                Start::Now => {
                    w.u8(0);
                    w.u64(0);
                }
                Start::AtFrame(frame) => {
                    w.u8(1);
                    w.u64(frame);
                }
            }
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
        Command::SetInsert { .. } | Command::AddClip(..) | Command::Capture(_) => return false,
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

    fn command(&mut self) -> Result<Command, WireError> {
        Ok(match self.u8()? {
            tag::SET_BUS => {
                let bus = self.bus()?;
                let output = self.bus()?;
                let gain_db = self.f32()?;
                let pan = self.f32()?;
                let flags = self.u8()?;
                let voice_limit = self.u16()?;
                Command::SetBus(
                    bus,
                    BusSettings {
                        output,
                        gain_db,
                        pan,
                        mute: flags & 1 != 0,
                        solo: flags & 2 != 0,
                        voice_limit,
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
                let start_kind = self.u8()?;
                let frame = self.u64()?;
                Command::Play(Play {
                    voice,
                    clip,
                    bus,
                    gain,
                    pitch,
                    pan,
                    looped,
                    priority,
                    start: if start_kind == 1 {
                        Start::AtFrame(frame)
                    } else {
                        Start::Now
                    },
                })
            }
            tag::VOICE_GAIN => Command::SetVoiceGain(self.voice()?, self.f32()?),
            tag::VOICE_PITCH => Command::SetVoicePitch(self.voice()?, self.f32()?),
            tag::VOICE_PAN => Command::SetVoicePan(self.voice()?, self.f32()?),
            tag::STOP => Command::Stop(self.voice()?, self.u32()?),
            tag::STOP_BUS => Command::StopBus(self.bus()?, self.u32()?),
            tag::STOP_ALL => Command::StopAll(self.u32()?),
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
                },
            ),
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
