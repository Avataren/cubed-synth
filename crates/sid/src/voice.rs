//! One SID voice: 24-bit phase accumulator, waveform latch, noise LFSR,
//! envelope and the waveform x envelope DAC product.
//!
//! The voice is driven by `Chip`, which owns the per-cycle ordering because
//! sync and ring mod couple voices together (see `chip.rs`). Everything a
//! caller can observe is exposed read-only here. Writes go through the
//! chip's register interface.
//!
//! Sources (GPL-free): MOS 6581/8580 datasheet (frequency/pulse-width
//! registers, control bits, "Fout = Fn * Fclk / 2^24"); public hardware notes.
//!
//! - Accumulator: `acc = (acc + freq) mod 2^24` each cycle. Exact.
//! - TEST bit: the accumulator is reset to 0 and held there while TEST is
//!   set (datasheet: "resets and locks Oscillator 1 at zero until the TEST
//!   bit is cleared"). Exact. For its effect on noise and pulse see
//!   `noise.rs` and `waveform.rs`.
//! - DAC (8580): linear, zero point at the waveform midpoint, so the voice
//!   output is `(wave - 0x800) / 0x800 * env / 255`. INFERRED from the
//!   8580 being documented as having near-linear DACs and none of the
//!   6581's large per-voice DC offset.
//!
//! 6581 (S2), all gated on `SidModel::Sid6581`:
//! - DAC DC offset. Public knowledge: the 6581's waveform DAC is not
//!   centred on its zero point, so every voice carries a DC level that the
//!   envelope scales. That is why 6581 gate edges thump and why
//!   envelope/volume "digis" are loud on a 6581. Modelled as a constant
//!   added to the normalised waveform before the envelope product:
//!     out = ((wave - 0x800) / 0x800 + VOICE_DC_6581) * amp
//!   INFERRED: the sign (positive) follows the public "zero point below
//!   mid-scale" description; the size 0.25 (a quarter of half-scale) is a
//!   tuning guess. Behind the chip's 16 Hz DC blocker this shows up as a
//!   thump on note-on/off and on envelope moves, never as a steady offset.
//!   Ears-gate.
//! - Attack shape. The plan (§1.2) cites a published 6581 measurement: a
//!   ~1.5 ms floor and a nonlinear attack shape. No GPL-free copy of that
//!   curve was available here, so the plan's figure is taken at face value
//!   and turned into a model: the envelope counter and the shared rate
//!   table are untouched (ENV3 still reads the linear digital level), but
//!   the amplitude the voice applies, `amp`, follows the level through a
//!   one-pole lag that acts on RISING level only:
//!     target = level / 255
//!     amp <- amp + (target - amp) * alpha   if target > amp
//!     amp <- target                         otherwise
//!     alpha = 1 - e^(-1 / tau),  tau = 1.5 ms / ln 9 * 985 248 = 672.61 cycles
//!   ln 9 makes the 10 %-90 % rise of a full-scale step exactly 1.5 ms: that
//!   is the "floor", since no attack can rise faster. A ramp of length T
//!   comes out as (t - tau (1 - e^(-t/tau))) / T: a slow, curved onset,
//!   then a straight line one tau late. At the fastest nibble (T = 255 * 9
//!   cycles = 2.33 ms) the whole attack is curved; slow attacks are just
//!   delayed by tau. Decay and release follow the level exactly, and so
//!   does any fall after the lag has caught up (no step at the stage change).
//!   Why a lag and not a stateless level -> amplitude table (the plan's
//!   "table lookup"): a table applied only in attack jumps when the stage
//!   changes mid-attack (gate off at level L would snap from table(L) to
//!   L/255), and a table applied in every stage changes every sustain level.
//!   INFERRED: the one-pole form and the 10-90 % reading of "1.5 ms floor".
//!   Ears-gate.
//! - Waveform 0 fade. With no waveform selected the DAC input floats (see
//!   `waveform.rs`). Public notes say the held value leaks away, far faster
//!   on the 6581 than on the 8580. Modelled as: after WAVE0_FADE_CYCLES_6581
//!   consecutive cycles with no waveform, the held value drops to 0. TEST
//!   does not drive the DAC and neither restarts nor stops the count. The
//!   8580 keeps S1's hold. INFERRED: the hold time (65 536 cycles,
//!   66.5 ms) and the single drop instead of a gradual leak. Ears-gate.

use super::envelope::{Envelope, Stage};
use super::noise::Noise;
use super::revision::R4AR;
use super::waveform::{waveform_output, GATE, NOISE, TEST};
use super::{SidModel, ACC_MASK, PAL_CLOCK_HZ};

const MSB: u32 = 0x80_0000;
const NOISE_CLOCK_BIT: u32 = 0x08_0000; // accumulator bit 19

/// 6581 waveform-DAC DC offset in normalised units (half-scale = 1), R4AR's
/// (the S2 reference). INFERRED tuning. A chip plays its own profile's
/// (`Voice::with_dc`, S5.16): the GT-reference profile's is 0.5625.
pub const VOICE_DC_6581: f64 = R4AR.voice_dc;
/// The 6581 attack floor: 10 %-90 % rise of a full-scale step, seconds.
/// The plan's cited figure (§1.2), taken at face value.
pub const ATTACK_FLOOR_6581_S: f64 = 1.5e-3;
/// Cycles of waveform 0 before a 6581's held DAC value drops to 0.
/// INFERRED tuning.
pub const WAVE0_FADE_CYCLES_6581: u32 = 65_536;

/// The 6581 attack lag's time constant in chip cycles:
/// ATTACK_FLOOR_6581_S / ln 9 * PAL clock.
pub fn attack_lag_tau_cycles() -> f64 {
    ATTACK_FLOOR_6581_S / 9f64.ln() * PAL_CLOCK_HZ
}

/// `level as f64 / 255.0` for every envelope level: the per-cycle amplitude
/// read as a table, the same values without a divide per voice per cycle
/// (the render loop's hottest line before; `benches/sid_render.rs`).
static LEVEL_AMP: [f64; 256] = {
    let mut t = [0.0; 256];
    let mut level = 0;
    while level < 256 {
        t[level] = level as f64 / 255.0;
        level += 1;
    }
    t
};

#[derive(Debug, Clone, Copy)]
pub struct Voice {
    model: SidModel,
    acc: u32,
    freq: u16,
    pw: u16,
    control: u8,
    noise: Noise,
    env: Envelope,
    /// The 12-bit waveform DAC input (held when no waveform is selected).
    wave: u16,
    /// Accumulator value at the start of the current cycle.
    prev_acc: u32,
    /// The MSB went 0 -> 1 during the current cycle.
    msb_rising: bool,
    /// 6581: the lagged envelope amplitude, 0..1. Unused on the 8580.
    amp: f64,
    /// 6581: per-cycle coefficient of the attack lag.
    lag_alpha: f64,
    /// 6581: consecutive cycles with no waveform selected (saturates at
    /// the fade time). Unused on the 8580.
    wave0_age: u32,
    /// 6581: the waveform DAC's DC offset, envelope-scaled. Unused on the 8580.
    dc: f64,
}

impl Default for Voice {
    fn default() -> Self {
        Voice::new(SidModel::Sid8580)
    }
}

impl Voice {
    /// A powered-on voice of `model`.
    pub fn new(model: SidModel) -> Self {
        Voice::with_dc(model, VOICE_DC_6581)
    }

    /// A powered-on voice of `model` whose 6581 DAC offset is `dc`.
    pub fn with_dc(model: SidModel, dc: f64) -> Self {
        Voice {
            model,
            acc: 0,
            freq: 0,
            pw: 0,
            control: 0,
            noise: Noise::default(),
            env: Envelope::default(),
            wave: 0,
            prev_acc: 0,
            msb_rising: false,
            amp: 0.0,
            lag_alpha: 1.0 - (-1.0 / attack_lag_tau_cycles()).exp(),
            wave0_age: 0,
            dc,
        }
    }

    /// Set the 6581 DAC offset (`with_dc`), e.g. on a revision switch.
    pub fn set_dc(&mut self, dc: f64) {
        self.dc = dc;
    }

    pub fn model(&self) -> SidModel {
        self.model
    }

    /// Become a voice of `model` (6581 DAC offset `dc`) while playing: the
    /// accumulator, noise register, envelope and waveform state carry on; only
    /// the model's own traits change from the next cycle.
    pub fn set_model(&mut self, model: SidModel, dc: f64) {
        self.model = model;
        self.dc = dc;
    }

    /// 24-bit phase accumulator.
    #[inline]
    pub fn accumulator(&self) -> u32 {
        self.acc
    }

    #[inline]
    pub fn frequency(&self) -> u16 {
        self.freq
    }

    /// 12-bit pulse width.
    #[inline]
    pub fn pulse_width(&self) -> u16 {
        self.pw
    }

    #[inline]
    pub fn control(&self) -> u8 {
        self.control
    }

    /// The 12-bit value on the waveform DAC input (what OSC3 reads the top
    /// 8 bits of for voice 3).
    #[inline]
    pub fn waveform(&self) -> u16 {
        self.wave
    }

    #[inline]
    pub fn envelope_level(&self) -> u8 {
        self.env.level()
    }

    #[inline]
    pub fn envelope_stage(&self) -> Stage {
        self.env.stage()
    }

    /// 15-bit envelope rate counter.
    #[inline]
    pub fn envelope_rate_counter(&self) -> u16 {
        self.env.rate_counter()
    }

    /// 23-bit noise LFSR.
    #[inline]
    pub fn noise_register(&self) -> u32 {
        self.noise.register()
    }

    /// The amplitude the envelope applies to the waveform, 0..1: level/255
    /// on the 8580, the attack-lagged level on the 6581 (header).
    #[inline]
    pub fn envelope_amplitude(&self) -> f64 {
        match self.model {
            SidModel::Sid8580 => LEVEL_AMP[self.env.level() as usize],
            SidModel::Sid6581 => self.amp,
        }
    }

    /// Envelope-scaled output, normalised to about -1..1 (8580) or
    /// -0.75..1.25 (6581, with its DAC DC offset).
    #[inline]
    pub fn output(&self) -> f64 {
        match self.model {
            SidModel::Sid8580 => {
                (self.wave as f64 - 2048.0) / 2048.0 * (LEVEL_AMP[self.env.level() as usize])
            }
            SidModel::Sid6581 => ((self.wave as f64 - 2048.0) / 2048.0 + self.dc) * self.amp,
        }
    }

    pub(super) fn set_freq_lo(&mut self, v: u8) {
        self.freq = (self.freq & 0xFF00) | v as u16;
    }

    pub(super) fn set_freq_hi(&mut self, v: u8) {
        self.freq = (self.freq & 0x00FF) | ((v as u16) << 8);
    }

    pub(super) fn set_pw_lo(&mut self, v: u8) {
        self.pw = (self.pw & 0x0F00) | v as u16;
    }

    pub(super) fn set_pw_hi(&mut self, v: u8) {
        self.pw = (self.pw & 0x00FF) | (((v & 0x0F) as u16) << 8);
    }

    pub(super) fn set_ad(&mut self, v: u8) {
        self.env.set_ad(v);
    }

    pub(super) fn set_sr(&mut self, v: u8) {
        self.env.set_sr(v);
    }

    /// Control register write. Like every register write here, it lands
    /// between chip cycles; TEST's resets happen on the next cycle.
    pub(super) fn set_control(&mut self, v: u8) {
        self.control = v;
        self.env.set_gate(v & GATE != 0);
    }

    /// Cycle phase 1: advance the accumulator (or hold it at 0 under TEST)
    /// and latch whether the MSB rose.
    #[inline]
    pub(super) fn clock_accumulator(&mut self) {
        self.prev_acc = self.acc;
        self.acc = if self.control & TEST != 0 {
            0
        } else {
            (self.acc + self.freq as u32) & ACC_MASK
        };
        self.msb_rising = self.prev_acc & MSB == 0 && self.acc & MSB != 0;
    }

    #[inline]
    pub(super) fn msb_rising(&self) -> bool {
        self.msb_rising
    }

    #[inline]
    pub(super) fn has_control(&self, bit: u8) -> bool {
        self.control & bit != 0
    }

    /// Cycle phase 2 (sync): reset the accumulator to 0. The cycle's own
    /// increment is discarded, and a reset accumulator has no MSB rise.
    #[inline]
    pub(super) fn sync_reset(&mut self) {
        self.acc = 0;
        self.msb_rising = false;
    }

    /// Cycle phase 3: noise clock, envelope, waveform latch. `source_acc` is
    /// the ring-mod source's accumulator after phases 1-2.
    #[inline]
    pub(super) fn finish_cycle(&mut self, source_acc: u32) {
        if self.control & TEST != 0 {
            self.noise.test_reset();
        } else if self.prev_acc & NOISE_CLOCK_BIT == 0 && self.acc & NOISE_CLOCK_BIT != 0 {
            if self.control & NOISE != 0 && self.control & 0x70 != 0 {
                if let Some(w) = self.select(source_acc) {
                    self.noise.write_back(w);
                }
            }
            self.noise.shift();
        }
        self.env.clock();
        match self.select(source_acc) {
            Some(w) => {
                self.wave = w;
                self.wave0_age = 0;
            }
            None if self.model == SidModel::Sid6581 && self.wave0_age < WAVE0_FADE_CYCLES_6581 => {
                self.wave0_age += 1;
                if self.wave0_age == WAVE0_FADE_CYCLES_6581 {
                    self.wave = 0;
                }
            }
            None => {}
        }
        if self.model == SidModel::Sid6581 {
            let target = LEVEL_AMP[self.env.level() as usize];
            self.amp = if target > self.amp {
                self.amp + (target - self.amp) * self.lag_alpha
            } else {
                target
            };
        }
    }

    #[inline]
    fn select(&self, source_acc: u32) -> Option<u16> {
        // The noise taps only matter when noise is selected; skip gathering them.
        let noise = if self.control & NOISE != 0 {
            self.noise.output()
        } else {
            0
        };
        waveform_output(
            self.model,
            self.control,
            self.acc,
            source_acc,
            self.pw,
            noise,
        )
    }
}
