//! S1 golden tests. Every expected value below was derived by hand FIRST, in
//! the comment above its assertion, then the code was pinned to it (the S0
//! spike's discipline). All tests drive the real chip through its public
//! surface: `Chip::new`, register `write`/`read`, `clock`/`clock_cycles`,
//! `render`, and the read-only `voice(i)` accessors. No hand-built voice or
//! envelope state is used.
//!
//! Shared facts used throughout:
//! - Power-on: every accumulator 0, every envelope rate counter 0, noise
//!   LFSR 0x7FFFF8. `run_to(c, n)` clocks until exactly n cycles have run
//!   since power-on. A register written "at cycle n" lands between cycle n
//!   and cycle n+1.
//! - Accumulator after c cycles at constant freq f, from 0: (f * c) mod 2^24.
//!   MSB = 0x800000 = 8_388_608; 2^24 = 16_777_216.
//! - Voice register bases: voice 1 = $00, voice 2 = $07, voice 3 = $0E.
//!   Sync/ring sources: voice 1 <- 3, voice 2 <- 1, voice 3 <- 2.

use super::chip::{
    source_of, REG_ENV3, REG_FC_HI, REG_FC_LO, REG_MODE_VOL, REG_OSC3, REG_RES_FILT, VOICE3_OFF,
};
use super::envelope::Stage;
use super::filter::{BP, HP, LP};
use super::waveform::{triangle, GATE, NOISE, PULSE, RING, SAW, SYNC, TEST, TRI};
use super::*;
use std::f64::consts::PI;

const V1: u8 = 0x00;
const V2: u8 = 0x07;
const V3: u8 = 0x0E;
const A4: u16 = 7493;

fn chip() -> Chip {
    Chip::new(SidModel::Sid8580).expect("8580 is implemented")
}

fn freq(c: &mut Chip, v: u8, f: u16) {
    c.write(v, f as u8);
    c.write(v + 1, (f >> 8) as u8);
}

fn pw(c: &mut Chip, v: u8, p: u16) {
    c.write(v + 2, p as u8);
    c.write(v + 3, (p >> 8) as u8);
}

fn ctrl(c: &mut Chip, v: u8, x: u8) {
    c.write(v + 4, x);
}

fn adsr(c: &mut Chip, v: u8, ad: u8, sr: u8) {
    c.write(v + 5, ad);
    c.write(v + 6, sr);
}

fn acc(c: &Chip, i: usize) -> u32 {
    c.voice(i).accumulator()
}

fn run_to(c: &mut Chip, cycle: u64) {
    assert!(c.cycles() <= cycle, "already at {} > {cycle}", c.cycles());
    c.clock_cycles(cycle - c.cycles());
}

fn render(c: &mut Chip, secs: f64) -> Vec<f32> {
    let mut out = vec![0.0f32; (secs * c.sample_rate()) as usize];
    c.render(&mut out);
    out
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64).sqrt()
}

fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

/// A gated full-sustain A-4 sawtooth on voice base `v`.
fn saw_a4(c: &mut Chip, v: u8) {
    freq(c, v, A4);
    adsr(c, v, 0x00, 0xF0);
    ctrl(c, v, SAW | GATE);
}

// ---------------------------------------------------------------------------
// Model flag
// ---------------------------------------------------------------------------

#[test]
fn model_flag_both_models_construct_per_instance() {
    // S2 rewrite of S1's refusal test: the 6581 is no longer a stub. Both
    // models construct, each instance keeps its own model, and the sample
    // rate check still refuses for either.
    let c = chip();
    assert_eq!(c.model(), SidModel::Sid8580);
    assert_eq!(c.sample_rate(), 44_100.0);
    let c6 = Chip::new(SidModel::Sid6581).expect("6581 is implemented (S2)");
    assert_eq!(c6.model(), SidModel::Sid6581);
    assert_eq!(
        c.model(),
        SidModel::Sid8580,
        "building a 6581 leaves the 8580 alone"
    );
    for m in [SidModel::Sid8580, SidModel::Sid6581] {
        assert_eq!(m.unimplemented_reason(), None);
        assert!(Chip::with_sample_rate(m, 48_000.0).is_ok());
        assert!(matches!(
            Chip::with_sample_rate(m, 0.0),
            Err(SidError::UnsupportedSampleRate(_))
        ));
    }
}

// ---------------------------------------------------------------------------
// Gate 1a: pitch
// ---------------------------------------------------------------------------

#[test]
fn a4_register_is_7493_at_440_02_hz() {
    // Datasheet: Fout = Fn * 985248 / 2^24.
    // 440 * 16_777_216 / 985_248 = 7_381_975_040 / 985_248 = 7492.5 -> 7493.
    // Back: 7493 * 985_248 = 7_382_463_264. 440 * 2^24 = 7_381_975_040,
    // remainder 488_224 / 16_777_216 = 0.0291, so 440.0291 Hz ("440.02" in
    // the plan and S0 is this value truncated). 1200*log2(440.0291/440) =
    // +0.114 cent. (S0's "+0.08 cent" was an arithmetic slip.)
    assert_eq!(hz_to_freq_reg(440.0), 7493);
    assert_eq!(note_to_freq_reg(69), 7493);
    let hz = freq_reg_to_hz(7493);
    assert!((hz - 440.0291).abs() < 0.0001, "{hz}");
    assert!((cents(hz, 440.0) - 0.114).abs() < 0.001);
}

#[test]
fn notes_from_c1_to_a_sharp_7_are_within_two_cents() {
    // Register quantum = 985_248 / 2^24 = 0.058725 Hz. Rounding is at most
    // half a step, i.e. 1200*log2(1 + 0.5/reg) ~= 865.6/reg cents, so every
    // note whose register is >= 433 is within 2 cents.
    //   C-1 (midi 24) = 32.703 Hz -> 32.703 / 0.058725 = 556.9 -> 557 (>= 433)
    //   A#7 (midi 106) = 3729.31 Hz -> 63_504.6 -> 63_505 (<= 65_535)
    //   B-7 (midi 107) = 3951.07 Hz -> 67_281 > 65_535: clamps to 65_535
    //     = 3848.4 Hz, 1200*log2(3848.4/3951.07) = -45.6 cents: the
    //     16-bit register tops out between A#7 and B-7.
    for midi in 24..=106 {
        let want = 440.0 * 2f64.powf((midi - 69) as f64 / 12.0);
        let c = cents(freq_reg_to_hz(note_to_freq_reg(midi)), want);
        assert!(c.abs() < 2.0, "midi {midi}: {c} cents");
    }
    assert_eq!(note_to_freq_reg(24), 557);
    assert_eq!(note_to_freq_reg(107), 65_535);
    let top = cents(freq_reg_to_hz(65_535), 440.0 * 2f64.powf(38.0 / 12.0));
    assert!((top + 45.6).abs() < 0.1, "{top}");
}

#[test]
fn accumulator_and_osc3_golden_trace() {
    // Voice 3 saw at 7493, no gate (OSC3 reads the oscillator regardless).
    //   c = 1000: acc = 7_493_000; OSC3 = acc >> 16 = 114 (114*65536 =
    //             7_471_104 <= 7_493_000 < 7_536_640)
    //   c = 2239: acc = 2239*7493 = 16_776_827 < 2^24; OSC3 = 255
    //   c = 2240: 16_784_320 - 16_777_216 = 7_104 (first wrap); OSC3 = 0
    let mut c = chip();
    freq(&mut c, V3, A4);
    ctrl(&mut c, V3, SAW);
    run_to(&mut c, 1000);
    assert_eq!(acc(&c, 2), 7_493_000);
    assert_eq!(c.read(REG_OSC3), 114);
    run_to(&mut c, 2239);
    assert_eq!(acc(&c, 2), 16_776_827);
    assert_eq!(c.read(REG_OSC3), 255);
    run_to(&mut c, 2240);
    assert_eq!(acc(&c, 2), 7_104);
    assert_eq!(c.read(REG_OSC3), 0);
}

#[test]
fn a4_wraps_exactly_100_times_in_224_000_cycles() {
    // Period = 2^24 / 7493 = 2239.0496 cycles. The n-th wrap is on cycle
    // ceil(n * 2239.0496): the 100th on 223_905, the 101st on 226_145.
    let mut c = chip();
    freq(&mut c, V1, A4);
    let mut wraps = 0;
    let mut last = 0;
    for _ in 0..224_000 {
        c.clock();
        if acc(&c, 0) < last {
            wraps += 1;
        }
        last = acc(&c, 0);
    }
    assert_eq!(wraps, 100);
}

#[test]
fn rendered_a4_saw_is_440_hz_at_44_1_and_48_khz() {
    // 440.03 Hz -> 440 rising zero crossings per second (+-1 for phase).
    for sr in [44_100.0, 48_000.0] {
        let mut c = Chip::with_sample_rate(SidModel::Sid8580, sr).unwrap();
        c.write(REG_MODE_VOL, 0x0F);
        saw_a4(&mut c, V1);
        let out = render(&mut c, 2.0);
        let s = &out[sr as usize..];
        let ups = s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((ups as i32 - 440).abs() <= 1, "sr {sr}: {ups}");
    }
}

// ---------------------------------------------------------------------------
// Gate 1b: envelope (read through ENV3 on voice 3)
// ---------------------------------------------------------------------------

#[test]
fn attack_steps_on_the_measured_period() {
    // Rate counter: 0 at power-on, +1 per cycle, step when it equals the
    // period, then back to 0. Attack 0 (period 9): level 1 on cycle 9,
    // level k on cycle 9k, 255 on 9*255 = 2295. Attack 2 (period 63): 255 on
    // 63*255 = 16_065.
    let mut c = chip();
    adsr(&mut c, V3, 0x00, 0xF0);
    ctrl(&mut c, V3, GATE);
    run_to(&mut c, 8);
    assert_eq!(c.read(REG_ENV3), 0);
    run_to(&mut c, 9);
    assert_eq!(c.read(REG_ENV3), 1);
    run_to(&mut c, 9 * 128);
    assert_eq!(c.read(REG_ENV3), 128); // linear
    run_to(&mut c, 2294);
    assert_eq!(c.read(REG_ENV3), 254);
    run_to(&mut c, 2295);
    assert_eq!(c.read(REG_ENV3), 255);
    assert_eq!(c.voice(2).envelope_stage(), Stage::DecaySustain);

    let mut c = chip();
    adsr(&mut c, V3, 0x20, 0xF0);
    ctrl(&mut c, V3, GATE);
    run_to(&mut c, 16_064);
    assert_eq!(c.read(REG_ENV3), 254);
    run_to(&mut c, 16_065);
    assert_eq!(c.read(REG_ENV3), 255);
}

#[test]
fn decay_follows_the_exponential_divider() {
    // Attack 0 peaks on cycle 2295 with the rate counter at 0; decay 0
    // (period 9) then steps from level L every exp_period(L) periods:
    //   255..=94: 162 steps x1 = 162     54..=27: 28 x4  = 112
    //    93..=55:  39 steps x2 =  78     26..=15: 12 x8  =  96
    //                                    14..=7:   8 x16 = 128
    //                                     6..=1:   6 x30 = 180
    // Total 756 periods = 6804 cycles: level 0 on cycle 2295 + 6804 = 9099,
    // still 1 on 9098. After 162 periods (cycle 2295 + 1458 = 3753) the
    // level is already 93: a fast drop and a long tail.
    let mut c = chip();
    adsr(&mut c, V3, 0x00, 0x00);
    ctrl(&mut c, V3, GATE);
    run_to(&mut c, 3753);
    assert_eq!(c.read(REG_ENV3), 93);
    run_to(&mut c, 9098);
    assert_eq!(c.read(REG_ENV3), 1);
    run_to(&mut c, 9099);
    assert_eq!(c.read(REG_ENV3), 0);
    run_to(&mut c, 200_000);
    assert_eq!(c.read(REG_ENV3), 0); // zero freeze
}

#[test]
fn sustain_is_an_equality_compare() {
    // AD 0x00, SR 0xA0: sustain 10 -> level 170. Decay 255 -> 170 is 85
    // steps x1, so 170 is reached on cycle 2295 + 765 = 3060 and held.
    // Cycle 200_007 = 9 * 22_223 is on the 9-cycle grid (2295 = 9 * 255), so
    // the rate counter is 0 there.
    // Lowering S to 8 (136): 170 -> 136 is 34 steps x1 = 306 cycles, done
    // on 200_313, then held.
    // Raising S to 15 (255) from 136: never equal, so the decay runs to 0:
    //   136..=94: 43 x1 = 43, then 39 x2 + 28 x4 + 12 x8 + 8 x16 + 6 x30
    //   = 78 + 112 + 96 + 128 + 180 = 594, total 637 periods = 5733 cycles.
    //   The write at 300_006 (= 9 * 33_334, on the grid) gives 0 on 305_739.
    let mut c = chip();
    adsr(&mut c, V3, 0x00, 0xA0);
    ctrl(&mut c, V3, GATE);
    run_to(&mut c, 3059);
    assert_eq!(c.read(REG_ENV3), 171);
    run_to(&mut c, 200_007);
    assert_eq!(c.read(REG_ENV3), 170);
    assert_eq!(c.voice(2).envelope_rate_counter(), 0);
    c.write(V3 + 6, 0x80);
    run_to(&mut c, 200_312);
    assert_eq!(c.read(REG_ENV3), 137);
    run_to(&mut c, 300_006);
    assert_eq!(c.read(REG_ENV3), 136);
    c.write(V3 + 6, 0xF0);
    run_to(&mut c, 305_738);
    assert_eq!(c.read(REG_ENV3), 1);
    run_to(&mut c, 305_739);
    assert_eq!(c.read(REG_ENV3), 0);
}

#[test]
fn release_runs_to_zero_and_freezes() {
    // As above, 170 held from cycle 3060. Gate off at 200_007 (rate counter
    // 0), release 0: 170..=94 is 77 x1, then 594 as above = 671 periods =
    // 6039 cycles -> 0 on 206_046.
    let mut c = chip();
    adsr(&mut c, V3, 0x00, 0xA0);
    ctrl(&mut c, V3, GATE);
    run_to(&mut c, 200_007);
    ctrl(&mut c, V3, 0);
    assert_eq!(c.voice(2).envelope_stage(), Stage::Release);
    run_to(&mut c, 206_045);
    assert_eq!(c.read(REG_ENV3), 1);
    run_to(&mut c, 206_046);
    assert_eq!(c.read(REG_ENV3), 0);
    run_to(&mut c, 400_000);
    assert_eq!(c.read(REG_ENV3), 0);
}

#[test]
fn adsr_delay_bug_from_the_15_bit_rate_counter() {
    // Attack F (period 31_251), gate on at power-on. On cycle 5000 the rate
    // counter is 5000 and the level 0. Switching to attack 0 (period 9): the
    // counter is already past 9, so it runs 5001..=32_767 (27_767 cycles),
    // wraps to 0 (1 cycle), then counts 1..=9 (9 cycles): the step lands
    // 27_777 cycles after the write, on cycle 32_777. After that it steps
    // normally: level 2 on 32_786.
    let mut c = chip();
    adsr(&mut c, V3, 0xF0, 0xF0);
    ctrl(&mut c, V3, GATE);
    run_to(&mut c, 5000);
    assert_eq!(c.voice(2).envelope_rate_counter(), 5000);
    c.write(V3 + 5, 0x00);
    run_to(&mut c, 32_776);
    assert_eq!(c.read(REG_ENV3), 0);
    run_to(&mut c, 32_777);
    assert_eq!(c.read(REG_ENV3), 1);
    run_to(&mut c, 32_786);
    assert_eq!(c.read(REG_ENV3), 2);
    // Control: switching while the counter (5) is still below 9 steps on
    // cycle 9 as usual.
    let mut c = chip();
    adsr(&mut c, V3, 0xF0, 0xF0);
    ctrl(&mut c, V3, GATE);
    run_to(&mut c, 5);
    c.write(V3 + 5, 0x00);
    run_to(&mut c, 8);
    assert_eq!(c.read(REG_ENV3), 0);
    run_to(&mut c, 9);
    assert_eq!(c.read(REG_ENV3), 1);
}

#[test]
fn retrigger_attacks_from_the_current_level() {
    // AD 0x00 SR 0x80 (sustain 136): 255 on 2295; 255 -> 136 is 119 steps
    // x1 = 1071 cycles -> 136 on 3366 (= 9 * 374, on the grid). Gate off
    // there; release 0 from 136: 43 steps x1 to 93, on 3366 + 387 = 3753.
    // Gate back on at 3753: attack 0 steps up from 93 on the next grid
    // point, 3762 -> 94. No reset to 0.
    let mut c = chip();
    adsr(&mut c, V3, 0x00, 0x80);
    ctrl(&mut c, V3, GATE);
    run_to(&mut c, 3366);
    assert_eq!(c.read(REG_ENV3), 136);
    ctrl(&mut c, V3, 0);
    run_to(&mut c, 3753);
    assert_eq!(c.read(REG_ENV3), 93);
    ctrl(&mut c, V3, GATE);
    assert_eq!(c.voice(2).envelope_stage(), Stage::Attack);
    run_to(&mut c, 3761);
    assert_eq!(c.read(REG_ENV3), 93);
    run_to(&mut c, 3762);
    assert_eq!(c.read(REG_ENV3), 94);
}

#[test]
fn test_bit_leaves_the_envelope_alone() {
    // The datasheet's TEST text names the oscillator, noise and pulse only.
    // Attack 0 with GATE|TEST must run on the same schedule as plain GATE:
    // 1 on cycle 9, 255 on 2295.
    let mut c = chip();
    adsr(&mut c, V3, 0x00, 0xF0);
    ctrl(&mut c, V3, GATE | TEST);
    run_to(&mut c, 9);
    assert_eq!(c.read(REG_ENV3), 1);
    run_to(&mut c, 2295);
    assert_eq!(c.read(REG_ENV3), 255);
}

// ---------------------------------------------------------------------------
// Gate 2: sync truth table and edge cases (voice 1 synced to voice 3)
// ---------------------------------------------------------------------------

#[test]
fn sync_resets_on_the_source_msb_rising_edge_only() {
    // Source v3 at 0x1000 (4096/cycle): MSB first set on c = 8_388_608/4096
    // = 2048 exactly; wraps (MSB 1 -> 0) on c = 4096; rises again on 6144.
    // Destination v1 at 0x0100 (256/cycle), SYNC set:
    //   c = 2047: 256*2047 = 524_032
    //   c = 2048: reset -> 0 (its own increment this cycle is discarded)
    //   c = 2049: 256
    //   c = 4096: source WRAPS, no sync: 256*(4096-2048) = 524_288
    //   c = 6143: 256*(6143-2048) = 1_048_320
    //   c = 6144: source rises again -> 0
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    freq(&mut c, V1, 0x0100);
    ctrl(&mut c, V1, SYNC);
    run_to(&mut c, 2047);
    assert_eq!(acc(&c, 0), 524_032);
    run_to(&mut c, 2048);
    assert_eq!(acc(&c, 2), 0x80_0000);
    assert_eq!(acc(&c, 0), 0);
    run_to(&mut c, 2049);
    assert_eq!(acc(&c, 0), 256);
    run_to(&mut c, 4096);
    assert_eq!(acc(&c, 2), 0);
    assert_eq!(acc(&c, 0), 524_288);
    run_to(&mut c, 6143);
    assert_eq!(acc(&c, 0), 1_048_320);
    run_to(&mut c, 6144);
    assert_eq!(acc(&c, 0), 0);
}

#[test]
fn sync_without_the_sync_bit_does_nothing() {
    // Same oscillators, SYNC clear: v1 runs free, 256*6144 = 1_572_864.
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    freq(&mut c, V1, 0x0100);
    run_to(&mut c, 6144);
    assert_eq!(acc(&c, 0), 1_572_864);
}

#[test]
fn sync_source_wrap_vs_destination_wrap() {
    // Destination faster than source. v1 at 0x2000 (8192/cycle) wraps on
    // its own every 2048 cycles; v3 at 0x0C00 (3072/cycle):
    //   MSB rise: ceil(8_388_608/3072) = ceil(2730.67) = 2731
    //     (3072*2730 = 8_386_560 < MSB <= 3072*2731 = 8_389_632)
    //   wrap:     ceil(16_777_216/3072) = 5462 (acc 16_779_264 -> 2048)
    //   next rise: 3072c - 2^24 >= 2^23 -> c >= 25_165_824/3072 = 8192.
    // v1 (SYNC):
    //   c = 2047: 8192*2047 = 16_769_024;  c = 2048: own wrap -> 0
    //   c = 2730: 8192*682 = 5_586_944
    //   c = 2731: sync -> 0 (free-running would be 5_595_136)
    //   c = 2732: 8192
    //   c = 5462: source wrap, no sync: 8192*2731 mod 2^24 = 22_372_352
    //             - 16_777_216 = 5_595_136
    //   c = 8191: 8192*5460 = 44_728_320 - 2*16_777_216 = 11_173_888
    //   c = 8192: sync -> 0
    let mut c = chip();
    freq(&mut c, V3, 0x0C00);
    freq(&mut c, V1, 0x2000);
    ctrl(&mut c, V1, SYNC);
    let want = [
        (2047u64, 16_769_024u32),
        (2048, 0),
        (2730, 5_586_944),
        (2731, 0),
        (2732, 8192),
        (5462, 5_595_136),
        (8191, 11_173_888),
        (8192, 0),
    ];
    for (cycle, v) in want {
        run_to(&mut c, cycle);
        assert_eq!(acc(&c, 0), v, "cycle {cycle}");
    }
    assert_eq!(acc(&c, 2), 3072 * 8192 - 16_777_216);
}

#[test]
fn sync_destination_held_by_test_bit() {
    // v3 at 0x1000 rises on 2048, 6144. v1 at 0x0100 with SYNC|TEST: held
    // at 0 through the 2048 rise (sync is a no-op on a held oscillator).
    // TEST cleared at 3000: v1 = 256*(c - 3000) -> 25_600 on 3100,
    // 256*3143 = 804_608 on 6143, then synced to 0 on 6144.
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    freq(&mut c, V1, 0x0100);
    ctrl(&mut c, V1, SYNC | TEST);
    run_to(&mut c, 3000);
    assert_eq!(acc(&c, 0), 0);
    ctrl(&mut c, V1, SYNC);
    run_to(&mut c, 3100);
    assert_eq!(acc(&c, 0), 25_600);
    run_to(&mut c, 6143);
    assert_eq!(acc(&c, 0), 804_608);
    run_to(&mut c, 6144);
    assert_eq!(acc(&c, 0), 0);
}

#[test]
fn sync_source_held_by_test_bit_locks_out_the_sync() {
    // Oscillator lockout: v3 under TEST sits at 0, its MSB never rises, and
    // v1 (SYNC, 0x0100) runs free: 256*5000 = 1_280_000 on 5000. TEST is
    // released at 5000: v3 = 4096*(c-5000) rises on c = 5000 + 2048 = 7048.
    // v1 on 7047: 256*7047 = 1_804_032; on 7048: 0.
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    ctrl(&mut c, V3, TEST);
    freq(&mut c, V1, 0x0100);
    ctrl(&mut c, V1, SYNC);
    run_to(&mut c, 5000);
    assert_eq!(acc(&c, 2), 0);
    assert_eq!(acc(&c, 0), 1_280_000);
    ctrl(&mut c, V3, 0);
    run_to(&mut c, 7047);
    assert_eq!(acc(&c, 0), 1_804_032);
    run_to(&mut c, 7048);
    assert_eq!(acc(&c, 0), 0);
}

#[test]
fn test_bit_on_a_source_with_msb_set_does_not_sync() {
    // v3 rises on 2048 (v1 synced to 0). On 3000 v3 = 4096*3000 =
    // 12_288_000 (MSB set). TEST there drops it to 0: an MSB FALL, no sync.
    // v1 on 3001: 256*(3001-2048) = 243_968.
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    freq(&mut c, V1, 0x0100);
    ctrl(&mut c, V1, SYNC);
    run_to(&mut c, 3000);
    assert_eq!(acc(&c, 2), 12_288_000);
    ctrl(&mut c, V3, TEST);
    run_to(&mut c, 3001);
    assert_eq!(acc(&c, 2), 0);
    assert_eq!(acc(&c, 0), 243_968);
}

#[test]
fn sync_holds_a_zero_frequency_destination_at_zero() {
    // v1 at 0x0100 for 1000 cycles (256_000), then freq 0: held at 256_000
    // until v3's rise on 2048 resets it to 0, where it then stays.
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    freq(&mut c, V1, 0x0100);
    ctrl(&mut c, V1, SYNC);
    run_to(&mut c, 1000);
    freq(&mut c, V1, 0);
    run_to(&mut c, 2047);
    assert_eq!(acc(&c, 0), 256_000);
    run_to(&mut c, 2048);
    assert_eq!(acc(&c, 0), 0);
    run_to(&mut c, 20_000);
    assert_eq!(acc(&c, 0), 0);
}

#[test]
fn zero_frequency_source_never_syncs() {
    // v3 at freq 0 stays at 0. v1 (SYNC, 0x0100) is free:
    // 256*100_000 = 25_600_000 - 16_777_216 = 8_822_784.
    let mut c = chip();
    freq(&mut c, V1, 0x0100);
    ctrl(&mut c, V1, SYNC);
    run_to(&mut c, 100_000);
    assert_eq!(acc(&c, 0), 8_822_784);
}

#[test]
fn chained_sync_a_reset_source_does_not_propagate_its_rise() {
    // INFERRED edge (see chip.rs): v3 0x1000 -> rises on 2048, 6144.
    // v1 0x1000, SYNC (source v3): its own MSB would rise on 2048 too, but
    // it is reset that cycle. v2 0x0100, SYNC (source v1): v1's rise is
    // cancelled, so v2 is NOT reset on 2048: 256*2048 = 524_288.
    // v1 restarts from 0 and rises on 2048 + 2048 = 4096, a cycle on which
    // v3 WRAPS (no reset of v1), so this rise propagates: v2 -> 0 on 4096
    // (1_048_320 on 4095).
    // On 6144 v3 rises and v1 wraps (4096*4096 = 2^24) and is reset anyway:
    // v1's MSB falls, so v2 runs on: 256*2048 = 524_288.
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    freq(&mut c, V1, 0x1000);
    ctrl(&mut c, V1, SYNC);
    freq(&mut c, V2, 0x0100);
    ctrl(&mut c, V2, SYNC);
    run_to(&mut c, 2048);
    assert_eq!(acc(&c, 0), 0);
    assert_eq!(acc(&c, 1), 524_288);
    run_to(&mut c, 4095);
    assert_eq!(acc(&c, 1), 1_048_320);
    run_to(&mut c, 4096);
    assert_eq!(acc(&c, 0), 0x80_0000);
    assert_eq!(acc(&c, 1), 0);
    run_to(&mut c, 6144);
    assert_eq!(acc(&c, 0), 0);
    assert_eq!(acc(&c, 1), 524_288);
}

#[test]
fn every_voice_syncs_to_its_datasheet_source() {
    // Voice i is synced by voice source_of(i): 1 <- 3, 2 <- 1, 3 <- 2.
    // Only the source runs (0x1000, rises on 2048); the destination runs at
    // 0x0100 with SYNC. On 2048 the destination is 0; every other voice
    // (freq 0) stays 0 and nothing else resets.
    for dest in 0..3usize {
        let src = source_of(dest);
        let base = |i: usize| (i * 7) as u8;
        let mut c = chip();
        freq(&mut c, base(src), 0x1000);
        freq(&mut c, base(dest), 0x0100);
        ctrl(&mut c, base(dest), SYNC);
        run_to(&mut c, 2047);
        assert_eq!(acc(&c, dest), 524_032, "dest {dest}");
        run_to(&mut c, 2048);
        assert_eq!(acc(&c, dest), 0, "dest {dest}");
    }
    assert_eq!([source_of(0), source_of(1), source_of(2)], [2, 0, 1]);
}

// ---------------------------------------------------------------------------
// Gate 2: ring modulation through the chip
// ---------------------------------------------------------------------------

#[test]
fn ring_mod_truth_table_through_the_chip() {
    // v1 TRI|RING at 0x0100; after 1000 cycles acc = 256_000 (MSB 0).
    //   plain triangle: 256_000 >> 11 = 125 -> & 0xFFE = 0x07C
    //   fold flipped:   (0x7FFFFF - 256_000) = 8_132_607 >> 11 = 3970
    //                   = 0xF82 (= 0x07C ^ 0xFFE)
    // fold = MSB(v1) ^ !MSB(v3):
    //   v3 held at 0 by TEST (MSB 0)       -> fold 1 -> 0xF82
    //   v3 parked at 0x800000 (MSB 1)      -> fold 0 -> 0x07C
    // Parking: v3 at 0x8000 for 256 cycles = 0x800000, then freq 0. v1 is
    // at freq 0 during those 256 cycles, then 0x0100 for 1000 more.
    let mut c = chip();
    ctrl(&mut c, V3, TEST);
    freq(&mut c, V1, 0x0100);
    ctrl(&mut c, V1, TRI | RING);
    run_to(&mut c, 1000);
    assert_eq!(acc(&c, 0), 256_000);
    assert_eq!(c.voice(0).waveform(), 0xF82);

    let mut c = chip();
    freq(&mut c, V3, 0x8000);
    run_to(&mut c, 256);
    assert_eq!(acc(&c, 2), 0x80_0000);
    freq(&mut c, V3, 0);
    freq(&mut c, V1, 0x0100);
    ctrl(&mut c, V1, TRI | RING);
    run_to(&mut c, 1256);
    assert_eq!(acc(&c, 0), 256_000);
    assert_eq!(c.voice(0).waveform(), 0x07C);
    // Ring bit without TRI: saw at 256_000 = 256_000 >> 12 = 62 = 0x03E.
    ctrl(&mut c, V1, SAW | RING);
    c.clock();
    assert_eq!(c.voice(0).waveform(), (256_256u32 >> 12) as u16);
}

#[test]
fn ring_mod_follows_the_live_source_msb_every_cycle() {
    // With free-running oscillators the ring-modulated triangle must equal
    // the plain triangle when the source MSB is 1 and the plain triangle
    // XOR 0xFFE when it is 0 (fold flipped), on every cycle and for each
    // datasheet pairing. The source MSB is read after the cycle's sync step.
    let freqs = [0x0123u16, 0x0456, 0x1789];
    for dest in 0..3usize {
        let mut c = chip();
        for (i, &f) in freqs.iter().enumerate() {
            freq(&mut c, (i * 7) as u8, f);
        }
        ctrl(&mut c, (dest * 7) as u8, TRI | RING);
        let mut flips = 0;
        for _ in 0..40_000 {
            c.clock();
            let own = acc(&c, dest);
            let src_msb = acc(&c, source_of(dest)) & 0x80_0000 != 0;
            let want = if src_msb {
                triangle(own)
            } else {
                triangle(own) ^ 0xFFE
            };
            assert_eq!(c.voice(dest).waveform(), want);
            flips += (!src_msb) as u32;
        }
        assert!(
            flips > 1000 && flips < 39_000,
            "both source halves seen: {flips}"
        );
    }
}

// ---------------------------------------------------------------------------
// Test bit: oscillator, pulse, noise
// ---------------------------------------------------------------------------

#[test]
fn test_bit_holds_the_accumulator_at_zero() {
    // v3 at 0x1000: 409_600 on 100. TEST from 100 to 1100: 0 throughout.
    // Released at 1100: 4096 * 10 = 40_960 on 1110.
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    run_to(&mut c, 100);
    assert_eq!(acc(&c, 2), 409_600);
    ctrl(&mut c, V3, TEST);
    run_to(&mut c, 101);
    assert_eq!(acc(&c, 2), 0);
    run_to(&mut c, 1100);
    assert_eq!(acc(&c, 2), 0);
    ctrl(&mut c, V3, 0);
    run_to(&mut c, 1110);
    assert_eq!(acc(&c, 2), 40_960);
}

#[test]
fn test_bit_holds_the_pulse_output_high() {
    // PW 0xFFF, freq 0 (acc 0): the comparator is low (0 < 0xFFF), OSC3 0.
    // With TEST the pulse output is forced to 0xFFF: OSC3 0xFF.
    let mut c = chip();
    pw(&mut c, V3, 0xFFF);
    ctrl(&mut c, V3, PULSE);
    run_to(&mut c, 1);
    assert_eq!(c.read(REG_OSC3), 0x00);
    ctrl(&mut c, V3, PULSE | TEST);
    run_to(&mut c, 2);
    assert_eq!(c.read(REG_OSC3), 0xFF);
}

#[test]
fn test_bit_resets_noise_and_its_phase_to_the_oscillator() {
    // Under TEST the LFSR is held at 0x7FFFFF (all 8 taps set: 0xFF0,
    // OSC3 0xFF) and the accumulator at 0. Released on cycle 10 with freq
    // 0x1000: bit 19 (0x80000 = 524_288) first rises on 10 + 524_288/4096 =
    // 138, then every 2^20/4096 = 256 cycles (394, 650, ...).
    //   shift 1 (138): fb = b22 ^ b17 = 1 ^ 1 = 0 -> 0x7FFFFE, tap 0 low
    //                  -> 0xFE0, OSC3 0xFE
    //   shift 2 (394): -> 0x7FFFFC; bit 1 is not a tap -> still 0xFE
    //   shift 3 (650): -> 0x7FFFF8; tap 2 low -> 0xFC0, OSC3 0xFC
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    ctrl(&mut c, V3, NOISE | TEST);
    run_to(&mut c, 10);
    assert_eq!(c.voice(2).noise_register(), 0x7F_FFFF);
    assert_eq!(c.read(REG_OSC3), 0xFF);
    ctrl(&mut c, V3, NOISE);
    let want = [
        (137u64, 0xFFu8),
        (138, 0xFE),
        (393, 0xFE),
        (394, 0xFE),
        (649, 0xFE),
        (650, 0xFC),
    ];
    for (cycle, v) in want {
        run_to(&mut c, cycle);
        assert_eq!(c.read(REG_OSC3), v, "cycle {cycle}");
    }
    assert_eq!(c.voice(2).noise_register(), 0x7F_FFF8);
}

#[test]
fn noise_combined_with_a_low_pulse_locks_up_until_test() {
    // v3 NOISE|PULSE, PW 0xFFF, freq 0x1000. Shifts land on c = 128 + 256k,
    // where acc >> 12 = c mod 4096 (128, 384, ...) is never 0xFFF, so the
    // pulse, and with it the combined output, is 0 at every shift: all
    // eight taps are cleared before each shift.
    // Hand derivation (write-back then shift): after shift n, positions
    // 1..=n hold bits that sat at position 0 (a tap) during a write-back, so
    // they are 0. After shift 23, positions 1..=22 are 0 and the new bit 0 is
    // b22 ^ b17 from positions that were already 0. The register is 0 no
    // later than shift 23 (cycle 128 + 256*22 = 5760), and 0 is a fixed point
    // of the LFSR: pure noise then stays silent.
    // TEST reloads 0x7FFFFF (OSC3 0xFF) and noise runs again.
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    pw(&mut c, V3, 0xFFF);
    ctrl(&mut c, V3, NOISE | PULSE);
    run_to(&mut c, 5760);
    assert_eq!(c.voice(2).noise_register(), 0);
    ctrl(&mut c, V3, NOISE);
    run_to(&mut c, 105_760);
    assert_eq!(c.voice(2).noise_register(), 0);
    assert_eq!(c.read(REG_OSC3), 0);
    ctrl(&mut c, V3, NOISE | TEST);
    run_to(&mut c, 105_761);
    assert_eq!(c.voice(2).noise_register(), 0x7F_FFFF);
    assert_eq!(c.read(REG_OSC3), 0xFF);
    ctrl(&mut c, V3, NOISE);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200_000 {
        c.clock();
        seen.insert(c.read(REG_OSC3));
    }
    assert!(
        seen.len() > 50,
        "noise alive again: {} distinct values",
        seen.len()
    );
}

#[test]
fn pure_noise_never_locks_up() {
    // Control for the test above: pure noise at the same rate for as long
    // never reaches the all-zero state.
    let mut c = chip();
    freq(&mut c, V3, 0x1000);
    ctrl(&mut c, V3, NOISE);
    for _ in 0..200 {
        c.clock_cycles(256);
        assert_ne!(c.voice(2).noise_register(), 0);
    }
}

// ---------------------------------------------------------------------------
// Combined waveforms, waveform 0 and OSC3/ENV3 readback
// ---------------------------------------------------------------------------

#[test]
fn combined_waveforms_through_osc3() {
    // v3 at 0x6000 for 256 cycles: acc = 24_576 * 256 = 6_291_456 =
    // 0x600000. Then freq 0 (parked). Waveform bits there:
    //   saw 0x600, tri 0xC00, pulse(PW 0x400) 0xFFF, pulse(PW 0x800) 0
    //   SAW|TRI         AND 0x400 -> pull -> 0     -> OSC3 0x00
    //   PULSE|SAW  (400)AND 0x600 -> pull -> 0     -> OSC3 0x00
    //   PULSE|TRI  (400)AND 0xC00 (plain AND)      -> OSC3 0xC0
    //   PULSE|SAW  (800)= 0     -> OSC3 0x00
    // S5.12 R2: the 8580's saw+tri, pulse+saw and pulse+saw+tri go through
    // the fitted neighbour pull (waveform.rs hand values: 0x400 and 0x600
    // both vanish at every 8580 threshold); pulse+tri stays the AND.
    let mut c = chip();
    freq(&mut c, V3, 0x6000);
    run_to(&mut c, 256);
    freq(&mut c, V3, 0);
    assert_eq!(acc(&c, 2), 0x60_0000);
    let cases = [
        (SAW | TRI, 0x400u16, 0x00u8),
        (PULSE | SAW, 0x400, 0x00),
        (PULSE | TRI, 0x400, 0xC0),
        (PULSE | SAW | TRI, 0x400, 0x00),
        (PULSE | SAW, 0x800, 0x00),
        (SAW, 0x800, 0x60),
        (TRI, 0x800, 0xC0),
    ];
    for (w, p, want) in cases {
        pw(&mut c, V3, p);
        ctrl(&mut c, V3, w);
        c.clock();
        assert_eq!(c.read(REG_OSC3), want, "wave {w:#x} pw {p:#x}");
    }
}

#[test]
fn waveform_zero_holds_the_last_output() {
    // v3 saw at 0x6000 for 256 cycles: OSC3 0x60. Waveform bits cleared:
    // the accumulator keeps running but the DAC input holds 0x600 (OSC3
    // 0x60). Reselecting SAW picks the running accumulator up again:
    // on cycle 10_001, 24_576 * 10_001 mod 2^24 = 245_784_576 - 14 * 2^24
    // = 10_903_552 -> saw 0xA66 -> OSC3 0xA6.
    let mut c = chip();
    freq(&mut c, V3, 0x6000);
    ctrl(&mut c, V3, SAW);
    run_to(&mut c, 256);
    assert_eq!(c.read(REG_OSC3), 0x60);
    ctrl(&mut c, V3, 0);
    run_to(&mut c, 10_000);
    assert_ne!(acc(&c, 2), 0x60_0000);
    assert_eq!(c.voice(2).waveform(), 0x600);
    assert_eq!(c.read(REG_OSC3), 0x60);
    ctrl(&mut c, V3, SAW);
    run_to(&mut c, 10_001);
    assert_eq!(acc(&c, 2), 10_903_552);
    assert_eq!(c.read(REG_OSC3), 0xA6);
}

#[test]
fn osc3_env3_reflect_voice_3_only() {
    // Voices 1 and 2 play loud gated saws; voice 3 is idle: OSC3 = ENV3 = 0.
    // On a fresh chip, a gated A-4 saw with attack 0 on voice 3 gives
    // ENV3 = 1 on cycle 9 (rate counter 0 at power-on).
    let mut c = chip();
    saw_a4(&mut c, V1);
    saw_a4(&mut c, V2);
    run_to(&mut c, 20_000);
    assert_eq!(c.read(REG_OSC3), 0);
    assert_eq!(c.read(REG_ENV3), 0);
    let mut c = chip();
    saw_a4(&mut c, V3);
    run_to(&mut c, 9);
    assert_eq!(c.read(REG_ENV3), 1);
    // OSC3 = saw >> 4 = acc >> 16 = 9 * 7493 >> 16 = 67_437 >> 16 = 1.
    assert_eq!(c.read(REG_OSC3), 1);
    // Other registers read 0; register numbers wrap at 0x20.
    assert_eq!(c.read(0x00), 0);
    assert_eq!(c.read(0x20 + REG_OSC3), 1);
}

// ---------------------------------------------------------------------------
// Routing: FILT bits, mode bits, 3OFF, volume
// ---------------------------------------------------------------------------

fn routed(setup: impl Fn(&mut Chip)) -> Vec<f32> {
    let mut c = chip();
    setup(&mut c);
    render(&mut c, 0.5)
}

fn tail_rms(x: &[f32]) -> f64 {
    rms(&x[x.len() / 2..])
}

#[test]
fn filt_bits_route_voices_through_the_filter() {
    // A-4 saw on voice 1, volume 15. Expected, with cutoff reg 0 = 30 Hz
    // (2-pole LP): the 440 Hz fundamental sits 3.9 octaves above fc, about
    // (30/440)^2 = 0.0046 of the direct level (-47 dB), so the filtered RMS
    // is < 2 % of direct. HP at 30 Hz passes it almost untouched (> 95 %).
    // Mode bits all 0 with FILT1: the filtered voice vanishes: exactly 0.
    let direct = tail_rms(&routed(|c| {
        c.write(REG_MODE_VOL, LP | 0x0F);
        saw_a4(c, V1);
    }));
    let lp = tail_rms(&routed(|c| {
        c.write(REG_RES_FILT, 0x01);
        c.write(REG_MODE_VOL, LP | 0x0F);
        saw_a4(c, V1);
    }));
    let hp = tail_rms(&routed(|c| {
        c.write(REG_RES_FILT, 0x01);
        c.write(REG_MODE_VOL, HP | 0x0F);
        saw_a4(c, V1);
    }));
    let none = routed(|c| {
        c.write(REG_RES_FILT, 0x01);
        c.write(REG_MODE_VOL, 0x0F);
        saw_a4(c, V1);
    });
    assert!(direct > 0.05, "{direct}");
    assert!(lp < 0.02 * direct, "lp {lp} vs {direct}");
    assert!(
        hp > 0.95 * direct && hp < 1.05 * direct,
        "hp {hp} vs {direct}"
    );
    assert!(none.iter().all(|&s| s == 0.0));
}

#[test]
fn filter_registers_program_the_filter() {
    // FC = (FC_HI << 3) | (FC_LO & 7): 0xAB << 3 | 5 = 0x55D. RES = $17 >> 4.
    let mut c = chip();
    c.write(REG_FC_LO, 0xFD);
    c.write(REG_FC_HI, 0xAB);
    c.write(REG_RES_FILT, 0xC7);
    c.write(REG_MODE_VOL, 0x80 | LP | BP | 0x03);
    assert_eq!(c.filter().cutoff_reg(), 0x55D);
    assert_eq!(c.filter().resonance(), 0xC);
    assert_eq!(c.filter().mode(), LP | BP);
}

#[test]
fn filt_bits_are_per_voice() {
    // Voice 1 filtered away (LP at 30 Hz), voice 2 direct: the output is
    // voice 2 alone to within the filter's residual 440 Hz leak (< 2 %).
    let v2_only = routed(|c| {
        c.write(REG_MODE_VOL, LP | 0x0F);
        saw_a4(c, V2);
    });
    let mixed = routed(|c| {
        c.write(REG_RES_FILT, 0x01);
        c.write(REG_MODE_VOL, LP | 0x0F);
        saw_a4(c, V1);
        saw_a4(c, V2);
    });
    let diff: Vec<f32> = mixed.iter().zip(&v2_only).map(|(a, b)| a - b).collect();
    assert!(tail_rms(&diff) < 0.02 * tail_rms(&v2_only));
}

#[test]
fn voice3_off_mutes_only_the_direct_voice_3() {
    // Datasheet: 3OFF with FILT3 = 0 -> voice 3 silent. With FILT3 = 1 (LP at
    // 12 kHz, open for a 440 Hz saw) 3OFF has no effect. 3OFF never touches
    // voices 1/2: their output is sample-identical with and without it.
    let v3_direct_off = routed(|c| {
        c.write(REG_MODE_VOL, VOICE3_OFF | LP | 0x0F);
        saw_a4(c, V3);
    });
    assert!(v3_direct_off.iter().all(|&s| s == 0.0));
    let v3_direct = tail_rms(&routed(|c| {
        c.write(REG_MODE_VOL, LP | 0x0F);
        saw_a4(c, V3);
    }));
    let v3_filtered_off = tail_rms(&routed(|c| {
        c.write(REG_FC_HI, 0xFF);
        c.write(REG_FC_LO, 0x07);
        c.write(REG_RES_FILT, 0x04);
        c.write(REG_MODE_VOL, VOICE3_OFF | LP | 0x0F);
        saw_a4(c, V3);
    }));
    assert!(
        v3_filtered_off > 0.8 * v3_direct,
        "{v3_filtered_off} vs {v3_direct}"
    );
    let v1 = routed(|c| {
        c.write(REG_MODE_VOL, LP | 0x0F);
        saw_a4(c, V1);
    });
    let v1_off = routed(|c| {
        c.write(REG_MODE_VOL, VOICE3_OFF | LP | 0x0F);
        saw_a4(c, V1);
    });
    assert_eq!(v1, v1_off);
}

#[test]
fn master_volume_scales_linearly() {
    // Everything after the volume multiply is linear, so volume 7 output is
    // volume 15 output * 7/15 sample by sample (to f32 rounding). Volume 0
    // is silent.
    let at = |vol: u8| {
        routed(move |c| {
            c.write(REG_MODE_VOL, vol);
            saw_a4(c, V1);
        })
    };
    let v15 = at(15);
    let v7 = at(7);
    for (a, b) in v15.iter().zip(&v7) {
        assert!((*a as f64 * 7.0 / 15.0 - *b as f64).abs() < 1e-6);
    }
    assert!(at(0).iter().all(|&s| s == 0.0));
}

#[test]
fn band_pass_selects_the_region_around_cutoff() {
    // BP at ~1.5 kHz (reg 0x100) passes a 1.5 kHz triangle far better than
    // a 110 Hz one: BP is 6 dB/oct each side, so at 110/1527 = 0.072 of fc
    // the fundamental gets ~0.072 against 0.707 at fc, a factor of ~10
    // (harmonics near fc leak through, so the bound used is a loose 3x).
    let run = |f: u16| {
        tail_rms(&routed(move |c| {
            c.write(REG_FC_HI, 0x20);
            c.write(REG_RES_FILT, 0x01);
            c.write(REG_MODE_VOL, BP | 0x0F);
            freq(c, V1, f);
            adsr(c, V1, 0x00, 0xF0);
            ctrl(c, V1, TRI | GATE);
        }))
    };
    let near = run(hz_to_freq_reg(1527.0));
    let far = run(hz_to_freq_reg(110.0));
    assert!(near > 3.0 * far, "near {near} far {far}");
}

// ---------------------------------------------------------------------------
// Gate 3: saw harmonics (the S0 open tolerance question)
// ---------------------------------------------------------------------------
//
// S0 reported the rendered A-4 saw's harmonics k=3, k=4 at 0.8 / 1.2 dB
// below -20log10(k), measured by a 4096-point Hann FFT that takes the peak
// bin within +-3 bins of k*f0. First cause, established by experiment
// (`.ai/sid-voice-core/saw_harmonic_experiment.py`,
// `.ai/checks-s1-saw-harmonic-experiment.txt`): it is the estimator's
// scalloping loss. k*f0 falls between FFT bins (bin spacing 44100/4096 =
// 10.77 Hz; k*440.03 Hz = k*40.87 bins, so k=4 sits 0.48 bins off-grid),
// and a Hann window's gain 0.5 bin off-centre is -1.42 dB. The same estimator
// on an IDEAL analytic saw gives the same deviations, and at a bin-centred
// pitch the deviation is 0.00 dB. The decimator is not the cause.
//
// Hand derivation of the expected numbers:
//   f0 = 7493 * 985_248 / 2^24 = 440.0291 Hz; bin position k*f0*4096/44100.
//   Hann scalloping at offset d bins: W(d) = |sin(pi d) / (pi d) / (1 - d^2)|.
//   S0 estimator dev_k = 20log10(W(d_k)/W(d_1)) + droop_k, where
//   droop_k = the boxcar decimator's response, 20log10(|sinc(k f0/fs)| /
//   |sinc(f0/fs)|) (an average over 1/fs, first null at fs):
//     k:        1     2     3     4     5     6     7     8     9    10
//     d_k:   0.13  0.26  0.39  0.48  0.35  0.22  0.09  0.04  0.17  0.30
//     scallop 0.00 -0.29 -0.77 -1.21 -0.59 -0.17 +0.05 +0.09 -0.07 -0.42
//     droop   0.00 -0.00 -0.01 -0.02 -0.03 -0.05 -0.07 -0.09 -0.11 -0.14
// A scalloping-free estimator (Hann-windowed DFT evaluated AT k*f0) must
// then show only the droop: |dev_k - droop_k| small. The S1 tolerance is
// 0.05 dB, 10x tighter than S0's 0.5 dB. The experiment measured <= 0.01 dB.

fn saw_render() -> (Vec<f32>, f64) {
    let mut c = chip();
    c.write(REG_MODE_VOL, 0x0F);
    saw_a4(&mut c, V1);
    // Same window as S0's analysis: 0.6 s .. 2.3 s (decay done, sustain F).
    let out = render(&mut c, 2.3);
    (
        out[(0.6 * 44_100.0) as usize..].to_vec(),
        freq_reg_to_hz(A4),
    )
}

fn boxcar_droop_db(k: usize, f0: f64) -> f64 {
    let sinc = |f: f64| {
        let x = PI * f / 44_100.0;
        (x.sin() / x).abs()
    };
    20.0 * (sinc(k as f64 * f0) / sinc(f0)).log10()
}

/// Power of the Hann-windowed DFT of `x` at `hz`.
fn hann_dft_power(x: &[f32], hz: f64) -> f64 {
    let n = x.len();
    let (mut re, mut im) = (0.0f64, 0.0f64);
    let w = 2.0 * PI * hz / 44_100.0;
    for (i, &v) in x.iter().enumerate() {
        let h = 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos();
        let a = w * i as f64;
        re += v as f64 * h * a.cos();
        im -= v as f64 * h * a.sin();
    }
    re * re + im * im
}

#[test]
fn saw_harmonics_match_ideal_minus_decimator_droop() {
    let (x, f0) = saw_render();
    let p1 = hann_dft_power(&x, f0);
    for k in 1..=10usize {
        let got = 10.0 * (hann_dft_power(&x, k as f64 * f0) / p1).log10();
        let want = -20.0 * (k as f64).log10() + boxcar_droop_db(k, f0);
        assert!(
            (got - want).abs() < 0.05,
            "k={k}: got {got:.3} want {want:.3}"
        );
    }
}

#[test]
fn s0_bin_grid_estimator_reproduces_the_scalloping_first_cause() {
    // The S0 estimator: 4096-point Hann frames, hop 4096, power averaged,
    // then the peak bin within +-3 of round(k*f0*N/fs). Evaluated here as a
    // DFT at those bin frequencies only.
    const N: usize = 4096;
    let (x, f0) = saw_render();
    let frames: Vec<&[f32]> = x.chunks_exact(N).collect();
    assert_eq!(frames.len(), 18); // (2.3 - 0.6) * 44100 / 4096 = 18.3
    let bin_hz = 44_100.0 / N as f64;
    let peak = |k: usize| {
        let c = (k as f64 * f0 / bin_hz).round() as i64;
        (c - 3..=c + 3)
            .map(|b| {
                frames
                    .iter()
                    .map(|f| hann_dft_power(f, b as f64 * bin_hz))
                    .sum::<f64>()
            })
            .fold(0.0f64, f64::max)
    };
    let scallop = |k: usize| {
        let pos = k as f64 * f0 / bin_hz;
        let d = (pos - pos.round()).abs();
        let x = PI * d;
        (x.sin() / x / (1.0 - d * d)).abs()
    };
    let p1 = peak(1);
    for k in 1..=10usize {
        let got = 10.0 * (peak(k) / p1).log10() + 20.0 * (k as f64).log10();
        let predicted = 20.0 * (scallop(k) / scallop(1)).log10() + boxcar_droop_db(k, f0);
        assert!(
            (got - predicted).abs() < 0.05,
            "k={k}: got {got:.3} predicted {predicted:.3}"
        );
    }
    // The S0 numbers themselves: k=4 misses -20log10(4) by ~1.2 dB.
    let k4 = 10.0 * (peak(4) / p1).log10() + 20.0 * 4f64.log10();
    assert!((-1.4..-1.0).contains(&k4), "{k4}");
}
