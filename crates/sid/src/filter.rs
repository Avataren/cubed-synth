//! 8580 filter: a zero-delay-feedback (topology-preserving transform)
//! state-variable filter at the output sample rate. 2 poles, 12 dB/oct LP/HP,
//! 6 dB/oct BP; the three taps are summed per the mode bits ($D418 bits 4-6).
//!
//! Sources (GPL-free): MOS 6581/8580 datasheet (12 dB/oct LP/HP, 6 dB/oct BP
//! from one filter, 30 Hz..12 kHz cutoff range, 4-bit resonance, summable
//! modes); public descriptions of the 8580 filter as near-linear with
//! clean, moderate, non-self-oscillating resonance.
//!
//! Why an SVF (kept from the accepted S0 spike): the SID filter is a
//! two-integrator-loop state-variable filter. Its LP/BP/HP taps and
//! resonance-by-damping are the datasheet's modes exactly, and summing the
//! taps gives the notch (LP+HP) and the other combinations for free. TPT is
//! stable for any cutoff below Nyquist and keeps the resonance peak in
//! place at high cutoffs, which a naive Chamberlin SVF does not. On the
//! plan's "3-pole" wording, see `.ai/sid-voice-core-verdict.md`: the
//! datasheet's slopes fix 2 poles, and no third pole is added.
//!
//! Register maps (8580): both are INFERRED tuning choices, not
//! measurements. They are carried unchanged from S0 and are ears-gate items.
//! - Cutoff: 11-bit register (FC_LO bits 0-2, FC_HI bits 0-7). The 8580 curve
//!   is documented as near-linear over the datasheet range, so
//!   fc = 30 + reg * (12000 - 30) / 2047 Hz (5.85 Hz per step).
//! - Resonance: Q = 0.707 + res / 15. Butterworth at 0, Q 1.707 (+4.6 dB)
//!   at 15: GoatTracker's reSID map (filter.cpp:269-278, one map for both
//!   models; `.ai/sid-chip-comparison-report.md` 6.6). S0's inferred
//!   0.707 * 2^(res / 8) reached Q 2.59 and ran resonant 8580 songs ~3.6 dB
//!   hot: plasma_intro's res-15 HP+BP bass attacks peaked at 0.87 with
//!   1.3x GT's RMS, into the app's limiter; with this map they match GT's
//!   RMS within 2 %.
//! - Ceiling (S5.12): `Filter::set` clamps the mapped cutoff to 4 kHz, the
//!   limit GoatTracker 2's reSID playback applies (`CUTOFF_CEILING_DELTA_HZ`),
//!   on the 8580 and on the 6581 profiles that follow GT (`GT_REF`, `R4AR`).
//!   The measured chip profiles (`R3`, `R4`) have none: the 6581 ceiling is
//!   the profile's `cutoff_ceiling_hz`. The maps themselves stay unclamped.
//!
//! Linear: no saturation. The 8580 is "clean", not perfectly so.
//!
//! 6581 (S2, plan §1.2 "static nonlinear remap of the cutoff register +
//! level-dependent peak gain"). Same SVF, same taps, same mode bits; three
//! things differ, all gated on `SidModel::Sid6581`:
//! - Cutoff map (S5.12 R2; S2's logistic curve replaced). Public knowledge
//!   (C64 community measurements of many chips): the 6581 cutoff is
//!   strongly nonlinear in the register. The bottom of the range barely
//!   moves and the floor sits in the low hundreds of Hz rather than at the
//!   datasheet's 30 Hz. It also varies a lot from chip to chip. The
//!   published nominal curve has two pieces: FC_HI bit 7 has its own DAC
//!   weight, so the curve steps DOWN from ~6 kHz at 0x3FF to ~4.6 kHz at
//!   0x400 and then rises again to 18 kHz. Modelled as log-linear
//!   interpolation through measured anchors, one anchor set per piece:
//!     0 220 Hz, 0x200 420, 0x300 1600, 0x3FF 6000 |
//!     0x400 4600, 0x500 9500, 0x600 14500, 0x7FF 18000
//!   Each piece is strictly monotonic. The 0x3FF -> 0x400 drop is the one
//!   deliberate non-monotonicity. The anchors are measured facts and the
//!   values between them are derived; see the DERIVED-VALUE disclosure at
//!   `CUTOFF_ANCHORS_6581_LO`. The old map was 2-3x too dark across FC_HI
//!   $50-$7F (report §6.3). Ears-gate.
//! - Resonance map: Q = 0.707 * 2^(res / 12) (Butterworth at 0, Q 1.68 =
//!   +4.5 dB at 15). INFERRED from the public description of the 6581's
//!   resonance as clearly weaker and rounder than the 8580's; the exponent
//!   is a tuning guess. Ears-gate.
//! - Level-dependent peak gain. The 6581 filter's integrators are not
//!   linear at the levels real mixes drive them to (public: the "gritty",
//!   distorting 6581 filter, strongest at high resonance and with several
//!   voices filtered). Modelled as a soft limit on the band-pass
//!   integrator state after each update:
//!     s1 <- SAT * tanh(s1 / SAT)
//!   Small signals are untouched (tanh(u) = u - u^3/3 + ...: the relative
//!   change is ~u^2/3, under 1e-3 for states below 0.05), so the
//!   small-signal peak is still Q.
//!   At full level the band-pass state is clamped below SAT, the resonant
//!   peak compresses and the output picks up odd harmonics. The band-pass
//!   state carries no DC, so the 6581's DC offsets (`voice.rs`) do not bias
//!   it. INFERRED: the placement (one limiter on the resonant state) and
//!   SAT = 1.0 (one full-scale voice) are tuning choices. Ears-gate.
//!   Not modelled: the real filter's input-dependent cutoff shift. The plan
//!   approximates it by the static remap, which is what this is.

use super::revision::{profile_6581, RevisionProfile};
use super::SidModel;
use std::f64::consts::PI;

/// Mode bits of $D418.
pub const LP: u8 = 0x10;
pub const BP: u8 = 0x20;
pub const HP: u8 = 0x40;

/// 8580 cutoff register -> Hz.
pub fn cutoff_hz(reg: u16) -> f64 {
    30.0 + (reg & 0x7FF) as f64 * (12_000.0 - 30.0) / 2047.0
}

/// 8580 resonance nibble -> Q: GoatTracker's reSID map (header).
pub fn resonance_q(res: u8) -> f64 {
    0.707 + (res & 0xF) as f64 / 15.0
}

/// 6581 cutoff floor (register 0), Hz.
pub const F_LO_6581: f64 = CUTOFF_ANCHORS_6581_LO[0].1;

/// 6581 cutoff anchors (register, Hz), low piece: registers 0..=0x3FF
/// (FC_HI bit 7 clear).
///
/// DERIVED-VALUE DISCLOSURE (S5.12 R2, both anchor sets).
/// - Measured facts: every anchor except the two ends is a figure quoted in
///   `.ai/sid-chip-comparison-report.md` §6.3 and the S5.12 brief, which
///   summarise the published 6581 cutoff measurements (Antti Lankila's
///   6581 filter / cutoff measurement write-ups, bel.fi/~ankila/, as cited
///   in the brief): ~420 Hz at 0x200, ~1.6 kHz at 0x300, ~6 kHz at 0x3FF,
///   a step DOWN to ~4.6 kHz at 0x400 (FC_HI $7F -> $80, where the top bit's
///   different DAC weight lands), ~9.5 kHz at 0x500, ~14.5 kHz at 0x600.
///   The ends, 220 Hz at 0 and 18 kHz at 0x7FF, are the published range of
///   that nominal curve (6581R4AR). The report states them as the range of
///   reSID's curve. They are also this map's previous endpoints.
/// - Derived: every value between anchors. It is log-linear (exponential
///   in the register) within each segment, so each piece is strictly
///   monotonic. reSID interpolates its measured points with a spline. That
///   spline and its point list (reSID filter.cpp) are GPL and were NOT used.
///   The anchors are rounded published figures, not reSID's points.
/// - Expected tolerance vs reSID's spline: within ±5% at the anchors, and
///   ±15% between them in the 1-10 kHz region. The log-linear chords sit
///   below a convex curve. Ears-gate.
///
/// S5.15: the anchors are revision data and live in the 6581 revision
/// profile (`revision::R4AR`, GENERIC-6581: S5.12's anchors unchanged).
pub const CUTOFF_ANCHORS_6581_LO: &[(u16, f64)] = profile_6581().cutoff_anchors_lo;
/// 6581 cutoff anchors, high piece: registers 0x400..=0x7FF (FC_HI bit 7
/// set). Disclosure at `CUTOFF_ANCHORS_6581_LO`.
pub const CUTOFF_ANCHORS_6581_HI: &[(u16, f64)] = profile_6581().cutoff_anchors_hi;

/// Log-linear interpolation through `anchors`. `reg` must lie within the
/// anchors' span (the first anchor to the last).
fn log_interp(anchors: &[(u16, f64)], reg: u16) -> f64 {
    let seg = anchors
        .windows(2)
        .find(|w| reg <= w[1].0)
        .expect("reg inside the anchor span");
    let ((r0, f0), (r1, f1)) = (seg[0], seg[1]);
    if reg == r1 {
        return f1;
    }
    let t = (reg - r0) as f64 / (r1 - r0) as f64;
    f0 * (f1 / f0).powf(t)
}

/// 6581 cutoff register -> Hz on the default profile (`profile_6581()`).
pub fn cutoff_hz_6581(reg: u16) -> f64 {
    cutoff_hz_6581_with(profile_6581(), reg)
}

/// 6581 cutoff register -> Hz on `profile`: its two-piece anchor curve
/// (header), log-linear inside each piece.
pub fn cutoff_hz_6581_with(profile: &RevisionProfile, reg: u16) -> f64 {
    let reg = reg & 0x7FF;
    if reg < 0x400 {
        log_interp(profile.cutoff_anchors_lo, reg)
    } else {
        log_interp(profile.cutoff_anchors_hi, reg)
    }
}

/// 6581 resonance nibble -> Q on the default profile.
pub fn resonance_q_6581(res: u8) -> f64 {
    resonance_q_6581_with(profile_6581(), res)
}

/// 6581 resonance nibble -> Q on `profile` (R4AR: exponential; GtRef: the
/// linear map fitted to GoatTracker's playback, see `revision::GT_REF`).
pub fn resonance_q_6581_with(profile: &RevisionProfile, res: u8) -> f64 {
    profile.resonance.q(res)
}

/// Effective cutoff ceiling, Hz, both models (S5.12). GoatTracker 2 plays
/// through classic reSID in its delta-clock mode (default SAMPLE_FAST,
/// gsound.c:216 -> gsid.cpp:77-80), where reSID limits the filter cutoff to
/// 4 kHz (reSID filter.cpp:259-265, applied via filter.h:461); GT2 songs
/// were mixed by ear against it. The value is that ceiling, a fact read from
/// the cited code's behaviour; no GPL text copied. The maps above stay
/// unclamped; only `Filter::set` applies it.
pub const CUTOFF_CEILING_DELTA_HZ: f64 = 4000.0;

/// Cutoff map of `model`.
pub fn cutoff_hz_for(model: SidModel, reg: u16) -> f64 {
    match model {
        SidModel::Sid8580 => cutoff_hz(reg),
        SidModel::Sid6581 => cutoff_hz_6581(reg),
    }
}

/// Resonance map of `model`.
pub fn resonance_q_for(model: SidModel, res: u8) -> f64 {
    match model {
        SidModel::Sid8580 => resonance_q(res),
        SidModel::Sid6581 => resonance_q_6581(res),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Filter {
    s1: f64,
    s2: f64,
    g: f64,
    k: f64,
    h: f64,
    fc: f64,
    cutoff_reg: u16,
    res: u8,
    mode: u8,
    sample_rate: f64,
    model: SidModel,
    /// The 6581 maps and soft limit this filter plays (ignored on the 8580).
    profile: &'static RevisionProfile,
}

impl Filter {
    /// An 8580 filter.
    pub fn new(sample_rate: f64) -> Self {
        Filter::with_model(SidModel::Sid8580, sample_rate)
    }

    /// A filter with `model`'s maps and (6581) its saturating resonance.
    pub fn with_model(model: SidModel, sample_rate: f64) -> Self {
        Filter::with_profile(model, profile_6581(), sample_rate)
    }

    /// A filter with `model`'s maps; a 6581 plays `profile`'s cutoff curve,
    /// resonance map and soft limit (an 8580 ignores it).
    pub fn with_profile(
        model: SidModel,
        profile: &'static RevisionProfile,
        sample_rate: f64,
    ) -> Self {
        let mut f = Filter {
            s1: 0.0,
            s2: 0.0,
            g: 0.0,
            k: 0.0,
            h: 0.0,
            fc: 0.0,
            cutoff_reg: 0,
            res: 0,
            mode: 0,
            sample_rate,
            model,
            profile,
        };
        f.set(0, 0);
        f
    }

    pub fn model(&self) -> SidModel {
        self.model
    }

    /// The current cutoff in Hz from this model's map (before the ceiling
    /// and the 0.49 * sample-rate clamp).
    pub fn cutoff(&self) -> f64 {
        match self.model {
            SidModel::Sid8580 => cutoff_hz(self.cutoff_reg),
            SidModel::Sid6581 => cutoff_hz_6581_with(self.profile, self.cutoff_reg),
        }
    }

    /// The cutoff in Hz the coefficients were computed for: the map, then
    /// the ceiling (`ceiling`), then 0.49 * sample rate.
    pub fn effective_cutoff(&self) -> f64 {
        self.fc
    }

    /// The highest cutoff this filter plays before the sample-rate clamp:
    /// `CUTOFF_CEILING_DELTA_HZ` on the 8580, the profile's on the 6581.
    pub fn ceiling(&self) -> f64 {
        match self.model {
            SidModel::Sid8580 => CUTOFF_CEILING_DELTA_HZ,
            SidModel::Sid6581 => self.profile.cutoff_ceiling_hz,
        }
    }

    /// The current Q from this model's resonance map.
    pub fn q(&self) -> f64 {
        match self.model {
            SidModel::Sid8580 => resonance_q(self.res),
            SidModel::Sid6581 => resonance_q_6581_with(self.profile, self.res),
        }
    }

    pub fn cutoff_reg(&self) -> u16 {
        self.cutoff_reg
    }

    pub fn resonance(&self) -> u8 {
        self.res
    }

    pub fn mode(&self) -> u8 {
        self.mode
    }

    /// The 6581 profile this filter plays (an 8580 ignores it).
    pub fn profile(&self) -> &'static RevisionProfile {
        self.profile
    }

    /// Play `profile` from now on: the coefficients are recomputed for the
    /// current registers, the integrator state carries on.
    pub fn set_profile(&mut self, profile: &'static RevisionProfile) {
        self.profile = profile;
        self.set(self.cutoff_reg, self.res);
    }

    /// Become a filter of `model` while playing: the integrator state carries
    /// on, the cutoff and resonance maps are the new model's.
    pub fn set_model(&mut self, model: SidModel) {
        self.model = model;
        self.set(self.cutoff_reg, self.res);
    }

    /// Mode bits (LP/BP/HP); other bits are ignored.
    pub fn set_mode(&mut self, mode: u8) {
        self.mode = mode & (LP | BP | HP);
    }

    /// Set cutoff register and resonance nibble; recomputes coefficients.
    pub fn set(&mut self, cutoff_reg: u16, res: u8) {
        self.cutoff_reg = cutoff_reg & 0x7FF;
        self.res = res & 0xF;
        let fc = self
            .cutoff()
            .min(self.ceiling())
            .min(self.sample_rate * 0.49);
        self.fc = fc;
        self.g = (PI * fc / self.sample_rate).tan();
        self.k = 1.0 / self.q();
        self.h = 1.0 / (1.0 + self.g * (self.g + self.k));
    }

    /// One sample in, the selected sum of LP/BP/HP out. With no mode bit set
    /// the output is silent (the datasheet: filtered voices then vanish).
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let hp = (x - (self.k + self.g) * self.s1 - self.s2) * self.h;
        let v1 = self.g * hp;
        let bp = v1 + self.s1;
        self.s1 = bp + v1;
        if self.model == SidModel::Sid6581 {
            let sat = self.profile.sat;
            self.s1 = sat * (self.s1 / sat).tanh();
        }
        let v2 = self.g * bp;
        let lp = v2 + self.s2;
        self.s2 = lp + v2;
        let mut y = 0.0;
        if self.mode & LP != 0 {
            y += lp;
        }
        if self.mode & BP != 0 {
            y += bp;
        }
        if self.mode & HP != 0 {
            y += hp;
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revision::{GT_REF, R4AR};

    const SR: f64 = 44_100.0;

    /// Steady-state gain of a sine at `hz` through the selected mode.
    fn sine_gain(mode: u8, cutoff: u16, res: u8, hz: f64) -> f64 {
        let mut f = Filter::new(SR);
        f.set_mode(mode);
        f.set(cutoff, res);
        let n = 44_100;
        let mut peak: f64 = 0.0;
        for i in 0..n {
            let y = f.process((2.0 * PI * hz * i as f64 / SR).sin());
            if i > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        peak
    }

    #[test]
    fn register_maps() {
        assert_eq!(cutoff_hz(0), 30.0);
        assert!((cutoff_hz(0x7FF) - 12_000.0).abs() < 1e-9);
        assert_eq!(cutoff_hz(0x800), cutoff_hz(0)); // 11 bits only
        assert!((resonance_q(0) - 0.707).abs() < 1e-12);
        assert!((resonance_q(15) - 1.707).abs() < 1e-12);
        assert!((resonance_q(8) - (0.707 + 8.0 / 15.0)).abs() < 1e-12);
        for r in 0..15 {
            assert!(resonance_q(r + 1) > resonance_q(r));
        }
    }

    #[test]
    fn effective_cutoff_has_gts_4_khz_delta_mode_ceiling_on_both_models() {
        // GT2 plays through reSID in its delta-clock mode, which limits the
        // cutoff to 4 kHz; the maps stay unclamped (register_maps and
        // map_6581_endpoints pin 12 kHz / 18 kHz at 0x7FF). The 6581 side is
        // the GT reference profile: the hardware revisions have no ceiling.
        for model in [SidModel::Sid8580, SidModel::Sid6581] {
            for sr in [44_100.0, 48_000.0] {
                let mut f = Filter::with_profile(model, &GT_REF, sr);
                f.set(0x7FF, 0);
                assert_eq!(f.effective_cutoff(), 4000.0, "{model:?} sr {sr} reg 0x7FF");
                assert!(
                    f.cutoff() > 4000.0,
                    "{model:?}: the map itself is unclamped"
                );
                // First register whose map exceeds 4 kHz clamps; below passes.
                let over = (0..=0x7FFu16)
                    .find(|&r| cutoff_hz_for_profile(model, r) > 4000.0)
                    .unwrap();
                f.set(over, 0);
                assert_eq!(f.effective_cutoff(), 4000.0, "{model:?} reg {over:#x}");
                f.set(over - 1, 0);
                assert_eq!(
                    f.effective_cutoff(),
                    cutoff_hz_for_profile(model, over - 1),
                    "{model:?} reg {:#x}",
                    over - 1
                );
            }
        }
    }

    /// The cutoff map of `model`, the 6581 on the GT reference profile.
    fn cutoff_hz_for_profile(model: SidModel, reg: u16) -> f64 {
        match model {
            SidModel::Sid6581 => cutoff_hz_6581_with(&GT_REF, reg),
            _ => cutoff_hz_for(model, reg),
        }
    }

    #[test]
    fn impulse_response_is_stable_and_decays_for_every_setting() {
        for sr in [44_100.0, 48_000.0] {
            for cutoff in [0u16, 100, 1024, 2047] {
                for res in 0..16u8 {
                    let mut f = Filter::new(sr);
                    f.set_mode(LP | BP | HP);
                    f.set(cutoff, res);
                    f.process(1.0);
                    let mut tail: f64 = 0.0;
                    for i in 0..200_000 {
                        let y = f.process(0.0);
                        assert!(y.is_finite());
                        if i > 190_000 {
                            tail = tail.max(y.abs());
                        }
                    }
                    assert!(tail < 1e-9, "sr {sr} cutoff {cutoff} res {res}: {tail}");
                }
            }
        }
    }

    #[test]
    fn step_response_settles_to_unity_dc_gain() {
        let mut f = Filter::new(SR);
        f.set_mode(LP);
        f.set(0x200, 15);
        let ys: Vec<f64> = (0..44_100).map(|_| f.process(1.0)).collect();
        let overshoot = ys.iter().fold(0.0f64, |m, &y| m.max(y));
        assert!((ys[ys.len() - 1] - 1.0).abs() < 1e-6);
        assert!(overshoot > 1.2, "{overshoot}");
        let mut f = Filter::new(SR);
        f.set_mode(LP);
        f.set(0x200, 0);
        let mut ov0: f64 = 0.0;
        for _ in 0..44_100 {
            ov0 = ov0.max(f.process(1.0));
        }
        assert!(ov0 < 1.06, "{ov0}"); // analog Butterworth: 4.3 %
    }

    #[test]
    fn resonance_peak_grows_with_the_register() {
        // |H_LP(fc)| = Q for the analog SVF; the prewarped TPT keeps fc exact.
        let reg = 0x155; // ~2 kHz
        let fc = cutoff_hz(reg);
        let mut last = 0.0;
        for res in 0..16u8 {
            let g = sine_gain(LP, reg, res, fc);
            assert!(g > last, "res {res}: {g} <= {last}");
            assert!((g / resonance_q(res) - 1.0).abs() < 0.02, "res {res}: {g}");
            last = g;
        }
    }

    #[test]
    fn slopes_are_twelve_and_six_db_per_octave() {
        // Two octaves above/below fc the asymptotes hold within 1.5 dB.
        let reg = 0x80; // ~780 Hz
        let fc = cutoff_hz(reg);
        let lp = 20.0 * (sine_gain(LP, reg, 0, fc * 4.0) / sine_gain(LP, reg, 0, fc * 8.0)).log10();
        assert!((lp - 12.0).abs() < 1.5, "LP {lp} dB/oct");
        let reg = 0x400; // ~6 kHz, so fc/8 is well above the DC region
        let fc = cutoff_hz(reg);
        let hp = 20.0 * (sine_gain(HP, reg, 0, fc / 4.0) / sine_gain(HP, reg, 0, fc / 8.0)).log10();
        assert!((hp - 12.0).abs() < 1.5, "HP {hp} dB/oct");
        let bp = 20.0 * (sine_gain(BP, reg, 0, fc / 4.0) / sine_gain(BP, reg, 0, fc / 8.0)).log10();
        assert!((bp - 6.0).abs() < 1.0, "BP {bp} dB/oct");
    }

    #[test]
    fn no_mode_bit_is_silent_and_lp_plus_hp_notches() {
        let reg = 0x155;
        let fc = cutoff_hz(reg);
        assert_eq!(sine_gain(0, reg, 0, fc), 0.0);
        // LP + HP of an SVF is a notch at fc: 1 - k*BP -> 0 at fc.
        assert!(sine_gain(LP | HP, reg, 0, fc) < 0.01);
        assert!(sine_gain(LP | HP, reg, 0, fc / 8.0) > 0.95);
    }

    /// Steady-state gain of a sine of amplitude `amp` at `hz`, 6581 filter.
    /// R4AR's 6581 filter (soft limit SAT 1.0); the GT-reference profile's is
    /// effectively linear (S5.16).
    fn sine_gain_6581(mode: u8, cutoff: u16, res: u8, hz: f64, amp: f64) -> f64 {
        let mut f = Filter::with_profile(SidModel::Sid6581, &R4AR, SR);
        f.set_mode(mode);
        f.set(cutoff, res);
        let n = 44_100;
        let mut peak: f64 = 0.0;
        for i in 0..n {
            let y = f.process(amp * (2.0 * PI * hz * i as f64 / SR).sin());
            if i > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        peak / amp
    }

    #[test]
    fn map_6581_endpoints_anchors_and_a_hand_point() {
        // Endpoints: 220 Hz floor at 0, 18 kHz at 0x7FF.
        assert!((cutoff_hz_6581_with(&R4AR, 0) - 220.0).abs() < 1e-9);
        assert!((cutoff_hz_6581_with(&R4AR, 0x7FF) - 18_000.0).abs() < 1e-6);
        assert_eq!(
            cutoff_hz_6581_with(&R4AR, 0x800),
            cutoff_hz_6581_with(&R4AR, 0)
        ); // 11 bits only
        assert_eq!(
            cutoff_hz_6581_with(&R4AR, 0xFFFF),
            cutoff_hz_6581_with(&R4AR, 0x7FF)
        );
        // Measured anchors (.ai/sid-chip-comparison-report.md §6.3), ±5%.
        for (reg, want) in [
            (0x200u16, 420.0),
            (0x300, 1_600.0),
            (0x3FF, 6_000.0),
            (0x400, 4_600.0),
            (0x500, 9_500.0),
            (0x600, 14_500.0),
        ] {
            let got = cutoff_hz_6581_with(&R4AR, reg);
            assert!(
                (got / want - 1.0).abs() < 0.05,
                "reg {reg:#05x}: {got} Hz, want {want}"
            );
        }
        // Hand point, reg 0x280, halfway (in register) between the 0x200 and
        // 0x300 anchors, so halfway in log f: sqrt(420 * 1600) = 819.756 Hz.
        // The 8580 map puts the same register at 30 + 640 * 5.84758 = 3772.45 Hz.
        assert!(
            (cutoff_hz_6581_with(&R4AR, 0x280) - 819.756).abs() < 0.01,
            "{}",
            cutoff_hz_6581_with(&R4AR, 0x280)
        );
        assert!((cutoff_hz(0x280) - 3772.45).abs() < 0.01);
    }

    #[test]
    fn map_6581_is_monotonic_per_piece_with_the_fc_hi_step_and_unlike_the_8580() {
        // Strictly rising within each piece (0..=0x3FF, 0x400..=0x7FF); the
        // one deliberate drop is 0x3FF -> 0x400 (~6 kHz -> ~4.6 kHz), the
        // FC_HI $7F/$80 step.
        for r in (0..0x3FFu16).chain(0x400..0x7FF) {
            assert!(
                cutoff_hz_6581_with(&GT_REF, r + 1) > cutoff_hz_6581_with(&GT_REF, r),
                "reg {r:#05x}"
            );
        }
        assert!(cutoff_hz_6581_with(&GT_REF, 0x400) < cutoff_hz_6581_with(&GT_REF, 0x3FF) * 0.85);
        // Bottom quarter: the 6581 barely moves (220 -> ~420 Hz) where the
        // 8580 sweeps 30 -> ~3 kHz; the top ends 18 kHz vs 12 kHz.
        assert!(cutoff_hz_6581_with(&GT_REF, 0x200) / cutoff_hz_6581(0) < 2.0);
        assert!(cutoff_hz(0x200) / cutoff_hz(0) > 100.0);
        // Hand values: reg 0 -> 220 vs 30 (x7.3); 0x100 -> sqrt(220 * 420)
        // = 304 vs 1527 (x5.0); 0x200 -> 420 vs 3024 (x7.2); 0x7FF -> 18000
        // vs 12000 (x1.5).
        for r in [0u16, 0x100, 0x200, 0x7FF] {
            let (a, b) = (cutoff_hz_6581_with(&GT_REF, r), cutoff_hz(r));
            assert!((a / b).max(b / a) > 1.45, "reg {r}: 6581 {a} vs 8580 {b}");
        }
        assert_eq!(cutoff_hz_for(SidModel::Sid8580, 0x333), cutoff_hz(0x333));
        assert_eq!(
            cutoff_hz_for(SidModel::Sid6581, 0x333),
            cutoff_hz_6581(0x333)
        );
    }

    #[test]
    fn resonance_6581_is_weaker_than_the_8580() {
        // Q = 0.707 * 2^(res/12): 0 -> 0.707, 12 -> 1.414, 15 -> 0.707 * 2^1.25
        // = 0.707 * 2.378414 = 1.681539. The 8580 (reSID's map) reaches 1.707.
        assert!((resonance_q_6581_with(&R4AR, 0) - 0.707).abs() < 1e-12);
        assert!((resonance_q_6581_with(&R4AR, 12) - 1.414).abs() < 1e-12);
        assert!((resonance_q_6581_with(&R4AR, 15) - 1.681539).abs() < 1e-6);
        for r in 1..16u8 {
            assert!(resonance_q_6581_with(&R4AR, r) > resonance_q_6581_with(&R4AR, r - 1));
            assert!(resonance_q_6581_with(&R4AR, r) < resonance_q(r));
        }
    }

    #[test]
    fn filter_6581_is_stable_even_when_overdriven() {
        for sr in [44_100.0, 48_000.0] {
            for cutoff in [0u16, 100, 1024, 2047] {
                for res in [0u8, 7, 15] {
                    for amp in [1.0, 10.0] {
                        let mut f = Filter::with_model(SidModel::Sid6581, sr);
                        f.set_mode(LP | BP | HP);
                        f.set(cutoff, res);
                        f.process(amp);
                        let mut tail: f64 = 0.0;
                        for i in 0..200_000 {
                            let y = f.process(0.0);
                            assert!(y.is_finite());
                            if i > 190_000 {
                                tail = tail.max(y.abs());
                            }
                        }
                        assert!(tail < 1e-9, "sr {sr} cutoff {cutoff} res {res} amp {amp}");
                    }
                }
            }
        }
    }

    #[test]
    fn filter_6581_small_signal_peak_is_q_and_compresses_with_level() {
        // Small signal: tanh is linear to ~u^2/3, so |H_LP(fc)| = Q within
        // the same 2 % the 8580 test uses. Reg 0x300 (~1.6 kHz) keeps the
        // map below the 4 kHz ceiling, so the probe sits on the real fc.
        let reg = 0x300;
        let fc = cutoff_hz_6581_with(&R4AR, reg);
        for res in [0u8, 8, 15] {
            let g = sine_gain_6581(LP, reg, res, fc, 0.01);
            assert!(
                (g / resonance_q_6581_with(&R4AR, res) - 1.0).abs() < 0.02,
                "res {res}: {g}"
            );
        }
        // Level-dependent peak gain: the band-pass state is clamped below
        // SAT = 1, so the resonant peak must fall as the level rises. At
        // amplitude 1 and Q 1.68 the linear state would reach ~Q; the limit
        // takes a clear bite. The 8580 stays linear at every level.
        let small = sine_gain_6581(LP, reg, 15, fc, 0.01);
        let full = sine_gain_6581(LP, reg, 15, fc, 1.0);
        let hot = sine_gain_6581(LP, reg, 15, fc, 3.0);
        assert!(full < small * 0.95, "{full} vs {small}");
        assert!(hot < full, "{hot} vs {full}");
        let lin_small = sine_gain(LP, 0x155, 15, cutoff_hz(0x155));
        let mut f = Filter::new(SR);
        f.set_mode(LP);
        f.set(0x155, 15);
        let mut peak: f64 = 0.0;
        for i in 0..44_100 {
            let y = f.process(3.0 * (2.0 * PI * cutoff_hz(0x155) * i as f64 / SR).sin());
            if i > 22_050 {
                peak = peak.max(y.abs());
            }
        }
        assert!((peak / 3.0 / lin_small - 1.0).abs() < 1e-9);
    }
}
