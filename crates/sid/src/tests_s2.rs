//! S2 tests: the render regression pins and the 6581 character pass.
//!
//! Same discipline as `tests.rs`: hand derivations first, in the comment
//! above each assertion, and every test drives the real chip through
//! `Chip::new` + register `write`/`read` + `clock`/`render`.
//!
//! The render pins are the exception to "hand-derived": they are captured
//! output, not derivations. `PIN_8580_*` was captured on the UNMODIFIED S1
//! tree (main tip 4f76a8d3) before any S2 edit, so it passing bit-exactly
//! after S2 is the proof that S2 left the 8580 path untouched.

use super::chip::{REG_FC_HI, REG_FC_LO, REG_MODE_VOL, REG_RES_FILT};
use super::revision::R4AR;
use super::waveform::{GATE, NOISE, PULSE, RING, SAW, SYNC, TEST, TRI};
use super::*;

const V1: u8 = 0x00;
const V2: u8 = 0x07;
const V3: u8 = 0x0E;

// ---------------------------------------------------------------------------
// Render regression pins
// ---------------------------------------------------------------------------

/// Samples in the pinned render: 0.6 s at 44.1 kHz.
const PIN_LEN: usize = 26_460;
/// Every `PIN_STRIDE`-th sample is pinned by value (32 values).
const PIN_STRIDE: usize = 826;

/// A fixed register program exercising waveforms, PWM, combined waveforms,
/// ring/sync, noise (incl. lock-up and TEST), the filter (LP/BP, the LP+HP
/// notch, a cutoff jump, resonance), the envelope (attack, decay, sustain,
/// release) and the master volume. Events are (sample index, reg, value);
/// writes land at render-chunk boundaries.
const PIN_PROGRAM: &[(usize, u8, u8)] = &[
    (0, REG_MODE_VOL, 0x3F), // LP|BP, volume 15
    (0, REG_FC_LO, 0x05),
    (0, REG_FC_HI, 0x60),    // fc = 0x305
    (0, REG_RES_FILT, 0xA3), // res 10, FILT1|FILT2
    (0, V1, 0x45),
    (0, V1 + 1, 0x1D), // 7493, A-4
    (0, V1 + 5, 0x08),
    (0, V1 + 6, 0xA6),
    (0, V1 + 4, SAW | GATE),
    (0, V2, 0x88),
    (0, V2 + 1, 0x13), // 5000
    (0, V2 + 2, 0x00),
    (0, V2 + 3, 0x06), // pw 0x600
    (0, V2 + 5, 0x22),
    (0, V2 + 6, 0x84),
    (0, V2 + 4, PULSE | GATE),
    (0, V3, 0xE0),
    (0, V3 + 1, 0x2E), // 12000
    (0, V3 + 5, 0x00),
    (0, V3 + 6, 0xF0),
    (0, V3 + 4, NOISE | GATE),
    (4_410, V2 + 4, SAW | TRI | GATE),
    (4_410, REG_FC_HI, 0x20),
    (8_820, V1 + 4, TRI | RING | SYNC | GATE),
    (8_820, V3 + 4, NOISE | PULSE | GATE),
    (13_230, V1 + 4, TRI | RING | SYNC),
    (13_230, REG_MODE_VOL, 0x5F), // LP|HP notch
    (13_230, REG_FC_HI, 0xFF),
    (17_640, V2 + 4, 0x00), // waveform 0, gate off
    (17_640, V3 + 4, TEST | PULSE | GATE),
    (22_050, V3 + 4, PULSE | SAW | GATE),
    (22_050, REG_MODE_VOL, 0x1A),
];

fn render_pin_program(model: SidModel) -> Vec<f32> {
    let mut c = Chip::with_profile(model, DEFAULT_SAMPLE_RATE, &R4AR).expect("model constructs");
    c.set_gain_trim(1.0); // the pins are at the S1/S2 reference level (S5.16 trims the app's)
    c.set_filter_sign(1.0); // ... and polarity (S5.16 inverts the filtered path)
    let mut out = vec![0.0f32; PIN_LEN];
    let mut at = 0;
    for &(t, reg, val) in PIN_PROGRAM {
        if t > at {
            c.render(&mut out[at..t]);
            at = t;
        }
        c.write(reg, val);
    }
    c.render(&mut out[at..]);
    out
}

/// FNV-1a 64 over every sample's f32 bit pattern.
fn fnv1a(x: &[f32]) -> u64 {
    x.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, v| {
        v.to_bits()
            .to_le_bytes()
            .iter()
            .fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
    })
}

fn pinned(x: &[f32]) -> Vec<u32> {
    x.iter().step_by(PIN_STRIDE).map(|v| v.to_bits()).collect()
}

fn check_pin(model: SidModel, hash: u64, values: &[u32]) {
    let out = render_pin_program(model);
    assert!(out.iter().all(|v| v.is_finite()));
    let got = pinned(&out);
    assert_eq!(
        (fnv1a(&out), got.as_slice()),
        (hash, values),
        "{model:?} render changed; actual hash {:#018x}, values {:#010x?}",
        fnv1a(&out),
        got
    );
}

/// Captured on the unmodified S1 tree (4f76a8d3), before any S2 edit, and
/// re-captured once in S5.12 for the 4 kHz cutoff ceiling
/// (`filter::CUTOFF_CEILING_DELTA_HZ`): the program's fc 0x305 (8580 map
/// ~4550 Hz) and fc 0x7F8+ now clamp. With the ceiling disabled the old pin
/// still reproduced bit-exactly, so the ceiling is the whole change.
/// Re-captured again in S5.12 R2 for the fitted combined-waveform levels
/// (the program's saw+tri at 4_410 and pulse+saw at 22_050; values 0-5,
/// before 4_410, are unchanged). With the fitted thresholds set back to
/// plain AND / 1536 and the old 6581 cutoff map, both old pins reproduced
/// bit-exactly, so those two fixes are the whole change.
/// Re-captured for the 8580 resonance map (reSID's 0.707 + res / 15; the
/// program's res 10 goes from Q 1.68 to 1.37). With S0's exponential map
/// back, the previous pin reproduced bit-exactly.
const PIN_8580_HASH: u64 = 0x90e5_528f_4cec_f367;
const PIN_8580_VALUES: [u32; 33] = [
    0x3a29691d, 0xbda35198, 0x39be78be, 0x3e83b39b, 0x3e91955f, 0xbe075d23, 0xbe6b2b6a, 0x3e835faf,
    0x3ee2b9c6, 0x3dac74b7, 0x3bc93b22, 0xbd050b5c, 0xbd3393fb, 0xbe24c18f, 0x3e72bbc0, 0x3e9a318a,
    0xbbbae05b, 0x3e7fb744, 0x3d8127a3, 0xbdb4b7d8, 0x3e69c45d, 0xbdc93b0d, 0x3ddc051c, 0x3d15e992,
    0x3c8f2d73, 0x3bb6c5d0, 0x3b241134, 0xbe9a91ea, 0xbdd652f6, 0xbdb64946, 0xbdb2fe7b, 0x3dc74b62,
    0xbdc06f45,
];

#[test]
fn pin_8580_render_is_bit_identical_to_s1() {
    check_pin(SidModel::Sid8580, PIN_8580_HASH, &PIN_8580_VALUES);
}

/// Captured from S2's own 6581 on first green (NOT a derivation, NOT a
/// hardware reference): future refactors must keep it bit-exact. Re-captured
/// in S5.12 for the 4 kHz cutoff ceiling (the fc 0x7F8+ write at 13_230;
/// values 0-16 are unchanged), same check as the 8580 pin. Re-captured
/// again in S5.12 R2 for the fitted combined-waveform levels and the
/// measured-anchor cutoff map (the fc 0x305 write at 0 moves 773.5 Hz ->
/// 1642.0 Hz, so every value changes); same disable-and-reproduce check.
/// Re-captured in S5.15 for the R4AR volume DAC table (the program's VOL 10
/// write at 22_050 now plays level 0.662069, not 10/15; values 0-26, before
/// it, are unchanged). With the profile's bit weights set to the linear
/// [1, 2, 4, 8] the old pin reproduced bit-exactly
/// (`.ai/checks-s515-pin-linear-reproduce.txt`), so the table is the whole
/// change.
const PIN_6581_HASH: u64 = 0x2d13_44bc_eadb_4e1f;
const PIN_6581_VALUES: [u32; 33] = [
    0x3dca67b2, 0x3befbbea, 0x3cddd8cd, 0x3e42cdc6, 0x3e68d0f0, 0xbdad0da8, 0xbe455549, 0x3d82138f,
    0x3e990a41, 0x3da4a948, 0xbc0520f5, 0xbe04e05e, 0xbd01f36c, 0xbcbd8bb0, 0xbcbb2204, 0x3ce2f218,
    0x3cc519f6, 0x3d089adc, 0xbc2d3190, 0x3d9109f0, 0x3c62e24a, 0x3d8f135a, 0x3e0556cb, 0x3cfaf9e0,
    0x3c39bce7, 0x3b3f7d7a, 0x3a97a591, 0xbe5cf034, 0xbd200d68, 0xbc44d81b, 0xbbfaac70, 0xbbe32eac,
    0xbbfa8232,
];

#[test]
fn pin_6581_render_is_bit_identical_to_s2() {
    check_pin(SidModel::Sid6581, PIN_6581_HASH, &PIN_6581_VALUES);
}

// ---------------------------------------------------------------------------
// Per-instance model switch
// ---------------------------------------------------------------------------

/// A chip on the R4AR profile: these are S2's pins, and the default 6581 is
/// the GT-reference filter since S5.16 (an 8580 ignores the profile).
fn chip(m: SidModel) -> Chip {
    let mut c = Chip::with_profile(m, DEFAULT_SAMPLE_RATE, &R4AR).expect("both models construct");
    c.set_gain_trim(1.0); // reference level; see `check_pin`
    c.set_filter_sign(1.0);
    c
}

fn freq(c: &mut Chip, v: u8, f: u16) {
    c.write(v, f as u8);
    c.write(v + 1, (f >> 8) as u8);
}

fn fc(c: &mut Chip, reg: u16) {
    c.write(REG_FC_LO, (reg & 7) as u8);
    c.write(REG_FC_HI, (reg >> 3) as u8);
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64).sqrt()
}

#[test]
fn two_models_side_by_side_share_no_state() {
    // Interleave a 6581 and an 8580 chunk by chunk through the same
    // register program. The 8580 must still reproduce its S1 pin, and the
    // 6581 its standalone render: no hidden shared state between instances.
    let mut a = chip(SidModel::Sid8580);
    let mut b = chip(SidModel::Sid6581);
    let mut oa = vec![0.0f32; PIN_LEN];
    let mut ob = vec![0.0f32; PIN_LEN];
    let mut at = 0;
    for &(t, reg, val) in PIN_PROGRAM {
        if t > at {
            a.render(&mut oa[at..t]);
            b.render(&mut ob[at..t]);
            at = t;
        }
        a.write(reg, val);
        b.write(reg, val);
    }
    a.render(&mut oa[at..]);
    b.render(&mut ob[at..]);
    assert_eq!(fnv1a(&oa), PIN_8580_HASH);
    assert_eq!(fnv1a(&ob), fnv1a(&render_pin_program(SidModel::Sid6581)));
}

#[test]
fn same_program_both_models_sane_and_different() {
    let o8 = render_pin_program(SidModel::Sid8580);
    let o6 = render_pin_program(SidModel::Sid6581);
    for (o, m) in [(&o8, "8580"), (&o6, "6581")] {
        let peak = o.iter().fold(0.0f32, |p, v| p.max(v.abs()));
        // S1 headroom lets resonance push past 1.0 (8580 peaks ~1.15 here).
        assert!(peak > 0.05 && peak < 2.0, "{m}: peak {peak}");
        assert!(rms(o) > 0.02, "{m}: rms {}", rms(o));
    }
    let diff: Vec<f32> = o8.iter().zip(&o6).map(|(a, b)| a - b).collect();
    assert!(
        rms(&diff) > 0.3 * rms(&o8),
        "models too alike: {}",
        rms(&diff)
    );
}

#[test]
fn each_instance_uses_its_own_filter_maps() {
    // Same register writes; each chip's filter reports its own map.
    // Hand values from filter.rs: reg 0x200 -> 8580 3023.96 Hz, 6581 420 Hz
    // (a measured anchor since S5.12 R2; S2's logistic gave 383.63). Res 15 -> 8580 Q 0.707 + 15 / 15 = 1.707
    // (reSID's map; S0's 0.707 * 2^(res / 8) gave 2.593287); 6581 1.681539.
    let mut a = chip(SidModel::Sid8580);
    let mut b = chip(SidModel::Sid6581);
    for c in [&mut a, &mut b] {
        fc(c, 0x200);
        c.write(REG_RES_FILT, 0xF1);
    }
    assert_eq!(a.filter().model(), SidModel::Sid8580);
    assert_eq!(b.filter().model(), SidModel::Sid6581);
    assert!((a.filter().cutoff() - 3023.96).abs() < 0.01);
    assert!((b.filter().cutoff() - 420.0).abs() < 1e-9);
    assert!((a.filter().q() - 1.707).abs() < 1e-12);
    assert!((b.filter().q() - 1.681539).abs() < 1e-6);
    for c in [&a, &b] {
        for i in 0..3 {
            assert_eq!(c.voice(i).model(), c.model());
        }
    }
}

// ---------------------------------------------------------------------------
// 6581 filter: level-dependent peak gain through the chip
// ---------------------------------------------------------------------------

/// Voice 1 A-4 saw into the LP filter at res 15, sustain `s`, steady
/// state RMS over 0.5 s.
fn filtered_saw_rms(m: SidModel, s: u8) -> f64 {
    let mut c = chip(m);
    c.write(REG_MODE_VOL, 0x1F);
    // 6581 ~882 Hz (2nd harmonic): 420 * (1600/420)^(142/256) (S5.12 R2
    // map; S2 used 0x32B, which the logistic put at ~880 Hz). 8580 ~3.85 kHz.
    fc(&mut c, 0x28E);
    c.write(REG_RES_FILT, 0xF1);
    freq(&mut c, V1, 7493);
    c.write(V1 + 5, 0x00);
    c.write(V1 + 6, s << 4);
    c.write(V1 + 4, SAW | GATE);
    let mut out = vec![0.0f32; 44_100];
    c.render(&mut out);
    rms(&out[22_050..])
}

#[test]
fn filter_6581_peak_gain_depends_on_level_the_8580_does_not() {
    // Sustain 15 -> level 255, sustain 1 -> level 17: the voice amplitude
    // ratio is 255/17 = 15. The 8580 chain (DAC product, SVF, volume, DC
    // blocker) is linear, so its output RMS ratio is exactly 15. On the
    // 6581 the voice DC (0.25 * amp) is removed by the blocker and the
    // saw part is linear up to the filter, whose band-pass state is
    // soft-limited at 1.0: the loud note is compressed, so its ratio falls
    // below 15 (derived direction). The size is MEASURED, not derived:
    // first run gave 11.225 (the loud note 2.5 dB compressed), pinned
    // +-0.1 so a retune of SAT/Q shows up here.
    let r8 = filtered_saw_rms(SidModel::Sid8580, 15) / filtered_saw_rms(SidModel::Sid8580, 1);
    assert!((r8 - 15.0).abs() < 1e-3, "8580 {r8}");
    let r6 = filtered_saw_rms(SidModel::Sid6581, 15) / filtered_saw_rms(SidModel::Sid6581, 1);
    assert!(r6 < 15.0 * 0.9, "6581 {r6}");
    assert!((r6 - 11.225).abs() < 0.1, "6581 {r6}");
}

// ---------------------------------------------------------------------------
// 6581 combined waveforms and waveform 0
// ---------------------------------------------------------------------------

/// Voice 3 at freq 0x1000 from acc 0: after n cycles acc = n * 0x1000, so
/// n = 0x600 -> acc 0x600000, n = 0xF00 -> acc 0xF00000.
fn osc3_at(m: SidModel, control: u8, pw: u16, n: u64) -> u8 {
    let mut c = chip(m);
    freq(&mut c, V3, 0x1000);
    c.write(V3 + 2, pw as u8);
    c.write(V3 + 3, (pw >> 8) as u8);
    c.write(V3 + 4, control);
    c.clock_cycles(n);
    c.read(super::chip::REG_OSC3)
}

#[test]
fn combined_waveforms_6581_are_attenuated_through_osc3() {
    // acc 0x600000: saw 0x600, tri 0xC00, pulse(pw 0x400) 0xFFF.
    //   saw+tri:   AND 0x400, isolated bit 10 (pull 2048) -> 0 on both
    //              models (S5.12 R2: the 8580 pulls too; waveform.rs).
    //   pulse+saw: AND 0x600 -> 0 on both (waveform.rs hand values).
    // acc 0xF00000: saw 0xF00, pulse high: AND 0xF00. Bit 8 sees zeros
    //   7..0 at d 1..8 = 1024+512+...+8 = 2040; bit 9 d 2..9 = 1020; bit 10
    //   510; bit 11 255. 8580 pulse+saw threshold 1314: bits 9-11 kept ->
    //   0xE00 -> OSC3 0xE0. 6581 threshold 167: every bit gone -> 0x00.
    // acc 0xFF0000: AND 0xFF0. Bit 8 sees zeros 3..0 at d 5..8 = 120 < 167
    //   (kept), bit 7 240 (gone) -> 6581 0xF00 -> OSC3 0xF0.
    let (m8, m6) = (SidModel::Sid8580, SidModel::Sid6581);
    assert_eq!(osc3_at(m8, SAW | TRI, 0, 0x600), 0x00);
    assert_eq!(osc3_at(m6, SAW | TRI, 0, 0x600), 0x00);
    assert_eq!(osc3_at(m8, PULSE | SAW, 0x400, 0x600), 0x00);
    assert_eq!(osc3_at(m6, PULSE | SAW, 0x400, 0x600), 0x00);
    assert_eq!(osc3_at(m8, PULSE | SAW, 0x400, 0xF00), 0xE0);
    assert_eq!(osc3_at(m6, PULSE | SAW, 0x400, 0xF00), 0x00);
    assert_eq!(osc3_at(m6, PULSE | SAW, 0x400, 0xFF0), 0xF0);
    // Single waveforms: identical on both models.
    for ctl in [SAW, TRI, PULSE] {
        assert_eq!(
            osc3_at(m8, ctl, 0x400, 0x600),
            osc3_at(m6, ctl, 0x400, 0x600)
        );
    }
}

#[test]
fn combined_waveform_6581_is_a_bit_subset_of_the_8580_every_cycle() {
    // The pull-down only clears bits of the AND, so on the same phase the
    // 6581 OSC3 is a bit subset of the 8580's, and over a period it is
    // strictly quieter. Full saw period at freq 0x1000 = 4096 cycles.
    for ctl in [SAW | TRI, PULSE | SAW, PULSE | TRI, PULSE | SAW | TRI] {
        let mut a = chip(SidModel::Sid8580);
        let mut b = chip(SidModel::Sid6581);
        let (mut sa, mut sb) = (0u32, 0u32);
        for c in [&mut a, &mut b] {
            freq(c, V3, 0x1000);
            c.write(V3 + 3, 0x04); // pw 0x400
            c.write(V3 + 4, ctl);
        }
        for _ in 0..4096 {
            a.clock();
            b.clock();
            let (x, y) = (a.read(super::chip::REG_OSC3), b.read(super::chip::REG_OSC3));
            assert_eq!(
                y & !x,
                0,
                "ctl {ctl:#04x}: 6581 {y:#04x} not within 8580 {x:#04x}"
            );
            sa += x as u32;
            sb += y as u32;
        }
        assert!(sb < sa, "ctl {ctl:#04x}: {sb} vs {sa}");
    }
}

#[test]
fn waveform_zero_fades_on_the_6581_and_test_does_not_touch_it() {
    // Voice 3 saw at 0x1000; at cycle 1536 acc = 0x600000 (OSC3 0x60).
    // Waveform 0 written at 1536: the DAC holds 0x600. 6581: the age
    // counts cycles 1537.. and reaches 65 536 on cycle 1536 + 65 536 =
    // 67 072, when the held value drops to 0. TEST (still no waveform)
    // written at 30 000 neither drives the DAC nor restarts the count.
    // 8580: S1's hold, forever.
    let r = super::chip::REG_OSC3;
    for m in [SidModel::Sid8580, SidModel::Sid6581] {
        let mut c = chip(m);
        freq(&mut c, V3, 0x1000);
        c.write(V3 + 4, SAW);
        c.clock_cycles(1536);
        assert_eq!(c.read(r), 0x60);
        c.write(V3 + 4, 0x00);
        c.clock_cycles(30_000 - 1536);
        assert_eq!(c.read(r), 0x60);
        c.write(V3 + 4, TEST);
        c.clock_cycles(67_071 - 30_000);
        assert_eq!(c.read(r), 0x60, "{m:?} one cycle before the fade");
        c.clock();
        let want = if m == SidModel::Sid6581 { 0x00 } else { 0x60 };
        assert_eq!(c.read(r), want, "{m:?} at the fade cycle");
        c.clock_cycles(100_000);
        assert_eq!(c.read(r), want);
        // Release TEST and select saw again: the DAC is driven from acc 0
        // (TEST held it there): 0x600 cycles later acc = 0x600000 again.
        c.write(V3 + 4, SAW);
        c.clock_cycles(0x600);
        assert_eq!(c.read(r), 0x60);
    }
}

#[test]
fn test_pulse_edge_into_waveform_zero() {
    // The test-bit edge: PULSE|TEST forces 0xFFF (OSC3 0xFF). Dropping to
    // TEST alone (waveform 0 + TEST) holds that 0xFFF. The 6581 then fades
    // it to 0 after 65 536 cycles; the 8580 holds it.
    let r = super::chip::REG_OSC3;
    for m in [SidModel::Sid8580, SidModel::Sid6581] {
        let mut c = chip(m);
        c.write(V3 + 3, 0x0F); // pw 0xF00: comparator low at acc 0
        c.write(V3 + 4, PULSE | TEST);
        c.clock_cycles(10);
        assert_eq!(c.read(r), 0xFF);
        c.write(V3 + 4, TEST);
        c.clock_cycles(65_535);
        assert_eq!(c.read(r), 0xFF);
        c.clock();
        assert_eq!(
            c.read(r),
            if m == SidModel::Sid6581 { 0 } else { 0xFF },
            "{m:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 6581 attack shape
// ---------------------------------------------------------------------------

#[test]
fn attack_6581_follows_the_hand_derived_lag_curve() {
    // AD 0x00, SR 0xF0, gate written at cycle 0. Rate period 9: the level
    // is floor(c/9) until 255 at c = 2295 (sustain 15 = 255 holds it).
    // tau = 1.5e-3 / ln 9 * 985 248 = 672.61 cycles. The staircase
    // floor(c/9)/255 is on average the ramp (c - 4)/2295, and a discrete
    // one-pole lags a ramp by (1/alpha - 1) ~= tau - 0.5 = 672.11 = tau'.
    // So, with u = c - 4:
    //   amp(c) ~= (u - tau' (1 - e^(-u/tau'))) / 2295
    //   c = 500:  u 496,  e^-0.73798 = 0.47808 -> (496 - 350.79)/2295 = 0.06327
    //   c = 1000: u 996,  e^-1.48190 = 0.22721 -> (996 - 519.40)/2295 = 0.20767
    //   c = 2295: u 2291, e^-3.40867 = 0.03308 -> (2291 - 649.87)/2295 = 0.71509
    // then target 1: amp(3000) = 1 - (1 - 0.71509) e^(-705/672.61)
    //                          = 1 - 0.28491 * 0.35060 = 0.90011
    // The 8580 applies the level directly: 55/255, 111/255, 1, 1.
    let mut c6 = chip(SidModel::Sid6581);
    let mut c8 = chip(SidModel::Sid8580);
    for c in [&mut c6, &mut c8] {
        c.write(V1 + 5, 0x00);
        c.write(V1 + 6, 0xF0);
        c.write(V1 + 4, GATE);
    }
    for (cyc, want6, want8) in [
        (500u64, 0.06327, 55.0 / 255.0),
        (1000, 0.20767, 111.0 / 255.0),
        (2295, 0.71509, 1.0),
        (3000, 0.90011, 1.0),
    ] {
        c6.clock_cycles(cyc - c6.cycles());
        c8.clock_cycles(cyc - c8.cycles());
        let a6 = c6.voice(0).envelope_amplitude();
        let a8 = c8.voice(0).envelope_amplitude();
        assert!(
            (a6 - want6).abs() < 5e-4,
            "cycle {cyc}: 6581 {a6} vs {want6}"
        );
        assert!((a8 - want8).abs() < 1e-12, "cycle {cyc}: 8580 {a8}");
        // The digital counter is the same on both models.
        assert_eq!(c6.voice(0).envelope_level(), c8.voice(0).envelope_level());
    }
    // The "1.5 ms floor": no rise can be faster than the lag, so even the
    // fastest attack reaches half amplitude later than the 8580's level.
    // 8580: level 128 at c = 1152 (1.17 ms). 6581: (u - tau'(1 -
    // e^(-u/tau')))/2295 = 0.5, i.e. u - tau'(1 - e^(-u/tau')) = 1147.5:
    //   u = 1772: e^-2.63646 = 0.07163 -> 1772 - 623.97 = 1148.03 (just over)
    //   so u ~ 1771.5 -> c ~ 1775.5 (1.80 ms).
    // (First derivation said 1758, a slip in the root-find; the test
    // caught it and the refined bisection above is the value pinned.)
    let mut c6 = chip(SidModel::Sid6581);
    c6.write(V1 + 6, 0xF0);
    c6.write(V1 + 4, GATE);
    let mut half = 0;
    while c6.voice(0).envelope_amplitude() < 0.5 {
        c6.clock();
        half += 1;
    }
    assert!((half as i64 - 1776).abs() <= 3, "{half}");
}

#[test]
fn attack_lag_releases_into_exact_decay_and_release() {
    // AD 0x00, SR 0x80 (sustain 136, release 0). Attack ends at c = 2295
    // with amp 0.715; the level then decays 1 per 9 cycles while amp keeps
    // rising. They meet near c ~ 2675 (d/2295 = 0.285 e^(-d/672.6) at
    // d ~ 380). From then on the amp IS the level/255 (the lag only acts
    // on rises), through the sustain (136 reached at c = 2295 + 119 * 9 =
    // 3366) and the release.
    let mut c = chip(SidModel::Sid6581);
    c.write(V1 + 6, 0x80);
    c.write(V1 + 4, GATE);
    c.clock_cycles(2800);
    for _ in 0..2000 {
        c.clock();
        let v = c.voice(0);
        assert_eq!(v.envelope_amplitude(), v.envelope_level() as f64 / 255.0);
    }
    assert_eq!(c.voice(0).envelope_level(), 136);
    c.write(V1 + 4, 0x00);
    let mut prev = c.voice(0).envelope_amplitude();
    for _ in 0..20_000 {
        c.clock();
        let v = c.voice(0);
        let a = v.envelope_amplitude();
        assert_eq!(a, v.envelope_level() as f64 / 255.0);
        assert!(a <= prev);
        prev = a;
    }
}

#[test]
fn env3_counter_and_rate_table_are_shared_by_both_models() {
    // The 6581 changes only the amplitude after the counter. ENV3 (the
    // digital level) must match cycle for cycle through a full ADSR with
    // several rate nibbles, including the exponential decay and release.
    let mut a = chip(SidModel::Sid8580);
    let mut b = chip(SidModel::Sid6581);
    for c in [&mut a, &mut b] {
        c.write(V3 + 5, 0x35);
        c.write(V3 + 6, 0x76);
        c.write(V3 + 4, GATE | TRI);
    }
    let env3 = super::chip::REG_ENV3;
    for n in 0..600_000u32 {
        if n == 300_000 {
            a.write(V3 + 4, TRI);
            b.write(V3 + 4, TRI);
        }
        a.clock();
        b.clock();
        if n % 97 == 0 {
            assert_eq!(a.read(env3), b.read(env3), "cycle {n}");
        }
    }
    assert_eq!(super::envelope::RATE_PERIODS[0], 9);
    assert_eq!(super::envelope::RATE_PERIODS[15], 31_251);
}

// ---------------------------------------------------------------------------
// 6581 output stage: DC, thump, level
// ---------------------------------------------------------------------------

#[test]
fn volume_write_steps_the_6581_mixer_dc_and_not_the_8580() {
    // Silent chip (no gate: every amp 0, filter unrouted, mode 0). Writing
    // VOL 15 on the 6581 steps x from 0 to MIX_DC * 1 * G6 = 0.5 * 0.197647
    // = 0.0988235, so the DC blocker gives y[n] = 0.0988235 r^n with
    // r = e^(-2 pi 16 / 44100) = 0.9977230: y[441] = 0.0361626.
    // The 8580 has no mixer DC: exactly 0 everywhere.
    use super::chip::{CHIP_GAIN_6581, MIX_DC_6581};
    assert!((CHIP_GAIN_6581 - 0.197_647_058_8).abs() < 1e-9);
    let r = (-2.0 * std::f64::consts::PI * 16.0 / 44_100.0).exp();
    let mut c6 = chip(SidModel::Sid6581);
    let mut c8 = chip(SidModel::Sid8580);
    let mut o6 = vec![0.0f32; 2_000];
    let mut o8 = vec![1.0f32; 2_000];
    c6.write(REG_MODE_VOL, 0x0F);
    c8.write(REG_MODE_VOL, 0x0F);
    c6.render(&mut o6);
    c8.render(&mut o8);
    assert!(o8.iter().all(|&v| v == 0.0));
    assert!((o6[0] as f64 - 0.0988235).abs() < 1e-6, "{}", o6[0]);
    assert!((o6[441] as f64 - 0.0361626).abs() < 1e-6, "{}", o6[441]);
    for (n, &y) in o6.iter().enumerate() {
        let want = MIX_DC_6581 * CHIP_GAIN_6581 * r.powi(n as i32);
        assert!((y as f64 - want).abs() < 1e-6, "n {n}: {y} vs {want}");
    }
    // A 4-bit "volume digi" (VOL toggled 15/0 every 50 samples) is loud on
    // the 6581 and silent on the 8580.
    let digi = |m| {
        let mut c = chip(m);
        let mut out = vec![0.0f32; 4_410];
        for (k, chunk) in out.chunks_mut(50).enumerate() {
            c.write(REG_MODE_VOL, if k % 2 == 0 { 0x0F } else { 0x00 });
            c.render(chunk);
        }
        rms(&out)
    };
    assert!(
        digi(SidModel::Sid6581) > 0.03,
        "{}",
        digi(SidModel::Sid6581)
    );
    assert_eq!(digi(SidModel::Sid8580), 0.0);
}

#[test]
fn note_on_thump_from_the_6581_voice_dc() {
    // Both chips: VOL 15, settle 1 s (the 6581 mixer-DC step decays as
    // r^44100 = e^-100.5 ~ 0). Then gate a saw at freq 0xFFFF (period 256
    // cycles, so the saw's own mean and its product with the attack cancel
    // to ~0), AD 0x00 SR 0xF0.
    // diff[m] = y6[m]/G6 - y8[m]/G8 = HP[0.25 * amp6] + HP[saw * (amp6 -
    // amp8)]. The second term is the lag difference. After the attack
    // (c = 2295, amp6 = 0.715) it shrinks as 0.285 e^(-(c - 2295)/672.6):
    // at sample 400 (c = 8936) it is 1.5e-5, so compare from there (a
    // first version started at sample 200, where it is still 1e-2, and
    // failed). The first term is the voice-DC thump: a rise of 0.25 with
    // centroid
    //   C = sum_{c>=1} (1 - amp6(c)) = stair + lag
    //     = (2294 - 9 * (254 * 255 / 2) / 255) + (1/alpha - 1)
    //     = (2294 - 1143.0) + 672.11 = 1823.11 cycles = 81.60 samples
    // through the blocker: diff[m] = 0.25 r^(m - 81.60) (spread error
    // ~0.3 %). The 8580 has no voice DC, so this term is 6581-only.
    let g6 = super::chip::CHIP_GAIN_6581;
    let g8 = super::chip::CHIP_GAIN;
    let r = (-2.0 * std::f64::consts::PI * 16.0 / 44_100.0).exp();
    let run = |m| {
        let mut c = chip(m);
        c.write(REG_MODE_VOL, 0x0F);
        let mut settle = vec![0.0f32; 44_100];
        c.render(&mut settle);
        freq(&mut c, V1, 0xFFFF);
        c.write(V1 + 6, 0xF0);
        c.write(V1 + 4, SAW | GATE);
        let mut out = vec![0.0f32; 3_000];
        c.render(&mut out);
        out
    };
    let (o6, o8) = (run(SidModel::Sid6581), run(SidModel::Sid8580));
    for m in 400..3_000 {
        let diff = o6[m] as f64 / g6 - o8[m] as f64 / g8;
        let want = 0.25 * r.powf(m as f64 - 81.60);
        assert!(
            (diff - want).abs() < 0.01 * want + 2e-5,
            "m {m}: {diff} vs {want}"
        );
    }
    // Size in output units: 0.25 * G6 = 0.0494 at the rise (-26 dBFS of
    // low-frequency bump on every note-on from silence), decayed at
    // m = 400 to 0.0494 * r^318.4 = 0.0494 * 0.48393 = 0.02391.
    let low = o6[400] as f64 - o8[400] as f64 * g6 / g8;
    assert!((low - 0.02391).abs() < 0.0003, "{low}");
}

#[test]
fn level_balance_6581_vs_8580_is_the_derived_gain_ratio() {
    // Unfiltered steady A-4 saw, measured 1 s after note-on (every DC
    // transient has decayed by r^44100 ~ e^-100). Both chains are then
    // G * HP[saw], so the RMS ratio is G6/G8 = 0.197647/0.28 = 0.705882
    // (-3.03 dB).
    let run = |m| {
        let mut c = chip(m);
        c.write(REG_MODE_VOL, 0x0F);
        freq(&mut c, V1, 7493);
        c.write(V1 + 6, 0xF0);
        c.write(V1 + 4, SAW | GATE);
        let mut out = vec![0.0f32; 88_200];
        c.render(&mut out);
        rms(&out[44_100..])
    };
    let ratio = run(SidModel::Sid6581) / run(SidModel::Sid8580);
    assert!((ratio - 0.705882).abs() < 1e-5, "{ratio}");
    assert!((20.0 * ratio.log10() + 3.0254).abs() < 1e-3);
}
