//! What a playing [`AhxPlayer`](crate::player::AhxPlayer) shows the outside:
//! where the song is and how loud each channel is.
//!
//! An [`AhxProbe`] is shared (`Arc`) between the audio thread, which owns the
//! player and writes it, and any other thread, which reads it. Every field is an
//! atomic, written with plain stores and read with plain loads: neither side
//! locks, waits or allocates (the probe is built off the audio thread). The
//! position is packed into one word, so a reader always sees a position, a row,
//! a tick and a speed that belong together. It follows `cubed_tracker::Probe`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::engine::LEVEL_SLOTS;

/// Where the transport is, as the player last reported it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AhxStatus {
    /// The song is playing (not stopped).
    pub playing: bool,
    /// A note is being auditioned (sounding or releasing).
    pub audition: bool,
    /// The song has reached its end (it then loops from its restart position).
    pub song_end: bool,
    /// Position in the position list.
    pub position: u16,
    /// The row playing (or the one a stopped song will start at).
    pub row: u16,
    /// The tick of the row playing, 0 on its first.
    pub tick: u8,
    /// Ticks per row (the song's current speed).
    pub speed: u8,
    /// 1 to 4: a tick lasts `1 / (50 * speed_multiplier)` seconds.
    pub speed_multiplier: u8,
    /// Rows started since the player was made or last rewound: it goes up by
    /// one for every row, so a follower that polls slower than the song can tell
    /// how many rows it missed.
    pub rows: u32,
    /// Frames the player has rendered.
    pub frames: u64,
    /// Changes whenever anything a UI shows has moved (the position, a level, the
    /// transport) and stays put while everything is still: a host that posts the
    /// probe somewhere can skip a quiet one.
    pub version: u32,
}

const POS_POSITION: u32 = 0;
const POS_ROW: u32 = 12;
const POS_TICK: u32 = 20;
const POS_SPEED: u32 = 28;
const POS_MULT: u32 = 36;
const POS_PLAYING: u32 = 39;
const POS_AUDITION: u32 = 40;
const POS_END: u32 = 41;

/// A player's position and levels, readable from any thread. See the module
/// docs.
pub struct AhxProbe {
    position: AtomicU64,
    /// Rows started (low 32 bits) and the version (high 32).
    counters: AtomicU64,
    frames: AtomicU64,
    channels: usize,
    /// Peak per channel (f32 bits), then the audition's.
    levels: Box<[AtomicU32]>,
    alive: AtomicBool,
}

impl AhxProbe {
    /// A probe for a song of `channels` channels (at most 16). Make it off the
    /// audio thread: it allocates.
    pub fn new(channels: usize) -> Arc<AhxProbe> {
        Arc::new(AhxProbe {
            position: AtomicU64::new(0),
            counters: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            channels: channels.min(crate::format::MAX_CHANNELS),
            levels: (0..LEVEL_SLOTS).map(|_| AtomicU32::new(0)).collect(),
            alive: AtomicBool::new(true),
        })
    }

    /// The song's channel count.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The transport as last published.
    pub fn status(&self) -> AhxStatus {
        let p = self.position.load(Ordering::Relaxed);
        let c = self.counters.load(Ordering::Relaxed);
        let field = |shift: u32, bits: u32| ((p >> shift) & ((1 << bits) - 1)) as u16;
        AhxStatus {
            playing: p >> POS_PLAYING & 1 == 1,
            audition: p >> POS_AUDITION & 1 == 1,
            song_end: p >> POS_END & 1 == 1,
            position: field(POS_POSITION, 12),
            row: field(POS_ROW, 8),
            tick: field(POS_TICK, 8) as u8,
            speed: field(POS_SPEED, 8) as u8,
            speed_multiplier: field(POS_MULT, 3) as u8,
            rows: c as u32,
            frames: self.frames.load(Ordering::Relaxed),
            version: (c >> 32) as u32,
        }
    }

    /// Each channel's recent peak, linear, in `out` (as many as fit), then the
    /// audition's; returns how many it wrote (`channels() + 1` at most). A
    /// peak is the loudest wave sample a channel could make of its volume and
    /// pan at the latest tick, falling 24 dB a second after it, so a display that
    /// polls slower than the song does not miss a hit; a silenced channel reads 0.
    pub fn levels(&self, out: &mut [f32]) -> usize {
        let n = out.len().min(self.channels + 1);
        for (i, o) in out[..n].iter_mut().enumerate() {
            let slot = if i < self.channels {
                i
            } else {
                crate::format::MAX_CHANNELS
            };
            *o = f32::from_bits(self.levels[slot].load(Ordering::Relaxed));
        }
        n
    }

    /// False once the player that writes this probe has been dropped.
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    // ---- the player's side ----

    /// Publishes the transport and the levels at the end of a block. Bumps the
    /// version if anything shown moved.
    pub(crate) fn publish(&self, s: &AhxStatus, levels: &[f32; LEVEL_SLOTS]) {
        let mut p = 0u64;
        let mut put = |value: u64, shift: u32, bits: u32| {
            p |= (value & ((1 << bits) - 1)) << shift;
        };
        put(s.position as u64, POS_POSITION, 12);
        put(s.row as u64, POS_ROW, 8);
        put(s.tick as u64, POS_TICK, 8);
        put(s.speed as u64, POS_SPEED, 8);
        put(s.speed_multiplier as u64, POS_MULT, 3);
        put(s.playing as u64, POS_PLAYING, 1);
        put(s.audition as u64, POS_AUDITION, 1);
        put(s.song_end as u64, POS_END, 1);
        let old = self.position.swap(p, Ordering::Relaxed);
        let c = self.counters.load(Ordering::Relaxed);
        let (mut rows, mut version) = (c as u32, (c >> 32) as u32);
        let mut moved = old != p || rows != s.rows;
        rows = s.rows;
        for (cell, &level) in self.levels.iter().zip(levels) {
            moved |= f32::from_bits(cell.load(Ordering::Relaxed)) != level;
            cell.store(level.to_bits(), Ordering::Relaxed);
        }
        if moved {
            version = version.wrapping_add(1);
        }
        self.counters
            .store((version as u64) << 32 | rows as u64, Ordering::Relaxed);
        self.frames.store(s.frames, Ordering::Relaxed);
    }

    pub(crate) fn set_alive(&self, alive: bool) {
        self.alive.store(alive, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for AhxProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AhxProbe({} channels, {:?})",
            self.channels,
            self.status()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_round_trips_through_the_packed_word() {
        let probe = AhxProbe::new(4);
        let s = AhxStatus {
            playing: true,
            audition: true,
            song_end: true,
            position: 999,
            row: 63,
            tick: 200,
            speed: 255,
            speed_multiplier: 4,
            rows: 77,
            frames: 5,
            version: 0,
        };
        let mut levels = [0.0; LEVEL_SLOTS];
        levels[1] = 0.5;
        levels[crate::format::MAX_CHANNELS] = 0.25;
        probe.publish(&s, &levels);
        let back = probe.status();
        assert_eq!(
            (back.position, back.row, back.tick, back.speed),
            (999, 63, 200, 255)
        );
        assert!(back.playing && back.audition && back.song_end);
        assert_eq!((back.speed_multiplier, back.rows, back.frames), (4, 77, 5));
        assert_eq!(back.version, 1);
        let mut out = [9.0; 8];
        assert_eq!(probe.levels(&mut out), 5);
        assert_eq!(out[..5], [0.0, 0.5, 0.0, 0.0, 0.25]);
        probe.publish(&s, &levels);
        assert_eq!(
            probe.status().version,
            1,
            "the same state leaves the version"
        );
    }
}
