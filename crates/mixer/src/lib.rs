//! A game audio mixer that runs the same code natively and in a browser
//! AudioWorklet.
//!
//! [`Mixer`] is a block renderer. The host [`apply`](Mixer::apply)s
//! [`Command`]s between blocks and calls [`process`](Mixer::process) for each
//! block of output. Sounds play as voices on named buses (the host maps its
//! names to [`BusId`] slots); each bus has a fader, balance, mute and solo,
//! an insert chain ([`effects`] has built-in ones whose parameters change
//! live), sends and ducking under another bus, and feeds another bus, ending
//! at master. A voice plays a [`Clip`] (decoded PCM) or a [`Source`], which
//! renders as it plays: a chip-music song (the `songs` feature) or a stream
//! (`native`). Voices can be placed in 2D or 3D and fade over any length.
//! The mixer reports [`Event`]s and per-bus [`BusMeter`]s back.
//!
//! Everything is allocated in [`Mixer::new`]: applying commands and
//! rendering never allocate or free. Clips, inserts and recordings the mixer
//! lets go of come back through [`Mixer::take_returned`] to be freed by the
//! host off the audio thread.
//!
//! The `native` feature adds [`native::NativeHost`], which runs a mixer in a
//! cpal output callback with lock-free rings to and from the game thread.
//! [`wire`] encodes commands and events as bytes for hosts that cross a
//! message port (the `cubed-worklet` AudioWorklet).

mod clip;
mod command;
pub mod effects;
mod insert;
mod mixer;
pub mod sfx;
mod smooth;
mod source;
mod spatial;
mod synth;
pub mod wire;

#[cfg(feature = "native")]
pub mod native;
#[cfg(feature = "songs")]
pub mod songs;
#[cfg(feature = "native")]
pub mod stream;

pub use clip::Clip;
pub use command::{
    BusId, BusSettings, Capture, ClipId, Command, Duck, Event, Play, PlaySfx, PlaySource, Returned,
    SendSettings, SourceId, Start, VoiceId,
};
pub use effects::{Effect, EffectKind};
pub use insert::{DspInsert, Insert};
pub use mixer::{BusMeter, Mixer, MixerConfig, Stats, MAX_INSERTS, MAX_SENDS};
pub use sfx::{Sfx, SfxPreset, SfxWave};
pub use smooth::MIN_DB;
pub use source::{Source, SourceEvent};
pub use spatial::{Listener, Rolloff, Spatial, SpatialMode};
pub use synth::SynthSource;
