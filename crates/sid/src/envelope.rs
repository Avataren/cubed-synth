//! SID envelope generator: an 8-bit up/down envelope counter clocked by a
//! 15-bit rate counter, with an extra "exponential" divider in decay and
//! release. Cycle-counted at the chip clock.
//!
//! Sources (GPL-free): MOS 6581/8580 datasheet "Envelope Rates" table;
//! published hardware measurements of the rate periods and exponential
//! breakpoints by the C64 community; public descriptions of the "ADSR delay
//! bug".
//!
//! - Attack/decay/release times: the datasheet gives attack 2 ms .. 8 s,
//!   with decay/release 3x the attack time for the same nibble, at 1 MHz. The
//!   nominal rate period is `time * 1e6 / 256` cycles. The table holds the
//!   MEASURED periods, which sit 0..1.6 cycles above nominal because the
//!   datasheet rounds its times. Derivation, nibble: datasheet ms ->
//!   ms*1000/256 -> measured period
//!     0:     2 ->     7.8 ->     9      8:   100 ->   390.6 ->   392
//!     1:     8 ->    31.3 ->    32      9:   250 ->   976.6 ->   977
//!     2:    16 ->    62.5 ->    63     10:   500 ->  1953.1 ->  1954
//!     3:    24 ->    93.8 ->    95     11:   800 ->  3125.0 ->  3126
//!     4:    38 ->   148.4 ->   149     12:  1000 ->  3906.3 ->  3907
//!     5:    56 ->   218.8 ->   220     13:  3000 -> 11718.8 -> 11720
//!     6:    68 ->   265.6 ->   267     14:  5000 -> 19531.3 -> 19532
//!     7:    80 ->   312.5 ->   313     15:  8000 -> 31250.0 -> 31251
//!   The 8580 and 6581 share the envelope circuit, so this one table serves
//!   both models, and this counter is identical on both (ENV3 reads it).
//!   The 6581's attack-curve shape (S2, plan §1.2) lives after the counter,
//!   in the amplitude the voice applies: see `voice.rs`.
//! - Rate counter: 15 bits, +1 per cycle, free-running across gate changes.
//!   The step fires when it EQUALS the selected period, and the counter
//!   then resets to 0. INFERRED from public descriptions: an equality
//!   compare is what produces the documented ADSR delay bug. When the period
//!   is lowered below the counter's current value, the counter has to run
//!   on to 0x7FFF, wrap, and count up again (up to ~33 ms) before the next
//!   step.
//! - Attack is linear: +1 per rate period to 255, then decay starts.
//! - Decay/release are piecewise exponential: the counter steps down once
//!   every N rate periods, N chosen from the current level. Measured
//!   breakpoints: level > 93 -> 1, 55..=93 -> 2, 27..=54 -> 4, 15..=26 -> 8,
//!   7..=14 -> 16, 1..=6 -> 30.
//! - Sustain level = the S nibble in both nibbles (S * 17). INFERRED
//!   (public descriptions of the comparator): decay stops when the level
//!   EQUALS the sustain value. Lowering S mid-sustain resumes the decay. Raising
//!   it above the current level never matches, so the decay runs on to 0.
//! - Zero freeze: a step that lands on 0 freezes the counter there, and
//!   only a gate-on (attack) unlocks it. The freeze is a latch, not "level
//!   is 0": a gate-on clears it even if no attack step follows, and a
//!   release step from an unfrozen 0 wraps the 8-bit counter to 255, which
//!   then releases from full level. The same wrap runs the other way: an
//!   attack step from 255 lands on 0 and freezes. reSID models both, as
//!   measured on ENV3. Songs lean on the first: a one-frame gate-on whose
//!   attack step the ADSR delay bug holds back (Stinsen's "Tribal
//!   Tribunal", voices 2 and 3) plays its whole release, and holding at 0
//!   instead silenced those voices.
//! - The TEST bit does not touch the envelope. The datasheet's TEST text
//!   names only the oscillator, noise and pulse outputs.
//!
//! Not modelled: the one-cycle pipeline delays on gate and register writes
//! (writes land between chip cycles here).

/// Measured rate-counter periods in chip cycles, indexed by nibble.
pub const RATE_PERIODS: [u16; 16] = [
    9, 32, 63, 95, 149, 220, 267, 313, 392, 977, 1954, 3126, 3907, 11720, 19532, 31251,
];

/// Datasheet attack times (ms) at 1 MHz, used only to cross-check the table.
pub const DATASHEET_ATTACK_MS: [u32; 16] = [
    2, 8, 16, 24, 38, 56, 68, 80, 100, 250, 500, 800, 1000, 3000, 5000, 8000,
];

const RATE_COUNTER_MASK: u16 = 0x7FFF;

/// Exponential divider for decay/release at a given envelope level.
#[inline]
pub fn exp_period(level: u8) -> u8 {
    match level {
        94..=255 => 1,
        55..=93 => 2,
        27..=54 => 4,
        15..=26 => 8,
        7..=14 => 16,
        1..=6 => 30,
        0 => 1,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Attack,
    DecaySustain,
    Release,
}

#[derive(Debug, Clone, Copy)]
pub struct Envelope {
    attack: u8,
    decay: u8,
    sustain: u8,
    release: u8,
    stage: Stage,
    level: u8,
    rate_counter: u16,
    exp_counter: u8,
    gate: bool,
    hold_zero: bool,
}

impl Default for Envelope {
    fn default() -> Self {
        Envelope {
            attack: 0,
            decay: 0,
            sustain: 0,
            release: 0,
            stage: Stage::Release,
            level: 0,
            rate_counter: 0,
            exp_counter: 0,
            gate: false,
            hold_zero: true,
        }
    }
}

impl Envelope {
    #[inline]
    pub fn level(&self) -> u8 {
        self.level
    }

    #[inline]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// Current 15-bit rate counter value.
    #[inline]
    pub fn rate_counter(&self) -> u16 {
        self.rate_counter
    }

    /// Write the AD register: attack high nibble, decay low nibble.
    pub fn set_ad(&mut self, ad: u8) {
        self.attack = ad >> 4;
        self.decay = ad & 0xF;
    }

    /// Write the SR register: sustain high nibble, release low nibble.
    pub fn set_sr(&mut self, sr: u8) {
        self.sustain = sr >> 4;
        self.release = sr & 0xF;
    }

    /// Gate bit of the control register. A rising edge starts attack from the
    /// current level (no reset to zero); a falling edge starts release.
    pub fn set_gate(&mut self, gate: bool) {
        if gate && !self.gate {
            self.stage = Stage::Attack;
            self.hold_zero = false;
        } else if !gate && self.gate {
            self.stage = Stage::Release;
        }
        self.gate = gate;
    }

    #[inline]
    fn period(&self) -> u16 {
        let nibble = match self.stage {
            Stage::Attack => self.attack,
            Stage::DecaySustain => self.decay,
            Stage::Release => self.release,
        };
        RATE_PERIODS[nibble as usize]
    }

    /// Advance one chip cycle.
    #[inline]
    pub fn clock(&mut self) {
        self.rate_counter = (self.rate_counter + 1) & RATE_COUNTER_MASK;
        if self.rate_counter != self.period() {
            return;
        }
        self.rate_counter = 0;
        if self.stage != Stage::Attack {
            self.exp_counter += 1;
            if self.exp_counter < exp_period(self.level) {
                return;
            }
        }
        self.exp_counter = 0;
        if self.hold_zero {
            return;
        }
        match self.stage {
            Stage::Attack => {
                self.level = self.level.wrapping_add(1);
                if self.level == 255 {
                    self.stage = Stage::DecaySustain;
                }
            }
            Stage::DecaySustain => {
                if self.level != self.sustain * 17 {
                    self.level = self.level.wrapping_sub(1);
                }
            }
            Stage::Release => self.level = self.level.wrapping_sub(1),
        }
        if self.level == 0 {
            self.hold_zero = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_periods_match_datasheet() {
        // Measured period vs nominal datasheet ms * 1e6 / 256: never below,
        // and within 1.6 cycles above (the datasheet rounds 7.8 up to "2 ms").
        for i in 0..16 {
            let nominal = DATASHEET_ATTACK_MS[i] as f64 * 1000.0 / 256.0;
            let d = RATE_PERIODS[i] as f64 - nominal;
            assert!((0.0..1.6).contains(&d), "nibble {i}: {d}");
        }
    }

    #[test]
    fn gate_on_without_an_attack_step_releases_from_full() {
        // Release rate 3 leaves the counter above attack rate 0's period, so the
        // gate-on waits on the ADSR delay bug. The gate drops before any attack
        // step: the release step from the unfrozen 0 wraps to 255 (reSID).
        let mut env = Envelope::default();
        env.set_sr(0x03);
        for _ in 0..50 {
            env.clock();
        }
        env.set_ad(0x00);
        env.set_sr(0x0A);
        env.set_gate(true);
        for _ in 0..100 {
            env.clock();
        }
        assert_eq!(
            env.level(),
            0,
            "the attack is still waiting on the counter's wrap"
        );
        env.set_gate(false);
        for _ in 0..0x8000 {
            env.clock();
        }
        assert!(env.level() > 200, "released from 255, got {}", env.level());

        // A decay that lands on 0 freezes: the gate-off after it stays silent.
        let mut env = Envelope::default();
        env.set_gate(true);
        for _ in 0..20_000 {
            env.clock();
        }
        assert_eq!(env.level(), 0, "attack 0, decay 0 to sustain 0");
        env.set_gate(false);
        for _ in 0..0x8000 {
            env.clock();
        }
        assert_eq!(env.level(), 0);
    }

    #[test]
    fn exp_period_breakpoints() {
        let want = [
            (255u8, 1u8),
            (94, 1),
            (93, 2),
            (55, 2),
            (54, 4),
            (27, 4),
            (26, 8),
            (15, 8),
            (14, 16),
            (7, 16),
            (6, 30),
            (1, 30),
        ];
        for (level, n) in want {
            assert_eq!(exp_period(level), n, "level {level}");
        }
    }
}
