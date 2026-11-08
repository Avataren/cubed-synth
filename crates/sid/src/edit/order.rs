//! Orderlists and subsongs: per voice, the patterns it plays in order, each with
//! a transpose and a repeat; and the list of subsongs.

use super::history::Changes;
use super::json::Res;
use super::SidDoc;
use crate::song::{
    OrderEntry, Orderlist, Pattern, Row, Subsong, MAX_ORDER_ENTRIES, MAX_PATTERNS,
    MAX_PATTERN_ROWS, MAX_REPEAT, MAX_SUBSONGS, MAX_TRANSPOSE, MIN_TRANSPOSE, SID_CHANNELS,
};

/// The fields of an orderlist entry an edit changes (`None` leaves a field alone).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OrderUpdate {
    pub pattern: Option<u8>,
    pub transpose: Option<i8>,
    pub repeat: Option<u8>,
}

impl SidDoc {
    fn check_entry(&self, e: &OrderEntry) -> Res<()> {
        if e.pattern as usize >= self.song.patterns.len() {
            return Err(format!(
                "pattern {} does not exist (the song has {})",
                e.pattern,
                self.song.patterns.len()
            ));
        }
        if !(MIN_TRANSPOSE..=MAX_TRANSPOSE).contains(&e.transpose) {
            return Err(format!(
                "a transpose is {MIN_TRANSPOSE}..{MAX_TRANSPOSE} semitones (a .sng holds {}..{})",
                crate::goattracker::GT_MIN_TRANSPOSE,
                crate::goattracker::GT_MAX_TRANSPOSE
            ));
        }
        if !(1..=MAX_REPEAT).contains(&e.repeat) {
            return Err(format!("a repeat is 1-{MAX_REPEAT}"));
        }
        Ok(())
    }

    /// Sets the fields of entry `index` of a voice's orderlist.
    pub fn set_order(
        &mut self,
        subsong: usize,
        voice: usize,
        index: usize,
        update: OrderUpdate,
    ) -> Res<Changes> {
        let list = self.list_ref(subsong, voice)?;
        let old = *list.entries.get(index).ok_or_else(|| {
            format!(
                "subsong {subsong} voice {voice} has {} entries: there is no entry {index}",
                list.entries.len()
            )
        })?;
        let new = OrderEntry {
            pattern: update.pattern.unwrap_or(old.pattern),
            transpose: update.transpose.unwrap_or(old.transpose),
            repeat: update.repeat.unwrap_or(old.repeat),
        };
        self.check_entry(&new)?;
        let mut list = list.clone();
        list.entries[index] = new;
        self.edit(|d| {
            d.put_orderlist(subsong, voice, list);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Inserts an entry before `index` (`index` = the list's length appends). The
    /// restart stays on the entry it was on.
    pub fn insert_order(
        &mut self,
        subsong: usize,
        voice: usize,
        index: usize,
        entry: OrderEntry,
    ) -> Res<Changes> {
        let list = self.list_ref(subsong, voice)?;
        if index > list.entries.len() {
            return Err(format!(
                "subsong {subsong} voice {voice} has {} entries: no place {index} to insert at",
                list.entries.len()
            ));
        }
        if list.entries.len() >= MAX_ORDER_ENTRIES {
            return Err(format!(
                "an orderlist has at most {MAX_ORDER_ENTRIES} entries"
            ));
        }
        self.check_entry(&entry)?;
        let mut list = list.clone();
        list.entries.insert(index, entry);
        if index <= list.restart as usize {
            list.restart += 1;
        }
        self.edit(|d| {
            d.put_orderlist(subsong, voice, list);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Deletes entry `index` (a list keeps at least one). The restart stays on the
    /// entry it was on, or moves to the one that took the place of a deleted one.
    pub fn delete_order(&mut self, subsong: usize, voice: usize, index: usize) -> Res<Changes> {
        let list = self.list_ref(subsong, voice)?;
        if index >= list.entries.len() {
            return Err(format!(
                "subsong {subsong} voice {voice} has {} entries: there is no entry {index}",
                list.entries.len()
            ));
        }
        if list.entries.len() == 1 {
            return Err("an orderlist keeps at least one entry".into());
        }
        let mut list = list.clone();
        list.entries.remove(index);
        let restart = list.restart as usize;
        if index < restart {
            list.restart -= 1;
        }
        list.restart = list.restart.min(list.entries.len() as u8 - 1);
        self.edit(|d| {
            d.put_orderlist(subsong, voice, list);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Moves entry `from` to place `to` (the place it has in the list afterwards).
    /// The restart follows the entry it was on.
    pub fn move_order(
        &mut self,
        subsong: usize,
        voice: usize,
        from: usize,
        to: usize,
    ) -> Res<Changes> {
        let list = self.list_ref(subsong, voice)?;
        let n = list.entries.len();
        if from >= n || to >= n {
            return Err(format!(
                "subsong {subsong} voice {voice} has {n} entries: {from} and {to} must be inside them"
            ));
        }
        let mut list = list.clone();
        let restart_on = list.restart as usize;
        let entry = list.entries.remove(from);
        list.entries.insert(to, entry);
        // Where the entry the restart pointed at went.
        let at = |i: usize| {
            if i == from {
                to
            } else {
                let after_remove = if i > from { i - 1 } else { i };
                if after_remove >= to {
                    after_remove + 1
                } else {
                    after_remove
                }
            }
        };
        list.restart = at(restart_on) as u8;
        self.edit(|d| {
            d.put_orderlist(subsong, voice, list);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Sets the entry a voice loops back to after its last (GoatTracker's `RST`).
    pub fn set_restart(&mut self, subsong: usize, voice: usize, restart: usize) -> Res<Changes> {
        let list = self.list_ref(subsong, voice)?;
        if restart >= list.entries.len() {
            return Err(format!(
                "subsong {subsong} voice {voice} has {} entries: it cannot restart at {restart}",
                list.entries.len()
            ));
        }
        let mut list = list.clone();
        list.restart = restart as u8;
        self.edit(|d| {
            d.put_orderlist(subsong, voice, list);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Appends a subsong and returns its number: a copy of subsong `copy_of`, or a
    /// new one whose voices each play a blank pattern of `rows` rows (three new
    /// patterns).
    pub fn add_subsong(&mut self, copy_of: Option<usize>, rows: usize) -> Res<(usize, Changes)> {
        let at = self.song.subsongs.len();
        if at >= MAX_SUBSONGS {
            return Err(format!(
                "the song has {MAX_SUBSONGS} subsongs, GoatTracker's most"
            ));
        }
        let copy = match copy_of {
            Some(s) => {
                self.list_ref(s, 0)?;
                Some(self.song.subsongs[s].clone())
            }
            None => {
                if !(1..=MAX_PATTERN_ROWS).contains(&rows) {
                    return Err(format!("a pattern has 1-{MAX_PATTERN_ROWS} rows"));
                }
                if self.song.patterns.len() + SID_CHANNELS > MAX_PATTERNS {
                    return Err(format!(
                        "a new subsong needs {SID_CHANNELS} new patterns and the song has {} of {MAX_PATTERNS}",
                        self.song.patterns.len()
                    ));
                }
                None
            }
        };
        let (_, changes) = self.edit(|d| {
            let subsong = match copy {
                Some(s) => s,
                None => {
                    let base = d.song.patterns.len();
                    for _ in 0..SID_CHANNELS {
                        let n = d.song.patterns.len();
                        d.insert_pattern_at(
                            n,
                            Pattern {
                                rows: vec![Row::default(); rows],
                            },
                        );
                    }
                    Subsong {
                        orderlists: (0..SID_CHANNELS)
                            .map(|v| Orderlist {
                                entries: vec![OrderEntry {
                                    pattern: (base + v) as u8,
                                    transpose: 0,
                                    repeat: 1,
                                }],
                                restart: 0,
                            })
                            .collect(),
                    }
                }
            };
            d.insert_subsong_at(at, subsong);
            Ok(())
        })?;
        Ok((at, changes))
    }

    /// Removes a subsong (a song keeps at least one). Its patterns stay.
    pub fn remove_subsong(&mut self, subsong: usize) -> Res<Changes> {
        self.list_ref(subsong, 0)?;
        if self.song.subsongs.len() == 1 {
            return Err("a song keeps at least one subsong".into());
        }
        let shown = self.subsong;
        let (_, changes) = self.edit(|d| {
            d.remove_subsong_at(subsong);
            Ok(())
        })?;
        // The subsong shown stays the same one when an earlier one goes.
        self.subsong = if subsong < shown { shown - 1 } else { shown };
        self.clamp_subsong();
        Ok(changes)
    }
}
