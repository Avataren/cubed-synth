//! Yamaha OPL3 (YMF262) FM core, with OPL2 music played in its compatibility
//! mode. `.ai/plan-opl.md` is the plan; this is batch O0.
//!
//! One chip for every OPL format: S3M AdLib channels, Adlib Tracker II and
//! whatever follows. Every player only writes registers (`Chip::write`) and
//! renders (`Chip::clock_sample`) at the native ≈ 49 716 Hz.
//!
//! The emulation is a port of ymfm's OPL path by Aaron Giles, chosen under
//! plan decision D1 (permissive sources only). `tables.rs`, `operator.rs` and
//! `chip.rs` derive from ymfm commit 81aec25c (`ymfm_fm.ipp`, `ymfm_fm.h`,
//! `ymfm_opl.cpp`, `ymfm_opl.h`), whose license requires this notice:
//!
//! > BSD 3-Clause License
//! >
//! > Copyright (c) 2021, Aaron Giles
//! > All rights reserved.
//! >
//! > Redistribution and use in source and binary forms, with or without
//! > modification, are permitted provided that the following conditions are met:
//! >
//! > 1. Redistributions of source code must retain the above copyright notice, this
//! >    list of conditions and the following disclaimer.
//! >
//! > 2. Redistributions in binary form must reproduce the above copyright notice,
//! >    this list of conditions and the following disclaimer in the documentation
//! >    and/or other materials provided with the distribution.
//! >
//! > 3. Neither the name of the copyright holder nor the names of its
//! >    contributors may be used to endorse or promote products derived from
//! >    this software without specific prior written permission.
//! >
//! > THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
//! > AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
//! > IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
//! > DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
//! > FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
//! > DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
//! > SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
//! > CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
//! > OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
//! > OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//!
//! Tests replay the register scripts in `golden/` and require every sample to
//! equal ymfm's own render of them (`golden/regen.sh`).

pub mod a2;
pub mod chip;
pub mod operator;
pub mod resample;
pub mod tables;
pub mod wasm;

pub use chip::{Chip, NATIVE_RATE};

#[cfg(test)]
mod tests;
