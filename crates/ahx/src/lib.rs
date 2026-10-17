//! AHX/HVL reference decoder (P0: format decode only, no audio rendering).
//!
//! See `.ai/ahx/verdict.md` and `.ai/ahx/ahx-requirements.md` for the
//! architectural context this module implements against, and
//! `.ai/p0-report.md` for exactly which `[VERIFY]` items this decode logic
//! resolves and how, against the vendored `hvl_replay.c` reference.

#![cfg_attr(feature = "nightly-simd", feature(portable_simd))]

pub mod engine;
pub mod envelope;
pub mod filter_sweep;
pub mod format;
pub mod hifi;
pub mod player;
pub mod plist;
pub mod voice;
pub mod waveform;

/// Stores into the reference's `int16` fields (`hvl_replay.h:91-188`) wrap
/// modulo 2^16 (gcc, two's complement). Rust `i32` arithmetic would not, so
/// every place the C assigns an intermediate `int` into an `int16` member
/// goes through this to stay bit-exact on overflow (period slides that run
/// away, envelope deltas of >127*256, ...).
#[inline]
pub(crate) fn wrap_i16(x: i32) -> i32 {
    x as i16 as i32
}
