//! Noise: a 23-bit Fibonacci LFSR, clocked by the voice's accumulator.
//!
//! Model and sources (GPL-free: public die-shot analysis notes, Bob Yannes'
//! interview, the MOS datasheet's TEST-bit text):
//! - The register shifts left once each time accumulator bit 19 goes 0 -> 1.
//!   Bit 19 toggles every 2^19 accumulator units, so the shift rate is
//!   `freq_reg * clock / 2^20` = 16x the oscillator pitch. That is why SID
//!   noise pitch tracks the note.
//! - Feedback: new bit 0 = bit 22 XOR bit 17. x^23 + x^18 + 1 is primitive,
//!   so the sequence visits every non-zero state (length 2^23 - 1).
//! - Output: register bits 20, 18, 14, 11, 9, 5, 2, 0 drive waveform DAC
//!   bits 11..4. DAC bits 3..0 are 0.
//! - Power-on value 0x7FFFF8 (public reverse-engineering notes).
//! - TEST bit (datasheet: "The Noise waveform output of Oscillator 1 is also
//!   reset"): while TEST is set the register is held at all ones,
//!   0x7FFFFF. INFERRED: the all-ones value is from public hardware notes.
//!   On a real chip the ones fill in over a leak time; here the reset is
//!   immediate. The accumulator is held at 0 at the same time, so when TEST
//!   is released the first shift comes exactly 2^19 / freq cycles later.
//!   The noise/oscillator phase relation is reset.
//! - Combined-waveform write-back ("noise lock-up", public C64 lore): when
//!   noise is selected together with another waveform, the shared output
//!   lines pulled low by the other waveform also pull the LFSR's tap cells
//!   low. INFERRED timing: the write-back is applied at each shift, just
//!   before the register moves, using the combined output at that moment.
//!   Enough shifts under a low combination drive the whole register to 0,
//!   a fixed point of the LFSR: noise then stays silent until TEST reloads
//!   the ones.
//!
//! The 8580 and 6581 LFSRs are the same circuit. The chips differ in the
//! analog path, not here.

pub const NOISE_SEED: u32 = 0x7F_FFF8;
pub const NOISE_TEST_VALUE: u32 = 0x7F_FFFF;
const NOISE_MASK: u32 = 0x7F_FFFF;

/// Register bit -> DAC bit, most significant first.
const TAPS: [(u32, u32); 8] = [
    (20, 11),
    (18, 10),
    (14, 9),
    (11, 8),
    (9, 7),
    (5, 6),
    (2, 5),
    (0, 4),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Noise {
    reg: u32,
}

impl Default for Noise {
    fn default() -> Self {
        Noise { reg: NOISE_SEED }
    }
}

impl Noise {
    /// Current 23-bit register value.
    #[inline]
    pub fn register(&self) -> u32 {
        self.reg
    }

    /// Shift once (a rising edge of accumulator bit 19).
    #[inline]
    pub fn shift(&mut self) {
        let fb = ((self.reg >> 22) ^ (self.reg >> 17)) & 1;
        self.reg = ((self.reg << 1) | fb) & NOISE_MASK;
    }

    /// 12-bit waveform output built from the eight output taps.
    #[inline]
    pub fn output(&self) -> u16 {
        TAPS.iter().fold(0u16, |o, &(from, to)| {
            o | ((((self.reg >> from) & 1) as u16) << to)
        })
    }

    /// Clear every tap cell whose DAC line the combined waveform pulls low.
    #[inline]
    pub fn write_back(&mut self, combined: u16) {
        for &(from, to) in &TAPS {
            if (combined >> to) & 1 == 0 {
                self.reg &= !(1 << from);
            }
        }
    }

    /// Hold the register at its TEST value.
    #[inline]
    pub fn test_reset(&mut self) {
        self.reg = NOISE_TEST_VALUE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_output_and_first_shift() {
        let mut n = Noise::default();
        // 0x7FFFF8: bits 0..2 clear, 3..22 set -> taps 20,18,14,11,9,5 set,
        // taps 2,0 clear -> 0xFC0.
        assert_eq!(n.output(), 0xFC0);
        // bit22 ^ bit17 = 1 ^ 1 = 0: shifted in 0, top bit falls off.
        n.shift();
        assert_eq!(n.register(), 0x7F_FFF0);
    }

    #[test]
    fn low_nibble_is_always_zero_and_output_is_roughly_balanced() {
        let mut n = Noise::default();
        let mut ones = 0u32;
        let count = 100_000;
        for _ in 0..count {
            n.shift();
            let o = n.output();
            assert_eq!(o & 0xF, 0);
            ones += (o >> 11) as u32;
        }
        let frac = ones as f64 / count as f64;
        assert!((frac - 0.5).abs() < 0.02, "{frac}");
    }

    #[test]
    fn never_reaches_the_all_zero_state_on_its_own() {
        let mut n = Noise::default();
        for _ in 0..1_000_000 {
            n.shift();
            assert_ne!(n.register(), 0);
        }
    }

    #[test]
    fn test_value_outputs_all_taps() {
        let mut n = Noise::default();
        n.test_reset();
        assert_eq!(n.register(), 0x7F_FFFF);
        assert_eq!(n.output(), 0xFF0);
    }

    #[test]
    fn write_back_clears_exactly_the_low_taps() {
        // Combined output 0x0F0 keeps DAC bits 7..4 (taps 9, 5, 2, 0) and
        // pulls DAC bits 11..8 (taps 20, 18, 14, 11) low:
        // 0x7FFFFF - 0x100000 - 0x040000 - 0x004000 - 0x000800
        //   = 0x6FFFFF - 0x040000 = 0x6BFFFF - 0x4000 = 0x6BBFFF - 0x800
        //   = 0x6BB7FF.
        let mut n = Noise::default();
        n.test_reset();
        n.write_back(0x0F0);
        assert_eq!(n.register(), 0x6B_B7FF);
        assert_eq!(n.output(), 0x0F0);
    }
}
