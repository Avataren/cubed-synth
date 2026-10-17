//! S5.15 tests: the 6581 die-revision profile (R4AR) and its volume DAC.
//!
//! Same discipline as `tests_s2.rs`: hand derivations in the comment above
//! each assertion, every render through `Chip::new` + `write` + `render`.
//!
//! Hand constants (R4AR profile, `revision.rs`): volume bit weights
//! [1.0, 2.0, 3.9, 7.6] (INFERRED), sum 14.5, so level(v) = (sum of the
//! weights of v's set bits) / 14.5:
//!   level(5) = 4.9 / 14.5 = 0.337931   (linear 0.333333)
//!   level(7) = 6.9 / 14.5 = 0.475862   (linear 0.466667)
//!   level(8) = 7.6 / 14.5 = 0.524138   (linear 0.533333)
//!   level(10) = 9.6 / 14.5 = 0.662069  (linear 0.666667)
//! G6 = CHIP_GAIN_6581 = 0.84 / 4.25 = 0.197647, MIX_DC = 0.5.

use super::chip::REG_MODE_VOL;
use super::waveform::{GATE, SAW};
use super::*;

const V1: u8 = 0x00;
const G6: f64 = 0.84 / 4.25;

fn chip(m: SidModel) -> Chip {
    // R4AR at the S2 reference level: these tests pin R4AR's numbers (S5.16
    // made the default 6581 the GT-reference profile and trims both models).
    let mut c = Chip::with_profile(m, DEFAULT_SAMPLE_RATE, &R4AR).expect("both models construct");
    c.set_gain_trim(1.0);
    c.set_filter_sign(1.0);
    c
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64).sqrt()
}

/// First sample after writing `vol` to a silent chip (no gate, mode 0): the
/// mixer-DC step MIX_DC * level(vol) * G6 before the blocker decays it.
fn dc_step(m: SidModel, vol: u8) -> f64 {
    let mut c = chip(m);
    c.write(REG_MODE_VOL, vol);
    let mut o = [0.0f32; 1];
    c.render(&mut o);
    o[0] as f64
}

/// RMS of a steady unfiltered A-4 saw at `vol`, 1 s after note-on (every DC
/// transient has decayed by r^44100 ~ e^-100).
fn saw_rms(m: SidModel, vol: u8) -> f64 {
    let mut c = chip(m);
    c.write(REG_MODE_VOL, vol);
    c.write(V1, 0x45);
    c.write(V1 + 1, 0x1D);
    c.write(V1 + 6, 0xF0);
    c.write(V1 + 4, SAW | GATE);
    let mut out = vec![0.0f32; 88_200];
    c.render(&mut out);
    rms(&out[44_100..])
}

// ---------------------------------------------------------------------------
// Behaviour: the 6581 volume DAC is nonlinear, the 8580's is not
// ---------------------------------------------------------------------------

#[test]
fn volume_dac_6581_mixer_dc_step_follows_the_r4ar_table_not_vol_over_15() {
    // Step = 0.5 * level(v) * G6:
    //   v 5  -> 0.5 * 0.337931 * 0.197647 = 0.0333955 (linear 0.0329412)
    //   v 7  -> 0.0470264 (linear 0.0461176)
    //   v 8  -> 0.0517972 (linear 0.0527059)
    //   v 10 -> 0.0654280 (linear 0.0658824)
    // Every one is >= 4.5e-4 away from the linear value, 450x the tolerance.
    for (vol, want, linear) in [
        (5u8, 0.033_395_5, 0.032_941_2),
        (7, 0.047_026_4, 0.046_117_6),
        (8, 0.051_797_2, 0.052_705_9),
        (10, 0.065_428_0, 0.065_882_4),
    ] {
        let got = dc_step(SidModel::Sid6581, vol);
        assert!((got - want).abs() < 1e-6, "vol {vol}: {got}, want {want}");
        assert!(
            (got - linear).abs() > 4e-4,
            "vol {vol}: {got} is still linear VOL/15"
        );
    }
    // Ends unchanged: 0 is silent, 15 is the S2 step 0.5 * G6 = 0.0988235.
    assert_eq!(dc_step(SidModel::Sid6581, 0), 0.0);
    assert!((dc_step(SidModel::Sid6581, 15) - 0.098_823_5).abs() < 1e-6);
}

#[test]
fn volume_dac_6581_scales_a_steady_tone_by_the_table_and_the_8580_stays_linear() {
    // Both chains are level(v) * G * HP[saw] once the transients decay, so
    // rms(v) / rms(15) = level(v): 6581 v 8 -> 0.524138, v 7 -> 0.475862;
    // 8580 v 8 -> 8/15 = 0.533333, v 7 -> 7/15 = 0.466667.
    let r15 = saw_rms(SidModel::Sid6581, 15);
    for (vol, want) in [(8u8, 0.524_138), (7, 0.475_862)] {
        let ratio = saw_rms(SidModel::Sid6581, vol) / r15;
        assert!(
            (ratio - want).abs() < 1e-5,
            "6581 vol {vol}: {ratio}, want {want}"
        );
    }
    let r15 = saw_rms(SidModel::Sid8580, 15);
    for vol in [8u8, 7] {
        let ratio = saw_rms(SidModel::Sid8580, vol) / r15;
        assert!(
            (ratio - vol as f64 / 15.0).abs() < 1e-5,
            "8580 vol {vol}: {ratio}"
        );
    }
}

#[test]
fn volume_digi_is_loud_on_the_6581_and_silent_on_the_8580() {
    // A 4-bit $D418 sample: a triangle ramp through all 16 levels, one level
    // per 25 samples (1764 Hz sample rate, ~55 Hz tone), on a silent chip.
    // 6581: each write steps the mixer DC by 0.5 * d(level) * G6, so the
    // ramp's full swing is 0.5 * G6 = 0.0988 (-20 dBFS) before the 16 Hz
    // blocker; its RMS is well above 0.01. 8580: no mixer DC, exact zero.
    let digi = |m| {
        let mut c = chip(m);
        let mut out = vec![0.0f32; 44_100];
        let ramp: Vec<u8> = (0..16u8).chain((1..15u8).rev()).collect();
        for (k, chunk) in out.chunks_mut(25).enumerate() {
            c.write(REG_MODE_VOL, ramp[k % ramp.len()]);
            c.render(chunk);
        }
        rms(&out[4_410..])
    };
    let (d6, d8) = (digi(SidModel::Sid6581), digi(SidModel::Sid8580));
    assert!(d6 > 0.01, "6581 digi {d6}");
    assert_eq!(d8, 0.0, "8580 digi {d8}");
    // The table shows in the digi's steps. The 7 -> 8 write (the midscale
    // transition, where the INFERRED MSB under-weighting lands) steps
    // 0.5 * (0.524138 - 0.475862) * G6 = 0.5 * 0.048276 * 0.197647
    // = 0.0047708, against 0.5 * (1/15) * G6 = 0.0065882 for a linear DAC.
    // Measure as y[after] - r * y[before], which cancels the blocker's decay:
    // for y[n] = x[n] - x[n-1] + r y[n-1], that difference is exactly dx.
    let r = (-2.0 * std::f64::consts::PI * 16.0 / 44_100.0).exp();
    let mut c = chip(SidModel::Sid6581);
    let mut a = [0.0f32; 1];
    let mut b = [0.0f32; 1];
    c.write(REG_MODE_VOL, 7);
    c.render(&mut a);
    c.write(REG_MODE_VOL, 8);
    c.render(&mut b);
    let step = b[0] as f64 - r * a[0] as f64;
    assert!((step - 0.004_770_8).abs() < 1e-6, "7 -> 8 step {step}");
}

// ---------------------------------------------------------------------------
// API: the revision profile is the one place revision data lives
// ---------------------------------------------------------------------------

use super::revision::{
    profile_6581, DieRevision, RevisionProfile, GT_REF, R3, R4AR, SID6581_REVISION,
};

#[test]
fn sid6581_plays_the_r3_hardware_profile_and_gt_ref_and_r4ar_stay_selectable() {
    // The default 6581 is the measured R3 (the most common revision; it fits
    // SOASC's real-chip recording of Terra Cresta). The GT-reference filter
    // and R4AR are still profiles a chip can be built with
    // (`Chip::with_profile`).
    assert_eq!(SID6581_REVISION, DieRevision::R3);
    assert_eq!(DieRevision::R3.profile(), &R3);
    assert_eq!(profile_6581(), &R3);
    assert_eq!(DieRevision::GtRef.profile(), &GT_REF);
    assert_eq!(DieRevision::R4AR.profile(), &R4AR);
    assert_eq!(R4AR.revision, DieRevision::R4AR);
    assert_eq!(GT_REF.revision, DieRevision::GtRef);
}

#[test]
fn r4ar_cutoff_is_the_s512_measured_curve() {
    // GENERIC-6581 (research notes): the R4AR profile carries S5.12 R2's
    // anchors unchanged, and the filter reads its anchors from the profile.
    let p: &RevisionProfile = &R4AR;
    assert_eq!(
        p.cutoff_anchors_lo,
        [
            (0, 220.0),
            (0x200, 420.0),
            (0x300, 1_600.0),
            (0x3FF, 6_000.0)
        ]
    );
    assert_eq!(
        p.cutoff_anchors_hi,
        [
            (0x400, 4_600.0),
            (0x500, 9_500.0),
            (0x600, 14_500.0),
            (0x7FF, 18_000.0)
        ]
    );
    // The filter's anchor consts follow the default profile (R3).
    assert_eq!(filter::CUTOFF_ANCHORS_6581_LO, R3.cutoff_anchors_lo);
    assert_eq!(filter::CUTOFF_ANCHORS_6581_HI, R3.cutoff_anchors_hi);
    // R4AR's own curve, asked for by profile, is unchanged.
    for &(reg, hz) in p.cutoff_anchors_lo.iter().chain(p.cutoff_anchors_hi.iter()) {
        assert_eq!(filter::cutoff_hz_6581_with(p, reg), hz, "reg {reg:#05x}");
    }
    // S5.12 hand point: sqrt(420 * 1600) = 819.756 Hz at 0x280.
    assert!((filter::cutoff_hz_6581_with(p, 0x280) - 819.756).abs() < 0.01);
}

#[test]
fn r4ar_output_dc_and_gain_are_s2s_values() {
    // Voice DC 0.25, mixer DC 0.5 (S2, INFERRED); gain 0.28 * 3 / (3 * 1.25
    // + 0.5) = 0.84 / 4.25 = 0.197647, and the chip constants are aliases.
    assert_eq!(R4AR.voice_dc, 0.25);
    assert_eq!(R4AR.mix_dc, 0.5);
    assert!((R4AR.chip_gain(chip::CHIP_GAIN) - G6).abs() < 1e-15);
    assert_eq!(chip::MIX_DC_6581, R4AR.mix_dc);
    assert_eq!(voice::VOICE_DC_6581, R4AR.voice_dc);
    assert_eq!(chip::CHIP_GAIN_6581, R4AR.chip_gain(chip::CHIP_GAIN));
}

#[test]
fn r4ar_volume_table_is_monotonic_nonlinear_and_exact_at_the_ends() {
    // Weights [1, 2, 3.9, 7.6] / 14.5 (header). Ends exact, so every
    // full-volume render (and S2's VOL 15 step test) is unchanged.
    assert_eq!(R4AR.volume_bit_weights, [1.0, 2.0, 3.9, 7.6]);
    let t = R4AR.volume_table();
    assert_eq!(t[0], 0.0);
    assert_eq!(t[15], 1.0);
    for v in 1..16 {
        assert!(t[v] > t[v - 1], "not monotonic at {v}");
        assert_eq!(t[v], R4AR.volume_level(v as u8));
    }
    for (v, want) in [
        (5usize, 4.9 / 14.5),
        (7, 6.9 / 14.5),
        (8, 7.6 / 14.5),
        (10, 9.6 / 14.5),
    ] {
        assert!((t[v] - want).abs() < 1e-12, "level({v}) {}", t[v]);
    }
    // Nonlinear: the largest deviation from v/15 is 0.0092 at 7 and 8, and
    // it stays under 0.01 of full scale (a slight nonlinearity, not a remap).
    let dev = (0..16)
        .map(|v| (t[v] - v as f64 / 15.0).abs())
        .fold(0.0, f64::max);
    assert!((dev - (6.9 / 14.5 - 7.0 / 15.0)).abs() < 1e-12, "{dev}");
    assert!(dev > 0.009 && dev < 0.01, "{dev}");
    // The chip uses the profile's table on the 6581 and v/15 on the 8580.
    let (c6, c8) = (chip(SidModel::Sid6581), chip(SidModel::Sid8580));
    for v in 0..16u8 {
        assert_eq!(c6.volume_level(v), t[v as usize]);
        assert_eq!(c8.volume_level(v), v as f64 / 15.0);
    }
}
