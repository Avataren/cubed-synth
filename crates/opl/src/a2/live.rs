//! Changes a playing Adlib Tracker II song can take without being reopened.
//!
//! `cubed_opl::edit::A2mDoc::live_edits` says which edits of a document are
//! these ([`LiveEdit`]), and [`A2Player::apply_edit`](super::player::A2Player::apply_edit)
//! lands one in a song that is playing, in place and without allocating. A song
//! reads its cells when its row comes, its instruments when a cell names them
//! (a track that holds the edited one loads it again at its next instrument
//! cell), and its macro tables as they run, so every one of these is heard the
//! next time the song gets to it; a note sounding now goes on as it was.
//!
//! Anything else (the pattern list, the order list, the pattern length, the
//! track count, flags, the macro speed-up, lock flags, 4-op pairs) moves the
//! song's shape: open the edited song as a new one, seek it to where the old one
//! is and swap them.

use serde_json::{json, Value};

use super::model::{ArpeggioMacro, Cell, FmMacro, VibratoMacro};

/// One live change. Cells and instruments are small and fixed in size (a host
/// can carry them as bytes); macro tables hold 255 entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveEdit {
    /// A cell of pattern `pattern` (its number, spare patterns included), as
    /// the file holds it.
    Cell {
        pattern: u16,
        row: u16,
        track: u8,
        cell: Cell,
    },
    /// The data of instrument `number` (from 1), not its name.
    Instrument {
        number: u8,
        fm: [u8; 11],
        panning: u8,
        finetune: i8,
        voice_type: u8,
    },
    FmMacro {
        number: u8,
        table: FmMacro,
    },
    ArpeggioMacro {
        number: u8,
        table: ArpeggioMacro,
    },
    VibratoMacro {
        number: u8,
        table: VibratoMacro,
    },
    /// The song's tempo and speed, now.
    Timing {
        tempo: u8,
        speed: u8,
    },
}

impl LiveEdit {
    /// `{"kind": "cell", "pattern", "row", "track", "raw": [note byte,
    /// instrument, effect, param, effect 2, param 2]}`, `{"kind": "instrument",
    /// "number", "fm": [11], "panning", "finetune", "voice_type"}`, `{"kind":
    /// "fm_macro" | "arpeggio_macro" | "vibrato_macro", "number"}` or `{"kind":
    /// "timing", "tempo", "speed"}`.
    pub fn to_json(&self) -> Value {
        match self {
            LiveEdit::Cell {
                pattern,
                row,
                track,
                cell,
            } => json!({
                "kind": "cell", "pattern": pattern, "row": row, "track": track,
                "raw": [cell.note, cell.instrument, cell.effects[0].0, cell.effects[0].1,
                        cell.effects[1].0, cell.effects[1].1],
            }),
            LiveEdit::Instrument {
                number,
                fm,
                panning,
                finetune,
                voice_type,
            } => json!({
                "kind": "instrument", "number": number, "fm": fm, "panning": panning,
                "finetune": finetune, "voice_type": voice_type,
            }),
            LiveEdit::FmMacro { number, .. } => json!({"kind": "fm_macro", "number": number}),
            LiveEdit::ArpeggioMacro { number, .. } => {
                json!({"kind": "arpeggio_macro", "number": number})
            }
            LiveEdit::VibratoMacro { number, .. } => {
                json!({"kind": "vibrato_macro", "number": number})
            }
            LiveEdit::Timing { tempo, speed } => {
                json!({"kind": "timing", "tempo": tempo, "speed": speed})
            }
        }
    }
}
