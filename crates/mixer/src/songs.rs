//! Chip-music songs as sources: AHX and HivelyTracker modules, Adlib Tracker
//! II modules and SID songs, each played by its cubed-synth player.

use cubed_ahx::player::AhxPlayer;
use cubed_opl::a2::player::A2Player;
use cubed_sid::{SidSong, SidSongPlayer};

use crate::source::Source;

/// The most frames a player is asked for at once; longer blocks are split,
/// so the players' buffers never grow past what the warm-up sized them for.
const CHUNK: usize = 256;
/// Frames played (and thrown away) before a song starts, so buffers that
/// grow on first use have grown before the audio thread plays it.
const WARM_UP_FRAMES: usize = 4096;
/// AHX's stereo separation for `.ahx` files (HVL files carry their own).
const AHX_STEREO_MODE: u8 = 2;

/// A song file format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SongFormat {
    /// AHX (`.ahx`) and HivelyTracker (`.hvl`) modules.
    Ahx,
    /// Adlib Tracker II modules (`.a2m`), on an emulated OPL3.
    A2m,
    /// cubed-synth's SID songs (`.asid`), on an emulated SID.
    Sid,
}

impl SongFormat {
    /// The format a file extension names, in any case.
    pub fn from_extension(extension: &str) -> Option<SongFormat> {
        match extension.to_ascii_lowercase().as_str() {
            "ahx" | "hvl" => Some(SongFormat::Ahx),
            "a2m" => Some(SongFormat::A2m),
            "asid" => Some(SongFormat::Sid),
            _ => None,
        }
    }
}

/// How a song plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SongOptions {
    /// The subsong (0 is the main song) for formats that have them.
    pub subsong: usize,
    /// Play on round the song's own loop; otherwise the voice ends when the
    /// song comes to its end.
    pub looped: bool,
}

impl Default for SongOptions {
    fn default() -> Self {
        Self {
            subsong: 0,
            looped: true,
        }
    }
}

/// A song ready to play at `sample_rate`. Build it off the audio thread: it
/// parses the file and warms the player up.
pub fn open_song(
    format: SongFormat,
    bytes: &[u8],
    sample_rate: f32,
    options: SongOptions,
) -> Result<Box<dyn Source>, String> {
    Ok(match format {
        SongFormat::Ahx => Box::new(AhxSong::new(bytes, sample_rate, options)?),
        SongFormat::A2m => Box::new(A2mSong::new(bytes, sample_rate, options)?),
        SongFormat::Sid => Box::new(SidSongSource::new(bytes, sample_rate, options)?),
    })
}

/// Calls `render` on chunks of at most [`CHUNK`] frames.
fn chunked(left: &mut [f32], right: &mut [f32], mut render: impl FnMut(&mut [f32], &mut [f32])) {
    let n = left.len().min(right.len());
    let mut at = 0;
    while at < n {
        let k = (n - at).min(CHUNK);
        render(&mut left[at..at + k], &mut right[at..at + k]);
        at += k;
    }
}

fn warm_up(mut render: impl FnMut(&mut [f32], &mut [f32])) {
    let (mut left, mut right) = ([0.0; CHUNK], [0.0; CHUNK]);
    for _ in 0..WARM_UP_FRAMES / CHUNK {
        render(&mut left, &mut right);
    }
}

struct AhxSong {
    player: AhxPlayer,
    looped: bool,
}

impl AhxSong {
    fn new(bytes: &[u8], sample_rate: f32, options: SongOptions) -> Result<AhxSong, String> {
        let mut player = AhxPlayer::new(bytes, sample_rate.round() as u32, AHX_STEREO_MODE)?;
        player.play();
        warm_up(|l, r| {
            player.render(l, r);
        });
        if !player.restart(options.subsong) {
            player.restart(0);
        }
        player.play();
        Ok(AhxSong {
            player,
            looped: options.looped,
        })
    }
}

impl Source for AhxSong {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        chunked(left, right, |l, r| {
            self.player.render(l, r);
        });
        self.looped || !self.player.song_end_reached()
    }
}

struct A2mSong {
    player: A2Player,
    looped: bool,
}

impl A2mSong {
    fn new(bytes: &[u8], sample_rate: f32, options: SongOptions) -> Result<A2mSong, String> {
        let mut player = A2Player::new(bytes, sample_rate as f64)?;
        player.play();
        warm_up(|l, r| {
            player.render(l, r);
        });
        player.seek(0, 0);
        Ok(A2mSong {
            player,
            looped: options.looped,
        })
    }
}

impl Source for A2mSong {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        chunked(left, right, |l, r| {
            self.player.render(l, r);
        });
        self.looped || !self.player.song_end_reached()
    }
}

struct SidSongSource {
    player: SidSongPlayer,
    looped: bool,
}

impl SidSongSource {
    fn new(bytes: &[u8], sample_rate: f32, options: SongOptions) -> Result<SidSongSource, String> {
        let song = SidSong::parse(bytes).map_err(|e| e.to_string())?;
        let model = song.model;
        // Its buffers are sized when it is built: no warm-up needed.
        let player = SidSongPlayer::with_model(song, model, sample_rate as f64, options.subsong)
            .map_err(|e| e.to_string())?;
        Ok(SidSongSource {
            player,
            looped: options.looped,
        })
    }
}

impl Source for SidSongSource {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        chunked(left, right, |l, r| {
            self.player.render(l);
            r.copy_from_slice(l);
        });
        self.looped || !self.player.looped()
    }
}
