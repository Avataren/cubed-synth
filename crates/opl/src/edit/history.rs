//! Undo and redo as an inverse-change log (the design of
//! `cubed_tracker::edit`'s history).
//!
//! Every edit records, before it changes anything, the [`Change`] that puts back
//! what it is about to overwrite: a rectangle of cells, one pattern, the order
//! list, an instrument, a macro, the header. Applying a change returns its own
//! inverse, so undoing a step produces the redo step and the other way round.
//! Only what an edit touches is ever copied: typing a note stores one cell, not
//! the song (a song is megabytes of macro tables and patterns).
//!
//! One user action is one step: edits made between `begin_group` and
//! `end_group` (they nest) are undone together. The log is bounded in steps and
//! in bytes; the oldest steps are dropped first.

use std::collections::{BTreeMap, VecDeque};
use std::mem::size_of;

use serde_json::{json, Value};

use crate::a2::model::{A2mSong, ArpeggioMacro, Cell, FmMacro, Instrument, Pattern, VibratoMacro};

/// Which macro table a change touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MacroKind {
    Fm,
    Arpeggio,
    Vibrato,
}

impl MacroKind {
    pub fn name(self) -> &'static str {
        match self {
            MacroKind::Fm => "fm",
            MacroKind::Arpeggio => "arpeggio",
            MacroKind::Vibrato => "vibrato",
        }
    }

    pub fn parse(name: &str) -> Option<MacroKind> {
        match name {
            "fm" => Some(MacroKind::Fm),
            "arpeggio" | "arp" => Some(MacroKind::Arpeggio),
            "vibrato" | "vib" => Some(MacroKind::Vibrato),
            _ => None,
        }
    }
}

/// What an edit touched, for a UI to redraw and for a playing song to learn
/// what it can take live.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    /// Pattern number to the rows (first, last, inclusive) that changed.
    pub patterns: BTreeMap<usize, (usize, usize)>,
    /// Patterns were added, removed or reshaped (their count, the song's
    /// length or its track count): refresh lists and sizes.
    pub layout: bool,
    /// The order list (and so the restart position).
    pub orders: bool,
    /// The title or composer.
    pub header: bool,
    /// Tempo or speed.
    pub timing: bool,
    /// Another song field: the flags, tracks, pattern length, macro
    /// speed-up, lock flags, 4-op pairs, rows per beat, tempo fine-tune.
    pub song: bool,
    /// Pattern names.
    pub names: bool,
    /// Instruments whose data changed (1-based).
    pub instruments: Vec<usize>,
    /// Macro tables that changed (1-based).
    pub macros: Vec<(MacroKind, usize)>,
    /// The cells whose content changed, as (pattern, row, channel), each once
    /// (up to [`MAX_CELLS`]; past that `cells_overflow` is set and the list is
    /// not complete). Cells of patterns swapped whole are not listed: those set
    /// `layout`.
    pub cells: Vec<(usize, usize, usize)>,
    pub cells_overflow: bool,
}

/// How many changed cells a [`Changes`] lists.
pub const MAX_CELLS: usize = 1024;

impl Changes {
    /// Whether a playing song can take the change without being reopened:
    /// only cell content, instrument data, macro tables, tempo and speed, the
    /// title, composer and pattern names moved (see `A2mDoc::live_edits`).
    pub fn is_live(&self) -> bool {
        !self.layout && !self.orders && !self.song && !self.cells_overflow
    }

    pub(crate) fn cell(&mut self, pattern: usize, row: usize, channel: usize) {
        let key = (pattern, row, channel);
        if self.cells.contains(&key) {
            return;
        }
        if self.cells.len() >= MAX_CELLS {
            self.cells_overflow = true;
        } else {
            self.cells.push(key);
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Changes::default()
    }

    pub(crate) fn pattern_rows(&mut self, pattern: usize, first: usize, last: usize) {
        let e = self.patterns.entry(pattern).or_insert((first, last));
        e.0 = e.0.min(first);
        e.1 = e.1.max(last);
    }

    pub(crate) fn instrument(&mut self, number: usize) {
        if !self.instruments.contains(&number) {
            self.instruments.push(number);
            self.instruments.sort_unstable();
        }
    }

    pub(crate) fn a_macro(&mut self, kind: MacroKind, number: usize) {
        if !self.macros.contains(&(kind, number)) {
            self.macros.push((kind, number));
            self.macros.sort_unstable();
        }
    }

    /// `{"patterns": [{"pattern", "row0", "row1"}], "layout", "orders",
    /// "header", "timing", "song", "names", "instruments": [n], "macros":
    /// [{"kind", "macro"}]}`; only what changed is present.
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
        for (key, on) in [
            ("layout", self.layout),
            ("orders", self.orders),
            ("header", self.header),
            ("timing", self.timing),
            ("song", self.song),
            ("names", self.names),
        ] {
            if on {
                v[key] = json!(true);
            }
        }
        if !self.instruments.is_empty() {
            v["instruments"] = json!(self.instruments);
        }
        if !self.macros.is_empty() {
            v["macros"] = Value::Array(
                self.macros
                    .iter()
                    .map(|(k, n)| json!({"kind": k.name(), "macro": n}))
                    .collect(),
            );
        }
        v
    }
}

/// The song-wide fields (everything but the order list, names and tables).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeaderState {
    pub name: Vec<u8>,
    pub composer: Vec<u8>,
    pub tempo: u8,
    pub speed: u8,
    pub flags: u8,
    pub pattern_len: u16,
    pub tracks: u8,
    pub macro_speedup: u16,
    pub four_op_tracks: u8,
    pub lock_flags: [u8; 20],
    pub four_op_instruments: Vec<u8>,
    pub rows_per_beat: Option<u8>,
    pub tempo_finetune: Option<i16>,
}

impl HeaderState {
    pub fn of(s: &A2mSong) -> HeaderState {
        HeaderState {
            name: s.name.clone(),
            composer: s.composer.clone(),
            tempo: s.tempo,
            speed: s.speed,
            flags: s.flags,
            pattern_len: s.pattern_len,
            tracks: s.tracks,
            macro_speedup: s.macro_speedup,
            four_op_tracks: s.four_op_tracks,
            lock_flags: s.lock_flags,
            four_op_instruments: s.four_op_instruments.clone(),
            rows_per_beat: s.rows_per_beat,
            tempo_finetune: s.tempo_finetune,
        }
    }

    fn swap(&mut self, s: &mut A2mSong) {
        std::mem::swap(&mut s.name, &mut self.name);
        std::mem::swap(&mut s.composer, &mut self.composer);
        std::mem::swap(&mut s.tempo, &mut self.tempo);
        std::mem::swap(&mut s.speed, &mut self.speed);
        std::mem::swap(&mut s.flags, &mut self.flags);
        std::mem::swap(&mut s.pattern_len, &mut self.pattern_len);
        std::mem::swap(&mut s.tracks, &mut self.tracks);
        std::mem::swap(&mut s.macro_speedup, &mut self.macro_speedup);
        std::mem::swap(&mut s.four_op_tracks, &mut self.four_op_tracks);
        std::mem::swap(&mut s.lock_flags, &mut self.lock_flags);
        std::mem::swap(&mut s.four_op_instruments, &mut self.four_op_instruments);
        std::mem::swap(&mut s.rows_per_beat, &mut self.rows_per_beat);
        std::mem::swap(&mut s.tempo_finetune, &mut self.tempo_finetune);
    }
}

/// A recorded change: applying it to a song puts back what it holds and
/// returns the change that undoes that.
#[derive(Clone, Debug)]
pub(crate) enum Change {
    /// A rectangle of cells, row by row.
    Block {
        pattern: usize,
        row0: usize,
        channel0: usize,
        rows: usize,
        channels: usize,
        cells: Vec<Cell>,
    },
    PatternInsert {
        index: usize,
        pattern: Pattern,
    },
    PatternRemove {
        index: usize,
    },
    /// The unnamed patterns that fill the file's last block.
    Spare(Vec<Pattern>),
    Order(Box<[u8; 128]>),
    Header(Box<HeaderState>),
    PatternNames(Vec<Vec<u8>>),
    Instrument {
        index: usize,
        instrument: Box<Instrument>,
    },
    FmMacro {
        index: usize,
        table: Box<FmMacro>,
    },
    ArpMacro {
        index: usize,
        table: Box<ArpeggioMacro>,
    },
    VibMacro {
        index: usize,
        table: Box<VibratoMacro>,
    },
    Columns {
        index: usize,
        columns: [u8; 28],
    },
}

impl Change {
    /// Puts this change's state into `s`; returns the change that restores
    /// what was there.
    pub(crate) fn apply(self, s: &mut A2mSong) -> Change {
        match self {
            Change::Block {
                pattern,
                row0,
                channel0,
                rows,
                channels,
                mut cells,
            } => {
                let p = &mut s.patterns[pattern];
                let width = p.channels;
                for r in 0..rows {
                    for c in 0..channels {
                        std::mem::swap(
                            &mut p.cells[(row0 + r) * width + channel0 + c],
                            &mut cells[r * channels + c],
                        );
                    }
                }
                Change::Block {
                    pattern,
                    row0,
                    channel0,
                    rows,
                    channels,
                    cells,
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
            Change::Spare(mut spare) => {
                std::mem::swap(&mut s.spare_patterns, &mut spare);
                Change::Spare(spare)
            }
            Change::Order(mut order) => {
                std::mem::swap(&mut s.order, &mut order);
                Change::Order(order)
            }
            Change::Header(mut h) => {
                h.swap(s);
                Change::Header(h)
            }
            Change::PatternNames(mut names) => {
                std::mem::swap(&mut s.pattern_names, &mut names);
                Change::PatternNames(names)
            }
            Change::Instrument {
                index,
                mut instrument,
            } => {
                std::mem::swap(&mut s.instruments[index], &mut *instrument);
                Change::Instrument { index, instrument }
            }
            Change::FmMacro { index, mut table } => {
                std::mem::swap(&mut s.fm_macros[index], &mut *table);
                Change::FmMacro { index, table }
            }
            Change::ArpMacro { index, mut table } => {
                std::mem::swap(&mut s.arpeggio_macros[index], &mut *table);
                Change::ArpMacro { index, table }
            }
            Change::VibMacro { index, mut table } => {
                std::mem::swap(&mut s.vibrato_macros[index], &mut *table);
                Change::VibMacro { index, table }
            }
            Change::Columns { index, mut columns } => {
                std::mem::swap(&mut s.disabled_fm_columns[index], &mut columns);
                Change::Columns { index, columns }
            }
        }
    }

    /// Roughly what this change costs to keep, in bytes.
    pub(crate) fn bytes(&self) -> usize {
        const BASE: usize = 64;
        match self {
            Change::Block { cells, .. } => BASE + cells.len() * size_of::<Cell>(),
            Change::PatternInsert { pattern, .. } => BASE + pattern.cells.len() * size_of::<Cell>(),
            Change::Spare(spare) => {
                BASE + spare
                    .iter()
                    .map(|p| p.cells.len() * size_of::<Cell>())
                    .sum::<usize>()
            }
            Change::Order(_) => BASE + 128,
            Change::Header(h) => {
                BASE + h.name.len() + h.composer.len() + h.four_op_instruments.len()
            }
            Change::PatternNames(n) => BASE + n.iter().map(|s| s.len() + 24).sum::<usize>(),
            Change::Instrument { instrument, .. } => BASE + 32 + instrument.name.len(),
            Change::FmMacro { table, .. } => BASE + table.steps.len() * 15,
            Change::ArpMacro { table, .. } => BASE + table.data.len() + 8,
            Change::VibMacro { table, .. } => BASE + table.data.len() + 8,
            Change::Columns { .. } => BASE + 28,
            Change::PatternRemove { .. } => BASE,
        }
    }

    /// Adds what this change touches to `out`: it holds the state that was
    /// replaced and `s` the state now in place.
    pub(crate) fn touch(&self, s: &A2mSong, out: &mut Changes) {
        match self {
            Change::Block {
                pattern,
                row0,
                channel0,
                rows,
                channels,
                cells,
            } => {
                out.pattern_rows(*pattern, *row0, row0 + rows.saturating_sub(1));
                if let Some(p) = s.patterns.get(*pattern) {
                    for r in 0..*rows {
                        for c in 0..*channels {
                            let (row, ch) = (row0 + r, channel0 + c);
                            if row < p.rows
                                && ch < p.channels
                                && cells[r * channels + c] != *p.cell(row, ch)
                            {
                                out.cell(*pattern, row, ch);
                            }
                        }
                    }
                }
            }
            Change::PatternInsert { .. } | Change::PatternRemove { .. } | Change::Spare(_) => {
                out.layout = true
            }
            Change::Order(_) => out.orders = true,
            Change::Header(h) => {
                let now = HeaderState::of(s);
                if h.name != now.name || h.composer != now.composer {
                    out.header = true;
                }
                if (h.tempo, h.speed) != (now.tempo, now.speed) {
                    out.timing = true;
                }
                if h.pattern_len != now.pattern_len || h.tracks != now.tracks {
                    out.layout = true;
                }
                let (mut a, mut b) = ((**h).clone(), now);
                (a.name, a.composer, a.tempo, a.speed) = (Vec::new(), Vec::new(), 0, 0);
                (b.name, b.composer, b.tempo, b.speed) = (Vec::new(), Vec::new(), 0, 0);
                if a != b {
                    out.song = true;
                }
            }
            Change::PatternNames(_) => out.names = true,
            Change::Instrument { index, .. } => out.instrument(index + 1),
            Change::FmMacro { index, .. } => out.a_macro(MacroKind::Fm, index + 1),
            Change::ArpMacro { index, .. } => out.a_macro(MacroKind::Arpeggio, index + 1),
            Change::VibMacro { index, .. } => out.a_macro(MacroKind::Vibrato, index + 1),
            // The columns a macro step may not write: part of the song's tables.
            Change::Columns { index, .. } => out.instrument(index + 1),
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

/// A default bound on the log: 1000 steps and 128 MiB.
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
