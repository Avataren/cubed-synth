//! Reading operation fields out of JSON, and the compact JSON shapes.

use super::limits::{parse_note, Res};
use crate::format::{Instrument, PListEntry, Step};
use serde_json::{json, Value};

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

pub(crate) fn opt_usize(op: &Value, key: &str) -> Res<Option<usize>> {
    match field(op, key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .map(|n| Some(n as usize))
            .ok_or_else(|| format!("'{key}' must be a whole number, not negative")),
    }
}

pub(crate) fn req_usize(op: &Value, key: &str) -> Res<usize> {
    opt_usize(op, key)?.ok_or_else(|| format!("'{key}' is needed (a whole number)"))
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
        Some(Value::Bool(b)) => Ok(Some(*b)),
        // Lua hosts may send 0 and 1.
        Some(Value::Number(n)) if n.as_u64().is_some_and(|n| n <= 1) => {
            Ok(Some(n.as_u64() == Some(1)))
        }
        Some(_) => Err(format!("'{key}' must be true or false")),
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

/// A byte from a JSON number.
pub(crate) fn byte_value(v: &Value, what: &str) -> Res<u8> {
    v.as_u64()
        .filter(|&n| n <= 255)
        .map(|n| n as u8)
        .ok_or_else(|| format!("{what} is 0-255"))
}

/// A note: a number (0 none, 1-60 C-1 to B-5) or text (`"C-1"`, `"---"`).
pub(crate) fn note_value(v: &Value) -> Res<u8> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .filter(|&n| n <= 63)
            .map(|n| n as u8)
            .ok_or_else(|| "A note is 0 to 63".to_string()),
        Value::String(s) => parse_note(s).ok_or_else(|| format!("'{s}' is not a note")),
        _ => Err("a note is a number or text like \"C-1\"".into()),
    }
}

/// A step as `[note, instrument, fx, param, fxb, fxb_param]` (the compact shape
/// every read returns: always six numbers, the last two 0 in an AHX song).
pub fn step_json(s: &Step) -> Value {
    json!([s.note, s.instrument, s.fx, s.fx_param, s.fxb, s.fxb_param])
}

/// The fields of one step an edit changes (`None` leaves a field alone).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepUpdate {
    pub note: Option<u8>,
    pub instrument: Option<u8>,
    pub fx: Option<u8>,
    pub fx_param: Option<u8>,
    pub fxb: Option<u8>,
    pub fxb_param: Option<u8>,
}

impl StepUpdate {
    /// Every field of `s`.
    pub fn of(s: &Step) -> StepUpdate {
        StepUpdate {
            note: Some(s.note),
            instrument: Some(s.instrument),
            fx: Some(s.fx),
            fx_param: Some(s.fx_param),
            fxb: Some(s.fxb),
            fxb_param: Some(s.fxb_param),
        }
    }

    /// `s` with these fields changed.
    pub fn apply(&self, s: &Step) -> Step {
        Step {
            note: self.note.unwrap_or(s.note),
            instrument: self.instrument.unwrap_or(s.instrument),
            fx: self.fx.unwrap_or(s.fx),
            fx_param: self.fx_param.unwrap_or(s.fx_param),
            fxb: self.fxb.unwrap_or(s.fxb),
            fxb_param: self.fxb_param.unwrap_or(s.fxb_param),
        }
    }

    /// From an operation: `cell` / `step` as `[note, instrument, fx, param[,
    /// fxb, fxb_param]]`, or any of `note`, `instrument`, `fx`, `param` (or
    /// `fx_param`), `fxb`, `fxb_param`.
    pub fn from_op(op: &Value) -> Res<StepUpdate> {
        let mut u = StepUpdate::default();
        if let Some(a) = field(op, "cell").or_else(|| field(op, "step")) {
            let a = a
                .as_array()
                .ok_or("a cell is [note, instrument, fx, param, fxb, fxb_param]")?;
            if a.len() < 4 || a.len() > 6 {
                return Err(
                    "a cell is [note, instrument, fx, param] and for HVL also [.., fxb, fxb_param]"
                        .into(),
                );
            }
            u.note = Some(note_value(&a[0])?);
            u.instrument = Some(byte_value(&a[1], "an instrument")?);
            u.fx = Some(byte_value(&a[2], "an effect")?);
            u.fx_param = Some(byte_value(&a[3], "a parameter")?);
            u.fxb = Some(a.get(4).map_or(Ok(0), |v| byte_value(v, "an effect"))?);
            u.fxb_param = Some(a.get(5).map_or(Ok(0), |v| byte_value(v, "a parameter"))?);
        }
        if let Some(v) = field(op, "note") {
            u.note = Some(note_value(v)?);
        }
        if let Some(n) = opt_u8(op, "instrument")? {
            u.instrument = Some(n);
        }
        if let Some(n) = opt_u8(op, "fx")? {
            u.fx = Some(n);
        }
        if let Some(n) = opt_u8(op, "param")?.or(opt_u8(op, "fx_param")?) {
            u.fx_param = Some(n);
        }
        if let Some(n) = opt_u8(op, "fxb")? {
            u.fxb = Some(n);
        }
        if let Some(n) = opt_u8(op, "fxb_param")? {
            u.fxb_param = Some(n);
        }
        if u == StepUpdate::default() {
            return Err(
                "nothing to set: give 'cell' or any of note, instrument, fx, param, fxb, fxb_param"
                    .into(),
            );
        }
        Ok(u)
    }
}

/// A PList entry as `[note, waveform, fixed (0/1), fx0, param0, fx1, param1]`.
pub fn plist_entry_json(e: &PListEntry) -> Value {
    json!([
        e.note,
        e.waveform,
        e.fixed as u8,
        e.fx[0],
        e.fx_param[0],
        e.fx[1],
        e.fx_param[1]
    ])
}

/// A PList entry from the array [`plist_entry_json`] makes.
pub fn plist_entry_from_json(v: &Value) -> Res<PListEntry> {
    let a = v
        .as_array()
        .filter(|a| a.len() == 7)
        .ok_or("a PList row is [note, waveform, fixed, fx0, param0, fx1, param1]")?;
    let b = |i: usize, what: &str| byte_value(&a[i], what);
    Ok(PListEntry {
        note: match &a[0] {
            Value::String(s) => parse_note(s).ok_or_else(|| format!("'{s}' is not a note"))?,
            v => byte_value(v, "a note")?,
        },
        waveform: b(1, "a tone")?,
        fixed: match &a[2] {
            Value::Bool(b) => *b,
            v => byte_value(v, "fixed")? != 0,
        },
        fx: [b(3, "a command")?, b(5, "a command")?],
        fx_param: [b(4, "a parameter")?, b(6, "a parameter")?],
    })
}

/// The fields of one PList row an edit changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PListUpdate {
    pub note: Option<u8>,
    pub waveform: Option<u8>,
    pub fixed: Option<bool>,
    pub fx: [Option<u8>; 2],
    pub fx_param: [Option<u8>; 2],
}

impl PListUpdate {
    pub fn apply(&self, e: &PListEntry) -> PListEntry {
        PListEntry {
            note: self.note.unwrap_or(e.note),
            waveform: self.waveform.unwrap_or(e.waveform),
            fixed: self.fixed.unwrap_or(e.fixed),
            fx: [self.fx[0].unwrap_or(e.fx[0]), self.fx[1].unwrap_or(e.fx[1])],
            fx_param: [
                self.fx_param[0].unwrap_or(e.fx_param[0]),
                self.fx_param[1].unwrap_or(e.fx_param[1]),
            ],
        }
    }

    /// From an operation: `entry` as the array of [`plist_entry_json`], or any
    /// of `note`, `waveform`, `fixed`, `fx0`, `param0`, `fx1`, `param1`.
    pub fn from_op(op: &Value) -> Res<PListUpdate> {
        let mut u = PListUpdate::default();
        if let Some(v) = field(op, "entry") {
            let e = plist_entry_from_json(v)?;
            u = PListUpdate {
                note: Some(e.note),
                waveform: Some(e.waveform),
                fixed: Some(e.fixed),
                fx: [Some(e.fx[0]), Some(e.fx[1])],
                fx_param: [Some(e.fx_param[0]), Some(e.fx_param[1])],
            };
        }
        if let Some(v) = field(op, "note") {
            u.note = Some(note_value(v)?);
        }
        u.waveform = opt_u8(op, "waveform")?.or(u.waveform);
        u.fixed = opt_bool(op, "fixed")?.or(u.fixed);
        for k in 0..2 {
            u.fx[k] = opt_u8(op, &format!("fx{k}"))?.or(u.fx[k]);
            u.fx_param[k] = opt_u8(op, &format!("param{k}"))?.or(u.fx_param[k]);
        }
        if u == PListUpdate::default() {
            return Err("nothing to set: give 'entry' or any of note, waveform, fixed, fx0, param0, fx1, param1".into());
        }
        Ok(u)
    }
}

/// An instrument as an object: the Rust field names, the envelope nested, and
/// the PList's speed and length (its rows are [`plist`](super::AhxDoc::plist)).
pub fn instrument_json(n: usize, ins: &Instrument) -> Value {
    json!({
        "n": n,
        "name": ins.name,
        "volume": ins.volume,
        "wave_length": ins.wave_length,
        "filter_lower_limit": ins.filter_lower_limit,
        "filter_upper_limit": ins.filter_upper_limit,
        "filter_speed": ins.filter_speed,
        "square_lower_limit": ins.square_lower_limit,
        "square_upper_limit": ins.square_upper_limit,
        "square_speed": ins.square_speed,
        "vibrato_delay": ins.vibrato_delay,
        "vibrato_speed": ins.vibrato_speed,
        "vibrato_depth": ins.vibrato_depth,
        "hard_cut_release": ins.hard_cut_release,
        "hard_cut_release_frames": ins.hard_cut_release_frames,
        "envelope": {
            "a_frames": ins.envelope.a_frames,
            "a_volume": ins.envelope.a_volume,
            "d_frames": ins.envelope.d_frames,
            "d_volume": ins.envelope.d_volume,
            "s_frames": ins.envelope.s_frames,
            "r_frames": ins.envelope.r_frames,
            "r_volume": ins.envelope.r_volume,
        },
        "plist_speed": ins.plist.speed,
        "plist_length": ins.plist.entries.len(),
    })
}

/// `ins` with the fields `op` names changed (the object [`instrument_json`]
/// makes, any subset; `envelope` is an object too, any subset). The PList rows
/// are not touched. `n`, `op` and unknown keys are ignored.
pub fn instrument_from_op(op: &Value, base: &Instrument) -> Res<Instrument> {
    let mut ins = base.clone();
    if let Some(name) = opt_str(op, "name")? {
        ins.name = name.to_string();
    }
    macro_rules! byte {
        ($key:literal, $field:expr) => {
            if let Some(v) = opt_u8(op, $key)? {
                $field = v;
            }
        };
    }
    byte!("volume", ins.volume);
    byte!("wave_length", ins.wave_length);
    byte!("filter_lower_limit", ins.filter_lower_limit);
    byte!("filter_upper_limit", ins.filter_upper_limit);
    byte!("filter_speed", ins.filter_speed);
    byte!("square_lower_limit", ins.square_lower_limit);
    byte!("square_upper_limit", ins.square_upper_limit);
    byte!("square_speed", ins.square_speed);
    byte!("vibrato_delay", ins.vibrato_delay);
    byte!("vibrato_speed", ins.vibrato_speed);
    byte!("vibrato_depth", ins.vibrato_depth);
    byte!("hard_cut_release_frames", ins.hard_cut_release_frames);
    byte!("plist_speed", ins.plist.speed);
    if let Some(b) = opt_bool(op, "hard_cut_release")? {
        ins.hard_cut_release = b;
    }
    if let Some(env) = field(op, "envelope") {
        if !env.is_object() {
            return Err("'envelope' must be an object".into());
        }
        macro_rules! ebyte {
            ($key:literal, $field:expr) => {
                if let Some(v) = opt_u8(env, $key)? {
                    $field = v;
                }
            };
        }
        ebyte!("a_frames", ins.envelope.a_frames);
        ebyte!("a_volume", ins.envelope.a_volume);
        ebyte!("d_frames", ins.envelope.d_frames);
        ebyte!("d_volume", ins.envelope.d_volume);
        ebyte!("s_frames", ins.envelope.s_frames);
        ebyte!("r_frames", ins.envelope.r_frames);
        ebyte!("r_volume", ins.envelope.r_volume);
    }
    Ok(ins)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64 (padding optional, whitespace ignored).
pub fn base64_decode(text: &str) -> Res<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return Err("not base64".into()),
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for n in 0..40usize {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 37 + 5) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&bytes)).unwrap(), bytes);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_decode("TWE").unwrap(), b"Ma");
    }
}
