//! Plays a `.sid` as it is: the tune's 6502 code runs on the emulated C64
//! ([`C64`]) in step with the audio, and every write it makes to a SID comes out
//! with the cycle it was made on, ready to be scheduled on a chip. A port of
//! another_synth's `psid/psid-runner.ts` (and of the driver in `sid-capture.ts`
//! it is made from).
//!
//! The tune is driven the way HVSC's environment drives it: a PSID with a play
//! address has init called with the subsong in A and play called once per tick,
//! per video frame or at CIA 1 timer A's rate; an RSID, or a play address of 0,
//! is left to its own interrupts after init.
//!
//! Time is in CPU cycles from the start of playback (the `origin`), so a write
//! init made before it is at cycle 0. After [`PsidRunner::advance`]`(c)`, every
//! write at a cycle below `c` is in the queue and no later write will be stamped
//! below it.
//!
//! One difference from the DAW's runner: it runs a play call to its end and may
//! queue writes well past the cycle asked for; this one stops a play call
//! *where the cycle asked for falls* and resumes it on the next `advance`. The
//! writes are the same, whatever the step; and the queue holds only a step's
//! worth, so a player that feeds a chip every few hundred cycles never makes it
//! grow.

use super::c64::{C64Clock, SidWrite, C64, KERNAL_BRK_HANDLER, NEVER};
use super::file::{PsidError, PsidFile, PsidType};
use std::fmt;

/// Where a host call returns to: in the I/O area, where no player's code runs.
pub const RETURN_TRAP: u16 = 0xdff0;
/// Where the host idles between the tune's interrupts.
pub const IDLE_TRAP: u16 = 0xdff8;
/// The cycles an init routine may take before the tune is refused.
pub const INIT_MAX_CYCLES: u64 = 60_000_000;
/// The cycles one play call may take before the tune is stopped.
pub const PLAY_MAX_CYCLES: u64 = 2_000_000;

/// The `$01` value for a PSID call at `addr` (libsidplayfp's `iomap`).
pub fn psid_banks_for(addr: u16) -> u8 {
    if addr < 0xa000 {
        0x37
    } else if addr < 0xd000 {
        0x36
    } else if addr >= 0xe000 {
        0x35
    } else {
        0x34
    }
}

/// Set the CPU up as if the host had JSR'd to `addr` with A = `a` and interrupts off.
pub fn enter_call(machine: &mut C64, addr: u16, a: u8) {
    machine.write(0x1ff, ((RETURN_TRAP - 1) >> 8) as u8);
    machine.write(0x1fe, ((RETURN_TRAP - 1) & 0xff) as u8);
    let cpu = &mut machine.cpu;
    cpu.a = a;
    cpu.x = 0;
    cpu.y = 0;
    cpu.p = 0x24;
    cpu.sp = 0xfd;
    cpu.pc = addr;
    cpu.jammed = false;
}

/// How a host call to a tune's routine ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallEnd {
    Return,
    /// The routine left through the KERNAL's interrupt exit (`$EA31`-`$EA83`).
    KernalExit,
    /// The routine ended with an RTI, as an interrupt handler does.
    Rti,
    Jam,
    Brk,
    Timeout,
}

/// Runs the call `enter_call` set up until it ends, the CPU reaches `limit`
/// cycles (a timeout), or the CPU's clock reaches `target` (`None`: still
/// running; call again to go on). Interrupts are off.
pub fn run_call(machine: &mut C64, limit: u64, target: u64) -> Option<CallEnd> {
    loop {
        if machine.cpu.cycles >= limit {
            return Some(CallEnd::Timeout);
        }
        let pc = machine.cpu.pc;
        if pc == RETURN_TRAP {
            return Some(CallEnd::Return);
        }
        if (0xea31..=0xea83).contains(&pc) && machine.banks() & 2 != 0 {
            return Some(CallEnd::KernalExit);
        }
        if pc == KERNAL_BRK_HANDLER && machine.banks() & 2 != 0 {
            return Some(CallEnd::Brk);
        }
        if machine.cpu.jammed {
            return Some(CallEnd::Jam);
        }
        if machine.read(pc) == 0x40 && machine.cpu.sp >= 0xfd {
            return Some(CallEnd::Rti);
        }
        if machine.cpu.cycles >= target {
            return None;
        }
        machine.step_cpu();
    }
}

/// A host JSR to `addr` with A = `a` and interrupts off, run until it returns
/// (or leaves the way an interrupt handler does).
pub fn call_routine(machine: &mut C64, addr: u16, a: u8, max_cycles: u64) -> CallEnd {
    enter_call(machine, addr, a);
    let limit = machine.cpu.cycles + max_cycles;
    run_call(machine, limit, NEVER).unwrap_or(CallEnd::Timeout)
}

/// Why a tune stopped playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// The play routine at `addr` stopped the CPU (a JAM opcode at `pc`).
    PlayJam {
        addr: u16,
        pc: u16,
    },
    PlayBrk {
        addr: u16,
    },
    /// The play routine did not return in [`PLAY_MAX_CYCLES`].
    PlayTimeout {
        addr: u16,
    },
    /// An interrupt-driven tune stopped the CPU.
    Jam {
        pc: u16,
    },
    Brk,
}

impl fmt::Display for EndReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = |v: u16| format!("${v:04X}");
        match *self {
            EndReason::PlayJam { addr, pc } => write!(
                f,
                "the tune's play routine ({}) stopped the CPU (JAM at {})",
                h(addr),
                h(pc)
            ),
            EndReason::PlayBrk { addr } => {
                write!(f, "the tune's play routine ({}) hit a BRK", h(addr))
            }
            EndReason::PlayTimeout { addr } => {
                write!(f, "the tune's play routine ({}) did not return", h(addr))
            }
            EndReason::Jam { pc } => write!(f, "the tune stopped the CPU (JAM at {})", h(pc)),
            EndReason::Brk => f.write_str("the tune hit a BRK"),
        }
    }
}

/// Host-driven (`Play`), or left to its own interrupts after init (`Irq`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    Play,
    Irq,
}

pub struct PsidRunner {
    machine: C64,
    clock: C64Clock,
    clock_hz: u32,
    mode: RunMode,
    /// Cycles between ticks of a host-driven tune: a video frame, or CIA 1 timer
    /// A's period, which a CIA-timed tune may reprogram while it plays (Rubicon
    /// starts at 60 Hz and sets about 124 Hz in its first play call), so it is
    /// read again after every tick.
    tick_cycles: u64,
    /// The song is timed by CIA 1 timer A (the header's speed bit), not by the video frame.
    cia_timed: bool,
    ended: Option<EndReason>,
    play_address: u16,
    /// Machine cycle that is cycle 0 of the output.
    origin: u64,
    /// The machine cycle the next host-driven tick is due on.
    next: u64,
    /// A play call in progress, with the machine cycle it times out on.
    call_limit: Option<u64>,
    /// Writes to chips that were asked for in the file but ignored (see `sid_count`).
    extra_sid_writes: u64,
}

impl PsidRunner {
    /// Subsong `subsong` (0-based) of `file`, initialised and ready to
    /// [`advance`](PsidRunner::advance). The tune's init routine runs here, up to
    /// [`INIT_MAX_CYCLES`] of it: build it off the audio thread. Never panics for
    /// a tune's behaviour.
    pub fn new(file: &PsidFile, subsong: usize) -> Result<PsidRunner, PsidError> {
        let clock = if file.clock == super::file::PsidClock::Ntsc {
            C64Clock::Ntsc
        } else {
            C64Clock::Pal
        };
        let mut machine = C64::new(clock, &file.extra_sids);
        machine.load(file.load_address, &file.data);
        let song = subsong.min(file.songs as usize - 1);
        let irq_mode = file.kind == PsidType::Rsid || file.play_address == 0;

        let mut tick_cycles = machine.sys.timing.frame_cycles();
        let mut cia_timed = false;
        if !irq_mode {
            machine.set_banks(psid_banks_for(file.init_address));
            match call_routine(&mut machine, file.init_address, song as u8, INIT_MAX_CYCLES) {
                CallEnd::Jam => {
                    return Err(PsidError::InitJam {
                        addr: file.init_address,
                        pc: machine.cpu.pc,
                    })
                }
                CallEnd::Brk => {
                    return Err(PsidError::InitBrk {
                        addr: file.init_address,
                    })
                }
                CallEnd::Timeout => {
                    return Err(PsidError::InitTimeout {
                        addr: file.init_address,
                        million_cycles: (INIT_MAX_CYCLES / 1_000_000) as u32,
                    })
                }
                CallEnd::Return | CallEnd::KernalExit | CallEnd::Rti => {}
            }
            if file.uses_cia(song as u16 + 1) {
                cia_timed = true;
                tick_cycles = machine.sys.cia1.timer_a_latch() as u64 + 1;
            }
        } else {
            if file.kind == PsidType::Psid {
                machine.set_banks(psid_banks_for(file.init_address));
            }
            enter_call(&mut machine, file.init_address, song as u8);
        }

        let origin = machine.cpu.cycles;
        Ok(PsidRunner {
            clock_hz: machine.sys.timing.hz,
            machine,
            clock,
            mode: if irq_mode {
                RunMode::Irq
            } else {
                RunMode::Play
            },
            tick_cycles,
            cia_timed,
            ended: None,
            play_address: file.play_address,
            origin,
            next: origin,
            call_limit: None,
            extra_sid_writes: 0,
        })
    }

    pub fn clock(&self) -> C64Clock {
        self.clock
    }

    /// CPU cycles per second of the machine.
    pub fn clock_hz(&self) -> u32 {
        self.clock_hz
    }

    pub fn mode(&self) -> RunMode {
        self.mode
    }

    /// Cycles between ticks of a host-driven tune.
    pub fn tick_cycles(&self) -> u64 {
        self.tick_cycles
    }

    /// Why the tune stopped (its player jammed the CPU, hit a BRK, ran away), or `None` while it plays.
    pub fn ended(&self) -> Option<EndReason> {
        self.ended
    }

    /// The emulated machine, for inspection.
    pub fn machine(&self) -> &C64 {
        &self.machine
    }

    /// How many SID chips the tune addresses (the first at `$D400`, then the file's second and third).
    pub fn sid_count(&self) -> usize {
        self.machine.sid_count()
    }

    /// The machine cycle of cycle 0 of the output (what the tune's init spent).
    pub fn origin(&self) -> u64 {
        self.origin
    }

    /// Writes handed over so far that named a chip past `max_chips` (see [`PsidRunner::drain`]).
    pub fn extra_sid_writes(&self) -> u64 {
        self.extra_sid_writes
    }

    /// The cycle (from the start of playback) before which every write is
    /// known; [`u64::MAX`] once the tune has stopped.
    pub fn horizon(&self) -> u64 {
        if self.ended.is_some() {
            return NEVER;
        }
        let cpu = self.machine.cpu.cycles;
        let h = if self.mode == RunMode::Play && self.call_limit.is_none() {
            self.next.max(cpu)
        } else {
            cpu
        };
        h.saturating_sub(self.origin)
    }

    /// How many writes wait to be drained.
    pub fn pending(&self) -> usize {
        self.machine.sys.writes.len()
    }

    /// Room in the write queue before it would grow.
    pub fn queue_capacity(&self) -> usize {
        self.machine.sys.writes.capacity()
    }

    /// Hand over, in order, the writes made before cycle `until` (from the start
    /// of playback), each with its cycle (never below 0), chip, register and
    /// value. Writes to a chip at or past `max_chips` are counted
    /// ([`extra_sid_writes`](PsidRunner::extra_sid_writes)) and not handed over.
    /// `until` must not be past [`horizon`](PsidRunner::horizon).
    pub fn drain(&mut self, until: u64, max_chips: usize, mut sink: impl FnMut(SidWrite)) {
        let limit = until.saturating_add(self.origin);
        let origin = self.origin;
        let queue = &mut self.machine.sys.writes;
        while let Some(w) = queue.front() {
            if w.cycle >= limit {
                break;
            }
            let w = queue.pop().expect("front exists");
            if (w.chip as usize) < max_chips {
                sink(SidWrite {
                    cycle: w.cycle.saturating_sub(origin),
                    ..w
                });
            } else {
                self.extra_sid_writes += 1;
            }
        }
    }

    fn end(&mut self, why: EndReason) {
        self.ended = Some(why);
    }

    /// Run the tune until emulated time reaches cycle `until` (from the start of playback).
    pub fn advance(&mut self, until: u64) {
        if self.ended.is_some() {
            return;
        }
        let target = until.saturating_add(self.origin);
        match self.mode {
            RunMode::Play => self.advance_play(target),
            RunMode::Irq => self.advance_irq(target),
        }
    }

    fn advance_play(&mut self, target: u64) {
        let addr = self.play_address;
        loop {
            let limit = match self.call_limit {
                Some(limit) => limit,
                None => {
                    // The next call starts no earlier than its tick, and no earlier than the last one ended.
                    if self.next.max(self.machine.cpu.cycles) >= target {
                        return;
                    }
                    self.machine.idle_to(self.next);
                    self.machine.set_banks(psid_banks_for(addr));
                    enter_call(&mut self.machine, addr, 0);
                    let limit = self.machine.cpu.cycles + PLAY_MAX_CYCLES;
                    self.call_limit = Some(limit);
                    limit
                }
            };
            let Some(end) = run_call(&mut self.machine, limit, target) else {
                return;
            };
            self.call_limit = None;
            match end {
                CallEnd::Jam => {
                    let pc = self.machine.cpu.pc;
                    return self.end(EndReason::PlayJam { addr, pc });
                }
                CallEnd::Brk => return self.end(EndReason::PlayBrk { addr }),
                CallEnd::Timeout => return self.end(EndReason::PlayTimeout { addr }),
                CallEnd::Return | CallEnd::KernalExit | CallEnd::Rti => {}
            }
            // A CIA-timed tune may have reprogrammed the timer during this call: the next tick follows it.
            if self.cia_timed {
                self.tick_cycles = (self.machine.sys.cia1.timer_a_latch() as u64 + 1).max(1);
            }
            self.next += self.tick_cycles;
        }
    }

    fn advance_irq(&mut self, target: u64) {
        let machine = &mut self.machine;
        while machine.cpu.cycles < target {
            if machine.cpu.jammed {
                let pc = machine.cpu.pc;
                return self.end(EndReason::Jam { pc });
            }
            if machine.cpu.pc == KERNAL_BRK_HANDLER && machine.banks() & 2 != 0 {
                return self.end(EndReason::Brk);
            }
            if machine.cpu.pc == RETURN_TRAP {
                // Init returned to the host: interrupts on, idle.
                machine.cpu.pc = IDLE_TRAP;
                machine.cpu.p &= !0x04;
            }
            if machine.cpu.pc == IDLE_TRAP && !machine.interrupt_waiting() {
                // Nothing runs between the tune's interrupts: time jumps to the next one (or to the end of this stretch).
                machine.idle_to(machine.next_event_cycle().min(target));
            }
            if machine.take_interrupt().is_some() {
                continue;
            }
            // An interrupt the tune keeps masked leaves the host idling, not running the I/O area.
            if machine.cpu.pc == IDLE_TRAP {
                machine.idle_to(target);
                continue;
            }
            machine.step_cpu();
        }
    }
}
