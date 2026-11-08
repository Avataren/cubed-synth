//! Songs as sources: AHX and HivelyTracker modules, Adlib Tracker II
//! modules, SID songs, and MOD, S3M and XM modules, each played by its
//! cubed-synth player.

use std::sync::Arc;

use cubed_ahx::player::AhxPlayer;
use cubed_opl::a2::player::A2Player;
use cubed_sid::{SidSong, SidSongPlayer};
use cubed_tracker::{Loop, Player, PlayerOptions, Probe};

use crate::song::{SongControl, SongLoop, SongReader, SongStatus};
use crate::source::{Source, SourceEvent};

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
    /// ProTracker modules and their relatives (`.mod`).
    Mod,
    /// Scream Tracker 3 modules (`.s3m`), AdLib instruments included.
    S3m,
    /// FastTracker 2 modules (`.xm`).
    Xm,
}

impl SongFormat {
    /// The format a file extension names, in any case.
    pub fn from_extension(extension: &str) -> Option<SongFormat> {
        match extension.to_ascii_lowercase().as_str() {
            "ahx" | "hvl" => Some(SongFormat::Ahx),
            "a2m" => Some(SongFormat::A2m),
            "asid" => Some(SongFormat::Sid),
            "mod" => Some(SongFormat::Mod),
            "s3m" => Some(SongFormat::S3m),
            "xm" => Some(SongFormat::Xm),
            _ => None,
        }
    }
}

/// How a song plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SongOptions {
    /// The subsong (0 is the main song) for formats that have them. For a
    /// module, the order (position in its song) to start at, for modules
    /// that hold several tunes.
    pub subsong: usize,
    /// Play on round the song's own loop; otherwise the voice ends when the
    /// song comes to its end.
    pub looped: bool,
    /// Modules: stereo width, 0 (mono) to 1 (the pans as written). None
    /// plays the module's own: 0.75 for MODs written for the Amiga's
    /// hard-panned channels, 1 otherwise.
    pub stereo_separation: Option<f32>,
}

impl Default for SongOptions {
    fn default() -> Self {
        Self {
            subsong: 0,
            looped: true,
            stereo_separation: None,
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
        SongFormat::Mod | SongFormat::S3m | SongFormat::Xm => {
            open_module_song(format, bytes, sample_rate, options, false)?
        }
    })
}

/// A MOD, S3M or XM file as a song under live control, ready to play at
/// `sample_rate`. It is a [`Source`] like any other, and the host also
/// [steers](SongControl) it with `SourceEvent::Song` and reads it back through
/// `Source::song_reader`. `stopped` opens it under transport control and stopped:
/// it makes no sound until `SongControl::Play` (a note preview or a seek works
/// meanwhile), and its voice never ends by itself, so an editor keeps one voice for
/// as long as it has the module open; at the end of a song that does not repeat
/// (`SongOptions::looped` false or `SongControl::Looped(false)`) the transport
/// stops at the top instead. With `stopped` false it is `open_song`'s: it plays at
/// once, and ends where the song does (when not looped).
///
/// Build it off the audio thread (it prepares every sample, rings and tables).
/// Errors for the other formats.
pub fn open_module_song(
    format: SongFormat,
    bytes: &[u8],
    sample_rate: f32,
    options: SongOptions,
    stopped: bool,
) -> Result<Box<dyn Source>, String> {
    let format = match format {
        SongFormat::Mod => cubed_tracker::Format::Mod,
        SongFormat::S3m => cubed_tracker::Format::S3m,
        SongFormat::Xm => cubed_tracker::Format::Xm,
        _ => return Err("not a tracker module format (MOD, S3M, XM)".into()),
    };
    let module = cubed_tracker::load_format(format, bytes)?;
    Ok(open_module(module, sample_rate, options, stopped))
}

/// [`open_module_song`] for a module the host already holds (an edited one).
pub fn open_module(
    module: cubed_tracker::Module,
    sample_rate: f32,
    options: SongOptions,
    stopped: bool,
) -> Box<dyn Source> {
    Box::new(ModuleSong::new(module, sample_rate, options, stopped))
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

/// A tracker module, played by cubed-tracker (which allocates nothing once
/// built, so it needs no warm-up), with a probe the host reads and
/// `SongControl`s it can send.
struct ModuleSong {
    player: Player,
    probe: Arc<Probe>,
    /// Under transport control: the voice never ends by itself.
    controlled: bool,
}

impl ModuleSong {
    fn new(
        module: cubed_tracker::Module,
        sample_rate: f32,
        options: SongOptions,
        stopped: bool,
    ) -> ModuleSong {
        let probe = Probe::new(module.channels);
        let mut player = Player::new(
            module,
            sample_rate,
            PlayerOptions {
                looped: options.looped,
                stereo_separation: options.stereo_separation,
                ..Default::default()
            },
        );
        if options.subsong > 0 {
            player.seek(options.subsong, 0);
        }
        if stopped {
            player.stop();
        }
        player.attach_probe(probe.clone());
        ModuleSong {
            player,
            probe,
            controlled: stopped,
        }
    }

    fn control(&mut self, control: SongControl) {
        let player = &mut self.player;
        match control {
            SongControl::Play => player.play(),
            SongControl::Stop => player.stop(),
            SongControl::Seek { order, row } => player.seek(order as usize, row as usize),
            SongControl::Loop(repeat) => player.set_loop(match repeat {
                SongLoop::None => Loop::None,
                SongLoop::Pattern { order } => Loop::Pattern(order as usize),
                SongLoop::Range { order, row0, row1 } => Loop::Range {
                    order: order as usize,
                    row0: row0 as usize,
                    row1: row1 as usize,
                },
            }),
            SongControl::Mute { channel, on } => player.set_mute(channel as usize, on),
            SongControl::Solo { channel, on } => player.set_solo(channel as usize, on),
            SongControl::MuteMask(mask) => player.set_mute_mask(mask),
            SongControl::SoloMask(mask) => player.set_solo_mask(mask),
            SongControl::PreviewOn {
                instrument,
                note,
                velocity,
                duration,
            } => {
                player.preview_on(instrument, note, velocity, duration);
            }
            SongControl::PreviewOff => player.preview_off(),
            SongControl::Looped(on) => player.set_looped(on),
            SongControl::Scopes(on) => self.probe.set_scopes(on),
        }
    }
}

impl Source for ModuleSong {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let more = self.player.render(left, right);
        if !more && self.controlled {
            // The song ran out: stop at the top, and keep the voice.
            self.player.stop();
            self.player.seek(0, 0);
            return true;
        }
        more
    }

    fn event(&mut self, event: SourceEvent) {
        if let SourceEvent::Song(control) = event {
            self.control(control);
        }
    }

    fn song_reader(&self) -> Option<Arc<dyn SongReader>> {
        Some(self.probe.clone())
    }
}

impl From<cubed_tracker::Status> for SongStatus {
    fn from(s: cubed_tracker::Status) -> SongStatus {
        SongStatus {
            playing: s.playing,
            preview: s.preview,
            order: s.order,
            pattern: s.pattern,
            row: s.row,
            tick: s.tick,
            speed: s.speed,
            tempo: s.tempo,
            rows: s.rows,
            frames: s.frames,
            version: s.version,
        }
    }
}

/// A module song's probe is its reader.
impl SongReader for Probe {
    fn status(&self) -> SongStatus {
        Probe::status(self).into()
    }

    fn channels(&self) -> usize {
        Probe::channels(self)
    }

    fn levels(&self, out: &mut [f32]) -> usize {
        Probe::levels(self, out)
    }

    fn scope(&self, channel: usize, out: &mut [f32]) -> usize {
        Probe::scope(self, channel, out)
    }

    fn scope_position(&self, channel: usize) -> u64 {
        Probe::scope_position(self, channel)
    }

    fn scopes_enabled(&self) -> bool {
        Probe::scopes_enabled(self)
    }

    fn alive(&self) -> bool {
        Probe::alive(self)
    }
}
