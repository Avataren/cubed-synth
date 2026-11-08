//! A [`SidSong`] as a GoatTracker 2 `.sng` (`GTS5`, readme §6.1): a port of the
//! DAW's writer (`src/audio/tracker/sid-doc/gt-sng-write.ts`).
//!
//! For every song it writes, [`import`](super::import) gives the song back
//! (with the chip and the speed as hints): `import(export(s)) == s` up to the
//! two facts a `.sng` has no field for. It refuses what a `.sng` cannot hold
//! without changing the music, with a reason written for the user: a start
//! tempo other than GoatTracker's 6, a transpose outside GoatTracker's
//! orderlist range (-16..+14), an orderlist that needs more than its 254
//! bytes, a NUL inside a text. What the file has no field for but GoatTracker
//! takes from its command line (the chip model, the speed multiplier) is
//! written anyway and reported in [`GtExport::notes`].
//!
//! # What is in the file, and what is lost
//!
//! A `.sng` is GoatTracker's own memory image of a song, used rows only; this
//! document is the same shape, so nothing is repacked:
//!
//! - **tables** are written whole, in order, each as its row count, the left
//!   column, then the right column. Instruments, pattern commands (8/9/A, 1-4,
//!   E), wave-table commands and `FF xx` jumps all name rows by their 1-based
//!   number, as the document does, so every pointer is written as it is. (The
//!   packer in GoatTracker's exporter compacts tables and renumbers: that is
//!   not part of a `.sng`.)
//! - **patterns** are shared by number, with a pattern-end row after the last
//!   row; **instruments** are written in number order (25 bytes each).
//! - **orderlists** are compiled to GoatTracker's byte stream (see
//!   [`orderlist_bytes`]): each entry's transpose when it changes, its repeat
//!   when above 1, its pattern; then the end mark and the restart byte.
//!
//! Lost, or changed, relative to the richer `.asid` document: the chip model
//! and the speed multiplier (reported in the notes; a `.sng` is played with
//! `-E` and `-S`); the start tempo (a `.sng` has none: refused unless 6, and
//! `SidDoc`'s `bake_tempo` moves it into an `F` command on the first row); a
//! transpose beyond -16..+14 (refused); a text of the full 32 characters, which
//! has no room for GoatTracker's terminator (reported). Dual SID (six
//! orderlists per subsong) is not supported by this crate at all.

use super::{
    GATE_NO_GATEOFF, GATE_NO_HARD_RESTART, INSTRUMENT_NAME_LENGTH, MAGIC_GTS5, NOTE_FIRST,
    NOTE_GT_KEY_OFF, NOTE_GT_KEY_ON, NOTE_REST, ORDER_END, ORDER_REPEAT, PATTERN_END, START_TEMPO,
    TEXT_LENGTH, TRANSPOSE_ZERO,
};
use crate::song::{Orderlist, SidSong, SongError, TableKind, NOTE_KEY_OFF, NOTE_KEY_ON, NOTE_NONE};
use crate::SidModel;

/// GoatTracker's orderlist transpose range: `$E0` is -16, `$F0` is 0, `$FE` is +14.
pub const GT_MIN_TRANSPOSE: i8 = -16;
pub const GT_MAX_TRANSPOSE: i8 = 14;
/// Readme §3.1: "254 pattern numbers/commands + the endmark".
pub const GT_MAX_ORDERLIST_BYTES: usize = 254;

/// A song as a `.sng`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GtExport {
    pub bytes: Vec<u8>,
    /// What the file cannot say (the chip, the speed) and GoatTracker takes from its command line.
    pub notes: Vec<String>,
}

/// Why `song` cannot be written as a `.sng`, or `None`.
pub fn export_problem(song: &SidSong) -> Option<String> {
    if let Err(e) = song.validate() {
        return Some(format!("the song breaks a rule of the SID model ({e})"));
    }
    if song.tempo != START_TEMPO {
        return Some(format!(
            "a .sng has no tempo field (GoatTracker starts every song at tempo {START_TEMPO}) and this song starts at tempo {}; put an F command with the tempo on its first row instead",
            song.tempo
        ));
    }
    let texts = [
        (&song.name, "the song name".to_string()),
        (&song.author, "the author".to_string()),
        (&song.copyright, "the copyright".to_string()),
    ]
    .into_iter()
    .chain(
        song.instruments
            .iter()
            .enumerate()
            .map(|(i, ins)| (&ins.name, format!("instrument {}'s name", i + 1))),
    );
    for (text, what) in texts {
        if text.contains(&0) {
            return Some(format!(
                "{what} contains a NUL character, which ends a .sng text field early"
            ));
        }
    }
    for (s, subsong) in song.subsongs.iter().enumerate() {
        for (c, list) in subsong.orderlists.iter().enumerate() {
            if let Some(bad) = list
                .entries
                .iter()
                .position(|e| e.transpose < GT_MIN_TRANSPOSE || e.transpose > GT_MAX_TRANSPOSE)
            {
                return Some(format!(
                    "subsong {s} channel {} entry {bad} transposes by {}; a GoatTracker orderlist transposes {GT_MIN_TRANSPOSE}..+{GT_MAX_TRANSPOSE}",
                    c + 1,
                    list.entries[bad].transpose
                ));
            }
            let n = orderlist_byte_length(list);
            if n > GT_MAX_ORDERLIST_BYTES {
                return Some(format!(
                    "subsong {s} channel {}'s orderlist needs {n} bytes with its repeats and transposes; GoatTracker's holds {GT_MAX_ORDERLIST_BYTES}",
                    c + 1
                ));
            }
        }
    }
    None
}

/// The bytes `list` takes in a `.sng` (end mark and restart not counted): at
/// most [`GT_MAX_ORDERLIST_BYTES`].
pub fn orderlist_byte_length(list: &Orderlist) -> usize {
    orderlist_bytes(list).len() - 2
}

/// `list` as GoatTracker's orderlist bytes (readme §6.1.2), the end mark and
/// the restart byte last: each entry's transpose when it changes (and, at the
/// restart entry, when the loop would otherwise carry the end's transpose into
/// it: GoatTracker keeps the running transpose when it loops), its repeat when
/// above 1, its pattern. Transpose comes before repeat (readme §3.1).
pub fn orderlist_bytes(list: &Orderlist) -> Vec<u8> {
    let mut out = Vec::with_capacity(list.entries.len() * 2 + 2);
    let last = list.entries.last().map_or(0, |e| e.transpose);
    let mut transpose = 0i8;
    let mut restart_byte = 0u8;
    for (i, e) in list.entries.iter().enumerate() {
        if i == list.restart as usize {
            restart_byte = out.len() as u8;
        }
        if e.transpose != transpose || (i == list.restart as usize && e.transpose != last) {
            out.push((TRANSPOSE_ZERO + e.transpose as i16) as u8);
            transpose = e.transpose;
        }
        if e.repeat > 1 {
            out.push(ORDER_REPEAT + e.repeat - 1);
        }
        out.push(e.pattern);
    }
    out.push(ORDER_END);
    out.push(restart_byte);
    out
}

/// A song row's note as GoatTracker's pattern byte.
pub fn note_byte(note: u8) -> u8 {
    match note {
        NOTE_NONE => NOTE_REST,
        NOTE_KEY_OFF => NOTE_GT_KEY_OFF,
        NOTE_KEY_ON => NOTE_GT_KEY_ON,
        n => NOTE_FIRST + n - 1,
    }
}

/// The `.sng` of `song`, or why it cannot be one.
pub fn export(song: &SidSong) -> Result<GtExport, SongError> {
    if let Some(problem) = export_problem(song) {
        return Err(SongError(problem));
    }
    let mut out: Vec<u8> = Vec::new();
    let text = |out: &mut Vec<u8>, s: &[u8], length: usize| {
        out.extend((0..length).map(|i| s.get(i).copied().unwrap_or(0)));
    };
    // §6.1.1 header.
    out.extend_from_slice(&MAGIC_GTS5);
    text(&mut out, &song.name, TEXT_LENGTH);
    text(&mut out, &song.author, TEXT_LENGTH);
    text(&mut out, &song.copyright, TEXT_LENGTH);
    out.push(song.subsongs.len() as u8);
    // §6.1.2 orderlists: length (end mark counted, restart not), data, restart.
    for subsong in &song.subsongs {
        for list in &subsong.orderlists {
            let bytes = orderlist_bytes(list);
            out.push((bytes.len() - 1) as u8);
            out.extend_from_slice(&bytes);
        }
    }
    // §6.1.3 instruments.
    out.push(song.instruments.len() as u8);
    for ins in &song.instruments {
        out.extend_from_slice(&[
            ins.ad(),
            ins.sr(),
            ins.wave_ptr,
            ins.pulse_ptr,
            ins.filter_ptr,
            ins.speed_ptr,
            ins.vibrato_delay,
            (if ins.hard_restart {
                0
            } else {
                GATE_NO_HARD_RESTART
            }) | (if ins.no_gate_off { GATE_NO_GATEOFF } else { 0 })
                | ins.gate_timer,
            ins.first_wave,
        ]);
        text(&mut out, &ins.name, INSTRUMENT_NAME_LENGTH);
    }
    // §6.1.4 tables: n, n left bytes, n right bytes.
    for kind in TableKind::ALL {
        let table = song.tables.get(kind);
        out.push(table.len() as u8);
        out.extend(table.iter().map(|r| r.left));
        out.extend(table.iter().map(|r| r.right));
    }
    // §6.1.5/6 patterns: the stored length counts the $FF end row (measured).
    out.push(song.patterns.len() as u8);
    for pattern in &song.patterns {
        out.push((pattern.rows.len() + 1) as u8);
        for r in &pattern.rows {
            out.extend_from_slice(&[note_byte(r.note), r.instrument, r.command, r.param]);
        }
        out.extend_from_slice(&[PATTERN_END, 0, 0, 0]);
    }

    let model = match song.model {
        SidModel::Sid6581 => "6581",
        SidModel::Sid8580 => "8580",
    };
    let mut notes = vec![format!(
        "the chip model ({model}) is not stored in a .sng: GoatTracker takes it from its -E option"
    )];
    if song.speed_multiplier != 1 {
        notes.push(format!(
            "the {0}x speed is not stored in a .sng: play it in GoatTracker with -S{0}",
            song.speed_multiplier
        ));
    }
    for (text, what) in [
        (&song.name, "the song name"),
        (&song.author, "the author"),
        (&song.copyright, "the copyright"),
    ] {
        if text.len() == TEXT_LENGTH {
            notes.push(format!(
                "{what} fills all {TEXT_LENGTH} bytes of its field, so it has no terminator; GoatTracker's own editor stops at {}",
                TEXT_LENGTH - 1
            ));
        }
    }
    Ok(GtExport { bytes: out, notes })
}
