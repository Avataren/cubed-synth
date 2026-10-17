//! SID waveform generator output: the 12-bit value presented to a voice's
//! waveform DAC, as a pure function of the control register, the voice's
//! 24-bit phase accumulator, its sync/ring source's accumulator, the
//! pulse-width register and the noise LFSR output.
//!
//! Model: combinational logic over the accumulator bits, one evaluation per
//! chip cycle.
//!
//! Sources (GPL-free): MOS 6581/8580 datasheet (control register bits,
//! "SYNC", "RING MOD", "TEST" descriptions); Bob Yannes' published interview
//! on the SID design (saw = upper 12 accumulator bits, triangle = MSB-folded
//! lower bits, pulse = 12-bit comparator, selected waveforms are wired
//! together on the same output lines); public C64 community hardware notes.
//!
//! - Sawtooth: accumulator bits 23..12. Exact.
//! - Triangle: bits 22..11, XOR-folded by the fold signal, output LSB 0.
//!   Without ring mod the fold signal is the voice's own MSB. Exact.
//! - Ring mod (control bit 2): the fold signal becomes
//!   `MSB(own) XOR NOT MSB(source)` (plan §1.2's wording, "triangle output XOR
//!   NOT MSB(osc B)"). INFERRED from the public die-level description that
//!   ring mod swaps the triangle's MSB for an XOR with the source MSB; the
//!   NOT is the plan's specified polarity. On its own it only shifts a
//!   static triangle by half a period (an inverted triangle is the same
//!   wave delayed by half a cycle). The ring-mod sound comes from the XOR.
//!   Ring mod touches only the triangle function. It has no effect unless
//!   TRI is selected, and in a combined waveform the triangle component is
//!   the ring-modulated one.
//! - Pulse: all ones when `acc[23:12] >= PW`, else zero. Exact. TEST forces
//!   the pulse output high. The datasheet says "the Pulse waveform output is
//!   held at a DC level"; the level being all ones is INFERRED from public
//!   hardware notes (the C64 "test-bit digi" technique relies on it).
//! - Noise: supplied by `noise.rs` (8 LFSR taps on DAC bits 11..4).
//! - Combined waveforms, 8580: the selected outputs share the waveform bit
//!   lines. The base is the bitwise AND of the selected 12-bit outputs (the
//!   public combinational-logic "wired-AND" view). The ideal AND is far too
//!   loud against the measured levels (S5.12 R2,
//!   `.ai/sid-chip-comparison-report.md` §2.1: pulse+saw about +6 dB,
//!   saw+tri about +10 dB). So pulse+saw, saw+tri and pulse+saw+tri go
//!   through the same neighbour-pull pass as the 6581 (below), with a
//!   weaker, per-combination threshold fitted to the measured mean level
//!   (`PULL_THRESHOLDS_8580`, DERIVED-VALUE disclosure there). Pulse+tri
//!   and noise combinations have no measured level and stay the plain AND.
//!   Noise in a combination also writes zeros back into the LFSR (see
//!   `noise.rs`).
//! - Combined waveforms, 6581 (S2). Public knowledge: the 6581's combined
//!   waveforms come out far weaker than the ideal AND. Saw+tri is mostly
//!   silent, and pulse+saw keeps only the upper part of the ramp. The
//!   public explanation is the same shared-bit-line picture, with stronger
//!   coupling on the 6581: a line held low by one waveform also drags its
//!   neighbours low, and the effect falls off with distance. Modelled as a
//!   pass over the ideal AND `a`:
//!     pull(i) = sum over bits j != i with a_j = 0 of 2^(11 - |i - j|)
//!     out bit i = a_i AND pull(i) < 1536
//!   The weight halves per bit of distance (a zero next door pulls 1024, one
//!   two bits away 512, ...), and the threshold 1536 = 0.75 * 2048. So an
//!   isolated one-bit inside the word always vanishes (a zero on each side
//!   = 2048). An edge bit (11 or 0) has only one neighbour. It vanishes
//!   when a long enough run of zeros follows it (e.g. 0x800 alone: 2047),
//!   but survives sparse zeros (bit 0 of 0x555: 1365). The bottom bit of a
//!   run that sits on a long run of zeros is eaten ("run-down"). Long runs
//!   of ones survive.
//!   All ones and all zeros are fixed points. The pass is not iterated, and
//!   it uses only the AND, not which waveforms made it.
//!   INFERRED: the neighbour-pull rule and its 2^-d weights are my own
//!   tuning to match the qualitative description. No measured 6581
//!   combined-waveform table was used: the published ones sit inside GPL
//!   emulators. It applies only when two or more waveforms are selected, so
//!   a single waveform is untouched.
//!   Thresholds (S5.12 R2): the S2 threshold 0.75 left the 6581 about
//!   +20 dB too loud on pulse+saw (report §2.2). Pulse+saw, saw+tri and
//!   pulse+saw+tri now use per-combination thresholds fitted to the
//!   measured mean levels (`PULL_THRESHOLDS_6581`, DERIVED-VALUE disclosure
//!   there). Pulse+tri and noise combinations keep 0.75
//!   (`PULL_THRESHOLD_6581`), because no level was measured for them.
//!   Because a lower threshold only removes more bits, each 6581
//!   combination is a bit subset of the 8580 one at the same phase.
//!   Precomputed into 4096-entry tables (`COMBINED_6581`,
//!   `COMBINED_FITTED_*`), because it runs every chip cycle. Ears-gate.
//! - No waveform selected ("waveform 0"): the DAC input floats and keeps
//!   the last output. INFERRED from public hardware notes. `waveform_output`
//!   returns `None` and the voice keeps its previous value. On the 8580 the
//!   slow leak of the held value is not modelled. On the 6581 (S2) the held
//!   value fades to 0 after a fixed time; see `voice.rs`. The TEST bit
//!   does not drive the DAC when no waveform is selected, so TEST with
//!   waveform 0 changes nothing here: the held value stays, and on the 6581
//!   its fade keeps running.

use super::SidModel;

/// Control register ($D404/$D40B/$D412) bits.
pub const GATE: u8 = 0x01;
pub const SYNC: u8 = 0x02;
pub const RING: u8 = 0x04;
pub const TEST: u8 = 0x08;
pub const TRI: u8 = 0x10;
pub const SAW: u8 = 0x20;
pub const PULSE: u8 = 0x40;
pub const NOISE: u8 = 0x80;

const MSB: u32 = 0x80_0000;

/// Sawtooth: the upper 12 bits of the accumulator, verbatim.
#[inline]
pub fn sawtooth(acc: u32) -> u16 {
    ((acc >> 12) & 0xFFF) as u16
}

/// Triangle with an explicit fold signal: accumulator bits 22..12 become
/// output bits 11..1 (inverted when `fold`), output bit 0 is always 0.
#[inline]
pub fn triangle_folded(acc: u32, fold: bool) -> u16 {
    let bits = if fold { acc ^ 0x7F_FFFF } else { acc };
    ((bits >> 11) & 0xFFE) as u16
}

/// Plain triangle: folded by the voice's own MSB.
#[inline]
pub fn triangle(acc: u32) -> u16 {
    triangle_folded(acc, acc & MSB != 0)
}

/// Ring-modulated triangle: fold = MSB(own) XOR NOT MSB(source).
#[inline]
pub fn triangle_ring(acc: u32, source_acc: u32) -> u16 {
    let own = acc & MSB != 0;
    let src = source_acc & MSB != 0;
    triangle_folded(acc, own ^ !src)
}

/// Pulse: 12-bit comparator, all ones when `acc[23:12] >= pw`.
#[inline]
pub fn pulse(acc: u32, pw: u16) -> u16 {
    if sawtooth(acc) >= (pw & 0xFFF) {
        0xFFF
    } else {
        0
    }
}

/// The 12-bit waveform DAC input for one voice, or `None` when no waveform
/// is selected (the voice then holds its previous output).
///
/// `noise_out` is the LFSR's 12-bit output (`Noise::output`).
#[inline]
pub fn waveform_output(
    model: SidModel,
    control: u8,
    acc: u32,
    source_acc: u32,
    pw: u16,
    noise_out: u16,
) -> Option<u16> {
    let sel = control & 0xF0;
    if sel == 0 {
        return None;
    }
    let mut out = 0xFFFu16;
    if sel & TRI != 0 {
        out &= if control & RING != 0 {
            triangle_ring(acc, source_acc)
        } else {
            triangle(acc)
        };
    }
    if sel & SAW != 0 {
        out &= sawtooth(acc);
    }
    if sel & PULSE != 0 {
        out &= if control & TEST != 0 {
            0xFFF
        } else {
            pulse(acc, pw)
        };
    }
    if sel & NOISE != 0 {
        out &= noise_out;
    }
    if sel.count_ones() < 2 {
        return Some(out);
    }
    let a = out as usize;
    Some(match (model, fitted_combination(sel)) {
        (SidModel::Sid8580, Some(c)) => COMBINED_FITTED_8580[c][a],
        // Pulse+tri and noise combinations: the ideal AND (see header).
        (SidModel::Sid8580, None) => out,
        (SidModel::Sid6581, Some(c)) => COMBINED_FITTED_6581[c][a],
        (SidModel::Sid6581, None) => COMBINED_6581[a],
    })
}

/// Threshold of the S2 6581 neighbour pull, 0.75 in units of 2048. Used for
/// the 6581 combinations without a measured level (pulse+tri, noise+X).
pub const PULL_THRESHOLD_6581: u32 = 1536;

/// The combinations with a measured level, in `PULL_THRESHOLDS_*` order.
pub const FITTED_COMBINATIONS: [u8; 3] = [PULSE | SAW, SAW | TRI, PULSE | SAW | TRI];

/// Neighbour-pull thresholds of the combinations with a measured level
/// (S5.12 R2), in `FITTED_COMBINATIONS` order: pulse+saw, saw+tri,
/// pulse+saw+tri. A bit survives when its pull (header) is below the
/// threshold, so a lower threshold means stronger bit-line coupling.
///
/// DERIVED-VALUE DISCLOSURE.
/// - Measured facts: the target mean levels, from
///   `.ai/sid-chip-comparison-report.md` §2.1/§2.2. That report is a
///   transient comparison against reSID's measured combined-waveform tables,
///   averaged over all 4096 phases with the pulse held high, on the 8-bit
///   OSC3 scale (`out >> 4`):
///     8580 pulse+saw ~62, saw+tri ~20;
///     6581 pulse+saw ~7,  saw+tri ~1.5, pulse+saw+tri ~0.4.
///   The 8580 pulse+saw+tri level is NOT reported. The value used,
///   sqrt(62 * 20) = 35.2, is the geometric mean of the two measured 8580
///   levels (the midpoint in dB). It is an interpolation, not a measurement.
/// - Shape source: the public description of the mechanism, as cited in
///   the S5.12 brief: Antti Lankila's reSID-fp combined-waveform measurement
///   write-ups (arXiv:0805.0171; bel.fi/~ankila/). The selected outputs
///   share bit lines, and a line held low drags its neighbours down, with
///   an influence that falls off with distance. That is the S2
///   neighbour-pull rule below, unchanged except for the threshold.
/// - Fitted parameters: the six thresholds. Each one is the integer that
///   brings its combination's 4096-phase mean closest to its target (the
///   2^-d weights are S2's, not refitted). Fitted means: 8580 62.01, 20.15,
///   35.01; 6581 6.99, 1.50, 0.40. See the test
///   `combined_waveform_means_match_the_measured_levels`, which allows ±15%.
/// - NOT used: reSID's wave6581_* / wave8580_* tables (GPL). No per-phase
///   value was taken from them; only the report's averages.
/// - Expected tolerance vs reSID: the means match by construction. The
///   per-phase shape is the pull rule's, not the chip's. It is expected to
///   be within ±3 dB RMS per combination, but that is unverified against
///   the tables. Ears-gate.
pub const PULL_THRESHOLDS_8580: [u32; 3] = [1314, 1792, 2031];
/// 6581 thresholds, same order and disclosure as `PULL_THRESHOLDS_8580`.
pub const PULL_THRESHOLDS_6581: [u32; 3] = [167, 1219, 761];

/// Index of `sel` (bits 4-7 of the control register) in
/// `FITTED_COMBINATIONS`, if it is one of them.
#[inline]
const fn fitted_combination(sel: u8) -> Option<usize> {
    let mut c = 0;
    while c < FITTED_COMBINATIONS.len() {
        if FITTED_COMBINATIONS[c] == sel {
            return Some(c);
        }
        c += 1;
    }
    None
}

/// The neighbour-pull pass over one ideal wired-AND value (header): bit i
/// of `and` survives when `pull(i) < threshold`.
///
/// `pull(i)` is split into its left half `L(i)` (zeros below i) and right
/// half `R(i)` (zeros above i). Each half follows the recurrence
/// L(i) = L(i-1)/2 + 1024*[bit i-1 zero]. That is exact in integers,
/// because every term of L(i-1) is 2^(12-i+j) with j >= 0, i <= 11.
pub const fn neighbour_pull(and: u16, threshold: u32) -> u16 {
    const fn zero_weight(and: u16, i: usize) -> u32 {
        if (and >> i) & 1 == 0 {
            1024
        } else {
            0
        }
    }
    let mut left = [0u32; 12];
    let mut i = 1;
    while i < 12 {
        left[i] = left[i - 1] / 2 + zero_weight(and, i - 1);
        i += 1;
    }
    let mut out = 0u16;
    let mut right = 0u32;
    let mut i = 12;
    while i > 0 {
        i -= 1;
        if (and >> i) & 1 != 0 && left[i] + right < threshold {
            out |= 1 << i;
        }
        right = right / 2 + zero_weight(and, i);
    }
    out
}

/// The S2 6581 neighbour-pull pass (threshold `PULL_THRESHOLD_6581`).
pub const fn combined_6581(and: u16) -> u16 {
    neighbour_pull(and, PULL_THRESHOLD_6581)
}

/// `neighbour_pull(_, threshold)` for every 12-bit AND value.
const fn pull_table(threshold: u32) -> [u16; 4096] {
    let mut t = [0u16; 4096];
    let mut a = 0;
    while a < 4096 {
        t[a] = neighbour_pull(a as u16, threshold);
        a += 1;
    }
    t
}

/// `combined_6581` for every 12-bit AND value.
pub static COMBINED_6581: [u16; 4096] = pull_table(PULL_THRESHOLD_6581);

/// Per fitted combination, the pull table at its 8580 threshold.
pub static COMBINED_FITTED_8580: [[u16; 4096]; 3] = [
    pull_table(PULL_THRESHOLDS_8580[0]),
    pull_table(PULL_THRESHOLDS_8580[1]),
    pull_table(PULL_THRESHOLDS_8580[2]),
];

/// Per fitted combination, the pull table at its 6581 threshold.
pub static COMBINED_FITTED_6581: [[u16; 4096]; 3] = [
    pull_table(PULL_THRESHOLDS_6581[0]),
    pull_table(PULL_THRESHOLDS_6581[1]),
    pull_table(PULL_THRESHOLDS_6581[2]),
];

#[cfg(test)]
mod tests {
    use super::*;

    const M: SidModel = SidModel::Sid8580;

    #[test]
    fn sawtooth_is_upper_twelve_bits() {
        assert_eq!(sawtooth(0x00_0000), 0x000);
        assert_eq!(sawtooth(0x00_0FFF), 0x000); // lower 12 bits ignored
        assert_eq!(sawtooth(0x00_1000), 0x001);
        assert_eq!(sawtooth(0x80_0000), 0x800);
        assert_eq!(sawtooth(0xFF_FFFF), 0xFFF);
    }

    #[test]
    fn triangle_folds_on_msb_and_has_even_output() {
        // Rising half: acc 0x400000 -> bits 22..11 = 0x800.
        assert_eq!(triangle(0x00_0000), 0x000);
        assert_eq!(triangle(0x40_0000), 0x800);
        assert_eq!(triangle(0x7F_FFFF), 0xFFE); // peak, LSB always 0
                                                // MSB set: 0x800000 ^ 0x7FFFFF = 0xFFFFFF -> bits 22..11 = 0xFFE.
        assert_eq!(triangle(0x80_0000), 0xFFE);
        // 0xC00000 ^ 0x7FFFFF = 0xBFFFFF -> (>>11) & 0xFFE = 0x7FE.
        assert_eq!(triangle(0xC0_0000), 0x7FE);
        assert_eq!(triangle(0xFF_FFFF), 0x000);
        for a in (0..0x80_0000u32).step_by(0x1_2345) {
            assert_eq!(triangle(a), triangle(0xFF_FFFF - a));
            assert_eq!(triangle(a) & 1, 0);
        }
    }

    #[test]
    fn pulse_compares_upper_bits_against_width() {
        assert_eq!(pulse(0x7F_F000, 0x800), 0);
        assert_eq!(pulse(0x7F_FFFF, 0x800), 0);
        assert_eq!(pulse(0x80_0000, 0x800), 0xFFF);
        for a in [0u32, 0x12_3456, 0xFF_FFFF] {
            assert_eq!(pulse(a, 0x000), 0xFFF, "pw 0 is always high");
        }
        assert_eq!(pulse(0xFF_EFFF, 0xFFF), 0);
        assert_eq!(pulse(0xFF_F000, 0xFFF), 0xFFF);
        assert_eq!(pulse(0x80_0000, 0x1800), pulse(0x80_0000, 0x800));
    }

    #[test]
    fn pulse_duty_cycle_over_a_full_sweep() {
        for (pw, want) in [(0x400u16, 0.75), (0x800, 0.5), (0xC00, 0.25)] {
            let high = (0..4096u32).filter(|u| pulse(u << 12, pw) != 0).count();
            assert_eq!(high as f64 / 4096.0, want);
        }
    }

    #[test]
    fn ring_mod_truth_table() {
        // Hand derivation. Accumulator low bits fixed at 0x400000 (rising
        // quarter: bits 22..11 = 0x800) or 0xC00000 (bits 22..11 = 0x800
        // too, own MSB set). Folding XORs bits 22..0 with ones, turning
        // bits 22..11 = 0x800 into 0x7FF, masked to 0x7FE.
        //   fold = own ^ !src
        //   own src | fold | acc      -> out
        //    0   0  |  1   | 0x400000 -> 0x7FE
        //    0   1  |  0   | 0x400000 -> 0x800
        //    1   0  |  0   | 0xC00000 -> 0x800
        //    1   1  |  1   | 0xC00000 -> 0x7FE
        // Ring off: fold = own -> 0x400000 -> 0x800, 0xC00000 -> 0x7FE.
        let lo = 0x40_0000u32;
        let hi = 0xC0_0000u32;
        let src0 = 0x00_0000u32;
        let src1 = 0x80_0000u32;
        let ring = TRI | RING;
        let w = |c, a, s| waveform_output(M, c, a, s, 0, 0).unwrap();
        assert_eq!(w(ring, lo, src0), 0x7FE);
        assert_eq!(w(ring, lo, src1), 0x800);
        assert_eq!(w(ring, hi, src0), 0x800);
        assert_eq!(w(ring, hi, src1), 0x7FE);
        assert_eq!(w(TRI, lo, src0), 0x800);
        assert_eq!(w(TRI, lo, src1), 0x800);
        assert_eq!(w(TRI, hi, src0), 0x7FE);
        assert_eq!(w(TRI, hi, src1), 0x7FE);
        // Ring mod is a triangle-only function: saw and pulse ignore it.
        for s in [src0, src1] {
            assert_eq!(w(SAW | RING, hi, s), 0xC00);
            assert_eq!(w(PULSE | RING, hi, s), 0xFFF);
        }
        // Flipping the fold flips output bits 11..1: ring output with a
        // source MSB of 0 is the plain triangle XOR 0xFFE, for any phase.
        for a in (0..0x100_0000u32).step_by(0x3_1415) {
            assert_eq!(w(ring, a, src0), triangle(a) ^ 0xFFE);
            assert_eq!(w(ring, a, src1), triangle(a));
        }
    }

    #[test]
    fn combined_waveforms_are_the_wired_and_then_the_fitted_pull() {
        // acc 0x600000: saw = 0x600; tri (own MSB 0) = bits 22..11 of
        // 0x600000 = 0xC00; pulse(pw 0x400) = 0xFFF (0x600 >= 0x400).
        //   saw & tri         = 0x600 & 0xC00 = 0x400
        //   pulse & saw       = 0x600
        //   pulse & tri       = 0xC00 (8580: the plain AND, no measured level)
        //   pulse & saw & tri = 0x400
        // The fitted 8580 pull (thresholds 1314 / 1792 / 2031):
        //   0x400: bit 10 alone, zeros on both sides: 2048 -> gone -> 0.
        //   0x600: bit 10: zero 11 (1024) + zeros 8..0 at d 2..10 (1022) =
        //          2046; bit 9: zero 11 (512) + zeros 8..0 at d 1..9 (2044)
        //          = 2556 -> both gone -> 0.
        // With pw 0x800 the pulse is low (0x600 < 0x800), so every pulse
        // combination is 0.
        let a = 0x60_0000u32;
        let w = |c, pw| waveform_output(M, c, a, 0, pw, 0xFF0).unwrap();
        assert_eq!(w(SAW | TRI, 0x400), 0);
        assert_eq!(w(PULSE | SAW, 0x400), 0);
        assert_eq!(w(PULSE | TRI, 0x400), 0xC00);
        assert_eq!(w(PULSE | SAW | TRI, 0x400), 0);
        for c in [PULSE | SAW, PULSE | TRI, PULSE | SAW | TRI] {
            assert_eq!(w(c, 0x800), 0);
        }
        // Noise 0xFF0 AND saw 0x600 = 0x600; low nibble of noise is 0.
        assert_eq!(w(NOISE | SAW, 0x400), 0x600);
        assert_eq!(
            waveform_output(M, NOISE | SAW, 0x60_F000, 0, 0, 0xFF0),
            Some(0x600)
        );
        // acc 0x7FF000: saw 0x7FF, tri 0xFFE, pulse(pw 0x400) high, so
        // saw & tri = 0x7FE. Bits 1 and 10: 1024 + 2 = 1026; bits 2..9:
        // at most 512 + 4 = 516. Every 8580 threshold is above 1026 -> 0x7FE.
        // acc 0xFF0000, pulse+saw: AND 0xFF0. Bit 4 sees zeros 3..0 at d 1..4
        // = 1920 (gone); bit 5 d 2..5 = 960 < 1314 (kept); higher bits less
        // -> 0xFE0.
        let w = |c, a| waveform_output(M, c, a, 0, 0x400, 0).unwrap();
        assert_eq!(w(SAW | TRI, 0x7F_F000), 0x7FE);
        assert_eq!(w(PULSE | SAW | TRI, 0x7F_F000), 0x7FE);
        assert_eq!(w(PULSE | SAW, 0xFF_0000), 0xFE0);
    }

    #[test]
    fn test_bit_forces_pulse_high_and_nothing_else() {
        // pw 0xFFF at acc 0: comparator low; TEST forces all ones.
        assert_eq!(waveform_output(M, PULSE, 0, 0, 0xFFF, 0), Some(0));
        assert_eq!(
            waveform_output(M, PULSE | TEST, 0, 0, 0xFFF, 0),
            Some(0xFFF)
        );
        assert_eq!(
            waveform_output(M, SAW | TEST, 0x60_0000, 0, 0, 0),
            Some(0x600)
        );
    }

    #[test]
    fn combined_6581_pull_down_hand_values() {
        // Weights: a zero at distance d pulls 2^(11 - d); keep if < 1536.
        //  0xFFF: no zeros -> kept. 0x000: nothing to keep.
        //  0x400 (bit 10 alone): zeros at 11 and 9 (1024 each) -> gone.
        //  0x800 (bit 11 alone): zeros 10..0, d 1..11: 2047 -> gone.
        //  0xC00: bit 11 sees zeros 9..0 at d 2..11: 1023 -> kept;
        //         bit 10 sees zeros 9..0 at d 1..10: 2046 -> gone. -> 0x800
        //  0xE00: bit 9: 2044 gone; bit 10: 1022 kept; bit 11: 511 kept -> 0xC00
        //  0xFF0: bit 4: zeros 3..0 at d 1..4 = 1920 gone; bit 5: d 2..5 =
        //         960 kept; higher bits smaller -> 0xFE0
        //  0x7FE: bit 1: zero 0 (1024) + zero 11 (d 10, 2) = 1026 kept;
        //         bit 10 symmetric -> 0x7FE unchanged
        //  0x555: bits 2..10 have zeros on both sides (2048) -> gone; bit 0
        //         is an edge bit: zero 1 (1024) + zeros 3,5,7,9,11 (256 + 64
        //         + 16 + 4 + 1) = 1365 -> kept. -> 0x001
        //         (first derivation said 0 and missed the edge; the test
        //         caught it)
        //  0x600: bit 10: zero 11 (1024) + zeros 8..0 at d 2..10 (1022) =
        //         2046 gone; bit 9: zeros 8..0 at d 1..9 (2044) gone -> 0
        for (a, want) in [
            (0xFFFu16, 0xFFFu16),
            (0, 0),
            (0x400, 0),
            (0x800, 0),
            (0xC00, 0x800),
            (0xE00, 0xC00),
            (0xFF0, 0xFE0),
            (0x7FE, 0x7FE),
            (0x555, 0x001),
            (0x600, 0),
        ] {
            assert_eq!(combined_6581(a), want, "{a:#05x}");
            assert_eq!(COMBINED_6581[a as usize], want);
        }
    }

    #[test]
    fn combined_6581_only_removes_bits_and_only_for_combinations() {
        for a in 0..4096u16 {
            assert_eq!(COMBINED_6581[a as usize] & !a, 0, "{a:#05x} gained bits");
        }
        // Single waveforms are identical on both models at every phase.
        let m6 = SidModel::Sid6581;
        for acc in (0..0x100_0000u32).step_by(0x1_0001) {
            for c in [TRI, SAW, PULSE, NOISE, TRI | RING, PULSE | TEST] {
                assert_eq!(
                    waveform_output(m6, c, acc, 0x80_0000, 0x800, 0xA50),
                    waveform_output(M, c, acc, 0x80_0000, 0x800, 0xA50)
                );
            }
        }
        // The fitted tables only remove bits too, and each 6581 threshold is
        // below its 8580 one, so the 6581 output is a bit subset of the 8580's.
        for c in 0..3 {
            assert!(PULL_THRESHOLDS_6581[c] < PULL_THRESHOLDS_8580[c]);
            for a in 0..4096usize {
                let (x8, x6) = (COMBINED_FITTED_8580[c][a], COMBINED_FITTED_6581[c][a]);
                assert_eq!(x8 & !(a as u16), 0, "8580 {c}: {a:#05x} gained bits");
                assert_eq!(
                    x6 & !x8,
                    0,
                    "{c}: {a:#05x}: 6581 {x6:#05x} not within 8580 {x8:#05x}"
                );
            }
        }
        // Saw+tri at acc 0x7FF000: AND 0x7FE (bits 1 and 10 pull 1026, the
        // rest at most 516). 6581 saw+tri (1219) keeps all of it; 6581
        // pulse+saw+tri (761) drops bits 1 and 10 -> 0x3FC; the 8580 keeps it.
        let w = |m, c| waveform_output(m, c, 0x7F_F000, 0, 0x400, 0);
        assert_eq!(w(m6, SAW | TRI), Some(0x7FE));
        assert_eq!(w(m6, PULSE | SAW | TRI), Some(0x3FC));
        assert_eq!(w(M, PULSE | SAW | TRI), Some(0x7FE));
        // Saw+tri at acc 0x600000: AND 0x400, isolated bit -> 0 on both.
        assert_eq!(waveform_output(m6, SAW | TRI, 0x60_0000, 0, 0, 0), Some(0));
        assert_eq!(waveform_output(M, SAW | TRI, 0x60_0000, 0, 0, 0), Some(0));
    }

    /// The S2 pull as first written (a double loop over bit pairs), kept as
    /// the reference for the recurrence in `neighbour_pull`.
    fn pull_reference(and: u16, threshold: u32) -> u16 {
        let mut out = 0u16;
        for i in 0..12 {
            if (and >> i) & 1 == 0 {
                continue;
            }
            let pull: u32 = (0..12)
                .filter(|&j| j != i && (and >> j) & 1 == 0)
                .map(|j: i32| 1u32 << (11 - (i - j).abs()))
                .sum();
            if pull < threshold {
                out |= 1 << i;
            }
        }
        out
    }

    #[test]
    fn neighbour_pull_recurrence_matches_the_pairwise_sum() {
        let thresholds = [
            0u32, 1, 167, 761, 1024, 1219, 1314, 1536, 1792, 2031, 2048, 4096,
        ];
        for t in thresholds {
            for a in 0..4096u16 {
                assert_eq!(
                    neighbour_pull(a, t),
                    pull_reference(a, t),
                    "{a:#05x} threshold {t}"
                );
            }
        }
        for c in 0..3 {
            assert_eq!(
                COMBINED_FITTED_8580[c][0x6A5],
                neighbour_pull(0x6A5, PULL_THRESHOLDS_8580[c])
            );
            assert_eq!(
                COMBINED_FITTED_6581[c][0xF3C],
                neighbour_pull(0xF3C, PULL_THRESHOLDS_6581[c])
            );
        }
    }

    /// Mean 8-bit (OSC3-scale, `out >> 4`) level of `control` over the 4096
    /// saw phases `acc = u << 12`, pulse held high (pw 0): the comparison
    /// report's §2 measurement.
    fn mean8(model: SidModel, control: u8) -> f64 {
        let sum: u32 = (0..4096u32)
            .map(|u| (waveform_output(model, control, u << 12, 0, 0, 0).unwrap() >> 4) as u32)
            .sum();
        sum as f64 / 4096.0
    }

    #[test]
    fn combined_waveform_means_match_the_measured_levels() {
        // Targets: .ai/sid-chip-comparison-report.md §2.1/§2.2 (reSID's
        // measured tables, 4096 phases, pulse high). 8580 P+S+T has no
        // reported figure: the geometric mean of P+S and S+T (DERIVED, see
        // `PULL_THRESHOLDS_8580`). Before S5.12 R2: 8580 P+S 127.5, S+T 63.75,
        // P+S+T 63.75; 6581 P+S 74.9, S+T 11.7, P+S+T 11.7.
        let (m8, m6) = (SidModel::Sid8580, SidModel::Sid6581);
        let cases = [
            (m8, PULSE | SAW, 62.0),
            (m8, SAW | TRI, 20.0),
            (m8, PULSE | SAW | TRI, (62.0f64 * 20.0).sqrt()),
            (m6, PULSE | SAW, 7.0),
            (m6, SAW | TRI, 1.5),
            (m6, PULSE | SAW | TRI, 0.4),
        ];
        for (model, control, want) in cases {
            let got = mean8(model, control);
            assert!(
                (got / want - 1.0).abs() <= 0.15,
                "{model:?} {control:#04x}: mean {got:.3}, want {want:.3} ±15%"
            );
        }
        // The report's relationship: the 6581's pulse+saw is far below the
        // 8580's (~7 vs ~62).
        assert!(mean8(m6, PULSE | SAW) < mean8(m8, PULSE | SAW));
        // Pulse gating is unchanged: pulse low (pw 0xFFF, phases below it)
        // silences every pulse combination on both models.
        for model in [m8, m6] {
            for control in [PULSE | SAW, PULSE | TRI, PULSE | SAW | TRI] {
                for u in (0..0xFFFu32).step_by(7) {
                    assert_eq!(
                        waveform_output(model, control, u << 12, 0, 0xFFF, 0),
                        Some(0)
                    );
                }
            }
        }
    }

    #[test]
    fn no_waveform_selected_is_none() {
        for c in [0u8, GATE, SYNC | RING | TEST | GATE] {
            assert_eq!(waveform_output(M, c, 0x60_0000, 0, 0, 0xFF0), None);
        }
    }
}
