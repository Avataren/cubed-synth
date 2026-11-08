//! Reading operation fields out of JSON.

use serde_json::Value;

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

/// A field bounded by `max` (0..=max).
pub(crate) fn opt_max(op: &Value, key: &str, max: u8) -> Res<Option<u8>> {
    match opt_i64(op, key)? {
        None => Ok(None),
        Some(n) if (0..=max as i64).contains(&n) => Ok(Some(n as u8)),
        Some(n) => Err(format!("'{key}' is {n}, not 0-{max}")),
    }
}

pub(crate) fn byte_value(v: &Value, what: &str) -> Res<u8> {
    v.as_u64()
        .filter(|&n| n <= 255)
        .map(|n| n as u8)
        .ok_or_else(|| format!("{what} is 0-255"))
}
