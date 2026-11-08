//! One OPL instrument as a document: the DAW's Adlib instrument editor, for an
//! instrument that is not (yet) in an Adlib Tracker II song.
//!
//! The case it exists for is the S3M AdLib instrument (`cubed_tracker::Adlib`:
//! 12 register bytes, of which the first eleven are the FM bytes). An editor
//! opens the instrument with [`OplInstrumentDoc::from_s3m`], edits it with the
//! same operations the song document takes (`set_instrument`, `set_operator`,
//! `swap_operators`, ...), plays it with [`OplInstrumentDoc::patch`] on a
//! [`crate::audition::OplAudition`], and sends [`OplInstrumentDoc::s3m_registers`]
//! to `cubed_tracker::edit::ModuleDoc`'s `set_sample` (`adlib.registers`). The
//! chip limits are the format's: an S3M's OPL2 has the waveforms 0-3, no
//! panning, no fine-tune.
//!
//! The instrument is tiny, so undo keeps whole states.

use serde_json::{json, Value};

use super::fm_json::{fm_bytes, operator_edit, opt_i8, voice_edit, voice_json};
use super::json::{field, opt_bool, opt_str, opt_u8, req_str, Res};
use super::limits::{encode_text, text};
use crate::a2::model::Instrument;
use crate::fm::{FmPatch, OperatorEdit, Voice, VoiceEdit, FM_BYTES, PLAIN_FM};

/// Whose chip the instrument is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OplKind {
    /// An S3M AdLib instrument: an OPL2 (waveforms 0-3), no name, panning,
    /// fine-tune or voice type of its own.
    S3m,
    /// An Adlib Tracker II instrument: an OPL3 (waveforms 0-7), with a name,
    /// panning, fine-tune and voice type.
    A2m,
}

/// The fields of an edit (as `A2mDoc::set_instrument` takes them).
pub use super::instruments::InstrumentEdit;

#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    name: Vec<u8>,
    fm: [u8; FM_BYTES],
    /// An S3M instrument's twelfth register byte, kept as it was.
    extra: u8,
    panning: u8,
    finetune: i8,
    voice_type: u8,
}

/// One instrument being edited. See the module docs.
#[derive(Debug)]
pub struct OplInstrumentDoc {
    kind: OplKind,
    state: State,
    undo: Vec<State>,
    redo: Vec<State>,
    depth: usize,
    group_start: Option<State>,
    revision: u64,
}

const MAX_UNDO: usize = 1000;

impl OplInstrumentDoc {
    fn make(kind: OplKind, state: State) -> OplInstrumentDoc {
        OplInstrumentDoc {
            kind,
            state,
            undo: Vec::new(),
            redo: Vec::new(),
            depth: 0,
            group_start: None,
            revision: 0,
        }
    }

    /// An S3M AdLib instrument's 12 registers (`Adlib::registers`).
    pub fn from_s3m(registers: &[u8; 12]) -> OplInstrumentDoc {
        let mut fm = [0u8; FM_BYTES];
        fm.copy_from_slice(&registers[..FM_BYTES]);
        OplInstrumentDoc::make(
            OplKind::S3m,
            State {
                name: Vec::new(),
                fm,
                extra: registers[11],
                panning: 0,
                finetune: 0,
                voice_type: 0,
            },
        )
    }

    /// A new instrument: the plain sine voice.
    pub fn new(kind: OplKind) -> OplInstrumentDoc {
        OplInstrumentDoc::make(
            kind,
            State {
                name: Vec::new(),
                fm: PLAIN_FM,
                extra: 0,
                panning: 0,
                finetune: 0,
                voice_type: 0,
            },
        )
    }

    /// An Adlib Tracker II instrument.
    pub fn from_a2m(instrument: &Instrument) -> OplInstrumentDoc {
        OplInstrumentDoc::make(
            OplKind::A2m,
            State {
                name: instrument.name.clone(),
                fm: instrument.fm,
                extra: 0,
                panning: instrument.panning,
                finetune: instrument.finetune,
                voice_type: instrument.voice_type,
            },
        )
    }

    pub fn kind(&self) -> OplKind {
        self.kind
    }

    pub fn fm(&self) -> &[u8; FM_BYTES] {
        &self.state.fm
    }

    /// The registers of an S3M AdLib instrument (the twelfth byte as it was read).
    pub fn s3m_registers(&self) -> [u8; 12] {
        let mut r = [0u8; 12];
        r[..FM_BYTES].copy_from_slice(&self.state.fm);
        r[11] = self.state.extra;
        r
    }

    /// The instrument as an Adlib Tracker II one.
    pub fn to_a2m(&self) -> Instrument {
        Instrument {
            name: self.state.name.clone(),
            fm: self.state.fm,
            panning: self.state.panning,
            finetune: self.state.finetune,
            voice_type: self.state.voice_type,
        }
    }

    /// What the audition and the song player play.
    pub fn patch(&self) -> FmPatch {
        FmPatch {
            fm: self.state.fm,
            panning: self.state.panning,
            finetune: self.state.finetune,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn can_undo(&self) -> bool {
        self.depth == 0 && !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        self.depth == 0 && !self.redo.is_empty()
    }

    fn opl3(&self) -> bool {
        self.kind == OplKind::A2m
    }

    /// The instrument as JSON: `{"kind": "s3m" | "a2m", "name", "fm": [11],
    /// "registers": [12] (an S3M's), "voice": {"modulator", "carrier",
    /// "feedback", "connection"}, "panning", "finetune", "voice_type",
    /// "revision", "can_undo", "can_redo"}`.
    pub fn info(&self) -> Value {
        let s = &self.state;
        json!({
            "kind": match self.kind { OplKind::S3m => "s3m", OplKind::A2m => "a2m" },
            "name": text(&s.name),
            "fm": s.fm,
            "registers": self.s3m_registers(),
            "voice": voice_json(&Voice::decode(&s.fm)),
            "panning": s.panning,
            "finetune": s.finetune,
            "voice_type": s.voice_type,
            "max_waveform": if self.opl3() { 7 } else { 3 },
            "revision": self.revision,
            "can_undo": self.can_undo(),
            "can_redo": self.can_redo(),
        })
    }

    /// Applies `f` to a copy of the state; on success the copy replaces the
    /// state and the old one becomes an undo step (or the start of the open
    /// group's); on failure nothing changes.
    fn edit(&mut self, f: impl FnOnce(&mut State, OplKind) -> Res<()>) -> Res<bool> {
        let mut next = self.state.clone();
        f(&mut next, self.kind)?;
        if next == self.state {
            return Ok(false);
        }
        let before = std::mem::replace(&mut self.state, next);
        if self.depth > 0 {
            self.group_start.get_or_insert(before);
        } else {
            self.push_undo(before);
        }
        self.redo.clear();
        self.revision += 1;
        Ok(true)
    }

    fn push_undo(&mut self, s: State) {
        self.undo.push(s);
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
    }

    pub fn begin_group(&mut self) {
        self.depth += 1;
    }

    pub fn end_group(&mut self) {
        if self.depth > 0 {
            self.depth -= 1;
            if self.depth == 0 {
                if let Some(s) = self.group_start.take() {
                    self.push_undo(s);
                }
            }
        }
    }

    /// Changes the instrument (see `A2mDoc::set_instrument` for the fields).
    /// Returns whether anything changed.
    pub fn set_instrument(&mut self, edit: &InstrumentEdit) -> Res<bool> {
        let opl3 = self.opl3();
        self.edit(|s, kind| {
            if let Some(name) = &edit.name {
                if kind == OplKind::S3m {
                    return Err(
                        "an S3M AdLib instrument has no name of its own: it is the sample's".into(),
                    );
                }
                s.name = encode_text(name, 42).0;
            }
            for (given, what) in [
                (edit.panning.is_some_and(|p| p != 0), "panning"),
                (edit.finetune.is_some_and(|f| f != 0), "fine-tune"),
                (edit.voice_type.is_some_and(|t| t != 0), "voice type"),
            ] {
                if given && kind == OplKind::S3m {
                    return Err(format!("an S3M AdLib instrument has no {what}"));
                }
            }
            if let Some(p) = edit.panning {
                if p > 2 {
                    return Err(format!(
                        "panning is 0 (centre), 1 (left) or 2 (right), not {p}"
                    ));
                }
                s.panning = p;
            }
            if let Some(f) = edit.finetune {
                s.finetune = f;
            }
            if let Some(t) = edit.voice_type {
                if t > 5 {
                    return Err(format!("voice type is 0 (melodic) to 5 (hi-hat), not {t}"));
                }
                s.voice_type = t;
            }
            if let Some(fm) = edit.fm {
                s.fm = fm;
            }
            if !edit.voice.is_empty() {
                let v = edit.voice.apply(&Voice::decode(&s.fm), opl3)?;
                s.fm = v.encode(&s.fm);
            }
            Ok(())
        })
    }

    pub fn set_operator(&mut self, carrier: bool, edit: &OperatorEdit) -> Res<bool> {
        let mut v = VoiceEdit::default();
        if carrier {
            v.carrier = *edit;
        } else {
            v.modulator = *edit;
        }
        self.set_instrument(&InstrumentEdit {
            voice: v,
            ..Default::default()
        })
    }

    pub fn swap_operators(&mut self) -> Res<bool> {
        let fm = Voice::decode(&self.state.fm)
            .swapped()
            .encode(&self.state.fm);
        self.set_instrument(&InstrumentEdit {
            fm: Some(fm),
            ..Default::default()
        })
    }

    /// The plain sine voice (panning, fine-tune and voice type cleared; the name stays).
    pub fn reset(&mut self) -> Res<bool> {
        self.edit(|s, _| {
            s.fm = PLAIN_FM;
            s.panning = 0;
            s.finetune = 0;
            s.voice_type = 0;
            Ok(())
        })
    }

    /// Empties the instrument (the name stays when `keep_name`).
    pub fn clear(&mut self, keep_name: bool) -> Res<bool> {
        self.edit(|s, _| {
            s.fm = [0; FM_BYTES];
            s.panning = 0;
            s.finetune = 0;
            s.voice_type = 0;
            if !keep_name {
                s.name.clear();
            }
            Ok(())
        })
    }

    pub fn undo(&mut self) -> bool {
        if !self.can_undo() {
            return false;
        }
        let Some(before) = self.undo.pop() else {
            return false;
        };
        let now = std::mem::replace(&mut self.state, before);
        self.redo.push(now);
        self.revision += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        if !self.can_redo() {
            return false;
        }
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let now = std::mem::replace(&mut self.state, next);
        self.undo.push(now);
        self.revision += 1;
        true
    }

    /// One operation as JSON (the song document's instrument operations without
    /// the `instrument` number): `info`, `set_instrument`, `set_operator`
    /// (`operator`, the operator's fields), `swap_operators`, `reset`, `clear`
    /// (`keep_name`), `begin_group`, `end_group`, `undo`, `redo`, `batch` (`ops`;
    /// all or none). Edits return `{"changed": bool, "instrument": <info>}`.
    pub fn apply(&mut self, op: &Value) -> Res<Value> {
        let name = req_str(op, "op")?;
        let changed =
            |d: &OplInstrumentDoc, c: bool| Ok(json!({"changed": c, "instrument": d.info()}));
        match name {
            "info" | "instrument" => Ok(self.info()),
            "begin_group" => {
                self.begin_group();
                Ok(json!({}))
            }
            "end_group" => {
                self.end_group();
                Ok(json!({}))
            }
            "undo" => {
                if self.undo() {
                    changed(self, true)
                } else {
                    Err("nothing to undo".into())
                }
            }
            "redo" => {
                if self.redo() {
                    changed(self, true)
                } else {
                    Err("nothing to redo".into())
                }
            }
            "set_instrument" => {
                let edit = InstrumentEdit {
                    name: opt_str(op, "name")?.map(str::to_string),
                    panning: opt_u8(op, "panning")?,
                    finetune: opt_i8(op, "finetune")?,
                    voice_type: opt_u8(op, "voice_type")?,
                    fm: match field(op, "fm") {
                        Some(v) => Some(fm_bytes(v, "'fm'")?),
                        None => None,
                    },
                    voice: voice_edit(op)?,
                };
                let c = self.set_instrument(&edit)?;
                changed(self, c)
            }
            "set_operator" => {
                let carrier = match req_str(op, "operator")? {
                    "modulator" => false,
                    "carrier" => true,
                    other => {
                        return Err(format!("'{other}' is not an operator (modulator, carrier)"))
                    }
                };
                let c = self.set_operator(carrier, &operator_edit(op)?)?;
                changed(self, c)
            }
            "swap_operators" => {
                let c = self.swap_operators()?;
                changed(self, c)
            }
            "reset" => {
                let c = self.reset()?;
                changed(self, c)
            }
            "clear" => {
                let c = self.clear(opt_bool(op, "keep_name")?.unwrap_or(false))?;
                changed(self, c)
            }
            "batch" => {
                let ops = op
                    .get("ops")
                    .and_then(Value::as_array)
                    .ok_or("batch needs 'ops': a list of operations")?;
                let snapshot = (
                    self.state.clone(),
                    self.undo.len(),
                    self.redo.clone(),
                    self.revision,
                );
                self.begin_group();
                let mut results = Vec::new();
                for (i, o) in ops.iter().enumerate() {
                    match self.apply(o) {
                        Ok(r) => results.push(r),
                        Err(e) => {
                            // All or none: put back what the earlier operations did.
                            self.depth = 0;
                            self.group_start = None;
                            self.state = snapshot.0;
                            self.undo.truncate(snapshot.1);
                            self.redo = snapshot.2;
                            self.revision = snapshot.3;
                            return Err(format!(
                                "operation {i} ({}) failed: {e}",
                                o.get("op").and_then(Value::as_str).unwrap_or("?")
                            ));
                        }
                    }
                }
                self.end_group();
                Ok(json!({"results": results, "instrument": self.info()}))
            }
            other => Err(format!("unknown operation '{other}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(d: &mut OplInstrumentDoc, op: Value) -> Value {
        d.apply(&op).unwrap_or_else(|e| panic!("{op}: {e}"))
    }

    #[test]
    fn an_s3m_instrument_round_trips_its_registers() {
        let regs = [
            0x21, 0x31, 0x4f, 0x00, 0xf1, 0xd2, 0x11, 0x22, 0x00, 0x00, 0x08, 0x5a,
        ];
        let mut d = OplInstrumentDoc::from_s3m(&regs);
        assert_eq!(d.s3m_registers(), regs, "untouched, as read");
        ok(
            &mut d,
            json!({"op": "set_operator", "operator": "carrier", "attack": 15, "level": 40}),
        );
        let r = d.s3m_registers();
        assert_eq!(r[5], 0xf2, "attack 15, decay 2");
        assert_eq!(r[3] & 0x3f, 63 - 40);
        assert_eq!(r[11], 0x5a, "the twelfth byte stays");
        assert_eq!(r[10], 0x08, "feedback and connection stay");
        ok(&mut d, json!({"op": "undo"}));
        assert_eq!(d.s3m_registers(), regs);
        ok(&mut d, json!({"op": "redo"}));
        assert_eq!(d.s3m_registers()[5], 0xf2);
    }

    #[test]
    fn an_s3m_instrument_is_an_opl2_with_no_extras() {
        let mut d = OplInstrumentDoc::from_s3m(&[0; 12]);
        let e = d
            .apply(&json!({"op": "set_operator", "operator": "modulator", "waveform": 4}))
            .unwrap_err();
        assert!(e.contains("on an OPL2"), "{e}");
        for (op, word) in [
            (json!({"op": "set_instrument", "panning": 1}), "panning"),
            (json!({"op": "set_instrument", "finetune": 3}), "fine-tune"),
            (
                json!({"op": "set_instrument", "voice_type": 2}),
                "voice type",
            ),
            (json!({"op": "set_instrument", "name": "x"}), "no name"),
        ] {
            let e = d.apply(&op).unwrap_err();
            assert!(e.contains(word), "{e}");
        }
        assert_eq!(d.revision(), 0, "refusals change nothing");
        // An A2M instrument has the OPL3's waveforms and the extras.
        let mut a = OplInstrumentDoc::new(OplKind::A2m);
        ok(
            &mut a,
            json!({"op": "set_operator", "operator": "carrier", "waveform": 6}),
        );
        ok(
            &mut a,
            json!({"op": "set_instrument", "name": "Bell ½", "panning": 2, "finetune": -5, "voice_type": 1}),
        );
        let info = ok(&mut a, json!({"op": "info"}));
        assert_eq!(info["name"], "Bell ½");
        assert_eq!(
            (info["panning"].as_u64(), info["finetune"].as_i64()),
            (Some(2), Some(-5))
        );
        assert_eq!(a.patch().panning, 2);
        assert_eq!(a.to_a2m().fm[9], 6);
    }

    #[test]
    fn groups_batches_and_swaps() {
        let mut d = OplInstrumentDoc::new(OplKind::A2m);
        let plain = *d.fm();
        ok(
            &mut d,
            json!({"op": "batch", "ops": [
                {"op": "set_operator", "operator": "modulator", "level": 30, "multiplier": 3},
                {"op": "set_operator", "operator": "carrier", "attack": 5},
                {"op": "set_instrument", "feedback": 6, "connection": "additive"},
            ]}),
        );
        let v = Voice::decode(d.fm());
        assert_eq!(
            (
                v.modulator.multiplier,
                v.carrier.attack,
                v.feedback,
                v.additive
            ),
            (3, 5, 6, true)
        );
        // One step undoes the lot.
        ok(&mut d, json!({"op": "undo"}));
        assert_eq!(*d.fm(), plain);
        assert!(!d.can_undo());
        ok(&mut d, json!({"op": "redo"}));
        // A failing batch leaves nothing behind.
        let before = *d.fm();
        let e = d
            .apply(&json!({"op": "batch", "ops": [
                {"op": "set_operator", "operator": "carrier", "decay": 9},
                {"op": "set_operator", "operator": "carrier", "decay": 99},
            ]}))
            .unwrap_err();
        assert!(e.contains("operation 1") && e.contains("decay"), "{e}");
        assert_eq!(*d.fm(), before);
        assert!(d.can_undo() && !d.can_redo());
        ok(&mut d, json!({"op": "swap_operators"}));
        let s = Voice::decode(d.fm());
        assert_eq!(s.carrier.multiplier, 3, "operators traded places");
        assert_eq!(s.feedback, 6, "the channel's bits stay");
        ok(&mut d, json!({"op": "reset"}));
        assert_eq!(*d.fm(), PLAIN_FM);
        ok(&mut d, json!({"op": "clear"}));
        assert_eq!(*d.fm(), [0; FM_BYTES]);
    }
}
