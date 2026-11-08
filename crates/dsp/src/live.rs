//! What the audio thread is told about a running synth: a parameter change as a
//! small plain message (no allocation to send, apply or smooth).
//!
//! [`patch_edit`](crate::patch_edit) turns "set parameter `cutoff` of node X"
//! into a [`ParamEdit`] (the node's id as a number, the parameter's index in the
//! [catalogue](crate::catalog), the value) while it edits the patch document.
//! The engine ([`AudioEngine::edit_param`](crate::audio_engine::native::AudioEngine))
//! takes it from there.

/// The node a [`ParamEdit`] names when it is about the patch, not a node.
pub const PATCH: u128 = 0;
/// `PATCH` parameter 0: the instrument's output gain.
pub const PATCH_GAIN: u16 = 0;
/// `PATCH` parameters 1 to 4: the macros' values.
pub const PATCH_MACRO0: u16 = 1;

/// A parameter of a node (or of the patch) set to a value. `Copy`, 24 bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamEdit {
    /// The node's id as a number: a UUID's 128 bits, or an effect's decimal id
    /// (`10003`), or [`PATCH`].
    pub node: u128,
    /// The parameter's index in the node type's list in the catalogue (for
    /// [`PATCH`], one of the constants above).
    pub param: u16,
    /// The value as the patch stores it: a bool is 0 or 1, an enum its index.
    pub value: f32,
}

impl ParamEdit {
    pub fn new(node: u128, param: u16, value: f32) -> ParamEdit {
        ParamEdit { node, param, value }
    }

    /// The instrument's output gain.
    pub fn gain(value: f32) -> ParamEdit {
        ParamEdit::new(PATCH, PATCH_GAIN, value)
    }

    /// A macro's value (0 to 1).
    pub fn macro_value(index: usize, value: f32) -> ParamEdit {
        ParamEdit::new(PATCH, PATCH_MACRO0 + index as u16, value)
    }
}

/// A node id string as the number a [`ParamEdit`] carries: a UUID's value or a
/// decimal effect id.
pub fn node_key(id: &str) -> Option<u128> {
    if let Ok(uuid) = uuid::Uuid::parse_str(id) {
        return Some(uuid.as_u128());
    }
    id.parse::<u128>().ok()
}
