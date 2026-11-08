//! `A2Player`: an Adlib Tracker II module played end to end, the shape of
//! `ahx/player.rs` and `sid/wasm.rs` (plan-opl.md O7, step 3). The engine
//! (`engine.rs`) writes registers straight into its own `Chip`, ticked at
//! the song's timer rate in native chip samples, and the resampler turns
//! the chip's ≈ 49 716 Hz into the output rate.
//!
//! Play follows Adlib Tracker II where the register gate showed AdPlug
//! departing from it (`A2Engine::adplug_quirks` off); see the O7 record.
//!
//! Seeking replays the song from the top without clocking the chip until
//! the target row starts, so every register (instrument, volume, pitch,
//! tables) is as the song leaves it there; envelopes restart from the
//! registers. Muting works on OPL channels: a track is muted by muting the
//! channel it plays on, which in percussion mode two drum tracks share.
//!
//! The wasm-bindgen attributes are `cfg_attr`-gated as in `opl/wasm.rs`, so
//! the class compiles and is tested natively.

use super::engine::{A2Engine, RegisterSink};
use super::live::LiveEdit;
use super::model::{cp437, parse, A2mSong};
use crate::chip::{Chip, CHANNELS, NATIVE_RATE};
use crate::resample::Resampler;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

impl RegisterSink for Chip {
    fn write(&mut self, reg: u16, val: u8) {
        Chip::write(self, reg, val);
    }
}

/// One operator's full swing reads ±1 on a tap (as `OplRenderer`'s).
const TAP_SCALE: f32 = 1.0 / 8192.0;
/// Full scale of the chip's 16-bit output.
const FULL_SCALE: f32 = 1.0 / 32768.0;
/// Most ticks a seek replays before it gives up finding its row by playing
/// and jumps there directly (about 40 minutes at 50 Hz).
const SEEK_TICK_LIMIT: usize = 120_000;
/// How fast a track's level falls once its sound is gone (24 dB a second, as
/// the tracker `Probe`'s do).
const LEVEL_FALL_DB_PER_SECOND: f64 = 24.0;

/// What the transport repeats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum A2Loop {
    /// Play on through the song.
    #[default]
    None,
    /// Repeat the pattern at this order position.
    Pattern(usize),
    /// Repeat rows `row0` to `row1` (both played) of the pattern at `order`.
    Range {
        order: usize,
        row0: usize,
        row1: usize,
    },
}

/// Where the song is, as `A2Player::status` reads it: what a position display
/// and a follower need.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct A2Status {
    pub playing: bool,
    /// The order position, pattern and row playing now (or, before the first
    /// row, the first to play).
    pub order: u16,
    pub pattern: u16,
    pub row: u16,
    /// The timer tick of the row, from 0.
    pub tick: u16,
    /// Timer ticks per row now.
    pub speed: u8,
    /// The tempo now; the timer runs at `refresh` Hz.
    pub tempo: u8,
    pub refresh: f32,
    /// Rows started since the player was made (a seek adds one, a repeated row
    /// counts): a follower that polls slower than the song can tell how many it
    /// missed.
    pub rows: u32,
    /// Output frames rendered while playing.
    pub frames: u64,
    /// Times the song has come round (passed an order marker).
    pub loops: u32,
    /// The song has stopped (its order list ran out of patterns).
    pub ended: bool,
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct A2Player {
    song: A2mSong,
    engine: A2Engine,
    chip: Chip,
    resampler: Resampler,
    out_rate: f64,
    playing: bool,
    gain: f32,
    /// Native samples until the next timer tick.
    until_tick: f64,
    loop_mode: A2Loop,
    end_reached: bool,
    mute: u32,
    solo: u32,
    taps_enabled: bool,
    taps: Vec<f32>,
    tap_frames: usize,
    /// Per-channel peak levels (`set_levels_enabled`): what they are now, and
    /// the loudest frame of the block being rendered.
    levels_enabled: bool,
    levels: [f32; CHANNELS],
    block_peaks: [f32; CHANNELS],
    /// Output frames rendered while playing, and the rows counter's offset.
    frames: u64,
    rows_base: u32,
    /// A tick's register writes reach the chip one per native sample (about
    /// 20 µs apart) instead of all at once. AT2 writes the ports one after
    /// another, so a key-off followed by a key-on in the same tick is seen by
    /// the envelope and the note restarts; applied at one instant it is not,
    /// and the note runs on. MEASURED (.ai/plan-opl.md O7 record): against
    /// NAB622's SB16 recording of Corridors of Time the spaced writes follow
    /// the loudness contour better (20 ms envelope r 0.60 vs 0.51) and the
    /// spectrum far better (third-octave shape σ 2.1 vs 3.7 dB); one and two
    /// samples apart measure the same. AdPlug's Nuked OPL3 output agrees
    /// (per-channel envelopes r ≈ 1.00 once spaced, 0.14 when not).
    /// Here: the current tick's writes, and the next one to apply.
    writes: Vec<(u16, u8)>,
    next_write: usize,
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
impl A2Player {
    /// Parses an `.a2m` module and builds a paused player at its start. A
    /// file the parser or the player cannot play is refused with one
    /// sentence saying why.
    #[cfg_attr(feature = "wasm", wasm_bindgen(constructor))]
    pub fn new(bytes: &[u8], sample_rate: f64) -> Result<A2Player, String> {
        let song = parse(bytes).map_err(|e| format!("Cannot read this A2M module: {e}"))?;
        if let Some(why) = A2Engine::refusal(&song) {
            return Err(format!("Cannot play this A2M module: {why}"));
        }
        let mut chip = Chip::new();
        let mut engine = A2Engine::new(song.clone());
        engine.reset(&mut chip);
        Ok(A2Player {
            song,
            engine,
            chip,
            resampler: Resampler::new(NATIVE_RATE, sample_rate),
            out_rate: sample_rate,
            playing: false,
            gain: 1.0,
            until_tick: 0.0,
            loop_mode: A2Loop::None,
            end_reached: false,
            mute: 0,
            solo: 0,
            taps_enabled: false,
            taps: Vec::new(),
            tap_frames: 0,
            levels_enabled: false,
            levels: [0.0; CHANNELS],
            block_peaks: [0.0; CHANNELS],
            frames: 0,
            rows_base: 0,
            writes: Vec::new(),
            next_write: 0,
        })
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Stops the clock (the output is silence) without losing the position.
    pub fn pause(&mut self) {
        self.playing = false;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn set_gain(&mut self, gain: f32) {
        self.gain = gain;
    }

    /// Renders `left.len()` frames (and the same into `right`); silence
    /// while paused. Returns the frames written.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        let n = left.len().min(right.len());
        let taps_on = self.taps_enabled && self.playing;
        let levels_on = self.levels_enabled && self.playing;
        if self.taps_enabled {
            self.taps.clear();
            self.taps.resize(CHANNELS * n, 0.0);
        }
        self.tap_frames = if self.taps_enabled { n } else { 0 };
        if !self.playing {
            left[..n].fill(0.0);
            right[..n].fill(0.0);
            self.fall_levels(n);
            return n;
        }
        self.frames += n as u64;
        for i in 0..n {
            let (engine, chip, until_tick, loop_mode, end, writes, next_write) = (
                &mut self.engine,
                &mut self.chip,
                &mut self.until_tick,
                self.loop_mode,
                &mut self.end_reached,
                &mut self.writes,
                &mut self.next_write,
            );
            let [l, r] = self.resampler.next(|| {
                if *until_tick <= 0.0 {
                    // A tick that wrote more than its samples could carry
                    // finishes before the next one starts.
                    for &(reg, val) in &writes[*next_write..] {
                        chip.write(reg, val);
                    }
                    writes.clear();
                    *next_write = 0;
                    step(engine, writes, loop_mode, end);
                    *until_tick += NATIVE_RATE / engine.refresh();
                }
                *until_tick -= 1.0;
                if let Some(&(reg, val)) = writes.get(*next_write) {
                    chip.write(reg, val);
                    *next_write += 1;
                }
                let (l, r) = chip.clock_sample();
                [l as f32, r as f32]
            });
            left[i] = l * FULL_SCALE * self.gain;
            right[i] = r * FULL_SCALE * self.gain;
            if taps_on {
                for ch in 0..CHANNELS {
                    self.taps[ch * n + i] = self.chip.channel_output(ch) as f32 * TAP_SCALE;
                }
            }
            if levels_on {
                for ch in 0..CHANNELS {
                    let v = (self.chip.channel_output(ch) as f32).abs() * TAP_SCALE;
                    if v > self.block_peaks[ch] {
                        self.block_peaks[ch] = v;
                    }
                }
            }
        }
        self.fall_levels(n);
        n
    }

    /// Moves to the start of `row` in order position `order`, keeping the
    /// play/pause state. The song is replayed silently from its top to get
    /// there; a row it never reaches is jumped to directly. Returns whether
    /// the row was reached by playing. It allocates nothing (the chip, the
    /// resampler and the engine are reset in place), so a host may call it on
    /// the audio thread, though a far row costs up to a few thousand ticks of
    /// register writes.
    pub fn seek(&mut self, order: usize, row: usize) -> bool {
        let rows = self.rows_total();
        let reached = self.seek_to(order, row);
        // A seek counts as a row, so a follower sees the jump.
        self.rows_base = rows
            .wrapping_add(1)
            .wrapping_sub(self.engine.rows_started());
        reached
    }

    fn seek_to(&mut self, order: usize, row: usize) -> bool {
        self.chip.reset();
        self.chip.set_channel_mask(self.channel_mask());
        self.resampler.reset();
        self.engine.restart();
        self.engine.reset(&mut self.chip);
        self.until_tick = 0.0;
        self.end_reached = false;
        self.writes.clear();
        self.next_write = 0;
        if (order, row) == (0, 0) {
            return true;
        }
        let target = |e: &A2Engine| {
            let (o, _, r) = e.next_position();
            (o, r) == (order, row)
        };
        let mut reached = false;
        for _ in 0..SEEK_TICK_LIMIT {
            if target(&self.engine) {
                reached = true;
                break;
            }
            self.engine.update(&mut self.chip);
            if self.engine.ended() || self.engine.loops() > 0 {
                break;
            }
        }
        if !reached {
            self.engine.restart();
            self.engine.reset(&mut self.chip);
            if !self.engine.redirect(order, row) {
                return false;
            }
        }
        // Play up to and including the target row's first tick.
        for _ in 0..SEEK_TICK_LIMIT {
            let (o, _, r) = self.engine.position();
            let started = (o, r) == (order, row) && !target(&self.engine);
            if started {
                break;
            }
            self.engine.update(&mut self.chip);
            self.until_tick = NATIVE_RATE / self.engine.refresh();
            if self.engine.ended() {
                break;
            }
        }
        reached
    }

    /// Keep playing order position `order` (its pattern) over and over:
    /// the tracker's "play pattern". `-1` plays the song on.
    pub fn set_loop_order(&mut self, order: i32) {
        self.loop_mode = if order >= 0 {
            A2Loop::Pattern(order as usize)
        } else {
            A2Loop::None
        };
    }

    /// Whether the song has come round once (passed its order list's end
    /// or a jump back) or stopped. It plays on either way.
    pub fn song_end_reached(&self) -> bool {
        self.end_reached
    }

    /// Bit masks over tracks (bit `t` = track `t`): muted tracks, and (when
    /// non-zero) the only tracks heard. Applied as the chip's channel mask.
    pub fn set_mute_solo(&mut self, mute: u32, solo: u32) {
        self.mute = mute;
        self.solo = solo;
        let mask = self.channel_mask();
        self.chip.set_channel_mask(mask);
    }

    /// Record per-channel scope taps during `render` (off by default).
    pub fn set_taps_enabled(&mut self, enabled: bool) {
        self.taps_enabled = enabled;
        if !enabled {
            self.taps = Vec::new();
            self.tap_frames = 0;
        }
    }

    /// OPL channel `ch`'s tap from the last `render` (±1: one operator's
    /// full swing); zeros when taps are off. `track_channel` maps tracks.
    pub fn read_tap(&self, ch: usize, out: &mut [f32]) {
        let n = out.len().min(self.tap_frames);
        if ch >= CHANNELS || n == 0 {
            out.fill(0.0);
            return;
        }
        out[..n].copy_from_slice(&self.taps[ch * self.tap_frames..ch * self.tap_frames + n]);
        out[n..].fill(0.0);
    }

    /// The OPL channel track `track` plays on.
    pub fn track_channel(&self, track: usize) -> usize {
        self.engine.track_channel(track)
    }

    pub fn track_count(&self) -> usize {
        self.engine.track_count()
    }

    /// Order positions before the first jump marker (the song's length as
    /// the tracker shows it).
    pub fn order_count(&self) -> usize {
        self.song
            .order
            .iter()
            .position(|&o| o >= 0x80)
            .unwrap_or(128)
    }

    /// The raw order entry at `index` (a pattern, or 0x80 + a jump target).
    pub fn order_entry(&self, index: usize) -> u8 {
        self.song.order.get(index).copied().unwrap_or(0)
    }

    pub fn rows_per_pattern(&self) -> usize {
        self.song.pattern_len as usize
    }

    /// The row playing now: its order position, pattern and row.
    pub fn order(&self) -> usize {
        self.engine.position().0
    }

    pub fn pattern(&self) -> usize {
        self.engine.position().1
    }

    pub fn row(&self) -> usize {
        self.engine.position().2
    }

    /// The timer rate now (Hz): tempo × macro speed-up.
    pub fn refresh(&self) -> f64 {
        self.engine.refresh()
    }

    pub fn song_name(&self) -> String {
        cp437(&self.song.name)
    }

    pub fn composer(&self) -> String {
        cp437(&self.song.composer)
    }

    pub fn version(&self) -> u8 {
        self.song.version
    }

    /// The instruments that hold FM data.
    pub fn instrument_count(&self) -> usize {
        self.song
            .instruments
            .iter()
            .rposition(|i| i.fm != [0; 11])
            .map_or(0, |i| i + 1)
    }

    pub fn instrument_name(&self, index: usize) -> String {
        self.song
            .instruments
            .get(index)
            .map_or(String::new(), |i| cp437(&i.name))
    }

    /// Pattern `pattern`'s cells for display, as the engine plays them (old
    /// effect numbers mapped to the v9+ set, fixed notes as notes): six bytes
    /// per cell (note, instrument, effect, param, effect 2, param 2),
    /// row-major over `rows_per_pattern()` rows and `track_count()` tracks.
    /// Empty for a pattern the song does not have.
    pub fn pattern_cells(&self, pattern: usize) -> Vec<u8> {
        let song = self.engine.song();
        let Some(p) = song.patterns.get(pattern) else {
            return Vec::new();
        };
        let rows = (song.pattern_len as usize).min(p.rows);
        let tracks = self.engine.track_count().min(p.channels);
        let mut out = Vec::with_capacity(rows * tracks * 6);
        for r in 0..rows {
            for t in 0..tracks {
                let c = p.cell(r, t);
                out.extend_from_slice(&[
                    c.note,
                    c.instrument,
                    c.effects[0].0,
                    c.effects[0].1,
                    c.effects[1].0,
                    c.effects[1].1,
                ]);
            }
        }
        out
    }

    /// 18: every OPL3 channel (what `read_tap` covers).
    pub fn channels(&self) -> usize {
        CHANNELS
    }
}

impl A2Player {
    fn channel_mask(&self) -> u32 {
        let audible = |t: usize| {
            let bit = 1u32 << t;
            self.mute & bit == 0 && (self.solo == 0 || self.solo & bit != 0)
        };
        let mut used = 0u32;
        let mut on = 0u32;
        for t in 0..self.engine.track_count() {
            let ch = 1u32 << self.engine.track_channel(t);
            used |= ch;
            if audible(t) {
                on |= ch;
            }
        }
        // Channels no track plays keep sounding (nothing is on them).
        (!used | on) & ((1 << CHANNELS) - 1)
    }

    /// The engine underneath (tests).
    pub fn engine(&self) -> &A2Engine {
        &self.engine
    }

    /// Plays AdPlug's departures from AT2 too (`A2Engine::adplug_quirks`):
    /// for the audio A/B against AdPlug renders, never in the app.
    pub fn set_adplug_quirks(&mut self, on: bool) {
        self.engine.adplug_quirks = on;
    }

    // ---- transport, position, levels and live edits: what a tracker editor
    // drives while the song plays. None of them allocates.

    /// Rows started since the player was made (see [`A2Status::rows`]).
    fn rows_total(&self) -> u32 {
        self.rows_base.wrapping_add(self.engine.rows_started())
    }

    /// Where the song is: see [`A2Status`].
    pub fn status(&self) -> A2Status {
        let (order, pattern, row) = self.engine.position();
        A2Status {
            playing: self.playing,
            order: order as u16,
            pattern: pattern as u16,
            row: row as u16,
            tick: self.engine.tick_in_row(),
            speed: self.engine.speed(),
            tempo: self.engine.tempo(),
            refresh: self.engine.refresh() as f32,
            rows: self.rows_total(),
            frames: self.frames,
            loops: self.engine.loops(),
            ended: self.engine.ended(),
        }
    }

    /// What the transport repeats (see [`A2Loop`]). It does not move the
    /// playhead: seek first.
    pub fn set_loop(&mut self, mode: A2Loop) {
        self.loop_mode = mode;
    }

    pub fn loop_mode(&self) -> A2Loop {
        self.loop_mode
    }

    /// Mutes or unmutes one track.
    pub fn set_mute(&mut self, track: usize, on: bool) {
        if track < 32 {
            let mask = self.mute & !(1 << track) | (on as u32) << track;
            self.set_mute_solo(mask, self.solo);
        }
    }

    /// Solos or unsolos one track (while any is soloed only soloed tracks are heard).
    pub fn set_solo(&mut self, track: usize, on: bool) {
        if track < 32 {
            let mask = self.solo & !(1 << track) | (on as u32) << track;
            self.set_mute_solo(self.mute, mask);
        }
    }

    pub fn mute_mask(&self) -> u32 {
        self.mute
    }

    pub fn solo_mask(&self) -> u32 {
        self.solo
    }

    /// Starts or stops measuring each OPL channel's level (off by default; it
    /// costs a read of the chip's channel outputs per frame).
    pub fn set_levels_enabled(&mut self, on: bool) {
        self.levels_enabled = on;
        if !on {
            self.levels = [0.0; CHANNELS];
            self.block_peaks = [0.0; CHANNELS];
        }
    }

    /// OPL channel `ch`'s level (linear, ±1 is one operator's full swing): the
    /// loudest frame of the latest block, falling 24 dB a second after it.
    /// A muted track's channel reads 0. Zeros while levels are off.
    pub fn channel_level(&self, ch: usize) -> f32 {
        if ch >= CHANNELS || self.channel_mask() & (1 << ch) == 0 {
            return 0.0;
        }
        self.levels[ch]
    }

    /// Each track's level (see [`channel_level`](Self::channel_level)) in `out`,
    /// in track order; returns how many it wrote (`track_count()` at most). The
    /// two tracks of a 4-op pair read the same, as they sound on one channel.
    pub fn track_levels(&self, out: &mut [f32]) -> usize {
        let n = out.len().min(self.track_count());
        let mask = self.channel_mask();
        for (t, o) in out[..n].iter_mut().enumerate() {
            let ch = self.track_channel(t);
            *o = if mask & (1 << ch) != 0 {
                self.levels[ch]
            } else {
                0.0
            };
        }
        n
    }

    fn fall_levels(&mut self, frames: usize) {
        if !self.levels_enabled {
            return;
        }
        let fall =
            10f64.powf(-LEVEL_FALL_DB_PER_SECOND / 20.0 * frames as f64 / self.out_rate) as f32;
        for ch in 0..CHANNELS {
            self.levels[ch] = (self.levels[ch] * fall).max(self.block_peaks[ch]);
            self.block_peaks[ch] = 0.0;
        }
    }

    /// Lands a live edit in the song that is playing (see [`LiveEdit`]; the
    /// document's `live_edits` makes them). False for one outside the song (a
    /// pattern or instrument it does not have): nothing changed.
    pub fn apply_edit(&mut self, edit: &LiveEdit) -> bool {
        match edit {
            LiveEdit::Cell {
                pattern,
                row,
                track,
                cell,
            } => {
                let (p, r, t) = (*pattern as usize, *row as usize, *track as usize);
                let landed = self.engine.set_cell(p, r, t, *cell);
                // The raw copy, for the readers that show the song as the file has it.
                let own = self.song.patterns.len();
                let raw = if p < own {
                    self.song.patterns.get_mut(p)
                } else {
                    self.song.spare_patterns.get_mut(p - own)
                };
                if let Some(pat) = raw {
                    if landed {
                        pat.cells[r * pat.channels + t] = *cell;
                    }
                }
                landed
            }
            LiveEdit::Instrument {
                number,
                fm,
                panning,
                finetune,
                voice_type,
            } => {
                let landed = self.engine.set_instrument_data(
                    *number as usize,
                    *fm,
                    *panning,
                    *finetune,
                    *voice_type,
                );
                if let Some(i) = (*number as usize)
                    .checked_sub(1)
                    .and_then(|n| self.song.instruments.get_mut(n))
                {
                    (i.fm, i.panning, i.finetune, i.voice_type) =
                        (*fm, *panning, *finetune, *voice_type);
                }
                landed
            }
            LiveEdit::FmMacro { number, table } => {
                self.engine.set_fm_macro(*number as usize, table)
            }
            LiveEdit::ArpeggioMacro { number, table } => {
                self.engine.set_arpeggio_macro(*number as usize, table)
            }
            LiveEdit::VibratoMacro { number, table } => {
                self.engine.set_vibrato_macro(*number as usize, table)
            }
            LiveEdit::Timing { tempo, speed } => {
                self.engine.set_timing(*tempo, *speed);
                (self.song.tempo, self.song.speed) = (*tempo, *speed);
                true
            }
        }
    }
}

/// One timer tick, then the loop and the end check.
fn step(engine: &mut A2Engine, out: &mut impl RegisterSink, mode: A2Loop, end: &mut bool) {
    let loops = engine.loops();
    engine.update(out);
    match mode {
        A2Loop::Pattern(o) => {
            if engine.next_position().0 != o || engine.ended() {
                engine.redirect(o, 0);
            }
        }
        A2Loop::Range { order, row0, row1 } => {
            let (next_order, _, next_row) = engine.next_position();
            if next_order != order || next_row > row1 || engine.ended() {
                engine.redirect(order, row0);
            }
        }
        A2Loop::None => {
            if engine.loops() != loops || engine.ended() {
                *end = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(rel: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/a2m")
                .join(rel),
        )
        .unwrap()
    }

    fn render(p: &mut A2Player, frames: usize) -> Vec<f32> {
        let (mut l, mut r) = (vec![0f32; frames], vec![0f32; frames]);
        for (lc, rc) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
            p.render(lc, rc);
        }
        l
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    const COT: &str = "NAB622/corridors of time.a2m";

    #[test]
    fn plays_a_song_and_is_silent_while_paused() {
        let mut p = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        assert!(
            rms(&render(&mut p, 4800)) == 0.0,
            "paused player makes sound"
        );
        p.play();
        let out = render(&mut p, 48_000 * 3);
        assert!(rms(&out) > 0.01, "no sound after 3 s");
        assert!(p.order() > 0 || p.row() > 0);
        assert!(!p.song_name().trim().is_empty());
    }

    #[test]
    fn the_timer_runs_at_the_songs_rate() {
        // 3 s at tempo × speed-up ticks per second: the row count follows.
        let mut p = A2Player::new(&corpus(COT), 44_100.0).unwrap();
        let rate = p.refresh();
        p.play();
        render(&mut p, 44_100 * 3);
        let mut e = A2Engine::new(parse(&corpus(COT)).unwrap());
        let mut sink: Vec<(u16, u8)> = Vec::new();
        e.reset(&mut sink);
        let n = (rate * 3.0) as usize;
        let mut seen = Vec::new();
        for i in 0..n + 3 {
            e.update(&mut sink);
            if i + 3 >= n {
                let (o, _, r) = e.position();
                seen.push((o, r));
            }
        }
        assert!(
            seen.contains(&(p.order(), p.row())),
            "{:?} not in {seen:?}",
            (p.order(), p.row())
        );
    }

    #[test]
    fn refuses_the_instrument_collection_truthfully() {
        let err = A2Player::new(
            &corpus("OxygenStar/oxygenstar's instrument set #001.a2m"),
            48_000.0,
        )
        .err()
        .unwrap();
        assert!(err.contains("only jump markers"), "{err}");
        assert!(A2Player::new(b"not a module", 48_000.0).is_err());
    }

    #[test]
    fn seek_lands_on_the_row_with_the_songs_registers() {
        // Seeking to a row equals playing up to it, register for register.
        let mut p = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        assert!(p.seek(3, 16));
        assert_eq!((p.order(), p.row()), (3, 16));
        let mut e = A2Engine::new(parse(&corpus(COT)).unwrap());
        let mut chip = Chip::new();
        e.reset(&mut chip);
        while e.position() != (3, p.pattern(), 16) {
            e.update(&mut chip);
        }
        for reg in 0..0x200u16 {
            assert_eq!(p.chip.written(reg), chip.written(reg), "register {reg:03x}");
        }
        p.play();
        assert!(rms(&render(&mut p, 24_000)) > 0.01);
    }

    #[test]
    fn a_looped_order_repeats_and_muting_every_track_is_silent() {
        let mut p = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        p.seek(2, 0);
        p.set_loop_order(2);
        p.play();
        for _ in 0..40 {
            render(&mut p, 12_000);
            assert_eq!(p.order(), 2);
        }
        assert!(!p.song_end_reached());
        p.set_mute_solo(u32::MAX, 0);
        let out = render(&mut p, 48_000);
        assert!(rms(&out[24_000..]) < 1e-4, "muted song still sounds");
    }

    #[test]
    fn pattern_cells_are_the_played_cells() {
        let p = A2Player::new(&corpus("Subz3ro/intro-tune coop.a2m"), 48_000.0).unwrap();
        let song = p.engine().song();
        let tracks = p.track_count();
        let rows = p.rows_per_pattern();
        let first = song.order.iter().copied().find(|&o| o < 0x80).unwrap() as usize;
        let cells = p.pattern_cells(first);
        assert_eq!(cells.len(), rows * tracks * 6);
        for r in 0..rows {
            for t in 0..tracks {
                let c = song.patterns[first].cell(r, t);
                let at = (r * tracks + t) * 6;
                assert_eq!(
                    &cells[at..at + 6],
                    &[
                        c.note,
                        c.instrument,
                        c.effects[0].0,
                        c.effects[0].1,
                        c.effects[1].0,
                        c.effects[1].1
                    ]
                );
            }
        }
        assert!(
            cells.chunks(6).any(|c| c[0] != 0),
            "the first pattern has notes"
        );
        assert!(p.pattern_cells(10_000).is_empty());
    }

    #[test]
    fn taps_follow_the_track_channels() {
        let mut p = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        p.set_taps_enabled(true);
        p.play();
        render(&mut p, 48_000);
        let mut tap = vec![0f32; 128];
        let (mut l, mut r) = (vec![0f32; 128], vec![0f32; 128]);
        let mut any = false;
        for _ in 0..100 {
            p.render(&mut l, &mut r);
            for t in 0..p.track_count() {
                p.read_tap(p.track_channel(t), &mut tap);
                any |= tap.iter().any(|v| v.abs() > 0.01);
            }
        }
        assert!(any, "no track tap moved");
    }

    /// AMEGAS's tracks 0 and 1 are one 4-op voice (instruments 1 and 9); the
    /// chip outputs it from the second track's channel, so soloing the first
    /// track used to be silent.
    #[test]
    fn either_track_of_a_four_op_pair_solos_the_voice() {
        let bytes = corpus("Encore/karsten obarski - amegas.a2m");
        let rms = |solo: u32| {
            let mut p = A2Player::new(&bytes, 48_000.0).unwrap();
            p.set_mute_solo(0, solo);
            p.play();
            let pcm = render(&mut p, 48_000 * 3);
            (pcm.iter().map(|v| v * v).sum::<f32>() / pcm.len() as f32).sqrt()
        };
        assert_eq!(
            p_channels(&bytes, 0),
            p_channels(&bytes, 1),
            "both tracks of the pair report the channel the voice is heard on"
        );
        assert!(rms(1 << 0) > 0.01, "soloing track 0 is silent");
        assert!(rms(1 << 1) > 0.01, "soloing track 1 is silent");
    }

    fn p_channels(bytes: &[u8], t: usize) -> usize {
        A2Player::new(bytes, 48_000.0).unwrap().track_channel(t)
    }

    #[test]
    fn status_follows_the_song_and_a_seek_counts_as_a_row() {
        let mut p = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        let s0 = p.status();
        assert!(!s0.playing && s0.rows == 0 && s0.frames == 0);
        p.play();
        render(&mut p, 48_000);
        let s1 = p.status();
        assert!(s1.playing && s1.rows > 3 && s1.frames == 48_000, "{s1:?}");
        assert_eq!((s1.order as usize, s1.row as usize), (p.order(), p.row()));
        assert!(s1.speed > 0 && s1.tempo > 0 && (s1.refresh - p.refresh() as f32).abs() < 1e-3);
        assert!(s1.tick < s1.speed as u16 * 8, "{s1:?}");
        assert!(p.seek(3, 16));
        let s2 = p.status();
        assert_eq!((s2.order, s2.row), (3, 16));
        assert_eq!(s2.rows, s1.rows + 1, "a seek is one row to a follower");
        render(&mut p, 24_000);
        assert!(p.status().rows > s2.rows);
        // Paused: the position and the clock stay.
        p.pause();
        let s3 = p.status();
        render(&mut p, 4_800);
        assert_eq!(p.status().row, s3.row);
        assert_eq!(p.status().frames, s3.frames);
        assert!(!p.status().playing);
    }

    #[test]
    fn mute_and_solo_one_track_at_a_time() {
        let mut p = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        p.set_mute(0, true);
        p.set_mute(5, true);
        assert_eq!(p.mute_mask(), 0b100001);
        p.set_mute(0, false);
        assert_eq!(p.mute_mask(), 0b100000);
        p.set_solo(2, true);
        assert_eq!((p.mute_mask(), p.solo_mask()), (0b100000, 0b100));
        p.set_solo(2, false);
        assert_eq!(p.solo_mask(), 0);
        p.set_mute(40, true);
        assert_eq!(p.mute_mask(), 0b100000, "tracks past 31 are ignored");
    }

    #[test]
    fn levels_follow_the_tracks_and_fall_when_the_sound_stops() {
        let mut p = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        let mut out = vec![0f32; 32];
        p.play();
        render(&mut p, 24_000);
        assert_eq!(p.track_levels(&mut out), p.track_count());
        assert!(out.iter().all(|&v| v == 0.0), "off until asked");
        p.set_levels_enabled(true);
        render(&mut p, 48_000);
        let n = p.track_levels(&mut out);
        assert_eq!(n, p.track_count());
        let loud = out[..n].iter().cloned().fold(0.0, f32::max);
        assert!(loud > 0.02, "a track is sounding: {loud}");
        // Muting the loudest track silences its level.
        let top = (0..n).max_by(|&a, &b| out[a].total_cmp(&out[b])).unwrap();
        p.set_mute(top, true);
        render(&mut p, 24_000);
        p.track_levels(&mut out);
        assert_eq!(out[top], 0.0, "a muted track reads 0");
        // Pausing lets every level fall 24 dB a second.
        p.set_mute(top, false);
        render(&mut p, 24_000);
        p.track_levels(&mut out);
        let before = out[..n].iter().cloned().fold(0.0, f32::max);
        p.pause();
        render(&mut p, 48_000);
        p.track_levels(&mut out);
        let after = out[..n].iter().cloned().fold(0.0, f32::max);
        let fall_db = 20.0 * (after / before).log10();
        assert!(
            (fall_db + 24.0).abs() < 1.0,
            "fell {fall_db:.1} dB in a second"
        );
        p.set_levels_enabled(false);
        p.track_levels(&mut out);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn a_range_loop_repeats_its_rows() {
        let mut p = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        assert!(p.seek(2, 8));
        p.set_loop(A2Loop::Range {
            order: 2,
            row0: 8,
            row1: 11,
        });
        p.play();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..80 {
            render(&mut p, 6_000);
            let s = p.status();
            assert_eq!(s.order, 2);
            seen.insert(s.row);
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![8, 9, 10, 11]);
        assert!(!p.song_end_reached());
        p.set_loop(A2Loop::None);
        assert_eq!(p.loop_mode(), A2Loop::None);
    }

    #[test]
    fn seek_after_a_reset_is_the_same_as_a_fresh_player() {
        // The in-place reset of the chip, the resampler and the engine leaves no
        // trace: a player that has played and seeked renders what a new one does.
        let mut a = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        a.play();
        render(&mut a, 30_000);
        a.seek(3, 16);
        let mut b = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        b.seek(3, 16);
        b.play();
        assert_eq!(render(&mut a, 24_000), render(&mut b, 24_000));
        // And back to the start.
        a.seek(0, 0);
        let mut c = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        c.play();
        assert_eq!(render(&mut a, 12_000), render(&mut c, 12_000));
    }

    #[test]
    fn live_edits_that_miss_the_song_are_refused() {
        let mut p = A2Player::new(&corpus(COT), 48_000.0).unwrap();
        let cell = super::super::model::Cell {
            note: 49,
            ..Default::default()
        };
        assert!(p.apply_edit(&LiveEdit::Cell {
            pattern: 0,
            row: 0,
            track: 0,
            cell
        }));
        assert!(!p.apply_edit(&LiveEdit::Cell {
            pattern: 200,
            row: 0,
            track: 0,
            cell
        }));
        assert!(!p.apply_edit(&LiveEdit::Cell {
            pattern: 0,
            row: 999,
            track: 0,
            cell
        }));
        assert!(!p.apply_edit(&LiveEdit::Instrument {
            number: 0,
            fm: [0; 11],
            panning: 0,
            finetune: 0,
            voice_type: 0
        }));
        assert!(p.apply_edit(&LiveEdit::Timing {
            tempo: 60,
            speed: 3
        }));
        assert_eq!((p.status().tempo, p.status().speed), (60, 3));
        // The edited cell is what the player shows.
        assert_eq!(p.pattern_cells(0)[0], 49);
    }
}
