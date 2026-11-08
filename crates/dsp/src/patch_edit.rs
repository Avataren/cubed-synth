//! Editing a patch as data. A [`PatchDoc`] holds the DAW's patch JSON (`metadata`,
//! `synthState`, `audioAssets`) and changes it the way the DAW's node editor does,
//! checking every change against the [catalogue](crate::catalog): nodes are added
//! with their default state and the wires the engine makes itself, parameters are
//! range-checked and typed, connections need ports that exist, may not make a
//! loop and are an upsert, system nodes cannot be deleted, a second noise node is
//! refused. Everything the document holds that the editor does not touch stays as
//! it was, so a patch from the DAW goes through unharmed.
//!
//! Two keys are the editor's own and are ignored by the DAW (and the loader
//! before this):
//!
//! - `editor` at the top: anything the editor wants kept (node positions, the
//!   canvas view). [`PatchDoc::set_editor`] replaces it; nothing else reads it.
//! - `synthState.effectChain`: the effects in processing order, as ids
//!   (`"10003"`). A patch without it has the DAW's fixed chain, `10000` to
//!   `10007`. Adding, removing or reordering an effect writes it.
//!
//! A change returns what the audio thread needs to hear of it: a
//! [`ParamEdit`] for a parameter. Structure changes (nodes, connections,
//! effects) are heard by building the engine again from [`PatchDoc::to_json`].
//!
//! [`PatchDoc::apply`] runs the same operations from a JSON description
//! (`{"op": "connect", ...}`), for hosts that talk JSON.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{json, Map, Value};

use crate::catalog::{
    self, effect_spec, node_spec, number, NodeSpec, StateShape, EFFECTS, EFFECT_ID_BASE,
    INSTRUMENT_GAIN, MACRO_COUNT, MACRO_VALUE,
};
use crate::graph::NodeId;
use crate::live::{node_key, ParamEdit};
use crate::traits::PortId;

/// The most voices a patch may ask for.
pub const MAX_VOICES: usize = 16;

/// A node as the document lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeInfo {
    pub id: String,
    pub type_name: String,
    pub name: String,
}

/// A connection as the document lists it. `from_port` is `None` for patches that
/// predate `fromPort`.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectionInfo {
    pub from_id: String,
    pub from_port: Option<PortId>,
    pub to_id: String,
    pub to_port: PortId,
    pub amount: f32,
    pub modulation_type: i32,
    pub transform: i32,
}

impl ConnectionInfo {
    pub fn to_json(&self) -> Value {
        let mut c = json!({
            "fromId": self.from_id,
            "toId": self.to_id,
            "target": self.to_port as u32,
            "amount": number(self.amount as f64),
            "modulationType": self.modulation_type,
            "modulationTransformation": self.transform,
        });
        if let Some(port) = self.from_port {
            c["fromPort"] = json!(port as u32);
        }
        c
    }
}

/// A macro routed to a node's input.
#[derive(Clone, Debug, PartialEq)]
pub struct MacroRoute {
    pub macro_index: usize,
    pub target_id: String,
    pub target_port: PortId,
    pub amount: f32,
    pub modulation_type: i32,
    pub transform: i32,
}

/// The result of setting a parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct SetParam {
    /// The value as the document stores it (clamped, an int rounded).
    pub value: Value,
    /// What to tell a running synth, if the parameter is one it can take live.
    pub edit: Option<ParamEdit>,
}

/// One patch of a bank file.
#[derive(Clone, Debug, PartialEq)]
pub struct BankEntry {
    pub name: String,
    pub category: String,
}

/// A patch document. See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchDoc {
    root: Value,
}

type Res<T> = Result<T, String>;

impl PatchDoc {
    // ----- making and reading ---------------------------------------------

    /// A new playable patch: one oscillator through an amp envelope to the voice
    /// mixer, the eight effects in the default chain, eight voices.
    pub fn new(name: &str) -> PatchDoc {
        let mut doc = PatchDoc {
            root: json!({
                "metadata": {"id": format!("patch_{}", NodeId::new().0.simple()), "name": name, "version": 1},
                "synthState": {
                    "layout": {
                        "voiceCount": 8,
                        "canonicalVoice": {"id": 0, "nodes": {}, "connections": []},
                        "globalNodes": {},
                    },
                    "macros": {"values": [0, 0, 0, 0], "routes": []},
                },
                "audioAssets": {},
            }),
        };
        for system in [
            "global_frequency",
            "glide",
            "global_velocity",
            "gatemixer",
            "mixer",
        ] {
            let spec = node_spec(system).expect("system nodes are in the catalogue");
            let id = doc.new_node_id();
            doc.insert_with_id(spec, &id);
        }
        let glide = doc.first_of("glide").expect("the glide");
        doc.wire_system_inputs(node_spec("glide").expect("the glide"), &glide);
        for effect in EFFECTS {
            let id = (EFFECT_ID_BASE + effect.slot.expect("an effect has a slot")).to_string();
            doc.insert_with_id(effect, &id);
        }
        let osc = doc.add_node("oscillator").expect("an oscillator");
        let env = doc.add_node("envelope").expect("an envelope");
        let mixer = doc.first_of("mixer").expect("the mixer");
        let additive = catalog::ModulationType::Additive.value();
        let vca = catalog::ModulationType::Vca.value();
        doc.connect(
            &osc,
            PortId::AudioOutput0,
            &mixer,
            PortId::AudioInput0,
            1.0,
            additive,
            0,
        )
        .expect("the init patch wires up");
        doc.connect(
            &env,
            PortId::AudioOutput0,
            &mixer,
            PortId::GainMod,
            1.0,
            vca,
            0,
        )
        .expect("the init patch wires up");
        doc
    }

    /// A patch from its JSON text (the DAW's patch format).
    pub fn from_json(text: &str) -> Res<PatchDoc> {
        let root: Value =
            serde_json::from_str(text).map_err(|e| format!("not a patch: invalid JSON: {e}"))?;
        PatchDoc::from_value(root)
    }

    /// A patch from a parsed document. Older layouts are brought up to date: a
    /// patch with `layout.voices` and no `canonicalVoice` gets the first voice as
    /// its canonical one and the count of voices as `voiceCount`.
    pub fn from_value(mut root: Value) -> Res<PatchDoc> {
        if !root.is_object() {
            return Err("not a patch: the document is not an object".into());
        }
        if root.get("synthState").is_none() {
            return Err("not a patch: there is no synthState".into());
        }
        normalize(&mut root)?;
        Ok(PatchDoc { root })
    }

    /// One patch of a bank file (`{metadata, patches: [..]}`).
    pub fn from_bank_json(text: &str, index: usize) -> Res<PatchDoc> {
        let bank: Value =
            serde_json::from_str(text).map_err(|e| format!("not a bank: invalid JSON: {e}"))?;
        let patches = bank
            .get("patches")
            .and_then(Value::as_array)
            .ok_or("not a bank: there is no 'patches' list")?;
        let patch = patches.get(index).ok_or_else(|| {
            format!(
                "the bank has {} patches, there is none at {index}",
                patches.len()
            )
        })?;
        PatchDoc::from_value(patch.clone())
    }

    /// The patches of a bank file, by name and category.
    pub fn bank_patches(text: &str) -> Res<Vec<BankEntry>> {
        let bank: Value =
            serde_json::from_str(text).map_err(|e| format!("not a bank: invalid JSON: {e}"))?;
        let patches = bank
            .get("patches")
            .and_then(Value::as_array)
            .ok_or("not a bank: there is no 'patches' list")?;
        Ok(patches
            .iter()
            .map(|p| BankEntry {
                name: p
                    .pointer("/metadata/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                category: p
                    .pointer("/metadata/category")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            })
            .collect())
    }

    /// The document as JSON text, for [`AudioEngine::init_with_patch`](crate::audio_engine::native::AudioEngine)
    /// and for saving.
    pub fn to_json(&self) -> String {
        self.root.to_string()
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.root).unwrap_or_default()
    }

    pub fn value(&self) -> &Value {
        &self.root
    }

    pub fn into_value(self) -> Value {
        self.root
    }

    pub fn name(&self) -> String {
        self.root
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    pub fn set_name(&mut self, name: &str) {
        if let Some(meta) = self.root.get_mut("metadata").and_then(Value::as_object_mut) {
            meta.insert("name".into(), json!(name));
        } else {
            self.root["metadata"] = json!({"id": format!("patch_{}", NodeId::new().0.simple()), "name": name, "version": 1});
        }
    }

    // ----- the editor's own data --------------------------------------------

    /// Keeps `editor` (node positions, the canvas view: whatever the editor
    /// wants) in the document. It is ignored by everything but the editor, and
    /// comes back from [`editor`](Self::editor), saved and loaded untouched. A
    /// null removes it.
    pub fn set_editor(&mut self, editor: Value) {
        if editor.is_null() {
            if let Some(root) = self.root.as_object_mut() {
                root.remove("editor");
            }
        } else {
            self.root["editor"] = editor;
        }
    }

    pub fn editor(&self) -> Option<&Value> {
        self.root.get("editor")
    }

    // ----- reading the graph --------------------------------------------------

    fn voice(&self) -> &Value {
        &self.root["synthState"]["layout"]["canonicalVoice"]
    }

    fn voice_mut(&mut self) -> &mut Value {
        &mut self.root["synthState"]["layout"]["canonicalVoice"]
    }

    fn synth_mut(&mut self) -> &mut Map<String, Value> {
        self.root["synthState"]
            .as_object_mut()
            .expect("normalized: synthState is an object")
    }

    /// Every node, voice nodes then effects, in the document's order.
    pub fn nodes(&self) -> Vec<NodeInfo> {
        let mut found = Vec::new();
        if let Some(nodes) = self.voice()["nodes"].as_object() {
            for (type_name, list) in nodes {
                for entry in list.as_array().into_iter().flatten() {
                    if let Some(id) = entry["id"].as_str() {
                        found.push(NodeInfo {
                            id: id.to_string(),
                            type_name: type_name.clone(),
                            name: entry["name"].as_str().unwrap_or("").to_string(),
                        });
                    }
                }
            }
        }
        found
    }

    pub fn node(&self, id: &str) -> Option<NodeInfo> {
        self.nodes().into_iter().find(|n| n.id == id)
    }

    /// The type of the node `id` (a voice node, or an effect: by its entry in the
    /// document, else by its place in the default chain).
    pub fn node_type(&self, id: &str) -> Option<String> {
        if let Some(info) = self.node(id) {
            return Some(info.type_name);
        }
        let number: usize = id.parse().ok()?;
        let slot = number.checked_sub(EFFECT_ID_BASE)?;
        EFFECTS.get(slot).map(|e| e.type_name.to_string())
    }

    fn first_of(&self, type_name: &str) -> Option<String> {
        self.nodes()
            .into_iter()
            .find(|n| n.type_name == type_name)
            .map(|n| n.id)
    }

    pub fn connections(&self) -> Vec<ConnectionInfo> {
        self.voice()["connections"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse_connection)
            .collect()
    }

    // ----- nodes ---------------------------------------------------------------

    /// Adds a node of the catalogue's type `type_name` with its default state and
    /// returns its id. A voice node also gets the wires the engine makes for it
    /// (the glide's pitch into an oscillator, the gate mixer's gate into an
    /// envelope); an effect goes to the end of the chain. A type that a patch
    /// has once (noise, the arpeggiator) is refused a second time.
    pub fn add_node(&mut self, type_name: &str) -> Res<String> {
        self.insert_node(type_name, true)
    }

    fn insert_node(&mut self, type_name: &str, wire: bool) -> Res<String> {
        let spec =
            node_spec(type_name).ok_or_else(|| format!("unknown node type '{type_name}'"))?;
        if spec.singleton && self.nodes().iter().any(|n| n.type_name == type_name) {
            return Err(format!("{} exists once in a patch", spec.title));
        }
        let id = if spec.is_effect() {
            self.new_effect_id()
        } else {
            self.new_node_id()
        };
        self.insert_with_id(spec, &id);
        if spec.is_effect() {
            // A new effect goes after the others.
            let mut chain = self.effect_chain();
            if !chain.contains(&id) {
                chain.push(id.clone());
            }
            self.set_chain(chain);
        } else if wire {
            self.wire_system_inputs(spec, &id);
        }
        Ok(id)
    }

    /// The node's entry and its state, nothing else.
    fn insert_with_id(&mut self, spec: &NodeSpec, id: &str) {
        let name = self.new_name(spec);
        {
            let voice = self.voice_mut();
            let list = voice["nodes"]
                .as_object_mut()
                .expect("normalized: nodes is an object")
                .entry(spec.type_name.to_string())
                .or_insert_with(|| json!([]));
            if !list.is_array() {
                *list = json!([]);
            }
            list.as_array_mut().expect("a list").push(json!({
                "id": id,
                "type": spec.type_name,
                "name": name,
            }));
        }
        self.create_state(spec, id, true);
    }

    fn new_node_id(&self) -> String {
        loop {
            let id = NodeId::new().to_string();
            if self.node(&id).is_none() {
                return id;
            }
        }
    }

    /// The lowest effect id (from 10000) that no node or chain entry has.
    fn new_effect_id(&self) -> String {
        let taken: HashSet<usize> = self
            .nodes()
            .iter()
            .filter_map(|n| n.id.parse::<usize>().ok())
            .chain(self.effect_chain().iter().filter_map(|id| id.parse().ok()))
            .collect();
        (EFFECT_ID_BASE..)
            .find(|id| !taken.contains(id))
            .expect("an unused id")
            .to_string()
    }

    fn new_name(&self, spec: &NodeSpec) -> String {
        let names: HashSet<String> = self.nodes().into_iter().map(|n| n.name).collect();
        if !names.contains(spec.title) {
            return spec.title.to_string();
        }
        (2..)
            .map(|n| format!("{} {n}", spec.title))
            .find(|name| !names.contains(name))
            .expect("an unused name")
    }

    /// The node's default state in `synthState`, unless it has one (`reset`: a
    /// node that has one state for itself, noise or velocity, starts afresh).
    fn create_state(&mut self, spec: &NodeSpec, id: &str, reset: bool) {
        let Some(map) = spec.state_map else { return };
        let state = Value::Object(spec.default_state(id));
        let synth = self.synth_mut();
        match spec.state_shape {
            StateShape::Map => {
                let entry = synth.entry(map.to_string()).or_insert_with(|| json!({}));
                if !entry.is_object() {
                    *entry = json!({});
                }
                if let Some(entry) = entry.as_object_mut() {
                    entry.entry(id.to_string()).or_insert(state);
                }
            }
            StateShape::Single => {
                if reset || !synth.get(map).is_some_and(Value::is_object) {
                    synth.insert(map.to_string(), state);
                }
            }
            StateShape::None => {}
        }
    }

    /// Adds the connections the engine makes when it creates a node: the glide's
    /// output into a pitch input, the gate mixer's gate into a gate input (and
    /// the glide's own: the global pitch and the gate).
    fn wire_system_inputs(&mut self, spec: &NodeSpec, id: &str) {
        for input in spec.inputs.iter().filter(|p| p.system) {
            let (source_type, source_port) = match (spec.type_name, input.port) {
                ("glide", PortId::AudioInput0) => ("global_frequency", PortId::GlobalFrequency),
                (_, PortId::GlobalFrequency) => ("glide", PortId::AudioOutput0),
                (_, PortId::CombinedGate) => ("gatemixer", PortId::CombinedGate),
                _ => continue,
            };
            let Some(source) = self.first_of(source_type) else {
                continue;
            };
            if source == id {
                continue;
            }
            self.push_connection(&ConnectionInfo {
                from_id: source,
                from_port: Some(source_port),
                to_id: id.to_string(),
                to_port: input.port,
                amount: 1.0,
                modulation_type: catalog::ModulationType::Additive.value(),
                transform: 0,
            });
        }
    }

    fn push_connection(&mut self, info: &ConnectionInfo) {
        let list = self.voice_mut()["connections"]
            .as_array_mut()
            .expect("normalized: connections is a list");
        list.push(info.to_json());
    }

    /// Removes a node with its state, its connections, its macro routes and its
    /// embedded assets. The voice's wiring nodes and its mixer cannot go.
    pub fn remove_node(&mut self, id: &str) -> Res<()> {
        let info = self
            .node(id)
            .ok_or_else(|| format!("no such node '{id}'"))?;
        let spec = node_spec(&info.type_name);
        if let Some(spec) = spec {
            if !spec.deletable {
                return Err(format!("{} cannot be removed", spec.title));
            }
        }
        // Its entry.
        if let Some(list) = self.voice_mut()["nodes"]
            .get_mut(&info.type_name)
            .and_then(Value::as_array_mut)
        {
            list.retain(|e| e["id"].as_str() != Some(id));
        }
        // Its connections.
        if let Some(list) = self.voice_mut()["connections"].as_array_mut() {
            list.retain(|c| c["fromId"].as_str() != Some(id) && c["toId"].as_str() != Some(id));
        }
        // Its state.
        if let Some(spec) = spec {
            if let (Some(map), StateShape::Map) = (spec.state_map, spec.state_shape) {
                if let Some(states) = self.synth_mut().get_mut(map).and_then(Value::as_object_mut) {
                    states.remove(id);
                }
            }
        }
        // Macro routes to it.
        if let Some(routes) = self.routes_mut_if_any() {
            routes.retain(|r| r["targetId"].as_str() != Some(id));
        }
        // Its embedded assets (`sample_<id>`, `impulse_response_<id>`, ...).
        if let Some(assets) = self.root["audioAssets"].as_object_mut() {
            let suffix = format!("_{id}");
            assets.retain(|key, _| !key.ends_with(&suffix));
        }
        // The chain.
        if spec.is_some_and(|s| s.is_effect()) {
            let mut chain = self.effect_chain();
            chain.retain(|c| c != id);
            self.set_chain(chain);
        }
        Ok(())
    }

    pub fn rename_node(&mut self, id: &str, name: &str) -> Res<()> {
        let info = self
            .node(id)
            .ok_or_else(|| format!("no such node '{id}'"))?;
        let list = self.voice_mut()["nodes"]
            .get_mut(&info.type_name)
            .and_then(Value::as_array_mut)
            .expect("the node's list");
        for entry in list {
            if entry["id"].as_str() == Some(id) {
                entry["name"] = json!(name);
            }
        }
        Ok(())
    }

    // ----- effects ----------------------------------------------------------------

    /// The effect ids in processing order.
    pub fn effect_chain(&self) -> Vec<String> {
        match self.root["synthState"]["effectChain"].as_array() {
            Some(chain) => chain
                .iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect(),
            None => (0..EFFECTS.len())
                .map(|slot| (EFFECT_ID_BASE + slot).to_string())
                .collect(),
        }
    }

    fn set_chain(&mut self, chain: Vec<String>) {
        let default: Vec<String> = (0..EFFECTS.len())
            .map(|slot| (EFFECT_ID_BASE + slot).to_string())
            .collect();
        if chain == default {
            self.synth_mut().remove("effectChain");
        } else {
            self.synth_mut().insert("effectChain".into(), json!(chain));
        }
    }

    /// The effects in processing order, with their types.
    pub fn effects(&self) -> Vec<NodeInfo> {
        self.effect_chain()
            .into_iter()
            .map(|id| {
                let found = self.node(&id);
                NodeInfo {
                    type_name: self.node_type(&id).unwrap_or_default(),
                    name: found.map(|n| n.name).unwrap_or_default(),
                    id,
                }
            })
            .collect()
    }

    /// Moves the effect at position `from` in the chain to position `to`.
    pub fn move_effect(&mut self, from: usize, to: usize) -> Res<()> {
        let mut chain = self.effect_chain();
        if from >= chain.len() || to >= chain.len() {
            return Err(format!(
                "the chain has {} effects, there is no position {}",
                chain.len(),
                from.max(to)
            ));
        }
        let moved = chain.remove(from);
        chain.insert(to, moved);
        self.set_chain(chain);
        Ok(())
    }

    // ----- parameters ----------------------------------------------------------------

    /// The state object of a node, if it has one in the document.
    pub fn node_state(&self, id: &str) -> Option<&Value> {
        let type_name = self.node_type(id)?;
        let spec = node_spec(&type_name)?;
        let map = spec.state_map?;
        match spec.state_shape {
            StateShape::Map => self.root["synthState"][map].get(id),
            StateShape::Single => self.root["synthState"].get(map),
            StateShape::None => None,
        }
    }

    /// A parameter's value as the document stores it (a node without the key in
    /// its state has the catalogue's default).
    pub fn get_param(&self, id: &str, key: &str) -> Res<Value> {
        let type_name = self
            .node_type(id)
            .ok_or_else(|| format!("no such node '{id}'"))?;
        let spec = node_spec(&type_name).expect("known type");
        let param = spec.param(key).ok_or_else(|| no_param_message(spec, key))?;
        Ok(self
            .node_state(id)
            .and_then(|s| s.get(key))
            .cloned()
            .unwrap_or_else(|| param.default_value()))
    }

    /// Sets a parameter of a node (see the catalogue for the keys). The value is
    /// checked against the parameter's kind and clamped to its range; what is
    /// stored comes back, with the message for a running synth.
    pub fn set_param(&mut self, id: &str, key: &str, value: &Value) -> Res<SetParam> {
        let type_name = self
            .node_type(id)
            .ok_or_else(|| format!("no such node '{id}'"))?;
        if self.node(id).is_none() && !self.effect_chain().iter().any(|e| e == id) {
            return Err(format!("no such node '{id}'"));
        }
        let spec = node_spec(&type_name).expect("known type");
        let param_index = spec
            .param_index(key)
            .ok_or_else(|| no_param_message(spec, key))?;
        let param = &spec.params[param_index];
        let stored = param.check(value)?;
        let map = spec.state_map.expect("a node with parameters has state");
        self.create_state(spec, id, false);
        let synth = self.synth_mut();
        let state = match spec.state_shape {
            StateShape::Map => synth
                .get_mut(map)
                .and_then(|m| m.get_mut(id))
                .and_then(Value::as_object_mut),
            StateShape::Single => synth.get_mut(map).and_then(Value::as_object_mut),
            StateShape::None => None,
        }
        .ok_or_else(|| format!("'{id}' has no state to set"))?;
        state.insert(key.to_string(), stored.clone());
        derive_fields(&type_name, state);
        Ok(SetParam {
            edit: node_key(id)
                .map(|node| ParamEdit::new(node, param_index as u16, to_f32(&stored))),
            value: stored,
        })
    }

    /// The instrument's output gain (`instrumentGain`, 1 when the patch has none).
    pub fn instrument_gain(&self) -> f32 {
        self.root["synthState"]["instrumentGain"]
            .as_f64()
            .unwrap_or(1.0) as f32
    }

    pub fn set_instrument_gain(&mut self, gain: f64) -> Res<SetParam> {
        let stored = INSTRUMENT_GAIN.check(&json!(gain))?;
        self.synth_mut()
            .insert("instrumentGain".into(), stored.clone());
        Ok(SetParam {
            edit: Some(ParamEdit::gain(to_f32(&stored))),
            value: stored,
        })
    }

    /// How many voices play (the canonical voice is cloned this many times).
    pub fn voice_count(&self) -> usize {
        self.root["synthState"]["layout"]["voiceCount"]
            .as_u64()
            .unwrap_or(1) as usize
    }

    pub fn set_voice_count(&mut self, count: usize) -> Res<()> {
        if count == 0 || count > MAX_VOICES {
            return Err(format!("a patch has 1 to {MAX_VOICES} voices, not {count}"));
        }
        self.root["synthState"]["layout"]["voiceCount"] = json!(count);
        Ok(())
    }

    // ----- connections ------------------------------------------------------------------

    /// How a connection into `to_port` of `to_id` modulates unless told otherwise.
    pub fn default_modulation(&self, to_id: &str, to_port: PortId) -> i32 {
        self.node_type(to_id)
            .and_then(|t| node_spec(&t))
            .and_then(|s| s.input(to_port))
            .map(|p| p.default_modulation().value())
            .unwrap_or(catalog::ModulationType::Additive.value())
    }

    /// Checks that `from_port` of one node may feed `to_port` of another, and
    /// says why not if it may not.
    pub fn can_connect(
        &self,
        from_id: &str,
        from_port: PortId,
        to_id: &str,
        to_port: PortId,
    ) -> Res<()> {
        let from = self
            .node(from_id)
            .ok_or_else(|| format!("no such node '{from_id}'"))?;
        let to = self
            .node(to_id)
            .ok_or_else(|| format!("no such node '{to_id}'"))?;
        if from_id == to_id {
            return Err("a node cannot feed itself".into());
        }
        let from_spec = node_spec(&from.type_name)
            .ok_or_else(|| format!("'{}' is not a node type the synth knows", from.type_name))?;
        let to_spec = node_spec(&to.type_name)
            .ok_or_else(|| format!("'{}' is not a node type the synth knows", to.type_name))?;
        if from_spec.is_effect() || to_spec.is_effect() {
            return Err("effects are in the effect chain, not wired".into());
        }
        let out = from_spec.output(from_port).ok_or_else(|| {
            format!(
                "{} has no output {}",
                from_spec.title,
                port_label(from_port)
            )
        })?;
        let input = to_spec
            .input(to_port)
            .ok_or_else(|| format!("{} has no input {}", to_spec.title, port_label(to_port)))?;
        if input.system {
            return Err("that input is wired by the engine".into());
        }
        if out.kind == catalog::PortKind::Control && input.kind == catalog::PortKind::Audio {
            return Err("a control signal cannot be heard: use it to modulate".into());
        }
        if self.reaches(to_id, from_id) {
            return Err("that would make a loop".into());
        }
        Ok(())
    }

    /// Whether `target` can be reached from `start` along the connections.
    fn reaches(&self, start: &str, target: &str) -> bool {
        let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for c in self.connections() {
            edges.entry(c.from_id).or_default().push(c.to_id);
        }
        let mut seen = BTreeSet::new();
        let mut stack = vec![start.to_string()];
        while let Some(id) = stack.pop() {
            if id == target {
                return true;
            }
            if seen.insert(id.clone()) {
                stack.extend(edges.get(&id).cloned().unwrap_or_default());
            }
        }
        false
    }

    /// Connects `from_port` of one node to `to_port` of another, or changes the
    /// connection that is there (an upsert on the four of them). `amount` is the
    /// scale of the source (a finite number, -100 to 100), `modulation_type` 0
    /// (VCA), 1 (bipolar) or 2 (additive), `transform` 0 to 3 (none, invert,
    /// square, cube). A connection from a patch that predates `fromPort` and
    /// goes to the same input is taken to be this one and gets the port.
    pub fn connect(
        &mut self,
        from_id: &str,
        from_port: PortId,
        to_id: &str,
        to_port: PortId,
        amount: f32,
        modulation_type: i32,
        transform: i32,
    ) -> Res<ConnectionInfo> {
        let existing = self.find_connection(from_id, from_port, to_id, to_port);
        if existing.is_none() {
            self.can_connect(from_id, from_port, to_id, to_port)?;
        }
        let info = ConnectionInfo {
            from_id: from_id.to_string(),
            from_port: Some(from_port),
            to_id: to_id.to_string(),
            to_port,
            amount: check_amount(amount)?,
            modulation_type: check_modulation(modulation_type)?,
            transform: check_transform(transform)?,
        };
        match existing {
            Some(index) => {
                self.voice_mut()["connections"][index] = info.to_json();
            }
            None => self.push_connection(&info),
        }
        Ok(info)
    }

    fn find_connection(
        &self,
        from_id: &str,
        from_port: PortId,
        to_id: &str,
        to_port: PortId,
    ) -> Option<usize> {
        self.voice()["connections"]
            .as_array()?
            .iter()
            .position(|c| {
                parse_connection(c).is_some_and(|c| {
                    c.from_id == from_id
                        && c.to_id == to_id
                        && c.to_port == to_port
                        && c.from_port.is_none_or(|p| p == from_port)
                })
            })
    }

    /// The connection between these ports, if there is one.
    pub fn connection(
        &self,
        from_id: &str,
        from_port: PortId,
        to_id: &str,
        to_port: PortId,
    ) -> Option<ConnectionInfo> {
        let index = self.find_connection(from_id, from_port, to_id, to_port)?;
        parse_connection(&self.voice()["connections"][index])
    }

    /// Removes a connection; false if there was none. The wires the engine makes
    /// itself (pitch, gate) cannot be removed.
    pub fn disconnect(
        &mut self,
        from_id: &str,
        from_port: PortId,
        to_id: &str,
        to_port: PortId,
    ) -> Res<bool> {
        if let Some(to) = self
            .node(to_id)
            .and_then(|n| node_spec(&n.type_name).map(|s| (n, s)))
        {
            if to.1.input(to_port).is_some_and(|p| p.system) {
                return Err("that input is wired by the engine".into());
            }
        }
        let Some(index) = self.find_connection(from_id, from_port, to_id, to_port) else {
            return Ok(false);
        };
        self.voice_mut()["connections"]
            .as_array_mut()
            .expect("a list")
            .remove(index);
        Ok(true)
    }

    /// Changes the amount of a connection.
    pub fn set_amount(
        &mut self,
        from_id: &str,
        from_port: PortId,
        to_id: &str,
        to_port: PortId,
        amount: f32,
    ) -> Res<ConnectionInfo> {
        let index = self
            .find_connection(from_id, from_port, to_id, to_port)
            .ok_or("no such connection")?;
        let amount = check_amount(amount)?;
        let slot = &mut self.voice_mut()["connections"][index];
        slot["amount"] = number(amount as f64);
        parse_connection(slot).ok_or_else(|| "the connection is malformed".to_string())
    }

    // ----- macros ---------------------------------------------------------------------------

    /// The macros' values (0 to 1).
    pub fn macro_values(&self) -> [f32; MACRO_COUNT] {
        let mut values = [0.0; MACRO_COUNT];
        if let Some(list) = self.root["synthState"]["macros"]["values"].as_array() {
            for (slot, v) in values.iter_mut().zip(list) {
                *slot = v.as_f64().unwrap_or(0.0) as f32;
            }
        }
        values
    }

    pub fn set_macro_value(&mut self, index: usize, value: f64) -> Res<SetParam> {
        if index >= MACRO_COUNT {
            return Err(format!("there are {MACRO_COUNT} macros, not {}", index + 1));
        }
        let stored = MACRO_VALUE.check(&json!(value))?;
        let mut values: Vec<Value> = self
            .macro_values()
            .iter()
            .map(|v| number(*v as f64))
            .collect();
        values[index] = stored.clone();
        self.macros_mut().insert("values".into(), json!(values));
        Ok(SetParam {
            edit: Some(ParamEdit::macro_value(index, to_f32(&stored))),
            value: stored,
        })
    }

    /// The macro routes, if the patch has a list of them (nothing is created).
    fn routes_mut_if_any(&mut self) -> Option<&mut Vec<Value>> {
        self.synth_mut()
            .get_mut("macros")?
            .get_mut("routes")?
            .as_array_mut()
    }

    fn macros_mut(&mut self) -> &mut Map<String, Value> {
        let synth = self.synth_mut();
        let macros = synth
            .entry("macros".to_string())
            .or_insert_with(|| json!({"values": [0, 0, 0, 0], "routes": []}));
        if !macros.is_object() {
            *macros = json!({"values": [0, 0, 0, 0], "routes": []});
        }
        let macros = macros.as_object_mut().expect("an object");
        macros.entry("routes").or_insert_with(|| json!([]));
        macros
    }

    pub fn macro_routes(&self) -> Vec<MacroRoute> {
        self.root["synthState"]["macros"]["routes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse_route)
            .collect()
    }

    /// Routes macro `macro_index` into `target_port` of a node, or changes the
    /// route that is there. As in the DAW, an `amount` of 0 or less removes it.
    pub fn set_macro_route(
        &mut self,
        macro_index: usize,
        target_id: &str,
        target_port: PortId,
        amount: f32,
        modulation_type: i32,
        transform: i32,
    ) -> Res<()> {
        if macro_index >= MACRO_COUNT {
            return Err(format!(
                "there are {MACRO_COUNT} macros, not {}",
                macro_index + 1
            ));
        }
        if amount <= 0.0 {
            self.remove_macro_route(macro_index, target_id, target_port);
            return Ok(());
        }
        let node = self
            .node(target_id)
            .ok_or_else(|| format!("no such node '{target_id}'"))?;
        let spec = node_spec(&node.type_name)
            .filter(|s| !s.is_effect())
            .ok_or("a macro drives a voice node, not an effect")?;
        let input = spec
            .input(target_port)
            .ok_or_else(|| format!("{} has no input {}", spec.title, port_label(target_port)))?;
        if input.system {
            return Err("that input is wired by the engine".into());
        }
        let route = json!({
            "macroIndex": macro_index,
            "targetId": target_id,
            "targetPort": target_port as u32,
            "amount": number(check_amount(amount)? as f64),
            "modulationType": check_modulation(modulation_type)?,
            "modulationTransformation": check_transform(transform)?,
        });
        let routes = self.macros_mut()["routes"].as_array_mut().expect("a list");
        let same = |r: &Value| {
            parse_route(r).is_some_and(|r| {
                r.macro_index == macro_index
                    && r.target_id == target_id
                    && r.target_port == target_port
            })
        };
        match routes.iter().position(same) {
            Some(i) => routes[i] = route,
            None => routes.push(route),
        }
        Ok(())
    }

    /// Removes the route; false if there was none.
    pub fn remove_macro_route(
        &mut self,
        macro_index: usize,
        target_id: &str,
        target_port: PortId,
    ) -> bool {
        let routes = self.macros_mut()["routes"].as_array_mut().expect("a list");
        let before = routes.len();
        routes.retain(|r| {
            !parse_route(r).is_some_and(|r| {
                r.macro_index == macro_index
                    && r.target_id == target_id
                    && r.target_port == target_port
            })
        });
        routes.len() != before
    }

    // ----- checking --------------------------------------------------------------------------

    /// What is wrong with the document as a patch for the engine: nodes with ids
    /// the loader cannot read, connections to nodes or ports that do not exist,
    /// loops. An empty list is a document the loader will build. (A patch from
    /// the DAW may have a few oddities the loader copes with.)
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let nodes = self.nodes();
        let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        for node in &nodes {
            let Some(spec) = node_spec(&node.type_name) else {
                problems.push(format!(
                    "node '{}' has unknown type '{}'",
                    node.id, node.type_name
                ));
                continue;
            };
            if spec.is_effect() {
                if node.id.parse::<usize>().is_err() {
                    problems.push(format!("effect id '{}' is not a number", node.id));
                }
            } else if uuid::Uuid::parse_str(&node.id).is_err() {
                problems.push(format!("node id '{}' is not a UUID", node.id));
            }
        }
        let mut seen = HashSet::new();
        for node in &nodes {
            if !seen.insert(node.id.as_str()) {
                problems.push(format!("node id '{}' twice", node.id));
            }
        }
        for fixed in ["global_frequency", "global_velocity", "gatemixer", "mixer"] {
            let count = nodes.iter().filter(|n| n.type_name == fixed).count();
            if count != 1 {
                problems.push(format!(
                    "a voice needs exactly one {fixed}, this has {count}"
                ));
            }
        }
        for c in self.connections() {
            for end in [&c.from_id, &c.to_id] {
                if !ids.contains(end.as_str()) {
                    problems.push(format!(
                        "connection {} -> {} names unknown node '{end}'",
                        c.from_id, c.to_id
                    ));
                }
            }
            let (Some(from), Some(to)) = (self.node(&c.from_id), self.node(&c.to_id)) else {
                continue;
            };
            let (Some(from_spec), Some(to_spec)) =
                (node_spec(&from.type_name), node_spec(&to.type_name))
            else {
                continue;
            };
            if let Some(port) = c.from_port {
                if from_spec.output(port).is_none() {
                    problems.push(format!(
                        "{} -> {}: {} has no output {}",
                        c.from_id,
                        c.to_id,
                        from_spec.title,
                        port.name()
                    ));
                }
            }
            if to_spec.input(c.to_port).is_none() {
                problems.push(format!(
                    "{} -> {}: {} has no input {}",
                    c.from_id,
                    c.to_id,
                    to_spec.title,
                    c.to_port.name()
                ));
            }
        }
        // A loop: some connection's target reaches its source.
        for c in self.connections() {
            if self.reaches(&c.to_id, &c.from_id) {
                problems.push(format!("a loop through {} -> {}", c.from_id, c.to_id));
                break;
            }
        }
        for id in self.effect_chain() {
            if self
                .node_type(&id)
                .is_none_or(|t| effect_spec(&t).is_none())
            {
                problems.push(format!("effect '{id}' of the chain has no known type"));
            }
        }
        problems
    }

    // ----- JSON operations -------------------------------------------------------------------

    /// Runs one operation described as JSON and returns its result as JSON:
    ///
    /// - `{"op": "add_node", "type": "oscillator"}` -> `{"id": ..}`
    /// - `{"op": "remove_node", "id": ..}`
    /// - `{"op": "rename_node", "id": .., "name": ..}`
    /// - `{"op": "set_param", "id": .., "key": .., "value": ..}` -> `{"value": ..,
    ///   "edit": {"node": "<decimal u128>", "param": n, "value": x}}`
    /// - `{"op": "connect", "from": .., "from_port": .., "to": .., "to_port": ..,
    ///   "amount": 1, "modulation_type": "vca", "transform": "none"}` -> `{"connection": ..}`
    ///   (ports by name or number; the amount, type and transform are optional:
    ///   an existing connection keeps what is not given, a new one takes the
    ///   port's default type)
    /// - `{"op": "disconnect", "from": .., "from_port": .., "to": .., "to_port": ..}`
    ///   -> `{"removed": bool}`
    /// - `{"op": "set_amount", ...the four.., "amount": x}`
    /// - `{"op": "set_macro", "index": 0, "value": 0.5}`
    /// - `{"op": "macro_route", "macro": 0, "target": .., "port": .., "amount": 1,
    ///   "modulation_type": .., "transform": ..}` / `{"op": "remove_macro_route", ..}`
    /// - `{"op": "set_instrument_gain", "value": 1}`, `{"op": "set_voices", "count": 8}`
    /// - `{"op": "move_effect", "from": 0, "to": 2}`
    /// - `{"op": "set_editor", "editor": {..}}`, `{"op": "set_name", "name": ..}`
    pub fn apply(&mut self, op: &Value) -> Res<Value> {
        let name = op
            .get("op")
            .and_then(Value::as_str)
            .ok_or("an operation needs an 'op'")?;
        match name {
            "add_node" => {
                let id = self.add_node(str_field(op, "type")?)?;
                Ok(json!({ "id": id }))
            }
            "remove_node" => {
                self.remove_node(str_field(op, "id")?)?;
                Ok(json!({}))
            }
            "rename_node" => {
                self.rename_node(str_field(op, "id")?, str_field(op, "name")?)?;
                Ok(json!({}))
            }
            "set_param" => {
                let value = op.get("value").ok_or("set_param needs a 'value'")?;
                let set = self.set_param(str_field(op, "id")?, str_field(op, "key")?, value)?;
                Ok(set_param_json(&set))
            }
            "connect" => {
                let (from, from_port, to, to_port) = connection_ends(op)?;
                let existing = self.connection(from, from_port, to, to_port);
                let amount = number_field(op, "amount")?
                    .or(existing.as_ref().map(|c| c.amount as f64))
                    .unwrap_or(1.0) as f32;
                let modulation = match op.get("modulation_type") {
                    Some(v) => parse_modulation(v)?,
                    None => existing
                        .as_ref()
                        .map(|c| c.modulation_type)
                        .unwrap_or_else(|| self.default_modulation(to, to_port)),
                };
                let transform = match op.get("transform") {
                    Some(v) => parse_transform(v)?,
                    None => existing.as_ref().map(|c| c.transform).unwrap_or(0),
                };
                let info =
                    self.connect(from, from_port, to, to_port, amount, modulation, transform)?;
                Ok(json!({ "connection": info.to_json() }))
            }
            "disconnect" => {
                let (from, from_port, to, to_port) = connection_ends(op)?;
                Ok(json!({ "removed": self.disconnect(from, from_port, to, to_port)? }))
            }
            "set_amount" => {
                let (from, from_port, to, to_port) = connection_ends(op)?;
                let amount = number_field(op, "amount")?.ok_or("set_amount needs an 'amount'")?;
                let info = self.set_amount(from, from_port, to, to_port, amount as f32)?;
                Ok(json!({ "connection": info.to_json() }))
            }
            "set_macro" => {
                let index = usize_field(op, "index")?;
                let value = number_field(op, "value")?.ok_or("set_macro needs a 'value'")?;
                Ok(set_param_json(&self.set_macro_value(index, value)?))
            }
            "macro_route" | "remove_macro_route" => {
                let index = usize_field(op, "macro")?;
                let target = str_field(op, "target")?;
                let port = parse_port(op.get("port").ok_or("a route needs a 'port'")?)?;
                if name == "remove_macro_route" {
                    return Ok(json!({ "removed": self.remove_macro_route(index, target, port) }));
                }
                let amount = number_field(op, "amount")?.unwrap_or(1.0) as f32;
                let modulation = match op.get("modulation_type") {
                    Some(v) => parse_modulation(v)?,
                    None => self.default_modulation(target, port),
                };
                let transform = match op.get("transform") {
                    Some(v) => parse_transform(v)?,
                    None => 0,
                };
                self.set_macro_route(index, target, port, amount, modulation, transform)?;
                Ok(json!({}))
            }
            "set_instrument_gain" => {
                let value =
                    number_field(op, "value")?.ok_or("set_instrument_gain needs a 'value'")?;
                Ok(set_param_json(&self.set_instrument_gain(value)?))
            }
            "set_voices" => {
                self.set_voice_count(usize_field(op, "count")?)?;
                Ok(json!({}))
            }
            "move_effect" => {
                self.move_effect(usize_field(op, "from")?, usize_field(op, "to")?)?;
                Ok(json!({ "chain": self.effect_chain() }))
            }
            "set_editor" => {
                self.set_editor(op.get("editor").cloned().unwrap_or(Value::Null));
                Ok(json!({}))
            }
            "set_name" => {
                self.set_name(str_field(op, "name")?);
                Ok(json!({}))
            }
            other => Err(format!("unknown operation '{other}'")),
        }
    }
}

// ---------------------------------------------------------------------------
// helpers

fn no_param_message(spec: &NodeSpec, key: &str) -> String {
    let keys: Vec<_> = spec.params.iter().map(|p| p.key).collect();
    format!(
        "{} has no parameter '{key}' (it has: {})",
        spec.title,
        keys.join(", ")
    )
}

fn port_label(port: PortId) -> &'static str {
    port.name()
}

fn to_f32(value: &Value) -> f32 {
    match value {
        Value::Bool(b) => *b as u8 as f32,
        other => other.as_f64().unwrap_or(0.0) as f32,
    }
}

fn check_amount(amount: f32) -> Res<f32> {
    if !amount.is_finite() {
        return Err("the amount must be a finite number".into());
    }
    Ok(amount.clamp(-100.0, 100.0))
}

fn check_modulation(value: i32) -> Res<i32> {
    if (0..=2).contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "modulation type {value} is not 0 (vca), 1 (bipolar) or 2 (additive)"
        ))
    }
}

fn check_transform(value: i32) -> Res<i32> {
    if (0..=3).contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "transform {value} is not 0 (none), 1 (invert), 2 (square) or 3 (cube)"
        ))
    }
}

/// Keeps the fields the engine derives from a node's parameters consistent:
/// the detune in cents from octaves, semitones and cents; a sampler's frequency.
fn derive_fields(type_name: &str, state: &mut Map<String, Value>) {
    let has_detune = matches!(type_name, "oscillator" | "wavetable_oscillator" | "sampler");
    if !has_detune {
        return;
    }
    let part = |state: &Map<String, Value>, key: &str| {
        state.get(key).and_then(Value::as_f64).unwrap_or(0.0)
    };
    let detune = part(state, "detune_oct") * 1200.0
        + part(state, "detune_semi") * 100.0
        + part(state, "detune_cents");
    state.insert("detune".into(), number(detune));
    if type_name == "sampler" {
        state.insert(
            "frequency".into(),
            number(440.0 * 2f64.powf(detune / 1200.0)),
        );
    }
}

fn parse_connection(c: &Value) -> Option<ConnectionInfo> {
    Some(ConnectionInfo {
        from_id: c["fromId"].as_str()?.to_string(),
        from_port: c["fromPort"]
            .as_u64()
            .and_then(|p| PortId::try_from_u32(p as u32)),
        to_id: c["toId"].as_str()?.to_string(),
        to_port: PortId::try_from_u32(c["target"].as_u64()? as u32)?,
        amount: c["amount"].as_f64().unwrap_or(1.0) as f32,
        modulation_type: c["modulationType"].as_i64().unwrap_or(2) as i32,
        transform: c["modulationTransformation"].as_i64().unwrap_or(0) as i32,
    })
}

fn parse_route(r: &Value) -> Option<MacroRoute> {
    Some(MacroRoute {
        macro_index: r["macroIndex"].as_u64()? as usize,
        target_id: r["targetId"].as_str()?.to_string(),
        target_port: PortId::try_from_u32(r["targetPort"].as_u64()? as u32)?,
        amount: r["amount"].as_f64().unwrap_or(0.0) as f32,
        modulation_type: r["modulationType"].as_i64().unwrap_or(2) as i32,
        transform: r["modulationTransformation"].as_i64().unwrap_or(0) as i32,
    })
}

fn set_param_json(set: &SetParam) -> Value {
    let mut result = json!({ "value": set.value });
    if let Some(edit) = set.edit {
        result["edit"] = json!({
            "node": edit.node.to_string(),
            "param": edit.param,
            "value": edit.value,
        });
    }
    result
}

fn str_field<'a>(op: &'a Value, key: &str) -> Res<&'a str> {
    op.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("'{key}' must be a string"))
}

fn number_field(op: &Value, key: &str) -> Res<Option<f64>> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be a number")),
    }
}

fn usize_field(op: &Value, key: &str) -> Res<usize> {
    op.get(key)
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .ok_or_else(|| format!("'{key}' must be a whole number"))
}

/// A port by `PortId` name or number.
pub fn parse_port(value: &Value) -> Res<PortId> {
    match value {
        Value::String(name) => {
            PortId::from_name(name).ok_or_else(|| format!("'{name}' is not a port"))
        }
        Value::Number(n) => n
            .as_u64()
            .and_then(|n| PortId::try_from_u32(n as u32))
            .ok_or_else(|| format!("{n} is not a port number")),
        _ => Err("a port is a name or a number".into()),
    }
}

fn parse_modulation(value: &Value) -> Res<i32> {
    match value {
        Value::String(name) => catalog::ModulationType::ALL
            .iter()
            .find(|m| m.name() == name)
            .map(|m| m.value())
            .ok_or_else(|| format!("'{name}' is not vca, bipolar or additive")),
        Value::Number(n) => check_modulation(n.as_i64().unwrap_or(-1) as i32),
        _ => Err("a modulation type is a name or a number".into()),
    }
}

fn parse_transform(value: &Value) -> Res<i32> {
    match value {
        Value::String(name) => catalog::TRANSFORMS
            .iter()
            .position(|t| t == name)
            .map(|i| i as i32)
            .ok_or_else(|| format!("'{name}' is not none, invert, square or cube")),
        Value::Number(n) => check_transform(n.as_i64().unwrap_or(-1) as i32),
        _ => Err("a transform is a name or a number".into()),
    }
}

fn connection_ends(op: &Value) -> Res<(&str, PortId, &str, PortId)> {
    let from = str_field(op, "from")?;
    let to = str_field(op, "to")?;
    let from_port = parse_port(
        op.get("from_port")
            .ok_or("a connection needs a 'from_port'")?,
    )?;
    let to_port = parse_port(op.get("to_port").ok_or("a connection needs a 'to_port'")?)?;
    Ok((from, from_port, to, to_port))
}

/// Brings a parsed patch to the shape the editor works on: `synthState.layout.
/// canonicalVoice` with `nodes` and `connections`.
fn normalize(root: &mut Value) -> Res<()> {
    let synth = root
        .get_mut("synthState")
        .and_then(Value::as_object_mut)
        .ok_or("not a patch: synthState is not an object")?;
    let layout = synth
        .entry("layout")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("not a patch: layout is not an object")?;
    if layout.get("canonicalVoice").is_none_or(Value::is_null) {
        let first = layout
            .get("voices")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
            .cloned();
        let count = layout
            .get("voices")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        match first {
            Some(voice) => {
                layout.insert("canonicalVoice".into(), voice);
                if layout.get("voiceCount").is_none_or(Value::is_null) {
                    layout.insert("voiceCount".into(), json!(count));
                }
            }
            None => return Err("not a patch: the layout has no voice".into()),
        }
    }
    layout.remove("voices");
    let voice = layout
        .get_mut("canonicalVoice")
        .and_then(Value::as_object_mut)
        .ok_or("not a patch: canonicalVoice is not an object")?;
    if !voice.get("nodes").is_some_and(Value::is_object) {
        voice.insert("nodes".into(), json!({}));
    }
    if !voice.get("connections").is_some_and(Value::is_array) {
        voice.insert("connections".into(), json!([]));
    }
    if layout.get("voiceCount").is_none_or(Value::is_null) {
        layout.insert("voiceCount".into(), json!(1));
    }
    if root.get("audioAssets").is_none_or(Value::is_null) {
        root["audioAssets"] = json!({});
    }
    if root.get("metadata").is_none_or(Value::is_null) {
        root["metadata"] = json!({"id": format!("patch_{}", NodeId::new().0.simple()), "name": "Patch", "version": 1});
    }
    Ok(())
}
