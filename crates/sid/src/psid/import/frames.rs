//! The register trace as the transcriber reads it: per voice per frame, the
//! frequency, pulse width, control byte, envelope and the in-frame gate
//! events; the filter and volume per frame. A port of `transcribe/frames.ts`.
//!
//! A "frame" here is one player tick at the rate the music really changes: a
//! player called more often than it writes (Defender of the Crown's is called
//! at ~394 Hz and writes on every 4th call) is decimated to the calls that
//! write, so a frame is a player step, what a GoatTracker tick becomes.

use super::capture::{SidTrace, SID_EVENT_GATE_ON, SID_TRACE_REGS};
use super::util::js_round;
use crate::gt_note_freq_reg;

#[derive(Clone, Debug, Default)]
pub struct VoiceFrames {
    pub freq: Vec<u16>,
    pub pw: Vec<u16>,
    pub ctrl: Vec<u8>,
    pub ad: Vec<u8>,
    pub sr: Vec<u8>,
    /// `SID_EVENT_*` bits of the frame.
    pub events: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct TraceFrames {
    pub frames: usize,
    pub voices: Vec<VoiceFrames>,
    /// 11-bit cutoff.
    pub cutoff: Vec<u16>,
    /// `$D417`: resonance << 4 | routing.
    pub resonance: Vec<u8>,
    /// `$D418`: mode << 4 | volume.
    pub mode_volume: Vec<u8>,
    /// Frames per second.
    pub rate_hz: f64,
    /// The clock the frequency registers count in (Hz).
    pub clock_hz: f64,
    /// Trace ticks per frame (1, or the decimation).
    pub decimation: usize,
    /// The frame the music continues with after the last (an exact loop), or `None`.
    pub loop_frame: Option<usize>,
}

/// A decimated player still steps at least this often: a 50 Hz tune that writes on even frames only is still 50 Hz.
const MIN_DECIMATED_HZ: f64 = 45.0;

/// The ticks-per-frame divisor at which every register change of the trace
/// falls on one residue, or 1; never below `MIN_DECIMATED_HZ` (the rates
/// GoatTracker plays start at the video frame's). Returns `(step, phase)`.
pub fn trace_decimation(trace: &SidTrace) -> (usize, usize) {
    let max_step = (16.0f64)
        .min((trace.clock_hz / trace.tick_cycles as f64 / MIN_DECIMATED_HZ).floor())
        as usize;
    if max_step < 2 {
        return (1, 0);
    }
    let regs = &trace.regs;
    let mut changed: Vec<usize> = Vec::new();
    for t in 1..trace.ticks {
        let a = (t - 1) * SID_TRACE_REGS;
        let b = t * SID_TRACE_REGS;
        if regs[a..a + SID_TRACE_REGS] != regs[b..b + SID_TRACE_REGS] {
            changed.push(t);
        }
    }
    if changed.len() < 8 {
        return (1, 0);
    }
    for step in (2..=max_step).rev() {
        let phase = changed[0] % step;
        if changed.iter().all(|t| t % step == phase) {
            return (step, phase);
        }
    }
    (1, 0)
}

/// `trace` as frames: decimated where the player writes on every n-th call only.
pub fn trace_frames(trace: &SidTrace) -> TraceFrames {
    let (step, phase) = trace_decimation(trace);
    let mut picks: Vec<usize> = Vec::new();
    let mut t = if step == 1 { 0 } else { phase };
    while t < trace.ticks {
        picks.push(t);
        t += step;
    }
    let n = picks.len();
    let mut voices = Vec::with_capacity(3);
    for v in 0..3 {
        let o = v * 7;
        let mut vf = VoiceFrames {
            freq: vec![0; n],
            pw: vec![0; n],
            ctrl: vec![0; n],
            ad: vec![0; n],
            sr: vec![0; n],
            events: vec![0; n],
        };
        for (i, &t) in picks.iter().enumerate() {
            let r = t * SID_TRACE_REGS + o;
            let g = &trace.regs;
            vf.freq[i] = g[r] as u16 | ((g[r + 1] as u16) << 8);
            vf.pw[i] = (g[r + 2] as u16 | ((g[r + 3] as u16) << 8)) & 0xfff;
            vf.ctrl[i] = g[r + 4];
            vf.ad[i] = g[r + 5];
            vf.sr[i] = g[r + 6];
            // A decimated frame carries the events of the calls it stands for.
            let mut e = 0;
            let from = (t + 1).saturating_sub(step);
            for k in from..=t {
                e |= trace.voice_events[k * 3 + v];
            }
            vf.events[i] = e;
        }
        voices.push(vf);
    }
    let mut cutoff = vec![0u16; n];
    let mut resonance = vec![0u8; n];
    let mut mode_volume = vec![0u8; n];
    for (i, &t) in picks.iter().enumerate() {
        let r = t * SID_TRACE_REGS;
        let g = &trace.regs;
        cutoff[i] = (g[r + 0x15] & 7) as u16 | ((g[r + 0x16] as u16) << 3);
        resonance[i] = g[r + 0x17];
        mode_volume[i] = g[r + 0x18];
    }
    let loop_frame = trace.loop_tick.map(|lt| {
        let off = if step == 1 { 0 } else { phase } as f64;
        let x = js_round((lt as f64 - off) / step as f64);
        x.min((n as f64) - 1.0).max(0.0) as usize
    });
    TraceFrames {
        frames: n,
        voices,
        cutoff,
        resonance,
        mode_volume,
        rate_hz: trace.clock_hz / (trace.tick_cycles as f64 * step as f64),
        clock_hz: trace.clock_hz,
        decimation: step,
        loop_frame,
    }
}

impl TraceFrames {
    /// Whether voice `v` starts a note (a gate-on edge) in frame `i`.
    #[inline]
    pub fn gate_on_at(&self, v: usize, i: usize) -> bool {
        self.voices[v].events[i] & SID_EVENT_GATE_ON != 0
    }
}

// ---------------------------------------------------------------------------
// Pitch
// ---------------------------------------------------------------------------

/// The PAL clock GoatTracker's exports run at, and its note table's reference.
const PAL_HZ: f64 = 985_248.0;

/// GoatTracker's A-4 register (index 57), unrounded, on its table's own clock.
fn gt_a4() -> f64 {
    (440.0 * 16_777_216.0) / 985_000.0
}

/// GoatTracker's note table: index 0 (C-0) to 95 (B-7).
pub fn gt_note_regs() -> [u16; 96] {
    let mut out = [0u16; 96];
    for (i, r) in out.iter_mut().enumerate() {
        *r = gt_note_freq_reg(i as u8);
    }
    out
}

/// The pitch of frequency register `freq` (counting at `clock_hz`) in
/// GoatTracker note units: 0 = C-0, 57 = A-4, fractional between notes, as the
/// note GoatTracker's PAL table plays nearest to it. `-inf` for 0.
pub fn pitch_of(freq: u16, clock_hz: f64) -> f64 {
    if freq == 0 {
        return f64::NEG_INFINITY;
    }
    let pal = (freq as f64 * clock_hz) / PAL_HZ;
    57.0 + 12.0 * (pal / gt_a4()).log2()
}

/// The nearest note index of GoatTracker's table (0-95) for a pitch.
pub fn nearest_note(pitch: f64) -> i32 {
    js_round(pitch).clamp(0.0, 95.0) as i32
}

/// How far the tune's own note table sits from GoatTracker's, in semitones
/// (-0.5..0.5): the most common fractional part of the pitches its voices
/// hold while gated (Galway's table is about 26 cents flat of GT's). Pitches
/// are snapped to notes after taking it off; the song then plays in GT's
/// tuning, which is the one thing a GoatTracker song cannot change.
pub fn estimate_tuning(f: &TraceFrames) -> f64 {
    let mut bins = [0i64; 100];
    for v in &f.voices {
        for i in 1..f.frames {
            // A pitch held for two frames with the gate on: a note, not a slide.
            if v.ctrl[i] & 1 == 0 || v.freq[i] != v.freq[i - 1] || v.freq[i] == 0 {
                continue;
            }
            let p = pitch_of(v.freq[i], f.clock_hz);
            let frac = p - js_round(p);
            let b = ((frac + 0.5) * 100.0).floor().clamp(0.0, 99.0) as usize;
            bins[b] += 1;
        }
    }
    // The densest 9-cent window (circular), its weighted centre.
    let mut best = 0usize;
    let mut best_sum: i64 = -1;
    for c in 0..100usize {
        let mut sum = 0;
        for k in -4i32..=4 {
            sum += bins[((c as i32 + k + 100) % 100) as usize];
        }
        if sum > best_sum {
            best = c;
            best_sum = sum;
        }
    }
    if best_sum <= 0 {
        return 0.0;
    }
    let mut acc = 0i64;
    for k in -4i32..=4 {
        acc += k as i64 * bins[((best as i32 + k + 100) % 100) as usize];
    }
    let centre = (best as f64 + acc as f64 / best_sum as f64 + 0.5) / 100.0 - 0.5;
    if centre > 0.5 {
        centre - 1.0
    } else if centre < -0.5 {
        centre + 1.0
    } else {
        centre
    }
}
