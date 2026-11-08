//! The FM instrument's JSON: what a read shows and what an edit takes. Shared by
//! the song document and the stand-alone instrument document.

use serde_json::{json, Value};

use super::json::{field, opt_bool, opt_i64, opt_max, Res};
use crate::fm::{range, Operator, OperatorEdit, Voice, VoiceEdit, FM_BYTES};

/// An operator as JSON: `{"tremolo", "vibrato", "sustain", "ksr", "multiplier",
/// "key_scale_level", "total_level" (0 loudest), "level" (63 loudest),
/// "attack", "decay", "sustain_level", "release", "waveform"}`.
pub fn operator_json(o: &Operator) -> Value {
    json!({
        "tremolo": o.tremolo,
        "vibrato": o.vibrato,
        "sustain": o.sustain,
        "ksr": o.ksr,
        "multiplier": o.multiplier,
        "key_scale_level": o.key_scale_level,
        "total_level": o.total_level,
        "level": o.level(),
        "attack": o.attack,
        "decay": o.decay,
        "sustain_level": o.sustain_level,
        "release": o.release,
        "waveform": o.waveform,
    })
}

/// A voice as JSON: `{"modulator", "carrier", "feedback", "connection":
/// "fm" | "additive"}`.
pub fn voice_json(v: &Voice) -> Value {
    json!({
        "modulator": operator_json(&v.modulator),
        "carrier": operator_json(&v.carrier),
        "feedback": v.feedback,
        "connection": if v.additive { "additive" } else { "fm" },
    })
}

/// An operator edit from an object's fields (any of those [`operator_json`]
/// shows; `ksl` is short for `key_scale_level`).
pub fn operator_edit(v: &Value) -> Res<OperatorEdit> {
    let flag = |k: &str| opt_bool(v, k);
    let num = |k: &str, max: u8| opt_max(v, k, max);
    Ok(OperatorEdit {
        tremolo: flag("tremolo")?,
        vibrato: flag("vibrato")?,
        sustain: flag("sustain")?,
        ksr: flag("ksr")?,
        multiplier: num("multiplier", range::MULTIPLIER)?,
        key_scale_level: match num("key_scale_level", range::KEY_SCALE_LEVEL)? {
            Some(n) => Some(n),
            None => num("ksl", range::KEY_SCALE_LEVEL)?,
        },
        total_level: num("total_level", range::TOTAL_LEVEL)?,
        level: num("level", range::TOTAL_LEVEL)?,
        attack: num("attack", range::ATTACK)?,
        decay: num("decay", range::DECAY)?,
        sustain_level: num("sustain_level", range::SUSTAIN_LEVEL)?,
        release: num("release", range::RELEASE)?,
        waveform: num("waveform", range::WAVEFORM)?,
    })
}

/// A voice edit from an operation: `modulator` and `carrier` objects (fields as
/// in [`operator_json`]), `feedback` 0-7 and `connection` (`"fm"` or
/// `"additive"`, or `additive: true | false`).
pub fn voice_edit(op: &Value) -> Res<VoiceEdit> {
    let mut e = VoiceEdit::default();
    if let Some(m) = field(op, "modulator") {
        e.modulator = operator_edit(m).map_err(|x| format!("modulator: {x}"))?;
    }
    if let Some(c) = field(op, "carrier") {
        e.carrier = operator_edit(c).map_err(|x| format!("carrier: {x}"))?;
    }
    e.feedback = opt_max(op, "feedback", range::FEEDBACK)?;
    e.additive = opt_bool(op, "additive")?;
    if let Some(c) = field(op, "connection") {
        e.additive = Some(match c.as_str() {
            Some("fm") => false,
            Some("additive") => true,
            _ => return Err("'connection' is \"fm\" or \"additive\"".into()),
        });
    }
    Ok(e)
}

/// `fm: [11 bytes]` of an operation.
pub fn fm_bytes(v: &Value, what: &str) -> Res<[u8; FM_BYTES]> {
    let a = v
        .as_array()
        .filter(|a| a.len() == FM_BYTES)
        .ok_or_else(|| format!("{what} is {FM_BYTES} bytes"))?;
    let mut out = [0u8; FM_BYTES];
    for (o, b) in out.iter_mut().zip(a) {
        *o = b
            .as_u64()
            .filter(|&n| n <= 255)
            .ok_or_else(|| format!("{what} holds bytes (0-255)"))? as u8;
    }
    Ok(out)
}

/// A signed byte field.
pub fn opt_i8(op: &Value, key: &str) -> Res<Option<i8>> {
    match opt_i64(op, key)? {
        None => Ok(None),
        Some(n) if (-128..=127).contains(&n) => Ok(Some(n as i8)),
        Some(n) => Err(format!("'{key}' is {n}, not -128 to 127")),
    }
}
