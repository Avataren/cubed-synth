//! The row grid of a transcription: where the rows of the GoatTracker song
//! fall in the trace's frames. A port of `transcribe/grid.ts`.
//!
//! Rows are laid over the notes: each starts where a voice's note needs a row
//! (one frame before the gate goes on: GoatTracker writes a note's pitch on
//! the frame after its gate, the wave table's first row, so the transcribed
//! instrument's first-frame waveform has the gate off and its wave table's
//! first row sets waveform, gate and pitch on the original's frame), and
//! between notes the rows keep the tune's current length. A row's length is
//! GoatTracker's tempo: an `F` command wherever it changes ([`tempo_changes`]).
//! So a steady tempo is one `F`, and Knucklebusters' rows of 9, 9 and 10
//! frames, or Comic Bakery's one short row that shifts its beat, are played
//! frame for frame.
//!
//! A voice whose notes start later in the row than the earliest voice's (Last
//! Ninja's third voice, two frames) keeps that as a delay, played by its
//! instruments' wave tables.

use super::util::OrdMap;

#[derive(Clone, Debug)]
pub struct RowGrid {
    /// The first frame of every row, ascending; row `k` is frames `[starts[k], starts[k + 1])`. May start at -1.
    pub starts: Vec<i64>,
    /// Per voice: its notes' gate goes on at the row's first frame + 1 + delay.
    pub delays: Vec<i64>,
    /// Share of the note starts that land on a row start (the rest are moved or folded into a note).
    pub coverage: f64,
    /// Frames from a row's first to its notes' gate-on (before the voice's delay): 1 when a row starts
    /// a frame early (the gate-off first frame, the wave table's first row on the original's gate-on
    /// frame), 0 when it starts on the original's own first frame, a bare test-bit frame as
    /// GoatTracker's players write it (`firstWave` $09, the wave table's first row on the next).
    pub lead: i64,
}

/// Row lengths are 3 or more frames: GoatTracker's shortest `F` tempo.
pub const MIN_ROW_FRAMES: i64 = 3;
/// GoatTracker's longest `F` tempo.
pub const MAX_ROW_FRAMES: i64 = 127;
const MAX_STEADY_ROW: i64 = 64;
/// A steady tempo that places this share of the note starts is taken whole.
const GOOD_COVERAGE: f64 = 0.995;

/// The length of row `k` (past the end: the last row's).
pub fn row_length(g: &RowGrid, k: i64) -> i64 {
    let s = &g.starts;
    if s.len() < 2 {
        return 6;
    }
    let i = k.min(s.len() as i64 - 2).max(0) as usize;
    s[i + 1] - s[i]
}

/// The first frame of row `k` (past the end: continued at the last length).
pub fn row_start(g: &RowGrid, k: i64) -> i64 {
    let s = &g.starts;
    if k < s.len() as i64 {
        return s[k.max(0) as usize];
    }
    s[s.len() - 1] + (k - s.len() as i64 + 1) * row_length(g, s.len() as i64 - 1)
}

/// The row whose frames contain `frame` (frames before the first row: row 0).
pub fn row_of_frame(g: &RowGrid, frame: i64) -> i64 {
    let s = &g.starts;
    if frame < s[0] {
        return 0;
    }
    let last = s[s.len() - 1];
    if frame >= last {
        return s.len() as i64 - 1 + (frame - last).div_euclid(row_length(g, s.len() as i64 - 1));
    }
    let mut lo = 0usize;
    let mut hi = s.len() - 1;
    while lo < hi {
        let mid = (lo + hi + 1) >> 1;
        if s[mid] <= frame {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo as i64
}

/// Per voice, the phase (mod `t`) that puts most onsets on row starts, and how many it puts there.
fn steady_fit(onsets: &[Vec<i64>], t: i64) -> (Vec<Option<i64>>, i64) {
    let mut phases = Vec::new();
    let mut hits = 0;
    for list in onsets {
        if list.is_empty() {
            phases.push(None);
            continue;
        }
        let mut counts = vec![0i64; t as usize];
        for &o in list {
            counts[(((o % t) + t) % t) as usize] += 1;
        }
        let mut best = 0usize;
        for p in 1..t as usize {
            if counts[p] > counts[best] {
                best = p;
            }
        }
        phases.push(Some(best as i64));
        hits += counts[best];
    }
    (phases, hits)
}

struct Steady {
    t: i64,
    phases: Vec<Option<i64>>,
    hits: i64,
}

/// The largest steady length that places `share` of the onsets, with its phases; `None` if none.
fn steady_tempo(onsets: &[Vec<i64>], share: f64) -> Option<Steady> {
    let total: usize = onsets.iter().map(|l| l.len()).sum();
    for t in (MIN_ROW_FRAMES..=MAX_STEADY_ROW).rev() {
        let (phases, hits) = steady_fit(onsets, t);
        if hits as f64 >= share * total as f64 {
            return Some(Steady { t, phases, hits });
        }
    }
    None
}

/// Per-voice delays for per-voice phases (mod `t`): after the earliest voice's, smallest worst case.
fn delays_for(phases: &[Option<i64>], t: i64, lead: i64) -> (Vec<i64>, i64) {
    let mut start = 0;
    let mut best_worst = i64::MAX;
    for s in 0..t {
        let mut worst = 0;
        for p in phases.iter().flatten() {
            worst = worst.max((((p - lead - s) % t) + t) % t);
        }
        if worst < best_worst {
            best_worst = worst;
            start = s;
        }
    }
    let delays = phases
        .iter()
        .map(|p| match p {
            None => 0,
            Some(p) => (MIN_ROW_FRAMES - 1).min((((p - lead - start) % t) + t) % t),
        })
        .collect();
    (delays, start)
}

/// The most common row length of the tune, window by window (for laying rows
/// between notes). A tune with no steady window (rows of 9, 9 and 10 frames
/// throughout) takes the most common distance between consecutive note starts.
fn typical_length(onsets: &[Vec<i64>], frames: i64) -> i64 {
    let window = 400;
    let mut votes: OrdMap<i64, i64> = OrdMap::new();
    let mut a = 0;
    while a < frames {
        let part: Vec<Vec<i64>> = onsets
            .iter()
            .map(|l| {
                l.iter()
                    .copied()
                    .filter(|&o| o >= a && o < a + window)
                    .collect()
            })
            .collect();
        a += window;
        if part.iter().map(|l| l.len()).sum::<usize>() < 6 {
            continue;
        }
        if let Some(s) = steady_tempo(&part, 0.97) {
            *votes.entry_or_insert_with(s.t, || 0) += 1;
        }
    }
    if votes.is_empty() {
        let mut starts: Vec<i64> = onsets.iter().flatten().copied().collect();
        starts.sort_unstable();
        starts.dedup();
        for i in 1..starts.len() {
            let gap = starts[i] - starts[i - 1];
            if (MIN_ROW_FRAMES..=MAX_STEADY_ROW).contains(&gap) {
                *votes.entry_or_insert_with(gap, || 0) += 1;
            }
        }
    }
    let mut best = 6;
    let mut count = 0;
    for &(t, c) in votes.iter() {
        if c > count || (c == count && t > best) {
            best = t;
            count = c;
        }
    }
    best
}

/// Per-voice delays from how the voices' note starts sit against each other:
/// a voice whose notes start a frame or two after another's (where they start
/// together in the music) is that much later in every row. Relative to the
/// voice with most notes; the earliest voice has delay 0.
fn pairwise_delays(onsets: &[Vec<i64>]) -> Vec<i64> {
    let n = onsets.len();
    let mut reference = 0;
    for (v, l) in onsets.iter().enumerate() {
        if l.len() > onsets[reference].len() {
            reference = v;
        }
    }
    let offset = |v: usize, u: usize| -> Option<i64> {
        let a = &onsets[v];
        let b = &onsets[u];
        if a.is_empty() || b.is_empty() {
            return None;
        }
        let mut counts: OrdMap<i64, i64> = OrdMap::new();
        let mut close = 0;
        let mut j = 0usize;
        for &o in a {
            while j + 1 < b.len() && (b[j + 1] - o).abs() <= (b[j] - o).abs() {
                j += 1;
            }
            let d = o - b[j];
            if d.abs() < MIN_ROW_FRAMES {
                close += 1;
                *counts.entry_or_insert_with(d, || 0) += 1;
            }
        }
        if close < 4 {
            return None;
        }
        // Sorted by count, descending, stably: the first of the highest.
        let mut best: Option<(i64, i64)> = None;
        for &(d, c) in counts.iter() {
            if best.is_none_or(|(_, bc)| c > bc) {
                best = Some((d, c));
            }
        }
        let (d, c) = best?;
        if c as f64 >= 0.6 * close as f64 {
            Some(d)
        } else {
            None
        }
    };
    let mut rel: Vec<Option<i64>> = (0..n)
        .map(|v| {
            if v == reference {
                Some(0)
            } else {
                offset(v, reference)
            }
        })
        .collect();
    // A voice with no clear offset against the reference: via another voice.
    for v in 0..n {
        if rel[v].is_some() {
            continue;
        }
        for u in 0..n {
            if u == v || rel[u].is_none() || u == reference {
                continue;
            }
            if let Some(d) = offset(v, u) {
                rel[v] = Some(d + rel[u].unwrap());
                break;
            }
        }
    }
    let known: Vec<i64> = rel.iter().map(|d| d.unwrap_or(0)).collect();
    let min = *known.iter().min().unwrap_or(&0);
    known
        .iter()
        .map(|d| (d - min).clamp(0, MIN_ROW_FRAMES - 1))
        .collect()
}

/// The grid for per-voice note starts `onsets` (frames, ascending) over a
/// trace of `frames` frames: one steady length when it places 99.5 % of the
/// notes and is the tune's typical row (not a finer grid that happens to fit),
/// else rows laid note by note at the tune's typical length.
pub fn detect_grid(onsets: &[Vec<i64>], frames: i64, lead: i64, steps: &[Vec<i64>]) -> RowGrid {
    let total: usize = onsets.iter().map(|l| l.len()).sum();
    if total == 0 {
        return steady_grid(6, -lead, frames, vec![0; onsets.len()], 1.0, lead);
    }
    let typical = typical_length(onsets, frames);
    let steady_onsets = steady_tempo(onsets, GOOD_COVERAGE);
    let held = held_steps(
        onsets,
        steps,
        frames,
        typical,
        steady_onsets.as_ref().map_or(typical, |s| s.t),
    );
    let steady = if held.is_empty() { steady_onsets } else { None };
    if let Some(steady) = steady.filter(|s| s.t >= typical) {
        let (delays, start) = delays_for(&steady.phases, steady.t, lead);
        let first = onsets
            .iter()
            .enumerate()
            .filter(|(_, l)| !l.is_empty())
            .map(|(v, l)| l[0] - lead - delays[v])
            .min()
            .unwrap();
        let mut origin = start + (first - start).div_euclid(steady.t) * steady.t;
        if origin < -lead {
            origin += steady.t;
        }
        return steady_grid(
            steady.t,
            origin,
            frames,
            delays,
            steady.hits as f64 / total as f64,
            lead,
        );
    }
    let delays = pairwise_delays(onsets);
    let mut anchors: Vec<i64> = onsets
        .iter()
        .enumerate()
        .flat_map(|(v, l)| l.iter().map(|o| o - lead - delays[v]).collect::<Vec<_>>())
        .chain(held.iter().copied())
        .collect();
    anchors.sort_unstable();
    anchors.dedup();
    laid_grid(&anchors, typical, frames, delays, lead)
}

/// A stretch with no note start this many typical rows long is one held note (an arpeggio stepping on a held gate).
const HELD_ROWS: i64 = 4;
/// In it, a run of this many equal steps lays the rows.
const MIN_RUN: usize = 4;

/// Row starts for the stretches where the gates stay on for long (a tune's intro that arpeggiates
/// on a held note) and the voice that steps most does so at a period the tune's rows `t` do not
/// keep (robocop_3: 9 frames against rows of 6; a period that is a multiple or a divisor of the row
/// is played by ties on rows already). Without them the steps fall between rows, where a tie cannot play them.
fn held_steps(
    onsets: &[Vec<i64>],
    steps: &[Vec<i64>],
    frames: i64,
    typical: i64,
    t: i64,
) -> Vec<i64> {
    let mut starts: Vec<i64> = onsets.iter().flatten().copied().collect();
    starts.sort_unstable();
    starts.dedup();
    let mut out: Vec<i64> = Vec::new();
    let mut prev = 0;
    starts.push(frames);
    for &end in &starts {
        if end - prev >= HELD_ROWS * typical {
            let mut best: Vec<i64> = Vec::new();
            for l in steps {
                let inside: Vec<i64> = l
                    .iter()
                    .copied()
                    .filter(|&f| f > prev + 2 && f < end - 2)
                    .collect();
                if inside.len() > best.len() {
                    best = inside;
                }
            }
            for i in 2..best.len() {
                let a = best[i - 2];
                let b = best[i - 1];
                let c = best[i];
                let d = b - a;
                if d != c - b
                    || !(MIN_ROW_FRAMES..=MAX_STEADY_ROW).contains(&d)
                    || d % t == 0
                    || t % d == 0
                {
                    continue;
                }
                out.extend([a, b, c]);
            }
        }
        prev = end;
    }
    out.sort_unstable();
    out.dedup();
    if out.len() >= MIN_RUN {
        out
    } else {
        Vec::new()
    }
}

fn steady_grid(
    t: i64,
    origin: i64,
    frames: i64,
    delays: Vec<i64>,
    coverage: f64,
    lead: i64,
) -> RowGrid {
    let mut starts = Vec::new();
    let mut r = origin;
    while r < frames {
        starts.push(r);
        r += t;
    }
    if starts.is_empty() {
        starts.push(origin);
    }
    RowGrid {
        starts,
        delays,
        coverage,
        lead,
    }
}

/// Rows laid over `anchors` (the frames rows should start at): each row is
/// the current length, shortened or lengthened by up to two frames to start
/// on the next anchor, or cut short at an anchor that comes early; the
/// current length follows the rows that land on anchors.
fn laid_grid(anchors: &[i64], typical: i64, frames: i64, delays: Vec<i64>, lead: i64) -> RowGrid {
    let origin = (-lead).max(anchors[0] - typical * (anchors[0] + 1).div_euclid(typical));
    let mut starts = vec![origin];
    let mut recent: Vec<i64> = Vec::new();
    let mut length = typical;
    let mut i = 0usize;
    let mut hit = if anchors[0] == origin { 1 } else { 0 };
    let mut r = origin;
    while r < frames - 1 {
        while i < anchors.len() && anchors[i] <= r {
            i += 1;
        }
        let a = if i < anchors.len() {
            anchors[i]
        } else {
            i64::MAX
        };
        let next = r + length;
        let end;
        if a < next {
            if a - r >= MIN_ROW_FRAMES {
                end = a;
            } else {
                // Too close to this row's start: the note folds into this row.
                i += 1;
                continue;
            }
        } else if a < next + MIN_ROW_FRAMES && a - r <= MAX_ROW_FRAMES {
            end = a;
        } else {
            end = next;
        }
        starts.push(end);
        if end == a {
            hit += 1;
            recent.push(end - r);
            if recent.len() > 12 {
                recent.remove(0);
            }
            // The current length: the most common of the recent rows that landed on notes.
            let mut counts: OrdMap<i64, i64> = OrdMap::new();
            for &x in &recent {
                *counts.entry_or_insert_with(x, || 0) += 1;
            }
            let mut best = length;
            let mut c = 0;
            for &(x, n) in counts.iter() {
                if n > c || (n == c && x == length) {
                    best = x;
                    c = n;
                }
            }
            length = best;
        }
        r = end;
    }
    RowGrid {
        starts,
        delays,
        coverage: if anchors.is_empty() {
            1.0
        } else {
            hit as f64 / anchors.len() as f64
        },
        lead,
    }
}

/// Rows whose length differs from the row before (and row 0): GoatTracker needs an `F` command there.
/// Returns `(row, length)` pairs.
pub fn tempo_changes(g: &RowGrid, rows: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut prev = -1;
    for k in 0..rows {
        let len = row_length(g, k).clamp(MIN_ROW_FRAMES, MAX_ROW_FRAMES);
        if len != prev {
            out.push((k, len));
        }
        prev = len;
    }
    out
}
