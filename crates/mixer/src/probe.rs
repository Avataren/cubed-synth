//! What a playing song shows the outside, the same for every song format: where the
//! transport is, how loud each channel is, and (when asked) the latest frames of each
//! channel for a scope.
//!
//! A [`SongProbe`] is shared (`Arc`) between the audio thread, which writes it after each
//! block (`songs::PlayerSong` does, for any [`SongPlayer`](crate::songs::SongPlayer)), and
//! any other thread, which reads it through [`SongReader`]. Every field is an atomic,
//! written with plain stores and read with plain loads: neither side locks, waits or
//! allocates (the probe is built, rings included, off the audio thread). The position is
//! packed into one word, so a reader always sees a row, a tick and a tempo that belong
//! together. It is `cubed_tracker::Probe`'s design for songs that are not modules.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::song::{SongReader, SongStatus, SONG_LEVEL_SLOTS, SONG_MAX_CHANNELS, SONG_SCOPE_FRAMES};

const POS_ORDER: u32 = 0;
const POS_PATTERN: u32 = 12;
const POS_ROW: u32 = 24;
const POS_TICK: u32 = 36;
const POS_SPEED: u32 = 44;
const POS_TEMPO: u32 = 52;
const POS_PLAYING: u32 = 61;
const POS_PREVIEW: u32 = 62;

/// A song's position, levels and channel scopes, readable from any thread. See the module
/// docs.
pub struct SongProbe {
    /// Order, pattern and row (12 bits each), tick, speed (8), tempo (9), and the transport
    /// flags.
    position: AtomicU64,
    /// Rows started (low 32 bits) and the version (high 32).
    counters: AtomicU64,
    frames: AtomicU64,
    channels: usize,
    /// Peak per channel (f32 bits), then the preview's.
    levels: Box<[AtomicU32]>,
    /// `channels` rings of [`SONG_SCOPE_FRAMES`] mono frames (f32 bits).
    scopes: Box<[AtomicU32]>,
    /// Frames written to each ring since the probe was made.
    written: Box<[AtomicU64]>,
    scopes_on: AtomicBool,
    alive: AtomicBool,
}

impl SongProbe {
    /// A probe for a song of `channels` channels (at most 32). Make it off the audio thread:
    /// it allocates the rings.
    pub fn new(channels: usize) -> Arc<SongProbe> {
        let channels = channels.min(SONG_MAX_CHANNELS);
        let zeros32 = |n: usize| (0..n).map(|_| AtomicU32::new(0)).collect::<Box<[_]>>();
        Arc::new(SongProbe {
            position: AtomicU64::new(0),
            counters: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            channels,
            levels: zeros32(SONG_LEVEL_SLOTS),
            scopes: zeros32(channels * SONG_SCOPE_FRAMES),
            written: (0..channels).map(|_| AtomicU64::new(0)).collect(),
            scopes_on: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        })
    }

    /// Starts or stops filling the channel scope rings. While off they cost the audio thread
    /// nothing; they keep what they had.
    pub fn set_scopes(&self, on: bool) {
        self.scopes_on.store(on, Ordering::Relaxed);
    }

    // ---- the player's side (the audio thread) ----

    /// Publishes the transport. The version goes up when the position moved.
    pub fn publish_position(&self, s: &SongStatus) {
        let mut p = 0u64;
        let mut put = |value: u64, shift: u32, bits: u32| {
            p |= (value & ((1 << bits) - 1)) << shift;
        };
        put(s.order as u64, POS_ORDER, 12);
        put(s.pattern as u64, POS_PATTERN, 12);
        put(s.row as u64, POS_ROW, 12);
        put(s.tick as u64, POS_TICK, 8);
        put(s.speed as u64, POS_SPEED, 8);
        put(s.tempo.min(511) as u64, POS_TEMPO, 9);
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

    /// Publishes the frame clock and the levels at the end of a block. Bumps the version if
    /// the levels moved.
    pub fn publish_block(&self, frames: u64, levels: &[f32; SONG_LEVEL_SLOTS]) {
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
    pub fn write_scope(&self, channel: usize, frames: &[f32]) {
        if channel >= self.channels || frames.is_empty() {
            return;
        }
        let ring = &self.scopes[channel * SONG_SCOPE_FRAMES..(channel + 1) * SONG_SCOPE_FRAMES];
        let written = &self.written[channel];
        // Only the writer changes `written`.
        let start = written.load(Ordering::Relaxed);
        let skip = frames.len().saturating_sub(SONG_SCOPE_FRAMES);
        for (i, &f) in frames.iter().enumerate().skip(skip) {
            let at = (start + i as u64) as usize % SONG_SCOPE_FRAMES;
            ring[at].store(f.to_bits(), Ordering::Relaxed);
        }
        written.store(start + frames.len() as u64, Ordering::Release);
    }

    pub fn set_alive(&self, alive: bool) {
        self.alive.store(alive, Ordering::Relaxed);
    }
}

impl SongReader for SongProbe {
    fn status(&self) -> SongStatus {
        let p = self.position.load(Ordering::Relaxed);
        let c = self.counters.load(Ordering::Relaxed);
        let field = |shift: u32, bits: u32| ((p >> shift) & ((1 << bits) - 1)) as u16;
        SongStatus {
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

    fn channels(&self) -> usize {
        self.channels
    }

    fn levels(&self, out: &mut [f32]) -> usize {
        let n = out.len().min(self.channels + 1);
        for (i, o) in out[..n].iter_mut().enumerate() {
            // The preview's slot is the one after the last channel.
            let slot = if i < self.channels {
                i
            } else {
                SONG_MAX_CHANNELS
            };
            *o = f32::from_bits(self.levels[slot].load(Ordering::Relaxed));
        }
        n
    }

    fn scope(&self, channel: usize, out: &mut [f32]) -> usize {
        if channel >= self.channels {
            return 0;
        }
        let ring = &self.scopes[channel * SONG_SCOPE_FRAMES..(channel + 1) * SONG_SCOPE_FRAMES];
        let written = &self.written[channel];
        let want = out.len().min(SONG_SCOPE_FRAMES);
        for attempt in 0..4 {
            let end = written.load(Ordering::Acquire);
            let have = (end.min(SONG_SCOPE_FRAMES as u64) as usize).min(want);
            let first = end - have as u64;
            for (i, o) in out[..have].iter_mut().enumerate() {
                let at = (first + i as u64) as usize % SONG_SCOPE_FRAMES;
                *o = f32::from_bits(ring[at].load(Ordering::Relaxed));
            }
            std::sync::atomic::fence(Ordering::Acquire);
            let now = written.load(Ordering::Acquire);
            let overwritten = now
                .saturating_sub(SONG_SCOPE_FRAMES as u64)
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

    fn scope_position(&self, channel: usize) -> u64 {
        self.written
            .get(channel)
            .map_or(0, |w| w.load(Ordering::Acquire))
    }

    fn scopes_enabled(&self) -> bool {
        self.scopes_on.load(Ordering::Relaxed)
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for SongProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SongProbe({} channels, {:?})",
            self.channels,
            SongReader::status(self)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_round_trips_through_the_packed_word() {
        let probe = SongProbe::new(4);
        let s = SongStatus {
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
        let probe = SongProbe::new(3);
        let mut levels = [0.0; SONG_LEVEL_SLOTS];
        levels[0] = 0.5;
        levels[2] = 0.25;
        levels[SONG_MAX_CHANNELS] = 0.75;
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
        let probe = SongProbe::new(2);
        let mut out = [0.0; SONG_SCOPE_FRAMES];
        assert_eq!(probe.scope(0, &mut out), 0);
        assert_eq!(probe.scope(5, &mut out), 0, "no such channel");
        let block: Vec<f32> = (0..300).map(|i| i as f32).collect();
        probe.write_scope(0, &block);
        assert_eq!(probe.scope(0, &mut out), 300);
        assert_eq!((out[0], out[299]), (0.0, 299.0));
        probe.write_scope(0, &block);
        assert_eq!(probe.scope(0, &mut out), SONG_SCOPE_FRAMES);
        assert_eq!(out[SONG_SCOPE_FRAMES - 1], 299.0);
        assert_eq!(out[0], 88.0, "212 frames of the first block are still in");
        let mut few = [0.0; 3];
        assert_eq!(probe.scope(0, &mut few), 3);
        assert_eq!(few, [297.0, 298.0, 299.0]);
        assert_eq!(probe.scope_position(0), 600);
        assert_eq!(probe.scope_position(1), 0);
        let long: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        probe.write_scope(1, &long);
        assert_eq!(probe.scope(1, &mut out), SONG_SCOPE_FRAMES);
        assert_eq!(out[SONG_SCOPE_FRAMES - 1], 999.0);
    }
}
