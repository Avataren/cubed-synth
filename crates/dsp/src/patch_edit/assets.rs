//! The patch's embedded audio (`audioAssets`) as the edit layer reaches it: putting an asset in or
//! taking it out (keeping the one it replaces for an undo), and decoding what is there, cached.

use std::sync::Arc;

use serde_json::{json, Value};

use super::{number, PatchDoc, Res};
use crate::wav::{base64_decode, decode_wav, Pcm};
use crate::wavetable_edit::{Table, FRAME_SIZE};

/// Decoded assets kept at most (a decoded wavetable is 2 MB, a sample as long as it is).
const KEPT: usize = 3;

/// Decoded audio the document has been asked about lately, by asset id.
#[derive(Debug, Default)]
pub(super) struct Decoded {
    pcm: Vec<(String, Arc<Pcm>)>,
    tables: Vec<(String, Arc<Table>)>,
}

impl Decoded {
    fn forget(&mut self, id: &str) {
        self.pcm.retain(|(k, _)| k != id);
        self.tables.retain(|(k, _)| k != id);
    }
}

fn keep<T>(list: &mut Vec<(String, T)>, id: &str, value: T) {
    list.retain(|(k, _)| k != id);
    list.push((id.to_string(), value));
    if list.len() > KEPT {
        list.remove(0);
    }
}

/// An asset entry as the DAW writes it (`AudioAsset`).
pub(super) fn asset_entry(
    id: &str,
    kind: &str,
    base64_data: String,
    rate: u32,
    channels: usize,
    seconds: f64,
) -> Value {
    json!({
        "id": id,
        "type": kind,
        "base64Data": base64_data,
        "sampleRate": rate,
        "channels": channels,
        "duration": number(seconds),
    })
}

impl PatchDoc {
    /// The asset `id`, if the document has it.
    pub(super) fn asset(&self, id: &str) -> Option<&Value> {
        self.root["audioAssets"].get(id)
    }

    /// Puts an asset in, or (`None`) takes it out. What it replaces goes to the undo step in
    /// the making, if the history is on.
    pub(super) fn put_asset(&mut self, id: &str, entry: Option<Value>) {
        let old = self.swap_asset(id, entry);
        self.history.stash(id, old);
    }

    /// As [`put_asset`](Self::put_asset), for what an undo gives back: the replaced one is
    /// dropped.
    pub(super) fn restore_asset(&mut self, id: &str, entry: Option<Value>) {
        drop(self.swap_asset(id, entry));
    }

    fn swap_asset(&mut self, id: &str, entry: Option<Value>) -> Option<Value> {
        self.decoded.borrow_mut().forget(id);
        if !self.root["audioAssets"].is_object() {
            self.root["audioAssets"] = json!({});
        }
        let assets = self.root["audioAssets"]
            .as_object_mut()
            .expect("audioAssets is an object");
        match entry {
            Some(entry) => assets.insert(id.to_string(), entry),
            None => assets.remove(id),
        }
    }

    /// The sample of asset `id` decoded.
    pub(super) fn pcm_of(&self, id: &str) -> Res<Arc<Pcm>> {
        if let Some((_, pcm)) = self.decoded.borrow().pcm.iter().find(|(k, _)| k == id) {
            return Ok(pcm.clone());
        }
        let asset = self.asset(id).ok_or_else(|| format!("no asset '{id}'"))?;
        let text = asset["base64Data"]
            .as_str()
            .ok_or_else(|| format!("asset '{id}' has no data"))?;
        let pcm = Arc::new(
            decode_wav(&base64_decode(text).map_err(|e| format!("asset '{id}': {e}"))?)
                .map_err(|e| format!("asset '{id}': {e}"))?,
        );
        keep(&mut self.decoded.borrow_mut().pcm, id, pcm.clone());
        Ok(pcm)
    }

    /// The wavetable of asset `id`, in frames of [`FRAME_SIZE`] samples as the DAW's engine
    /// cuts the file.
    pub(super) fn table_of(&self, id: &str) -> Res<Arc<Table>> {
        if let Some((_, table)) = self.decoded.borrow().tables.iter().find(|(k, _)| k == id) {
            return Ok(table.clone());
        }
        let asset = self.asset(id).ok_or_else(|| format!("no asset '{id}'"))?;
        let text = asset["base64Data"]
            .as_str()
            .ok_or_else(|| format!("asset '{id}' has no data"))?;
        let bytes = base64_decode(text).map_err(|e| format!("asset '{id}': {e}"))?;
        let (table, _) =
            Table::from_wav(&bytes, FRAME_SIZE).map_err(|e| format!("asset '{id}': {e}"))?;
        let table = Arc::new(table);
        keep(&mut self.decoded.borrow_mut().tables, id, table.clone());
        Ok(table)
    }

    /// Remembers a table that has just been written, so the next edit does not decode it again.
    pub(super) fn keep_table(&self, id: &str, table: Arc<Table>) {
        keep(&mut self.decoded.borrow_mut().tables, id, table);
    }

    /// Remembers a sample that has just been written.
    pub(super) fn keep_pcm(&self, id: &str, pcm: Arc<Pcm>) {
        keep(&mut self.decoded.borrow_mut().pcm, id, pcm);
    }
}
