//! Reading operation fields out of JSON, and the compact JSON shapes.

use serde_json::{json, Value};

use super::limits::{parse_effect, parse_note};
use crate::module::{Cell, Format, Note};

pub(crate) type Res<T> = Result<T, String>;

pub(crate) fn field<'a>(op: &'a Value, key: &str) -> Option<&'a Value> {
    match op.get(key) {
        None | Some(Value::Null) => None,
        Some(v) => Some(v),
    }
}

pub(crate) fn req_str<'a>(op: &'a Value, key: &str) -> Res<&'a str> {
    field(op, key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("'{key}' must be a string"))
}

pub(crate) fn opt_str<'a>(op: &'a Value, key: &str) -> Res<Option<&'a str>> {
    match field(op, key) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be a string")),
    }
}

pub(crate) fn req_usize(op: &Value, key: &str) -> Res<usize> {
    opt_usize(op, key)?.ok_or_else(|| format!("'{key}' is needed (a whole number)"))
}

pub(crate) fn opt_usize(op: &Value, key: &str) -> Res<Option<usize>> {
    match field(op, key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .map(|n| Some(n as usize))
            .ok_or_else(|| format!("'{key}' must be a whole number, not negative")),
    }
}

pub(crate) fn opt_i64(op: &Value, key: &str) -> Res<Option<i64>> {
    match field(op, key) {
        None => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be a whole number")),
    }
}

pub(crate) fn req_i64(op: &Value, key: &str) -> Res<i64> {
    opt_i64(op, key)?.ok_or_else(|| format!("'{key}' is needed (a whole number)"))
}

pub(crate) fn opt_bool(op: &Value, key: &str) -> Res<Option<bool>> {
    match field(op, key) {
        None => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be true or false")),
    }
}

/// A byte field (0-255).
pub(crate) fn opt_u8(op: &Value, key: &str) -> Res<Option<u8>> {
    match opt_i64(op, key)? {
        None => Ok(None),
        Some(n) if (0..=255).contains(&n) => Ok(Some(n as u8)),
        Some(n) => Err(format!("'{key}' is {n}, not 0-255")),
    }
}

/// A cell as `[note, instrument, volume, effect, param]`, the compact shape
/// every read returns. The note is a MIDI number (60 is middle C; each format's
/// range is in `info().note`), 0 for none, 255 for key off.
pub fn cell_json(cell: &Cell) -> Value {
    json!([
        note_number(cell.note),
        cell.instrument,
        cell.volume,
        cell.effect,
        cell.param
    ])
}

pub(crate) fn note_number(note: Note) -> u32 {
    match note {
        Note::None => 0,
        Note::Off => 255,
        Note::On(n) => n as u32,
    }
}

pub(crate) fn note_from_number(n: u64) -> Res<Note> {
    match n {
        0 => Ok(Note::None),
        255 => Ok(Note::Off),
        1..=254 => Ok(Note::On(n as u8)),
        _ => Err(format!("note {n} is not 0-255")),
    }
}

/// A note field: a number (0 none, 255 off, else MIDI), or text (`"C-4"`,
/// `"off"`, `"---"`).
pub(crate) fn note_value(format: Format, v: &Value) -> Res<Note> {
    match v {
        Value::Number(n) => note_from_number(n.as_u64().ok_or("a note is not negative")?),
        Value::String(s) => parse_note(format, s).ok_or_else(|| format!("'{s}' is not a note")),
        _ => Err("a note is a number or text like \"C-4\"".into()),
    }
}

/// An effect field: the command number, or its letter (`"C"`, `"0"`).
pub(crate) fn effect_value(format: Format, v: &Value) -> Res<u8> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .filter(|&n| n <= 255)
            .map(|n| n as u8)
            .ok_or_else(|| "an effect is 0-255".to_string()),
        Value::String(s) => {
            parse_effect(format, s).ok_or_else(|| format!("'{s}' is not an effect letter"))
        }
        _ => Err("an effect is a number or a letter".into()),
    }
}

fn byte_value(v: &Value, what: &str) -> Res<u8> {
    v.as_u64()
        .filter(|&n| n <= 255)
        .map(|n| n as u8)
        .ok_or_else(|| format!("{what} is 0-255"))
}

/// The fields of one cell an edit changes (`None` leaves a field alone).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellUpdate {
    pub note: Option<Note>,
    pub instrument: Option<u8>,
    pub volume: Option<u8>,
    pub effect: Option<u8>,
    pub param: Option<u8>,
}

impl CellUpdate {
    /// Every field of `cell`, to replace a cell outright.
    pub fn whole(cell: &Cell) -> CellUpdate {
        CellUpdate {
            note: Some(cell.note),
            instrument: Some(cell.instrument),
            volume: Some(cell.volume),
            effect: Some(cell.effect),
            param: Some(cell.param),
        }
    }

    /// From an operation: `cell: [n, i, v, e, p]` for all five, or any of
    /// `note`, `instrument`, `volume`, `effect`, `param`.
    pub(crate) fn from_op(format: Format, op: &Value) -> Res<CellUpdate> {
        let mut u = CellUpdate::default();
        if let Some(cell) = field(op, "cell") {
            let (c, _) = cell_from_json(format, cell)?;
            u = CellUpdate::whole(&c);
        }
        if let Some(v) = field(op, "note") {
            u.note = Some(note_value(format, v)?);
        }
        if let Some(n) = opt_u8(op, "instrument")? {
            u.instrument = Some(n);
        }
        if let Some(n) = opt_u8(op, "volume")? {
            u.volume = Some(n);
        }
        if let Some(v) = field(op, "effect") {
            u.effect = Some(effect_value(format, v)?);
        }
        if let Some(n) = opt_u8(op, "param")? {
            u.param = Some(n);
        }
        Ok(u)
    }
}

/// A cell from its compact array (5 numbers; a MOD's may add the period as
/// a sixth, which `copy_range` does for notes off ProTracker's table).
pub(crate) fn cell_from_json(format: Format, v: &Value) -> Res<(Cell, Option<u16>)> {
    let a = v
        .as_array()
        .ok_or("a cell is [note, instrument, volume, effect, param]")?;
    if a.len() < 5 || a.len() > 6 {
        return Err("a cell is [note, instrument, volume, effect, param]".into());
    }
    let note = note_value(format, &a[0])?;
    let cell = Cell {
        note,
        period: 0,
        instrument: byte_value(&a[1], "an instrument")?,
        volume: byte_value(&a[2], "a volume column value")?,
        effect: effect_value(format, &a[3])?,
        param: byte_value(&a[4], "a parameter")?,
    };
    let period = match a.get(5) {
        Some(p) => Some(
            p.as_u64()
                .filter(|&p| p <= 0xfff)
                .ok_or("a period is 0-4095")? as u16,
        ),
        None => None,
    };
    Ok((cell, period))
}

/// The standard base64 alphabet, padding optional, whitespace ignored.
pub(crate) fn base64_decode(text: &str) -> Res<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for b in text.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return Err(format!("'{}' is not base64", b as char)),
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}
