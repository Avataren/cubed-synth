//! Undo and redo as an inverse-change log, as `cubed_tracker::edit` keeps it.
//!
//! Every edit records, before it changes anything, the [`Change`] that puts
//! back what it is about to overwrite: a run of rows of one pattern, one
//! pattern, one orderlist, one instrument, one table, the header. Applying a
//! change returns its own inverse, so undoing a step produces the redo step
//! and the other way round. Typing a note stores one row, not the song.
//!
//! One user action is one step: edits made between `begin_group` and
//! `end_group` (they nest) are undone together. The log is bounded in steps
//! and in bytes; the oldest steps are dropped first.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::mem::size_of;

use serde_json::{json, Value};

use crate::song::{Instrument, Orderlist, Pattern, Row, SidSong, Subsong, TableKind, TableRow};
use crate::SidModel;

/// How many changed rows a [`Changes`] lists.
pub const MAX_CELLS: usize = 1024;

/// What an edit touched, for a UI to redraw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    /// Pattern number to the rows (first, last, inclusive) that changed.
    pub patterns: BTreeMap<usize, (usize, usize)>,
    /// Patterns were added, removed or reshaped (their count or their row
    /// count): refresh lists and sizes.
    pub layout: bool,
    /// Orderlists that changed, as (subsong, voice).
    pub orders: BTreeSet<(usize, usize)>,
    /// Subsongs were added or removed.
    pub subsongs: bool,
    /// Title, author or copyright.
    pub texts: bool,
    /// Tempo or speed multiplier.
    pub timing: bool,
    /// The chip model.
    pub model: bool,
    /// Instruments whose fields changed (1-based).
    pub instruments: Vec<usize>,
    /// Instruments were added or removed.
    pub instrument_list: bool,
    /// Per table, the rows (first, last, 0-based, inclusive) whose content
    /// changed.
    pub tables: BTreeMap<TableKind, (usize, usize)>,
    /// A table gained or lost rows.
    pub table_resized: bool,
    /// The pattern rows whose content changed, as (pattern, row), each once
    /// (up to [`MAX_CELLS`]; past that `cells_overflow` is set and the list is
    /// not complete). Rows of patterns that were swapped whole with a new
    /// length are not listed: those set `layout`.
    pub cells: Vec<(usize, usize)>,
    pub cells_overflow: bool,
}

impl Changes {
    /// Whether what changed can be given to a song that is playing as
    /// [`LiveEdit`](crate::song::LiveEdit)s: pattern rows, instrument fields,
    /// the content of table rows, the chip model, the texts. Anything about the
    /// song's shape (pattern lengths or count, orderlists, subsongs, the
    /// instrument list, a table's length) or its timing needs a new player.
    pub fn is_live(&self) -> bool {
        !self.layout
            && self.orders.is_empty()
            && !self.subsongs
            && !self.timing
            && !self.instrument_list
            && !self.table_resized
            && !self.cells_overflow
    }

    pub fn is_empty(&self) -> bool {
        *self == Changes::default()
    }

    pub(crate) fn cell(&mut self, pattern: usize, row: usize) {
        let key = (pattern, row);
        if self.cells.contains(&key) {
            return;
        }
        if self.cells.len() >= MAX_CELLS {
            self.cells_overflow = true;
        } else {
            self.cells.push(key);
        }
    }

    pub(crate) fn pattern_rows(&mut self, pattern: usize, first: usize, last: usize) {
        let e = self.patterns.entry(pattern).or_insert((first, last));
        e.0 = e.0.min(first);
        e.1 = e.1.max(last);
    }

    pub(crate) fn table_rows(&mut self, table: TableKind, first: usize, last: usize) {
        let e = self.tables.entry(table).or_insert((first, last));
        e.0 = e.0.min(first);
        e.1 = e.1.max(last);
    }

    pub(crate) fn instrument(&mut self, number: usize) {
        if !self.instruments.contains(&number) {
            self.instruments.push(number);
            self.instruments.sort_unstable();
        }
    }

    /// `{"patterns": [{"pattern", "row0", "row1"}], "layout", "orders":
    /// [{"subsong", "voice"}], "subsongs", "header", "instruments": [n],
    /// "instrument_list", "tables": [{"table", "row0", "row1"}],
    /// "table_layout"}`; only what changed is present.
    pub fn to_json(&self) -> Value {
        let mut v = json!({});
        if !self.patterns.is_empty() {
            v["patterns"] = Value::Array(
                self.patterns
                    .iter()
                    .map(|(&p, &(a, b))| json!({"pattern": p, "row0": a, "row1": b}))
                    .collect(),
            );
        }
        if !self.orders.is_empty() {
            v["orders"] = Value::Array(
                self.orders
                    .iter()
                    .map(|&(s, c)| json!({"subsong": s, "voice": c}))
                    .collect(),
            );
        }
        if !self.tables.is_empty() {
            v["tables"] = Value::Array(
                self.tables
                    .iter()
                    .map(|(t, &(a, b))| json!({"table": t.name(), "row0": a, "row1": b}))
                    .collect(),
            );
        }
        for (key, on) in [
            ("layout", self.layout),
            ("subsongs", self.subsongs),
            ("header", self.texts || self.timing || self.model),
            ("instrument_list", self.instrument_list),
            ("table_layout", self.table_resized),
        ] {
            if on {
                v[key] = json!(true);
            }
        }
        if !self.instruments.is_empty() {
            v["instruments"] = json!(self.instruments);
        }
        v
    }
}

/// The song-wide fields an edit of the header changes.
#[derive(Clone, Debug)]
pub(crate) struct HeaderState {
    pub name: Vec<u8>,
    pub author: Vec<u8>,
    pub copyright: Vec<u8>,
    pub model: SidModel,
    pub speed_multiplier: u8,
    pub tempo: u8,
}

impl HeaderState {
    pub fn of(s: &SidSong) -> HeaderState {
        HeaderState {
            name: s.name.clone(),
            author: s.author.clone(),
            copyright: s.copyright.clone(),
            model: s.model,
            speed_multiplier: s.speed_multiplier,
            tempo: s.tempo,
        }
    }
}

/// A recorded change: applying it to a song puts back what it holds and
/// returns the change that undoes that.
#[derive(Clone, Debug)]
pub(crate) enum Change {
    /// A run of rows of one pattern, swapped in place.
    Rows {
        pattern: usize,
        row0: usize,
        rows: Vec<Row>,
    },
    /// A whole pattern, swapped in place (its length may differ).
    Pattern {
        index: usize,
        pattern: Pattern,
    },
    PatternInsert {
        index: usize,
        pattern: Pattern,
    },
    PatternRemove {
        index: usize,
    },
    /// One orderlist.
    Order {
        subsong: usize,
        voice: usize,
        list: Orderlist,
    },
    SubsongInsert {
        index: usize,
        subsong: Subsong,
    },
    SubsongRemove {
        index: usize,
    },
    /// One instrument, swapped in place (0-based index).
    Instrument {
        index: usize,
        instrument: Instrument,
    },
    InstrumentInsert {
        index: usize,
        instrument: Instrument,
    },
    InstrumentRemove {
        index: usize,
    },
    /// A whole table, swapped in place (its length may differ).
    Table {
        kind: TableKind,
        rows: Vec<TableRow>,
    },
    Header(Box<HeaderState>),
}

impl Change {
    /// Puts this change's state into `s`; returns the change that restores
    /// what was there.
    pub(crate) fn apply(self, s: &mut SidSong) -> Change {
        match self {
            Change::Rows {
                pattern,
                row0,
                mut rows,
            } => {
                let p = &mut s.patterns[pattern].rows;
                for (i, r) in rows.iter_mut().enumerate() {
                    std::mem::swap(&mut p[row0 + i], r);
                }
                Change::Rows {
                    pattern,
                    row0,
                    rows,
                }
            }
            Change::Pattern { index, pattern } => {
                let old = std::mem::replace(&mut s.patterns[index], pattern);
                Change::Pattern {
                    index,
                    pattern: old,
                }
            }
            Change::PatternInsert { index, pattern } => {
                s.patterns.insert(index, pattern);
                Change::PatternRemove { index }
            }
            Change::PatternRemove { index } => {
                let pattern = s.patterns.remove(index);
                Change::PatternInsert { index, pattern }
            }
            Change::Order {
                subsong,
                voice,
                list,
            } => {
                let old = std::mem::replace(&mut s.subsongs[subsong].orderlists[voice], list);
                Change::Order {
                    subsong,
                    voice,
                    list: old,
                }
            }
            Change::SubsongInsert { index, subsong } => {
                s.subsongs.insert(index, subsong);
                Change::SubsongRemove { index }
            }
            Change::SubsongRemove { index } => {
                let subsong = s.subsongs.remove(index);
                Change::SubsongInsert { index, subsong }
            }
            Change::Instrument { index, instrument } => {
                let old = std::mem::replace(&mut s.instruments[index], instrument);
                Change::Instrument {
                    index,
                    instrument: old,
                }
            }
            Change::InstrumentInsert { index, instrument } => {
                s.instruments.insert(index, instrument);
                Change::InstrumentRemove { index }
            }
            Change::InstrumentRemove { index } => {
                let instrument = s.instruments.remove(index);
                Change::InstrumentInsert { index, instrument }
            }
            Change::Table { kind, rows } => {
                let old = std::mem::replace(s.tables.get_mut(kind), rows);
                Change::Table { kind, rows: old }
            }
            Change::Header(mut h) => {
                std::mem::swap(&mut s.name, &mut h.name);
                std::mem::swap(&mut s.author, &mut h.author);
                std::mem::swap(&mut s.copyright, &mut h.copyright);
                std::mem::swap(&mut s.model, &mut h.model);
                std::mem::swap(&mut s.speed_multiplier, &mut h.speed_multiplier);
                std::mem::swap(&mut s.tempo, &mut h.tempo);
                Change::Header(h)
            }
        }
    }

    /// Roughly what this change costs to keep, in bytes.
    pub(crate) fn bytes(&self) -> usize {
        const BASE: usize = 64;
        match self {
            Change::Rows { rows, .. } => BASE + rows.len() * size_of::<Row>(),
            Change::Pattern { pattern, .. } | Change::PatternInsert { pattern, .. } => {
                BASE + pattern.rows.len() * size_of::<Row>()
            }
            Change::Order { list, .. } => BASE + list.entries.len() * 3,
            Change::SubsongInsert { subsong, .. } => {
                BASE + subsong
                    .orderlists
                    .iter()
                    .map(|l| l.entries.len() * 3)
                    .sum::<usize>()
            }
            Change::Instrument { instrument, .. } | Change::InstrumentInsert { instrument, .. } => {
                BASE + 16 + instrument.name.len()
            }
            Change::Table { rows, .. } => BASE + rows.len() * size_of::<TableRow>(),
            Change::Header(h) => BASE + h.name.len() + h.author.len() + h.copyright.len(),
            Change::PatternRemove { .. }
            | Change::SubsongRemove { .. }
            | Change::InstrumentRemove { .. } => BASE,
        }
    }

    /// Adds what this change touches to `out` (read before it is applied or
    /// after: the same places). The change holds the state the song does not.
    pub(crate) fn touch(&self, s: &SidSong, out: &mut Changes) {
        match self {
            Change::Rows {
                pattern,
                row0,
                rows,
            } => {
                if let Some(p) = s.patterns.get(*pattern) {
                    for (i, old) in rows.iter().enumerate() {
                        let row = row0 + i;
                        if p.rows.get(row).is_some_and(|now| now != old) {
                            out.pattern_rows(*pattern, row, row);
                            out.cell(*pattern, row);
                        }
                    }
                }
            }
            Change::Pattern { index, pattern } => match s.patterns.get(*index) {
                Some(now) if now.rows.len() == pattern.rows.len() => {
                    for (row, (a, b)) in now.rows.iter().zip(&pattern.rows).enumerate() {
                        if a != b {
                            out.pattern_rows(*index, row, row);
                            out.cell(*index, row);
                        }
                    }
                }
                _ => {
                    out.pattern_rows(*index, 0, pattern.rows.len().max(1) - 1);
                    out.layout = true;
                }
            },
            Change::PatternInsert { .. } | Change::PatternRemove { .. } => out.layout = true,
            Change::Order { subsong, voice, .. } => {
                out.orders.insert((*subsong, *voice));
            }
            Change::SubsongInsert { .. } | Change::SubsongRemove { .. } => out.subsongs = true,
            Change::Instrument { index, instrument } => {
                if s.instruments
                    .get(*index)
                    .is_some_and(|now| now != instrument)
                {
                    out.instrument(index + 1);
                }
            }
            Change::InstrumentInsert { index, .. } | Change::InstrumentRemove { index } => {
                out.instrument(index + 1);
                out.instrument_list = true;
            }
            Change::Table { kind, rows } => {
                let now = s.tables.get(*kind);
                if now.len() != rows.len() {
                    out.table_resized = true;
                    out.table_rows(*kind, 0, now.len().max(rows.len()).max(1) - 1);
                } else {
                    let differs: Vec<usize> =
                        (0..now.len()).filter(|&i| now[i] != rows[i]).collect();
                    if let (Some(&a), Some(&b)) = (differs.first(), differs.last()) {
                        out.table_rows(*kind, a, b);
                    }
                }
            }
            Change::Header(h) => {
                out.texts |= h.name != s.name || h.author != s.author || h.copyright != s.copyright;
                out.timing |= h.speed_multiplier != s.speed_multiplier || h.tempo != s.tempo;
                out.model |= h.model != s.model;
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Step {
    pub changes: Vec<Change>,
    bytes: usize,
}

impl Step {
    fn new(changes: Vec<Change>) -> Step {
        let bytes = changes.iter().map(Change::bytes).sum();
        Step { changes, bytes }
    }
}

/// The undo and redo stacks and the group being recorded.
#[derive(Debug)]
pub(crate) struct History {
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    /// The inverse changes of the group being recorded, in the order made.
    pub open: Vec<Change>,
    pub depth: usize,
    bytes: usize,
    pub max_steps: usize,
    pub max_bytes: usize,
}

/// A default bound on the log, as `cubed_tracker::edit` has it: 1000 steps and
/// 128 MiB (a song is far smaller: a step is rarely more than a pattern's 512 bytes).
pub const DEFAULT_MAX_STEPS: usize = 1000;
pub const DEFAULT_MAX_BYTES: usize = 128 << 20;

impl History {
    pub fn new() -> History {
        History {
            undo: VecDeque::new(),
            redo: Vec::new(),
            open: Vec::new(),
            depth: 0,
            bytes: 0,
            max_steps: DEFAULT_MAX_STEPS,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    pub fn can_undo(&self) -> bool {
        self.depth == 0 && !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        self.depth == 0 && !self.redo.is_empty()
    }

    pub fn undo_steps(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_steps(&self) -> usize {
        self.redo.len()
    }

    pub fn bytes(&self) -> usize {
        self.bytes + self.open.iter().map(Change::bytes).sum::<usize>()
    }

    /// Closes the open group as an undo step (a new edit ends the redo line).
    pub fn commit(&mut self) {
        if self.open.is_empty() {
            return;
        }
        self.redo.clear();
        let step = Step::new(std::mem::take(&mut self.open));
        self.bytes += step.bytes;
        self.undo.push_back(step);
        self.trim();
    }

    fn trim(&mut self) {
        while self.undo.len() > 1
            && (self.undo.len() > self.max_steps || self.bytes > self.max_bytes)
        {
            if let Some(old) = self.undo.pop_front() {
                self.bytes -= old.bytes;
            }
        }
    }

    pub fn pop_undo(&mut self) -> Option<Step> {
        let step = self.undo.pop_back()?;
        self.bytes -= step.bytes;
        Some(step)
    }

    pub fn pop_redo(&mut self) -> Option<Step> {
        self.redo.pop()
    }

    pub fn push_redo(&mut self, changes: Vec<Change>) {
        self.redo.push(Step::new(changes));
    }

    pub fn push_undo_keep_redo(&mut self, changes: Vec<Change>) {
        let step = Step::new(changes);
        self.bytes += step.bytes;
        self.undo.push_back(step);
        self.trim();
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.open.clear();
        self.depth = 0;
        self.bytes = 0;
    }
}
