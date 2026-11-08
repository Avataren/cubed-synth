//! Playing `.sid` files (PSID and RSID, the C64's music files) by running their
//! 6502 code on an emulated C64 and feeding the SID writes it makes to the SID
//! chip emulation. A port of another_synth's `src/audio/tracker/psid/` runner.
//! See the workspace README, "Playing PSID and RSID tunes".
//!
//! - [`PsidPlayer`]: the player. `new(bytes, sample_rate, options)` (off the
//!   audio thread: the tune's init runs there), then `render` (mono), `render_taps`
//!   or `render_stereo` on the audio thread, which allocate nothing.
//! - [`PsidFile`]: the parsed header and the C64 bytes.
//! - [`PsidRunner`]: the tune and its machine run in step with a clock, handing
//!   over the SID writes with the cycles they were made on (`advance`, `drain`):
//!   what the player uses, and what a host with its own chip would.
//! - [`C64`] and [`Cpu6510`]: the machine and the CPU, for tests and tools.
//!
//! The CPU runs live inside `render` (see `player.rs` for why, and for how a
//! block is played), not offline into a captured register stream.

pub mod c64;
pub mod cpu;
pub mod file;
pub mod player;
pub mod runner;

pub use c64::{C64Clock, Kernal, SidWrite, C64};
pub use cpu::Cpu6510;
pub use file::{looks_like_psid, PsidClock, PsidError, PsidFile, PsidSidModel, PsidType};
pub use player::{PsidOptions, PsidPlayer, PsidSeekError, SEEK_MAX_SECONDS};
pub use runner::{EndReason, PsidRunner, RunMode};

#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests_cpu;
#[cfg(test)]
mod tests_runner;
