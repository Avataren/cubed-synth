//! Undo and redo for a [`PatchDoc`], off until `{"op": "set_history", "steps": n}`.
//!
//! A patch can carry megabytes (a sample, a wavetable of 256 frames is 2.8 MB of base64), so an
//! editor that undoes by keeping copies of the whole JSON pays that for every step. This keeps
//! each step as what the operation *replaced*:
//!
//! - the document without its assets and its `editor` key, as it was before (a few tens of
//!   kilobytes: node states and connections; the `editor` key, positions and view, belongs to the
//!   editor and is not undone);
//! - the asset entries the operation put in, took out or replaced, *moved* out of the document
//!   into the step (nothing is copied).
//!
//! Undoing swaps the step with the document, which turns it into the redo step; redoing swaps
//! again. So an asset is held once, by whichever side of the swap it is on.
//!
//! The log is bounded twice: at most `steps` steps (the oldest go first), and [`MAX_STASH_BYTES`]
//! of assets held in steps. An operation with a `"group"` equal to the one before it (a slider
//! drag, a harmonic being pulled) joins that step: the first one's `before` is kept, and the
//! assets the others replaced are dropped at once, so a drag over a wavetable holds one old table,
//! not one per mouse move.

use std::collections::VecDeque;

use serde_json::{json, Map, Value};

use super::{PatchDoc, Res};

/// The most steps `set_history` may ask for.
pub const MAX_STEPS: usize = 1000;
/// The most bytes of replaced assets the steps hold together.
pub const MAX_STASH_BYTES: usize = 128 << 20;

/// What an editor undoes: the document apart from its assets and its `editor`.
type Light = Map<String, Value>;

#[derive(Debug)]
struct Step {
    op: String,
    group: Option<String>,
    light: Light,
    /// (id, the entry the document had before, or none if it had none).
    assets: Vec<(String, Option<Value>)>,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct History {
    limit: usize,
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    /// Assets the operation in progress replaced.
    pending: Option<Vec<(String, Option<Value>)>>,
}

impl std::fmt::Debug for History {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "History {{ limit: {}, undo: {}, redo: {} }}",
            self.limit,
            self.undo.len(),
            self.redo.len()
        )
    }
}

fn bytes_of(assets: &[(String, Option<Value>)]) -> usize {
    assets
        .iter()
        .filter_map(|(_, v)| v.as_ref())
        .filter_map(|v| v["base64Data"].as_str())
        .map(str::len)
        .sum()
}

impl History {
    pub(super) fn enabled(&self) -> bool {
        self.limit > 0
    }

    /// Keeps `old` (the asset a change replaced) for the step being made.
    pub(super) fn stash(&mut self, id: &str, old: Option<Value>) {
        if let Some(pending) = &mut self.pending {
            // The first replacement of an id is the one an undo restores.
            if !pending.iter().any(|(known, _)| known == id) {
                pending.push((id.to_string(), old));
            }
        }
    }

    fn stash_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .map(|s| s.bytes)
            .sum()
    }

    fn trim(&mut self) {
        while self.undo.len() > self.limit
            || (self.undo.len() > 1 && self.stash_bytes() > MAX_STASH_BYTES)
        {
            self.undo.pop_front();
        }
        while self.redo.len() > self.limit
            || (!self.redo.is_empty() && self.stash_bytes() > MAX_STASH_BYTES)
        {
            self.redo.remove(0);
        }
    }
}

impl PatchDoc {
    /// The document as an editor undoes it, moved out (see the module docs): everything but the
    /// assets and the `editor`.
    fn take_light(&mut self) -> Light {
        let Some(root) = self.root.as_object_mut() else {
            return Light::new();
        };
        let keys: Vec<String> = root
            .keys()
            .filter(|k| !matches!(k.as_str(), "audioAssets" | "editor"))
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|k| root.remove(&k).map(|v| (k, v)))
            .collect()
    }

    fn put_light(&mut self, light: Light) {
        if let Some(root) = self.root.as_object_mut() {
            for (k, v) in light {
                root.insert(k, v);
            }
        }
    }

    /// A copy of the undoable part of the document.
    fn light_copy(&self) -> Light {
        self.root
            .as_object()
            .map(|root| {
                root.iter()
                    .filter(|(k, _)| !matches!(k.as_str(), "audioAssets" | "editor"))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn light_is(&self, light: &Light) -> bool {
        let Some(root) = self.root.as_object() else {
            return light.is_empty();
        };
        let mut count = 0;
        for (k, v) in root {
            if matches!(k.as_str(), "audioAssets" | "editor") {
                continue;
            }
            count += 1;
            if light.get(k) != Some(v) {
                return false;
            }
        }
        count == light.len()
    }

    /// Runs `run` as one undoable operation. With the history off it is just `run`.
    pub(super) fn record(
        &mut self,
        name: &str,
        group: Option<&str>,
        run: impl FnOnce(&mut PatchDoc) -> Res<Value>,
    ) -> Res<Value> {
        // The editor's own data is not undone, and not a step.
        if !self.history.enabled() || name == "set_editor" {
            return run(self);
        }
        let joins = group.is_some()
            && self
                .history
                .undo
                .back()
                .is_some_and(|top| top.group.as_deref() == group);
        let before = if joins { None } else { Some(self.light_copy()) };
        self.history.pending = Some(Vec::new());
        let result = run(self);
        let stash = self.history.pending.take().unwrap_or_default();
        match result {
            Err(e) => {
                // An operation that fails leaves the document as it found it: put back
                // what it took.
                for (id, old) in stash {
                    self.restore_asset(&id, old);
                }
                Err(e)
            }
            Ok(value) => {
                match before {
                    Some(before) => {
                        if !stash.is_empty() || !self.light_is(&before) {
                            let bytes = bytes_of(&stash);
                            self.history.undo.push_back(Step {
                                op: name.to_string(),
                                group: group.map(str::to_string),
                                light: before,
                                assets: stash,
                                bytes,
                            });
                            self.history.redo.clear();
                            self.history.trim();
                        }
                    }
                    // Joined: the first step of the group stands, what the others replaced goes.
                    None => self.history.redo.clear(),
                }
                Ok(value)
            }
        }
    }

    fn swap_step(&mut self, mut step: Step) -> Step {
        let current = self.take_light();
        self.put_light(std::mem::replace(&mut step.light, current));
        for (id, slot) in &mut step.assets {
            let now = self
                .root
                .get_mut("audioAssets")
                .and_then(Value::as_object_mut)
                .and_then(|assets| assets.remove(id));
            let put_back = slot.take();
            self.restore_asset(id, put_back);
            *slot = now;
        }
        step.bytes = bytes_of(&step.assets);
        step
    }

    pub(super) fn undo(&mut self) -> Res<Value> {
        let step = self.history.undo.pop_back().ok_or("nothing to undo")?;
        let name = step.op.clone();
        let redo = self.swap_step(step);
        self.history.redo.push(redo);
        self.history.trim();
        Ok(
            json!({"undone": name, "undo": self.history.undo.len(), "redo": self.history.redo.len()}),
        )
    }

    pub(super) fn redo(&mut self) -> Res<Value> {
        let step = self.history.redo.pop().ok_or("nothing to redo")?;
        let name = step.op.clone();
        let undo = self.swap_step(step);
        self.history.undo.push_back(undo);
        self.history.trim();
        Ok(
            json!({"redone": name, "undo": self.history.undo.len(), "redo": self.history.redo.len()}),
        )
    }

    /// `{"op": "set_history", "steps": n}`: keeps the last `n` steps (0 switches the history off
    /// and forgets it).
    pub(super) fn set_history(&mut self, op: &Value) -> Res<Value> {
        let steps = op
            .get("steps")
            .and_then(Value::as_u64)
            .ok_or("'steps' must be a whole number")? as usize;
        if steps > MAX_STEPS {
            return Err(format!("the history keeps at most {MAX_STEPS} steps"));
        }
        self.history.limit = steps;
        if steps == 0 {
            self.history.undo.clear();
            self.history.redo.clear();
        }
        self.history.trim();
        Ok(self.history_json())
    }

    pub(super) fn history_json(&self) -> Value {
        let h = &self.history;
        json!({
            "enabled": h.enabled(),
            "steps": h.limit,
            "undo": h.undo.len(),
            "redo": h.redo.len(),
            "next_undo": h.undo.back().map(|s| s.op.clone()),
            "next_redo": h.redo.last().map(|s| s.op.clone()),
            "asset_bytes": h.stash_bytes(),
        })
    }
}
