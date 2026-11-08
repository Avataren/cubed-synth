//! GoatTracker's own limits, the song's note names, and what each pattern
//! command does.

use serde_json::{json, Value};

use crate::song::{
    Row, MAX_INSTRUMENTS, MAX_ORDER_ENTRIES, MAX_PATTERNS, MAX_PATTERN_ROWS, MAX_REPEAT,
    MAX_SPEED_MULTIPLIER, MAX_SUBSONGS, MAX_TEMPO, MAX_TEXT, MAX_TRANSPOSE, MIN_TRANSPOSE,
    NOTE_FIRST, NOTE_KEY_OFF, NOTE_KEY_ON, NOTE_LAST, NOTE_NONE,
};

/// Rows of one step table: a pointer is a byte and 0 means "no table".
pub const MAX_TABLE_ROWS: usize = 255;
/// The longest instrument name.
pub const MAX_NAME: usize = crate::song::MAX_INSTRUMENT_NAME;
/// A MIDI number C-0 has (C-4 is 60): song note = MIDI - 11.
pub const MIDI_OFFSET: i64 = 11;
/// GoatTracker's start tempo, the only one a `.sng` can say.
pub const SNG_TEMPO: u8 = 6;

const NAMES: [&str; 12] = [
    "C-", "C#", "D-", "D#", "E-", "F-", "F#", "G-", "G#", "A-", "A#", "B-",
];

/// A song note as text: `C-0` to `G#7`, `---` for none, `===` for key off,
/// `+++` for key on, `???` for anything else.
pub fn note_name(note: u8) -> String {
    match note {
        NOTE_NONE => "---".into(),
        NOTE_KEY_OFF => "===".into(),
        NOTE_KEY_ON => "+++".into(),
        n if (NOTE_FIRST..=NOTE_LAST).contains(&n) => {
            let i = (n - NOTE_FIRST) as usize;
            format!("{}{}", NAMES[i % 12], i / 12)
        }
        _ => "???".into(),
    }
}

/// A note from its text: `C-4`, `c4`, `C#4`, `---` / `...` / empty (none),
/// `off` / `===` / `###` (key off), `on` / `+++` (key on).
pub fn parse_note(text: &str) -> Option<u8> {
    let t = text.trim();
    match t.to_ascii_lowercase().as_str() {
        "" | "---" | "..." | "none" => return Some(NOTE_NONE),
        "off" | "===" | "###" | "keyoff" => return Some(NOTE_KEY_OFF),
        "on" | "+++" | "keyon" => return Some(NOTE_KEY_ON),
        _ => {}
    }
    let b = t.as_bytes();
    if b.len() < 2 || b.len() > 3 || !b[0].is_ascii_alphabetic() {
        return None;
    }
    let base = match b[0].to_ascii_uppercase() {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        b'B' => 11,
        _ => return None,
    };
    let (sharp, octave) = match b.len() {
        2 => (false, b[1]),
        _ => match b[1] {
            b'#' => (true, b[2]),
            b'-' => (false, b[2]),
            _ => return None,
        },
    };
    if !octave.is_ascii_digit() {
        return None;
    }
    let index = (octave - b'0') as u32 * 12 + base + sharp as u32;
    let note = index + NOTE_FIRST as u32;
    (note <= NOTE_LAST as u32).then_some(note as u8)
}

/// The song note of a MIDI number (C-4 is 60), refused outside C-0..G#7.
pub fn midi_to_note(midi: i64) -> Result<u8, String> {
    let note = midi - MIDI_OFFSET;
    if (NOTE_FIRST as i64..=NOTE_LAST as i64).contains(&note) {
        Ok(note as u8)
    } else {
        Err(format!(
            "MIDI note {midi} is outside C-0..G#7 ({}..{})",
            NOTE_FIRST as i64 + MIDI_OFFSET,
            NOTE_LAST as i64 + MIDI_OFFSET
        ))
    }
}

/// Why `row` is not a row of a song with `instruments` instruments, or `None`.
pub fn row_problem(row: &Row, instruments: usize) -> Option<String> {
    let is_note = row.note == NOTE_NONE
        || row.note == NOTE_KEY_OFF
        || row.note == NOTE_KEY_ON
        || (NOTE_FIRST..=NOTE_LAST).contains(&row.note);
    if !is_note {
        return Some(format!(
            "note {} is not a note (0 none, {NOTE_FIRST}-{NOTE_LAST} C-0..G#7, {NOTE_KEY_OFF} key off, {NOTE_KEY_ON} key on)",
            row.note
        ));
    }
    if row.instrument as usize > instruments {
        return Some(format!(
            "instrument {} does not exist (the song has {instruments})",
            row.instrument
        ));
    }
    if row.command > 0x0F {
        return Some(format!("command {} is not 0-F", row.command));
    }
    None
}

/// What each pattern command is and what its parameter means, as GoatTracker's
/// readme gives them (`sid-command-help.ts`): `(name, parameter)`.
pub const COMMANDS: [(&str, &str); 16] = [
    ("No command", "ignored"),
    ("Portamento up", "speed table row"),
    ("Portamento down", "speed table row"),
    ("Tone portamento", "speed table row (00 = tie the note)"),
    (
        "Vibrato",
        "speed table row (left: ticks per direction, right: depth)",
    ),
    ("Set attack/decay", "AD byte"),
    ("Set sustain/release", "SR byte"),
    ("Set waveform", "waveform byte"),
    ("Set wave table pointer", "wave table row (00 stops)"),
    ("Set pulse table pointer", "pulse table row (00 stops)"),
    ("Set filter table pointer", "filter table row (00 stops)"),
    (
        "Set filter control",
        "X resonance, Y voice bitmask (00 = filter off)",
    ),
    ("Set filter cutoff", "cutoff byte"),
    ("Set master volume", "0Y: volume 0-F"),
    (
        "Funk tempo",
        "speed table row (its two values alternate as the tempo)",
    ),
    (
        "Set tempo",
        "03-7F all voices, 83-FF this voice only (minus 80), 00-01 recall the funk tempo",
    ),
];

/// The limits, as `info()` reports them.
pub fn limits_json() -> Value {
    json!({
        "channels": 3,
        "subsongs": MAX_SUBSONGS,
        "patterns": MAX_PATTERNS,
        "rows": [1, MAX_PATTERN_ROWS],
        "instruments": MAX_INSTRUMENTS,
        "table_rows": MAX_TABLE_ROWS,
        "order_entries": MAX_ORDER_ENTRIES,
        "repeat": [1, MAX_REPEAT],
        "transpose": [MIN_TRANSPOSE, MAX_TRANSPOSE],
        "sng_transpose": [
            crate::goattracker::GT_MIN_TRANSPOSE,
            crate::goattracker::GT_MAX_TRANSPOSE
        ],
        "speed_multiplier": [1, MAX_SPEED_MULTIPLIER],
        "tempo": [1, MAX_TEMPO],
        "text": MAX_TEXT,
        "instrument_name": MAX_NAME,
    })
}
