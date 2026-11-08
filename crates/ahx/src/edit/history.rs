//! Undo and redo as an inverse-change log.
//!
//! Every edit records, before it changes anything, the [`Change`] that puts back
//! what it is about to overwrite: some rows of a track, a position's entry, the
//! header, an instrument's fields, some rows of a PList. Applying a change
//! returns its own inverse, so undoing a step produces the redo step and the
//! other way round. Only what an edit touches is ever copied: typing a note
//! stores one step, not the song.
//!
//! One user action is one step: edits made between `begin_group` and
//! `end_group` (they nest) are undone together. The log is bounded in steps and
//! in bytes; the oldest steps are dropped first.

use std::collections::{BTreeMap, VecDeque};
use std::mem::size_of;

use serde_json::{json, Value};

use crate::format::{Instrument, PListEntry, Position, Song, Step, Track};

/// What an edit touched, for a UI to redraw and a playing song to follow.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    /// Track number to the rows (first, last, inclusive) that changed.
    pub tracks: BTreeMap<usize, (usize, usize)>,
    /// The first and last position whose entry (a channel's track or
    /// transpose) changed.
    pub positions: Option<(usize, usize)>,
    /// Positions were inserted, removed or moved: refresh the whole list.
    pub position_list: bool,
    /// The track length or the list of tracks changed (tracks added, tracks
    /// compacted): refresh sizes and lists.
    pub layout: bool,
    /// The name, the speed multiplier, the HVL mix or the subsong list.
    pub header: bool,
    /// The restart position.
    pub restart: bool,
    /// Instruments (1-based) whose fields or PList changed, the name included.
    pub instruments: Vec<usize>,
    /// Instrument to the PList rows (first, last, inclusive) that changed; an
    /// insert or a delete reports the rows from the first moved one to the end.
    pub plists: BTreeMap<usize, (usize, usize)>,
    /// Instruments were added.
    pub instrument_list: bool,
    /// The (track, row) of every step whose content changed, each once, up to
    /// [`MAX_STEPS`] (past that `steps_overflow` is set and the list is not
    /// complete). Steps of tracks swapped whole are not listed: those set
    /// `layout`.
    pub steps: Vec<(usize, usize)>,
    pub steps_overflow: bool,
    /// The (position, channel) of every entry whose track or transpose changed.
    pub position_cells: Vec<(usize, usize)>,
    /// Instruments whose change can be heard (anything but a rename).
    pub sounding: Vec<usize>,
    /// The speed multiplier, the mix or the subsong list changed.
    pub header_sounds: bool,
    /// Tracks added minus tracks removed (an edit that gave a cell a track of
    /// its own appends one; undoing it takes it off again).
    pub tracks_net: i32,
    /// Instruments added minus instruments removed.
    pub instruments_net: i32,
}

/// How many changed steps a [`Changes`] lists.
pub const MAX_STEPS: usize = 1024;

impl Changes {
    /// Whether what changed is nothing a playing song cannot take without being
    /// opened again: steps, position entries, the restart position, instruments
    /// (their fields and PLists, and instruments or tracks added at the end:
    /// see `live.rs`). Anything that moves the rest of the song's shape (the
    /// track length, tracks compacted, the position list, the speed multiplier
    /// and the HVL mix) is a new song.
    pub fn is_live(&self) -> bool {
        !self.layout && !self.position_list && !self.header_sounds && !self.steps_overflow
    }

    pub fn is_empty(&self) -> bool {
        *self == Changes::default()
    }

    pub(crate) fn step(&mut self, track: usize, row: usize) {
        let key = (track, row);
        if self.steps.contains(&key) {
            return;
        }
        if self.steps.len() >= MAX_STEPS {
            self.steps_overflow = true;
        } else {
            self.steps.push(key);
        }
    }

    pub(crate) fn track_rows(&mut self, track: usize, first: usize, last: usize) {
        let e = self.tracks.entry(track).or_insert((first, last));
        e.0 = e.0.min(first);
        e.1 = e.1.max(last);
    }

    pub(crate) fn position(&mut self, position: usize, channel: usize) {
        let key = (position, channel);
        if !self.position_cells.contains(&key) {
            self.position_cells.push(key);
        }
        let e = self.positions.get_or_insert((position, position));
        e.0 = e.0.min(position);
        e.1 = e.1.max(position);
    }

    pub(crate) fn plist_rows(&mut self, instrument: usize, first: usize, last: usize) {
        let e = self.plists.entry(instrument).or_insert((first, last));
        e.0 = e.0.min(first);
        e.1 = e.1.max(last);
    }

    pub(crate) fn instrument(&mut self, number: usize, sounds: bool) {
        if !self.instruments.contains(&number) {
            self.instruments.push(number);
            self.instruments.sort_unstable();
        }
        if sounds && !self.sounding.contains(&number) {
            self.sounding.push(number);
            self.sounding.sort_unstable();
        }
    }

    /// `{"tracks": [{"track", "row0", "row1"}], "positions": {"first", "last"},
    /// "position_list", "layout", "header", "restart", "instruments": [n],
    /// "plists": [{"instrument", "row0", "row1"}], "instrument_list"}`; only
    /// what changed is present.
    pub fn to_json(&self) -> Value {
        let mut v = json!({});
        if !self.tracks.is_empty() {
            v["tracks"] = Value::Array(
                self.tracks
                    .iter()
                    .map(|(&t, &(a, b))| json!({"track": t, "row0": a, "row1": b}))
                    .collect(),
            );
        }
        if let Some((a, b)) = self.positions {
            v["positions"] = json!({"first": a, "last": b});
        }
        for (key, on) in [
            ("position_list", self.position_list),
            ("layout", self.layout),
            ("header", self.header),
            ("restart", self.restart),
            ("instrument_list", self.instrument_list),
        ] {
            if on {
                v[key] = json!(true);
            }
        }
        if !self.instruments.is_empty() {
            v["instruments"] = json!(self.instruments);
        }
        if !self.plists.is_empty() {
            v["plists"] = Value::Array(
                self.plists
                    .iter()
                    .map(|(&i, &(a, b))| json!({"instrument": i, "row0": a, "row1": b}))
                    .collect(),
            );
        }
        v
    }
}

/// The song-wide fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeaderState {
    pub name: String,
    pub speed_multiplier: u8,
    pub restart: u16,
    pub subsongs: Vec<u16>,
    pub mixgain_raw: Option<u8>,
    pub defstereo: Option<u8>,
}

impl HeaderState {
    pub fn of(s: &Song) -> HeaderState {
        HeaderState {
            name: s.name.clone(),
            speed_multiplier: s.speed_multiplier,
            restart: s.restart,
            subsongs: s.subsongs.clone(),
            mixgain_raw: s.mixgain_raw,
            defstereo: s.defstereo,
        }
    }
}

/// The track table with its row count (and the positions that name tracks,
/// when an edit renumbers them).
#[derive(Clone, Debug)]
pub(crate) struct LayoutState {
    pub track_length: u8,
    pub tracks: Vec<Track>,
    pub positions: Option<Vec<Position>>,
}

/// A recorded change: applying it to a song puts back what it holds and
/// returns the change that undoes that.
#[derive(Clone, Debug)]
pub(crate) enum Change {
    /// Rows of a track, swapped in place.
    Steps {
        track: usize,
        row0: usize,
        steps: Vec<Step>,
    },
    /// A track added at the end.
    TrackPush {
        steps: Track,
    },
    TrackPop,
    /// The track table and the row count.
    Layout(Box<LayoutState>),
    /// A position's entry, swapped in place.
    Position {
        index: usize,
        entry: Position,
    },
    PositionInsert {
        index: usize,
        entry: Position,
    },
    PositionRemove {
        index: usize,
    },
    Header(Box<HeaderState>),
    /// An instrument's fields (everything but its PList rows).
    InstrumentCore {
        index: usize,
        ins: Box<Instrument>,
    },
    /// Rows of a PList, swapped in place.
    PListRows {
        index: usize,
        row0: usize,
        entries: Vec<PListEntry>,
    },
    /// A whole PList (rows inserted or deleted).
    PListAll {
        index: usize,
        entries: Vec<PListEntry>,
    },
    /// An instrument added at the end.
    InstrumentPush {
        ins: Box<Instrument>,
    },
    InstrumentPop,
}

/// Brings the song's own counts in line with its lists.
pub(crate) fn sync_counts(s: &mut Song) {
    s.position_nr = s.positions.len();
    s.track_nr = s.tracks.len().saturating_sub(1) as u8;
    s.instrument_nr = s.instruments.len().saturating_sub(1) as u8;
    s.subsong_nr = s.subsongs.len() as u8;
}

impl Change {
    /// Puts this change's state into `s`; returns the change that restores
    /// what was there.
    pub(crate) fn apply(self, s: &mut Song) -> Change {
        match self {
            Change::Steps {
                track,
                row0,
                mut steps,
            } => {
                let t = &mut s.tracks[track];
                for (i, step) in steps.iter_mut().enumerate() {
                    std::mem::swap(&mut t[row0 + i], step);
                }
                Change::Steps { track, row0, steps }
            }
            Change::TrackPush { steps } => {
                s.tracks.push(steps);
                sync_counts(s);
                Change::TrackPop
            }
            Change::TrackPop => {
                let steps = s.tracks.pop().expect("a track to pop");
                sync_counts(s);
                Change::TrackPush { steps }
            }
            Change::Layout(mut l) => {
                std::mem::swap(&mut s.track_length, &mut l.track_length);
                std::mem::swap(&mut s.tracks, &mut l.tracks);
                if let Some(p) = l.positions.as_mut() {
                    std::mem::swap(&mut s.positions, p);
                }
                sync_counts(s);
                Change::Layout(l)
            }
            Change::Position { index, mut entry } => {
                std::mem::swap(&mut s.positions[index], &mut entry);
                Change::Position { index, entry }
            }
            Change::PositionInsert { index, entry } => {
                s.positions.insert(index, entry);
                sync_counts(s);
                Change::PositionRemove { index }
            }
            Change::PositionRemove { index } => {
                let entry = s.positions.remove(index);
                sync_counts(s);
                Change::PositionInsert { index, entry }
            }
            Change::Header(mut h) => {
                std::mem::swap(&mut s.name, &mut h.name);
                std::mem::swap(&mut s.speed_multiplier, &mut h.speed_multiplier);
                std::mem::swap(&mut s.restart, &mut h.restart);
                std::mem::swap(&mut s.subsongs, &mut h.subsongs);
                std::mem::swap(&mut s.mixgain_raw, &mut h.mixgain_raw);
                std::mem::swap(&mut s.defstereo, &mut h.defstereo);
                sync_counts(s);
                Change::Header(h)
            }
            Change::InstrumentCore { index, mut ins } => {
                let cur = &mut s.instruments[index];
                // Swap the PList rows first so the whole-instrument swap leaves
                // each side with the rows it had.
                std::mem::swap(&mut cur.plist.entries, &mut ins.plist.entries);
                std::mem::swap(cur, &mut *ins);
                Change::InstrumentCore { index, ins }
            }
            Change::PListRows {
                index,
                row0,
                mut entries,
            } => {
                let list = &mut s.instruments[index].plist.entries;
                for (i, e) in entries.iter_mut().enumerate() {
                    std::mem::swap(&mut list[row0 + i], e);
                }
                Change::PListRows {
                    index,
                    row0,
                    entries,
                }
            }
            Change::PListAll { index, mut entries } => {
                std::mem::swap(&mut s.instruments[index].plist.entries, &mut entries);
                Change::PListAll { index, entries }
            }
            Change::InstrumentPush { ins } => {
                s.instruments.push(*ins);
                sync_counts(s);
                Change::InstrumentPop
            }
            Change::InstrumentPop => {
                let ins = s.instruments.pop().expect("an instrument to pop");
                sync_counts(s);
                Change::InstrumentPush { ins: Box::new(ins) }
            }
        }
    }

    /// Roughly what this change costs to keep, in bytes.
    pub(crate) fn bytes(&self) -> usize {
        const BASE: usize = 64;
        let track_bytes = |t: &Track| t.len() * size_of::<Step>() + size_of::<Track>();
        let position_bytes = |p: &Position| p.track.len() * 2 + 2 * size_of::<Vec<u8>>();
        let ins_bytes = |i: &Instrument| {
            size_of::<Instrument>() + i.name.len() + i.plist.entries.len() * size_of::<PListEntry>()
        };
        match self {
            Change::Steps { steps, .. } => BASE + steps.len() * size_of::<Step>(),
            Change::TrackPush { steps } => BASE + track_bytes(steps),
            Change::TrackPop => BASE,
            Change::Layout(l) => {
                BASE + l.tracks.iter().map(track_bytes).sum::<usize>()
                    + l.positions
                        .as_ref()
                        .map_or(0, |p| p.iter().map(position_bytes).sum())
            }
            Change::Position { entry, .. } | Change::PositionInsert { entry, .. } => {
                BASE + position_bytes(entry)
            }
            Change::PositionRemove { .. } => BASE,
            Change::Header(h) => BASE + h.name.len() + h.subsongs.len() * 2,
            Change::InstrumentCore { ins, .. } | Change::InstrumentPush { ins } => {
                BASE + ins_bytes(ins)
            }
            Change::PListRows { entries, .. } | Change::PListAll { entries, .. } => {
                BASE + entries.len() * size_of::<PListEntry>()
            }
            Change::InstrumentPop => BASE,
        }
    }

    /// Adds what this change touches to `out`. Called with the change an edit
    /// recorded (it holds what was there) after the edit, or the one an undo
    /// produced (it holds what is there now): either way it compares what it
    /// holds with the song and reports what differs.
    pub(crate) fn touch(&self, s: &Song, out: &mut Changes) {
        match self {
            Change::Steps { track, row0, steps } => {
                if let Some(t) = s.tracks.get(*track) {
                    out.track_rows(*track, *row0, row0 + steps.len().saturating_sub(1));
                    for (i, old) in steps.iter().enumerate() {
                        if t.get(row0 + i).is_some_and(|now| now != old) {
                            out.step(*track, row0 + i);
                        }
                    }
                }
            }
            // `touch` is given the inverse of what was done: a pop undoes an
            // append, a push undoes a removal.
            Change::TrackPop => out.tracks_net += 1,
            Change::TrackPush { .. } => out.tracks_net -= 1,
            Change::Layout(_) => out.layout = true,
            Change::Position { index, entry } => {
                if let Some(now) = s.positions.get(*index) {
                    for ch in 0..entry.track.len().min(now.track.len()) {
                        if entry.track[ch] != now.track[ch]
                            || entry.transpose[ch] != now.transpose[ch]
                        {
                            out.position(*index, ch);
                        }
                    }
                }
            }
            Change::PositionInsert { .. } | Change::PositionRemove { .. } => {
                out.position_list = true;
            }
            Change::Header(h) => {
                if h.name != s.name {
                    out.header = true;
                }
                if h.speed_multiplier != s.speed_multiplier
                    || h.subsongs != s.subsongs
                    || h.mixgain_raw != s.mixgain_raw
                    || h.defstereo != s.defstereo
                {
                    out.header = true;
                    out.header_sounds = true;
                }
                if h.restart != s.restart {
                    out.restart = true;
                }
            }
            Change::InstrumentCore { index, ins } => {
                if let Some(now) = s.instruments.get(*index) {
                    let (changed, sounds) = core_differs(ins, now);
                    if changed {
                        out.instrument(*index, sounds);
                    }
                }
            }
            Change::PListRows {
                index,
                row0,
                entries,
            } => {
                if let Some(now) = s.instruments.get(*index) {
                    if entries
                        .iter()
                        .enumerate()
                        .any(|(i, e)| now.plist.entries.get(row0 + i) != Some(e))
                    {
                        out.instrument(*index, true);
                        out.plist_rows(*index, *row0, row0 + entries.len().saturating_sub(1));
                    }
                }
            }
            Change::PListAll { index, entries } => {
                if let Some(now) = s.instruments.get(*index) {
                    let old = entries;
                    let new = &now.plist.entries;
                    if old != new {
                        out.instrument(*index, true);
                        let first = old
                            .iter()
                            .zip(new)
                            .position(|(a, b)| a != b)
                            .unwrap_or(old.len().min(new.len()));
                        out.plist_rows(*index, first, old.len().max(new.len()).saturating_sub(1));
                    }
                }
            }
            Change::InstrumentPop => {
                out.instrument_list = true;
                out.instruments_net += 1;
            }
            Change::InstrumentPush { .. } => {
                out.instrument_list = true;
                out.instruments_net -= 1;
            }
        }
    }
}

/// How two instruments' fields (not their PList rows) differ: at all, and in a
/// way that can be heard (anything but the name).
pub(crate) fn core_differs(a: &Instrument, b: &Instrument) -> (bool, bool) {
    let sounds = a.volume != b.volume
        || a.wave_length != b.wave_length
        || a.filter_lower_limit != b.filter_lower_limit
        || a.filter_upper_limit != b.filter_upper_limit
        || a.filter_speed != b.filter_speed
        || a.square_lower_limit != b.square_lower_limit
        || a.square_upper_limit != b.square_upper_limit
        || a.square_speed != b.square_speed
        || a.vibrato_delay != b.vibrato_delay
        || a.vibrato_speed != b.vibrato_speed
        || a.vibrato_depth != b.vibrato_depth
        || a.hard_cut_release != b.hard_cut_release
        || a.hard_cut_release_frames != b.hard_cut_release_frames
        || a.envelope != b.envelope
        || a.plist.speed != b.plist.speed;
    (sounds || a.name != b.name, sounds)
}

#[derive(Debug)]
pub(crate) struct UndoStep {
    pub changes: Vec<Change>,
    bytes: usize,
}

impl UndoStep {
    fn new(changes: Vec<Change>) -> UndoStep {
        let bytes = changes.iter().map(Change::bytes).sum();
        UndoStep { changes, bytes }
    }
}

/// The undo and redo stacks and the group being recorded.
#[derive(Debug)]
pub(crate) struct History {
    undo: VecDeque<UndoStep>,
    redo: Vec<UndoStep>,
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
        let step = UndoStep::new(std::mem::take(&mut self.open));
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

    pub fn pop_undo(&mut self) -> Option<UndoStep> {
        let step = self.undo.pop_back()?;
        self.bytes -= step.bytes;
        Some(step)
    }

    pub fn pop_redo(&mut self) -> Option<UndoStep> {
        self.redo.pop()
    }

    pub fn push_redo(&mut self, changes: Vec<Change>) {
        self.redo.push(UndoStep::new(changes));
    }

    pub fn push_undo_keep_redo(&mut self, changes: Vec<Change>) {
        let step = UndoStep::new(changes);
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
