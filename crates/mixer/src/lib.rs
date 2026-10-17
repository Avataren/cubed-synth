//! A game audio mixer that runs the same code natively and in a browser
//! AudioWorklet.
//!
//! [`Mixer`] is a block renderer. The host [`apply`](Mixer::apply)s
//! [`Command`]s between blocks and calls [`process`](Mixer::process) for each
//! block of output. Sounds play as voices on named buses (the host maps its
//! names to [`BusId`] slots); each bus has a fader, balance, mute and solo,
//! an insert chain and sends, and feeds another bus, ending at master. The
//! mixer reports [`Event`]s and per-bus [`BusMeter`]s back.
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
mod insert;
mod mixer;
mod smooth;
mod spatial;
pub mod wire;

#[cfg(feature = "native")]
pub mod native;

pub use clip::Clip;
pub use command::{
    BusId, BusSettings, Capture, ClipId, Command, Event, Play, Returned, SendSettings, Start,
    VoiceId,
};
pub use insert::{DspInsert, Insert};
pub use mixer::{BusMeter, Mixer, MixerConfig, Stats, MAX_INSERTS, MAX_SENDS};
pub use smooth::MIN_DB;
pub use spatial::{Listener, Rolloff, Spatial, SpatialMode};
