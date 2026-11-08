//! What an AHX or HVL song can hold, and what a person may type into it: note
//! names, step and instrument checks.

use crate::format::{Instrument, PListEntry, SongFormat, Step};
use crate::write::{HVL_BLANK_STEP, MAX_INSTRUMENTS};

pub(crate) type Res<T> = Result<T, String>;

/// The lowest note a step plays (C-1) and the highest (B-5); a step also holds
/// 61 to 63 (C-6 to D-6), which the engine plays as 60.
pub const NOTE_MIN: u8 = 1;
pub const NOTE_MAX: u8 = 60;
/// The most a step's note holds (6 bits).
pub const NOTE_FIELD_MAX: u8 = 63;
/// Note `n` is MIDI note `n + 23`: AHX note 1 (C-1) is MIDI 24.
pub use crate::format::MIDI_OFFSET;
/// An instrument's volume and the envelope's levels go to 64.
pub const VOLUME_MAX: u8 = 64;
/// Wave lengths the engine has tables for: 4 << 0 to 4 << 5 samples.
pub const WAVE_LENGTH_MAX: u8 = 5;
/// The PList waveforms an edit writes: 0 keeps the tone, 1 to 4 are triangle,
/// sawtooth, square and noise. The file holds up to 7 but the engine has no
/// table for the rest.
pub const PLIST_WAVEFORM_MAX: u8 = 4;
const NAMES: [&str; 12] = [
    "C-", "C#", "D-", "D#", "E-", "F-", "F#", "G-", "G#", "A-", "A#", "B-",
];

/// A note as text: `---` for none, `C-1` (note 1) to `B-5` (note 60), and
/// `C-6` to `D-6` for 61 to 63.
pub fn note_name(note: u8) -> String {
    if note == 0 {
        return "---".into();
    }
    let i = note as usize - 1;
    format!("{}{}", NAMES[i % 12], i / 12 + 1)
}

/// A note from text (`C-1`, `c#3`, `D1`; `---`, `...` and `` are none) or a
/// number (`"12"`). `None` when it is neither.
pub fn parse_note(text: &str) -> Option<u8> {
    let t = text.trim();
    if t.is_empty() || t == "---" || t == "..." || t == "-" {
        return Some(0);
    }
    if let Ok(n) = t.parse::<u8>() {
        return (n <= NOTE_FIELD_MAX).then_some(n);
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
    let (sharp, octave) = match rest.chars().next()? {
        '#' => (1, &rest[1..]),
        '-' => (0, &rest[1..]),
        c if c.is_ascii_digit() => (0, &rest[..]),
        _ => return None,
    };
    let octave: i32 = octave.parse().ok()?;
    let n = (octave - 1) * 12 + base + sharp + 1;
    (1..=NOTE_FIELD_MAX as i32).contains(&n).then_some(n as u8)
}

/// The MIDI note of an AHX note (`None` for no note).
pub fn midi_of(note: u8) -> Option<u8> {
    (note != 0).then(|| (note as i32 + MIDI_OFFSET) as u8)
}

/// The AHX note of a MIDI note (`None` outside C-1 to B-5).
pub fn note_of_midi(midi: i32) -> Option<u8> {
    let n = midi - MIDI_OFFSET;
    (NOTE_MIN as i32..=NOTE_MAX as i32)
        .contains(&n)
        .then_some(n as u8)
}

/// Why a step cannot be written, or `Ok`. A note is 0 to 63 (an HVL step also
/// not 63: the byte marks a blank step in the file), an instrument 0 to 63, an
/// effect one hex digit and a parameter a byte; an AHX step has no second
/// effect column.
pub fn check_step(format: SongFormat, s: &Step) -> Res<()> {
    if s.note > NOTE_FIELD_MAX {
        return Err(format!("A note is 0 to {NOTE_FIELD_MAX} (got {}).", s.note));
    }
    if s.instrument as usize > MAX_INSTRUMENTS {
        return Err(format!(
            "Instruments go up to {MAX_INSTRUMENTS} (got {}).",
            s.instrument
        ));
    }
    if s.fx > 15 || s.fxb > 15 {
        return Err("An effect command is one hex digit.".into());
    }
    match format {
        SongFormat::Hvl => {
            if s.note == HVL_BLANK_STEP {
                return Err(
                    "HVL cannot store note 63 (D-6): that byte marks a blank step in the file. Use a note up to C#6."
                        .into(),
                );
            }
        }
        SongFormat::Ahx => {
            if s.fxb != 0 || s.fxb_param != 0 {
                return Err("AHX steps have one effect column.".into());
            }
        }
    }
    Ok(())
}

/// The largest parameter command `fx` takes in a PList: a version-0 AHX file
/// drops the high nibble of a filter-toggle (command 4) parameter when it loads,
/// so only 0 to 15 can be heard there.
pub fn plist_param_max(fx: u8, format: SongFormat, version: u8) -> u8 {
    if fx == 4 && format == SongFormat::Ahx && version == 0 {
        0x0f
    } else {
        255
    }
}

/// Why `e` cannot be written as a PList entry of a song of `format` and
/// `version`, or `Ok`.
pub fn check_plist_entry(format: SongFormat, version: u8, e: &PListEntry) -> Res<()> {
    if e.note > 0x3f {
        return Err(format!("A PList note is 0 to 63 (got {}).", e.note));
    }
    if e.waveform > PLIST_WAVEFORM_MAX {
        return Err(format!(
            "A PList tone is 0 to {PLIST_WAVEFORM_MAX} (got {}).",
            e.waveform
        ));
    }
    for k in 0..2 {
        let fx = e.fx[k];
        if !crate::write::plist_command_ok(fx, format) {
            return Err(format!(
                "Command {fx:X} is not available in {} PList.",
                if format == SongFormat::Hvl {
                    "an HVL"
                } else {
                    "an AHX"
                }
            ));
        }
        let max = plist_param_max(fx, format, version);
        if e.fx_param[k] > max {
            return Err(format!(
                "Command {fx:X} takes a parameter of 0 to {max:X} in a version-0 AHX file (got {:02X}).",
                e.fx_param[k]
            ));
        }
    }
    Ok(())
}

/// Why `ins` cannot be an instrument of a song of `format` and `version`:
/// the format's own limits (fields that do not fit their bits, PList commands
/// the format lacks) and the ranges an edit writes (volumes 0 to 64, wave
/// length 0 to 5, PList tones 0 to 4). A parsed song may hold more than an edit
/// writes (a wave length of 7), and still writes back as it is.
pub fn check_instrument(format: SongFormat, version: u8, ins: &Instrument) -> Res<()> {
    if let Some(p) = crate::write::instrument_problem(ins, format) {
        return Err(p);
    }
    if ins.volume > VOLUME_MAX {
        return Err(format!("volume is {}, not 0-{VOLUME_MAX}", ins.volume));
    }
    if ins.wave_length > WAVE_LENGTH_MAX {
        return Err(format!(
            "wave_length is {}, not 0-{WAVE_LENGTH_MAX}",
            ins.wave_length
        ));
    }
    let e = &ins.envelope;
    for (name, v) in [
        ("envelope.a_volume", e.a_volume),
        ("envelope.d_volume", e.d_volume),
        ("envelope.r_volume", e.r_volume),
    ] {
        if v > VOLUME_MAX {
            return Err(format!("{name} is {v}, not 0-{VOLUME_MAX}"));
        }
    }
    for (row, entry) in ins.plist.entries.iter().enumerate() {
        check_plist_entry(format, version, entry).map_err(|e| format!("PList row {row}: {e}"))?;
    }
    Ok(())
}

pub(crate) fn format_name(f: SongFormat) -> &'static str {
    match f {
        SongFormat::Ahx => "ahx",
        SongFormat::Hvl => "hvl",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_round_trip_through_their_names() {
        assert_eq!(note_name(0), "---");
        assert_eq!(note_name(1), "C-1");
        assert_eq!(note_name(13), "C-2");
        assert_eq!(note_name(60), "B-5");
        assert_eq!(note_name(63), "D-6");
        for n in 0..=63u8 {
            assert_eq!(parse_note(&note_name(n)), Some(n), "{n}");
            assert_eq!(parse_note(&n.to_string()), Some(n));
        }
        assert_eq!(parse_note("c#3"), Some(26));
        assert_eq!(parse_note("D1"), Some(3));
        assert_eq!(parse_note("C-7"), None);
        assert_eq!(parse_note("H-1"), None);
        assert_eq!(parse_note("64"), None);
        assert_eq!(midi_of(1), Some(24));
        assert_eq!(note_of_midi(83), Some(60));
        assert_eq!(note_of_midi(84), None);
        assert_eq!(note_of_midi(23), None);
    }
}
