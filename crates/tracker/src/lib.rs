//! Tracker modules: ProTracker MOD (and its Soundtracker and multi-channel
//! relatives), Scream Tracker 3 S3M (AdLib instruments included) and
//! FastTracker 2 XM.
//!
//! [`load`] parses a file into a [`Module`], an editable model that keeps
//! the format's own cell and sample bytes. A [`Player`] plays a module into
//! stereo blocks: the order list row by row, each channel's effects tick by
//! tick with the format's quirks, and one sample voice per channel (a new
//! note cuts the last) with windowed-sinc interpolation. Building a player
//! does all the allocating; rendering does none, so it can run on an audio
//! thread. An [`Observer`] sees every row and every command a channel's
//! voice receives, for tests and for a tracker's display.
//!
//! The replay semantics are a port of the another_synth DAW's
//! `tracker-playback` package (TypeScript), checked against it command for
//! command.

#[cfg(feature = "adlib")]
mod adlib;
mod effects;
mod filter;
mod module;
mod pitch;
mod player;
mod profile;
mod protracker;
mod s3m;
mod voice;
mod xm;

pub use effects::{Command, PitchSource, Ramp};
pub use module::{
    Adlib, AutoVibrato, Cell, Envelope, Flags, Format, Instrument, LoopKind, ModFlavor, Module,
    Note, Pattern, Sample,
};
pub use player::{Observer, Player, PlayerOptions, Position};

/// Whether `bytes` look like a module this crate reads, and which kind.
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if xm::looks_like_xm(bytes) {
        Some(Format::Xm)
    } else if s3m::looks_like_s3m(bytes) {
        Some(Format::S3m)
    } else if protracker::looks_like_mod(bytes) {
        Some(Format::Mod)
    } else {
        None
    }
}

/// Parses a module file of any supported format.
pub fn load(bytes: &[u8]) -> Result<Module, String> {
    match sniff(bytes) {
        Some(format) => load_format(format, bytes),
        None => Err("not a MOD, S3M or XM file".into()),
    }
}

/// Parses a file in a known format.
pub fn load_format(format: Format, bytes: &[u8]) -> Result<Module, String> {
    match format {
        Format::Mod => protracker::parse(bytes),
        Format::S3m => s3m::parse(bytes),
        Format::Xm => xm::parse(bytes),
    }
}
