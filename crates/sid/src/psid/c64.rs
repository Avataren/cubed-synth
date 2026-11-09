//! Enough of a C64 to run `.sid` players. A port of another_synth's
//! `psid/c64.ts`: 64 KB of RAM, the 6510's `$01` banking, and the chips a
//! player talks to:
//!
//! - **SID** (`$D400`, and a second/third one where the file says): every write
//!   is queued ([`SidWrite`]) with the cycle it was made on; the registers read
//!   back the last value on the chip's bus, except voice 3's oscillator
//!   (`$D41B`) and envelope (`$D41C`), which are modelled (players use them as
//!   random numbers);
//! - **CIA 1 / CIA 2** timers A and B (continuous or one-shot, force load, timer
//!   B counting timer A), their interrupt control register (CIA 1 on the IRQ
//!   line, CIA 2 on the NMI line); ports read as "nothing pressed";
//! - **VIC-II** raster counter (`$D011`/`$D012`) and raster interrupt
//!   (`$D019`/`$D01A`), PAL (312 lines x 63 cycles) or NTSC (263 x 65).
//!
//! No ROM images (not ours to ship): a stand-in KERNAL of our own is visible
//! where `$01` banks the KERNAL in. It has the KERNAL's IRQ/BRK entry at `$FF48`
//! (`JMP ($0314)` / `JMP ($0316)`), its NMI entry at `$FE43` (`JMP ($0318)`),
//! the IRQ exits `$EA31`/`$EA7E`/`$EA81` and the NMI exit `$FEBC`, the hardware
//! vectors, and `RTS` everywhere else, so a call into a KERNAL routine returns.
//! The BASIC ROM's range reads `RTS` too. The RAM vectors and `$02A6` (the PAL
//! flag) are set as the KERNAL leaves them.
//!
//! The DAW's Zobrist state hash (it finds the loop of a tune to end a capture)
//! is optional here: a player that runs for ever never asks for it, a capture
//! (`import::capture`) turns it on with [`C64::enable_state_hash`], and the
//! machine then keeps a 64-bit hash of its RAM and of the I/O registers
//! written ([`C64::state_hash`]), the same one the DAW's `stateHash()` has.

use super::cpu::{Cpu6510, CpuBus, I};

/// Which video standard (and so which CPU clock) the machine has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum C64Clock {
    Pal,
    Ntsc,
}

#[derive(Clone, Copy, Debug)]
pub struct C64Timing {
    /// CPU cycles per second.
    pub hz: u32,
    pub cycles_per_line: u32,
    pub lines: u32,
    /// The CIA 1 timer A latch the KERNAL sets at boot (its 60 Hz interrupt).
    pub kernal_cia_latch: u16,
}

impl C64Timing {
    /// Cycles of one video frame.
    pub const fn frame_cycles(&self) -> u64 {
        self.cycles_per_line as u64 * self.lines as u64
    }
}

impl C64Clock {
    pub const fn timing(self) -> C64Timing {
        match self {
            C64Clock::Pal => C64Timing {
                hz: 985_248,
                cycles_per_line: 63,
                lines: 312,
                kernal_cia_latch: 0x4025,
            },
            C64Clock::Ntsc => C64Timing {
                hz: 1_022_727,
                cycles_per_line: 65,
                lines: 263,
                kernal_cia_latch: 0x4295,
            },
        }
    }
}

/// Addresses in the stand-in KERNAL (the real KERNAL's).
pub const KERNAL_IRQ_ENTRY: u16 = 0xff48;
pub const KERNAL_NMI_ENTRY: u16 = 0xfe43;
pub const KERNAL_IRQ_HANDLER: u16 = 0xea31;
pub const KERNAL_IRQ_EXIT: u16 = 0xea81;
pub const KERNAL_NMI_HANDLER: u16 = 0xfe47;
/// Where the KERNAL's BRK vector (`$0316`) points: the driver stops a call that gets here.
pub const KERNAL_BRK_HANDLER: u16 = 0xfe66;

const RTS: u8 = 0x60;
/// "Never": the cycle of an event that cannot happen.
pub const NEVER: u64 = u64::MAX;

const fn stand_in_kernal(extended: bool) -> [u8; 0x2000] {
    let mut rom = [RTS; 0x2000];
    // The bytes are copied in by address.
    macro_rules! put {
        ($addr:expr, $bytes:expr) => {{
            let bytes: &[u8] = &$bytes;
            let mut i = 0;
            while i < bytes.len() {
                rom[$addr as usize - 0xe000 + i] = bytes[i];
                i += 1;
            }
        }};
    }
    // IRQ/BRK entry: save A, X, Y; B flag set -> ($0316), else ($0314).
    put!(
        KERNAL_IRQ_ENTRY,
        [
            0x48, 0x8a, 0x48, 0x98, 0x48, 0xba, 0xbd, 0x04, 0x01, 0x29, 0x10, 0xf0, 0x03, 0x6c,
            0x16, 0x03, 0x6c, 0x14, 0x03
        ]
    );
    // NMI entry: SEI, JMP ($0318).
    put!(KERNAL_NMI_ENTRY, [0x78, 0x6c, 0x18, 0x03]);
    // Default NMI handler: acknowledge CIA 2, return.
    put!(KERNAL_NMI_HANDLER, [0x48, 0xad, 0x0d, 0xdd, 0x68, 0x40]);
    // NMI exit: pull Y, X, A; RTI.
    put!(0xfebcu16, [0x68, 0xa8, 0x68, 0xaa, 0x68, 0x40]);
    // Default IRQ handler: JMP $EA7E (the keyboard scan and clock are left out).
    put!(KERNAL_IRQ_HANDLER, [0x4c, 0x7e, 0xea]);
    // $EA7E: acknowledge CIA 1; $EA81: pull Y, X, A; RTI.
    put!(
        0xea7eu16,
        [0xad, 0x0d, 0xdc, 0x68, 0xa8, 0x68, 0xaa, 0x68, 0x40]
    );
    // Reset lands in an endless loop (nothing here resets).
    put!(0xfce2u16, [0x4c, 0xe2, 0xfc]);
    put!(
        0xfffau16,
        [
            (KERNAL_NMI_ENTRY & 0xff) as u8,
            (KERNAL_NMI_ENTRY >> 8) as u8,
            0xe2,
            0xfc,
            (KERNAL_IRQ_ENTRY & 0xff) as u8,
            (KERNAL_IRQ_ENTRY >> 8) as u8
        ]
    );
    if extended {
        // Interrupt handlers of RSID tunes end in the middle of the KERNAL's own handler: `$EA34` is where it
        // goes on after updating the clock (cursor blink, keyboard scan, acknowledge, pull the registers and RTI)
        // and `$EA7B` is its keyboard scan (`JSR $EA87`, then the acknowledge at `$EA7E`). Without them a
        // handler that jumps there returns to whatever is on the stack.
        put!(0xea34u16, [0x4c, 0x7b, 0xea]); // JMP $EA7B
        put!(0xea7bu16, [0x20, 0x87, 0xea]); // JSR $EA87 (an RTS here), falls into $EA7E
    }
    rom
}

static STAND_IN_KERNAL: [u8; 0x2000] = stand_in_kernal(false);
static STAND_IN_KERNAL_EXTENDED: [u8; 0x2000] = stand_in_kernal(true);

/// Which stand-in KERNAL a machine has (there are no ROM images: see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kernal {
    /// The DAW's: the entries and exits listed above, an `RTS` everywhere else. Tunes whose interrupt
    /// handler ends by jumping into the middle of the real handler (`$EA34`, `$EA7B`) run into it and stop.
    Daw,
    /// The DAW's, with those two entries (`$EA34`, `$EA7B`) leading on to the acknowledge and the exit. What
    /// the DAW's runner does with a tune that does not use them is unchanged. The default.
    #[default]
    Extended,
}

impl Kernal {
    fn rom(self) -> &'static [u8; 0x2000] {
        match self {
            Kernal::Daw => &STAND_IN_KERNAL,
            Kernal::Extended => &STAND_IN_KERNAL_EXTENDED,
        }
    }
}

// ---------------------------------------------------------------------------
// CIA 6526
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct CiaTimer {
    latch: u16,
    /// The counter while stopped.
    counter: u16,
    running: bool,
    one_shot: bool,
    /// While running: the cycle of the next underflow.
    next_underflow: u64,
    /// Timer B only: counts timer A underflows instead of cycles.
    counts_a: bool,
}

impl CiaTimer {
    fn new() -> CiaTimer {
        CiaTimer {
            latch: 0xffff,
            counter: 0xffff,
            running: false,
            one_shot: false,
            next_underflow: NEVER,
            counts_a: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Cia {
    a: CiaTimer,
    b: CiaTimer,
    cra: u8,
    crb: u8,
    mask: u8,
    flags: u8,
    port_a: u8,
    port_b: u8,
    ddr_a: u8,
    ddr_b: u8,
}

impl Cia {
    fn new() -> Cia {
        Cia {
            a: CiaTimer::new(),
            b: CiaTimer::new(),
            cra: 0,
            crb: 0,
            mask: 0,
            flags: 0,
            port_a: 0xff,
            port_b: 0xff,
            ddr_a: 0,
            ddr_b: 0,
        }
    }

    /// Timer A's latch: a CIA-timed tune's tick is `latch + 1` cycles.
    pub fn timer_a_latch(&self) -> u16 {
        self.a.latch
    }

    /// Bring the timers to cycle `now`, setting the interrupt flags of every underflow up to it.
    fn sync(&mut self, now: u64) {
        while self.a.running && self.a.next_underflow <= now {
            let at = self.a.next_underflow;
            self.flags |= 1;
            if self.b.running && self.b.counts_a {
                self.count_b();
            }
            if self.a.one_shot {
                self.a.running = false;
                self.a.counter = self.a.latch;
                self.cra &= !1;
                self.a.next_underflow = NEVER;
            } else {
                self.a.next_underflow = at + self.a.latch as u64 + 1;
            }
        }
        while self.b.running && !self.b.counts_a && self.b.next_underflow <= now {
            let at = self.b.next_underflow;
            self.flags |= 2;
            if self.b.one_shot {
                self.b.running = false;
                self.b.counter = self.b.latch;
                self.crb &= !1;
                self.b.next_underflow = NEVER;
            } else {
                self.b.next_underflow = at + self.b.latch as u64 + 1;
            }
        }
    }

    fn count_b(&mut self) {
        let b = &mut self.b;
        if b.counter == 0 {
            self.flags |= 2;
            b.counter = b.latch;
            if b.one_shot {
                b.running = false;
                self.crb &= !1;
            }
        } else {
            b.counter -= 1;
        }
    }

    /// The counter's value at `now` (after `sync(now)`).
    fn counter_at(&self, is_b: bool, now: u64) -> u16 {
        let t = if is_b { &self.b } else { &self.a };
        if !t.running || (is_b && t.counts_a) {
            return t.counter;
        }
        (t.next_underflow as i64 - now as i64 - 1).clamp(0, 0xffff) as u16
    }

    /// The next cycle at which an enabled interrupt source fires ([`NEVER`]: none).
    fn next_interrupt(&self) -> u64 {
        let mut next = NEVER;
        if self.mask & 1 != 0 && self.a.running {
            next = self.a.next_underflow;
        }
        if self.mask & 2 != 0 && self.b.running && !self.b.counts_a {
            next = next.min(self.b.next_underflow);
        }
        // Timer B counting A: fires on one of A's underflows.
        if self.mask & 2 != 0 && self.b.running && self.b.counts_a && self.a.running {
            next = next.min(self.a.next_underflow);
        }
        next
    }

    fn asserted(&self) -> bool {
        self.flags & self.mask & 0x1f != 0
    }

    fn read(&mut self, reg: u8, now: u64) -> u8 {
        self.sync(now);
        match reg {
            0x0 => self.port_a | !self.ddr_a,
            0x1 => self.port_b | !self.ddr_b,
            0x2 => self.ddr_a,
            0x3 => self.ddr_b,
            0x4 => (self.counter_at(false, now) & 0xff) as u8,
            0x5 => (self.counter_at(false, now) >> 8) as u8,
            0x6 => (self.counter_at(true, now) & 0xff) as u8,
            0x7 => (self.counter_at(true, now) >> 8) as u8,
            0xd => {
                let v = (self.flags & 0x1f) | if self.asserted() { 0x80 } else { 0 };
                self.flags = 0;
                v
            }
            0xe => self.cra & 0xef,
            0xf => self.crb & 0xef,
            _ => 0,
        }
    }

    fn write(&mut self, reg: u8, v: u8, now: u64) {
        self.sync(now);
        match reg {
            0x0 => self.port_a = v,
            0x1 => self.port_b = v,
            0x2 => self.ddr_a = v,
            0x3 => self.ddr_b = v,
            0x4 => self.a.latch = (self.a.latch & 0xff00) | v as u16,
            0x5 => {
                self.a.latch = (self.a.latch & 0x00ff) | ((v as u16) << 8);
                if !self.a.running {
                    self.a.counter = self.a.latch;
                }
            }
            0x6 => self.b.latch = (self.b.latch & 0xff00) | v as u16,
            0x7 => {
                self.b.latch = (self.b.latch & 0x00ff) | ((v as u16) << 8);
                if !self.b.running {
                    self.b.counter = self.b.latch;
                }
            }
            0xd => {
                if v & 0x80 != 0 {
                    self.mask |= v & 0x1f;
                } else {
                    self.mask &= !(v & 0x1f);
                }
            }
            0xe => {
                self.control(false, v, now);
                self.cra = v & !0x10;
            }
            0xf => {
                self.control(true, v, now);
                self.crb = v & !0x10;
            }
            _ => {}
        }
    }

    fn control(&mut self, is_b: bool, v: u8, now: u64) {
        let current = self.counter_at(is_b, now);
        let t = if is_b { &mut self.b } else { &mut self.a };
        t.counter = if v & 0x10 != 0 { t.latch } else { current };
        t.one_shot = v & 0x08 != 0;
        // Counting the CNT pin (nothing drives it) never counts; timer B may count timer A.
        let counts_cnt = if is_b {
            v & 0x60 == 0x20
        } else {
            v & 0x20 != 0
        };
        t.counts_a = is_b && v & 0x40 != 0;
        t.running = v & 1 != 0 && !counts_cnt;
        t.next_underflow = if t.running && !t.counts_a {
            now + t.counter as u64 + 1
        } else {
            NEVER
        };
    }
}

// ---------------------------------------------------------------------------
// SID voice 3 readback
// ---------------------------------------------------------------------------

/// reSID's rate counter periods (cycles per envelope step) for rates 0-15.
const ENV_PERIOD: [i64; 16] = [
    9, 32, 63, 95, 149, 220, 267, 313, 392, 977, 1954, 3126, 3907, 11720, 19532, 31251,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EnvState {
    Attack,
    Decay,
    Release,
}

/// Voice 3's oscillator and envelope, as far as `$D41B`/`$D41C` show them.
#[derive(Clone, Debug)]
struct Voice3 {
    freq: i64,
    pw: i64,
    ctrl: u8,
    ad: u8,
    sr: u8,
    acc: i64,
    lfsr: i64,
    /// The cycle `acc` and `lfsr` are at.
    at: i64,
    // Envelope.
    env: u8,
    state: EnvState,
    env_at: i64,
    rate_count: i64,
    exp_count: i64,
    /// The zero freeze: set by a step that lands on 0, cleared only by a gate-on (reSID's `hold_zero`).
    hold_zero: bool,
}

impl Voice3 {
    fn new() -> Voice3 {
        Voice3 {
            freq: 0,
            pw: 0,
            ctrl: 0,
            ad: 0,
            sr: 0,
            acc: 0,
            lfsr: 0x7ffff8,
            at: 0,
            env: 0,
            state: EnvState::Release,
            env_at: 0,
            rate_count: 0,
            exp_count: 0,
            hold_zero: true,
        }
    }

    fn sync_osc(&mut self, now: i64) {
        let dt = now - self.at;
        self.at = now;
        if dt <= 0 {
            return;
        }
        if self.ctrl & 0x08 != 0 {
            self.acc = 0;
            return;
        }
        let before = self.acc;
        let total = before + self.freq * dt;
        // Noise: the LFSR clocks on every rise of accumulator bit 19.
        let rises = ((total + 0x80000) >> 20) - ((before + 0x80000) >> 20);
        for _ in 0..rises.min(0x7fffff) {
            let bit = ((self.lfsr >> 22) ^ (self.lfsr >> 17)) & 1;
            self.lfsr = ((self.lfsr << 1) | bit) & 0x7fffff;
        }
        self.acc = total % 0x1000000;
    }

    fn osc(&mut self, now: i64) -> u8 {
        self.sync_osc(now);
        let acc = self.acc;
        let mut out: i64 = 0xff;
        let mut any = false;
        if self.ctrl & 0x10 != 0 {
            let tri = (if acc & 0x800000 != 0 {
                acc ^ 0xffffff
            } else {
                acc
            }) >> 15;
            out &= tri & 0xff;
            any = true;
        }
        if self.ctrl & 0x20 != 0 {
            out &= acc >> 16;
            any = true;
        }
        if self.ctrl & 0x40 != 0 {
            out &= if (acc >> 12) >= (self.pw & 0xfff) {
                0xff
            } else {
                0
            };
            any = true;
        }
        if self.ctrl & 0x80 != 0 {
            let l = self.lfsr;
            let noise = (((l >> 22) & 1) << 7)
                | (((l >> 20) & 1) << 6)
                | (((l >> 16) & 1) << 5)
                | (((l >> 13) & 1) << 4)
                | (((l >> 11) & 1) << 3)
                | (((l >> 7) & 1) << 2)
                | (((l >> 4) & 1) << 1)
                | ((l >> 2) & 1);
            out &= noise;
            any = true;
        }
        if any {
            out as u8
        } else {
            0
        }
    }

    /// Step the envelope to `now`, one rate period at a time.
    fn sync_env(&mut self, now: i64) {
        loop {
            let rate = match self.state {
                EnvState::Attack => self.ad >> 4,
                EnvState::Decay => self.ad & 0x0f,
                EnvState::Release => self.sr & 0x0f,
            };
            let period = ENV_PERIOD[rate as usize];
            let next = self.env_at + period - self.rate_count;
            if next > now {
                self.rate_count += now - self.env_at;
                self.env_at = now;
                return;
            }
            self.env_at = next;
            self.rate_count = 0;
            if self.hold_zero {
                self.env_at = now;
                return;
            }
            if self.state == EnvState::Attack {
                // From 255 (a release step after a gate-on that never stepped) the counter wraps to 0 and freezes.
                self.env = self.env.wrapping_add(1);
                if self.env == 0 {
                    self.hold_zero = true;
                }
                if self.env == 0xff {
                    self.state = EnvState::Decay;
                }
                continue;
            }
            let exp_period = if self.env >= 0x5d {
                1
            } else if self.env >= 0x36 {
                2
            } else if self.env >= 0x1a {
                4
            } else if self.env >= 0x0e {
                8
            } else if self.env >= 0x06 {
                16
            } else {
                30
            };
            self.exp_count += 1;
            if self.exp_count < exp_period {
                continue;
            }
            self.exp_count = 0;
            let sustain = (self.sr >> 4) * 0x11;
            // An unfrozen 0 (a gate-on with no attack step yet) wraps to 255 in release.
            if self.state == EnvState::Release {
                self.env = self.env.wrapping_sub(1);
            } else if self.env > sustain {
                self.env -= 1;
            }
            if self.env == 0 {
                self.hold_zero = true;
            }
            if self.hold_zero || (self.state == EnvState::Decay && self.env <= sustain) {
                // Held (sustain) or frozen at zero until the next gate change.
                self.env_at = now;
                return;
            }
        }
    }

    fn write_ctrl(&mut self, v: u8, now: i64) {
        self.sync_osc(now);
        self.sync_env(now);
        let gate_on = v & 1 != 0 && self.ctrl & 1 == 0;
        let gate_off = v & 1 == 0 && self.ctrl & 1 != 0;
        if v & 0x08 != 0 {
            self.acc = 0;
        }
        self.ctrl = v;
        if gate_on {
            self.state = EnvState::Attack;
            self.hold_zero = false;
        }
        if gate_off {
            self.state = EnvState::Release;
        }
    }
}

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

/// One write to a SID, as the CPU made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidWrite {
    /// The machine cycle (the CPU's clock) of the instruction that wrote.
    pub cycle: u64,
    /// Which chip: 0 is the one at `$D400`.
    pub chip: u8,
    pub reg: u8,
    pub value: u8,
}

/// The machine's SID writes, in the order they were made, waiting to be handed
/// over. Room for a stretch of playback is reserved up front: the player
/// drains it after every few hundred cycles, so it does not grow while a tune
/// plays (a tune's init may write any number of times, and grows it then).
#[derive(Clone, Debug)]
pub struct WriteQueue {
    items: Vec<SidWrite>,
    head: usize,
}

/// Writes the queue has room for before it grows.
pub const WRITE_QUEUE_ROOM: usize = 1024;

impl WriteQueue {
    fn new() -> WriteQueue {
        WriteQueue {
            items: Vec::with_capacity(WRITE_QUEUE_ROOM),
            head: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len() - self.head
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.items.capacity()
    }

    fn push(&mut self, w: SidWrite) {
        if self.items.len() == self.items.capacity() && self.head > 0 {
            // Reuse the room the drained writes left rather than grow.
            self.items.drain(..self.head);
            self.head = 0;
        }
        self.items.push(w);
    }

    /// The oldest write waiting.
    pub fn front(&self) -> Option<&SidWrite> {
        self.items.get(self.head)
    }

    /// Takes the oldest write waiting.
    pub fn pop(&mut self) -> Option<SidWrite> {
        let w = *self.items.get(self.head)?;
        self.head += 1;
        if self.head == self.items.len() {
            self.head = 0;
            self.items.clear();
        }
        Some(w)
    }
}

/// The DAW's Zobrist key half: a 32-bit mix of `k`.
fn zobrist(k: u32, seed: u32) -> u32 {
    let mut x = (k ^ seed).wrapping_mul(0x9e37_79b1);
    x ^= x >> 15;
    x = x.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 13;
    x = x.wrapping_mul(0xc2b2_ae35);
    x ^= x >> 16;
    x
}

const SEED_A: u32 = 0x2545_f491;
const SEED_B: u32 = 0x6c8e_9cf5;
/// I/O keys live above RAM's in the key space.
const IO_KEY: u32 = 0x1_0000;

/// A 64-bit Zobrist hash of everything a player can change: RAM and the I/O
/// registers written, updated on every write (`C64::enable_state_hash`).
#[derive(Clone)]
struct StateHash {
    /// RAM as the hash has it.
    ram: Box<[u8; 0x10000]>,
    /// The I/O registers as written, by `addr & $FFF`.
    io: Box<[u8; 0x1000]>,
    a: u32,
    b: u32,
    /// While set, writes leave the hash alone (an NMI handler's sample playback is not the music's state).
    paused: bool,
}

impl StateHash {
    #[inline]
    fn ram_write(&mut self, addr: usize, v: u8) {
        if self.paused {
            return;
        }
        let old = self.ram[addr];
        if old == v {
            return;
        }
        let k = (addr as u32) << 8;
        self.a ^= zobrist(k | old as u32, SEED_A) ^ zobrist(k | v as u32, SEED_A);
        self.b ^= zobrist(k | old as u32, SEED_B) ^ zobrist(k | v as u32, SEED_B);
        self.ram[addr] = v;
    }

    #[inline]
    fn io_write(&mut self, addr: u16, v: u8) {
        if self.paused {
            return;
        }
        let i = (addr & 0xfff) as usize;
        let old = self.io[i];
        if old == v {
            return;
        }
        let k = (IO_KEY | addr as u32) << 8;
        self.a ^= zobrist(k | old as u32, SEED_A) ^ zobrist(k | v as u32, SEED_A);
        self.b ^= zobrist(k | old as u32, SEED_B) ^ zobrist(k | v as u32, SEED_B);
        self.io[i] = v;
    }
}

/// Everything on the C64's bus but the CPU.
pub struct System {
    ram: Box<[u8; 0x10000]>,
    pub timing: C64Timing,
    pub cia1: Cia,
    pub cia2: Cia,
    /// SID chip base addresses; chip 0 is `$D400`.
    sid_bases: [u16; 3],
    sid_count: usize,
    /// Every SID write, for the runner to hand over.
    pub writes: WriteQueue,
    basic_in: bool,
    kernal_in: bool,
    io_in: bool,
    vic: [u8; 0x40],
    raster_compare: u32,
    vic_flags: u8,
    vic_mask: u8,
    /// Cycle of the next raster interrupt event ([`NEVER`] while none can fire).
    next_raster: u64,
    nmi_asserted: bool,
    /// An NMI edge waits to be taken.
    nmi_pending: bool,
    sid_bus: u8,
    voice3: Voice3,
    /// Cycle of the next timer/raster event to look at.
    next_event: u64,
    kernal: &'static [u8; 0x2000],
    /// The Zobrist state hash, when a capture asked for it.
    hash: Option<Box<StateHash>>,
}

/// What an interrupt check took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    Irq,
    Nmi,
}

pub struct C64 {
    pub cpu: Cpu6510,
    pub sys: System,
}

impl C64 {
    /// A powered-on machine as the KERNAL leaves it. `extra_sids` are the base
    /// addresses of a second and third SID (at most two are used).
    pub fn new(clock: C64Clock, extra_sids: &[u16]) -> C64 {
        C64::with_kernal(clock, extra_sids, Kernal::default())
    }

    /// [`C64::new`] with the stand-in KERNAL of your choice.
    pub fn with_kernal(clock: C64Clock, extra_sids: &[u16], kernal: Kernal) -> C64 {
        let timing = clock.timing();
        let mut sid_bases = [0xd400, 0, 0];
        let mut sid_count = 1;
        for &base in extra_sids.iter().take(2) {
            sid_bases[sid_count] = base;
            sid_count += 1;
        }
        let mut sys = System {
            ram: vec![0u8; 0x10000]
                .into_boxed_slice()
                .try_into()
                .expect("64 KiB"),
            timing,
            cia1: Cia::new(),
            cia2: Cia::new(),
            sid_bases,
            sid_count,
            writes: WriteQueue::new(),
            basic_in: true,
            kernal_in: true,
            io_in: true,
            vic: [0; 0x40],
            raster_compare: 0,
            vic_flags: 0,
            vic_mask: 0,
            next_raster: NEVER,
            nmi_asserted: false,
            nmi_pending: false,
            sid_bus: 0,
            voice3: Voice3::new(),
            next_event: NEVER,
            kernal: kernal.rom(),
            hash: None,
        };
        let ram = &mut sys.ram;
        ram[0x00] = 0x2f;
        ram[0x01] = 0x37;
        // KERNAL RAM vectors and the PAL/NTSC flag, as after boot.
        for (i, &v) in [
            KERNAL_IRQ_HANDLER & 0xff,
            KERNAL_IRQ_HANDLER >> 8,
            KERNAL_BRK_HANDLER & 0xff,
            KERNAL_BRK_HANDLER >> 8,
            KERNAL_NMI_HANDLER & 0xff,
            KERNAL_NMI_HANDLER >> 8,
        ]
        .iter()
        .enumerate()
        {
            ram[0x0314 + i] = v as u8;
        }
        ram[0x02a6] = if clock == C64Clock::Pal { 1 } else { 0 };
        sys.update_banks();
        // The KERNAL's 60 Hz interrupt: CIA 1 timer A, continuous, IRQ enabled.
        sys.cia1
            .write(0x4, (timing.kernal_cia_latch & 0xff) as u8, 0);
        sys.cia1.write(0x5, (timing.kernal_cia_latch >> 8) as u8, 0);
        sys.cia1.write(0xd, 0x81, 0);
        sys.cia1.write(0xe, 0x11, 0);
        sys.vic[0x11] = 0x1b;
        let mut m = C64 {
            cpu: Cpu6510::new(),
            sys,
        };
        m.sys.schedule_events(0);
        m
    }

    /// How many SID chips the machine has (1 to 3).
    pub fn sid_count(&self) -> usize {
        self.sys.sid_count
    }

    /// Start keeping the state hash ([`C64::state_hash`]). Call it on a fresh
    /// machine, before the tune is loaded, as the DAW's machine always hashes:
    /// the hash then covers every write from the start.
    pub fn enable_state_hash(&mut self) {
        let sys = &mut self.sys;
        sys.hash = Some(Box::new(StateHash {
            ram: sys.ram.clone(),
            io: Box::new([0; 0x1000]),
            a: 0,
            b: 0,
            paused: false,
        }));
    }

    /// The 64-bit hash of the RAM and the I/O registers written (0 before
    /// [`C64::enable_state_hash`]): two equal hashes at two moments mean the
    /// machine's state, as far as a player can change it, is the same.
    pub fn state_hash(&self) -> u64 {
        match self.sys.hash.as_deref() {
            Some(h) => ((h.a as u64) << 32) | h.b as u64,
            None => 0,
        }
    }

    /// While paused, writes leave the state hash alone (a capture pauses it
    /// while an NMI handler runs: a sample pointer that never repeats would
    /// hide the song's loop).
    pub fn set_hash_paused(&mut self, paused: bool) {
        if let Some(h) = self.sys.hash.as_deref_mut() {
            h.paused = paused;
        }
    }

    /// Load `data` at `address` (RAM, whatever the banking).
    pub fn load(&mut self, address: u16, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            self.sys.ram_set((address as usize + i) & 0xffff, b);
        }
    }

    /// Set the `$01` banking (as a write to `$01`).
    pub fn set_banks(&mut self, value: u8) {
        self.sys.ram_set(1, value);
        self.sys.update_banks();
    }

    /// The `$01` banking.
    pub fn banks(&self) -> u8 {
        self.sys.ram[1]
    }

    /// The RAM, for tests and the runner's setup.
    pub fn ram(&self) -> &[u8; 0x10000] {
        &self.sys.ram
    }

    /// A read through the bus at the CPU's cycle (I/O reads have their effects).
    pub fn read(&mut self, addr: u16) -> u8 {
        let now = self.cpu.cycles;
        self.sys.read(addr, now)
    }

    /// A write through the bus at the CPU's cycle.
    pub fn write(&mut self, addr: u16, value: u8) {
        let now = self.cpu.cycles;
        self.sys.write(addr, value, now);
    }

    /// Whether the IRQ line is held (CIA 1 or the VIC).
    pub fn irq_line(&self) -> bool {
        self.sys.irq_line()
    }

    /// The cycle of the next interrupt event (for idling up to it).
    pub fn next_event_cycle(&self) -> u64 {
        self.sys.next_event
    }

    /// Whether the CPU would take an interrupt before its next instruction: an
    /// NMI edge, or the IRQ line held with I clear. An idle CPU must not skip
    /// time past one (an IRQ that came while an NMI ran is taken as that NMI
    /// returns, not at the next event).
    pub fn interrupt_waiting(&mut self) -> bool {
        if self.cpu.cycles >= self.sys.next_event {
            self.sys.service(self.cpu.cycles);
        }
        self.sys.nmi_pending || (self.sys.irq_line() && self.cpu.p & I == 0)
    }

    /// Take a waiting interrupt, if any. Call before each instruction ([`C64::step`] does).
    pub fn take_interrupt(&mut self) -> Option<Taken> {
        if self.cpu.cycles >= self.sys.next_event {
            self.sys.service(self.cpu.cycles);
        }
        if self.sys.nmi_pending {
            self.sys.nmi_pending = false;
            self.cpu.nmi(&mut self.sys);
            return Some(Taken::Nmi);
        }
        if self.sys.irq_line() && self.cpu.irq(&mut self.sys) {
            return Some(Taken::Irq);
        }
        None
    }

    /// Advance time to `cycle` without running code (an idle CPU).
    pub fn idle_to(&mut self, cycle: u64) {
        if cycle > self.cpu.cycles {
            self.cpu.cycles = cycle;
        }
    }

    /// Run one instruction (taking an interrupt first if one is due).
    pub fn step(&mut self) -> Option<Taken> {
        let taken = self.take_interrupt();
        self.cpu.step(&mut self.sys);
        taken
    }

    /// Run one instruction without taking interrupts.
    #[inline]
    pub fn step_cpu(&mut self) -> u32 {
        self.cpu.step(&mut self.sys)
    }
}

impl System {
    #[inline]
    fn ram_set(&mut self, addr: usize, v: u8) {
        self.ram[addr] = v;
        if let Some(h) = self.hash.as_deref_mut() {
            h.ram_write(addr, v);
        }
    }

    fn update_banks(&mut self) {
        let bits = (self.ram[1] | !self.ram[0]) & 7;
        let lo = bits & 1 != 0;
        let hi = bits & 2 != 0;
        self.basic_in = lo && hi;
        self.kernal_in = hi;
        self.io_in = (lo || hi) && bits & 4 != 0;
    }

    fn sid_chip(&self, addr: u16) -> Option<usize> {
        for i in (1..self.sid_count).rev() {
            let base = self.sid_bases[i];
            if addr >= base && addr < base + 0x20 {
                return Some(i);
            }
        }
        if (0xd400..0xd800).contains(&addr) {
            Some(0)
        } else {
            None
        }
    }

    fn read_io(&mut self, addr: u16, now: u64) -> u8 {
        if addr < 0xd400 {
            return self.read_vic((addr & 0x3f) as u8, now);
        }
        if !(0xd800..0xde00).contains(&addr) {
            let Some(chip) = self.sid_chip(addr) else {
                return 0;
            };
            let reg = (addr & 0x1f) as u8;
            if chip == 0 && reg == 0x1b {
                return self.voice3.osc(now as i64);
            }
            if chip == 0 && reg == 0x1c {
                self.voice3.sync_env(now as i64);
                return self.voice3.env;
            }
            if reg == 0x19 || reg == 0x1a {
                return 0xff;
            }
            return self.sid_bus;
        }
        if addr < 0xdc00 {
            return self.ram[addr as usize] & 0x0f;
        }
        if addr < 0xdd00 {
            let v = self.cia1.read((addr & 0x0f) as u8, now);
            if addr & 0x0f == 0xd {
                self.schedule_events(now);
            }
            return v;
        }
        if addr < 0xde00 {
            let v = self.cia2.read((addr & 0x0f) as u8, now);
            if addr & 0x0f == 0xd {
                self.nmi_asserted = self.cia2.asserted();
                self.schedule_events(now);
            }
            return v;
        }
        0
    }

    fn write_io(&mut self, addr: u16, v: u8, now: u64) {
        if let Some(h) = self.hash.as_deref_mut() {
            h.io_write(addr, v);
        }
        if addr < 0xd400 {
            self.write_vic((addr & 0x3f) as u8, v, now);
            return;
        }
        if !(0xd800..0xde00).contains(&addr) {
            let Some(chip) = self.sid_chip(addr) else {
                return;
            };
            let reg = (addr & 0x1f) as u8;
            self.sid_bus = v;
            if chip == 0 {
                self.voice3_write(reg, v, now as i64);
            }
            self.writes.push(SidWrite {
                cycle: now,
                chip: chip as u8,
                reg,
                value: v,
            });
            return;
        }
        if addr < 0xdc00 {
            self.ram_set(addr as usize, v & 0x0f);
            return;
        }
        if addr < 0xdd00 {
            self.cia1.write((addr & 0x0f) as u8, v, now);
            self.schedule_events(now);
            return;
        }
        // Past $DE00 is the SID area handled above; this is CIA 2.
        self.cia2.write((addr & 0x0f) as u8, v, now);
        self.schedule_events(now);
    }

    fn voice3_write(&mut self, reg: u8, v: u8, now: i64) {
        let v3 = &mut self.voice3;
        match reg {
            0x0e => {
                v3.sync_osc(now);
                v3.freq = (v3.freq & 0xff00) | v as i64;
            }
            0x0f => {
                v3.sync_osc(now);
                v3.freq = (v3.freq & 0x00ff) | ((v as i64) << 8);
            }
            0x10 => v3.pw = (v3.pw & 0xf00) | v as i64,
            0x11 => v3.pw = (v3.pw & 0x0ff) | (((v & 0x0f) as i64) << 8),
            0x12 => v3.write_ctrl(v, now),
            0x13 => {
                v3.sync_env(now);
                v3.ad = v;
            }
            0x14 => {
                v3.sync_env(now);
                v3.sr = v;
            }
            _ => {}
        }
    }

    // --- VIC-II -----------------------------------------------------------

    fn raster_line(&self, now: u64) -> u32 {
        ((now / self.timing.cycles_per_line as u64) % self.timing.lines as u64) as u32
    }

    fn read_vic(&mut self, reg: u8, now: u64) -> u8 {
        self.sync_raster(now);
        match reg {
            0x11 => (self.vic[0x11] & 0x7f) | ((self.raster_line(now) & 0x100) >> 1) as u8,
            0x12 => (self.raster_line(now) & 0xff) as u8,
            0x19 => {
                (self.vic_flags & 0x0f)
                    | 0x70
                    | if self.vic_flags & self.vic_mask & 0x0f != 0 {
                        0x80
                    } else {
                        0
                    }
            }
            0x1a => self.vic_mask | 0xf0,
            _ => self.vic[reg as usize],
        }
    }

    fn write_vic(&mut self, reg: u8, v: u8, now: u64) {
        self.sync_raster(now);
        self.vic[reg as usize] = v;
        match reg {
            0x11 => {
                self.raster_compare = (self.raster_compare & 0xff) | (((v & 0x80) as u32) << 1);
                self.schedule_raster(now);
            }
            0x12 => {
                self.raster_compare = (self.raster_compare & 0x100) | v as u32;
                self.schedule_raster(now);
            }
            0x19 => self.vic_flags &= !v & 0x0f,
            0x1a => self.vic_mask = v & 0x0f,
            _ => {}
        }
        self.schedule_events(now);
    }

    /// The start cycle of the next line `raster_compare` after `now` ([`NEVER`] if the line does not exist).
    fn schedule_raster(&mut self, now: u64) {
        let t = self.timing;
        if self.raster_compare >= t.lines {
            self.next_raster = NEVER;
            return;
        }
        let line_now = now / t.cycles_per_line as u64;
        let in_frame = (line_now % t.lines as u64) as u32;
        let mut delta = (self.raster_compare + t.lines - in_frame) % t.lines;
        if delta == 0 {
            delta = t.lines;
        }
        self.next_raster = (line_now + delta as u64) * t.cycles_per_line as u64;
    }

    fn sync_raster(&mut self, now: u64) {
        while self.next_raster <= now {
            self.vic_flags |= 1;
            self.next_raster += self.timing.frame_cycles();
        }
    }

    // --- Interrupts and time ----------------------------------------------

    fn schedule_events(&mut self, now: u64) {
        if self.next_raster == NEVER && self.raster_compare < self.timing.lines {
            self.schedule_raster(now);
        }
        let raster = if self.vic_mask & 1 != 0 {
            self.next_raster
        } else {
            NEVER
        };
        self.next_event = raster
            .min(self.cia1.next_interrupt())
            .min(self.cia2.next_interrupt());
    }

    fn irq_line(&self) -> bool {
        self.cia1.asserted() || self.vic_flags & self.vic_mask & 0x0f != 0
    }

    /// Bring the chips to the CPU's cycle: set flags and latch an NMI edge.
    fn service(&mut self, now: u64) {
        self.sync_raster(now);
        self.cia1.sync(now);
        self.cia2.sync(now);
        let nmi = self.cia2.asserted();
        if nmi && !self.nmi_asserted {
            self.nmi_pending = true;
        }
        self.nmi_asserted = nmi;
        self.schedule_events(now);
    }

    fn read_ram_or_rom(&mut self, addr: u16, now: u64) -> u8 {
        if addr >= 0xa000 {
            if addr < 0xc000 {
                if self.basic_in {
                    return RTS;
                }
            } else if addr >= 0xe000 {
                if self.kernal_in {
                    return self.kernal[addr as usize - 0xe000];
                }
            } else if addr >= 0xd000 && self.io_in {
                return self.read_io(addr, now);
            }
        }
        self.ram[addr as usize]
    }
}

impl CpuBus for System {
    #[inline]
    fn read(&mut self, addr: u16, cycle: u64) -> u8 {
        self.read_ram_or_rom(addr, cycle)
    }

    #[inline]
    fn write(&mut self, addr: u16, v: u8, cycle: u64) {
        if (0xd000..0xe000).contains(&addr) && self.io_in {
            self.write_io(addr, v, cycle);
            return;
        }
        self.ram_set(addr as usize, v);
        if addr < 2 {
            self.update_banks();
        }
    }
}
