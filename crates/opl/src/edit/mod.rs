//! Editing Adlib Tracker II songs and OPL instruments as data.
//!
//! An [`A2mDoc`] holds an [`A2mSong`] (an `.a2m` file's song: instrument bank,
//! macro tables, patterns, order list, header) and changes it the way Adlib
//! Tracker II does, checking every change against what the song's version of the
//! format can hold: 9 (versions 1-4), 18 (5-8) or 1-20 (9 and up) tracks, 250
//! or 255 instruments, effects 0-F, 0-Z or 0-47 (`<`), one effect column or
//! two. A change that does not fit is refused with a message and leaves the song
//! as it was.
//!
//! Everything is available as typed methods and as one JSON entry point,
//! [`A2mDoc::apply`], for hosts that talk JSON (the Lua editor does):
//! `{"op": "set_cell", "pattern": 0, "row": 3, "channel": 1, "note": "C-4",
//! "instrument": 1}` returns `{"cell": [...], "changed": {...}}` (what to
//! redraw and, with [`live_edits`](A2mDoc::live_edits), what a playing song can
//! take as it is). [`OplInstrumentDoc`] does the same for one instrument alone
//! (an S3M AdLib instrument, an instrument file): the DAW's instrument editor.
//!
//! # Numbers
//!
//! Patterns, rows, tracks (`channel`) and order positions count from 0;
//! instruments, macros and macro entries count from 1, as a cell names them (0 is
//! none). Ranges (`row0`..`row1`, `channel0`..`channel1`) include both ends. A
//! pattern is `pattern_len` rows (the song's length: every pattern of an A2M
//! song has the same) by `tracks` tracks.
//!
//! A cell is the array `[note, instrument, effect, param, effect2, param2]`:
//!
//! - `note`: 0 none, 255 key off, otherwise a MIDI number from 12 (C-0, AT2's
//!   note 1) to 107 (B-7), or 108 (C-8, AT2's note 97, which real songs use), as in `cubed_tracker::edit`. A fixed note (version 9
//!   and up: it plays as that note and the editor will not transpose it) is the
//!   number plus 128. A byte the format does not define is shown as 256 plus the
//!   byte, so a read never loses one. `info().note.octave_offset` is what to
//!   take from `note / 12` for AT2's octave number (1: 60 is `C-4`).
//! - `instrument`: 1-250 (255 from version 9), 0 for none.
//! - `effect`, `param`, `effect2`, `param2`: the command number as the file holds
//!   it (0-9, then 10 is `A` ... 35 is `Z`, then `&%!@=#$~^`><` are 36-47) and its
//!   parameter byte, in the module's own version's numbering. The operations also
//!   take the letter as text. The second column exists from version 9.
//!
//! # Undo
//!
//! Every edit is undoable: [`A2mDoc::undo`] and [`A2mDoc::redo`] step through
//! the log and return what changed. Edits made between [`A2mDoc::begin_group`]
//! and [`A2mDoc::end_group`] are one step. The log records only what each edit
//! overwrites (see `history.rs`), so it is small, and it is bounded in steps
//! and bytes.

mod cells;
mod fm_json;
mod history;
mod instrument_doc;
mod instruments;
mod json;
mod limits;
mod song;

use serde_json::{json, Value};

pub use cells::{cell_json, effect_text, CellUpdate, ClearMask, Clip, PasteMode, Pasted, Region};
pub use fm_json::{operator_edit, operator_json, voice_edit, voice_json};
pub use history::{Changes, MacroKind, MAX_CELLS};
pub use instrument_doc::{OplInstrumentDoc, OplKind};
pub use instruments::{
    ArpeggioEdit, FmMacroEdit, FmStepEdit, InstrumentEdit, VibratoEdit, VOICE_TYPES,
};
pub use limits::{
    check_cell, effect_char, encode_text, limits, limits_of, note_from_json, note_name,
    note_to_json, parse_effect, parse_note, text, Limits, EFFECT_CHARS,
};
pub use song::Flag;

use self::history::{Change, History};
use self::json::{opt_bool, opt_str, opt_u8, opt_usize, req_i64, req_str, req_usize, Res};
use crate::a2::engine::A2Engine;
use crate::a2::live::LiveEdit;
use crate::a2::model::{parse, A2mSong, Cell, Pattern};
use crate::a2::write::{new_song, output_version, write};

/// A song being edited. See the module docs.
#[derive(Debug)]
pub struct A2mDoc {
    song: A2mSong,
    history: History,
    revision: u64,
    /// What the last edit, undo or redo changed.
    last: Changes,
}

/// A parsed song's order list may name patterns past the file's pattern count
/// (Adlib Tracker II plays them as empty, or from the rest of the file's last
/// block). The document shows them: they join the pattern list.
fn promote_named_patterns(song: &mut A2mSong) {
    let lim = limits(song);
    let top = song
        .order
        .iter()
        .filter(|&&o| o < 0x80)
        .max()
        .map_or(0, |&o| o as usize);
    if song.patterns.len() > top || top >= lim.max_patterns {
        return;
    }
    let blank = || Pattern {
        rows: lim.storage_rows,
        channels: lim.storage_tracks,
        cells: vec![Cell::default(); lim.storage_rows * lim.storage_tracks],
    };
    while song.patterns.len() <= top {
        let p = if song.spare_patterns.is_empty() {
            blank()
        } else {
            song.spare_patterns.remove(0)
        };
        song.patterns.push(p);
    }
    let n = song.patterns.len();
    let want = n.next_multiple_of(lim.patterns_per_block) - n;
    song.spare_patterns.truncate(want);
    while song.spare_patterns.len() < want {
        song.spare_patterns.push(blank());
    }
}

impl A2mDoc {
    // ----- making and reading ------------------------------------------------

    /// A new, empty song (version 14): one empty 64-row pattern played over and
    /// over, one plain instrument, tempo 50, speed 6. `opl3` picks 18 tracks over
    /// 9 (an OPL2's).
    pub fn new(opl3: bool) -> A2mDoc {
        A2mDoc::from_song(new_song(opl3))
    }

    /// Edits a song the caller built or parsed.
    pub fn from_song(mut song: A2mSong) -> A2mDoc {
        promote_named_patterns(&mut song);
        A2mDoc {
            song,
            history: History::new(),
            revision: 0,
            last: Changes::default(),
        }
    }

    /// Reads an `.a2m` file. Patterns the order list names past the file's
    /// pattern count join the list (see `info`); nothing else changes.
    pub fn from_bytes(bytes: &[u8]) -> Res<A2mDoc> {
        let song = parse(bytes).map_err(|e| format!("Cannot read this A2M module: {e}"))?;
        Ok(A2mDoc::from_song(song))
    }

    /// The song as an `.a2m` file. Versions 1 and 5 (SIXPACK) are written as
    /// 4 and 8, the same layouts stored; later versions keep theirs (aPLib or
    /// LZH packed). The writer's error says what the file cannot hold.
    pub fn to_bytes(&self) -> Res<Vec<u8>> {
        write(&self.song)
    }

    /// The song, for playing or inspecting.
    pub fn song(&self) -> &A2mSong {
        &self.song
    }

    /// The song, taken out.
    pub fn into_song(self) -> A2mSong {
        self.song
    }

    pub fn limits(&self) -> Limits {
        limits(&self.song)
    }

    /// Rows every pattern shows (`pattern_len`).
    pub fn pattern_len(&self) -> usize {
        let stored = self.song.patterns.first().map_or(1, |p| p.rows);
        (self.song.pattern_len as usize).clamp(1, stored.max(1))
    }

    /// Tracks every pattern shows.
    pub fn tracks(&self) -> usize {
        let stored = self.song.patterns.first().map_or(1, |p| p.channels);
        (self.song.tracks as usize).clamp(1, stored.max(1))
    }

    /// What the last edit, undo or redo changed (also what its result's
    /// `changed` says).
    pub fn last_changes(&self) -> &Changes {
        &self.last
    }

    /// Counts every change made (and every undo and redo): a UI can cache by it.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Why a player would refuse this song (its order list plays nothing), or
    /// `None` when it plays. An instrument collection saved as a song is a
    /// document like any other: the instrument editor opens it.
    pub fn refusal(&self) -> Option<String> {
        A2Engine::refusal(&self.song)
    }

    /// The last change as live edits, for a song that is playing this one:
    /// `Some` with the current content of each cell, instrument and macro that
    /// changed (and the tempo and speed, if they did) when the change was
    /// nothing else; `None` when it moved the song's shape (the pattern list,
    /// the order list, the pattern length, the tracks, the flags, the macro
    /// speed-up, lock flags, 4-op pairs) or touched more than [`MAX_CELLS`]
    /// cells, which is a new song: open the edited one and swap it in. Each
    /// [`LiveEdit`] lands in a playing `A2Player` with `apply_edit`.
    pub fn live_edits(&self) -> Option<Vec<LiveEdit>> {
        self.live_edits_of(&self.last)
    }

    /// [`live_edits`](Self::live_edits) for a [`Changes`] this song's edits
    /// returned.
    pub fn live_edits_of(&self, changes: &Changes) -> Option<Vec<LiveEdit>> {
        if !changes.is_live() {
            return None;
        }
        let s = &self.song;
        let mut out = Vec::new();
        for &(p, r, c) in &changes.cells {
            let pattern = s.patterns.get(p)?;
            if r >= pattern.rows || c >= pattern.channels {
                return None;
            }
            out.push(LiveEdit::Cell {
                pattern: p as u16,
                row: r as u16,
                track: c as u8,
                cell: *pattern.cell(r, c),
            });
        }
        for &n in &changes.instruments {
            let i = s.instruments.get(n.checked_sub(1)?)?;
            out.push(LiveEdit::Instrument {
                number: n as u8,
                fm: i.fm,
                panning: i.panning,
                finetune: i.finetune,
                voice_type: i.voice_type,
            });
        }
        for &(kind, n) in &changes.macros {
            let at = n.checked_sub(1)?;
            out.push(match kind {
                MacroKind::Fm => LiveEdit::FmMacro {
                    number: n as u8,
                    table: s.fm_macros.get(at)?.clone(),
                },
                MacroKind::Arpeggio => LiveEdit::ArpeggioMacro {
                    number: n as u8,
                    table: s.arpeggio_macros.get(at)?.clone(),
                },
                MacroKind::Vibrato => LiveEdit::VibratoMacro {
                    number: n as u8,
                    table: s.vibrato_macros.get(at)?.clone(),
                },
            });
        }
        if changes.timing {
            out.push(LiveEdit::Timing {
                tempo: s.tempo,
                speed: s.speed,
            });
        }
        Some(out)
    }

    /// What is wrong with the song as a file of its version: cells it cannot
    /// hold (the first 20), orders that name a pattern that is not there, and
    /// what the writer refuses. An empty list is a song that saves.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = self.validate_content();
        if let Err(e) = write(&self.song) {
            problems.push(format!("cannot be saved: {e}"));
        }
        problems
    }

    /// [`validate`](Self::validate) without trying the writer (which packs
    /// megabytes): the cells and the order list.
    pub fn validate_content(&self) -> Vec<String> {
        let lim = self.limits();
        let mut problems = Vec::new();
        let (rows, tracks) = (self.pattern_len(), self.tracks());
        'outer: for (p, pattern) in self.song.patterns.iter().enumerate() {
            for row in 0..rows.min(pattern.rows) {
                for ch in 0..tracks.min(pattern.channels) {
                    if let Err(e) = check_cell(&lim, pattern.cell(row, ch)) {
                        problems.push(format!("pattern {p}, row {row}, track {}: {e}", ch + 1));
                        if problems.len() >= 20 {
                            break 'outer;
                        }
                    }
                }
            }
        }
        let (leading, _, _) = self.order_parts();
        for (i, &p) in leading.iter().enumerate() {
            if p as usize >= self.song.patterns.len() {
                problems.push(format!("order {i} names pattern {p}, which does not exist"));
            }
        }
        problems
    }

    /// The song's summary: `{"format": "a2m", "version", "file_version" (what
    /// `to_bytes` writes), "title", "composer", "tracks", "patterns", "orders",
    /// "restart", "pattern_len", "speed", "tempo", "refresh" (timer Hz),
    /// "macro_speedup", "flags": {name: bool, "raw"}, "four_op_tracks",
    /// "lock_flags", "rows_per_beat", "tempo_finetune", "note": {"min", "max",
    /// "octave_offset"}, "limits", "instruments" (slots), "used_instruments" (up to
    /// the last with data), "revision", "can_undo", "can_redo", "undo_steps",
    /// "redo_steps", "history_bytes", "playable": null | the reason a player
    /// refuses it}`.
    pub fn info(&self) -> Value {
        let s = &self.song;
        let (leading, restart, _) = self.order_parts();
        let flags: serde_json::Map<String, Value> = Flag::ALL
            .iter()
            .map(|f| (f.name().to_string(), json!(s.flags & f.bit() != 0)))
            .chain([("raw".to_string(), json!(s.flags))])
            .collect();
        let tempo = if s.tempo <= 18 { 18.2 } else { s.tempo as f64 };
        let used = s
            .instruments
            .iter()
            .rposition(|i| i.fm != [0; 11] || !i.name.is_empty())
            .map_or(0, |i| i + 1);
        json!({
            "format": "a2m",
            "version": s.version,
            "file_version": output_version(s.version),
            "title": text(&s.name),
            "composer": text(&s.composer),
            "tracks": self.tracks(),
            "patterns": s.patterns.len(),
            "orders": leading.len(),
            "restart": restart,
            "pattern_len": self.pattern_len(),
            "speed": s.speed,
            "tempo": s.tempo,
            "refresh": tempo * s.macro_speedup.max(1) as f64,
            "macro_speedup": s.macro_speedup,
            "flags": flags,
            "four_op_tracks": s.four_op_tracks,
            "lock_flags": s.lock_flags,
            "rows_per_beat": s.rows_per_beat,
            "tempo_finetune": s.tempo_finetune,
            "note": {"min": 12, "max": 108, "typed_max": 107, "octave_offset": 1},
            "limits": self.limits().to_json(),
            "instruments": s.instruments.len(),
            "used_instruments": used,
            "revision": self.revision,
            "can_undo": self.can_undo(),
            "can_redo": self.can_redo(),
            "undo_steps": self.history.undo_steps(),
            "redo_steps": self.history.redo_steps(),
            "history_bytes": self.history.bytes(),
            "playable": self.refusal(),
        })
    }

    /// The cells of `count` rows from `row0` of a pattern, for drawing:
    /// `{"pattern", "row0", "count", "rows" (the pattern's length), "channels"
    /// (the tracks), "cells": [[[note, instrument, effect, param, effect2,
    /// param2] per track] per row]}`. Rows past the end are left out. With
    /// `flat`, `cells` is one array of `count * channels * 6` numbers, row by
    /// row.
    pub fn rows(&self, pattern: usize, row0: usize, count: usize, flat: bool) -> Res<Value> {
        let p = self.pattern_ref(pattern)?;
        let (rows, tracks) = (
            self.pattern_len().min(p.rows),
            self.tracks().min(p.channels),
        );
        let end = row0.saturating_add(count).min(rows);
        let row0 = row0.min(end);
        let cells = if flat {
            let mut v = Vec::with_capacity((end - row0) * tracks * 6);
            for r in row0..end {
                for c in 0..tracks {
                    let cell = p.cell(r, c);
                    v.extend([
                        note_to_json(cell.note),
                        cell.instrument as u32,
                        cell.effects[0].0 as u32,
                        cell.effects[0].1 as u32,
                        cell.effects[1].0 as u32,
                        cell.effects[1].1 as u32,
                    ]);
                }
            }
            json!(v)
        } else {
            Value::Array(
                (row0..end)
                    .map(|r| Value::Array((0..tracks).map(|c| cell_json(p.cell(r, c))).collect()))
                    .collect(),
            )
        };
        Ok(json!({
            "pattern": pattern,
            "row0": row0,
            "count": end - row0,
            "rows": rows,
            "channels": tracks,
            "cells": cells,
        }))
    }

    /// The song: `{"orders": [pattern numbers before the first jump marker],
    /// "restart" (the position that marker jumps to, `null` if the list is
    /// full), "tail" (the entries after the marker, as stored), "rows": [the
    /// length of each order's pattern], "names": [pattern names], "patterns"
    /// (the count)}`.
    pub fn order(&self) -> Value {
        let (leading, restart, tail) = self.order_parts();
        json!({
            "orders": leading,
            "restart": restart,
            "tail": tail,
            "rows": leading.iter().map(|_| self.pattern_len()).collect::<Vec<_>>(),
            "names": self.song.pattern_names.iter().take(self.song.patterns.len()).map(|n| text(n)).collect::<Vec<_>>(),
            "patterns": self.song.patterns.len(),
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
            let inverse = c.apply(&mut self.song);
            inverse.touch(&self.song, &mut changes);
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
            let inverse = c.apply(&mut self.song);
            inverse.touch(&self.song, &mut changes);
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
        let inverse = c.apply(&mut self.song);
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
                    c.touch(&self.song, &mut changes);
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
                        c.apply(&mut self.song);
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
    /// - `{"op": "info"}`, `{"op": "order"}`, `{"op": "instruments"}`,
    ///   `{"op": "instrument", "instrument": n}`, `{"op": "macros"}`,
    ///   `{"op": "macro", "kind": "fm" | "arpeggio" | "vibrato", "macro": n}`,
    ///   `{"op": "validate"}` -> `{"problems": [..]}`
    /// - `{"op": "live_edits"}` -> `{"edits": [..] | null}` for the last edit
    ///   (see [`live_edits`](Self::live_edits))
    /// - `{"op": "rows", "pattern", "row0", "count", "flat"}`
    /// - `{"op": "copy_range", "pattern", "row0", "row1", "channel0", "channel1"}`
    ///   -> the clip (`rows`, `channels`, `cells`)
    /// - `{"op": "note_name", "note": n}` / `{"op": "effect_text", "effect",
    ///   "param"}` -> the text a grid shows
    ///
    /// Cells and patterns:
    /// - `set_cell` (`pattern`, `row`, `channel`, and `cell: [n,i,e,p,e2,p2]` or
    ///   any of `note`, `instrument`, `effect`, `param`, `effect2`, `param2`; a
    ///   note may be text like `"C-4"`, an effect a letter; `fixed: true` makes the
    ///   note a fixed one) -> `{"cell"}`
    /// - `clear_range` (`pattern`, `row0`, `row1`, `channel0`, `channel1`,
    ///   optional `columns: ["note", "instrument", "effect", "effect2"]`)
    /// - `insert_row` / `delete_row` (`pattern`, `row`, `count`, optional
    ///   `channel0`/`channel1` to shift only those tracks)
    /// - `transpose` (a range, `semitones`, `clamp`, `instrument`) -> `{"count"}`
    /// - `paste` (`pattern`, `row`, `channel`, `cells` or a `clip`, `mode`:
    ///   `"overwrite"` or `"mix"`) -> `{"rows", "channels", "clipped"}`
    /// - `set_pattern_len` (`rows`, `force`; also `set_pattern_rows`),
    ///   `new_pattern` -> `{"pattern"}`, `duplicate_pattern`, `delete_pattern`,
    ///   `set_pattern_name` (`pattern`, `name`; version 11+)
    ///
    /// Song:
    /// - `set_order` (`index`, `pattern`), `insert_order`, `delete_order`
    ///   (`index`), `move_order` (`from`, `to`), `set_restart` (`order`),
    ///   `set_orders` (`orders`, `restart`)
    /// - `set_title`, `set_composer` (`text`), `set_tempo` (`tempo`), `set_speed`
    ///   (`speed`), `set_flag` (`flag`, `value`), `set_tracks` (`count`, `force`;
    ///   version 9+), `set_macro_speedup` (`factor`), `set_four_op_tracks`
    ///   (`mask`, or `pair` 0-5 and `on`), `set_lock_flags` (`track`, `value`),
    ///   `set_rows_per_beat` (`rows`), `set_tempo_finetune` (`finetune`),
    ///   `set_four_op_instrument` (`pair` 1-128, `on`)
    ///
    /// Instruments (`instrument` 1-250/255):
    /// - `set_instrument` (`name`, `panning`, `finetune`, `voice_type`, `fm`: the
    ///   eleven bytes, and the voice fields `modulator` / `carrier`: objects of
    ///   `tremolo`, `vibrato`, `sustain`, `ksr`, `multiplier`, `key_scale_level`,
    ///   `total_level` or `level`, `attack`, `decay`, `sustain_level`, `release`,
    ///   `waveform`; `feedback`, `connection`) -> `{"name"}`
    /// - `set_operator` (`instrument`, `operator`: `"modulator"` or
    ///   `"carrier"`, and the operator's fields flat)
    /// - `swap_operators`, `reset_instrument`, `clear_instrument` (`keep_name`,
    ///   `with_macro`), `copy_instrument` (`from`, `to`), `set_disabled_columns`
    ///   (`instrument`, `columns`: 28 flags; version 11+)
    ///
    /// Macros (version 9+; `macro` 1-255):
    /// - `set_fm_macro` (`length`, `loop_begin`, `loop_length`, `keyoff_pos`,
    ///   `arpeggio_table`, `vibrato_table`, `steps: [{"step": n, "fm" or voice
    ///   fields, "freq_slide", "panning", "duration"}]`), `set_arpeggio_macro`
    ///   (+ `speed`, `data: [..]` from entry 1 or `entries: [{"entry", "value"}]`),
    ///   `set_vibrato_macro` (+ `speed`, `delay`), `clear_macro` (`kind`, `macro`)
    ///
    /// History: `begin_group`, `end_group`, `undo`, `redo` (each returns
    /// `{"changed"}` of what moved), and `{"op": "batch", "ops": [...]}`, which
    /// runs the operations as one undo step and, if one fails, none of them.
    pub fn apply(&mut self, op: &Value) -> Res<Value> {
        let (mut v, changes) = self.run(op)?;
        if let Some(c) = changes {
            v["changed"] = c.to_json();
        }
        Ok(v)
    }

    /// One operation: its own fields and, for an edit, what it changed.
    fn run(&mut self, op: &Value) -> Res<(Value, Option<Changes>)> {
        let name = req_str(op, "op")?;
        let lim = self.limits();
        let read = |v: Value| Ok((v, None));
        let done = |c: Changes| Ok((json!({}), Some(c)));
        let with = |v: Value, c: Changes| Ok((v, Some(c)));
        match name {
            // ---- reading
            "info" => read(self.info()),
            "order" => read(self.order()),
            "instruments" => read(self.instruments()),
            "instrument" => read(self.instrument(req_usize(op, "instrument")?)?),
            "macros" => read(self.macros()),
            "macro" => read(self.macro_json(macro_kind(op)?, req_usize(op, "macro")?)?),
            "validate" => read(json!({"problems": self.validate()})),
            "live_edits" => read(json!({
                "edits": self.live_edits().map(|v| v.iter().map(LiveEdit::to_json).collect::<Vec<_>>()),
            })),
            "rows" => read(self.rows(
                req_usize(op, "pattern")?,
                opt_usize(op, "row0")?.unwrap_or(0),
                opt_usize(op, "count")?.unwrap_or(usize::MAX),
                opt_bool(op, "flat")?.unwrap_or(false),
            )?),
            "copy_range" => {
                let region = self.region_from_op(op)?;
                read(self.copy_range(region)?.to_json())
            }
            "note_name" => {
                let n = req_i64(op, "note")?;
                let byte = note_from_json(n.max(0) as u64, &lim)?;
                read(json!({"text": note_name(byte)}))
            }
            "effect_text" => {
                let fx = op
                    .get("effect")
                    .and_then(|v| {
                        v.as_u64()
                            .map(|n| n as u8)
                            .or_else(|| v.as_str().and_then(parse_effect))
                    })
                    .ok_or("'effect' is a number or a letter")?;
                read(json!({"text": effect_text(fx, opt_u8(op, "param")?.unwrap_or(0))}))
            }
            // ---- history
            "begin_group" => {
                self.begin_group();
                read(json!({}))
            }
            "end_group" => {
                self.end_group();
                read(json!({}))
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
                let (results, changes) = self.edit(|d| {
                    let mut results = Vec::with_capacity(ops.len());
                    for (i, o) in ops.iter().enumerate() {
                        let r = d.apply(o).map_err(|e| {
                            format!(
                                "operation {i} ({}) failed: {e}",
                                o.get("op").and_then(Value::as_str).unwrap_or("?")
                            )
                        })?;
                        results.push(r);
                    }
                    Ok(results)
                })?;
                with(json!({"results": results}), changes)
            }
            // ---- cells
            "set_cell" => {
                let update = CellUpdate::from_op(&lim, op)?;
                let (cell, changes) = self.set_cell(
                    req_usize(op, "pattern")?,
                    req_usize(op, "row")?,
                    req_usize(op, "channel")?,
                    update,
                )?;
                with(json!({"cell": cell_json(&cell)}), changes)
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
                let last = self.tracks() - 1;
                let channels = channels.map(|(a, b)| (a.min(last), b.min(last)));
                done(if name == "insert_row" {
                    self.insert_row(pattern, row, channels, count)?
                } else {
                    self.delete_row(pattern, row, channels, count)?
                })
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
                with(json!({"count": count}), changes)
            }
            "paste" => {
                let clip = Clip::from_json(
                    &lim,
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
                with(
                    json!({"rows": p.rows, "channels": p.channels, "clipped": p.clipped}),
                    changes,
                )
            }
            "set_pattern_len" | "set_pattern_rows" => done(self.set_pattern_len(
                req_usize(op, "rows")?,
                opt_bool(op, "force")?.unwrap_or(false),
            )?),
            "new_pattern" => {
                let (p, c) = self.new_pattern()?;
                with(json!({"pattern": p}), c)
            }
            "duplicate_pattern" => {
                let (p, c) = self.duplicate_pattern(req_usize(op, "pattern")?)?;
                with(json!({"pattern": p}), c)
            }
            "delete_pattern" => done(self.delete_pattern(req_usize(op, "pattern")?)?),
            "set_pattern_name" => {
                let (n, c) =
                    self.set_pattern_name(req_usize(op, "pattern")?, req_str(op, "name")?)?;
                with(json!({"name": n}), c)
            }
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
            "set_orders" => {
                let list = op
                    .get("orders")
                    .and_then(Value::as_array)
                    .ok_or("'orders' is a list of pattern numbers")?;
                let orders = list
                    .iter()
                    .map(|v| {
                        v.as_u64()
                            .map(|n| n as usize)
                            .ok_or("'orders' holds pattern numbers")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                done(self.set_orders(orders, opt_usize(op, "restart")?.unwrap_or(0))?)
            }
            "set_title" => {
                let (t, c) = self.set_title(text_arg(op, "title")?)?;
                with(json!({"title": t}), c)
            }
            "set_composer" => {
                let (t, c) = self.set_composer(text_arg(op, "composer")?)?;
                with(json!({"composer": t}), c)
            }
            "set_tempo" => done(self.set_tempo(byte(op, "tempo")?)?),
            "set_speed" => done(self.set_speed(byte(op, "speed")?)?),
            "set_flag" => {
                let flag = req_str(op, "flag")?;
                let flag = Flag::parse(flag).ok_or_else(|| {
                    format!(
                        "'{flag}' is not a flag ({})",
                        Flag::ALL.map(|f| f.name()).join(", ")
                    )
                })?;
                let value =
                    opt_bool(op, "value")?.ok_or("set_flag needs 'value': true or false")?;
                done(self.set_flag(flag, value)?)
            }
            "set_tracks" => done(self.set_tracks(
                req_usize(op, "count")?,
                opt_bool(op, "force")?.unwrap_or(false),
            )?),
            "set_macro_speedup" => {
                done(self.set_macro_speedup(req_usize(op, "factor")?.min(65535) as u16)?)
            }
            "set_four_op_tracks" => {
                let mask = match opt_usize(op, "mask")? {
                    Some(m) => m.min(255) as u8,
                    None => {
                        let pair = req_usize(op, "pair")?;
                        if pair > 5 {
                            return Err("a 4-op track pair is 0-5".into());
                        }
                        let on = opt_bool(op, "on")?.ok_or("'on' is true or false")?;
                        let m = self.song.four_op_tracks;
                        if on {
                            m | 1 << pair
                        } else {
                            m & !(1 << pair)
                        }
                    }
                };
                done(self.set_four_op_tracks(mask)?)
            }
            "set_lock_flags" => {
                done(self.set_lock_flags(req_usize(op, "track")?, byte(op, "value")?)?)
            }
            "set_rows_per_beat" => done(self.set_rows_per_beat(byte(op, "rows")?)?),
            "set_tempo_finetune" => {
                let n = req_i64(op, "finetune")?;
                if !(-32768..=32767).contains(&n) {
                    return Err("the tempo fine-tune is a 16-bit number".into());
                }
                done(self.set_tempo_finetune(n as i16)?)
            }
            "set_four_op_instrument" => done(self.set_four_op_instrument(
                req_usize(op, "pair")?,
                opt_bool(op, "on")?.ok_or("'on' is true or false")?,
            )?),
            // ---- instruments
            "set_instrument" => {
                let (n, c) = self
                    .set_instrument(req_usize(op, "instrument")?, &InstrumentEdit::from_op(op)?)?;
                with(json!({"name": n}), c)
            }
            "set_operator" => {
                let carrier = match req_str(op, "operator")? {
                    "modulator" => false,
                    "carrier" => true,
                    other => {
                        return Err(format!("'{other}' is not an operator (modulator, carrier)"))
                    }
                };
                done(self.set_operator(
                    req_usize(op, "instrument")?,
                    carrier,
                    &operator_edit(op)?,
                )?)
            }
            "swap_operators" => done(self.swap_operators(req_usize(op, "instrument")?)?),
            "reset_instrument" => done(self.reset_instrument(req_usize(op, "instrument")?)?),
            "clear_instrument" => done(self.clear_instrument(
                req_usize(op, "instrument")?,
                opt_bool(op, "keep_name")?.unwrap_or(false),
                opt_bool(op, "with_macro")?.unwrap_or(true),
            )?),
            "copy_instrument" => {
                done(self.copy_instrument(req_usize(op, "from")?, req_usize(op, "to")?)?)
            }
            "set_disabled_columns" => done(self.set_disabled_columns(
                req_usize(op, "instrument")?,
                instruments::column_flags(op)?,
            )?),
            // ---- macros
            "set_fm_macro" => {
                done(self.set_fm_macro(req_usize(op, "macro")?, &instruments::fm_macro_edit(op)?)?)
            }
            "set_arpeggio_macro" => done(
                self.set_arpeggio_macro(req_usize(op, "macro")?, &instruments::arpeggio_edit(op)?)?,
            ),
            "set_vibrato_macro" => done(
                self.set_vibrato_macro(req_usize(op, "macro")?, &instruments::vibrato_edit(op)?)?,
            ),
            "clear_macro" => done(self.clear_macro(macro_kind(op)?, req_usize(op, "macro")?)?),
            other => Err(format!("unknown operation '{other}'")),
        }
    }
}

// ---------------------------------------------------------------------------
// operation field readers

fn byte(op: &Value, key: &str) -> Res<u8> {
    opt_u8(op, key)?.ok_or_else(|| format!("'{key}' is needed (0-255)"))
}

fn text_arg<'a>(op: &'a Value, key: &str) -> Res<&'a str> {
    opt_str(op, key)?
        .or(opt_str(op, "text")?)
        .ok_or_else(|| format!("'{key}' is needed (text)"))
}

fn macro_kind(op: &Value) -> Res<MacroKind> {
    let k = req_str(op, "kind")?;
    MacroKind::parse(k).ok_or_else(|| format!("'{k}' is not a macro kind (fm, arpeggio, vibrato)"))
}
