//! A `.sid` of any player, transcribed into a GoatTracker song: every subsong
//! is run on the emulated C64, its register trace cut into rows and notes,
//! the notes grouped into instruments, and the rows compiled through the flat
//! song model, the one the editor edits. Subsongs share the instruments and
//! tables; the start song comes first, and a subsong that no longer fits
//! GoatTracker's limits (63 instruments, 255 rows a table, 208 patterns, 32
//! subsongs) or runs at another speed is left out with a note. A port of
//! `transcribe/index.ts`.

use super::capture::{capture_sid_with, CaptureOptions, SidTrace, CANCELLED, SID_EVENT_GATE_ON};
use super::export::gt_packed_pattern_size;
use super::flat::{compile_flat_song, FlatSubsong};
use super::fold::fold_instruments;
use super::frames::{estimate_tuning, trace_frames, TraceFrames};
use super::grid::{detect_grid, row_length, row_of_frame, tempo_changes, RowGrid};
use super::instruments::{
    build_instruments, group_notes, Plans, TRow, Table, TableBuilder, TableProgram,
    MAX_TRANSCRIBED_INSTRUMENTS,
};
use super::notes::{
    filter_driver, note_base, note_programs, onsets_of, place_notes, split_lines, NoteProgram,
    MAX_FILTER_FRAMES,
};
use super::pitch::{plan_pitch, PitchPlan, PitchRow};
use super::song::{
    find_loop, flat_subsong, subsong_rows, LoopKind, PatternPitch, RowCommand, SongLoop,
    SubsongRows,
};
use super::util::{js_round, OrdMap};
use crate::psid::c64::Kernal;
use crate::psid::file::{PsidFile, PsidSidModel};
use crate::song::{
    Instrument, OrderEntry, Orderlist, Pattern, Row, SidSong, Subsong, TableRow, Tables,
    MAX_INSTRUMENTS, MAX_SPEED_MULTIPLIER, MAX_SUBSONGS, SID_CHANNELS, SONG_FILE_VERSION,
};
use crate::SidModel;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// What to transcribe.
#[derive(Clone, Debug, Default)]
pub struct TranscribeOptions {
    /// PSID subsongs to import (0-based), in order. Default: the start song, then the others.
    pub subsongs: Option<Vec<usize>>,
    /// Capture length cap per subsong, seconds (default 600).
    pub max_seconds: Option<f64>,
    /// The stand-in KERNAL the tune runs on (default `Kernal::Extended`; `Kernal::Daw` is the DAW's own).
    pub kernal: Kernal,
    /// Set from another thread to stop the import between ticks of a capture and between subsongs.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// How one subsong went.
#[derive(Clone, Debug)]
pub struct SubsongReport {
    /// The PSID subsong (0-based).
    pub subsong: usize,
    /// The GoatTracker subsong it became, or `None` when it was left out.
    pub gt_subsong: Option<usize>,
    pub reason: Option<String>,
    pub frames: Option<usize>,
    pub grid: Option<RowGrid>,
    pub notes: Option<usize>,
    pub folded: Option<i64>,
    pub loop_kind: Option<LoopKind>,
    pub rows: Option<usize>,
    /// The capture (for measuring the result against).
    pub trace: Option<Rc<SidTrace>>,
    /// It did not loop and was cut to this many rows to fit GoatTracker.
    pub cut_rows: Option<usize>,
}

impl SubsongReport {
    fn left_out(subsong: usize, reason: String) -> SubsongReport {
        SubsongReport {
            subsong,
            gt_subsong: None,
            reason: Some(reason),
            frames: None,
            grid: None,
            notes: None,
            folded: None,
            loop_kind: None,
            rows: None,
            trace: None,
            cut_rows: None,
        }
    }
}

pub struct Transcription {
    pub song: SidSong,
    pub reports: Vec<SubsongReport>,
    pub notes: Vec<String>,
}

pub struct TranscribeFailure {
    pub reason: String,
    pub reports: Vec<SubsongReport>,
}

/// GoatTracker's 1x frame rate (PAL, `player.rs` `frame_cycles`).
const PAL_FRAME_HZ: f64 = 985_248.0 / 19_656.0;

const DEFAULT_TEMPO: u8 = 6;

fn wave_name(w: u8) -> String {
    match w {
        0x10 => "tri".to_string(),
        0x20 => "saw".to_string(),
        0x40 => "pulse".to_string(),
        0x80 => "noise".to_string(),
        w => format!("${w:x}"),
    }
}

fn instrument_name(rep: &NoteProgram, vibrato: bool, n: usize) -> String {
    let mut waves: Vec<u8> = Vec::new();
    for f in &rep.frames {
        let w = f.ctrl & 0xf0;
        if w != 0 && !waves.contains(&w) {
            waves.push(w);
        }
    }
    let parts: Vec<String> = waves.iter().take(2).map(|&w| wave_name(w)).collect();
    let head = if parts.is_empty() {
        "silent".to_string()
    } else {
        parts.join("+")
    };
    let s = format!("{head}{} {n}", if vibrato { " vib" } else { "" });
    s.chars().take(16).collect()
}

#[derive(Clone)]
struct Prepared {
    subsong: usize,
    /// The voice whose notes drive the filter, or `None`.
    driver: Option<usize>,
    trace: Rc<SidTrace>,
    frames: Rc<TraceFrames>,
    grid: Rc<RowGrid>,
    lp: SongLoop,
    /// The notes of one pass of the song (up to its loop's end).
    programs: Rc<Vec<Rc<NoteProgram>>>,
    /// Per voice, the pitch effects' pattern cells (`pitch.rs`).
    pitch: Rc<Vec<Vec<PitchRow>>>,
    /// Pitch is played by pattern effects (else by the instruments alone, to take less room).
    effects: bool,
    folded: i64,
}

/// Per row, what each voice starts there: the note, its envelope and first frames.
fn note_keys(rows: usize, programs: &[NoteProgram]) -> Vec<String> {
    let mut cells: Vec<Vec<String>> = (0..3).map(|_| vec![".".to_string(); rows]).collect();
    for p in programs {
        if p.note.row >= rows as i64 {
            continue;
        }
        let head = p
            .frames
            .iter()
            .take(4)
            .map(|x| x.ctrl.to_string())
            .collect::<Vec<_>>()
            .join(",");
        cells[p.note.voice][p.note.row as usize] = format!("{}/{}/{}/{}", p.base, p.ad, p.sr, head);
    }
    (0..rows)
        .map(|r| format!("{}|{}|{}", cells[0][r], cells[1][r], cells[2][r]))
        .collect()
}

/// Whether the tune's notes start on a bare test-bit frame (the gate on with the test bit set, the
/// waveform on the next frame), as GoatTracker's player writes them: then a row starts on that
/// frame (`RowGrid::lead` 0) and its instrument's `first_wave` is that frame, with no wave row for it.
fn start_lead(f: &TraceFrames, onsets: &[Vec<i64>]) -> i64 {
    let mut bare = 0usize;
    let mut total = 0usize;
    for (v, list) in onsets.iter().enumerate() {
        let ctrl = &f.voices[v].ctrl;
        for &o in list {
            total += 1;
            let o = o as usize;
            if ctrl[o] & 0x08 != 0 && o + 1 < f.frames && ctrl[o + 1] & 0x08 == 0 {
                bare += 1;
            }
        }
    }
    if total > 0 && bare as f64 >= 0.9 * total as f64 {
        0
    } else {
        1
    }
}

/// Per voice, the frames where a held, audible note's frequency changes (not a note start).
fn pitch_steps(f: &TraceFrames) -> Vec<Vec<i64>> {
    f.voices
        .iter()
        .map(|v| {
            let mut out = Vec::new();
            for i in 1..f.frames {
                let c = v.ctrl[i];
                let p = v.ctrl[i - 1];
                if c & 1 == 0 || p & 1 == 0 || c & 0xf0 == 0 || c & 0x08 != 0 || p & 0x08 != 0 {
                    continue;
                }
                if v.freq[i] != v.freq[i - 1] {
                    out.push(i as i64);
                }
            }
            out
        })
        .collect()
}

fn prepare(
    file: &PsidFile,
    subsong: usize,
    options: &TranscribeOptions,
) -> Result<Prepared, String> {
    let trace = capture_sid_with(
        file,
        &CaptureOptions {
            subsong,
            max_ticks: None,
            max_seconds: options.max_seconds,
            cancel: options.cancel.clone(),
        },
        options.kernal,
    )?;
    transcribed(subsong, Rc::new(trace), true)
}

/// The rows and notes of a captured subsong; `effects`: its pitch as pattern
/// effects (`pitch.rs`), or, when the song must be smaller to fit
/// GoatTracker, as the instruments' wave tables alone.
fn transcribed(subsong: usize, trace: Rc<SidTrace>, effects: bool) -> Result<Prepared, String> {
    let frames = trace_frames(&trace);
    let onsets = onsets_of(&frames);
    if onsets.iter().all(|l| l.is_empty()) {
        return Err("plays no notes".to_string());
    }
    let grid = detect_grid(
        &onsets,
        frames.frames as i64,
        start_lead(&frames, &onsets),
        &pitch_steps(&frames),
    );
    let tuning = estimate_tuning(&frames);
    let notes = split_lines(
        &frames,
        &grid,
        &place_notes(&frames, &grid, &onsets),
        tuning,
    );
    let plans: Vec<PitchPlan> = if effects {
        notes
            .iter()
            .map(|list| {
                plan_pitch(
                    &frames,
                    &grid,
                    list,
                    &|n| note_base(&frames, &grid, n, tuning),
                    tuning,
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut all: Vec<NoteProgram> = notes
        .iter()
        .enumerate()
        .flat_map(|(v, list)| note_programs(&frames, list, tuning, &grid, plans.get(v)))
        .collect();
    let driver = filter_driver(&frames, &onsets);
    for p in all.iter_mut() {
        if Some(p.note.voice) != driver {
            continue;
        }
        p.filter = true;
        let from = 0i64.max(p.note.tick0 + 1);
        let to = (frames.frames as i64)
            .min(p.note.end)
            .min(from + MAX_FILTER_FRAMES);
        p.filter_track = Some(
            (0..0i64.max(to - from))
                .map(|k| (frames.cutoff[(from + k) as usize] >> 3) as i64)
                .collect(),
        );
    }
    let rows = (row_of_frame(&grid, frames.frames as i64 - 1) + 1) as usize;
    let lp = find_loop(&frames, &grid, &note_keys(rows, &all));
    let programs: Vec<Rc<NoteProgram>> = all
        .into_iter()
        .filter(|p| p.note.row < lp.length as i64)
        .map(Rc::new)
        .collect();
    let folded: i64 = notes
        .iter()
        .flatten()
        .map(|x| {
            if x.row < lp.length as i64 {
                x.folded
            } else {
                0
            }
        })
        .sum();
    let pitch: Vec<Vec<PitchRow>> = plans
        .iter()
        .map(|p| {
            p.rows
                .iter()
                .filter(|x| x.row < lp.length as i64)
                .cloned()
                .collect()
        })
        .collect();
    Ok(Prepared {
        subsong,
        driver,
        trace,
        frames: Rc::new(frames),
        grid: Rc::new(grid),
        lp,
        programs: Rc::new(programs),
        pitch: Rc::new(pitch),
        effects,
        folded,
    })
}

/// `x` with `n` decimals as JavaScript's `toFixed` writes it.
fn to_fixed(x: f64, n: usize) -> String {
    format!("{x:.n$}")
}

/// Transcribe `file` into a GoatTracker song. Never panics for a tune's behaviour.
pub fn transcribe_psid(
    file: &PsidFile,
    options: &TranscribeOptions,
) -> Result<Transcription, TranscribeFailure> {
    let start = file.start_subsong();
    let order: Vec<usize> = options.subsongs.clone().unwrap_or_else(|| {
        std::iter::once(start)
            .chain((0..file.songs as usize).filter(|&i| i != start))
            .collect()
    });
    let mut reports: Vec<SubsongReport> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut preps: Vec<Prepared> = Vec::new();
    let mut speed_multiplier: Option<u8> = None;

    for s in order {
        if preps.len() >= MAX_SUBSONGS {
            reports.push(SubsongReport::left_out(
                s,
                format!("GoatTracker holds {MAX_SUBSONGS} subsongs"),
            ));
            continue;
        }
        let prep = match prepare(file, s, options) {
            Ok(p) => p,
            Err(reason) if reason == CANCELLED => {
                return Err(TranscribeFailure { reason, reports });
            }
            Err(reason) => {
                reports.push(SubsongReport::left_out(s, reason));
                continue;
            }
        };
        let mult = (js_round(prep.frames.rate_hz / PAL_FRAME_HZ)
            .clamp(1.0, MAX_SPEED_MULTIPLIER as f64)) as u8;
        match speed_multiplier {
            None => {
                speed_multiplier = Some(mult);
                let gt_hz = PAL_FRAME_HZ * mult as f64;
                let off = prep.frames.rate_hz / gt_hz - 1.0;
                if off.abs() > 0.01 {
                    notes.push(format!(
                        "The tune's player runs at {} Hz; GoatTracker plays it at {} Hz ({}x), {}% {}.",
                        to_fixed(prep.frames.rate_hz, 2),
                        to_fixed(gt_hz, 2),
                        mult,
                        to_fixed((off * 100.0).abs(), 1),
                        if off > 0.0 { "slower" } else { "faster" }
                    ));
                }
            }
            Some(m) if m != mult => {
                reports.push(SubsongReport::left_out(
                    s,
                    format!("{mult}x speed; the song plays at {m}x"),
                ));
                continue;
            }
            Some(_) => {}
        }
        preps.push(prep);
    }
    let Some(speed_multiplier) = speed_multiplier.filter(|_| !preps.is_empty()) else {
        let reason = {
            let parts: Vec<String> = reports
                .iter()
                .map(|r| {
                    format!(
                        "subsong {}: {}",
                        r.subsong + 1,
                        r.reason.clone().unwrap_or_else(|| "?".to_string())
                    )
                })
                .collect();
            if parts.is_empty() {
                "the file has no subsongs".to_string()
            } else {
                parts.join("; ")
            }
        };
        return Err(TranscribeFailure { reason, reports });
    };

    // All the subsongs that fit: while the song does not, a subsong that does not
    // loop is cut shorter first, then subsongs are left out from the last.
    let mut last_reason = String::new();
    let mut current: Vec<Prepared> = preps.clone();
    let mut cut: OrdMap<usize, usize> = OrdMap::new();
    // Per-note filter pointers (`Axx`) make patterns unique: first try with them, and without them
    // before anything is cut or left out.
    let mut point_filter = true;
    loop {
        match build(file, &current, speed_multiplier, point_filter) {
            Ok((song, rows)) => {
                for prep in &preps {
                    let at = current.iter().position(|p| p.subsong == prep.subsong);
                    let Some((at, r)) = at.and_then(|a| rows.get(a).map(|r| (a, r))) else {
                        reports.push(SubsongReport::left_out(
                            prep.subsong,
                            format!("no room left in GoatTracker's limits: {last_reason}"),
                        ));
                        continue;
                    };
                    let cut_rows = cut.get(&prep.subsong).copied();
                    if let Some(cut_rows) = cut_rows {
                        let c = &current[at];
                        notes.push(format!(
                            "Subsong {} does not repeat within its first {} s, and only its first {} rows fit GoatTracker.",
                            prep.subsong + 1,
                            js_round(c.frames.frames as f64 / c.frames.rate_hz),
                            cut_rows
                        ));
                    }
                    reports.push(SubsongReport {
                        subsong: prep.subsong,
                        gt_subsong: Some(at),
                        reason: None,
                        frames: Some(prep.frames.frames),
                        grid: Some((*prep.grid).clone()),
                        notes: Some(current[at].programs.len()),
                        folded: Some(prep.folded),
                        loop_kind: Some(r.loop_kind),
                        rows: Some(r.length),
                        trace: Some(prep.trace.clone()),
                        cut_rows,
                    });
                }
                return Ok(Transcription {
                    song,
                    reports,
                    notes,
                });
            }
            Err(reason) => last_reason = reason,
        }
        if point_filter {
            point_filter = false;
            continue;
        }
        // A subsong that does not loop is cut to make room, but only once while other subsongs can still be
        // left out instead: halved again and again, the start song shrank to a few rows (rubicon: 3001 rows
        // to 46) to keep jingles.
        let mut longest: Option<usize> = None;
        for (i, p) in current.iter().enumerate() {
            let ok = p.lp.kind == LoopKind::None
                && p.lp.length > 64
                && (current.len() == 1 || cut.get(&p.subsong).is_none());
            if ok && longest.is_none_or(|l| p.lp.length > current[l].lp.length) {
                longest = Some(i);
            }
        }
        if let Some(i) = longest {
            let p = current[i].clone();
            // First the same rows with its pitch in the instruments alone: fewer distinct patterns.
            let plain = if p.effects {
                transcribed(p.subsong, p.trace.clone(), false).ok()
            } else {
                None
            };
            if let Some(plain) = plain.filter(|pl| pl.lp.kind == LoopKind::None) {
                current[i] = plain;
                continue;
            }
            let length = p.lp.length / 2;
            let mut next = p.clone();
            next.lp = SongLoop {
                loop_row: 0,
                length,
                kind: LoopKind::None,
            };
            next.programs = Rc::new(
                p.programs
                    .iter()
                    .filter(|x| x.note.row < length as i64)
                    .cloned()
                    .collect(),
            );
            current[i] = next;
            *cut.entry_or_insert_with(p.subsong, || 0) = length;
            continue;
        }
        if current.len() > 1 {
            // The most subsongs (from the first) that fit: a binary search, since a build is costly.
            let mut lo = 1usize;
            let mut hi = current.len() - 1;
            let mut fitting: Option<Vec<Prepared>> = None;
            while lo <= hi {
                let mid = (lo + hi) >> 1;
                match build(file, &current[..mid], speed_multiplier, point_filter) {
                    Ok(_) => {
                        fitting = Some(current[..mid].to_vec());
                        lo = mid + 1;
                    }
                    Err(reason) => {
                        last_reason = reason;
                        if mid == 0 {
                            break;
                        }
                        hi = mid - 1;
                    }
                }
            }
            if let Some(f) = fitting {
                current = f;
                continue;
            }
            current.truncate(1);
            continue;
        }
        return Err(TranscribeFailure {
            reason: last_reason,
            reports,
        });
    }
}

/// A filter no voice's notes drive but the song uses (some voice routed through
/// it in a tenth of the frames): set once on row 0 (`A`, a filter program of
/// its most common mode, resonance and routing and its median cutoff).
fn static_filter(prep: &Prepared, tables: &mut TableBuilder) -> Vec<RowCommand> {
    if prep.driver.is_some() {
        return Vec::new();
    }
    let f = &prep.frames;
    let routed: Vec<usize> = (0..f.frames).filter(|&i| f.resonance[i] & 7 != 0).collect();
    if (routed.len() as f64) < f.frames as f64 / 10.0 {
        return Vec::new();
    }
    let common = |values: Vec<i32>| -> i32 {
        let mut counts: Vec<(i32, usize)> = Vec::new();
        for v in values {
            match counts.iter_mut().find(|(k, _)| *k == v) {
                Some(e) => e.1 += 1,
                None => counts.push((v, 1)),
            }
        }
        // Sorted by count, descending, stably: the first of the highest.
        let mut best = counts[0];
        for &c in &counts {
            if c.1 > best.1 {
                best = c;
            }
        }
        best.0
    };
    let mut cutoffs: Vec<i32> = routed.iter().map(|&i| (f.cutoff[i] >> 3) as i32).collect();
    cutoffs.sort_unstable();
    let program = TableProgram {
        rows: vec![
            TRow {
                left: 0x80
                    | (common(
                        routed
                            .iter()
                            .map(|&i| ((f.mode_volume[i] >> 4) & 7) as i32)
                            .collect(),
                    ) << 4),
                right: common(routed.iter().map(|&i| f.resonance[i] as i32).collect()),
            },
            TRow {
                left: 0x00,
                right: cutoffs[cutoffs.len() >> 1],
            },
            TRow {
                left: 0xff,
                right: 0,
            },
        ],
    };
    vec![RowCommand {
        row: 0,
        command: 0xa,
        param: tables.place(Table::Filter, &program) as u8,
    }]
}

/// An instrument's name numbered `n` (its number after `fold_instruments`).
fn renumbered(name: &[u8], n: usize) -> Vec<u8> {
    // `name.replace(/ ?\d*$/, '')`
    let mut end = name.len();
    while end > 0 && name[end - 1].is_ascii_digit() {
        end -= 1;
    }
    if end > 0 && name[end - 1] == b' ' {
        end -= 1;
    }
    let mut out = name[..end].to_vec();
    out.extend_from_slice(format!(" {n}").as_bytes());
    out.truncate(16);
    out
}

type Built = Result<(SidSong, Vec<SubsongRows>), String>;

/// One song of `preps`: instruments grouped across all of them, tables
/// fitted, rows compiled; then every instrument that differs from another in
/// one thing only folded into it, as a command on its rows (`fold_instruments`).
/// (Grouping into more than 63 before the fold makes more table programs than
/// GoatTracker's 255 rows hold: they are cut, and the song sounds no closer.)
fn build(file: &PsidFile, preps: &[Prepared], speed_multiplier: u8, point_filter: bool) -> Built {
    let mut tables = TableBuilder::new();
    // A gate timer must stay below every row's length (GoatTracker reads the next row
    // when its counter meets the gate timer).
    let mut row_frames = 127i64;
    for prep in preps {
        for k in 0..prep.lp.length {
            row_frames = row_frames.min(row_length(&prep.grid, k as i64));
        }
    }
    let arena: Vec<Rc<NoteProgram>> = preps
        .iter()
        .flat_map(|p| p.programs.iter().cloned())
        .collect();
    let plans: Plans = group_notes(&arena, MAX_TRANSCRIBED_INSTRUMENTS);
    let mut accents: Vec<Option<(u8, u8)>> = vec![None; arena.len()];
    let built = build_instruments(
        &arena,
        &plans,
        &mut tables,
        row_frames,
        &instrument_name,
        3 * preps.len(),
        if point_filter {
            Some(&mut accents)
        } else {
            None
        },
    );
    if !tables.fits() {
        return Err("GoatTracker's tables are full".to_string());
    }
    let rows = song_rows(
        preps,
        &plans,
        &built,
        &mut tables,
        speed_multiplier,
        &accents,
        arena.len(),
    );
    let (folded_instruments, folded_songs) = fold_instruments(&built, rows);
    let instruments: Vec<Instrument> = folded_instruments
        .into_iter()
        .enumerate()
        .map(|(i, ins)| Instrument {
            name: renumbered(&ins.name, i + 1),
            ..ins
        })
        .collect();
    compile_song(
        file,
        preps,
        speed_multiplier,
        instruments,
        &tables,
        folded_songs,
    )
}

/// The rows of every subsong of `preps`, their notes on the instruments `instruments` (1-based) of `plans`.
fn song_rows(
    preps: &[Prepared],
    plans: &Plans,
    instruments: &[Instrument],
    tables: &mut TableBuilder,
    speed_multiplier: u8,
    accents: &[Option<(u8, u8)>],
    arena_len: usize,
) -> Vec<SubsongRows> {
    let mut instrument_of = vec![0usize; arena_len];
    for (i, plan) in plans.plans.iter().enumerate() {
        for &m in &plan.members {
            instrument_of[m] = i + 1;
        }
    }
    let mut offset = 0usize;
    preps
        .iter()
        .map(|prep| {
            let n = prep.programs.len();
            let off = offset;
            offset += n;
            // Tempo: where the row length changes, and again at the loop row (the song comes
            // back there from its end); row 0 at 1x may rely on GoatTracker's start tempo 6.
            let mut tempo: Vec<(i64, i64)> = tempo_changes(&prep.grid, prep.lp.length as i64)
                .into_iter()
                .filter(|&(row, length)| {
                    !(row == 0 && length == DEFAULT_TEMPO as i64 && speed_multiplier == 1)
                })
                .collect();
            let loop_row = prep.lp.loop_row;
            if loop_row > 0 && !tempo.iter().any(|&(row, _)| row == loop_row as i64) {
                tempo.push((loop_row as i64, row_length(&prep.grid, loop_row as i64)));
            }
            let mut pitch: Vec<PatternPitch> = Vec::new();
            for (voice, list) in prep.pitch.iter().enumerate() {
                for x in list {
                    let param = if x.command == 4 {
                        let (l, r) = x.vibrato.unwrap();
                        tables.speed_row(l as i32, r as i32)
                    } else if x.speed == 0 {
                        0
                    } else {
                        tables.speed_row(
                            0x7f.min((x.speed >> 8) as i32),
                            if x.speed >= 0x8000 {
                                0xff
                            } else {
                                (x.speed & 0xff) as i32
                            },
                        )
                    };
                    pitch.push(PatternPitch {
                        voice,
                        row: x.row as usize,
                        note: x.note,
                        command: x.command,
                        param: param as u8,
                        continued: x.continued,
                    });
                }
            }
            let commands = static_filter(prep, tables);
            let progs: Vec<NoteProgram> = prep.programs.iter().map(|p| (**p).clone()).collect();
            subsong_rows(
                &prep.lp,
                &prep.grid,
                &progs,
                &instrument_of[off..off + n],
                &|ins: usize| {
                    if ins >= 1 && ins <= instruments.len() {
                        instruments[ins - 1].gate_timer as i64
                    } else {
                        2
                    }
                },
                &tempo,
                &commands,
                &pitch,
                &accents[off..off + n],
            )
        })
        .collect()
}

fn latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| c as u32 as u8).collect()
}

fn text32(s: &str) -> Vec<u8> {
    let mut v = latin1(s);
    v.truncate(32);
    v
}

/// The song of `rows_list` (one per subsong of `preps`) on `instruments` and `tables`, compiled through the flat model.
fn compile_song(
    file: &PsidFile,
    preps: &[Prepared],
    speed_multiplier: u8,
    instruments: Vec<Instrument>,
    tables: &TableBuilder,
    rows_list: Vec<SubsongRows>,
) -> Built {
    if !tables.fits() {
        return Err("GoatTracker's tables are full".to_string());
    }
    let to_rows = |t: &[TRow]| -> Vec<TableRow> {
        t.iter()
            .map(|r| TableRow {
                left: r.left as u8,
                right: r.right as u8,
            })
            .collect()
    };
    debug_assert!(instruments.len() <= MAX_INSTRUMENTS);
    let base = SidSong {
        version: SONG_FILE_VERSION,
        model: if file.sid_model == PsidSidModel::Sid8580 {
            SidModel::Sid8580
        } else {
            SidModel::Sid6581
        },
        channels: SID_CHANNELS as u8,
        speed_multiplier,
        tempo: DEFAULT_TEMPO,
        name: text32(&file.name),
        author: text32(&file.author),
        copyright: text32(&file.released),
        subsongs: vec![Subsong {
            orderlists: (0..3)
                .map(|_| Orderlist {
                    entries: vec![OrderEntry {
                        pattern: 0,
                        transpose: 0,
                        repeat: 1,
                    }],
                    restart: 0,
                })
                .collect(),
        }],
        patterns: vec![Pattern {
            rows: vec![Row::default()],
        }],
        instruments,
        tables: Tables {
            wave: to_rows(&tables.wave),
            pulse: to_rows(&tables.pulse),
            filter: to_rows(&tables.filter),
            speed: to_rows(&tables.speed),
        },
    };

    // Per subsong, the longest patterns that pack for GoatTracker's player (256
    // bytes) and keep its orderlists within 254 bytes; then all compiled together.
    let mut flats: Vec<FlatSubsong> = Vec::new();
    for (i, rows) in rows_list.iter().enumerate() {
        let mut chosen: Option<FlatSubsong> = None;
        let mut why = "no pattern length fits".to_string();
        for size in [128usize, 96, 64, 48, 32, 24, 16, 8] {
            let flat = flat_subsong(rows, size);
            if flat.patterns.iter().any(|p| {
                p.cells
                    .iter()
                    .any(|c| gt_packed_pattern_size(&c.rows) > 256)
            }) {
                why =
                    format!("patterns of {size} rows pack to more than GoatTracker's player reads");
                continue;
            }
            if let Err(reason) = compile_flat_song(&base, std::slice::from_ref(&flat)) {
                why = reason;
                continue;
            }
            chosen = Some(flat);
            break;
        }
        let Some(chosen) = chosen else {
            return Err(format!("subsong {}: {}", preps[i].subsong + 1, why));
        };
        flats.push(chosen);
    }
    if !tables.fits() {
        return Err("GoatTracker's tables are full".to_string());
    }
    let song = compile_flat_song(&base, &flats)?;
    Ok((song, rows_list))
}

#[allow(dead_code)]
fn _unused(_: &HashSet<u8>, _: u8) -> u8 {
    SID_EVENT_GATE_ON
}
