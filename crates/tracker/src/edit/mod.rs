//! Editing a tracker module as data.
//!
//! A [`ModuleDoc`] holds a [`Module`] (MOD, S3M or XM) and changes it the way
//! the original trackers did, checking every change against the format's own
//! limits: a MOD has 4 channels (or 1-32 by signature), 31 samples, notes
//! C-1 to B-3 as Amiga periods and effects 0-F; an S3M up to 32 channels, 99
//! samples, a volume column of 0-64 and effects A-Z; an XM up to 32 channels,
//! 128 instruments with up to 16 samples each, a volume column with commands
//! and effects 0-F, G, H, K, L, P, R, T, X. A change that does not fit is
//! refused with a message and leaves the module as it was.
//!
//! Everything is available as typed methods and as one JSON entry point,
//! [`ModuleDoc::apply`], for hosts that talk JSON (the Lua editor does):
//! `{"op": "set_cell", "pattern": 0, "row": 3, "channel": 1, "note": 60}`
//! returns `{"changed": {...}}` (what to redraw) and any result fields.
//!
//! # Numbers
//!
//! Patterns, rows, channels, orders and samples within an XM instrument count
//! from 0; instruments and samples as a cell names them count from 1 (0 is
//! none). Ranges (`row0`..`row1`, `channel0`..`channel1`) include both ends.
//!
//! A cell is the array `[note, instrument, volume, effect, param]`:
//!
//! - `note`: 0 none, 255 key off, otherwise a MIDI note number (60 is middle C)
//!   in every format. A MOD has C-1 (24) to B-3 (59), or seven octaves down to
//!   1 when it is not limited to ProTracker's; an S3M C-0 (24) to B-7 (119); an
//!   XM C-0 (12) to B-7 (107). A tracker names octaves differently:
//!   `info().note.octave_offset` is what to take from `note / 12` (MOD and
//!   XM 1, so 60 is `C-4`; S3M 2, so 72 is `C-4`, as ST3 shows it).
//! - `instrument`: the sample (MOD, S3M) or instrument (XM) number.
//! - `volume`: the volume column byte as the file holds it, 0 for none: 0x10-0x50
//!   is a volume of 0-64; an XM's 0x60-0xFF are its volume-column commands. A
//!   MOD has no volume column.
//! - `effect`: the command number as the file holds it. A MOD's and XM's are
//!   0-9 then 10 (A) and up (an XM's G is 16, H 17, K 20, L 21, P 25, R 27, T 29,
//!   X 33); an S3M's 1-26 are A-Z. The operations also take the letter as text.
//! - `param`: 0-255.
//!
//! # Undo
//!
//! Every edit is undoable: [`ModuleDoc::undo`] and [`ModuleDoc::redo`] step
//! through the log and return what changed. Edits made between
//! [`ModuleDoc::begin_group`] and [`ModuleDoc::end_group`] are one step. The
//! log records only what each edit overwrites (see `history.rs`), so it is
//! small, and it is bounded in steps and bytes.

mod cells;
mod history;
mod instruments;
mod json;
mod limits;
mod song;
mod wav;

use serde_json::{json, Value};

pub use cells::{CellUpdate, ClearMask, Clip, PasteMode, Pasted, Region};
pub use history::{Changes, MAX_CELLS};
pub use instruments::{
    parse_loop_kind, EnvelopeEdit, EnvelopeKind, ImportOptions, InstrumentEdit, SampleEdit,
    SampleRef, SampleSource, Wave,
};
pub use json::cell_json;
pub use limits::{
    check_cell, clean_name, effect_char, limits, note_name, note_range, octave_offset,
    parse_effect, parse_note, Limits,
};
pub use song::Flag;
pub use wav::{decode_wav, encode_wav, resample, ChannelMix, Pcm, Quality};

use self::history::{Change, History};
use self::json::{
    base64_decode, field, note_number, opt_bool, opt_i64, opt_str, opt_u8, opt_usize, req_i64,
    req_str, req_usize, Res,
};
use self::song::s3m_default_pan;
use crate::module::{Flags, Format, ModFlavor, Module, Pattern, Sample};
use crate::{load, write};

/// A module being edited. See the module docs.
#[derive(Debug)]
pub struct ModuleDoc {
    module: Module,
    history: History,
    revision: u64,
    /// What the last edit, undo or redo changed.
    last: Changes,
}

/// One pattern cell's content as a live song takes it: field for field the
/// numbers of `cubed_mixer::SongCell` (the mixer's `SongControl::SetCell`) and
/// of [`Cell`](crate::Cell). See [`ModuleDoc::live_edits`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellEdit {
    /// The pattern's number (not its position in the order list).
    pub pattern: u16,
    pub row: u16,
    pub channel: u8,
    /// 0 none, 255 key off, otherwise a MIDI note number.
    pub note: u8,
    /// MOD: the Amiga period of the note; 0 elsewhere.
    pub period: u16,
    pub instrument: u8,
    pub volume: u8,
    pub effect: u8,
    pub param: u8,
}

impl CellEdit {
    pub fn of(pattern: usize, row: usize, channel: usize, c: &crate::module::Cell) -> CellEdit {
        CellEdit {
            pattern: pattern as u16,
            row: row as u16,
            channel: channel as u8,
            note: note_number(c.note) as u8,
            period: c.period,
            instrument: c.instrument,
            volume: c.volume,
            effect: c.effect,
            param: c.param,
        }
    }

    /// The cell (what `Player::set_cell` takes).
    pub fn cell(&self) -> crate::module::Cell {
        crate::module::Cell {
            note: match self.note {
                0 => crate::module::Note::None,
                255 => crate::module::Note::Off,
                n => crate::module::Note::On(n),
            },
            period: self.period,
            instrument: self.instrument,
            volume: self.volume,
            effect: self.effect,
            param: self.param,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "pattern": self.pattern, "row": self.row, "channel": self.channel,
            "note": self.note, "period": self.period, "instrument": self.instrument,
            "volume": self.volume, "effect": self.effect, "param": self.param,
        })
    }
}

impl ModuleDoc {
    // ----- making and reading ------------------------------------------------

    /// A new, empty module of a format: one empty 64-row pattern played once.
    /// A MOD has 4 channels and 31 empty samples; an S3M 8 channels and no
    /// samples; an XM 8 channels and no instruments.
    pub fn new(format: Format) -> ModuleDoc {
        let module = match format {
            Format::Mod => Module {
                format,
                title: String::new(),
                channels: 4,
                orders: vec![0],
                restart: 0,
                patterns: vec![Pattern::new(64, 4)],
                samples: (0..31)
                    .map(|_| Sample {
                        volume: 64,
                        ..Sample::default()
                    })
                    .collect(),
                instruments: Vec::new(),
                speed: 6,
                tempo: 125,
                channel_pan: (0..4)
                    .map(|c| Some(crate::protracker::channel_pan(c, 4)))
                    .collect(),
                adlib_channels: vec![None; 4],
                muted: vec![false; 4],
                jump_targets: None,
                global_volume: 1.0,
                adlib_gain: 0.0,
                stereo_separation: 0.75,
                flags: Flags {
                    amiga_limits: true,
                    ..Flags::default()
                },
                mod_flavor: Some(ModFlavor::ProTracker),
                tracker_version: 0,
            },
            Format::S3m => Module {
                format,
                title: String::new(),
                channels: 8,
                orders: vec![0],
                restart: 0,
                patterns: vec![Pattern::new(64, 8)],
                samples: Vec::new(),
                instruments: Vec::new(),
                speed: 6,
                tempo: 125,
                channel_pan: (0..8).map(|c| Some(s3m_default_pan(c))).collect(),
                adlib_channels: vec![None; 8],
                muted: vec![false; 8],
                jump_targets: Some(vec![0]),
                global_volume: 1.0,
                adlib_gain: crate::s3m::adlib_gain(0xb0, 2),
                stereo_separation: 1.0,
                flags: Flags::default(),
                mod_flavor: None,
                tracker_version: 0x1320,
            },
            Format::Xm => Module {
                format,
                title: String::new(),
                channels: 8,
                orders: vec![0],
                restart: 0,
                patterns: vec![Pattern::new(64, 8)],
                samples: Vec::new(),
                instruments: Vec::new(),
                speed: 6,
                tempo: 125,
                channel_pan: vec![None; 8],
                adlib_channels: vec![None; 8],
                muted: vec![false; 8],
                jump_targets: None,
                global_volume: 1.0,
                adlib_gain: 0.0,
                stereo_separation: 1.0,
                flags: Flags {
                    linear_frequency: true,
                    ..Flags::default()
                },
                mod_flavor: None,
                tracker_version: 0x0104,
            },
        };
        ModuleDoc::from_module(module)
    }

    /// Edits a module the caller built or parsed.
    pub fn from_module(module: Module) -> ModuleDoc {
        ModuleDoc {
            module,
            history: History::new(),
            revision: 0,
            last: Changes::default(),
        }
    }

    /// Reads a MOD, S3M or XM file.
    pub fn from_bytes(bytes: &[u8]) -> Res<ModuleDoc> {
        Ok(ModuleDoc::from_module(load(bytes)?))
    }

    /// The module as a file of `format`. Only the module's own format can be
    /// written (the document converts nothing between formats); the writers'
    /// errors say what the format cannot hold.
    pub fn to_bytes(&self, format: Format) -> Res<Vec<u8>> {
        if format != self.module.format {
            return Err(format!(
                "this module is {}: converting it to {} is not supported",
                limits::format_name(self.module.format),
                limits::format_name(format)
            ));
        }
        write(&self.module)
    }

    /// The module, for playing or inspecting.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// The module, taken out.
    pub fn into_module(self) -> Module {
        self.module
    }

    pub fn format(&self) -> Format {
        self.module.format
    }

    /// What the last edit, undo or redo changed (also what its result's
    /// `changed` says).
    pub fn last_changes(&self) -> &Changes {
        &self.last
    }

    /// The last change as live cell edits, for a song that is playing the
    /// module: `Some` with the current content of each cell that changed when
    /// the change was nothing but cell content (a note typed, a range cleared
    /// or pasted, rows inserted, a MOD's speed `Fxx`), to send to the
    /// player (`Player::set_cell`, the mixer's `SongControl::SetCell`) one by
    /// one; `None` when the change moved the module's shape (patterns, order
    /// list, header, channels, samples, instruments) or touched more than
    /// [`MAX_CELLS`] cells, which is a new song: open the edited module and
    /// swap it in.
    pub fn live_edits(&self) -> Option<Vec<CellEdit>> {
        self.cell_edits(&self.last)
    }

    /// [`live_edits`](Self::live_edits) for a [`Changes`] this module's edits
    /// returned.
    pub fn cell_edits(&self, changes: &Changes) -> Option<Vec<CellEdit>> {
        if !changes.is_cells_only() {
            return None;
        }
        let mut out = Vec::with_capacity(changes.cells.len());
        for &(p, r, c) in &changes.cells {
            let pattern = self.module.patterns.get(p)?;
            if r >= pattern.rows() || c >= pattern.channels() {
                return None;
            }
            out.push(CellEdit::of(p, r, c, pattern.cell(r, c)));
        }
        Some(out)
    }

    /// Counts every change made (and every undo and redo): a UI can cache by it.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// What is wrong with the module as a file of its format: cells the format
    /// cannot hold (the first 20) and what the writer refuses. An empty list is
    /// a module that saves.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        'outer: for (p, pattern) in self.module.patterns.iter().enumerate() {
            for row in 0..pattern.rows() {
                for (ch, cell) in pattern.row(row).iter().enumerate() {
                    if let Err(e) = check_cell(&self.module, cell) {
                        problems.push(format!("pattern {p}, row {row}, channel {}: {e}", ch + 1));
                        if problems.len() >= 20 {
                            break 'outer;
                        }
                    }
                }
            }
        }
        if let Err(e) = write(&self.module) {
            problems.push(format!("cannot be saved: {e}"));
        }
        problems
    }

    /// The module's summary: `{"format", "title", "channels", "patterns",
    /// "orders", "restart", "speed", "tempo", "global_volume", "flags",
    /// "note": {"min", "max", "octave_offset"}, "limits", "instruments" (the
    /// count a cell can name), "channel_pan", "muted", "adlib_channels",
    /// "revision", "can_undo", "can_redo", ...}`.
    pub fn info(&self) -> Value {
        let m = &self.module;
        let lim = limits(m);
        let (lo, hi) = note_range(m);
        let (speed, tempo) = if m.format == Format::Mod {
            self.mod_start_timing()
        } else {
            (m.speed, m.tempo)
        };
        json!({
            "format": match m.format { Format::Mod => "mod", Format::S3m => "s3m", Format::Xm => "xm" },
            "title": m.title,
            "channels": m.channels,
            "patterns": m.patterns.len(),
            "orders": m.orders.len(),
            "restart": m.restart,
            "speed": speed,
            "tempo": tempo,
            "global_volume": (m.global_volume * 64.0).round(),
            "global_volume_scale": 64,
            "flags": {
                "linear_frequency": m.flags.linear_frequency,
                "amiga_limits": m.flags.amiga_limits,
                "fast_volume_slides": m.flags.fast_volume_slides,
                "vblank_timing": m.flags.vblank_timing,
            },
            "note": {"min": lo, "max": hi, "octave_offset": octave_offset(m.format)},
            "limits": {
                "channels": [lim.min_channels, lim.max_channels],
                "patterns": lim.max_patterns,
                "orders": lim.max_orders,
                "slots": lim.max_slots,
                "rows": [lim.min_rows, lim.max_rows],
                "variable_rows": lim.variable_rows,
                "samples_per_instrument": lim.max_samples_per_instrument,
                "title": lim.max_title,
                "name": lim.max_name,
                "speed": [lim.speed.0, lim.speed.1],
                "tempo": [lim.tempo.0, lim.tempo.1],
            },
            "instruments": if m.format == Format::Xm { m.instruments.len() } else { m.samples.len() },
            "channel_pan": m.channel_pan,
            "muted": m.muted,
            "adlib_channels": m.adlib_channels,
            "mod_flavor": m.mod_flavor.map(|f| match f {
                ModFlavor::ProTracker => "protracker",
                ModFlavor::NoiseTracker => "noisetracker",
                ModFlavor::Soundtracker => "soundtracker",
                ModFlavor::UltimateSoundtracker => "ultimate_soundtracker",
            }),
            "revision": self.revision,
            "can_undo": self.can_undo(),
            "can_redo": self.can_redo(),
            "undo_steps": self.history.undo_steps(),
            "redo_steps": self.history.redo_steps(),
            "history_bytes": self.history.bytes(),
        })
    }

    /// The cells of `count` rows from `row0` of a pattern, for drawing:
    /// `{"pattern", "row0", "count", "rows" (the pattern's length), "channels",
    /// "cells": [[[note, instrument, volume, effect, param] per channel] per
    /// row]}`. Rows past the end are left out. With `flat`, `cells` is one
    /// array of `count * channels * 5` numbers, row by row.
    pub fn rows(&self, pattern: usize, row0: usize, count: usize, flat: bool) -> Res<Value> {
        let p = self.module.patterns.get(pattern).ok_or_else(|| {
            format!(
                "pattern {pattern} does not exist (the module has {})",
                self.module.patterns.len()
            )
        })?;
        let end = row0.saturating_add(count).min(p.rows());
        let row0 = row0.min(end);
        let cells = if flat {
            let mut v = Vec::with_capacity((end - row0) * p.channels() * 5);
            for r in row0..end {
                for c in p.row(r) {
                    v.extend([
                        note_number(c.note),
                        c.instrument as u32,
                        c.volume as u32,
                        c.effect as u32,
                        c.param as u32,
                    ]);
                }
            }
            json!(v)
        } else {
            Value::Array(
                (row0..end)
                    .map(|r| Value::Array(p.row(r).iter().map(cell_json).collect()))
                    .collect(),
            )
        };
        Ok(json!({
            "pattern": pattern,
            "row0": row0,
            "count": end - row0,
            "rows": p.rows(),
            "channels": p.channels(),
            "cells": cells,
        }))
    }

    /// The song: `{"orders": [pattern numbers], "restart", "rows": [the length
    /// of each order's pattern], "pattern_rows": [the length of every pattern]}`.
    pub fn order(&self) -> Value {
        let m = &self.module;
        json!({
            "orders": m.orders,
            "restart": m.restart,
            "rows": m.orders.iter().map(|&o| m.patterns[o].rows()).collect::<Vec<_>>(),
            "pattern_rows": m.patterns.iter().map(|p| p.rows()).collect::<Vec<_>>(),
        })
    }

    // ----- history ------------------------------------------------------------

    /// Starts a group: the edits until the matching [`end_group`](Self::end_group)
    /// are undone as one step. Groups nest.
    pub fn begin_group(&mut self) {
        self.history.depth += 1;
    }

    /// Ends a group (the outermost one closes the undo step).
    pub fn end_group(&mut self) {
        if self.history.depth > 0 {
            self.history.depth -= 1;
            if self.history.depth == 0 {
                self.history.commit();
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Bounds the log: at most `steps` undo steps and `bytes` of recorded
    /// changes (the oldest go first; the newest step always stays).
    pub fn set_history_limits(&mut self, steps: usize, bytes: usize) {
        self.history.max_steps = steps.max(1);
        self.history.max_bytes = bytes;
    }

    /// Forgets all undo and redo steps (after a save, say).
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Undoes the last step; what changed, or `None` if there is nothing to
    /// undo (or a group is open).
    pub fn undo(&mut self) -> Option<Changes> {
        if !self.history.can_undo() {
            return None;
        }
        let step = self.history.pop_undo()?;
        let mut produced = Vec::with_capacity(step.changes.len());
        let mut changes = Changes::default();
        for c in step.changes.into_iter().rev() {
            let inverse = c.apply(&mut self.module);
            inverse.touch(&self.module, &mut changes);
            produced.push(inverse);
        }
        self.history.push_redo(produced);
        self.revision += 1;
        self.last = changes.clone();
        Some(changes)
    }

    /// Redoes the step the last undo took back.
    pub fn redo(&mut self) -> Option<Changes> {
        if !self.history.can_redo() {
            return None;
        }
        let step = self.history.pop_redo()?;
        let mut produced = Vec::with_capacity(step.changes.len());
        let mut changes = Changes::default();
        for c in step.changes.into_iter().rev() {
            let inverse = c.apply(&mut self.module);
            inverse.touch(&self.module, &mut changes);
            produced.push(inverse);
        }
        self.history.push_undo_keep_redo(produced);
        self.revision += 1;
        self.last = changes.clone();
        Some(changes)
    }

    // ----- the edit machinery --------------------------------------------------

    /// Applies a change and records what undoes it.
    pub(crate) fn change(&mut self, c: Change) {
        let inverse = c.apply(&mut self.module);
        self.history.open.push(inverse);
    }

    /// Runs an edit as part of the current group (or a group of its own). If it
    /// fails, whatever it already changed is put back. Returns what it touched.
    pub(crate) fn edit<T>(&mut self, f: impl FnOnce(&mut Self) -> Res<T>) -> Res<(T, Changes)> {
        self.history.depth += 1;
        let mark = self.history.open.len();
        let result = f(self);
        let out = match result {
            Ok(v) => {
                let mut changes = Changes::default();
                for c in &self.history.open[mark..] {
                    c.touch(&self.module, &mut changes);
                }
                if self.history.open.len() > mark {
                    self.revision += 1;
                }
                self.last = changes.clone();
                Ok((v, changes))
            }
            Err(e) => {
                while self.history.open.len() > mark {
                    if let Some(c) = self.history.open.pop() {
                        c.apply(&mut self.module);
                    }
                }
                Err(e)
            }
        };
        self.history.depth -= 1;
        if self.history.depth == 0 {
            self.history.commit();
        }
        out
    }

    // ----- JSON ---------------------------------------------------------------------

    /// Runs one operation described as JSON and returns its result as JSON.
    /// Edits return `{"changed": {...}}` (see [`Changes::to_json`]) with their
    /// own fields; a failed edit returns the message and changes nothing.
    ///
    /// Reading:
    /// - `{"op": "info"}`, `{"op": "order"}`, `{"op": "samples"}`,
    ///   `{"op": "instruments"}`, `{"op": "validate"}` -> `{"problems": [..]}`
    /// - `{"op": "live_edits"}` -> `{"cells": [{"pattern", "row", "channel",
    ///   "note", "period", "instrument", "volume", "effect", "param"}]}` for the
    ///   last edit if it only changed cells (what a playing song can take as
    ///   cell edits), else `{"cells": null}` (open the edited module as a new
    ///   song); see [`live_edits`](Self::live_edits)
    /// - `{"op": "rows", "pattern", "row0", "count", "flat"}`
    /// - `{"op": "copy_range", "pattern", "row0", "row1", "channel0", "channel1"}`
    ///   -> the clip (`rows`, `channels`, `cells`)
    /// - `{"op": "peaks", "instrument", "sample", "buckets", "from", "to"}`
    ///
    /// Cells and patterns:
    /// - `set_cell` (`pattern`, `row`, `channel`, and `cell: [n,i,v,e,p]` or any
    ///   of `note`, `instrument`, `volume`, `effect`, `param`; a note may be
    ///   text like `"C-4"`, an effect a letter) -> `{"cell"}`
    /// - `clear_range` (`pattern`, `row0`, `row1`, `channel0`, `channel1`,
    ///   optional `columns: ["note", ...]`)
    /// - `insert_row` / `delete_row` (`pattern`, `row`, `count`, optional
    ///   `channel0`/`channel1` to shift only those channels)
    /// - `transpose` (a range, `semitones`, `clamp`, `instrument`) -> `{"count"}`
    /// - `paste` (`pattern`, `row`, `channel`, `cells` or a `clip`, `mode`:
    ///   `"overwrite"` or `"mix"`) -> `{"rows", "channels", "clipped"}`
    /// - `set_pattern_rows` (`pattern`, `rows`, `force`), `new_pattern` (`rows`)
    ///   -> `{"pattern"}`, `duplicate_pattern`, `delete_pattern`
    ///
    /// Song:
    /// - `set_order` (`index`, `pattern`), `insert_order`, `delete_order`
    ///   (`index`), `move_order` (`from`, `to`), `set_restart` (`order`)
    /// - `set_speed` (`speed`), `set_tempo` (`tempo`), `set_title` (`title`),
    ///   `set_global_volume` (`volume` 0-64, S3M), `set_flag` (`flag`, `value`)
    /// - `set_channels` (`count`, `force`), `set_channel` (S3M: `channel`,
    ///   `pan` 0-15, `muted`), `set_mod_flavor` (`flavor`: `"protracker"` or
    ///   `"noisetracker"`, MOD: the 15-sample layouts convert to 31 samples)
    ///
    /// Samples and instruments (`instrument` and `sample` as [`SampleRef`]):
    /// - `set_sample` (`name`, `volume`, `finetune`, `relative_note`, `pan`,
    ///   `c2spd`, `loop_kind`, `loop_start`, `loop_len`, `adlib`)
    /// - `import_sample` (`data`: base64 of the file or the raw bytes, `kind`:
    ///   `"wav"`, `"pcm8"`, `"pcm8u"` or `"pcm16"`, `rate` and `channels` for
    ///   raw, `mix`, `resample`: `"sinc"`/`"linear"`, `note` (MOD), `target_rate`,
    ///   `bits`, `normalize`, `name`, `keep_loop`, `truncate`)
    /// - `normalize_sample`, `reverse_sample`, `clear_sample`,
    ///   `generate_wave` (`wave`, `length`, `seed`), `halve_sample`,
    ///   `generate_pulse` (`cycle_length`, `cycles`, `duty_start`, `duty_end`,
    ///   `sweep`: `up` or `pingpong`)
    /// - `add_instrument` (`name`), `remove_instrument` (`instrument`, `force`),
    ///   XM: `add_sample`, `remove_sample`, `set_instrument` (`name`, `fadeout`,
    ///   `vibrato_kind`, `vibrato_sweep`, `vibrato_depth`, `vibrato_rate`),
    ///   `set_envelope` (`kind`: `"volume"`/`"panning"`, `enabled`, `points`,
    ///   `sustain`, `loop`), `set_keymap` (`from`, `to`, `sample`, or `keymap`)
    ///
    /// History: `begin_group`, `end_group`, `undo`, `redo` (each returns
    /// `{"changed"}` of what moved), and `{"op": "batch", "ops": [...]}`, which
    /// runs the operations as one undo step and, if one fails, none of them.
    pub fn apply(&mut self, op: &Value) -> Res<Value> {
        let name = req_str(op, "op")?;
        let format = self.module.format;
        let done = |changes: Changes| Ok(json!({"changed": changes.to_json()}));
        match name {
            // ---- reading
            "info" => Ok(self.info()),
            "order" => Ok(self.order()),
            "samples" => Ok(self.samples()),
            "instruments" => Ok(self.instruments()),
            "validate" => Ok(json!({"problems": self.validate()})),
            "live_edits" => Ok(json!({
                "cells": self.live_edits().map(|v| v.iter().map(CellEdit::to_json).collect::<Vec<_>>()),
            })),
            "rows" => self.rows(
                req_usize(op, "pattern")?,
                opt_usize(op, "row0")?.unwrap_or(0),
                opt_usize(op, "count")?.unwrap_or(usize::MAX),
                opt_bool(op, "flat")?.unwrap_or(false),
            ),
            "copy_range" => {
                let region = self.region_from_op(op)?;
                Ok(self.copy_range(region)?.to_json(format))
            }
            "peaks" => {
                let range = match (opt_usize(op, "from")?, opt_usize(op, "to")?) {
                    (None, None) => None,
                    (a, b) => Some((a.unwrap_or(0), b.unwrap_or(usize::MAX))),
                };
                self.sample_peaks(
                    sample_ref(op)?,
                    opt_usize(op, "buckets")?.unwrap_or(256),
                    range,
                )
            }
            // ---- history
            "begin_group" => {
                self.begin_group();
                Ok(json!({}))
            }
            "end_group" => {
                self.end_group();
                Ok(json!({}))
            }
            "undo" => match self.undo() {
                Some(c) => done(c),
                None => Err("nothing to undo".into()),
            },
            "redo" => match self.redo() {
                Some(c) => done(c),
                None => Err("nothing to redo".into()),
            },
            "batch" => {
                let ops = op
                    .get("ops")
                    .and_then(Value::as_array)
                    .ok_or("batch needs 'ops': a list of operations")?;
                let ((results, merged), _) = self.edit(|d| {
                    let mut results = Vec::with_capacity(ops.len());
                    let mut merged = Changes::default();
                    for (i, o) in ops.iter().enumerate() {
                        let r = d.apply(o).map_err(|e| {
                            format!(
                                "operation {i} ({}) failed: {e}",
                                o.get("op").and_then(Value::as_str).unwrap_or("?")
                            )
                        })?;
                        if let Some(c) = r.get("changed") {
                            merged.merge(&changes_from_json(c));
                        }
                        results.push(r);
                    }
                    Ok((results, merged))
                })?;
                Ok(json!({"results": results, "changed": merged.to_json()}))
            }
            // ---- cells
            "set_cell" => {
                let update = CellUpdate::from_op(format, op)?;
                let (cell, changes) = self.set_cell(
                    req_usize(op, "pattern")?,
                    req_usize(op, "row")?,
                    req_usize(op, "channel")?,
                    update,
                )?;
                Ok(json!({"cell": cell_json(&cell), "changed": changes.to_json()}))
            }
            "clear_range" => {
                let region = self.region_from_op(op)?;
                let mask = match op.get("columns").and_then(Value::as_array) {
                    Some(list) => {
                        let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
                        ClearMask::from_names(&names)?
                    }
                    None => ClearMask::ALL,
                };
                done(self.clear_range(region, mask)?)
            }
            "insert_row" | "delete_row" => {
                let pattern = req_usize(op, "pattern")?;
                let row = req_usize(op, "row")?;
                let count = opt_usize(op, "count")?.unwrap_or(1);
                let channels = match (opt_usize(op, "channel0")?, opt_usize(op, "channel1")?) {
                    (None, None) => opt_usize(op, "channel")?.map(|c| (c, c)),
                    (a, b) => Some((a.unwrap_or(0), b.unwrap_or(usize::MAX))),
                };
                let channels = match channels {
                    Some((a, b)) => {
                        let last = self.module.channels.saturating_sub(1);
                        Some((a.min(last), b.min(last)))
                    }
                    None => None,
                };
                let c = if name == "insert_row" {
                    self.insert_row(pattern, row, channels, count)?
                } else {
                    self.delete_row(pattern, row, channels, count)?
                };
                done(c)
            }
            "transpose" => {
                let region = self.region_from_op(op)?;
                let semitones = req_i64(op, "semitones")?;
                let (count, changes) = self.transpose(
                    region,
                    semitones.clamp(-255, 255) as i32,
                    opt_bool(op, "clamp")?.unwrap_or(false),
                    opt_u8(op, "instrument")?,
                )?;
                Ok(json!({"count": count, "changed": changes.to_json()}))
            }
            "paste" => {
                let clip = Clip::from_json(
                    format,
                    op.get("clip")
                        .or_else(|| op.get("cells"))
                        .ok_or("paste needs 'cells' (as copy_range returns them)")?,
                )?;
                let mode = match opt_str(op, "mode")? {
                    None | Some("overwrite") => PasteMode::Overwrite,
                    Some("mix") => PasteMode::Mix,
                    Some(other) => {
                        return Err(format!("'{other}' is not a paste mode (overwrite, mix)"))
                    }
                };
                let (p, changes) = self.paste(
                    req_usize(op, "pattern")?,
                    req_usize(op, "row")?,
                    opt_usize(op, "channel")?.unwrap_or(0),
                    &clip,
                    mode,
                )?;
                Ok(
                    json!({"rows": p.rows, "channels": p.channels, "clipped": p.clipped, "changed": changes.to_json()}),
                )
            }
            "set_pattern_rows" => done(self.set_pattern_rows(
                req_usize(op, "pattern")?,
                req_usize(op, "rows")?,
                opt_bool(op, "force")?.unwrap_or(false),
            )?),
            "new_pattern" => {
                let (p, c) = self.new_pattern(opt_usize(op, "rows")?)?;
                Ok(json!({"pattern": p, "changed": c.to_json()}))
            }
            "duplicate_pattern" => {
                let (p, c) = self.duplicate_pattern(req_usize(op, "pattern")?)?;
                Ok(json!({"pattern": p, "changed": c.to_json()}))
            }
            "delete_pattern" => done(self.delete_pattern(req_usize(op, "pattern")?)?),
            // ---- song
            "set_order" => {
                done(self.set_order(req_usize(op, "index")?, req_usize(op, "pattern")?)?)
            }
            "insert_order" => {
                done(self.insert_order(req_usize(op, "index")?, req_usize(op, "pattern")?)?)
            }
            "delete_order" => done(self.delete_order(req_usize(op, "index")?)?),
            "move_order" => done(self.move_order(req_usize(op, "from")?, req_usize(op, "to")?)?),
            "set_restart" => done(self.set_restart(req_usize(op, "order")?)?),
            "set_speed" => done(self.set_speed(byte(op, "speed")?)?),
            "set_tempo" => done(self.set_tempo(byte(op, "tempo")?)?),
            "set_title" => {
                let (title, c) = self.set_title(req_str(op, "title")?)?;
                Ok(json!({"title": title, "changed": c.to_json()}))
            }
            "set_global_volume" => done(self.set_global_volume(byte(op, "volume")?)?),
            "set_flag" => {
                let flag = req_str(op, "flag")?;
                let flag = Flag::parse(flag).ok_or_else(|| {
                    format!("'{flag}' is not a flag (linear_frequency, amiga_limits, fast_volume_slides, vblank_timing)")
                })?;
                let value =
                    opt_bool(op, "value")?.ok_or("set_flag needs 'value': true or false")?;
                done(self.set_flag(flag, value)?)
            }
            "set_mod_flavor" => {
                let flavor = match req_str(op, "flavor")? {
                    "protracker" => ModFlavor::ProTracker,
                    "noisetracker" => ModFlavor::NoiseTracker,
                    other => return Err(format!("'{other}' is not protracker or noisetracker")),
                };
                let (adjusted, c) = self.set_mod_flavor(flavor)?;
                Ok(json!({"adjusted": adjusted, "changed": c.to_json()}))
            }
            "set_channels" => done(self.set_channels(
                req_usize(op, "count")?,
                opt_bool(op, "force")?.unwrap_or(false),
            )?),
            "set_channel" => done(self.set_channel(
                req_usize(op, "channel")?,
                opt_u8(op, "pan")?,
                opt_bool(op, "muted")?,
            )?),
            // ---- samples and instruments
            "set_sample" => {
                let (s, c) = self.set_sample(sample_ref(op)?, &sample_edit(op)?)?;
                Ok(json!({"sample": s, "changed": c.to_json()}))
            }
            "import_sample" => {
                let bytes = base64_decode(req_str(op, "data")?)?;
                let opts = import_options(op)?;
                let (s, c) = self.import_sample(sample_ref(op)?, &bytes, &opts)?;
                Ok(json!({"sample": s, "changed": c.to_json()}))
            }
            "normalize_sample" => done(self.normalize_sample(sample_ref(op)?)?),
            "reverse_sample" => done(self.reverse_sample(sample_ref(op)?)?),
            "clear_sample" => done(self.clear_sample(sample_ref(op)?)?),
            "halve_sample" => done(self.halve_sample(sample_ref(op)?)?),
            "generate_pulse" => {
                let duty = |key: &str, default: f64| -> Res<f32> {
                    let v = op.get(key).and_then(Value::as_f64).unwrap_or(default);
                    if (1.0..=99.0).contains(&v) {
                        Ok(v as f32)
                    } else {
                        Err(format!(
                            "'{key}' is a duty cycle of 1 to 99 percent, not {v}"
                        ))
                    }
                };
                let sweep = opt_str(op, "sweep")?.unwrap_or("up");
                if sweep != "up" && sweep != "pingpong" {
                    return Err(format!("'{sweep}' is not a sweep (up, pingpong)"));
                }
                let (s, c) = self.generate_pulse(
                    sample_ref(op)?,
                    opt_usize(op, "cycle_length")?.unwrap_or(64),
                    opt_usize(op, "cycles")?.unwrap_or(8),
                    duty("duty_start", 25.0)?,
                    duty("duty_end", 50.0)?,
                    sweep == "pingpong",
                )?;
                Ok(json!({"sample": s, "changed": c.to_json()}))
            }
            "generate_wave" => {
                let wave = req_str(op, "wave")?;
                let wave = Wave::parse(wave).ok_or_else(|| {
                    format!("'{wave}' is not a wave (sine, triangle, saw, square, pulse, noise)")
                })?;
                let (s, c) = self.generate_wave(
                    sample_ref(op)?,
                    wave,
                    opt_usize(op, "length")?.unwrap_or(64),
                    opt_usize(op, "seed")?.unwrap_or(1) as u32,
                )?;
                Ok(json!({"sample": s, "changed": c.to_json()}))
            }
            "add_instrument" => {
                let (n, c) = self.add_instrument(opt_str(op, "name")?)?;
                Ok(json!({"instrument": n, "changed": c.to_json()}))
            }
            "remove_instrument" => done(self.remove_instrument(
                req_usize(op, "instrument")?,
                opt_bool(op, "force")?.unwrap_or(false),
            )?),
            "add_sample" => {
                let (n, c) = self.add_sample(req_usize(op, "instrument")?)?;
                Ok(json!({"sample": n, "changed": c.to_json()}))
            }
            "remove_sample" => done(self.remove_sample(
                req_usize(op, "instrument")?,
                opt_usize(op, "sample")?.unwrap_or(0),
            )?),
            "set_instrument" => {
                let edit = instruments::InstrumentEdit {
                    name: opt_str(op, "name")?.map(str::to_string),
                    fadeout: opt_usize(op, "fadeout")?.map(|n| n.min(u16::MAX as usize) as u16),
                    vibrato_kind: opt_u8(op, "vibrato_kind")?,
                    vibrato_sweep: opt_u8(op, "vibrato_sweep")?,
                    vibrato_depth: opt_u8(op, "vibrato_depth")?,
                    vibrato_rate: opt_u8(op, "vibrato_rate")?,
                };
                done(self.set_instrument(req_usize(op, "instrument")?, &edit)?)
            }
            "set_envelope" => {
                let kind = match req_str(op, "kind")? {
                    "volume" => EnvelopeKind::Volume,
                    "panning" => EnvelopeKind::Panning,
                    other => return Err(format!("'{other}' is not an envelope (volume, panning)")),
                };
                done(self.set_envelope(req_usize(op, "instrument")?, kind, &envelope_edit(op)?)?)
            }
            "set_keymap" => {
                let instrument = req_usize(op, "instrument")?;
                if let Some(list) = op.get("keymap").and_then(Value::as_array) {
                    if list.len() != 96 {
                        return Err("a keymap has 96 entries (C-0 to B-7)".into());
                    }
                    let mut map = [0u8; 96];
                    for (k, v) in map.iter_mut().zip(list) {
                        *k = v
                            .as_u64()
                            .filter(|&n| n < 16)
                            .ok_or("keymap entries are sample numbers 0-15")?
                            as u8;
                    }
                    done(self.set_keymap_all(instrument, map)?)
                } else {
                    let from = byte(op, "from")?;
                    let to = opt_u8(op, "to")?.unwrap_or(from);
                    done(self.set_keymap(instrument, from, to, req_usize(op, "sample")?)?)
                }
            }
            other => Err(format!("unknown operation '{other}'")),
        }
    }
}

// ---------------------------------------------------------------------------
// operation field readers

fn byte(op: &Value, key: &str) -> Res<u8> {
    opt_u8(op, key)?.ok_or_else(|| format!("'{key}' is needed (0-255)"))
}

fn sample_ref(op: &Value) -> Res<SampleRef> {
    let instrument = opt_usize(op, "instrument")?
        .or(opt_usize(op, "sample_slot")?)
        .ok_or("'instrument' is needed (the sample or instrument number, from 1)")?;
    let sample = match (opt_usize(op, "sample")?, field(op, "sample")) {
        (Some(s), _) => s,
        (None, Some(_)) => return Err("'sample' must be a whole number".into()),
        (None, None) => 0,
    };
    Ok(SampleRef { instrument, sample })
}

fn sample_edit(op: &Value) -> Res<SampleEdit> {
    let mut e = SampleEdit {
        name: opt_str(op, "name")?.map(str::to_string),
        volume: opt_u8(op, "volume")?,
        finetune: opt_i64(op, "finetune")?.map(|n| n.clamp(-1000, 1000) as i32),
        relative_note: opt_i64(op, "relative_note")?.map(|n| n.clamp(-1000, 1000) as i32),
        pan: opt_u8(op, "pan")?,
        c2spd: opt_usize(op, "c2spd")?.map(|n| n.min(u32::MAX as usize) as u32),
        loop_kind: None,
        loop_start: opt_usize(op, "loop_start")?,
        loop_len: opt_usize(op, "loop_len")?,
        adlib: None,
    };
    if let Some(k) = opt_str(op, "loop_kind")? {
        e.loop_kind = Some(
            parse_loop_kind(k)
                .ok_or_else(|| format!("'{k}' is not a loop kind (off, forward, pingpong)"))?,
        );
    }
    if let Some(a) = field(op, "adlib") {
        let regs = a
            .get("registers")
            .and_then(Value::as_array)
            .filter(|r| r.len() == 12)
            .ok_or("adlib needs 'registers': 12 bytes")?;
        let mut registers = [0u8; 12];
        for (d, v) in registers.iter_mut().zip(regs) {
            *d = v
                .as_u64()
                .filter(|&n| n <= 255)
                .ok_or("registers are bytes")? as u8;
        }
        e.adlib = Some(crate::module::Adlib {
            registers,
            melodic: a.get("melodic").and_then(Value::as_bool).unwrap_or(true),
        });
    }
    Ok(e)
}

fn import_options(op: &Value) -> Res<ImportOptions> {
    let kind = opt_str(op, "kind")?.unwrap_or("wav");
    let channels = opt_usize(op, "channels")?.unwrap_or(1);
    let rate = || {
        opt_usize(op, "rate")?
            .map(|r| r as u32)
            .ok_or_else(|| "raw audio needs 'rate' (Hz)".to_string())
    };
    let mut o = match kind {
        "wav" => ImportOptions::wav(),
        "pcm8" => ImportOptions::raw(8, false, channels, rate()?),
        "pcm8u" => ImportOptions::raw(8, true, channels, rate()?),
        "pcm16" => ImportOptions::raw(16, false, channels, rate()?),
        other => {
            return Err(format!(
                "'{other}' is not a kind of audio (wav, pcm8, pcm8u, pcm16)"
            ))
        }
    };
    if let Some(m) = opt_str(op, "mix")? {
        o.mix = ChannelMix::parse(m).ok_or_else(|| format!("'{m}' is not mix, left or right"))?;
    }
    if let Some(q) = opt_str(op, "resample")? {
        o.quality = Quality::parse(q).ok_or_else(|| format!("'{q}' is not sinc or linear"))?;
    }
    o.note = opt_u8(op, "note")?;
    o.target_rate = opt_usize(op, "target_rate")?.map(|r| r as u32);
    o.bits = opt_u8(op, "bits")?;
    o.normalize = opt_bool(op, "normalize")?.unwrap_or(false);
    o.name = opt_str(op, "name")?.map(str::to_string);
    if let Some(k) = opt_bool(op, "keep_loop")? {
        o.keep_loop = k;
    }
    o.truncate = opt_bool(op, "truncate")?.unwrap_or(false);
    Ok(o)
}

fn envelope_edit(op: &Value) -> Res<EnvelopeEdit> {
    let mut e = EnvelopeEdit {
        enabled: opt_bool(op, "enabled")?,
        ..EnvelopeEdit::default()
    };
    if let Some(list) = field(op, "points") {
        let list = list
            .as_array()
            .ok_or("'points' is a list of [tick, value]")?;
        let mut points = Vec::with_capacity(list.len());
        for p in list {
            let a = p
                .as_array()
                .filter(|a| a.len() == 2)
                .ok_or("a point is [tick, value]")?;
            let t = a[0]
                .as_u64()
                .filter(|&n| n <= 65535)
                .ok_or("a tick is a whole number")?;
            let v = a[1]
                .as_u64()
                .filter(|&n| n <= 65535)
                .ok_or("a value is a whole number")?;
            points.push((t as u16, v as u16));
        }
        e.points = Some(points);
    }
    if op.get("sustain").is_some() {
        e.sustain = Some(opt_usize(op, "sustain")?);
    }
    if let Some(l) = op.get("loop") {
        e.looped = Some(match l {
            Value::Null => None,
            Value::Array(a) if a.len() == 2 => {
                let first = a[0].as_u64().ok_or("loop is [first point, last point]")? as usize;
                let last = a[1].as_u64().ok_or("loop is [first point, last point]")? as usize;
                Some((first, last))
            }
            _ => return Err("loop is [first point, last point] or null".into()),
        });
    }
    Ok(e)
}

/// Reads back the parts of a `changed` object a batch merges.
fn changes_from_json(v: &Value) -> Changes {
    let mut c = Changes::default();
    if let Some(list) = v.get("patterns").and_then(Value::as_array) {
        for p in list {
            if let (Some(n), Some(a), Some(b)) = (
                p.get("pattern").and_then(Value::as_u64),
                p.get("row0").and_then(Value::as_u64),
                p.get("row1").and_then(Value::as_u64),
            ) {
                c.pattern_rows(n as usize, a as usize, b as usize);
            }
        }
    }
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    c.layout = flag("layout");
    c.orders = flag("orders");
    c.header = flag("header");
    c.channels = flag("channels");
    c.instrument_list = flag("instrument_list");
    if let Some(list) = v.get("instruments").and_then(Value::as_array) {
        for i in list.iter().filter_map(Value::as_u64) {
            c.instrument(i as usize);
        }
    }
    c
}
