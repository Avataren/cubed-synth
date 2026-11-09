//! PSID and RSID tunes (`.sid`, the C64's music files) under the same control as the other formats:
//! a [`SongPlayer`] over `cubed_sid::psid`'s `PsidPlayer`. A tune is **a program**, not a document: it
//! plays, stops and carries on from where it stopped, and that is all.
//!
//! - **Transport.** `Play` and `Stop` (a stopped tune is silent and its CPU does not run: the
//!   position stays). A tune comes to no end of its own (nearly all loop for ever): if its code stops the CPU
//!   the player reports the transport as stopped and `status().pattern` is 1 (0 otherwise). `Seek`, `Loop` and
//!   the previews are **ignored on the audio thread**: a seek runs the tune's code from the start (a seek back
//!   starts it again) and a start runs its init routine (up to 60 million cycles), neither of which belongs
//!   there. A host that wants a tune at a place builds it there: [`open_psid_at`] with the subsong and the
//!   seconds, off the audio thread.
//! - **Position.** `status().order` is the subsong (0-based), `row` the whole seconds played and `tick` the
//!   hundredths of the second after them (so the position in seconds is `row + tick / 100`), `rows` the whole
//!   seconds, `frames` the samples rendered; `speed` is 1 and `tempo` 50 (a tune has no rows).
//! - **Channels** are the voices of the first SID (up to three SIDs play, each on a chip of its own, mixed as
//!   their average; the others' voices are heard but not shown). A mute or solo bit `i` silences voice `i` of
//!   every chip. Levels are each voice's recent peak (24 dB a second down), the scopes the voices' own signals
//!   (before the filter), from the player's taps.
//! - **Build** [`open_psid_at`] off the audio thread: the player's `new` runs the tune's init routine. The
//!   worklet builds it in its message handler (a big tune can glitch the sound once).

use cubed_sid::psid::{PsidOptions, PsidPlayer};

use super::player::{SongPlayer, CHUNK};
use crate::song::{SongControl, SongStatus};

/// How fast a level falls once its voice is quiet (24 dB a second, as the other players').
const LEVEL_FALL_DB_PER_SECOND: f64 = 24.0;

/// A PSID tune. See the module docs.
pub struct PsidSongPlayer {
    player: PsidPlayer,
    playing: bool,
    mute: u32,
    solo: u32,
    scopes: bool,
    /// The first chip's voices in the block just rendered.
    taps: [[f32; CHUNK]; 3],
    block: usize,
    levels: [f32; 3],
    /// Per block's worth of frames: how much a level keeps (computed for a `CHUNK`; scaled by the block).
    fall_per_frame: f32,
}

impl PsidSongPlayer {
    /// A stopped player of subsong `subsong` (0-based; `None` the file's own start song), run to `seconds`
    /// into it. Runs the tune's code: build it off the audio thread.
    pub fn new(
        bytes: &[u8],
        sample_rate: f32,
        subsong: Option<usize>,
        seconds: f64,
    ) -> Result<PsidSongPlayer, String> {
        let options = PsidOptions {
            subsong,
            ..PsidOptions::default()
        };
        let mut player =
            PsidPlayer::new(bytes, sample_rate as f64, options).map_err(|e| e.to_string())?;
        if seconds > 0.0 {
            let _ = player.seek(seconds);
        }
        Ok(PsidSongPlayer {
            player,
            playing: false,
            mute: 0,
            solo: 0,
            scopes: false,
            taps: [[0.0; CHUNK]; 3],
            block: 0,
            levels: [0.0; 3],
            fall_per_frame: 10f64.powf(-LEVEL_FALL_DB_PER_SECOND / 20.0 / sample_rate as f64)
                as f32,
        })
    }

    /// The player (tests).
    pub fn player(&self) -> &PsidPlayer {
        &self.player
    }
}

impl SongPlayer for PsidSongPlayer {
    fn channels(&self) -> usize {
        3
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len()).min(CHUNK);
        if !self.playing || self.player.ended().is_some() {
            left[..n].fill(0.0);
            right[..n].fill(0.0);
            self.block = n;
            for tap in &mut self.taps {
                tap[..n].fill(0.0);
            }
        } else {
            let [t0, t1, t2] = &mut self.taps;
            self.player
                .render_taps(&mut left[..n], [&mut t0[..n], &mut t1[..n], &mut t2[..n]]);
            right[..n].copy_from_slice(&left[..n]);
            self.block = n;
        }
        // Each voice's recent peak: the block's loudest frame, falling away after it.
        let fall = self.fall_per_frame.powi(n as i32);
        for (level, tap) in self.levels.iter_mut().zip(&self.taps) {
            let peak = tap[..n].iter().fold(0.0f32, |m, s| m.max(s.abs()));
            *level = (*level * fall).max(peak);
            if *level < 1e-4 {
                *level = 0.0;
            }
        }
    }

    /// A tune has no end of its own.
    fn ended(&self) -> bool {
        false
    }

    fn control(&mut self, control: SongControl) {
        match control {
            SongControl::Play => self.playing = true,
            SongControl::Stop => self.playing = false,
            SongControl::Mute { channel, on } => {
                let bit = 1u32.checked_shl(channel as u32).unwrap_or(0);
                self.mute = if on {
                    self.mute | bit
                } else {
                    self.mute & !bit
                };
                self.player.set_mute_solo(self.mute, self.solo);
            }
            SongControl::Solo { channel, on } => {
                let bit = 1u32.checked_shl(channel as u32).unwrap_or(0);
                self.solo = if on {
                    self.solo | bit
                } else {
                    self.solo & !bit
                };
                self.player.set_mute_solo(self.mute, self.solo);
            }
            SongControl::MuteMask(mask) => {
                self.mute = mask;
                self.player.set_mute_solo(self.mute, self.solo);
            }
            SongControl::SoloMask(mask) => {
                self.solo = mask;
                self.player.set_mute_solo(self.mute, self.solo);
            }
            // A seek and a start run the tune's code: not on the audio thread. The rest is not a
            // tune's.
            _ => {}
        }
    }

    fn status(&self) -> SongStatus {
        let ended = self.player.ended().is_some();
        let seconds = self.player.position_seconds();
        SongStatus {
            playing: self.playing && !ended,
            preview: false,
            order: self.player.subsong() as u16,
            pattern: ended as u16,
            row: seconds as u64 as u16,
            tick: ((seconds.fract() * 100.0) as u8).min(99),
            speed: 1,
            tempo: 50,
            rows: seconds as u32,
            frames: self.player.frames(),
            version: 0,
        }
    }

    fn levels(&self, out: &mut [f32]) -> f32 {
        for (o, l) in out.iter_mut().zip(self.levels) {
            *o = l;
        }
        0.0
    }

    fn set_scopes(&mut self, on: bool) {
        self.scopes = on;
    }

    fn scope(&self, channel: usize, out: &mut [f32]) -> usize {
        let Some(tap) = self.taps.get(channel) else {
            return 0;
        };
        let n = out.len().min(self.block);
        out[..n].copy_from_slice(&tap[..n]);
        n
    }
}
