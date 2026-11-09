//! A sampler node's sample, in the patch.
//!
//! The DAW keeps a sampler's sample as the asset `sample_<node id>` in `audioAssets`: `base64Data`
//! is the base64 text of a 16-bit PCM WAV file (what its `float32ArrayToWav` writes), beside
//! `sampleRate`, `channels`, `rootNote`, `fileName` and `duration`, and the node's state in
//! `synthState.samplers` has `sampleLength` (frames), `sampleRate`, `channels` and `fileName`
//! as well. These operations keep both in step, so the DAW reads the patch back and the engine
//! plays it (the native loader reads the asset when it builds the patch).
//!
//! Operations (`apply`):
//!
//! - `{"op": "load_sample", "node": id, ...}` decodes the audio and puts it in. The audio is
//!   `"data"`: base64 text of a WAV file (8, 16, 24, 32-bit PCM or 32-bit float, mono or stereo), or
//!   of raw PCM when `"format"` (`"i16"`, `"u8"`, `"i24"`, `"i32"`, `"f32"`; little endian) or `"rate"` is
//!   given (then `"rate"` is needed, `"channels"` is 1 unless said); or `"samples"`, a list of numbers
//!   (-1 to 1, interleaved) with `"rate"` and `"channels"`. Options: `"mix"` (true: the channels
//!   averaged to mono), `"normalize"` (true or a peak between 0 and 1), `"root_note"` (0 to 127; the
//!   node keeps its own if not given), `"name"` (the file name), `"peaks"` (how many buckets of a
//!   waveform to answer with). A sample is stored as 16-bit PCM, so one of more bits is rounded
//!   to it. It may hold at most [`MAX_VALUES`] numbers. The loop points go back to the whole
//!   sample. Answers `{asset, frames, rate, channels, duration, peak, root_note, name, bytes,
//!   mixed, peaks?}`.
//! - `{"op": "clear_sample", "node": id}` takes it out (the node plays its built-in tone again).
//!   Answers `{cleared}`.
//! - `{"op": "set_sample", "node": id, ...}` sets what the DAW's sampler panel has, all or
//!   some, checked before anything is changed: `"root_note"`, `"loop_mode"` (`"off"`, `"loop"`,
//!   `"pingpong"` or 0 to 2), `"loop_start"` and `"loop_end"` (0 to 1 of the sample, or frames
//!   with `"unit": "frames"`; as in the DAW, moving the start past the end pushes the end on, and
//!   the end stays [`LOOP_EPSILON`] after the start), `"gain"` (0 to 1.5), `"octave"`,
//!   `"semitones"`, `"cents"` (the tuning), `"trigger"` (`"free"`, `"gate"`, `"oneshot"` or 0
//!   to 2), `"active"`. Answers `{state, edits}`: the sampler's state and a `ParamEdit` for each
//!   thing that changed (`{node, param, value}`, as `set_param` answers with), which a host can
//!   send to a playing synth instead of rebuilding it.
//!
//! Queries (`PatchDoc::query`): `sample_info` (what is loaded, how long, the loop points in
//! fractions and in frames, the node's settings), `sample_peaks` (`"buckets"`, `"from"`, `"to"` in
//! frames: the minimum and maximum of each bucket, for drawing the waveform) and `sample_wav`
//! (the asset's WAV file as base64, to save).

use serde_json::{json, Value};

use super::assets::asset_entry;
use super::{number, str_field, to_f32, PatchDoc, Res};
use crate::catalog::{node_spec, StateShape};
use crate::live::{node_key, ParamEdit};
use crate::wav::{
    base64_decode, base64_decoded_len, base64_encode, decode_raw, decode_wav, encode_wav_i16, Pcm,
    RawFormat,
};

/// A sample holds at most this many numbers (frames times channels): 16 Mi, six minutes of
/// stereo at 44.1 kHz, 32 MiB as 16-bit PCM.
pub const MAX_VALUES: usize = 16 << 20;
/// The DAW's slider keeps the loop end this far (of the sample) after the start.
pub const LOOP_EPSILON: f64 = 0.0005;
const MAX_BUCKETS: usize = 8192;

/// The asset id of a sampler's sample.
pub fn asset_id(node: &str) -> String {
    format!("sample_{node}")
}

fn bool_field(op: &Value, key: &str) -> Res<Option<bool>> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("'{key}' must be true or false")),
    }
}

fn opt_number(op: &Value, key: &str) -> Res<Option<f64>> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite())
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be a number")),
    }
}

fn opt_str<'a>(op: &'a Value, key: &str) -> Res<Option<&'a str>> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("'{key}' must be text")),
    }
}

fn mode_index(value: &Value, key: &str, names: &[&str], aliases: &[(&str, usize)]) -> Res<usize> {
    match value {
        Value::String(name) => {
            let flat = name.to_ascii_lowercase().replace(['-', '_', ' '], "");
            names
                .iter()
                .position(|n| n.to_ascii_lowercase() == flat)
                .or_else(|| aliases.iter().find(|(a, _)| *a == flat).map(|(_, i)| *i))
                .ok_or_else(|| {
                    format!(
                        "'{key}' is one of {} (or 0 to {})",
                        names.join(", "),
                        names.len() - 1
                    )
                })
        }
        Value::Number(n) => n
            .as_u64()
            .filter(|n| (*n as usize) < names.len())
            .map(|n| n as usize)
            .ok_or_else(|| format!("'{key}' is 0 to {}", names.len() - 1)),
        _ => Err(format!("'{key}' is a name or a number")),
    }
}

impl PatchDoc {
    /// Checks that `id` is a node of type `type_name`.
    pub(super) fn expect_node(&self, id: &str, type_name: &str) -> Res<()> {
        let node = self
            .node(id)
            .ok_or_else(|| format!("no such node '{id}'"))?;
        if node.type_name != type_name {
            let title = node_spec(&node.type_name).map_or(node.type_name.as_str(), |s| s.title);
            let wanted = node_spec(type_name).map_or(type_name, |s| s.title);
            return Err(format!("'{id}' is a {title}, not a {wanted}"));
        }
        Ok(())
    }

    /// The sampler's state object, made if it has none.
    fn sampler_state_mut(&mut self, id: &str) -> &mut serde_json::Map<String, Value> {
        let spec = node_spec("sampler").expect("the sampler is in the catalogue");
        self.create_state(spec, id, false);
        debug_assert_eq!(spec.state_shape, StateShape::Map);
        self.synth_mut()
            .get_mut("samplers")
            .and_then(|m| m.get_mut(id))
            .and_then(Value::as_object_mut)
            .expect("the state was just made")
    }

    fn sampler_f64(&self, id: &str, key: &str) -> f64 {
        self.node_state(id)
            .and_then(|s| s.get(key))
            .and_then(Value::as_f64)
            .or_else(|| {
                node_spec("sampler")
                    .and_then(|s| s.param(key))
                    .map(|p| p.default)
            })
            .unwrap_or(0.0)
    }

    // ----- load_sample ------------------------------------------------------------------

    fn sample_source(op: &Value) -> Res<Pcm> {
        let rate_field = opt_number(op, "rate")?;
        let channels_field = opt_number(op, "channels")?;
        if let Some(list) = op.get("samples") {
            let list = list
                .as_array()
                .ok_or("'samples' must be a list of numbers")?;
            let rate = rate_field.ok_or("'samples' needs a 'rate'")?;
            let samples = list
                .iter()
                .map(|v| {
                    v.as_f64()
                        .filter(|n| n.is_finite())
                        .map(|n| n as f32)
                        .ok_or("'samples' holds something that is not a number")
                })
                .collect::<Result<Vec<f32>, _>>()?;
            return Self::pcm_from(samples, channels_field.unwrap_or(1.0), rate);
        }
        let data = str_field(op, "data").map_err(|_| {
            "load_sample needs 'data' (base64 of a WAV file or raw PCM) or 'samples'".to_string()
        })?;
        if data.len() > MAX_VALUES * 3 {
            return Err(format!(
                "that is too much audio for a patch (at most {} MiB of base64)",
                (MAX_VALUES * 3) >> 20
            ));
        }
        let bytes = base64_decode(data)?;
        let format = opt_str(op, "format")?;
        let raw = match (format, rate_field) {
            (Some("wav"), _) | (None, None) => None,
            (Some(name), _) => Some(RawFormat::parse(name).ok_or_else(|| {
                format!("'format' is wav, i16, u8, i24, i32 or f32, not '{name}'")
            })?),
            (None, Some(_)) => Some(RawFormat::I16),
        };
        match raw {
            None => {
                let pcm = decode_wav(&bytes)?;
                Self::pcm_from(pcm.samples, pcm.channels as f64, pcm.rate as f64)
            }
            Some(format) => {
                let rate = rate_field.ok_or("raw audio needs a 'rate'")?;
                Self::pcm_from(
                    decode_raw(&bytes, format),
                    channels_field.unwrap_or(1.0),
                    rate,
                )
            }
        }
    }

    fn pcm_from(samples: Vec<f32>, channels: f64, rate: f64) -> Res<Pcm> {
        if !(1.0..=64.0).contains(&channels) || channels.fract() != 0.0 {
            return Err(format!(
                "'channels' is a whole number from 1, not {channels}"
            ));
        }
        if !(100.0..=768_000.0).contains(&rate) || rate.fract() != 0.0 {
            return Err(format!(
                "'rate' is a whole number of Hz, 100 to 768000, not {rate}"
            ));
        }
        if samples.len() > MAX_VALUES {
            return Err(format!(
                "that is too long for a patch ({} numbers, at most {MAX_VALUES}): trim it or lower its rate",
                samples.len()
            ));
        }
        Ok(Pcm {
            samples,
            channels: channels as usize,
            rate: rate as u32,
        })
    }

    pub(super) fn op_load_sample(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "sampler")?;
        let mut pcm = Self::sample_source(op)?;
        let root_note = match opt_number(op, "root_note")? {
            Some(n) => Some(
                node_spec("sampler")
                    .and_then(|s| s.param("rootNote"))
                    .expect("the sampler has a root note")
                    .check(&json!(n))?
                    .as_i64()
                    .unwrap_or(60),
            ),
            None => None,
        };
        let name = opt_str(op, "name")?.map(str::to_string);
        let mixed = bool_field(op, "mix")?.unwrap_or(false) && pcm.channels > 1;
        if mixed {
            pcm = pcm.to_mono();
        }
        if pcm.channels > 2 {
            return Err(format!(
                "a sampler plays one or two channels, this has {}: pass \"mix\": true",
                pcm.channels
            ));
        }
        if pcm.frames() == 0 {
            return Err("there is no audio in that".into());
        }
        match op.get("normalize") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => {}
            Some(Value::Bool(true)) => pcm.normalize(1.0),
            Some(v) => {
                let peak = v
                    .as_f64()
                    .filter(|p| *p > 0.0 && *p <= 1.0)
                    .ok_or("'normalize' is true or a peak above 0 up to 1")?;
                pcm.normalize(peak as f32);
            }
        }
        let buckets = match opt_number(op, "peaks")? {
            Some(n) if n >= 1.0 => Some((n as usize).min(MAX_BUCKETS)),
            Some(_) => return Err("'peaks' is a number of buckets, 1 or more".into()),
            None => None,
        };

        // Everything is checked: write.
        let wav = encode_wav_i16(&pcm.samples, pcm.channels, pcm.rate);
        let id = asset_id(&node);
        let root = root_note.unwrap_or_else(|| self.sampler_f64(&node, "rootNote") as i64);
        let mut entry = asset_entry(
            &id,
            "sample",
            base64_encode(&wav),
            pcm.rate,
            pcm.channels,
            pcm.seconds(),
        );
        entry["rootNote"] = json!(root);
        if let Some(name) = &name {
            entry["fileName"] = json!(name);
        }
        self.put_asset(&id, Some(entry));

        let frames = pcm.frames();
        let state = self.sampler_state_mut(&node);
        state.insert("sampleLength".into(), json!(frames));
        state.insert("sampleRate".into(), json!(pcm.rate));
        state.insert("channels".into(), json!(pcm.channels));
        state.insert("loopStart".into(), json!(0));
        state.insert("loopEnd".into(), json!(1));
        state.insert("rootNote".into(), json!(root));
        match &name {
            Some(name) => {
                state.insert("fileName".into(), json!(name));
            }
            None => {
                state.remove("fileName");
            }
        }

        let peak = pcm.peak();
        let mut result = json!({
            "asset": id,
            "frames": frames,
            "rate": pcm.rate,
            "channels": pcm.channels,
            "duration": number(pcm.seconds()),
            "peak": number(peak as f64),
            "root_note": root,
            "name": name,
            "bytes": wav.len(),
            "mixed": mixed,
        });
        if let Some(buckets) = buckets {
            result["peaks"] = peaks_json(&pcm, 0, frames, buckets);
        }
        self.keep_pcm(&id, std::sync::Arc::new(pcm));
        Ok(result)
    }

    pub(super) fn op_clear_sample(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "sampler")?;
        let id = asset_id(&node);
        if self.asset(&id).is_none() {
            return Ok(json!({"cleared": false}));
        }
        self.put_asset(&id, None);
        let state = self.sampler_state_mut(&node);
        state.insert("sampleLength".into(), json!(0));
        state.remove("fileName");
        Ok(json!({"cleared": true}))
    }

    // ----- set_sample -------------------------------------------------------------------

    pub(super) fn op_set_sample(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "sampler")?;
        let spec = node_spec("sampler").expect("the sampler is in the catalogue");

        // What to set, in the order of the keys, as (key, value).
        let mut sets: Vec<(&'static str, Value)> = Vec::new();
        let mut put = |key: &'static str, value: Value| {
            sets.retain(|(k, _)| *k != key);
            sets.push((key, value));
        };
        if let Some(n) = opt_number(op, "root_note")? {
            put("rootNote", json!(n));
        }
        if let Some(v) = op.get("loop_mode").filter(|v| !v.is_null()) {
            put(
                "loopMode",
                json!(mode_index(
                    v,
                    "loop_mode",
                    &["off", "loop", "pingpong"],
                    &[]
                )?),
            );
        }
        if let Some(v) = op.get("trigger").filter(|v| !v.is_null()) {
            put(
                "triggerMode",
                json!(mode_index(
                    v,
                    "trigger",
                    &["free", "gate", "oneshot"],
                    &[("freerunning", 0)]
                )?),
            );
        }
        if let Some(n) = opt_number(op, "gain")? {
            put("gain", json!(n));
        }
        for (field, key) in [
            ("octave", "detune_oct"),
            ("semitones", "detune_semi"),
            ("cents", "detune_cents"),
        ] {
            if let Some(n) = opt_number(op, field)? {
                put(key, json!(n));
            }
        }
        if let Some(on) = bool_field(op, "active")? {
            put("active", json!(on));
        }

        // The loop points, as the DAW's two sliders move them.
        let start = opt_number(op, "loop_start")?;
        let end = opt_number(op, "loop_end")?;
        if start.is_some() || end.is_some() {
            let scale = match opt_str(op, "unit")? {
                None | Some("fraction") => 1.0,
                Some("frames") => {
                    let id = asset_id(&node);
                    if self.asset(&id).is_none() {
                        return Err("loop points in frames need a sample: load_sample first".into());
                    }
                    self.pcm_of(&id)?.frames().max(1) as f64
                }
                Some(other) => return Err(format!("'unit' is fraction or frames, not '{other}'")),
            };
            let mut loop_start = self.sampler_f64(&node, "loopStart");
            let mut loop_end = self.sampler_f64(&node, "loopEnd");
            if let Some(s) = start {
                loop_start = (s / scale).clamp(0.0, 1.0);
                if loop_end <= loop_start {
                    loop_end = (loop_start + LOOP_EPSILON).min(1.0);
                }
            }
            if let Some(e) = end {
                let e = (e / scale).clamp(0.0, 1.0);
                loop_end = e.max(loop_start + LOOP_EPSILON).min(1.0);
            }
            // The start cannot be so close to the end of the sample that the end has no room.
            loop_start = loop_start.min(1.0 - LOOP_EPSILON);
            put("loopStart", json!(loop_start));
            put("loopEnd", json!(loop_end));
        }

        // Checked first, applied after.
        let mut checked = Vec::with_capacity(sets.len());
        for (key, value) in &sets {
            let index = spec.param_index(key).expect("a sampler parameter");
            checked.push((index, spec.params[index].check(value)?));
        }
        let mut edits = Vec::new();
        for ((key, _), (index, stored)) in sets.iter().zip(checked) {
            let changed = self.get_param(&node, key)? != stored;
            let state = self.sampler_state_mut(&node);
            state.insert((*key).to_string(), stored.clone());
            if changed {
                if let Some(id) = node_key(&node) {
                    let edit = ParamEdit::new(id, index as u16, to_f32(&stored));
                    edits.push(json!({
                        "node": edit.node.to_string(),
                        "param": edit.param,
                        "value": edit.value,
                    }));
                }
            }
        }
        // The tuning in cents and the frequency the engine plays follow from the parts.
        let state = self.sampler_state_mut(&node);
        super::derive_fields("sampler", state);
        let state = Value::Object(state.clone());
        // The asset keeps the root note too, as the DAW writes it.
        if let Some(root) = state.get("rootNote").cloned() {
            let id = asset_id(&node);
            if self.asset(&id).is_some_and(|a| a["rootNote"] != root) {
                let mut entry = self.asset(&id).cloned().unwrap_or(Value::Null);
                entry["rootNote"] = root;
                self.put_asset(&id, Some(entry));
            }
        }
        Ok(json!({"state": state, "edits": edits}))
    }

    // ----- queries ----------------------------------------------------------------------

    pub(super) fn q_sample_info(&self, question: &Value) -> Res<Value> {
        let node = str_field(question, "node")?;
        self.expect_node(node, "sampler")?;
        let id = asset_id(node);
        let loop_names = ["off", "loop", "pingpong"];
        let trigger_names = ["free", "gate", "oneshot"];
        let name_of = |key: &str, names: [&'static str; 3]| -> &'static str {
            names[(self.sampler_f64(node, key) as usize).min(2)]
        };
        let loop_start = self.sampler_f64(node, "loopStart");
        let loop_end = self.sampler_f64(node, "loopEnd");
        let mut info = json!({
            "node": node,
            "loaded": false,
            "asset": id,
            "root_note": self.sampler_f64(node, "rootNote") as i64,
            "loop_mode": name_of("loopMode", loop_names),
            "loop_start": number(loop_start),
            "loop_end": number(loop_end),
            "gain": number(self.sampler_f64(node, "gain")),
            "trigger": name_of("triggerMode", trigger_names),
            "octave": self.sampler_f64(node, "detune_oct") as i64,
            "semitones": self.sampler_f64(node, "detune_semi") as i64,
            "cents": number(self.sampler_f64(node, "detune_cents")),
            "active": self.node_state(node).and_then(|s| s["active"].as_bool()).unwrap_or(true),
        });
        if let Some(name) = self
            .node_state(node)
            .and_then(|s| s.get("fileName"))
            .filter(|n| n.is_string())
        {
            info["name"] = name.clone();
        }
        if self.asset(&id).is_some() {
            let pcm = self.pcm_of(&id)?;
            let frames = pcm.frames();
            info["loaded"] = json!(true);
            info["frames"] = json!(frames);
            info["rate"] = json!(pcm.rate);
            info["channels"] = json!(pcm.channels);
            info["duration"] = number(pcm.seconds());
            info["peak"] = number(pcm.peak() as f64);
            info["loop_start_frame"] = json!((loop_start * frames as f64).round() as u64);
            info["loop_end_frame"] = json!((loop_end * frames as f64).round() as u64);
            info["bytes"] = json!(self
                .asset(&id)
                .and_then(|a| a["base64Data"].as_str())
                .map_or(0, base64_decoded_len));
        }
        Ok(info)
    }

    pub(super) fn q_sample_peaks(&self, question: &Value) -> Res<Value> {
        let node = str_field(question, "node")?;
        self.expect_node(node, "sampler")?;
        let id = asset_id(node);
        if self.asset(&id).is_none() {
            return Err(format!("'{node}' has no sample: load_sample first"));
        }
        let pcm = self.pcm_of(&id)?;
        let frames = pcm.frames();
        let buckets = match opt_number(question, "buckets")? {
            Some(n) if n >= 1.0 => (n as usize).min(MAX_BUCKETS),
            Some(_) => return Err("'buckets' is 1 or more".into()),
            None => 256,
        };
        let from = opt_number(question, "from")?.unwrap_or(0.0).max(0.0) as usize;
        let to = opt_number(question, "to")?.map_or(frames, |t| t.max(0.0) as usize);
        let mut result = peaks_json(&pcm, from, to, buckets);
        result["frames"] = json!(frames);
        Ok(result)
    }

    pub(super) fn q_sample_wav(&self, question: &Value) -> Res<Value> {
        let node = str_field(question, "node")?;
        self.expect_node(node, "sampler")?;
        let id = asset_id(node);
        let asset = self
            .asset(&id)
            .ok_or_else(|| format!("'{node}' has no sample: load_sample first"))?;
        let data = asset["base64Data"].as_str().unwrap_or_default();
        let pcm = self.pcm_of(&id)?;
        Ok(json!({
            "data": data,
            "name": asset.get("fileName").cloned().unwrap_or(json!("sample.wav")),
            "rate": pcm.rate,
            "channels": pcm.channels,
            "frames": pcm.frames(),
            "bytes": base64_decoded_len(data),
        }))
    }
}

/// `{from, to, buckets, min: [..], max: [..]}` for a waveform view.
fn peaks_json(pcm: &Pcm, from: usize, to: usize, buckets: usize) -> Value {
    let frames = pcm.frames();
    let to = to.min(frames);
    let from = from.min(to);
    let (low, high) = pcm.peaks(from, to, buckets);
    json!({
        "from": from,
        "to": to,
        "buckets": low.len(),
        "min": low.iter().map(|x| number(*x as f64)).collect::<Vec<_>>(),
        "max": high.iter().map(|x| number(*x as f64)).collect::<Vec<_>>(),
    })
}
