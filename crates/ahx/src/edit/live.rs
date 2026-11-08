//! Following an edit with a song that is playing.
//!
//! After an edit, [`AhxDoc::live_edits`] says whether a player that holds the
//! song as it was can take the change without being opened again, and what to
//! hand it: `Some(LiveEdits)` when the change was nothing but
//!
//! | what changed | what the player does |
//! |---|---|
//! | a step (note, instrument, effects, rows inserted, deleted, pasted, transposed) | [`AhxPlayer::set_step`]: read when the song reaches the row; the row playing now is not retriggered |
//! | a position's track or transpose | [`AhxPlayer::set_position_entry`]: refreshed at the next row |
//! | the restart position | [`AhxPlayer::set_restart`] |
//! | an instrument's fields, envelope, sweeps, vibrato or PList | [`AhxPlayer::replace_instrument`]: a voice already holding it picks up its PList rows and the envelope's stage lengths and levels at once; what a trigger copies onto the voice (volume, wave length, vibrato, hard cut, PList speed, the filter and square sweep set-up) from the voice's next trigger |
//! | a track added at the end (a cell given a track of its own) | [`AhxPlayer::push_track`] (undoing it: `pop_track`) |
//! | an instrument added at the end | [`AhxPlayer::push_instrument`] (undoing it: `pop_instrument`) |
//! | a rename or the song's name | nothing: names make no sound |
//!
//! and `None` for anything that moves the song's shape: the track length, tracks
//! compacted, positions inserted, deleted or moved, the speed multiplier, the HVL
//! mix gain and stereo preset, the subsong list, or more than [`MAX_STEPS`] steps
//! at once. Those need a new song: build the edited song
//! ([`AhxPlayer::from_song`]) and swap it in, or let the player do it in place
//! with [`AhxPlayer::reload_song`], carrying the playhead through
//! [`PositionMap::apply`](super::PositionMap::apply).
//!
//! [`AhxDoc::apply_live`] hands a `LiveEdits` to a player in the order that
//! works (tracks and instruments added or removed, then steps, positions, the
//! restart, instrument fields). None of it allocates on the audio thread except
//! the tracks and instruments it appends, so a host calls it from a message
//! handler. With hi-fi on, a step that names an instrument owes the player a walk
//! of the song for the wave tables the instrument reaches: it is paid by
//! `finish_instrument_edits`, which `apply_live` calls last.

use serde_json::{json, Value};

use super::history::Changes;
use super::json::step_json;
use super::limits::Res;
use super::AhxDoc;
use crate::format::Step;
use crate::player::AhxPlayer;

/// One step to put in a playing song.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepEdit {
    pub track: usize,
    pub row: usize,
    /// The step as it is now.
    pub step: Step,
}

/// One position entry to put in a playing song.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionEdit {
    pub position: usize,
    pub channel: usize,
    pub track: u8,
    pub transpose: i8,
}

/// What a playing song needs to follow an edit (see the module docs). Everything
/// is the document's content now, so applying it brings a player that held the
/// song before the edit level with the document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveEdits {
    /// The song's track count after the edit: a player with more takes the extra
    /// tracks off the end.
    pub track_count: usize,
    /// Tracks appended, in order, as they are now (a player with fewer tracks
    /// than `track_count - new_tracks.len()` is out of step).
    pub new_tracks: Vec<Vec<Step>>,
    /// The song's instrument count after the edit.
    pub instrument_count: usize,
    /// The numbers of instruments appended, in order.
    pub new_instruments: Vec<usize>,
    pub steps: Vec<StepEdit>,
    pub positions: Vec<PositionEdit>,
    /// Instruments that existed before and changed in a way that sounds.
    pub instruments: Vec<usize>,
    pub restart: Option<usize>,
}

impl LiveEdits {
    /// Whether there is nothing to do.
    pub fn is_empty(&self) -> bool {
        self.new_tracks.is_empty()
            && self.new_instruments.is_empty()
            && self.steps.is_empty()
            && self.positions.is_empty()
            && self.instruments.is_empty()
            && self.restart.is_none()
    }

    /// `{"track_count", "new_tracks": [[step per row]], "instrument_count",
    /// "new_instruments": [n], "steps": [{"track", "row", "cell"}], "positions":
    /// [{"position", "channel", "track", "transpose"}], "instruments": [n],
    /// "restart"}`.
    pub fn to_json(&self) -> Value {
        json!({
            "track_count": self.track_count,
            "new_tracks": self.new_tracks.iter()
                .map(|t| Value::Array(t.iter().map(step_json).collect()))
                .collect::<Vec<_>>(),
            "instrument_count": self.instrument_count,
            "new_instruments": self.new_instruments,
            "steps": self.steps.iter()
                .map(|s| json!({"track": s.track, "row": s.row, "cell": step_json(&s.step)}))
                .collect::<Vec<_>>(),
            "positions": self.positions.iter()
                .map(|p| json!({
                    "position": p.position, "channel": p.channel,
                    "track": p.track, "transpose": p.transpose,
                }))
                .collect::<Vec<_>>(),
            "instruments": self.instruments,
            "restart": self.restart,
        })
    }
}

impl AhxDoc {
    /// The last edit, undo or redo as live edits for a playing song: `Some` when
    /// the change was only what a player can take (see the module docs), `None`
    /// when the edited song must be opened again. A group's changes (a `batch`,
    /// or edits between `begin_group` and `end_group`) are what the last edit
    /// call changed, so a host that wants a whole group asks after the last one
    /// with [`live_edits_for`](Self::live_edits_for) and the merged changes.
    pub fn live_edits(&self) -> Option<LiveEdits> {
        self.live_edits_for(&self.last)
    }

    /// [`live_edits`](Self::live_edits) for a [`Changes`] this song's edits
    /// returned.
    pub fn live_edits_for(&self, changes: &Changes) -> Option<LiveEdits> {
        if !changes.is_live() {
            return None;
        }
        let s = &self.song;
        let (tracks, instruments) = (s.tracks.len(), s.instruments.len() - 1);
        let tracks_before = (tracks as i64 - changes.tracks_net as i64).max(1) as usize;
        let instruments_before =
            (instruments as i64 - changes.instruments_net as i64).max(0) as usize;
        let mut live = LiveEdits {
            track_count: tracks,
            instrument_count: instruments,
            ..LiveEdits::default()
        };
        if tracks > tracks_before {
            live.new_tracks = s.tracks[tracks_before..].to_vec();
        }
        if instruments > instruments_before {
            live.new_instruments = (instruments_before + 1..=instruments).collect();
        }
        // Steps of tracks that were appended travel whole; those of tracks that
        // are gone are not there.
        let existing = tracks.min(tracks_before);
        for &(t, r) in &changes.steps {
            if t < existing {
                if let Some(step) = s.tracks[t].get(r) {
                    live.steps.push(StepEdit {
                        track: t,
                        row: r,
                        step: *step,
                    });
                }
            }
        }
        for &(p, ch) in &changes.position_cells {
            if let Some(entry) = s.positions.get(p) {
                if ch < entry.track.len() {
                    live.positions.push(PositionEdit {
                        position: p,
                        channel: ch,
                        track: entry.track[ch],
                        transpose: entry.transpose[ch],
                    });
                }
            }
        }
        live.instruments = changes
            .sounding
            .iter()
            .copied()
            .filter(|&n| n <= instruments_before.min(instruments))
            .collect();
        if changes.restart {
            live.restart = Some(s.restart as usize);
        }
        Some(live)
    }

    /// Hands `edits` to a player that holds the song as it was before the edit
    /// (see the module docs for the order and what allocates). An error names
    /// what the player refused; a player that is out of step with the document
    /// (a different song) is refused like that and the edits already applied stay.
    pub fn apply_live(&self, player: &mut AhxPlayer, edits: &LiveEdits) -> Res<()> {
        let refused = |what: String| -> Res<()> { Err(format!("the player refused {what}")) };
        while player.track_count() > edits.track_count {
            if !player.pop_track() {
                return refused("to drop a track".into());
            }
        }
        for t in &edits.new_tracks {
            let bytes: Vec<u8> = t
                .iter()
                .flat_map(|c| [c.note, c.instrument, c.fx, c.fx_param, c.fxb, c.fxb_param])
                .collect();
            if !player.push_track(&bytes) {
                return refused("a new track".into());
            }
        }
        while player.instrument_count() > edits.instrument_count {
            if !player.pop_instrument() {
                return refused("to drop an instrument".into());
            }
        }
        for &n in &edits.new_instruments {
            player.push_instrument(&self.instrument_bytes(n)?)?;
        }
        for s in &edits.steps {
            let c = s.step;
            if !player.set_step(
                s.track,
                s.row,
                c.note,
                c.instrument,
                c.fx,
                c.fx_param,
                c.fxb,
                c.fxb_param,
            ) {
                return refused(format!("a step at track {}, row {}", s.track, s.row));
            }
        }
        for p in &edits.positions {
            if !player.set_position_entry(p.position, p.channel, p.track, p.transpose as i32) {
                return refused(format!("position {}, channel {}", p.position, p.channel));
            }
        }
        if let Some(r) = edits.restart {
            if !player.set_restart(r) {
                return refused(format!("restart position {r}"));
            }
        }
        for &n in &edits.instruments {
            player.replace_instrument_deferred(n, &self.instrument_bytes(n)?)?;
        }
        player.finish_instrument_edits();
        Ok(())
    }
}
