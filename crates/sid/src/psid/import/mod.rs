//! Importing a `.sid` (PSID or RSID) as an editable GoatTracker-style song: a
//! port of another_synth's `.sid` importer (`src/audio/tracker/psid/`,
//! `plan-psid-import.md`). The song is a [`SidSong`], the crate's document, so
//! [`SidDoc`](crate::edit::SidDoc) edits it and writes it as `.sng` / `.asid`.
//!
//! ```no_run
//! # fn main() -> Result<(), String> {
//! use cubed_sid::psid::import::{import, ImportOptions};
//! let bytes = std::fs::read("tune.sid").map_err(|e| e.to_string())?;
//! let r = import(&bytes, &ImportOptions::default())?;
//! println!("{} patterns, fidelity {:?}", r.song.patterns.len(), r.report.fidelity.map(|f| f.score));
//! # Ok(()) }
//! ```
//!
//! A tune GoatTracker made (its player is GoatTracker 2's) is **unpacked**: its
//! song data read back into the song it was packed from. Any other is run on an
//! emulated C64 and what its player plays is **transcribed**: the register
//! trace of every subsong is cut into rows and notes, the notes grouped into
//! instruments, the rows compiled into patterns and orderlists; how close that
//! sounds is measured on the start song ([`fidelity`]).
//!
//! It runs the tune: up to ten minutes of emulated time per subsong (the
//! DAW's cap, [`ImportOptions::max_seconds`]) and up to 32 subsongs, which
//! takes seconds, so call it off the audio thread (and off the UI thread).
//!
//! Module map: [`capture`] (the register trace), [`frames`], [`grid`], [`notes`],
//! [`pitch`], [`instruments`], [`fold`], [`song`] and [`flat`] (the song's rows
//! and compile), [`transcribe`] (the whole transcription), [`fidelity`].

pub mod capture;
pub mod envelope;
pub mod export;
pub mod fidelity;
pub mod flat;
pub mod fold;
pub mod frames;
pub mod grid;
pub mod gt_unpack;
pub mod instruments;
pub mod notes;
pub mod pitch;
pub mod song;
pub mod transcribe;
pub mod util;
pub mod vibrato;

pub use fidelity::{Fidelity, VoiceFidelity};
pub use transcribe::{SubsongReport, TranscribeOptions};

use super::c64::Kernal;
use super::file::{looks_like_psid, PsidFile};
use crate::song::SidSong;

/// What to import and how far to go.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// PSID subsongs to import (0-based), in order. Default: the start song, then the others.
    pub subsongs: Option<Vec<usize>>,
    /// Capture length cap per subsong, seconds (default 600).
    pub max_seconds: Option<f64>,
    /// Measure the start song against the original (default true).
    pub measure: bool,
    /// Recognise a tune GoatTracker made and unpack its song data (default true); false transcribes every tune.
    pub gt_unpack: bool,
    /// The stand-in KERNAL the tune runs on. The default, `Kernal::Extended`, also runs the tunes whose
    /// interrupt handlers end in the middle of the real KERNAL's (`$EA34`, `$EA7B`); `Kernal::Daw` is the
    /// DAW's own, which refuses them.
    pub kernal: Kernal,
}

impl Default for ImportOptions {
    fn default() -> ImportOptions {
        ImportOptions {
            subsongs: None,
            max_seconds: None,
            measure: true,
            gt_unpack: true,
            kernal: Kernal::default(),
        }
    }
}

/// What the import tells about what it made.
#[derive(Clone, Debug)]
pub struct ImportReport {
    /// The start song against the original, or `None` when it was not measured or could not be.
    pub fidelity: Option<Fidelity>,
    /// Things the user should know (speed, cut songs, subsongs left out).
    pub notes: Vec<String>,
    /// What went wrong without stopping the import (a measure that could not be made).
    pub warnings: Vec<String>,
    /// The song was unpacked from GoatTracker's own song data, not transcribed.
    pub used_gt_unpack: bool,
    /// Unpacked, and packing the song again gives the file's bytes (`None`: transcribed).
    pub exact: Option<bool>,
    /// The PSID subsong (0-based) that became the song's first subsong.
    pub subsong: usize,
    /// Frames of that subsong's capture (0 when unpacked).
    pub frames: usize,
    /// Per subsong, how the transcription went (none when unpacked).
    pub subsongs: Vec<SubsongReport>,
}

/// An imported song.
#[derive(Clone, Debug)]
pub struct ImportResult {
    pub song: SidSong,
    pub report: ImportReport,
}

/// Subsong numbers (1-based) as runs: "3-20, 22".
fn runs(subsongs: &[usize]) -> String {
    let mut sorted = subsongs.to_vec();
    sorted.sort_unstable();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1] == sorted[j] + 1 {
            j += 1;
        }
        out.push(if j > i {
            format!("{}-{}", sorted[i], sorted[j])
        } else {
            format!("{}", sorted[i])
        });
        i = j + 1;
    }
    out.join(", ")
}

/// The subsongs left out, grouped by why: "Not imported: subsongs 3-20 (2x speed; the song plays at 1x)."
fn left_out_note(left: &[&SubsongReport]) -> String {
    // Grouped by reason, in the order the reasons first came up.
    let mut by_reason: Vec<(String, Vec<usize>)> = Vec::new();
    for r in left {
        let why = r.reason.clone().unwrap_or_else(|| "left out".to_string());
        match by_reason.iter_mut().find(|(w, _)| *w == why) {
            Some((_, v)) => v.push(r.subsong + 1),
            None => by_reason.push((why, vec![r.subsong + 1])),
        }
    }
    let parts: Vec<String> = by_reason
        .iter()
        .map(|(why, subsongs)| format!("{} ({})", runs(subsongs), why))
        .collect();
    format!(
        "Not imported: subsong{} {}.",
        if left.len() == 1 { "" } else { "s" },
        parts.join("; ")
    )
}

/// `bytes` (a `.sid`) as a GoatTracker song, or the true reason it cannot be one.
/// Never panics for a tune's behaviour.
pub fn import(bytes: &[u8], opts: &ImportOptions) -> Result<ImportResult, String> {
    if !looks_like_psid(bytes) {
        return Err("it is not a SID file".to_string());
    }
    let file = PsidFile::parse(bytes).map_err(|e| e.to_string())?;
    import_file(&file, opts)
}

/// An unpacked song that does not play like the file plays less like it than this, or it is transcribed instead.
const UNPACKED_FIDELITY: f64 = 0.98;

/// `file` unpacked from its GoatTracker data, or `None` when it is not a GoatTracker file (or
/// unpacks to something that does not play like it).
fn unpacked(file: &PsidFile, opts: &ImportOptions) -> Option<ImportResult> {
    let u = gt_unpack::unpack_gt_sid(file).ok()?;
    let mut fidelity = None;
    if !u.exact {
        // Not the file's bytes again (an option GoatTracker's packer has and ours does not): kept
        // only when it plays like the file.
        let trace = capture::capture_sid_with(
            file,
            &capture::CaptureOptions {
                subsong: 0,
                max_ticks: None,
                max_seconds: Some(120.0),
            },
            opts.kernal,
        )
        .ok()?;
        let m = fidelity::measure_fidelity(&trace, &u.song, 0, 3000, opts.kernal).ok()?;
        if m.score < UNPACKED_FIDELITY {
            return None;
        }
        fidelity = if opts.measure { Some(m) } else { None };
    }
    Some(ImportResult {
        report: ImportReport {
            fidelity,
            notes: u.notes,
            warnings: Vec::new(),
            used_gt_unpack: true,
            exact: Some(u.exact),
            subsong: 0,
            frames: 0,
            subsongs: Vec::new(),
        },
        song: u.song,
    })
}

/// [`import`] of a parsed file.
pub fn import_file(file: &PsidFile, opts: &ImportOptions) -> Result<ImportResult, String> {
    if opts.gt_unpack {
        if let Some(r) = unpacked(file, opts) {
            return Ok(r);
        }
    }
    let t = transcribe::transcribe_psid(
        file,
        &TranscribeOptions {
            subsongs: opts.subsongs.clone(),
            max_seconds: opts.max_seconds,
            kernal: opts.kernal,
        },
    )
    .map_err(|f| f.reason)?;
    let mut notes = t.notes.clone();
    let left: Vec<&SubsongReport> = t
        .reports
        .iter()
        .filter(|r| r.gt_subsong.is_none())
        .collect();
    if !left.is_empty() {
        notes.push(left_out_note(&left));
    }
    if !file.extra_sids.is_empty() {
        notes.push(
            "The tune plays more SID chips; only the first was imported (GoatTracker has one)."
                .to_string(),
        );
    }
    if t.reports
        .iter()
        .any(|r| r.trace.as_ref().is_some_and(|tr| tr.digi))
    {
        notes.push(
            "The tune plays samples through the volume register; GoatTracker cannot, so they are left out."
                .to_string(),
        );
    }
    let mut warnings = Vec::new();
    let mut fidelity = None;
    let start = t.reports.iter().find(|r| r.gt_subsong == Some(0));
    if opts.measure {
        if let Some(trace) = start.and_then(|s| s.trace.as_ref()) {
            match fidelity::measure_fidelity(trace, &t.song, 0, 3000, opts.kernal) {
                Ok(f) => fidelity = Some(f),
                Err(e) => warnings.push(format!("the fidelity could not be measured: {e}")),
            }
        }
    }
    Ok(ImportResult {
        report: ImportReport {
            fidelity,
            notes,
            warnings,
            used_gt_unpack: false,
            exact: None,
            subsong: start.map_or(file.start_subsong(), |s| s.subsong),
            frames: start.and_then(|s| s.frames).unwrap_or(0),
            subsongs: t.reports,
        },
        song: t.song,
    })
}
