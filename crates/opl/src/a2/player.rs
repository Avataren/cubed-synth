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
    loop_order: Option<usize>,
    end_reached: bool,
    mute: u32,
    solo: u32,
    taps_enabled: bool,
    taps: Vec<f32>,
    tap_frames: usize,
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
            loop_order: None,
            end_reached: false,
            mute: 0,
            solo: 0,
            taps_enabled: false,
            taps: Vec::new(),
            tap_frames: 0,
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
        if self.taps_enabled {
            self.taps.clear();
            self.taps.resize(CHANNELS * n, 0.0);
        }
        self.tap_frames = if self.taps_enabled { n } else { 0 };
        if !self.playing {
            left[..n].fill(0.0);
            right[..n].fill(0.0);
            return n;
        }
        for i in 0..n {
            let (engine, chip, until_tick, loop_order, end, writes, next_write) = (
                &mut self.engine,
                &mut self.chip,
                &mut self.until_tick,
                self.loop_order,
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
                    step(engine, writes, loop_order, end);
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
        }
        n
    }

    /// Moves to the start of `row` in order position `order`, keeping the
    /// play/pause state. The song is replayed silently from its top to get
    /// there; a row it never reaches is jumped to directly. Returns whether
    /// the row was reached by playing.
    pub fn seek(&mut self, order: usize, row: usize) -> bool {
        self.chip = Chip::new();
        self.chip.set_channel_mask(self.channel_mask());
        self.resampler = Resampler::new(NATIVE_RATE, self.out_rate);
        self.engine = A2Engine::new(self.song.clone());
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
            self.engine = A2Engine::new(self.song.clone());
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
        self.loop_order = (order >= 0).then_some(order as usize);
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
}

/// One timer tick, then the order loop and the end check.
fn step(
    engine: &mut A2Engine,
    out: &mut impl RegisterSink,
    loop_order: Option<usize>,
    end: &mut bool,
) {
    let loops = engine.loops();
    engine.update(out);
    if let Some(o) = loop_order {
        if engine.next_position().0 != o || engine.ended() {
            engine.redirect(o, 0);
        }
    } else if engine.loops() != loops || engine.ended() {
        *end = true;
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
}
