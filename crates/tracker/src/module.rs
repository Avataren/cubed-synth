//! The module model: what the parsers produce and the player plays.
//!
//! Cells keep the file's own effect bytes and the samples their own tuning
//! fields, so a module round-trips to its format and an editor can change
//! any of it before handing it to a [`Player`](crate::Player). What the
//! bytes mean is decided at play time, by the format's rules.

/// The tracker whose semantics a module plays with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// ProTracker and its relatives (`.mod`).
    Mod,
    /// Scream Tracker 3 (`.s3m`).
    S3m,
    /// FastTracker 2 (`.xm`).
    Xm,
}

/// Which tracker wrote a MOD, as far as the file tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModFlavor {
    ProTracker,
    NoiseTracker,
    /// The 15-sample layout with no signature.
    Soundtracker,
    /// Karsten Obarski's original: arpeggio on command 1, loop starts in
    /// bytes. Its commands are translated to ProTracker's at load.
    UltimateSoundtracker,
}

/// The note column of a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Note {
    #[default]
    None,
    /// A note, as a MIDI note number (60 is middle C).
    On(u8),
    /// Key off (XM's `===`, S3M's `^^`).
    Off,
}

/// One channel of one row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    pub note: Note,
    /// MOD only: the Amiga period the file stored, which sets the exact
    /// pitch (`note` is its nearest semitone). 0 elsewhere.
    pub period: u16,
    /// Instrument (MOD/S3M: sample) number, 1-based; 0 for none.
    pub instrument: u8,
    /// The volume column: 0 for none, 0x10-0x50 a volume of 0-64, and (XM)
    /// 0x60-0xFF a volume-column command. S3M volumes are stored the XM way.
    pub volume: u8,
    /// The format's own effect command byte and its parameter. MOD and XM
    /// number commands 0x0-0x21 (effect 0 with parameter 0 is no effect);
    /// S3M stores the letter minus 0x40 (1 is A), 0 for none.
    pub effect: u8,
    pub param: u8,
}

impl Cell {
    pub fn is_empty(&self) -> bool {
        *self == Cell::default()
    }
}

/// A block of rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    rows: usize,
    channels: usize,
    cells: Vec<Cell>,
}

impl Pattern {
    /// An empty pattern.
    pub fn new(rows: usize, channels: usize) -> Pattern {
        Pattern {
            rows,
            channels,
            cells: vec![Cell::default(); rows * channels],
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn cell(&self, row: usize, channel: usize) -> &Cell {
        &self.cells[row * self.channels + channel]
    }

    pub fn cell_mut(&mut self, row: usize, channel: usize) -> &mut Cell {
        &mut self.cells[row * self.channels + channel]
    }

    /// The cells of one row, by channel.
    pub fn row(&self, row: usize) -> &[Cell] {
        &self.cells[row * self.channels..(row + 1) * self.channels]
    }
}

/// How a sample repeats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopKind {
    #[default]
    Off,
    Forward,
    PingPong,
}

/// An S3M AdLib instrument: the OPL2 registers ST3 writes for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Adlib {
    /// Modulator and carrier: characteristics, levels, attack/decay,
    /// sustain/release, wave select (the file's bytes 0-9), then the
    /// feedback/connection byte (10).
    pub registers: [u8; 12],
    /// A melodic instrument; the drum kinds ST3 never plays.
    pub melodic: bool,
}

/// A sample: mono PCM and the format's own tuning and volume fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    pub name: String,
    /// Mono frames, -1 to 1.
    pub data: Vec<f32>,
    /// Default volume, 0-64 (S3M AdLib instruments: their raw byte).
    pub volume: u8,
    /// MOD: -8 to 7, in eighths of a semitone.
    pub finetune: i8,
    /// S3M: the rate middle C plays at (8363 is untuned).
    pub c2spd: u32,
    /// XM: semitones the sample is transposed by, and its finetune in
    /// 128ths of a semitone (in `finetune`, -128 to 127 as i8).
    pub relative_note: i8,
    /// XM: the sample's own pan, 0-255 (128 centred).
    pub pan: Option<u8>,
    /// S3M: an AdLib instrument, played on an OPL2 rather than as PCM.
    pub adlib: Option<Adlib>,
    pub loop_kind: LoopKind,
    /// In frames.
    pub loop_start: usize,
    pub loop_len: usize,
}

/// An XM envelope: points (tick, value 0-64), with an optional sustain point
/// and loop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Envelope {
    pub points: Vec<(u16, u16)>,
    /// Holds here while the key is down.
    pub sustain: Option<usize>,
    /// Point indices the envelope loops between.
    pub looped: Option<(usize, usize)>,
}

/// An XM instrument's automatic vibrato.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AutoVibrato {
    /// 0 sine, 1 square, 2 ramp up, 3 ramp down.
    pub kind: u8,
    /// Ticks the depth takes to grow in.
    pub sweep: u8,
    pub depth: u8,
    /// Steps of a 256-step cycle per tick.
    pub rate: u8,
}

/// An XM instrument: a keymap over its samples, envelopes, fadeout,
/// auto-vibrato.
#[derive(Clone, Debug, PartialEq)]
pub struct Instrument {
    pub name: String,
    /// For each of the 96 notes, which of `samples` it plays.
    pub keymap: [u8; 96],
    /// Indices into the module's samples.
    pub samples: Vec<usize>,
    pub volume_envelope: Option<Envelope>,
    pub panning_envelope: Option<Envelope>,
    /// Per tick after key-off, out of 32768.
    pub fadeout: u16,
    pub vibrato: AutoVibrato,
}

impl Default for Instrument {
    fn default() -> Self {
        Instrument {
            name: String::new(),
            keymap: [0; 96],
            samples: Vec::new(),
            volume_envelope: None,
            panning_envelope: None,
            fadeout: 0,
            vibrato: AutoVibrato::default(),
        }
    }
}

/// Per-file playback switches the formats carry in their headers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// Periods stay inside ProTracker's three octaves (MOD: the file has the
    /// shape of a ProTracker module) or ST3's Amiga range (S3M header flag).
    pub amiga_limits: bool,
    /// MOD: the song runs off the 50 Hz vertical blank, so Fxx with a
    /// parameter of 0x20 or more sets the speed, not the tempo.
    pub vblank_timing: bool,
    /// S3M: volume slides step on tick 0 too (ST3.00 files, header flag 0x40).
    pub fast_volume_slides: bool,
    /// XM: the linear frequency table (otherwise the Amiga one).
    pub linear_frequency: bool,
}

/// A whole module.
#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub format: Format,
    pub title: String,
    /// Channel count; every pattern has this many.
    pub channels: usize,
    /// The song: pattern numbers in play order.
    pub orders: Vec<usize>,
    pub patterns: Vec<Pattern>,
    /// MOD and S3M: instrument `n` plays `samples[n - 1]`. XM: every
    /// instrument's samples, which `instruments` index.
    pub samples: Vec<Sample>,
    /// XM only: instrument `n` is `instruments[n - 1]`.
    pub instruments: Vec<Instrument>,
    /// Ticks per row at the start.
    pub speed: u8,
    /// Beats per minute at the start (a tick is 2.5 / tempo seconds).
    pub tempo: u8,
    /// Each channel's position before any pan command, 0 (left) to 1; None
    /// for a channel the format leaves centred without saying so (an S3M
    /// in mono, an AdLib channel).
    pub channel_pan: Vec<Option<f32>>,
    /// S3M: the OPL2 channel (0-8) each AdLib channel plays on.
    pub adlib_channels: Vec<Option<u8>>,
    /// Channels the file switches off (S3M's channel settings): they run
    /// their effects but are not heard.
    pub muted: Vec<bool>,
    /// S3M: where a position jump to each of the file's order positions
    /// lands in `orders` (the file's list has separators `orders` leaves
    /// out). None jumps by `orders` itself.
    pub jump_targets: Option<Vec<usize>>,
    /// The global volume the song starts at, 0-1.
    pub global_volume: f32,
    /// S3M: the AdLib chip's level against the samples (from the header's
    /// master volume, as OpenMPT mixes it).
    pub adlib_gain: f32,
    /// How wide the pans play by default: 1 as written, 0 mono. MODs written
    /// for the Amiga, whose channels are hard left and right, default to
    /// 0.75; a [`PlayerOptions`](crate::PlayerOptions) can choose another.
    pub stereo_separation: f32,
    pub flags: Flags,
    /// MOD only.
    pub mod_flavor: Option<ModFlavor>,
    /// S3M: the tracker that wrote the file (`Cwt/v`, 0x1320 is ST3.20).
    pub tracker_version: u16,
}
