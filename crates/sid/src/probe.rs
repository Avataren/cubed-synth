//! What a playing [`SidSongPlayer`](crate::SidSongPlayer) shows the outside:
//! where each voice is in the song and how loud it is.
//!
//! A [`SidProbe`] is shared (`Arc`) between the audio thread, which owns the
//! player and writes it, and any other thread, which reads it. Every field is an
//! atomic, written with plain stores and read with plain loads: neither side
//! locks, waits or allocates (the probe is built off the audio thread). The
//! position is guarded by a sequence counter, so a reader always sees the three
//! voices of one frame together.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::song::SID_CHANNELS;

/// Where a player is, as of its last frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SidStatus {
    /// The subsong playing.
    pub subsong: u8,
    /// Per voice, the position in its orderlist of the row it started last.
    pub order: [u8; SID_CHANNELS],
    /// Per voice, the pattern at that position.
    pub pattern: [u8; SID_CHANNELS],
    /// Per voice, the row of that pattern.
    pub row: [u8; SID_CHANNELS],
    /// Per voice, the frames played of that row (0 on its first).
    pub tick: [u8; SID_CHANNELS],
    /// Frames per row of the reference voice now (the song's start tempo, or what a
    /// tempo command set).
    pub tempo: u8,
    /// Preview mode: no sequencer, auditions only.
    pub preview: bool,
    /// The voices an audition holds, a bit each.
    pub audition: u8,
    /// A loop is set.
    pub looping: bool,
    /// An orderlist has wrapped to its restart at least once.
    pub looped: bool,
    /// Muted and soloed voices, a bit each.
    pub mute: u8,
    pub solo: u8,
    /// The reference voice's row counted from the top of the subsong, and the
    /// song's length in rows (the longest voice's first pass).
    pub song_row: u64,
    pub song_rows: u64,
    /// Frames played so far (a seek's replay is not counted).
    pub frames: u64,
    /// Changes whenever anything here or any peak has moved, and stays put while
    /// everything is still: a host that posts the probe somewhere can skip a quiet one.
    pub version: u32,
}

/// A player's position and peaks, readable from any thread. See the module docs.
#[derive(Debug)]
pub struct SidProbe {
    /// Odd while the position is being written.
    seq: AtomicU32,
    /// Per voice: order, pattern, row, tick, a byte each.
    voices: [AtomicU32; SID_CHANNELS],
    /// Subsong, tempo, audition, mute, solo (a byte each) and the flags.
    meta: AtomicU64,
    song_row: AtomicU64,
    song_rows: AtomicU64,
    frames: AtomicU64,
    version: AtomicU32,
    /// Peak per voice (f32 bits).
    levels: [AtomicU32; SID_CHANNELS],
}

const FLAG_PREVIEW: u64 = 1;
const FLAG_LOOPING: u64 = 2;
const FLAG_LOOPED: u64 = 4;

impl SidProbe {
    /// A probe for one player. Make it off the audio thread.
    pub fn new() -> Arc<SidProbe> {
        Arc::new(SidProbe {
            seq: AtomicU32::new(0),
            voices: Default::default(),
            meta: AtomicU64::new(0),
            song_row: AtomicU64::new(0),
            song_rows: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            version: AtomicU32::new(0),
            levels: Default::default(),
        })
    }

    /// Writes the position (the player does, once a frame).
    pub(crate) fn publish(&self, s: &SidStatus, peaks: &[f32; SID_CHANNELS]) {
        let voice = |c: usize| {
            s.order[c] as u32
                | (s.pattern[c] as u32) << 8
                | (s.row[c] as u32) << 16
                | (s.tick[c] as u32) << 24
        };
        let packed: [u32; SID_CHANNELS] = [voice(0), voice(1), voice(2)];
        let mut meta = s.subsong as u64
            | (s.tempo as u64) << 8
            | (s.audition as u64) << 16
            | (s.mute as u64) << 24
            | (s.solo as u64) << 32;
        for (flag, on) in [
            (FLAG_PREVIEW, s.preview),
            (FLAG_LOOPING, s.looping),
            (FLAG_LOOPED, s.looped),
        ] {
            if on {
                meta |= flag << 40;
            }
        }
        let same = (0..SID_CHANNELS).all(|c| self.voices[c].load(Ordering::Relaxed) == packed[c])
            && self.meta.load(Ordering::Relaxed) == meta
            && self.song_row.load(Ordering::Relaxed) == s.song_row
            && self.song_rows.load(Ordering::Relaxed) == s.song_rows;
        if !same {
            self.seq.fetch_add(1, Ordering::Release);
            for (v, p) in self.voices.iter().zip(packed) {
                v.store(p, Ordering::Relaxed);
            }
            self.meta.store(meta, Ordering::Relaxed);
            self.song_row.store(s.song_row, Ordering::Relaxed);
            self.song_rows.store(s.song_rows, Ordering::Relaxed);
            self.seq.fetch_add(1, Ordering::Release);
            self.version.fetch_add(1, Ordering::Relaxed);
        }
        self.frames.store(s.frames, Ordering::Relaxed);
        self.publish_levels(peaks);
    }

    /// Writes the peaks (the player does, once a block).
    pub(crate) fn publish_levels(&self, peaks: &[f32; SID_CHANNELS]) {
        let mut moved = false;
        for (slot, p) in self.levels.iter().zip(peaks) {
            // Peaks fall continuously: call it a change when one moves by a visible amount.
            let old = f32::from_bits(slot.load(Ordering::Relaxed));
            if (old - p).abs() > 1e-4 {
                moved = true;
                slot.store(p.to_bits(), Ordering::Relaxed);
            }
        }
        if moved {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The position as last published: the three voices of one frame together.
    pub fn status(&self) -> SidStatus {
        for _ in 0..8 {
            let before = self.seq.load(Ordering::Acquire);
            if before & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let voices: [u32; SID_CHANNELS] =
                std::array::from_fn(|c| self.voices[c].load(Ordering::Relaxed));
            let meta = self.meta.load(Ordering::Relaxed);
            let song_row = self.song_row.load(Ordering::Relaxed);
            let song_rows = self.song_rows.load(Ordering::Relaxed);
            std::sync::atomic::fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return self.build(voices, meta, song_row, song_rows);
            }
        }
        // The writer is busy: the latest anyway.
        let voices = std::array::from_fn(|c| self.voices[c].load(Ordering::Relaxed));
        self.build(
            voices,
            self.meta.load(Ordering::Relaxed),
            self.song_row.load(Ordering::Relaxed),
            self.song_rows.load(Ordering::Relaxed),
        )
    }

    fn build(
        &self,
        voices: [u32; SID_CHANNELS],
        meta: u64,
        song_row: u64,
        song_rows: u64,
    ) -> SidStatus {
        let byte = |v: u32, shift: u32| (v >> shift) as u8;
        let flags = meta >> 40;
        SidStatus {
            subsong: meta as u8,
            order: voices.map(|v| byte(v, 0)),
            pattern: voices.map(|v| byte(v, 8)),
            row: voices.map(|v| byte(v, 16)),
            tick: voices.map(|v| byte(v, 24)),
            tempo: (meta >> 8) as u8,
            audition: (meta >> 16) as u8,
            mute: (meta >> 24) as u8,
            solo: (meta >> 32) as u8,
            preview: flags & FLAG_PREVIEW != 0,
            looping: flags & FLAG_LOOPING != 0,
            looped: flags & FLAG_LOOPED != 0,
            song_row,
            song_rows,
            frames: self.frames.load(Ordering::Relaxed),
            version: self.version.load(Ordering::Relaxed),
        }
    }

    /// Each voice's recent peak (`SidSongPlayer::peaks`), linear.
    pub fn levels(&self) -> [f32; SID_CHANNELS] {
        std::array::from_fn(|c| f32::from_bits(self.levels[c].load(Ordering::Relaxed)))
    }
}
