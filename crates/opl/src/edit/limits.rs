//! What each version of the A2M format can hold, notes and effects as text,
//! and the DOS text of names.

use serde_json::{json, Value};

use super::json::Res;
use crate::a2::model::{cp437, A2mSong, Cell};

/// Effect letters by effect number (`techinfo.htm`'s table, 0x00-0x2F).
pub const EFFECT_CHARS: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ&%!@=#$~^`><";

/// AT2's note 1 is C-0, MIDI 12.
pub const NOTE_OFFSET: u32 = 11;
/// The lowest and highest note as a MIDI number: C-0 to B-7, and note 97 (C-8),
/// which AT2 plays as the top of block 7 and real songs use.
pub const NOTE_MIN: u32 = 12;
pub const NOTE_MAX: u32 = 108;
/// The highest note the editor types and transposes to (B-7).
pub const NOTE_TYPED_MAX: u8 = 96;
/// A fixed note (version 9+) is the note's MIDI number plus this.
pub const FIXED_OFFSET: u32 = 128;
/// A note byte the format does not define is shown as the byte plus this.
pub const RAW_OFFSET: u32 = 256;
/// The note byte of a key off.
pub const KEY_OFF: u8 = 255;
/// The file's flag for a fixed note: `0x90 + note`.
pub const FIXED_FLAG: u8 = 0x90;

/// What a version of the format holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub version: u8,
    /// Instrument slots: 250 before version 9, 255 from it.
    pub instruments: usize,
    /// The longest instrument name (CP437 characters).
    pub name_len: usize,
    pub max_patterns: usize,
    /// Rows per pattern: fixed at 64 before version 9.
    pub min_rows: usize,
    pub max_rows: usize,
    /// Tracks: 9 (versions 1-4), 18 (5-8) or 1-20 (9 up).
    pub min_tracks: usize,
    pub max_tracks: usize,
    /// The highest effect number of a cell (0x0F before version 5, 0x2F from it).
    pub max_effect: u8,
    pub two_effects: bool,
    pub fixed_notes: bool,
    pub panning: bool,
    pub voice_types: bool,
    pub flags: bool,
    /// FM, arpeggio and vibrato macros (version 9+).
    pub macros: bool,
    /// The 4-op track flags and the initial lock flags (version 10+).
    pub four_op_tracks: bool,
    /// Pattern names and disabled FM columns (version 11+).
    pub pattern_names: bool,
    /// 4-op instrument pairs (version 12+).
    pub four_op_instruments: bool,
    /// Rows per beat and the tempo fine-tune (version 14).
    pub beat: bool,
    /// Patterns in a block of the file (what `spare_patterns` fills).
    pub patterns_per_block: usize,
    /// The size a pattern is stored at.
    pub storage_rows: usize,
    pub storage_tracks: usize,
}

pub fn limits(song: &A2mSong) -> Limits {
    limits_of(song.version)
}

pub fn limits_of(version: u8) -> Limits {
    let v = version;
    let (max_effect, max_patterns, per_block, tracks) = match v {
        0..=4 => (0x0f, 64, 16, (9, 9)),
        // techinfo.htm gives 0-35 for versions 5-8; real songs of version 5 use the
        // rest of the table (`&` and up), which the player plays, so the files win.
        5..=8 => (0x2f, 64, 8, (18, 18)),
        _ => (0x2f, 128, 8, (1, 20)),
    };
    Limits {
        version: v,
        instruments: if v >= 9 { 255 } else { 250 },
        name_len: if v >= 9 { 42 } else { 32 },
        max_patterns,
        min_rows: if v >= 9 { 1 } else { 64 },
        max_rows: if v >= 9 { 256 } else { 64 },
        min_tracks: tracks.0,
        max_tracks: tracks.1,
        max_effect,
        two_effects: v >= 9,
        fixed_notes: v >= 9,
        panning: v >= 5,
        voice_types: v >= 9,
        flags: v >= 5,
        macros: v >= 9,
        four_op_tracks: v >= 10,
        pattern_names: v >= 11,
        four_op_instruments: v >= 12,
        beat: v >= 14,
        patterns_per_block: per_block,
        storage_rows: if v >= 9 { 256 } else { 64 },
        storage_tracks: if v >= 9 { 20 } else { tracks.0 },
    }
}

impl Limits {
    pub fn to_json(&self) -> Value {
        json!({
            "instruments": self.instruments,
            "name": self.name_len,
            "patterns": self.max_patterns,
            "rows": [self.min_rows, self.max_rows],
            "tracks": [self.min_tracks, self.max_tracks],
            "max_effect": self.max_effect,
            "two_effects": self.two_effects,
            "fixed_notes": self.fixed_notes,
            "panning": self.panning,
            "voice_types": self.voice_types,
            "flags": self.flags,
            "macros": self.macros,
            "four_op_tracks": self.four_op_tracks,
            "pattern_names": self.pattern_names,
            "four_op_instruments": self.four_op_instruments,
            "beat": self.beat,
            "orders": 128,
        })
    }
}

/// A cell's note byte as the number the JSON shows: 0 none, 255 key off, 12-107
/// the MIDI number of a plain note (60 is middle C), 140-235 a fixed note
/// (version 9+: the MIDI number plus 128), and 256 plus the byte for one the
/// format does not define.
pub fn note_to_json(byte: u8) -> u32 {
    match byte {
        0 => 0,
        KEY_OFF => 255,
        1..=97 => byte as u32 + NOTE_OFFSET,
        0x91..=0xf1 => (byte - FIXED_FLAG) as u32 + NOTE_OFFSET + FIXED_OFFSET,
        b => RAW_OFFSET + b as u32,
    }
}

/// The note byte for a JSON note number (see [`note_to_json`]).
pub fn note_from_json(n: u64, lim: &Limits) -> Res<u8> {
    let n = n as u32;
    Ok(match n {
        0 => 0,
        255 => KEY_OFF,
        NOTE_MIN..=NOTE_MAX => (n - NOTE_OFFSET) as u8,
        n if (NOTE_MIN + FIXED_OFFSET..=NOTE_MAX + FIXED_OFFSET).contains(&n) => {
            if !lim.fixed_notes {
                return Err(format!(
                    "version {} has no fixed notes (they start with version 9)",
                    lim.version
                ));
            }
            (n - FIXED_OFFSET - NOTE_OFFSET) as u8 + FIXED_FLAG
        }
        n if (RAW_OFFSET..RAW_OFFSET + 256).contains(&n) => (n - RAW_OFFSET) as u8,
        _ => {
            return Err(format!(
                "note {n} is not an Adlib Tracker II note: {NOTE_MIN} (C-0) to {NOTE_MAX} (C-8), \
                 0 for none, 255 for a key off"
            ))
        }
    })
}

const NAMES: [&str; 12] = [
    "C-", "C#", "D-", "D#", "E-", "F-", "F#", "G-", "G#", "A-", "A#", "B-",
];

/// A note byte as text: `C-4`, `###` for a key off, `---` for none, `=C-4`
/// for a fixed note.
pub fn note_name(byte: u8) -> String {
    match byte {
        0 => "---".into(),
        KEY_OFF => "###".into(),
        _ => {
            let n = note_to_json(byte);
            let (fixed, midi) = if (FIXED_OFFSET..RAW_OFFSET).contains(&n) {
                (true, n - FIXED_OFFSET)
            } else if n >= RAW_OFFSET {
                return format!("?{byte:02X}");
            } else {
                (false, n)
            };
            format!(
                "{}{}{}",
                if fixed { "=" } else { "" },
                NAMES[(midi % 12) as usize],
                midi / 12 - 1
            )
        }
    }
}

/// A note as text (`C-4`, `C4`, `C#4`, `Db4`, `###`, `off`, `---`) to its note
/// byte; the fixed-note mark `=` is read too.
pub fn parse_note(text: &str, lim: &Limits) -> Option<u8> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    if matches!(lower.as_str(), "---" | "" | "none") {
        return Some(0);
    }
    if matches!(lower.as_str(), "###" | "===" | "off" | "key off") {
        return Some(KEY_OFF);
    }
    let (fixed, t) = match t.strip_prefix('=') {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    let mut chars = t.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let base = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (shift, octave) = if let Some(o) = rest.strip_prefix('#') {
        (1, o)
    } else if let Some(o) = rest.strip_prefix('-') {
        (0, o)
    } else if let Some(o) = rest.strip_prefix('b') {
        (-1, o)
    } else {
        (0, rest.as_str())
    };
    let octave: i32 = octave.parse().ok()?;
    let midi = (octave + 1) * 12 + base + shift;
    if !(NOTE_MIN as i32..=NOTE_MAX as i32).contains(&midi) {
        return None;
    }
    let byte = (midi as u32 - NOTE_OFFSET) as u8;
    if fixed {
        lim.fixed_notes.then_some(byte + FIXED_FLAG)
    } else {
        Some(byte)
    }
}

/// An effect command as its letter (`0`-`9`, `A`-`Z`, then `&%!@=#$~^`><`).
pub fn effect_char(effect: u8) -> Option<char> {
    EFFECT_CHARS.chars().nth(effect as usize)
}

/// An effect letter to its number.
pub fn parse_effect(text: &str) -> Option<u8> {
    let mut it = text.trim().chars();
    let c = it.next()?.to_ascii_uppercase();
    if it.next().is_some() {
        return None;
    }
    EFFECT_CHARS.find(c).map(|i| i as u8)
}

/// Why `cell` cannot be stored in a song of this version, if it cannot.
pub fn check_cell(lim: &Limits, c: &Cell) -> Res<()> {
    match c.note {
        0 | KEY_OFF | 1..=97 => {}
        0x91..=0xf1 if lim.fixed_notes => {}
        0x91..=0xf1 => {
            return Err(format!(
                "version {} has no fixed notes (they start with version 9)",
                lim.version
            ))
        }
        b => return Err(format!("note byte {b} is not a note")),
    }
    if c.instrument as usize > lim.instruments {
        return Err(format!(
            "instrument {} does not exist (version {} has {})",
            c.instrument, lim.version, lim.instruments
        ));
    }
    for (column, &(fx, _)) in c.effects.iter().enumerate() {
        if fx > lim.max_effect {
            return Err(format!(
                "effect {fx:#04x} does not exist in version {} (0-{:#04x})",
                lim.version, lim.max_effect
            ));
        }
        if column == 1 && !lim.two_effects && c.effects[1] != (0, 0) {
            return Err(format!(
                "version {} has one effect column (the second starts with version 9)",
                lim.version
            ));
        }
    }
    Ok(())
}

/// A name as text, from its CP437 bytes.
pub fn text(bytes: &[u8]) -> String {
    cp437(bytes)
}

const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

/// `text` as code page 437 bytes, at most `max` of them; what it cannot say
/// becomes `?`. Returns the bytes and whether the text had to be cut.
pub fn encode_text(text: &str, max: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    for ch in text.chars() {
        if out.len() >= max {
            return (out, true);
        }
        let code = ch as u32;
        if (0x20..0x7f).contains(&code) {
            out.push(code as u8);
        } else if let Some(i) = CP437_HIGH.chars().position(|c| c == ch) {
            out.push(0x80 + i as u8);
        } else {
            out.push(b'?');
        }
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_map_between_bytes_midi_and_text() {
        let lim = limits_of(14);
        assert_eq!(note_to_json(49), 60);
        assert_eq!(note_name(49), "C-4");
        assert_eq!(note_name(1), "C-0");
        assert_eq!(note_name(96), "B-7");
        assert_eq!(note_name(97), "C-8");
        assert_eq!(note_name(0x90 + 49), "=C-4");
        assert_eq!(note_name(255), "###");
        assert_eq!(note_to_json(0x90 + 49), 60 + 128);
        assert_eq!(note_from_json(60, &lim).unwrap(), 49);
        assert_eq!(note_from_json(188, &lim).unwrap(), 0x90 + 49);
        assert_eq!(parse_note("C#4", &lim), Some(50));
        assert_eq!(parse_note("Db4", &lim), Some(50));
        assert_eq!(parse_note("c-4", &lim), Some(49));
        assert_eq!(parse_note("=C-4", &lim), Some(0x90 + 49));
        assert_eq!(parse_note("off", &lim), Some(255));
        assert_eq!(parse_note("---", &lim), Some(0));
        assert_eq!(parse_note("C-8", &lim), Some(97));
        assert_eq!(parse_note("C#8", &lim), None);
        assert_eq!(parse_note("H-4", &lim), None);
        assert_eq!(parse_note("=C-4", &limits_of(8)), None);
        // Every byte comes back as it went in.
        for b in 0..=255u8 {
            assert_eq!(note_from_json(note_to_json(b) as u64, &lim).unwrap(), b);
        }
        assert!(note_from_json(11, &lim).is_err());
        assert!(note_from_json(109, &lim).is_err());
        assert!(note_from_json(188, &limits_of(8)).is_err());
    }

    #[test]
    fn effect_letters_cover_all_48_commands() {
        assert_eq!(EFFECT_CHARS.chars().count(), 0x30);
        assert_eq!(effect_char(0x0f), Some('F'));
        assert_eq!(effect_char(0x23), Some('Z'));
        assert_eq!(effect_char(0x24), Some('&'));
        assert_eq!(effect_char(0x2f), Some('<'));
        assert_eq!(effect_char(0x30), None);
        for fx in 0..0x30u8 {
            assert_eq!(
                parse_effect(&effect_char(fx).unwrap().to_string()),
                Some(fx)
            );
        }
        assert_eq!(parse_effect("f"), Some(15));
        assert_eq!(parse_effect("FF"), None);
    }

    #[test]
    fn versions_have_their_own_limits() {
        let v1 = limits_of(1);
        assert_eq!((v1.instruments, v1.name_len, v1.max_tracks), (250, 32, 9));
        assert!(!v1.two_effects && !v1.fixed_notes && !v1.panning);
        let v5 = limits_of(5);
        assert_eq!(
            (v5.max_effect, v5.max_tracks, v5.max_patterns),
            (0x2f, 18, 64)
        );
        assert!(v5.panning && v5.flags);
        let v14 = limits_of(14);
        assert_eq!(
            (v14.instruments, v14.name_len, v14.max_rows),
            (255, 42, 256)
        );
        assert!(v14.beat && v14.four_op_instruments && v14.pattern_names);
        let cell = Cell {
            note: 49,
            instrument: 1,
            effects: [(0x10, 1), (0, 0)],
        };
        assert!(check_cell(&limits_of(14), &cell).is_ok());
        assert!(check_cell(&limits_of(1), &cell)
            .unwrap_err()
            .contains("effect"));
        let two = Cell {
            effects: [(1, 1), (2, 2)],
            ..Cell::default()
        };
        assert!(check_cell(&limits_of(8), &two)
            .unwrap_err()
            .contains("one effect column"));
    }

    #[test]
    fn names_round_trip_through_cp437() {
        let (bytes, cut) = encode_text("Bell ½ ä", 32);
        assert!(!cut);
        assert_eq!(text(&bytes), "Bell ½ ä");
        let (bytes, cut) = encode_text("abcdef", 4);
        assert!(cut);
        assert_eq!(bytes, b"abcd");
        assert_eq!(encode_text("日本", 8).0, b"??");
    }
}
