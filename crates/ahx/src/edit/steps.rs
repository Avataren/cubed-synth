//! Editing tracks: steps, ranges, rows, the clipboard, the track table.

use serde_json::{json, Value};

use super::history::{Change, Changes, LayoutState};
use super::json::{field, opt_bool, opt_usize, step_json, StepUpdate};
use super::limits::{check_step, Res, NOTE_MAX, NOTE_MIN};
use super::AhxDoc;
use crate::format::Step;
use crate::write::{self, is_blank_step, is_blank_track};

/// Where a step edit lands: a track by its number, or the track a position's
/// channel plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    Track(usize),
    Cell { position: usize, channel: usize },
}

/// `<track>` of an operation: `"track": n`, or `"position": p, "channel": c`
/// with an optional `"unique": true`.
pub(crate) fn at_from_op(op: &Value) -> Res<(At, bool)> {
    let unique = opt_bool(op, "unique")?.unwrap_or(false);
    if let Some(t) = opt_usize(op, "track")? {
        return Ok((At::Track(t), unique));
    }
    match (opt_usize(op, "position")?, opt_usize(op, "channel")?) {
        (Some(position), Some(channel)) => Ok((At::Cell { position, channel }, unique)),
        _ => Err("needs 'track', or 'position' and 'channel'".into()),
    }
}

const TRACK_0_REASON: &str = "Track 0 is the blank track every empty cell points at; give the channel a track of its own before writing into it.";

/// Which columns a clear touches (`fx` clears the parameter too, as `fxb` does
/// its own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClearMask {
    pub note: bool,
    pub instrument: bool,
    pub fx: bool,
    pub fxb: bool,
}

impl ClearMask {
    pub const ALL: ClearMask = ClearMask {
        note: true,
        instrument: true,
        fx: true,
        fxb: true,
    };

    /// From a list of column names (`"note"`, `"instrument"`, `"fx"`, `"fxb"`).
    pub fn from_names(names: &[&str]) -> Res<ClearMask> {
        let mut m = ClearMask {
            note: false,
            instrument: false,
            fx: false,
            fxb: false,
        };
        for n in names {
            match *n {
                "note" => m.note = true,
                "instrument" => m.instrument = true,
                "fx" | "effect" | "param" => m.fx = true,
                "fxb" | "effect2" => m.fxb = true,
                other => {
                    return Err(format!(
                        "'{other}' is not a column (note, instrument, fx, fxb)"
                    ))
                }
            }
        }
        Ok(m)
    }

    fn clear(&self, s: &Step) -> Step {
        let mut s = *s;
        if self.note {
            s.note = 0;
        }
        if self.instrument {
            s.instrument = 0;
        }
        if self.fx {
            s.fx = 0;
            s.fx_param = 0;
        }
        if self.fxb {
            s.fxb = 0;
            s.fxb_param = 0;
        }
        s
    }
}

/// How a paste combines with what is there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PasteMode {
    /// Replace every step.
    #[default]
    Overwrite,
    /// Fill only the empty fields of the steps there.
    Mix,
}

/// What a paste did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pasted {
    /// Rows written.
    pub rows: usize,
    /// Rows of the clip that ran past the end of the track and were cut.
    pub clipped: usize,
}

/// Rows of one track, as `copy_range` returns them and `paste` takes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepClip {
    pub cells: Vec<Step>,
}

impl StepClip {
    /// `{"rows", "cells": [[note, instrument, fx, param, fxb, fxb_param] per row]}`.
    pub fn to_json(&self) -> Value {
        json!({
            "rows": self.cells.len(),
            "cells": self.cells.iter().map(step_json).collect::<Vec<_>>(),
        })
    }

    /// From the array of steps (or an object with `cells`) `to_json` makes.
    pub fn from_json(v: &Value) -> Res<StepClip> {
        let list = match v {
            Value::Array(a) => a,
            Value::Object(_) => field(v, "cells")
                .and_then(Value::as_array)
                .ok_or("a clip is {\"cells\": [..]}")?,
            _ => return Err("a clip is a list of steps".into()),
        };
        let mut cells = Vec::with_capacity(list.len());
        for c in list {
            let a = c
                .as_array()
                .filter(|a| (4..=6).contains(&a.len()))
                .ok_or("a step is [note, instrument, fx, param, fxb, fxb_param]")?;
            let b = |i: usize| -> Res<u8> {
                a.get(i).map_or(Ok(0), |v| {
                    v.as_u64()
                        .filter(|&n| n <= 255)
                        .map(|n| n as u8)
                        .ok_or_else(|| "a step field is 0-255".to_string())
                })
            };
            cells.push(Step {
                note: b(0)?,
                instrument: b(1)?,
                fx: b(2)?,
                fx_param: b(3)?,
                fxb: b(4)?,
                fxb_param: b(5)?,
            });
        }
        Ok(StepClip { cells })
    }
}

impl AhxDoc {
    fn rows_of_tracks(&self) -> usize {
        self.song.track_length as usize
    }

    fn check_row(&self, row: usize) -> Res<()> {
        if row >= self.rows_of_tracks() {
            return Err(format!(
                "Row {row} is outside this song's tracks ({} rows).",
                self.rows_of_tracks()
            ));
        }
        Ok(())
    }

    fn check_rows(&self, row0: usize, row1: usize) -> Res<()> {
        self.check_row(row0)?;
        self.check_row(row1)?;
        if row0 > row1 {
            return Err(format!("the range runs backwards (row {row0} to {row1})"));
        }
        Ok(())
    }

    /// The track `at` names (without changing anything).
    pub fn track_of(&self, at: At) -> Res<usize> {
        let s = &self.song;
        match at {
            At::Track(t) => {
                if t >= s.tracks.len() {
                    return Err(format!(
                        "There is no track {t} (the song has {}).",
                        s.tracks.len()
                    ));
                }
                Ok(t)
            }
            At::Cell { position, channel } => {
                self.check_cell(position, channel)?;
                let t = s.positions[position].track[channel] as usize;
                if t >= s.tracks.len() {
                    return Err(format!(
                        "position {position}, channel {channel} plays track {t}, which does not exist"
                    ));
                }
                Ok(t)
            }
        }
    }

    /// The track `at` names, after giving the cell a track of its own when
    /// `unique` asks and others share it. Inside an edit.
    fn resolve(&mut self, at: At, unique: bool) -> Res<usize> {
        match at {
            At::Cell { position, channel } if unique => {
                self.check_cell(position, channel)?;
                self.make_unique_in(position, channel)
            }
            _ => self.track_of(at),
        }
    }

    /// Replaces `new.len()` rows of `track` from `row0`, recording what was
    /// there. Track 0, while it is blank, takes no content.
    pub(crate) fn put_steps(&mut self, track: usize, row0: usize, new: Vec<Step>) -> Res<()> {
        let t = &self.song.tracks[track];
        if t[row0..row0 + new.len()] == new[..] {
            return Ok(());
        }
        if track == 0 && is_blank_track(t) && new.iter().any(|s| !is_blank_step(s)) {
            return Err(TRACK_0_REASON.into());
        }
        self.change(Change::Steps {
            track,
            row0,
            steps: new,
        });
        Ok(())
    }

    /// Sets (some fields of) the step at `row` of the track `at` names. Returns
    /// the step as it is now.
    pub fn set_step(
        &mut self,
        at: At,
        unique: bool,
        row: usize,
        update: StepUpdate,
    ) -> Res<(Step, Changes)> {
        self.check_row(row)?;
        let format = self.song.format;
        self.edit(|d| {
            let track = d.resolve(at, unique)?;
            let step = update.apply(&d.song.tracks[track][row]);
            check_step(format, &step)?;
            let before = d.used();
            d.put_steps(track, row, vec![step])?;
            if format == crate::format::SongFormat::Hvl {
                d.check_growth(before, "this step")?;
            }
            Ok(step)
        })
    }

    /// Clears the columns `mask` names in rows `row0..=row1`.
    pub fn clear_range(
        &mut self,
        at: At,
        unique: bool,
        row0: usize,
        row1: usize,
        mask: ClearMask,
    ) -> Res<Changes> {
        self.check_rows(row0, row1)?;
        self.edit(|d| {
            let track = d.resolve(at, unique)?;
            let new: Vec<Step> = d.song.tracks[track][row0..=row1]
                .iter()
                .map(|s| mask.clear(s))
                .collect();
            d.put_steps(track, row0, new)
        })
        .map(|(_, c)| c)
    }

    /// Inserts `count` blank rows at `row`; the rows below move down and the ones
    /// pushed off the end are lost.
    pub fn insert_row(&mut self, at: At, unique: bool, row: usize, count: usize) -> Res<Changes> {
        self.check_row(row)?;
        if count == 0 {
            return Err("'count' must be at least 1".into());
        }
        self.edit(|d| {
            let track = d.resolve(at, unique)?;
            let len = d.rows_of_tracks();
            let count = count.min(len - row);
            let old = d.song.tracks[track][row..].to_vec();
            let new: Vec<Step> = (0..len - row)
                .map(|i| {
                    if i < count {
                        Step::default()
                    } else {
                        old[i - count]
                    }
                })
                .collect();
            d.put_steps(track, row, new)
        })
        .map(|(_, c)| c)
    }

    /// Deletes `count` rows at `row`; the rows below move up and blank ones fill
    /// the end.
    pub fn delete_row(&mut self, at: At, unique: bool, row: usize, count: usize) -> Res<Changes> {
        self.check_row(row)?;
        if count == 0 {
            return Err("'count' must be at least 1".into());
        }
        self.edit(|d| {
            let track = d.resolve(at, unique)?;
            let len = d.rows_of_tracks();
            let old = d.song.tracks[track][row..].to_vec();
            let new: Vec<Step> = (0..len - row)
                .map(|i| old.get(i + count).copied().unwrap_or_default())
                .collect();
            d.put_steps(track, row, new)
        })
        .map(|(_, c)| c)
    }

    /// Shifts the notes of rows `row0..=row1` by `semitones` (only steps with a
    /// note, and with `instrument` when given). A note that would leave C-1 to
    /// B-5 refuses the edit unless `clamp` holds it at the edge. Returns how
    /// many notes moved.
    #[allow(clippy::too_many_arguments)]
    pub fn transpose(
        &mut self,
        at: At,
        unique: bool,
        row0: usize,
        row1: usize,
        semitones: i32,
        clamp: bool,
        instrument: Option<u8>,
    ) -> Res<(usize, Changes)> {
        self.check_rows(row0, row1)?;
        self.edit(|d| {
            let track = d.resolve(at, unique)?;
            let mut new = d.song.tracks[track][row0..=row1].to_vec();
            let mut count = 0;
            for (i, s) in new.iter_mut().enumerate() {
                if s.note == 0 || instrument.is_some_and(|n| n != s.instrument) {
                    continue;
                }
                let n = s.note as i32 + semitones;
                let n = if (NOTE_MIN as i32..=NOTE_MAX as i32).contains(&n) {
                    n
                } else if clamp {
                    n.clamp(NOTE_MIN as i32, NOTE_MAX as i32)
                } else {
                    return Err(format!(
                        "Transposing by {semitones} takes row {} outside C-1 to B-5.",
                        row0 + i
                    ));
                };
                if n != s.note as i32 {
                    s.note = n as u8;
                    count += 1;
                }
            }
            d.put_steps(track, row0, new)?;
            Ok(count)
        })
    }

    /// The steps of rows `row0..=row1` of a track.
    pub fn copy_range(&self, track: usize, row0: usize, row1: usize) -> Res<StepClip> {
        self.check_rows(row0, row1)?;
        let t = self
            .song
            .tracks
            .get(track)
            .ok_or_else(|| format!("There is no track {track}."))?;
        Ok(StepClip {
            cells: t[row0..=row1].to_vec(),
        })
    }

    /// Writes a clip at `row` of the track `at` names. A step the format cannot
    /// hold refuses the whole paste; rows past the end of the track are cut
    /// (`clipped` says how many).
    pub fn paste(
        &mut self,
        at: At,
        unique: bool,
        row: usize,
        clip: &StepClip,
        mode: PasteMode,
    ) -> Res<(Pasted, Changes)> {
        self.check_row(row)?;
        let format = self.song.format;
        for (i, s) in clip.cells.iter().enumerate() {
            check_step(format, s).map_err(|e| format!("Row {i} of the clip: {e}"))?;
        }
        self.edit(|d| {
            let track = d.resolve(at, unique)?;
            let len = d.rows_of_tracks();
            let rows = clip.cells.len().min(len - row);
            let new: Vec<Step> = (0..rows)
                .map(|i| {
                    let cur = d.song.tracks[track][row + i];
                    let c = clip.cells[i];
                    match mode {
                        PasteMode::Overwrite => c,
                        PasteMode::Mix => Step {
                            note: if cur.note == 0 { c.note } else { cur.note },
                            instrument: if cur.instrument == 0 {
                                c.instrument
                            } else {
                                cur.instrument
                            },
                            fx: if cur.fx == 0 && cur.fx_param == 0 {
                                c.fx
                            } else {
                                cur.fx
                            },
                            fx_param: if cur.fx == 0 && cur.fx_param == 0 {
                                c.fx_param
                            } else {
                                cur.fx_param
                            },
                            fxb: if cur.fxb == 0 && cur.fxb_param == 0 {
                                c.fxb
                            } else {
                                cur.fxb
                            },
                            fxb_param: if cur.fxb == 0 && cur.fxb_param == 0 {
                                c.fxb_param
                            } else {
                                cur.fxb_param
                            },
                        },
                    }
                })
                .collect();
            let before = d.used();
            d.put_steps(track, row, new)?;
            if format == crate::format::SongFormat::Hvl {
                d.check_growth(before, "this paste")?;
            }
            Ok(Pasted {
                rows,
                clipped: clip.cells.len() - rows,
            })
        })
    }

    /// Rows per track for the whole song: longer adds blank rows, shorter cuts
    /// rows off every track (refused while a cut row holds a step, unless
    /// `force`).
    pub fn set_track_length(&mut self, rows: usize, force: bool) -> Res<Changes> {
        if !(1..=write::MAX_TRACK_LENGTH).contains(&rows) {
            return Err(format!(
                "Rows per track is 1 to {} (got {rows}).",
                write::MAX_TRACK_LENGTH
            ));
        }
        let old = self.rows_of_tracks();
        self.edit(|d| {
            if rows == old {
                return Ok(());
            }
            if rows < old && !force {
                let cut = d
                    .song
                    .tracks
                    .iter()
                    .filter(|t| t[rows..].iter().any(|s| !is_blank_step(s)))
                    .count();
                if cut > 0 {
                    return Err(format!(
                        "Shortening the tracks to {rows} rows would cut steps from {cut} track{}; pass 'force' to do it.",
                        if cut == 1 { "" } else { "s" }
                    ));
                }
            }
            let tracks = d
                .song
                .tracks
                .iter()
                .map(|t| {
                    let mut t = t.clone();
                    t.resize(rows, Step::default());
                    t
                })
                .collect();
            let before = d.used();
            d.change(Change::Layout(Box::new(LayoutState {
                track_length: rows as u8,
                tracks,
                positions: None,
            })));
            d.check_growth(before, &format!("{rows} rows per track"))
        })
        .map(|(_, c)| c)
    }

    /// A track for a channel that needs one of its own: the first track from 1
    /// that no position uses and that is all blank (its slot is reused, so the
    /// file does not grow), else a new one at the end. `copy_of` gives it that
    /// track's steps; `append` skips the reuse and always adds one. Refused at
    /// 256 tracks and when the file would no longer fit.
    pub fn alloc_track(&mut self, copy_of: Option<usize>, append: bool) -> Res<(usize, Changes)> {
        self.edit(|d| d.alloc_track_in(copy_of, append))
    }

    pub(crate) fn alloc_track_in(&mut self, copy_of: Option<usize>, append: bool) -> Res<usize> {
        let s = &self.song;
        let content: Vec<Step> = match copy_of {
            Some(c) => s
                .tracks
                .get(c)
                .ok_or_else(|| format!("There is no track {c} to copy."))?
                .clone(),
            None => vec![Step::default(); self.rows_of_tracks()],
        };
        if !append {
            let usage = self.track_usage();
            for t in 1..self.song.tracks.len() {
                if usage[t] == 0 && is_blank_track(&self.song.tracks[t]) {
                    if copy_of.is_some() {
                        self.put_steps(t, 0, content)?;
                    }
                    return Ok(t);
                }
            }
        }
        if self.song.tracks.len() >= write::MAX_TRACKS {
            return Err(format!(
                "A song holds at most {} tracks.",
                write::MAX_TRACKS
            ));
        }
        let before = self.used();
        self.change(Change::TrackPush { steps: content });
        self.check_growth(before, "a new track")?;
        Ok(self.song.tracks.len() - 1)
    }

    /// Removes the tracks no position plays (track 0 stays) and renumbers the
    /// rest, in the positions too. Returns how many went. Refused while a
    /// position names a track that does not exist.
    pub fn compact_tracks(&mut self) -> Res<(usize, Changes)> {
        self.edit(|d| {
            let tracks = d.song.tracks.len();
            if let Some((p, ch)) = d
                .song
                .positions
                .iter()
                .enumerate()
                .flat_map(|(p, e)| e.track.iter().enumerate().map(move |(ch, &t)| (p, ch, t)))
                .find(|&(_, _, t)| t as usize >= tracks)
                .map(|(p, ch, _)| (p, ch))
            {
                return Err(format!(
                    "position {p}, channel {ch} plays a track that does not exist: fix it first"
                ));
            }
            let usage = d.track_usage();
            let keep: Vec<bool> = (0..tracks).map(|t| t == 0 || usage[t] > 0).collect();
            let removed = keep.iter().filter(|&&k| !k).count();
            if removed == 0 {
                return Ok(0);
            }
            let mut map = vec![0u8; tracks];
            let mut next = 0u8;
            let mut new_tracks = Vec::with_capacity(tracks - removed);
            for t in 0..tracks {
                if keep[t] {
                    map[t] = next;
                    next += 1;
                    new_tracks.push(d.song.tracks[t].clone());
                }
            }
            let mut positions = d.song.positions.clone();
            for p in &mut positions {
                for t in &mut p.track {
                    *t = map[*t as usize];
                }
            }
            d.change(Change::Layout(Box::new(LayoutState {
                track_length: d.song.track_length,
                tracks: new_tracks,
                positions: Some(positions),
            })));
            Ok(removed)
        })
    }
}
