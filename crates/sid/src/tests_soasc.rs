//! The measured 6581 profiles, `R2`, `R3` and `R4` (SOASC real-chip recordings,
//! `.ai/sid-soasc/NOTES.md`): their cutoff curves and the per-profile
//! ceiling that lets them open past GoatTracker's 4 kHz.

use super::filter::{cutoff_hz_6581_with, CUTOFF_CEILING_DELTA_HZ};
use super::revision::{DieRevision, RevisionProfile, GT_REF, R2, R3, R4, R4AR};
use super::*;

/// Cutoff at FC_HI `hi` with FC_LO 0, the register SIDBENCH swept.
fn at_hi(p: &RevisionProfile, hi: u16) -> f64 {
    cutoff_hz_6581_with(p, hi << 3)
}

#[test]
fn the_measured_chips_are_revisions_a_chip_can_take() {
    assert_eq!(DieRevision::R2.profile(), &R2);
    assert_eq!(R2.revision, DieRevision::R2);
    assert_eq!(DieRevision::R3.profile(), &R3);
    assert_eq!(DieRevision::R4.profile(), &R4);
    assert_eq!(R3.revision, DieRevision::R3);
    assert_eq!(R4.revision, DieRevision::R4);
    let c = Chip::with_profile(SidModel::Sid6581, DEFAULT_SAMPLE_RATE, &R3).unwrap();
    assert_eq!(c.filter().cutoff(), cutoff_hz_6581_with(&R3, 0));
}

#[test]
fn r4_hits_the_sidbench_measurement() {
    // LP/HP crossover of SOASC's R4 sweeps, median of three voices, Hz.
    // The anchors are a smoothed fit: within 10% of each raw point.
    for (hi, hz) in [
        (14u16, 297.0),
        (62, 294.0),
        (110, 452.0),
        (146, 1_193.0),
        (170, 3_689.0),
        (194, 8_276.0),
        (218, 13_196.0),
    ] {
        let got = at_hi(&R4, hi);
        assert!((got / hz - 1.0).abs() < 0.10, "FC_HI {hi}: {got} vs {hz}");
    }
}

#[test]
fn r2_hits_its_sidbench_measurement() {
    // LP/HP crossover of SOASC's R2 sweeps, Hz. R2 is R4's curve read 680
    // steps higher: 6.5% off on average, 12% at worst (FC_HI 104), below 15 kHz.
    for (hi, hz) in [
        (14u16, 376.0),
        (50, 879.0),
        (68, 1_783.0),
        (86, 3_785.0),
        (104, 6_908.0),
        (122, 11_291.0),
        (140, 13_773.0),
    ] {
        let got = at_hi(&R2, hi);
        assert!((got / hz - 1.0).abs() < 0.15, "FC_HI {hi}: {got} vs {hz}");
    }
}

#[test]
fn measured_curves_rise_without_the_step_at_0x400() {
    for p in [&R2, &R3, &R4] {
        let mut last = 0.0;
        for reg in 0..0x800u16 {
            let hz = cutoff_hz_6581_with(p, reg);
            assert!(
                hz >= last * 0.995,
                "{:?} reg {reg:#05x}: {hz} after {last}",
                p.revision
            );
            last = hz;
        }
    }
}

#[test]
fn r3_is_r4_shifted_brighter_by_880() {
    // R3's fit: R4's curve read 880 register steps higher. Checked where both
    // are inside the measured range, allowing the anchors' interpolation.
    for reg in (0..0x480u16).step_by(0x20) {
        let want = cutoff_hz_6581_with(&R4, reg + 880);
        let got = cutoff_hz_6581_with(&R3, reg);
        assert!(
            (got / want - 1.0).abs() < 0.10,
            "reg {reg:#05x}: {got} vs {want}"
        );
    }
    // The order the recordings showed at every register: R4 darkest, R3 brightest.
    for reg in (0x100..0x400u16).step_by(0x40) {
        let (r2, r3, r4) = (
            cutoff_hz_6581_with(&R2, reg),
            cutoff_hz_6581_with(&R3, reg),
            cutoff_hz_6581_with(&R4, reg),
        );
        assert!(r3 > r2 && r2 > r4 * 1.5, "reg {reg:#05x}: {r2} {r3} {r4}");
    }
}

#[test]
fn only_the_measured_profiles_open_past_the_gt_ceiling() {
    let top = |m, p| {
        let mut c = Chip::with_profile(m, DEFAULT_SAMPLE_RATE, p).unwrap();
        c.write(0x15, 0x07);
        c.write(0x16, 0xFF);
        c.filter().effective_cutoff()
    };
    for p in [&GT_REF, &R4AR] {
        assert_eq!(p.cutoff_ceiling_hz, CUTOFF_CEILING_DELTA_HZ);
        assert_eq!(top(SidModel::Sid6581, p), CUTOFF_CEILING_DELTA_HZ);
    }
    for p in [&R2, &R3, &R4] {
        assert_eq!(p.cutoff_ceiling_hz, f64::INFINITY);
        let want = cutoff_hz_6581_with(p, 0x7FF).min(DEFAULT_SAMPLE_RATE * 0.49);
        assert_eq!(top(SidModel::Sid6581, p), want);
        assert!(want > 15_000.0);
    }
    // The 8580 keeps the ceiling whatever profile the chip was built with.
    assert_eq!(top(SidModel::Sid8580, &R3), CUTOFF_CEILING_DELTA_HZ);
}

#[test]
fn measured_profiles_take_everything_but_the_cutoff_from_gt_ref() {
    for p in [&R2, &R3, &R4] {
        assert_eq!(p.resonance, GT_REF.resonance);
        assert_eq!(p.sat, GT_REF.sat);
        assert_eq!(p.gain_trim, GT_REF.gain_trim);
        assert_eq!(p.volume_bit_weights, GT_REF.volume_bit_weights);
        assert_eq!(p.voice_dc, GT_REF.voice_dc);
        assert_eq!(p.mix_dc, GT_REF.mix_dc);
    }
}

#[test]
fn revision_names_round_trip() {
    for r in DieRevision::ALL {
        assert_eq!(DieRevision::from_name(r.name()), Some(r));
    }
    assert_eq!(DieRevision::from_name("R3"), None);
}

/// A short filtered noise burst on voice 1, cutoff `fc_hi`.
fn noise_through_filter(c: &mut Chip, fc_hi: u8, n: usize) -> Vec<f32> {
    for (reg, val) in [
        (0x01, 0x40),
        (0x05, 0x00),
        (0x06, 0xF0),
        (0x16, fc_hi),
        (0x17, 0x01),
        (0x18, 0x1F),
        (0x04, 0x81),
    ] {
        c.write(reg, val);
    }
    let mut out = vec![0.0; n];
    c.render(&mut out);
    out
}

#[test]
fn switching_a_fresh_chip_is_building_it_with_that_profile() {
    for p in [&R2, &R3, &R4, &R4AR, &GT_REF] {
        let mut built = Chip::with_profile(SidModel::Sid6581, DEFAULT_SAMPLE_RATE, p).unwrap();
        let mut switched = Chip::with_profile(SidModel::Sid6581, DEFAULT_SAMPLE_RATE, &R4).unwrap();
        switched.set_profile(p);
        assert_eq!(switched.profile(), p);
        assert_eq!(switched.tap_full_scale(), built.tap_full_scale());
        assert_eq!(
            noise_through_filter(&mut switched, 0x40, 2048),
            noise_through_filter(&mut built, 0x40, 2048)
        );
    }
}

#[test]
fn a_switch_mid_play_moves_the_cutoff_at_once_and_keeps_the_registers() {
    let mut c = Chip::with_profile(SidModel::Sid6581, DEFAULT_SAMPLE_RATE, &R4).unwrap();
    noise_through_filter(&mut c, 0x40, 512);
    assert_eq!(
        c.filter().effective_cutoff(),
        cutoff_hz_6581_with(&R4, 0x40 << 3)
    );
    c.set_profile(&R3);
    assert_eq!(
        c.filter().effective_cutoff(),
        cutoff_hz_6581_with(&R3, 0x40 << 3)
    );
    assert_eq!(c.written(0x16), 0x40);
    // An 8580 keeps its own filter whatever it is told.
    let mut e = Chip::with_profile(SidModel::Sid8580, DEFAULT_SAMPLE_RATE, &GT_REF).unwrap();
    let before = (e.tap_full_scale(), e.filter().cutoff());
    e.set_profile(&R3);
    assert_eq!((e.tap_full_scale(), e.filter().cutoff()), before);
}
