//! Editing instruments and the macro tables (versions 9 and up).
//!
//! An instrument is a slot (1-250 before version 9, 1-255 from it) holding a
//! name, the eleven FM bytes (see `crate::fm`), a panning, a fine-tune and a
//! voice type; from version 9 each also has an FM macro (the table of the same
//! number) and the song has 255 arpeggio and 255 vibrato macros an FM macro can
//! name. Slots are fixed in number: a cell that names an empty one plays nothing.

use serde_json::{json, Value};

use super::fm_json::{fm_bytes, opt_i8, voice_edit, voice_json};
use super::history::{Change, Changes, MacroKind};
use super::json::{field, opt_str, opt_u8, req_usize, Res};
use super::limits::{encode_text, text};
use super::A2mDoc;
use crate::a2::model::{ArpeggioMacro, FmMacro, FmMacroStep, Instrument, VibratoMacro};
use crate::fm::{OperatorEdit, Voice, VoiceEdit, FM_BYTES, PLAIN_FM};

/// The voice types an instrument can have (version 9 and up).
pub const VOICE_TYPES: [&str; 6] = [
    "melodic",
    "bass drum",
    "snare",
    "tom-tom",
    "cymbal",
    "hi-hat",
];

/// What an edit of an instrument changes (`None` leaves a field alone).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstrumentEdit {
    pub name: Option<String>,
    /// 0 centre, 1 left, 2 right (version 5 and up).
    pub panning: Option<u8>,
    pub finetune: Option<i8>,
    /// 0 melodic, 1-5 bass drum, snare, tom-tom, cymbal, hi-hat (version 9 and up).
    pub voice_type: Option<u8>,
    /// The eleven FM bytes outright; the edits below apply to them.
    pub fm: Option<[u8; FM_BYTES]>,
    pub voice: VoiceEdit,
}

impl InstrumentEdit {
    /// From an operation: `name`, `panning`, `finetune`, `voice_type`, `fm`, and
    /// the voice fields (`modulator`/`carrier` objects, `feedback`,
    /// `connection`) as in [`voice_edit`](super::fm_json::voice_edit).
    pub fn from_op(op: &Value) -> Res<InstrumentEdit> {
        Ok(InstrumentEdit {
            name: opt_str(op, "name")?.map(str::to_string),
            panning: opt_u8(op, "panning")?,
            finetune: opt_i8(op, "finetune")?,
            voice_type: opt_u8(op, "voice_type")?,
            fm: match field(op, "fm") {
                Some(v) => Some(fm_bytes(v, "'fm'")?),
                None => None,
            },
            voice: voice_edit(op)?,
        })
    }
}

/// One FM macro step's fields to change.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FmStepEdit {
    pub fm: Option<[u8; FM_BYTES]>,
    pub voice: VoiceEdit,
    pub freq_slide: Option<i16>,
    pub panning: Option<u8>,
    pub duration: Option<u8>,
}

/// An edit of an FM macro: the header fields, and steps by number (from 1).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FmMacroEdit {
    pub length: Option<u8>,
    pub loop_begin: Option<u8>,
    pub loop_length: Option<u8>,
    pub keyoff_pos: Option<u8>,
    /// The arpeggio and vibrato tables the macro runs (0 none).
    pub arpeggio_table: Option<u8>,
    pub vibrato_table: Option<u8>,
    pub steps: Vec<(usize, FmStepEdit)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArpeggioEdit {
    pub length: Option<u8>,
    pub speed: Option<u8>,
    pub loop_begin: Option<u8>,
    pub loop_length: Option<u8>,
    pub keyoff_pos: Option<u8>,
    /// Entries by number (from 1): 0 the base note, 1-96 semitones up,
    /// 0x80 plus 1-97 a fixed note.
    pub entries: Vec<(usize, u8)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VibratoEdit {
    pub length: Option<u8>,
    pub speed: Option<u8>,
    pub delay: Option<u8>,
    pub loop_begin: Option<u8>,
    pub loop_length: Option<u8>,
    pub keyoff_pos: Option<u8>,
    /// Entries by number (from 1): frequency units to add.
    pub entries: Vec<(usize, i8)>,
}

fn voice_type_name(n: u8) -> &'static str {
    VOICE_TYPES.get(n as usize).copied().unwrap_or("?")
}

fn is_empty_instrument(i: &Instrument) -> bool {
    i.fm == [0; FM_BYTES] && i.panning == 0 && i.finetune == 0 && i.voice_type == 0
}

/// Whether a macro's loop and key-off positions make sense for its length.
fn check_positions(length: u8, loop_begin: u8, loop_length: u8, keyoff: u8) -> Res<()> {
    if loop_length > 0 {
        if loop_begin == 0 {
            return Err("a loop starts at entry 1 or later".into());
        }
        if loop_begin as usize + loop_length as usize - 1 > length as usize {
            return Err(format!(
                "the loop ({loop_length} entries from {loop_begin}) runs past the macro's length {length}"
            ));
        }
    }
    if keyoff > length {
        return Err(format!(
            "the key-off position {keyoff} is past the macro's length {length}"
        ));
    }
    Ok(())
}

fn macro_index(n: usize) -> Res<usize> {
    if (1..=255).contains(&n) {
        Ok(n - 1)
    } else {
        Err(format!("a macro is numbered 1-255, not {n}"))
    }
}

fn entry_index(n: usize) -> Res<usize> {
    if (1..=255).contains(&n) {
        Ok(n - 1)
    } else {
        Err(format!("a macro entry is numbered 1-255, not {n}"))
    }
}

impl A2mDoc {
    fn instrument_index(&self, n: usize) -> Res<usize> {
        let count = self.song.instruments.len();
        if n >= 1 && n <= count {
            Ok(n - 1)
        } else {
            Err(format!(
                "instrument {n} does not exist (this version has 1-{count})"
            ))
        }
    }

    fn need_macros(&self, what: &str) -> Res<()> {
        if self.limits().macros {
            Ok(())
        } else {
            Err(format!(
                "version {} has no {what} (macros start with version 9)",
                self.song.version
            ))
        }
    }

    /// The instrument in slot `n` (from 1).
    pub fn instrument_data(&self, n: usize) -> Res<&Instrument> {
        Ok(&self.song.instruments[self.instrument_index(n)?])
    }

    /// An instrument as JSON: `{"instrument", "name", "empty", "fm": [11],
    /// "voice": {"modulator", "carrier", "feedback", "connection"}, "panning",
    /// "finetune", "voice_type", "voice_type_name"}` and, from version 9, `"macro":
    /// {"length", "arpeggio_table", "vibrato_table"}`.
    pub fn instrument(&self, n: usize) -> Res<Value> {
        let i = self.instrument_data(n)?;
        let mut v = json!({
            "instrument": n,
            "name": text(&i.name),
            "empty": is_empty_instrument(i) && i.name.is_empty(),
            "fm": i.fm,
            "voice": voice_json(&Voice::decode(&i.fm)),
            "panning": i.panning,
            "finetune": i.finetune,
            "voice_type": i.voice_type,
            "voice_type_name": voice_type_name(i.voice_type),
        });
        if let Some(m) = self.song.fm_macros.get(n - 1) {
            v["macro"] = json!({
                "length": m.length,
                "arpeggio_table": m.arpeggio_table,
                "vibrato_table": m.vibrato_table,
            });
        }
        if let Some(cols) = self.song.disabled_fm_columns.get(n - 1) {
            v["disabled_columns"] = json!(cols.to_vec());
        }
        Ok(v)
    }

    /// Every slot in brief: `{"count", "instruments": [{"instrument", "name",
    /// "empty", "voice_type"}]}`.
    pub fn instruments(&self) -> Value {
        let list: Vec<Value> = self
            .song
            .instruments
            .iter()
            .enumerate()
            .map(|(k, i)| {
                json!({
                    "instrument": k + 1,
                    "name": text(&i.name),
                    "empty": is_empty_instrument(i) && i.name.is_empty(),
                    "voice_type": i.voice_type,
                })
            })
            .collect();
        json!({"count": list.len(), "instruments": list})
    }

    /// Changes an instrument. The name is cut to the version's length; returns
    /// the name as stored.
    pub fn set_instrument(&mut self, n: usize, edit: &InstrumentEdit) -> Res<(String, Changes)> {
        let at = self.instrument_index(n)?;
        let lim = self.limits();
        let mut ins = self.song.instruments[at].clone();
        if let Some(name) = &edit.name {
            ins.name = encode_text(name, lim.name_len).0;
        }
        if let Some(p) = edit.panning {
            if p > 2 {
                return Err(format!(
                    "panning is 0 (centre), 1 (left) or 2 (right), not {p}"
                ));
            }
            if p != 0 && !lim.panning {
                return Err(format!(
                    "version {} has no instrument panning (it starts with version 5)",
                    lim.version
                ));
            }
            ins.panning = p;
        }
        if let Some(f) = edit.finetune {
            ins.finetune = f;
        }
        if let Some(t) = edit.voice_type {
            if t > 5 {
                return Err(format!("voice type is 0 (melodic) to 5 (hi-hat), not {t}"));
            }
            if t != 0 && !lim.voice_types {
                return Err(format!(
                    "version {} has no voice types (they start with version 9)",
                    lim.version
                ));
            }
            ins.voice_type = t;
        }
        if let Some(fm) = edit.fm {
            ins.fm = fm;
        }
        if !edit.voice.is_empty() {
            let v = edit.voice.apply(&Voice::decode(&ins.fm), true)?;
            ins.fm = v.encode(&ins.fm);
        }
        let stored = text(&ins.name);
        let ((), changes) = self.edit(|d| {
            if ins != d.song.instruments[at] {
                d.change(Change::Instrument {
                    index: at,
                    instrument: Box::new(ins),
                });
            }
            Ok(())
        })?;
        Ok((stored, changes))
    }

    /// Edits one operator of an instrument (`modulator` is operator 0).
    pub fn set_operator(&mut self, n: usize, carrier: bool, edit: &OperatorEdit) -> Res<Changes> {
        let mut v = VoiceEdit::default();
        if carrier {
            v.carrier = *edit;
        } else {
            v.modulator = *edit;
        }
        let (_, c) = self.set_instrument(
            n,
            &InstrumentEdit {
                voice: v,
                ..Default::default()
            },
        )?;
        Ok(c)
    }

    /// Swaps the two operators of an instrument (the feedback and connection stay).
    pub fn swap_operators(&mut self, n: usize) -> Res<Changes> {
        let at = self.instrument_index(n)?;
        let fm = self.song.instruments[at].fm;
        let fm = Voice::decode(&fm).swapped().encode(&fm);
        let (_, c) = self.set_instrument(
            n,
            &InstrumentEdit {
                fm: Some(fm),
                ..Default::default()
            },
        )?;
        Ok(c)
    }

    /// The plain sine voice (the DAW's "Reset"): panning, fine-tune and voice
    /// type are cleared, the name stays.
    pub fn reset_instrument(&mut self, n: usize) -> Res<Changes> {
        let at = self.instrument_index(n)?;
        let name = text(&self.song.instruments[at].name);
        let (_, c) = self.set_instrument(
            n,
            &InstrumentEdit {
                name: Some(name),
                fm: Some(PLAIN_FM),
                panning: Some(0),
                finetune: Some(0),
                voice_type: Some(0),
                ..Default::default()
            },
        )?;
        Ok(c)
    }

    /// Empties a slot (AT2's own idea of an unused one). The FM macro of the
    /// same number goes with it when `with_macro`; the name stays when
    /// `keep_name`.
    pub fn clear_instrument(
        &mut self,
        n: usize,
        keep_name: bool,
        with_macro: bool,
    ) -> Res<Changes> {
        let at = self.instrument_index(n)?;
        let mut ins = Instrument::default();
        if keep_name {
            ins.name = self.song.instruments[at].name.clone();
        }
        let macro_empty = self.limits().macros && with_macro;
        let ((), c) = self.edit(|d| {
            if ins != d.song.instruments[at] {
                d.change(Change::Instrument {
                    index: at,
                    instrument: Box::new(ins),
                });
            }
            if macro_empty {
                let empty = empty_fm_macro();
                if empty != d.song.fm_macros[at] {
                    d.change(Change::FmMacro {
                        index: at,
                        table: Box::new(empty),
                    });
                }
            }
            Ok(())
        })?;
        Ok(c)
    }

    /// Copies instrument `from` over `to`: its data, name and (version 9 up) its
    /// FM macro and disabled columns. The arpeggio and vibrato tables the macro
    /// names are shared, not copied.
    pub fn copy_instrument(&mut self, from: usize, to: usize) -> Res<Changes> {
        let (a, b) = (self.instrument_index(from)?, self.instrument_index(to)?);
        if a == b {
            return Err("copy an instrument onto another one".into());
        }
        let ins = self.song.instruments[a].clone();
        let m = self.song.fm_macros.get(a).cloned();
        let cols = self.song.disabled_fm_columns.get(a).copied();
        let ((), c) = self.edit(|d| {
            if ins != d.song.instruments[b] {
                d.change(Change::Instrument {
                    index: b,
                    instrument: Box::new(ins),
                });
            }
            if let Some(m) = m {
                if m != d.song.fm_macros[b] {
                    d.change(Change::FmMacro {
                        index: b,
                        table: Box::new(m),
                    });
                }
            }
            if let Some(columns) = cols {
                if d.song.disabled_fm_columns.get(b) != Some(&columns) {
                    d.change(Change::Columns { index: b, columns });
                }
            }
            Ok(())
        })?;
        Ok(c)
    }

    /// Sets which of an instrument's 28 FM register columns an FM macro may not
    /// write (version 11 and up): `columns` is 28 flags (0 or 1).
    pub fn set_disabled_columns(&mut self, n: usize, columns: [u8; 28]) -> Res<Changes> {
        let at = self.instrument_index(n)?;
        if !self.limits().pattern_names {
            return Err(format!(
                "version {} has no disabled FM columns (they start with version 11)",
                self.song.version
            ));
        }
        let ((), c) = self.edit(|d| {
            if d.song.disabled_fm_columns[at] != columns {
                d.change(Change::Columns { index: at, columns });
            }
            Ok(())
        })?;
        Ok(c)
    }

    // ----- macros ------------------------------------------------------------

    /// A macro table as JSON. FM: `{"kind", "macro", "length", "loop_begin",
    /// "loop_length", "keyoff_pos", "arpeggio_table", "vibrato_table", "steps":
    /// [{"step", "fm", "voice", "freq_slide", "panning", "duration"}]}` (the
    /// steps up to the last that is not empty); arpeggio and vibrato: the
    /// header fields and `"data"` the same way.
    pub fn macro_json(&self, kind: MacroKind, n: usize) -> Res<Value> {
        self.need_macros("macros")?;
        let at = macro_index(n)?;
        Ok(match kind {
            MacroKind::Fm => {
                let m = &self.song.fm_macros[at];
                let used = m
                    .steps
                    .iter()
                    .rposition(|s| *s != FmMacroStep::default())
                    .map_or(0, |i| i + 1)
                    .max(m.length as usize);
                let steps: Vec<Value> = m.steps[..used.min(255)]
                    .iter()
                    .enumerate()
                    .map(|(k, s)| {
                        json!({
                            "step": k + 1,
                            "fm": s.fm,
                            "voice": voice_json(&Voice::decode(&s.fm)),
                            "freq_slide": s.freq_slide,
                            "panning": s.panning,
                            "duration": s.duration,
                        })
                    })
                    .collect();
                json!({
                    "kind": "fm", "macro": n, "length": m.length,
                    "loop_begin": m.loop_begin, "loop_length": m.loop_length,
                    "keyoff_pos": m.keyoff_pos, "arpeggio_table": m.arpeggio_table,
                    "vibrato_table": m.vibrato_table, "steps": steps,
                })
            }
            MacroKind::Arpeggio => {
                let m = &self.song.arpeggio_macros[at];
                let used = m
                    .data
                    .iter()
                    .rposition(|&b| b != 0)
                    .map_or(0, |i| i + 1)
                    .max(m.length as usize);
                json!({
                    "kind": "arpeggio", "macro": n, "length": m.length, "speed": m.speed,
                    "loop_begin": m.loop_begin, "loop_length": m.loop_length,
                    "keyoff_pos": m.keyoff_pos, "data": m.data[..used.min(255)].to_vec(),
                })
            }
            MacroKind::Vibrato => {
                let m = &self.song.vibrato_macros[at];
                let used = m
                    .data
                    .iter()
                    .rposition(|&b| b != 0)
                    .map_or(0, |i| i + 1)
                    .max(m.length as usize);
                json!({
                    "kind": "vibrato", "macro": n, "length": m.length, "speed": m.speed,
                    "delay": m.delay, "loop_begin": m.loop_begin,
                    "loop_length": m.loop_length, "keyoff_pos": m.keyoff_pos,
                    "data": m.data[..used.min(255)].to_vec(),
                })
            }
        })
    }

    /// Which macros hold something, for a list: `{"fm": [n], "arpeggio": [n],
    /// "vibrato": [n]}` (version 9 and up; empty lists before).
    pub fn macros(&self) -> Value {
        let used = |v: Vec<bool>| -> Vec<usize> {
            v.into_iter()
                .enumerate()
                .filter(|(_, u)| *u)
                .map(|(i, _)| i + 1)
                .collect()
        };
        json!({
            "fm": used(self.song.fm_macros.iter().map(|m| *m != empty_fm_macro()).collect()),
            "arpeggio": used(self.song.arpeggio_macros.iter().map(|m| *m != empty_arpeggio()).collect()),
            "vibrato": used(self.song.vibrato_macros.iter().map(|m| *m != empty_vibrato()).collect()),
        })
    }

    pub fn set_fm_macro(&mut self, n: usize, edit: &FmMacroEdit) -> Res<Changes> {
        self.need_macros("FM macros")?;
        let at = macro_index(n)?;
        let mut m = self.song.fm_macros[at].clone();
        if let Some(v) = edit.length {
            m.length = v;
        }
        if let Some(v) = edit.loop_begin {
            m.loop_begin = v;
        }
        if let Some(v) = edit.loop_length {
            m.loop_length = v;
        }
        if let Some(v) = edit.keyoff_pos {
            m.keyoff_pos = v;
        }
        if let Some(v) = edit.arpeggio_table {
            m.arpeggio_table = v;
        }
        if let Some(v) = edit.vibrato_table {
            m.vibrato_table = v;
        }
        check_positions(m.length, m.loop_begin, m.loop_length, m.keyoff_pos)?;
        for (step, e) in &edit.steps {
            let s = &mut m.steps[entry_index(*step)?];
            if let Some(fm) = e.fm {
                s.fm = fm;
            }
            if !e.voice.is_empty() {
                let v = e
                    .voice
                    .apply(&Voice::decode(&s.fm), true)
                    .map_err(|x| format!("step {step}: {x}"))?;
                s.fm = v.encode(&s.fm);
            }
            if let Some(v) = e.freq_slide {
                s.freq_slide = v;
            }
            if let Some(p) = e.panning {
                if p > 2 {
                    return Err(format!("step {step}: panning is 0, 1 or 2, not {p}"));
                }
                s.panning = p;
            }
            if let Some(d) = e.duration {
                s.duration = d;
            }
        }
        let ((), c) = self.edit(|d| {
            if m != d.song.fm_macros[at] {
                d.change(Change::FmMacro {
                    index: at,
                    table: Box::new(m),
                });
            }
            Ok(())
        })?;
        Ok(c)
    }

    pub fn set_arpeggio_macro(&mut self, n: usize, edit: &ArpeggioEdit) -> Res<Changes> {
        self.need_macros("arpeggio macros")?;
        let at = macro_index(n)?;
        let mut m = self.song.arpeggio_macros[at].clone();
        if let Some(v) = edit.length {
            m.length = v;
        }
        if let Some(v) = edit.speed {
            m.speed = v;
        }
        if let Some(v) = edit.loop_begin {
            m.loop_begin = v;
        }
        if let Some(v) = edit.loop_length {
            m.loop_length = v;
        }
        if let Some(v) = edit.keyoff_pos {
            m.keyoff_pos = v;
        }
        check_positions(m.length, m.loop_begin, m.loop_length, m.keyoff_pos)?;
        for &(entry, value) in &edit.entries {
            if !matches!(value, 0..=96 | 0x81..=0xe1) {
                return Err(format!(
                    "entry {entry}: {value} is not an arpeggio step (0 base note, 1-96 semitones, 0x81-0xE1 a fixed note)"
                ));
            }
            m.data[entry_index(entry)?] = value;
        }
        let ((), c) = self.edit(|d| {
            if m != d.song.arpeggio_macros[at] {
                d.change(Change::ArpMacro {
                    index: at,
                    table: Box::new(m),
                });
            }
            Ok(())
        })?;
        Ok(c)
    }

    pub fn set_vibrato_macro(&mut self, n: usize, edit: &VibratoEdit) -> Res<Changes> {
        self.need_macros("vibrato macros")?;
        let at = macro_index(n)?;
        let mut m = self.song.vibrato_macros[at].clone();
        if let Some(v) = edit.length {
            m.length = v;
        }
        if let Some(v) = edit.speed {
            m.speed = v;
        }
        if let Some(v) = edit.delay {
            m.delay = v;
        }
        if let Some(v) = edit.loop_begin {
            m.loop_begin = v;
        }
        if let Some(v) = edit.loop_length {
            m.loop_length = v;
        }
        if let Some(v) = edit.keyoff_pos {
            m.keyoff_pos = v;
        }
        check_positions(m.length, m.loop_begin, m.loop_length, m.keyoff_pos)?;
        for &(entry, value) in &edit.entries {
            if value == i8::MIN {
                return Err(format!("entry {entry}: a vibrato step is -127 to 127"));
            }
            m.data[entry_index(entry)?] = value;
        }
        let ((), c) = self.edit(|d| {
            if m != d.song.vibrato_macros[at] {
                d.change(Change::VibMacro {
                    index: at,
                    table: Box::new(m),
                });
            }
            Ok(())
        })?;
        Ok(c)
    }

    /// Empties a macro table.
    pub fn clear_macro(&mut self, kind: MacroKind, n: usize) -> Res<Changes> {
        self.need_macros("macros")?;
        let at = macro_index(n)?;
        let ((), c) = self.edit(|d| {
            match kind {
                MacroKind::Fm => {
                    let e = empty_fm_macro();
                    if e != d.song.fm_macros[at] {
                        d.change(Change::FmMacro {
                            index: at,
                            table: Box::new(e),
                        });
                    }
                }
                MacroKind::Arpeggio => {
                    let e = empty_arpeggio();
                    if e != d.song.arpeggio_macros[at] {
                        d.change(Change::ArpMacro {
                            index: at,
                            table: Box::new(e),
                        });
                    }
                }
                MacroKind::Vibrato => {
                    let e = empty_vibrato();
                    if e != d.song.vibrato_macros[at] {
                        d.change(Change::VibMacro {
                            index: at,
                            table: Box::new(e),
                        });
                    }
                }
            }
            Ok(())
        })?;
        Ok(c)
    }
}

/// An empty FM macro (what AT2 initializes: nothing, 255 default steps).
pub fn empty_fm_macro() -> FmMacro {
    FmMacro {
        length: 0,
        loop_begin: 0,
        loop_length: 0,
        keyoff_pos: 0,
        arpeggio_table: 0,
        vibrato_table: 0,
        steps: vec![FmMacroStep::default(); 255],
    }
}

pub fn empty_arpeggio() -> ArpeggioMacro {
    ArpeggioMacro {
        length: 0,
        speed: 1,
        loop_begin: 0,
        loop_length: 0,
        keyoff_pos: 0,
        data: vec![0; 255],
    }
}

pub fn empty_vibrato() -> VibratoMacro {
    VibratoMacro {
        length: 0,
        speed: 1,
        delay: 0,
        loop_begin: 0,
        loop_length: 0,
        keyoff_pos: 0,
        data: vec![0; 255],
    }
}

// ---------------------------------------------------------------------------
// operation readers

/// An [`FmMacroEdit`] from an operation: `length`, `loop_begin`, `loop_length`,
/// `keyoff_pos`, `arpeggio_table`, `vibrato_table`, and `steps: [{"step": n,
/// "fm": [..] or voice fields, "freq_slide", "panning", "duration"}]`.
pub fn fm_macro_edit(op: &Value) -> Res<FmMacroEdit> {
    let mut e = FmMacroEdit {
        length: opt_u8(op, "length")?,
        loop_begin: opt_u8(op, "loop_begin")?,
        loop_length: opt_u8(op, "loop_length")?,
        keyoff_pos: opt_u8(op, "keyoff_pos")?,
        arpeggio_table: opt_u8(op, "arpeggio_table")?,
        vibrato_table: opt_u8(op, "vibrato_table")?,
        steps: Vec::new(),
    };
    if let Some(list) = field(op, "steps") {
        for s in list.as_array().ok_or("'steps' is a list of step edits")? {
            let step = req_usize(s, "step")?;
            e.steps.push((
                step,
                FmStepEdit {
                    fm: match field(s, "fm") {
                        Some(v) => Some(fm_bytes(v, "a step's 'fm'")?),
                        None => None,
                    },
                    voice: voice_edit(s)?,
                    freq_slide: match super::json::opt_i64(s, "freq_slide")? {
                        None => None,
                        Some(n) if (-32768..=32767).contains(&n) => Some(n as i16),
                        Some(n) => return Err(format!("freq_slide {n} is out of 16 bits")),
                    },
                    panning: opt_u8(s, "panning")?,
                    duration: opt_u8(s, "duration")?,
                },
            ));
        }
    }
    Ok(e)
}

fn entries(op: &Value) -> Res<Vec<(usize, i64)>> {
    let mut out = Vec::new();
    if let Some(list) = field(op, "data") {
        let list = list.as_array().ok_or("'data' is a list of numbers")?;
        for (i, v) in list.iter().enumerate() {
            out.push((i + 1, v.as_i64().ok_or("'data' holds whole numbers")?));
        }
    }
    if let Some(list) = field(op, "entries") {
        for e in list
            .as_array()
            .ok_or("'entries' is a list of {entry, value}")?
        {
            out.push((req_usize(e, "entry")?, super::json::req_i64(e, "value")?));
        }
    }
    Ok(out)
}

pub fn arpeggio_edit(op: &Value) -> Res<ArpeggioEdit> {
    let mut list = Vec::new();
    for (i, v) in entries(op)? {
        if !(0..=255).contains(&v) {
            return Err(format!("entry {i}: {v} is not a byte"));
        }
        list.push((i, v as u8));
    }
    Ok(ArpeggioEdit {
        length: opt_u8(op, "length")?,
        speed: opt_u8(op, "speed")?,
        loop_begin: opt_u8(op, "loop_begin")?,
        loop_length: opt_u8(op, "loop_length")?,
        keyoff_pos: opt_u8(op, "keyoff_pos")?,
        entries: list,
    })
}

pub fn vibrato_edit(op: &Value) -> Res<VibratoEdit> {
    let mut list = Vec::new();
    for (i, v) in entries(op)? {
        if !(-127..=127).contains(&v) {
            return Err(format!("entry {i}: {v} is not -127 to 127"));
        }
        list.push((i, v as i8));
    }
    Ok(VibratoEdit {
        length: opt_u8(op, "length")?,
        speed: opt_u8(op, "speed")?,
        delay: opt_u8(op, "delay")?,
        loop_begin: opt_u8(op, "loop_begin")?,
        loop_length: opt_u8(op, "loop_length")?,
        keyoff_pos: opt_u8(op, "keyoff_pos")?,
        entries: list,
    })
}

/// `columns: [28 flags]` of an operation.
pub fn column_flags(op: &Value) -> Res<[u8; 28]> {
    let a = field(op, "columns")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 28)
        .ok_or("'columns' is 28 flags (0 or 1)")?;
    let mut out = [0u8; 28];
    for (o, v) in out.iter_mut().zip(a) {
        *o = match v {
            Value::Bool(b) => *b as u8,
            v => v
                .as_u64()
                .filter(|&n| n <= 1)
                .ok_or("'columns' holds 0 or 1")? as u8,
        };
    }
    Ok(out)
}
