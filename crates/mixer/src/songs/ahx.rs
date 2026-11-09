//! AHX and HivelyTracker songs under live control: a [`SongPlayer`] over `cubed_ahx`'s
//! `AhxPlayer`.
//!
//! The transport maps one to one (`AhxPlayer` has play and pause, a seek that replays the
//! song silently to the row, loops of a position, positions or rows, mute and solo by channel,
//! and an audition of an instrument on a voice of its own, so a preview needs nothing
//! besides it). Channels are the song's (4 for AHX, 4 to 16 for HVL).
//!
//! Live edits that fit the wire: `SetCell` (a step: `pattern` is the track, the note is the
//! file's 0-63, the second effect column in `period`), `SetOrder` (a position's track and
//! transpose for a channel) and `SetRestart`. An instrument is bigger than a wire message
//! (its PList is up to 255 rows), so an instrument edit is a new song, as is everything that
//! changes the song's shape (the track length, positions inserted, deleted or moved, the speed
//! multiplier): the host builds the edited song and swaps it in, and a held preview note is
//! struck again on the new voice.
//!
//! Levels are `AhxPlayer::levels` (a bound on the channel's loudest sample at the latest tick,
//! falling 24 dB a second). There are no scopes: the engine's per-voice capture allocates when
//! it is switched on, which the audio thread may not do.
//!
//! `status().tempo` is the timer's rate in Hz (50 x the speed multiplier) and `speed` the ticks
//! per row; `pattern` is the position (AHX has no patterns, a position names a track per channel).

use cubed_ahx::engine::Loop;
use cubed_ahx::format::Song;
use cubed_ahx::player::AhxPlayer;

use super::player::{SongPlayer, CHUNK};
use crate::song::{SongControl, SongLoop, SongStatus, SONG_MAX_CHANNELS};

/// AHX's stereo separation for `.ahx` files (HVL files carry their own).
const STEREO_MODE: u8 = 2;
/// Output frames played (and thrown away) before the song starts, so the buffers that grow on
/// first use have grown before the audio thread plays it.
const WARM_UP_FRAMES: usize = 8_192;

/// An AHX or HVL song. See the module docs.
pub struct AhxSongPlayer {
    player: AhxPlayer,
}

impl AhxSongPlayer {
    /// A paused player at the start of `song` (subsong `subsong`; 0 is the main song). Plays
    /// its first moments to warm up: make it off the audio thread.
    pub fn new(song: Song, sample_rate: f32, subsong: usize) -> Result<AhxSongPlayer, String> {
        let mut player = AhxPlayer::from_song(song, sample_rate.round() as u32, STEREO_MODE)?;
        player.set_levels(true);
        player.play();
        let (mut left, mut right) = ([0.0; CHUNK], [0.0; CHUNK]);
        for _ in 0..WARM_UP_FRAMES / CHUNK {
            player.render(&mut left, &mut right);
        }
        // Back to the start, paused, with nothing the warm-up measured left in the levels.
        if !player.restart(subsong) {
            player.restart(0);
        }
        player.set_levels(false);
        player.set_levels(true);
        Ok(AhxSongPlayer { player })
    }

    /// The player (tests).
    pub fn player(&self) -> &AhxPlayer {
        &self.player
    }
}

impl SongPlayer for AhxSongPlayer {
    fn channels(&self) -> usize {
        self.player.channels().min(SONG_MAX_CHANNELS)
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.player.render(left, right);
    }

    fn ended(&self) -> bool {
        self.player.song_end_reached()
    }

    fn control(&mut self, control: SongControl) {
        let p = &mut self.player;
        match control {
            SongControl::Play => p.play(),
            SongControl::Stop => p.pause(),
            SongControl::Seek { order, row } => {
                p.seek(order as usize, row as usize);
            }
            SongControl::Loop(repeat) => {
                match repeat {
                    SongLoop::None => p.set_loop(Loop::None),
                    SongLoop::Pattern { order } => p.set_loop(Loop::Position(order as usize)),
                    SongLoop::Range { order, row0, row1 } => p.set_loop(Loop::Rows {
                        position: order as usize,
                        row0: row0 as usize,
                        row1: row1 as usize,
                    }),
                };
            }
            SongControl::Mute { channel, on } => p.set_mute(channel as usize, on),
            SongControl::Solo { channel, on } => p.set_solo(channel as usize, on),
            SongControl::MuteMask(mask) => {
                let (_, solo) = p.engine().mute_solo();
                p.set_mute_solo(mask, solo);
            }
            SongControl::SoloMask(mask) => {
                let (mute, _) = p.engine().mute_solo();
                p.set_mute_solo(mute, mask);
            }
            SongControl::PreviewOn {
                instrument,
                note,
                velocity,
                duration,
            } => {
                let velocity = (velocity.clamp(0.0, 1.0) * 127.0).round() as u32;
                p.audition_on(
                    instrument as usize,
                    note as i32,
                    velocity,
                    duration as usize,
                );
            }
            SongControl::PreviewOff => p.audition_off(),
            SongControl::SetCell(cell) => {
                p.set_step(
                    cell.pattern as usize,
                    cell.row as usize,
                    cell.note,
                    cell.instrument,
                    cell.effect,
                    cell.param,
                    (cell.period >> 8) as u8,
                    cell.period as u8,
                );
            }
            SongControl::SetOrder {
                order,
                channel,
                pattern,
                transpose,
            } => {
                p.set_position_entry(order as usize, channel as usize, pattern, transpose as i32);
            }
            SongControl::SetRestart(order) => {
                p.set_restart(order as usize);
            }
            // The adapter's, and other formats'.
            SongControl::Looped(_)
            | SongControl::Scopes(_)
            | SongControl::SetInstrument(_)
            | SongControl::Timing { .. }
            | SongControl::SetTableRow { .. }
            | SongControl::SeekVoice { .. }
            | SongControl::VoiceLoop { .. } => {}
        }
    }

    fn status(&self) -> SongStatus {
        let s = self.player.status();
        let (mut order, mut row, mut tick) = (s.position, s.row, s.tick);
        // The engine shows the tick rendered last. A stopped song whose last tick was a row's
        // last (after a seek: the one before the target) resumes at the next row, which is
        // where its cursor belongs.
        if !s.playing && tick as u16 + 1 >= s.speed.max(1) as u16 {
            let engine = self.player.engine();
            order = engine.pos_nr().max(0) as u16;
            row = engine.note_nr().max(0) as u16;
            tick = 0;
        }
        SongStatus {
            playing: s.playing,
            preview: s.audition,
            order,
            pattern: order,
            row,
            tick,
            speed: s.speed,
            tempo: 50 * s.speed_multiplier as u16,
            rows: s.rows,
            frames: s.frames,
            version: 0,
            voices: Default::default(),
        }
    }

    fn levels(&self, out: &mut [f32]) -> f32 {
        // The player's levels are the channels', then the audition's.
        let mut all = [0.0f32; SONG_MAX_CHANNELS + 1];
        let n = self.player.levels(&mut all);
        let channels = out.len().min(self.channels());
        out[..channels].copy_from_slice(&all[..channels]);
        if n > channels {
            all[self.channels()]
        } else {
            0.0
        }
    }
}
