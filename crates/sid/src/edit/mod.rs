//! Editing a SID song as data.
//!
//! A [`SidDoc`] holds a [`SidSong`] (GoatTracker's model: three orderlists per
//! subsong over a shared pool of patterns, instruments that point into four
//! shared step tables) and changes it the way GoatTracker's editor does,
//! checking every change against GoatTracker's own limits: 1-128 rows in a
//! pattern, 208 patterns, 63 instruments, 255 rows in a table, 254 entries in
//! an orderlist, 32 subsongs, repeats of 1-16, commands 0-F. A change that does
//! not fit is refused with a message and leaves the song as it was.
//!
//! Everything is available as typed methods and as one JSON entry point,
//! [`SidDoc::apply`], for hosts that talk JSON (the Lua editor does):
//! `{"op": "set_cell", "pattern": 0, "row": 3, "note": "C-4", "instrument": 1}`
//! returns `{"changed": {...}}` (what to redraw) and any result fields.
//!
//! # Numbers
//!
//! Patterns, rows, voices (0-2), subsongs, orderlist entries and table rows
//! count from 0. Instruments count from 1, as a pattern row names them (0 is
//! none). **Table pointers are GoatTracker's: 1-based, 0 meaning none**, in the
//! instruments' `wave_ptr`/`pulse_ptr`/`filter_ptr`/`speed_ptr`, in pattern
//! commands 8/9/A/1-4/E, in wave-table commands and in `FF xx` jumps, so table
//! row 0 is what a pointer calls 1. Ranges (`row0`..`row1`) include both ends.
//!
//! A pattern row ("cell") is the array `[note, instrument, command, param]`:
//!
//! - `note`: 0 none, 1-93 C-0 to G#7 (the note table index plus one), 126 key
//!   off, 127 key on. A MIDI number is `note + 11` (`info().note.midi_offset`);
//!   operations also take the name as text (`"C-4"`, `"off"`, `"on"`) or `midi`.
//! - `instrument`: 0 or 1-63, an instrument the song has.
//! - `command`: 0-15 (`info().commands` says what each does).
//! - `param`: 0-255.
//!
//! # Undo
//!
//! Every edit is undoable: [`SidDoc::undo`] and [`SidDoc::redo`] step through
//! the log and return what changed. Edits made between [`SidDoc::begin_group`]
//! and [`SidDoc::end_group`] are one step. The log records only what each edit
//! overwrites (see `history.rs`), so it is small, and it is bounded in steps
//! and bytes.

mod cells;
mod history;
mod instruments;
mod json;
mod limits;
mod order;

use serde_json::{json, Value};

pub use cells::{ClearMask, Clip, PasteMode, Pasted};
pub use history::{Changes, MAX_CELLS};
pub use instruments::InstrumentUpdate;
pub use json::{row_json, table_row_json, CellUpdate};
pub use limits::{
    limits_json, midi_to_note, note_name, parse_note, row_problem, COMMANDS, MAX_TABLE_ROWS,
    MIDI_OFFSET, SNG_TEMPO,
};
pub use order::OrderUpdate;

use self::history::{Change, HeaderState, History};
use self::json::{
    field, latin1, opt_bool, opt_i64, opt_str, opt_u8, opt_usize, req_i64, req_str, req_table,
    req_usize, to_latin1, Res,
};
use crate::goattracker::{self, GtExport, GtSongHints};
use crate::song::{
    Instrument, LiveEdit, OrderEntry, Orderlist, Pattern, Row, SidSong, Subsong, TableKind,
    TableRow, Tables, MAX_PATTERN_ROWS, MAX_SPEED_MULTIPLIER, MAX_TEMPO, MAX_TEXT, SID_CHANNELS,
    SONG_FILE_VERSION,
};
use crate::SidModel;

/// A song being edited. See the module docs.
#[derive(Debug)]
pub struct SidDoc {
    song: SidSong,
    /// The subsong the editor shows (and the default of the orderlist
    /// operations): UI state, not part of the song, not undone.
    subsong: usize,
    history: History,
    revision: u64,
    /// What the last edit, undo or redo changed.
    last: Changes,
}

/// What a new song starts with.
#[derive(Clone, Debug)]
pub struct NewSong {
    pub title: String,
    pub author: String,
    pub model: SidModel,
    /// 1-16 (GoatTracker's `-S`).
    pub speed_multiplier: u8,
    /// Rows of each voice's first pattern, 1-128.
    pub pattern_rows: usize,
    /// Frames per row at the song's multispeed, as GoatTracker's F command
    /// counts them (3-127, longer than the new instrument's gate timer).
    /// `None` is GoatTracker's own start, 6 per 1x.
    pub tempo: Option<u8>,
}

impl Default for NewSong {
    fn default() -> NewSong {
        NewSong {
            title: String::new(),
            author: String::new(),
            model: SidModel::Sid6581,
            speed_multiplier: 1,
            pattern_rows: 64,
            tempo: None,
        }
    }
}

/// GoatTracker's new instrument (ginstr.c:214-220): first frame `$09` (test and
/// gate, which restarts the oscillator), gate timer 2 per 1x with hard
/// restart, a short pluck, no tables.
pub fn default_instrument(speed_multiplier: u8) -> Instrument {
    Instrument {
        name: Vec::new(),
        attack: 0,
        decay: 9,
        sustain: 0,
        release: 0,
        first_wave: 0x09,
        gate_timer: (2 * speed_multiplier as u32).min(63) as u8,
        hard_restart: true,
        no_gate_off: false,
        vibrato_delay: 0,
        wave_ptr: 0,
        pulse_ptr: 0,
        filter_ptr: 0,
        speed_ptr: 0,
    }
}

/// The wave rows a new instrument starts with: `41 00` (pulse and gate at the
/// played note), `FF 00` (stop). With its first frame `$09` that is
/// GoatTracker's plain pulse.
pub const NEW_WAVE_ROWS: [TableRow; 2] = [
    TableRow {
        left: 0x41,
        right: 0x00,
    },
    TableRow {
        left: 0xFF,
        right: 0x00,
    },
];
/// The pulse rows a new instrument starts with: `88 00` (50 %), `FF 00`.
pub const NEW_PULSE_ROWS: [TableRow; 2] = [
    TableRow {
        left: 0x88,
        right: 0x00,
    },
    TableRow {
        left: 0xFF,
        right: 0x00,
    },
];

impl SidDoc {
    // ----- making and reading ------------------------------------------------

    /// A new song: one subsong whose three orderlists each play their own blank
    /// 64-row pattern, and instrument 1, GoatTracker's new instrument on its own
    /// wave and pulse rows (a plain pulse).
    pub fn new() -> SidDoc {
        SidDoc::with_options(&NewSong::default()).expect("the default song is valid")
    }

    /// [`new`](Self::new) with options. A tempo other than GoatTracker's own is
    /// an `F` command on row 0 of voice 1's pattern (the song's start tempo stays
    /// 6: it is the one a `.sng` can say), as GoatTracker's own songs set it.
    pub fn with_options(o: &NewSong) -> Res<SidDoc> {
        if !(1..=MAX_PATTERN_ROWS).contains(&o.pattern_rows) {
            return Err(format!("a pattern has 1-{MAX_PATTERN_ROWS} rows"));
        }
        if !(1..=MAX_SPEED_MULTIPLIER).contains(&o.speed_multiplier) {
            return Err(format!(
                "the speed multiplier is not 1-{MAX_SPEED_MULTIPLIER}"
            ));
        }
        let tempo = o
            .tempo
            .unwrap_or((SNG_TEMPO as u32 * o.speed_multiplier as u32).min(MAX_TEMPO as u32) as u8);
        if !(3..=MAX_TEMPO).contains(&tempo) {
            return Err(format!(
                "the tempo is not 3-{MAX_TEMPO} (GoatTracker's F command)"
            ));
        }
        let ins = default_instrument(o.speed_multiplier);
        if tempo <= ins.gate_timer {
            return Err(format!(
                "at {}x the tempo is at least {}: the instrument's gate timer is {} frames, and GoatTracker stops a song whose rows are not longer than that",
                o.speed_multiplier,
                ins.gate_timer + 1,
                ins.gate_timer
            ));
        }
        let mut patterns: Vec<Pattern> = (0..SID_CHANNELS)
            .map(|_| Pattern {
                rows: vec![Row::default(); o.pattern_rows],
            })
            .collect();
        if tempo != SNG_TEMPO || o.speed_multiplier != 1 {
            patterns[0].rows[0] = Row {
                command: 0xF,
                param: tempo,
                ..Row::default()
            };
        }
        let orderlists = (0..SID_CHANNELS)
            .map(|c| Orderlist {
                entries: vec![OrderEntry {
                    pattern: c as u8,
                    transpose: 0,
                    repeat: 1,
                }],
                restart: 0,
            })
            .collect();
        let song = SidSong {
            version: SONG_FILE_VERSION,
            model: o.model,
            channels: SID_CHANNELS as u8,
            speed_multiplier: o.speed_multiplier,
            tempo: SNG_TEMPO,
            name: to_latin1(&o.title, "the title")?,
            author: to_latin1(&o.author, "the author")?,
            copyright: Vec::new(),
            subsongs: vec![Subsong { orderlists }],
            patterns,
            instruments: vec![Instrument {
                wave_ptr: 1,
                pulse_ptr: 1,
                ..ins
            }],
            tables: Tables {
                wave: NEW_WAVE_ROWS.to_vec(),
                pulse: NEW_PULSE_ROWS.to_vec(),
                filter: Vec::new(),
                speed: Vec::new(),
            },
        };
        SidDoc::from_song(song)
    }

    /// Edits a song the caller built or parsed; it must be one the file layout
    /// can hold (`SidSong::validate`).
    pub fn from_song(song: SidSong) -> Res<SidDoc> {
        song.validate().map_err(|e| e.to_string())?;
        Ok(SidDoc {
            song,
            subsong: 0,
            history: History::new(),
            revision: 0,
            last: Changes::default(),
        })
    }

    /// Reads a `.asid` file (the DAW's SID song) or a GoatTracker `.sng` (GTS5 or
    /// GTS!), by its magic bytes. A `.sng` has no chip model or speed: it gets
    /// the 6581 at 1x (see [`from_sng`](Self::from_sng)).
    pub fn from_bytes(bytes: &[u8]) -> Res<SidDoc> {
        SidDoc::from_sng(bytes, GtSongHints::default())
    }

    /// [`from_bytes`](Self::from_bytes) with what a `.sng` cannot say (GoatTracker's
    /// `-E` and `-S`; `goattracker::hints_from_name` reads them from a file name).
    pub fn from_sng(bytes: &[u8], hints: GtSongHints) -> Res<SidDoc> {
        if bytes.starts_with(&crate::song::MAGIC) {
            return SidDoc::from_song(SidSong::parse(bytes).map_err(|e| e.to_string())?);
        }
        if goattracker::looks_like_gt_song(bytes) {
            let import = goattracker::import(bytes, hints).map_err(|e| e.to_string())?;
            return SidDoc::from_song(import.song);
        }
        Err("it is not a SID song (no ASID, GTS5 or GTS! identification string)".into())
    }

    /// The song as the DAW's `.asid` file.
    pub fn to_asid_bytes(&self) -> Vec<u8> {
        self.song.to_bytes()
    }

    /// The song as a GoatTracker `.sng` (GTS5): the bytes, or why it cannot be
    /// one. See [`goattracker::export`] for what is written and what is lost.
    pub fn to_sng_bytes(&self) -> Res<Vec<u8>> {
        Ok(self.export_sng()?.bytes)
    }

    /// [`to_sng_bytes`](Self::to_sng_bytes) with what the file cannot say.
    pub fn export_sng(&self) -> Res<GtExport> {
        goattracker::export(&self.song).map_err(|e| e.to_string())
    }

    /// The song, for playing or inspecting.
    pub fn song(&self) -> &SidSong {
        &self.song
    }

    /// The song, taken out.
    pub fn into_song(self) -> SidSong {
        self.song
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

    /// The subsong the editor shows.
    pub fn subsong(&self) -> usize {
        self.subsong
    }

    /// Chooses the subsong the editor shows (and the orderlist operations
    /// default to). Not an edit: it is not undone.
    pub fn select_subsong(&mut self, subsong: usize) -> Res<()> {
        if subsong >= self.song.subsongs.len() {
            return Err(format!(
                "subsong {subsong} does not exist (the song has {})",
                self.song.subsongs.len()
            ));
        }
        self.subsong = subsong;
        Ok(())
    }

    fn clamp_subsong(&mut self) {
        self.subsong = self.subsong.min(self.song.subsongs.len() - 1);
    }

    // ----- live edits ------------------------------------------------------------

    /// The last change as live edits, for a song that is playing this one:
    /// `Some` when the change was nothing a player has to be rebuilt for, to
    /// give to `SidSongPlayer::apply_live` one by one: pattern rows (typing,
    /// clear, paste, transpose, insert or delete rows that keep the pattern's
    /// length), the fields of instruments, the content of table rows, the chip
    /// model. `None` for anything that moves the song's shape: a pattern's length
    /// or the pattern count, an orderlist or a subsong, the instrument list, a
    /// table's length (so a table row inserted or deleted, and an instrument
    /// added with its own rows), the tempo or the speed multiplier, or more than
    /// [`MAX_CELLS`] rows: open the edited song as a new player, seek it to the
    /// old one's position (`SidSongPlayer::status`) and swap it in.
    ///
    /// Titles are not played: a change of them alone is `Some` of nothing.
    pub fn live_edits(&self) -> Option<Vec<LiveEdit>> {
        self.live_edits_of(&self.last)
    }

    /// [`live_edits`](Self::live_edits) for a [`Changes`] this song's edits returned.
    pub fn live_edits_of(&self, changes: &Changes) -> Option<Vec<LiveEdit>> {
        if !changes.is_live() {
            return None;
        }
        let s = &self.song;
        let mut out = Vec::new();
        for &(p, r) in &changes.cells {
            let cell = *s.patterns.get(p)?.rows.get(r)?;
            out.push(LiveEdit::Row {
                pattern: p as u8,
                row: r as u8,
                cell,
            });
        }
        for &n in &changes.instruments {
            let mut instrument = s.instruments.get(n - 1)?.clone();
            instrument.name = Vec::new();
            out.push(LiveEdit::Instrument {
                number: n as u8,
                instrument,
            });
        }
        for (&table, &(a, b)) in &changes.tables {
            let rows = s.tables.get(table);
            for row in a..=b {
                out.push(LiveEdit::TableRow {
                    table,
                    row: row as u8,
                    value: *rows.get(row)?,
                });
            }
        }
        if changes.model {
            out.push(LiveEdit::Model(s.model));
        }
        (out.len() <= MAX_CELLS).then_some(out)
    }

    /// What is wrong with the song as a file: what the writers refuse, and
    /// (`warnings`) what plays but is probably a mistake: a command naming a
    /// table row past the table's end, a volume above F, an instrument that points
    /// past a table. Both lists empty is a song that saves and plays as meant.
    pub fn validate(&self) -> (Vec<String>, Vec<String>) {
        let mut problems = Vec::new();
        if let Err(e) = self.song.validate() {
            problems.push(format!("breaks a rule of the SID model: {e}"));
        }
        if let Some(why) = goattracker::export_problem(&self.song) {
            problems.push(format!("cannot be written as a .sng: {why}"));
        }
        (problems, self.warnings())
    }

    fn warnings(&self) -> Vec<String> {
        let s = &self.song;
        let mut out = Vec::new();
        let len = |k: TableKind| s.tables.get(k).len();
        'outer: for (p, pattern) in s.patterns.iter().enumerate() {
            for (r, row) in pattern.rows.iter().enumerate() {
                let (table, what) = match row.command {
                    0x8 => (Some(TableKind::Wave), "wave"),
                    0x9 => (Some(TableKind::Pulse), "pulse"),
                    0xA => (Some(TableKind::Filter), "filter"),
                    0x1..=0x4 | 0xE => (Some(TableKind::Speed), "speed"),
                    _ => (None, ""),
                };
                let at = format!("pattern {p} row {r}");
                if let Some(t) = table {
                    if row.param as usize > len(t) {
                        out.push(format!(
                            "{at}: command {:X} names {what} table row {}, past the table's {} rows",
                            row.command,
                            row.param,
                            len(t)
                        ));
                    }
                }
                if row.command == 0xD && row.param >= 0x10 {
                    out.push(format!(
                        "{at}: volume {:02X} is above F; GoatTracker ignores it",
                        row.param
                    ));
                }
                if row.command == 0xF && matches!(row.param, 0x02 | 0x82) {
                    out.push(format!(
                        "{at}: tempo {:02X} is below 3 (00 and 01 recall the funk tempo)",
                        row.param
                    ));
                }
                if out.len() >= 20 {
                    break 'outer;
                }
            }
        }
        out
    }

    // ----- the edit machinery -------------------------------------------------------

    fn record(&mut self, c: Change) {
        let inverse = c.apply(&mut self.song);
        self.history.open.push(inverse);
    }

    pub(crate) fn put_rows(&mut self, pattern: usize, row0: usize, rows: Vec<Row>) {
        if self.song.patterns[pattern].rows[row0..row0 + rows.len()] != rows[..] {
            self.record(Change::Rows {
                pattern,
                row0,
                rows,
            });
        }
    }

    pub(crate) fn put_pattern(&mut self, index: usize, pattern: Pattern) {
        if self.song.patterns[index] != pattern {
            self.record(Change::Pattern { index, pattern });
        }
    }

    pub(crate) fn insert_pattern_at(&mut self, index: usize, pattern: Pattern) {
        self.record(Change::PatternInsert { index, pattern });
    }

    pub(crate) fn remove_pattern_at(&mut self, index: usize) {
        self.record(Change::PatternRemove { index });
    }

    pub(crate) fn put_orderlist(&mut self, subsong: usize, voice: usize, list: Orderlist) {
        if self.song.subsongs[subsong].orderlists[voice] != list {
            self.record(Change::Order {
                subsong,
                voice,
                list,
            });
        }
    }

    pub(crate) fn insert_subsong_at(&mut self, index: usize, subsong: Subsong) {
        self.record(Change::SubsongInsert { index, subsong });
    }

    pub(crate) fn remove_subsong_at(&mut self, index: usize) {
        self.record(Change::SubsongRemove { index });
    }

    pub(crate) fn put_instrument(&mut self, index: usize, instrument: Instrument) {
        if self.song.instruments[index] != instrument {
            self.record(Change::Instrument { index, instrument });
        }
    }

    pub(crate) fn insert_instrument_at(&mut self, index: usize, instrument: Instrument) {
        self.record(Change::InstrumentInsert { index, instrument });
    }

    pub(crate) fn remove_instrument_at(&mut self, index: usize) {
        self.record(Change::InstrumentRemove { index });
    }

    pub(crate) fn put_table(&mut self, kind: TableKind, rows: Vec<TableRow>) {
        if *self.song.tables.get(kind) != rows {
            self.record(Change::Table { kind, rows });
        }
    }

    pub(crate) fn put_header(&mut self, h: HeaderState) {
        let now = HeaderState::of(&self.song);
        if now.name != h.name
            || now.author != h.author
            || now.copyright != h.copyright
            || now.model != h.model
            || now.speed_multiplier != h.speed_multiplier
            || now.tempo != h.tempo
        {
            self.record(Change::Header(Box::new(h)));
        }
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
        self.clamp_subsong();
        out
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
        self.clamp_subsong();
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
        self.clamp_subsong();
        self.last = changes.clone();
        Some(changes)
    }

    // ----- reads ----------------------------------------------------------------------

    /// The song's summary: `{"title", "author", "copyright", "model" ("6581" or
    /// "8580"), "channels": 3, "speed_multiplier", "tempo", "subsongs" (count),
    /// "subsong" (the one shown), "patterns", "instruments", "tables": {"wave",
    /// "pulse", "filter", "speed"} (row counts), "note": {"min", "max", "off",
    /// "on", "midi_offset"}, "limits", "commands": [{"name", "param"}],
    /// "revision", "can_undo", "can_redo", "undo_steps", "redo_steps",
    /// "history_bytes"}`.
    pub fn info(&self) -> Value {
        let s = &self.song;
        json!({
            "title": latin1(&s.name),
            "author": latin1(&s.author),
            "copyright": latin1(&s.copyright),
            "model": model_name(s.model),
            "channels": SID_CHANNELS,
            "speed_multiplier": s.speed_multiplier,
            "tempo": s.tempo,
            "subsongs": s.subsongs.len(),
            "subsong": self.subsong,
            "patterns": s.patterns.len(),
            "instruments": s.instruments.len(),
            "tables": {
                "wave": s.tables.wave.len(),
                "pulse": s.tables.pulse.len(),
                "filter": s.tables.filter.len(),
                "speed": s.tables.speed.len(),
            },
            "note": {
                "min": crate::song::NOTE_FIRST,
                "max": crate::song::NOTE_LAST,
                "off": crate::song::NOTE_KEY_OFF,
                "on": crate::song::NOTE_KEY_ON,
                "midi_offset": MIDI_OFFSET,
            },
            "limits": limits_json(),
            "commands": COMMANDS.iter().map(|(n, p)| json!({"name": n, "param": p})).collect::<Vec<_>>(),
            "revision": self.revision,
            "can_undo": self.can_undo(),
            "can_redo": self.can_redo(),
            "undo_steps": self.history.undo_steps(),
            "redo_steps": self.history.redo_steps(),
            "history_bytes": self.history.bytes(),
        })
    }

    /// The rows of `count` rows from `row0` of a pattern, for drawing:
    /// `{"pattern", "row0", "count", "rows" (the pattern's length), "cells":
    /// [[note, instrument, command, param] per row]}`. Rows past the end are
    /// left out. With `flat`, `cells` is one array of `count * 4` numbers.
    pub fn rows(&self, pattern: usize, row0: usize, count: usize, flat: bool) -> Res<Value> {
        let p = self.pattern_ref(pattern)?;
        let end = row0.saturating_add(count).min(p.rows.len());
        let row0 = row0.min(end);
        let cells = if flat {
            json!(p.rows[row0..end]
                .iter()
                .flat_map(|r| [r.note, r.instrument, r.command, r.param])
                .collect::<Vec<_>>())
        } else {
            Value::Array(p.rows[row0..end].iter().map(row_json).collect())
        };
        Ok(json!({
            "pattern": pattern,
            "row0": row0,
            "count": end - row0,
            "rows": p.rows.len(),
            "cells": cells,
        }))
    }

    /// Every pattern: `{"patterns": [{"pattern", "rows", "used"}]}` where `used`
    /// counts the orderlist entries (all subsongs, all voices) that play it.
    pub fn patterns(&self) -> Value {
        let mut used = vec![0usize; self.song.patterns.len()];
        for sub in &self.song.subsongs {
            for list in &sub.orderlists {
                for e in &list.entries {
                    used[e.pattern as usize] += 1;
                }
            }
        }
        json!({"patterns": self.song.patterns.iter().enumerate().map(|(i, p)| {
            json!({"pattern": i, "rows": p.rows.len(), "used": used[i]})
        }).collect::<Vec<_>>()})
    }

    /// One voice's orderlist: `{"subsong", "voice", "entries": [[pattern,
    /// transpose, repeat]], "restart", "rows": [the length of each entry's
    /// pattern times its repeat]}`.
    pub fn orderlist(&self, subsong: usize, voice: usize) -> Res<Value> {
        let list = self.list_ref(subsong, voice)?;
        Ok(json!({
            "subsong": subsong,
            "voice": voice,
            "entries": list.entries.iter().map(|e| json!([e.pattern, e.transpose, e.repeat])).collect::<Vec<_>>(),
            "restart": list.restart,
            "rows": list.entries.iter().map(|e| self.song.patterns[e.pattern as usize].rows.len() * e.repeat as usize).collect::<Vec<_>>(),
        }))
    }

    /// The three orderlists of a subsong: `{"subsong", "voices": [..]}`.
    pub fn orderlists(&self, subsong: usize) -> Res<Value> {
        let voices = (0..SID_CHANNELS)
            .map(|v| self.orderlist(subsong, v))
            .collect::<Res<Vec<_>>>()?;
        Ok(json!({"subsong": subsong, "voices": voices}))
    }

    /// Every instrument, as `instrument(n)` gives it, in number order.
    pub fn instruments(&self) -> Value {
        let uses = self.instrument_uses();
        json!({"instruments": (1..=self.song.instruments.len()).map(|n| self.instrument_json(n, uses[n - 1])).collect::<Vec<_>>()})
    }

    /// Instrument `n` (1-based): `{"instrument", "name", "attack", "decay",
    /// "sustain", "release", "ad", "sr", "first_wave", "gate_timer",
    /// "hard_restart", "no_gate_off", "vibrato_delay", "wave_ptr", "pulse_ptr",
    /// "filter_ptr", "speed_ptr", "uses"}`; `uses` counts the pattern rows that
    /// name it.
    pub fn instrument(&self, n: usize) -> Res<Value> {
        self.instrument_ref(n)?;
        Ok(self.instrument_json(n, self.instrument_uses()[n - 1]))
    }

    fn instrument_uses(&self) -> Vec<usize> {
        let mut uses = vec![0usize; self.song.instruments.len()];
        for p in &self.song.patterns {
            for r in &p.rows {
                if r.instrument > 0 {
                    uses[r.instrument as usize - 1] += 1;
                }
            }
        }
        uses
    }

    fn instrument_json(&self, n: usize, uses: usize) -> Value {
        let i = &self.song.instruments[n - 1];
        json!({
            "instrument": n,
            "name": latin1(&i.name),
            "attack": i.attack,
            "decay": i.decay,
            "sustain": i.sustain,
            "release": i.release,
            "ad": i.ad(),
            "sr": i.sr(),
            "first_wave": i.first_wave,
            "gate_timer": i.gate_timer,
            "hard_restart": i.hard_restart,
            "no_gate_off": i.no_gate_off,
            "vibrato_delay": i.vibrato_delay,
            "wave_ptr": i.wave_ptr,
            "pulse_ptr": i.pulse_ptr,
            "filter_ptr": i.filter_ptr,
            "speed_ptr": i.speed_ptr,
            "uses": uses,
        })
    }

    /// All four tables: `{"wave": [[left, right]], "pulse": .., "filter": ..,
    /// "speed": ..}`. The tables are the song's, shared by its subsongs.
    pub fn tables(&self) -> Value {
        let mut v = json!({});
        for k in TableKind::ALL {
            v[k.name()] =
                Value::Array(self.song.tables.get(k).iter().map(table_row_json).collect());
        }
        v
    }

    /// Rows `row0..row0 + count` (0-based) of a table: `{"table", "row0",
    /// "count", "rows" (the table's length), "cells": [[left, right]]}`. A
    /// pointer calls row 0 "1".
    pub fn table(&self, kind: TableKind, row0: usize, count: usize) -> Value {
        let t = self.song.tables.get(kind);
        let end = row0.saturating_add(count).min(t.len());
        let row0 = row0.min(end);
        json!({
            "table": kind.name(),
            "row0": row0,
            "count": end - row0,
            "rows": t.len(),
            "cells": t[row0..end].iter().map(table_row_json).collect::<Vec<_>>(),
        })
    }

    pub(crate) fn pattern_ref(&self, pattern: usize) -> Res<&Pattern> {
        self.song.patterns.get(pattern).ok_or_else(|| {
            format!(
                "pattern {pattern} does not exist (the song has {})",
                self.song.patterns.len()
            )
        })
    }

    pub(crate) fn list_ref(&self, subsong: usize, voice: usize) -> Res<&Orderlist> {
        let s = self.song.subsongs.get(subsong).ok_or_else(|| {
            format!(
                "subsong {subsong} does not exist (the song has {})",
                self.song.subsongs.len()
            )
        })?;
        s.orderlists
            .get(voice)
            .ok_or_else(|| format!("voice {voice} does not exist (voices are 0-2)"))
    }

    pub(crate) fn instrument_ref(&self, n: usize) -> Res<&Instrument> {
        n.checked_sub(1)
            .and_then(|i| self.song.instruments.get(i))
            .ok_or_else(|| {
                format!(
                    "instrument {n} does not exist (the song has {}; instruments count from 1)",
                    self.song.instruments.len()
                )
            })
    }

    // ----- header ---------------------------------------------------------------------

    /// Sets the title, author and copyright that are given (latin-1, at most 32
    /// characters each).
    pub fn set_texts(
        &mut self,
        title: Option<&str>,
        author: Option<&str>,
        copyright: Option<&str>,
    ) -> Res<Changes> {
        let mut h = HeaderState::of(&self.song);
        for (text, slot, what) in [
            (title, &mut h.name, "the title"),
            (author, &mut h.author, "the author"),
            (copyright, &mut h.copyright, "the copyright"),
        ] {
            if let Some(t) = text {
                let bytes = to_latin1(t, what)?;
                if bytes.len() > MAX_TEXT {
                    return Err(format!("{what} is longer than {MAX_TEXT} characters"));
                }
                *slot = bytes;
            }
        }
        self.edit(|d| {
            d.put_header(h);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Sets the chip model the song is written for.
    pub fn set_model(&mut self, model: SidModel) -> Res<Changes> {
        let mut h = HeaderState::of(&self.song);
        h.model = model;
        self.edit(|d| {
            d.put_header(h);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Sets the multispeed (1-16): the player ticks this many times per PAL frame.
    pub fn set_speed_multiplier(&mut self, multiplier: u8) -> Res<Changes> {
        if !(1..=MAX_SPEED_MULTIPLIER).contains(&multiplier) {
            return Err(format!(
                "the speed multiplier is not 1-{MAX_SPEED_MULTIPLIER}"
            ));
        }
        let mut h = HeaderState::of(&self.song);
        h.speed_multiplier = multiplier;
        self.edit(|d| {
            d.put_header(h);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Sets the start tempo, frames per row per 1x (1-127). A `.sng` has no tempo
    /// field: it can only say 6, so any other tempo has to be written as an `F`
    /// command ([`bake_tempo`](Self::bake_tempo)) before the song is a `.sng`.
    pub fn set_tempo(&mut self, tempo: u8) -> Res<Changes> {
        if !(1..=MAX_TEMPO).contains(&tempo) {
            return Err(format!("the tempo is not 1-{MAX_TEMPO}"));
        }
        let mut h = HeaderState::of(&self.song);
        h.tempo = tempo;
        self.edit(|d| {
            d.put_header(h);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Moves the start tempo into the song, where a `.sng` can say it: the tempo
    /// becomes GoatTracker's 6 and row 0 of voice 1's first pattern gets an `F`
    /// command with the frames per row the song started at (`tempo` times the
    /// speed multiplier, as the player starts it). Refused when that row already
    /// has a command or the tempo does not fit an F command (3-127). The song
    /// plays as it did.
    pub fn bake_tempo(&mut self) -> Res<Changes> {
        let tempo = self.song.tempo;
        if tempo == SNG_TEMPO {
            return Ok(Changes::default());
        }
        let frames = tempo as u32 * self.song.speed_multiplier as u32;
        if !(3..=MAX_TEMPO as u32).contains(&frames) {
            return Err(format!(
                "{tempo} x {} = {frames} frames per row does not fit an F command (3-{MAX_TEMPO})",
                self.song.speed_multiplier
            ));
        }
        let first = self.song.subsongs[self.subsong].orderlists[0].entries[0].pattern as usize;
        let row = self.song.patterns[first].rows[0];
        if row.command != 0 || row.param != 0 {
            return Err(format!(
                "row 0 of pattern {first} (voice 1's first) already has the command {:X}{:02X}: clear it first, then the tempo can go there",
                row.command, row.param
            ));
        }
        let mut h = HeaderState::of(&self.song);
        h.tempo = SNG_TEMPO;
        self.edit(|d| {
            d.put_rows(
                first,
                0,
                vec![Row {
                    command: 0xF,
                    param: frames as u8,
                    ..row
                }],
            );
            d.put_header(h);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    // ----- JSON ---------------------------------------------------------------------

    /// Runs one operation described as JSON and returns its result as JSON.
    /// Edits return `{"changed": {...}}` (see [`Changes::to_json`]) with their
    /// own fields; a failed edit returns the message and changes nothing.
    ///
    /// Reading:
    /// - `{"op": "info"}`, `{"op": "patterns"}`, `{"op": "instruments"}`,
    ///   `{"op": "tables"}`, `{"op": "validate"}` -> `{"problems", "warnings"}`
    /// - `{"op": "rows", "pattern", "row0", "count", "flat"}`
    /// - `{"op": "orderlist", "subsong", "voice"}`, `{"op": "orderlists",
    ///   "subsong"}` (the subsong defaults to the one shown)
    /// - `{"op": "instrument", "instrument"}`
    /// - `{"op": "table", "table", "row0", "count"}`
    /// - `{"op": "live_edits"}` -> `{"edits": [..] | null}` for the last edit
    ///   (`{"kind": "row", "pattern", "row", "cell"}`, `{"kind": "instrument",
    ///   "number", ..fields}`, `{"kind": "table_row", "table", "row", "left",
    ///   "right"}`, `{"kind": "model", "model"}`), or `null` when the last edit
    ///   needs a new player; see [`live_edits`](Self::live_edits)
    /// - `{"op": "copy_range", "pattern", "row0", "row1"}` -> `{"rows", "cells"}`
    /// - `{"op": "export_sng"}` -> `{"bytes": [..], "notes": [..]}` or the reason
    /// - `{"op": "note_name", "note"}` -> `{"name"}`, `{"op": "parse_note", "text"}`
    ///   -> `{"note"}`
    ///
    /// Cells and patterns (rows of a pattern: `[note, instrument, command, param]`):
    /// - `set_cell` (`pattern`, `row`, and `cell: [n,i,c,p]` or any of `note`,
    ///   `midi`, `instrument`, `command`, `param`) -> `{"cell"}`
    /// - `clear_range` (`pattern`, `row0`, `row1`, optional `columns: ["note",
    ///   "instrument", "command"]`)
    /// - `insert_row` / `delete_row` (`pattern`, `row`, `count`; rows pushed off
    ///   the end are lost and blanks come in, unless `grow` / `shrink` change the
    ///   pattern's length instead)
    /// - `transpose` (`pattern`, `row0`, `row1`, `semitones`, `clamp`,
    ///   `instrument`) -> `{"count"}`
    /// - `paste` (`pattern`, `row`, `cells`, `mode`: `"overwrite"` or `"mix"`) ->
    ///   `{"rows", "clipped"}`
    /// - `set_pattern_rows` (`pattern`, `rows`, `force`), `new_pattern` (`rows`)
    ///   -> `{"pattern"}`, `duplicate_pattern` -> `{"pattern"}`, `delete_pattern`
    ///   (`pattern`, `force`)
    ///
    /// Orderlists (`subsong` defaults to the one shown; `voice` is 0-2 or `"all"`):
    /// - `set_order` (`index`, any of `pattern`, `transpose`, `repeat`),
    ///   `insert_order` (`index`, `pattern`, `transpose`, `repeat`), `delete_order`
    ///   (`index`), `move_order` (`from`, `to`), `set_restart` (`restart`)
    /// - `add_subsong` (`copy_of`, `rows`) -> `{"subsong"}`, `remove_subsong`,
    ///   `select_subsong` (`subsong`)
    ///
    /// Instruments (`instrument` counts from 1):
    /// - `set_instrument` (`name`, `attack`, `decay`, `sustain`, `release`,
    ///   `first_wave`, `gate_timer`, `hard_restart`, `no_gate_off`,
    ///   `vibrato_delay`, `wave_ptr`, `pulse_ptr`, `filter_ptr`, `speed_ptr`),
    ///   `rename_instrument` (`name`), `clear_instrument`
    /// - `add_instrument` (`name`, `tables`: `false` for none of its own) ->
    ///   `{"instrument"}`, `clone_instrument` (`instrument`, optional `to`: an
    ///   instrument to overwrite) -> `{"instrument"}`, `delete_instrument`
    ///   (`instrument`, `clear_uses`)
    ///
    /// Tables (`table` is `"wave"`, `"pulse"`, `"filter"` or `"speed"`; `row`
    /// counts from 0):
    /// - `set_table_row` (`table`, `row`, `left`, `right`; `row` = the length
    ///   appends), `insert_table_row` (`table`, `row`, `left`, `right`; every
    ///   pointer, jump and command that names a later row follows its data),
    ///   `delete_table_row`, `clear_table_row`, `remove_unused_table_rows`
    ///
    /// Header: `set_title` (`title`), `set_author`, `set_copyright`, `set_texts`
    /// (any of the three), `set_model` (`model`: `"6581"` or `"8580"`),
    /// `set_speed_multiplier` (`multiplier`), `set_tempo` (`tempo`), `bake_tempo`.
    ///
    /// History: `begin_group`, `end_group`, `undo`, `redo` (each returns
    /// `{"changed"}` of what moved), `clear_history`, and `{"op": "batch", "ops":
    /// [...]}`, which runs the operations as one undo step and, if one fails,
    /// none of them.
    pub fn apply(&mut self, op: &Value) -> Res<Value> {
        let name = req_str(op, "op")?;
        let done = |changes: Changes| Ok(json!({"changed": changes.to_json()}));
        let subsong =
            |d: &SidDoc| -> Res<usize> { Ok(opt_usize(op, "subsong")?.unwrap_or(d.subsong)) };
        match name {
            // ---- reading
            "info" => Ok(self.info()),
            "patterns" => Ok(self.patterns()),
            "instruments" => Ok(self.instruments()),
            "tables" => Ok(self.tables()),
            "validate" => {
                let (problems, warnings) = self.validate();
                Ok(json!({"problems": problems, "warnings": warnings}))
            }
            "live_edits" => Ok(json!({
                "edits": self.live_edits().map(|v| v.iter().map(live_edit_json).collect::<Vec<_>>()),
            })),
            "rows" => self.rows(
                req_usize(op, "pattern")?,
                opt_usize(op, "row0")?.unwrap_or(0),
                opt_usize(op, "count")?.unwrap_or(usize::MAX),
                opt_bool(op, "flat")?.unwrap_or(false),
            ),
            "orderlist" => self.orderlist(subsong(self)?, req_usize(op, "voice")?),
            "orderlists" => self.orderlists(subsong(self)?),
            "instrument" => self.instrument(req_usize(op, "instrument")?),
            "table" => Ok(self.table(
                req_table(op)?,
                opt_usize(op, "row0")?.unwrap_or(0),
                opt_usize(op, "count")?.unwrap_or(usize::MAX),
            )),
            "copy_range" => {
                let clip = self.copy_range(
                    req_usize(op, "pattern")?,
                    req_usize(op, "row0")?,
                    req_usize(op, "row1")?,
                )?;
                Ok(clip.to_json())
            }
            "export_sng" => {
                let e = self.export_sng()?;
                Ok(json!({"bytes": e.bytes, "notes": e.notes}))
            }
            "note_name" => {
                let n = opt_u8(op, "note")?.ok_or("'note' is needed (0-255)")?;
                Ok(json!({"name": note_name(n)}))
            }
            "parse_note" => {
                let t = req_str(op, "text")?;
                Ok(json!({"note": parse_note(t).ok_or_else(|| format!("'{t}' is not a note"))?}))
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
            "clear_history" => {
                self.clear_history();
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
                let (results, merged) = self.edit(|d| {
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
                Ok(json!({"results": results, "changed": merged.to_json()}))
            }
            "select_subsong" => {
                self.select_subsong(req_usize(op, "subsong")?)?;
                Ok(json!({"subsong": self.subsong}))
            }
            // ---- header
            "set_title" | "set_author" | "set_copyright" => {
                let text = opt_str(op, "text")?
                    .or(opt_str(op, &name[4..])?)
                    .ok_or_else(|| format!("'{}' is needed (text)", &name[4..]))?;
                let c = match name {
                    "set_title" => self.set_texts(Some(text), None, None)?,
                    "set_author" => self.set_texts(None, Some(text), None)?,
                    _ => self.set_texts(None, None, Some(text))?,
                };
                done(c)
            }
            "set_texts" => done(self.set_texts(
                opt_str(op, "title")?,
                opt_str(op, "author")?,
                opt_str(op, "copyright")?,
            )?),
            "set_model" => {
                let m = req_str(op, "model")?;
                let model = match m {
                    "6581" => SidModel::Sid6581,
                    "8580" => SidModel::Sid8580,
                    other => return Err(format!("'{other}' is not a chip model (6581, 8580)")),
                };
                done(self.set_model(model)?)
            }
            "set_speed_multiplier" => {
                let m = req_i64(op, "multiplier")?;
                if !(1..=MAX_SPEED_MULTIPLIER as i64).contains(&m) {
                    return Err(format!(
                        "the speed multiplier is not 1-{MAX_SPEED_MULTIPLIER}"
                    ));
                }
                done(self.set_speed_multiplier(m as u8)?)
            }
            "set_tempo" => {
                let t = req_i64(op, "tempo")?;
                if !(1..=MAX_TEMPO as i64).contains(&t) {
                    return Err(format!("the tempo is not 1-{MAX_TEMPO}"));
                }
                done(self.set_tempo(t as u8)?)
            }
            "bake_tempo" => done(self.bake_tempo()?),
            _ => self.apply_edit(name, op),
        }
    }
}

fn model_name(m: SidModel) -> &'static str {
    match m {
        SidModel::Sid6581 => "6581",
        SidModel::Sid8580 => "8580",
    }
}

/// A live edit as JSON (what `{"op": "live_edits"}` returns).
pub fn live_edit_json(e: &LiveEdit) -> Value {
    match e {
        LiveEdit::Row { pattern, row, cell } => {
            json!({"kind": "row", "pattern": pattern, "row": row, "cell": row_json(cell)})
        }
        LiveEdit::Instrument {
            number,
            instrument: i,
        } => json!({
            "kind": "instrument", "number": number,
            "attack": i.attack, "decay": i.decay, "sustain": i.sustain, "release": i.release,
            "first_wave": i.first_wave, "gate_timer": i.gate_timer,
            "hard_restart": i.hard_restart, "no_gate_off": i.no_gate_off,
            "vibrato_delay": i.vibrato_delay, "wave_ptr": i.wave_ptr, "pulse_ptr": i.pulse_ptr,
            "filter_ptr": i.filter_ptr, "speed_ptr": i.speed_ptr,
        }),
        LiveEdit::TableRow { table, row, value } => json!({
            "kind": "table_row", "table": table.name(), "row": row,
            "left": value.left, "right": value.right,
        }),
        LiveEdit::Model(m) => json!({"kind": "model", "model": model_name(*m)}),
    }
}

impl Default for SidDoc {
    fn default() -> SidDoc {
        SidDoc::new()
    }
}

/// The voices an operation names: `"voice": 0-2`, or `"all"`.
fn voices_of(op: &Value) -> Res<Vec<usize>> {
    match field(op, "voice") {
        Some(Value::String(s)) if s == "all" => Ok((0..SID_CHANNELS).collect()),
        Some(v) => match v.as_u64() {
            Some(n) if (n as usize) < SID_CHANNELS => Ok(vec![n as usize]),
            _ => Err("'voice' is 0-2 or \"all\"".into()),
        },
        None => Err("'voice' is needed (0-2 or \"all\")".into()),
    }
}

/// The `pattern` of an orderlist operation for voice `v` (of `n` named): one
/// number for every voice, or (with several voices) a list with one each.
fn pattern_for(op: &Value, v: usize, n: usize) -> Res<Option<u8>> {
    match field(op, "pattern") {
        None => Ok(None),
        Some(Value::Array(a)) if a.len() == n => a[v.min(n - 1)]
            .as_u64()
            .filter(|&p| p <= 255)
            .map(|p| Some(p as u8))
            .ok_or_else(|| "'pattern' is a pattern number".into()),
        Some(Value::Array(_)) => Err(format!("'pattern' as a list needs {n} numbers")),
        Some(v) => v
            .as_u64()
            .filter(|&p| p <= 255)
            .map(|p| Some(p as u8))
            .ok_or_else(|| "'pattern' is a pattern number".into()),
    }
}

fn opt_i8(op: &Value, key: &str) -> Res<Option<i8>> {
    match opt_i64(op, key)? {
        None => Ok(None),
        Some(n) if (-128..=127).contains(&n) => Ok(Some(n as i8)),
        Some(n) => Err(format!("'{key}' is {n}, not -128..127")),
    }
}

impl SidDoc {
    /// The editing operations of [`apply`](Self::apply).
    fn apply_edit(&mut self, name: &str, op: &Value) -> Res<Value> {
        let done = |changes: Changes| Ok(json!({"changed": changes.to_json()}));
        let subsong = opt_usize(op, "subsong")?.unwrap_or(self.subsong);
        match name {
            // ---- cells and patterns
            "set_cell" => {
                let (row, changes) = self.set_cell(
                    req_usize(op, "pattern")?,
                    req_usize(op, "row")?,
                    CellUpdate::from_op(op)?,
                )?;
                Ok(json!({"cell": row_json(&row), "changed": changes.to_json()}))
            }
            "clear_range" => {
                let mask = match op.get("columns").and_then(Value::as_array) {
                    Some(list) => {
                        let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
                        ClearMask::from_names(&names)?
                    }
                    None => ClearMask::ALL,
                };
                done(self.clear_range(
                    req_usize(op, "pattern")?,
                    req_usize(op, "row0")?,
                    req_usize(op, "row1")?,
                    mask,
                )?)
            }
            "insert_row" => done(self.insert_rows(
                req_usize(op, "pattern")?,
                req_usize(op, "row")?,
                opt_usize(op, "count")?.unwrap_or(1),
                opt_bool(op, "grow")?.unwrap_or(false),
            )?),
            "delete_row" => done(self.delete_rows(
                req_usize(op, "pattern")?,
                req_usize(op, "row")?,
                opt_usize(op, "count")?.unwrap_or(1),
                opt_bool(op, "shrink")?.unwrap_or(false),
            )?),
            "transpose" => {
                let semitones = req_i64(op, "semitones")?.clamp(-255, 255) as i32;
                let (count, changes) = self.transpose(
                    req_usize(op, "pattern")?,
                    req_usize(op, "row0")?,
                    req_usize(op, "row1")?,
                    semitones,
                    opt_bool(op, "clamp")?.unwrap_or(false),
                    opt_u8(op, "instrument")?,
                )?;
                Ok(json!({"count": count, "changed": changes.to_json()}))
            }
            "paste" => {
                let clip = Clip::from_json(
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
                    &clip,
                    mode,
                )?;
                Ok(json!({"rows": p.rows, "clipped": p.clipped, "changed": changes.to_json()}))
            }
            "set_pattern_rows" => done(self.set_pattern_rows(
                req_usize(op, "pattern")?,
                req_usize(op, "rows")?,
                opt_bool(op, "force")?.unwrap_or(false),
            )?),
            "new_pattern" => {
                let (p, changes) = self.new_pattern(opt_usize(op, "rows")?.unwrap_or(64))?;
                Ok(json!({"pattern": p, "changed": changes.to_json()}))
            }
            "duplicate_pattern" => {
                let (p, changes) = self.duplicate_pattern(req_usize(op, "pattern")?)?;
                Ok(json!({"pattern": p, "changed": changes.to_json()}))
            }
            "delete_pattern" => done(self.delete_pattern(
                req_usize(op, "pattern")?,
                opt_bool(op, "force")?.unwrap_or(false),
            )?),
            // ---- orderlists
            "set_order" | "insert_order" | "delete_order" | "move_order" | "set_restart" => {
                let voices = voices_of(op)?;
                let n = voices.len();
                let ((), changes) = self.edit(|d| {
                    for (k, &v) in voices.iter().enumerate() {
                        match name {
                            "set_order" => {
                                d.set_order(
                                    subsong,
                                    v,
                                    req_usize(op, "index")?,
                                    OrderUpdate {
                                        pattern: pattern_for(op, k, n)?,
                                        transpose: opt_i8(op, "transpose")?,
                                        repeat: opt_u8(op, "repeat")?,
                                    },
                                )?;
                            }
                            "insert_order" => {
                                let pattern = pattern_for(op, k, n)?
                                    .ok_or("insert_order needs a 'pattern'")?;
                                d.insert_order(
                                    subsong,
                                    v,
                                    req_usize(op, "index")?,
                                    OrderEntry {
                                        pattern,
                                        transpose: opt_i8(op, "transpose")?.unwrap_or(0),
                                        repeat: opt_u8(op, "repeat")?.unwrap_or(1),
                                    },
                                )?;
                            }
                            "delete_order" => {
                                d.delete_order(subsong, v, req_usize(op, "index")?)?;
                            }
                            "move_order" => {
                                d.move_order(
                                    subsong,
                                    v,
                                    req_usize(op, "from")?,
                                    req_usize(op, "to")?,
                                )?;
                            }
                            _ => {
                                d.set_restart(subsong, v, req_usize(op, "restart")?)?;
                            }
                        }
                    }
                    Ok(())
                })?;
                done(changes)
            }
            "add_subsong" => {
                let (s, changes) = self.add_subsong(
                    opt_usize(op, "copy_of")?,
                    opt_usize(op, "rows")?.unwrap_or(64),
                )?;
                Ok(json!({"subsong": s, "changed": changes.to_json()}))
            }
            "remove_subsong" => done(self.remove_subsong(req_usize(op, "subsong")?)?),
            // ---- instruments
            "set_instrument" | "rename_instrument" => {
                let u = InstrumentUpdate {
                    name: opt_str(op, "name")?.map(str::to_string),
                    attack: opt_u8(op, "attack")?,
                    decay: opt_u8(op, "decay")?,
                    sustain: opt_u8(op, "sustain")?,
                    release: opt_u8(op, "release")?,
                    first_wave: opt_u8(op, "first_wave")?,
                    gate_timer: opt_u8(op, "gate_timer")?,
                    hard_restart: opt_bool(op, "hard_restart")?,
                    no_gate_off: opt_bool(op, "no_gate_off")?,
                    vibrato_delay: opt_u8(op, "vibrato_delay")?,
                    wave_ptr: opt_u8(op, "wave_ptr")?,
                    pulse_ptr: opt_u8(op, "pulse_ptr")?,
                    filter_ptr: opt_u8(op, "filter_ptr")?,
                    speed_ptr: opt_u8(op, "speed_ptr")?,
                };
                if name == "rename_instrument" && u.name.is_none() {
                    return Err("'name' is needed (text)".into());
                }
                done(self.set_instrument(req_usize(op, "instrument")?, &u)?)
            }
            "clear_instrument" => done(self.clear_instrument(req_usize(op, "instrument")?)?),
            "add_instrument" => {
                let (n, changes) = self.add_instrument(
                    opt_str(op, "name")?.unwrap_or(""),
                    opt_bool(op, "tables")?.unwrap_or(true),
                )?;
                Ok(json!({"instrument": n, "changed": changes.to_json()}))
            }
            "clone_instrument" => {
                let (n, changes) =
                    self.clone_instrument(req_usize(op, "instrument")?, opt_usize(op, "to")?)?;
                Ok(json!({"instrument": n, "changed": changes.to_json()}))
            }
            "delete_instrument" => done(self.delete_instrument(
                req_usize(op, "instrument")?,
                opt_bool(op, "clear_uses")?.unwrap_or(false),
            )?),
            // ---- tables
            "set_table_row" => done(self.set_table_row(
                req_table(op)?,
                req_usize(op, "row")?,
                opt_u8(op, "left")?,
                opt_u8(op, "right")?,
            )?),
            "insert_table_row" => done(self.insert_table_row(
                req_table(op)?,
                req_usize(op, "row")?,
                TableRow {
                    left: opt_u8(op, "left")?.unwrap_or(0),
                    right: opt_u8(op, "right")?.unwrap_or(0),
                },
            )?),
            "delete_table_row" => {
                done(self.delete_table_row(req_table(op)?, req_usize(op, "row")?)?)
            }
            "clear_table_row" => done(self.clear_table_row(req_table(op)?, req_usize(op, "row")?)?),
            "remove_unused_table_rows" => {
                let (n, changes) = self.remove_unused_table_rows()?;
                Ok(json!({"removed": n, "changed": changes.to_json()}))
            }
            other => Err(format!(
                "'{other}' is not an operation (see SidDoc::apply for the list)"
            )),
        }
    }
}
