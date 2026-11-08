//! The FM instrument every OPL format shares: two operators and the channel's
//! feedback and connection, as eleven register bytes.
//!
//! Adlib Tracker II stores an instrument as `fm: [u8; 11]` (`techinfo.htm`,
//! "instrument specifications"), modulator then carrier in each pair, and an S3M
//! AdLib instrument's first eleven registers are the same bytes in the same order
//! (`cubed_tracker::Adlib::registers[..11]`; the twelfth is unused):
//!
//! | byte | register | holds |
//! |---|---|---|
//! | 0-1 | `0x20` | tremolo (AM, bit 7), vibrato (bit 6), sustain (EG, bit 5), key scale rate (bit 4), multiplier (bits 0-3) |
//! | 2-3 | `0x40` | key scale level (bits 6-7), total level (bits 0-5; 0 is loudest) |
//! | 4-5 | `0x60` | attack (high nibble), decay (low) |
//! | 6-7 | `0x80` | sustain level (high nibble; 0 loudest), release (low) |
//! | 8-9 | `0xE0` | waveform, 0-7 (0-3 on an OPL2) |
//! | 10 | `0xC0` | feedback (bits 1-3), connection (bit 0: 1 is additive) |
//!
//! [`Voice`] is the editor's view of those bytes (what the DAW's instrument page
//! shows) and [`FmPatch`] the bytes with the per-instrument panning and
//! fine-tune: what [`crate::audition::OplAudition`] plays. Nothing here
//! allocates and nothing needs `serde_json`.

use crate::a2::model::Instrument;

/// Bytes of FM data in an instrument.
pub const FM_BYTES: usize = 11;

/// What an operator's fields can hold, as the chip's registers do.
pub mod range {
    pub const ATTACK: u8 = 15;
    pub const DECAY: u8 = 15;
    pub const SUSTAIN_LEVEL: u8 = 15;
    pub const RELEASE: u8 = 15;
    pub const TOTAL_LEVEL: u8 = 63;
    pub const KEY_SCALE_LEVEL: u8 = 3;
    pub const MULTIPLIER: u8 = 15;
    /// The OPL3's eight shapes (Adlib Tracker II); an OPL2 has the first four.
    pub const WAVEFORM: u8 = 7;
    pub const WAVEFORM_OPL2: u8 = 3;
    pub const FEEDBACK: u8 = 7;
}

/// The waveforms' names, by number.
pub const WAVEFORM_NAMES: [&str; 8] = [
    "Sine",
    "Half sine",
    "Abs sine",
    "Pulse sine",
    "Sine, double speed",
    "Abs sine, double speed",
    "Square",
    "Derived square",
];

/// One operator of an instrument.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Operator {
    /// AM: the operator's amplitude follows the tremolo LFO.
    pub tremolo: bool,
    /// VIB: its pitch follows the vibrato LFO.
    pub vibrato: bool,
    /// EG: holds at the sustain level until key-off instead of decaying on.
    pub sustain: bool,
    /// KSR: the envelope speeds up with pitch.
    pub ksr: bool,
    /// Frequency multiplier, 0-15 (0 is one half).
    pub multiplier: u8,
    /// Key scale level, 0-3: attenuation per octave.
    pub key_scale_level: u8,
    /// Total level 0-63 in 0.75 dB steps of attenuation: 0 is loudest.
    pub total_level: u8,
    pub attack: u8,
    pub decay: u8,
    /// 0 is loudest, 15 silent.
    pub sustain_level: u8,
    pub release: u8,
    pub waveform: u8,
}

impl Operator {
    /// The operator's loudness the way a slider wants it: 63 loudest.
    pub fn level(&self) -> u8 {
        63 - self.total_level.min(63)
    }

    /// Reads operator `i` (0 modulator, 1 carrier) of eleven FM bytes.
    pub fn decode(fm: &[u8], i: usize) -> Operator {
        let at = |k: usize| fm.get(k).copied().unwrap_or(0);
        let flags = at(i);
        let level = at(2 + i);
        let ad = at(4 + i);
        let sr = at(6 + i);
        Operator {
            tremolo: flags & 0x80 != 0,
            vibrato: flags & 0x40 != 0,
            sustain: flags & 0x20 != 0,
            ksr: flags & 0x10 != 0,
            multiplier: flags & 15,
            key_scale_level: level >> 6,
            total_level: level & 63,
            attack: ad >> 4,
            decay: ad & 15,
            sustain_level: sr >> 4,
            release: sr & 15,
            waveform: at(8 + i) & 7,
        }
    }

    /// Writes this operator into `fm` as operator `i`. A waveform keeps the
    /// register's upper bits (`fm`'s own), which the editor does not show.
    pub fn encode(&self, fm: &mut [u8; FM_BYTES], i: usize) {
        fm[i] = (self.tremolo as u8) << 7
            | (self.vibrato as u8) << 6
            | (self.sustain as u8) << 5
            | (self.ksr as u8) << 4
            | self.multiplier.min(range::MULTIPLIER);
        fm[2 + i] = self.key_scale_level.min(range::KEY_SCALE_LEVEL) << 6
            | self.total_level.min(range::TOTAL_LEVEL);
        fm[4 + i] = self.attack.min(range::ATTACK) << 4 | self.decay.min(range::DECAY);
        fm[6 + i] =
            self.sustain_level.min(range::SUSTAIN_LEVEL) << 4 | self.release.min(range::RELEASE);
        fm[8 + i] = fm[8 + i] & !7 | self.waveform.min(range::WAVEFORM);
    }
}

/// An instrument as the editor shows it: two operators, the feedback of the
/// modulator and how the two connect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Voice {
    pub modulator: Operator,
    pub carrier: Operator,
    /// Modulator feedback, 0-7.
    pub feedback: u8,
    /// The two operators are summed (additive); otherwise the modulator
    /// drives the carrier (FM).
    pub additive: bool,
}

impl Voice {
    /// The editor's view of eleven FM bytes (missing bytes read as 0).
    pub fn decode(fm: &[u8]) -> Voice {
        let fb = fm.get(10).copied().unwrap_or(0);
        Voice {
            modulator: Operator::decode(fm, 0),
            carrier: Operator::decode(fm, 1),
            feedback: (fb >> 1) & 7,
            additive: fb & 1 != 0,
        }
    }

    /// The eleven FM bytes for this voice. `base` is the bytes it came from:
    /// the bits the editor does not show (a waveform's upper bits, the
    /// feedback byte's high nibble) keep their value.
    pub fn encode(&self, base: &[u8; FM_BYTES]) -> [u8; FM_BYTES] {
        let mut out = *base;
        self.modulator.encode(&mut out, 0);
        self.carrier.encode(&mut out, 1);
        out[10] = out[10] & !0x0f | self.feedback.min(range::FEEDBACK) << 1 | self.additive as u8;
        out
    }

    /// The plain voice a new instrument starts as (Adlib Tracker II's):
    /// a sustained sine carrier and an unmodulated sine modulator.
    pub fn plain() -> Voice {
        Voice::decode(&PLAIN_FM)
    }

    /// The same voice with its operators swapped.
    pub fn swapped(&self) -> Voice {
        Voice {
            modulator: self.carrier,
            carrier: self.modulator,
            ..*self
        }
    }
}

/// A plain two-operator voice: a sine carrier, a silent sine modulator,
/// sustained (what a new Adlib Tracker II song's first instrument holds).
pub const PLAIN_FM: [u8; FM_BYTES] = [
    0x21, 0x21, 0x3f, 0x00, 0xf2, 0xf4, 0x24, 0x27, 0x00, 0x00, 0x00,
];

/// Which parts of an operator an edit changes (`None` leaves a field alone).
/// `level` is the slider's (63 loudest) and `total_level` the chip's (0 loudest);
/// give one of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OperatorEdit {
    pub tremolo: Option<bool>,
    pub vibrato: Option<bool>,
    pub sustain: Option<bool>,
    pub ksr: Option<bool>,
    pub multiplier: Option<u8>,
    pub key_scale_level: Option<u8>,
    pub total_level: Option<u8>,
    pub level: Option<u8>,
    pub attack: Option<u8>,
    pub decay: Option<u8>,
    pub sustain_level: Option<u8>,
    pub release: Option<u8>,
    pub waveform: Option<u8>,
}

fn within(name: &str, v: u8, max: u8) -> Result<u8, String> {
    if v > max {
        Err(format!("{name} is 0-{max}, not {v}"))
    } else {
        Ok(v)
    }
}

impl OperatorEdit {
    pub fn is_empty(&self) -> bool {
        *self == OperatorEdit::default()
    }

    /// The operator after this edit, or why a field does not fit the chip.
    /// `opl3` allows the waveforms 4-7.
    pub fn apply(&self, op: &Operator, opl3: bool) -> Result<Operator, String> {
        let mut o = *op;
        if let Some(v) = self.tremolo {
            o.tremolo = v;
        }
        if let Some(v) = self.vibrato {
            o.vibrato = v;
        }
        if let Some(v) = self.sustain {
            o.sustain = v;
        }
        if let Some(v) = self.ksr {
            o.ksr = v;
        }
        if let Some(v) = self.multiplier {
            o.multiplier = within("multiplier", v, range::MULTIPLIER)?;
        }
        if let Some(v) = self.key_scale_level {
            o.key_scale_level = within("key scale level", v, range::KEY_SCALE_LEVEL)?;
        }
        if self.total_level.is_some() && self.level.is_some() {
            return Err("give 'level' or 'total_level', not both".into());
        }
        if let Some(v) = self.total_level {
            o.total_level = within("total level", v, range::TOTAL_LEVEL)?;
        }
        if let Some(v) = self.level {
            o.total_level = range::TOTAL_LEVEL - within("level", v, range::TOTAL_LEVEL)?;
        }
        if let Some(v) = self.attack {
            o.attack = within("attack", v, range::ATTACK)?;
        }
        if let Some(v) = self.decay {
            o.decay = within("decay", v, range::DECAY)?;
        }
        if let Some(v) = self.sustain_level {
            o.sustain_level = within("sustain level", v, range::SUSTAIN_LEVEL)?;
        }
        if let Some(v) = self.release {
            o.release = within("release", v, range::RELEASE)?;
        }
        if let Some(v) = self.waveform {
            let max = if opl3 {
                range::WAVEFORM
            } else {
                range::WAVEFORM_OPL2
            };
            if v > max {
                return Err(format!(
                    "waveform is 0-{max}{}, not {v}",
                    if opl3 { "" } else { " on an OPL2" }
                ));
            }
            o.waveform = v;
        }
        Ok(o)
    }
}

/// An edit of a whole voice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VoiceEdit {
    pub modulator: OperatorEdit,
    pub carrier: OperatorEdit,
    pub feedback: Option<u8>,
    pub additive: Option<bool>,
}

impl VoiceEdit {
    pub fn is_empty(&self) -> bool {
        *self == VoiceEdit::default()
    }

    pub fn apply(&self, v: &Voice, opl3: bool) -> Result<Voice, String> {
        let mut out = *v;
        out.modulator = self
            .modulator
            .apply(&v.modulator, opl3)
            .map_err(|e| format!("modulator {e}"))?;
        out.carrier = self
            .carrier
            .apply(&v.carrier, opl3)
            .map_err(|e| format!("carrier {e}"))?;
        if let Some(f) = self.feedback {
            out.feedback = within("feedback", f, range::FEEDBACK)?;
        }
        if let Some(a) = self.additive {
            out.additive = a;
        }
        Ok(out)
    }
}

/// An instrument as the audition and the song player play it: the FM bytes,
/// the panning and the fine-tune.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FmPatch {
    pub fm: [u8; FM_BYTES],
    /// 0 centre, 1 left, 2 right.
    pub panning: u8,
    /// Added to the note's frequency number as the song player does.
    pub finetune: i8,
}

impl FmPatch {
    pub fn new(fm: [u8; FM_BYTES]) -> FmPatch {
        FmPatch {
            fm,
            panning: 0,
            finetune: 0,
        }
    }

    /// An Adlib Tracker II instrument's playing data.
    pub fn from_a2m(instrument: &Instrument) -> FmPatch {
        FmPatch {
            fm: instrument.fm,
            panning: instrument.panning.min(2),
            finetune: instrument.finetune,
        }
    }

    /// An S3M AdLib instrument's registers (`cubed_tracker::Adlib::registers`:
    /// the first eleven are the FM bytes; the twelfth is ignored).
    pub fn from_s3m(registers: &[u8; 12]) -> FmPatch {
        let mut fm = [0u8; FM_BYTES];
        fm.copy_from_slice(&registers[..FM_BYTES]);
        FmPatch::new(fm)
    }

    /// Adlib Tracker II's plain starting voice.
    pub fn plain() -> FmPatch {
        FmPatch::new(PLAIN_FM)
    }

    pub fn voice(&self) -> Voice {
        Voice::decode(&self.fm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_and_encode_are_inverses_for_every_byte_pattern() {
        // Every value of the registers the editor shows; the bits it does not show survive.
        for seed in 0..4096u32 {
            let mut fm = [0u8; FM_BYTES];
            let mut x = seed.wrapping_mul(2654435761).wrapping_add(12345);
            for b in fm.iter_mut() {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                *b = (x >> 24) as u8;
            }
            let v = Voice::decode(&fm);
            let back = v.encode(&fm);
            assert_eq!(back, fm, "{fm:02x?}");
        }
    }

    #[test]
    fn encode_puts_each_field_where_the_register_has_it() {
        let v = Voice {
            modulator: Operator {
                tremolo: true,
                vibrato: false,
                sustain: true,
                ksr: false,
                multiplier: 7,
                key_scale_level: 2,
                total_level: 0x1d,
                attack: 10,
                decay: 3,
                sustain_level: 4,
                release: 11,
                waveform: 2,
            },
            carrier: Operator {
                tremolo: false,
                vibrato: true,
                sustain: false,
                ksr: true,
                multiplier: 1,
                key_scale_level: 0,
                total_level: 0,
                attack: 15,
                decay: 0,
                sustain_level: 15,
                release: 1,
                waveform: 5,
            },
            feedback: 5,
            additive: true,
        };
        let fm = v.encode(&[0; FM_BYTES]);
        assert_eq!(
            fm,
            [0xa7, 0x51, 0x9d, 0x00, 0xa3, 0xf0, 0x4b, 0xf1, 0x02, 0x05, 0x0b]
        );
        assert_eq!(Voice::decode(&fm), v);
        assert_eq!(v.modulator.level(), 63 - 0x1d);
        assert_eq!(v.swapped().carrier, v.modulator);
    }

    #[test]
    fn edits_are_range_checked_per_chip() {
        let plain = Voice::plain();
        let ok = OperatorEdit {
            attack: Some(15),
            level: Some(63),
            waveform: Some(3),
            ..Default::default()
        };
        let o = ok.apply(&plain.carrier, false).unwrap();
        assert_eq!((o.attack, o.total_level, o.waveform), (15, 0, 3));
        for (edit, text) in [
            (
                OperatorEdit {
                    attack: Some(16),
                    ..Default::default()
                },
                "attack is 0-15",
            ),
            (
                OperatorEdit {
                    level: Some(64),
                    ..Default::default()
                },
                "level is 0-63",
            ),
            (
                OperatorEdit {
                    key_scale_level: Some(4),
                    ..Default::default()
                },
                "key scale level is 0-3",
            ),
            (
                OperatorEdit {
                    waveform: Some(4),
                    ..Default::default()
                },
                "on an OPL2",
            ),
            (
                OperatorEdit {
                    waveform: Some(8),
                    ..Default::default()
                },
                "waveform is 0-7",
            ),
            (
                OperatorEdit {
                    level: Some(1),
                    total_level: Some(1),
                    ..Default::default()
                },
                "not both",
            ),
        ] {
            let e = edit
                .apply(&plain.carrier, edit.waveform == Some(8))
                .unwrap_err();
            assert!(e.contains(text), "{e}");
        }
        assert!(OperatorEdit {
            waveform: Some(7),
            ..Default::default()
        }
        .apply(&plain.carrier, true)
        .is_ok());
        let e = VoiceEdit {
            feedback: Some(8),
            ..Default::default()
        }
        .apply(&plain, true)
        .unwrap_err();
        assert!(e.contains("feedback is 0-7"));
    }

    #[test]
    fn the_plain_voice_is_a_sustained_sine_carrier() {
        let v = Voice::plain();
        assert!(v.carrier.sustain && !v.additive);
        assert_eq!(v.modulator.total_level, 0x3f, "the modulator is silent");
        assert_eq!(v.carrier.total_level, 0);
        assert_eq!(v.carrier.waveform, 0);
    }

    #[test]
    fn s3m_registers_are_the_same_eleven_bytes() {
        let regs = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let p = FmPatch::from_s3m(&regs);
        assert_eq!(&p.fm[..], &regs[..11]);
    }
}
