//! 6581 die-revision profiles (S5.15). Every characteristic that could
//! differ between 6581 die revisions lives here, in one `RevisionProfile`,
//! so a later pass can add a revision (R3) by adding a profile and changing
//! `SID6581_REVISION`. Nothing else in the engine names a revision.
//!
//! Selection is engine-side: `SidModel::Sid6581` always plays
//! `profile_6581()`. No .sng/.sid field encodes the die revision, so there
//! is nothing in the file format to read it from.
//!
//! Sources (GPL-free; `.ai/sid-r4ar-research-notes.md`):
//! - Wikipedia, "MOS Technology 6581", Revisions: the 6581 went through R2,
//!   R3, R4 and R4AR with "no substantial alterations", only input-pin
//!   protection/buffering, silicon grade and packaging changes.
//! - C64-Wiki, "SID": 6581R4AR was produced 22/1986-06/1987, the last 6581.
//!   Trivia: every write to the 6581's volume register gave an audible click
//!   (the $D418 sample trick); on the 8580 "samples were inaudible".
//!
//! Trait classification. Only the volume DAC is new in S5.15; the rest is
//! earlier work moved here unchanged:
//! - Cutoff curve: GENERIC-6581. The public record gives 6581-vs-8580
//!   nonlinearity but no per-revision anchors. The R4AR profile carries the
//!   S5.12 measured-anchor curve as is. The published nominal curve's range
//!   (220 Hz..18 kHz) is quoted for the 6581 generally — the record does not
//!   break it down per die revision; R4AR is consistent with it.
//! - FC_HI $7F -> $80 step: GENERIC-6581. It is the 0x3FF -> 0x400 drop
//!   between the two anchor sets. No revision-specific position or shape is
//!   invented.
//! - Output DC (voice DC, mixer DC) and the gain derived from them:
//!   GENERIC-6581. They keep S2's INFERRED values (`voice.rs`, `chip.rs`).
//! - Volume DAC: GENERIC-6581 vs 8580. The 8580 stays linear, VOL / 15. See
//!   `R4AR` for the INFERRED 6581 table.
//!
//! `R2`, `R3` and `R4` are MEASURED on real chips, cutoff only: filter sweeps and
//! music recorded from Stone Oakvalley's Authentic SID Collection (SOASC),
//! one physical chip each (method and data: `.ai/sid-soasc/NOTES.md`). Chips
//! of one revision vary about as much as the revisions do, so each profile
//! is "that chip", not the revision as a whole. Neither has the 4 kHz
//! ceiling GoatTracker's playback has (`cutoff_ceiling_hz`).

use super::filter::CUTOFF_CEILING_DELTA_HZ;

/// A 6581 die revision the engine can play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DieRevision {
    /// 6581R4AR (1986-1987), the last and most common 6581.
    R4AR,
    /// Not a chip: the 6581 filter response GoatTracker's default playback
    /// has (classic reSID), measured and fitted in S5.16
    /// (`.ai/sid-6581-gt-fit-notes.md`). It differs from `R4AR` in the filter
    /// only (cutoff curve, resonance map, soft limit); every other trait is
    /// R4AR's. Named here because a GT song is mixed against that filter.
    GtRef,
    /// A 6581R2: SOASC's MOS6581 4982 (1982). Bright; cutoff measured from
    /// filter sweeps (`R2`).
    R2,
    /// A 6581R3: SOASC's MOS6581 2383 (1983). Brightest of the measured
    /// chips; cutoff fitted from music recordings (`R3`).
    R3,
    /// A 6581R4: SOASC's MOS6581R4 3387 (1987). Dark; cutoff measured from
    /// filter sweeps (`R4`).
    R4,
}

impl DieRevision {
    /// Every revision, in the order a picker lists them.
    pub const ALL: [DieRevision; 5] = [
        DieRevision::GtRef,
        DieRevision::R2,
        DieRevision::R3,
        DieRevision::R4,
        DieRevision::R4AR,
    ];

    /// The revision's name on the wasm API and in the app's settings.
    pub const fn name(self) -> &'static str {
        match self {
            DieRevision::GtRef => "gt",
            DieRevision::R2 => "r2",
            DieRevision::R3 => "r3",
            DieRevision::R4 => "r4",
            DieRevision::R4AR => "r4ar",
        }
    }

    /// The revision called `name` (`name`), or `None`.
    pub fn from_name(name: &str) -> Option<DieRevision> {
        DieRevision::ALL.into_iter().find(|r| r.name() == name)
    }

    pub const fn profile(self) -> &'static RevisionProfile {
        match self {
            DieRevision::R4AR => &R4AR,
            DieRevision::GtRef => &GT_REF,
            DieRevision::R2 => &R2,
            DieRevision::R3 => &R3,
            DieRevision::R4 => &R4,
        }
    }
}

/// The revision `SidModel::Sid6581` plays. The one swap point. R3, a measured
/// real chip and the most common revision; GT_REF (GoatTracker's playback)
/// stays selectable but is not hardware.
pub const SID6581_REVISION: DieRevision = DieRevision::R3;

/// The profile `SidModel::Sid6581` plays.
pub const fn profile_6581() -> &'static RevisionProfile {
    SID6581_REVISION.profile()
}

/// How the resonance nibble maps to Q: both start at Butterworth, 0.707.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResonanceMap {
    /// Q = 0.707 * 2^(res / divisor). R4AR's (S2, INFERRED), divisor 12.
    Exponential { divisor: f64 },
    /// Q = 0.707 + slope * res. The measured GT response (S5.16).
    Linear { slope: f64 },
}

impl ResonanceMap {
    pub fn q(self, res: u8) -> f64 {
        let r = (res & 0xF) as f64;
        match self {
            ResonanceMap::Exponential { divisor } => 0.707 * 2f64.powf(r / divisor),
            ResonanceMap::Linear { slope } => 0.707 + slope * r,
        }
    }
}

/// Everything revision-specific about a 6581.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RevisionProfile {
    pub revision: DieRevision,
    /// Cutoff anchors (register, Hz), low piece: 0..=0x3FF (FC_HI bit 7
    /// clear). Log-linear between anchors (`filter::cutoff_hz_6581_with`).
    pub cutoff_anchors_lo: &'static [(u16, f64)],
    /// Cutoff anchors, high piece: 0x400..=0x7FF (FC_HI bit 7 set).
    pub cutoff_anchors_hi: &'static [(u16, f64)],
    /// Highest cutoff `Filter::set` plays, Hz, before the 0.49 * sample-rate
    /// clamp. GoatTracker's reSID playback has 4 kHz
    /// (`filter::CUTOFF_CEILING_DELTA_HZ`); real chips have none (infinity).
    pub cutoff_ceiling_hz: f64,
    /// Resonance nibble -> Q.
    pub resonance: ResonanceMap,
    /// Soft limit of the band-pass state, in voice units (`filter.rs`):
    /// s <- sat * tanh(s / sat). Large = near linear.
    pub sat: f64,
    /// Output level trim on top of the headroom-rule gain (`chip_gain`).
    /// 1.0 = the S2 level. `GT_REF` trims to GoatTracker's playback level.
    pub gain_trim: f64,
    /// Weights of VOL bits 0..3 in the volume DAC. The level of VOL v is
    /// the sum of the weights of v's set bits over the sum of all four, so
    /// VOL 0 = 0 and VOL 15 = 1 whatever the weights.
    pub volume_bit_weights: [f64; 4],
    /// Per-voice DAC DC in voice units, envelope-scaled (`voice.rs`).
    pub voice_dc: f64,
    /// Mixer/volume-stage DC in voice units (`chip.rs`).
    pub mix_dc: f64,
}

/// 6581R4AR.
///
/// Cutoff: the S5.12 R2 anchors, disclosure at
/// `filter::CUTOFF_ANCHORS_6581_LO`. Output DC: S2's voice DC 0.25 and
/// mixer DC 0.5, both INFERRED tuning (`voice.rs`, `chip.rs` headers).
///
/// Volume DAC, INFERRED. Public record: a $D418 write clicks loudly on a
/// 6581 and not on an 8580. The loudness comes from the mixer DC the DAC
/// scales (`mix_dc`, kept from S2). No verified per-bit weights of the
/// 6581's 4-bit volume DAC exist in the public record, so the weights are
/// a disclosed guess, [1.0, 2.0, 3.9, 7.6]: a binary-weighted DAC whose two
/// upper bits run a little under their ideal 4 and 8, the usual shape of an
/// untrimmed NMOS DAC. The result stays monotonic. Its largest error is at
/// the midscale 7 -> 8 transition, +/-0.0092 of full scale (level(7) =
/// 0.4759 vs 0.4667, level(8) = 0.5241 vs 0.5333). There, the 7 -> 8 step
/// is 0.0483 instead of 1/15 = 0.0667. VOL 15 is exactly 1, so every
/// full-volume render is unchanged. Ears-gate item.
pub const R4AR: RevisionProfile = RevisionProfile {
    revision: DieRevision::R4AR,
    cutoff_anchors_lo: &[
        (0, 220.0),
        (0x200, 420.0),
        (0x300, 1_600.0),
        (0x3FF, 6_000.0),
    ],
    cutoff_anchors_hi: &R4AR_ANCHORS_HI,
    cutoff_ceiling_hz: CUTOFF_CEILING_DELTA_HZ,
    resonance: ResonanceMap::Exponential { divisor: 12.0 },
    sat: 1.0,
    gain_trim: 1.0,
    volume_bit_weights: [1.0, 2.0, 3.9, 7.6],
    voice_dc: 0.25,
    mix_dc: 0.5,
};

const R4AR_ANCHORS_HI: [(u16, f64); 4] = [
    (0x400, 4_600.0),
    (0x500, 9_500.0),
    (0x600, 14_500.0),
    (0x7FF, 18_000.0),
];

/// The GoatTracker-reference 6581 filter (S5.16). Filter traits only; the
/// output DC, gain and volume DAC are R4AR's, since feeding GT's register
/// stream to both chips gave matching overall levels (RMS ratio 0.97-1.00).
///
/// MEASURED, not published: a two-pole fit (0.03-0.06 dB RMS) of GT 2.72's
/// default 6581 low-pass, noise through the filter, per register and per
/// resonance setting (data and method in `.ai/sid-6581-gt-fit-notes.md`).
/// - Cutoff: 14 low-piece anchors, flat near 220 Hz up to 0x100 then
///   climbing; R4AR's four chords ran 15-25% high across 0x080-0x1C0. Above
///   the 4 kHz ceiling the high piece is R4AR's, untouched.
/// - Resonance: Q = 0.707 + 0.0698 * res (measured 0.72 at 0, 1.76 at 15).
/// - Soft limit: GT's filter core is linear; see `sat` for the level kept.
/// - Level: `gain_trim` (S5.16), measured against the same playback.
pub const GT_REF: RevisionProfile = RevisionProfile {
    revision: DieRevision::GtRef,
    cutoff_anchors_lo: &[
        (0, 219.0),
        (0x080, 229.0),
        (0x100, 248.0),
        (0x140, 266.0),
        (0x180, 299.0),
        (0x1C0, 339.0),
        (0x200, 417.0),
        (0x240, 550.0),
        (0x280, 778.0),
        (0x2C0, 1_139.0),
        (0x300, 1_628.0),
        (0x340, 2_329.0),
        (0x380, 3_331.0),
        (0x3FF, 6_000.0),
    ],
    cutoff_anchors_hi: &R4AR_ANCHORS_HI,
    cutoff_ceiling_hz: CUTOFF_CEILING_DELTA_HZ,
    resonance: ResonanceMap::Linear { slope: 0.0698 },
    // reSID's filter core is linear, so effectively no soft limit: 1000 leaves
    // the tanh in place but a million times off. MEASURED on the bass channel
    // of Coconut Conundrum, filtered level ours / reSID: sat 4 -> 0.919,
    // 16 -> 0.987, 1000 -> 0.999. (S5.16 first picked 4 from the noise
    // transfer function alone; a strong resonant bass shows the loss.)
    sat: 1000.0,
    // Level: steady tri/saw/pulse/noise at 788, 7493 and 20000 Hz-ish
    // registers, unfiltered, ran 1.14-1.16x GT's (reSID) RMS on the 6581 at
    // R4AR's gain 0.1976; 1 / 1.16 = 0.862 of that is 0.1704. The headroom-rule
    // gain follows the DC terms (0.1619 at voice DC 0.5625), so the trim is
    // 0.1704 / 0.1619 = 1.052.
    gain_trim: 1.052,
    volume_bit_weights: R4AR.volume_bit_weights,
    // Voice DC 0.5625 = (0x800 - 0x380) / 0x800: the 6581 waveform DAC's zero
    // sits at 0x380, not mid-scale, so a centred wave carries 0.5625 of its
    // amplitude as envelope-scaled DC. MEASURED against GT's playback two
    // ways: the DC step when the envelope opens on a no-waveform note (ours
    // -0.105 at 0.25, reSID -0.051; 0.5625 gives -0.050), and the release tail
    // after a hard restart (0.53 0.62 0.49 0.38 ... vs reSID 0.51 0.61 0.48
    // 0.37 ...; at 0.25 ours ran 0.30 0.41 0.31 0.24 ...).
    voice_dc: 0.5625,
    mix_dc: R4AR.mix_dc,
};

/// SOASC's 6581R4 chip (MOS6581R4 3387). MEASURED cutoff: Plogue's
/// SIDBENCH noise sweeps (res 0, FC_HI 0..255, LP/BP/HP) recorded on the
/// chip; cutoff is where the LP and HP spectra cross, median of three voices,
/// smoothed and made monotonic. Flat near 295 Hz up to FC_HI ~$50, then
/// climbing; no step at 0x400. The top anchors are where the measurement
/// ends (~19.7 kHz), not a known limit. Anchors every 0x40, within 7.5% of
/// the measured curve. No ceiling.
///
/// Everything but the cutoff is GT_REF's (resonance, soft limit, DC, gain,
/// volume DAC): the recordings do not measure those.
pub const R4: RevisionProfile = RevisionProfile {
    revision: DieRevision::R4,
    cutoff_anchors_lo: &[
        (0x000, 293.0),
        (0x080, 293.0),
        (0x0C0, 296.0),
        (0x100, 298.0),
        (0x180, 300.0),
        (0x200, 302.0),
        (0x240, 306.0),
        (0x280, 322.0),
        (0x2C0, 342.0),
        (0x300, 374.0),
        (0x340, 408.0),
        (0x380, 500.0),
        (0x3C0, 619.0),
        (0x3FF, 695.0),
    ],
    cutoff_anchors_hi: &[
        (0x400, 698.0),
        (0x440, 846.0),
        (0x480, 1_147.0),
        (0x4C0, 1_663.0),
        (0x500, 2_471.0),
        (0x540, 3_458.0),
        (0x580, 4_713.0),
        (0x5C0, 6_398.0),
        (0x600, 8_257.0),
        (0x640, 9_462.0),
        (0x680, 11_220.0),
        (0x6C0, 12_878.0),
        (0x700, 14_640.0),
        (0x740, 16_161.0),
        (0x780, 17_881.0),
        (0x7C0, 19_508.0),
        (0x7FF, 19_688.0),
    ],
    cutoff_ceiling_hz: f64::INFINITY,
    ..GT_REF
};

/// SOASC's 6581R2 chip (MOS6581 4982). MEASURED cutoff: SIDBENCH sweeps as
/// for `R4`. The chip's curve is `R4`'s shifted along the register,
/// fc(reg) = R4(reg + 680), within 6.5% mean over its measured points
/// (350 Hz-13 kHz); the anchors are that shifted curve, which keeps the
/// three measured profiles on one shape. Above ~15 kHz the sweeps cannot
/// resolve the cutoff; the curve reaches the end of the measured range
/// (~19.7 kHz) near 0x540. No ceiling.
///
/// Everything but the cutoff is GT_REF's, as for `R4`.
pub const R2: RevisionProfile = RevisionProfile {
    revision: DieRevision::R2,
    cutoff_anchors_lo: &[
        (0x000, 340.0),
        (0x040, 358.0),
        (0x080, 389.0),
        (0x0C0, 457.0),
        (0x100, 559.0),
        (0x140, 674.0),
        (0x180, 813.0),
        (0x1C0, 1_021.0),
        (0x200, 1_427.0),
        (0x240, 2_206.0),
        (0x280, 2_996.0),
        (0x2C0, 4_281.0),
        (0x300, 5_642.0),
        (0x340, 7_752.0),
        (0x380, 8_736.0),
        (0x3C0, 10_428.0),
        (0x3FF, 12_208.0),
    ],
    cutoff_anchors_hi: &[
        (0x400, 12_235.0),
        (0x440, 13_943.0),
        (0x480, 15_691.0),
        (0x4C0, 17_262.0),
        (0x500, 18_917.0),
        (0x540, 19_688.0),
        (0x7FF, 19_688.0),
    ],
    cutoff_ceiling_hz: f64::INFINITY,
    ..GT_REF
};

/// SOASC's 6581R3 chip (MOS6581 2383). MEASURED, less directly than `R4`:
/// no sweep recording of this chip exists, so its cutoff is `R4`'s curve
/// shifted along the register, fc(reg) = R4(reg + 880), with the shift
/// fitted from SOASC's recordings of three filter-heavy HVSC tunes against
/// renders of their register streams. The same fit put SOASC's R2 at +680,
/// which its own SIDBENCH sweeps confirm to within 6.5%, and ran that chip
/// about 17% short, corrected for here. Shift uncertainty +-80 (roughly
/// +-25% in Hz on the slope). The floor below ~500 Hz is extrapolated: the
/// music cannot show whether this chip flattens there like R4. The curve
/// reaches the end of the measured range (~19.7 kHz) near 0x480. No ceiling.
///
/// Everything but the cutoff is GT_REF's, as for `R4`.
pub const R3: RevisionProfile = RevisionProfile {
    revision: DieRevision::R3,
    cutoff_anchors_lo: &[
        (0x000, 466.0),
        (0x040, 586.0),
        (0x080, 675.0),
        (0x0C0, 821.0),
        (0x100, 1_074.0),
        (0x140, 1_498.0),
        (0x180, 2_302.0),
        (0x1C0, 3_158.0),
        (0x200, 4_419.0),
        (0x240, 5_876.0),
        (0x280, 8_072.0),
        (0x2C0, 8_989.0),
        (0x300, 10_675.0),
        (0x340, 12_456.0),
        (0x380, 14_182.0),
        (0x3C0, 15_879.0),
        (0x3FF, 17_561.0),
    ],
    cutoff_anchors_hi: &[
        (0x400, 17_604.0),
        (0x440, 19_137.0),
        (0x480, 19_688.0),
        (0x7FF, 19_688.0),
    ],
    cutoff_ceiling_hz: f64::INFINITY,
    ..GT_REF
};

impl RevisionProfile {
    /// Output gain from the S2 headroom rule (`chip.rs` header): with every
    /// DC term at its worst, the peak equals the 8580's three full-scale
    /// voices. CHIP_GAIN * 3 / (3 * (1 + voice_dc) + mix_dc).
    pub const fn chip_gain(&self, chip_gain_8580: f64) -> f64 {
        chip_gain_8580 * 3.0 / (3.0 * (1.0 + self.voice_dc) + self.mix_dc)
    }

    /// Volume DAC level of VOL `vol` (low nibble), 0..=1.
    pub fn volume_level(&self, vol: u8) -> f64 {
        let w = &self.volume_bit_weights;
        let set = (0..4)
            .filter(|b| vol >> b & 1 != 0)
            .fold(0.0, |a, b| a + w[b]);
        set / w.iter().fold(0.0, |a, x| a + x)
    }

    /// `volume_level` for every VOL value.
    pub fn volume_table(&self) -> [f64; 16] {
        std::array::from_fn(|v| self.volume_level(v as u8))
    }
}
