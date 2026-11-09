//! Instruments of a transcription. A port of `transcribe/instruments.ts`.
//! Notes are grouped in two steps:
//!
//!  1. by timbre: the envelope, whether a pulse sounds, and the frames the note
//!     plays (control byte and whole-semitone pitch per frame); a shorter note
//!     whose frames are the start of a longer one's joins it, and a frame whose
//!     pitch lies between semitones (a vibrato, a slide) matches any pitch;
//!  2. by vibrato: within a timbre, the notes whose pitch swings (the long
//!     notes, in Hubbard's players) and those whose pitch holds become two
//!     instruments sharing the timbre's tables.
//!
//! Each becomes a GoatTracker instrument whose tables replay the timbre's
//! longest note:
//!
//!  - the wave table: one row per frame (waveform byte, gate included, and the
//!    note: relative to the row's note, absolute past the relative range, or
//!    "no change" while a vibrato moves the pitch; a slide as the nearest
//!    notes), a repeating tail as a loop (arpeggios), runs of unchanged frames
//!    as delay rows, a stop once it holds;
//!  - the pulse table: the shape of the width's movement (steady, one sweep,
//!    a triangle between two widths, a sawtooth) in a few rows;
//!  - the instrument vibrato: GoatTracker's calculated speed (the step to the
//!    next note shifted right, what Hubbard's players compute), fitted to the
//!    period and depth of the pitch's swing, with its delay;
//!  - the envelope as the note starts, the first-frame waveform with the gate
//!    off (the row starts a frame before the gate, `grid.rs`), and the hard
//!    restart the frames before its notes show: envelope zeroed (GT's hard
//!    restart), gate off only (no hard restart), or a single frame (no gate-off;
//!    the first-frame waveform takes the gate off).
//!
//! The notes of all subsongs are one arena (`&[NoteProgram]`); groups, plans
//! and instruments refer to a note by its index there, where the DAW's objects
//! refer to it by identity.

use super::notes::{NoteProgram, ProgramFrame, PulseFrame, MAX_HARD_RESTART_GAP, MAX_PULSE_FRAMES};
use super::util::{js_round, median_i, mode};
use super::vibrato::vibrato_for;
use crate::song::Instrument;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Frames whose pitch lies this close to a whole semitone are that note.
const PITCH_SNAP: f64 = 0.12;
/// The most instruments a song holds.
pub const MAX_TRANSCRIBED_INSTRUMENTS: usize = 63;
/// GoatTracker's start tempo: a gate timer must stay below it.
const DEFAULT_TEMPO: i64 = 6;

/// Semitones from the row's note; an absolute note (noise, whose pitch does not follow the melody);
/// between semitones; no frequency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PitchClass {
    Rel(i32),
    Abs(i32),
    Free,
    None,
}

/// How a group's noise frames name their pitch: absolute (a drum that sounds
/// the same under any note) or relative to the row's note (Hubbard's drums,
/// transposed with the bass line). A group takes the one its notes agree in.
/// `All`: every sounding frame is an absolute note (a drum or a stab that sounds
/// the same under any note, merged from groups that differ only in the row's note).
/// `Burst(k)`: the first k frames are absolute notes, the rest follow the row's note
/// (a drum's click and pitch drop, then the bass line: `81da 41a6 4100`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseMode {
    Absolute,
    Relative,
    All,
    Burst(u8),
}

/// The most frames a drum's absolute burst (`Burst(k)`) is tried over.
const MAX_BURST: usize = 6;

/// How a note's pulse width starts ('' without pulse): the width's top nibble, and its first sweep's direction and pace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PulseShape {
    None,
    Start(u16),
    Move(u16, i32, i32),
}

#[derive(Clone, Debug)]
pub struct Timbre {
    pub ad: u8,
    pub sr: u8,
    pub pulse: bool,
    pub pulse_shape: PulseShape,
    /// Notes that start without a gate-on (`RowNote::legato`).
    pub legato: bool,
    /// For a note that drives the filter: its mode and `$D417` as it starts; `None` otherwise.
    pub filter: Option<(u8, u8)>,
    /// Per frame: the control byte and the pitch class.
    pub ctrl: Vec<u8>,
    pub pitch: Vec<PitchClass>,
    /// As `pitch`, with the noise frames relative to the row's note.
    pub rel_pitch: Vec<PitchClass>,
    /// As `pitch`, with every sounding frame an absolute note.
    pub all_pitch: Vec<PitchClass>,
    /// As `pitch`, with the first K frames absolute notes and the rest relative: index K - 1.
    pub burst_pitch: Vec<Vec<PitchClass>>,
}

/// Notes of one timbre.
#[derive(Clone, Debug)]
pub struct NoteGroup {
    pub timbre: Timbre,
    /// The longest member (an index into the arena): its frames define the tables.
    pub rep: usize,
    pub members: Vec<usize>,
    /// How its noise frames' pitch is written; `None` until two of its notes tell (absolute then).
    pub noise: Option<NoiseMode>,
}

/// One instrument to build: a timbre, with or without its vibrato.
#[derive(Clone, Debug)]
pub struct InstrumentPlan {
    /// An index into [`Plans::groups`].
    pub group: usize,
    pub vibrato: Option<VibratoFit>,
    pub members: Vec<usize>,
    used: i64,
}

/// The plans and the groups they share.
#[derive(Clone, Debug)]
pub struct Plans {
    pub groups: Vec<NoteGroup>,
    pub plans: Vec<InstrumentPlan>,
}

impl Plans {
    pub fn group_of(&self, plan: &InstrumentPlan) -> &NoteGroup {
        &self.groups[plan.group]
    }
}

/// The pitch class of frame `f` of a note on `base`.
fn pitch_class(f: &ProgramFrame, base: i32, noise: NoiseMode, index: usize) -> PitchClass {
    if f.owned {
        return PitchClass::Free;
    }
    // A silent frame (the test bit holds the oscillator, or no waveform): its pitch is not heard.
    if f.ctrl & 0x08 != 0 || f.ctrl & 0xf0 == 0 {
        return PitchClass::Free;
    }
    let Some(pitch) = f.pitch else {
        return PitchClass::None;
    };
    let k = js_round(pitch) as i32;
    if (pitch - k as f64).abs() > PITCH_SNAP {
        return PitchClass::Free;
    }
    // Noise: its pitch is the drum's, whatever the note.
    let burst = match noise {
        NoiseMode::Burst(n) => n as usize,
        _ => 0,
    };
    if (f.ctrl & 0x80 != 0 && noise == NoiseMode::Absolute)
        || noise == NoiseMode::All
        || index < burst
    {
        return PitchClass::Abs((base + k).clamp(1, 95));
    }
    PitchClass::Rel(k)
}

pub fn timbre_of(p: &NoteProgram) -> Timbre {
    let pitch = |noise: NoiseMode| -> Vec<PitchClass> {
        p.frames
            .iter()
            .enumerate()
            .map(|(i, f)| pitch_class(f, p.base, noise, i))
            .collect()
    };
    Timbre {
        ad: p.ad,
        sr: p.sr,
        pulse: p.frames.iter().any(|f| f.ctrl & 0x40 != 0),
        // A legato note's instrument leaves the pulse table running: its width is the line's first note's.
        pulse_shape: if p.note.legato {
            PulseShape::None
        } else {
            pulse_shape(p)
        },
        legato: p.note.legato,
        filter: if p.filter && !p.frames.is_empty() {
            Some((p.frames[0].mode, p.frames[0].resonance))
        } else {
            None
        },
        ctrl: p.frames.iter().map(|f| f.ctrl).collect(),
        pitch: pitch(NoiseMode::Absolute),
        rel_pitch: pitch(NoiseMode::Relative),
        all_pitch: pitch(NoiseMode::All),
        burst_pitch: (0..MAX_BURST)
            .map(|k| pitch(NoiseMode::Burst(k as u8 + 1)))
            .collect(),
    }
}

/// A note's pulse width, coarsely: the top nibble it starts on and, over its
/// first 16 frames, the direction and pace (log2 of the step) of its first
/// move; notes alike in it share an instrument's pulse program.
fn pulse_shape(p: &NoteProgram) -> PulseShape {
    if !p.frames.iter().any(|f| f.ctrl & 0x40 != 0) || p.pulse.is_empty() {
        return PulseShape::None;
    }
    let pw: Vec<u16> = p.pulse.iter().map(|x| x.pw).collect();
    let first = (0..pw.len()).find(|&i| i > 0 && i < 16 && pw[i] != pw[i - 1]);
    let Some(first) = first else {
        return PulseShape::Start(pw[0] >> 8);
    };
    let d = pulse_delta(pw[first - 1] as i64, pw[first] as i64);
    PulseShape::Move(
        pw[0] >> 8,
        if d > 0 { 1 } else { -1 },
        js_round((d.abs() as f64).log2()) as i32,
    )
}

/// The timbre's pitch classes under `mode`.
fn pitches_of(t: &Timbre, mode: NoiseMode) -> &[PitchClass] {
    match mode {
        NoiseMode::Absolute => &t.pitch,
        NoiseMode::All => &t.all_pitch,
        NoiseMode::Relative => &t.rel_pitch,
        NoiseMode::Burst(k) => &t.burst_pitch[k as usize - 1],
    }
}

/// `short` plays the start of what `long` plays (a between-semitones pitch
/// matches any), its noise pitched as `noise` says; `pulse`: the pulse shapes
/// must agree.
fn is_prefix(short: &Timbre, long: &Timbre, pulse: bool, noise: NoiseMode) -> bool {
    if pulse && short.pulse_shape != long.pulse_shape {
        return false;
    }
    if short.ad != long.ad
        || short.sr != long.sr
        || short.pulse != long.pulse
        || short.legato != long.legato
        || short.filter != long.filter
    {
        return false;
    }
    if short.ctrl.len() > long.ctrl.len() {
        return false;
    }
    let sp = pitches_of(short, noise);
    let lp = pitches_of(long, noise);
    for i in 0..short.ctrl.len() {
        if short.ctrl[i] != long.ctrl[i] {
            return false;
        }
        let a = sp[i];
        let b = lp[i];
        if a != b && a != PitchClass::Free && b != PitchClass::Free {
            return false;
        }
    }
    true
}

/// How unlike two timbres sound (for merging past the instrument limit).
fn distance(a: &Timbre, b: &Timbre) -> i64 {
    let mut d = 0;
    if a.ad != b.ad {
        d += 6;
    }
    // The sustain level is how loud the note holds: each step apart is heard (an echo voice at sustain 1 on a lead's 6).
    if a.sr != b.sr {
        d += 6 + if ((a.sr >> 4) as i32 - (b.sr >> 4) as i32).abs() >= 4 {
            15
        } else {
            0
        };
    }
    if a.pulse != b.pulse {
        d += 2;
    }
    if a.pulse_shape != b.pulse_shape {
        d += 2;
    }
    // A legato note on a gated instrument retriggers; a gated note on a legato one never sounds.
    if a.legato != b.legato {
        d += 1000;
    }
    if a.filter != b.filter {
        d += 4;
    }
    // Over the first 12 frames either plays; a frame only one plays differs.
    let n = 12.min(a.ctrl.len().max(b.ctrl.len()));
    for i in 0..n {
        if a.ctrl.get(i) != b.ctrl.get(i) {
            d += 1;
        }
        if a.pitch.get(i) != b.pitch.get(i) {
            d += 1;
        }
    }
    d
}

/// Whether `timbre` joins group `gi` (`is_prefix`), in the group's noise mode or,
/// while it has none, in either; a join that only one mode allows settles the
/// group's (a drum on another note that matches only relative to it: the drum
/// follows the melody).
fn joins(timbre: &Timbre, groups: &mut [NoteGroup], gi: usize, pulse: bool) -> bool {
    let group = &mut groups[gi];
    if let Some(noise) = group.noise {
        return is_prefix(timbre, &group.timbre, pulse, noise);
    }
    let abs = is_prefix(timbre, &group.timbre, pulse, NoiseMode::Absolute);
    let rel = is_prefix(timbre, &group.timbre, pulse, NoiseMode::Relative);
    if abs != rel {
        group.noise = Some(if abs {
            NoiseMode::Absolute
        } else {
            NoiseMode::Relative
        });
    }
    abs || rel
}

/// The instrument plans of `progs`: timbres, split by vibrato; never more than `max` (the least used merge into their nearest).
pub fn group_notes(progs: &[Rc<NoteProgram>], max: usize) -> Plans {
    // Notes that differ only in how their pulse width starts share an instrument: the pulse
    // program is shared per envelope (`share_pulse`), as GoatTracker's own songs do (the round trip
    // of the GT corpus makes 40 instruments of 19 when the pulse is told apart).
    let mut p = plans_of(progs, false);
    merge_absolute(progs, &mut p);
    if p.plans.len() > max {
        merged_to(progs, &mut p, max);
    }
    p
}

/// Drum plans that play the same absolute notes on different row notes (which the
/// grouping by row-relative pitch kept apart) become one instrument whose wave program names
/// absolute notes, as GoatTracker songs write them (`81da 41a6 009c`).
fn merge_absolute(progs: &[Rc<NoteProgram>], p: &mut Plans) {
    let mut order: Vec<usize> = (0..p.plans.len()).collect();
    order.sort_by(|&a, &b| {
        let fa = progs[p.groups[p.plans[a].group].rep].frames.len();
        let fb = progs[p.groups[p.plans[b].group].rep].frames.len();
        fb.cmp(&fa)
    });
    let mut hosts: Vec<usize> = Vec::new();
    for pi in order {
        let plan_group = p.plans[pi].group;
        // Frames whose pitch is not heard match any: merging needs notes that say the pitch is the same.
        let evidence = p.groups[plan_group]
            .timbre
            .all_pitch
            .iter()
            .filter(|x| matches!(x, PitchClass::Abs(_)))
            .count();
        // Drums only: a tonal line merged on its absolute notes lost pitch on real tunes (terra_cresta voice 2 0.76 -> 0.43).
        let drum = p.groups[plan_group]
            .timbre
            .ctrl
            .iter()
            .any(|c| c & 0x80 != 0);
        if p.plans[pi].vibrato.is_some()
            || p.groups[plan_group].timbre.legato
            || evidence < 2
            || !drum
        {
            hosts.push(pi);
            continue;
        }
        // The mode both plans (and every note of the host) agree in: the host's own once set, else the
        // fewest absolute frames that do.
        let mut chosen: Option<NoiseMode> = None;
        let mut host: Option<usize> = None;
        for &h in &hosts {
            let hg = p.plans[h].group;
            if p.plans[h].vibrato.is_some()
                || p.groups[hg].timbre.legato
                || p.groups[hg].timbre.pulse != p.groups[plan_group].timbre.pulse
            {
                continue;
            }
            let fixed = matches!(
                p.groups[hg].noise,
                Some(NoiseMode::All) | Some(NoiseMode::Burst(_))
            );
            let modes: Vec<NoiseMode> = if fixed {
                vec![p.groups[hg].noise.unwrap()]
            } else {
                (1..=MAX_BURST)
                    .map(|k| NoiseMode::Burst(k as u8))
                    .chain(std::iter::once(NoiseMode::All))
                    .collect()
            };
            let mode =
                modes.into_iter().find(|&m| {
                    is_prefix(&p.groups[plan_group].timbre, &p.groups[hg].timbre, false, m)
                        && p.plans[h].members.iter().all(|&x| {
                            is_prefix(&timbre_of(&progs[x]), &p.groups[hg].timbre, false, m)
                        })
                        && p.plans[pi].members.iter().all(|&x| {
                            is_prefix(&timbre_of(&progs[x]), &p.groups[hg].timbre, false, m)
                        })
                });
            if let Some(m) = mode {
                chosen = Some(m);
                host = Some(h);
                break;
            }
        }
        let (Some(h), Some(chosen)) = (host, chosen) else {
            hosts.push(pi);
            continue;
        };
        let moved = p.plans[pi].members.clone();
        p.plans[h].members.extend(moved);
        // Only when it joined another base's notes: a plan of one base keeps its mode.
        let bases: HashSet<i32> = p.plans[h].members.iter().map(|&m| progs[m].base).collect();
        if bases.len() > 1 {
            let hg = p.plans[h].group;
            p.groups[hg].noise = Some(chosen);
        }
    }
    let kept: Vec<InstrumentPlan> = hosts.iter().map(|&i| p.plans[i].clone()).collect();
    p.plans = kept;
}

fn plans_of(progs: &[Rc<NoteProgram>], pulse: bool) -> Plans {
    let mut sorted: Vec<usize> = (0..progs.len()).collect();
    sorted.sort_by(|&a, &b| progs[b].frames.len().cmp(&progs[a].frames.len()));
    let mut groups: Vec<NoteGroup> = Vec::new();
    for pi in sorted {
        let timbre = timbre_of(&progs[pi]);
        let mut home = None;
        for gi in 0..groups.len() {
            if joins(&timbre, &mut groups, gi, pulse) {
                home = Some(gi);
                break;
            }
        }
        match home {
            Some(gi) => groups[gi].members.push(pi),
            None => groups.push(NoteGroup {
                timbre,
                rep: pi,
                members: vec![pi],
                noise: None,
            }),
        }
    }
    let mut plans: Vec<InstrumentPlan> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let mut still: Vec<usize> = Vec::new();
        let mut swinging: Vec<(usize, VibratoFit)> = Vec::new();
        // The fits by (left, right), in the order first seen, with how many notes each has.
        let mut fits: Vec<(VibratoFit, usize)> = Vec::new();
        for &m in &g.members {
            let Some(fit) = fit_vibrato(&progs[m]) else {
                still.push(m);
                continue;
            };
            swinging.push((m, fit));
            match fits
                .iter_mut()
                .find(|(f, _)| f.left == fit.left && f.right == fit.right)
            {
                Some(seen) => seen.1 += 1,
                None => fits.push((fit, 1)),
            }
        }
        if !swinging.is_empty() {
            // The most used fit, the first seen of them on a tie.
            let mut best = fits[0];
            for &(f, c) in &fits {
                if c > best.1 {
                    best = (f, c);
                }
            }
            let best = best.0;
            let mut delays: Vec<i64> = swinging.iter().map(|(_, f)| f.delay).collect();
            delays.sort_unstable();
            plans.push(InstrumentPlan {
                group: gi,
                vibrato: Some(VibratoFit {
                    delay: delays[delays.len() >> 1],
                    ..best
                }),
                members: swinging.iter().map(|(m, _)| *m).collect(),
                used: 0,
            });
        }
        if !still.is_empty() {
            plans.push(InstrumentPlan {
                group: gi,
                vibrato: None,
                members: still,
                used: 0,
            });
        }
    }
    Plans { groups, plans }
}

/// A note counts at least this many frames when merging: a drum's one-frame program sounds on in its envelope.
const MIN_USED_FRAMES: i64 = 12;

/// Frames a merge would give another instrument's sound: the frames its notes'
/// programs describe, at least `MIN_USED_FRAMES` a note (a long sweep counts for
/// more than a held line's one row, a drum hit on every bar for more than
/// either), and a held line's first note the line's too (its envelope and
/// pulse table play all of it).
fn used_frames(progs: &[Rc<NoteProgram>], plan: &InstrumentPlan) -> i64 {
    plan.members
        .iter()
        .map(|&m| {
            (progs[m].frames.len() as i64).max(MIN_USED_FRAMES)
                + MAX_PULSE_FRAMES.min(progs[m].line_frames)
        })
        .sum()
}

/// `plans` down to `max`: the least used (by the frames their notes sound) merge into their nearest.
fn merged_to(progs: &[Rc<NoteProgram>], p: &mut Plans, max: usize) {
    for plan in p.plans.iter_mut() {
        plan.used = used_frames(progs, plan);
    }
    while p.plans.len() > max {
        p.plans.sort_by_key(|plan| std::cmp::Reverse(plan.used));
        let victim = p.plans.pop().unwrap();
        let mut best = 0usize;
        let mut best_d = i64::MAX;
        for (i, other) in p.plans.iter().enumerate() {
            let d = distance(
                &p.groups[victim.group].timbre,
                &p.groups[other.group].timbre,
            ) + if other.group == victim.group { -100 } else { 0 };
            if d < best_d {
                best = i;
                best_d = d;
            }
        }
        let target = &mut p.plans[best];
        target.members.extend(victim.members);
        target.used += victim.used;
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// A table row while a program is being made: the jump rows hold wider values than a byte before they are placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TRow {
    pub left: i32,
    pub right: i32,
}

/// One table program before it is placed: jump rows (left `$FF`) hold 1-based targets inside the program (0 = stop).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableProgram {
    pub rows: Vec<TRow>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Table {
    Wave,
    Pulse,
    Filter,
}

/// The four GoatTracker tables being filled, each program placed once: the
/// same rows twice share one copy (jumps are program-relative until placed).
#[derive(Default)]
pub struct TableBuilder {
    pub wave: Vec<TRow>,
    pub pulse: Vec<TRow>,
    pub filter: Vec<TRow>,
    pub speed: Vec<TRow>,
    placed: HashMap<Vec<i32>, usize>,
}

impl TableBuilder {
    pub fn new() -> TableBuilder {
        TableBuilder::default()
    }

    /// Place `program` in `table`; its 1-based start row.
    pub fn place(&mut self, table: Table, program: &TableProgram) -> usize {
        if program.rows.is_empty() {
            return 0;
        }
        let mut key: Vec<i32> = vec![table as i32];
        for r in &program.rows {
            key.push(r.left);
            key.push(r.right);
        }
        if let Some(&at) = self.placed.get(&key) {
            return at;
        }
        let rows = match table {
            Table::Wave => &mut self.wave,
            Table::Pulse => &mut self.pulse,
            Table::Filter => &mut self.filter,
        };
        let start = rows.len() + 1;
        for r in &program.rows {
            rows.push(if r.left == 0xff && r.right != 0 {
                TRow {
                    left: 0xff,
                    right: r.right + start as i32 - 1,
                }
            } else {
                *r
            });
        }
        self.placed.insert(key, start);
        start
    }

    /// A speed-table row (left, right); its 1-based row.
    pub fn speed_row(&mut self, left: i32, right: i32) -> usize {
        let key = vec![3, left, right];
        if let Some(&at) = self.placed.get(&key) {
            return at;
        }
        self.speed.push(TRow { left, right });
        self.placed.insert(key, self.speed.len());
        self.speed.len()
    }

    pub fn fits(&self) -> bool {
        self.wave.len() <= 255
            && self.pulse.len() <= 255
            && self.filter.len() <= 255
            && self.speed.len() <= 255
    }
}

// ---------------------------------------------------------------------------
// Wave table
// ---------------------------------------------------------------------------

/// The wave-table left byte for control byte `ctrl` (GT: `$10-$DF` as is, `$00-$0F` as `$E0-$EF`); `None` past `$DF`.
fn wave_left(ctrl: u8) -> Option<i32> {
    if (0x10..=0xdf).contains(&ctrl) {
        Some(ctrl as i32)
    } else if ctrl < 0x10 {
        Some(0xe0 | ctrl as i32)
    } else {
        None
    }
}

/// The wave-table right byte for a note `offset` semitones from the row's note `base` (relative, or absolute past its range).
fn note_right(base: i32, offset: i32) -> i32 {
    if (-32..=95).contains(&offset) && base + offset >= 0 && base + offset <= 95 {
        return offset & 0x7f;
    }
    0x80 | (base + offset).clamp(1, 95)
}

const NO_CHANGE: i32 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaveStep {
    pub left: i32,
    pub right: i32,
    /// A wave-table command row (`$F7`: set the whole waveform byte).
    pub command: bool,
}

/// Frames (0-based) of `frames` whose between-semitones pitch belongs to a slide: a monotonic run over half a semitone or more.
fn slide_frames(frames: &[ProgramFrame]) -> HashSet<usize> {
    let mut out = HashSet::new();
    let free = |f: &ProgramFrame| pitch_class(f, 0, NoiseMode::Absolute, 0) == PitchClass::Free;
    let mut i = 0;
    while i < frames.len() {
        if !free(&frames[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < frames.len() && free(&frames[j + 1]) {
            j += 1;
        }
        let run: Vec<f64> = frames[i.saturating_sub(1)..frames.len().min(j + 2)]
            .iter()
            .map(|f| f.pitch.unwrap_or(0.0))
            .collect();
        let diffs: Vec<f64> = run.windows(2).map(|w| w[1] - w[0]).collect();
        let monotonic = diffs.iter().all(|&d| d >= -0.01) || diffs.iter().all(|&d| d <= 0.01);
        if monotonic && (run[run.len() - 1] - run[0]).abs() >= 0.5 {
            for k in i..=j {
                out.insert(k);
            }
        }
        i = j + 1;
    }
    out
}

/// The per-frame wave steps of a note (frames 1..n): its waveform and note.
/// `vibrato_from`: from this frame (1-based) on the pitch is left to the vibrato.
pub fn wave_steps(
    rep: &NoteProgram,
    vibrato_from: Option<i64>,
    noise: Option<NoiseMode>,
) -> Vec<WaveStep> {
    let mut steps: Vec<WaveStep> = Vec::new();
    let mut last_note: Option<i32> = None;
    // Where the vibrato takes over, the note it swings around (the mean of its swing).
    // (The frames a pattern effect moves are not the vibrato's.)
    let swing: Vec<f64> = match vibrato_from {
        None => Vec::new(),
        Some(vf) => rep
            .frames
            .iter()
            .skip((vf - 1).max(0) as usize)
            .filter(|f| !f.owned && f.pitch.is_some())
            .map(|f| f.pitch.unwrap())
            .collect(),
    };
    let centre: Option<i32> = vibrato_from
        .map(|_| js_round(swing.iter().sum::<f64>() / (swing.len().max(1) as f64)) as i32);
    for (i, f) in rep.frames.iter().enumerate() {
        let frame = i as i64 + 1;
        let pc = pitch_class(f, rep.base, noise.unwrap_or(NoiseMode::Absolute), i);
        let mut right = NO_CHANGE;
        // A test-bit frame is silent: its pitch is not heard, and writing it would only tell notes apart.
        let free = vibrato_from.is_some_and(|vf| frame >= vf) || f.owned || f.ctrl & 0x08 != 0;
        if free && !f.owned && Some(frame) == vibrato_from {
            if let Some(c) = centre {
                if last_note != Some(rep.base + c) {
                    // GoatTracker's vibrato swings around the last note set: set the centre as it starts.
                    right = note_right(rep.base, c);
                    last_note = Some(rep.base + c);
                }
            }
        }
        if !free {
            // The note the frame sounds (absolute index), and how the row says it.
            let mut target: Option<i32> = None;
            let mut code = NO_CHANGE;
            match pc {
                PitchClass::Rel(k) => {
                    target = Some(rep.base + k);
                    code = note_right(rep.base, k);
                }
                PitchClass::Abs(n) => {
                    target = Some(n);
                    code = 0x80 | n;
                }
                _ => {
                    if let Some(p) = f.pitch {
                        // Between semitones (a slide, a detuned note): the nearest note GoatTracker's table has.
                        let r = js_round(p) as i32;
                        target = Some(rep.base + r);
                        code = note_right(rep.base, r);
                    }
                }
            }
            if let Some(t) = target {
                if Some(t) != last_note {
                    right = code;
                    last_note = Some(t);
                }
            }
        }
        match wave_left(f.ctrl) {
            None => {
                // A $F7 row sets the waveform only: the note waits for the next row.
                steps.push(WaveStep {
                    left: 0xf7,
                    right: f.ctrl as i32,
                    command: true,
                });
                if right != NO_CHANGE {
                    last_note = None;
                }
            }
            Some(left) => steps.push(WaveStep {
                left,
                right,
                command: false,
            }),
        }
    }
    steps
}

/// The wave program of `steps`: a repeating tail becomes a loop, a run of
/// frames that change nothing becomes a delay row, and the program stops once
/// nothing changes any more.
pub fn wave_program(steps: &[WaveStep]) -> TableProgram {
    if steps.is_empty() {
        return TableProgram {
            rows: vec![TRow {
                left: 0xff,
                right: 0,
            }],
        };
    }
    let n = steps.len();
    // The loop: the smallest period P whose repetition covers the note's end, from the earliest start.
    // The last few frames may break the pattern (the next note's hard restart
    // nearing): a repetition that stops up to 3 frames short still counts.
    let mut loop_start: i64 = -1;
    let mut period: i64 = 0;
    for p in 2..=16.min(n / 2) {
        let mut end = n;
        while end >= (2 * p).max(n.saturating_sub(3)) {
            let mut s = end - p;
            while s >= 1 && steps[s - 1] == steps[s - 1 + p] {
                s -= 1;
            }
            if end - s >= 2 * p {
                let body = &steps[s..s + p];
                let changes = body.iter().any(|b| b.right != NO_CHANGE)
                    || body.iter().map(|b| b.left).collect::<HashSet<_>>().len() > 1;
                if changes && (loop_start < 0 || ((s + p) as i64) < loop_start + period) {
                    loop_start = s as i64;
                    period = p as i64;
                }
            }
            if end == 0 {
                break;
            }
            end -= 1;
        }
    }
    let body: &[WaveStep] = if loop_start >= 0 {
        &steps[..(loop_start + period) as usize]
    } else {
        // Stop after the last frame that changes the waveform or the note.
        let mut last = 0;
        for i in 1..n {
            if steps[i].right != NO_CHANGE || steps[i].left != steps[i - 1].left || steps[i].command
            {
                last = i;
            }
        }
        &steps[..last + 1]
    };
    // Delay rows for runs of frames that change nothing (not inside the loop, whose rows are jumped to).
    let mut rows: Vec<TRow> = Vec::new();
    let mut row_of_step: Vec<usize> = vec![0; body.len()];
    let compress_to = if loop_start >= 0 {
        loop_start as usize
    } else {
        body.len()
    };
    let mut i = 0;
    while i < body.len() {
        let s = body[i];
        row_of_step[i] = rows.len();
        rows.push(TRow {
            left: s.left,
            right: s.right,
        });
        if i + 1 < compress_to && !s.command {
            let mut j = i + 1;
            while j < compress_to
                && !body[j].command
                && body[j].right == NO_CHANGE
                && body[j].left == s.left
                && j - (i + 1) < 16
            {
                j += 1;
            }
            let run = j - (i + 1);
            if run >= 2 {
                // A delay row of N lasts N + 1 frames.
                rows.push(TRow {
                    left: run as i32 - 1,
                    right: NO_CHANGE,
                });
                for k in i + 1..j {
                    row_of_step[k] = rows.len() - 1;
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    rows.push(TRow {
        left: 0xff,
        right: if loop_start >= 0 {
            row_of_step[loop_start as usize] as i32 + 1
        } else {
            0
        },
    });
    TableProgram { rows }
}

// ---------------------------------------------------------------------------
// Pulse table
// ---------------------------------------------------------------------------

fn median_f(values: &[i64]) -> i64 {
    median_i(values)
}

/// A pulse program never takes more rows than this (GoatTracker's table holds 255 for the whole song).
const MAX_PULSE_ROWS: usize = 40;
/// Frames of a fast sweep (a step past a signed byte) set one by one before the rest holds.
const MAX_SET_RUN: usize = 8;

/// The width change from `a` to `b` on GoatTracker's 12-bit register (it wraps).
fn pulse_delta(a: i64, b: i64) -> i64 {
    ((((b - a + 2048) % 4096) + 4096) % 4096) - 2048
}

/// The start and period (frames) from which the width's frame-to-frame steps
/// repeat to the end, seen at least twice; or `None`. A pulse sweep's cycle
/// (a triangle, a sawtooth that jumps back) loops in the table.
fn pulse_loop(pw: &[i64]) -> Option<(usize, usize)> {
    let n = pw.len();
    let d: Vec<i64> = pw
        .iter()
        .enumerate()
        .map(|(i, &w)| if i == 0 { 0 } else { pulse_delta(pw[i - 1], w) })
        .collect();
    let mut best: Option<(usize, usize)> = None;
    let max_period = 128.min((n.saturating_sub(1)) / 2);
    for period in 2..=max_period {
        let mut s = n - period;
        while s >= 2 && d[s - 1] == d[s - 1 + period] {
            s -= 1;
        }
        if n - s < 2 * period {
            continue;
        }
        // A still width is no loop.
        if d[s..s + period].iter().all(|&x| x == 0) {
            continue;
        }
        let better = match best {
            None => true,
            Some((bs, bp)) => s + period < bs + bp,
        };
        if better {
            best = Some((s, period));
        }
    }
    best
}

/// The pulse program that plays the width `pulse` (frame 1 on, `NoteProgram::pulse`):
/// a set row for the first frame, then each run of equal steps as a row that
/// adds that step for the run (the change the run makes, over the frames
/// GoatTracker steps: it skips the table on the row's frame `row_length -
/// gate_timer`, and on a held line's legato note's first frame), a step past a
/// signed byte as set rows, frame by frame; the cycle the width repeats as a
/// loop, else a stop after the last change. `None`: the note sounds no pulse.
pub fn pulse_program(
    frames: &[ProgramFrame],
    pulse: &[PulseFrame],
    gate_timer: i64,
) -> Option<TableProgram> {
    if !frames.iter().any(|f| f.ctrl & 0x40 != 0) || pulse.is_empty() {
        return None;
    }
    let set = |w: i64| TRow {
        left: 0x80 | ((w >> 8) & 0x0f) as i32,
        right: (w & 0xff) as i32,
    };
    let pw: Vec<i64> = pulse.iter().map(|p| p.pw as i64).collect();
    let skipped = |i: usize| -> bool {
        i > 0
            && ((pulse[i].k >= 1 && pulse[i].k == pulse[i].row_length - gate_timer)
                || pulse[i].note_start)
    };
    let lp = pulse_loop(&pw);
    let mut end = pw.len();
    match lp {
        Some((start, period)) => end = start + period,
        None => {
            while end > 1 && pw[end - 1] == pw[end - 2] {
                end -= 1;
            }
        }
    }
    let mut rows: Vec<TRow> = vec![set(pw[0])];
    // The row each frame's run starts on (for the loop's jump).
    let mut row_at: HashMap<usize, usize> = HashMap::new();
    let mut i = 1;
    while i < end && rows.len() < MAX_PULSE_ROWS - 1 {
        row_at.insert(i, rows.len());
        let step = pulse_delta(pw[i - 1], pw[i]);
        let mut j = i;
        // A run stops where the step changes, and at the loop's start (the jump lands on a row).
        while j + 1 < end
            && pulse_delta(pw[j], pw[j + 1]) == step
            && Some(j + 1) != lp.map(|l| l.0)
            && j + 1 - i < 126
        {
            j += 1;
        }
        if step.abs() > 127 {
            for k in i..=j.min(i + MAX_SET_RUN - 1) {
                rows.push(set(pw[k]));
            }
            if j > i + MAX_SET_RUN - 1 {
                rows.push(set(pw[j]));
            }
        } else {
            // A steady sweep (a loop that is this one run, a frame or two long): the step over every
            // time round, as the frames GoatTracker skips fall on other places of it each time.
            let steady = lp.is_some_and(|(ls, lper)| i == ls && j + 1 == ls + lper);
            let reps = if steady {
                let lper = lp.unwrap().1;
                (((pw.len() - j - 1) / lper) + 1).max(1)
            } else {
                1
            };
            let mut steps = 0i64;
            let mut total = 0i64;
            let lper = lp.map_or(0, |l| l.1);
            for m in 0..reps {
                let o = m * lper;
                for k in i + o..=j + o {
                    if !skipped(k) {
                        steps += 1;
                    }
                }
                total += pulse_delta(pw[i - 1 + o], pw[j + o]);
            }
            if steps > 0 {
                let speed = (js_round(total as f64 / steps as f64) as i64).clamp(-127, 127);
                rows.push(TRow {
                    left: (js_round(steps as f64 / reps as f64) as i32).max(1),
                    right: (speed & 0xff) as i32,
                });
            }
        }
        i = j + 1;
    }
    let target = lp.and_then(|l| row_at.get(&l.0).copied());
    rows.push(TRow {
        left: 0xff,
        right: target.map_or(0, |t| t as i32 + 1),
    });
    Some(TableProgram { rows })
}

// ---------------------------------------------------------------------------
// Filter table
// ---------------------------------------------------------------------------

/// Longest run of equal steps a filter row holds (time is 7 bits).
const MAX_FILTER_RUN: i64 = 127;
/// Segments an unrolled filter program keeps (the table is shared by the whole song).
const MAX_FILTER_SEGMENTS: usize = 24;

/// The filter program of a note that drives the filter: its mode, resonance
/// and routing and its cutoff on the first frame (one frame: GoatTracker runs
/// a set-cutoff row right after a set-mode row), then the cutoff's runs of
/// equal steps (8-bit, as GoatTracker's cutoff is; `track` gives the whole note,
/// else its first frames), a step past a signed byte as a set row. A sweep that
/// repeats (a slow rise made of steps and holds, as GoatTracker songs write it)
/// loops. Holds after its last row.
pub fn filter_program(frames: &[ProgramFrame], track: Option<&[i64]>) -> TableProgram {
    let first = &frames[0];
    let mut rows: Vec<TRow> = vec![
        TRow {
            left: 0x80 | (((first.mode & 7) as i32) << 4),
            right: first.resonance as i32,
        },
        TRow {
            left: 0x00,
            right: first.cutoff as i32,
        },
    ];
    let cut: Vec<i64> = match track {
        Some(t) if t.len() >= frames.len() => t.to_vec(),
        _ => frames.iter().map(|f| f.cutoff as i64).collect(),
    };
    let mut segs: Vec<(i64, i64)> = Vec::new();
    for i in 1..cut.len() {
        let d = cut[i] - cut[i - 1];
        match segs.last_mut() {
            Some(last) if last.0 == d && last.1 < MAX_FILTER_RUN => last.1 += 1,
            _ => segs.push((d, 1)),
        }
    }
    while segs.last().is_some_and(|s| s.0 == 0) {
        segs.pop();
    }
    // A repeating tail: the smallest cycle (in segments) seen twice, the last segment (cut by the
    // note's end) left out of the comparison.
    let mut loop_at: i64 = -1;
    let mut period = 0usize;
    let body = &segs[..segs.len().saturating_sub(1)];
    let mut p = 1;
    while p <= 8 && loop_at < 0 {
        let mut s = 0;
        while s + 2 * p <= body.len() {
            let mut ok = true;
            let mut k = s;
            while k + p < body.len() && ok {
                ok = body[k] == body[k + p];
                k += 1;
            }
            if ok {
                loop_at = s as i64;
                period = p;
                break;
            }
            s += 1;
        }
        p += 1;
    }
    let keep: &[(i64, i64)] = if loop_at >= 0 {
        &segs[..loop_at as usize + period]
    } else {
        &segs[..segs.len().min(MAX_FILTER_SEGMENTS)]
    };
    let mut row_of_seg: Vec<i32> = Vec::new();
    let mut value = first.cutoff as i64;
    for &(step, n) in keep {
        row_of_seg.push(rows.len() as i32 + 1);
        if (-128..=127).contains(&step) {
            rows.push(TRow {
                left: n as i32,
                right: (step & 0xff) as i32,
            });
            value += step * n;
            continue;
        }
        // A jump: set the value it lands on, then the rest of the run.
        value += step;
        rows.push(TRow {
            left: 0x00,
            right: (value & 0xff) as i32,
        });
        if n > 1 {
            rows.push(TRow {
                left: n as i32 - 1,
                right: 0,
            });
        }
        value += step * (n - 1);
    }
    rows.push(TRow {
        left: 0xff,
        right: if loop_at >= 0 {
            row_of_seg[loop_at as usize]
        } else {
            0
        },
    });
    TableProgram { rows }
}

// ---------------------------------------------------------------------------
// Vibrato
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct VibratoFit {
    /// Speed-table left byte (`$80 | compare value`: the depth calculated from the note step).
    pub left: u8,
    /// And right (the shift).
    pub right: u8,
    /// GoatTracker's instrument vibrato delay (1 = at once).
    pub delay: i64,
    /// The frame (1-based) the vibrato takes the pitch from.
    pub from: i64,
}

/// A GoatTracker vibrato for the swing of `rep`'s pitch after its gate goes on, or `None` when it holds still or slides.
pub fn fit_vibrato(rep: &NoteProgram) -> Option<VibratoFit> {
    let frames = &rep.frames;
    let gate_on = frames.iter().position(|f| f.ctrl & 1 != 0).unwrap_or(0);
    let slides = slide_frames(frames);
    let start = frames.iter().enumerate().position(|(i, f)| {
        i > gate_on
            && !f.owned
            && pitch_class(f, rep.base, NoiseMode::Absolute, 0) == PitchClass::Free
            && !slides.contains(&i)
    })?;
    let stop = frames
        .iter()
        .enumerate()
        .position(|(i, f)| i > start && f.owned)
        .unwrap_or(frames.len());
    let pitches: Vec<f64> = frames[start..stop]
        .iter()
        .map(|f| f.pitch.unwrap_or(0.0))
        .collect();
    if pitches.len() < 6 {
        return None;
    }
    let mut turns: Vec<i64> = Vec::new();
    for i in 1..pitches.len() - 1 {
        if pitches[i] - pitches[i - 1] > 0.0 && pitches[i + 1] - pitches[i] <= 0.0 {
            turns.push(i as i64);
        }
    }
    if turns.len() < 2 {
        return None;
    }
    let gaps: Vec<i64> = turns.windows(2).map(|w| w[1] - w[0]).collect();
    let period = median_f(&gaps);
    let max = pitches.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let min = pitches.iter().copied().fold(f64::INFINITY, f64::min);
    let span = max - min;
    if period < 2 || span < 0.05 {
        return None;
    }
    let (left, right) = vibrato_for(period as f64, span, 0);
    let from = start as i64 + 1;
    Some(VibratoFit {
        left,
        right,
        delay: (from - 1).max(1),
        from,
    })
}

// ---------------------------------------------------------------------------
// Instruments
// ---------------------------------------------------------------------------

/// The first-frame waveform of a note that starts with a gate-on, which
/// GoatTracker writes on the row's first tick, after the note's attack/decay
/// and sustain/release; the note's pitch and waveform follow on the next tick
/// (the wave table's first row, the original's gate-on frame). Two ways, and
/// the SID's envelope tells them apart (`NoteProgram::test_start_gain`, the
/// members' sum decides):
///
///  - the gate on, the test bit holding the oscillator silent (`$09`): the
///    envelope starts a frame early, right after the gate-off before it, and
///    the frame that still has the last note's pitch is silent. After a hard
///    restart this is the one that sounds: with the gate off here, the note's
///    own release rate would run the envelope's rate counter for a frame, and
///    the chip's ADSR delay bug would then hold the attack up to 33 ms (a
///    one-frame drum hit would never sound);
///  - the gate off, in the waveform the original had on this frame: the
///    attack starts on the original's frame. A note whose one frame of
///    gate-off is this one (`no_gate_off`: GoatTracker keeps the gate on up to
///    here) must have it, or it would never retrigger.
fn first_wave(progs: &[Rc<NoteProgram>], groups: &[NoteGroup], plan: &InstrumentPlan) -> u8 {
    let group = &groups[plan.group];
    if group.timbre.legato {
        return 0x00;
    }
    // A row that starts on the original's own test-bit frame (`RowGrid::lead` 0): that frame, as it was.
    let bare: Vec<&NoteProgram> = plan
        .members
        .iter()
        .map(|&m| &*progs[m])
        .filter(|m| m.note.onset == m.note.tick0 && (m.tick0_ctrl & 0x09) == 0x09)
        .collect();
    if bare.len() * 2 > plan.members.len() {
        return mode(bare.iter().map(|m| m.tick0_ctrl)).unwrap();
    }
    if plan
        .members
        .iter()
        .map(|&m| progs[m].test_start_gain)
        .sum::<i64>()
        > 0
    {
        return 0x09;
    }
    // A waveform GoatTracker takes as one (`$00`, `$FE`, `$FF` are commands), gate off.
    let ok = |w: u8| -> bool { w >= 0x10 && w != 0xfe && w != 0xff && w & 1 == 0 };
    let seen = mode(
        plan.members
            .iter()
            .map(|&m| progs[m].tick0_ctrl & 0xfe)
            .filter(|&w| ok(w)),
    );
    if let Some(seen) = seen {
        return seen;
    }
    let own = progs[group.rep]
        .frames
        .iter()
        .find(|f| f.ctrl & 1 != 0)
        .map_or(0x41, |f| f.ctrl)
        & 0xfe;
    if ok(own) {
        own
    } else {
        0x10
    }
}

struct Timing {
    gate_timer: u8,
    hard_restart: bool,
    no_gate_off: bool,
}

/// How GoatTracker ends and starts notes of `plan`, from what the original did around them.
fn note_timing(
    progs: &[Rc<NoteProgram>],
    groups: &[NoteGroup],
    plan: &InstrumentPlan,
    row_frames: i64,
) -> Timing {
    let members: Vec<&NoteProgram> = plan.members.iter().map(|&m| &*progs[m]).collect();
    let legato = groups[plan.group].timbre.legato;
    // The gap before this instrument's notes decides how GoatTracker ends the note before.
    let gap_before = mode(members.iter().map(|m| m.gap_before.min(9))).unwrap_or(2);
    let hard_restart = members.iter().filter(|m| m.hard_restart_before).count() * 2 > members.len();
    // The gap after its notes decides how early the next row is read (the gate timer): the
    // gate goes off `gap_after` frames before the next gate-on, GoatTracker's `gate_timer` frames
    // before the next row's first frame, which is 1 + the voice's delay before it.
    let lead = mode(members.iter().map(|m| m.note.onset - m.note.tick0))
        .unwrap_or(1)
        .max(0);
    // (A gap past MAX_HARD_RESTART_GAP is a rest, the note's own release: a key-off, not the gate timer's.)
    let gap_after = mode(members.iter().filter_map(|m| match m.gap_after {
        Some(g) if g <= MAX_HARD_RESTART_GAP => Some(g),
        _ => None,
    }));
    // (The song starts at GoatTracker's tempo 6 until an `F` command runs: a gate timer at or past it stops the player.)
    let gate_timer = 1i64.max(
        (row_frames - 1)
            .clamp(1, DEFAULT_TEMPO - 1)
            .min(gap_after.unwrap_or(2) - lead),
    );
    // A legato note: no gate-off before it (GoatTracker's $40 gate-timer bit).
    Timing {
        gate_timer: gate_timer as u8,
        hard_restart: hard_restart && !legato,
        no_gate_off: legato || gap_before <= 1,
    }
}

fn row_key(rows: &[TRow]) -> Vec<(i32, i32)> {
    rows.iter().map(|r| (r.left, r.right)).collect()
}

/// Rows `programs` take once identical ones share a copy.
fn shared_rows<'a>(programs: impl IntoIterator<Item = &'a TableProgram>) -> usize {
    let mut seen: HashSet<Vec<(i32, i32)>> = HashSet::new();
    let mut rows = 0;
    for p in programs {
        if !seen.insert(row_key(&p.rows)) {
            continue;
        }
        rows += p.rows.len();
    }
    rows
}

/// Per step of `plan`'s wave program, how many of its notes play that far.
fn step_usage(progs: &[Rc<NoteProgram>], plan: &InstrumentPlan, steps: usize) -> Vec<i64> {
    let mut usage = vec![0i64; steps];
    for &m in &plan.members {
        for u in usage.iter_mut().take(progs[m].frames.len().min(steps)) {
            *u += 1;
        }
    }
    usage
}

/// The wave programs of `plans`, cut to fit `budget` rows between them: while
/// they do not, the program whose next cut loses the fewest note-frames (its
/// notes that play past the cut; nothing when it still loops) gives up its
/// last frame, which then holds.
fn fit_wave_programs(progs: &[Rc<NoteProgram>], p: &Plans, budget: usize) -> Vec<TableProgram> {
    let steps: Vec<Vec<WaveStep>> = p
        .plans
        .iter()
        .map(|plan| {
            let g = &p.groups[plan.group];
            wave_steps(&progs[g.rep], plan.vibrato.map(|v| v.from), g.noise)
        })
        .collect();
    let mut cuts: Vec<usize> = steps.iter().map(|s| s.len()).collect();
    let usage: Vec<Vec<i64>> = p
        .plans
        .iter()
        .enumerate()
        .map(|(i, plan)| step_usage(progs, plan, steps[i].len()))
        .collect();
    let mut programs: Vec<TableProgram> = steps.iter().map(|s| wave_program(s)).collect();
    let loops = |prog: &TableProgram| -> bool {
        prog.rows
            .last()
            .is_some_and(|l| l.left == 0xff && l.right != 0)
    };
    let mut total = shared_rows(programs.iter());
    while total > budget {
        let mut best: i64 = -1;
        let mut best_cost = f64::INFINITY;
        for i in 0..p.plans.len() {
            if cuts[i] <= 1 {
                continue;
            }
            let next = wave_program(&steps[i][..cuts[i] - 1]);
            // A cut that saves no row yet costs a little (it leads to one that does).
            let lost: i64 = if loops(&next) {
                0
            } else {
                usage[i][cuts[i] - 1..].iter().sum()
            };
            let cost = lost as f64
                / (1.0f64).max(programs[i].rows.len() as f64 - next.rows.len() as f64 + 0.5);
            if cost < best_cost {
                best = i as i64;
                best_cost = cost;
            }
        }
        if best < 0 {
            break;
        }
        let b = best as usize;
        cuts[b] -= 1;
        programs[b] = wave_program(&steps[b][..cuts[b]]);
        total = shared_rows(programs.iter());
    }
    programs
}

/// `p` cut to `max` rows (its first `max - 1`, then a stop; a loop's jump past the cut stops too).
fn cut_pulse(p: &TableProgram, max: usize) -> Rc<TableProgram> {
    let mut rows: Vec<TRow> = p
        .rows
        .iter()
        .take(max - 1)
        .map(|r| {
            if r.left == 0xff && r.right >= max as i32 {
                TRow {
                    left: 0xff,
                    right: 0,
                }
            } else {
                *r
            }
        })
        .collect();
    rows.push(TRow {
        left: 0xff,
        right: 0,
    });
    Rc::new(TableProgram { rows })
}

type Program = Option<Rc<TableProgram>>;

fn rows_of(programs: &[Program]) -> usize {
    shared_rows(programs.iter().flatten().map(|p| p.as_ref()))
}

/// The pulse programs, in place, within `budget` rows: while they do not fit,
/// the longest is cut by a quarter (each note keeps its own start and first
/// sweep); only when every one is down to two rows do the least-used take the
/// most-used one's.
fn fit_pulse_programs(pulse: &mut [Program], plans: &[InstrumentPlan], budget: usize) {
    while rows_of(pulse) > budget {
        let mut longest: i64 = -1;
        for (i, p) in pulse.iter().enumerate() {
            if let Some(p) = p {
                if p.rows.len() > 2
                    && (longest < 0
                        || p.rows.len() > pulse[longest as usize].as_ref().unwrap().rows.len())
                {
                    longest = i as i64;
                }
            }
        }
        if longest < 0 {
            break;
        }
        let was = pulse[longest as usize].clone().unwrap();
        let cut = cut_pulse(&was, 2.max((was.rows.len() as f64 * 0.75).floor() as usize));
        // Every plan with that same program is cut alike (they share its rows).
        for slot in pulse.iter_mut() {
            if slot.as_ref().is_some_and(|s| Rc::ptr_eq(s, &was)) {
                *slot = Some(cut.clone());
            }
        }
    }
    let mut by_use: Vec<(usize, usize)> = plans
        .iter()
        .enumerate()
        .map(|(i, p)| (p.members.len(), i))
        .collect();
    by_use.sort_by_key(|&(uses, _)| std::cmp::Reverse(uses));
    while rows_of(pulse) > budget {
        let donor: Program = by_use.iter().find_map(|&(_, i)| pulse[i].clone());
        let victim = by_use.iter().rev().find(|&&(_, i)| {
            pulse[i].is_some()
                && !match (&pulse[i], &donor) {
                    (Some(a), Some(d)) => Rc::ptr_eq(a, d),
                    _ => false,
                }
        });
        let (Some(victim), Some(donor)) = (victim, donor) else {
            break;
        };
        pulse[victim.1] = Some(donor);
    }
}

/// Plans of one envelope (and filter) whose pulse programs sweep alike play one program, the most
/// used plan's: the starting width is the note's own, not an instrument's, in the songs GoatTracker
/// makes (the round trip of its corpus made 40 instruments of 19 without this).
fn share_pulse(pulse: &mut [Program], p: &Plans) {
    let mut classes: Vec<(Vec<i64>, Vec<usize>)> = Vec::new();
    for (i, plan) in p.plans.iter().enumerate() {
        let Some(prog) = &pulse[i] else { continue };
        let t = &p.groups[plan.group].timbre;
        // The same sweep structure, whatever width it starts from (the set-width rows aside).
        let mut key: Vec<i64> = vec![
            t.ad as i64,
            t.sr as i64,
            match t.filter {
                None => -1,
                Some((m, r)) => ((m as i64) << 8) | r as i64,
            },
        ];
        for r in &prog.rows {
            if r.left >= 0x80 && r.left != 0xff {
                key.push(-1);
                key.push(-1);
            } else {
                key.push(r.left as i64);
                key.push(r.right as i64);
            }
        }
        match classes.iter_mut().find(|(k, _)| *k == key) {
            Some((_, idx)) => idx.push(i),
            None => classes.push((key, vec![i])),
        }
    }
    for (_, idx) in &classes {
        let mut donor = idx[0];
        for &i in idx {
            if p.plans[i].members.len() > p.plans[donor].members.len() {
                donor = i;
            }
        }
        let shared = pulse[donor].clone();
        for &i in idx {
            pulse[i] = shared.clone();
        }
    }
}

/// The filter programs, in place, within `budget` rows: the longest is cut by a quarter until they fit.
fn fit_filter_programs(filters: &mut [Program], budget: usize) {
    while rows_of(filters) > budget {
        let mut longest: i64 = -1;
        for (i, f) in filters.iter().enumerate() {
            if let Some(f) = f {
                if f.rows.len() > 4
                    && (longest < 0
                        || f.rows.len() > filters[longest as usize].as_ref().unwrap().rows.len())
                {
                    longest = i as i64;
                }
            }
        }
        if longest < 0 {
            break;
        }
        let was = filters[longest as usize].clone().unwrap();
        let cut = cut_pulse(&was, 4.max((was.rows.len() as f64 * 0.75).floor() as usize));
        for slot in filters.iter_mut() {
            if slot.as_ref().is_some_and(|s| Rc::ptr_eq(s, &was)) {
                *slot = Some(cut.clone());
            }
        }
    }
}

/// The GoatTracker instruments of `plans`, their programs placed in `tables`
/// within GoatTracker's 255 rows a table: wave programs cut where they cost
/// least (`fit_wave_programs`), and when the pulse programs do not fit, the
/// least-used share the most-used one. `row_frames`: the shortest row, which a
/// gate timer must stay below; `name`: each instrument's name.
/// `filter_accents`, when given, gets for each note whose filter program is not its instrument's
/// the `Axx` command that points at it.
#[allow(clippy::too_many_arguments)]
pub fn build_instruments(
    progs: &[Rc<NoteProgram>],
    p: &Plans,
    tables: &mut TableBuilder,
    row_frames: i64,
    name: &dyn Fn(&NoteProgram, bool, usize) -> String,
    reserve_filter_rows: usize,
    filter_accents: Option<&mut Vec<Option<(u8, u8)>>>,
) -> Vec<Instrument> {
    let plans = &p.plans;
    let wave_budget = 255usize.saturating_sub(tables.wave.len());
    let wave = fit_wave_programs(progs, p, wave_budget);
    let timings: Vec<Timing> = plans
        .iter()
        .map(|pl| note_timing(progs, &p.groups, pl, row_frames))
        .collect();
    let mut pulse: Vec<Program> = plans
        .iter()
        .enumerate()
        .map(|(i, pl)| {
            let g = &p.groups[pl.group];
            if g.timbre.legato {
                None
            } else {
                let rep = &progs[g.rep];
                pulse_program(&rep.frames, &rep.pulse, timings[i].gate_timer as i64).map(Rc::new)
            }
        })
        .collect();
    share_pulse(&mut pulse, p);
    fit_pulse_programs(
        &mut pulse,
        plans,
        255usize.saturating_sub(tables.pulse.len()),
    );
    let mut filters: Vec<Program> = vec![None; plans.len()];
    struct Alt {
        members: Vec<usize>,
        program: TableProgram,
    }
    let mut alts: Vec<Alt> = Vec::new();
    let track_of = |m: &NoteProgram| -> Vec<i64> {
        match &m.filter_track {
            Some(t) if t.len() >= m.frames.len() => t.clone(),
            _ => m.frames.iter().map(|f| f.cutoff as i64).collect(),
        }
    };
    // A note's cutoff that is the start of a longer note's follows the same program (it just ends sooner).
    let start_of = |short: &[i64], long: &[i64]| -> bool {
        short.len() <= long.len() && short.iter().zip(long).all(|(a, b)| a == b)
    };
    for (i, plan) in plans.iter().enumerate() {
        let g = &p.groups[plan.group];
        if g.timbre.filter.is_none() {
            continue;
        }
        // The programs of the plan's notes, longest first: the first is the instrument's own, another that
        // at least two notes share points the filter at it (`Axx`, filter_accents).
        struct Cluster {
            track: Vec<i64>,
            members: Vec<usize>,
        }
        let mut clusters: Vec<Cluster> = Vec::new();
        let mut sorted: Vec<usize> = plan.members.clone();
        sorted.sort_by(|&x, &y| track_of(&progs[y]).len().cmp(&track_of(&progs[x]).len()));
        for m in sorted {
            let t = track_of(&progs[m]);
            match clusters.iter_mut().find(|c| start_of(&t, &c.track)) {
                Some(home) => home.members.push(m),
                None => clusters.push(Cluster {
                    track: t,
                    members: vec![m],
                }),
            }
        }
        let rep = &progs[g.rep];
        let main_track = &clusters[0].track;
        let track: &[i64] = rep.filter_track.as_deref().unwrap_or(main_track);
        filters[i] = Some(Rc::new(filter_program(&rep.frames, Some(track))));
        for c in clusters.iter().skip(1) {
            if c.members.len() >= 2 {
                let m0 = &progs[c.members[0]];
                alts.push(Alt {
                    members: c.members.clone(),
                    program: filter_program(&m0.frames, m0.filter_track.as_deref()),
                });
            }
        }
    }
    let budget = 255usize
        .saturating_sub(tables.filter.len())
        .saturating_sub(reserve_filter_rows);
    fit_filter_programs(&mut filters, budget);
    // The instruments' own programs come first; an alternative goes in only if it fits what is left,
    // the most shared first (whole or not at all).
    if let Some(accents) = filter_accents {
        let mut placed: Vec<&TableProgram> = filters.iter().flatten().map(|f| f.as_ref()).collect();
        let mut order: Vec<usize> = (0..alts.len()).collect();
        order.sort_by(|&x, &y| alts[y].members.len().cmp(&alts[x].members.len()));
        for ai in order {
            let a = &alts[ai];
            let mut with: Vec<&TableProgram> = placed.clone();
            with.push(&a.program);
            if shared_rows(with.iter().copied()) > budget {
                continue;
            }
            placed.push(&a.program);
            let ptr = tables.place(Table::Filter, &a.program);
            for &m in &a.members {
                accents[m] = Some((0xa, ptr as u8));
            }
        }
    }
    plans
        .iter()
        .enumerate()
        .map(|(i, plan)| {
            let g = &p.groups[plan.group];
            let rep = &progs[g.rep];
            let members = &plan.members;
            let vib = plan.vibrato;
            let ad = mode(members.iter().map(|&m| progs[m].ad)).unwrap_or(rep.ad);
            let sr = mode(members.iter().map(|&m| progs[m].sr)).unwrap_or(rep.sr);
            let timing = &timings[i];
            Instrument {
                name: name(rep, vib.is_some(), i + 1)
                    .into_bytes()
                    .into_iter()
                    .take(16)
                    .collect(),
                attack: ad >> 4,
                decay: ad & 0x0f,
                sustain: sr >> 4,
                release: sr & 0x0f,
                // A legato note keeps the waveform and gate as they are ($00).
                first_wave: first_wave(progs, &p.groups, plan),
                gate_timer: timing.gate_timer,
                hard_restart: timing.hard_restart,
                no_gate_off: timing.no_gate_off,
                vibrato_delay: vib.map_or(0, |v| v.delay.min(255) as u8),
                wave_ptr: tables.place(Table::Wave, &wave[i]) as u8,
                pulse_ptr: match &pulse[i] {
                    None => 0,
                    Some(pr) => tables.place(Table::Pulse, pr) as u8,
                },
                filter_ptr: match &filters[i] {
                    None => 0,
                    Some(f) => tables.place(Table::Filter, f) as u8,
                },
                speed_ptr: match vib {
                    None => 0,
                    Some(v) => tables.speed_row(v.left as i32, v.right as i32) as u8,
                },
            }
        })
        .collect()
}
