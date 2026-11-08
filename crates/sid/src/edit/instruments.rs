//! Instruments and the four shared step tables.
//!
//! GoatTracker's tables are one pool per kind that every instrument points
//! into (1-based, 0 = none), and so do pattern commands, wave-table commands
//! and `FF xx` jumps. Rows inserted or deleted keep every one of those
//! references on its data, as GoatTracker's table editor does (and the DAW's
//! `sid-table-rows.ts`, which this follows).

use std::collections::BTreeSet;

use super::history::Changes;
use super::json::{to_latin1, Res};
use super::limits::{MAX_NAME, MAX_TABLE_ROWS};
use super::{default_instrument, SidDoc, NEW_PULSE_ROWS, NEW_WAVE_ROWS};
use crate::song::{Row, TableKind, TableRow, MAX_INSTRUMENTS};

/// The fields of an instrument an edit changes (`None` leaves a field alone).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstrumentUpdate {
    pub name: Option<String>,
    pub attack: Option<u8>,
    pub decay: Option<u8>,
    pub sustain: Option<u8>,
    pub release: Option<u8>,
    pub first_wave: Option<u8>,
    pub gate_timer: Option<u8>,
    pub hard_restart: Option<bool>,
    pub no_gate_off: Option<bool>,
    pub vibrato_delay: Option<u8>,
    pub wave_ptr: Option<u8>,
    pub pulse_ptr: Option<u8>,
    pub filter_ptr: Option<u8>,
    pub speed_ptr: Option<u8>,
}

/// The pattern commands whose parameter is a row of a table: 8/9/A start the
/// wave/pulse/filter table, 1-4 and E read a speed row.
fn pattern_commands(kind: TableKind) -> &'static [u8] {
    match kind {
        TableKind::Wave => &[0x8],
        TableKind::Pulse => &[0x9],
        TableKind::Filter => &[0xA],
        TableKind::Speed => &[0x1, 0x2, 0x3, 0x4, 0xE],
    }
}

/// The table a wave-table row's command names a row of ($F1-$F4 and $FE the
/// speed table, $F8 the wave table, $F9 pulse, $FA filter), or `None` for any
/// other row and for a parameter of 0.
fn wave_command_table(row: &TableRow) -> Option<TableKind> {
    if row.left < 0xF0 || row.left == 0xFF || row.right == 0 {
        return None;
    }
    match row.left & 0x0F {
        0x1..=0x4 | 0xE => Some(TableKind::Speed),
        0x8 => Some(TableKind::Wave),
        0x9 => Some(TableKind::Pulse),
        0xA => Some(TableKind::Filter),
        _ => None,
    }
}

/// The contiguous rows (1-based, inclusive) a table plays from `ptr`: forward to
/// the first jump or stop (`FF xx`), or the table's end. The speed table is not
/// stepped: an instrument names one row of it.
fn chain_of(rows: &[TableRow], kind: TableKind, ptr: usize) -> (usize, usize) {
    if kind == TableKind::Speed {
        return (ptr, ptr);
    }
    let mut end = ptr;
    while end < rows.len() && rows[end - 1].left != 0xFF {
        end += 1;
    }
    (ptr, end)
}

/// The rows (1-based) a table plays from `ptr`: forward until the table ends, a
/// stop or a jump, which is followed once (a loop back ends the walk).
pub(crate) fn rows_from(rows: &[TableRow], ptr: usize) -> Vec<usize> {
    let mut seen = BTreeSet::new();
    let mut at = ptr;
    while at >= 1 && at <= rows.len() && !seen.contains(&at) {
        seen.insert(at);
        let row = rows[at - 1];
        if row.left == 0xFF {
            at = row.right as usize;
            continue;
        }
        at += 1;
    }
    seen.into_iter().collect()
}

impl SidDoc {
    // ----- instruments ---------------------------------------------------------

    /// Sets the fields of instrument `n` (1-based) that `update` names. Envelope
    /// values are 0-15, the gate timer 0-63, a table pointer a row of its table
    /// (1-based) or 0.
    pub fn set_instrument(&mut self, n: usize, update: &InstrumentUpdate) -> Res<Changes> {
        let mut i = self.instrument_ref(n)?.clone();
        if let Some(name) = &update.name {
            let bytes = to_latin1(name, "the name")?;
            if bytes.len() > MAX_NAME {
                return Err(format!("a name is at most {MAX_NAME} characters"));
            }
            i.name = bytes;
        }
        for (v, slot, what) in [
            (update.attack, &mut i.attack, "attack"),
            (update.decay, &mut i.decay, "decay"),
            (update.sustain, &mut i.sustain, "sustain"),
            (update.release, &mut i.release, "release"),
        ] {
            if let Some(v) = v {
                if v > 15 {
                    return Err(format!("{what} is 0-15, not {v}"));
                }
                *slot = v;
            }
        }
        if let Some(v) = update.first_wave {
            i.first_wave = v;
        }
        if let Some(v) = update.gate_timer {
            if v > 63 {
                return Err(format!("the gate timer is 0-63, not {v}"));
            }
            i.gate_timer = v;
        }
        if let Some(v) = update.hard_restart {
            i.hard_restart = v;
        }
        if let Some(v) = update.no_gate_off {
            i.no_gate_off = v;
        }
        if let Some(v) = update.vibrato_delay {
            i.vibrato_delay = v;
        }
        for (kind, v) in [
            (TableKind::Wave, update.wave_ptr),
            (TableKind::Pulse, update.pulse_ptr),
            (TableKind::Filter, update.filter_ptr),
            (TableKind::Speed, update.speed_ptr),
        ] {
            if let Some(v) = v {
                let len = self.song.tables.get(kind).len();
                if v as usize > len {
                    return Err(format!(
                        "the {} table has {len} rows: a pointer into it is 1-{len} (0 for none), not {v}",
                        kind.name()
                    ));
                }
                kind.set_pointer(&mut i, v);
            }
        }
        self.edit(|d| {
            d.put_instrument(n - 1, i);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Resets instrument `n` to GoatTracker's new instrument with no tables and
    /// no name (its table rows stay: other instruments may use them).
    pub fn clear_instrument(&mut self, n: usize) -> Res<Changes> {
        self.instrument_ref(n)?;
        let fresh = default_instrument(self.song.speed_multiplier);
        self.edit(|d| {
            d.put_instrument(n - 1, fresh);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Appends GoatTracker's new instrument and returns its number. With `tables`
    /// it gets rows of its own (wave `41 00 / FF 00`, pulse `88 00 / FF 00`: a
    /// plain pulse) appended to the tables, so it sounds and can be shaped
    /// without touching another instrument; without, no pointers.
    pub fn add_instrument(&mut self, name: &str, tables: bool) -> Res<(usize, Changes)> {
        let at = self.song.instruments.len();
        if at >= MAX_INSTRUMENTS {
            return Err(format!(
                "the song has {MAX_INSTRUMENTS} instruments, GoatTracker's most"
            ));
        }
        let mut ins = default_instrument(self.song.speed_multiplier);
        ins.name = to_latin1(name, "the name")?;
        if ins.name.len() > MAX_NAME {
            return Err(format!("a name is at most {MAX_NAME} characters"));
        }
        let (wave, pulse) = (
            self.song.tables.wave.clone(),
            self.song.tables.pulse.clone(),
        );
        if tables {
            if wave.len() + NEW_WAVE_ROWS.len() > MAX_TABLE_ROWS
                || pulse.len() + NEW_PULSE_ROWS.len() > MAX_TABLE_ROWS
            {
                return Err(format!(
                    "the wave or pulse table has no room for a new instrument's 2 rows ({MAX_TABLE_ROWS} at most)"
                ));
            }
            ins.wave_ptr = wave.len() as u8 + 1;
            ins.pulse_ptr = pulse.len() as u8 + 1;
        }
        let (_, changes) = self.edit(|d| {
            if tables {
                d.put_table(TableKind::Wave, [wave, NEW_WAVE_ROWS.to_vec()].concat());
                d.put_table(TableKind::Pulse, [pulse, NEW_PULSE_ROWS.to_vec()].concat());
            }
            d.insert_instrument_at(at, ins);
            Ok(())
        })?;
        Ok((at + 1, changes))
    }

    /// Copies instrument `n` and its table rows: appended as a new instrument
    /// (returns its number), or over instrument `to`. The rows the copy points at
    /// are copies too (appended to each table, jumps inside the copy moved with
    /// it, jumps out of it kept), so editing the copy never changes the original;
    /// wave-table commands keep naming the rows they named.
    pub fn clone_instrument(&mut self, n: usize, to: Option<usize>) -> Res<(usize, Changes)> {
        let source = self.instrument_ref(n)?.clone();
        let target = match to {
            Some(t) => {
                self.instrument_ref(t)?;
                t
            }
            None => {
                if self.song.instruments.len() >= MAX_INSTRUMENTS {
                    return Err(format!(
                        "the song has {MAX_INSTRUMENTS} instruments, GoatTracker's most"
                    ));
                }
                self.song.instruments.len() + 1
            }
        };
        let mut copy = source.clone();
        let mut tables = Vec::new();
        for kind in TableKind::ALL {
            let ptr = kind.pointer(&source) as usize;
            let rows = self.song.tables.get(kind);
            if ptr == 0 || ptr > rows.len() {
                continue;
            }
            let (from, to_row) = chain_of(rows, kind, ptr);
            let base = rows.len();
            if base + (to_row - from + 1) > MAX_TABLE_ROWS {
                return Err(format!(
                    "the {} table has no room for a copy of instrument {n}'s {} row{} ({MAX_TABLE_ROWS} at most)",
                    kind.name(),
                    to_row - from + 1,
                    if to_row == from { "" } else { "s" }
                ));
            }
            let moved = |r: usize| base + r - from + 1;
            let rows_copy: Vec<TableRow> = rows[from - 1..to_row]
                .iter()
                .map(|row| {
                    let (r, f, t) = (row.right as usize, from, to_row);
                    if kind != TableKind::Speed && row.left == 0xFF && r >= f && r <= t {
                        TableRow {
                            left: 0xFF,
                            right: moved(r) as u8,
                        }
                    } else {
                        *row
                    }
                })
                .collect();
            kind.set_pointer(&mut copy, moved(ptr) as u8);
            tables.push((kind, [rows.clone(), rows_copy].concat()));
        }
        let (_, changes) = self.edit(|d| {
            for (kind, rows) in tables {
                d.put_table(kind, rows);
            }
            match to {
                Some(t) => d.put_instrument(t - 1, copy),
                None => {
                    let at = d.song.instruments.len();
                    d.insert_instrument_at(at, copy);
                }
            }
            Ok(())
        })?;
        Ok((target, changes))
    }

    /// Deletes instrument `n`. The instruments after it move down one and every
    /// row naming one of them follows it. Rows that name `n` itself refuse the
    /// delete, unless `clear_uses`: then they name no instrument (the voice keeps
    /// the one it had). Its table rows stay (the tables are shared; another
    /// instrument or command may reach them).
    pub fn delete_instrument(&mut self, n: usize, clear_uses: bool) -> Res<Changes> {
        self.instrument_ref(n)?;
        let mut first = None;
        let mut uses = 0;
        for (p, pattern) in self.song.patterns.iter().enumerate() {
            for (r, row) in pattern.rows.iter().enumerate() {
                if row.instrument as usize == n {
                    uses += 1;
                    first.get_or_insert((p, r));
                }
            }
        }
        if uses > 0 && !clear_uses {
            let (p, r) = first.unwrap();
            return Err(format!(
                "instrument {n} is named on {uses} row{} (the first: pattern {p} row {r}); clear them, or use clear_uses",
                if uses == 1 { "" } else { "s" }
            ));
        }
        let patterns = self.map_rows(|row| {
            let mut r = *row;
            if r.instrument as usize == n {
                r.instrument = 0;
            } else if r.instrument as usize > n {
                r.instrument -= 1;
            }
            r
        });
        self.edit(|d| {
            d.put_pattern_rows(patterns);
            d.remove_instrument_at(n - 1);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Every pattern through `edit`: the changed rows' span of each changed pattern.
    fn map_rows(&self, edit: impl Fn(&Row) -> Row) -> Vec<(usize, usize, Vec<Row>)> {
        let mut out = Vec::new();
        for (p, pattern) in self.song.patterns.iter().enumerate() {
            let new: Vec<Row> = pattern.rows.iter().map(&edit).collect();
            let first = new.iter().zip(&pattern.rows).position(|(a, b)| a != b);
            if let Some(first) = first {
                let last = new
                    .iter()
                    .zip(&pattern.rows)
                    .rposition(|(a, b)| a != b)
                    .unwrap();
                out.push((p, first, new[first..=last].to_vec()));
            }
        }
        out
    }

    pub(crate) fn put_pattern_rows(&mut self, spans: Vec<(usize, usize, Vec<Row>)>) {
        for (p, first, rows) in spans {
            self.put_rows(p, first, rows);
        }
    }

    // ----- tables -----------------------------------------------------------------

    fn table_row_ref(&self, kind: TableKind, row: usize) -> Res<TableRow> {
        let t = self.song.tables.get(kind);
        t.get(row).copied().ok_or_else(|| {
            format!(
                "the {} table has {} rows: there is no row {row} (rows count from 0; a pointer calls row 0 '1')",
                kind.name(),
                t.len()
            )
        })
    }

    /// Writes row `row` (0-based) of a table: the columns given (`None` keeps it),
    /// or appends a row when `row` is the table's length.
    pub fn set_table_row(
        &mut self,
        kind: TableKind,
        row: usize,
        left: Option<u8>,
        right: Option<u8>,
    ) -> Res<Changes> {
        let mut rows = self.song.tables.get(kind).clone();
        if row == rows.len() {
            if rows.len() >= MAX_TABLE_ROWS {
                return Err(format!(
                    "the {} table is full ({MAX_TABLE_ROWS} rows)",
                    kind.name()
                ));
            }
            rows.push(TableRow {
                left: left.unwrap_or(0),
                right: right.unwrap_or(0),
            });
        } else {
            let old = self.table_row_ref(kind, row)?;
            rows[row] = TableRow {
                left: left.unwrap_or(old.left),
                right: right.unwrap_or(old.right),
            };
        }
        self.edit(|d| {
            d.put_table(kind, rows);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Sets row `row` of a table to `00 00`.
    pub fn clear_table_row(&mut self, kind: TableKind, row: usize) -> Res<Changes> {
        self.table_row_ref(kind, row)?;
        let mut rows = self.song.tables.get(kind).clone();
        rows[row] = TableRow::default();
        self.edit(|d| {
            d.put_table(kind, rows);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Inserts `value` before row `row` (0-based; the table's length appends).
    /// Every pointer, jump and command naming that row or a later one moves down
    /// with its data.
    pub fn insert_table_row(
        &mut self,
        kind: TableKind,
        row: usize,
        value: TableRow,
    ) -> Res<Changes> {
        let mut rows = self.song.tables.get(kind).clone();
        if rows.len() >= MAX_TABLE_ROWS {
            return Err(format!(
                "the {} table is full ({MAX_TABLE_ROWS} rows)",
                kind.name()
            ));
        }
        if row > rows.len() {
            return Err(format!(
                "the {} table has {} rows: no place {row} to insert at",
                kind.name(),
                rows.len()
            ));
        }
        rows.insert(row, value);
        let at = row + 1; // the pointer number of the new row
        self.edit(|d| {
            d.remap_references(kind, rows, &|r| {
                if r as usize >= at {
                    r.saturating_add(1)
                } else {
                    r
                }
            })
        })
        .map(|(_, c)| c)
    }

    /// Deletes row `row` (0-based). References past it move up with their data;
    /// one to the deleted row itself then names the row that took its place (or
    /// the new last row, or none when the table is left empty).
    pub fn delete_table_row(&mut self, kind: TableKind, row: usize) -> Res<Changes> {
        self.table_row_ref(kind, row)?;
        let mut rows = self.song.tables.get(kind).clone();
        rows.remove(row);
        let at = row + 1;
        let len = rows.len();
        self.edit(|d| {
            d.remap_references(kind, rows, &|r| {
                let r = r as usize;
                (if r == at {
                    at.min(len)
                } else if r > at {
                    r - 1
                } else {
                    r
                }) as u8
            })
        })
        .map(|(_, c)| c)
    }

    /// Replaces table `kind` with `rows` (already changed: rows inserted, deleted
    /// or removed) and moves every reference to one of its rows through `mv`
    /// (a 1-based row to its new number, 0 staying 0): the instruments'
    /// pointers, the table's own jumps, wave-table commands that name a row of
    /// it, and pattern commands.
    fn remap_references(
        &mut self,
        kind: TableKind,
        mut rows: Vec<TableRow>,
        mv: &dyn Fn(u8) -> u8,
    ) -> Res<()> {
        let map = |r: u8| if r == 0 { 0 } else { mv(r) };
        // Jumps in the edited table itself (the speed table has none: its left
        // column is data).
        if kind != TableKind::Speed {
            for row in rows.iter_mut() {
                if row.left == 0xFF && row.right != 0 {
                    row.right = map(row.right);
                }
            }
        }
        // Wave-table commands that name a row of this table.
        let mut wave = if kind == TableKind::Wave {
            rows.clone()
        } else {
            self.song.tables.wave.clone()
        };
        for row in wave.iter_mut() {
            if wave_command_table(row) == Some(kind) {
                row.right = map(row.right);
            }
        }
        if kind == TableKind::Wave {
            rows = wave;
            self.put_table(kind, rows);
        } else {
            self.put_table(kind, rows);
            self.put_table(TableKind::Wave, wave);
        }
        // The instruments' pointers.
        for i in 0..self.song.instruments.len() {
            let mut ins = self.song.instruments[i].clone();
            let ptr = kind.pointer(&ins);
            kind.set_pointer(&mut ins, map(ptr));
            self.put_instrument(i, ins);
        }
        // Pattern commands.
        let commands = pattern_commands(kind);
        let spans = self.map_rows(|row| {
            if row.param != 0 && commands.contains(&row.command) {
                Row {
                    param: map(row.param),
                    ..*row
                }
            } else {
                *row
            }
        });
        self.put_pattern_rows(spans);
        Ok(())
    }

    /// Removes every table row nothing in the song reaches, the rest renumbered:
    /// tables as dense as GoatTracker's exporter packs them, without dropping an
    /// instrument no pattern plays yet. A row is reached from an instrument's
    /// pointer, from a pattern command that names a table row (8/9/A, 1-4, E) in
    /// any pattern, by playing on from a start (following jumps) and through the
    /// wave rows' commands. Returns how many rows went.
    pub fn remove_unused_table_rows(&mut self) -> Res<(usize, Changes)> {
        let reached = self.reached_table_rows();
        let mut removed = 0;
        // Per table: which rows stay, and where each stays row goes (1-based).
        let mut plan: Vec<(TableKind, Vec<bool>, Vec<u8>)> = Vec::new();
        for kind in TableKind::ALL {
            let len = self.song.tables.get(kind).len();
            let keep: Vec<bool> = (1..=len).map(|r| reached[&kind].contains(&r)).collect();
            let mut renumber = vec![0u8; len + 1];
            let mut next = 0u8;
            for (i, &k) in keep.iter().enumerate() {
                if k {
                    next += 1;
                    renumber[i + 1] = next;
                }
            }
            removed += len - next as usize;
            if (next as usize) != len {
                plan.push((kind, keep, renumber));
            }
        }
        let (_, changes) = self.edit(|d| {
            for (kind, keep, renumber) in plan {
                // The table as it is now: an earlier table's renumbering may have
                // moved the references inside it.
                let rows: Vec<TableRow> = d
                    .song
                    .tables
                    .get(kind)
                    .iter()
                    .zip(&keep)
                    .filter(|(_, &k)| k)
                    .map(|(r, _)| *r)
                    .collect();
                d.remap_references(kind, rows, &|r| {
                    renumber.get(r as usize).copied().unwrap_or(0)
                })?;
            }
            Ok(())
        })?;
        Ok((removed, changes))
    }

    fn reached_table_rows(&self) -> std::collections::BTreeMap<TableKind, BTreeSet<usize>> {
        let s = &self.song;
        let mut sets: std::collections::BTreeMap<TableKind, BTreeSet<usize>> = TableKind::ALL
            .into_iter()
            .map(|k| (k, BTreeSet::new()))
            .collect();
        let mut work: Vec<(TableKind, usize)> = Vec::new();
        for ins in &s.instruments {
            for kind in TableKind::ALL {
                work.push((kind, kind.pointer(ins) as usize));
            }
        }
        for p in &s.patterns {
            for row in &p.rows {
                if row.param == 0 {
                    continue;
                }
                for kind in TableKind::ALL {
                    if pattern_commands(kind).contains(&row.command) {
                        work.push((kind, row.param as usize));
                    }
                }
            }
        }
        while let Some((kind, ptr)) = work.pop() {
            if ptr == 0 {
                continue;
            }
            let rows = s.tables.get(kind);
            let reach = if kind == TableKind::Speed {
                if ptr <= rows.len() {
                    vec![ptr]
                } else {
                    Vec::new()
                }
            } else {
                rows_from(rows, ptr)
            };
            for r in reach {
                if !sets.get_mut(&kind).unwrap().insert(r) {
                    continue;
                }
                if kind == TableKind::Wave {
                    if let Some(target) = wave_command_table(&rows[r - 1]) {
                        work.push((target, rows[r - 1].right as usize));
                    }
                }
            }
        }
        sets
    }
}
