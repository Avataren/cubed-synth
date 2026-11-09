//! A wavetable oscillator's table, in the patch.
//!
//! The DAW keeps the table as the asset `wavetable_<node id>`: `base64Data` is the base64 text of a
//! mono 32-bit float WAV (44.1 kHz, a 44-byte header) of `frames * 2048` samples, one cycle of
//! 2,048 samples after another, at most 256 frames (`import_wavetable(node, bytes, 2048)` cuts it
//! so). The oscillator's `wave_index` (0 to 1, a node parameter, `set_param` takes it live) morphs
//! along the frames. A node without the asset plays the engine's built-in table: the four
//! waveforms sine, triangle, saw and square as four frames.
//!
//! The frames are the whole truth: nothing else is kept (the DAW's editor does not keep its
//! keyframes either). A frame is edited as samples (`set_frame`), as harmonics
//! (`wavetable_from_harmonics`, and `wavetable_harmonics` reads a frame's harmonics back), or the
//! whole table is made from keyframes or a preset the way the DAW's editor does
//! ([`wavetable_edit`](crate::wavetable_edit)). Every operation rewrites the asset (about 2.8 MB
//! of base64 at 256 frames, a few milliseconds) and a playing synth takes the new table by
//! being built again: the frames' mip levels are cached, so a rebuild after one frame changed
//! builds that frame only.
//!
//! Operations (`apply`; `"node"` is a wavetable oscillator's id in all of them; the answer is
//! `{asset, frames, size, bytes, peak}` unless it says otherwise; frames count from 0):
//!
//! - `set_wavetable`: the table from `"frames"` (a list of lists of numbers: floats, or 16-bit
//!   integers with `"format": "i16"`) or `"wav"` (base64 of a WAV file, cut into frames of `"size"`
//!   samples, 2048 unless said). `"size"` is the length of the frames given (4 or more, the same for
//!   all; they are stored at 2048, resampled band limited if they differ). Options `"normalize"`,
//!   `"remove_dc"` (per frame, as the DAW's `processWaveform` does), `"name"`. More than 256
//!   frames are cut (`"truncated": true`).
//! - `wavetable_preset`: `"preset"` is one of `wavetable_presets` (the DAW's twelve, in any
//!   spelling: `analogPWM`, `analog_pwm`, `saw`, ..., and `pulse`, `organ`, and `default`, the
//!   built-in table's four waveforms). `"harmonics"` (1 to 1024, default 32), `"frames"` (1 to 256,
//!   default 256 for a preset that moves, 1 for one that does not), `"remove_dc"` (false),
//!   `"normalize"` (true): the DAW's Apply.
//! - `wavetable_from_keyframes`: `"keyframes"` (`[{time, harmonics: [{amplitude, phase}|amplitude,
//!   ...]}]`, `time` 0 to 100), `"warps"` (`[{time, x_amount, y_amount, asymmetric}]`, amounts -4 to
//!   50), `"frames"` (default 256), `"remove_dc"`, `"normalize"`: `generateWavetableWav`.
//! - `wavetable_from_harmonics`: `"harmonics"` (a list of amplitudes or of `{amplitude, phase}`,
//!   with `"phases"` beside a list of amplitudes) for `"frame"` (default 0, or `"all"`);
//!   `"normalize"` (true), `"remove_dc"` (false). With no table yet one is made of `"frames"`
//!   (default 1) copies of it.
//! - `set_frame` (`"frame"`, `"values"`, `"size"`, `"format"`, `"normalize"`, `"remove_dc"`),
//!   `insert_frame` (`"at"` 0 to the frame count, `"source"` the frame to copy, default the one
//!   before), `copy_frame` (`"from"`, `"to"`), `delete_frame` (`"frame"`; the last one stays: use
//!   `clear_wavetable`), `interpolate_frames` (`"from"`, `"to"`: the frames between are blends of
//!   the two), `wavetable_warp` (`"frame"` or all, `"x_amount"`, `"y_amount"`, `"asymmetric"`),
//!   `normalize_wavetable` (`"frame"` or all, `"normalize"` true, `"remove_dc"` false).
//! - `clear_wavetable`: back to the built-in table. Answers `{cleared}`.
//! - `set_frame_position` (`"frame"`): sets `wave_index` so that the oscillator plays that frame;
//!   answers as `set_param` does (`{value, edit}`, a live edit).
//!
//! Queries (`PatchDoc::query`): `wavetable_info`, `wavetable` (the samples: `"from"`, `"count"`,
//! `"size"` points a frame), `wavetable_preview` (`"frame"` or `"position"`, `"points"`),
//! `wavetable_harmonics` (`"frame"`, `"count"`), `wavetable_presets`.

use std::sync::Arc;

use serde_json::{json, Value};

use super::assets::asset_entry;
use super::{number, str_field, usize_field, PatchDoc, Res};
use crate::wav::{base64_decode, base64_decoded_len, base64_encode};
use crate::wavetable_edit::{
    apply_wave_warp, builtin_table, generate_from_harmonics, generate_table, harmonics_of, preset,
    preset_name, process_waveform, resample_cycle, Harmonic, Keyframe, Table, Warp, WarpKeyframe,
    FILE_RATE, FRAME_SIZE, MAX_FRAMES, PRESETS,
};

const MAX_HARMONICS: usize = 1024;
const MAX_POINTS: usize = 4096;
/// The DAW's warp sliders.
const WARP_RANGE: std::ops::RangeInclusive<f64> = -4.0..=50.0;

/// The asset id of an oscillator's table.
pub fn asset_id(node: &str) -> String {
    format!("wavetable_{node}")
}

/// What `wavetable_presets` answers.
pub(super) fn presets_json() -> Value {
    let mut list: Vec<Value> = PRESETS
        .iter()
        .map(|(name, label, animated)| {
            json!({
                "name": name,
                "label": label,
                "frames": if *animated { MAX_FRAMES } else { 1 },
                "animated": animated,
            })
        })
        .collect();
    list.push(json!({
        "name": "default",
        "label": "Built-in waveforms",
        "frames": 4,
        "animated": true,
    }));
    json!({ "presets": list, "frame_size": FRAME_SIZE, "max_frames": MAX_FRAMES })
}

fn bool_or(op: &Value, key: &str, default: bool) -> Res<bool> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("'{key}' must be true or false")),
    }
}

fn number_or(op: &Value, key: &str, default: f64) -> Res<f64> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or_else(|| format!("'{key}' must be a number")),
    }
}

fn count_in(op: &Value, key: &str, default: usize, low: usize, high: usize) -> Res<usize> {
    let n = match op.get(key) {
        None | Some(Value::Null) => return Ok(default),
        Some(v) => v
            .as_u64()
            .ok_or_else(|| format!("'{key}' must be a whole number"))? as usize,
    };
    if n < low || n > high {
        return Err(format!("'{key}' is {low} to {high}, not {n}"));
    }
    Ok(n)
}

/// `"frame"`: an index below `frames`.
fn frame_index(op: &Value, key: &str, frames: usize) -> Res<usize> {
    let n = usize_field(op, key)?;
    if n >= frames {
        return Err(format!(
            "there is no frame {n}: the table has {frames} (0 to {})",
            frames - 1
        ));
    }
    Ok(n)
}

/// Numbers (floats, or 16-bit integers) as a frame.
fn read_values(list: &Value, what: &str, i16_scale: bool) -> Res<Vec<f32>> {
    let list = list
        .as_array()
        .ok_or_else(|| format!("{what} must be a list of numbers"))?;
    list.iter()
        .map(|v| {
            v.as_f64()
                .filter(|n| n.is_finite())
                .map(|n| {
                    if i16_scale {
                        (n / 32768.0) as f32
                    } else {
                        n as f32
                    }
                })
                .ok_or_else(|| format!("{what} holds something that is not a number"))
        })
        .collect()
}

fn read_format(op: &Value) -> Res<bool> {
    match op.get("format").and_then(Value::as_str) {
        None | Some("float") | Some("f32") => Ok(false),
        Some("i16") => Ok(true),
        Some(other) => Err(format!("'format' is float or i16, not '{other}'")),
    }
}

/// A frame of any length (4 or more) as one of [`FRAME_SIZE`].
fn to_frame_size(frame: Vec<f32>) -> Res<Vec<f32>> {
    if frame.len() < 4 {
        return Err(format!(
            "a frame has at least 4 samples, not {}",
            frame.len()
        ));
    }
    Ok(resample_cycle(&frame, FRAME_SIZE))
}

fn read_harmonics(op: &Value) -> Res<Vec<Harmonic>> {
    let list = op
        .get("harmonics")
        .and_then(Value::as_array)
        .ok_or("'harmonics' must be a list (amplitudes, or {amplitude, phase})")?;
    harmonics_from(list, op.get("phases"))
}

fn harmonics_from(list: &[Value], phases: Option<&Value>) -> Res<Vec<Harmonic>> {
    if list.is_empty() || list.len() > MAX_HARMONICS {
        return Err(format!(
            "there are 1 to {MAX_HARMONICS} harmonics, not {}",
            list.len()
        ));
    }
    let phases: Option<Vec<f64>> = match phases {
        None | Some(Value::Null) => None,
        Some(p) => Some(
            read_values(p, "'phases'", false)?
                .into_iter()
                .map(f64::from)
                .collect(),
        ),
    };
    list.iter()
        .enumerate()
        .map(|(i, v)| {
            let (amplitude, phase) = match v {
                Value::Number(n) => (
                    n.as_f64().unwrap_or(0.0),
                    phases
                        .as_ref()
                        .and_then(|p| p.get(i))
                        .copied()
                        .unwrap_or(0.0),
                ),
                Value::Object(o) => (
                    o.get("amplitude").and_then(Value::as_f64).unwrap_or(0.0),
                    o.get("phase").and_then(Value::as_f64).unwrap_or(0.0),
                ),
                _ => {
                    return Err(format!(
                        "harmonic {} is a number or {{amplitude, phase}}",
                        i + 1
                    ))
                }
            };
            if !amplitude.is_finite() || !phase.is_finite() {
                return Err(format!("harmonic {} is not finite", i + 1));
            }
            Ok(Harmonic { amplitude, phase })
        })
        .collect()
}

fn read_warp(v: &Value, i: usize) -> Res<Warp> {
    let get = |key: &str| -> Res<f64> {
        let n = v.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        if !WARP_RANGE.contains(&n) {
            return Err(format!("warp {i}: '{key}' is -4 to 50, not {n}"));
        }
        Ok(n)
    };
    Ok(Warp {
        x_amount: get("x_amount")?,
        y_amount: get("y_amount")?,
        asymmetric: v
            .get("asymmetric")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn process_all(table: &mut Table, remove_dc: bool, normalize: bool) {
    if !remove_dc && !normalize {
        return;
    }
    for f in 0..table.frames() {
        let out = process_waveform(table.frame(f), remove_dc, normalize);
        table.frame_mut(f).copy_from_slice(&out);
    }
}

/// A frame as points for drawing: `points` samples evenly over the cycle, linearly interpolated.
fn subsample(frame: &[f32], points: usize) -> Vec<f32> {
    let n = frame.len();
    if points == n {
        return frame.to_vec();
    }
    (0..points)
        .map(|i| {
            let at = i as f64 * n as f64 / points as f64;
            let low = at.floor() as usize % n;
            let high = (low + 1) % n;
            let t = (at - at.floor()) as f32;
            frame[low] + t * (frame[high] - frame[low])
        })
        .collect()
}

fn numbers(values: &[f32]) -> Value {
    Value::Array(values.iter().map(|x| number(*x as f64)).collect())
}

impl PatchDoc {
    /// The node's table: its asset's, or the built-in one (the second value says which).
    fn table_or_builtin(&self, node: &str) -> Res<(Table, bool)> {
        let id = asset_id(node);
        if self.asset(&id).is_some() {
            Ok(((*self.table_of(&id)?).clone(), false))
        } else {
            Ok((builtin_table(), true))
        }
    }

    /// Writes `table` as the node's asset and answers with what a caller wants to know of it.
    fn store_table(&mut self, node: &str, table: Table, name: Option<&str>) -> Res<Value> {
        debug_assert_eq!(table.size, FRAME_SIZE);
        if table.frames() == 0 || table.frames() > MAX_FRAMES {
            return Err(format!("a table has 1 to {MAX_FRAMES} frames"));
        }
        let id = asset_id(node);
        let wav = table.to_wav();
        let seconds = table.data.len() as f64 / FILE_RATE as f64;
        let mut entry = asset_entry(&id, "wavetable", base64_encode(&wav), FILE_RATE, 1, seconds);
        let name = name.map(str::to_string).or_else(|| {
            self.asset(&id)
                .and_then(|a| a["fileName"].as_str().map(str::to_string))
        });
        if let Some(name) = &name {
            entry["fileName"] = json!(name);
        }
        self.put_asset(&id, Some(entry));
        let result = json!({
            "asset": id,
            "frames": table.frames(),
            "size": table.size,
            "bytes": wav.len(),
            "peak": number(table.peak() as f64),
        });
        self.keep_table(&id, Arc::new(table));
        Ok(result)
    }

    fn name_of(op: &Value) -> Res<Option<String>> {
        match op.get("name") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err("'name' must be text".into()),
        }
    }

    // ----- making a whole table ----------------------------------------------------------

    pub(super) fn op_set_wavetable(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let i16_scale = read_format(op)?;
        let size = match op.get("size") {
            None | Some(Value::Null) => None,
            Some(_) => Some(count_in(op, "size", FRAME_SIZE, 4, 1 << 20)?),
        };
        let (mut table, truncated) = if let Some(frames) = op.get("frames").filter(|v| !v.is_null())
        {
            let list = frames
                .as_array()
                .ok_or("'frames' must be a list of frames")?;
            if list.is_empty() {
                return Err("'frames' is empty".into());
            }
            let mut frames_in = Vec::with_capacity(list.len());
            for (i, frame) in list.iter().enumerate() {
                frames_in.push(read_values(frame, &format!("frame {i}"), i16_scale)?);
            }
            let given = size.unwrap_or(frames_in[0].len());
            if let Some(bad) = frames_in.iter().position(|f| f.len() != given) {
                return Err(format!(
                    "frame {bad} has {} samples, the frames are {given} long",
                    frames_in[bad].len()
                ));
            }
            let truncated = frames_in.len() > MAX_FRAMES;
            frames_in.truncate(MAX_FRAMES);
            let mut table = Table::new(FRAME_SIZE);
            for frame in frames_in {
                table.push(&to_frame_size(frame)?);
            }
            (table, truncated)
        } else if let Some(wav) = op.get("wav").and_then(Value::as_str) {
            let bytes = base64_decode(wav)?;
            let given = size.unwrap_or(FRAME_SIZE);
            let (cut, truncated) = Table::from_wav(&bytes, given)?;
            let mut table = Table::new(FRAME_SIZE);
            for f in 0..cut.frames() {
                table.push(&to_frame_size(cut.frame(f).to_vec())?);
            }
            (table, truncated)
        } else {
            return Err("set_wavetable needs 'frames' or 'wav'".into());
        };
        process_all(
            &mut table,
            bool_or(op, "remove_dc", false)?,
            bool_or(op, "normalize", false)?,
        );
        let name = Self::name_of(op)?;
        let mut result = self.store_table(&node, table, name.as_deref())?;
        result["truncated"] = json!(truncated);
        Ok(result)
    }

    pub(super) fn op_wavetable_preset(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let name = str_field(op, "preset")?;
        let remove_dc = bool_or(op, "remove_dc", false)?;
        let normalize = bool_or(op, "normalize", true)?;
        let table = if matches!(name.to_ascii_lowercase().as_str(), "default" | "builtin") {
            let mut table = builtin_table();
            process_all(
                &mut table,
                remove_dc,
                op.get("normalize").is_some() && normalize,
            );
            table
        } else {
            let canonical = preset_name(name).ok_or_else(|| {
                let names: Vec<&str> = PRESETS.iter().map(|(n, _, _)| *n).collect();
                format!(
                    "no preset '{name}' (there are: {}, default)",
                    names.join(", ")
                )
            })?;
            let animated = PRESETS.iter().any(|(n, _, a)| *n == canonical && *a);
            let harmonics = count_in(op, "harmonics", 32, 1, MAX_HARMONICS)?;
            let frames = count_in(
                op,
                "frames",
                if animated { MAX_FRAMES } else { 1 },
                1,
                MAX_FRAMES,
            )?;
            let preset = preset(canonical, harmonics).expect("a named preset");
            generate_table(
                &preset.keyframes,
                &preset.warps,
                frames,
                FRAME_SIZE,
                remove_dc,
                normalize,
            )?
        };
        let name = Self::name_of(op)?.unwrap_or_else(|| format!("{name}.wav"));
        self.store_table(&node, table, Some(&name))
    }

    pub(super) fn op_wavetable_from_keyframes(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let list = op
            .get("keyframes")
            .and_then(Value::as_array)
            .filter(|l| !l.is_empty())
            .ok_or("'keyframes' must be a list: [{time, harmonics}]")?;
        let mut keyframes = Vec::with_capacity(list.len());
        for (i, k) in list.iter().enumerate() {
            let time = k.get("time").and_then(Value::as_f64).unwrap_or(0.0);
            if !(0.0..=100.0).contains(&time) {
                return Err(format!("keyframe {i}: 'time' is 0 to 100, not {time}"));
            }
            let harmonics = k
                .get("harmonics")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("keyframe {i} needs 'harmonics'"))?;
            keyframes.push(Keyframe {
                time,
                harmonics: harmonics_from(harmonics, k.get("phases"))
                    .map_err(|e| format!("keyframe {i}: {e}"))?,
            });
        }
        keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        let warps = match op.get("warps").filter(|w| !w.is_null()) {
            None => vec![WarpKeyframe {
                time: 0.0,
                params: Warp::default(),
            }],
            Some(w) => {
                let list = w
                    .as_array()
                    .filter(|l| !l.is_empty())
                    .ok_or("'warps' must be a list: [{time, x_amount, y_amount, asymmetric}]")?;
                let mut warps = Vec::with_capacity(list.len());
                for (i, v) in list.iter().enumerate() {
                    let time = v.get("time").and_then(Value::as_f64).unwrap_or(0.0);
                    if !(0.0..=100.0).contains(&time) {
                        return Err(format!("warp {i}: 'time' is 0 to 100, not {time}"));
                    }
                    warps.push(WarpKeyframe {
                        time,
                        params: read_warp(v, i)?,
                    });
                }
                warps.sort_by(|a, b| a.time.total_cmp(&b.time));
                warps
            }
        };
        let frames = count_in(op, "frames", MAX_FRAMES, 1, MAX_FRAMES)?;
        let table = generate_table(
            &keyframes,
            &warps,
            frames,
            FRAME_SIZE,
            bool_or(op, "remove_dc", false)?,
            bool_or(op, "normalize", true)?,
        )?;
        let name = Self::name_of(op)?;
        self.store_table(&node, table, name.as_deref())
    }

    pub(super) fn op_wavetable_from_harmonics(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let harmonics = read_harmonics(op)?;
        let remove_dc = bool_or(op, "remove_dc", false)?;
        let normalize = bool_or(op, "normalize", true)?;
        let wave: Vec<f32> = generate_from_harmonics(&harmonics, FRAME_SIZE)
            .into_iter()
            .map(|x| x as f32)
            .collect();
        let frame = process_waveform(&wave, remove_dc, normalize);
        let exists = self.asset(&asset_id(&node)).is_some();
        let table = if exists {
            let (mut table, _) = self.table_or_builtin(&node)?;
            match op.get("frame") {
                Some(Value::String(s)) if s == "all" => {
                    for f in 0..table.frames() {
                        table.frame_mut(f).copy_from_slice(&frame);
                    }
                }
                _ => {
                    let at = match op.get("frame") {
                        None | Some(Value::Null) => 0,
                        Some(_) => frame_index(op, "frame", table.frames())?,
                    };
                    table.frame_mut(at).copy_from_slice(&frame);
                }
            }
            table
        } else {
            let frames = count_in(op, "frames", 1, 1, MAX_FRAMES)?;
            let mut table = Table::new(FRAME_SIZE);
            for _ in 0..frames {
                table.push(&frame);
            }
            table
        };
        self.store_table(&node, table, None)
    }

    // ----- editing frames ------------------------------------------------------------------

    pub(super) fn op_set_frame(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let (mut table, _) = self.table_or_builtin(&node)?;
        let at = frame_index(op, "frame", table.frames())?;
        let values = read_values(
            op.get("values").ok_or("set_frame needs 'values'")?,
            "'values'",
            read_format(op)?,
        )?;
        if let Some(size) = op.get("size").filter(|s| !s.is_null()) {
            if size.as_u64() != Some(values.len() as u64) {
                return Err(format!(
                    "'size' is {size} but 'values' has {} numbers",
                    values.len()
                ));
            }
        }
        let frame = process_waveform(
            &to_frame_size(values)?,
            bool_or(op, "remove_dc", false)?,
            bool_or(op, "normalize", false)?,
        );
        table.frame_mut(at).copy_from_slice(&frame);
        let mut result = self.store_table(&node, table, None)?;
        result["frame"] = json!(at);
        Ok(result)
    }

    pub(super) fn op_insert_frame(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let (mut table, _) = self.table_or_builtin(&node)?;
        let frames = table.frames();
        let at = usize_field(op, "at")?;
        if at > frames {
            return Err(format!("'at' is 0 to {frames} (the frame count), not {at}"));
        }
        if frames >= MAX_FRAMES {
            return Err(format!("a table has at most {MAX_FRAMES} frames"));
        }
        let source = match op.get("source") {
            None | Some(Value::Null) => at.saturating_sub(1).min(frames - 1),
            Some(_) => frame_index(op, "source", frames)?,
        };
        let copy = table.frame(source).to_vec();
        table.insert(at, &copy);
        let mut result = self.store_table(&node, table, None)?;
        result["frame"] = json!(at);
        Ok(result)
    }

    pub(super) fn op_copy_frame(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let (mut table, _) = self.table_or_builtin(&node)?;
        let from = frame_index(op, "from", table.frames())?;
        let to = frame_index(op, "to", table.frames())?;
        let copy = table.frame(from).to_vec();
        table.frame_mut(to).copy_from_slice(&copy);
        self.store_table(&node, table, None)
    }

    pub(super) fn op_delete_frame(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let (mut table, _) = self.table_or_builtin(&node)?;
        let at = frame_index(op, "frame", table.frames())?;
        if table.frames() == 1 {
            return Err("a table keeps at least one frame: use clear_wavetable to go back to the built-in table".into());
        }
        table.remove(at);
        self.store_table(&node, table, None)
    }

    pub(super) fn op_interpolate_frames(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let (mut table, _) = self.table_or_builtin(&node)?;
        let from = frame_index(op, "from", table.frames())?;
        let to = frame_index(op, "to", table.frames())?;
        if to <= from {
            return Err(format!("'from' ({from}) must be before 'to' ({to})"));
        }
        let (a, b) = (table.frame(from).to_vec(), table.frame(to).to_vec());
        for f in from + 1..to {
            let mix = (f - from) as f32 / (to - from) as f32;
            for (i, out) in table.frame_mut(f).iter_mut().enumerate() {
                *out = a[i] + mix * (b[i] - a[i]);
            }
        }
        self.store_table(&node, table, None)
    }

    /// The frames an operation covers: the one in `"frame"`, or all of them.
    fn frames_of(op: &Value, frames: usize) -> Res<std::ops::Range<usize>> {
        match op.get("frame") {
            None | Some(Value::Null) => Ok(0..frames),
            Some(Value::String(s)) if s == "all" => Ok(0..frames),
            Some(_) => {
                let at = frame_index(op, "frame", frames)?;
                Ok(at..at + 1)
            }
        }
    }

    pub(super) fn op_wavetable_warp(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let (mut table, _) = self.table_or_builtin(&node)?;
        let range = Self::frames_of(op, table.frames())?;
        let warp = read_warp(op, 0)?;
        for f in range {
            let wave: Vec<f64> = table.frame(f).iter().map(|&x| x as f64).collect();
            let warped = apply_wave_warp(&wave, warp);
            for (out, x) in table.frame_mut(f).iter_mut().zip(warped) {
                *out = x as f32;
            }
        }
        self.store_table(&node, table, None)
    }

    pub(super) fn op_normalize_wavetable(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let (mut table, _) = self.table_or_builtin(&node)?;
        let range = Self::frames_of(op, table.frames())?;
        let remove_dc = bool_or(op, "remove_dc", false)?;
        let normalize = bool_or(op, "normalize", true)?;
        for f in range {
            let out = process_waveform(table.frame(f), remove_dc, normalize);
            table.frame_mut(f).copy_from_slice(&out);
        }
        self.store_table(&node, table, None)
    }

    pub(super) fn op_clear_wavetable(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let id = asset_id(&node);
        if self.asset(&id).is_none() {
            return Ok(json!({"cleared": false}));
        }
        self.put_asset(&id, None);
        Ok(json!({"cleared": true}))
    }

    pub(super) fn op_set_frame_position(&mut self, op: &Value) -> Res<Value> {
        let node = str_field(op, "node")?.to_string();
        self.expect_node(&node, "wavetable_oscillator")?;
        let frames = self.frame_count(&node)?;
        let at = frame_index(op, "frame", frames)?;
        let position = if frames > 1 {
            at as f64 / (frames - 1) as f64
        } else {
            0.0
        };
        let set = self.set_param(&node, "wave_index", &json!(position))?;
        Ok(super::set_param_json(&set))
    }

    fn frame_count(&self, node: &str) -> Res<usize> {
        let id = asset_id(node);
        if self.asset(&id).is_some() {
            Ok(self.table_of(&id)?.frames())
        } else {
            Ok(4)
        }
    }

    // ----- queries ----------------------------------------------------------------------

    pub(super) fn q_wavetable_info(&self, question: &Value) -> Res<Value> {
        let node = str_field(question, "node")?;
        self.expect_node(node, "wavetable_oscillator")?;
        let id = asset_id(node);
        let loaded = self.asset(&id).is_some();
        let (table, _) = self.table_or_builtin(node)?;
        let mut info = json!({
            "node": node,
            "loaded": loaded,
            "builtin": !loaded,
            "asset": id,
            "frames": table.frames(),
            "size": table.size,
            "peak": number(table.peak() as f64),
            "position": self.get_param(node, "wave_index")?,
        });
        if let Some(asset) = self.asset(&id) {
            info["bytes"] = json!(asset["base64Data"].as_str().map_or(0, base64_decoded_len));
            if let Some(name) = asset.get("fileName") {
                info["name"] = name.clone();
            }
        }
        Ok(info)
    }

    pub(super) fn q_wavetable(&self, question: &Value) -> Res<Value> {
        let node = str_field(question, "node")?;
        self.expect_node(node, "wavetable_oscillator")?;
        let (table, builtin) = self.table_or_builtin(node)?;
        let frames = table.frames();
        let from = match question.get("from") {
            None | Some(Value::Null) => 0,
            Some(_) => frame_index(question, "from", frames)?,
        };
        let count = count_in(question, "count", frames - from, 1, frames - from)?;
        let size = count_in(question, "size", FRAME_SIZE, 2, FRAME_SIZE)?;
        let data: Vec<Value> = (from..from + count)
            .map(|f| numbers(&subsample(table.frame(f), size)))
            .collect();
        Ok(json!({
            "node": node,
            "builtin": builtin,
            "frames": frames,
            "size": size,
            "from": from,
            "data": data,
        }))
    }

    pub(super) fn q_wavetable_preview(&self, question: &Value) -> Res<Value> {
        let node = str_field(question, "node")?;
        self.expect_node(node, "wavetable_oscillator")?;
        let (table, _) = self.table_or_builtin(node)?;
        let points = count_in(question, "points", 256, 2, MAX_POINTS)?;
        let frame = match (question.get("frame"), question.get("position")) {
            (Some(f), _) if !f.is_null() => table
                .frame(frame_index(question, "frame", table.frames())?)
                .to_vec(),
            (_, Some(p)) if !p.is_null() => {
                table.at_position(number_or(question, "position", 0.0)?)
            }
            _ => table.at_position(self.get_param(node, "wave_index")?.as_f64().unwrap_or(0.0)),
        };
        let wave = subsample(&frame, points);
        let peak = wave.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        Ok(json!({
            "points": points,
            "data": numbers(&wave),
            "peak": number(peak as f64),
        }))
    }

    pub(super) fn q_wavetable_harmonics(&self, question: &Value) -> Res<Value> {
        let node = str_field(question, "node")?;
        self.expect_node(node, "wavetable_oscillator")?;
        let (table, _) = self.table_or_builtin(node)?;
        let at = match question.get("frame") {
            None | Some(Value::Null) => 0,
            Some(_) => frame_index(question, "frame", table.frames())?,
        };
        let count = count_in(question, "count", 32, 1, MAX_HARMONICS)?;
        let harmonics = harmonics_of(table.frame(at), count);
        Ok(json!({
            "frame": at,
            "amplitudes": harmonics.iter().map(|h| number(h.amplitude)).collect::<Vec<_>>(),
            "phases": harmonics.iter().map(|h| number(h.phase)).collect::<Vec<_>>(),
        }))
    }
}
