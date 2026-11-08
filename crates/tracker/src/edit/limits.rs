//! What each format can hold, and the checks every edit runs against it.

use crate::module::{Cell, Format, ModFlavor, Module, Note};

type Res<T> = Result<T, String>;

/// The size limits of a module's format (the original tracker's, which are
/// what a file written for it can rely on).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub min_channels: usize,
    pub max_channels: usize,
    pub max_patterns: usize,
    pub max_orders: usize,
    /// Sample slots (MOD, S3M) or instruments (XM).
    pub max_slots: usize,
    pub min_rows: usize,
    pub max_rows: usize,
    /// Whether a pattern's length can be changed (a MOD's and an S3M's is 64).
    pub variable_rows: bool,
    pub max_samples_per_instrument: usize,
    pub max_title: usize,
    pub max_name: usize,
    /// The speed range of the initial speed.
    pub speed: (u8, u8),
    pub tempo: (u8, u8),
}

/// The limits of `module`'s format (a MOD's depend on its layout).
pub fn limits(module: &Module) -> Limits {
    match module.format {
        Format::Mod => {
            let layout31 = !matches!(
                module.mod_flavor,
                Some(ModFlavor::Soundtracker | ModFlavor::UltimateSoundtracker)
            );
            Limits {
                min_channels: if layout31 { 1 } else { 4 },
                max_channels: if layout31 { 32 } else { 4 },
                max_patterns: if layout31 { 128 } else { 64 },
                max_orders: 128,
                max_slots: if layout31 { 31 } else { 15 },
                min_rows: 64,
                max_rows: 64,
                variable_rows: false,
                max_samples_per_instrument: 1,
                max_title: 20,
                max_name: 22,
                speed: (1, 31),
                tempo: (32, 255),
            }
        }
        Format::S3m => Limits {
            min_channels: 1,
            max_channels: 32,
            max_patterns: 100,
            max_orders: 255,
            max_slots: 99,
            min_rows: 64,
            max_rows: 64,
            variable_rows: false,
            max_samples_per_instrument: 1,
            max_title: 28,
            max_name: 28,
            speed: (1, 254),
            tempo: (33, 255),
        },
        Format::Xm => Limits {
            min_channels: 1,
            max_channels: 32,
            max_patterns: 256,
            max_orders: 256,
            max_slots: 128,
            min_rows: 1,
            max_rows: 256,
            variable_rows: true,
            max_samples_per_instrument: 16,
            max_title: 20,
            max_name: 22,
            speed: (1, 31),
            tempo: (32, 255),
        },
    }
}

// ---------------------------------------------------------------------------
// notes

/// The lowest and highest note (a MIDI number, 60 is middle C) a cell of
/// this module can hold.
///
/// - MOD: C-1 (24) to B-3 (59) when the module is limited to ProTracker's
///   periods, else the PC trackers' seven octaves down to MIDI 1.
/// - S3M: C-0 (24) to B-7 (119).
/// - XM: C-0 (12) to B-7 (107).
pub fn note_range(module: &Module) -> (u8, u8) {
    match module.format {
        Format::Mod if module.flags.amiga_limits => (24, 59),
        Format::Mod => (1, 83),
        Format::S3m => (24, 119),
        Format::Xm => (12, 107),
    }
}

/// How the format counts octaves: a note `n` is in octave `n / 12 - offset`.
/// MOD and XM name MIDI 60 `C-4` (offset 1; a MOD's lowest note, MIDI 24, is
/// `C-1`); S3M, whose notes are stored an octave higher, names MIDI 72 `C-4`
/// (offset 2).
pub fn octave_offset(format: Format) -> i32 {
    match format {
        Format::S3m => 2,
        _ => 1,
    }
}

const NAMES: [&str; 12] = [
    "C-", "C#", "D-", "D#", "E-", "F-", "F#", "G-", "G#", "A-", "A#", "B-",
];

/// A note as the format's tracker writes it: `C-4`, `F#2`.
pub fn note_name(format: Format, note: u8) -> String {
    let octave = note as i32 / 12 - octave_offset(format);
    format!("{}{}", NAMES[note as usize % 12], octave)
}

/// The inverse of [`note_name`]; also reads `c4`, `C#4`, `Db4` and the
/// key-off marks `===`, `^^^`, `off`. The empty note is `---`, `...` or empty.
pub fn parse_note(format: Format, text: &str) -> Option<Note> {
    let t = text.trim();
    match t {
        "" | "---" | "..." | "--" | "-" | "none" => return Some(Note::None),
        "===" | "^^^" | "^^" | "off" | "OFF" | "key off" => return Some(Note::Off),
        _ => {}
    }
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
    let (shift, digits) = if let Some(d) = rest.strip_prefix('#') {
        (1, d)
    } else if let Some(d) = rest.strip_prefix('b') {
        (-1, d)
    } else if let Some(d) = rest.strip_prefix('-') {
        (0, d)
    } else {
        (0, rest.as_str())
    };
    let octave: i32 = digits.parse().ok()?;
    let n = (octave + octave_offset(format)) * 12 + base + shift;
    (0..=255).contains(&n).then_some(Note::On(n as u8))
}

// ---------------------------------------------------------------------------
// effects

/// The letter a tracker shows for an effect command: `0`-`9` then `A`-`Z`
/// for a MOD or XM command number (XM's `G` is 16), `A`-`Z` for an S3M's
/// 1-26 (`.` for none).
pub fn effect_char(format: Format, effect: u8) -> char {
    match format {
        Format::S3m => match effect {
            1..=26 => (b'A' + effect - 1) as char,
            _ => '.',
        },
        _ => match effect {
            0..=9 => (b'0' + effect) as char,
            10..=35 => (b'A' + effect - 10) as char,
            _ => '?',
        },
    }
}

/// The inverse of [`effect_char`]: a command number from its letter or digit.
pub fn parse_effect(format: Format, text: &str) -> Option<u8> {
    let mut chars = text.trim().chars();
    let c = chars.next()?.to_ascii_uppercase();
    if chars.next().is_some() {
        return None;
    }
    match (format, c) {
        (Format::S3m, '.') => Some(0),
        (Format::S3m, 'A'..='Z') => Some(c as u8 - b'A' + 1),
        (_, '0'..='9') => Some(c as u8 - b'0'),
        (_, 'A'..='Z') => Some(c as u8 - b'A' + 10),
        _ => None,
    }
}

/// The effect commands an XM (FastTracker 2) has: 0-F, G, H, K, L, P, R, T, X.
const XM_EFFECTS: [u8; 24] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    0x10, 0x11, 0x14, 0x15, 0x19, 0x1b, 0x1d, 0x21,
];

// ---------------------------------------------------------------------------
// cells

/// Whether a cell is one this module's format can hold, with the reason when
/// it is not. Every edit that writes a cell runs it first.
pub fn check_cell(m: &Module, cell: &Cell) -> Res<()> {
    let (lo, hi) = note_range(m);
    match cell.note {
        Note::On(n) if n < lo || n > hi => {
            return Err(format!(
                "note {} is outside {}'s range ({} to {})",
                note_name(m.format, n),
                format_name(m.format),
                note_name(m.format, lo),
                note_name(m.format, hi)
            ));
        }
        Note::Off if m.format == Format::Mod => {
            return Err("a MOD has no key-off: use a note cut (effect EC0)".into());
        }
        _ => {}
    }
    let slots = match m.format {
        Format::Xm => m.instruments.len(),
        _ => m.samples.len(),
    };
    if cell.instrument as usize > slots {
        let what = if m.format == Format::Xm {
            "instrument"
        } else {
            "sample"
        };
        return Err(format!(
            "{what} {} does not exist (the module has {slots})",
            cell.instrument
        ));
    }
    match m.format {
        Format::Mod => {
            if cell.volume != 0 {
                return Err("a MOD has no volume column: use effect C".into());
            }
            if cell.effect > 0x0f {
                return Err(format!(
                    "effect {:#x} is not a MOD effect (0-F)",
                    cell.effect
                ));
            }
            if m.mod_flavor == Some(ModFlavor::UltimateSoundtracker)
                && matches!(cell.effect, 1 | 2)
                && cell.param > 0x0f
            {
                return Err(
                    "Ultimate Soundtracker's pitch bends take a single nibble (0-F)".into(),
                );
            }
        }
        Format::S3m => {
            if cell.volume != 0 && !(0x10..=0x50).contains(&cell.volume) {
                return Err(format!(
                    "volume column {:#x}: an S3M's is a volume 0-64 (0x10-0x50)",
                    cell.volume
                ));
            }
            if cell.effect > 26 {
                return Err(format!("effect {} is not A-Z", cell.effect));
            }
            if cell.effect == 0 && cell.param != 0 {
                return Err("a parameter needs an effect".into());
            }
            if cell.effect == 3 && (cell.param >> 4 > 9 || cell.param & 0x0f > 9) {
                return Err(
                    "pattern break (C) takes the row as two decimal digits; ST3 drops others"
                        .into(),
                );
            }
        }
        Format::Xm => {
            if cell.volume != 0 && !(0x10..=0x50).contains(&cell.volume) && cell.volume < 0x60 {
                return Err(format!(
                    "volume column {:#x}: XM has 0x10-0x50 (volume) and 0x60-0xFF (commands)",
                    cell.volume
                ));
            }
            if !XM_EFFECTS.contains(&cell.effect) {
                return Err(format!(
                    "effect {:#x} is not an XM effect (0-F, G, H, K, L, P, R, T, X)",
                    cell.effect
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn format_name(format: Format) -> &'static str {
    match format {
        Format::Mod => "a MOD",
        Format::S3m => "an S3M",
        Format::Xm => "an XM",
    }
}

/// A name for a title, sample or instrument: printable Latin-1, cut to `max`.
/// Returns the text as stored and whether it was cut.
pub fn clean_name(text: &str, max: usize) -> Res<(String, bool)> {
    for ch in text.chars() {
        let c = ch as u32;
        if !((0x20..=0x7e).contains(&c) || (0xa0..=0xff).contains(&c)) {
            return Err(format!("names are printable Latin-1 (U+{c:04X} is not)"));
        }
    }
    let cut = text.chars().count() > max;
    let stored: String = text
        .chars()
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_string();
    Ok((stored, cut))
}
