//! Undo and redo as an inverse-change log.
//!
//! Every edit records, before it changes anything, the [`Change`] that puts
//! back what it is about to overwrite: a rectangle of cells, one pattern, a
//! sample's data (moved out, not copied), the order list, the header. Applying
//! a change returns its own inverse, so undoing a step produces the redo step
//! and the other way round. Only what an edit touches is ever copied: typing
//! a note stores one cell, not the song.
//!
//! One user action is one step: edits made between `begin_group` and
//! `end_group` (they nest) are undone together. The log is bounded in steps
//! and in bytes; the oldest steps are dropped first.

use std::collections::{BTreeMap, VecDeque};
use std::mem::size_of;

use serde_json::{json, Value};

use crate::module::{Cell, Flags, Instrument, ModFlavor, Module, Pattern, Sample};

/// What an edit touched, for a UI to redraw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    /// Pattern number to the rows (first, last, inclusive) that changed.
    pub patterns: BTreeMap<usize, (usize, usize)>,
    /// Patterns were added, removed or reshaped (their count, row count or
    /// channel count): refresh lists and sizes.
    pub layout: bool,
    /// The order list or the restart position.
    pub orders: bool,
    /// Title, speed, tempo, flags, global volume.
    pub header: bool,
    /// Channel count, pans, mutes.
    pub channels: bool,
    /// Instruments whose data, fields or samples changed (1-based; a MOD's or
    /// S3M's sample slots count as instruments of one sample).
    pub instruments: Vec<usize>,
    /// Instruments or sample slots were added or removed.
    pub instrument_list: bool,
}

impl Changes {
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

    pub(crate) fn merge(&mut self, other: &Changes) {
        for (&p, &(a, b)) in &other.patterns {
            self.pattern_rows(p, a, b);
        }
        self.layout |= other.layout;
        self.orders |= other.orders;
        self.header |= other.header;
        self.channels |= other.channels;
        self.instrument_list |= other.instrument_list;
        for &i in &other.instruments {
            self.instrument(i);
        }
    }

    /// `{"patterns": [{"pattern", "row0", "row1"}], "layout", "orders",
    /// "header", "channels", "instruments": [n], "instrument_list"}`; only
    /// what changed is present.
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
            ("channels", self.channels),
            ("instrument_list", self.instrument_list),
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

/// The order list with its companions.
#[derive(Clone, Debug)]
pub(crate) struct OrderState {
    pub orders: Vec<usize>,
    pub jump_targets: Option<Vec<usize>>,
    pub restart: usize,
}

/// The song-wide fields.
#[derive(Clone, Debug)]
pub(crate) struct HeaderState {
    pub title: String,
    pub speed: u8,
    pub tempo: u8,
    pub global_volume: f32,
    pub flags: Flags,
    pub tracker_version: u16,
    pub mod_flavor: Option<ModFlavor>,
    pub adlib_gain: f32,
    pub stereo_separation: f32,
}

impl HeaderState {
    pub fn of(m: &Module) -> HeaderState {
        HeaderState {
            title: m.title.clone(),
            speed: m.speed,
            tempo: m.tempo,
            global_volume: m.global_volume,
            flags: m.flags,
            tracker_version: m.tracker_version,
            mod_flavor: m.mod_flavor,
            adlib_gain: m.adlib_gain,
            stereo_separation: m.stereo_separation,
        }
    }
}

/// The per-channel lists.
#[derive(Clone, Debug)]
pub(crate) struct ChannelState {
    pub channels: usize,
    pub channel_pan: Vec<Option<f32>>,
    pub muted: Vec<bool>,
    pub adlib_channels: Vec<Option<u8>>,
}

impl ChannelState {
    pub fn of(m: &Module) -> ChannelState {
        ChannelState {
            channels: m.channels,
            channel_pan: m.channel_pan.clone(),
            muted: m.muted.clone(),
            adlib_channels: m.adlib_channels.clone(),
        }
    }
}

/// A sample without its data (the fields an edit of its properties saves).
pub(crate) fn sample_meta(s: &Sample) -> Sample {
    Sample {
        name: s.name.clone(),
        data: Vec::new(),
        volume: s.volume,
        finetune: s.finetune,
        c2spd: s.c2spd,
        relative_note: s.relative_note,
        pan: s.pan,
        adlib: s.adlib,
        loop_kind: s.loop_kind,
        loop_start: s.loop_start,
        loop_len: s.loop_len,
    }
}

/// An instrument without its sample list.
pub(crate) fn instrument_meta(i: &Instrument) -> Instrument {
    Instrument {
        name: i.name.clone(),
        keymap: i.keymap,
        samples: Vec::new(),
        volume_envelope: i.volume_envelope.clone(),
        panning_envelope: i.panning_envelope.clone(),
        fadeout: i.fadeout,
        vibrato: i.vibrato,
    }
}

/// An XM's instruments number their samples from 0 across the module, in
/// order. Restores that after samples were added or removed.
pub(crate) fn renumber_xm_samples(m: &mut Module) {
    let mut next = 0;
    for inst in &mut m.instruments {
        for (k, s) in inst.samples.iter_mut().enumerate() {
            *s = next + k;
        }
        next += inst.samples.len();
    }
}

/// The index of an instrument's first sample in the module's sample list.
pub(crate) fn xm_sample_base(m: &Module, owner: usize) -> usize {
    m.instruments[..owner].iter().map(|i| i.samples.len()).sum()
}

/// A recorded change: applying it to a module puts back what it holds and
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
    /// A whole pattern, swapped in place.
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
    Orders(OrderState),
    Header(Box<HeaderState>),
    Channels(Box<ChannelState>),
    /// A sample's fields, not its data.
    SampleMeta {
        index: usize,
        meta: Sample,
    },
    SampleData {
        index: usize,
        data: Vec<f32>,
    },
    /// A sample slot (MOD, S3M).
    SampleInsert {
        index: usize,
        sample: Sample,
    },
    SampleRemove {
        index: usize,
    },
    /// A sample inside an XM instrument, at `slot` of its list.
    XmSampleInsert {
        owner: usize,
        slot: usize,
        sample: Sample,
    },
    XmSampleRemove {
        owner: usize,
        slot: usize,
    },
    /// An instrument's fields, not its sample list.
    InstrumentMeta {
        index: usize,
        meta: Instrument,
    },
    InstrumentInsert {
        index: usize,
        meta: Instrument,
    },
    InstrumentRemove {
        index: usize,
    },
}

fn swap_sample_meta(a: &mut Sample, b: &mut Sample) {
    std::mem::swap(&mut a.name, &mut b.name);
    std::mem::swap(&mut a.volume, &mut b.volume);
    std::mem::swap(&mut a.finetune, &mut b.finetune);
    std::mem::swap(&mut a.c2spd, &mut b.c2spd);
    std::mem::swap(&mut a.relative_note, &mut b.relative_note);
    std::mem::swap(&mut a.pan, &mut b.pan);
    std::mem::swap(&mut a.adlib, &mut b.adlib);
    std::mem::swap(&mut a.loop_kind, &mut b.loop_kind);
    std::mem::swap(&mut a.loop_start, &mut b.loop_start);
    std::mem::swap(&mut a.loop_len, &mut b.loop_len);
}

impl Change {
    /// Puts this change's state into `m`; returns the change that restores
    /// what was there.
    pub(crate) fn apply(self, m: &mut Module) -> Change {
        match self {
            Change::Block {
                pattern,
                row0,
                channel0,
                rows,
                channels,
                mut cells,
            } => {
                let p = &mut m.patterns[pattern];
                for r in 0..rows {
                    for c in 0..channels {
                        std::mem::swap(
                            p.cell_mut(row0 + r, channel0 + c),
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
            Change::Pattern { index, pattern } => {
                let old = std::mem::replace(&mut m.patterns[index], pattern);
                Change::Pattern {
                    index,
                    pattern: old,
                }
            }
            Change::PatternInsert { index, pattern } => {
                m.patterns.insert(index, pattern);
                Change::PatternRemove { index }
            }
            Change::PatternRemove { index } => {
                let pattern = m.patterns.remove(index);
                Change::PatternInsert { index, pattern }
            }
            Change::Orders(mut s) => {
                std::mem::swap(&mut m.orders, &mut s.orders);
                std::mem::swap(&mut m.jump_targets, &mut s.jump_targets);
                std::mem::swap(&mut m.restart, &mut s.restart);
                Change::Orders(s)
            }
            Change::Header(mut h) => {
                std::mem::swap(&mut m.title, &mut h.title);
                std::mem::swap(&mut m.speed, &mut h.speed);
                std::mem::swap(&mut m.tempo, &mut h.tempo);
                std::mem::swap(&mut m.global_volume, &mut h.global_volume);
                std::mem::swap(&mut m.flags, &mut h.flags);
                std::mem::swap(&mut m.tracker_version, &mut h.tracker_version);
                std::mem::swap(&mut m.mod_flavor, &mut h.mod_flavor);
                std::mem::swap(&mut m.adlib_gain, &mut h.adlib_gain);
                std::mem::swap(&mut m.stereo_separation, &mut h.stereo_separation);
                Change::Header(h)
            }
            Change::Channels(mut c) => {
                std::mem::swap(&mut m.channels, &mut c.channels);
                std::mem::swap(&mut m.channel_pan, &mut c.channel_pan);
                std::mem::swap(&mut m.muted, &mut c.muted);
                std::mem::swap(&mut m.adlib_channels, &mut c.adlib_channels);
                Change::Channels(c)
            }
            Change::SampleMeta { index, mut meta } => {
                swap_sample_meta(&mut m.samples[index], &mut meta);
                Change::SampleMeta { index, meta }
            }
            Change::SampleData { index, mut data } => {
                std::mem::swap(&mut m.samples[index].data, &mut data);
                Change::SampleData { index, data }
            }
            Change::SampleInsert { index, sample } => {
                m.samples.insert(index, sample);
                Change::SampleRemove { index }
            }
            Change::SampleRemove { index } => {
                let sample = m.samples.remove(index);
                Change::SampleInsert { index, sample }
            }
            Change::XmSampleInsert {
                owner,
                slot,
                sample,
            } => {
                let at = xm_sample_base(m, owner) + slot;
                m.samples.insert(at, sample);
                m.instruments[owner].samples.insert(slot, 0);
                renumber_xm_samples(m);
                Change::XmSampleRemove { owner, slot }
            }
            Change::XmSampleRemove { owner, slot } => {
                let at = xm_sample_base(m, owner) + slot;
                let sample = m.samples.remove(at);
                m.instruments[owner].samples.remove(slot);
                renumber_xm_samples(m);
                Change::XmSampleInsert {
                    owner,
                    slot,
                    sample,
                }
            }
            Change::InstrumentMeta { index, mut meta } => {
                let i = &mut m.instruments[index];
                std::mem::swap(&mut i.name, &mut meta.name);
                std::mem::swap(&mut i.keymap, &mut meta.keymap);
                std::mem::swap(&mut i.volume_envelope, &mut meta.volume_envelope);
                std::mem::swap(&mut i.panning_envelope, &mut meta.panning_envelope);
                std::mem::swap(&mut i.fadeout, &mut meta.fadeout);
                std::mem::swap(&mut i.vibrato, &mut meta.vibrato);
                Change::InstrumentMeta { index, meta }
            }
            Change::InstrumentInsert { index, meta } => {
                m.instruments.insert(index, meta);
                Change::InstrumentRemove { index }
            }
            Change::InstrumentRemove { index } => {
                let meta = m.instruments.remove(index);
                Change::InstrumentInsert { index, meta }
            }
        }
    }

    /// Roughly what this change costs to keep, in bytes.
    pub(crate) fn bytes(&self) -> usize {
        const BASE: usize = 64;
        match self {
            Change::Block { cells, .. } => BASE + cells.len() * size_of::<Cell>(),
            Change::Pattern { pattern, .. } | Change::PatternInsert { pattern, .. } => {
                BASE + pattern.rows() * pattern.channels() * size_of::<Cell>()
            }
            Change::Orders(s) => {
                BASE + (s.orders.len() + s.jump_targets.as_ref().map_or(0, Vec::len))
                    * size_of::<usize>()
            }
            Change::Header(h) => BASE + h.title.len(),
            Change::Channels(c) => BASE + c.channels * 16,
            Change::SampleMeta { meta, .. } => BASE + meta.name.len(),
            Change::SampleData { data, .. } => BASE + data.len() * size_of::<f32>(),
            Change::SampleInsert { sample, .. } | Change::XmSampleInsert { sample, .. } => {
                BASE + sample.name.len() + sample.data.len() * size_of::<f32>()
            }
            Change::InstrumentMeta { meta, .. } | Change::InstrumentInsert { meta, .. } => {
                BASE + 128 + meta.name.len()
            }
            Change::PatternRemove { .. }
            | Change::SampleRemove { .. }
            | Change::XmSampleRemove { .. }
            | Change::InstrumentRemove { .. } => BASE,
        }
    }

    /// Adds what this change touches to `out` (read before it is applied or
    /// after: the same places).
    pub(crate) fn touch(&self, m: &Module, out: &mut Changes) {
        match self {
            Change::Block {
                pattern,
                row0,
                rows,
                ..
            } => out.pattern_rows(*pattern, *row0, row0 + rows.saturating_sub(1)),
            Change::Pattern { index, pattern } => {
                out.pattern_rows(*index, 0, pattern.rows().saturating_sub(1));
                out.layout = true;
            }
            Change::PatternInsert { .. } | Change::PatternRemove { .. } => out.layout = true,
            Change::Orders(_) => out.orders = true,
            Change::Header(_) => out.header = true,
            Change::Channels(_) => {
                out.channels = true;
                out.layout = true;
            }
            Change::SampleMeta { index, .. } | Change::SampleData { index, .. } => {
                if m.format == crate::module::Format::Xm {
                    if let Some(owner) = xm_owner(m, *index) {
                        out.instrument(owner + 1);
                    }
                } else {
                    out.instrument(index + 1);
                }
            }
            Change::SampleInsert { index, .. } | Change::SampleRemove { index } => {
                out.instrument(index + 1);
                out.instrument_list = true;
            }
            Change::XmSampleInsert { owner, .. } | Change::XmSampleRemove { owner, .. } => {
                out.instrument(owner + 1);
                out.instrument_list = true;
            }
            Change::InstrumentMeta { index, .. } => out.instrument(index + 1),
            Change::InstrumentInsert { index, .. } | Change::InstrumentRemove { index } => {
                out.instrument(index + 1);
                out.instrument_list = true;
            }
        }
    }
}

/// The instrument that owns global sample `index` of an XM.
pub(crate) fn xm_owner(m: &Module, index: usize) -> Option<usize> {
    m.instruments
        .iter()
        .position(|i| i.samples.contains(&index))
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
