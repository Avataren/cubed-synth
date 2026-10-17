//! `SidPlayer`: the wasm-facing shell around [`SidSongPlayer`] (S4 of
//! `.ai/plan-sid-tracking.md`), what the app's SID worklet
//! (`src/audio/worklets/sid-core.ts`) drives.
//!
//! It adds the transport state a worklet needs (play/pause, a gain) and
//! nothing else: every sound decision is the player's and the chip's. The
//! chip renders mono; the worklet copies it to both sides of its stereo
//! output and hands the three voice taps to their own outputs (the tracker's
//! per-track scopes and spectrum).
//!
//! The wasm-bindgen attribute is `cfg_attr`-gated on the `wasm` feature, as
//! `ahx/player.rs` does it, so the class also compiles and is unit-tested
//! natively.

use super::chip::ALL_VOICES;
use super::revision::DieRevision;
use super::song::SidSong;
use super::{SidModel, SidSongPlayer, GT_NOTE_COUNT};
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct SidPlayer {
    player: SidSongPlayer,
    playing: bool,
    gain: f32,
    mute: u32,
    solo: u32,
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
impl SidPlayer {
    /// Parses a SID song file (`ASID`, what the app's `SidDoc` saves) and
    /// builds a paused player for subsong 0 on a chip of the song's model.
    #[cfg_attr(feature = "wasm", wasm_bindgen(constructor))]
    pub fn new(bytes: &[u8], sample_rate: f64) -> Result<SidPlayer, String> {
        let song = SidSong::parse(bytes).map_err(|e| e.to_string())?;
        let player = SidSongPlayer::new(song, sample_rate).map_err(|e| e.to_string())?;
        Ok(SidPlayer {
            player,
            playing: false,
            gain: 1.0,
            mute: 0,
            solo: 0,
        })
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Stops rendering (the output is silence) without losing the position.
    pub fn pause(&mut self) {
        self.playing = false;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Moves to the start of song row `row`, keeping the play/pause state
    /// (`SidSongPlayer::seek_row`).
    pub fn seek_row(&mut self, row: u32) {
        self.player.seek_row(row as u64);
    }

    /// Loops song rows `start..end` (`end` exclusive; `end <= start` plays on).
    pub fn set_loop_rows(&mut self, start: u32, end: u32) {
        self.player.set_loop_rows(Some((start as u64, end as u64)));
    }

    pub fn clear_loop_rows(&mut self) {
        self.player.set_loop_rows(None);
    }

    pub fn set_gain(&mut self, gain: f32) {
        self.gain = if gain.is_finite() { gain.max(0.0) } else { 1.0 };
    }

    /// Bit masks, bit `i` = voice `i`: muted voices, and (when non-zero) the
    /// only voices heard. Applied to the chip's voice mask.
    pub fn set_mute_solo(&mut self, mute: u32, solo: u32) {
        self.mute = mute;
        self.solo = solo;
        let heard = if solo & ALL_VOICES as u32 != 0 {
            solo
        } else {
            ALL_VOICES as u32
        };
        self.player.chip_mut().set_voice_mask((heard & !mute) as u8);
    }

    /// Fills `out` with the mix and `v0`..`v2` with the three voices' taps;
    /// silence (all four) while paused. Every buffer must be as long as `out`.
    /// Returns the frames written.
    pub fn render(
        &mut self,
        out: &mut [f32],
        v0: &mut [f32],
        v1: &mut [f32],
        v2: &mut [f32],
    ) -> usize {
        let n = out.len().min(v0.len()).min(v1.len()).min(v2.len());
        if !self.playing {
            out[..n].fill(0.0);
            v0[..n].fill(0.0);
            v1[..n].fill(0.0);
            v2[..n].fill(0.0);
            return n;
        }
        self.player
            .render_taps(&mut out[..n], [&mut v0[..n], &mut v1[..n], &mut v2[..n]]);
        if self.gain != 1.0 {
            for buffer in [&mut out[..n], &mut v0[..n], &mut v1[..n], &mut v2[..n]] {
                for s in buffer.iter_mut() {
                    *s *= self.gain;
                }
            }
        }
        n
    }

    /// The row being played, counted from the top of the song.
    pub fn song_row(&self) -> u32 {
        self.player.song_row().min(u32::MAX as u64) as u32
    }

    /// The song's length in rows (the longest channel's first pass).
    pub fn song_rows(&self) -> u32 {
        self.player.song_rows().min(u32::MAX as u64) as u32
    }

    /// Whether the player has played past the song's last row.
    pub fn song_end_reached(&self) -> bool {
        self.player.song_rows() > 0 && self.player.song_row() >= self.player.song_rows()
    }

    /// Ticks per row now (the song's start tempo, or what an F command set).
    pub fn tempo(&self) -> u32 {
        self.player.tempo() as u32
    }

    pub fn channels(&self) -> u32 {
        super::song::SID_CHANNELS as u32
    }

    /// `"8580"` or `"6581"`: the chip this player built from the song's tag.
    pub fn chip_model(&self) -> String {
        match self.player.chip().model() {
            SidModel::Sid8580 => "8580".to_string(),
            SidModel::Sid6581 => "6581".to_string(),
        }
    }

    /// Switch the chip to the 8580 (`true`) or the 6581 from the next sample on,
    /// without a reload: the song keeps its place and sounding notes carry on.
    pub fn set_chip_model(&mut self, model_8580: bool) {
        let model = if model_8580 {
            SidModel::Sid8580
        } else {
            SidModel::Sid6581
        };
        // Both models construct, so this cannot fail.
        let _ = self.player.chip_mut().set_model(model);
    }

    /// Play the 6581 as revision `name` (`DieRevision::name`: "gt", "r2",
    /// "r3", "r4", "r4ar") from the next sample on, without a reload. An 8580
    /// song keeps its chip. `false` for an unknown name, which changes nothing.
    pub fn set_revision(&mut self, name: &str) -> bool {
        match DieRevision::from_name(name) {
            Some(r) => {
                self.player.chip_mut().set_profile(r.profile());
                true
            }
            None => false,
        }
    }

    /// The name of the 6581 revision the chip plays (`set_revision`).
    pub fn revision(&self) -> String {
        self.player.chip().profile().revision.name().to_string()
    }

    /// A voice tap's full scale (`Chip::tap_full_scale`) at gain 1.0.
    pub fn tap_full_scale(&self) -> f64 {
        self.player.chip().tap_full_scale()
    }

    pub fn instrument_count(&self) -> u32 {
        self.player.song().instruments.len() as u32
    }

    /// Keyboard-preview mode: the song no longer plays; `preview_note_on`
    /// sounds its instruments on one voice. Renders at once.
    pub fn enable_preview(&mut self) {
        self.player.set_preview(true);
        self.playing = true;
    }

    /// Preview mode: `instrument` (1-based) at note table index `note`
    /// (0 = C-0 .. 92 = G#7). `false` outside preview or for a missing instrument.
    pub fn preview_note_on(&mut self, instrument: u32, note: u32) -> bool {
        self.player.preview_note_on(
            instrument as usize,
            note.min(GT_NOTE_COUNT as u32 - 1) as u8,
        )
    }

    pub fn preview_note_off(&mut self) {
        self.player.preview_note_off();
    }
}

impl SidPlayer {
    /// The player underneath (tests and native callers).
    pub fn inner(&self) -> &SidSongPlayer {
        &self.player
    }

    pub fn mute_solo(&self) -> (u32, u32) {
        (self.mute, self.solo)
    }
}
