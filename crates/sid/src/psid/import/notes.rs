//! The notes of a transcription: every gate-on of the trace becomes a note on
//! a row of the grid, or, when its row already has one (a retrigger inside the
//! row), part of that note's sound. Each note carries its frames as a program
//! relative to its row's first frame, what a GoatTracker instrument must play
//! for it. A port of `transcribe/notes.ts`.

use super::capture::{SID_EVENT_GATE_LOW, SID_EVENT_GATE_ON};
use super::envelope::{apply_envelope_frame, EnvelopeFrame, SidEnvelope};
use super::frames::{nearest_note, pitch_of, TraceFrames, VoiceFrames};
use super::grid::{row_length, row_of_frame, row_start, RowGrid};
use super::pitch::PitchPlan;
use super::util::{js_round, OrdMap};

/// Longest stretch of a note the instrument tables describe; the rest holds.
pub const MAX_PROGRAM_FRAMES: usize = 48;
/// A gate-off this close before the next note is the hard restart's, not the instrument's.
pub const MAX_HARD_RESTART_GAP: i64 = 8;

#[derive(Clone, Debug)]
pub struct RowNote {
    pub voice: usize,
    pub row: i64,
    /// The frame of the row's first frame (GoatTracker's tick 0).
    pub tick0: i64,
    /// The frame the original's gate goes on.
    pub onset: i64,
    /// The frame of the next note's tick 0 on this voice, or the trace's end.
    pub end: i64,
    /// Retriggers folded into this note.
    pub folded: i64,
    /// The note starts without a gate-on: the pitch moves on while the note sounds (a tie, a legato line).
    pub legato: bool,
}

/// One frame of a note's program, relative to its tick 0 (index 1 = the wave table's first row).
#[derive(Clone, Debug)]
pub struct ProgramFrame {
    /// The control byte (waveform, gate, sync, ring, test).
    pub ctrl: u8,
    /// Pitch relative to the note's base, in semitones (fractional); `None`: no frequency (0).
    pub pitch: Option<f64>,
    /// The pattern sets this frame's pitch (a glide, a bend: `pitch.rs`); the instrument leaves it.
    pub owned: bool,
    pub pw: u16,
    /// The (global) filter in that frame: cutoff high byte, `$D417` (resonance, routing), mode bits (LP 1, BP 2, HP 4).
    pub cutoff: u16,
    pub resonance: u8,
    pub mode: u8,
}

#[derive(Clone, Debug)]
pub struct PulseFrame {
    pub pw: u16,
    pub k: i64,
    pub row_length: i64,
    /// A legato note of the line starts on this frame (its tick 0): GoatTracker skips the pulse table's step there.
    pub note_start: bool,
}

#[derive(Clone, Debug)]
pub struct NoteProgram {
    pub note: RowNote,
    /// GoatTracker note index (0-95) the row plays: the pitch at the gate-on.
    pub base: i32,
    /// Envelope at the gate-on frame.
    pub ad: u8,
    pub sr: u8,
    /// The control byte on tick 0 (the frame before the note's own frames).
    pub tick0_ctrl: u8,
    /// How much closer the note's envelope comes to the original's (summed
    /// level difference over its start) when GoatTracker starts it with the
    /// gate on and the test bit on tick 0 (`$09`) rather than with the gate off
    /// there: positive favours `$09`. 0 for a legato note.
    pub test_start_gain: i64,
    /// Frames 1..n of the program: up to the hard-restart tail or `MAX_PROGRAM_FRAMES`.
    pub frames: Vec<ProgramFrame>,
    /// Frames from the last gate-on frame to the next note's gate-on, when the next note cut it (the hard-restart gap).
    pub gap_after: Option<i64>,
    /// The envelope was zeroed (a hard restart) in the frames before this note's gate-on.
    pub hard_restart_before: bool,
    /// Frames of gate-off right before this note's gate-on.
    pub gap_before: i64,
    /// This note's voice drives the filter (`filter_driver`): its instrument plays the filter's program.
    pub filter: bool,
    /// The frame (relative to tick 0) the note's gate went off for good, when
    /// that is later than its first few frames: a key-off in the pattern, not
    /// part of the instrument (its frames are kept gated from there).
    pub key_off: Option<i64>,
    /// Frames of the held line this note starts (the legato notes after it, `split_lines`); 0 for none.
    pub line_frames: i64,
    /// The pulse width from frame 1 on, up to the next note (at most
    /// `MAX_PULSE_FRAMES`), with each frame's place in its row.
    pub pulse: Vec<PulseFrame>,
    /// For a note that drives the filter (`filter`): the cutoff's high byte per frame from the wave
    /// table's first row on, up to the next note and `MAX_FILTER_FRAMES`. Set with `filter`.
    pub filter_track: Option<Vec<i64>>,
}

/// Longest stretch of a note's cutoff the filter table follows.
pub const MAX_FILTER_FRAMES: i64 = 400;
/// Longest stretch of a note's pulse width the pulse table follows (a slow sweep's whole cycle).
pub const MAX_PULSE_FRAMES: i64 = 256;

/// A gate-off within this many frames of the gate going on is the instrument's (a pluck), not a key-off.
const EARLY_GATE_OFF: usize = 4;
/// The gate timer a key-off is expected to be read with.
const TYPICAL_GATE_TIMER: i64 = 2;

/// The voice whose notes drive the filter, or `None`: routed through it in most
/// of its gated frames, and the cutoff moves as its notes start (within the
/// two frames after the gate goes on) in most of them.
pub fn filter_driver(f: &TraceFrames, onsets: &[Vec<i64>]) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_score = 0.3;
    for v in 0..3 {
        let vf = &f.voices[v];
        let mut gated = 0;
        let mut routed = 0;
        for i in 0..f.frames {
            if vf.ctrl[i] & 1 == 0 {
                continue;
            }
            gated += 1;
            if f.resonance[i] & (1 << v) != 0 {
                routed += 1;
            }
        }
        let list = &onsets[v];
        if gated == 0 || list.is_empty() {
            continue;
        }
        let mut restarts = 0;
        for &o in list {
            let before = f.cutoff[(o - 1).max(0) as usize] as i64;
            let hi = (f.frames as i64 - 1).min(o + 2);
            let mut k = o;
            while k <= hi {
                if (f.cutoff[k as usize] as i64 - before).abs() >= 16 {
                    restarts += 1;
                    break;
                }
                k += 1;
            }
        }
        let score = (routed as f64 / gated as f64) * (restarts as f64 / list.len() as f64);
        if score > best_score {
            best = Some(v);
            best_score = score;
        }
    }
    best
}

/// Per voice, the frames where the gate goes on.
pub fn onsets_of(f: &TraceFrames) -> Vec<Vec<i64>> {
    f.voices
        .iter()
        .map(|v| {
            (0..f.frames)
                .filter(|&i| v.events[i] & SID_EVENT_GATE_ON != 0)
                .map(|i| i as i64)
                .collect()
        })
        .collect()
}

/// The row nearest to `frame` (as a note's tick 0).
fn nearest_row_to(g: &RowGrid, frame: i64) -> i64 {
    let k = row_of_frame(g, frame);
    let here = row_start(g, k);
    let next = row_start(g, k + 1);
    if frame - here <= next - frame {
        k
    } else {
        k + 1
    }
}

/// Per voice, the notes on rows; a gate-on whose row already has a note is folded into the note sounding then.
pub fn place_notes(f: &TraceFrames, g: &RowGrid, onsets: &[Vec<i64>]) -> Vec<Vec<RowNote>> {
    onsets
        .iter()
        .enumerate()
        .map(|(voice, list)| {
            let mut notes: Vec<RowNote> = Vec::new();
            let delay = g.delays.get(voice).copied().unwrap_or(0);
            for &onset in list {
                let k = nearest_row_to(g, onset - g.lead - delay).max(0);
                if let Some(last) = notes.last_mut() {
                    if last.row >= k {
                        last.folded += 1;
                        continue;
                    }
                }
                notes.push(RowNote {
                    voice,
                    row: k,
                    tick0: row_start(g, k),
                    onset,
                    end: f.frames as i64,
                    folded: 0,
                    legato: false,
                });
            }
            for i in 0..notes.len().saturating_sub(1) {
                notes[i].end = notes[i + 1].tick0;
            }
            notes
        })
        .collect()
}

/// A held row plays notes further apart than this: no tie (one note a row) can play it.
const LINE_SPREAD: i32 = 2;
/// A frame this close to a whole semitone plays that note.
const ON_NOTE: f64 = 0.15;

/// A trace value at `i`, 0 where the index is out of range (JavaScript's `undefined & x`).
#[inline]
fn at8(v: &[u8], i: i64) -> u8 {
    if i < 0 {
        0
    } else {
        v.get(i as usize).copied().unwrap_or(0)
    }
}

/// Per voice, the notes with every held line split where a tie cannot play it:
/// a row, under a gate that stays on, that plays several notes further apart
/// than `LINE_SPREAD`, moving at least twice (Hubbard's bass: three notes to a
/// row, each an octave down on its first frame) and not the ones the row
/// before played (a chord's arpeggio goes on as it is) starts a legato note,
/// which plays its row from its own wave table. Without it the line stops
/// where the first note's program does (`MAX_PROGRAM_FRAMES`).
pub fn split_lines(
    f: &TraceFrames,
    g: &RowGrid,
    voices: &[Vec<RowNote>],
    tuning: f64,
) -> Vec<Vec<RowNote>> {
    voices
        .iter()
        .enumerate()
        .map(|(voice, notes)| {
            let vf = &f.voices[voice];
            let delay = g.delays.get(voice).copied().unwrap_or(0);
            // A row's notes (sorted, distinct) and whether it is a held line's: every frame gated and on a note.
            let row_notes = |s: i64, e: i64| -> (Vec<i32>, bool) {
                let mut ns: Vec<i32> = Vec::new();
                let mut line = e - s >= 3;
                let mut moves = 0;
                for i in s..e {
                    let c = at8(&vf.ctrl, i);
                    let p = pitch_of(vf.freq.get(i as usize).copied().unwrap_or(0), f.clock_hz)
                        - tuning;
                    let on = (c & 1) != 0
                        && (c & 0xf0) != 0
                        && (c & 0x88) == 0
                        && p.is_finite()
                        && (p - js_round(p)).abs() <= ON_NOTE;
                    if on {
                        let r = js_round(p) as i32;
                        if let Some(&last) = ns.last() {
                            if last != r {
                                moves += 1;
                            }
                        }
                        ns.push(r);
                    } else {
                        line = false;
                    }
                }
                ns.sort_unstable();
                ns.dedup();
                let spread = match (ns.first(), ns.last()) {
                    (Some(a), Some(b)) => b - a > LINE_SPREAD,
                    _ => false,
                };
                (ns, line && moves >= 2 && spread)
            };
            let mut out: Vec<RowNote> = Vec::new();
            for note in notes {
                out.push(note.clone());
                let end = note.end;
                let mut prev: Option<Vec<i32>> = None;
                let last_row = row_of_frame(g, end - 1 - delay);
                let mut r = note.row;
                while r <= last_row {
                    let tick0 = row_start(g, r);
                    let s = note.onset.max(tick0 + g.lead + delay);
                    let e = end.min(row_start(g, r + 1) + g.lead + delay);
                    if e <= s {
                        r += 1;
                        continue;
                    }
                    let (set, line) = row_notes(s, e);
                    if r > note.row && line && prev.as_ref() != Some(&set) {
                        out.last_mut().unwrap().end = tick0;
                        out.push(RowNote {
                            voice,
                            row: r,
                            tick0,
                            onset: tick0 + g.lead + delay,
                            end,
                            folded: 0,
                            legato: true,
                        });
                    }
                    prev = Some(set);
                    r += 1;
                }
            }
            out
        })
        .collect()
}

/// Frames of gate-off right before `frame` on voice `v` (up to `max`).
fn gate_off_run(f: &TraceFrames, v: usize, frame: i64, max: i64) -> i64 {
    let ctrl = &f.voices[v].ctrl;
    let mut n = 0;
    while n < max && frame - n - 1 >= 0 && at8(ctrl, frame - n - 1) & 1 == 0 {
        n += 1;
    }
    n
}

/// The note a note is on: the pitch its gated frames hold most (a grace note
/// or a slide into it is then relative to it), but the pitch it starts on when
/// that is nearly as common (an arpeggio's first note).
fn base_note(f: &TraceFrames, voice: usize, from: i64, to: i64, tuning: f64) -> i32 {
    let vf = &f.voices[voice];
    // Frames that sound a pitch: a waveform on and the test bit clear. A note's own test frame says
    // nothing about its pitch (its frequency is left over), a release frame does.
    let mut counts: OrdMap<i32, i64> = OrdMap::new();
    let mut first = 0;
    let mut seen = false;
    let mut i = from;
    while i < (from + 1).max(to) {
        let c = at8(&vf.ctrl, i);
        let idx = i;
        i += 1;
        if (c & 0xf0) == 0 || (c & 0x08) != 0 {
            continue;
        }
        let p = pitch_of(vf.freq.get(idx as usize).copied().unwrap_or(0), f.clock_hz) - tuning;
        if !p.is_finite() {
            continue;
        }
        let n = nearest_note(p);
        *counts.entry_or_insert_with(n, || 0) += 1;
        if !seen {
            first = n;
            seen = true;
        }
    }
    let mut best = first;
    let mut best_count = counts.get(&first).copied().unwrap_or(0);
    for &(n, c) in counts.iter() {
        if c > best_count {
            best = n;
            best_count = c;
        }
    }
    if counts.get(&first).copied().unwrap_or(0) as f64 >= 0.6 * best_count as f64 {
        first
    } else {
        best
    }
}

/// The note a note's row plays (`base_note` over its first row, at least 4
/// frames, at most 24): what its first row holds, not where a later tie or
/// glide takes it.
pub fn note_base(f: &TraceFrames, g: &RowGrid, note: &RowNote, tuning: f64) -> i32 {
    let onset = note.onset.clamp(0, f.frames as i64 - 1);
    let first_row_end =
        row_start(g, note.row + 1) + g.lead + g.delays.get(note.voice).copied().unwrap_or(0);
    let to = note.end.min(onset + 24).min(first_row_end.max(onset + 4));
    base_note(f, note.voice, onset, to, tuning)
}

/// Frames after a note's gate-on over which its two possible starts are weighed.
const START_WINDOW: i64 = 16;

/// Voice frame `i` as the envelope takes it.
fn envelope_frame(vf: &VoiceFrames, i: usize) -> EnvelopeFrame {
    EnvelopeFrame {
        ad: vf.ad[i],
        sr: vf.sr[i],
        ctrl: vf.ctrl[i],
        gate_low: vf.events[i] & SID_EVENT_GATE_LOW != 0,
    }
}

struct StartContext<'a> {
    vf: &'a VoiceFrames,
    /// The original's envelope level per frame.
    levels: &'a [u8],
    cycles: i64,
}

/// The summed difference from the original's envelope levels over frames
/// [`from`, `to`) of a note GoatTracker plays: gate off from `from` (its gate
/// timer; with a hard restart, attack/decay `$0F` and sustain/release `$00`,
/// else the envelope the original had), on tick 0 the note's envelope with the
/// gate on (`test`, `$09`) or off, then the note's frames. `state`: the
/// original's envelope as frame `from` starts.
fn start_error(
    ctx: &StartContext,
    state: &SidEnvelope,
    p: &NoteProgram,
    from: i64,
    to: i64,
    test: bool,
) -> i64 {
    let mut env = state.clone();
    let tick0 = p.note.tick0;
    let mut err = 0;
    for i in from..to {
        let frame = if i < tick0 {
            if p.hard_restart_before {
                EnvelopeFrame {
                    ad: 0x0f,
                    sr: 0x00,
                    ctrl: 0,
                    gate_low: true,
                }
            } else {
                EnvelopeFrame {
                    ctrl: 0,
                    gate_low: true,
                    ..envelope_frame(ctx.vf, i as usize)
                }
            }
        } else if i == tick0 {
            EnvelopeFrame {
                ad: p.ad,
                sr: p.sr,
                ctrl: if test { 1 } else { 0 },
                gate_low: false,
            }
        } else {
            let k = (i - tick0 - 1) as usize;
            EnvelopeFrame {
                ad: p.ad,
                sr: p.sr,
                ctrl: if k < p.frames.len() {
                    p.frames[k].ctrl
                } else {
                    ctx.vf.ctrl[i as usize]
                },
                gate_low: false,
            }
        };
        apply_envelope_frame(&mut env, &frame, ctx.cycles);
        err += (env.level - ctx.levels[i as usize] as i64).abs();
    }
    err
}

/// The program of every note of `notes` (one voice). `tuning`: the tune's
/// offset from GoatTracker's note table (`estimate_tuning`), taken off every
/// pitch before it is snapped to a note.
pub fn note_programs(
    f: &TraceFrames,
    notes: &[RowNote],
    tuning: f64,
    g: &RowGrid,
    plan: Option<&PitchPlan>,
) -> Vec<NoteProgram> {
    let mut out: Vec<NoteProgram> = Vec::new();
    if notes.is_empty() {
        return out;
    }
    let nframes = f.frames as i64;
    // The original's envelope, and its state where each note's start is weighed from: where
    // its gate went off before the note (GoatTracker's gate timer turns it off there too).
    let vf0 = &f.voices[notes[0].voice];
    let cycles = js_round(f.clock_hz / f.rate_hz) as i64;
    let clamp = |i: i64| -> i64 { i.clamp(0, nframes - 1) };
    let start_from: Vec<i64> =
        notes
            .iter()
            .map(|note| {
                let onset = clamp(note.onset);
                0i64.max(note.tick0.min(
                    onset - 1i64.max(gate_off_run(f, note.voice, onset, MAX_HARD_RESTART_GAP)),
                ))
            })
            .collect();
    let mut wanted = vec![false; f.frames];
    for &s in &start_from {
        wanted[s as usize] = true;
    }
    let mut states: Vec<Option<SidEnvelope>> = vec![None; f.frames];
    let mut levels = vec![0u8; f.frames];
    let mut env = SidEnvelope::new();
    for i in 0..f.frames {
        if wanted[i] {
            states[i] = Some(env.clone());
        }
        apply_envelope_frame(&mut env, &envelope_frame(vf0, i), cycles);
        levels[i] = env.level as u8;
    }
    let ctx = StartContext {
        vf: vf0,
        levels: &levels,
        cycles,
    };
    for (n, note) in notes.iter().enumerate() {
        let vf = &f.voices[note.voice];
        let at = clamp;
        let onset = at(note.onset);
        let base = note_base(f, g, note, tuning);
        let next = notes.get(n + 1);
        // The hard-restart tail: gate off in the frames before the next note's gate-on.
        let gap_after =
            next.map(|nx| gate_off_run(f, note.voice, nx.onset, MAX_HARD_RESTART_GAP + 1));
        let tail_start = match (next, gap_after) {
            (Some(nx), Some(gap)) if gap <= MAX_HARD_RESTART_GAP => nx.onset - gap,
            _ => note.end,
        };
        // The note's own frames: tick 0 + 1 up to the tail (a quantized early gate-on shifts to tick 0 + 1).
        let lead = g.lead;
        let shift = 0i64.max(note.tick0 + lead - note.onset);
        let count = (MAX_PROGRAM_FRAMES as i64).min(1i64.max(tail_start - note.tick0 - 1 + shift));
        let mut frames: Vec<ProgramFrame> = Vec::new();
        for i in 1..=count {
            let src = at(note.tick0 + i - shift) as usize;
            let p = pitch_of(vf.freq[src], f.clock_hz) - tuning;
            // After a tie or a glide the wave table counts from the note the pattern set.
            let rel = match plan {
                Some(pl) if pl.base[src] >= 0 => pl.base[src] as f64,
                _ => base as f64,
            };
            frames.push(ProgramFrame {
                ctrl: vf.ctrl[src],
                pitch: if p.is_finite() { Some(p - rel) } else { None },
                // The first frame sets the note (the wave table's first row), whatever moves it after.
                owned: plan.is_some_and(|pl| pl.owned[src] != 0) && i > 1,
                pw: vf.pw[src],
                cutoff: f.cutoff[src] >> 3,
                resonance: f.resonance[src],
                mode: (f.mode_volume[src] >> 4) & 7,
            });
        }
        // A late gate-off that holds to the end: a key-off; the instrument sustains.
        let mut key_off: Option<i64> = None;
        // A key-off is a row of the pattern (`subsong_rows`): one that would land on the note's own row (a
        // gate of a few frames in rows of 32) cannot be written there, and is the instrument's gate-off.
        let placeable = |rel: i64| -> bool {
            let when = note.tick0 + rel + TYPICAL_GATE_TIMER;
            let k = row_of_frame(g, when);
            (if when - row_start(g, k) <= row_start(g, k + 1) - when {
                k
            } else {
                k + 1
            }) > note.row
        };
        if let Some(on) = frames.iter().position(|x| x.ctrl & 1 != 0) {
            let mut j = frames.len();
            while j > 0 && frames[j - 1].ctrl & 1 == 0 {
                j -= 1;
            }
            let held_on = frames[on..j].iter().all(|x| x.ctrl & 1 != 0);
            if held_on && j < frames.len() && j - on > EARLY_GATE_OFF && placeable(j as i64 + 1) {
                key_off = Some(j as i64 + 1);
                for fr in frames.iter_mut().skip(j) {
                    fr.ctrl |= 1;
                }
            } else if held_on && j == frames.len() && count == MAX_PROGRAM_FRAMES as i64 {
                // Held past the frames the instrument describes: the gate-off, if any, is further on
                // (a long note, or a legato line of ties); a key-off where it holds to the note's end.
                let mut off: i64 = -1;
                let mut src = note.tick0 + count + 1 - shift;
                while src < tail_start {
                    if at8(&vf.ctrl, at(src)) & 1 == 0 {
                        if off < 0 {
                            off = src;
                        }
                    } else {
                        off = -1;
                    }
                    src += 1;
                }
                if off >= 0 && placeable(off - note.tick0 + shift) {
                    key_off = Some(off - note.tick0 + shift);
                }
            }
        }
        let gap_before = gate_off_run(f, note.voice, onset, 40);
        let mut hard_restart_before = false;
        for k in 1..=gap_before.min(MAX_HARD_RESTART_GAP) {
            let i = onset - k;
            if i >= 0 && vf.ad[i as usize] == 0 && (vf.sr[i as usize] & 0x0f) == 0 {
                hard_restart_before = true;
            }
        }
        // The width as the note goes on, each frame's place in its row.
        let mut pulse: Vec<PulseFrame> = Vec::new();
        // Through a held line's legato notes: their instruments leave the pulse table running.
        let rest = &notes[n + 1..];
        let cut_at = rest.iter().position(|x| !x.legato);
        let line = &rest[..cut_at.unwrap_or(rest.len())];
        let cut = cut_at.map(|k| &rest[k]);
        let pulse_end = nframes
            .min(cut.map_or(nframes, |c| c.onset))
            .min(note.tick0 + 1 - shift + MAX_PULSE_FRAMES);
        let mut src = 0i64.max(note.tick0 + 1 - shift);
        while src < pulse_end {
            let t = src + shift;
            let r = row_of_frame(g, t);
            pulse.push(PulseFrame {
                pw: vf.pw[src as usize] & 0xfff,
                k: t - row_start(g, r),
                row_length: row_length(g, r),
                note_start: line.iter().any(|x| x.tick0 == t),
            });
            src += 1;
        }
        let mut program = NoteProgram {
            note: note.clone(),
            base,
            pulse,
            ad: vf.ad[onset as usize],
            sr: vf.sr[onset as usize],
            tick0_ctrl: vf.ctrl[at(note.tick0) as usize],
            frames,
            gap_after,
            hard_restart_before,
            gap_before,
            key_off,
            line_frames: if note.legato || line.is_empty() {
                0
            } else {
                line[line.len() - 1].end - line[0].tick0
            },
            filter: false,
            filter_track: None,
            test_start_gain: 0,
        };
        if !note.legato {
            let from = start_from[n];
            let to = nframes
                .min(onset + START_WINDOW)
                .min(next.map_or(i64::MAX, |_| start_from[n + 1]));
            let state = states[from as usize].as_ref().unwrap();
            program.test_start_gain = start_error(&ctx, state, &program, from, to, false)
                - start_error(&ctx, state, &program, from, to, true);
        }
        out.push(program);
    }
    out
}
