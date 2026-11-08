//! Song formats under live control: the trait a format implements, and the source that
//! makes any such player a controllable, readable voice.
//!
//! A tracker editor plays the song it edits and wants the same of every format: start,
//! stop, seek, repeat part of it, mute and solo channels, hear a note of an instrument, see
//! where the song is and how loud each channel is, and have an edit land in the song while
//! it plays. [`SongPlayer`] is what a format's player has to offer for that; [`PlayerSong`]
//! turns one into a [`Source`] that takes `SourceEvent::Song(SongControl)`, publishes its
//! state to a [`SongProbe`] after every block (`Source::song_reader`) and keeps its voice
//! for as long as the editor has the song open. Modules (`ModuleSong` in `songs.rs`) keep
//! their own player's probe; every other format is one `impl SongPlayer` (see "Adding a song
//! format" in the README).

use std::sync::Arc;

use crate::probe::SongProbe;
use crate::song::{SongControl, SongReader, SongStatus, SONG_LEVEL_SLOTS, SONG_MAX_CHANNELS};
use crate::source::{Source, SourceEvent};

/// The most frames a player is asked for at once; longer blocks are split, so a player's
/// buffers never grow past what its warm-up sized them for.
pub const CHUNK: usize = 256;

/// A format's player, as [`PlayerSong`] drives it on the audio thread: none of these may
/// allocate, lock or block (build and warm the player up off the audio thread).
pub trait SongPlayer: Send + 'static {
    /// The song's channels (tracks, voices) at most 32: what `levels` and `scope` index.
    fn channels(&self) -> usize;

    /// Writes the next `left.len()` frames (`CHUNK` at most) over `left` and `right`:
    /// silence while the transport is stopped.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]);

    /// Whether the song has come to its end, or round once: with `Looped(false)` the song
    /// then stops at the top. Reset by a seek.
    fn ended(&self) -> bool;

    /// A transport change or live edit. `Looped` and `Scopes` never arrive here (the
    /// adapter keeps the first and calls [`set_scopes`](Self::set_scopes) for the second);
    /// a control the format has no use for is ignored.
    fn control(&mut self, control: SongControl);

    /// Where the song is. `version` is the adapter's to fill.
    fn status(&self) -> SongStatus;

    /// Each channel's recent peak into `out[..channels]`, linear, after the player's own
    /// gain (the loudest frame of the latest block, falling away after it; a muted channel
    /// reads 0); returns the note preview's.
    fn levels(&self, out: &mut [f32]) -> f32;

    /// Switches the per-channel scope data on or off (a player that has none ignores it).
    fn set_scopes(&mut self, on: bool) {
        let _ = on;
    }

    /// The frames `channel` made in the block `render` just wrote (mono; at most that
    /// many), into `out`; returns how many. 0 for a format that has no per-channel sound
    /// (its scopes then read empty).
    fn scope(&self, channel: usize, out: &mut [f32]) -> usize {
        let _ = (channel, out);
        0
    }
}

/// A [`SongPlayer`] as a mixer source. See the module docs.
pub struct PlayerSong<P: SongPlayer> {
    player: P,
    probe: Arc<SongProbe>,
    /// Under transport control: the voice never ends by itself.
    controlled: bool,
    /// The song starts again from the top at its end.
    looped: bool,
    scopes: bool,
    channels: usize,
    scratch: [f32; CHUNK],
}

impl<P: SongPlayer> PlayerSong<P> {
    /// `controlled` keeps the voice for as long as the host wants it (at the end of a song
    /// that does not repeat the transport stops at the top); otherwise the voice ends with
    /// the song. `looped` makes the song go round. The player is built (and warmed up) by
    /// the caller, off the audio thread.
    pub fn new(player: P, controlled: bool, looped: bool) -> PlayerSong<P> {
        let channels = player.channels().min(SONG_MAX_CHANNELS);
        let song = PlayerSong {
            probe: SongProbe::new(channels),
            player,
            controlled,
            looped,
            scopes: false,
            channels,
            scratch: [0.0; CHUNK],
        };
        song.publish();
        song
    }

    /// Publishes the player's state: the position and the levels.
    fn publish(&self) {
        let status = self.player.status();
        self.probe.publish_position(&status);
        let mut levels = [0.0; SONG_LEVEL_SLOTS];
        levels[SONG_MAX_CHANNELS] = self.player.levels(&mut levels[..self.channels]);
        self.probe.publish_block(status.frames, &levels);
    }

    fn write_scopes(&mut self, frames: usize) {
        for channel in 0..self.channels {
            let got = self.player.scope(channel, &mut self.scratch[..frames]);
            self.probe.write_scope(channel, &self.scratch[..got]);
        }
    }

    /// The player, for the host's tests and tools.
    pub fn player(&self) -> &P {
        &self.player
    }
}

impl<P: SongPlayer> Source for PlayerSong<P> {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let n = left.len().min(right.len());
        let mut at = 0;
        let mut more = true;
        while at < n {
            let k = (n - at).min(CHUNK);
            self.player
                .render(&mut left[at..at + k], &mut right[at..at + k]);
            if self.scopes {
                self.write_scopes(k);
            }
            at += k;
            if self.player.ended() && !self.looped {
                if self.controlled {
                    // The song ran out: stop at the top, and keep the voice.
                    self.player.control(SongControl::Stop);
                    self.player.control(SongControl::Seek { order: 0, row: 0 });
                } else {
                    more = false;
                }
            }
        }
        self.publish();
        more
    }

    fn event(&mut self, event: SourceEvent) {
        let SourceEvent::Song(control) = event else {
            return;
        };
        match control {
            SongControl::Looped(on) => self.looped = on,
            SongControl::Scopes(on) => {
                self.scopes = on;
                self.probe.set_scopes(on);
                self.player.set_scopes(on);
            }
            other => self.player.control(other),
        }
    }

    fn song_reader(&self) -> Option<Arc<dyn SongReader>> {
        Some(self.probe.clone())
    }
}

impl<P: SongPlayer> Drop for PlayerSong<P> {
    fn drop(&mut self) {
        self.probe.set_alive(false);
    }
}
