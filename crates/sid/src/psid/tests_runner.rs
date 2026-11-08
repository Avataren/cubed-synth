//! The C64 around the CPU, the runner and the file parser.

use super::c64::{C64Clock, Taken, C64, KERNAL_BRK_HANDLER, KERNAL_IRQ_ENTRY, KERNAL_NMI_ENTRY};
use super::cpu::{I, U};
use super::file::{PsidClock, PsidError, PsidFile, PsidSidModel, PsidType};
use super::runner::{
    call_routine, psid_banks_for, CallEnd, EndReason, PsidRunner, RunMode, RETURN_TRAP,
};
use super::testkit::*;

fn pal() -> C64 {
    C64::new(C64Clock::Pal, &[])
}

// --- The C64 ------------------------------------------------------------------

#[test]
fn banking_shows_the_stand_in_rom_where_01_says() {
    let mut m = pal();
    m.write(0xa000, 0xaa);
    m.write(0xe000, 0xee);
    // $37: BASIC, KERNAL and I/O in. The BASIC range reads RTS, the KERNAL's its stand-in.
    assert_eq!(m.banks(), 0x37);
    assert_eq!(m.read(0xa000), 0x60);
    assert_eq!(m.read(0xe000), 0x60);
    assert_eq!(m.read(0xfffe), (KERNAL_IRQ_ENTRY & 0xff) as u8);
    assert_eq!(m.read(0xffff), (KERNAL_IRQ_ENTRY >> 8) as u8);
    assert_eq!(m.read(0xfffa), (KERNAL_NMI_ENTRY & 0xff) as u8);
    // The writes went to the RAM under them.
    m.set_banks(0x34);
    assert_eq!((m.read(0xa000), m.read(0xe000)), (0xaa, 0xee));
    // $36: BASIC out, KERNAL in.
    m.set_banks(0x36);
    assert_eq!((m.read(0xa000), m.read(0xe000)), (0xaa, 0x60));
    // $35: KERNAL out, I/O in; $34: all RAM.
    m.set_banks(0x35);
    assert_eq!(m.read(0xe000), 0xee);
    m.write(0xd400, 0x12);
    assert_eq!(m.sys.writes.len(), 1, "$35 has the I/O area in");
    m.set_banks(0x34);
    m.write(0xd401, 0x34);
    assert_eq!(m.sys.writes.len(), 1, "$34 has RAM at $D000: no SID write");
    assert_eq!(m.read(0xd401), 0x34);
    // The data direction register decides which bits of $01 count.
    m.write(0x00, 0x00);
    m.write(0x01, 0x00);
    assert_eq!(m.read(0x01), 0x00);
    m.write(0x00, 0x2f);
    assert_eq!(m.banks() & 7, 0);
}

#[test]
fn sid_writes_are_queued_with_their_chip_register_and_cycle() {
    let mut m = C64::new(C64Clock::Pal, &[0xd420, 0xde00]);
    assert_eq!(m.sid_count(), 3);
    m.cpu.cycles = 100;
    m.write(0xd400, 1);
    m.cpu.cycles = 105;
    m.write(0xd41f, 2);
    m.write(0xd500, 3); // a mirror of the first chip
    m.write(0xd420, 4); // the second
    m.write(0xd43f, 5);
    m.write(0xd440, 6); // past the second: the first again
    m.write(0xde05, 7); // the third, in the I/O area
    m.write(0xdf00, 8); // nothing there
    let got: Vec<_> = std::iter::from_fn(|| m.sys.writes.pop())
        .map(|w| (w.cycle, w.chip, w.reg, w.value))
        .collect();
    assert_eq!(
        got,
        [
            (100, 0, 0x00, 1),
            (105, 0, 0x1f, 2),
            (105, 0, 0x00, 3),
            (105, 1, 0x00, 4),
            (105, 1, 0x1f, 5),
            (105, 0, 0x00, 6),
            (105, 2, 0x05, 7),
        ]
    );
}

#[test]
fn sid_registers_read_back_the_bus_except_the_pots_and_voice_three() {
    let mut m = pal();
    m.write(0xd400, 0x5a);
    assert_eq!(m.read(0xd400), 0x5a, "the last value on the chip's bus");
    assert_eq!(m.read(0xd419), 0xff);
    assert_eq!(m.read(0xd41a), 0xff);
    // Voice 3's oscillator: a sawtooth of frequency $1000 a thousand cycles on is acc >> 16.
    m.write(0xd40e, 0x00);
    m.write(0xd40f, 0x10);
    m.write(0xd412, 0x20);
    m.idle_to(1000);
    assert_eq!(m.read(0xd41b), 0x3e, "0x1000 * 1000 = 0x3E8000");
    // Its envelope: gate on, attack rate 0 steps every 9 cycles.
    let mut m = pal();
    m.write(0xd413, 0x00);
    m.write(0xd414, 0xf0);
    m.write(0xd412, 0x01);
    m.idle_to(90);
    assert_eq!(m.read(0xd41c), 10);
    // Noise reads from the LFSR: it is not constant and not the saw.
    let mut m = pal();
    m.write(0xd40f, 0x40);
    m.write(0xd412, 0x80);
    let mut seen = std::collections::HashSet::new();
    for i in 1..200u64 {
        m.idle_to(i * 500);
        seen.insert(m.read(0xd41b));
    }
    assert!(seen.len() > 30, "noise gave {} values", seen.len());
}

#[test]
fn color_ram_keeps_four_bits() {
    let mut m = pal();
    m.write(0xd800, 0xab);
    assert_eq!(m.read(0xd800), 0x0b);
}

#[test]
fn cia_timer_a_underflows_after_latch_plus_one_and_interrupts() {
    // The KERNAL's own timer: latch $4025, continuous, interrupt on.
    let mut m = pal();
    m.cpu.p = U;
    m.idle_to(0x4025);
    assert!(!m.interrupt_waiting());
    m.idle_to(0x4026);
    assert!(m.interrupt_waiting());
    assert_eq!(m.take_interrupt(), Some(Taken::Irq));
    assert_eq!(m.cpu.pc, KERNAL_IRQ_ENTRY);
    assert_eq!(m.cpu.p & I, I);
    // Reading the interrupt control register gives the flag and clears it.
    assert_eq!(m.read(0xdc0d), 0x81);
    assert_eq!(m.read(0xdc0d), 0x00);
    // And the next underflow is a period on.
    m.idle_to(0x4026 + 0x4026 - 1);
    assert_eq!(m.read(0xdc0d), 0x00);
    m.idle_to(0x4026 + 0x4026);
    assert_eq!(m.read(0xdc0d), 0x81);
}

#[test]
fn cia_counter_reads_back_and_a_one_shot_stops() {
    let mut m = pal();
    m.write(0xdc04, 0x00);
    m.write(0xdc05, 0x01);
    m.write(0xdc0e, 0x11); // start, force load
    m.idle_to(10);
    assert_eq!((m.read(0xdc04), m.read(0xdc05)), (0xf6, 0x00));
    assert_eq!(m.read(0xdc0e), 0x01);
    // A one-shot underflows once and clears its start bit.
    let mut m = pal();
    m.write(0xdc0d, 0x7f);
    m.write(0xdc04, 0x04);
    m.write(0xdc05, 0x00);
    m.write(0xdc0e, 0x19);
    m.idle_to(4);
    assert_eq!(m.read(0xdc0d), 0x00);
    m.idle_to(6);
    assert_eq!(m.read(0xdc0d) & 0x1f, 0x01, "underflowed");
    assert_eq!(m.read(0xdc0e), 0x08, "stopped");
    m.idle_to(100);
    assert_eq!(m.read(0xdc0d) & 0x1f, 0x00, "and only once");
}

#[test]
fn cia_timer_b_counts_timer_a_underflows() {
    let mut m = pal();
    m.write(0xdc0d, 0x7f);
    m.write(0xdc04, 9);
    m.write(0xdc05, 0);
    m.write(0xdc0e, 0x11); // A: every 10 cycles
    m.write(0xdc06, 2);
    m.write(0xdc07, 0);
    m.write(0xdc0f, 0x51); // B: count A's underflows, force load, start
    m.idle_to(29);
    assert_eq!(
        m.read(0xdc0d) & 0x1f,
        0x01,
        "A underflowed twice; B is at 0"
    );
    m.idle_to(31);
    assert_eq!(m.read(0xdc0d) & 0x1f, 0x03, "the third underflow reaches B");
}

#[test]
fn cia2_drives_the_nmi_line_whatever_i_says() {
    let mut m = pal();
    m.cpu.p = U | I;
    m.write(0xdd0d, 0x81);
    m.write(0xdd04, 5);
    m.write(0xdd05, 0);
    m.write(0xdd0e, 0x11);
    m.idle_to(5);
    assert_eq!(m.take_interrupt(), None);
    m.idle_to(7);
    assert_eq!(m.take_interrupt(), Some(Taken::Nmi));
    assert_eq!(m.cpu.pc, KERNAL_NMI_ENTRY);
    // The edge is taken once.
    assert_eq!(m.take_interrupt(), None);
}

#[test]
fn vic_raster_counter_and_interrupt() {
    let mut m = pal();
    m.idle_to(63 * 10 + 5);
    assert_eq!(m.read(0xd012), 10);
    m.idle_to(63 * 260);
    assert_eq!((m.read(0xd011) & 0x80, m.read(0xd012)), (0x80, 4));
    m.idle_to(63 * 312 + 63 * 2);
    assert_eq!(
        (m.read(0xd011) & 0x80, m.read(0xd012)),
        (0, 2),
        "the frame wraps at 312 lines"
    );
    // A raster interrupt at line 100.
    let mut m = pal();
    m.cpu.p = U;
    m.write(0xdc0d, 0x7f);
    m.write(0xd012, 100);
    m.write(0xd011, 0x1b);
    m.write(0xd01a, 0x01);
    m.idle_to(63 * 100 - 1);
    assert!(!m.interrupt_waiting());
    m.idle_to(63 * 100);
    assert!(m.interrupt_waiting());
    assert_eq!(m.read(0xd019), 0xf1);
    m.write(0xd019, 0x01); // acknowledge
    assert!(!m.interrupt_waiting());
    // And again a frame later.
    m.idle_to(63 * 100 + 312 * 63);
    assert!(m.interrupt_waiting());
}

#[test]
fn ntsc_has_65_cycles_by_263_lines() {
    let mut m = C64::new(C64Clock::Ntsc, &[]);
    assert_eq!(m.sys.timing.frame_cycles(), 65 * 263);
    m.idle_to(65 * 262 + 1);
    assert_eq!(
        m.read(0xd012) as u32 | ((m.read(0xd011) as u32 & 0x80) << 1),
        262
    );
    assert_eq!(m.ram()[0x02a6], 0, "the PAL flag");
    assert_eq!(pal().ram()[0x02a6], 1);
}

#[test]
fn the_stand_in_kernal_runs_an_irq_through_the_ram_vector_and_returns() {
    let mut m = pal();
    // LDA #$11; LDX #$22; LDY #$33; CLI; then NOPs.
    m.load(0x0800, &[0xa9, 0x11, 0xa2, 0x22, 0xa0, 0x33, 0x58]);
    for i in 0..0x4000u16 {
        m.load(0x0807 + i, &[0xea]);
    }
    m.cpu.pc = 0x0800;
    let mut taken = false;
    let mut after = 0;
    for _ in 0..30_000 {
        if m.step() == Some(Taken::Irq) {
            taken = true;
        }
        if taken {
            after += 1;
            if after > 40 && m.cpu.pc >= 0x0807 && m.cpu.pc < 0x4807 {
                break;
            }
        }
    }
    assert!(taken, "the KERNAL's timer interrupt came");
    assert_eq!(
        (m.cpu.a, m.cpu.x, m.cpu.y),
        (0x11, 0x22, 0x33),
        "the handler saved and restored them"
    );
    assert_eq!(m.cpu.p & I, 0, "and RTI restored the flags");
    assert!(m.cpu.pc >= 0x0807 && m.cpu.pc < 0x4807);
    // The default handler acknowledged CIA 1.
    assert_eq!(m.read(0xdc0d) & 0x80, 0);
}

#[test]
fn a_brk_reaches_the_kernals_brk_vector() {
    let mut m = pal();
    m.load(0x0800, &[0x00, 0xea]);
    m.cpu.pc = 0x0800;
    m.cpu.p = U | I;
    for _ in 0..30 {
        m.step();
        if m.cpu.pc == KERNAL_BRK_HANDLER {
            break;
        }
    }
    assert_eq!(m.cpu.pc, KERNAL_BRK_HANDLER);
}

#[test]
fn psid_banking_per_call_address() {
    assert_eq!(psid_banks_for(0x1000), 0x37);
    assert_eq!(psid_banks_for(0x9fff), 0x37);
    assert_eq!(psid_banks_for(0xa000), 0x36);
    assert_eq!(psid_banks_for(0xcfff), 0x36);
    assert_eq!(psid_banks_for(0xd000), 0x34);
    assert_eq!(psid_banks_for(0xdfff), 0x34);
    assert_eq!(psid_banks_for(0xe000), 0x35);
}

#[test]
fn call_routine_ends_with_the_way_the_routine_leaves() {
    let ends = |code: &[u8]| {
        let mut m = pal();
        m.load(0x1000, code);
        m.set_banks(0x37);
        call_routine(&mut m, 0x1000, 0, 100_000)
    };
    assert_eq!(ends(&[0x60]), CallEnd::Return); // RTS
    assert_eq!(ends(&[0x4c, 0x31, 0xea]), CallEnd::KernalExit); // JMP $EA31
    assert_eq!(ends(&[0x40]), CallEnd::Rti); // RTI as an interrupt handler's last instruction
    assert_eq!(ends(&[0x02]), CallEnd::Jam);
    assert_eq!(ends(&[0x00]), CallEnd::Brk);
    assert_eq!(ends(&[0x4c, 0x00, 0x10]), CallEnd::Timeout); // JMP $1000, for ever
                                                             // A call arrives with A as given and returns to the trap.
    let mut m = pal();
    m.load(0x1000, &[0x85, 0xf0, 0x60]);
    call_routine(&mut m, 0x1000, 7, 1000);
    assert_eq!(m.ram()[0xf0], 7);
    assert_eq!(m.cpu.pc, RETURN_TRAP);
}

// --- The runner (the DAW's runner tests) ----------------------------------------

fn counting_tune() -> Vec<u8> {
    build_psid(
        &Header {
            play: 0x1005,
            ..Header::default()
        },
        &place_code(&[
            (0x00, &[0xa9, 0x00, 0x85, 0xf0, 0x60]),
            (0x05, &[&COUNT_TO_D400[..], &[0x60]].concat()),
        ]),
    )
}

#[test]
fn a_host_driven_tune_is_called_once_per_pal_frame_and_each_write_has_its_cycle() {
    let file = file_of(&counting_tune());
    let mut r = created(&file, 0);
    assert_eq!(r.mode(), RunMode::Play);
    let w: Vec<_> = writes_over(&mut r, 5 * 312 * 63, 1000)
        .into_iter()
        .filter(|w| w.1 == 0)
        .collect();
    assert_eq!(w.iter().map(|w| w.2).collect::<Vec<_>>(), [1, 2, 3, 0, 1]);
    let gaps: Vec<_> = w.windows(2).map(|p| p[1].0 - p[0].0).collect();
    assert_eq!(gaps, [312 * 63; 4]);
    // The first write is where the code puts it: after LDX zp (3), INX (2), TXA (2), AND # (2), STA zp (3) = 12 cycles.
    assert_eq!(w[0].0, 12);
}

#[test]
fn the_same_writes_however_finely_it_is_advanced() {
    let file = file_of(&counting_tune());
    let total = 12 * 312 * 63;
    let whole = writes_over(&mut created(&file, 0), total, total);
    for step in [1u64, 7, 1000, 2500, 20_000] {
        let parts = writes_over(&mut created(&file, 0), total - total % step, step);
        let cut = whole
            .iter()
            .take_while(|w| w.0 < total - total % step)
            .count();
        assert_eq!(parts, whole[..cut], "step {step}");
    }
    assert!(whole.len() > 5);
}

#[test]
fn a_write_is_kept_until_its_cycle_has_been_asked_for() {
    let file = file_of(&counting_tune());
    let mut r = created(&file, 0);
    r.advance(312 * 63 * 3);
    let mut early = vec![];
    r.drain(1, 1, |w| early.push(w));
    assert!(early.iter().all(|w| w.cycle < 1));
    let mut rest = vec![];
    r.drain(312 * 63 * 3, 1, |w| rest.push(w));
    assert_eq!(rest.iter().filter(|w| w.reg == 0).count(), 3);
}

#[test]
fn a_cia_timed_tune_ticks_at_the_timers_period_and_follows_it_when_reprogrammed() {
    // Init sets CIA 1 timer A's latch to 9999: a tick every 10000 cycles.
    let cia = build_psid(
        &Header {
            play: 0x1010,
            speed: 1,
            ..Header::default()
        },
        &place_code(&[
            (
                0x00,
                &[
                    0xa9, 0x0f, 0x8d, 0x04, 0xdc, 0xa9, 0x27, 0x8d, 0x05, 0xdc, 0x60,
                ],
            ),
            (0x10, &[&COUNT_TO_D400[..], &[0x60]].concat()),
        ]),
    );
    let mut r = created(&file_of(&cia), 0);
    assert_eq!(r.tick_cycles(), 10_000);
    let w: Vec<_> = writes_over(&mut r, 50_000, 700)
        .into_iter()
        .filter(|w| w.1 == 0)
        .collect();
    assert!(w.len() >= 5);
    assert_eq!(w[1].0 - w[0].0, 10_000);
    // Rubicon starts at 60 Hz and sets about 124 Hz in its first play call: play sets the latch to 4999.
    let retimed = build_psid(
        &Header {
            play: 0x1010,
            speed: 1,
            ..Header::default()
        },
        &place_code(&[
            (
                0x00,
                &[
                    0xa9, 0x0f, 0x8d, 0x04, 0xdc, 0xa9, 0x27, 0x8d, 0x05, 0xdc, 0x60,
                ],
            ),
            (
                0x10,
                &[
                    &COUNT_TO_D400[..],
                    &[
                        0xa9, 0x87, 0x8d, 0x04, 0xdc, 0xa9, 0x13, 0x8d, 0x05, 0xdc, 0x60,
                    ],
                ]
                .concat(),
            ),
        ]),
    );
    let mut r = created(&file_of(&retimed), 0);
    assert_eq!(r.tick_cycles(), 10_000);
    let w: Vec<_> = writes_over(&mut r, 60_000, 700)
        .into_iter()
        .filter(|w| w.1 == 0)
        .collect();
    assert_eq!(r.tick_cycles(), 5_000);
    let gaps: Vec<_> = w.windows(2).map(|p| p[1].0 - p[0].0).collect();
    assert!(gaps.len() > 5);
    assert!(gaps[1..].iter().all(|&g| g == 5_000), "{gaps:?}");
}

#[test]
fn a_play_routine_that_overruns_its_tick_starts_the_next_call_when_it_ends() {
    // Play burns about 26000 cycles (more than a frame of 19656): LDX #0 ; loop DEX ; BNE loop (256 * 5 = 1280 per pass): use a nested loop.
    // LDY #$14; outer: LDX #0; inner: DEX; BNE inner; DEY; BNE outer; INC $F0; LDA $F0; STA $D400; RTS
    let play: &[u8] = &[
        0xa0, 0x14, 0xa2, 0x00, 0xca, 0xd0, 0xfd, 0x88, 0xd0, 0xf8, 0xe6, 0xf0, 0xa5, 0xf0, 0x8d,
        0x00, 0xd4, 0x60,
    ];
    let tune = build_psid(
        &Header::default(),
        &place_code(&[(0x00, &[0x60]), (0x03, play)]),
    );
    let mut r = created(&file_of(&tune), 0);
    let w: Vec<_> = writes_over(&mut r, 200_000, 1000)
        .into_iter()
        .filter(|w| w.1 == 0)
        .collect();
    // 20 passes of (2 + 256 * 5 - 1 + 3...) cycles: whatever it is, it is over a frame, so calls start back to back.
    let gaps: Vec<_> = w.windows(2).map(|p| p[1].0 - p[0].0).collect();
    assert!(gaps.len() >= 3);
    assert!(gaps.iter().all(|&g| g > 312 * 63), "{gaps:?}");
    assert!(
        gaps.iter().all(|&g| g == gaps[0]),
        "a steady overrun: {gaps:?}"
    );
}

#[test]
fn a_tune_that_jams_stops_with_its_reason_and_the_horizon_is_unbounded() {
    let jam = build_psid(
        &Header::default(),
        &place_code(&[(0x00, &[0x60]), (0x03, &[0x02])]),
    );
    let mut r = created(&file_of(&jam), 0);
    r.advance(200_000);
    assert_eq!(
        r.ended(),
        Some(EndReason::PlayJam {
            addr: 0x1003,
            pc: 0x1003
        })
    );
    assert!(r.ended().unwrap().to_string().contains("stopped the CPU"));
    assert_eq!(r.horizon(), u64::MAX);
    // A BRK ends it too.
    let brk = build_psid(
        &Header::default(),
        &place_code(&[(0x00, &[0x60]), (0x03, &[0x00])]),
    );
    let mut r = created(&file_of(&brk), 0);
    r.advance(100_000);
    assert_eq!(r.ended(), Some(EndReason::PlayBrk { addr: 0x1003 }));
    // A routine that never returns is stopped after 2 million cycles.
    let spin = build_psid(
        &Header::default(),
        &place_code(&[(0x00, &[0x60]), (0x03, &[0x4c, 0x03, 0x10])]),
    );
    let mut r = created(&file_of(&spin), 0);
    r.advance(3_000_000);
    assert_eq!(r.ended(), Some(EndReason::PlayTimeout { addr: 0x1003 }));
}

#[test]
fn a_tune_whose_init_fails_is_refused_with_the_reason() {
    let init_jam = build_psid(&Header::default(), &place_code(&[(0x00, &[0x02])]));
    let e = PsidRunner::new(&file_of(&init_jam), 0).err().unwrap();
    assert_eq!(
        e,
        PsidError::InitJam {
            addr: 0x1000,
            pc: 0x1000
        }
    );
    assert_eq!(
        e.to_string(),
        "the tune's init routine ($1000) stopped the CPU (a JAM opcode at $1000)"
    );
    let init_brk = build_psid(&Header::default(), &place_code(&[(0x00, &[0x00])]));
    assert!(matches!(
        PsidRunner::new(&file_of(&init_brk), 0),
        Err(PsidError::InitBrk { .. })
    ));
    let init_spin = build_psid(
        &Header::default(),
        &place_code(&[(0x00, &[0x4c, 0x00, 0x10])]),
    );
    let e = PsidRunner::new(&file_of(&init_spin), 0).err().unwrap();
    assert_eq!(
        e.to_string(),
        "the tune's init routine ($1000) did not return in 60 million cycles"
    );
}

#[test]
fn init_gets_the_subsong_in_a_and_what_it_wrote_is_the_state_at_cycle_zero() {
    // init: STA $D400 (the subsong), LDA #$0F, STA $D418, RTS. play: RTS.
    let tune = build_psid(
        &Header {
            songs: 5,
            ..Header::default()
        },
        &place_code(&[
            (
                0x00,
                &[0x8d, 0x00, 0xd4, 0xa9, 0x0f, 0x8d, 0x18, 0xd4, 0x60],
            ),
            (0x09, &[0x60]),
        ]),
    );
    let file = file_of(&tune);
    let mut r = PsidRunner::new(&file, 3).unwrap();
    let mut early = vec![];
    r.drain(0, 1, |w| early.push((w.cycle, w.reg, w.value)));
    assert_eq!(early, [(0, 0x00, 3), (0, 0x18, 0x0f)]);
    // A subsong past the last is the last.
    let mut r = PsidRunner::new(&file, 99).unwrap();
    let mut early = vec![];
    r.drain(0, 1, |w| early.push((w.reg, w.value)));
    assert_eq!(early[0], (0x00, 4));
}

#[test]
fn writes_to_chips_the_caller_has_no_room_for_are_counted_not_handed_over() {
    let tune = build_psid(
        &Header {
            version: 3,
            second: 0x42,
            ..Header::default()
        },
        &place_code(&[
            (0x00, &[0x60]),
            (
                0x03,
                &[0xa9, 0x05, 0x8d, 0x00, 0xd4, 0x8d, 0x20, 0xd4, 0x60],
            ),
        ]),
    );
    let file = file_of(&tune);
    assert_eq!(file.extra_sids, [0xd420]);
    let mut r = created(&file, 0);
    assert_eq!(r.sid_count(), 2);
    r.advance(100);
    let mut got = vec![];
    r.drain(100, 1, |w| got.push((w.chip, w.reg, w.value)));
    assert_eq!(got, [(0, 0, 5)]);
    assert_eq!(r.extra_sid_writes(), 1);
}

#[test]
fn an_interrupt_driven_tune_runs_from_its_own_cia_interrupt() {
    // RSID: init installs a handler at $0314 and starts timer A at 10000 cycles; the handler counts into $D400.
    // init: SEI; LDA #<h; STA $0314; LDA #>h; STA $0315; LDA #$7F; STA $DC0D; LDA #<9999; STA $DC04; LDA #>9999; STA $DC05;
    //       LDA #$81; STA $DC0D; LDA #$11; STA $DC0E; CLI; RTS
    // h: INC $F0; LDA $F0; STA $D400; LDA $DC0D; JMP $EA81
    let init: Vec<u8> = vec![
        0x78, 0xa9, 0x30, 0x8d, 0x14, 0x03, 0xa9, 0x10, 0x8d, 0x15, 0x03, 0xa9, 0x7f, 0x8d, 0x0d,
        0xdc, 0xa9, 0x0f, 0x8d, 0x04, 0xdc, 0xa9, 0x27, 0x8d, 0x05, 0xdc, 0xa9, 0x81, 0x8d, 0x0d,
        0xdc, 0xa9, 0x11, 0x8d, 0x0e, 0xdc, 0x58, 0x60,
    ];
    let handler: Vec<u8> = vec![
        0xe6, 0xf0, 0xa5, 0xf0, 0x8d, 0x00, 0xd4, 0xad, 0x0d, 0xdc, 0x4c, 0x81, 0xea,
    ];
    let tune = build_psid(
        &Header {
            magic: b"RSID",
            play: 0,
            ..Header::default()
        },
        &place_code(&[(0x00, &init), (0x30, &handler)]),
    );
    let file = file_of(&tune);
    assert_eq!(file.kind, PsidType::Rsid);
    let mut r = created(&file, 0);
    assert_eq!(r.mode(), RunMode::Irq);
    let w = writes_over(&mut r, 100_000, 700);
    let d400: Vec<_> = w.iter().filter(|w| w.1 == 0).collect();
    assert!(d400.len() >= 9, "{d400:?}");
    let gaps: Vec<_> = d400.windows(2).map(|p| p[1].0 - p[0].0).collect();
    assert!(
        gaps.iter().all(|&g| g == 10_000),
        "an interrupt every 10000 cycles: {gaps:?}"
    );
    assert_eq!(
        d400.iter().map(|w| w.2).take(4).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert_eq!(r.ended(), None);
    // The same writes however it is advanced.
    let again = writes_over(&mut created(&file, 0), 100_000, 99_999);
    assert_eq!(again[..], w[..again.len()]);
}

#[test]
fn an_interrupt_driven_tune_that_jams_ends() {
    // RSID whose init is JAM: the call has not returned, so the CPU simply jams.
    let tune = build_psid(
        &Header {
            magic: b"RSID",
            play: 0,
            ..Header::default()
        },
        &[0x02],
    );
    let mut r = created(&file_of(&tune), 0);
    r.advance(100_000);
    assert_eq!(r.ended(), Some(EndReason::Jam { pc: 0x1000 }));
    // One that masks interrupts and returns idles: no panic, time passes, no writes.
    let quiet = build_psid(
        &Header {
            magic: b"RSID",
            play: 0,
            ..Header::default()
        },
        &[0x78, 0x60],
    );
    let mut r = created(&file_of(&quiet), 0);
    r.advance(5_000_000);
    assert_eq!(r.ended(), None);
    assert_eq!(r.pending(), 0);
}

#[test]
fn an_ntsc_tune_runs_on_the_ntsc_clock() {
    let tune = build_psid(
        &Header {
            flags: 0x18,
            ..Header::default()
        },
        &place_code(&[
            (0x00, &[0x60]),
            (0x03, &[0xe6, 0xf0, 0xa5, 0xf0, 0x8d, 0x00, 0xd4, 0x60]),
        ]),
    );
    let file = file_of(&tune);
    assert_eq!(file.clock, PsidClock::Ntsc);
    let mut r = created(&file, 0);
    assert_eq!((r.clock(), r.clock_hz()), (C64Clock::Ntsc, 1_022_727));
    let w = writes_over(&mut r, 5 * 263 * 65, 5000);
    let gaps: Vec<_> = w.windows(2).map(|p| p[1].0 - p[0].0).collect();
    assert_eq!(gaps, [263 * 65; 4]);
}

#[test]
fn the_queue_does_not_grow_while_a_tune_plays() {
    let file = file_of(&counting_tune());
    let mut r = created(&file, 0);
    let cap = r.queue_capacity();
    for t in 1..=2000u64 {
        r.advance(t * 500);
        r.drain(t * 500, 1, |_| {});
    }
    assert_eq!(r.queue_capacity(), cap);
}

// --- The file ----------------------------------------------------------------------

#[test]
fn a_v2_file_parses_with_its_fields() {
    let mut b = build_psid(
        &Header {
            songs: 7,
            start: 3,
            speed: 0b101,
            init: 0x1000,
            play: 0x1003,
            flags: 0b0001_1000_0100 | 0x20,
            ..Header::default()
        },
        &[0x60, 0x60, 0x60, 0x60],
    );
    b[0x78] = 0x10;
    b[0x79] = 4;
    let f = file_of(&b);
    assert_eq!((f.kind, f.version), (PsidType::Psid, 2));
    assert_eq!(
        (f.load_address, f.init_address, f.play_address),
        (0x1000, 0x1000, 0x1003)
    );
    assert_eq!((f.songs, f.start_song, f.speed), (7, 3, 5));
    assert_eq!((f.name.as_str(), f.author.as_str()), ("Test", "Tests"));
    assert_eq!(f.clock, PsidClock::Pal);
    assert_eq!(f.sid_model, PsidSidModel::Sid8580);
    assert_eq!((f.reloc_start_page, f.reloc_pages), (0x10, 4));
    assert_eq!(f.data, [0x60; 4]);
    assert!(f.uses_cia(1) && !f.uses_cia(2) && f.uses_cia(3));
    assert_eq!(f.start_subsong(), 2);
    // An RSID never uses the CIA speed bit; a start song out of range reads as 1.
    let mut r = build_psid(
        &Header {
            magic: b"RSID",
            speed: 1,
            start: 99,
            songs: 2,
            ..Header::default()
        },
        &[0x60],
    );
    r[0x76] = 0;
    r[0x77] = 0;
    let f = file_of(&r);
    assert!(!f.uses_cia(1));
    assert_eq!(f.start_song, 1);
}

#[test]
fn load_address_zero_takes_the_first_two_data_bytes() {
    let b = build_psid(
        &Header {
            load: 0,
            init: 0,
            play: 0,
            ..Header::default()
        },
        &[0x00, 0x20, 0x60, 0x60],
    );
    let f = file_of(&b);
    assert_eq!(f.load_address, 0x2000);
    assert_eq!(f.init_address, 0x2000, "init 0 is the load address");
    assert_eq!(f.data, [0x60, 0x60]);
}

#[test]
fn a_v1_header_and_the_extra_sid_bytes() {
    let b = build_psid(
        &Header {
            version: 1,
            ..Header::default()
        },
        &[0x60],
    );
    let f = file_of(&b);
    assert_eq!(
        (f.version, f.flags, f.clock, f.sid_model),
        (1, 0, PsidClock::Unknown, PsidSidModel::Unknown)
    );
    // v3: a second SID at $D420 and the models of SID 2.
    let b = build_psid(
        &Header {
            version: 3,
            second: 0x42,
            flags: 0x14 | (2 << 6),
            ..Header::default()
        },
        &[0x60],
    );
    let f = file_of(&b);
    assert_eq!(f.extra_sids, [0xd420]);
    assert_eq!(f.extra_models[0], PsidSidModel::Sid8580);
    // v4: a third at $DE00; an odd or out-of-range byte is no SID; the same address twice is one.
    let b = build_psid(
        &Header {
            version: 4,
            second: 0x42,
            third: 0xe0,
            ..Header::default()
        },
        &[0x60],
    );
    assert_eq!(file_of(&b).extra_sids, [0xd420, 0xde00]);
    let b = build_psid(
        &Header {
            version: 4,
            second: 0x42,
            third: 0x42,
            ..Header::default()
        },
        &[0x60],
    );
    assert_eq!(file_of(&b).extra_sids, [0xd420]);
    let b = build_psid(
        &Header {
            version: 4,
            second: 0x43,
            third: 0x20,
            ..Header::default()
        },
        &[0x60],
    );
    assert!(file_of(&b).extra_sids.is_empty());
    // v2 has no such bytes.
    let b = build_psid(
        &Header {
            version: 2,
            second: 0x42,
            ..Header::default()
        },
        &[0x60],
    );
    assert!(file_of(&b).extra_sids.is_empty());
}

#[test]
fn bad_files_are_refused_with_the_reason() {
    let reason = |b: &[u8]| PsidFile::parse(b).err().unwrap().to_string();
    assert_eq!(reason(b"nope"), "it is not a SID file (no PSID/RSID magic)");
    assert_eq!(
        reason(b"PSID\0\x02"),
        "the file is 6 bytes, shorter than a SID header"
    );
    let ok = build_psid(&Header::default(), &[0x60]);
    let mut b = ok.clone();
    b[5] = 9;
    assert_eq!(
        reason(&b),
        "its header is version 9; SID files are versions 1-4"
    );
    let mut b = ok.clone();
    b[7] = 0x80;
    assert_eq!(
        reason(&b),
        "its data starts at $0080; a version 2 file's starts at $007C"
    );
    let b = build_psid(
        &Header {
            magic: b"RSID",
            version: 1,
            ..Header::default()
        },
        &[0x60],
    );
    assert_eq!(
        reason(&b),
        "it is an RSID file of version 1, which the format does not have"
    );
    assert_eq!(reason(&ok[..0x7c]), "the file has no C64 data");
    let b = build_psid(
        &Header {
            load: 0xffff,
            ..Header::default()
        },
        &[0x60, 0x60],
    );
    assert_eq!(
        reason(&b),
        "its 2 bytes load at $FFFF and run past the end of the C64's memory"
    );
    let b = build_psid(
        &Header {
            songs: 0,
            ..Header::default()
        },
        &[0x60],
    );
    assert_eq!(reason(&b), "its header says 0 songs; a SID file has 1-256");
    let b = build_psid(
        &Header {
            songs: 257,
            ..Header::default()
        },
        &[0x60],
    );
    assert_eq!(
        reason(&b),
        "its header says 257 songs; a SID file has 1-256"
    );
    let b = build_psid(
        &Header {
            flags: 0x15,
            ..Header::default()
        },
        &[0x60],
    );
    assert_eq!(
        reason(&b),
        "it holds Compute!'s Sidplayer (MUS) data, not a program to run"
    );
    let b = build_psid(
        &Header {
            magic: b"RSID",
            flags: 0x16,
            ..Header::default()
        },
        &[0x60],
    );
    assert!(reason(&b).starts_with("it is a C64 BASIC program"));
    assert!(super::file::looks_like_psid(&ok));
    assert!(!super::file::looks_like_psid(b"PSI"));
}
