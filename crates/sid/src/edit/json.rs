//! Reading operation fields out of JSON, and the compact JSON shapes.

use serde_json::{json, Value};

use super::limits::parse_note;
use crate::song::{Row, TableKind, TableRow};

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

pub(crate) fn req_table(op: &Value) -> Res<TableKind> {
    let name = req_str(op, "table")?;
    TableKind::from_name(name)
        .ok_or_else(|| format!("'{name}' is not a table (wave, pulse, filter, speed)"))
}

/// A row as `[note, instrument, command, param]`, the compact shape every read returns.
pub fn row_json(row: &Row) -> Value {
    json!([row.note, row.instrument, row.command, row.param])
}

/// A table row as `[left, right]`.
pub fn table_row_json(row: &TableRow) -> Value {
    json!([row.left, row.right])
}

/// Latin-1 bytes as text.
pub(crate) fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Text as latin-1 bytes (a character beyond U+00FF is refused).
pub(crate) fn to_latin1(text: &str, what: &str) -> Res<Vec<u8>> {
    text.chars()
        .map(|c| {
            u8::try_from(c as u32)
                .map_err(|_| format!("{what} has a character ('{c}') outside latin-1"))
        })
        .collect()
}

/// A note field: a number (the song's note: 0 none, 1-93, 126 off, 127 on), or
/// text (`"C-4"`, `"off"`, `"---"`).
pub(crate) fn note_value(v: &Value) -> Res<u8> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .filter(|&n| n <= 255)
            .map(|n| n as u8)
            .ok_or_else(|| "a note is 0-255".to_string()),
        Value::String(s) => parse_note(s).ok_or_else(|| format!("'{s}' is not a note")),
        _ => Err("a note is a number or text like \"C-4\"".into()),
    }
}

fn byte_value(v: &Value, what: &str) -> Res<u8> {
    v.as_u64()
        .filter(|&n| n <= 255)
        .map(|n| n as u8)
        .ok_or_else(|| format!("{what} is 0-255"))
}

/// The fields of one row an edit changes (`None` leaves a field alone).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellUpdate {
    pub note: Option<u8>,
    pub instrument: Option<u8>,
    pub command: Option<u8>,
    pub param: Option<u8>,
}

impl CellUpdate {
    /// Every field of `row`, to replace a row outright.
    pub fn whole(row: &Row) -> CellUpdate {
        CellUpdate {
            note: Some(row.note),
            instrument: Some(row.instrument),
            command: Some(row.command),
            param: Some(row.param),
        }
    }

    /// `row` with the fields set.
    pub fn applied(&self, row: &Row) -> Row {
        Row {
            note: self.note.unwrap_or(row.note),
            instrument: self.instrument.unwrap_or(row.instrument),
            command: self.command.unwrap_or(row.command),
            param: self.param.unwrap_or(row.param),
        }
    }

    /// From an operation: `cell: [n, i, c, p]` for all four, or any of `note`,
    /// `midi` (a MIDI note number, C-4 is 60), `instrument`, `command` (a number
    /// or its hex digit as text), `param`.
    pub(crate) fn from_op(op: &Value) -> Res<CellUpdate> {
        let mut u = CellUpdate::default();
        if let Some(cell) = field(op, "cell") {
            u = CellUpdate::whole(&row_from_json(cell)?);
        }
        if let Some(v) = field(op, "note") {
            u.note = Some(note_value(v)?);
        }
        if let Some(m) = opt_i64(op, "midi")? {
            u.note = Some(super::limits::midi_to_note(m)?);
        }
        if let Some(n) = opt_u8(op, "instrument")? {
            u.instrument = Some(n);
        }
        if let Some(v) = field(op, "command") {
            u.command = Some(command_value(v)?);
        }
        if let Some(n) = opt_u8(op, "param")? {
            u.param = Some(n);
        }
        Ok(u)
    }
}

/// A command field: the number 0-15, or its hex digit as text.
pub(crate) fn command_value(v: &Value) -> Res<u8> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .filter(|&n| n <= 15)
            .map(|n| n as u8)
            .ok_or_else(|| "a command is 0-F".to_string()),
        Value::String(s) if s.len() == 1 => {
            u8::from_str_radix(s, 16).map_err(|_| format!("'{s}' is not a command digit (0-F)"))
        }
        _ => Err("a command is a number 0-15 or a hex digit".into()),
    }
}

/// A row from its compact array: `[note, instrument, command, param]`.
pub(crate) fn row_from_json(v: &Value) -> Res<Row> {
    let a = v
        .as_array()
        .filter(|a| a.len() == 4)
        .ok_or("a row is [note, instrument, command, param]")?;
    Ok(Row {
        note: note_value(&a[0])?,
        instrument: byte_value(&a[1], "an instrument")?,
        command: command_value(&a[2])?,
        param: byte_value(&a[3], "a parameter")?,
    })
}
