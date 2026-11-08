//! Pattern rows and patterns: typing, clearing, shifting, transposing,
//! copying, pasting, and the pool of patterns.

use serde_json::{json, Value};

use super::history::Changes;
use super::json::{row_from_json, row_json, CellUpdate, Res};
use super::limits::row_problem;
use super::SidDoc;
use crate::song::{Orderlist, Pattern, Row, MAX_PATTERNS, MAX_PATTERN_ROWS, NOTE_FIRST, NOTE_LAST};

/// Which columns of a row a clear touches. `command` is the command and its
/// parameter together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClearMask {
    pub note: bool,
    pub instrument: bool,
    pub command: bool,
}

impl ClearMask {
    pub const ALL: ClearMask = ClearMask {
        note: true,
        instrument: true,
        command: true,
    };

    pub fn from_names(names: &[&str]) -> Res<ClearMask> {
        let mut m = ClearMask {
            note: false,
            instrument: false,
            command: false,
        };
        for n in names {
            match *n {
                "note" => m.note = true,
                "instrument" => m.instrument = true,
                "command" | "param" => m.command = true,
                other => {
                    return Err(format!(
                        "'{other}' is not a column (note, instrument, command)"
                    ))
                }
            }
        }
        Ok(m)
    }

    fn clear(&self, row: &Row) -> Row {
        let mut r = *row;
        if self.note {
            r.note = 0;
        }
        if self.instrument {
            r.instrument = 0;
        }
        if self.command {
            r.command = 0;
            r.param = 0;
        }
        r
    }
}

/// Rows copied out of a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clip {
    pub rows: Vec<Row>,
}

impl Clip {
    /// `{"rows": n, "cells": [[note, instrument, command, param]]}`.
    pub fn to_json(&self) -> Value {
        json!({"rows": self.rows.len(), "cells": self.rows.iter().map(row_json).collect::<Vec<_>>()})
    }

    /// From the `cells` array (or a whole clip object).
    pub fn from_json(v: &Value) -> Res<Clip> {
        let cells = v.get("cells").unwrap_or(v);
        let a = cells
            .as_array()
            .ok_or("paste needs 'cells': a list of [note, instrument, command, param]")?;
        Ok(Clip {
            rows: a.iter().map(row_from_json).collect::<Res<_>>()?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteMode {
    /// The pasted rows replace what is there.
    Overwrite,
    /// The pasted rows fill only what is empty: a note, an instrument, a command.
    Mix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pasted {
    /// Rows written.
    pub rows: usize,
    /// The clip ran past the pattern's end and was cut.
    pub clipped: bool,
}

fn is_blank(r: &Row) -> bool {
    *r == Row::default()
}

impl SidDoc {
    fn row_at(&self, pattern: usize, row: usize) -> Res<Row> {
        let p = self.pattern_ref(pattern)?;
        p.rows.get(row).copied().ok_or_else(|| {
            format!(
                "pattern {pattern} has {} rows: there is no row {row}",
                p.rows.len()
            )
        })
    }

    fn check_range(&self, pattern: usize, row0: usize, row1: usize) -> Res<()> {
        let len = self.pattern_ref(pattern)?.rows.len();
        if row0 > row1 || row1 >= len {
            return Err(format!(
                "rows {row0}-{row1} are not inside pattern {pattern}'s {len} rows"
            ));
        }
        Ok(())
    }

    /// Sets the fields of one row that `update` names. Returns the row as it is.
    pub fn set_cell(
        &mut self,
        pattern: usize,
        row: usize,
        update: CellUpdate,
    ) -> Res<(Row, Changes)> {
        let old = self.row_at(pattern, row)?;
        let new = update.applied(&old);
        if let Some(why) = row_problem(&new, self.song.instruments.len()) {
            return Err(why);
        }
        let (_, changes) = self.edit(|d| {
            d.put_rows(pattern, row, vec![new]);
            Ok(())
        })?;
        Ok((new, changes))
    }

    /// Clears the chosen columns of rows `row0..=row1`.
    pub fn clear_range(
        &mut self,
        pattern: usize,
        row0: usize,
        row1: usize,
        mask: ClearMask,
    ) -> Res<Changes> {
        self.check_range(pattern, row0, row1)?;
        let rows: Vec<Row> = self.song.patterns[pattern].rows[row0..=row1]
            .iter()
            .map(|r| mask.clear(r))
            .collect();
        self.edit(|d| {
            d.put_rows(pattern, row0, rows);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Inserts `count` blank rows before `row`. The rows below move down and the
    /// last ones fall off the end, unless `grow`: then the pattern gets longer
    /// (to 128 rows at most) and `row` may be the pattern's length (append).
    pub fn insert_rows(
        &mut self,
        pattern: usize,
        row: usize,
        count: usize,
        grow: bool,
    ) -> Res<Changes> {
        let len = self.pattern_ref(pattern)?.rows.len();
        if count == 0 {
            return Err("'count' is 1 or more".into());
        }
        if row > len || (row == len && !grow) {
            return Err(format!(
                "pattern {pattern} has {len} rows: no row {row} to insert before"
            ));
        }
        let mut rows = self.song.patterns[pattern].rows.clone();
        if grow {
            if len + count > MAX_PATTERN_ROWS {
                return Err(format!(
                    "pattern {pattern} would have {} rows, GoatTracker's most is {MAX_PATTERN_ROWS}",
                    len + count
                ));
            }
            rows.splice(row..row, std::iter::repeat_n(Row::default(), count));
            self.edit(|d| {
                d.put_pattern(pattern, Pattern { rows });
                Ok(())
            })
            .map(|(_, c)| c)
        } else {
            rows.splice(row..row, std::iter::repeat_n(Row::default(), count));
            rows.truncate(len);
            self.edit(|d| {
                d.put_rows(pattern, row, rows[row..].to_vec());
                Ok(())
            })
            .map(|(_, c)| c)
        }
    }

    /// Deletes `count` rows from `row`. The rows below move up and blanks come in
    /// at the end, unless `shrink`: then the pattern gets shorter (it keeps at
    /// least one row).
    pub fn delete_rows(
        &mut self,
        pattern: usize,
        row: usize,
        count: usize,
        shrink: bool,
    ) -> Res<Changes> {
        let len = self.pattern_ref(pattern)?.rows.len();
        if count == 0 {
            return Err("'count' is 1 or more".into());
        }
        if row >= len {
            return Err(format!(
                "pattern {pattern} has {len} rows: there is no row {row}"
            ));
        }
        let removed = count.min(len - row);
        let mut rows = self.song.patterns[pattern].rows.clone();
        rows.drain(row..row + removed);
        if shrink {
            if rows.is_empty() {
                return Err("a pattern keeps at least one row".into());
            }
            self.edit(|d| {
                d.put_pattern(pattern, Pattern { rows });
                Ok(())
            })
            .map(|(_, c)| c)
        } else {
            rows.resize(len, Row::default());
            self.edit(|d| {
                d.put_rows(pattern, row, rows[row..].to_vec());
                Ok(())
            })
            .map(|(_, c)| c)
        }
    }

    /// Transposes the notes (C-0 to G#7) of rows `row0..=row1` by `semitones`
    /// (only those that name `instrument`, if given). A note that would leave
    /// the range refuses the edit unless `clamp`. Returns how many notes moved.
    pub fn transpose(
        &mut self,
        pattern: usize,
        row0: usize,
        row1: usize,
        semitones: i32,
        clamp: bool,
        instrument: Option<u8>,
    ) -> Res<(usize, Changes)> {
        self.check_range(pattern, row0, row1)?;
        let mut rows: Vec<Row> = self.song.patterns[pattern].rows[row0..=row1].to_vec();
        let mut count = 0;
        for (i, r) in rows.iter_mut().enumerate() {
            if !(NOTE_FIRST..=NOTE_LAST).contains(&r.note)
                || instrument.is_some_and(|n| r.instrument != n)
            {
                continue;
            }
            let moved = r.note as i32 + semitones;
            let ok = (NOTE_FIRST as i32..=NOTE_LAST as i32).contains(&moved);
            if !ok && !clamp {
                return Err(format!(
                    "row {} would leave C-0..G#7 ({} {:+} semitones)",
                    row0 + i,
                    super::limits::note_name(r.note),
                    semitones
                ));
            }
            r.note = moved.clamp(NOTE_FIRST as i32, NOTE_LAST as i32) as u8;
            count += 1;
        }
        let (_, changes) = self.edit(|d| {
            d.put_rows(pattern, row0, rows);
            Ok(())
        })?;
        Ok((count, changes))
    }

    /// Copies rows `row0..=row1`.
    pub fn copy_range(&self, pattern: usize, row0: usize, row1: usize) -> Res<Clip> {
        self.check_range(pattern, row0, row1)?;
        Ok(Clip {
            rows: self.song.patterns[pattern].rows[row0..=row1].to_vec(),
        })
    }

    /// Writes a clip from `row` on. A clip that runs past the pattern's end is
    /// cut (`clipped`); a row that names an instrument the song lacks refuses it.
    pub fn paste(
        &mut self,
        pattern: usize,
        row: usize,
        clip: &Clip,
        mode: PasteMode,
    ) -> Res<(Pasted, Changes)> {
        let len = self.pattern_ref(pattern)?.rows.len();
        if row >= len {
            return Err(format!(
                "pattern {pattern} has {len} rows: there is no row {row}"
            ));
        }
        if clip.rows.is_empty() {
            return Err("there is nothing to paste".into());
        }
        let n = clip.rows.len().min(len - row);
        let mut rows: Vec<Row> = Vec::with_capacity(n);
        for (i, src) in clip.rows[..n].iter().enumerate() {
            if let Some(why) = row_problem(src, self.song.instruments.len()) {
                return Err(format!("pasted row {i}: {why}"));
            }
            let here = self.song.patterns[pattern].rows[row + i];
            rows.push(match mode {
                PasteMode::Overwrite => *src,
                PasteMode::Mix => Row {
                    note: if here.note == 0 { src.note } else { here.note },
                    instrument: if here.instrument == 0 {
                        src.instrument
                    } else {
                        here.instrument
                    },
                    command: if here.command == 0 && here.param == 0 {
                        src.command
                    } else {
                        here.command
                    },
                    param: if here.command == 0 && here.param == 0 {
                        src.param
                    } else {
                        here.param
                    },
                },
            });
        }
        let (_, changes) = self.edit(|d| {
            d.put_rows(pattern, row, rows);
            Ok(())
        })?;
        Ok((
            Pasted {
                rows: n,
                clipped: n < clip.rows.len(),
            },
            changes,
        ))
    }

    /// Sets a pattern's length (1-128). Rows are cut from the end (a pattern that
    /// holds notes there needs `force`) or blanks added.
    pub fn set_pattern_rows(&mut self, pattern: usize, rows: usize, force: bool) -> Res<Changes> {
        let len = self.pattern_ref(pattern)?.rows.len();
        if !(1..=MAX_PATTERN_ROWS).contains(&rows) {
            return Err(format!("a pattern has 1-{MAX_PATTERN_ROWS} rows"));
        }
        let mut new = self.song.patterns[pattern].rows.clone();
        if rows < len {
            if !force {
                if let Some(at) = new[rows..].iter().position(|r| !is_blank(r)) {
                    return Err(format!(
                        "shortening pattern {pattern} to {rows} rows would drop row {} and others that have data (use force)",
                        rows + at
                    ));
                }
            }
            new.truncate(rows);
        } else {
            new.resize(rows, Row::default());
        }
        self.edit(|d| {
            d.put_pattern(pattern, Pattern { rows: new });
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Appends a blank pattern of `rows` rows; returns its number.
    pub fn new_pattern(&mut self, rows: usize) -> Res<(usize, Changes)> {
        if !(1..=MAX_PATTERN_ROWS).contains(&rows) {
            return Err(format!("a pattern has 1-{MAX_PATTERN_ROWS} rows"));
        }
        self.append_pattern(Pattern {
            rows: vec![Row::default(); rows],
        })
    }

    /// Appends a copy of a pattern; returns its number.
    pub fn duplicate_pattern(&mut self, pattern: usize) -> Res<(usize, Changes)> {
        let copy = self.pattern_ref(pattern)?.clone();
        self.append_pattern(copy)
    }

    fn append_pattern(&mut self, pattern: Pattern) -> Res<(usize, Changes)> {
        let at = self.song.patterns.len();
        if at >= MAX_PATTERNS {
            return Err(format!(
                "the song has {MAX_PATTERNS} patterns, GoatTracker's most"
            ));
        }
        let (_, changes) = self.edit(|d| {
            d.insert_pattern_at(at, pattern);
            Ok(())
        })?;
        Ok((at, changes))
    }

    /// Deletes a pattern; the patterns after it move down one and every orderlist
    /// entry follows. A pattern some orderlist plays is refused unless `force`:
    /// then those entries are removed (an orderlist keeps at least one entry).
    pub fn delete_pattern(&mut self, pattern: usize, force: bool) -> Res<Changes> {
        self.pattern_ref(pattern)?;
        if self.song.patterns.len() == 1 {
            return Err("a song keeps at least one pattern".into());
        }
        let mut lists: Vec<(usize, usize, Orderlist)> = Vec::new();
        let mut first_use = None;
        let mut uses = 0;
        for (s, sub) in self.song.subsongs.iter().enumerate() {
            for (v, list) in sub.orderlists.iter().enumerate() {
                let n = list
                    .entries
                    .iter()
                    .filter(|e| e.pattern as usize == pattern)
                    .count();
                if n > 0 {
                    uses += n;
                    first_use.get_or_insert((s, v));
                }
                let restart = list.restart as usize;
                let mut entries = Vec::with_capacity(list.entries.len());
                let mut new_restart = None;
                for (i, e) in list.entries.iter().enumerate() {
                    if i >= restart && new_restart.is_none() && e.pattern as usize != pattern {
                        new_restart = Some(entries.len());
                    }
                    if e.pattern as usize == pattern {
                        continue;
                    }
                    let mut e = *e;
                    if e.pattern as usize > pattern {
                        e.pattern -= 1;
                    }
                    entries.push(e);
                }
                if entries.is_empty() {
                    if force {
                        return Err(format!(
                            "deleting pattern {pattern} would leave subsong {s} voice {v} with no orderlist entry"
                        ));
                    }
                } else {
                    let restart = new_restart.unwrap_or(entries.len() - 1);
                    lists.push((
                        s,
                        v,
                        Orderlist {
                            entries,
                            restart: restart as u8,
                        },
                    ));
                }
            }
        }
        if uses > 0 && !force {
            let (s, v) = first_use.unwrap();
            return Err(format!(
                "pattern {pattern} is played by {uses} orderlist {} (the first in subsong {s} voice {v}); remove them first, or use force",
                if uses == 1 { "entry" } else { "entries" }
            ));
        }
        self.edit(|d| {
            for (s, v, list) in lists {
                d.put_orderlist(s, v, list);
            }
            d.remove_pattern_at(pattern);
            Ok(())
        })
        .map(|(_, c)| c)
    }
}
