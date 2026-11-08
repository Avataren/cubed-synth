//! Editing instruments: their fields, their envelopes and sweeps, their PLists.

use serde_json::Value;

use super::history::{Change, Changes};
use super::json::{instrument_from_op, PListUpdate};
use super::limits::{check_instrument, check_plist_entry, Res, PLIST_WAVEFORM_MAX};
use super::steps::PasteMode;
use super::AhxDoc;
use crate::format::{Envelope, Instrument, PList, PListEntry, SongFormat};
use crate::write::{self, to_latin1};

/// Which sweep `enable_sweep` turns on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepKind {
    Filter,
    Square,
}

/// What "New instrument" starts from: audible, with an envelope that goes up,
/// down and out (the attack and decay are never both 0 frames, which would make
/// the release ramp the wrong way), and one PList row that selects the sawtooth
/// wave and sets relative note 1, the key played (a row with note 0 keeps the
/// voice's pitch, which on a fresh channel is 0 and plays a semitone flat).
/// another_synth's `defaultAhxInstrument`.
pub fn default_instrument() -> Instrument {
    Instrument {
        name: "New instrument".into(),
        volume: 64,
        wave_length: 3,
        filter_lower_limit: 0,
        filter_upper_limit: 0,
        filter_speed: 0,
        square_lower_limit: 32,
        square_upper_limit: 63,
        square_speed: 1,
        vibrato_delay: 0,
        vibrato_speed: 0,
        vibrato_depth: 0,
        hard_cut_release: false,
        hard_cut_release_frames: 0,
        envelope: Envelope {
            a_frames: 1,
            a_volume: 64,
            d_frames: 10,
            d_volume: 48,
            s_frames: 0,
            r_frames: 10,
            r_volume: 0,
        },
        plist: PList {
            speed: 1,
            entries: vec![PListEntry {
                note: 1,
                waveform: 2,
                ..PListEntry::default()
            }],
        },
    }
}

/// The toggle command a sweep is turned on with: PList command 4 with this
/// parameter (square 0x00, filter 0x10, both heading up).
fn sweep_param(kind: SweepKind) -> u8 {
    match kind {
        SweepKind::Square => 0x00,
        SweepKind::Filter => 0x10,
    }
}

/// Whether a PList command 4 toggles `kind` on in `ins`, and whether a row
/// selects the square wave.
fn sweep_toggled(ins: &Instrument, kind: SweepKind, filter_dropped: bool) -> bool {
    ins.plist.entries.iter().any(|e| {
        (0..2).any(|k| {
            e.fx[k] == 4
                && match kind {
                    SweepKind::Square => e.fx_param[k] == 0 || e.fx_param[k] & 0x0f != 0,
                    SweepKind::Filter => !filter_dropped && e.fx_param[k] & 0xf0 != 0,
                }
        })
    })
}

impl AhxDoc {
    fn plist_len(&self, n: usize) -> Res<usize> {
        Ok(self.instrument_ref(n)?.plist.entries.len())
    }

    fn check_plist_row(&self, n: usize, row: usize) -> Res<()> {
        let len = self.plist_len(n)?;
        if row >= len {
            return Err(if len == 0 {
                "This PList has no rows.".to_string()
            } else {
                format!("There is no PList row {row}: this PList has {len}.")
            });
        }
        Ok(())
    }

    fn check_plist_rows(&self, n: usize, row0: usize, row1: usize) -> Res<()> {
        self.check_plist_row(n, row0)?;
        self.check_plist_row(n, row1)?;
        if row0 > row1 {
            return Err(format!("the range runs backwards (row {row0} to {row1})"));
        }
        Ok(())
    }

    fn check_instrument_fits(&self, ins: &Instrument) -> Res<()> {
        check_instrument(self.song.format, self.song.version, ins)
    }

    /// Replaces the fields of instrument `n` (not its PList rows), recording
    /// what was there.
    fn put_core(&mut self, n: usize, mut ins: Instrument) {
        ins.plist.entries.clear();
        let mut old = self.song.instruments[n].clone();
        old.plist.entries.clear();
        if old != ins {
            self.change(Change::InstrumentCore {
                index: n,
                ins: Box::new(ins),
            });
        }
    }

    /// Replaces all of instrument `n`: its fields and its PList.
    fn put_instrument(&mut self, n: usize, ins: Instrument) {
        let rows = ins.plist.entries.clone();
        self.put_core(n, ins);
        if self.song.instruments[n].plist.entries != rows {
            self.change(Change::PListAll {
                index: n,
                entries: rows,
            });
        }
    }

    /// Appends an instrument; returns its number. Refused at 63 instruments and
    /// when the file would no longer fit.
    pub fn add_instrument(&mut self, ins: Instrument) -> Res<(usize, Changes)> {
        self.check_instrument_fits(&ins)?;
        self.edit(|d| {
            let n = d.song.instruments.len();
            if n > write::MAX_INSTRUMENTS {
                return Err(format!(
                    "A song holds {} instruments at most.",
                    write::MAX_INSTRUMENTS
                ));
            }
            let mut ins = ins;
            ins.name = to_latin1(&ins.name).0;
            let before = d.used();
            d.change(Change::InstrumentPush { ins: Box::new(ins) });
            d.check_growth(before, "an instrument")?;
            Ok(n)
        })
    }

    /// `add_instrument` as an operation: the fields in `op` over
    /// [`default_instrument`].
    pub(crate) fn add_instrument_op(&mut self, op: &Value) -> Res<(usize, Changes)> {
        let ins = instrument_from_op(op, &default_instrument())?;
        self.add_instrument(ins)
    }

    /// Replaces instrument `n` whole (fields and PList). The name is kept as
    /// Latin-1.
    pub fn set_instrument(&mut self, n: usize, ins: Instrument) -> Res<Changes> {
        self.instrument_ref(n)?;
        self.check_instrument_fits(&ins)?;
        self.edit(|d| {
            let mut ins = ins;
            ins.name = to_latin1(&ins.name).0;
            let before = d.used();
            d.put_instrument(n, ins);
            d.check_growth(before, "this instrument")
        })
        .map(|(_, c)| c)
    }

    /// Sets the fields `op` names (see [`instrument_json`](super::instrument_json))
    /// of instrument `n`, leaving the rest and its PList rows alone.
    pub(crate) fn set_instrument_op(&mut self, n: usize, op: &Value) -> Res<Changes> {
        let base = self.instrument_ref(n)?.clone();
        let mut ins = instrument_from_op(op, &base)?;
        ins.name = to_latin1(&ins.name).0;
        self.check_instrument_fits(&ins)?;
        self.edit(|d| {
            d.put_core(n, ins);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Renames instrument `n`.
    pub fn rename_instrument(&mut self, n: usize, name: &str) -> Res<Changes> {
        let mut ins = self.instrument_ref(n)?.clone();
        ins.name = to_latin1(name).0;
        self.edit(|d| {
            d.put_core(n, ins);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Copies instrument `from` over instrument `to` (name and PList too), or,
    /// with `to` left out, to a new instrument at the end. Returns the number it
    /// landed on.
    pub fn copy_instrument(&mut self, from: usize, to: Option<usize>) -> Res<(usize, Changes)> {
        let ins = self.instrument_ref(from)?.clone();
        match to {
            None => self.add_instrument(ins),
            Some(to) => {
                self.instrument_ref(to)?;
                if to == from {
                    return Ok((to, Changes::default()));
                }
                self.set_instrument(to, ins).map(|c| (to, c))
            }
        }
    }

    /// Empties instrument `n`: silent, no PList, no name (the slot stays, so no
    /// step is renumbered).
    pub fn clear_instrument(&mut self, n: usize) -> Res<Changes> {
        self.instrument_ref(n)?;
        self.set_instrument(n, Instrument::default())
    }

    fn put_plist_rows(&mut self, n: usize, row0: usize, rows: Vec<PListEntry>) {
        let cur = &self.song.instruments[n].plist.entries;
        if cur[row0..row0 + rows.len()] != rows[..] {
            self.change(Change::PListRows {
                index: n,
                row0,
                entries: rows,
            });
        }
    }

    fn put_plist_all(&mut self, n: usize, rows: Vec<PListEntry>) -> Res<()> {
        if self.song.instruments[n].plist.entries == rows {
            return Ok(());
        }
        let before = self.used();
        self.change(Change::PListAll {
            index: n,
            entries: rows,
        });
        self.check_growth(before, "PList rows")
    }

    fn check_entry(&self, e: &PListEntry) -> Res<()> {
        check_plist_entry(self.song.format, self.song.version, e)
    }

    /// The PList rows `row0..=row1` of instrument `n`.
    pub fn copy_plist(&self, n: usize, row0: usize, row1: usize) -> Res<Vec<PListEntry>> {
        self.check_plist_rows(n, row0, row1)?;
        Ok(self.instrument_ref(n)?.plist.entries[row0..=row1].to_vec())
    }

    /// Changes (some fields of) PList row `row` of instrument `n`.
    pub fn set_plist_row(&mut self, n: usize, row: usize, update: PListUpdate) -> Res<Changes> {
        self.check_plist_row(n, row)?;
        let new = update.apply(&self.song.instruments[n].plist.entries[row]);
        self.check_entry(&new)?;
        self.edit(|d| {
            d.put_plist_rows(n, row, vec![new]);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Inserts `count` rows (`entry`, or empty ones) before row `row` (`row` =
    /// the row count appends). Later rows move down and Jump commands (`5xx`,
    /// absolute row numbers) keep their numbers: they now aim at other rows.
    pub fn insert_plist_rows(
        &mut self,
        n: usize,
        row: usize,
        count: usize,
        entry: Option<PListEntry>,
    ) -> Res<Changes> {
        let len = self.plist_len(n)?;
        if row > len {
            return Err(format!(
                "There is no place for a row at {row}: this PList has {len}."
            ));
        }
        if count == 0 {
            return Err("'count' must be at least 1".into());
        }
        if len + count > write::MAX_PLIST_ROWS {
            return Err(format!(
                "A PList has at most {} rows.",
                write::MAX_PLIST_ROWS
            ));
        }
        let e = entry.unwrap_or_default();
        self.check_entry(&e)?;
        self.edit(|d| {
            let mut rows = d.song.instruments[n].plist.entries.clone();
            rows.splice(row..row, std::iter::repeat_n(e, count));
            d.put_plist_all(n, rows)
        })
        .map(|(_, c)| c)
    }

    /// Deletes `count` rows from `row` (removing them all leaves an empty PList,
    /// which is legal).
    pub fn delete_plist_rows(&mut self, n: usize, row: usize, count: usize) -> Res<Changes> {
        self.check_plist_row(n, row)?;
        if count == 0 {
            return Err("'count' must be at least 1".into());
        }
        self.edit(|d| {
            let mut rows = d.song.instruments[n].plist.entries.clone();
            let end = (row + count).min(rows.len());
            rows.drain(row..end);
            d.put_plist_all(n, rows)
        })
        .map(|(_, c)| c)
    }

    /// A copy of row `row` right after it.
    pub fn duplicate_plist_row(&mut self, n: usize, row: usize) -> Res<Changes> {
        self.check_plist_row(n, row)?;
        let e = self.song.instruments[n].plist.entries[row];
        self.insert_plist_rows(n, row + 1, 1, Some(e))
    }

    /// Empties rows `row0..=row1` in place (the rows stay, so no later row
    /// moves). `columns` names what to clear (`"note"`, `"waveform"`, `"fixed"`,
    /// `"fx0"`, `"fx1"`; a command's parameter goes with it); none clears all.
    pub fn clear_plist_range(
        &mut self,
        n: usize,
        row0: usize,
        row1: usize,
        columns: &[&str],
    ) -> Res<Changes> {
        self.check_plist_rows(n, row0, row1)?;
        let mut mask = [false; 5];
        for c in columns {
            match *c {
                "note" => mask[0] = true,
                "waveform" | "tone" => mask[1] = true,
                "fixed" => mask[2] = true,
                "fx0" => mask[3] = true,
                "fx1" => mask[4] = true,
                other => {
                    return Err(format!(
                        "'{other}' is not a column (note, waveform, fixed, fx0, fx1)"
                    ))
                }
            }
        }
        if columns.is_empty() {
            mask = [true; 5];
        }
        self.edit(|d| {
            let new: Vec<PListEntry> = d.song.instruments[n].plist.entries[row0..=row1]
                .iter()
                .map(|e| {
                    let mut e = *e;
                    if mask[0] {
                        e.note = 0;
                    }
                    if mask[1] {
                        e.waveform = 0;
                    }
                    if mask[2] {
                        e.fixed = false;
                    }
                    if mask[3] {
                        e.fx[0] = 0;
                        e.fx_param[0] = 0;
                    }
                    if mask[4] {
                        e.fx[1] = 0;
                        e.fx_param[1] = 0;
                    }
                    e
                })
                .collect();
            d.put_plist_rows(n, row0, new);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Writes rows at `row` of instrument `n`'s PList; rows past its end are
    /// appended (a PList is as long as it is made; refused past 255 rows). A row
    /// the format cannot hold refuses the whole paste.
    pub fn paste_plist(
        &mut self,
        n: usize,
        row: usize,
        rows: &[PListEntry],
        mode: PasteMode,
    ) -> Res<Changes> {
        let len = self.plist_len(n)?;
        if row > len {
            return Err(format!(
                "There is no place for a row at {row}: this PList has {len}."
            ));
        }
        if row + rows.len() > write::MAX_PLIST_ROWS {
            return Err(format!(
                "A PList has at most {} rows.",
                write::MAX_PLIST_ROWS
            ));
        }
        for (i, e) in rows.iter().enumerate() {
            self.check_entry(e)
                .map_err(|e| format!("Row {i} of the clip: {e}"))?;
        }
        self.edit(|d| {
            let mut list = d.song.instruments[n].plist.entries.clone();
            for (i, c) in rows.iter().enumerate() {
                let new = match (mode, list.get(row + i)) {
                    (PasteMode::Mix, Some(cur)) => PListEntry {
                        note: if cur.note == 0 { c.note } else { cur.note },
                        waveform: if cur.waveform == 0 {
                            c.waveform
                        } else {
                            cur.waveform
                        },
                        fixed: cur.fixed || c.fixed,
                        fx: [
                            if cur.fx[0] == 0 && cur.fx_param[0] == 0 {
                                c.fx[0]
                            } else {
                                cur.fx[0]
                            },
                            if cur.fx[1] == 0 && cur.fx_param[1] == 0 {
                                c.fx[1]
                            } else {
                                cur.fx[1]
                            },
                        ],
                        fx_param: [
                            if cur.fx[0] == 0 && cur.fx_param[0] == 0 {
                                c.fx_param[0]
                            } else {
                                cur.fx_param[0]
                            },
                            if cur.fx[1] == 0 && cur.fx_param[1] == 0 {
                                c.fx_param[1]
                            } else {
                                cur.fx_param[1]
                            },
                        ],
                    },
                    _ => *c,
                };
                if row + i < list.len() {
                    list[row + i] = new;
                } else {
                    list.push(new);
                }
            }
            d.put_plist_all(n, list)
        })
        .map(|(_, c)| c)
    }

    /// Shifts the notes of PList rows `row0..=row1` (rows with a note) by
    /// `semitones`; a note leaving 1 to 63 refuses the edit unless `clamp`.
    /// Returns how many moved.
    pub fn transpose_plist(
        &mut self,
        n: usize,
        row0: usize,
        row1: usize,
        semitones: i32,
        clamp: bool,
    ) -> Res<(usize, Changes)> {
        self.check_plist_rows(n, row0, row1)?;
        self.edit(|d| {
            let mut new = d.song.instruments[n].plist.entries[row0..=row1].to_vec();
            let mut count = 0;
            for (i, e) in new.iter_mut().enumerate() {
                if e.note == 0 {
                    continue;
                }
                let v = e.note as i32 + semitones;
                let v = if (1..=63).contains(&v) {
                    v
                } else if clamp {
                    v.clamp(1, 63)
                } else {
                    return Err(format!(
                        "Transposing by {semitones} takes PList row {} outside 1 to 63.",
                        row0 + i
                    ));
                };
                if v != e.note as i32 {
                    e.note = v as u8;
                    count += 1;
                }
            }
            d.put_plist_rows(n, row0, new);
            Ok(count)
        })
    }

    /// Sets the waveform of PList row 0 (0 keeps the tone, 1 to 4 are triangle,
    /// sawtooth, square, noise), adding a row 0 to an empty PList: "the
    /// instrument's waveform" is the one its first row picks.
    pub fn set_start_waveform(&mut self, n: usize, waveform: usize) -> Res<Changes> {
        self.instrument_ref(n)?;
        if waveform > PLIST_WAVEFORM_MAX as usize {
            return Err(format!(
                "A PList tone is 0 to {PLIST_WAVEFORM_MAX} (got {waveform})."
            ));
        }
        self.edit(|d| {
            d.ensure_row0(n)?;
            let mut e = d.song.instruments[n].plist.entries[0];
            e.waveform = waveform as u8;
            d.put_plist_rows(n, 0, vec![e]);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    /// Sets (or, with 0, clears) the filter position PList row 0 carries: a
    /// command 0 with a position 1 to 63 in the slot that already holds one,
    /// else the first free slot, adding a row 0 to an empty PList. Refused when
    /// row 0 has no free command slot.
    pub fn set_start_filter(&mut self, n: usize, position: usize) -> Res<Changes> {
        self.instrument_ref(n)?;
        if position > 0x3f {
            return Err(format!("A filter position is 0 to 63 (got {position})."));
        }
        self.edit(|d| {
            d.ensure_row0(n)?;
            let mut e = d.song.instruments[n].plist.entries[0];
            let slot = (0..2)
                .find(|&k| e.fx[k] == 0 && e.fx_param[k] != 0)
                .or_else(|| (0..2).find(|&k| e.fx[k] == 0));
            let Some(slot) = slot else {
                return Err("PList row 0 has no free command slot for a filter position.".into());
            };
            e.fx_param[slot] = position as u8;
            d.put_plist_rows(n, 0, vec![e]);
            Ok(())
        })
        .map(|(_, c)| c)
    }

    fn ensure_row0(&mut self, n: usize) -> Res<()> {
        if self.song.instruments[n].plist.entries.is_empty() {
            self.insert_plist_rows_in(n, 0, PListEntry::default())?;
        }
        Ok(())
    }

    fn insert_plist_rows_in(&mut self, n: usize, row: usize, e: PListEntry) -> Res<()> {
        let mut rows = self.song.instruments[n].plist.entries.clone();
        rows.insert(row, e);
        self.put_plist_all(n, rows)
    }

    /// Turns a sweep on the way the engine only can: PList command 4 (a toggle)
    /// on row 0, in a free command slot. A square sweep also needs a row that
    /// selects the square wave, so row 0 is switched to it when none does.
    /// Nothing changes when the sweep is already toggled on; refused when row 0
    /// has no free slot, and for the filter in a version-0 AHX file (which drops
    /// the filter toggle's high nibble).
    pub fn enable_sweep(&mut self, n: usize, kind: SweepKind) -> Res<Changes> {
        let ins = self.instrument_ref(n)?;
        let filter_dropped = self.song.format == SongFormat::Ahx && self.song.version == 0;
        if kind == SweepKind::Filter && filter_dropped {
            return Err("A version-0 AHX file cannot toggle the filter sweep.".into());
        }
        let toggled = sweep_toggled(ins, kind, filter_dropped);
        if !toggled {
            if let Some(e) = ins.plist.entries.first() {
                if !(0..2).any(|k| e.fx[k] == 0 && e.fx_param[k] == 0) {
                    return Err(
                        "PList row 0 has no free command slot to turn the sweep on in.".into(),
                    );
                }
            }
        }
        self.edit(|d| {
            if !toggled {
                d.ensure_row0(n)?;
                let mut e = d.song.instruments[n].plist.entries[0];
                let slot = (0..2)
                    .find(|&k| e.fx[k] == 0 && e.fx_param[k] == 0)
                    .expect("checked above");
                e.fx[slot] = 4;
                e.fx_param[slot] = sweep_param(kind);
                d.put_plist_rows(n, 0, vec![e]);
            }
            let has_square = d.song.instruments[n]
                .plist
                .entries
                .iter()
                .any(|e| e.waveform == 3);
            if kind == SweepKind::Square && !has_square {
                d.ensure_row0(n)?;
                let mut e = d.song.instruments[n].plist.entries[0];
                e.waveform = 3;
                d.put_plist_rows(n, 0, vec![e]);
            }
            Ok(())
        })
        .map(|(_, c)| c)
    }
}
