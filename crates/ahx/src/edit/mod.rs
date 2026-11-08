//! Editing an AHX or HivelyTracker song as data.
//!
//! An [`AhxDoc`] holds a [`Song`] and changes it the way the trackers do,
//! checking every change against the format's own limits: a track has 1 to 64
//! rows, a song up to 1000 positions, 256 tracks and 63 instruments, a PList up
//! to 255 rows, notes C-1 to B-5 (and C-6 to D-6, which the engine plays as B-5),
//! effects 0-F, and the file's 16-bit string table offset caps its whole size.
//! A change that does not fit is refused with a message and leaves the song as
//! it was.
//!
//! Everything is available as typed methods and as one JSON entry point,
//! [`AhxDoc::apply`], for hosts that talk JSON (the Lua editor does):
//! `{"op": "set_step", "track": 3, "row": 5, "note": "C-3", "instrument": 2}`
//! returns `{"changed": {...}}` (what to redraw) and any result fields.
//!
//! # The song
//!
//! A song is a list of **positions**. Each position names, for every channel (4
//! in AHX, 4 to 16 in HVL), a **track** and a **transpose** (semitones, -128 to
//! 127, added to every note of the track at that position). A track is
//! `track_length` rows of steps, and tracks are shared: two positions can play
//! the same track, so editing it edits both (the way to give a cell a track of
//! its own is `make_unique`, or `"unique": true` on an edit). Track 0 is the
//! track every empty cell points at; while it is blank it never takes content (a
//! write to it is refused, and `unique` gives the cell a track first).
//! **Instruments** are numbered from 1 (0 is none) without gaps; each has an
//! envelope, a filter sweep, a square sweep, a vibrato and a **PList**, a small
//! program of rows that sets the pitch, the waveform and effects tick by tick.
//!
//! # Numbers
//!
//! Positions, tracks, rows, channels and PList rows count from 0; instruments
//! as a step names them from 1. Ranges (`row0`..`row1`) include both ends.
//!
//! A step is the array `[note, instrument, fx, param, fxb, fxb_param]`, always
//! six numbers (`fxb` and its parameter are HVL's second effect column and 0 in
//! an AHX song, which refuses anything else):
//!
//! - `note`: 0 none, 1 to 60 C-1 to B-5, 61 to 63 C-6 to D-6 (an HVL step
//!   cannot hold 63: that byte marks a blank step in the file). MIDI is
//!   `note + 23`; [`note_name`] and [`parse_note`] do the text, and operations
//!   take `"C-3"`, `"C#2"`, `"---"` as well as numbers.
//! - `instrument`: 0 to 63.
//! - `fx`, `fxb`: the effect command 0-F as a number; `param`, `fxb_param`: 0-255.
//!
//! A PList row is `[note, waveform, fixed, fx0, param0, fx1, param1]`: a note
//! 0-63 (relative to the played note unless `fixed`), a waveform 0 (keep) to 4
//! (triangle, sawtooth, square, noise), and two commands (AHX has 0-5, C and F;
//! HVL 0-F).
//!
//! # Undo
//!
//! Every edit is undoable: [`AhxDoc::undo`] and [`AhxDoc::redo`] step through
//! the log and return what changed. Edits made between [`AhxDoc::begin_group`]
//! and [`AhxDoc::end_group`] are one step. The log records only what each edit
//! overwrites (see `history.rs`), so it is small, and it is bounded in steps and
//! bytes.
//!
//! # Playing it
//!
//! [`AhxDoc::to_bytes`] writes the song (see [`crate::write`]) and
//! [`AhxDoc::song`] borrows it for [`AhxPlayer::from_song`](crate::player::AhxPlayer::from_song).
//! A song that is playing follows an edit without being opened again when
//! [`AhxDoc::live_edits`] says so (see `live.rs`).

mod history;
mod instruments;
mod json;
mod limits;
mod live;
mod positions;
mod steps;
mod visuals;

use serde_json::{json, Value};

pub use history::{Changes, MAX_STEPS};
pub use instruments::SweepKind;
pub use json::{
    base64_decode, base64_encode, instrument_json, plist_entry_from_json, plist_entry_json,
    step_json, PListUpdate, StepUpdate,
};
pub use limits::{
    check_instrument, check_plist_entry, check_step, midi_of, note_name, note_of_midi, parse_note,
    plist_param_max, MIDI_OFFSET, NOTE_FIELD_MAX, NOTE_MAX, NOTE_MIN, PLIST_WAVEFORM_MAX,
    VOLUME_MAX, WAVE_LENGTH_MAX,
};
pub use live::{LiveEdits, PositionEdit, StepEdit};
pub use positions::{InsertKind, PositionMap};
pub use steps::{At, ClearMask, PasteMode, Pasted, StepClip};

use self::history::{Change, History};
use self::json::{opt_bool, opt_i64, opt_str, opt_u8, opt_usize, req_i64, req_str, req_usize};
use self::limits::{format_name, Res};
use crate::format::{self, Instrument, Position, Song, SongFormat, Step};
use crate::write::{self, Origin};

/// The settings of a new song (see [`AhxDoc::create`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSong {
    pub format: SongFormat,
    /// HVL only: 4 to 16 (an AHX song has 4).
    pub channels: usize,
    /// Rows per track, 1 to 64.
    pub track_length: usize,
    /// 1 to 4: the engine ticks at `50 * speed_multiplier` Hz.
    pub speed_multiplier: u8,
    pub name: Option<String>,
}

impl NewSong {
    pub fn new(format: SongFormat) -> NewSong {
        NewSong {
            format,
            channels: 4,
            track_length: 64,
            speed_multiplier: 1,
            name: None,
        }
    }
}

/// A song being edited. See the module docs.
#[derive(Debug)]
pub struct AhxDoc {
    song: Song,
    /// What the file the song came from held that the model does not (see
    /// [`write::Origin`]).
    origin: Origin,
    history: History,
    revision: u64,
    /// What the last edit, undo or redo changed.
    last: Changes,
}

/// The song name of a new AHX song and of a new HVL song.
pub const NEW_AHX_NAME: &str = "Untitled AHX song";
pub const NEW_HVL_NAME: &str = "Untitled HVL song";
/// The stereo preset a new HVL song stores (what AHX songs play at).
pub const NEW_HVL_DEFSTEREO: u8 = 2;

/// The mix gain a new HVL song of `channels` channels stores, in percent: 76
/// at 4 channels (the gain the engine gives an AHX song at stereo mode 2), and
/// turned down by the square root of the channel ratio for wider ones, so all
/// of them playing at once clip no sooner.
pub fn new_hvl_mixgain(channels: usize) -> u8 {
    (76.0 * (4.0 / channels as f64).sqrt()).round() as u8
}

impl AhxDoc {
    // ----- making and reading ------------------------------------------------

    /// A new, empty AHX or HVL song: one position whose first channel plays the
    /// blank track 1 and the others the shared blank track 0, restart 0, no
    /// subsongs and no instruments, 64 rows per track. Version 1 (the most
    /// common; HVL's version 1 has HivelyTracker 1.5's misc-flags effect).
    pub fn new(format: SongFormat) -> AhxDoc {
        AhxDoc::with_options(&NewSong::new(format)).expect("the defaults are valid")
    }

    pub fn with_options(o: &NewSong) -> Res<AhxDoc> {
        let channels = match o.format {
            SongFormat::Ahx => {
                if o.channels != 4 {
                    return Err(format!("AHX has 4 channels, not {}", o.channels));
                }
                4
            }
            SongFormat::Hvl => {
                if !(4..=write::ENGINE_MAX_CHANNELS).contains(&o.channels) {
                    return Err(format!(
                        "An HVL song has 4 to {} channels (got {})",
                        write::ENGINE_MAX_CHANNELS,
                        o.channels
                    ));
                }
                o.channels
            }
        };
        if !(1..=write::MAX_TRACK_LENGTH).contains(&o.track_length) {
            return Err(format!(
                "Rows per track is 1 to {} (got {})",
                write::MAX_TRACK_LENGTH,
                o.track_length
            ));
        }
        if !(1..=4).contains(&o.speed_multiplier) {
            return Err(format!(
                "The speed multiplier is 1 to 4 (got {})",
                o.speed_multiplier
            ));
        }
        let name = o.name.clone().unwrap_or_else(|| match o.format {
            SongFormat::Ahx => NEW_AHX_NAME.into(),
            SongFormat::Hvl => NEW_HVL_NAME.into(),
        });
        let mut track = vec![0u8; channels];
        track[0] = 1;
        let hvl = o.format == SongFormat::Hvl;
        let song = Song {
            format: o.format,
            version: 1,
            name: write::to_latin1(&name).0,
            channels,
            position_nr: 1,
            restart: 0,
            speed_multiplier: o.speed_multiplier,
            track_length: o.track_length as u8,
            track_nr: 1,
            instrument_nr: 0,
            subsong_nr: 0,
            subsongs: Vec::new(),
            positions: vec![Position {
                track,
                transpose: vec![0; channels],
            }],
            tracks: vec![vec![Step::default(); o.track_length]; 2],
            instruments: vec![Instrument::default()],
            mixgain_raw: hvl.then(|| new_hvl_mixgain(channels)),
            defstereo: hvl.then_some(NEW_HVL_DEFSTEREO),
        };
        AhxDoc::from_song_with(song, Origin::default())
    }

    /// A new song from JSON: `{"format": "ahx"|"hvl", "channels", "track_length",
    /// "speed_multiplier", "name"}`, every field optional (an AHX song by
    /// default).
    pub fn create(options: &Value) -> Res<AhxDoc> {
        let format = match opt_str(options, "format")?.unwrap_or("ahx") {
            "ahx" => SongFormat::Ahx,
            "hvl" => SongFormat::Hvl,
            other => return Err(format!("'{other}' is not a format (ahx, hvl)")),
        };
        let mut o = NewSong::new(format);
        if let Some(n) = opt_usize(options, "channels")? {
            o.channels = n;
        }
        if let Some(n) = opt_usize(options, "track_length")? {
            o.track_length = n;
        }
        if let Some(n) = opt_u8(options, "speed_multiplier")? {
            o.speed_multiplier = n;
        }
        o.name = opt_str(options, "name")?.map(str::to_string);
        AhxDoc::with_options(&o)
    }

    /// Edits a song the caller built or parsed. An error for a song the layer
    /// cannot edit: an HVL song wider than the engine plays (channels past 16
    /// would never sound), or one whose lists disagree with its counts.
    pub fn from_song(song: Song) -> Res<AhxDoc> {
        AhxDoc::from_song_with(song, Origin::default())
    }

    /// [`from_song`](Self::from_song) with what the file it was parsed from held
    /// that the model does not (see [`Origin::from_bytes`]).
    pub fn from_song_with(song: Song, origin: Origin) -> Res<AhxDoc> {
        check_song(&song)?;
        Ok(AhxDoc {
            song,
            origin,
            history: History::new(),
            revision: 0,
            last: Changes::default(),
        })
    }

    /// Reads an `.ahx` or `.hvl` file.
    pub fn from_bytes(bytes: &[u8]) -> Res<AhxDoc> {
        let song = format::parse(bytes).map_err(|e| e.to_string())?;
        let origin = Origin::from_bytes(bytes).map_err(|e| e.0)?;
        AhxDoc::from_song_with(song, origin)
    }

    /// The song as a file of its own format (an HVL song stays HVL; see
    /// [`to_ahx_bytes`](Self::to_ahx_bytes)). The errors say what the format
    /// cannot hold.
    pub fn to_bytes(&self) -> Res<Vec<u8>> {
        write::write_with(&self.song, &self.origin).map_err(|e| e.to_string())
    }

    /// An HVL song as an AHX file when it fits (see
    /// [`write::convert_hvl_to_ahx`]); an AHX song as it is.
    pub fn to_ahx_bytes(&self) -> Res<Vec<u8>> {
        match self.song.format {
            SongFormat::Ahx => self.to_bytes(),
            SongFormat::Hvl => {
                let ahx = write::convert_hvl_to_ahx(&self.song)?;
                write::write_with(&ahx, &Origin::default()).map_err(|e| e.to_string())
            }
        }
    }

    /// The song, for playing or inspecting.
    pub fn song(&self) -> &Song {
        &self.song
    }

    /// The song, taken out.
    pub fn into_song(self) -> Song {
        self.song
    }

    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    pub fn format(&self) -> SongFormat {
        self.song.format
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

    /// The song's size as a file apart from its names, and the most the format
    /// holds (the 16-bit string table offset): `(used, max)`.
    pub fn size(&self) -> (usize, usize) {
        (
            write::used_bytes(&self.song, &self.origin),
            write::SIZE_LIMIT,
        )
    }

    /// What is wrong with the song as a file of its format: positions that
    /// name tracks that do not exist, steps and instruments the format cannot
    /// hold (the first 20), and what the writer refuses. An empty list is a
    /// song that saves.
    pub fn validate(&self) -> Vec<String> {
        let s = &self.song;
        let mut problems = Vec::new();
        'outer: for (p, pos) in s.positions.iter().enumerate() {
            for (ch, &t) in pos.track.iter().enumerate() {
                if t as usize >= s.tracks.len() {
                    problems.push(format!(
                        "position {p}, channel {ch}: track {t} does not exist (the song has {})",
                        s.tracks.len()
                    ));
                    if problems.len() >= 20 {
                        break 'outer;
                    }
                }
            }
        }
        'steps: for (t, track) in s.tracks.iter().enumerate() {
            for (row, step) in track.iter().enumerate() {
                if let Err(e) = limits::check_step(s.format, step) {
                    problems.push(format!("track {t}, row {row}: {e}"));
                    if problems.len() >= 20 {
                        break 'steps;
                    }
                }
            }
        }
        for n in 1..s.instruments.len() {
            if let Some(e) = write::instrument_problem(&s.instruments[n], s.format) {
                problems.push(format!("instrument {n}: {e}"));
            }
        }
        if let Err(e) = write::write_with(s, &self.origin) {
            problems.push(format!("cannot be saved: {}", e.0));
        }
        problems
    }

    /// The song's summary: `{"format", "version", "name", "channels",
    /// "positions", "tracks", "track_length", "instruments" (the count a step
    /// can name), "subsongs", "restart", "speed_multiplier", "tick_hz",
    /// "mixgain" and "defstereo" (HVL), "effect_columns", "note": {"min", "max",
    /// "field_max", "midi_offset"}, "limits", "size": {"used", "max",
    /// "remaining"}, "blank_track0", "revision", "can_undo", "can_redo",
    /// "undo_steps", "redo_steps", "history_bytes"}`.
    pub fn info(&self) -> Value {
        let s = &self.song;
        let hvl = s.format == SongFormat::Hvl;
        let (used, max) = self.size();
        let commands: Vec<u8> = if hvl {
            (0..=15).collect()
        } else {
            write::AHX_PLIST_COMMANDS.to_vec()
        };
        let mut v = json!({
            "format": format_name(s.format),
            "version": s.version,
            "name": s.name,
            "channels": s.channels,
            "positions": s.positions.len(),
            "tracks": s.tracks.len(),
            "track_length": s.track_length,
            "instruments": s.instruments.len().saturating_sub(1),
            "subsongs": s.subsongs,
            "restart": s.restart,
            "speed_multiplier": s.speed_multiplier,
            "tick_hz": 50 * s.speed_multiplier as u32,
            "effect_columns": if hvl { 2 } else { 1 },
            "note": {
                "min": NOTE_MIN, "max": NOTE_MAX, "field_max": NOTE_FIELD_MAX,
                "midi_offset": MIDI_OFFSET,
            },
            "limits": {
                "positions": write::MAX_POSITIONS,
                "tracks": write::MAX_TRACKS,
                "track_length": [1, write::MAX_TRACK_LENGTH],
                "instruments": write::MAX_INSTRUMENTS,
                "plist_rows": write::MAX_PLIST_ROWS,
                "subsongs": write::MAX_SUBSONGS,
                "channels": if hvl { json!([4, write::ENGINE_MAX_CHANNELS]) } else { json!([4, 4]) },
                "plist_commands": commands,
                "plist_waveform": PLIST_WAVEFORM_MAX,
                "wave_length": WAVE_LENGTH_MAX,
                "volume": VOLUME_MAX,
                "transpose": [-128, 127],
                "speed_multiplier": [1, 4],
            },
            "size": {"used": used, "max": max, "remaining": max as i64 - used as i64},
            "blank_track0": s.tracks.first().is_some_and(|t| write::is_blank_track(t)),
            "revision": self.revision,
            "can_undo": self.can_undo(),
            "can_redo": self.can_redo(),
            "undo_steps": self.history.undo_steps(),
            "redo_steps": self.history.redo_steps(),
            "history_bytes": self.history.bytes(),
        });
        if hvl {
            v["mixgain"] = json!(s.mixgain_raw);
            v["defstereo"] = json!(s.defstereo);
        }
        v
    }

    /// The steps of `count` rows from `row0` of a track, for drawing:
    /// `{"track", "row0", "count", "rows" (the track length), "cells": [[note,
    /// instrument, fx, param, fxb, fxb_param] per row]}`. Rows past the end are
    /// left out. With `flat`, `cells` is one array of `count * 6` numbers.
    pub fn rows(&self, track: usize, row0: usize, count: usize, flat: bool) -> Res<Value> {
        let t = self.song.tracks.get(track).ok_or_else(|| {
            format!(
                "track {track} does not exist (the song has {})",
                self.song.tracks.len()
            )
        })?;
        let end = row0.saturating_add(count).min(t.len());
        let row0 = row0.min(end);
        let cells = steps_json(&t[row0..end], flat);
        Ok(json!({
            "track": track,
            "row0": row0,
            "count": end - row0,
            "rows": t.len(),
            "cells": cells,
        }))
    }

    /// The rows of a position across its channels, for the pattern grid:
    /// `{"position", "row0", "count", "rows", "channels", "tracks": [the track
    /// each channel plays], "transpose": [..], "cells": [[step per channel] per
    /// row]}`; with `flat`, `cells` is `count * channels * 6` numbers row by
    /// row. A channel whose track does not exist reads as blank.
    pub fn position_rows(
        &self,
        position: usize,
        row0: usize,
        count: usize,
        flat: bool,
    ) -> Res<Value> {
        let s = &self.song;
        let p = s.positions.get(position).ok_or_else(|| {
            format!(
                "position {position} does not exist (the song has {})",
                s.positions.len()
            )
        })?;
        let rows = s.track_length as usize;
        let end = row0.saturating_add(count).min(rows);
        let row0 = row0.min(end);
        let blank = Step::default();
        let step = |ch: usize, row: usize| -> Step {
            s.tracks
                .get(p.track[ch] as usize)
                .and_then(|t| t.get(row))
                .copied()
                .unwrap_or(blank)
        };
        let cells = if flat {
            let mut v = Vec::with_capacity((end - row0) * s.channels * 6);
            for r in row0..end {
                for ch in 0..s.channels {
                    let c = step(ch, r);
                    v.extend([c.note, c.instrument, c.fx, c.fx_param, c.fxb, c.fxb_param]);
                }
            }
            json!(v)
        } else {
            Value::Array(
                (row0..end)
                    .map(|r| {
                        Value::Array((0..s.channels).map(|ch| step_json(&step(ch, r))).collect())
                    })
                    .collect(),
            )
        };
        Ok(json!({
            "position": position,
            "row0": row0,
            "count": end - row0,
            "rows": rows,
            "channels": s.channels,
            "tracks": p.track,
            "transpose": p.transpose,
            "cells": cells,
        }))
    }

    /// The position list: `{"first", "count", "total", "channels", "restart",
    /// "subsongs", "entries": [[track0, transpose0, track1, transpose1, ..] per
    /// position]}` for `count` positions from `first`.
    pub fn positions(&self, first: usize, count: usize) -> Value {
        let s = &self.song;
        let end = first.saturating_add(count).min(s.positions.len());
        let first = first.min(end);
        let entries: Vec<Value> = s.positions[first..end]
            .iter()
            .map(|p| {
                Value::Array(
                    (0..s.channels)
                        .flat_map(|ch| [json!(p.track[ch]), json!(p.transpose[ch])])
                        .collect(),
                )
            })
            .collect();
        json!({
            "first": first,
            "count": end - first,
            "total": s.positions.len(),
            "channels": s.channels,
            "restart": s.restart,
            "subsongs": s.subsongs,
            "entries": entries,
        })
    }

    /// How the song's tracks are used: `{"count", "used": [how many positions'
    /// channels play each track], "blank": [whether it is all zero]}`.
    pub fn tracks(&self) -> Value {
        let usage = self.track_usage();
        json!({
            "count": self.song.tracks.len(),
            "used": usage,
            "blank": self.song.tracks.iter().map(|t| write::is_blank_track(t)).collect::<Vec<_>>(),
        })
    }

    /// How many (position, channel) cells play each track.
    pub fn track_usage(&self) -> Vec<usize> {
        let mut usage = vec![0usize; self.song.tracks.len()];
        for p in &self.song.positions {
            for &t in &p.track {
                if let Some(u) = usage.get_mut(t as usize) {
                    *u += 1;
                }
            }
        }
        usage
    }

    /// The instruments: `{"count", "instruments": [{"n", "name", "volume",
    /// "plist_length", "used"}]}`, where `used` is how many steps play it.
    pub fn instruments(&self) -> Value {
        let s = &self.song;
        let mut used = vec![0usize; s.instruments.len().max(256)];
        for track in &s.tracks {
            for step in track {
                used[step.instrument as usize] += 1;
            }
        }
        let list: Vec<Value> = (1..s.instruments.len())
            .map(|n| {
                let i = &s.instruments[n];
                json!({
                    "n": n, "name": i.name, "volume": i.volume,
                    "plist_length": i.plist.entries.len(), "used": used[n],
                })
            })
            .collect();
        json!({"count": s.instruments.len().saturating_sub(1), "instruments": list})
    }

    /// One instrument in full (see [`instrument_json`]).
    pub fn instrument(&self, n: usize) -> Res<Value> {
        let ins = self.instrument_ref(n)?;
        Ok(instrument_json(n, ins))
    }

    /// An instrument in the wire form the player takes
    /// ([`AhxPlayer::replace_instrument`](crate::player::AhxPlayer::replace_instrument)):
    /// the 22-byte core and its PList rows in the song's layout.
    pub fn instrument_bytes(&self, n: usize) -> Res<Vec<u8>> {
        let ins = self.instrument_ref(n)?;
        write::write_instrument(ins, self.song.format, self.song.version).map_err(|e| e.to_string())
    }

    /// The PList rows of an instrument: `{"instrument", "row0", "count",
    /// "length", "speed", "rows": [[note, waveform, fixed, fx0, param0, fx1,
    /// param1] per row]}`; with `flat`, `rows` is `count * 7` numbers.
    pub fn plist(&self, n: usize, row0: usize, count: usize, flat: bool) -> Res<Value> {
        let ins = self.instrument_ref(n)?;
        let list = &ins.plist.entries;
        let end = row0.saturating_add(count).min(list.len());
        let row0 = row0.min(end);
        let rows = if flat {
            json!(list[row0..end]
                .iter()
                .flat_map(|e| {
                    [
                        e.note,
                        e.waveform,
                        e.fixed as u8,
                        e.fx[0],
                        e.fx_param[0],
                        e.fx[1],
                        e.fx_param[1],
                    ]
                })
                .collect::<Vec<u8>>())
        } else {
            Value::Array(list[row0..end].iter().map(plist_entry_json).collect())
        };
        Ok(json!({
            "instrument": n,
            "row0": row0,
            "count": end - row0,
            "length": list.len(),
            "speed": ins.plist.speed,
            "rows": rows,
        }))
    }

    pub(crate) fn instrument_ref(&self, n: usize) -> Res<&Instrument> {
        if n == 0 || n >= self.song.instruments.len() {
            return Err(format!(
                "there is no instrument {n} (the song has {})",
                self.song.instruments.len().saturating_sub(1)
            ));
        }
        Ok(&self.song.instruments[n])
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

    /// The file size now, to compare with after an edit that can grow it.
    pub(crate) fn used(&self) -> usize {
        write::used_bytes(&self.song, &self.origin)
    }

    /// Refuses an edit that made the file longer than the format holds (the
    /// edit is then put back). An edit that does not grow the file is never
    /// refused, so a song that is already full can still be edited and shrunk.
    pub(crate) fn check_growth(&self, before: usize, what: &str) -> Res<()> {
        let after = self.used();
        if after > write::SIZE_LIMIT && after > before {
            return Err(format!(
                "Song is {before} of {} bytes; {what} needs {}.",
                write::SIZE_LIMIT,
                after - before
            ));
        }
        Ok(())
    }

    // ----- JSON ---------------------------------------------------------------------

    /// Runs one operation described as JSON and returns its result as JSON.
    /// Edits return `{"changed": {...}}` (see [`Changes::to_json`]) with their
    /// own fields; a failed edit returns the message and changes nothing.
    ///
    /// Reading:
    /// - `{"op": "info"}`, `{"op": "tracks"}`, `{"op": "instruments"}`,
    ///   `{"op": "validate"}` -> `{"problems": [..]}`
    /// - `{"op": "rows", "track", "row0", "count", "flat"}` and `{"op":
    ///   "position_rows", "position", "row0", "count", "flat"}`
    /// - `{"op": "positions", "first", "count"}`
    /// - `{"op": "instrument", "n"}`, `{"op": "plist", "n", "row0", "count",
    ///   "flat"}`
    /// - `{"op": "live_edits"}` -> `{"live": {...} | null}`, see
    ///   [`live_edits`](Self::live_edits)
    /// - what the instrument controls draw, from the replayer's own code:
    ///   `{"op": "envelope_curve", "n"}`, `{"op": "waveform_shape", "waveform",
    ///   "wave_length", "square_pos"}`, `{"op": "sweep_trace", "n", "kind",
    ///   "frames", "sign", "start"}`, `{"op": "vibrato_curve", "n", "frames"}`
    /// - `{"op": "bytes"}` -> `{"bytes": base64}` (the file), `{"op":
    ///   "instrument_bytes", "n"}` -> `{"bytes": base64}` (the wire form)
    /// - `{"op": "copy_range", <track>, "row0", "row1"}` -> a clip, `{"op":
    ///   "copy_plist", "n", "row0", "row1"}` -> `{"rows", "cells"}`
    ///
    /// `<track>` means `"track": n`, or `"position": p, "channel": c` for the
    /// track that cell plays; with the second form `"unique": true` first gives
    /// the cell a track of its own when others share it.
    ///
    /// Steps and tracks:
    /// - `set_step` (`<track>`, `row`, `cell: [note, instrument, fx, param, fxb,
    ///   fxb_param]` or any of `note` (number or `"C-3"`), `instrument`, `fx`,
    ///   `param`, `fxb`, `fxb_param`) -> `{"step"}`
    /// - `clear_range` (`<track>`, `row0`, `row1`, optional `columns: ["note",
    ///   "instrument", "fx", "fxb"]`), `insert_row` / `delete_row` (`<track>`,
    ///   `row`, `count`; rows pushed off the end are lost), `transpose`
    ///   (`<track>`, `row0`, `row1`, `semitones`, `clamp`, `instrument`) ->
    ///   `{"count"}`, `paste` (`<track>`, `row`, `cells` or `clip`, `mode`:
    ///   `"overwrite"` or `"mix"`) -> `{"rows", "clipped"}`
    /// - `set_track_length` (`rows`, `force`), `alloc_track` (`copy_of`,
    ///   `append`) -> `{"track"}`, `compact_tracks` -> `{"removed"}`
    ///
    /// Positions and the song:
    /// - `assign_track` (`position`, `channel`, `track`), `set_transpose`
    ///   (`position`, `channel`, `value`), `make_unique` (`position`, `channel`)
    ///   -> `{"track"}`
    /// - `insert_position` (`at`, `kind`: `"blank"`, `"duplicate"` or
    ///   `"duplicate_unique"`, `of`), `delete_position` (`at`), `move_position`
    ///   (`from`, `to`) -> `{"map"}` (how old positions moved: `{"kind":
    ///   "insert"|"delete"|"move", ..}`)
    /// - `set_restart` (`position`), `set_subsongs` (`starts`), `set_name`
    ///   (`name`) -> `{"name", "altered"}`, `set_speed_multiplier` (`value`),
    ///   `set_mix` (HVL: `mixgain`, `defstereo`)
    ///
    /// Instruments and PLists (`n` is the instrument's number):
    /// - `add_instrument` (optional fields; the default is a plain sawtooth
    ///   note) -> `{"instrument"}`, `set_instrument` (`n` and any fields of
    ///   [`instrument_json`], the envelope as an object), `copy_instrument`
    ///   (`from`, optional `to`: a new instrument when left out), `clear_instrument`
    ///   (`n`; silent and empty), `rename_instrument` (`n`, `name`)
    /// - `set_plist_row` (`n`, `row`, `entry` or any of `note`, `waveform`,
    ///   `fixed`, `fx0`, `param0`, `fx1`, `param1`), `insert_plist_row` (`n`,
    ///   `row`, `count`, optional `entry`), `delete_plist_row` (`n`, `row`,
    ///   `count`), `clear_plist_range` (`n`, `row0`, `row1`, optional `columns`:
    ///   `"note"`, `"waveform"`, `"fx0"`, `"fx1"`), `duplicate_plist_row` (`n`,
    ///   `row`), `paste_plist` (`n`, `row`, `cells`, `mode`), `transpose_plist`
    ///   (`n`, `row0`, `row1`, `semitones`, `clamp`)
    /// - `set_start_waveform` (`n`, `waveform`), `set_start_filter` (`n`,
    ///   `position`), `enable_sweep` (`n`, `kind`: `"filter"` or `"square"`)
    ///
    /// History: `begin_group`, `end_group`, `undo`, `redo`, `batch` (`ops`: one
    /// undo step, all or none; a multi-channel selection is a batch of one op per
    /// channel).
    pub fn apply(&mut self, op: &Value) -> Res<Value> {
        let name = req_str(op, "op")?;
        let done = |changes: Changes| Ok(json!({"changed": changes.to_json()}));
        match name {
            // ---- reading
            "info" => Ok(self.info()),
            "tracks" => Ok(self.tracks()),
            "instruments" => Ok(self.instruments()),
            "validate" => Ok(json!({"problems": self.validate()})),
            "live_edits" => Ok(json!({"live": self.live_edits().map(|l| l.to_json())})),
            "rows" => self.rows(
                req_usize(op, "track")?,
                opt_usize(op, "row0")?.unwrap_or(0),
                opt_usize(op, "count")?.unwrap_or(usize::MAX),
                opt_bool(op, "flat")?.unwrap_or(false),
            ),
            "position_rows" => self.position_rows(
                req_usize(op, "position")?,
                opt_usize(op, "row0")?.unwrap_or(0),
                opt_usize(op, "count")?.unwrap_or(usize::MAX),
                opt_bool(op, "flat")?.unwrap_or(false),
            ),
            "positions" => Ok(self.positions(
                opt_usize(op, "first")?.unwrap_or(0),
                opt_usize(op, "count")?.unwrap_or(usize::MAX),
            )),
            "instrument" => self.instrument(req_usize(op, "n")?),
            "plist" => self.plist(
                req_usize(op, "n")?,
                opt_usize(op, "row0")?.unwrap_or(0),
                opt_usize(op, "count")?.unwrap_or(usize::MAX),
                opt_bool(op, "flat")?.unwrap_or(false),
            ),
            "envelope_curve" => self.envelope_curve(req_usize(op, "n")?),
            "waveform_shape" => self.waveform_shape(
                req_usize(op, "waveform")?,
                req_usize(op, "wave_length")?,
                opt_usize(op, "square_pos")?.unwrap_or(32),
            ),
            "sweep_trace" => {
                let kind = match req_str(op, "kind")? {
                    "filter" => SweepKind::Filter,
                    "square" => SweepKind::Square,
                    other => return Err(format!("'{other}' is not a sweep (filter, square)")),
                };
                self.sweep_trace(
                    req_usize(op, "n")?,
                    kind,
                    opt_usize(op, "frames")?.unwrap_or(200),
                    opt_i64(op, "sign")?.unwrap_or(1) as i32,
                    opt_i64(op, "start")?.unwrap_or(0) as i32,
                )
            }
            "vibrato_curve" => {
                self.vibrato_curve(req_usize(op, "n")?, opt_usize(op, "frames")?.unwrap_or(200))
            }
            "bytes" => Ok(json!({"bytes": base64_encode(&self.to_bytes()?)})),
            "instrument_bytes" => {
                Ok(json!({"bytes": base64_encode(&self.instrument_bytes(req_usize(op, "n")?)?)}))
            }
            "copy_range" => {
                let (at, _) = steps::at_from_op(op)?;
                let track = self.track_of(at)?;
                let clip =
                    self.copy_range(track, req_usize(op, "row0")?, req_usize(op, "row1")?)?;
                Ok(clip.to_json())
            }
            "copy_plist" => {
                let clip = self.copy_plist(
                    req_usize(op, "n")?,
                    req_usize(op, "row0")?,
                    req_usize(op, "row1")?,
                )?;
                Ok(json!({
                    "rows": clip.len(),
                    "cells": clip.iter().map(plist_entry_json).collect::<Vec<_>>(),
                }))
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
                Ok(json!({"results": results, "changed": changes.to_json()}))
            }
            // ---- steps and tracks
            "set_step" => {
                let (at, unique) = steps::at_from_op(op)?;
                let (step, c) =
                    self.set_step(at, unique, req_usize(op, "row")?, StepUpdate::from_op(op)?)?;
                Ok(json!({"step": step_json(&step), "changed": c.to_json()}))
            }
            "clear_range" => {
                let (at, unique) = steps::at_from_op(op)?;
                let mask = match op.get("columns").and_then(Value::as_array) {
                    Some(list) => {
                        let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
                        ClearMask::from_names(&names)?
                    }
                    None => ClearMask::ALL,
                };
                done(self.clear_range(
                    at,
                    unique,
                    req_usize(op, "row0")?,
                    req_usize(op, "row1")?,
                    mask,
                )?)
            }
            "insert_row" | "delete_row" => {
                let (at, unique) = steps::at_from_op(op)?;
                let row = req_usize(op, "row")?;
                let count = opt_usize(op, "count")?.unwrap_or(1);
                done(if name == "insert_row" {
                    self.insert_row(at, unique, row, count)?
                } else {
                    self.delete_row(at, unique, row, count)?
                })
            }
            "transpose" => {
                let (at, unique) = steps::at_from_op(op)?;
                let (count, c) = self.transpose(
                    at,
                    unique,
                    req_usize(op, "row0")?,
                    req_usize(op, "row1")?,
                    req_i64(op, "semitones")?.clamp(-255, 255) as i32,
                    opt_bool(op, "clamp")?.unwrap_or(false),
                    opt_u8(op, "instrument")?,
                )?;
                Ok(json!({"count": count, "changed": c.to_json()}))
            }
            "paste" => {
                let (at, unique) = steps::at_from_op(op)?;
                let clip = StepClip::from_json(
                    op.get("clip")
                        .or_else(|| op.get("cells"))
                        .ok_or("paste needs 'cells' (as copy_range returns them)")?,
                )?;
                let mode = paste_mode(op)?;
                let (p, c) = self.paste(at, unique, req_usize(op, "row")?, &clip, mode)?;
                Ok(json!({"rows": p.rows, "clipped": p.clipped, "changed": c.to_json()}))
            }
            "set_track_length" => done(self.set_track_length(
                req_usize(op, "rows")?,
                opt_bool(op, "force")?.unwrap_or(false),
            )?),
            "alloc_track" => {
                let (t, c) = self.alloc_track(
                    opt_usize(op, "copy_of")?,
                    opt_bool(op, "append")?.unwrap_or(false),
                )?;
                Ok(json!({"track": t, "changed": c.to_json()}))
            }
            "compact_tracks" => {
                let (n, c) = self.compact_tracks()?;
                Ok(json!({"removed": n, "changed": c.to_json()}))
            }
            // ---- positions and the song
            "assign_track" => done(self.assign_track(
                req_usize(op, "position")?,
                req_usize(op, "channel")?,
                req_usize(op, "track")?,
            )?),
            "set_transpose" => done(self.set_transpose(
                req_usize(op, "position")?,
                req_usize(op, "channel")?,
                req_i64(op, "value")?,
            )?),
            "make_unique" => {
                let (t, c) =
                    self.make_unique(req_usize(op, "position")?, req_usize(op, "channel")?)?;
                Ok(json!({"track": t, "changed": c.to_json()}))
            }
            "insert_position" => {
                let at = req_usize(op, "at")?;
                let kind = match opt_str(op, "kind")?.unwrap_or("blank") {
                    "blank" => InsertKind::Blank,
                    "duplicate" => InsertKind::Duplicate(req_usize(op, "of")?),
                    "duplicate_unique" => InsertKind::DuplicateUnique(req_usize(op, "of")?),
                    other => {
                        return Err(format!(
                        "'{other}' is not a kind of position (blank, duplicate, duplicate_unique)"
                    ))
                    }
                };
                let (map, c) = self.insert_position(at, kind)?;
                Ok(json!({"map": map.to_json(), "changed": c.to_json()}))
            }
            "delete_position" => {
                let (map, c) = self.delete_position(req_usize(op, "at")?)?;
                Ok(json!({"map": map.to_json(), "changed": c.to_json()}))
            }
            "move_position" => {
                let (map, c) = self.move_position(req_usize(op, "from")?, req_usize(op, "to")?)?;
                Ok(json!({"map": map.to_json(), "changed": c.to_json()}))
            }
            "set_restart" => done(self.set_restart(req_usize(op, "position")?)?),
            "set_subsongs" => {
                let list = op
                    .get("starts")
                    .and_then(Value::as_array)
                    .ok_or("'starts' must be a list of positions")?;
                let starts = list
                    .iter()
                    .map(|v| {
                        v.as_u64()
                            .map(|n| n as usize)
                            .ok_or_else(|| "a subsong start is a position number".to_string())
                    })
                    .collect::<Res<Vec<_>>>()?;
                done(self.set_subsongs(&starts)?)
            }
            "set_name" => {
                let (altered, c) = self.set_name(req_str(op, "name")?)?;
                Ok(json!({"name": self.song.name, "altered": altered, "changed": c.to_json()}))
            }
            "set_speed_multiplier" => done(self.set_speed_multiplier(req_usize(op, "value")?)?),
            "set_mix" => done(self.set_mix(opt_u8(op, "mixgain")?, opt_u8(op, "defstereo")?)?),
            // ---- instruments and PLists
            "add_instrument" => {
                let (n, c) = self.add_instrument_op(op)?;
                Ok(json!({"instrument": n, "changed": c.to_json()}))
            }
            "set_instrument" => done(self.set_instrument_op(req_usize(op, "n")?, op)?),
            "rename_instrument" => {
                done(self.rename_instrument(req_usize(op, "n")?, req_str(op, "name")?)?)
            }
            "copy_instrument" => {
                let (n, c) = self.copy_instrument(req_usize(op, "from")?, opt_usize(op, "to")?)?;
                Ok(json!({"instrument": n, "changed": c.to_json()}))
            }
            "clear_instrument" => done(self.clear_instrument(req_usize(op, "n")?)?),
            "set_plist_row" => done(self.set_plist_row(
                req_usize(op, "n")?,
                req_usize(op, "row")?,
                PListUpdate::from_op(op)?,
            )?),
            "insert_plist_row" => {
                let entry = match op.get("entry") {
                    Some(Value::Null) | None => None,
                    Some(v) => Some(plist_entry_from_json(v)?),
                };
                done(self.insert_plist_rows(
                    req_usize(op, "n")?,
                    req_usize(op, "row")?,
                    opt_usize(op, "count")?.unwrap_or(1),
                    entry,
                )?)
            }
            "delete_plist_row" => done(self.delete_plist_rows(
                req_usize(op, "n")?,
                req_usize(op, "row")?,
                opt_usize(op, "count")?.unwrap_or(1),
            )?),
            "duplicate_plist_row" => {
                done(self.duplicate_plist_row(req_usize(op, "n")?, req_usize(op, "row")?)?)
            }
            "clear_plist_range" => {
                let names: Vec<&str> = match op.get("columns").and_then(Value::as_array) {
                    Some(list) => list.iter().filter_map(Value::as_str).collect(),
                    None => Vec::new(),
                };
                done(self.clear_plist_range(
                    req_usize(op, "n")?,
                    req_usize(op, "row0")?,
                    req_usize(op, "row1")?,
                    &names,
                )?)
            }
            "paste_plist" => {
                let cells = op
                    .get("cells")
                    .and_then(Value::as_array)
                    .ok_or("paste_plist needs 'cells' (as copy_plist returns them)")?;
                let rows = cells
                    .iter()
                    .map(plist_entry_from_json)
                    .collect::<Res<Vec<_>>>()?;
                done(self.paste_plist(
                    req_usize(op, "n")?,
                    req_usize(op, "row")?,
                    &rows,
                    paste_mode(op)?,
                )?)
            }
            "transpose_plist" => {
                let (count, c) = self.transpose_plist(
                    req_usize(op, "n")?,
                    req_usize(op, "row0")?,
                    req_usize(op, "row1")?,
                    req_i64(op, "semitones")?.clamp(-255, 255) as i32,
                    opt_bool(op, "clamp")?.unwrap_or(false),
                )?;
                Ok(json!({"count": count, "changed": c.to_json()}))
            }
            "set_start_waveform" => {
                done(self.set_start_waveform(req_usize(op, "n")?, req_usize(op, "waveform")?)?)
            }
            "set_start_filter" => {
                done(self.set_start_filter(req_usize(op, "n")?, req_usize(op, "position")?)?)
            }
            "enable_sweep" => {
                let kind = match req_str(op, "kind")? {
                    "filter" => SweepKind::Filter,
                    "square" => SweepKind::Square,
                    other => return Err(format!("'{other}' is not a sweep (filter, square)")),
                };
                done(self.enable_sweep(req_usize(op, "n")?, kind)?)
            }
            other => Err(format!("'{other}' is not an operation")),
        }
    }
}

fn paste_mode(op: &Value) -> Res<PasteMode> {
    match opt_str(op, "mode")? {
        None | Some("overwrite") => Ok(PasteMode::Overwrite),
        Some("mix") => Ok(PasteMode::Mix),
        Some(other) => Err(format!("'{other}' is not a paste mode (overwrite, mix)")),
    }
}

fn steps_json(steps: &[Step], flat: bool) -> Value {
    if flat {
        json!(steps
            .iter()
            .flat_map(|c| [c.note, c.instrument, c.fx, c.fx_param, c.fxb, c.fxb_param])
            .collect::<Vec<u8>>())
    } else {
        Value::Array(steps.iter().map(step_json).collect())
    }
}

/// Whether `s` is a song this layer can edit: its lists agree with its counts,
/// every track has the song's row count and every position its channel count,
/// and an HVL song is no wider than the engine plays.
fn check_song(s: &Song) -> Res<()> {
    match s.format {
        SongFormat::Ahx => {
            if s.channels != 4 {
                return Err(format!(
                    "AHX has 4 channels (this song says {})",
                    s.channels
                ));
            }
        }
        SongFormat::Hvl => {
            if !(4..=write::ENGINE_MAX_CHANNELS).contains(&s.channels) {
                return Err(format!(
                    "An HVL song has 4 to {max} channels to edit (this one has {}): channels past {max} would never sound",
                    s.channels,
                    max = write::ENGINE_MAX_CHANNELS
                ));
            }
        }
    }
    if s.tracks.is_empty()
        || s.instruments.is_empty()
        || s.position_nr != s.positions.len()
        || s.track_nr as usize != s.tracks.len() - 1
        || s.instrument_nr as usize != s.instruments.len() - 1
        || s.subsong_nr as usize != s.subsongs.len()
    {
        return Err("the song's counts disagree with its lists".into());
    }
    if s.tracks.iter().any(|t| t.len() != s.track_length as usize) {
        return Err("a track does not have the song's number of rows".into());
    }
    if s.positions
        .iter()
        .any(|p| p.track.len() != s.channels || p.transpose.len() != s.channels)
    {
        return Err("a position does not have one track and transpose per channel".into());
    }
    Ok(())
}
