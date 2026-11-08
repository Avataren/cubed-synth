//! Editing the position list, the song header and which track a channel plays.

use serde_json::{json, Value};

use super::history::{Change, Changes, HeaderState};
use super::limits::Res;
use super::AhxDoc;
use crate::format::{Position, SongFormat};
use crate::write::{self, is_blank_track, to_latin1};

/// What a position op did to the positions' numbers: an old index to its new
/// one, or `None` when the position no longer exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionMap {
    /// A position was inserted at `at`: everything from there on moves down one.
    Insert { at: usize },
    /// Position `at` was deleted.
    Delete { at: usize },
    /// Position `from` was moved to index `to`.
    Move { from: usize, to: usize },
}

impl PositionMap {
    pub fn apply(&self, old: usize) -> Option<usize> {
        match *self {
            PositionMap::Insert { at } => Some(if old >= at { old + 1 } else { old }),
            PositionMap::Delete { at } => match old.cmp(&at) {
                std::cmp::Ordering::Less => Some(old),
                std::cmp::Ordering::Equal => None,
                std::cmp::Ordering::Greater => Some(old - 1),
            },
            PositionMap::Move { from, to } => Some(if old == from {
                to
            } else if from < to && old > from && old <= to {
                old - 1
            } else if from > to && old >= to && old < from {
                old + 1
            } else {
                old
            }),
        }
    }

    /// `{"kind": "insert", "at"}`, `{"kind": "delete", "at"}` or `{"kind":
    /// "move", "from", "to"}`.
    pub fn to_json(&self) -> Value {
        match *self {
            PositionMap::Insert { at } => json!({"kind": "insert", "at": at}),
            PositionMap::Delete { at } => json!({"kind": "delete", "at": at}),
            PositionMap::Move { from, to } => json!({"kind": "move", "from": from, "to": to}),
        }
    }
}

/// What a new position plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertKind {
    /// Every channel plays the blank track 0 (or, when track 0 holds content,
    /// a blank track of its own).
    Blank,
    /// The tracks of position `of`, shared with it.
    Duplicate(usize),
    /// A copy of each of position `of`'s tracks, one of its own per channel.
    DuplicateUnique(usize),
}

impl AhxDoc {
    pub(crate) fn check_cell(&self, position: usize, channel: usize) -> Res<()> {
        let s = &self.song;
        if position >= s.positions.len() {
            return Err(format!(
                "There is no position {position} (the song has {}).",
                s.positions.len()
            ));
        }
        if channel >= s.channels {
            return Err(format!(
                "{} channels 0 to {}, not {channel}.",
                if s.format == SongFormat::Hvl {
                    "This song has"
                } else {
                    "AHX has"
                },
                s.channels - 1
            ));
        }
        Ok(())
    }

    /// Replaces one channel's entry of a position (a track and/or a transpose).
    pub(crate) fn put_entry(
        &mut self,
        position: usize,
        channel: usize,
        track: Option<u8>,
        transpose: Option<i8>,
    ) {
        let mut entry = self.song.positions[position].clone();
        if let Some(t) = track {
            entry.track[channel] = t;
        }
        if let Some(x) = transpose {
            entry.transpose[channel] = x;
        }
        if entry != self.song.positions[position] {
            self.change(Change::Position {
                index: position,
                entry,
            });
        }
    }

    /// Makes channel `channel` of `position` play track `track`.
    pub fn assign_track(&mut self, position: usize, channel: usize, track: usize) -> Res<Changes> {
        self.check_cell(position, channel)?;
        if track >= self.song.tracks.len() {
            return Err(format!(
                "There is no track {track} (the song has {}).",
                self.song.tracks.len()
            ));
        }
        self.edit(|d| {
            d.put_entry(position, channel, Some(track as u8), None);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Sets the transpose (semitones added to every note of the track at this
    /// position, -128 to 127).
    pub fn set_transpose(&mut self, position: usize, channel: usize, value: i64) -> Res<Changes> {
        self.check_cell(position, channel)?;
        if !(-128..=127).contains(&value) {
            return Err(format!("A transpose is -128 to 127 (got {value})."));
        }
        self.edit(|d| {
            d.put_entry(position, channel, None, Some(value as i8));
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Gives the cell a track of its own: when other cells play its track, a
    /// copy of it is assigned to this one (the number it now has; the original
    /// when it was already alone). While track 0 is blank it counts as always
    /// shared, so the cell gets a copy even when it is the last one playing it.
    pub fn make_unique(&mut self, position: usize, channel: usize) -> Res<(usize, Changes)> {
        self.check_cell(position, channel)?;
        self.edit(|d| d.make_unique_in(position, channel))
    }

    pub(crate) fn make_unique_in(&mut self, position: usize, channel: usize) -> Res<usize> {
        let current = self.song.positions[position].track[channel] as usize;
        if current >= self.song.tracks.len() {
            return Err(format!(
                "position {position}, channel {channel} plays track {current}, which does not exist"
            ));
        }
        // The blank track 0 is shared by definition (no cell may write it, so a
        // cell must never be handed it back as its own), however few play it.
        let blank_shared = current == 0 && is_blank_track(&self.song.tracks[0]);
        if !blank_shared && self.track_usage()[current] <= 1 {
            return Ok(current);
        }
        let copy = self.alloc_track_in(Some(current), false)?;
        self.put_entry(position, channel, Some(copy as u8), None);
        Ok(copy)
    }

    /// Moves the restart position and the subsong starts through `map`, a
    /// deleted one landing on `fallback`; all clamped into the list.
    fn remap_header(&mut self, map: PositionMap, fallback: usize) {
        let last = self.song.positions.len().saturating_sub(1);
        let through = |old: usize| map.apply(old).unwrap_or(fallback).min(last);
        let mut h = HeaderState::of(&self.song);
        h.restart = through(h.restart as usize) as u16;
        for s in &mut h.subsongs {
            *s = through(*s as usize) as u16;
        }
        if h != HeaderState::of(&self.song) {
            self.change(Change::Header(Box::new(h)));
        }
    }

    /// A new position at index `at` (0..=positions; the one that was at `at`
    /// moves down). Returns how old positions moved.
    pub fn insert_position(&mut self, at: usize, kind: InsertKind) -> Res<(PositionMap, Changes)> {
        let len = self.song.positions.len();
        if at > len {
            return Err(format!("A position cannot be inserted at {at}."));
        }
        if len >= write::MAX_POSITIONS {
            return Err(format!(
                "A song holds at most {} positions.",
                write::MAX_POSITIONS
            ));
        }
        let channels = self.song.channels;
        let source =
            match kind {
                InsertKind::Blank => None,
                InsertKind::Duplicate(of) | InsertKind::DuplicateUnique(of) => {
                    Some(self.song.positions.get(of).cloned().ok_or_else(|| {
                        format!("There is no position {of} (the song has {len}).")
                    })?)
                }
            };
        let map = PositionMap::Insert { at };
        let changes = self.edit(|d| {
            let before = d.used();
            let entry = source.clone().unwrap_or_else(|| Position {
                track: vec![0; channels],
                transpose: vec![0; channels],
            });
            d.change(Change::PositionInsert { index: at, entry });
            d.remap_header(map, at);
            // Give channels tracks of their own one at a time: a track is only
            // unreferenced (and so reusable) until it is assigned.
            let track0_blank = is_blank_track(&d.song.tracks[0]);
            let mut own: Vec<(usize, Option<usize>)> = Vec::new();
            match kind {
                InsertKind::Blank => {
                    if !track0_blank {
                        own.extend((0..channels).map(|ch| (ch, None)));
                    }
                }
                InsertKind::DuplicateUnique(_) => {
                    for ch in 0..channels {
                        let t = d.song.positions[at].track[ch] as usize;
                        // A copy of the shared blank track 0 would only waste a track.
                        if !(t == 0 && track0_blank) {
                            own.push((ch, Some(t)));
                        }
                    }
                }
                InsertKind::Duplicate(_) => {}
            }
            for (ch, copy_of) in own {
                let fresh = d.alloc_track_in(copy_of, false)?;
                d.put_entry(at, ch, Some(fresh as u8), None);
            }
            d.check_growth(before, "a new position")
        })?;
        Ok((map, changes.1))
    }

    /// Removes position `at`. The last position cannot go (the engine refuses a
    /// song with none). Its tracks stay in the table, unreferenced.
    pub fn delete_position(&mut self, at: usize) -> Res<(PositionMap, Changes)> {
        let len = self.song.positions.len();
        if at >= len {
            return Err(format!("There is no position {at} (the song has {len})."));
        }
        if len == 1 {
            return Err("A song needs at least one position.".into());
        }
        let map = PositionMap::Delete { at };
        let (_, c) = self.edit(|d| {
            d.change(Change::PositionRemove { index: at });
            d.remap_header(map, at);
            Ok(())
        })?;
        Ok((map, c))
    }

    /// Moves position `from` so that it ends up at index `to`.
    pub fn move_position(&mut self, from: usize, to: usize) -> Res<(PositionMap, Changes)> {
        let len = self.song.positions.len();
        for p in [from, to] {
            if p >= len {
                return Err(format!("There is no position {p} (the song has {len})."));
            }
        }
        let map = PositionMap::Move { from, to };
        if from == to {
            return Ok((map, Changes::default()));
        }
        let (_, c) = self.edit(|d| {
            let entry = d.song.positions[from].clone();
            d.change(Change::PositionRemove { index: from });
            d.change(Change::PositionInsert { index: to, entry });
            d.remap_header(map, to);
            Ok(())
        })?;
        Ok((map, c))
    }

    fn set_header(&mut self, f: impl FnOnce(&mut HeaderState) -> Res<()>) -> Res<Changes> {
        self.edit(|d| {
            let old = HeaderState::of(&d.song);
            let mut h = old.clone();
            f(&mut h)?;
            if h != old {
                d.change(Change::Header(Box::new(h)));
            }
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// The position the song starts over from when it reaches its end.
    pub fn set_restart(&mut self, position: usize) -> Res<Changes> {
        let len = self.song.positions.len();
        if position >= len {
            return Err(format!(
                "The restart position is 0 to {} (got {position}).",
                len.saturating_sub(1)
            ));
        }
        self.set_header(|h| {
            h.restart = position as u16;
            Ok(())
        })
    }

    /// The start positions of subsongs 1.. (an AHX or HVL file starts subsong 0
    /// at position 0).
    pub fn set_subsongs(&mut self, starts: &[usize]) -> Res<Changes> {
        let len = self.song.positions.len();
        if starts.len() > write::MAX_SUBSONGS {
            return Err(format!(
                "A song holds at most {} subsongs.",
                write::MAX_SUBSONGS
            ));
        }
        if let Some(bad) = starts.iter().find(|&&p| p >= len) {
            return Err(format!(
                "A subsong cannot start at position {bad} (the song has {len})."
            ));
        }
        let starts: Vec<u16> = starts.iter().map(|&p| p as u16).collect();
        self.set_header(|h| {
            h.subsongs = starts;
            Ok(())
        })
    }

    /// The song's name as the file can hold it (NUL removed, anything above
    /// U+00FF as `?`); the first value says it changed.
    pub fn set_name(&mut self, name: &str) -> Res<(bool, Changes)> {
        let (text, altered) = to_latin1(name);
        let c = self.set_header(|h| {
            h.name = text;
            Ok(())
        })?;
        Ok((altered, c))
    }

    /// 1 to 4: the engine ticks at `50 * value` Hz, so everything runs that
    /// much faster.
    pub fn set_speed_multiplier(&mut self, value: usize) -> Res<Changes> {
        if !(1..=4).contains(&value) {
            return Err(format!("The speed multiplier is 1 to 4 (got {value})."));
        }
        self.set_header(|h| {
            h.speed_multiplier = value as u8;
            Ok(())
        })
    }

    /// HVL's mix gain (percent, 0-255) and stereo preset (0-4). An AHX file has
    /// no room for them.
    pub fn set_mix(&mut self, mixgain: Option<u8>, defstereo: Option<u8>) -> Res<Changes> {
        if self.song.format != SongFormat::Hvl {
            return Err("AHX has no mix gain or stereo preset in its file (HVL has).".into());
        }
        if mixgain.is_none() && defstereo.is_none() {
            return Err("give 'mixgain' and/or 'defstereo'".into());
        }
        if defstereo.is_some_and(|d| d > 4) {
            return Err("The stereo preset is 0 to 4.".into());
        }
        self.set_header(|h| {
            if let Some(g) = mixgain {
                h.mixgain_raw = Some(g);
            }
            if let Some(d) = defstereo {
                h.defstereo = Some(d);
            }
            Ok(())
        })
    }
}
