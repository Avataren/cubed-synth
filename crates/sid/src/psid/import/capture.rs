//! Runs one subsong of a `.sid` on the emulated C64 and records what its
//! player does to the SID, tick by tick: the register trace every later step
//! reads. A port of another_synth's `psid/sid-capture.ts`.
//!
//! How the tune is driven, as HVSC's SID file environment says:
//!  - **PSID with a play address** ([`CaptureMode::Play`]): `$01` is set for
//!    the call address (`$37` below `$A000`, `$36` to `$CFFF`, `$34` in
//!    `$D000-$DFFF`, `$35` from `$E000`), init is called with the subsong in
//!    A, then play once per tick with interrupts off: per video frame (VBI) or
//!    at CIA 1 timer A's rate as init left it (speed bit set). A play routine
//!    that leaves through the KERNAL's interrupt exit or with an RTI ends its
//!    call there.
//!  - **RSID, or a play address of 0** ([`CaptureMode::Irq`]): the C64 as the
//!    KERNAL leaves it, init is called, then the CPU idles with interrupts on
//!    and the tune's own interrupts run. A tick is one IRQ handler run that
//!    writes the SID. A tune whose player runs outside interrupts (a main
//!    loop) is sampled once per video frame instead ([`CaptureMode::Frame`]).
//!
//! The capture ends when the machine's state (RAM, I/O written) is one it had
//! after an earlier tick: from there the music repeats exactly, and
//! `loop_tick` says where. Otherwise it ends at the tick or cycle cap.
//!
//! The one difference from the DAW's: the SID writes come out of the
//! machine's write queue after every few thousand cycles, not through a
//! callback, so a tune that writes the SID millions of times in its init never
//! queues them all.

use super::super::c64::{C64Clock, Kernal, SidWrite, Taken, C64, KERNAL_BRK_HANDLER, NEVER};
use super::super::file::{PsidClock, PsidFile, PsidType};
use super::super::runner::{
    enter_call, psid_banks_for, run_call, CallEnd, IDLE_TRAP, INIT_MAX_CYCLES, PLAY_MAX_CYCLES,
    RETURN_TRAP,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Per voice per tick, `SidTrace::voice_events` bits: a 0 -> 1 gate edge.
pub const SID_EVENT_GATE_ON: u8 = 1;
/// The gate written 0.
pub const SID_EVENT_GATE_LOW: u8 = 2;
/// The test bit written 1.
pub const SID_EVENT_TEST: u8 = 4;
/// The control register written.
pub const SID_EVENT_CTRL_WRITTEN: u8 = 8;

/// Registers a tick records: `$D400-$D418`.
pub const SID_TRACE_REGS: usize = 25;

/// Seconds without a SID-writing interrupt that end an interrupt-driven capture.
const SILENCE_SECONDS: u64 = 3;

/// How a tune was driven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    Play,
    Irq,
    Frame,
}

/// Why a capture stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureEnd {
    Loop,
    Ticks,
    Cycles,
    Silent,
    Jam,
    Brk,
}

/// What a capture records.
#[derive(Clone, Debug)]
pub struct SidTrace {
    pub clock: C64Clock,
    pub mode: CaptureMode,
    /// CPU cycles per second of the machine that ran it.
    pub clock_hz: f64,
    /// Cycles of one video frame.
    pub frame_cycles: u64,
    /// Cycles between ticks: the median.
    pub tick_cycles: u64,
    pub ticks: usize,
    /// `ticks` x 25: the first SID's registers after each tick.
    pub regs: Vec<u8>,
    /// `ticks` x 3: per voice, what happened to its control register inside the tick (`SID_EVENT_*`).
    pub voice_events: Vec<u8>,
    /// The tick the music continues with after the last (an exact repeat), or `None` (cut at a cap).
    pub loop_tick: Option<usize>,
    /// SID writes per tick, on average.
    pub writes_per_tick: f64,
    /// `$D418` writes from NMI handlers, or several per tick: sample playback.
    pub digi: bool,
    /// A second or third SID was written.
    pub extra_sid_writes: u64,
    /// Why the capture stopped.
    pub end: CaptureEnd,
}

/// What a capture is asked to do.
#[derive(Clone, Debug, Default)]
pub struct CaptureOptions {
    /// 0-based.
    pub subsong: usize,
    /// At most this many ticks (default: ten minutes' worth).
    pub max_ticks: Option<usize>,
    /// At most this much emulated time, in seconds (default 600).
    pub max_seconds: Option<f64>,
    /// Set from another thread to stop the capture (it ends with the reason [`CANCELLED`]).
    pub cancel: Option<Arc<AtomicBool>>,
}

/// The reason a capture, and an import, ends with when its `cancel` flag was set.
pub const CANCELLED: &str = "the import was cancelled";

pub(crate) fn cancelled(flag: &Option<Arc<AtomicBool>>) -> bool {
    flag.as_ref().is_some_and(|f| f.load(Ordering::Relaxed))
}

pub(crate) fn hex4(v: u16) -> String {
    format!("${v:04X}")
}

/// The recorder the machine's SID writes go through.
struct Recorder {
    regs: [u8; 32],
    ctrl: [u8; 3],
    events: [u8; 3],
    writes: u64,
    tick_writes: u64,
    volume_writes: u64,
    nmi_volume_writes: u64,
    extra_sid_writes: u64,
    /// The code running is an NMI handler's (the innermost interrupt is an NMI).
    in_nmi: bool,
}

impl Recorder {
    fn new() -> Recorder {
        Recorder {
            regs: [0; 32],
            ctrl: [0; 3],
            events: [0; 3],
            writes: 0,
            tick_writes: 0,
            volume_writes: 0,
            nmi_volume_writes: 0,
            extra_sid_writes: 0,
            in_nmi: false,
        }
    }

    fn write(&mut self, w: SidWrite) {
        if w.chip != 0 {
            self.extra_sid_writes += 1;
            return;
        }
        self.writes += 1;
        // An NMI's writes (samples) are not the tick's: a tick is the music's interrupt.
        if !self.in_nmi {
            self.tick_writes += 1;
        }
        let reg = w.reg as usize;
        let value = w.value;
        self.regs[reg & 31] = value;
        if reg == 0x18 {
            self.volume_writes += 1;
            if self.in_nmi {
                self.nmi_volume_writes += 1;
            }
        }
        if reg == 4 || reg == 11 || reg == 18 {
            let v = (reg - 4) / 7;
            let old = self.ctrl[v];
            let mut e = self.events[v] | SID_EVENT_CTRL_WRITTEN;
            if value & 1 != 0 && old & 1 == 0 {
                e |= SID_EVENT_GATE_ON;
            }
            if value & 1 == 0 {
                e |= SID_EVENT_GATE_LOW;
            }
            if value & 8 != 0 {
                e |= SID_EVENT_TEST;
            }
            self.events[v] = e;
            self.ctrl[v] = value;
        }
    }

    /// Takes every SID write the machine has queued.
    fn take(&mut self, machine: &mut C64) {
        while let Some(w) = machine.sys.writes.pop() {
            self.write(w);
        }
    }
}

/// Growable per-tick storage.
struct TickStore {
    regs: Vec<u8>,
    events: Vec<u8>,
    cycles: Vec<u64>,
    ticks: usize,
}

impl TickStore {
    fn new() -> TickStore {
        TickStore {
            regs: Vec::with_capacity(SID_TRACE_REGS * 1024),
            events: Vec::with_capacity(3 * 1024),
            cycles: Vec::new(),
            ticks: 0,
        }
    }

    fn push(&mut self, rec: &mut Recorder, cycle: u64) {
        self.regs.extend_from_slice(&rec.regs[..SID_TRACE_REGS]);
        self.events.extend_from_slice(&rec.events);
        rec.events = [0; 3];
        self.cycles.push(cycle);
        self.ticks += 1;
    }
}

/// A host JSR to `addr` with A = `a` and interrupts off, run until it returns
/// (or leaves the way an interrupt handler does), with the SID writes taken as it goes.
fn call_routine(
    machine: &mut C64,
    rec: &mut Recorder,
    addr: u16,
    a: u8,
    max_cycles: u64,
) -> CallEnd {
    enter_call(machine, addr, a);
    let limit = machine.cpu.cycles + max_cycles;
    loop {
        let slice = machine.cpu.cycles + 20_000;
        let end = run_call(machine, limit, slice);
        rec.take(machine);
        if let Some(end) = end {
            return end;
        }
    }
}

/// Whether the file's clock is NTSC (the DAW: anything else is PAL).
fn clock_of(file: &PsidFile) -> C64Clock {
    if file.clock == PsidClock::Ntsc {
        C64Clock::Ntsc
    } else {
        C64Clock::Pal
    }
}

/// Runs `file`'s subsong `options.subsong` and records its SID trace. Never panics for a tune's behaviour.
pub fn capture_sid(file: &PsidFile, options: &CaptureOptions) -> Result<SidTrace, String> {
    capture_sid_with(file, options, Kernal::default())
}

/// [`capture_sid`] on a machine with the stand-in KERNAL of your choice (`Kernal::Daw` is the DAW's own).
pub fn capture_sid_with(
    file: &PsidFile,
    options: &CaptureOptions,
    kernal: Kernal,
) -> Result<SidTrace, String> {
    let clock = clock_of(file);
    let mut machine = C64::with_kernal(clock, &file.extra_sids, kernal);
    machine.enable_state_hash();
    machine.load(file.load_address, &file.data);
    let mut rec = Recorder::new();
    let timing = machine.sys.timing;
    let frame_cycles = timing.frame_cycles();
    let hz = timing.hz as f64;
    let subsong = options.subsong.min(file.songs as usize - 1);
    let irq_mode = file.kind == PsidType::Rsid || file.play_address == 0;

    if !irq_mode {
        machine.set_banks(psid_banks_for(file.init_address));
        let end = call_routine(
            &mut machine,
            &mut rec,
            file.init_address,
            subsong as u8,
            INIT_MAX_CYCLES,
        );
        match end {
            CallEnd::Jam => {
                return Err(format!(
                    "the tune's init routine ({}) stopped the CPU (a JAM opcode at {})",
                    hex4(file.init_address),
                    hex4(machine.cpu.pc)
                ))
            }
            CallEnd::Brk => {
                return Err(format!(
                    "the tune's init routine ({}) hit a BRK",
                    hex4(file.init_address)
                ))
            }
            CallEnd::Timeout => {
                return Err(format!(
                    "the tune's init routine ({}) did not return in {} million cycles",
                    hex4(file.init_address),
                    INIT_MAX_CYCLES / 1_000_000
                ))
            }
            _ => {}
        }
    } else {
        if file.kind == PsidType::Psid {
            machine.set_banks(psid_banks_for(file.init_address));
        }
        enter_call(&mut machine, file.init_address, subsong as u8);
    }

    let mut store = TickStore::new();
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let max_cycles = options.max_seconds.unwrap_or(600.0) * hz;
    let mut loop_tick: Option<usize> = None;
    let mut end = CaptureEnd::Ticks;
    let mut mode = if irq_mode {
        CaptureMode::Irq
    } else {
        CaptureMode::Play
    };
    let tick_cycles;

    // Records the tick that ends at `cycle`; true when the machine is in a state it had before.
    fn end_tick(
        machine: &C64,
        rec: &mut Recorder,
        store: &mut TickStore,
        seen: &mut HashMap<u64, usize>,
        loop_tick: &mut Option<usize>,
        end: &mut CaptureEnd,
        cycle: u64,
    ) -> bool {
        store.push(rec, cycle);
        let h = machine.state_hash();
        if let Some(&earlier) = seen.get(&h) {
            *loop_tick = Some(earlier + 1);
            *end = CaptureEnd::Loop;
            return true;
        }
        seen.insert(h, store.ticks - 1);
        false
    }

    if !irq_mode {
        // Host-driven play calls.
        let cia = file.uses_cia(subsong as u16 + 1);
        tick_cycles = if cia {
            machine.sys.cia1.timer_a_latch() as u64 + 1
        } else {
            frame_cycles
        };
        let max_ticks = options
            .max_ticks
            .unwrap_or_else(|| ((600.0 * hz) / tick_cycles as f64).ceil() as usize);
        let mut next = machine.cpu.cycles;
        while store.ticks < max_ticks {
            if cancelled(&options.cancel) {
                return Err(CANCELLED.to_string());
            }
            machine.idle_to(next);
            machine.set_banks(psid_banks_for(file.play_address));
            rec.tick_writes = 0;
            let e = call_routine(
                &mut machine,
                &mut rec,
                file.play_address,
                0,
                PLAY_MAX_CYCLES,
            );
            if matches!(e, CallEnd::Jam | CallEnd::Brk | CallEnd::Timeout) {
                if store.ticks == 0 {
                    let why = match e {
                        CallEnd::Jam => {
                            format!("stopped the CPU (JAM at {})", hex4(machine.cpu.pc))
                        }
                        CallEnd::Brk => "hit a BRK".to_string(),
                        _ => "did not return".to_string(),
                    };
                    return Err(format!(
                        "the tune's play routine ({}) {}",
                        hex4(file.play_address),
                        why
                    ));
                }
                end = match e {
                    CallEnd::Timeout => CaptureEnd::Cycles,
                    CallEnd::Jam => CaptureEnd::Jam,
                    _ => CaptureEnd::Brk,
                };
                break;
            }
            if end_tick(
                &machine,
                &mut rec,
                &mut store,
                &mut seen,
                &mut loop_tick,
                &mut end,
                next,
            ) {
                break;
            }
            next += tick_cycles;
            if machine.cpu.cycles as f64 > max_cycles {
                end = CaptureEnd::Cycles;
                break;
            }
        }
    } else {
        let r = run_interrupt_driven(
            &mut machine,
            &mut rec,
            &mut store,
            &mut seen,
            &mut loop_tick,
            &mut end,
            options,
            frame_cycles,
            max_cycles,
        )?;
        mode = r.0;
        tick_cycles = r.1;
        if let Some(e) = r.2 {
            end = e;
        }
    }

    if store.ticks == 0 {
        return Err("the tune never wrote to the SID".to_string());
    }
    let ticks = store.ticks;
    let volume_per_tick = rec.volume_writes as f64 / ticks as f64;
    Ok(SidTrace {
        clock,
        mode,
        clock_hz: hz,
        frame_cycles,
        tick_cycles,
        ticks,
        regs: store.regs,
        voice_events: store.events,
        loop_tick,
        writes_per_tick: rec.writes as f64 / ticks as f64,
        digi: rec.nmi_volume_writes as usize > ticks || volume_per_tick > 4.0,
        extra_sid_writes: rec.extra_sid_writes,
        end,
    })
}

/// RSID / play address 0: the tune's own interrupts, from init's first
/// instruction on (init is running when this starts). When init returns, the
/// host turns interrupts on and idles at `IDLE_TRAP` (time jumps to the next
/// interrupt); a tune whose init never returns keeps running as the main
/// program. Returns the mode, the tick length and why it ended (if it did here).
#[allow(clippy::too_many_arguments)]
fn run_interrupt_driven(
    machine: &mut C64,
    rec: &mut Recorder,
    store: &mut TickStore,
    seen: &mut HashMap<u64, usize>,
    loop_tick: &mut Option<usize>,
    end_reason: &mut CaptureEnd,
    options: &CaptureOptions,
    frame_cycles: u64,
    max_cycles: f64,
) -> Result<(CaptureMode, u64, Option<CaptureEnd>), String> {
    let hz = machine.sys.timing.hz as f64;
    let max_ticks = options
        .max_ticks
        .unwrap_or_else(|| ((600.0 * hz) / frame_cycles as f64).ceil() as usize * 4);
    // The interrupts being handled, innermost last (an NMI can come inside the music's IRQ).
    let mut stack: Vec<Taken> = Vec::new();
    // When the outermost interrupt being handled was taken.
    let mut entered = machine.cpu.cycles;
    let mut irq_writes = 0u64;
    let mut main_writes = 0u64;
    let mut mode = CaptureMode::Irq;
    let mut last_frame_cut = machine.cpu.cycles;
    let start = machine.cpu.cycles;
    let mut end: Option<CaptureEnd> = None;
    // No SID-writing interrupt for this long after the last: the music has stopped.
    let silence = (SILENCE_SECONDS as f64 * hz) as u64;

    // The tick ends here: records it and reports a repeat.
    macro_rules! tick {
        ($cycle:expr) => {{
            store.push(rec, $cycle);
            let h = machine.state_hash();
            if let Some(&earlier) = seen.get(&h) {
                *loop_tick = Some(earlier + 1);
                *end_reason = CaptureEnd::Loop;
                true
            } else {
                seen.insert(h, store.ticks - 1);
                false
            }
        }};
    }

    let mut spins = 0u32;
    while store.ticks < max_ticks {
        spins = spins.wrapping_add(1);
        if spins & 0xfff == 0 && cancelled(&options.cancel) {
            return Err(CANCELLED.to_string());
        }
        if (machine.cpu.cycles - start) as f64 > max_cycles {
            end = Some(CaptureEnd::Cycles);
            break;
        }
        let last = if store.ticks > 0 {
            store.cycles[store.ticks - 1]
        } else {
            start
        };
        if machine.cpu.cycles.saturating_sub(last) > silence
            && (store.ticks > 0
                || (mode == CaptureMode::Irq && irq_writes == 0 && main_writes == 0))
        {
            end = Some(CaptureEnd::Silent);
            break;
        }
        if machine.cpu.jammed {
            end = Some(CaptureEnd::Jam);
            break;
        }
        if machine.cpu.pc == KERNAL_BRK_HANDLER && machine.banks() & 2 != 0 {
            end = Some(CaptureEnd::Brk);
            break;
        }
        if machine.cpu.pc == RETURN_TRAP && stack.is_empty() {
            // Init returned to the host: interrupts on, idle.
            machine.cpu.pc = IDLE_TRAP;
            machine.cpu.p &= !0x04;
        }
        // Idle between interrupts: nothing runs, time jumps (unless one is already waiting).
        if machine.cpu.pc == IDLE_TRAP && stack.is_empty() && !machine.interrupt_waiting() {
            let next = machine.next_event_cycle();
            if next == NEVER {
                end = Some(CaptureEnd::Silent);
                break;
            }
            if mode == CaptureMode::Frame {
                let cut = last_frame_cut + frame_cycles;
                if cut <= next {
                    machine.idle_to(cut);
                    last_frame_cut = cut;
                    if tick!(cut) {
                        break;
                    }
                    continue;
                }
            }
            machine.idle_to(next);
        }
        let taken = machine.take_interrupt();
        if let Some(taken) = taken {
            if stack.is_empty() {
                rec.tick_writes = 0;
                entered = machine.cpu.cycles;
            }
            stack.push(taken);
            rec.in_nmi = taken == Taken::Nmi;
            machine.set_hash_paused(rec.in_nmi);
            continue;
        }
        let op = machine.read(machine.cpu.pc);
        let before = rec.writes;
        machine.step_cpu();
        rec.take(machine);
        let wrote = rec.writes - before;
        if !stack.is_empty() {
            if !rec.in_nmi {
                irq_writes += wrote;
            }
            if op == 0x40 {
                let kind = stack.pop();
                rec.in_nmi = stack.last() == Some(&Taken::Nmi);
                machine.set_hash_paused(rec.in_nmi);
                // The tick is timed from the interrupt's start: its handler's length varies.
                if stack.is_empty()
                    && kind == Some(Taken::Irq)
                    && mode == CaptureMode::Irq
                    && rec.tick_writes > 0
                    && tick!(entered)
                {
                    break;
                }
            }
        } else {
            main_writes += wrote;
        }
        // A player that writes the SID outside interrupts: sample per frame.
        if mode == CaptureMode::Irq
            && store.ticks == 0
            && (machine.cpu.cycles - start) as f64 > 2.0 * hz
            && main_writes > 0
            && irq_writes == 0
        {
            mode = CaptureMode::Frame;
            last_frame_cut = machine.cpu.cycles;
        }
        if mode == CaptureMode::Frame
            && machine.cpu.cycles - last_frame_cut >= frame_cycles
            && stack.is_empty()
        {
            last_frame_cut += frame_cycles;
            if tick!(last_frame_cut) {
                break;
            }
        }
    }
    if store.ticks == 0 {
        return Err("the tune set up no interrupt that plays music, and wrote nothing to the SID from its main program".to_string());
    }
    let c = &store.cycles;
    let mut gaps: Vec<i64> = c.windows(2).map(|w| w[1] as i64 - w[0] as i64).collect();
    gaps.sort_unstable();
    let tick_cycles = if mode == CaptureMode::Frame {
        frame_cycles
    } else if !gaps.is_empty() {
        gaps[gaps.len() >> 1].max(0) as u64
    } else {
        frame_cycles
    };
    Ok((mode, tick_cycles, end))
}
