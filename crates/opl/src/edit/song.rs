//! Editing the song: the pattern list, the order list and the header.

use super::history::{Change, Changes, HeaderState};
use super::json::Res;
use super::limits::{encode_text, text};
use super::A2mDoc;
use crate::a2::model::{Cell, Pattern};

/// A song flag (the song's flags byte, version 5 and up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    /// Bit 0: "update speed".
    UpdateSpeed,
    /// Bit 1: the track volume lock.
    TrackVolumeLock,
    /// Bit 2: the volume peak lock.
    VolumePeakLock,
    /// Bit 3: deep tremolo (4.8 dB; shallow is 1 dB).
    TremoloDepth,
    /// Bit 4: deep vibrato (14 cents; shallow is 7).
    VibratoDepth,
    /// Bit 5: the track panning lock.
    TrackPanningLock,
    /// Bit 6: the percussion track extension (rhythm mode on tracks 16-20).
    Percussion,
    /// Bit 7: volume scaling.
    VolumeScaling,
}

impl Flag {
    pub const ALL: [Flag; 8] = [
        Flag::UpdateSpeed,
        Flag::TrackVolumeLock,
        Flag::VolumePeakLock,
        Flag::TremoloDepth,
        Flag::VibratoDepth,
        Flag::TrackPanningLock,
        Flag::Percussion,
        Flag::VolumeScaling,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Flag::UpdateSpeed => "update_speed",
            Flag::TrackVolumeLock => "track_volume_lock",
            Flag::VolumePeakLock => "volume_peak_lock",
            Flag::TremoloDepth => "tremolo_depth",
            Flag::VibratoDepth => "vibrato_depth",
            Flag::TrackPanningLock => "track_panning_lock",
            Flag::Percussion => "percussion",
            Flag::VolumeScaling => "volume_scaling",
        }
    }

    pub fn parse(name: &str) -> Option<Flag> {
        Flag::ALL.into_iter().find(|f| f.name() == name)
    }

    pub fn bit(self) -> u8 {
        1 << Flag::ALL.iter().position(|&f| f == self).unwrap()
    }
}

/// Where the order list's first jump marker points: `0x80 + position`.
const MARKER: u8 = 0x80;

impl A2mDoc {
    // ----- the pattern list ----------------------------------------------------

    /// An empty pattern of the size this version stores.
    pub fn blank_pattern(&self) -> Pattern {
        let lim = self.limits();
        Pattern {
            rows: lim.storage_rows,
            channels: lim.storage_tracks,
            cells: vec![Cell::default(); lim.storage_rows * lim.storage_tracks],
        }
    }

    /// Keeps the unnamed patterns that fill the file's last block at the size
    /// the pattern count asks for.
    fn fix_spare(&mut self) {
        let per = self.limits().patterns_per_block;
        let n = self.song.patterns.len();
        let want = n.next_multiple_of(per) - n;
        if self.song.spare_patterns.len() == want {
            return;
        }
        let mut spare = self.song.spare_patterns.clone();
        while spare.len() > want {
            spare.remove(0);
        }
        while spare.len() < want {
            spare.push(self.blank_pattern());
        }
        self.change(Change::Spare(spare));
    }

    /// Appends an empty pattern.
    pub fn new_pattern(&mut self) -> Res<(usize, Changes)> {
        self.add_pattern(None)
    }

    /// Appends a copy of pattern `pattern`.
    pub fn duplicate_pattern(&mut self, pattern: usize) -> Res<(usize, Changes)> {
        let copy = self.pattern_ref(pattern)?.clone();
        self.add_pattern(Some(copy))
    }

    fn add_pattern(&mut self, with: Option<Pattern>) -> Res<(usize, Changes)> {
        let max = self.limits().max_patterns;
        let at = self.song.patterns.len();
        if at >= max {
            return Err(format!(
                "version {} holds {max} patterns",
                self.song.version
            ));
        }
        let pattern = with.unwrap_or_else(|| self.blank_pattern());
        let ((), c) = self.edit(|d| {
            d.change(Change::PatternInsert { index: at, pattern });
            d.fix_spare();
            Ok(())
        })?;
        Ok((at, c))
    }

    /// Deletes a pattern: the order positions that play it go and the later
    /// pattern numbers shift down (so do the names). A song keeps at least one
    /// pattern and one order.
    pub fn delete_pattern(&mut self, pattern: usize) -> Res<Changes> {
        self.pattern_ref(pattern)?;
        if self.song.patterns.len() == 1 {
            return Err("a song needs at least one pattern".into());
        }
        let (leading, restart, rest) = self.order_parts();
        let kept: Vec<usize> = (0..leading.len())
            .filter(|&i| leading[i] as usize != pattern)
            .collect();
        if kept.is_empty() {
            return Err(format!(
                "pattern {pattern} is the only one the order list plays: add another to the order list first"
            ));
        }
        let renumber = |p: u8| if p as usize > pattern { p - 1 } else { p };
        let new_leading: Vec<u8> = kept.iter().map(|&i| renumber(leading[i])).collect();
        // A marker's target is a position: positions shift with the ones removed before it.
        let removed_before = |t: usize| {
            leading[..t.min(leading.len())]
                .iter()
                .filter(|&&p| p as usize == pattern)
                .count()
        };
        let order = self.rebuild_order(new_leading, restart, rest, |t| t - removed_before(t))?;
        let names = self.limits().pattern_names.then(|| {
            let mut n = self.song.pattern_names.clone();
            if pattern < n.len() {
                n.remove(pattern);
                n.push(Vec::new());
            }
            n
        });
        let ((), c) = self.edit(|d| {
            d.change(Change::Order(Box::new(order)));
            d.change(Change::PatternRemove { index: pattern });
            d.fix_spare();
            if let Some(n) = names {
                d.change(Change::PatternNames(n));
            }
            Ok(())
        })?;
        Ok(c)
    }

    /// Sets the song's pattern length (every pattern of an A2M song has the same
    /// number of rows; versions before 9 are fixed at 64). Making it shorter over
    /// cells that hold something needs `force`, which clears them.
    pub fn set_pattern_len(&mut self, rows: usize, force: bool) -> Res<Changes> {
        let lim = self.limits();
        if rows < lim.min_rows || rows > lim.max_rows {
            return Err(if lim.min_rows == lim.max_rows {
                format!(
                    "version {} has {} rows in every pattern",
                    self.song.version, lim.min_rows
                )
            } else {
                format!(
                    "a pattern has {}-{} rows, not {rows}",
                    lim.min_rows, lim.max_rows
                )
            });
        }
        let old = self.pattern_len();
        if rows < old {
            let occupied = self.song.patterns.iter().any(|p| {
                (rows..old.min(p.rows))
                    .any(|r| (0..p.channels).any(|c| *p.cell(r, c) != Cell::default()))
            });
            if occupied && !force {
                return Err(format!(
                    "rows {rows} and later hold notes or effects: pass force to clear them"
                ));
            }
        }
        let ((), c) = self.edit(|d| {
            if rows < old {
                for p in 0..d.song.patterns.len() {
                    let (height, width) = (d.song.patterns[p].rows, d.song.patterns[p].channels);
                    let from = rows.min(height);
                    let to = old.min(height);
                    if from < to {
                        let cells: Vec<Cell> = vec![Cell::default(); (to - from) * width];
                        let current: Vec<Cell> = (from..to)
                            .flat_map(|r| (0..width).map(move |c| (r, c)))
                            .map(|(r, c)| *d.song.patterns[p].cell(r, c))
                            .collect();
                        if current != cells {
                            d.change(Change::Block {
                                pattern: p,
                                row0: from,
                                channel0: 0,
                                rows: to - from,
                                channels: width,
                                cells,
                            });
                        }
                    }
                }
            }
            d.change_header(|h| h.pattern_len = rows as u16);
            Ok(())
        })?;
        Ok(c)
    }

    /// Names a pattern (version 11 and up). Returns the name as stored.
    pub fn set_pattern_name(&mut self, pattern: usize, name: &str) -> Res<(String, Changes)> {
        self.pattern_ref(pattern)?;
        if !self.limits().pattern_names {
            return Err(format!(
                "version {} has no pattern names (they start with version 11)",
                self.song.version
            ));
        }
        let (bytes, _) = encode_text(name, 42);
        let stored = text(&bytes);
        let mut names = self.song.pattern_names.clone();
        if pattern >= names.len() {
            return Err(format!("pattern {pattern} has no name slot"));
        }
        names[pattern] = bytes;
        let ((), c) = self.edit(|d| {
            if names != d.song.pattern_names {
                d.change(Change::PatternNames(names));
            }
            Ok(())
        })?;
        Ok((stored, c))
    }

    // ----- the order list -------------------------------------------------------

    /// The list as the tracker shows it: the pattern numbers before the first
    /// jump marker, where that marker jumps to (`None` if the list is full and
    /// has none) and the entries after it.
    pub fn order_parts(&self) -> (Vec<u8>, Option<usize>, Vec<u8>) {
        let o = &self.song.order;
        match o.iter().position(|&e| e >= MARKER) {
            Some(m) => (
                o[..m].to_vec(),
                Some((o[m] - MARKER) as usize),
                o[m + 1..].to_vec(),
            ),
            None => (o.to_vec(), None, Vec::new()),
        }
    }

    /// The 128 bytes of an order list made of `leading` pattern numbers, a
    /// marker that jumps to `restart` and the old entries after the marker
    /// (their jump targets passed through `adjust`).
    pub(crate) fn rebuild_order(
        &self,
        leading: Vec<u8>,
        restart: Option<usize>,
        rest: Vec<u8>,
        adjust: impl Fn(usize) -> usize,
    ) -> Res<[u8; 128]> {
        if leading.is_empty() {
            return Err("a song needs at least one order".into());
        }
        if leading.len() > 128 {
            return Err("an order list holds 128 positions".into());
        }
        if let Some(&bad) = leading
            .iter()
            .find(|&&p| p as usize >= self.song.patterns.len())
        {
            return Err(format!(
                "pattern {bad} does not exist (the song has {})",
                self.song.patterns.len()
            ));
        }
        let mut out = leading;
        let target = |t: usize| (adjust(t)).min(127);
        let restart = target(restart.unwrap_or(0));
        let jump = MARKER + restart as u8;
        if out.len() < 128 {
            out.push(jump);
            for e in rest {
                if out.len() == 128 {
                    break;
                }
                out.push(if e >= MARKER {
                    MARKER + target((e - MARKER) as usize) as u8
                } else {
                    e
                });
            }
            out.resize(128, jump);
        }
        let mut a = [0u8; 128];
        a.copy_from_slice(&out);
        Ok(a)
    }

    fn put_order(&mut self, order: [u8; 128]) -> Res<()> {
        if order != self.song.order {
            self.change(Change::Order(Box::new(order)));
        }
        Ok(())
    }

    /// Puts pattern `pattern` at order position `index`.
    pub fn set_order(&mut self, index: usize, pattern: usize) -> Res<Changes> {
        let (mut leading, restart, rest) = self.order_parts();
        if index >= leading.len() {
            return Err(format!(
                "order {index} does not exist (the song has {} orders)",
                leading.len()
            ));
        }
        if pattern > 127 {
            return Err(format!(
                "pattern {pattern} is past the 128 an order can name"
            ));
        }
        leading[index] = pattern as u8;
        let order = self.rebuild_order(leading, restart, rest, |t| t)?;
        let ((), c) = self.edit(|d| d.put_order(order))?;
        Ok(c)
    }

    /// Inserts an order position before `index` (`index` equal to the length
    /// appends). The list holds 128 positions, the restart marker included
    /// unless the list is full.
    pub fn insert_order(&mut self, index: usize, pattern: usize) -> Res<Changes> {
        let (mut leading, restart, rest) = self.order_parts();
        if index > leading.len() {
            return Err(format!(
                "order {index} is past the end of the song ({} orders)",
                leading.len()
            ));
        }
        if leading.len() >= 128 {
            return Err("an order list holds 128 positions".into());
        }
        if pattern > 127 {
            return Err(format!(
                "pattern {pattern} is past the 128 an order can name"
            ));
        }
        leading.insert(index, pattern as u8);
        let order = self.rebuild_order(leading, restart, rest, |t| t + usize::from(t >= index))?;
        let ((), c) = self.edit(|d| d.put_order(order))?;
        Ok(c)
    }

    /// Removes an order position (the song keeps at least one).
    pub fn delete_order(&mut self, index: usize) -> Res<Changes> {
        let (mut leading, restart, rest) = self.order_parts();
        if index >= leading.len() {
            return Err(format!(
                "order {index} does not exist (the song has {} orders)",
                leading.len()
            ));
        }
        if leading.len() == 1 {
            return Err("a song needs at least one order".into());
        }
        leading.remove(index);
        let order = self.rebuild_order(leading, restart, rest, |t| t - usize::from(t > index))?;
        let ((), c) = self.edit(|d| d.put_order(order))?;
        Ok(c)
    }

    /// Moves an order position.
    pub fn move_order(&mut self, from: usize, to: usize) -> Res<Changes> {
        let (mut leading, restart, rest) = self.order_parts();
        if from >= leading.len() || to >= leading.len() {
            return Err(format!(
                "order {} does not exist (the song has {} orders)",
                from.max(to),
                leading.len()
            ));
        }
        let o = leading.remove(from);
        leading.insert(to, o);
        // Where a position ends up.
        let map = |t: usize| {
            if t == from {
                to
            } else if from < to && t > from && t <= to {
                t - 1
            } else if to < from && t >= to && t < from {
                t + 1
            } else {
                t
            }
        };
        let order = self.rebuild_order(leading, restart, rest, map)?;
        let ((), c) = self.edit(|d| d.put_order(order))?;
        Ok(c)
    }

    /// Replaces the order list: `orders` (pattern numbers) and where the song
    /// jumps back to at its end.
    pub fn set_orders(&mut self, orders: Vec<usize>, restart: usize) -> Res<Changes> {
        if orders.iter().any(|&p| p > 127) {
            return Err("an order can name patterns 0-127".into());
        }
        if restart >= orders.len().max(1) {
            return Err(format!(
                "the restart position {restart} is past the song's {} orders",
                orders.len()
            ));
        }
        let (_, _, rest) = self.order_parts();
        let order = self.rebuild_order(
            orders.into_iter().map(|p| p as u8).collect(),
            Some(restart),
            rest,
            |t| t,
        )?;
        let ((), c) = self.edit(|d| d.put_order(order))?;
        Ok(c)
    }

    /// Sets the order the song jumps back to at its end (the first jump marker).
    pub fn set_restart(&mut self, order: usize) -> Res<Changes> {
        let (leading, restart, _) = self.order_parts();
        let Some(old) = restart else {
            return Err(
                "the order list is full (128 positions): there is no room for a restart marker"
                    .into(),
            );
        };
        if order >= leading.len() {
            return Err(format!(
                "order {order} does not exist (the song has {} orders)",
                leading.len()
            ));
        }
        let mut new = self.song.order;
        for e in new[leading.len()..].iter_mut() {
            if *e == MARKER + old as u8 {
                *e = MARKER + order as u8;
            }
        }
        let ((), c) = self.edit(|d| d.put_order(new))?;
        Ok(c)
    }

    // ----- header ---------------------------------------------------------------

    pub(crate) fn change_header(&mut self, f: impl FnOnce(&mut HeaderState)) {
        let mut h = HeaderState::of(&self.song);
        f(&mut h);
        self.change(Change::Header(Box::new(h)));
    }

    /// Sets the song's title (CP437, cut to 42 characters). Returns it as stored.
    pub fn set_title(&mut self, title: &str) -> Res<(String, Changes)> {
        let (bytes, _) = encode_text(title, 42);
        let stored = text(&bytes);
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.name = bytes);
            Ok(())
        })?;
        Ok((stored, c))
    }

    /// Sets the composer's name (CP437, cut to 42 characters).
    pub fn set_composer(&mut self, composer: &str) -> Res<(String, Changes)> {
        let (bytes, _) = encode_text(composer, 42);
        let stored = text(&bytes);
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.composer = bytes);
            Ok(())
        })?;
        Ok((stored, c))
    }

    /// Sets the tempo (timer ticks per second, 1-255; below 19 the timer runs at 18.2 Hz).
    pub fn set_tempo(&mut self, tempo: u8) -> Res<Changes> {
        if tempo == 0 {
            return Err("the tempo is 1-255".into());
        }
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.tempo = tempo);
            Ok(())
        })?;
        Ok(c)
    }

    /// Sets the speed (timer ticks per row, 1-255).
    pub fn set_speed(&mut self, speed: u8) -> Res<Changes> {
        if speed == 0 {
            return Err("the speed is 1-255".into());
        }
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.speed = speed);
            Ok(())
        })?;
        Ok(c)
    }

    fn need(&self, ok: bool, what: &str, from: u8) -> Res<()> {
        if ok {
            Ok(())
        } else {
            Err(format!(
                "version {} has no {what} (it starts with version {from})",
                self.song.version
            ))
        }
    }

    /// Sets or clears a song flag (version 5 and up).
    pub fn set_flag(&mut self, flag: Flag, on: bool) -> Res<Changes> {
        self.need(self.limits().flags, "song flags", 5)?;
        let ((), c) = self.edit(|d| {
            d.change_header(|h| {
                if on {
                    h.flags |= flag.bit();
                } else {
                    h.flags &= !flag.bit();
                }
            });
            Ok(())
        })?;
        Ok(c)
    }

    /// Sets the number of tracks (version 9 and up: 1-20). Dropping tracks that
    /// hold notes or effects needs `force`, which clears them.
    pub fn set_tracks(&mut self, count: usize, force: bool) -> Res<Changes> {
        let lim = self.limits();
        if lim.min_tracks == lim.max_tracks {
            return Err(format!(
                "version {} has {} tracks (the count is free from version 9)",
                self.song.version, lim.max_tracks
            ));
        }
        if !(lim.min_tracks..=lim.max_tracks).contains(&count) {
            return Err(format!(
                "a song has {}-{} tracks, not {count}",
                lim.min_tracks, lim.max_tracks
            ));
        }
        let old = self.tracks();
        if count < old {
            let occupied = self.song.patterns.iter().any(|p| {
                (0..self.pattern_len().min(p.rows))
                    .any(|r| (count..old.min(p.channels)).any(|c| *p.cell(r, c) != Cell::default()))
            });
            if occupied && !force {
                return Err(format!(
                    "tracks {} and later hold notes or effects: pass force to clear them",
                    count + 1
                ));
            }
        }
        let rows = self.pattern_len();
        let ((), c) = self.edit(|d| {
            if count < old {
                for p in 0..d.song.patterns.len() {
                    let width = d.song.patterns[p].channels;
                    let to = old.min(width);
                    if count < to {
                        let n = to - count;
                        let rows = rows.min(d.song.patterns[p].rows);
                        let current: Vec<Cell> = (0..rows)
                            .flat_map(|r| (count..to).map(move |c| (r, c)))
                            .map(|(r, c)| *d.song.patterns[p].cell(r, c))
                            .collect();
                        let cells = vec![Cell::default(); rows * n];
                        if current != cells {
                            d.change(Change::Block {
                                pattern: p,
                                row0: 0,
                                channel0: count,
                                rows,
                                channels: n,
                                cells,
                            });
                        }
                    }
                }
            }
            d.change_header(|h| h.tracks = count as u8);
            Ok(())
        })?;
        Ok(c)
    }

    /// Sets the macro speed-up factor (version 9 and up, 1-255): macros and the
    /// timer run this many times faster than the rows.
    pub fn set_macro_speedup(&mut self, factor: u16) -> Res<Changes> {
        self.need(self.limits().macros, "macro speed-up", 9)?;
        if !(1..=255).contains(&factor) {
            return Err(format!("the macro speed-up is 1-255, not {factor}"));
        }
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.macro_speedup = factor);
            Ok(())
        })?;
        Ok(c)
    }

    /// Joins or splits the 4-op track pairs (version 10 and up): bit `n` of
    /// `mask` for pair `n` (tracks 1+2, 3+4, 5+6, 10+11, 12+13, 14+15).
    pub fn set_four_op_tracks(&mut self, mask: u8) -> Res<Changes> {
        self.need(self.limits().four_op_tracks, "4-op track pairs", 10)?;
        if mask > 0x3f {
            return Err("the 4-op pairs are bits 0-5".into());
        }
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.four_op_tracks = mask);
            Ok(())
        })?;
        Ok(c)
    }

    /// Sets one track's initial lock flags (version 10 and up): bits 0-1 the
    /// panning (0 centre, 1 left, 2 right), 2-3 the volume slide type (0 default,
    /// 1 carrier, 2 modulator, 3 both), 4 the volume lock, 5 the peak lock,
    /// 6 the 4-op volume lock (version 13 and up).
    pub fn set_lock_flags(&mut self, track: usize, value: u8) -> Res<Changes> {
        self.need(self.limits().four_op_tracks, "track lock flags", 10)?;
        if track >= 20 {
            return Err(format!("track {track} does not exist (0-19)"));
        }
        if value & 0x80 != 0 {
            return Err("the lock flags use bits 0-6".into());
        }
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.lock_flags[track] = value);
            Ok(())
        })?;
        Ok(c)
    }

    /// Sets rows per beat (version 14 only: the editor's beat marking).
    pub fn set_rows_per_beat(&mut self, rows: u8) -> Res<Changes> {
        self.need(self.limits().beat, "rows per beat", 14)?;
        if rows == 0 {
            return Err("rows per beat is 1-255".into());
        }
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.rows_per_beat = Some(rows));
            Ok(())
        })?;
        Ok(c)
    }

    /// Sets the tempo fine-tune (version 14 only).
    pub fn set_tempo_finetune(&mut self, finetune: i16) -> Res<Changes> {
        self.need(self.limits().beat, "tempo fine-tune", 14)?;
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.tempo_finetune = Some(finetune));
            Ok(())
        })?;
        Ok(c)
    }

    /// Marks instrument pair `pair` (1-128) as a 4-op instrument (version 12 and
    /// up) and keeps the pair count in step.
    pub fn set_four_op_instrument(&mut self, pair: usize, on: bool) -> Res<Changes> {
        self.need(self.limits().four_op_instruments, "4-op instruments", 12)?;
        if !(1..=128).contains(&pair) {
            return Err(format!("an instrument pair is numbered 1-128, not {pair}"));
        }
        let mut flags = self.song.four_op_instruments.clone();
        if flags.len() != 129 {
            return Err("the song's 4-op instrument table is damaged".into());
        }
        flags[pair] = on as u8;
        flags[0] = flags[1..].iter().filter(|&&f| f != 0).count() as u8;
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.four_op_instruments = flags);
            Ok(())
        })?;
        Ok(c)
    }
}
