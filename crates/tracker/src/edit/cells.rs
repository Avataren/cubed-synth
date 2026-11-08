//! Editing patterns: cells, ranges, rows, the clipboard, the pattern list.

use serde_json::{json, Value};

use super::history::{Change, Changes};
use super::json::{cell_from_json, cell_json, Res};
use super::limits::{check_cell, limits, note_name, note_range};
use super::ModuleDoc;
use crate::module::{Cell, Format, Note, Pattern};
use crate::write::mod_period_for_note;

pub use super::json::CellUpdate;

/// A rectangle of cells in one pattern: rows `row0..=row1` of channels
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

/// Which columns a clear touches (`effect` clears the parameter too).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClearMask {
    pub note: bool,
    pub instrument: bool,
    pub volume: bool,
    pub effect: bool,
}

impl ClearMask {
    pub const ALL: ClearMask = ClearMask {
        note: true,
        instrument: true,
        volume: true,
        effect: true,
    };

    /// From a list of column names (`"note"`, `"instrument"`, `"volume"`,
    /// `"effect"`).
    pub fn from_names(names: &[&str]) -> Res<ClearMask> {
        let mut m = ClearMask {
            note: false,
            instrument: false,
            volume: false,
            effect: false,
        };
        for n in names {
            match *n {
                "note" => m.note = true,
                "instrument" | "sample" => m.instrument = true,
                "volume" => m.volume = true,
                "effect" | "param" => m.effect = true,
                other => {
                    return Err(format!(
                        "'{other}' is not a column (note, instrument, volume, effect)"
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
    /// A pasted field fills only a field that is empty (note, instrument,
    /// volume column, effect with its parameter); fields already there stay.
    Mix,
}

/// Cells copied out of a pattern: row by row, with a MOD's exact period
/// where it is not the table's.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub rows: usize,
    pub channels: usize,
    pub cells: Vec<(Cell, Option<u16>)>,
}

impl Clip {
    /// `{"format", "rows", "channels", "cells": [[[n, i, v, e, p], ...], ...]}`
    /// (cells by row, then channel; a MOD cell whose period is not the
    /// table's has the period as a sixth number).
    pub fn to_json(&self, format: Format) -> Value {
        let rows: Vec<Value> = (0..self.rows)
            .map(|r| {
                Value::Array(
                    (0..self.channels)
                        .map(|c| {
                            let (cell, period) = &self.cells[r * self.channels + c];
                            let mut v = cell_json(cell);
                            if let Some(p) = period {
                                v.as_array_mut().unwrap().push(json!(p));
                            }
                            v
                        })
                        .collect(),
                )
            })
            .collect();
        json!({
            "format": match format { Format::Mod => "mod", Format::S3m => "s3m", Format::Xm => "xm" },
            "rows": self.rows,
            "channels": self.channels,
            "cells": rows,
        })
    }

    /// Reads what [`Clip::to_json`] wrote (the `cells` array alone is enough).
    pub fn from_json(format: Format, v: &Value) -> Res<Clip> {
        let cells = v.get("cells").unwrap_or(v);
        let rows = cells
            .as_array()
            .ok_or("a clip is {\"cells\": [[[n,i,v,e,p],..],..]}")?;
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
                out.push(cell_from_json(format, cell)?);
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

/// The cell a MOD stores for a note: its table period.
fn mod_cell_note(period_hint: Option<u16>, note: Note, amiga_limits: bool) -> (Note, u16) {
    match note {
        Note::On(n) => {
            let table = mod_period_for_note(n, amiga_limits).unwrap_or(0);
            // A pasted exact period stays when it still names the same note.
            let period = match period_hint {
                Some(p) if p != 0 && table != 0 => p,
                _ => table,
            };
            (note, period)
        }
        other => (other, 0),
    }
}

impl ModuleDoc {
    fn pattern_ref(&self, pattern: usize) -> Res<&Pattern> {
        self.module.patterns.get(pattern).ok_or_else(|| {
            format!(
                "pattern {pattern} does not exist (the module has {})",
                self.module.patterns.len()
            )
        })
    }

    /// A region, checked against its pattern.
    pub fn region(
        &self,
        pattern: usize,
        rows: (usize, usize),
        channels: Option<(usize, usize)>,
    ) -> Res<Region> {
        let p = self.pattern_ref(pattern)?;
        let (row0, row1) = (rows.0.min(rows.1), rows.0.max(rows.1));
        let (channel0, channel1) = channels.unwrap_or((0, p.channels() - 1));
        let (channel0, channel1) = (channel0.min(channel1), channel0.max(channel1));
        if row1 >= p.rows() {
            return Err(format!(
                "row {row1} is past the end of pattern {pattern} ({} rows)",
                p.rows()
            ));
        }
        if channel1 >= p.channels() {
            return Err(format!(
                "channel {channel1} does not exist (the module has {}; channels count from 0)",
                p.channels()
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

    /// A region of a whole row range in every channel from an operation.
    pub(crate) fn region_from_op(&self, op: &Value) -> Res<Region> {
        use super::json::{opt_usize, req_usize};
        let pattern = req_usize(op, "pattern")?;
        let p = self.pattern_ref(pattern)?;
        let row0 = opt_usize(op, "row0")?
            .or(opt_usize(op, "row")?)
            .unwrap_or(0);
        let row1 = opt_usize(op, "row1")?.unwrap_or(p.rows() - 1);
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
        let p = &self.module.patterns[r.pattern];
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

    /// The cell after `update` (a MOD's period follows its note).
    fn updated(&self, cell: &Cell, u: &CellUpdate, period: Option<u16>) -> Res<Cell> {
        let mut c = *cell;
        if let Some(note) = u.note {
            if self.module.format == Format::Mod {
                let (n, p) = mod_cell_note(period, note, self.module.flags.amiga_limits);
                c.note = n;
                c.period = p;
            } else {
                c.note = note;
            }
        }
        if let Some(i) = u.instrument {
            c.instrument = i;
        }
        if let Some(v) = u.volume {
            c.volume = v;
        }
        if let Some(e) = u.effect {
            c.effect = e;
            // A new effect without a parameter clears the old one's.
            if u.param.is_none() {
                c.param = 0;
            }
        }
        if let Some(p) = u.param {
            c.param = p;
        }
        check_cell(&self.module, &c)?;
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
        let old = *self.module.patterns[pattern].cell(row, channel);
        let new = self.updated(&old, &update, None)?;
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
                    n.note = Note::None;
                    n.period = 0;
                }
                if mask.instrument {
                    n.instrument = 0;
                }
                if mask.volume {
                    n.volume = 0;
                }
                if mask.effect {
                    n.effect = 0;
                    n.param = 0;
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

    /// Inserts `count` empty rows at `row`, pushing the rows below down in
    /// the channels given (all by default); rows pushed past the end of the
    /// pattern are lost.
    pub fn insert_row(
        &mut self,
        pattern: usize,
        row: usize,
        channels: Option<(usize, usize)>,
        count: usize,
    ) -> Res<Changes> {
        self.shift_rows(pattern, row, channels, count, true)
    }

    /// Deletes `count` rows at `row`, pulling the rows below up in the
    /// channels given (all by default); the rows at the end become empty.
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
        let total = self.pattern_ref(pattern)?.rows();
        if count == 0 {
            return Err("count must be at least 1".into());
        }
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

    /// Moves the notes of a region by `semitones`. A note that would leave
    /// the format's range refuses the whole edit, or is held at the range's
    /// edge when `clamp` is set. `instrument` restricts it to cells that
    /// play that instrument. Returns the number of cells changed.
    pub fn transpose(
        &mut self,
        region: Region,
        semitones: i32,
        clamp: bool,
        instrument: Option<u8>,
    ) -> Res<(usize, Changes)> {
        let (lo, hi) = note_range(&self.module);
        let format = self.module.format;
        let amiga = self.module.flags.amiga_limits;
        let old = self.block_of(&region);
        let w = region.channels();
        let mut new = old.clone();
        let mut count = 0;
        for (i, cell) in new.iter_mut().enumerate() {
            let Note::On(n) = cell.note else { continue };
            if instrument.is_some_and(|x| x != cell.instrument) {
                continue;
            }
            let moved = n as i32 + semitones;
            let moved = if (lo as i32..=hi as i32).contains(&moved) {
                moved
            } else if clamp {
                moved.clamp(lo as i32, hi as i32)
            } else {
                let (row, ch) = (region.row0 + i / w, region.channel0 + i % w);
                return Err(format!(
                    "{} moved by {semitones} leaves {}'s range ({} to {}) at pattern {}, row {row}, channel {}",
                    note_name(format, n),
                    super::limits::format_name(format),
                    note_name(format, lo),
                    note_name(format, hi),
                    region.pattern,
                    ch + 1
                ));
            };
            if moved != n as i32 {
                count += 1;
            }
            cell.note = Note::On(moved as u8);
            if format == Format::Mod {
                cell.period = mod_period_for_note(moved as u8, amiga).unwrap_or(0);
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

    /// The cells of a region, for a clipboard.
    pub fn copy_range(&self, region: Region) -> Res<Clip> {
        // A region is checked when made; patterns may have changed since.
        self.region(
            region.pattern,
            (region.row0, region.row1),
            Some((region.channel0, region.channel1)),
        )?;
        let p = &self.module.patterns[region.pattern];
        let amiga = self.module.flags.amiga_limits;
        let mut cells = Vec::with_capacity(region.rows() * region.channels());
        for row in region.row0..=region.row1 {
            for ch in region.channel0..=region.channel1 {
                let c = *p.cell(row, ch);
                let table = match c.note {
                    Note::On(n) => mod_period_for_note(n, amiga),
                    _ => None,
                };
                let period =
                    (self.module.format == Format::Mod && c.period != 0 && Some(c.period) != table)
                        .then_some(c.period);
                cells.push((c, period));
            }
        }
        Ok(Clip {
            rows: region.rows(),
            channels: region.channels(),
            cells,
        })
    }

    /// Writes a clip with its top-left cell at (`row`, `channel`). The part
    /// that falls outside the pattern is left out (`clipped`). Every pasted
    /// cell is checked against the module's format first: one that does not
    /// fit refuses the whole paste.
    pub fn paste(
        &mut self,
        pattern: usize,
        row: usize,
        channel: usize,
        clip: &Clip,
        mode: PasteMode,
    ) -> Res<(Pasted, Changes)> {
        let p = self.pattern_ref(pattern)?;
        if row >= p.rows() || channel >= p.channels() {
            return Err(format!(
                "row {row}, channel {channel} is outside pattern {pattern}"
            ));
        }
        let rows = clip.rows.min(p.rows() - row);
        let channels = clip.channels.min(p.channels() - channel);
        let r = Region {
            pattern,
            row0: row,
            row1: row + rows - 1,
            channel0: channel,
            channel1: channel + channels - 1,
        };
        let old = self.block_of(&r);
        let mut new = Vec::with_capacity(old.len());
        for rr in 0..rows {
            for cc in 0..channels {
                let (src, period) = &clip.cells[rr * clip.channels + cc];
                let dst = old[rr * channels + cc];
                let update = match mode {
                    PasteMode::Overwrite => CellUpdate::whole(src),
                    PasteMode::Mix => CellUpdate {
                        note: (dst.note == Note::None).then_some(src.note),
                        instrument: (dst.instrument == 0).then_some(src.instrument),
                        volume: (dst.volume == 0).then_some(src.volume),
                        effect: (dst.effect == 0 && dst.param == 0).then_some(src.effect),
                        param: (dst.effect == 0 && dst.param == 0).then_some(src.param),
                    },
                };
                let base = match mode {
                    PasteMode::Overwrite => Cell::default(),
                    PasteMode::Mix => dst,
                };
                let c = self.updated(&base, &update, *period).map_err(|e| {
                    format!(
                        "pasted cell (row {rr}, channel {cc}) does not fit {}: {e}",
                        super::limits::format_name(self.module.format)
                    )
                })?;
                new.push(c);
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

    // ----- the pattern list -------------------------------------------------

    /// Gives a pattern `rows` rows (XM only). Shortening drops the rows past
    /// the new end: refused when they hold notes unless `force`.
    pub fn set_pattern_rows(&mut self, pattern: usize, rows: usize, force: bool) -> Res<Changes> {
        let lim = limits(&self.module);
        let p = self.pattern_ref(pattern)?;
        if !lim.variable_rows {
            return Err(format!(
                "{} pattern always has {} rows",
                super::limits::format_name(self.module.format),
                lim.max_rows
            ));
        }
        if rows < lim.min_rows || rows > lim.max_rows {
            return Err(format!(
                "a pattern has {}-{} rows, not {rows}",
                lim.min_rows, lim.max_rows
            ));
        }
        if rows == p.rows() {
            return Ok(Changes::default());
        }
        if rows < p.rows() && !force {
            for row in rows..p.rows() {
                if p.row(row).iter().any(|c| !c.is_empty()) {
                    return Err(format!(
                        "row {row} of pattern {pattern} has data that rows={rows} would drop (pass force to drop it)"
                    ));
                }
            }
        }
        let mut new = Pattern::new(rows, p.channels());
        for row in 0..rows.min(p.rows()) {
            for ch in 0..p.channels() {
                *new.cell_mut(row, ch) = *p.cell(row, ch);
            }
        }
        let ((), changes) = self.edit(|d| {
            d.change(Change::Pattern {
                index: pattern,
                pattern: new,
            });
            Ok(())
        })?;
        Ok(changes)
    }

    fn default_rows(&self) -> usize {
        self.module
            .patterns
            .last()
            .map_or(64, |p| p.rows())
            .clamp(limits(&self.module).min_rows, limits(&self.module).max_rows)
    }

    /// Appends an empty pattern (the length of the last one by default).
    pub fn new_pattern(&mut self, rows: Option<usize>) -> Res<(usize, Changes)> {
        let lim = limits(&self.module);
        let rows = rows.unwrap_or_else(|| self.default_rows());
        if self.module.patterns.len() >= lim.max_patterns {
            return Err(format!(
                "{} holds {} patterns",
                super::limits::format_name(self.module.format),
                lim.max_patterns
            ));
        }
        if rows < lim.min_rows || rows > lim.max_rows {
            return Err(format!(
                "a pattern has {}-{} rows, not {rows}",
                lim.min_rows, lim.max_rows
            ));
        }
        let index = self.module.patterns.len();
        let pattern = Pattern::new(rows, self.module.channels);
        let ((), changes) = self.edit(|d| {
            d.change(Change::PatternInsert { index, pattern });
            Ok(())
        })?;
        Ok((index, changes))
    }

    /// Appends a copy of a pattern.
    pub fn duplicate_pattern(&mut self, pattern: usize) -> Res<(usize, Changes)> {
        let lim = limits(&self.module);
        let copy = self.pattern_ref(pattern)?.clone();
        if self.module.patterns.len() >= lim.max_patterns {
            return Err(format!(
                "{} holds {} patterns",
                super::limits::format_name(self.module.format),
                lim.max_patterns
            ));
        }
        let index = self.module.patterns.len();
        let ((), changes) = self.edit(|d| {
            d.change(Change::PatternInsert {
                index,
                pattern: copy,
            });
            Ok(())
        })?;
        Ok((index, changes))
    }

    /// Removes a pattern. The orders that played it are removed and the
    /// patterns after it renumbered in the order list; a module keeps at least
    /// one pattern and one order.
    pub fn delete_pattern(&mut self, pattern: usize) -> Res<Changes> {
        self.pattern_ref(pattern)?;
        if self.module.patterns.len() == 1 {
            return Err("a module needs at least one pattern".into());
        }
        let orders: Vec<usize> = self
            .module
            .orders
            .iter()
            .filter(|&&o| o != pattern)
            .map(|&o| if o > pattern { o - 1 } else { o })
            .collect();
        if orders.is_empty() {
            return Err(format!(
                "pattern {pattern} is the whole song: put another pattern in the order list first"
            ));
        }
        let restart = self.module.orders[..self.module.restart.min(self.module.orders.len())]
            .iter()
            .filter(|&&o| o != pattern)
            .count();
        let ((), changes) = self.edit(|d| {
            d.set_orders(orders, restart)?;
            d.change(Change::PatternRemove { index: pattern });
            Ok(())
        })?;
        Ok(changes)
    }
}
