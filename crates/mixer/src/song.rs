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
/// Voices whose own positions a status carries at most ([`SongStatus::voices`]): what the widest
/// format that has them needs (a SID song has three, each with its own orderlist).
pub const SONG_MAX_VOICES: usize = 3;
/// `u32`s per voice in a song's voice words (see [`SongStatus::voice_words`]).
pub const SONG_VOICE_WORD_STRIDE: usize = 2;
/// `u32`s in a song's voice words, as the worklet posts them next to the status words.
pub const SONG_VOICE_WORDS: usize = SONG_MAX_VOICES * SONG_VOICE_WORD_STRIDE;

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
    /// Moves one voice to `order` and `row` of its own order list, from a clean state (a SID
    /// song, whose three voices each have an order list of their own: voice `voice` is about to
    /// read row `row` of the pattern at its position `order`, and the other voices stand where the
    /// song has them at that moment). `SeekVoice { voice: 0, .. }` is `Seek`. Works while stopped.
    /// Wire kind 18. Formats whose channels share one position ignore it.
    SeekVoice {
        voice: u8,
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
    /// Puts a cell in the module's pattern, for the song to play the next time it
    /// reaches it: a live edit of what is written, that allocates nothing. Wire
    /// tag 31, 21 bytes. Anything that changes the module's shape (a pattern's
    /// rows, the order list, a sample) is a new song: open the edited module,
    /// seek it to where the old one is and swap them (`swap_commands`).
    SetCell(SongCell),
    /// Puts an Adlib instrument's data in the song, for the notes that load it from now on
    /// (and the preview, if it plays this instrument): a live edit that allocates nothing.
    /// Wire tag 32, 24 bytes. Other formats ignore it.
    SetInstrument(SongInstrument),
    /// Sets an Adlib song's tempo and speed now (a module changes them with its first row
    /// effect, so its tempo is a new song). Other formats ignore it.
    Timing {
        tempo: u8,
        speed: u8,
    },
    /// Sets one entry of the order list now: channel `channel` of position `order` plays
    /// track `pattern` at `transpose` semitones (AHX and HVL; a module's order list entry
    /// is a new song). Wire kind 15. Other formats ignore it.
    SetOrder {
        order: u16,
        channel: u8,
        pattern: u8,
        transpose: i8,
    },
    /// Sets where the song starts over when it ends (AHX and HVL). Wire kind 16.
    SetRestart(u16),
    /// Puts a row in one of a SID song's step tables (`table` 0 wave, 1 pulse, 2 filter, 3 speed;
    /// the table keeps its length): heard where the table next reads it. Wire kind 17. Other
    /// formats ignore it.
    SetTableRow {
        table: u8,
        row: u8,
        left: u8,
        right: u8,
    },
}

/// An instrument as `SongControl::SetInstrument` carries it: what the song loads into a channel
/// at a note. Adlib: `fm` is the eleven FM bytes and the rest is the instrument's panning,
/// fine-tune and voice type (`cubed_opl::a2::live::LiveEdit::Instrument`). SID: `fm[..10]` is
/// attack/decay, sustain/release, first wave, gate timer, flags (bit 0 hard restart, bit 1 no gate
/// off), vibrato delay and the wave, pulse, filter and speed pointers; the rest is not used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SongInstrument {
    /// The instrument's number, from 1 (as a cell names it).
    pub number: u8,
    /// The eleven FM bytes, as an A2M file holds them.
    pub fm: [u8; 11],
    /// 0 centre, 1 left, 2 right.
    pub panning: u8,
    pub finetune: i8,
    /// 0 melodic, 1-5 bass drum, snare, tom, cymbal, hi-hat.
    pub voice_type: u8,
}

/// A pattern cell as `SongControl::SetCell` carries it: the same numbers a
/// module file and the editor's cells hold (see `cubed_tracker::Cell`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SongCell {
    /// The pattern's number (not its position in the order list).
    pub pattern: u16,
    pub row: u16,
    pub channel: u8,
    /// 0 none, 255 key off, otherwise a MIDI note number (60 is middle C).
    pub note: u8,
    /// MOD: the Amiga period of the note. Adlib: the second effect and its parameter,
    /// `effect2 << 8 | param2` (a cell has two effect columns; `note` is the file's note
    /// byte and `volume` is not used). AHX and HVL: the second effect column the same way
    /// (`fxb << 8 | fxb_param`; `pattern` is the *track* the step is in, `channel` is not
    /// used, `note` is the file's 0-63, `volume` is not used). 0 elsewhere.
    pub period: u16,
    /// The instrument (MOD, S3M: sample) number, 1-based; 0 for none.
    pub instrument: u8,
    /// The volume column as the file holds it, 0 for none.
    pub volume: u8,
    /// The format's own effect byte and its parameter.
    pub effect: u8,
    pub param: u8,
}

/// Where one voice of a song is, for a format whose voices each have a position of their own (a SID
/// song: every voice has its own order list, so its own pattern and row at any time).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VoicePos {
    /// Position in the voice's order list.
    pub order: u16,
    /// The pattern at that position.
    pub pattern: u16,
    /// The row playing, or the row a stopped song will start at.
    pub row: u16,
    /// Whether the song reports this voice (false for every voice of a format that has no
    /// positions of its own, and for the slots past the song's last voice).
    pub valid: bool,
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
    /// Each voice's own position, for a format that has them (SID: `order`, `pattern` and `row`
    /// above are voice 0's, which `voices[0]` repeats); all `valid == false` otherwise.
    pub voices: [VoicePos; SONG_MAX_VOICES],
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

    /// The voices' positions as the worklet posts them, next to [`to_words`](Self::to_words)'s
    /// words (a separate array in the `'songpos'` message, so a page that does not know it reads
    /// the message as before): per voice two words, `pattern << 16 | order` and `valid << 16 | row`.
    pub fn voice_words(&self) -> [u32; SONG_VOICE_WORDS] {
        let mut words = [0u32; SONG_VOICE_WORDS];
        for (k, v) in self.voices.iter().enumerate() {
            words[k * SONG_VOICE_WORD_STRIDE] = (v.pattern as u32) << 16 | v.order as u32;
            words[k * SONG_VOICE_WORD_STRIDE + 1] = (v.valid as u32) << 16 | v.row as u32;
        }
        words
    }

    /// Takes the voices' positions from [`voice_words`](Self::voice_words)'s words (as many voices
    /// as `words` holds; the rest are not reported). Returns whether `words` was long enough for
    /// one voice.
    pub fn set_voice_words(&mut self, words: &[u32]) -> bool {
        self.voices = [VoicePos::default(); SONG_MAX_VOICES];
        for (k, v) in self.voices.iter_mut().enumerate() {
            let at = k * SONG_VOICE_WORD_STRIDE;
            let Some(&[first, second]) = words.get(at..at + SONG_VOICE_WORD_STRIDE) else {
                break;
            };
            *v = VoicePos {
                order: first as u16,
                pattern: (first >> 16) as u16,
                row: second as u16,
                valid: second >> 16 & 1 != 0,
            };
        }
        words.len() >= SONG_VOICE_WORD_STRIDE
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
                voices: [VoicePos::default(); SONG_MAX_VOICES],
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
        self.update_status_voices(status, levels, &[])
    }

    /// [`update_status`](Self::update_status) with the song's voice words (what
    /// [`SongStatus::voice_words`] made, `'songpos'`'s `voices`; empty from a worklet that does not
    /// post them: no voice is then reported).
    pub fn update_status_voices(&mut self, status: &[u32], levels: &[f32], voices: &[u32]) -> bool {
        let Some((mut status, channels, alive, scopes_on)) = SongStatus::from_words(status) else {
            return false;
        };
        status.set_voice_words(voices);
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

/// The songs the page follows, by source slot: what a `'songpos'` and a
/// `'songscope'` message carry, parsed into one [`WebSong`] each (the web
/// counterpart of the native readers, as `WebAnalysis` is of `NativeHost`). Feed
/// it each message with [`update_positions`](Self::update_positions) and
/// [`update_scopes`](Self::update_scopes); read a song with [`get`](Self::get).
#[derive(Debug, Default)]
pub struct WebSongs {
    songs: Vec<Option<WebSong>>,
}

impl WebSongs {
    pub fn new() -> WebSongs {
        WebSongs::default()
    }

    /// Takes a `'songpos'` message: the source slots it reports, their status
    /// words ([`SONG_STATUS_WORDS`] each) and their levels
    /// ([`SONG_LEVEL_SLOTS`] each). A song that is not in the message keeps what it
    /// had (it has not moved); one reported gone reads `alive() == false` until
    /// [`forget`](Self::forget). Returns how many entries were taken.
    pub fn update_positions(&mut self, sources: &[u32], status: &[u32], levels: &[f32]) -> usize {
        self.update_positions_voices(sources, status, levels, &[])
    }

    /// [`update_positions`](Self::update_positions) with the message's `voices`: for each song in
    /// `sources` its voice words ([`SONG_VOICE_WORDS`] of them, `voices.len() / sources.len()` each
    /// as the worklet posted them: a longer entry is read as far as this build knows voices, a
    /// shorter one reports fewer). Empty `voices` (a worklet that posts none) reports no voice.
    pub fn update_positions_voices(
        &mut self,
        sources: &[u32],
        status: &[u32],
        levels: &[f32],
        voices: &[u32],
    ) -> usize {
        let stride = voices.len().checked_div(sources.len()).unwrap_or(0);
        let mut taken = 0;
        for (i, &source) in sources.iter().enumerate() {
            let (Some(words), Some(slots)) = (
                status.get(i * SONG_STATUS_WORDS..(i + 1) * SONG_STATUS_WORDS),
                levels.get(i * SONG_LEVEL_SLOTS..(i + 1) * SONG_LEVEL_SLOTS),
            ) else {
                break;
            };
            let source = source as usize;
            if source >= 4096 {
                continue;
            }
            if self.songs.len() <= source {
                self.songs.resize_with(source + 1, || None);
            }
            let song = self.songs[source].get_or_insert_with(WebSong::new);
            let own = voices.get(i * stride..(i + 1) * stride).unwrap_or(&[]);
            taken += song.update_status_voices(words, slots, own) as usize;
        }
        taken
    }

    /// Takes a `'songscope'` message: `frames` per channel, and for each song in
    /// `sources` its channel count (`channels`), then `written` (frames seen, per
    /// channel of each song in order) and `data` (each channel's frames, in the
    /// same order). A message that does not add up is ignored.
    pub fn update_scopes(
        &mut self,
        frames: usize,
        sources: &[u32],
        channels: &[u32],
        written: &[f64],
        data: &[f32],
    ) {
        let total: usize = channels.iter().map(|&c| c as usize).sum();
        if frames == 0
            || sources.len() != channels.len()
            || written.len() < total
            || data.len() < total * frames
        {
            return;
        }
        let (mut channel_at, mut data_at) = (0, 0);
        for (&source, &count) in sources.iter().zip(channels) {
            let count = count as usize;
            if let Some(Some(song)) = self.songs.get_mut(source as usize) {
                song.update_scopes(
                    frames,
                    &written[channel_at..channel_at + count],
                    &data[data_at..data_at + count * frames],
                );
            }
            channel_at += count;
            data_at += count * frames;
        }
    }

    /// The song in source slot `source`, if the worklet has reported one.
    pub fn get(&self, source: u32) -> Option<&WebSong> {
        self.songs.get(source as usize)?.as_ref()
    }

    /// Drops what it holds for `source` (the song was reported gone, or the page
    /// no longer cares).
    pub fn forget(&mut self, source: u32) {
        if let Some(slot) = self.songs.get_mut(source as usize) {
            *slot = None;
        }
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
            voices: Default::default(),
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
    fn the_voices_positions_round_trip_through_their_words_and_reach_the_page() {
        let mut s = SongStatus {
            playing: true,
            ..Default::default()
        };
        s.voices[0] = VoicePos {
            order: 3,
            pattern: 40,
            row: 17,
            valid: true,
        };
        s.voices[2] = VoicePos {
            order: 65_535,
            pattern: 65_000,
            row: 65_535,
            valid: true,
        };
        let words = s.voice_words();
        assert_eq!(words.len(), SONG_VOICE_WORDS);
        let mut back = SongStatus::default();
        assert!(back.set_voice_words(&words));
        assert_eq!(back.voices, s.voices);
        assert!(!back.voices[1].valid);
        // The status words do not carry them (a page that predates them reads the message as before).
        let (plain, ..) = SongStatus::from_words(&s.to_words(3, true, false)).unwrap();
        assert_eq!(plain.voices, [VoicePos::default(); SONG_MAX_VOICES]);
        // Fewer words report fewer voices; none report none.
        assert!(back.set_voice_words(&words[..SONG_VOICE_WORD_STRIDE]));
        assert!(back.voices[0].valid && !back.voices[2].valid);
        assert!(!back.set_voice_words(&[]));
        assert_eq!(back.voices, [VoicePos::default(); SONG_MAX_VOICES]);

        // On the page: a message with voices for two songs, then one without (an older worklet).
        let mut web = WebSongs::new();
        let (a, b) = (s, SongStatus::default());
        let status = [a.to_words(3, true, false), b.to_words(4, true, false)].concat();
        let levels = vec![0.0f32; 2 * SONG_LEVEL_SLOTS];
        let voices = [a.voice_words(), b.voice_words()].concat();
        assert_eq!(
            web.update_positions_voices(&[5, 6], &status, &levels, &voices),
            2
        );
        assert_eq!(web.get(5).unwrap().status().voices, a.voices);
        assert_eq!(web.get(6).unwrap().status().voices, b.voices);
        // A longer entry (a build with more voices) is read as far as this one knows voices.
        let mut wider = Vec::new();
        for song in [&a, &b] {
            wider.extend(song.voice_words());
            wider.extend([7u32; 4]);
        }
        web.update_positions_voices(&[5, 6], &status, &levels, &wider);
        assert_eq!(web.get(5).unwrap().status().voices, a.voices);
        // None at all: no voice is reported, the rest of the status is.
        assert_eq!(web.update_positions(&[5], &status, &levels), 1);
        let song = web.get(5).unwrap();
        assert!(song.status().voices.iter().all(|v| !v.valid));
        assert!(song.status().playing);
    }

    #[test]
    fn web_songs_split_the_messages_by_source_slot() {
        let mut web = WebSongs::new();
        assert!(web.get(3).is_none());
        let a = SongStatus {
            playing: true,
            row: 5,
            ..Default::default()
        };
        let b = SongStatus {
            order: 2,
            ..Default::default()
        };
        let status = [a.to_words(2, true, true), b.to_words(4, true, false)].concat();
        let mut levels = vec![0.0f32; 2 * SONG_LEVEL_SLOTS];
        levels[0] = 0.5;
        levels[SONG_LEVEL_SLOTS] = 0.25;
        assert_eq!(web.update_positions(&[3, 1], &status, &levels), 2);
        assert_eq!(web.get(3).unwrap().status(), a);
        assert_eq!(web.get(1).unwrap().status(), b);
        let mut out = [0.0; 5];
        web.get(1).unwrap().levels(&mut out);
        assert_eq!(out[0], 0.25);
        // A short message takes what adds up.
        assert_eq!(
            web.update_positions(&[3, 1], &status[..SONG_STATUS_WORDS], &levels),
            1
        );
        // Scopes: song 3 has 2 channels of 3 frames, song 1 none yet.
        let data = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        web.update_scopes(3, &[3], &[2], &[10.0, 20.0], &data);
        let mut ring = [0.0; 3];
        assert_eq!(web.get(3).unwrap().scope(1, &mut ring), 3);
        assert_eq!(ring, [4.0, 5.0, 6.0]);
        assert_eq!(web.get(3).unwrap().scope_position(1), 20);
        assert_eq!(web.get(1).unwrap().scope(0, &mut ring), 0);
        // One that does not add up is ignored.
        web.update_scopes(3, &[3], &[2], &[1.0, 2.0], &data[..4]);
        assert_eq!(web.get(3).unwrap().scope_position(1), 20);
        // Gone, then forgotten.
        let gone = SongStatus::default().to_words(2, false, false);
        web.update_positions(&[3], &gone, &levels);
        assert!(!web.get(3).unwrap().alive());
        web.forget(3);
        assert!(web.get(3).is_none());
        // A slot beyond any mixer's is refused.
        assert_eq!(web.update_positions(&[9999], &gone, &levels), 0);
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
