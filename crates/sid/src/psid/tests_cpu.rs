//! The 6510 against reference formulas and the documented timings.

use super::cpu::{instruction_len, mnemonic, Cpu6510, CpuBus, B, C, D, I, N, U, V, Z};

/// Flat RAM, every write kept.
struct FlatRam {
    mem: Box<[u8; 0x10000]>,
    writes: Vec<(u16, u8)>,
}

impl FlatRam {
    fn new() -> FlatRam {
        FlatRam {
            mem: vec![0u8; 0x10000].into_boxed_slice().try_into().unwrap(),
            writes: Vec::new(),
        }
    }
}

impl CpuBus for FlatRam {
    fn read(&mut self, addr: u16, _cycle: u64) -> u8 {
        self.mem[addr as usize]
    }
    fn write(&mut self, addr: u16, value: u8, _cycle: u64) {
        self.writes.push((addr, value));
        self.mem[addr as usize] = value;
    }
}

/// A CPU over flat RAM with `code` at `at` and PC there.
fn machine_at(code: &[u8], at: u16) -> (Cpu6510, FlatRam) {
    let mut ram = FlatRam::new();
    ram.mem[at as usize..at as usize + code.len()].copy_from_slice(code);
    let mut cpu = Cpu6510::new();
    cpu.pc = at;
    (cpu, ram)
}

fn machine(code: &[u8]) -> (Cpu6510, FlatRam) {
    machine_at(code, 0x0200)
}

fn steps(cpu: &mut Cpu6510, ram: &mut FlatRam, n: usize) -> Vec<u32> {
    (0..n).map(|_| cpu.step(ram)).collect()
}

/// Bruce Clark's decimal-mode ADC for the NMOS 6502 ("The 6502/65C02/65C816
/// Decimal Mode", sequences 1 and 2): the result, and the C, N, V flags; Z is the
/// binary sum's.
fn clark_adc(a: u8, b: u8, c: u8) -> (u8, bool, bool, bool, bool) {
    let (a, b, c) = (a as i32, b as i32, c as i32);
    // Sequence 1 (A and the carry).
    let mut al = (a & 0x0f) + (b & 0x0f) + c;
    if al >= 0x0a {
        al = ((al + 0x06) & 0x0f) + 0x10;
    }
    let mut r = (a & 0xf0) + (b & 0xf0) + al;
    // Sequence 2 (N and V), from the sum before the high digit is corrected, signed.
    let s = (a as i8 as i32 & -16) + (b as i8 as i32 & -16) + al;
    let n = (s & 0x80) != 0;
    let v = !(-128..=127).contains(&s);
    if r >= 0xa0 {
        r += 0x60;
    }
    let carry = r >= 0x100;
    let z = ((a + b + c) & 0xff) == 0;
    ((r & 0xff) as u8, carry, n, v, z)
}

/// The same for SBC (sequence 3): the NMOS 6502's flags are the binary subtraction's.
fn clark_sbc(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i32, b as i32, c as i32);
    let mut al = (a & 0x0f) - (b & 0x0f) + c - 1;
    if al < 0 {
        al = ((al - 0x06) & 0x0f) - 0x10;
    }
    let mut r = (a & 0xf0) - (b & 0xf0) + al;
    if r < 0 {
        r -= 0x60;
    }
    (r & 0xff) as u8
}

#[test]
fn binary_adc_and_sbc_against_the_arithmetic_for_every_input() {
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            for c in 0..=1u8 {
                // ADC #b
                let (mut cpu, mut ram) = machine(&[0x69, b]);
                cpu.a = a;
                cpu.p = U | c;
                cpu.step(&mut ram);
                let sum = a as u32 + b as u32 + c as u32;
                let r = (sum & 0xff) as u8;
                assert_eq!(cpu.a, r, "adc {a:#x}+{b:#x}+{c}");
                let v = ((a ^ r) & (b ^ r) & 0x80) != 0;
                let want = (if sum > 0xff { C } else { 0 })
                    | (if v { V } else { 0 })
                    | (if r == 0 { Z } else { 0 })
                    | (r & N)
                    | U;
                assert_eq!(cpu.p, want, "adc flags {a:#x}+{b:#x}+{c}");
                // SBC #b
                let (mut cpu, mut ram) = machine(&[0xe9, b]);
                cpu.a = a;
                cpu.p = U | c;
                cpu.step(&mut ram);
                let diff = a as i32 - b as i32 - (1 - c as i32);
                let r = (diff & 0xff) as u8;
                assert_eq!(cpu.a, r, "sbc {a:#x}-{b:#x}-{}", 1 - c);
                let v = ((a ^ b) & (a ^ r) & 0x80) != 0;
                let want = (if diff >= 0 { C } else { 0 })
                    | (if v { V } else { 0 })
                    | (if r == 0 { Z } else { 0 })
                    | (r & N)
                    | U;
                assert_eq!(cpu.p, want, "sbc flags {a:#x}-{b:#x}-{}", 1 - c);
            }
        }
    }
}

#[test]
fn decimal_adc_and_sbc_against_clarks_sequences_for_every_input() {
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            for c in 0..=1u8 {
                let (mut cpu, mut ram) = machine(&[0x69, b]);
                cpu.a = a;
                cpu.p = U | D | c;
                cpu.step(&mut ram);
                let (r, carry, n, v, z) = clark_adc(a, b, c);
                assert_eq!(cpu.a, r, "decimal adc {a:#x}+{b:#x}+{c}");
                let want = U
                    | D
                    | (if carry { C } else { 0 })
                    | (if n { N } else { 0 })
                    | (if v { V } else { 0 })
                    | (if z { Z } else { 0 });
                assert_eq!(cpu.p, want, "decimal adc flags {a:#x}+{b:#x}+{c}");

                let (mut cpu, mut ram) = machine(&[0xe9, b]);
                cpu.a = a;
                cpu.p = U | D | c;
                cpu.step(&mut ram);
                assert_eq!(
                    cpu.a,
                    clark_sbc(a, b, c),
                    "decimal sbc {a:#x}-{b:#x}, c={c}"
                );
                // Its flags are the binary subtraction's.
                let diff = a as i32 - b as i32 - (1 - c as i32);
                let rb = (diff & 0xff) as u8;
                let vb = ((a ^ b) & (a ^ rb) & 0x80) != 0;
                let want = U
                    | D
                    | (if diff >= 0 { C } else { 0 })
                    | (if vb { V } else { 0 })
                    | (if rb == 0 { Z } else { 0 })
                    | (rb & N);
                assert_eq!(cpu.p, want, "decimal sbc flags {a:#x}-{b:#x}, c={c}");
            }
        }
    }
}

#[test]
fn decimal_arithmetic_the_way_the_daws_tests_state_it() {
    // $58 + $46 = $04, carry; N and V from the unadjusted sum, Z from the binary one.
    let (mut cpu, mut ram) = machine(&[0xf8, 0x18, 0xa9, 0x58, 0x69, 0x46]);
    steps(&mut cpu, &mut ram, 4);
    assert_eq!(cpu.a, 0x04);
    assert_eq!(cpu.p & (N | V | Z | C), N | V | C);
    // $99 + $01 = $00 with carry, yet Z clear ($9A in binary).
    let (mut cpu, mut ram) = machine(&[0xf8, 0x18, 0xa9, 0x99, 0x69, 0x01]);
    steps(&mut cpu, &mut ram, 4);
    assert_eq!(cpu.a, 0x00);
    assert_eq!(cpu.p & (Z | C), C);
    // $46 - $12 = $34; $12 - $21 = $91 with a borrow; $EB is SBC #imm too.
    let (mut cpu, mut ram) = machine(&[0xf8, 0x38, 0xa9, 0x46, 0xeb, 0x12]);
    steps(&mut cpu, &mut ram, 4);
    assert_eq!(cpu.a, 0x34);
    let (mut cpu, mut ram) = machine(&[0xf8, 0x38, 0xa9, 0x12, 0xe9, 0x21]);
    steps(&mut cpu, &mut ram, 4);
    assert_eq!(cpu.a, 0x91);
    assert_eq!(cpu.p & (N | C), N);
}

#[test]
fn loads_transfers_and_logic_set_n_and_z() {
    for v in 0..=255u8 {
        let nz = (v & N) | if v == 0 { Z } else { 0 };
        let check = |code: &[u8],
                     setup: &dyn Fn(&mut Cpu6510, &mut FlatRam),
                     get: &dyn Fn(&Cpu6510) -> u8| {
            let (mut cpu, mut ram) = machine(code);
            cpu.p = U | C | V | if v == 0 { N } else { Z }; // flags the op must overwrite
            setup(&mut cpu, &mut ram);
            cpu.step(&mut ram);
            assert_eq!(get(&cpu), v, "{:?} {v:#x}", mnemonic(code[0]));
            assert_eq!(cpu.p & (N | Z), nz, "{} {v:#x}", mnemonic(code[0]));
            assert_eq!(
                cpu.p & (C | V),
                C | V,
                "{} must keep C and V",
                mnemonic(code[0])
            );
        };
        check(&[0xa9, v], &|_, _| {}, &|c| c.a); // LDA #
        check(&[0xa2, v], &|_, _| {}, &|c| c.x); // LDX #
        check(&[0xa0, v], &|_, _| {}, &|c| c.y); // LDY #
        check(&[0xaa], &|c, _| c.a = v, &|c| c.x); // TAX
        check(&[0xa8], &|c, _| c.a = v, &|c| c.y); // TAY
        check(&[0x8a], &|c, _| c.x = v, &|c| c.a); // TXA
        check(&[0x98], &|c, _| c.y = v, &|c| c.a); // TYA
        check(&[0xba], &|c, _| c.sp = v, &|c| c.x); // TSX
        check(&[0xe8], &|c, _| c.x = v.wrapping_sub(1), &|c| c.x); // INX
        check(&[0xc8], &|c, _| c.y = v.wrapping_sub(1), &|c| c.y); // INY
        check(&[0xca], &|c, _| c.x = v.wrapping_add(1), &|c| c.x); // DEX
        check(&[0x88], &|c, _| c.y = v.wrapping_add(1), &|c| c.y); // DEY
        check(&[0x49, 0x5a], &|c, _| c.a = v ^ 0x5a, &|c| c.a); // EOR #
    }
}

#[test]
fn txs_changes_no_flags() {
    let (mut cpu, mut ram) = machine(&[0x9a]);
    cpu.x = 0;
    cpu.p = U | N;
    cpu.step(&mut ram);
    assert_eq!(cpu.sp, 0);
    assert_eq!(cpu.p, U | N);
}

#[test]
fn compares_for_every_pair() {
    for r in 0..=255u8 {
        for m in 0..=255u8 {
            for (code, set) in [
                (0xc9u8, 0usize), // CMP #
                (0xe0, 1),        // CPX #
                (0xc0, 2),        // CPY #
            ] {
                let (mut cpu, mut ram) = machine(&[code, m]);
                match set {
                    0 => cpu.a = r,
                    1 => cpu.x = r,
                    _ => cpu.y = r,
                }
                cpu.p = U | V;
                cpu.step(&mut ram);
                let d = r.wrapping_sub(m);
                let want =
                    U | V | (if r >= m { C } else { 0 }) | (if d == 0 { Z } else { 0 }) | (d & N);
                assert_eq!(cpu.p, want, "{} {r:#x},{m:#x}", mnemonic(code));
            }
        }
    }
}

#[test]
fn shifts_and_rotates_for_every_value() {
    for v in 0..=255u8 {
        for c in 0..=1u8 {
            // (opcode, expected result, expected carry)
            let cases = [
                (0x0au8, v << 1, v >> 7),           // ASL A
                (0x4a, v >> 1, v & 1),              // LSR A
                (0x2a, (v << 1) | c, v >> 7),       // ROL A
                (0x6a, (v >> 1) | (c << 7), v & 1), // ROR A
            ];
            for (op, result, carry) in cases {
                let (mut cpu, mut ram) = machine(&[op]);
                cpu.a = v;
                cpu.p = U | c;
                cpu.step(&mut ram);
                assert_eq!(cpu.a, result, "{} {v:#x} c={c}", mnemonic(op));
                let want = U | carry | (result & N) | (if result == 0 { Z } else { 0 });
                assert_eq!(cpu.p, want, "{} flags {v:#x} c={c}", mnemonic(op));
            }
            // Memory forms: ASL/ROL/LSR/ROR zp at $06/$26/$46/$66.
            for (op, result, carry) in [
                (0x06u8, v << 1, v >> 7),
                (0x46, v >> 1, v & 1),
                (0x26, (v << 1) | c, v >> 7),
                (0x66, (v >> 1) | (c << 7), v & 1),
            ] {
                let (mut cpu, mut ram) = machine(&[op, 0x10]);
                ram.mem[0x10] = v;
                cpu.p = U | c;
                cpu.step(&mut ram);
                assert_eq!(ram.mem[0x10], result, "{} zp {v:#x}", mnemonic(op));
                assert_eq!(
                    cpu.p,
                    U | carry | (result & N) | (if result == 0 { Z } else { 0 })
                );
            }
        }
    }
}

#[test]
fn inc_dec_and_bit() {
    for v in 0..=255u8 {
        for (op, r) in [(0xe6u8, v.wrapping_add(1)), (0xc6, v.wrapping_sub(1))] {
            let (mut cpu, mut ram) = machine(&[op, 0x10]);
            ram.mem[0x10] = v;
            cpu.p = U | C;
            cpu.step(&mut ram);
            assert_eq!(ram.mem[0x10], r);
            assert_eq!(cpu.p, U | C | (r & N) | (if r == 0 { Z } else { 0 }));
        }
        for a in [0x00u8, 0x01, 0x80, 0xff, 0x40] {
            let (mut cpu, mut ram) = machine(&[0x24, 0x10]);
            ram.mem[0x10] = v;
            cpu.a = a;
            cpu.p = U | C;
            cpu.step(&mut ram);
            let want = U | C | (v & (N | V)) | (if a & v == 0 { Z } else { 0 });
            assert_eq!(cpu.p, want, "bit {a:#x} & {v:#x}");
        }
    }
}

#[test]
fn stack_jumps_and_the_b_flag() {
    // PHA / PLA
    let (mut cpu, mut ram) = machine(&[0x48, 0xa9, 0x00, 0x68]);
    cpu.a = 0x80;
    steps(&mut cpu, &mut ram, 3);
    assert_eq!(cpu.a, 0x80);
    assert_eq!(cpu.p & (N | Z), N);
    assert_eq!(cpu.sp, 0xff);
    // PHP pushes B and the unused bit; PLP drops B.
    let (mut cpu, mut ram) = machine(&[0x08, 0x28]);
    cpu.p = U | C | N;
    cpu.step(&mut ram);
    assert_eq!(ram.mem[0x1ff], U | C | N | B);
    cpu.step(&mut ram);
    assert_eq!(cpu.p, U | C | N);
    // JSR pushes the address of its last byte; RTS returns after it.
    let (mut cpu, mut ram) = machine(&[0x20, 0x00, 0x03, 0xea]);
    ram.mem[0x300] = 0x60;
    assert_eq!(cpu.step(&mut ram), 6);
    assert_eq!(cpu.pc, 0x0300);
    assert_eq!((ram.mem[0x1ff], ram.mem[0x1fe]), (0x02, 0x02));
    assert_eq!(cpu.step(&mut ram), 6);
    assert_eq!(cpu.pc, 0x0203);
    // RTI pulls P then PC (no +1).
    let (mut cpu, mut ram) = machine(&[0x40]);
    cpu.sp = 0xfc;
    ram.mem[0x1fd] = C | B;
    ram.mem[0x1fe] = 0x34;
    ram.mem[0x1ff] = 0x12;
    cpu.step(&mut ram);
    assert_eq!(cpu.pc, 0x1234);
    assert_eq!(cpu.p, U | C);
    // The stack wraps in page 1.
    let (mut cpu, mut ram) = machine(&[0x48]);
    cpu.sp = 0x00;
    cpu.a = 0x77;
    cpu.step(&mut ram);
    assert_eq!((ram.mem[0x100], cpu.sp), (0x77, 0xff));
}

#[test]
fn flag_instructions() {
    let (mut cpu, mut ram) = machine(&[0x38, 0xf8, 0x78, 0x18, 0xd8, 0x58, 0xb8]);
    cpu.p = U | V;
    steps(&mut cpu, &mut ram, 3);
    assert_eq!(cpu.p, U | V | C | D | I);
    steps(&mut cpu, &mut ram, 4);
    assert_eq!(cpu.p, U);
}

#[test]
fn branches_by_flag_with_their_cycles() {
    // opcode, flag, taken when the flag is set?
    for (op, flag, when_set) in [
        (0x10u8, N, false),
        (0x30, N, true),
        (0x50, V, false),
        (0x70, V, true),
        (0x90, C, false),
        (0xb0, C, true),
        (0xd0, Z, false),
        (0xf0, Z, true),
    ] {
        for set in [false, true] {
            let taken = set == when_set;
            for offset in [5i8, -3, 0x40, -0x40] {
                // A branch at $02F0 is followed by $02F2: +$40 crosses to $0332, -$40 to $02B2.
                let at = 0x02f0u16;
                let (mut cpu, mut ram) = machine_at(&[op, offset as u8], at);
                cpu.p = U | if set { flag } else { 0 };
                let cycles = cpu.step(&mut ram);
                let target = (at + 2).wrapping_add(offset as i16 as u16);
                if taken {
                    assert_eq!(cpu.pc, target, "{} taken", mnemonic(op));
                    let crossed = (at + 2) & 0xff00 != target & 0xff00;
                    assert_eq!(
                        cycles,
                        if crossed { 4 } else { 3 },
                        "{} offset {offset}",
                        mnemonic(op)
                    );
                } else {
                    assert_eq!(cpu.pc, at + 2);
                    assert_eq!(cycles, 2);
                }
            }
        }
    }
}

/// The documented base cycle counts of the official opcodes (0: not an official one).
#[rustfmt::skip]
const OFFICIAL_CYCLES: [u8; 256] = [
    7,6,0,0,0,3,5,0,3,2,2,0,0,4,6,0,
    2,5,0,0,0,4,6,0,2,4,0,0,0,4,7,0,
    6,6,0,0,3,3,5,0,4,2,2,0,4,4,6,0,
    2,5,0,0,0,4,6,0,2,4,0,0,0,4,7,0,
    6,6,0,0,0,3,5,0,3,2,2,0,3,4,6,0,
    2,5,0,0,0,4,6,0,2,4,0,0,0,4,7,0,
    6,6,0,0,0,3,5,0,4,2,2,0,5,4,6,0,
    2,5,0,0,0,4,6,0,2,4,0,0,0,4,7,0,
    0,6,0,0,3,3,3,0,2,0,2,0,4,4,4,0,
    2,6,0,0,4,4,4,0,2,5,2,0,0,5,0,0,
    2,6,2,0,3,3,3,0,2,2,2,0,4,4,4,0,
    2,5,0,0,4,4,4,0,2,4,2,0,4,4,4,0,
    2,6,0,0,3,3,5,0,2,2,2,0,4,4,6,0,
    2,5,0,0,0,4,6,0,2,4,0,0,0,4,7,0,
    2,6,0,0,3,3,5,0,2,2,2,0,4,4,6,0,
    2,5,0,0,0,4,6,0,2,4,0,0,0,4,7,0,
];

#[test]
fn every_official_opcode_takes_its_documented_cycles_and_bytes() {
    for op in 0..=255u8 {
        let base = OFFICIAL_CYCLES[op as usize];
        if base == 0 {
            continue;
        }
        let name = mnemonic(op);
        let branch = op & 0x1f == 0x10;
        let control = matches!(op, 0x00 | 0x20 | 0x40 | 0x60 | 0x4c | 0x6c);
        // Operands that stay in one page: zero-page pointers to $0300, indexes of 0.
        let (mut cpu, mut ram) = machine(&[op, 0x10, 0x03]);
        ram.mem[0x10] = 0x00;
        ram.mem[0x11] = 0x03;
        ram.mem[0x300] = 0x60;
        if branch {
            // Whichever way the flags go, one of the two states does not branch.
            let mut least = u32::MAX;
            for p in [U, U | 0xdf] {
                let (mut cpu, mut ram) = machine(&[op, 0x05]);
                cpu.p = p;
                least = least.min(cpu.step(&mut ram));
            }
            assert_eq!(least, 2, "{name} {op:#x}");
            continue;
        }
        cpu.sp = 0xff;
        let cycles = cpu.step(&mut ram);
        assert_eq!(cycles, base as u32, "{name} {op:#04x}: cycles");
        assert_eq!(cpu.cycles, base as u64);
        if !control {
            assert_eq!(
                cpu.pc,
                0x0200 + instruction_len(op) as u16,
                "{name} {op:#04x}: bytes"
            );
        }
    }
}

#[test]
fn a_page_crossing_costs_a_cycle_on_reads_only() {
    // ,X and ,Y loads and ALU ops: +1; stores and read-modify-writes: never.
    for op in 0..=255u8 {
        if OFFICIAL_CYCLES[op as usize] == 0 || op & 0x1f == 0x10 {
            continue;
        }
        let is_store = matches!(op, 0x91 | 0x99 | 0x9d);
        let indexed_read =
            (matches!(op & 0x1f, 0x11 | 0x19 | 0x1d) && !is_store) || matches!(op, 0xbe | 0xbc);
        let rmw = matches!(op, 0x1e | 0x3e | 0x5e | 0x7e | 0xde | 0xfe);
        if !(indexed_read || is_store || rmw)
            || matches!(op, 0x14 | 0x34 | 0x54 | 0x74 | 0xd4 | 0xf4)
        {
            continue;
        }
        let abs = matches!(op & 0x1f, 0x19 | 0x1d | 0x1e) || matches!(op, 0xbe | 0xbc);
        let (mut cpu, mut ram) = if abs {
            machine(&[op, 0xf0, 0x03])
        } else {
            // (zp),Y: the pointer at $10 is $03F0.
            let (cpu, mut ram) = machine(&[op, 0x10]);
            ram.mem[0x10] = 0xf0;
            ram.mem[0x11] = 0x03;
            (cpu, ram)
        };
        cpu.x = 0x20;
        cpu.y = 0x20;
        let cycles = cpu.step(&mut ram);
        let base = OFFICIAL_CYCLES[op as usize] as u32;
        let want = if indexed_read { base + 1 } else { base };
        assert_eq!(cycles, want, "{} {op:#04x}", mnemonic(op));
    }
}

#[test]
fn indexed_modes_find_their_addresses() {
    // LDA $0310,X ; LDA $0310,Y ; LDA ($10,X) ; LDA ($10),Y ; LDA $10,X (wraps in zero page)
    let (mut cpu, mut ram) = machine(&[
        0xbd, 0x10, 0x03, 0xb9, 0x10, 0x03, 0xa1, 0x10, 0xb1, 0x20, 0xb5, 0xf0,
    ]);
    ram.mem[0x315] = 0x11;
    ram.mem[0x317] = 0x22;
    ram.mem[0x14] = 0x00;
    ram.mem[0x15] = 0x04;
    ram.mem[0x400] = 0x33;
    ram.mem[0x20] = 0x00;
    ram.mem[0x21] = 0x04;
    ram.mem[0x404] = 0x44;
    ram.mem[0x05] = 0x55;
    cpu.x = 5;
    cpu.step(&mut ram);
    assert_eq!(cpu.a, 0x11);
    cpu.y = 7;
    cpu.step(&mut ram);
    assert_eq!(cpu.a, 0x22);
    cpu.x = 4;
    cpu.step(&mut ram);
    assert_eq!(cpu.a, 0x33);
    cpu.y = 4;
    cpu.step(&mut ram);
    assert_eq!(cpu.a, 0x44);
    cpu.x = 0x15;
    cpu.step(&mut ram); // $F0 + $15 = $105 -> $05
    assert_eq!(cpu.a, 0x55);
}

#[test]
fn a_read_modify_write_writes_the_old_value_then_the_new() {
    // `inc $d019` acknowledges the VIC with the first.
    let (mut cpu, mut ram) = machine(&[0xee, 0x19, 0xd0]);
    ram.mem[0xd019] = 0x81;
    cpu.step(&mut ram);
    assert_eq!(ram.writes, vec![(0xd019, 0x81), (0xd019, 0x82)]);
}

#[test]
fn jmp_indirect_takes_its_high_byte_from_the_same_page() {
    let (mut cpu, mut ram) = machine(&[0x6c, 0xff, 0x10]);
    ram.mem[0x10ff] = 0x34;
    ram.mem[0x1000] = 0x12;
    ram.mem[0x1100] = 0x99;
    cpu.step(&mut ram);
    assert_eq!(cpu.pc, 0x1234);
}

#[test]
fn interrupts() {
    // IRQ waits while I is set; taken, it pushes PC and P (B clear) and jumps through $FFFE; RTI returns.
    let (mut cpu, mut ram) = machine(&[0xea, 0xea]);
    ram.mem[0xfffe] = 0x00;
    ram.mem[0xffff] = 0x30;
    ram.mem[0x3000] = 0x40;
    cpu.p = U | I | C;
    assert!(!cpu.irq(&mut ram));
    cpu.p = U | C;
    assert!(cpu.irq(&mut ram));
    assert_eq!(cpu.pc, 0x3000);
    assert_eq!(cpu.p & I, I);
    assert_eq!(cpu.cycles, 7);
    assert_eq!(
        (ram.mem[0x1ff], ram.mem[0x1fe], ram.mem[0x1fd] & (B | C)),
        (0x02, 0x00, C)
    );
    cpu.step(&mut ram);
    assert_eq!(cpu.pc, 0x0200);
    assert_eq!(cpu.p & (I | C), C);
    // NMI is taken whatever I says.
    let (mut cpu, mut ram) = machine(&[0xea]);
    ram.mem[0xfffa] = 0x00;
    ram.mem[0xfffb] = 0x40;
    cpu.p = U | I;
    cpu.nmi(&mut ram);
    assert_eq!(cpu.pc, 0x4000);
    // BRK pushes PC + 2 with B set.
    let (mut cpu, mut ram) = machine(&[0x00, 0xff, 0xea]);
    ram.mem[0xfffe] = 0x00;
    ram.mem[0xffff] = 0x30;
    cpu.step(&mut ram);
    assert_eq!(cpu.pc, 0x3000);
    assert_eq!(
        (ram.mem[0x1ff], ram.mem[0x1fe], ram.mem[0x1fd] & B),
        (0x02, 0x02, B)
    );
    // Decimal mode survives (P is pushed and pulled whole).
    let (mut cpu, mut ram) = machine(&[0xf8, 0x08, 0xd8, 0x28]);
    steps(&mut cpu, &mut ram, 4);
    assert_eq!(cpu.p & D, D);
    // An interrupt frees a jammed CPU, as the hardware's reset line would not but its vector is read.
    let (mut cpu, mut ram) = machine(&[0x02]);
    cpu.step(&mut ram);
    assert!(cpu.jammed);
    ram.mem[0xfffa] = 0x00;
    ram.mem[0xfffb] = 0x50;
    cpu.nmi(&mut ram);
    assert!(!cpu.jammed);
}

#[test]
fn the_twelve_jam_opcodes_stop_the_cpu_where_it_is_but_time_passes() {
    for op in [
        0x02u8, 0x12, 0x22, 0x32, 0x42, 0x52, 0x62, 0x72, 0x92, 0xb2, 0xd2, 0xf2,
    ] {
        assert_eq!(mnemonic(op), "JAM");
        let (mut cpu, mut ram) = machine(&[0xea, op, 0xea]);
        steps(&mut cpu, &mut ram, 2);
        assert!(cpu.jammed);
        assert_eq!(cpu.pc, 0x201);
        let before = cpu.cycles;
        steps(&mut cpu, &mut ram, 3);
        assert_eq!(cpu.pc, 0x201);
        assert_eq!(cpu.cycles, before + 6);
    }
}

#[test]
fn every_undocumented_opcode_is_a_known_one_or_a_jam() {
    let mut jams = 0;
    for op in 0..=255u8 {
        if mnemonic(op) == "JAM" {
            jams += 1;
        }
    }
    assert_eq!(jams, 12, "only the twelve JAM opcodes jam");
    assert_eq!(
        [
            0xa7u8, 0xeb, 0xcb, 0x9f, 0xab, 0x8b, 0xbb, 0x9b, 0x9c, 0x9e, 0x0b, 0x2b, 0x4b, 0x6b,
            0x83, 0x97
        ]
        .map(mnemonic),
        [
            "LAX", "SBC", "SBX", "SHA", "LXA", "ANE", "LAS", "TAS", "SHY", "SHX", "ANC", "ANC",
            "ALR", "ARR", "SAX", "SAX"
        ]
    );
}

#[test]
fn the_nops_of_every_length_skip_their_operands() {
    // NOP ($1A), NOP #imm ($80), NOP zp ($04), NOP zp,X ($14), NOP abs ($0C), NOP abs,X ($1C)
    let (mut cpu, mut ram) = machine(&[
        0x1a, 0x80, 0xff, 0x04, 0xff, 0x14, 0xff, 0x0c, 0xff, 0xff, 0x1c, 0xff, 0xff,
    ]);
    let mut pcs = vec![];
    let mut cycles = vec![];
    for _ in 0..6 {
        cycles.push(cpu.step(&mut ram));
        pcs.push(cpu.pc);
    }
    assert_eq!(pcs, [0x201, 0x203, 0x205, 0x207, 0x20a, 0x20d]);
    assert_eq!(cycles, [2, 2, 3, 4, 4, 4]);
    // The other NOPs: $3A $5A $7A $DA $FA, $82 $C2 $E2 $89, $44 $64, $34.. , $3C..
    for (op, len) in [
        (0x3au8, 1u16),
        (0xfa, 1),
        (0x82, 2),
        (0x89, 2),
        (0xe2, 2),
        (0x44, 2),
        (0x64, 2),
        (0xd4, 2),
        (0xfc, 3),
        (0x5c, 3),
    ] {
        let (mut cpu, mut ram) = machine(&[op, 0, 0]);
        cpu.step(&mut ram);
        assert_eq!(cpu.pc, 0x200 + len, "{op:#x}");
    }
}

/// The illegal read-modify-write opcodes are the documented operation on memory followed by the ALU operation on the result.
#[test]
fn slo_rla_sre_rra_dcp_isc_equal_their_two_instruction_sequences() {
    // (illegal opcode zp, (first op zp, second op imm-form or zp ALU), name)
    // A second CPU runs the official pair on the same memory byte.
    let pairs: [(u8, u8, u8, &str); 6] = [
        (0x07, 0x06, 0x05, "SLO"), // ASL zp ; ORA zp
        (0x27, 0x26, 0x25, "RLA"), // ROL zp ; AND zp
        (0x47, 0x46, 0x45, "SRE"), // LSR zp ; EOR zp
        (0x67, 0x66, 0x65, "RRA"), // ROR zp ; ADC zp
        (0xc7, 0xc6, 0xc5, "DCP"), // DEC zp ; CMP zp
        (0xe7, 0xe6, 0xe5, "ISC"), // INC zp ; SBC zp
    ];
    for (illegal, first, second, name) in pairs {
        assert_eq!(mnemonic(illegal), name);
        for m in (0..=255u8).step_by(3) {
            for a in (0..=255u8).step_by(5) {
                for p in [U, U | C, U | D | C, U | N | V] {
                    let (mut x, mut xr) = machine(&[illegal, 0x10]);
                    xr.mem[0x10] = m;
                    x.a = a;
                    x.p = p;
                    let cycles = x.step(&mut xr);
                    assert_eq!(cycles, 5);
                    let (mut y, mut yr) = machine(&[first, 0x10, second, 0x10]);
                    yr.mem[0x10] = m;
                    y.a = a;
                    y.p = p;
                    y.step(&mut yr);
                    y.step(&mut yr);
                    assert_eq!(xr.mem[0x10], yr.mem[0x10], "{name} memory {m:#x}");
                    assert_eq!((x.a, x.p), (y.a, y.p), "{name} a={a:#x} m={m:#x} p={p:#x}");
                }
            }
        }
    }
}

#[test]
fn illegal_read_modify_write_cycles_by_mode() {
    // zp 5, zp,x 6, abs 6, abs,x 7, abs,y 7, (zp,x) 8, (zp),y 8
    for base in [0x03u8, 0x23, 0x43, 0x63, 0xc3, 0xe3] {
        for (off, want) in [
            (0x04u8, 5),
            (0x14, 6),
            (0x0c, 6),
            (0x1c, 7),
            (0x18, 7),
            (0x00, 8),
            (0x10, 8),
        ] {
            let (mut cpu, mut ram) = machine(&[base + off, 0x10, 0x03]);
            ram.mem[0x10] = 0x00;
            ram.mem[0x11] = 0x03;
            // abs forms read $0310; keep it clear of the code
            assert_eq!(
                cpu.step(&mut ram),
                want,
                "{} {:#x}",
                mnemonic(base + off),
                base + off
            );
        }
    }
}

#[test]
fn lax_sax_and_the_immediate_illegals() {
    // LAX loads A and X from every mode; SAX stores A & X without touching flags.
    let (mut cpu, mut ram) = machine(&[0xa7, 0x10, 0xa2, 0x0f, 0x87, 0x11]);
    ram.mem[0x10] = 0x3c;
    cpu.step(&mut ram);
    assert_eq!((cpu.a, cpu.x), (0x3c, 0x3c));
    steps(&mut cpu, &mut ram, 2);
    assert_eq!(ram.mem[0x11], 0x0c);
    // LAX (zp),Y; LAX abs,Y with a page cross costs a cycle.
    let (mut cpu, mut ram) = machine(&[0xbf, 0xf0, 0x03]);
    cpu.y = 0x20;
    ram.mem[0x410] = 0x81;
    assert_eq!(cpu.step(&mut ram), 5);
    assert_eq!((cpu.a, cpu.x, cpu.p & (N | Z)), (0x81, 0x81, N));
    // ANC: A &= imm; C = N.
    let (mut cpu, mut ram) = machine(&[0xa9, 0xf0, 0x0b, 0x81]);
    steps(&mut cpu, &mut ram, 2);
    assert_eq!(cpu.a, 0x80);
    assert_eq!(cpu.p & (N | C), N | C);
    // ALR: (A & imm) >> 1.
    let (mut cpu, mut ram) = machine(&[0xa9, 0xff, 0x4b, 0x03]);
    steps(&mut cpu, &mut ram, 2);
    assert_eq!((cpu.a, cpu.p & C), (0x01, C));
    // ARR with carry in: C = bit 6, V = bit 6 ^ bit 5.
    let (mut cpu, mut ram) = machine(&[0x38, 0xa9, 0xff, 0x6b, 0xc0]);
    steps(&mut cpu, &mut ram, 3);
    assert_eq!(cpu.a, 0xe0);
    assert_eq!(cpu.p & (C | V | N), C | N);
    // SBX: X = (A & X) - imm, carry as a compare.
    let (mut cpu, mut ram) = machine(&[0xa9, 0x3c, 0xa2, 0x0f, 0xcb, 0x02]);
    steps(&mut cpu, &mut ram, 3);
    assert_eq!((cpu.x, cpu.p & C), (0x0a, C));
    // LAS: A = X = SP = mem & SP.
    let (mut cpu, mut ram) = machine(&[0xbb, 0x10, 0x03]);
    ram.mem[0x310] = 0xf0;
    cpu.sp = 0x3c;
    cpu.step(&mut ram);
    assert_eq!((cpu.a, cpu.x, cpu.sp), (0x30, 0x30, 0x30));
    // LXA / ANE use the magic constant $EE.
    let (mut cpu, mut ram) = machine(&[0xab, 0xff]);
    cpu.a = 0x11;
    cpu.step(&mut ram);
    assert_eq!((cpu.a, cpu.x), (0xff, 0xff));
    let (mut cpu, mut ram) = machine(&[0x8b, 0xff]);
    cpu.a = 0x00;
    cpu.x = 0x7f;
    cpu.step(&mut ram);
    assert_eq!(cpu.a, 0x6e);
}

#[test]
fn sha_shx_shy_tas_store_the_value_anded_with_the_high_byte_plus_one() {
    // SHY $0300,X: Y & ($03 + 1)
    let (mut cpu, mut ram) = machine(&[0x9c, 0x00, 0x03]);
    cpu.x = 4;
    cpu.y = 0xff;
    cpu.step(&mut ram);
    assert_eq!(ram.mem[0x304], 0x04);
    // SHX $0300,Y
    let (mut cpu, mut ram) = machine(&[0x9e, 0x00, 0x03]);
    cpu.y = 4;
    cpu.x = 0xfe;
    cpu.step(&mut ram);
    assert_eq!(ram.mem[0x304], 0x04);
    // SHA $0300,Y and ($10),Y: A & X & (high + 1)
    let (mut cpu, mut ram) = machine(&[0x9f, 0x00, 0x03]);
    cpu.y = 1;
    cpu.a = 0xff;
    cpu.x = 0x07;
    cpu.step(&mut ram);
    assert_eq!(ram.mem[0x301], 0x04);
    // TAS: SP = A & X, stores SP & (high + 1)
    let (mut cpu, mut ram) = machine(&[0x9b, 0x00, 0x03]);
    cpu.y = 2;
    cpu.a = 0xf7;
    cpu.x = 0x0f;
    cpu.step(&mut ram);
    assert_eq!(cpu.sp, 0x07);
    assert_eq!(ram.mem[0x302], 0x04);
}

#[test]
fn adc_and_sbc_carry_chains_add_multi_byte_numbers() {
    // $01FF + $0001 = $0200, with CLC; LDA; ADC; STA; LDA; ADC; STA
    let (mut cpu, mut ram) = machine(&[
        0x18, 0xa5, 0x10, 0x65, 0x12, 0x85, 0x14, 0xa5, 0x11, 0x65, 0x13, 0x85, 0x15,
    ]);
    ram.mem[0x10] = 0xff;
    ram.mem[0x11] = 0x01;
    ram.mem[0x12] = 0x01;
    ram.mem[0x13] = 0x00;
    steps(&mut cpu, &mut ram, 7);
    assert_eq!((ram.mem[0x14], ram.mem[0x15]), (0x00, 0x02));
}

#[test]
fn the_default_cpu_is_as_after_reset() {
    let cpu = Cpu6510::new();
    assert_eq!(
        (cpu.sp, cpu.p, cpu.pc, cpu.cycles, cpu.jammed),
        (0xff, U | I, 0, 0, false)
    );
}
