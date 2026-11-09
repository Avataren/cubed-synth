//! The pitch a note's pattern plays: what the original does to a sounding
//! note's pitch after it starts, as GoatTracker's pattern commands rather than
//! as more notes or instruments. A port of `transcribe/pitch.ts`.
//!
//!  - a glide that lands on a note: that note with tone portamento (`3xx`) on
//!    the row the glide starts, at the speed that arrives when the original
//!    does, the command held on the rows it spans;
//!  - a bend that does not land on a note (a fall, a slide into the next note,
//!    a long drop from the note's start): portamento up or down (`1xx`, `2xx`)
//!    on the rows it spans;
//!  - a pitch that moves to another note with no glide and no new gate (a
//!    legato line, a tie): that note with `3 00`, which moves the pitch
//!    without retriggering the note or restarting its instrument.
//!
//! A glide inside a note's first row (a drum's pitch drop, a slide into the
//! note) stays the instrument's, as its wave table plays it. The frames whose
//! pitch a pattern command sets are the pattern's, not the instrument's: the
//! note programs leave them free (`PitchPlan::owned`).
//!
//! GoatTracker's tick effects skip each row's first frame (its realtime
//! optimisation), so a speed is the distance over the frames that move, the
//! rows' first frames left out.

use super::frames::{gt_note_regs, nearest_note, pitch_of, TraceFrames};
use super::grid::{row_length, row_of_frame, row_start, RowGrid};
use super::notes::RowNote;
use super::util::js_round;
use super::vibrato::vibrato_for;
use std::collections::HashSet;

/// A pattern cell the pitch plan writes on a voice's row.
#[derive(Clone, Debug)]
pub struct PitchRow {
    pub row: i64,
    /// GoatTracker note index (0-95) to set, or `None` for the command alone.
    pub note: Option<i32>,
    /// 1 portamento up, 2 down, 3 tone portamento (speed 0: a tie), 4 vibrato (`vibrato`).
    pub command: u8,
    /// The speed-table row of a vibrato (`command` 4): left, right.
    pub vibrato: Option<(u8, u8)>,
    /// The speed in frequency-register units a moving frame (0 = a tie).
    pub speed: i64,
    /// A row that keeps an effect started on an earlier row running (it may give way to a tempo command).
    pub continued: bool,
}

#[derive(Clone, Debug)]
pub struct PitchPlan {
    pub rows: Vec<PitchRow>,
    /// Per frame of the trace: the pattern sets this frame's pitch (the instrument must not).
    pub owned: Vec<u8>,
    /// Per frame: the note the pattern last set (the base the instrument's wave table counts from), or -1 (the note's own).
    pub base: Vec<i16>,
}

/// A glide may start this many frames after its row's second tick and still be a pattern effect.
const GLIDE_START_SLACK: i64 = 3;
/// A glide moves at least this far (semitones).
const MIN_GLIDE: f64 = 0.6;
/// A glide's end pitch within this of a note lands on it.
const LANDS: f64 = 0.15;
/// Frame-to-frame changes below this are a held pitch.
const STILL: f64 = 0.02;

/// GoatTracker's register for note `n`, on the PAL clock its table plays at.
fn reg_of(regs: &[u16; 96], n: i32) -> f64 {
    regs[n.clamp(0, 95) as usize] as f64
}

/// The frames of the note starting at `note.onset` that sound a pitch: the waveform on, the test bit off, no noise.
fn pitch_series(f: &TraceFrames, voice: usize, from: i64, to: i64, tuning: f64) -> Vec<f64> {
    let vf = &f.voices[voice];
    let mut out = vec![f64::NAN; (to - from).max(0) as usize];
    for i in from..to {
        let c = vf.ctrl[i as usize];
        if c & 0xf0 == 0 || c & 0x08 != 0 || c & 0x80 != 0 {
            continue;
        }
        let p = pitch_of(vf.freq[i as usize], f.clock_hz) - tuning;
        if p.is_finite() {
            out[(i - from) as usize] = p;
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
struct Run {
    /// First moving frame and last (absolute).
    a: i64,
    b: i64,
    dir: i32,
    /// Pitch before the first moving frame and at the last.
    from: f64,
    to: f64,
}

fn sign(d: f64) -> i32 {
    if d > 0.0 {
        1
    } else if d < 0.0 {
        -1
    } else {
        0
    }
}

/// The monotonic runs of `p` (frames `base`..): consecutive moving frames that keep one direction.
fn monotonic_runs(p: &[f64], base: i64) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut start: i64 = -1;
    let mut dir = 0;
    let close = |runs: &mut Vec<Run>, start: &mut i64, dir: &mut i32, end: usize| {
        if *start >= 0 {
            runs.push(Run {
                a: base + *start,
                b: base + end as i64,
                dir: *dir,
                from: p[*start as usize - 1],
                to: p[end],
            });
        }
        *start = -1;
        *dir = 0;
    };
    for i in 1..p.len() {
        let d = p[i] - p[i - 1];
        // A held pitch, or a jump to another note (an arpeggio step, a new note without a gate):
        // neither is part of a glide.
        let jump = d.abs() >= 2.5 || (d.abs() >= 0.75 && (d - js_round(d)).abs() <= 0.1);
        if !d.is_finite() || d.abs() < STILL || jump {
            close(&mut runs, &mut start, &mut dir, i - 1);
            continue;
        }
        let s = sign(d);
        if start >= 0 && s != dir {
            close(&mut runs, &mut start, &mut dir, i - 1);
        }
        if start < 0 {
            start = i as i64;
            dir = s;
        }
    }
    close(&mut runs, &mut start, &mut dir, p.len() - 1);
    runs.retain(|r| r.b >= r.a);
    runs
}

/// Whether every step of the run is a whole number of semitones (an arpeggio walking, not a glide).
fn stepped_by_notes(p: &[f64], base: i64, r: &Run) -> bool {
    for i in r.a..=r.b {
        let d = p[(i - base) as usize] - p[(i - base - 1) as usize];
        if (d - js_round(d)).abs() > 0.08 || js_round(d) == 0.0 {
            return false;
        }
    }
    true
}

/// A swing is at least this many extrema (two periods), swinging this far (semitones, peak to peak).
const MIN_EXTREMA: usize = 5;
/// A pattern vibrato swings 4 times deeper than the instrument vibrato's model says (measured on the player, robocop_3): 2 more shifts.
const PATTERN_VIBRATO_SHIFT: i32 = 2;
/// A slide this many frames long or more, then a swing: a pattern vibrato.
const MIN_SLIDE_FRAMES: i64 = 6;
/// ... that starts this many frames into its note or later (a slide at the start leaves its swing to the instrument's vibrato).
const LATE_SLIDE: i64 = 24;
const SWING_SPAN: (f64, f64) = (0.1, 2.0);

struct Swing {
    a: i64,
    b: i64,
    period: f64,
    span: f64,
}

/// The regular swing the pitch makes from frame `from` on (a vibrato after a slide): its frames
/// [a, b], period and peak-to-peak span, or `None`. The extrema must keep one spacing and depth.
fn swing_from(p: &[f64], base: i64, from: i64) -> Option<Swing> {
    let at = |i: i64| -> f64 { p.get((i - base) as usize).copied().unwrap_or(f64::NAN) };
    let mut ext: Vec<i64> = Vec::new();
    let mut dir = 0;
    let mut i = from + 1;
    while i < base + p.len() as i64 {
        let d = at(i) - at(i - 1);
        if !d.is_finite() {
            break;
        }
        if d.abs() < STILL {
            i += 1;
            continue;
        }
        let s = sign(d);
        if dir != 0 && s != dir {
            ext.push(i - 1);
        }
        dir = s;
        i += 1;
    }
    if ext.len() < MIN_EXTREMA {
        return None;
    }
    let gaps: Vec<i64> = ext.windows(2).map(|w| w[1] - w[0]).collect();
    let mut sorted_gaps = gaps.clone();
    sorted_gaps.sort_unstable();
    let half = sorted_gaps[gaps.len() >> 1];
    let depths: Vec<f64> = ext
        .windows(2)
        .map(|w| (at(w[1]) - at(w[0])).abs())
        .collect();
    let mut sorted_depths = depths.clone();
    sorted_depths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let depth = sorted_depths[depths.len() >> 1];
    if half < 2 || depth < SWING_SPAN.0 || depth > SWING_SPAN.1 {
        return None;
    }
    // Keep the extrema while they keep the spacing and depth.
    let mut last = 0usize;
    while last + 1 < ext.len()
        && (gaps[last] - half).abs() <= 1
        && (depths[last] - depth).abs() <= 0.4 * depth
    {
        last += 1;
    }
    if last + 1 < MIN_EXTREMA {
        return None;
    }
    Some(Swing {
        a: from + 1,
        b: ext[last],
        period: (2 * half) as f64,
        span: depth,
    })
}

/// The glides of a note: runs over `MIN_GLIDE` that are not a vibrato's swing or an arpeggio.
fn glides(p: &[f64], base: i64) -> Vec<Run> {
    let runs = monotonic_runs(p, base);
    let mut out = Vec::new();
    for (k, r) in runs.iter().enumerate() {
        let span = (r.to - r.from).abs();
        if span < MIN_GLIDE || !span.is_finite() || stepped_by_notes(p, base, r) {
            continue;
        }
        // A glide moves over several frames; one step that makes most of it is a jump (a vibrato's
        // last swing into a new note, say), not a glide.
        let mut biggest: f64 = 0.0;
        for i in r.a..=r.b {
            biggest = biggest.max((p[(i - base) as usize] - p[(i - base - 1) as usize]).abs());
        }
        if r.b - r.a + 1 < 3 || biggest > 0.6 * span {
            continue;
        }
        // A swing: a run the other way right before or after, of a like width (a vibrato, however
        // wide, or a trill). A glide into a note that then vibrates is far wider than the vibrato.
        let near = |o: Option<&Run>| -> bool {
            let Some(o) = o else { return false };
            if o.dir == r.dir {
                return false;
            }
            let gap = if o.a > r.a { o.a - r.b } else { r.a - o.b };
            let w = (o.to - o.from).abs();
            gap <= 2 && w >= 0.5 * span && w <= 2.0 * span
        };
        if (k > 0 && near(runs.get(k - 1))) || near(runs.get(k + 1)) {
            continue;
        }
        out.push(*r);
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EffectKind {
    Porta,
    Bend,
    Vibrato,
}

struct Effect {
    kind: EffectKind,
    vibrato: Option<(u8, u8)>,
    /// First and last row it runs on.
    r: i64,
    rb: i64,
    /// Its first moving frame (tick 1 of row `r`) and the original's last.
    s: i64,
    b: i64,
    note: Option<i32>,
    speed: i64,
    command: u8,
    /// The pitch the original ends on.
    to: f64,
}

/// Frames that move in [s, e] (absolute): every frame but the rows' first (GoatTracker's tick effects skip it).
fn moving_frames(g: &RowGrid, s: i64, e: i64) -> i64 {
    let mut n = 0;
    for i in s..=e {
        if row_start(g, row_of_frame(g, i)) != i {
            n += 1;
        }
    }
    n
}

/// `speed` on a 4% geometric grid, so a song's speeds share few speed-table rows
/// (`up`: the grid value at or above it, for a tone portamento, which stops at
/// its note anyway).
pub fn grid_speed(speed: f64, up: bool) -> i64 {
    if speed <= 16.0 {
        return (js_round(speed) as i64).max(1);
    }
    let k = (speed / 16.0).ln() / 1.04f64.ln();
    let e = if up { (k - 1e-9).ceil() } else { js_round(k) };
    let q = js_round(16.0 * 1.04f64.powf(e)) as i64;
    q.min(0x7fff)
}

/// The register value of pitch `p` (GoatTracker note units, the tuning taken off) on GoatTracker's clock.
fn reg_of_pitch(regs: &[u16; 96], p: f64) -> f64 {
    reg_of(regs, 57) * 2f64.powf((p - 57.0) / 12.0)
}

/// The pitch plan of one voice: the pattern cells its glides, bends and ties
/// take, and the frames they own. `notes`: the voice's notes on rows (gate-ons);
/// `base_of`: each note's own note index (the row's note).
pub fn plan_pitch(
    f: &TraceFrames,
    g: &RowGrid,
    notes: &[RowNote],
    base_of: &dyn Fn(&RowNote) -> i32,
    tuning: f64,
) -> PitchPlan {
    let regs = gt_note_regs();
    let nframes = f.frames as i64;
    let mut owned = vec![0u8; f.frames];
    let mut base = vec![-1i16; f.frames];
    let mut rows: Vec<PitchRow> = Vec::new();
    if notes.is_empty() {
        return PitchPlan { rows, owned, base };
    }
    let voice = notes[0].voice;
    let delay = g.delays.get(voice).copied().unwrap_or(0);
    let mut taken: HashSet<i64> = notes.iter().map(|n| n.row).collect();
    let frame_of_tick = |r: i64, t: i64| -> i64 { row_start(g, r) + t + delay };
    let ctrl_of = &f.voices[voice].ctrl;
    let ctrl_at = |i: i64| -> u8 {
        if i < 0 {
            0
        } else {
            ctrl_of.get(i as usize).copied().unwrap_or(0)
        }
    };
    // The tick (0 or 1) at which this note's rows hold one pitch each: the one whose windows are the
    // cleaner (a window with two notes in it is cut at the wrong frame).
    let tie_phase = |note: &RowNote, first_row_end: i64| -> i64 {
        let last = row_of_frame(g, nframes.min(note.end) - 1 - delay).min(note.row + 64);
        let mut score = [0, 0];
        for t in [0i64, 1] {
            let mut r = note.row + 1;
            while r <= last {
                let s = frame_of_tick(r, t);
                let e = note.end.min(nframes).min(frame_of_tick(r + 1, t));
                r += 1;
                if e - s < 2 || s < first_row_end - 1 {
                    continue;
                }
                let mut steps: HashSet<u16> = HashSet::new();
                for i in s..e {
                    steps.insert(f.voices[voice].freq[i as usize]);
                }
                if (ctrl_at(s) & 1) != 0 && steps.len() == 1 {
                    score[t as usize] += 1;
                }
            }
        }
        if score[0] > score[1] {
            0
        } else {
            1
        }
    };
    for (n, note) in notes.iter().enumerate() {
        let from = note.onset.max(0);
        let next = notes.get(n + 1);
        let to = nframes.min(next.map_or(nframes, |nx| nx.onset));
        if to - from < 3 {
            continue;
        }
        let p = pitch_series(f, voice, from, to, tuning);
        let first_row_end = frame_of_tick(note.row + 1, 1);
        // Frames from `at` on hold the note the pattern set there (until a later setting).
        let set_base = |base: &mut Vec<i16>, at: i64, value: i32| {
            for i in from.max(at)..to {
                base[i as usize] = value as i16;
            }
        };
        let own = |owned: &mut Vec<u8>, a: i64, b: i64| {
            for i in from.max(a)..=(to - 1).min(b) {
                owned[i as usize] = 1;
            }
        };
        // The glides, as effects on rows (nothing written yet).
        let mut effects: Vec<Effect> = Vec::new();
        let mut last_end = from;
        for gl in glides(&p, from) {
            if gl.a <= last_end {
                continue;
            }
            // Inside the note's first row: the instrument's.
            if gl.b < first_row_end && gl.a - from <= 2 {
                continue;
            }
            // The row whose second frame (tick 1) is at or before the glide's first moving frame.
            let r = note.row.max(row_of_frame(g, gl.a - 1 - delay));
            let rb = r.max(row_of_frame(g, gl.b - delay));
            if let Some(prev) = effects.last() {
                if r <= prev.rb {
                    continue;
                }
            }
            let mut clash = false;
            for k in r + 1..=rb {
                if taken.contains(&k) {
                    clash = true;
                }
            }
            if clash {
                continue;
            }
            // On the note's own row the wave table's first row sets the note on tick 1: the effect moves from tick 2.
            let s = frame_of_tick(r, if r == note.row { 2 } else { 1 });
            // A pattern effect starts on a row's second tick: a glide that begins later in its row (after a hold)
            // would be played from the start of the row, so it stays the instrument's (as its wave table steps).
            if gl.a - s > GLIDE_START_SLACK {
                continue;
            }
            let lands = (gl.to - js_round(gl.to)).abs() <= LANDS;
            if lands && r > note.row {
                let t = nearest_note(gl.to);
                // From the pitch the pattern holds when the row starts (the row before's), which is lower than the
                // glide's own start when the original steps up in the row's first frames before it slides.
                let idx = row_start(g, r) - 1 - from;
                let before = if idx >= 0 {
                    p.get(idx as usize).copied()
                } else {
                    None
                };
                let start = match before {
                    Some(b) if b.is_finite() && (b - gl.from).abs() <= 12.0 => b.min(gl.from),
                    _ => gl.from,
                };
                let dist = (reg_of(&regs, t) - reg_of_pitch(&regs, start)).abs();
                let speed = grid_speed(
                    (dist / (moving_frames(g, s, gl.b).max(1)) as f64).ceil(),
                    true,
                );
                effects.push(Effect {
                    kind: EffectKind::Porta,
                    vibrato: None,
                    r,
                    rb,
                    s,
                    b: gl.b,
                    note: Some(t),
                    speed,
                    command: 3,
                    to: gl.to,
                });
            } else {
                // A bend: the speed that reaches the end pitch by the end of the last row it spans
                // (the command runs to the row's end); the pitch then stays where it went.
                let end_frame = frame_of_tick(rb + 1, 0) - 1;
                let dist = (reg_of_pitch(&regs, gl.to) - reg_of_pitch(&regs, gl.from)).abs();
                let speed =
                    grid_speed(dist / (moving_frames(g, s, end_frame).max(1)) as f64, false);
                effects.push(Effect {
                    kind: EffectKind::Bend,
                    vibrato: None,
                    r,
                    rb,
                    s,
                    b: gl.b,
                    note: None,
                    speed,
                    command: if gl.to > gl.from { 1 } else { 2 },
                    to: gl.to,
                });
            }
            last_end = gl.b;
            // A swing that follows it, once it has landed (a vibrato on the note it slid to): command 4 from the next row.
            let last_rb = effects.last().unwrap().rb;
            // (Only after a real slide: the swing after a short bend is the instrument's own vibrato.)
            let sw = if gl.b - gl.a + 1 >= MIN_SLIDE_FRAMES && gl.a - from >= LATE_SLIDE {
                swing_from(&p, from, gl.b)
            } else {
                None
            };
            let Some(sw) = sw else { continue };
            let vr = (last_rb + 1).max(row_of_frame(g, sw.a - 1 - delay));
            let vrb = vr.max(row_of_frame(g, sw.b - delay));
            let vs = frame_of_tick(vr, 1);
            // A row's first frame does not advance the effect: a swing of `period` frames is one of that many less ticks.
            let len = row_length(g, vr).max(2);
            if vs >= sw.b || ((sw.b - vs) as f64) < 2.0 * sw.period || taken.contains(&vr) {
                continue;
            }
            let mut vclash = false;
            for k in vr..=vrb {
                if taken.contains(&k) {
                    vclash = true;
                }
            }
            if vclash {
                continue;
            }
            effects.push(Effect {
                kind: EffectKind::Vibrato,
                vibrato: Some(vibrato_for(
                    (sw.period * (len - 1) as f64) / len as f64,
                    sw.span,
                    PATTERN_VIBRATO_SHIFT,
                )),
                r: vr,
                rb: vrb,
                s: vs,
                b: sw.b,
                note: None,
                speed: 0,
                command: 4,
                to: gl.to,
            });
            last_end = sw.b;
        }
        // The rows in order: each glide's rows, and on the rows between, a tie where the held
        // pitch moves to another note. After a bend the pitch is the pattern's until a tie or
        // a glide sets a note again.
        // Where a row's held pitch starts: GoatTracker's own ties act on a row's second tick, but a tune whose
        // player writes its notes on a row's first frame (a bare test-bit frame, `RowGrid::lead` 0) steps there.
        let tie = if g.lead == 0 && tie_phase(note, first_row_end) == 0 {
            0
        } else {
            1
        };
        let mut current: Option<i32> = Some(base_of(note));
        let mut bent_to = 0.0;
        let mut bend_from: i64 = -1;
        let end_bend = |owned: &mut Vec<u8>, bend_from: &mut i64, at: i64| {
            if *bend_from >= 0 {
                own(owned, *bend_from, at - 1);
            }
            *bend_from = -1;
        };
        let last_row = row_of_frame(g, to - 1 - delay);
        let mut ei = 0usize;
        let mut r = note.row;
        while r <= last_row {
            if let Some(eff) = effects.get(ei).filter(|e| e.r == r) {
                ei += 1;
                match eff.kind {
                    EffectKind::Porta => {
                        end_bend(&mut owned, &mut bend_from, eff.s);
                        rows.push(PitchRow {
                            row: r,
                            note: eff.note,
                            command: 3,
                            vibrato: None,
                            speed: eff.speed,
                            continued: false,
                        });
                        for k in r + 1..=eff.rb {
                            rows.push(PitchRow {
                                row: k,
                                note: None,
                                command: 3,
                                vibrato: None,
                                speed: eff.speed,
                                continued: true,
                            });
                        }
                        own(&mut owned, eff.s, eff.b);
                        set_base(&mut base, eff.b + 1, eff.note.unwrap());
                        current = eff.note;
                    }
                    EffectKind::Vibrato => {
                        end_bend(&mut owned, &mut bend_from, eff.s);
                        for k in r..=eff.rb {
                            rows.push(PitchRow {
                                row: k,
                                note: None,
                                command: 4,
                                vibrato: eff.vibrato,
                                speed: 0,
                                continued: k > r,
                            });
                        }
                        own(&mut owned, eff.s, eff.b);
                    }
                    EffectKind::Bend => {
                        end_bend(&mut owned, &mut bend_from, eff.s);
                        for k in r..=eff.rb {
                            rows.push(PitchRow {
                                row: k,
                                note: None,
                                command: eff.command,
                                vibrato: None,
                                speed: eff.speed,
                                continued: k > r,
                            });
                        }
                        bend_from = eff.s;
                        bent_to = eff.to;
                        current = None;
                    }
                }
                for k in r..=eff.rb {
                    taken.insert(k);
                }
                r = eff.rb + 1;
                continue;
            }
            let this = r;
            r += 1;
            if this == note.row || taken.contains(&this) {
                continue;
            }
            let s = frame_of_tick(this, tie);
            let e = to.min(frame_of_tick(this + 1, tie));
            if e - s < 2 {
                continue;
            }
            // Only while the gate stays on: a pitch that changes as the note is let go is its release
            // (the row keeps its note column for the key-off).
            let mut held: Vec<i32> = Vec::new();
            let mut released = false;
            for i in s..e {
                if ctrl_at(i) & 1 == 0 {
                    released = true;
                    continue;
                }
                let idx = i - from;
                if idx >= 0 {
                    if let Some(&v) = p.get(idx as usize) {
                        if v.is_finite() {
                            held.push(nearest_note(v));
                        }
                    }
                }
            }
            if released || held.len() < 2 {
                continue;
            }
            held.sort_unstable();
            if held[held.len() - 1] - held[0] > 2 {
                continue;
            }
            let pitch = held[held.len() >> 1];
            let moved = match current {
                None => {
                    pitch != nearest_note(bent_to) || (bent_to - js_round(bent_to)).abs() > LANDS
                }
                Some(c) => pitch != c,
            };
            if moved {
                end_bend(&mut owned, &mut bend_from, s);
                rows.push(PitchRow {
                    row: this,
                    note: Some(pitch),
                    command: 3,
                    vibrato: None,
                    speed: 0,
                    continued: false,
                });
                taken.insert(this);
                set_base(&mut base, s, pitch);
                current = Some(pitch);
            }
        }
        end_bend(&mut owned, &mut bend_from, to);
    }
    PitchPlan { rows, owned, base }
}
