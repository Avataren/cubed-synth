//! The part of `std::simd` the cubed-synth crates use, on stable Rust.
//!
//! The types and traits have std's names and paths (`Simd`, `f32x4`,
//! `num::SimdFloat`, `cmp::SimdOrd`, `StdFloat`, `Mask`), so code written
//! against `std::simd` builds against this crate unchanged. With the `nightly`
//! feature this crate re-exports `std::simd` itself instead.
//!
//! `Simd<T, N>` here is an array with lane-wise operations that the compiler
//! vectorizes (SSE on x86-64, simd128 on wasm32 with `+simd128`). The results
//! are std's, lane for lane: integer arithmetic and shifts wrap, float
//! reductions add in lane order from -0.0, `simd_max`/`simd_min` are
//! `f32::max`/`f32::min`, and `mul_add` is fused.

#![cfg_attr(feature = "nightly", feature(portable_simd))]

#[cfg(feature = "nightly")]
pub use std::simd::*;

#[cfg(not(feature = "nightly"))]
mod fallback;
#[cfg(not(feature = "nightly"))]
pub use fallback::*;
