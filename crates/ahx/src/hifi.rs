//! Band-limited ("hi-fi") oscillators for the AHX/HVL mixer, built the way the
//! rest of this crate builds mipmapped wavetables (`nodes/wavetable.rs`):
//! construct the spectrum, zero every bin above a level's Nyquist, inverse-FFT.
//!
//! ## What the reference does, and why it aliases
//!
//! A voice plays a cyclic table of `N = 4 << wave_length` signed bytes (4..128)
//! straight out of `voice_buffer`, nearest-neighbour at a 16.16 step: no
//! interpolation, no band limit. Read at a step of `f0` cycles per output
//! sample, the staircase's harmonics -- and the images of the staircase itself
//! -- above Nyquist fold back below it. On a high note that lands as
//! inharmonic garbage, including energy far below the pitch.
//!
//! ## What hi-fi does
//!
//! The waveform the reference *means* is the continuous staircase
//! `s(t) = x[floor(t * N)]`. Its Fourier series is exact and closed-form:
//!
//! ```text
//! c_0 = X_0 / N
//! c_k = X_(k mod N) * e^(-i*pi*k/N) * sin(pi*k/N) / (pi*k)        (k >= 1)
//! ```
//!
//! with `X` the N-point DFT of the table (the `sin` term is the zero-order
//! hold's sinc, so the staircase images are kept -- below Nyquist they are
//! part of the sound). A mip level keeps harmonics `1..=H`, inverse-FFTs them
//! into a [`table_size_for`]`(H)`-point cycle and is used only while
//! `H * f0 <= Nyquist`, so nothing above Nyquist exists to fold. That is the
//! whole idea: the reference sound, minus exactly the part that cannot be
//! represented.
//!
//! * **Table size** follows the level: 64 points per period of the top partial
//!   (`next_pow2(64 * H)`), clamped to `64..=`[`TABLE_SIZE`], and never below
//!   the cycle's own `N` (so `size / N` stays a whole number). The table is
//!   read with linear interpolation, which images the top partial around
//!   multiples of the table's point rate: with only 8 points per period those
//!   images fold back 50-60 dB down on a thin level, with 64 they sit at or
//!   under what a fixed 4096-point table gave. Levels with 45 or more partials
//!   keep 4096 points (level 0 still has 8 per period, buried under 511 other
//!   partials, as it always was); only the thinner ones shrink.
//!
//! * **Levels** are a geometric ladder of harmonic counts (`LEVELS_PER_OCTAVE`
//!   per octave, from [`MAX_HARMONICS`] down to 1) and are built lazily, the
//!   first time a voice needs one.
//! * **Sources** are keyed by the table's *contents*, not by a parameter pair.
//!   That covers every way AHX makes a table without enumerating them: the
//!   per-frame square duty (`CalcSquare`'s 32 widths, subsampled per wave
//!   length), the filter-sweep rows (a lowpass/highpass row is just a
//!   different table), triangle and sawtooth at each wave length. Equal
//!   contents share one spectrum and one set of levels.
//! * **DC is kept** (the reference's squares carry a duty-dependent DC and its
//!   sawtooth a half-LSB one); `wavetable.rs` zeroes DC because its sources are
//!   symmetric. The Nyquist bin is never populated: `H <= size / 8` at every
//!   level (level 0's 512 partials in 4096 points is the tightest).
//!
//! ## The AHX filter
//!
//! AHX's "filter" is not a stage on the oscillator output: it is baked into
//! the wave tables (`waveform::gen_filter_waves`, 31 low- and 31 high-pass
//! rows selected by `filter.pos`). A voice on a filtered row simply plays a
//! different `N`-byte table, and hi-fi band-limits *that* table. The filter
//! response is therefore untouched (resonant peaks and all, up to Nyquist);
//! only what could not have been represented is removed. Nothing is filtered
//! twice and nothing here depends on the filter's internals.
//!
//! ## What stays reference-authentic
//!
//! * **Noise** (`WAVEFORM_NOISE`): a 0x280-byte pseudo-random buffer, not a
//!   periodic table, so there is no harmonic series to truncate. Its aliasing
//!   is a different problem and a different fix (a noise source has no
//!   fundamental to fold). It plays the reference path.
//! * **Ring modulation**: the modulator stays the reference's byte table. The
//!   product of two waves has sum and difference partials that neither
//!   factor's band limit bounds, so band-limiting the factors would not
//!   deliver what it promises.
//!
//! Hi-fi is off in the engine by default and, off, is not merely equivalent
//! but *the same code path*: the mixer's `HIFI` const-generic is `false`, and
//! nothing here runs. The render goldens hold byte for byte. (The *app* always
//! turns it on, unconditionally, through `AhxPlayer::set_hifi` (the worklet's
//! `set-hifi`, sent as soon as its client exists); the engine's own default,
//! and the golden harness that drives it directly, are unaffected.)
//!
//! ## Prewarm: the render path never builds a table
//!
//! Building a level is an inverse FFT plus a quantise (~0.1 ms), and a tick
//! on many voices can want a dozen cold ones at once -- against a 2.7 ms
//! render quantum. So the product path never builds inside `render`: the
//! bank has three [`BankMode`]s. `Lazy` (the engine default, what the tests
//! use) builds on demand. `Prewarm` builds on demand too, but never evicts and
//! stops at the cap. `Locked` only *looks up*: a miss is counted, and the voice
//! takes the nearest built level with fewer partials of the same table (duller,
//! never aliased), or failing that the reference path for that tick. The
//! decision is a plain lookup: no lock is taken and nothing blocks.
//!
//! Who builds, and when, depends on the engine:
//!
//! * **A song** is prewarmed as a whole: `AhxEngine::prewarm_hifi` runs the
//!   song's ticks (no mixing) in `Prewarm` mode, then locks; see there for
//!   what that covers.
//! * **The keyboard preview** plays whatever key is pressed, so there is no
//!   song to walk. Its bank is `Locked` from the start, and each note-on
//!   (`AhxEngine::live_note_on`, called from the worklet's message handler,
//!   between render quanta) prewarms the pressed instrument at the pressed
//!   pitch by running its ticks -- sweeps and all -- on a scratch voice, then
//!   locks again. A table a held note reaches that the scratch run did not (a
//!   sweep longer than the simulated hold) is a miss, degraded for that tick
//!   and counted, not built.
//!
//! Neither builds on the render path, but a build is still CPU on the thread
//! that runs the audio worklet: the preview's prewarm is a burst in the
//! message handler, between quanta. It is paid once per (instrument, note) and
//! is mostly cache hits after the first note of a level.
//!
//! Zero new dependencies: `rustfft` is what `wavetable.rs` already uses.

use cubed_simd::i32x4;
use rustc_hash::FxHashMap;
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::sync::{Arc, OnceLock};

/// Most samples per band-limited cycle (a power of two): the size of every
/// level with 64 or more partials. At the top level (`MAX_HARMONICS` = 512)
/// that is 8 points per period of the highest partial, where linear
/// interpolation costs under half a dB of level; see [`table_size_for`] for the
/// rest of the ladder.
pub const TABLE_SIZE: usize = 4096;

/// Fewest samples per band-limited cycle (the 1-partial level).
pub const MIN_TABLE_SIZE: usize = 64;

/// Table points per period of a level's top partial, before the clamp to
/// `MIN_TABLE_SIZE..=TABLE_SIZE`. Linear interpolation images the top partial
/// around multiples of the point rate; 8 points per period left them 50-60 dB
/// down (measured, `.ai/checks-bandlimit2.txt`), 64 keeps every level at or
/// under what the fixed 4096-point tables gave.
pub const POINTS_PER_TOP_PARTIAL: usize = 64;

/// Most partials a level can hold; `TABLE_SIZE / 8`. Only reached for
/// fundamentals below `Nyquist / 512` (~47 Hz at 48 kHz), where the reference's
/// own tables have long run out of content that matters.
pub const MAX_HARMONICS: usize = TABLE_SIZE / 8;

/// Mip levels per octave of pitch. 2 keeps the audible brightness step when a
/// slide or vibrato crosses a level under ~3 dB at the top of the band, while
/// a song only ever touches two or three levels per source.
pub const LEVELS_PER_OCTAVE: usize = 2;

/// `log2(MAX_HARMONICS) * LEVELS_PER_OCTAVE`: from 512 partials down to 1 (the
/// last rung rounds `512 * 2^(-17/2)` = 1.4 to 1, so 1 is not repeated).
pub const LEVEL_COUNT: usize = 9 * LEVELS_PER_OCTAVE;

/// Fixed-point fraction bits of a table sample: entries are the waveform
/// (`i8` scale) times 128, so the table's own rounding sits 42 dB under the
/// reference's `i8` quantisation instead of at it. The Gibbs overshoot peaks
/// at ~1.19 x `i8` full scale, ~19 300 here: inside `i16` with room to spare.
pub const FRAC_BITS: u32 = 7;

/// Tables kept before the cache is dropped and rebuilt on demand. Tables are
/// pure functions of their key, so eviction can only cost time, never change
/// a sample.
///
/// The ceiling is bigger than the tables alone: a table is at most 8 KiB, so
/// 4096 of them are at most 32 MiB, and every cached table hangs off a [`Source`] that is
/// not counted here -- 513 `Complex<f32>` (4104 B), its `<= 128 B` key and an
/// 18-slot level array (288 B), ~4.5 KiB each. A source holds at least one
/// table, so there are at most 4096 of them: up to ~18 MiB more, ~50 MiB in
/// all. A real song sits far below (tens of sources, a few hundred tables).
const MAX_CACHED_TABLES: usize = 4096;

/// Partials kept by level `j`: `MAX_HARMONICS * 2^(-j / LEVELS_PER_OCTAVE)`,
/// rounded, never below 1.
pub fn level_harmonics(level: usize) -> usize {
    // `exp2` per level was the cost of every `level_for` call (up to 18 of
    // them, per voice per tick of a prewarm walk): computed once, with the
    // same expression, so every level is the number it always was.
    static TABLE: OnceLock<[usize; LEVEL_COUNT]> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [1usize; LEVEL_COUNT];
        for (level, slot) in t.iter_mut().enumerate() {
            let h = MAX_HARMONICS as f64 * (-(level as f64) / LEVELS_PER_OCTAVE as f64).exp2();
            *slot = (h.round() as usize).max(1);
        }
        t
    });
    match table.get(level) {
        Some(&h) => h,
        // Past the table (nothing asks for it): the formula, as before.
        None => ((MAX_HARMONICS as f64 * (-(level as f64) / LEVELS_PER_OCTAVE as f64).exp2())
            .round() as usize)
            .max(1),
    }
}

/// Points in a level's table with `harmonics` partials:
/// `next_pow2(POINTS_PER_TOP_PARTIAL * harmonics)`, clamped to
/// `MIN_TABLE_SIZE..=TABLE_SIZE`. Levels 0-7 (512..45 partials) get 4096,
/// down to 64 for the 1-partial level. A power of two, and at least
/// `8 * harmonics` for every level on the ladder.
pub fn table_size_for(harmonics: usize) -> usize {
    (POINTS_PER_TOP_PARTIAL * harmonics)
        .next_power_of_two()
        .clamp(MIN_TABLE_SIZE, TABLE_SIZE)
}

/// The size a source with an `n`-byte cycle builds `harmonics` partials at:
/// [`table_size_for`], floored at `n`. Both are powers of two, so the floor
/// keeps `size / n` -- the oscillator's phase ratio -- a whole number (a table
/// smaller than its cycle would give a ratio of 0). In the engine a level thin
/// enough to need the floor is never selected for a cycle long enough to hit
/// it; only a fundamental near Nyquist on a long cycle gets there.
fn level_table_size(harmonics: usize, n: usize) -> usize {
    table_size_for(harmonics).max(n)
}

/// The level for a voice stepping `f0` cycles per output sample: the first
/// (richest) level whose top partial is strictly under Nyquist
/// (`H * f0 < 0.5`; a partial exactly at Nyquist has no defined phase to
/// keep, so it is dropped too). A fundamental at or above Nyquist gets the
/// 1-partial level.
pub fn level_for(f0_cycles_per_sample: f64) -> usize {
    (0..LEVEL_COUNT)
        .find(|&l| (level_harmonics(l) as f64) * f0_cycles_per_sample < 0.5)
        .unwrap_or(LEVEL_COUNT - 1)
}

/// What a voice keeps between mixer calls: one band-limited cycle and how it
/// maps onto the voice's `0x280`-byte position.
#[derive(Clone)]
pub struct HifiOsc {
    table: Arc<[i16]>,
    /// `table.len() / N`: the mixer position counts bytes of a `0x280` buffer
    /// that holds `0x280 / N` whole cycles, so `pos * ratio` is table phase.
    ratio: u64,
    /// Mask for a 16.16 phase into this table: `(table.len() << 16) - 1`.
    phase_mask: u64,
    /// Index wrap for the interpolation's second read: `table.len() - 1`.
    last: usize,
}

impl HifiOsc {
    /// `table` (a power-of-two length, at least `n`) played as an `n`-byte cycle.
    fn new(table: Arc<[i16]>, n: usize) -> Self {
        let size = table.len();
        debug_assert!(size.is_power_of_two() && size >= n && size.is_multiple_of(n));
        HifiOsc {
            ratio: (size / n) as u64,
            phase_mask: ((size as u64) << 16) - 1,
            last: size - 1,
            table,
        }
    }

    /// The band-limited value at 16.16 buffer position `pos`, in `i8` scale
    /// times `1 << FRAC_BITS`; linearly interpolated.
    #[inline]
    pub fn sample(&self, pos: u32) -> i32 {
        let p = (pos as u64 * self.ratio) & self.phase_mask;
        let i = (p >> 16) as usize;
        let frac = (p & 0xffff) as i32;
        let a = self.table[i] as i32;
        let b = self.table[(i + 1) & self.last] as i32;
        a + (((b - a) * frac) >> 16)
    }

    /// [`sample`](Self::sample) at four positions at once: the phase and the two
    /// table reads stay scalar (wasm has no gather), the interpolation is one
    /// vector expression. Lane `k` is `sample(pos[k])` exactly -- the same
    /// integer ops in the same order, so nothing here can round differently.
    #[inline]
    pub fn sample4(&self, pos: [u32; 4]) -> i32x4 {
        let mut a = [0i32; 4];
        let mut b = [0i32; 4];
        let mut frac = [0i32; 4];
        for k in 0..4 {
            let p = (pos[k] as u64 * self.ratio) & self.phase_mask;
            let i = (p >> 16) as usize;
            frac[k] = (p & 0xffff) as i32;
            a[k] = self.table[i] as i32;
            b[k] = self.table[(i + 1) & self.last] as i32;
        }
        let (a, b, frac) = (
            i32x4::from_array(a),
            i32x4::from_array(b),
            i32x4::from_array(frac),
        );
        a + (((b - a) * frac) >> i32x4::splat(16))
    }
}

/// One table's spectrum plus the levels built from it so far.
struct Source {
    /// `c_0..=c_MAX_HARMONICS`, in `i8` units.
    coeffs: Vec<Complex<f32>>,
    levels: [Option<Arc<[i16]>>; LEVEL_COUNT],
}

/// How a [`HifiBank`] treats a table it does not have yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BankMode {
    /// Build it. At the cap, drop everything and start over.
    Lazy,
    /// Build it, but never evict: at the cap, stop building (`is_full`).
    Prewarm,
    /// Never build: count a miss and degrade (see the module docs). This is
    /// the mode the audio thread runs in once a song is prewarmed.
    Locked,
}

/// Lazy cache of band-limited cycles, keyed by table contents.
pub struct HifiBank {
    /// Inverse FFTs, one plan per table size (the planner caches them).
    planner: FftPlanner<f32>,
    sources: FxHashMap<Box<[i8]>, Source>,
    cached_tables: usize,
    /// Samples in the cached tables (they differ in size by level).
    cached_points: usize,
    /// `TABLE_SIZE` slots; a build uses the first `size`.
    scratch: Vec<Complex<f32>>,
    mode: BankMode,
    /// Lookups a `Locked` bank could not serve from the exact level.
    misses: u64,
    /// A `Prewarm` bank wanted a table past the cap.
    full: bool,
}

impl Default for HifiBank {
    fn default() -> Self {
        Self::new()
    }
}

impl HifiBank {
    pub fn new() -> Self {
        HifiBank {
            planner: FftPlanner::new(),
            sources: FxHashMap::default(),
            cached_tables: 0,
            cached_points: 0,
            scratch: vec![Complex::new(0.0, 0.0); TABLE_SIZE],
            mode: BankMode::Lazy,
            misses: 0,
            full: false,
        }
    }

    pub fn mode(&self) -> BankMode {
        self.mode
    }

    /// Switches mode. Entering `Prewarm` forgets an earlier `is_full`;
    /// entering `Locked` zeroes the miss count, so it counts from the lock.
    pub fn set_mode(&mut self, mode: BankMode) {
        match mode {
            BankMode::Prewarm => self.full = false,
            BankMode::Locked => self.misses = 0,
            BankMode::Lazy => {}
        }
        self.mode = mode;
    }

    /// Back to `Locked` after a temporary `Prewarm`, keeping the miss count
    /// (which `set_mode(Locked)` zeroes: that is for a first lock).
    pub fn relock(&mut self) {
        self.mode = BankMode::Locked;
    }

    /// Drops every cached table (voices holding one keep it alive, so a sound
    /// already playing is unaffected). For a bank that hit the cap and is about
    /// to be rebuilt with what is wanted now.
    pub fn clear(&mut self) {
        self.sources.clear();
        self.cached_tables = 0;
        self.cached_points = 0;
        self.full = false;
    }

    /// Lookups since the bank was locked that the exact table could not
    /// serve. Zero means the audio thread built nothing and degraded nowhere.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// A prewarm wanted more tables than the cache holds.
    pub fn is_full(&self) -> bool {
        self.full
    }

    /// Distinct table contents seen so far (diagnostics and tests).
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// Mip tables built and currently cached.
    pub fn table_count(&self) -> usize {
        self.cached_tables
    }

    /// Bytes of `i16` samples in the cached tables (diagnostics and tests).
    pub fn table_bytes(&self) -> usize {
        self.cached_points * std::mem::size_of::<i16>()
    }

    /// The oscillator for one cycle (`cycle.len()` is `4 << wave_length`) read
    /// at `f0_cycles_per_sample`; `None` for a silent table (the reference
    /// path already plays exactly that) and, in `Locked` mode, when the source
    /// has no level at or below the wanted one.
    pub fn oscillator(&mut self, cycle: &[i8], f0_cycles_per_sample: f64) -> Option<HifiOsc> {
        let n = cycle.len();
        debug_assert!(n.is_power_of_two() && (4..=128).contains(&n));
        if cycle.iter().all(|&b| b == 0) {
            return None;
        }
        let level = level_for(f0_cycles_per_sample);
        let table = match self.mode {
            BankMode::Locked => self.lookup(cycle, level),
            BankMode::Lazy | BankMode::Prewarm => self.get_or_build(cycle, level),
        }?;
        Some(HifiOsc::new(table, n))
    }

    /// `Locked`: the exact level, else the nearest built level with fewer
    /// partials (duller, still band-limited), else nothing.
    fn lookup(&mut self, cycle: &[i8], level: usize) -> Option<Arc<[i16]>> {
        let source = self.sources.get(cycle);
        if let Some(t) = source.and_then(|s| s.levels[level].as_ref()) {
            return Some(t.clone());
        }
        self.misses += 1;
        source?.levels[level..].iter().flatten().next().cloned()
    }

    fn get_or_build(&mut self, cycle: &[i8], level: usize) -> Option<Arc<[i16]>> {
        // The common case, by far (a prewarm walk asks for the same table tick
        // after tick): one hash of the key, and out.
        if let Some(t) = self
            .sources
            .get(cycle)
            .and_then(|s| s.levels[level].as_ref())
        {
            return Some(t.clone());
        }
        // The table is not there: building it needs room.
        if self.cached_tables >= MAX_CACHED_TABLES {
            if self.mode == BankMode::Prewarm {
                self.full = true;
                return None;
            }
            self.sources.clear();
            self.cached_tables = 0;
            self.cached_points = 0;
        }
        if !self.sources.contains_key(cycle) {
            self.sources.insert(
                cycle.into(),
                Source {
                    coeffs: staircase_spectrum(cycle),
                    levels: Default::default(),
                },
            );
        }
        let source = self.sources.get_mut(cycle).expect("inserted above");
        let harmonics = level_harmonics(level);
        let size = level_table_size(harmonics, cycle.len());
        let inverse = self.planner.plan_fft_inverse(size);
        let t = build_level(
            &*inverse,
            &mut self.scratch[..size],
            &source.coeffs,
            harmonics,
        );
        source.levels[level] = Some(t.clone());
        self.cached_tables += 1;
        self.cached_points += size;
        Some(t)
    }
}

/// Exact Fourier coefficients `c_0..=MAX_HARMONICS` of the zero-order-held
/// staircase of `cycle` (see the module docs), via a direct `N`-point DFT
/// (`N <= 128`, so ~16k multiply-adds; not worth a planner).
fn staircase_spectrum(cycle: &[i8]) -> Vec<Complex<f32>> {
    let n = cycle.len();
    let twiddle: Vec<(f64, f64)> = (0..n)
        .map(|m| {
            let a = -2.0 * std::f64::consts::PI * m as f64 / n as f64;
            (a.cos(), a.sin())
        })
        .collect();
    let dft: Vec<(f64, f64)> = (0..n)
        .map(|k| {
            let (mut re, mut im) = (0.0, 0.0);
            for (j, &x) in cycle.iter().enumerate() {
                let (c, s) = twiddle[(j * k) % n];
                re += x as f64 * c;
                im += x as f64 * s;
            }
            (re, im)
        })
        .collect();

    let mut out = Vec::with_capacity(MAX_HARMONICS + 1);
    out.push(Complex::new((dft[0].0 / n as f64) as f32, 0.0));
    for k in 1..=MAX_HARMONICS {
        let (xr, xi) = dft[k % n];
        let theta = std::f64::consts::PI * k as f64 / n as f64;
        // e^(-i*theta) * sin(theta) / (pi*k)
        let g = theta.sin() / (std::f64::consts::PI * k as f64);
        let (gr, gi) = (theta.cos() * g, -theta.sin() * g);
        out.push(Complex::new(
            (xr * gr - xi * gi) as f32,
            (xr * gi + xi * gr) as f32,
        ));
    }
    out
}

/// One mip level: bins `1..=harmonics` (and their conjugates) populated, every
/// other bin zero -- `wavetable.rs`'s truncation -- then one inverse FFT of
/// `scratch.len()` points (the level's table size, `inverse`'s length).
fn build_level(
    inverse: &dyn Fft<f32>,
    scratch: &mut [Complex<f32>],
    coeffs: &[Complex<f32>],
    harmonics: usize,
) -> Arc<[i16]> {
    let size = scratch.len();
    debug_assert_eq!(inverse.len(), size);
    debug_assert!(harmonics <= size / 8);
    scratch.fill(Complex::new(0.0, 0.0));
    let m = size as f32;
    scratch[0] = coeffs[0] * m;
    for k in 1..=harmonics {
        let v = coeffs[k] * m;
        scratch[k] = v;
        scratch[size - k] = v.conj();
    }
    inverse.process(scratch);
    // rustfft's inverse is unscaled: dividing by M gives sum_k c_k e^(+i..)
    // and the `* m` above already put the coefficients on that scale.
    let scale = (1 << FRAC_BITS) as f32 / m;
    scratch
        .iter()
        .map(|c| {
            (c.re * scale)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_ladder_runs_from_full_band_to_one_partial() {
        assert_eq!(level_harmonics(0), MAX_HARMONICS);
        assert_eq!(level_harmonics(LEVEL_COUNT - 1), 1);
        for l in 1..LEVEL_COUNT {
            assert!(level_harmonics(l) < level_harmonics(l - 1));
        }
    }

    #[test]
    fn level_choice_never_lets_a_partial_pass_nyquist() {
        for i in 1..2000 {
            let f0 = i as f64 * 0.00025; // 0.00025 .. 0.5 cycles/sample
            let l = level_for(f0);
            assert!(
                level_harmonics(l) as f64 * f0 < 0.5 || l == LEVEL_COUNT - 1,
                "f0 {f0} level {l}"
            );
            // ... and it is the richest such level.
            if l > 0 {
                assert!(
                    level_harmonics(l - 1) as f64 * f0 >= 0.5,
                    "f0 {f0}: level {} would also fit",
                    l - 1
                );
            }
        }
        assert_eq!(level_for(0.0), 0);
        assert_eq!(level_for(0.9), LEVEL_COUNT - 1);
    }

    #[test]
    fn a_partial_exactly_at_nyquist_is_dropped() {
        // 4 partials at f0 = 0.125 sit exactly on Nyquist (0.5): not kept.
        let f0 = 0.125;
        assert_eq!(4.0 * f0, 0.5);
        let h = level_harmonics(level_for(f0));
        assert!((h as f64) * f0 < 0.5, "level keeps {h} partials");
    }

    #[test]
    fn table_size_follows_the_level_ladder() {
        // (harmonics, size) for every level: 64 points per top partial,
        // clamped to 64..=4096.
        let want = [
            (512, 4096),
            (362, 4096),
            (256, 4096),
            (181, 4096),
            (128, 4096),
            (91, 4096),
            (64, 4096),
            (45, 4096),
            (32, 2048),
            (23, 2048),
            (16, 1024),
            (11, 1024),
            (8, 512),
            (6, 512),
            (4, 256),
            (3, 256),
            (2, 128),
            (1, 64),
        ];
        assert_eq!(want.len(), LEVEL_COUNT);
        for (level, &(h, size)) in want.iter().enumerate() {
            assert_eq!(level_harmonics(level), h, "level {level}");
            assert_eq!(table_size_for(h), size, "level {level} (H={h})");
            // The Nyquist bin stays empty: at least 8 points per top partial.
            assert!(h <= size / 8, "level {level}");
        }
        // Per source, every level built: 18 x 4096 before, this now.
        assert_eq!(want.iter().map(|&(_, s)| s).sum::<usize>(), 40640);
    }

    #[test]
    fn table_size_clamps_at_both_ends() {
        assert_eq!(table_size_for(1), MIN_TABLE_SIZE);
        assert_eq!(table_size_for(0), MIN_TABLE_SIZE);
        assert_eq!(table_size_for(MAX_HARMONICS), TABLE_SIZE);
        // 64 x 64 is exactly 4096: no rounding up past the clamp needed.
        assert_eq!(table_size_for(64), 4096);
        assert_eq!(table_size_for(33), 4096);
        // Not a power of two: 64 x 45 = 2880, rounded up.
        assert_eq!(table_size_for(45), 4096);
        assert_eq!(table_size_for(23), 2048);
        for h in 1..=MAX_HARMONICS {
            assert!(table_size_for(h).is_power_of_two());
        }
    }

    #[test]
    fn a_table_is_never_shorter_than_its_cycle() {
        for n in [4usize, 8, 16, 32, 64, 128] {
            let cycle: Vec<i8> = (0..n).map(|i| if i < n / 3 { 100 } else { -60 }).collect();
            // What the engine can ask for (`f0 * n <= 0.65` bytes per sample):
            // the level's own size already covers the cycle, the floor is idle.
            for i in 1..=200 {
                let f0 = 0.65 / n as f64 * i as f64 / 200.0;
                let h = level_harmonics(level_for(f0));
                assert!(
                    table_size_for(h) >= n,
                    "n {n} f0 {f0}: H={h} gives {} points",
                    table_size_for(h)
                );
            }
            // Anything else (up to past Nyquist): the floor holds the ratio whole.
            let mut bank = HifiBank::new();
            for i in 1..=400 {
                let f0 = 0.9 * i as f64 / 400.0;
                let o = bank.oscillator(&cycle, f0).unwrap();
                let size = o.table.len();
                assert!(
                    size >= n && size.is_multiple_of(n),
                    "n {n} f0 {f0}: size {size}"
                );
                assert_eq!(o.ratio as usize * n, size);
                assert_eq!(size, level_table_size(level_harmonics(level_for(f0)), n));
            }
            // The floor does bite past the engine's range: 1 partial of a
            // 128-byte cycle is 128 points, not 64.
            assert_eq!(level_table_size(1, 128), 128);
        }
    }

    #[test]
    fn table_bytes_counts_each_table_at_its_own_size() {
        let cycle: Vec<i8> = (0..32).map(|i| if i < 9 { 90 } else { -30 }).collect();
        let mut bank = HifiBank::new();
        bank.oscillator(&cycle, 1e-5).unwrap(); // level 0: 4096 points
        bank.oscillator(&cycle, 0.3).unwrap(); // 1 partial: 64 points (>= n = 32, no floor)
        assert_eq!(bank.table_count(), 2);
        assert_eq!(bank.table_bytes(), (4096 + 64) * 2);
        bank.clear();
        assert_eq!(bank.table_bytes(), 0);
    }

    fn osc(cycle: &[i8], f0: f64) -> HifiOsc {
        HifiBank::new().oscillator(cycle, f0).unwrap()
    }

    /// Value at the middle of table cell `j` of an `n`-point cycle.
    fn cell_mid(o: &HifiOsc, n: usize, j: usize) -> f64 {
        // buffer position of the cell's middle, 16.16
        let pos = ((j as f64 + 0.5) * 65536.0) as u32;
        assert!(n <= 128);
        o.sample(pos) as f64 / (1 << FRAC_BITS) as f64
    }

    #[test]
    fn full_band_level_reproduces_the_staircase_it_was_built_from() {
        // The lowest pitch selects level 0 (512 partials): mid-cell values
        // must sit on the reference's own bytes, away from the Gibbs edges.
        let cycle: [i8; 4] = [0, 0x7f, 0, -128];
        let o = osc(&cycle, 1e-5);
        for (j, &x) in cycle.iter().enumerate() {
            let v = cell_mid(&o, 4, j);
            assert!((v - x as f64).abs() < 2.0, "cell {j}: {v} vs {x}");
        }
    }

    #[test]
    fn a_wider_table_keeps_its_cells_too() {
        let cycle: Vec<i8> = (0..128).map(|i| if i < 100 { -128 } else { 127 }).collect();
        let o = osc(&cycle, 1e-5);
        for &j in &[10usize, 50, 90, 110, 120] {
            let v = cell_mid(&o, 128, j);
            assert!((v - cycle[j] as f64).abs() < 8.0, "cell {j}: {v}");
        }
    }

    #[test]
    fn dc_is_preserved() {
        let cycle: Vec<i8> = (0..128).map(|i| if i < 100 { -100 } else { 50 }).collect();
        let mean = cycle.iter().map(|&b| b as f64).sum::<f64>() / 128.0;
        for f0 in [1e-5, 0.01, 0.1] {
            let o = osc(&cycle, f0);
            // Mean of the interpolated table over one cycle.
            let total: f64 = (0..TABLE_SIZE)
                .map(|i| o.sample((i as u64 * (128u64 << 16) / TABLE_SIZE as u64) as u32) as f64)
                .sum();
            let got = total / TABLE_SIZE as f64 / (1 << FRAC_BITS) as f64;
            assert!((got - mean).abs() < 0.3, "f0 {f0}: mean {got} vs {mean}");
        }
    }

    #[test]
    fn a_level_has_no_energy_above_its_partial_count() {
        let cycle: Vec<i8> = (0..64).map(|i| if i < 20 { -128 } else { 127 }).collect();
        let mut bank = HifiBank::new();
        let f0 = 0.05; // 10 partials fit under Nyquist
        let o = bank.oscillator(&cycle, f0).unwrap();
        let h = level_harmonics(level_for(f0));
        assert!(h as f64 * f0 < 0.5);
        let size = o.table.len();
        let mut spec: Vec<Complex<f32>> = o
            .table
            .iter()
            .map(|&s| Complex::new(s as f32, 0.0))
            .collect();
        FftPlanner::new().plan_fft_forward(size).process(&mut spec);
        let peak = spec[1..=h].iter().map(|c| c.norm()).fold(0.0, f32::max);
        let above = spec[h + 2..size / 2]
            .iter()
            .map(|c| c.norm())
            .fold(0.0, f32::max);
        assert!(
            above < peak * 1e-3,
            "energy above H={h}: {above} vs peak {peak}"
        );
    }

    #[test]
    fn silent_table_falls_back_and_equal_contents_share_levels() {
        let mut bank = HifiBank::new();
        assert!(bank.oscillator(&[0i8; 8], 0.01).is_none());
        let a: Vec<i8> = (0..32).map(|i| if i < 9 { 10 } else { -10 }).collect();
        bank.oscillator(&a, 0.01).unwrap();
        bank.oscillator(&a.clone(), 0.01).unwrap();
        assert_eq!((bank.source_count(), bank.table_count()), (1, 1));
        bank.oscillator(&a, 0.2).unwrap();
        assert_eq!((bank.source_count(), bank.table_count()), (1, 2));
    }

    fn table_ptr(o: &HifiOsc) -> *const i16 {
        o.table.as_ptr()
    }

    #[test]
    fn a_locked_bank_never_builds_and_degrades_to_a_duller_level() {
        let cycle: Vec<i8> = (0..32).map(|i| if i < 9 { 90 } else { -30 }).collect();
        let mut bank = HifiBank::new();
        // Prewarm two levels: the one for a slow note and a coarser one.
        let slow = bank.oscillator(&cycle, 0.01).unwrap();
        let coarse = bank.oscillator(&cycle, 0.2).unwrap();
        let built = bank.table_count();
        bank.set_mode(BankMode::Locked);
        assert_eq!(bank.misses(), 0);

        // Exact hits are free and count nothing.
        assert_eq!(
            table_ptr(&bank.oscillator(&cycle, 0.01).unwrap()),
            table_ptr(&slow)
        );
        assert_eq!(bank.misses(), 0);

        // A pitch that wants a level in between: served by the next coarser
        // one built (fewer partials -- never aliased), counted, nothing built.
        let mid_f0 = 0.05;
        assert!(level_for(mid_f0) > level_for(0.01) && level_for(mid_f0) < level_for(0.2));
        assert_eq!(
            table_ptr(&bank.oscillator(&cycle, mid_f0).unwrap()),
            table_ptr(&coarse)
        );
        assert_eq!((bank.misses(), bank.table_count()), (1, built));

        // A table never seen has nothing to stand in for it: reference path.
        let other: Vec<i8> = (0..32).map(|i| if i < 20 { 90 } else { -30 }).collect();
        assert!(bank.oscillator(&other, 0.01).is_none());
        assert_eq!(bank.table_count(), built, "a locked bank built a table");
        assert_eq!(bank.source_count(), 1);
    }

    #[test]
    fn a_locked_miss_with_nothing_coarser_falls_back_to_the_reference() {
        let cycle: Vec<i8> = (0..16).map(|i| if i < 5 { 60 } else { -20 }).collect();
        let mut bank = HifiBank::new();
        bank.oscillator(&cycle, 0.2).unwrap(); // only a coarse level exists
        bank.set_mode(BankMode::Locked);
        // A slow note wants a far richer level than the one built: the built
        // one has *fewer* partials, so it is the stand-in that exists.
        assert!(bank.oscillator(&cycle, 0.001).is_some());
        // But a fast note asking for a coarser level than anything built has none.
        let fast = level_for(0.45);
        assert!(fast > level_for(0.2));
        assert!(bank.oscillator(&cycle, 0.45).is_none());
        assert_eq!(bank.misses(), 2);
    }

    #[test]
    fn locking_zeroes_the_miss_count_and_prewarm_never_evicts() {
        let cycle: Vec<i8> = (0..8).map(|i| if i < 3 { 50 } else { -50 }).collect();
        let mut bank = HifiBank::new();
        bank.set_mode(BankMode::Locked);
        assert!(bank.oscillator(&cycle, 0.01).is_none());
        assert_eq!(bank.misses(), 1);
        bank.set_mode(BankMode::Prewarm);
        bank.oscillator(&cycle, 0.01).unwrap();
        assert!(!bank.is_full());
        bank.set_mode(BankMode::Locked);
        assert_eq!(bank.misses(), 0);
    }
}
