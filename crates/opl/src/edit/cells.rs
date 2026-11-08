//! Editing patterns: cells, ranges, rows, the clipboard.

use serde_json::{json, Value};

use super::history::{Change, Changes};
use super::json::{byte_value, field, opt_bool, opt_u8, Res};
use super::limits::{
    check_cell, effect_char, note_from_json, note_to_json, parse_effect, parse_note, Limits,
    FIXED_FLAG, NOTE_TYPED_MAX,
};
use super::A2mDoc;
use crate::a2::model::{Cell, Pattern};

/// A rectangle of cells in one pattern: rows `row0..=row1` of tracks
/// `channel0..=channel1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub pattern: usize,
    pub row0: usize,
    pub row1: usize,
    pub channel0: usize,
    pub channel1: usize,
}

impl Region {
    pub fn rows(&self) -> usize {
        self.row1 - self.row0 + 1
    }

    pub fn channels(&self) -> usize {
        self.channel1 - self.channel0 + 1
    }
}

/// Which columns a clear touches (`effect` clears the first effect column and
/// its parameter, `effect2` the second).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClearMask {
    pub note: bool,
    pub instrument: bool,
    pub effect: bool,
    pub effect2: bool,
}

impl ClearMask {
    pub const ALL: ClearMask = ClearMask {
        note: true,
        instrument: true,
        effect: true,
        effect2: true,
    };

    /// From a list of column names (`"note"`, `"instrument"`, `"effect"`,
    /// `"effect2"`, or `"effects"` for both).
    pub fn from_names(names: &[&str]) -> Res<ClearMask> {
        let mut m = ClearMask {
            note: false,
            instrument: false,
            effect: false,
            effect2: false,
        };
        for n in names {
            match *n {
                "note" => m.note = true,
                "instrument" => m.instrument = true,
                "effect" | "param" => m.effect = true,
                "effect2" | "param2" => m.effect2 = true,
                "effects" => {
                    m.effect = true;
                    m.effect2 = true;
                }
                other => {
                    return Err(format!(
                        "'{other}' is not a column (note, instrument, effect, effect2, effects)"
                    ))
                }
            }
        }
        Ok(m)
    }
}

/// How a paste combines with what is there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PasteMode {
    /// The pasted cells replace the cells under them.
    #[default]
    Overwrite,
    /// A pasted field fills only a field that is empty (note, instrument, each
    /// effect column with its parameter); fields already there stay.
    Mix,
}

/// Cells copied out of a pattern, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub rows: usize,
    pub channels: usize,
    pub cells: Vec<Cell>,
}

/// A cell as the compact array every read returns: `[note, instrument,
/// effect, param, effect2, param2]`. The note is a MIDI number (60 is middle C),
/// 0 for none, 255 for a key off; see [`note_to_json`] for fixed notes.
pub fn cell_json(cell: &Cell) -> Value {
    json!([
        note_to_json(cell.note),
        cell.instrument,
        cell.effects[0].0,
        cell.effects[0].1,
        cell.effects[1].0,
        cell.effects[1].1
    ])
}

/// An effect field: the command number, or its letter (`"C"`, `"0"`).
fn effect_value(v: &Value) -> Res<u8> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .filter(|&n| n <= 255)
            .map(|n| n as u8)
            .ok_or_else(|| "an effect is 0-255".to_string()),
        Value::String(s) => parse_effect(s)
            .ok_or_else(|| format!("'{s}' is not an effect letter (0-9, A-Z, &%!@=#$~^`><)")),
        _ => Err("an effect is a number or a letter".into()),
    }
}

/// A note field: a number (0 none, 255 off, else MIDI) or text (`"C-4"`).
fn note_value(lim: &Limits, v: &Value) -> Res<u8> {
    match v {
        Value::Number(n) => note_from_json(n.as_u64().ok_or("a note is not negative")?, lim),
        Value::String(s) => parse_note(s, lim).ok_or_else(|| {
            format!("'{s}' is not a note (C-0 to B-7, '###' for a key off, '=' marks a fixed note)")
        }),
        _ => Err("a note is a number or text like \"C-4\"".into()),
    }
}

/// A cell from its compact array (six numbers; the second effect column may be
/// left out in a song that has one column).
pub(crate) fn cell_from_json(lim: &Limits, v: &Value) -> Res<Cell> {
    let a = v
        .as_array()
        .ok_or("a cell is [note, instrument, effect, param, effect2, param2]")?;
    if a.len() != 6 && a.len() != 4 {
        return Err("a cell is [note, instrument, effect, param, effect2, param2]".into());
    }
    let cell = Cell {
        note: note_value(lim, &a[0])?,
        instrument: byte_value(&a[1], "an instrument")?,
        effects: [
            (effect_value(&a[2])?, byte_value(&a[3], "a parameter")?),
            if a.len() == 6 {
                (effect_value(&a[4])?, byte_value(&a[5], "a parameter")?)
            } else {
                (0, 0)
            },
        ],
    };
    check_cell(lim, &cell)?;
    Ok(cell)
}

/// The fields of one cell an edit changes (`None` leaves a field alone).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellUpdate {
    /// The note byte as the file holds it.
    pub note: Option<u8>,
    pub instrument: Option<u8>,
    pub effect: [Option<u8>; 2],
    pub param: [Option<u8>; 2],
}

impl CellUpdate {
    /// Every field of `cell`, to replace a cell outright.
    pub fn whole(cell: &Cell) -> CellUpdate {
        CellUpdate {
            note: Some(cell.note),
            instrument: Some(cell.instrument),
            effect: [Some(cell.effects[0].0), Some(cell.effects[1].0)],
            param: [Some(cell.effects[0].1), Some(cell.effects[1].1)],
        }
    }

    /// From an operation: `cell: [n, i, e, p, e2, p2]` for all six, or any of
    /// `note`, `instrument`, `effect`, `param`, `effect2`, `param2`; with
    /// `fixed: true` a note is a fixed note (version 9+).
    pub(crate) fn from_op(lim: &Limits, op: &Value) -> Res<CellUpdate> {
        let mut u = CellUpdate::default();
        if let Some(cell) = field(op, "cell") {
            u = CellUpdate::whole(&cell_from_json(lim, cell)?);
        }
        if let Some(v) = field(op, "note") {
            u.note = Some(note_value(lim, v)?);
        }
        if opt_bool(op, "fixed")? == Some(true) {
            let note = u.note.ok_or("'fixed' needs a note")?;
            if !(1..=97).contains(&note) {
                return Err("only a plain note can be fixed".into());
            }
            if !lim.fixed_notes {
                return Err(format!(
                    "version {} has no fixed notes (they start with version 9)",
                    lim.version
                ));
            }
            u.note = Some(note + FIXED_FLAG);
        }
        if let Some(n) = opt_u8(op, "instrument")? {
            u.instrument = Some(n);
        }
        if let Some(v) = field(op, "effect") {
            u.effect[0] = Some(effect_value(v)?);
        }
        if let Some(n) = opt_u8(op, "param")? {
            u.param[0] = Some(n);
        }
        if let Some(v) = field(op, "effect2") {
            u.effect[1] = Some(effect_value(v)?);
        }
        if let Some(n) = opt_u8(op, "param2")? {
            u.param[1] = Some(n);
        }
        Ok(u)
    }
}

impl Clip {
    /// `{"rows", "channels", "cells": [[[n, i, e, p, e2, p2], ...], ...]}`
    /// (cells by row, then track).
    pub fn to_json(&self) -> Value {
        let rows: Vec<Value> = (0..self.rows)
            .map(|r| {
                Value::Array(
                    (0..self.channels)
                        .map(|c| cell_json(&self.cells[r * self.channels + c]))
                        .collect(),
                )
            })
            .collect();
        json!({"format": "a2m", "rows": self.rows, "channels": self.channels, "cells": rows})
    }

    /// Reads what [`Clip::to_json`] wrote (the `cells` array alone is enough).
    pub fn from_json(lim: &Limits, v: &Value) -> Res<Clip> {
        let cells = v.get("cells").unwrap_or(v);
        let rows = cells
            .as_array()
            .ok_or("a clip is {\"cells\": [[[n,i,e,p,e2,p2],..],..]}")?;
        let channels = rows.first().and_then(Value::as_array).map_or(0, Vec::len);
        if rows.is_empty() || channels == 0 {
            return Err("the clip is empty".into());
        }
        let mut out = Vec::with_capacity(rows.len() * channels);
        for (r, row) in rows.iter().enumerate() {
            let row = row.as_array().ok_or("a clip row is a list of cells")?;
            if row.len() != channels {
                return Err(format!(
                    "clip row {r} has {} cells, the first has {channels}",
                    row.len()
                ));
            }
            for cell in row {
                out.push(cell_from_json(lim, cell)?);
            }
        }
        Ok(Clip {
            rows: rows.len(),
            channels,
            cells: out,
        })
    }
}

/// What a paste did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pasted {
    pub rows: usize,
    pub channels: usize,
    /// Part of the clip fell outside the pattern and was left out.
    pub clipped: bool,
}

impl A2mDoc {
    pub(crate) fn pattern_ref(&self, pattern: usize) -> Res<&Pattern> {
        self.song.patterns.get(pattern).ok_or_else(|| {
            format!(
                "pattern {pattern} does not exist (the song has {})",
                self.song.patterns.len()
            )
        })
    }

    /// A region, checked against its pattern's rows (`pattern_len`) and the
    /// song's tracks.
    pub fn region(
        &self,
        pattern: usize,
        rows: (usize, usize),
        channels: Option<(usize, usize)>,
    ) -> Res<Region> {
        self.pattern_ref(pattern)?;
        let (row0, row1) = (rows.0.min(rows.1), rows.0.max(rows.1));
        let tracks = self.tracks();
        let (channel0, channel1) = channels.unwrap_or((0, tracks - 1));
        let (channel0, channel1) = (channel0.min(channel1), channel0.max(channel1));
        if row1 >= self.pattern_len() {
            return Err(format!(
                "row {row1} is past the end of pattern {pattern} ({} rows)",
                self.pattern_len()
            ));
        }
        if channel1 >= tracks {
            return Err(format!(
                "track {channel1} does not exist (the song has {tracks}; tracks count from 0)"
            ));
        }
        Ok(Region {
            pattern,
            row0,
            row1,
            channel0,
            channel1,
        })
    }

    /// A region from an operation (`pattern`, `row0`/`row`, `row1`,
    /// `channel0`/`channel`, `channel1`); the default is the whole pattern.
    pub(crate) fn region_from_op(&self, op: &Value) -> Res<Region> {
        use super::json::{opt_usize, req_usize};
        let pattern = req_usize(op, "pattern")?;
        self.pattern_ref(pattern)?;
        let row0 = opt_usize(op, "row0")?
            .or(opt_usize(op, "row")?)
            .unwrap_or(0);
        let row1 = opt_usize(op, "row1")?.unwrap_or(self.pattern_len() - 1);
        let c0 = opt_usize(op, "channel0")?.or(opt_usize(op, "channel")?);
        let c1 = opt_usize(op, "channel1")?;
        let channels = match (c0, c1) {
            (None, None) => None,
            (Some(a), None) => Some((a, a)),
            (None, Some(b)) => Some((0, b)),
            (Some(a), Some(b)) => Some((a, b)),
        };
        self.region(pattern, (row0, row1), channels)
    }

    fn block_of(&self, r: &Region) -> Vec<Cell> {
        let p = &self.song.patterns[r.pattern];
        let mut cells = Vec::with_capacity(r.rows() * r.channels());
        for row in r.row0..=r.row1 {
            for ch in r.channel0..=r.channel1 {
                cells.push(*p.cell(row, ch));
            }
        }
        cells
    }

    fn put_block(&mut self, r: &Region, cells: Vec<Cell>) {
        self.change(Change::Block {
            pattern: r.pattern,
            row0: r.row0,
            channel0: r.channel0,
            rows: r.rows(),
            channels: r.channels(),
            cells,
        });
    }

    /// The cell after `update`.
    fn updated(&self, cell: &Cell, u: &CellUpdate) -> Res<Cell> {
        let mut c = *cell;
        if let Some(n) = u.note {
            c.note = n;
        }
        if let Some(i) = u.instrument {
            c.instrument = i;
        }
        for k in 0..2 {
            if let Some(e) = u.effect[k] {
                c.effects[k].0 = e;
                // A new effect without a parameter clears the old one's.
                if u.param[k].is_none() {
                    c.effects[k].1 = 0;
                }
            }
            if let Some(p) = u.param[k] {
                c.effects[k].1 = p;
            }
        }
        check_cell(&self.limits(), &c)?;
        Ok(c)
    }

    /// Changes fields of one cell. Returns the cell as stored.
    pub fn set_cell(
        &mut self,
        pattern: usize,
        row: usize,
        channel: usize,
        update: CellUpdate,
    ) -> Res<(Cell, Changes)> {
        let r = self.region(pattern, (row, row), Some((channel, channel)))?;
        let old = *self.song.patterns[pattern].cell(row, channel);
        let new = self.updated(&old, &update)?;
        let ((), changes) = self.edit(|d| {
            if new != old {
                d.put_block(&r, vec![new]);
            }
            Ok(())
        })?;
        Ok((new, changes))
    }

    /// Clears the columns of `mask` in a region.
    pub fn clear_range(&mut self, region: Region, mask: ClearMask) -> Res<Changes> {
        let old = self.block_of(&region);
        let new: Vec<Cell> = old
            .iter()
            .map(|c| {
                let mut n = *c;
                if mask.note {
                    n.note = 0;
                }
                if mask.instrument {
                    n.instrument = 0;
                }
                if mask.effect {
                    n.effects[0] = (0, 0);
                }
                if mask.effect2 {
                    n.effects[1] = (0, 0);
                }
                n
            })
            .collect();
        let ((), changes) = self.edit(|d| {
            if new != old {
                d.put_block(&region, new);
            }
            Ok(())
        })?;
        Ok(changes)
    }

    /// Inserts `count` empty rows at `row`, pushing the rows below down in the
    /// tracks given (all by default); rows pushed past the end are lost.
    pub fn insert_row(
        &mut self,
        pattern: usize,
        row: usize,
        channels: Option<(usize, usize)>,
        count: usize,
    ) -> Res<Changes> {
        self.shift_rows(pattern, row, channels, count, true)
    }

    /// Deletes `count` rows at `row`, pulling the rows below up in the tracks
    /// given (all by default); the rows at the end become empty.
    pub fn delete_row(
        &mut self,
        pattern: usize,
        row: usize,
        channels: Option<(usize, usize)>,
        count: usize,
    ) -> Res<Changes> {
        self.shift_rows(pattern, row, channels, count, false)
    }

    fn shift_rows(
        &mut self,
        pattern: usize,
        row: usize,
        channels: Option<(usize, usize)>,
        count: usize,
        insert: bool,
    ) -> Res<Changes> {
        if count == 0 {
            return Err("count must be at least 1".into());
        }
        let total = self.pattern_len();
        let r = self.region(pattern, (row, total - 1), channels)?;
        let n = r.rows();
        let k = count.min(n);
        let old = self.block_of(&r);
        let w = r.channels();
        let mut new = vec![Cell::default(); old.len()];
        for i in 0..n {
            let src = if insert {
                i.checked_sub(k)
            } else {
                (i + k < n).then_some(i + k)
            };
            if let Some(src) = src {
                new[i * w..(i + 1) * w].copy_from_slice(&old[src * w..(src + 1) * w]);
            }
        }
        let ((), changes) = self.edit(|d| {
            if new != old {
                d.put_block(&r, new);
            }
            Ok(())
        })?;
        Ok(changes)
    }

    /// Moves the notes of a region by `semitones`. A plain note that would
    /// leave C-0 to B-7 refuses the whole edit, or is held at the edge when
    /// `clamp` is set. Fixed notes, key offs and note 97 (C-8) stay (AT2's
    /// editor does not transpose a fixed note). `instrument` restricts it to cells that play
    /// that instrument. Returns the number of cells changed.
    pub fn transpose(
        &mut self,
        region: Region,
        semitones: i32,
        clamp: bool,
        instrument: Option<u8>,
    ) -> Res<(usize, Changes)> {
        let old = self.block_of(&region);
        let mut new = old.clone();
        let mut count = 0;
        for (i, c) in new.iter_mut().enumerate() {
            if !(1..=NOTE_TYPED_MAX).contains(&c.note)
                || instrument.is_some_and(|n| c.instrument != n)
            {
                continue;
            }
            let moved = c.note as i32 + semitones;
            let target = if (1..=NOTE_TYPED_MAX as i32).contains(&moved) {
                moved
            } else if clamp {
                moved.clamp(1, NOTE_TYPED_MAX as i32)
            } else {
                let (row, ch) = (
                    region.row0 + i / region.channels(),
                    region.channel0 + i % region.channels(),
                );
                return Err(format!(
                    "transposing by {semitones} takes row {row}, track {ch} out of C-0 to B-7"
                ));
            };
            if target != c.note as i32 {
                c.note = target as u8;
                count += 1;
            }
        }
        let ((), changes) = self.edit(|d| {
            if new != old {
                d.put_block(&region, new);
            }
            Ok(())
        })?;
        Ok((count, changes))
    }

    /// Copies a region out.
    pub fn copy_range(&self, region: Region) -> Res<Clip> {
        Ok(Clip {
            rows: region.rows(),
            channels: region.channels(),
            cells: self.block_of(&region),
        })
    }

    /// Pastes a clip with its top left cell at `row`, `channel`; what runs past
    /// the pattern or the tracks is left out.
    pub fn paste(
        &mut self,
        pattern: usize,
        row: usize,
        channel: usize,
        clip: &Clip,
        mode: PasteMode,
    ) -> Res<(Pasted, Changes)> {
        let rows = clip.rows.min(self.pattern_len().saturating_sub(row));
        let channels = clip.channels.min(self.tracks().saturating_sub(channel));
        if rows == 0 || channels == 0 {
            return Err("the paste position is outside the pattern".into());
        }
        let r = self.region(
            pattern,
            (row, row + rows - 1),
            Some((channel, channel + channels - 1)),
        )?;
        let old = self.block_of(&r);
        let lim = self.limits();
        let mut new = Vec::with_capacity(old.len());
        for i in 0..rows {
            for j in 0..channels {
                let have = old[i * channels + j];
                let put = clip.cells[i * clip.channels + j];
                check_cell(&lim, &put)?;
                new.push(match mode {
                    PasteMode::Overwrite => put,
                    PasteMode::Mix => Cell {
                        note: if have.note == 0 { put.note } else { have.note },
                        instrument: if have.instrument == 0 {
                            put.instrument
                        } else {
                            have.instrument
                        },
                        effects: [0, 1].map(|k| {
                            if have.effects[k] == (0, 0) {
                                put.effects[k]
                            } else {
                                have.effects[k]
                            }
                        }),
                    },
                });
            }
        }
        let ((), changes) = self.edit(|d| {
            if new != old {
                d.put_block(&r, new);
            }
            Ok(())
        })?;
        Ok((
            Pasted {
                rows,
                channels,
                clipped: rows < clip.rows || channels < clip.channels,
            },
            changes,
        ))
    }
}

/// An effect command as the text a tracker shows: its letter and the
/// parameter in hex (`"F06"`), empty for none.
pub fn effect_text(fx: u8, param: u8) -> String {
    if (fx, param) == (0, 0) {
        return String::new();
    }
    match effect_char(fx) {
        Some(c) => format!("{c}{param:02X}"),
        None => format!("?{param:02X}"),
    }
}
