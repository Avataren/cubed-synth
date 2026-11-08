//! What a playing [`Player`](crate::Player) shows the outside: where the song
//! is, how loud each channel is, and (when asked) the latest frames of each
//! channel for a scope.
//!
//! A [`Probe`] is shared (`Arc`) between the audio thread, which owns the
//! player and writes it, and any other thread, which reads it. Every field is
//! an atomic, written with plain stores and read with plain loads: neither
//! side locks, waits or allocates (the probe is built, rings included, off the
//! audio thread). The position is packed into one word, so a reader always
//! sees a row, a tick and a tempo that belong together.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::protracker::MAX_CHANNELS;

/// Frames each channel's scope ring keeps (11 ms at 48 kHz).
pub const SCOPE_FRAMES: usize = 512;

/// Slots in [`Probe::levels`]: every channel, then the note preview's voice.
pub const LEVEL_SLOTS: usize = MAX_CHANNELS + 1;

/// Where the transport is, as the player last reported it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Status {
    /// The song is playing (not stopped).
    pub playing: bool,
    /// A previewed note is sounding or releasing.
    pub preview: bool,
    /// Position in the order list.
    pub order: u16,
    /// The pattern at that position.
    pub pattern: u16,
    /// The row playing (or the one a stopped song will start at).
    pub row: u16,
    /// The tick of the row playing, 0 on its first.
    pub tick: u8,
    /// Ticks per row.
    pub speed: u8,
    /// Beats per minute (a tick lasts 2.5 / tempo seconds).
    pub tempo: u16,
    /// Rows started since the player was made: it goes up by one for every row
    /// (a repeated row of a pattern delay included), so a follower that polls
    /// slower than the song can tell how many rows it missed.
    pub rows: u32,
    /// Frames the player has rendered (the song's clock keeps running while it
    /// is stopped, so a preview still moves it).
    pub frames: u64,
    /// Changes whenever anything a UI shows has moved (the position, a level, the
    /// transport), and stays put while everything is still: a host that posts
    /// the probe somewhere can skip a quiet one.
    pub version: u32,
}

const POS_ORDER: u32 = 0;
const POS_PATTERN: u32 = 12;
const POS_ROW: u32 = 24;
const POS_TICK: u32 = 36;
const POS_SPEED: u32 = 44;
const POS_TEMPO: u32 = 52;
const POS_PLAYING: u32 = 61;
const POS_PREVIEW: u32 = 62;

/// A player's position, levels and channel scopes, readable from any thread.
/// See the module docs.
pub struct Probe {
    /// Order, pattern and row (12 bits each), tick, speed (8), tempo (9), and the
    /// transport flags.
    position: AtomicU64,
    /// Rows started (low 32 bits) and the version (high 32).
    counters: AtomicU64,
    frames: AtomicU64,
    channels: usize,
    /// Peak per channel (f32 bits), then the preview's.
    levels: Box<[AtomicU32]>,
    /// `channels` rings of [`SCOPE_FRAMES`] mono frames (f32 bits).
    scopes: Box<[AtomicU32]>,
    /// Frames written to each ring since the probe was made.
    written: Box<[AtomicU64]>,
    scopes_on: AtomicBool,
    alive: AtomicBool,
}

impl Probe {
    /// A probe for a song of `channels` channels (at most 32). Make it off the
    /// audio thread: it allocates the rings.
    pub fn new(channels: usize) -> Arc<Probe> {
        let channels = channels.min(MAX_CHANNELS);
        let zeros32 = |n: usize| (0..n).map(|_| AtomicU32::new(0)).collect::<Box<[_]>>();
        Arc::new(Probe {
            position: AtomicU64::new(0),
            counters: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            channels,
            levels: zeros32(LEVEL_SLOTS),
            scopes: zeros32(channels * SCOPE_FRAMES),
            written: (0..channels).map(|_| AtomicU64::new(0)).collect(),
            scopes_on: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        })
    }

    /// The song's channel count.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The transport as last published.
    pub fn status(&self) -> Status {
        let p = self.position.load(Ordering::Relaxed);
        let c = self.counters.load(Ordering::Relaxed);
        let field = |shift: u32, bits: u32| ((p >> shift) & ((1 << bits) - 1)) as u16;
        Status {
            playing: p >> POS_PLAYING & 1 == 1,
            preview: p >> POS_PREVIEW & 1 == 1,
            order: field(POS_ORDER, 12),
            pattern: field(POS_PATTERN, 12),
            row: field(POS_ROW, 12),
            tick: field(POS_TICK, 8) as u8,
            speed: field(POS_SPEED, 8) as u8,
            tempo: field(POS_TEMPO, 9),
            rows: c as u32,
            frames: self.frames.load(Ordering::Relaxed),
            version: (c >> 32) as u32,
        }
    }

    /// Each channel's recent peak, linear, in `out` (as many as fit), then the
    /// preview voice's; returns how many it wrote (`channels() + 1` at most). A
    /// peak is the loudest frame of the latest block, falling 24 dB a second
    /// after it, so a display that polls slower than the song does not miss a
    /// hit. A muted channel reads 0. An AdLib channel's is an estimate from its
    /// key and volume (the chip's channels are not separable).
    pub fn levels(&self, out: &mut [f32]) -> usize {
        let n = out.len().min(self.channels + 1);
        for (i, o) in out[..n].iter_mut().enumerate() {
            // The preview's slot is the one after the last channel.
            let slot = if i < self.channels { i } else { MAX_CHANNELS };
            *o = f32::from_bits(self.levels[slot].load(Ordering::Relaxed));
        }
        n
    }

    /// Whether the channel scopes are being written (see
    /// [`set_scopes`](Self::set_scopes)).
    pub fn scopes_enabled(&self) -> bool {
        self.scopes_on.load(Ordering::Relaxed)
    }

    /// Starts or stops filling the channel scope rings. While off they cost the
    /// audio thread nothing; they keep what they had.
    pub fn set_scopes(&self, on: bool) {
        self.scopes_on.store(on, Ordering::Relaxed);
    }

    /// Copies the latest frames of `channel` (mono, the mean of its left and
    /// right, before the master volume), oldest first, into `out`: as many as
    /// fit, at most [`SCOPE_FRAMES`]. Returns how many it copied: 0 for a
    /// channel that does not exist, fewer than asked until the ring has filled.
    /// If the writer laps the read, the oldest frames are dropped rather than
    /// returned torn.
    pub fn scope(&self, channel: usize, out: &mut [f32]) -> usize {
        if channel >= self.channels {
            return 0;
        }
        let ring = &self.scopes[channel * SCOPE_FRAMES..(channel + 1) * SCOPE_FRAMES];
        let written = &self.written[channel];
        let want = out.len().min(SCOPE_FRAMES);
        for attempt in 0..4 {
            let end = written.load(Ordering::Acquire);
            let have = (end.min(SCOPE_FRAMES as u64) as usize).min(want);
            let first = end - have as u64;
            for (i, o) in out[..have].iter_mut().enumerate() {
                let at = (first + i as u64) as usize % SCOPE_FRAMES;
                *o = f32::from_bits(ring[at].load(Ordering::Relaxed));
            }
            std::sync::atomic::fence(Ordering::Acquire);
            let now = written.load(Ordering::Acquire);
            let overwritten = now
                .saturating_sub(SCOPE_FRAMES as u64)
                .saturating_sub(first) as usize;
            if overwritten == 0 {
                return have;
            }
            if overwritten >= have {
                continue;
            }
            if attempt == 3 {
                out.copy_within(overwritten..have, 0);
                return have - overwritten;
            }
        }
        0
    }

    /// Frames written to `channel`'s scope ring so far: it changes when new
    /// frames have come in, so a display can skip a redraw.
    pub fn scope_position(&self, channel: usize) -> u64 {
        self.written
            .get(channel)
            .map_or(0, |w| w.load(Ordering::Acquire))
    }

    /// False once the player that writes this probe has been dropped.
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    // ---- the player's side ----

    /// Publishes the transport (the audio thread's).
    pub(crate) fn publish_position(&self, s: &Status) {
        let mut p = 0u64;
        let mut put = |value: u64, shift: u32, bits: u32| {
            p |= (value & ((1 << bits) - 1)) << shift;
        };
        put(s.order as u64, POS_ORDER, 12);
        put(s.pattern as u64, POS_PATTERN, 12);
        put(s.row as u64, POS_ROW, 12);
        put(s.tick as u64, POS_TICK, 8);
        put(s.speed as u64, POS_SPEED, 8);
        put(s.tempo as u64, POS_TEMPO, 9);
        put(s.playing as u64, POS_PLAYING, 1);
        put(s.preview as u64, POS_PREVIEW, 1);
        let old = self.position.swap(p, Ordering::Relaxed);
        let c = self.counters.load(Ordering::Relaxed);
        let (mut rows, mut version) = (c as u32, (c >> 32) as u32);
        let moved = old != p || rows != s.rows;
        rows = s.rows;
        if moved {
            version = version.wrapping_add(1);
        }
        self.counters
            .store((version as u64) << 32 | rows as u64, Ordering::Relaxed);
    }

    /// Publishes the frame clock and the levels at the end of a block. Bumps the
    /// version if the levels moved.
    pub(crate) fn publish_block(&self, frames: u64, levels: &[f32; LEVEL_SLOTS]) {
        self.frames.store(frames, Ordering::Relaxed);
        let mut moved = false;
        for (cell, &level) in self.levels.iter().zip(levels) {
            let old = f32::from_bits(cell.load(Ordering::Relaxed));
            moved |= old != level;
            cell.store(level.to_bits(), Ordering::Relaxed);
        }
        if moved {
            let c = self.counters.load(Ordering::Relaxed);
            let version = ((c >> 32) as u32).wrapping_add(1);
            self.counters.store(
                (version as u64) << 32 | (c & 0xffff_ffff),
                Ordering::Relaxed,
            );
        }
    }

    /// Appends `frames` to `channel`'s scope ring.
    pub(crate) fn write_scope(&self, channel: usize, frames: &[f32]) {
        if channel >= self.channels || frames.is_empty() {
            return;
        }
        let ring = &self.scopes[channel * SCOPE_FRAMES..(channel + 1) * SCOPE_FRAMES];
        let written = &self.written[channel];
        // Only the writer changes `written`.
        let start = written.load(Ordering::Relaxed);
        let skip = frames.len().saturating_sub(SCOPE_FRAMES);
        for (i, &f) in frames.iter().enumerate().skip(skip) {
            let at = (start + i as u64) as usize % SCOPE_FRAMES;
            ring[at].store(f.to_bits(), Ordering::Relaxed);
        }
        written.store(start + frames.len() as u64, Ordering::Release);
    }

    pub(crate) fn set_alive(&self, alive: bool) {
        self.alive.store(alive, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Probe({} channels, {:?})", self.channels, self.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_round_trips_through_the_packed_word() {
        let probe = Probe::new(4);
        let s = Status {
            playing: true,
            preview: true,
            order: 4095,
            pattern: 255,
            row: 4000,
            tick: 31,
            speed: 255,
            tempo: 255,
            rows: 77,
            frames: 0,
            version: 0,
        };
        probe.publish_position(&s);
        let back = probe.status();
        assert_eq!((back.order, back.pattern, back.row), (4095, 255, 4000));
        assert_eq!((back.tick, back.speed, back.tempo), (31, 255, 255));
        assert!(back.playing && back.preview);
        assert_eq!(back.rows, 77);
        assert_eq!(back.version, 1, "a change bumps the version");
        probe.publish_position(&s);
        assert_eq!(probe.status().version, 1, "the same state does not");
    }

    #[test]
    fn levels_and_the_preview_slot() {
        let probe = Probe::new(3);
        let mut levels = [0.0; LEVEL_SLOTS];
        levels[0] = 0.5;
        levels[2] = 0.25;
        levels[MAX_CHANNELS] = 0.75;
        probe.publish_block(480, &levels);
        let mut out = [9.0; 8];
        assert_eq!(probe.levels(&mut out), 4, "three channels and the preview");
        assert_eq!(out[..4], [0.5, 0.0, 0.25, 0.75]);
        assert_eq!(probe.status().frames, 480);
        let v = probe.status().version;
        probe.publish_block(960, &levels);
        assert_eq!(probe.status().version, v, "still levels leave the version");
    }

    #[test]
    fn a_scope_ring_keeps_the_latest_frames_in_order() {
        let probe = Probe::new(2);
        let mut out = [0.0; SCOPE_FRAMES];
        assert_eq!(probe.scope(0, &mut out), 0);
        assert_eq!(probe.scope(5, &mut out), 0, "no such channel");
        let block: Vec<f32> = (0..300).map(|i| i as f32).collect();
        probe.write_scope(0, &block);
        assert_eq!(probe.scope(0, &mut out), 300);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[299], 299.0);
        probe.write_scope(0, &block);
        assert_eq!(probe.scope(0, &mut out), SCOPE_FRAMES);
        assert_eq!(out[SCOPE_FRAMES - 1], 299.0);
        assert_eq!(
            out[SCOPE_FRAMES - 1 - 300],
            299.0,
            "the last frame of the first block"
        );
        assert_eq!(out[0], 88.0, "212 frames of the first block are still in");
        // A shorter read is the newest frames.
        let mut few = [0.0; 3];
        assert_eq!(probe.scope(0, &mut few), 3);
        assert_eq!(few, [297.0, 298.0, 299.0]);
        assert_eq!(probe.scope_position(0), 600);
        assert_eq!(probe.scope_position(1), 0, "the other channel is untouched");
        // A block longer than the ring leaves its tail.
        let long: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        probe.write_scope(1, &long);
        assert_eq!(probe.scope(1, &mut out), SCOPE_FRAMES);
        assert_eq!(out[SCOPE_FRAMES - 1], 999.0);
        assert_eq!(out[0], (1000 - SCOPE_FRAMES) as f32);
    }
}
