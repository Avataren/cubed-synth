//! Editing the song: the order list, the header, the channels.

use super::history::{sample_meta, Change, Changes, ChannelState, HeaderState, OrderState};
use super::json::Res;
use super::limits::{clean_name, format_name, limits};
use super::ModuleDoc;
use crate::module::{Format, ModFlavor, Pattern};
use crate::write::{mod_period, s3m_nibble_pan};

/// A header flag a module can have set (which ones a format has is in
/// [`ModuleDoc::set_flag`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    /// XM: the linear frequency table (else the Amiga one).
    LinearFrequency,
    /// MOD: notes stay inside ProTracker's three octaves (the signature is
    /// then `M.K.`). S3M: the header's Amiga-limits flag.
    AmigaLimits,
    /// S3M: volume slides step on tick 0 too.
    FastVolumeSlides,
    /// MOD: the song runs off the 50 Hz vertical blank. The format cannot
    /// store this: it is derived from the song when a file is loaded.
    VblankTiming,
}

impl Flag {
    pub fn parse(name: &str) -> Option<Flag> {
        match name {
            "linear_frequency" => Some(Flag::LinearFrequency),
            "amiga_limits" => Some(Flag::AmigaLimits),
            "fast_volume_slides" => Some(Flag::FastVolumeSlides),
            "vblank_timing" => Some(Flag::VblankTiming),
            _ => None,
        }
    }
}

const S3M_LEFT: f32 = 51.0 / 255.0;
const S3M_RIGHT: f32 = 204.0 / 255.0;

/// The default pan of an S3M channel that has none set: alternating sides.
pub(crate) fn s3m_default_pan(channel: usize) -> f32 {
    if channel.is_multiple_of(2) {
        S3M_LEFT
    } else {
        S3M_RIGHT
    }
}

impl ModuleDoc {
    // ----- the order list -------------------------------------------------------------

    /// An S3M's `Bxx` counts the file's order list, separators included; the
    /// module's `jump_targets` maps those to `orders`. Before the list is
    /// edited the map is folded into the jumps themselves (each `Bxx` names a
    /// position of `orders`), so the list is the plain list from then on.
    fn plain_s3m_jumps(&mut self) -> Res<()> {
        let Some(map) = self.module.jump_targets.clone() else {
            return Ok(());
        };
        let identity =
            map.len() == self.module.orders.len() && map.iter().enumerate().all(|(i, &t)| i == t);
        if !identity {
            for p in 0..self.module.patterns.len() {
                let pattern = &self.module.patterns[p];
                let mut new = None;
                for row in 0..pattern.rows() {
                    for ch in 0..pattern.channels() {
                        let c = pattern.cell(row, ch);
                        if c.effect == 2 {
                            if let Some(&t) = map.get(c.param as usize) {
                                if t != c.param as usize {
                                    let n = new.get_or_insert_with(|| pattern.clone());
                                    n.cell_mut(row, ch).param = t.min(255) as u8;
                                }
                            }
                        }
                    }
                }
                if let Some(pattern) = new {
                    self.change(Change::Pattern { index: p, pattern });
                }
            }
        }
        Ok(())
    }

    /// Replaces the order list (and where an XM restarts).
    pub(crate) fn set_orders(&mut self, orders: Vec<usize>, restart: usize) -> Res<()> {
        let lim = limits(&self.module);
        if orders.is_empty() {
            return Err("a module needs at least one order".into());
        }
        if orders.len() > lim.max_orders {
            return Err(format!(
                "{} holds {} order positions",
                format_name(self.module.format),
                lim.max_orders
            ));
        }
        if let Some(&bad) = orders.iter().find(|&&o| o >= self.module.patterns.len()) {
            return Err(format!(
                "pattern {bad} does not exist (the module has {})",
                self.module.patterns.len()
            ));
        }
        let restart = if self.module.format == Format::Xm {
            restart.min(orders.len() - 1)
        } else {
            0
        };
        if self.module.format == Format::S3m {
            self.plain_s3m_jumps()?;
        }
        // An S3M's jumps count the file's order list: here it has no
        // separators, so the map is the identity.
        let jump_targets = (self.module.format == Format::S3m).then(|| (0..orders.len()).collect());
        self.change(Change::Orders(OrderState {
            orders,
            jump_targets,
            restart,
        }));
        Ok(())
    }

    /// Puts pattern `pattern` at order position `index`.
    pub fn set_order(&mut self, index: usize, pattern: usize) -> Res<Changes> {
        let mut orders = self.module.orders.clone();
        if index >= orders.len() {
            return Err(format!(
                "order {index} does not exist (the song has {} orders)",
                orders.len()
            ));
        }
        orders[index] = pattern;
        let restart = self.module.restart;
        let ((), c) = self.edit(|d| d.set_orders(orders, restart))?;
        Ok(c)
    }

    /// Inserts an order position before `index` (`index` equal to the length
    /// appends).
    pub fn insert_order(&mut self, index: usize, pattern: usize) -> Res<Changes> {
        let mut orders = self.module.orders.clone();
        if index > orders.len() {
            return Err(format!(
                "order {index} is past the end of the song ({} orders)",
                orders.len()
            ));
        }
        orders.insert(index, pattern);
        let restart = self.module.restart + usize::from(index <= self.module.restart);
        let ((), c) = self.edit(|d| d.set_orders(orders, restart))?;
        Ok(c)
    }

    /// Removes an order position (the song keeps at least one).
    pub fn delete_order(&mut self, index: usize) -> Res<Changes> {
        let mut orders = self.module.orders.clone();
        if index >= orders.len() {
            return Err(format!(
                "order {index} does not exist (the song has {} orders)",
                orders.len()
            ));
        }
        if orders.len() == 1 {
            return Err("a song needs at least one order".into());
        }
        orders.remove(index);
        let restart = self.module.restart - usize::from(index < self.module.restart);
        let ((), c) = self.edit(|d| d.set_orders(orders, restart))?;
        Ok(c)
    }

    /// Moves an order position.
    pub fn move_order(&mut self, from: usize, to: usize) -> Res<Changes> {
        let mut orders = self.module.orders.clone();
        if from >= orders.len() || to >= orders.len() {
            return Err(format!(
                "order {} does not exist (the song has {} orders)",
                from.max(to),
                orders.len()
            ));
        }
        let o = orders.remove(from);
        orders.insert(to, o);
        let r = self.module.restart;
        let restart = if r == from {
            to
        } else {
            let mut x = r;
            if from < r {
                x -= 1;
            }
            if to <= x {
                x += 1;
            }
            x
        };
        let ((), c) = self.edit(|d| d.set_orders(orders, restart))?;
        Ok(c)
    }

    /// Sets the order an XM loops back to at its end. A MOD or S3M has no
    /// such field (put a position jump, `Bxx`, in the last pattern).
    pub fn set_restart(&mut self, order: usize) -> Res<Changes> {
        if self.module.format != Format::Xm {
            return Err(format!(
                "{} cannot store a restart position: put a position jump (Bxx) at the end of the last pattern",
                format_name(self.module.format)
            ));
        }
        if order >= self.module.orders.len() {
            return Err(format!(
                "order {order} does not exist (the song has {} orders)",
                self.module.orders.len()
            ));
        }
        let orders = self.module.orders.clone();
        let ((), c) = self.edit(|d| d.set_orders(orders, order))?;
        Ok(c)
    }

    // ----- header ---------------------------------------------------------------------

    fn change_header(&mut self, f: impl FnOnce(&mut HeaderState)) {
        let mut h = HeaderState::of(&self.module);
        f(&mut h);
        self.change(Change::Header(Box::new(h)));
    }

    /// Sets the song's title (cut to the format's length). Returns it as
    /// stored.
    pub fn set_title(&mut self, title: &str) -> Res<(String, Changes)> {
        let (stored, _) = clean_name(title, limits(&self.module).max_title)?;
        let s = stored.clone();
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.title = s);
            Ok(())
        })?;
        Ok((stored, c))
    }

    /// The speed and tempo a MOD starts at: ProTracker's 6 and 125 unless the
    /// first row has an `Fxx`.
    pub(crate) fn mod_start_timing(&self) -> (u8, u8) {
        let m = &self.module;
        let (mut speed, mut tempo) = (6, 125);
        if let Some(p) = m.orders.first().and_then(|&o| m.patterns.get(o)) {
            for c in p.row(0) {
                if c.effect == 0x0f {
                    if (1..0x20).contains(&c.param) {
                        speed = c.param;
                    } else if c.param >= 0x20 {
                        tempo = c.param;
                    }
                }
            }
        }
        (speed, tempo)
    }

    /// A MOD's header has no speed or tempo: they are `Fxx` effects on the
    /// first row. Places (or replaces, or removes when it is the default) one.
    fn set_mod_start_effect(&mut self, tempo: bool, value: u8, default: u8) -> Res<()> {
        let p = self.module.orders[0];
        let pattern = &self.module.patterns[p];
        let class = |param: u8| {
            if tempo {
                param >= 0x20
            } else {
                (1..0x20).contains(&param)
            }
        };
        let existing = (0..pattern.channels()).find(|&c| {
            let cell = pattern.cell(0, c);
            cell.effect == 0x0f && class(cell.param)
        });
        let (channel, effect, param) = match (existing, value == default) {
            (Some(c), true) => (c, 0, 0),
            (None, true) => return Ok(()),
            (Some(c), false) => (c, 0x0f, value),
            (None, false) => {
                let free = (0..pattern.channels()).find(|&c| {
                    let cell = pattern.cell(0, c);
                    cell.effect == 0 && cell.param == 0
                });
                match free {
                    Some(c) => (c, 0x0f, value),
                    None => {
                        return Err(format!(
                            "a MOD's speed and tempo are Fxx effects on the first row, and row 0 of pattern {p} has no free effect"
                        ))
                    }
                }
            }
        };
        let mut cell = *pattern.cell(0, channel);
        cell.effect = effect;
        cell.param = param;
        self.change(Change::Block {
            pattern: p,
            row0: 0,
            channel0: channel,
            rows: 1,
            channels: 1,
            cells: vec![cell],
        });
        Ok(())
    }

    /// Sets the speed (ticks per row) the song starts at. A MOD has no header
    /// field for it: the speed becomes an `F` effect on the first row.
    pub fn set_speed(&mut self, speed: u8) -> Res<Changes> {
        let lim = limits(&self.module);
        if speed < lim.speed.0 || speed > lim.speed.1 {
            return Err(format!(
                "the speed of {} is {}-{}, not {speed}",
                format_name(self.module.format),
                lim.speed.0,
                lim.speed.1
            ));
        }
        let ((), c) = self.edit(|d| {
            if d.module.format == Format::Mod {
                d.set_mod_start_effect(false, speed, 6)
            } else {
                d.change_header(|h| h.speed = speed);
                Ok(())
            }
        })?;
        Ok(c)
    }

    /// Sets the tempo (beats per minute) the song starts at (see
    /// [`set_speed`](Self::set_speed) for a MOD).
    pub fn set_tempo(&mut self, tempo: u8) -> Res<Changes> {
        let lim = limits(&self.module);
        if tempo < lim.tempo.0 || tempo > lim.tempo.1 {
            return Err(format!(
                "the tempo of {} is {}-{}, not {tempo}",
                format_name(self.module.format),
                lim.tempo.0,
                lim.tempo.1
            ));
        }
        if self.module.format == Format::Mod && self.module.flags.vblank_timing {
            return Err(
                "this MOD runs off the vertical blank, where Fxx above 31 sets the speed, not the tempo".into(),
            );
        }
        let ((), c) = self.edit(|d| {
            if d.module.format == Format::Mod {
                d.set_mod_start_effect(true, tempo, 125)
            } else {
                d.change_header(|h| h.tempo = tempo);
                Ok(())
            }
        })?;
        Ok(c)
    }

    /// S3M: the global volume the song starts at, 0-64.
    pub fn set_global_volume(&mut self, volume: u8) -> Res<Changes> {
        if self.module.format != Format::S3m {
            return Err(format!(
                "{} has no global volume in its header: use the G effect (S3M: V)",
                format_name(self.module.format)
            ));
        }
        if volume > 64 {
            return Err(format!("the global volume is 0-64, not {volume}"));
        }
        if volume == 0 && self.module.tracker_version < 0x1320 {
            return Err(
                "an S3M from ST3 older than 3.20 reads a global volume of 0 as full".into(),
            );
        }
        let v = volume as f32 / 64.0;
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.global_volume = v);
            Ok(())
        })?;
        Ok(c)
    }

    /// Sets a header flag, where the format has it.
    pub fn set_flag(&mut self, flag: Flag, on: bool) -> Res<Changes> {
        let m = &self.module;
        let mut flags = m.flags;
        match (m.format, flag) {
            (Format::Xm, Flag::LinearFrequency) => flags.linear_frequency = on,
            (Format::S3m, Flag::AmigaLimits) => flags.amiga_limits = on,
            (Format::S3m, Flag::FastVolumeSlides) => {
                if !on && m.tracker_version == 0x1300 {
                    return Err("an ST3.00 module always slides volume on tick 0".into());
                }
                flags.fast_volume_slides = on;
            }
            (Format::Mod, Flag::AmigaLimits) => {
                if on {
                    if m.channels != 4 {
                        return Err(
                            "only a 4-channel MOD can be limited to ProTracker's octaves".into(),
                        );
                    }
                    for (p, pattern) in m.patterns.iter().enumerate() {
                        for row in 0..pattern.rows() {
                            for (ch, cell) in pattern.row(row).iter().enumerate() {
                                let period = mod_period(cell, true).unwrap_or(0);
                                if period != 0 && !(113..=856).contains(&period) {
                                    return Err(format!(
                                        "pattern {p}, row {row}, channel {}: period {period} is outside ProTracker's C-1 to B-3",
                                        ch + 1
                                    ));
                                }
                            }
                        }
                    }
                }
                flags.amiga_limits = on;
            }
            (Format::Mod, Flag::VblankTiming) => {
                if on
                    && (m.channels != 4
                        || !matches!(
                            m.mod_flavor,
                            Some(ModFlavor::ProTracker | ModFlavor::NoiseTracker)
                        ))
                {
                    return Err("vertical-blank timing is for 4-channel ProTracker modules".into());
                }
                flags.vblank_timing = on;
            }
            (f, flag) => return Err(format!("{} has no such flag ({flag:?})", format_name(f))),
        }
        let ((), c) = self.edit(|d| {
            d.change_header(|h| h.flags = flags);
            Ok(())
        })?;
        Ok(c)
    }

    /// MOD: converts a 15-sample Soundtracker or Ultimate Soundtracker module
    /// to ProTracker's 31-sample layout (or NoiseTracker's). Those layouts have
    /// no signature, so which of them a file is can only be guessed from its
    /// data and an edit can change the guess; ProTracker's `M.K.` is certain.
    /// Loops that began on an odd byte (Ultimate Soundtracker counts bytes)
    /// are moved down to a whole word; their sample numbers are returned.
    pub fn set_mod_flavor(&mut self, flavor: ModFlavor) -> Res<(Vec<usize>, Changes)> {
        let m = &self.module;
        if m.format != Format::Mod {
            return Err("only a MOD has a flavor".into());
        }
        if !matches!(flavor, ModFlavor::ProTracker | ModFlavor::NoiseTracker) {
            return Err("a module converts to ProTracker or NoiseTracker, not back to the 15-sample layouts".into());
        }
        if flavor == ModFlavor::NoiseTracker && m.channels != 4 {
            return Err("NoiseTracker modules have four channels".into());
        }
        let mut header = HeaderState::of(m);
        header.mod_flavor = Some(flavor);
        let mut adjusted = Vec::new();
        let mut metas = Vec::new();
        for (i, s) in m.samples.iter().enumerate() {
            if s.loop_kind != crate::module::LoopKind::Off && s.loop_start % 2 != 0 {
                let mut meta = sample_meta(s);
                meta.loop_start -= 1;
                meta.loop_len = meta.loop_len.min(s.data.len() - meta.loop_start) & !1;
                metas.push((i, meta));
                adjusted.push(i + 1);
            }
        }
        let have = m.samples.len();
        let ((), c) = self.edit(|d| {
            for (index, meta) in metas {
                d.change(Change::SampleMeta { index, meta });
            }
            for index in have..31 {
                d.change(Change::SampleInsert {
                    index,
                    sample: crate::module::Sample {
                        volume: 64,
                        ..Default::default()
                    },
                });
            }
            d.change(Change::Header(Box::new(header)));
            Ok(())
        })?;
        Ok((adjusted, c))
    }

    // ----- channels -------------------------------------------------------------------

    /// Gives the module `count` channels. Channels past the new count are
    /// dropped from every pattern: refused when they hold data unless `force`.
    pub fn set_channels(&mut self, count: usize, force: bool) -> Res<Changes> {
        let lim = limits(&self.module);
        let m = &self.module;
        if count < lim.min_channels || count > lim.max_channels {
            return Err(format!(
                "{} holds {}-{} channels, not {count}",
                format_name(m.format),
                lim.min_channels,
                lim.max_channels
            ));
        }
        if count == m.channels {
            return Ok(Changes::default());
        }
        if count < m.channels && !force {
            for (p, pattern) in m.patterns.iter().enumerate() {
                for row in 0..pattern.rows() {
                    for ch in count..m.channels {
                        if !pattern.cell(row, ch).is_empty() {
                            return Err(format!(
                                "channel {} has data (pattern {p}, row {row}) that {count} channels would drop (pass force to drop it)",
                                ch + 1
                            ));
                        }
                    }
                }
            }
        }
        let keep = count.min(m.channels);
        let mut patterns = Vec::with_capacity(m.patterns.len());
        for pattern in &m.patterns {
            let mut n = Pattern::new(pattern.rows(), count);
            for row in 0..pattern.rows() {
                for ch in 0..keep {
                    *n.cell_mut(row, ch) = *pattern.cell(row, ch);
                }
            }
            patterns.push(n);
        }
        let mut state = ChannelState::of(m);
        state.channels = count;
        state.muted.resize(count, false);
        state.adlib_channels.resize(count, None);
        match m.format {
            Format::Mod => {
                state.channel_pan = (0..count)
                    .map(|c| Some(crate::protracker::channel_pan(c, count)))
                    .collect();
            }
            Format::S3m => {
                let stereo = state
                    .channel_pan
                    .iter()
                    .zip(&state.adlib_channels)
                    .all(|(p, a)| p.is_some() || a.is_some())
                    || state.channel_pan.is_empty();
                let old = state.channel_pan.len();
                state.channel_pan.truncate(count);
                for c in old..count {
                    state.channel_pan.push(stereo.then(|| s3m_default_pan(c)));
                }
            }
            Format::Xm => state.channel_pan = vec![None; count],
        }
        let mut header = HeaderState::of(m);
        if m.format == Format::Mod {
            header.stereo_separation = if count <= 4 { 0.75 } else { 1.0 };
            if count != 4 {
                header.flags.amiga_limits = false;
                header.flags.vblank_timing = false;
                if header.mod_flavor == Some(ModFlavor::NoiseTracker) {
                    header.mod_flavor = Some(ModFlavor::ProTracker);
                }
            }
        }
        let ((), c) = self.edit(|d| {
            for (index, pattern) in patterns.into_iter().enumerate() {
                d.change(Change::Pattern { index, pattern });
            }
            d.change(Change::Channels(Box::new(state)));
            d.change(Change::Header(Box::new(header)));
            Ok(())
        })?;
        Ok(c)
    }

    /// S3M: one channel's pan (0-15, as the pan table holds it: 0 left, 15
    /// right) and mute. A MOD's channels are the Amiga's (left, right, right,
    /// left) and an XM's pans live in its samples and the 8xx effect.
    pub fn set_channel(
        &mut self,
        channel: usize,
        pan: Option<u8>,
        muted: Option<bool>,
    ) -> Res<Changes> {
        let m = &self.module;
        if m.format != Format::S3m {
            return Err(format!(
                "{} has no per-channel pan or mute in its header",
                format_name(m.format)
            ));
        }
        if channel >= m.channels {
            return Err(format!(
                "channel {channel} does not exist (the module has {}; channels count from 0)",
                m.channels
            ));
        }
        if m.adlib_channels[channel].is_some() && pan.is_some() {
            return Err("an AdLib channel has no pan".into());
        }
        if pan.is_some_and(|p| p > 15) {
            return Err("an S3M pan is 0-15".into());
        }
        let mut state = ChannelState::of(m);
        if let Some(p) = pan {
            state.channel_pan[channel] = Some(s3m_nibble_pan(p));
        }
        if let Some(mute) = muted {
            state.muted[channel] = mute;
        }
        let ((), c) = self.edit(|d| {
            d.change(Change::Channels(Box::new(state)));
            Ok(())
        })?;
        Ok(c)
    }
}
