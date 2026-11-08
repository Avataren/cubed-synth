//! The C64's CPU, a MOS 6510 (an NMOS 6502 with an I/O port), for running
//! `.sid` players. A port of another_synth's `psid/mos6510.ts`, which was tuned
//! against real tunes. It runs every opcode the NMOS part runs, because players
//! use the undocumented ones:
//!
//! - the documented set, with decimal-mode ADC/SBC as the NMOS part computes
//!   them (flags N, V, Z from the binary sum, as documented by VICE);
//! - the stable undocumented ones (SLO RLA SRE RRA SAX LAX DCP ISC ANC ALR ARR
//!   SBX, the NOPs of every length, SBC `$EB`);
//! - the unstable ones with their common behaviour (ANE and LXA with the magic
//!   constant `$EE`, SHA/SHX/SHY/TAS store `value & (high + 1)`, LAS);
//! - the twelve JAM opcodes stop the CPU ([`Cpu6510::jammed`]).
//!
//! Cycle counts are the documented NMOS ones, with the page-crossing and branch
//! penalties. A read-modify-write instruction writes the unmodified value first
//! and then the result, as the chip does: an IRQ handler's `inc $d019`
//! acknowledges the VIC through that first write.
//!
//! Time: the bus is told the cycle an instruction *started* on, for every
//! access it makes (the cycles are added when the instruction ends). A tune's
//! SID writes are stamped with it, as the DAW's are.

/// What the CPU reads and writes through. `cycle` is the cycle the running
/// instruction (or interrupt entry) started on.
pub trait CpuBus {
    fn read(&mut self, addr: u16, cycle: u64) -> u8;
    fn write(&mut self, addr: u16, value: u8, cycle: u64);
}

pub const C: u8 = 0x01;
pub const Z: u8 = 0x02;
pub const I: u8 = 0x04;
pub const D: u8 = 0x08;
pub const B: u8 = 0x10;
pub const U: u8 = 0x20;
pub const V: u8 = 0x40;
pub const N: u8 = 0x80;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Mode {
    Imp,
    Imm,
    Zp,
    Zpx,
    Zpy,
    Abs,
    Abx,
    Aby,
    Ind,
    Izx,
    Izy,
    Rel,
    Acc,
}

macro_rules! ops {
    ($($name:ident),* $(,)?) => {
        #[allow(clippy::upper_case_acronyms)]
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(u8)]
        enum Op { $($name),* }
        const OP_NAMES: &[&str] = &[$(stringify!($name)),*];
    };
}

ops!(
    ADC, AND, ASL, BCC, BCS, BEQ, BIT, BMI, BNE, BPL, BRK, BVC, BVS, CLC, CLD, CLI, CLV, CMP, CPX,
    CPY, DEC, DEX, DEY, EOR, INC, INX, INY, JMP, JSR, LDA, LDX, LDY, LSR, NOP, ORA, PHA, PHP, PLA,
    PLP, ROL, ROR, RTI, RTS, SBC, SEC, SED, SEI, STA, STX, STY, TAX, TAY, TSX, TXA, TXS, TYA, SLO,
    RLA, SRE, RRA, SAX, LAX, DCP, ISC, ANC, ALR, ARR, SBX, LAS, SHA, SHX, SHY, TAS, ANE, LXA, JAM,
);

/// Per opcode: the operation, its addressing mode, base cycles, and whether a
/// page crossing costs a cycle (reads only).
#[derive(Clone, Copy)]
struct Entry {
    op: Op,
    mode: Mode,
    cycles: u8,
    page: bool,
}

const JAM_ENTRY: Entry = Entry {
    op: Op::JAM,
    mode: Mode::Imp,
    cycles: 2,
    page: false,
};

type Table = [Entry; 256];

const fn op(t: &mut Table, code: usize, name: Op, mode: Mode, cycles: u8, page: bool) {
    t[code] = Entry {
        op: name,
        mode,
        cycles,
        page,
    };
}

/// The eight-mode group of ORA/AND/EOR/ADC/LDA/CMP/SBC at `base` (the (zp,X) opcode).
const fn alu_group(t: &mut Table, base: usize, name: Op) {
    op(t, base, name, Mode::Izx, 6, false);
    op(t, base + 0x04, name, Mode::Zp, 3, false);
    op(t, base + 0x08, name, Mode::Imm, 2, false);
    op(t, base + 0x0c, name, Mode::Abs, 4, false);
    op(t, base + 0x10, name, Mode::Izy, 5, true);
    op(t, base + 0x14, name, Mode::Zpx, 4, false);
    op(t, base + 0x18, name, Mode::Aby, 4, true);
    op(t, base + 0x1c, name, Mode::Abx, 4, true);
}

/// The shift/INC/DEC group at `zp` (the zero-page opcode).
const fn rmw_group(t: &mut Table, zp: usize, name: Op, acc: bool) {
    op(t, zp, name, Mode::Zp, 5, false);
    if acc {
        op(t, zp + 0x04, name, Mode::Acc, 2, false);
    }
    op(t, zp + 0x08, name, Mode::Abs, 6, false);
    op(t, zp + 0x10, name, Mode::Zpx, 6, false);
    op(t, zp + 0x18, name, Mode::Abx, 7, false);
}

/// The undocumented read-modify-write + ALU group (SLO..ISC) at `base` (the (zp,X) opcode).
const fn illegal_rmw_group(t: &mut Table, base: usize, name: Op) {
    op(t, base, name, Mode::Izx, 8, false);
    op(t, base + 0x04, name, Mode::Zp, 5, false);
    op(t, base + 0x0c, name, Mode::Abs, 6, false);
    op(t, base + 0x10, name, Mode::Izy, 8, false);
    op(t, base + 0x14, name, Mode::Zpx, 6, false);
    op(t, base + 0x18, name, Mode::Aby, 7, false);
    op(t, base + 0x1c, name, Mode::Abx, 7, false);
}

const fn build_table() -> Table {
    let mut t = [JAM_ENTRY; 256];
    let t = &mut t;
    alu_group(t, 0x01, Op::ORA);
    alu_group(t, 0x21, Op::AND);
    alu_group(t, 0x41, Op::EOR);
    alu_group(t, 0x61, Op::ADC);
    alu_group(t, 0xa1, Op::LDA);
    alu_group(t, 0xc1, Op::CMP);
    alu_group(t, 0xe1, Op::SBC);
    // STA: no immediate, no page penalty.
    op(t, 0x81, Op::STA, Mode::Izx, 6, false);
    op(t, 0x85, Op::STA, Mode::Zp, 3, false);
    op(t, 0x8d, Op::STA, Mode::Abs, 4, false);
    op(t, 0x91, Op::STA, Mode::Izy, 6, false);
    op(t, 0x95, Op::STA, Mode::Zpx, 4, false);
    op(t, 0x99, Op::STA, Mode::Aby, 5, false);
    op(t, 0x9d, Op::STA, Mode::Abx, 5, false);
    op(t, 0x89, Op::NOP, Mode::Imm, 2, false);

    rmw_group(t, 0x06, Op::ASL, true);
    rmw_group(t, 0x26, Op::ROL, true);
    rmw_group(t, 0x46, Op::LSR, true);
    rmw_group(t, 0x66, Op::ROR, true);
    rmw_group(t, 0xc6, Op::DEC, false);
    rmw_group(t, 0xe6, Op::INC, false);

    illegal_rmw_group(t, 0x03, Op::SLO);
    illegal_rmw_group(t, 0x23, Op::RLA);
    illegal_rmw_group(t, 0x43, Op::SRE);
    illegal_rmw_group(t, 0x63, Op::RRA);
    illegal_rmw_group(t, 0xc3, Op::DCP);
    illegal_rmw_group(t, 0xe3, Op::ISC);

    op(t, 0x83, Op::SAX, Mode::Izx, 6, false);
    op(t, 0x87, Op::SAX, Mode::Zp, 3, false);
    op(t, 0x8f, Op::SAX, Mode::Abs, 4, false);
    op(t, 0x97, Op::SAX, Mode::Zpy, 4, false);
    op(t, 0xa3, Op::LAX, Mode::Izx, 6, false);
    op(t, 0xa7, Op::LAX, Mode::Zp, 3, false);
    op(t, 0xab, Op::LXA, Mode::Imm, 2, false);
    op(t, 0xaf, Op::LAX, Mode::Abs, 4, false);
    op(t, 0xb3, Op::LAX, Mode::Izy, 5, true);
    op(t, 0xb7, Op::LAX, Mode::Zpy, 4, false);
    op(t, 0xbf, Op::LAX, Mode::Aby, 4, true);
    op(t, 0x0b, Op::ANC, Mode::Imm, 2, false);
    op(t, 0x2b, Op::ANC, Mode::Imm, 2, false);
    op(t, 0x4b, Op::ALR, Mode::Imm, 2, false);
    op(t, 0x6b, Op::ARR, Mode::Imm, 2, false);
    op(t, 0x8b, Op::ANE, Mode::Imm, 2, false);
    op(t, 0xcb, Op::SBX, Mode::Imm, 2, false);
    op(t, 0xeb, Op::SBC, Mode::Imm, 2, false);
    op(t, 0x93, Op::SHA, Mode::Izy, 6, false);
    op(t, 0x9f, Op::SHA, Mode::Aby, 5, false);
    op(t, 0x9b, Op::TAS, Mode::Aby, 5, false);
    op(t, 0x9c, Op::SHY, Mode::Abx, 5, false);
    op(t, 0x9e, Op::SHX, Mode::Aby, 5, false);
    op(t, 0xbb, Op::LAS, Mode::Aby, 4, true);

    op(t, 0xa2, Op::LDX, Mode::Imm, 2, false);
    op(t, 0xa6, Op::LDX, Mode::Zp, 3, false);
    op(t, 0xae, Op::LDX, Mode::Abs, 4, false);
    op(t, 0xb6, Op::LDX, Mode::Zpy, 4, false);
    op(t, 0xbe, Op::LDX, Mode::Aby, 4, true);
    op(t, 0xa0, Op::LDY, Mode::Imm, 2, false);
    op(t, 0xa4, Op::LDY, Mode::Zp, 3, false);
    op(t, 0xac, Op::LDY, Mode::Abs, 4, false);
    op(t, 0xb4, Op::LDY, Mode::Zpx, 4, false);
    op(t, 0xbc, Op::LDY, Mode::Abx, 4, true);
    op(t, 0x86, Op::STX, Mode::Zp, 3, false);
    op(t, 0x8e, Op::STX, Mode::Abs, 4, false);
    op(t, 0x96, Op::STX, Mode::Zpy, 4, false);
    op(t, 0x84, Op::STY, Mode::Zp, 3, false);
    op(t, 0x8c, Op::STY, Mode::Abs, 4, false);
    op(t, 0x94, Op::STY, Mode::Zpx, 4, false);
    op(t, 0xe0, Op::CPX, Mode::Imm, 2, false);
    op(t, 0xe4, Op::CPX, Mode::Zp, 3, false);
    op(t, 0xec, Op::CPX, Mode::Abs, 4, false);
    op(t, 0xc0, Op::CPY, Mode::Imm, 2, false);
    op(t, 0xc4, Op::CPY, Mode::Zp, 3, false);
    op(t, 0xcc, Op::CPY, Mode::Abs, 4, false);
    op(t, 0x24, Op::BIT, Mode::Zp, 3, false);
    op(t, 0x2c, Op::BIT, Mode::Abs, 4, false);

    op(t, 0x10, Op::BPL, Mode::Rel, 2, false);
    op(t, 0x30, Op::BMI, Mode::Rel, 2, false);
    op(t, 0x50, Op::BVC, Mode::Rel, 2, false);
    op(t, 0x70, Op::BVS, Mode::Rel, 2, false);
    op(t, 0x90, Op::BCC, Mode::Rel, 2, false);
    op(t, 0xb0, Op::BCS, Mode::Rel, 2, false);
    op(t, 0xd0, Op::BNE, Mode::Rel, 2, false);
    op(t, 0xf0, Op::BEQ, Mode::Rel, 2, false);

    op(t, 0x00, Op::BRK, Mode::Imp, 7, false);
    op(t, 0x20, Op::JSR, Mode::Abs, 6, false);
    op(t, 0x40, Op::RTI, Mode::Imp, 6, false);
    op(t, 0x60, Op::RTS, Mode::Imp, 6, false);
    op(t, 0x4c, Op::JMP, Mode::Abs, 3, false);
    op(t, 0x6c, Op::JMP, Mode::Ind, 5, false);
    op(t, 0x08, Op::PHP, Mode::Imp, 3, false);
    op(t, 0x28, Op::PLP, Mode::Imp, 4, false);
    op(t, 0x48, Op::PHA, Mode::Imp, 3, false);
    op(t, 0x68, Op::PLA, Mode::Imp, 4, false);
    op(t, 0x18, Op::CLC, Mode::Imp, 2, false);
    op(t, 0x38, Op::SEC, Mode::Imp, 2, false);
    op(t, 0x58, Op::CLI, Mode::Imp, 2, false);
    op(t, 0x78, Op::SEI, Mode::Imp, 2, false);
    op(t, 0xb8, Op::CLV, Mode::Imp, 2, false);
    op(t, 0xd8, Op::CLD, Mode::Imp, 2, false);
    op(t, 0xf8, Op::SED, Mode::Imp, 2, false);
    op(t, 0x88, Op::DEY, Mode::Imp, 2, false);
    op(t, 0xc8, Op::INY, Mode::Imp, 2, false);
    op(t, 0xca, Op::DEX, Mode::Imp, 2, false);
    op(t, 0xe8, Op::INX, Mode::Imp, 2, false);
    op(t, 0x8a, Op::TXA, Mode::Imp, 2, false);
    op(t, 0x98, Op::TYA, Mode::Imp, 2, false);
    op(t, 0x9a, Op::TXS, Mode::Imp, 2, false);
    op(t, 0xa8, Op::TAY, Mode::Imp, 2, false);
    op(t, 0xaa, Op::TAX, Mode::Imp, 2, false);
    op(t, 0xba, Op::TSX, Mode::Imp, 2, false);
    op(t, 0xea, Op::NOP, Mode::Imp, 2, false);
    // The NOPs of every length.
    let mut i = 0;
    while i < 6 {
        op(
            t,
            [0x1a, 0x3a, 0x5a, 0x7a, 0xda, 0xfa][i],
            Op::NOP,
            Mode::Imp,
            2,
            false,
        );
        op(
            t,
            [0x1c, 0x3c, 0x5c, 0x7c, 0xdc, 0xfc][i],
            Op::NOP,
            Mode::Abx,
            4,
            true,
        );
        op(
            t,
            [0x14, 0x34, 0x54, 0x74, 0xd4, 0xf4][i],
            Op::NOP,
            Mode::Zpx,
            4,
            false,
        );
        i += 1;
    }
    let mut i = 0;
    while i < 4 {
        op(t, [0x80, 0x82, 0xc2, 0xe2][i], Op::NOP, Mode::Imm, 2, false);
        i += 1;
    }
    let mut i = 0;
    while i < 3 {
        op(t, [0x04, 0x44, 0x64][i], Op::NOP, Mode::Zp, 3, false);
        i += 1;
    }
    op(t, 0x0c, Op::NOP, Mode::Abs, 4, false);
    // The twelve JAMs are the table's default.
    *t
}

static TABLE: Table = build_table();

/// The mnemonic of `opcode` (upper case), for messages and tests.
pub fn mnemonic(opcode: u8) -> &'static str {
    OP_NAMES[TABLE[opcode as usize].op as usize]
}

/// The bytes an instruction occupies, by its opcode (an operand-less one is 1).
pub fn instruction_len(opcode: u8) -> u8 {
    match TABLE[opcode as usize].mode {
        Mode::Imp | Mode::Acc => 1,
        Mode::Imm | Mode::Zp | Mode::Zpx | Mode::Zpy | Mode::Izx | Mode::Izy | Mode::Rel => 2,
        Mode::Abs | Mode::Abx | Mode::Aby | Mode::Ind => 3,
    }
}

#[derive(Clone, Debug)]
pub struct Cpu6510 {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub p: u8,
    pub pc: u16,
    /// Clock cycles run so far.
    pub cycles: u64,
    /// A JAM opcode stopped the CPU at `pc`.
    pub jammed: bool,
}

impl Default for Cpu6510 {
    fn default() -> Self {
        Cpu6510 {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xff,
            p: U | I,
            pc: 0,
            cycles: 0,
            jammed: false,
        }
    }
}

impl Cpu6510 {
    pub fn new() -> Cpu6510 {
        Cpu6510::default()
    }

    #[inline]
    fn push<Bus: CpuBus>(&mut self, bus: &mut Bus, now: u64, v: u8) {
        bus.write(0x100 | self.sp as u16, v, now);
        self.sp = self.sp.wrapping_sub(1);
    }

    #[inline]
    fn pull<Bus: CpuBus>(&mut self, bus: &mut Bus, now: u64) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(0x100 | self.sp as u16, now)
    }

    #[inline]
    fn set_nz(&mut self, v: u8) -> u8 {
        self.p = (self.p & !(N | Z)) | (v & N) | if v == 0 { Z } else { 0 };
        v
    }

    fn adc(&mut self, v: u8) {
        let a = self.a as i32;
        let v = v as i32;
        let c = (self.p & C) as i32;
        if self.p & D != 0 {
            let mut lo = (a & 0x0f) + (v & 0x0f) + c;
            if lo > 0x09 {
                lo += 0x06;
            }
            let mut t = if lo <= 0x0f {
                (lo & 0x0f) + (a & 0xf0) + (v & 0xf0)
            } else {
                (lo & 0x0f) + (a & 0xf0) + (v & 0xf0) + 0x10
            };
            let zero = ((a + v + c) & 0xff) == 0;
            let neg = (t & 0x80) != 0;
            let ovf = ((a ^ t) & 0x80) != 0 && ((a ^ v) & 0x80) == 0;
            if (t & 0x1f0) > 0x90 {
                t += 0x60;
            }
            let carry = (t & 0xff0) > 0xf0;
            self.p = (self.p & !(N | V | Z | C))
                | if neg { N } else { 0 }
                | if ovf { V } else { 0 }
                | if zero { Z } else { 0 }
                | if carry { C } else { 0 };
            self.a = (t & 0xff) as u8;
            return;
        }
        let r = a + v + c;
        self.p = (self.p & !(V | C))
            | if (!(a ^ v) & (a ^ r) & 0x80) != 0 {
                V
            } else {
                0
            }
            | if r > 0xff { C } else { 0 };
        self.a = self.set_nz((r & 0xff) as u8);
    }

    fn sbc(&mut self, v: u8) {
        let a = self.a as i32;
        let v = v as i32;
        let borrow = ((self.p & C) ^ 1) as i32;
        let r = a - v - borrow;
        // Flags are the binary subtraction's, in both modes (NMOS).
        self.p = (self.p & !(V | C))
            | if ((a ^ v) & (a ^ r) & 0x80) != 0 {
                V
            } else {
                0
            }
            | if r >= 0 { C } else { 0 };
        self.set_nz((r & 0xff) as u8);
        if self.p & D != 0 {
            let lo = (a & 0x0f) - (v & 0x0f) - borrow;
            let mut t = if lo & 0x10 != 0 {
                ((lo - 6) & 0x0f) | ((a & 0xf0) - (v & 0xf0) - 0x10)
            } else {
                (lo & 0x0f) | ((a & 0xf0) - (v & 0xf0))
            };
            if t & 0x100 != 0 {
                t -= 0x60;
            }
            self.a = (t & 0xff) as u8;
            return;
        }
        self.a = (r & 0xff) as u8;
    }

    fn compare(&mut self, reg: u8, v: u8) {
        let r = reg as i32 - v as i32;
        self.p = (self.p & !C) | if r >= 0 { C } else { 0 };
        self.set_nz((r & 0xff) as u8);
    }

    fn asl(&mut self, v: u8) -> u8 {
        self.p = (self.p & !C) | (v >> 7);
        self.set_nz(v << 1)
    }

    fn lsr(&mut self, v: u8) -> u8 {
        self.p = (self.p & !C) | (v & 1);
        self.set_nz(v >> 1)
    }

    fn rol(&mut self, v: u8) -> u8 {
        let r = (v << 1) | (self.p & C);
        self.p = (self.p & !C) | (v >> 7);
        self.set_nz(r)
    }

    fn ror(&mut self, v: u8) -> u8 {
        let r = (v >> 1) | ((self.p & C) << 7);
        self.p = (self.p & !C) | (v & 1);
        self.set_nz(r)
    }

    /// Take the IRQ line: false (nothing done) while I is set.
    pub fn irq<Bus: CpuBus>(&mut self, bus: &mut Bus) -> bool {
        if self.p & I != 0 {
            return false;
        }
        self.interrupt(bus, 0xfffe);
        true
    }

    /// Take an NMI (edge): always.
    pub fn nmi<Bus: CpuBus>(&mut self, bus: &mut Bus) {
        self.interrupt(bus, 0xfffa);
    }

    fn interrupt<Bus: CpuBus>(&mut self, bus: &mut Bus, vector: u16) {
        let now = self.cycles;
        self.jammed = false;
        self.push(bus, now, (self.pc >> 8) as u8);
        self.push(bus, now, (self.pc & 0xff) as u8);
        self.push(bus, now, (self.p & !B) | U);
        self.p |= I;
        self.pc = bus.read(vector, now) as u16 | ((bus.read(vector + 1, now) as u16) << 8);
        self.cycles += 7;
    }

    /// A branch to `target` when `taken`: its extra cycles (1, or 2 across a page).
    #[inline]
    fn branch(&mut self, taken: bool, target: u16) -> u32 {
        if !taken {
            return 0;
        }
        let extra = if target & 0xff00 == self.pc & 0xff00 {
            1
        } else {
            2
        };
        self.pc = target;
        extra
    }

    fn shift(&mut self, op: Op, v: u8) -> u8 {
        match op {
            Op::ASL => self.asl(v),
            Op::LSR => self.lsr(v),
            Op::ROL => self.rol(v),
            Op::ROR => self.ror(v),
            Op::INC => self.set_nz(v.wrapping_add(1)),
            _ => self.set_nz(v.wrapping_sub(1)),
        }
    }

    /// Run one instruction; its cycles. A jammed CPU stays put (2 cycles a
    /// call, so time still passes).
    pub fn step<Bus: CpuBus>(&mut self, bus: &mut Bus) -> u32 {
        if self.jammed {
            self.cycles += 2;
            return 2;
        }
        let now = self.cycles;
        let opcode = bus.read(self.pc, now);
        let e = TABLE[opcode as usize];
        let mut pc = self.pc.wrapping_add(1);
        let mut cycles = e.cycles as u32;
        let mut addr: u16 = 0;
        // For SHA/SHX/SHY/TAS: the high byte of the base address, plus one.
        let mut high1: u8 = 0;
        match e.mode {
            Mode::Imm => {
                addr = pc;
                pc = pc.wrapping_add(1);
            }
            Mode::Zp => {
                addr = bus.read(pc, now) as u16;
                pc = pc.wrapping_add(1);
            }
            Mode::Zpx => {
                addr = bus.read(pc, now).wrapping_add(self.x) as u16;
                pc = pc.wrapping_add(1);
            }
            Mode::Zpy => {
                addr = bus.read(pc, now).wrapping_add(self.y) as u16;
                pc = pc.wrapping_add(1);
            }
            Mode::Abs => {
                addr = bus.read(pc, now) as u16 | ((bus.read(pc.wrapping_add(1), now) as u16) << 8);
                pc = pc.wrapping_add(2);
            }
            Mode::Abx | Mode::Aby => {
                let base =
                    bus.read(pc, now) as u16 | ((bus.read(pc.wrapping_add(1), now) as u16) << 8);
                let index = if e.mode == Mode::Abx { self.x } else { self.y };
                addr = base.wrapping_add(index as u16);
                if e.page && (base ^ addr) & 0xff00 != 0 {
                    cycles += 1;
                }
                high1 = ((base >> 8) as u8).wrapping_add(1);
                pc = pc.wrapping_add(2);
            }
            Mode::Ind => {
                let ptr =
                    bus.read(pc, now) as u16 | ((bus.read(pc.wrapping_add(1), now) as u16) << 8);
                // The NMOS page-wrap: the high byte comes from the same page.
                addr = bus.read(ptr, now) as u16
                    | ((bus.read((ptr & 0xff00) | (ptr.wrapping_add(1) & 0xff), now) as u16) << 8);
                pc = pc.wrapping_add(2);
            }
            Mode::Izx => {
                let zp = bus.read(pc, now).wrapping_add(self.x);
                addr = bus.read(zp as u16, now) as u16
                    | ((bus.read(zp.wrapping_add(1) as u16, now) as u16) << 8);
                pc = pc.wrapping_add(1);
            }
            Mode::Izy => {
                let zp = bus.read(pc, now);
                let base = bus.read(zp as u16, now) as u16
                    | ((bus.read(zp.wrapping_add(1) as u16, now) as u16) << 8);
                addr = base.wrapping_add(self.y as u16);
                if e.page && (base ^ addr) & 0xff00 != 0 {
                    cycles += 1;
                }
                high1 = ((base >> 8) as u8).wrapping_add(1);
                pc = pc.wrapping_add(1);
            }
            Mode::Rel => {
                let d = bus.read(pc, now);
                pc = pc.wrapping_add(1);
                addr = pc.wrapping_add(d as i8 as u16);
            }
            Mode::Imp | Mode::Acc => {}
        }
        self.pc = pc;

        match e.op {
            Op::LDA => {
                let v = bus.read(addr, now);
                self.a = self.set_nz(v);
            }
            Op::LDX => {
                let v = bus.read(addr, now);
                self.x = self.set_nz(v);
            }
            Op::LDY => {
                let v = bus.read(addr, now);
                self.y = self.set_nz(v);
            }
            Op::STA => bus.write(addr, self.a, now),
            Op::STX => bus.write(addr, self.x, now),
            Op::STY => bus.write(addr, self.y, now),
            Op::ADC => {
                let v = bus.read(addr, now);
                self.adc(v);
            }
            Op::SBC => {
                let v = bus.read(addr, now);
                self.sbc(v);
            }
            Op::AND => {
                let v = self.a & bus.read(addr, now);
                self.a = self.set_nz(v);
            }
            Op::ORA => {
                let v = self.a | bus.read(addr, now);
                self.a = self.set_nz(v);
            }
            Op::EOR => {
                let v = self.a ^ bus.read(addr, now);
                self.a = self.set_nz(v);
            }
            Op::CMP => {
                let v = bus.read(addr, now);
                self.compare(self.a, v);
            }
            Op::CPX => {
                let v = bus.read(addr, now);
                self.compare(self.x, v);
            }
            Op::CPY => {
                let v = bus.read(addr, now);
                self.compare(self.y, v);
            }
            Op::BIT => {
                let v = bus.read(addr, now);
                self.p =
                    (self.p & !(N | V | Z)) | (v & (N | V)) | if self.a & v == 0 { Z } else { 0 };
            }
            Op::ASL | Op::LSR | Op::ROL | Op::ROR | Op::INC | Op::DEC => {
                if e.mode == Mode::Acc {
                    self.a = self.shift(e.op, self.a);
                } else {
                    let v = bus.read(addr, now);
                    bus.write(addr, v, now);
                    let r = self.shift(e.op, v);
                    bus.write(addr, r, now);
                }
            }
            Op::SLO => {
                let v = bus.read(addr, now);
                bus.write(addr, v, now);
                let r = self.asl(v);
                bus.write(addr, r, now);
                self.a = self.set_nz(self.a | r);
            }
            Op::RLA => {
                let v = bus.read(addr, now);
                bus.write(addr, v, now);
                let r = self.rol(v);
                bus.write(addr, r, now);
                self.a = self.set_nz(self.a & r);
            }
            Op::SRE => {
                let v = bus.read(addr, now);
                bus.write(addr, v, now);
                let r = self.lsr(v);
                bus.write(addr, r, now);
                self.a = self.set_nz(self.a ^ r);
            }
            Op::RRA => {
                let v = bus.read(addr, now);
                bus.write(addr, v, now);
                let r = self.ror(v);
                bus.write(addr, r, now);
                self.adc(r);
            }
            Op::DCP => {
                let v = bus.read(addr, now);
                bus.write(addr, v, now);
                let r = v.wrapping_sub(1);
                bus.write(addr, r, now);
                self.compare(self.a, r);
            }
            Op::ISC => {
                let v = bus.read(addr, now);
                bus.write(addr, v, now);
                let r = v.wrapping_add(1);
                bus.write(addr, r, now);
                self.sbc(r);
            }
            Op::SAX => bus.write(addr, self.a & self.x, now),
            Op::LAX => {
                let v = bus.read(addr, now);
                self.a = self.set_nz(v);
                self.x = self.a;
            }
            Op::LXA => {
                let v = (self.a | 0xee) & bus.read(addr, now);
                self.a = self.set_nz(v);
                self.x = self.a;
            }
            Op::ANE => {
                let v = (self.a | 0xee) & self.x & bus.read(addr, now);
                self.a = self.set_nz(v);
            }
            Op::ANC => {
                let v = self.a & bus.read(addr, now);
                self.a = self.set_nz(v);
                self.p = (self.p & !C) | (self.a >> 7);
            }
            Op::ALR => {
                let v = self.a & bus.read(addr, now);
                self.a = self.lsr(v);
            }
            Op::ARR => {
                let t = self.a & bus.read(addr, now);
                let r = (t >> 1) | ((self.p & C) << 7);
                self.set_nz(r);
                self.p = (self.p & !(C | V))
                    | ((r >> 6) & 1)
                    | if ((r >> 6) ^ (r >> 5)) & 1 != 0 { V } else { 0 };
                self.a = r;
            }
            Op::SBX => {
                let t = (self.a & self.x) as i32 - bus.read(addr, now) as i32;
                self.p = (self.p & !C) | if t >= 0 { C } else { 0 };
                self.x = self.set_nz((t & 0xff) as u8);
            }
            Op::LAS => {
                let v = bus.read(addr, now) & self.sp;
                self.set_nz(v);
                self.a = v;
                self.x = v;
                self.sp = v;
            }
            Op::SHA => bus.write(addr, self.a & self.x & high1, now),
            Op::SHX => bus.write(addr, self.x & high1, now),
            Op::SHY => bus.write(addr, self.y & high1, now),
            Op::TAS => {
                self.sp = self.a & self.x;
                bus.write(addr, self.sp & high1, now);
            }
            Op::NOP => {
                if e.mode != Mode::Imp && e.mode != Mode::Imm {
                    bus.read(addr, now);
                }
            }
            Op::INX => self.x = self.set_nz(self.x.wrapping_add(1)),
            Op::INY => self.y = self.set_nz(self.y.wrapping_add(1)),
            Op::DEX => self.x = self.set_nz(self.x.wrapping_sub(1)),
            Op::DEY => self.y = self.set_nz(self.y.wrapping_sub(1)),
            Op::TAX => self.x = self.set_nz(self.a),
            Op::TAY => self.y = self.set_nz(self.a),
            Op::TXA => self.a = self.set_nz(self.x),
            Op::TYA => self.a = self.set_nz(self.y),
            Op::TSX => self.x = self.set_nz(self.sp),
            Op::TXS => self.sp = self.x,
            Op::PHA => self.push(bus, now, self.a),
            Op::PHP => self.push(bus, now, self.p | B | U),
            Op::PLA => {
                let v = self.pull(bus, now);
                self.a = self.set_nz(v);
            }
            Op::PLP => self.p = (self.pull(bus, now) & !B) | U,
            Op::CLC => self.p &= !C,
            Op::SEC => self.p |= C,
            Op::CLI => self.p &= !I,
            Op::SEI => self.p |= I,
            Op::CLV => self.p &= !V,
            Op::CLD => self.p &= !D,
            Op::SED => self.p |= D,
            Op::BPL => cycles += self.branch(self.p & N == 0, addr),
            Op::BMI => cycles += self.branch(self.p & N != 0, addr),
            Op::BVC => cycles += self.branch(self.p & V == 0, addr),
            Op::BVS => cycles += self.branch(self.p & V != 0, addr),
            Op::BCC => cycles += self.branch(self.p & C == 0, addr),
            Op::BCS => cycles += self.branch(self.p & C != 0, addr),
            Op::BNE => cycles += self.branch(self.p & Z == 0, addr),
            Op::BEQ => cycles += self.branch(self.p & Z != 0, addr),
            Op::JMP => self.pc = addr,
            Op::JSR => {
                let ret = self.pc.wrapping_sub(1);
                self.push(bus, now, (ret >> 8) as u8);
                self.push(bus, now, (ret & 0xff) as u8);
                self.pc = addr;
            }
            Op::RTS => {
                let lo = self.pull(bus, now) as u16;
                let hi = self.pull(bus, now) as u16;
                self.pc = ((hi << 8) | lo).wrapping_add(1);
            }
            Op::RTI => {
                self.p = (self.pull(bus, now) & !B) | U;
                let lo = self.pull(bus, now) as u16;
                let hi = self.pull(bus, now) as u16;
                self.pc = (hi << 8) | lo;
            }
            Op::BRK => {
                let ret = self.pc.wrapping_add(1);
                self.push(bus, now, (ret >> 8) as u8);
                self.push(bus, now, (ret & 0xff) as u8);
                self.push(bus, now, self.p | B | U);
                self.p |= I;
                self.pc = bus.read(0xfffe, now) as u16 | ((bus.read(0xffff, now) as u16) << 8);
            }
            Op::JAM => {
                self.jammed = true;
                self.pc = self.pc.wrapping_sub(1);
            }
        }
        self.cycles += cycles as u64;
        cycles
    }
}
