//! Live control and readout of a playing tracker module (MOD, S3M, XM).
//!
//! A module song is a source (`songs::open_module_song`) whose transport an
//! editor drives while it plays, and whose state it reads back every frame.
//!
//! **Control** is [`SongControl`], sent as `SourceEvent::Song` in a
//! `Command::SourceEvent` to the song's voice: play and stop, seek (also while
//! stopped), loop a pattern or a range of rows, mute and solo channels, preview a
//! note of an instrument on a spare channel, switch the channel scopes on. It has a
//! fixed-size byte encoding (`wire` tag 30), so it crosses the worklet's message
//! port like every other command.
//!
//! **Readout** is [`SongReader`]: the position (order, pattern, row, tick, speed,
//! tempo), a peak level per channel, and the latest frames of each channel for a
//! scope. Natively a reader is the song's own state: ask the source for it
//! (`Source::song_reader`) before handing it to the mixer, and read it from any
//! thread (atomics, no waiting). On the web the worklet owns the song, so it posts
//! `'songpos'` and `'songscope'` messages, and [`WebSong`] holds the latest on the
//! page and implements the same trait.

/// Channels a module has at most.
pub const SONG_MAX_CHANNELS: usize = 32;
/// Level slots per song: every channel, then the note preview's.
pub const SONG_LEVEL_SLOTS: usize = SONG_MAX_CHANNELS + 1;
/// Frames of each channel a scope reader gets at most.
pub const SONG_SCOPE_FRAMES: usize = 512;
/// `u32`s in a song's status as the worklet posts it (see [`SongStatus::to_words`]).
pub const SONG_STATUS_WORDS: usize = 12;

/// What a module's transport repeats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SongLoop {
    /// Play on through the song.
    #[default]
    None,
    /// Repeat the pattern at this position of the order list.
    Pattern { order: u16 },
    /// Repeat rows `row0` to `row1` (both played) of the pattern at `order`.
    Range { order: u16, row0: u16, row1: u16 },
}

/// A change to a module song's transport (`SourceEvent::Song`). The song must
/// have been opened by `songs::open_module_song` or `open_module`; other
/// sources ignore these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SongControl {
    /// Starts from the current position (where a seek or a stop left it).
    Play,
    /// Stops where it is; the notes die away, the position stays. A previewed
    /// note goes on.
    Stop,
    /// Moves to `order` and `row`, from a clean state; works while stopped (the
    /// position moves, and `Play` starts there). Out of range clamps.
    Seek {
        order: u16,
        row: u16,
    },
    /// Repeats a pattern or a range of rows (see [`SongLoop`]); it does not move
    /// the playhead, so seek first.
    Loop(SongLoop),
    Mute {
        channel: u8,
        on: bool,
    },
    Solo {
        channel: u8,
        on: bool,
    },
    /// Every channel's mute at once, a bit each (bit 0 is channel 0).
    MuteMask(u32),
    /// Every channel's solo at once. While any is set only soloed channels are
    /// heard, muted or not.
    SoloMask(u32),
    /// Plays `instrument` (1-based, as in a cell) at MIDI `note` on the spare
    /// channel, whatever the song and the mutes do. `velocity` 0-1 scales the
    /// sample's own volume; the note lets go by itself after `duration` frames
    /// (0 holds it until `PreviewOff`).
    PreviewOn {
        instrument: u8,
        note: u8,
        velocity: f32,
        duration: u32,
    },
    PreviewOff,
    /// Whether the song starts again from the top at its end (a song under
    /// transport control stops at its end otherwise).
    Looped(bool),
    /// Switches the per-channel scope rings on or off. They cost the audio
    /// thread nothing while off.
    Scopes(bool),
}

/// Where a module song's transport is, as the player last published it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SongStatus {
    /// The song is playing (not stopped).
    pub playing: bool,
    /// A previewed note is sounding.
    pub preview: bool,
    /// Position in the order list.
    pub order: u16,
    /// The pattern at that position.
    pub pattern: u16,
    /// The row playing, or the row a stopped song will start at.
    pub row: u16,
    /// The tick of the row playing, from 0.
    pub tick: u8,
    /// Ticks per row.
    pub speed: u8,
    /// Beats per minute: a tick lasts 2.5 / tempo seconds.
    pub tempo: u16,
    /// Rows started since the song was opened (a repeated row counts): a
    /// follower that polls slower than the song can tell how many it missed.
    pub rows: u32,
    /// Frames the song has rendered.
    pub frames: u64,
    /// Changes whenever anything shown has moved and stays put while all is still,
    /// so a poster can skip a quiet song.
    pub version: u32,
}

impl SongStatus {
    /// The words the worklet posts for a song: flags (bit 0 playing, 1 preview, 2
    /// alive, 3 scopes on), order, pattern, row, tick, speed, tempo, rows,
    /// frames (low, high), version, channels.
    pub fn to_words(&self, channels: usize, alive: bool, scopes: bool) -> [u32; SONG_STATUS_WORDS] {
        [
            self.playing as u32
                | (self.preview as u32) << 1
                | (alive as u32) << 2
                | (scopes as u32) << 3,
            self.order as u32,
            self.pattern as u32,
            self.row as u32,
            self.tick as u32,
            self.speed as u32,
            self.tempo as u32,
            self.rows,
            self.frames as u32,
            (self.frames >> 32) as u32,
            self.version,
            channels as u32,
        ]
    }

    /// The status, and (channels, alive, scopes on), from [`to_words`](Self::to_words)'s
    /// words; `None` if there are too few.
    pub fn from_words(words: &[u32]) -> Option<(SongStatus, usize, bool, bool)> {
        let w: &[u32; SONG_STATUS_WORDS] = words.get(..SONG_STATUS_WORDS)?.try_into().ok()?;
        Some((
            SongStatus {
                playing: w[0] & 1 != 0,
                preview: w[0] & 2 != 0,
                order: w[1] as u16,
                pattern: w[2] as u16,
                row: w[3] as u16,
                tick: w[4] as u8,
                speed: w[5] as u8,
                tempo: w[6] as u16,
                rows: w[7],
                frames: w[8] as u64 | (w[9] as u64) << 32,
                version: w[10],
            },
            (w[11] as usize).min(SONG_MAX_CHANNELS),
            w[0] & 4 != 0,
            w[0] & 8 != 0,
        ))
    }
}

/// Reads a playing module song, from any thread, without waiting on the audio
/// thread. Natively it is the song's own shared state (`Source::song_reader`); on
/// the web [`WebSong`].
pub trait SongReader: Send + Sync {
    /// The transport as last published.
    fn status(&self) -> SongStatus;

    /// The song's channel count.
    fn channels(&self) -> usize;

    /// Each channel's recent peak, then the preview's, into `out` (as many as
    /// fit; `channels() + 1` at most); returns how many. Linear, after the
    /// master volume: the loudest frame of the latest block, falling 24 dB a
    /// second after it, 0 below -60 dB. A muted channel reads 0; an AdLib
    /// channel's is an estimate from its key and volume.
    fn levels(&self, out: &mut [f32]) -> usize;

    /// The latest frames of `channel` (mono, oldest first, at most
    /// [`SONG_SCOPE_FRAMES`]) into `out`; returns how many: 0 while the scopes
    /// are off or the channel does not exist. An AdLib channel's scope is flat
    /// (the chip is not separable).
    fn scope(&self, channel: usize, out: &mut [f32]) -> usize;

    /// Frames written to `channel`'s scope so far: it changes when there is new
    /// data, so a display can skip a redraw.
    fn scope_position(&self, channel: usize) -> u64;

    /// Whether the scopes are on (see `SongControl::Scopes`).
    fn scopes_enabled(&self) -> bool;

    /// False once the song is gone.
    fn alive(&self) -> bool;
}

/// A song as the page sees it on the web: the latest `'songpos'` and
/// `'songscope'` messages the worklet posted for one prepared source, read
/// through [`SongReader`]. Keep one per source slot; feed it with
/// [`update_status`](Self::update_status) and
/// [`update_scopes`](Self::update_scopes). It allocates when a song first shows
/// up and when the channel count grows, not otherwise.
#[derive(Debug, Default, Clone)]
pub struct WebSong {
    channels: usize,
    alive: bool,
    scopes_on: bool,
    status: SongStatus,
    levels: Vec<f32>,
    scopes: Vec<f32>,
    scope_frames: usize,
    written: Vec<u64>,
}

impl WebSong {
    pub fn new() -> WebSong {
        WebSong::default()
    }

    /// Takes one song's entry of a `'songpos'` message: its `status` words
    /// ([`SONG_STATUS_WORDS`] of them, see [`SongStatus::to_words`]) and its
    /// `levels` ([`SONG_LEVEL_SLOTS`] floats: what `SongReader::levels` gave,
    /// the channels' then the preview's; shorter is padded with 0). A malformed
    /// entry is ignored; returns whether it was taken.
    pub fn update_status(&mut self, status: &[u32], levels: &[f32]) -> bool {
        let Some((status, channels, alive, scopes_on)) = SongStatus::from_words(status) else {
            return false;
        };
        self.status = status;
        self.channels = channels;
        self.alive = alive;
        self.scopes_on = scopes_on;
        self.levels.clear();
        self.levels
            .extend(levels.iter().take(SONG_LEVEL_SLOTS).copied());
        self.levels.resize(SONG_LEVEL_SLOTS, 0.0);
        true
    }

    /// Takes a `'songscope'` message: `frames` frames for each of the song's
    /// channels in `data` (channel 0's, then channel 1's, ...), and `written`,
    /// the frames each channel's ring had seen when it was cut. A message that
    /// does not add up is ignored.
    pub fn update_scopes(&mut self, frames: usize, written: &[f64], data: &[f32]) {
        let channels = written.len();
        if frames == 0 || channels == 0 || data.len() < channels * frames {
            return;
        }
        self.scope_frames = frames;
        self.scopes.clear();
        self.scopes.extend_from_slice(&data[..channels * frames]);
        self.written.clear();
        self.written.extend(written.iter().map(|&w| w as u64));
    }
}

impl SongReader for WebSong {
    fn status(&self) -> SongStatus {
        self.status
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn levels(&self, out: &mut [f32]) -> usize {
        // The preview's level follows the last channel's.
        let n = out.len().min(self.channels + 1);
        for (o, &level) in out[..n].iter_mut().zip(&self.levels) {
            *o = level;
        }
        n
    }

    fn scope(&self, channel: usize, out: &mut [f32]) -> usize {
        if channel >= self.written.len() || self.scope_frames == 0 {
            return 0;
        }
        let ring = &self.scopes[channel * self.scope_frames..(channel + 1) * self.scope_frames];
        let have = (self.written[channel].min(self.scope_frames as u64) as usize).min(out.len());
        out[..have].copy_from_slice(&ring[self.scope_frames - have..]);
        have
    }

    fn scope_position(&self, channel: usize) -> u64 {
        self.written.get(channel).copied().unwrap_or(0)
    }

    fn scopes_enabled(&self) -> bool {
        self.scopes_on
    }

    fn alive(&self) -> bool {
        self.alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_round_trips_through_its_words() {
        let s = SongStatus {
            playing: true,
            preview: false,
            order: 12,
            pattern: 3,
            row: 47,
            tick: 5,
            speed: 6,
            tempo: 125,
            rows: 99_999,
            frames: 0x1_2345_6789,
            version: 7,
        };
        let words = s.to_words(8, true, false);
        assert_eq!(words.len(), SONG_STATUS_WORDS);
        assert_eq!(SongStatus::from_words(&words), Some((s, 8, true, false)));
        let words = s.to_words(40, false, true);
        let (_, channels, alive, scopes) = SongStatus::from_words(&words).unwrap();
        assert_eq!((channels, alive, scopes), (32, false, true));
        assert_eq!(SongStatus::from_words(&words[..5]), None);
    }

    #[test]
    fn a_web_song_reads_what_the_worklet_posted() {
        let mut web = WebSong::new();
        let mut out = [9.0f32; 8];
        assert_eq!(web.channels(), 0);
        assert!(!web.alive());
        assert_eq!(web.scope(0, &mut out), 0);
        let status = SongStatus {
            playing: true,
            order: 1,
            row: 20,
            tempo: 150,
            speed: 4,
            ..Default::default()
        };
        let mut levels = vec![0.0f32; SONG_LEVEL_SLOTS];
        levels[0] = 0.5;
        levels[1] = 0.25;
        levels[2] = 0.75;
        assert!(web.update_status(&status.to_words(2, true, true), &levels));
        assert_eq!(web.status(), status);
        assert_eq!(web.channels(), 2);
        assert!(web.alive() && web.scopes_enabled());
        assert_eq!(web.levels(&mut out), 3, "two channels and the preview");
        assert_eq!(out[..3], [0.5, 0.25, 0.75]);
        // A short levels slice is padded; a short status is refused.
        assert!(web.update_status(&status.to_words(2, true, true), &[0.1]));
        assert_eq!(web.levels(&mut out), 3);
        assert_eq!(out[..3], [0.1, 0.0, 0.0]);
        assert!(!web.update_status(&[1, 2, 3], &levels));
        assert_eq!(web.status(), status, "a bad entry changes nothing");

        // Scopes: two channels of 4 frames.
        let data = [1.0, 2.0, 3.0, 4.0, -1.0, -2.0, -3.0, -4.0];
        web.update_scopes(4, &[1000.0, 3.0], &data);
        assert_eq!(web.scope_position(0), 1000);
        assert_eq!(web.scope(0, &mut out), 4);
        assert_eq!(out[..4], [1.0, 2.0, 3.0, 4.0]);
        // Channel 1 had seen only 3 frames: they are the last 3 of its ring.
        assert_eq!(web.scope(1, &mut out), 3);
        assert_eq!(out[..3], [-2.0, -3.0, -4.0]);
        // A shorter read is the newest frames.
        let mut two = [0.0; 2];
        assert_eq!(web.scope(0, &mut two), 2);
        assert_eq!(two, [3.0, 4.0]);
        assert_eq!(web.scope(2, &mut out), 0, "no such channel");
        web.update_scopes(4, &[1.0], &data[..3]);
        assert_eq!(
            web.scope_position(0),
            1000,
            "a message that does not add up"
        );
    }
}
