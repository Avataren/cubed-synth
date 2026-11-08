//! Playing `.sid` files (PSID and RSID, the C64's music files) by running their
//! 6502 code on an emulated C64 and feeding the SID writes it makes to the SID
//! chip emulation. A port of another_synth's `src/audio/tracker/psid/` runner.
//! See the workspace README, "Playing PSID and RSID tunes".

pub mod c64;
pub mod cpu;
pub mod file;
pub mod player;
pub mod runner;

pub use c64::{C64Clock, SidWrite, C64};
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
