//! The rows of a transcribed subsong and their flat song: per voice a row per
//! grid row (the note, its instrument), the start tempo on row 0 of voice 1
//! (`F`, or funktempo `E` with the two lengths in a speed-table row), and the
//! loop: the trace's own when the capture found the machine repeating, else
//! the shortest repeating tail of the rows. A port of `transcribe/song.ts`.

use super::flat::{FlatCell, FlatPattern, FlatSubsong};
use super::frames::TraceFrames;
use super::grid::{row_of_frame, row_start, RowGrid};
use super::notes::NoteProgram;
use crate::song::{Row, MAX_PATTERN_ROWS, NOTE_FIRST, NOTE_KEY_OFF, NOTE_LAST};
use std::collections::HashMap;

/// Where the loop came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopKind {
    Exact,
    Found,
    None,
}

impl LoopKind {
    pub fn name(self) -> &'static str {
        match self {
            LoopKind::Exact => "exact",
            LoopKind::Found => "found",
            LoopKind::None => "none",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SubsongRows {
    /// Per voice, one row per grid row.
    pub voices: Vec<Vec<Row>>,
    /// Rows the song plays before it loops (the loop's end).
    pub length: usize,
    /// The row the song loops back to.
    pub loop_row: usize,
    /// Where the loop came from.
    pub loop_kind: LoopKind,
}

/// A pitch cell of a voice's pattern (`PitchRow`, its speed placed in the speed table).
#[derive(Clone, Debug)]
pub struct PatternPitch {
    pub voice: usize,
    pub row: usize,
    pub note: Option<i32>,
    pub command: u8,
    pub param: u8,
    pub continued: bool,
}

/// A row's note for GoatTracker note index `index` (clamped to the rows' range C-0..G#7).
fn row_note(index: i32) -> u8 {
    (index + 1).clamp(NOTE_FIRST as i32, NOTE_LAST as i32) as u8
}

/// Where a subsong loops, in rows: it plays rows `[0, length)` and continues at `loop_row`.
#[derive(Clone, Copy, Debug)]
pub struct SongLoop {
    pub loop_row: usize,
    pub length: usize,
    pub kind: LoopKind,
}

/// Where the transcribed subsong loops: the trace's own loop when the capture
/// found the machine repeating (`loop_frame`), else the shortest repeating
/// tail of the notes (`note_keys`: per row, what each voice starts there),
/// else the end of the capture back to the start.
pub fn find_loop(f: &TraceFrames, grid: &RowGrid, note_keys: &[String]) -> SongLoop {
    let length = note_keys.len();
    if let Some(loop_frame) = f.loop_frame {
        // Frames [loop_frame, end) repeat: the rows from the one before the loop frame
        // (a note's row starts a frame before its gate) to the row of the last frame.
        let loop_row = row_of_frame(grid, loop_frame as i64 - 1)
            .clamp(0, length as i64 - 1)
            .max(0) as usize;
        let end = (loop_row as i64 + 1).max(row_of_frame(grid, f.frames as i64 - 1)) as usize;
        return SongLoop {
            loop_row,
            length: end,
            kind: LoopKind::Exact,
        };
    }
    // The last row's notes are cut short by the end of the capture: not compared.
    let keys = &note_keys[..note_keys.len().saturating_sub(1)];
    if let Some((start, period)) = repeating_tail(keys) {
        return SongLoop {
            loop_row: start,
            length: start + period,
            kind: LoopKind::Found,
        };
    }
    SongLoop {
        loop_row: 0,
        length,
        kind: LoopKind::None,
    }
}

/// A tempo or other command to put on a row of the first voice whose command column is free.
#[derive(Clone, Copy, Debug)]
pub struct RowCommand {
    pub row: usize,
    pub command: u8,
    pub param: u8,
}

/// The rows of a subsong, `loop.length` long: `instrument_of` gives each
/// note's instrument number (1-based; by index into `programs`), `tempo` the
/// F commands, `commands` the song's other commands, `accents` per program the
/// filter pointer command it takes.
#[allow(clippy::too_many_arguments)]
pub fn subsong_rows(
    lp: &SongLoop,
    grid: &RowGrid,
    programs: &[NoteProgram],
    instrument_of: &[usize],
    gate_timer_of: &dyn Fn(usize) -> i64,
    tempo: &[(i64, i64)],
    commands: &[RowCommand],
    pitch: &[PatternPitch],
    accents: &[Option<(u8, u8)>],
) -> SubsongRows {
    let length = lp.length;
    let mut voices: Vec<Vec<Row>> = (0..3).map(|_| vec![Row::default(); length]).collect();
    // Every note names its instrument: a pattern then plays the same wherever it
    // is used (GoatTracker's packer drops the repeats).
    for (i, p) in programs.iter().enumerate() {
        if p.note.row >= length as i64 {
            continue;
        }
        voices[p.note.voice][p.note.row as usize] = Row {
            note: row_note(p.base),
            instrument: instrument_of.get(i).copied().unwrap_or(0) as u8,
            command: 0,
            param: 0,
        };
    }
    // The pitch effects (`pitch.rs`): a tie or glide's note (no instrument: the sounding one goes on),
    // and its command; never over a note of the voice's own.
    let mut yielding: Vec<(usize, usize)> = Vec::new();
    for x in pitch {
        if x.row >= length {
            continue;
        }
        let v = &mut voices[x.voice];
        let cell = v[x.row];
        if x.note.is_some() && cell.note != 0 {
            continue;
        }
        let mut next = cell;
        if let Some(n) = x.note {
            next.note = row_note(n);
        }
        next.command = x.command;
        next.param = x.param;
        v[x.row] = next;
        if x.continued {
            yielding.push((x.voice, x.row));
        }
    }
    // Key-offs: GoatTracker clears the gate when it reads the row, the gate timer's
    // frames before the row starts; the row whose reading lands nearest the original's.
    for (i, p) in programs.iter().enumerate() {
        let Some(key_off) = p.key_off else { continue };
        if p.note.row >= length as i64 {
            continue;
        }
        let ins = instrument_of.get(i).copied().unwrap_or(0);
        let at = p.note.tick0 + key_off + gate_timer_of(ins);
        let k = row_of_frame(grid, at);
        let row = if at - row_start(grid, k) <= row_start(grid, k + 1) - at {
            k
        } else {
            k + 1
        };
        let v = &mut voices[p.note.voice];
        if row <= p.note.row || row >= length as i64 {
            continue;
        }
        // Only into an empty row, and before the voice's next note (a tie's is the same note going on).
        let mut clear = v[row as usize].note == 0;
        for r in p.note.row + 1..row {
            if v[r as usize].note != 0 && v[r as usize].command != 3 {
                clear = false;
            }
        }
        if clear {
            v[row as usize].note = NOTE_KEY_OFF;
        }
    }
    // Tempo commands (F sets every voice's) and the song's other commands, on the
    // first voice whose command is free.
    let all: Vec<RowCommand> = tempo
        .iter()
        .map(|&(row, len)| RowCommand {
            row: row as usize,
            command: 0xf,
            param: len as u8,
        })
        .chain(commands.iter().copied())
        .collect();
    for RowCommand {
        row,
        command,
        param,
    } in all
    {
        if row >= length {
            continue;
        }
        // A free command column, else one that only keeps a running effect going.
        let v = (0..3)
            .find(|&v| voices[v][row].command == 0)
            .or_else(|| (0..3).find(|&v| yielding.contains(&(v, row))));
        if let Some(v) = v {
            voices[v][row].command = command;
            voices[v][row].param = param;
        }
    }
    // A note that points the filter at another program than its instrument's (`Axx`): last, into a
    // column still free (the tempo and the song's commands come first).
    for (i, p) in programs.iter().enumerate() {
        let Some(Some((command, param))) = accents.get(i) else {
            continue;
        };
        if p.note.row >= length as i64 {
            continue;
        }
        let cell = &mut voices[p.note.voice][p.note.row as usize];
        if cell.command == 0 {
            cell.command = *command;
            cell.param = *param;
        }
    }
    SubsongRows {
        voices,
        length,
        loop_row: lp.loop_row,
        loop_kind: lp.kind,
    }
}

/// The shortest period whose repetition runs to the end of `keys`, and where
/// it starts: at least two periods seen, at least 8 rows long.
fn repeating_tail(keys: &[String]) -> Option<(usize, usize)> {
    let length = keys.len();
    let mut best: Option<(usize, usize)> = None;
    let mut p = 8;
    while p * 2 <= length {
        let mut s = length - p;
        while s > 0 && keys[s - 1] == keys[s - 1 + p] {
            s -= 1;
        }
        if length - s >= 2 * p {
            let better = match best {
                None => true,
                Some((bs, bp)) => s + p < bs + bp,
            };
            if better {
                best = Some((s, p));
            }
        }
        p += 1;
    }
    best
}

/// The flat subsong of `rows`: positions of `pattern_rows` rows (the part
/// before the loop and the loop each cut on their own, so the loop starts a
/// position), identical positions sharing one pattern.
pub fn flat_subsong(rows: &SubsongRows, pattern_rows: usize) -> FlatSubsong {
    let size = pattern_rows.clamp(1, MAX_PATTERN_ROWS);
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let cut = |cuts: &mut Vec<(usize, usize)>, from: usize, to: usize| {
        let mut r = from;
        while r < to {
            cuts.push((r, to.min(r + size)));
            r += size;
        }
    };
    cut(&mut cuts, 0, rows.loop_row);
    let loop_index = cuts.len();
    cut(&mut cuts, rows.loop_row, rows.length);
    let mut patterns: Vec<FlatPattern> = Vec::new();
    let mut by_key: HashMap<Vec<[u8; 4]>, usize> = HashMap::new();
    let mut sequence: Vec<usize> = Vec::new();
    for (from, to) in cuts {
        let key: Vec<[u8; 4]> = rows
            .voices
            .iter()
            .flat_map(|v| {
                v[from..to]
                    .iter()
                    .map(|x| [x.note, x.instrument, x.command, x.param])
                    .chain(std::iter::once([255, 255, 255, 255]))
            })
            .collect();
        let id = match by_key.get(&key) {
            Some(&id) => id,
            None => {
                let id = patterns.len();
                by_key.insert(key, id);
                patterns.push(FlatPattern {
                    rows: to - from,
                    cells: rows
                        .voices
                        .iter()
                        .map(|v| FlatCell {
                            transpose: 0,
                            rows: v[from..to].to_vec(),
                            start: true,
                        })
                        .collect(),
                });
                id
            }
        };
        sequence.push(id);
    }
    let restart = loop_index.min(sequence.len().saturating_sub(1));
    FlatSubsong {
        patterns,
        sequence,
        restarts: vec![restart; 3],
    }
}
