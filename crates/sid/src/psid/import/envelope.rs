//! The SID's envelope generator, stepped a frame at a time, for measuring how
//! loud each voice is: a register trace says what a player wrote, not what the
//! chip made of it. The chip's ADSR delay bug in particular (a gate-on whose
//! rate counter has run past the attack's period waits for the 15-bit counter
//! to wrap, up to 33 ms) can hold a note silent that the registers say is
//! playing. A port of `psid/envelope.ts`: the same model as the app's chip
//! (the measured rate periods, a 15-bit rate counter compared for equality,
//! linear attack, the piecewise-exponential divider in decay and release, the
//! sustain hold and the zero-freeze latch). Here it runs step by step rather
//! than cycle by cycle.

/// Chip cycles per envelope step, by rate nibble (the measured periods).
pub const SID_RATE_PERIODS: [i64; 16] = [
    9, 32, 63, 95, 149, 220, 267, 313, 392, 977, 1954, 3126, 3907, 11720, 19532, 31251,
];

fn exp_period(level: i64) -> i64 {
    if level >= 94 {
        1
    } else if level >= 55 {
        2
    } else if level >= 27 {
        4
    } else if level >= 15 {
        8
    } else if level >= 7 {
        16
    } else if level >= 1 {
        30
    } else {
        1
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Attack,
    Decay,
    Release,
}

#[derive(Clone, Debug)]
pub struct SidEnvelope {
    pub level: i64,
    stage: Stage,
    counter: i64,
    exp: i64,
    gate: bool,
    /// The zero freeze: set by a step that lands on 0, cleared only by a gate-on.
    hold_zero: bool,
    ad: u8,
    sr: u8,
}

impl Default for SidEnvelope {
    fn default() -> Self {
        SidEnvelope {
            level: 0,
            stage: Stage::Release,
            counter: 0,
            exp: 0,
            gate: false,
            hold_zero: true,
            ad: 0,
            sr: 0,
        }
    }
}

impl SidEnvelope {
    pub fn new() -> SidEnvelope {
        SidEnvelope::default()
    }

    pub fn set_adsr(&mut self, ad: u8, sr: u8) {
        self.ad = ad;
        self.sr = sr;
    }

    pub fn set_gate(&mut self, on: bool) {
        if on && !self.gate {
            self.stage = Stage::Attack;
            self.hold_zero = false;
        } else if !on && self.gate {
            self.stage = Stage::Release;
        }
        self.gate = on;
    }

    fn period(&self) -> i64 {
        let nibble = match self.stage {
            Stage::Attack => self.ad >> 4,
            Stage::Decay => self.ad & 0x0f,
            Stage::Release => self.sr & 0x0f,
        };
        SID_RATE_PERIODS[nibble as usize]
    }

    /// Nothing a step does changes the level: frozen at zero, or decay at the sustain level.
    fn holding(&self) -> bool {
        if self.hold_zero {
            return true;
        }
        self.stage == Stage::Decay && self.level == (self.sr >> 4) as i64 * 17
    }

    /// Run `cycles` chip cycles.
    pub fn run(&mut self, cycles: i64) {
        let mut left = cycles;
        while left > 0 {
            let period = self.period();
            // The counter counts up and steps when it equals the period; past it, it wraps at $8000 first.
            let until = if self.counter < period {
                period - self.counter
            } else {
                0x8000 - self.counter + period
            };
            if until > left {
                self.counter = (self.counter + left) & 0x7fff;
                return;
            }
            left -= until;
            self.counter = 0;
            self.step();
            if self.holding() {
                // Every further step until the next write does nothing but keep the counters turning.
                let steps = left / period;
                self.exp = (self.exp + steps) % exp_period(self.level);
                self.counter = left % period;
                return;
            }
        }
    }

    fn step(&mut self) {
        if self.stage != Stage::Attack {
            self.exp += 1;
            if self.exp < exp_period(self.level) {
                return;
            }
        }
        self.exp = 0;
        if self.hold_zero {
            return;
        }
        if self.stage == Stage::Attack {
            self.level = (self.level + 1) & 0xff;
            if self.level == 255 {
                self.stage = Stage::Decay;
            }
        } else if self.stage == Stage::Release || self.level != (self.sr >> 4) as i64 * 17 {
            // An unfrozen 0 (a gate-on with no attack step yet) wraps to 255.
            self.level = (self.level - 1) & 0xff;
        }
        if self.level == 0 {
            self.hold_zero = true;
        }
    }
}

/// Per frame, one voice's register writes as the trace has them: attack/decay, sustain/release, control, and whether the gate went low inside the frame.
#[derive(Clone, Copy, Debug)]
pub struct EnvelopeFrame {
    pub ad: u8,
    pub sr: u8,
    pub ctrl: u8,
    /// The gate went low inside the frame (a retrigger, when it ends high).
    pub gate_low: bool,
}

/// The envelope level at the end of each frame of `frames`, each frame's
/// writes applied at its start (attack/decay and sustain/release, then the
/// gate), `cycles_per_frame` apart.
pub fn envelope_levels(frames: &[EnvelopeFrame], cycles_per_frame: i64) -> Vec<u8> {
    let mut env = SidEnvelope::new();
    frames
        .iter()
        .map(|f| {
            apply_envelope_frame(&mut env, f, cycles_per_frame);
            env.level as u8
        })
        .collect()
}

/// One frame of `f`'s writes on `env`, then the frame's cycles.
pub fn apply_envelope_frame(env: &mut SidEnvelope, f: &EnvelopeFrame, cycles_per_frame: i64) {
    env.set_adsr(f.ad, f.sr);
    if f.gate_low {
        env.set_gate(false);
    }
    env.set_gate(f.ctrl & 1 != 0);
    env.run(cycles_per_frame);
}
