//! `AhxPlayer`: the wasm-facing shell around [`AhxEngine`] (P4).
//!
//! The engine renders interleaved `i16` stereo, exactly what `hvl_DecodeFrame`
//! writes, because that is what the bit-exact goldens compare. An
//! `AudioWorkletProcessor` wants planar `f32` in `[-1, 1)`, one channel array
//! per output. This module is that adapter and nothing else: no DSP, no
//! transport decisions, so the bit-exactness proven in `tests/` is untouched
//! (the adapter is tested against the engine's own `i16` stream).
//!
//! The wasm-bindgen attribute is `cfg_attr`-gated on the `wasm` feature the
//! same way `audio_engine/wasm.rs` does it, so the whole class also compiles
//! and is unit-tested natively.

use super::engine::{AhxEngine, SeekKind};
use super::format;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

/// Full-scale divisor for `i16 -> f32`. `-32768` maps to exactly `-1.0` and
/// `32767` to just under `1.0`, so the conversion is lossless in `f32` and
/// never exceeds full scale.
const I16_SCALE: f32 = 1.0 / 32768.0;

/// `i16 -> f32`, `[-32768, 32767] -> [-1.0, 32767/32768]`.
#[inline]
pub fn i16_to_f32(s: i16) -> f32 {
    s as f32 * I16_SCALE
}

/// De-interleaves `interleaved` (`L, R, L, R, ...`) into `left`/`right`,
/// converting to `f32` and applying `gain`. Processes
/// `min(interleaved.len() / 2, left.len(), right.len())` frames and returns
/// that count; anything beyond it in `left`/`right` is left untouched.
pub fn deinterleave_to_f32(
    interleaved: &[i16],
    left: &mut [f32],
    right: &mut [f32],
    gain: f32,
) -> usize {
    let frames = (interleaved.len() / 2).min(left.len()).min(right.len());
    for i in 0..frames {
        left[i] = i16_to_f32(interleaved[2 * i]) * gain;
        right[i] = i16_to_f32(interleaved[2 * i + 1]) * gain;
    }
    frames
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct AhxPlayer {
    engine: AhxEngine,
    /// Reused across `render` calls; grows once to the largest block seen
    /// (the audio thread is not supposed to allocate in steady state).
    scratch: Vec<i16>,
    gain: f32,
    playing: bool,
    /// Edits made through [`replace_instrument_deferred`](AhxPlayer::replace_instrument_deferred)
    /// that may reach a table the bank lacks: the walk they owe, paid by
    /// [`finish_instrument_edits`](AhxPlayer::finish_instrument_edits).
    prewarm_owed: bool,
}

#[cfg_attr(feature = "wasm", wasm_bindgen)]
impl AhxPlayer {
    /// Parses an AHX (`THX`) or HVL file and builds a paused player.
    /// `stereo_mode` (0..=4) is AHX's stereo-separation setting; HVL files
    /// carry their own. The channel count follows the song: 4 for AHX, the
    /// header's native count for HVL, see [`channels`](Self::channels).
    #[cfg_attr(feature = "wasm", wasm_bindgen(constructor))]
    pub fn new(bytes: &[u8], sample_rate: u32, stereo_mode: u8) -> Result<AhxPlayer, String> {
        let song = format::parse(bytes).map_err(|e| e.to_string())?;
        let engine = AhxEngine::new(song, sample_rate, stereo_mode).map_err(|e| e.to_string())?;
        Ok(AhxPlayer {
            engine,
            scratch: Vec::new(),
            gain: 1.0,
            playing: false,
            prewarm_owed: false,
        })
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Stops rendering (the output is silence) without losing the position.
    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Back to the start of `subsong` (0 = the main song), paused. `false`
    /// for an out-of-range subsong, in which case nothing changes.
    pub fn restart(&mut self, subsong: usize) -> bool {
        let ok = self.engine.init_subsong(subsong);
        if ok {
            self.playing = false;
        }
        ok
    }

    /// Moves to `row` of `position` (an index into the song's position list),
    /// keeping the play/pause state: a playing song carries on from there, a
    /// paused one waits there. The next sample rendered is the first of that
    /// row. Returns 0 for a position or row out of range (nothing changes), 1
    /// when the song's own flow reaches it (every voice is exactly as if the
    /// song had played to there, see [`AhxEngine::seek`]), 2 when it never
    /// does and the row starts cold.
    pub fn seek(&mut self, position: usize, row: usize) -> u8 {
        match self.engine.seek(position, row) {
            None => 0,
            Some(SeekKind::Exact) => 1,
            Some(SeekKind::Cold) => 2,
        }
    }

    /// Loop the current position instead of moving on from it; see
    /// [`AhxEngine::set_loop_position`]. Kept across `restart` and `seek`.
    pub fn set_loop_position(&mut self, on: bool) {
        self.engine.set_loop_position(on);
    }

    pub fn loop_position(&self) -> bool {
        self.engine.loop_position()
    }

    /// Live (keyboard preview) mode: this player stops playing the song and
    /// is played by [`preview_note_on`](Self::preview_note_on) /
    /// [`preview_note_off`](Self::preview_note_off) instead, one mono voice
    /// with the song's instruments (see [`AhxEngine::enable_live`]). It starts
    /// rendering at once (a preview has no transport to `play`) and there is no
    /// way back: make a separate player for the song. Turning hi-fi on after
    /// this builds its tables lazily instead of walking the song, which a
    /// preview never plays.
    pub fn enable_preview(&mut self) {
        self.engine.enable_live();
        self.playing = true;
    }

    pub fn preview_enabled(&self) -> bool {
        self.engine.live_enabled()
    }

    /// Replaces instrument `instrument` (1-based, as a pattern step numbers it)
    /// of the loaded song with the one in `bytes`: the 22-byte instrument core
    /// followed by its PList entries in the song's own layout (4 bytes each in
    /// AHX, 5 in HVL), no name, exactly as long as its length byte says. The
    /// bytes are decoded by the file loader's own functions
    /// ([`format::parse_instrument`]), the name is kept, and the song's
    /// instrument list is what changes: a song player plays the new instrument
    /// from its next trigger (a voice already holding it also picks up PList and
    /// envelope changes at once, see [`AhxEngine::replace_instrument`]) and a
    /// preview player from its next note-on. Nothing is reloaded and the
    /// transport does not move.
    ///
    /// With hi-fi on, a song player rebuilds the tables the edited song asks
    /// for before this returns (see [`AhxEngine::prewarm_hifi_after_edit`]) --
    /// unless the edit reaches no table (volume, envelope, hard cut) or the
    /// instrument is one no step ever triggers, which costs nothing; a preview player only forgets what it prewarmed for that
    /// instrument. Both happen here, in the caller's message handler, never in
    /// `render`.
    ///
    /// An error, with the song untouched, for bytes the format does not decode
    /// to one instrument or an `instrument` the song does not have.
    pub fn replace_instrument(&mut self, instrument: usize, bytes: &[u8]) -> Result<(), String> {
        self.replace_instrument_deferred(instrument, bytes)?;
        self.finish_instrument_edits();
        Ok(())
    }

    /// [`replace_instrument`](Self::replace_instrument) without the hi-fi walk:
    /// the instrument is swapped now, and any walk it owes waits for
    /// [`finish_instrument_edits`](Self::finish_instrument_edits). A burst of
    /// edits (the editor sends what has piled up in one message) is then one
    /// walk of the song instead of one per instrument.
    pub fn replace_instrument_deferred(
        &mut self,
        instrument: usize,
        bytes: &[u8],
    ) -> Result<(), String> {
        let song = self.engine.song();
        let ins = format::parse_instrument(bytes, song.format, song.version, String::new())
            .map_err(|e| e.to_string())?;
        let Some(tables_may_differ) = self.engine.replace_instrument(instrument, ins) else {
            return Err(format!("the song has no instrument {instrument}"));
        };
        // Only an edit that can reach another wave table costs a walk of the
        // song: volume, envelope and hard-cut edits change no table (see
        // `engine::same_tables`), and an instrument no step ever triggers can
        // never ask for one (`AhxEngine::instrument_is_triggered`).
        if tables_may_differ
            && self.engine.hifi_enabled()
            && !self.engine.live_enabled()
            && self.engine.instrument_is_triggered(instrument)
        {
            self.prewarm_owed = true;
        }
        Ok(())
    }

    /// Pays the walk that deferred edits owe (nothing when none does): one
    /// [`AhxEngine::prewarm_hifi_after_edit`] however many instruments changed.
    /// Call it from the message handler, never from `render`.
    pub fn finish_instrument_edits(&mut self) {
        if std::mem::take(&mut self.prewarm_owed)
            && self.engine.hifi_enabled()
            && !self.engine.live_enabled()
        {
            self.engine.prewarm_hifi_after_edit();
        }
    }

    /// Instruments the song has (1-based numbering runs `1..=instrument_count`).
    pub fn instrument_count(&self) -> usize {
        self.engine.song().instrument_nr as usize
    }

    /// Ticks a note-on's prewarm holds a key down for `instrument` (1-based)
    /// before releasing it, bounded by what the instrument can produce (see
    /// [`live_warm_hold_ticks`](super::engine::live_warm_hold_ticks)); 0 for an
    /// instrument the song does not have. Diagnostics.
    pub fn preview_warm_hold_ticks(&self, instrument: usize) -> u32 {
        self.engine
            .live_warm_hold_ticks_for(instrument)
            .unwrap_or(0)
    }

    /// Plays `instrument` (1-based) at `note` (1..=60, the AHX pitch table's
    /// index) with `velocity` (0..=127), retriggering the voice on the next
    /// tick. With hi-fi on, builds the tables this note will want first, so
    /// call it from a message handler and not from the render callback. `false`, changing nothing, outside preview mode or for an
    /// instrument the song does not have.
    pub fn preview_note_on(&mut self, instrument: usize, note: i32, velocity: u32) -> bool {
        self.engine.live_note_on(instrument, note, velocity)
    }

    /// Releases the previewed note (the instrument's release, or its hard cut).
    pub fn preview_note_off(&mut self) {
        self.engine.live_note_off();
    }

    /// The PList row the previewed note's voice ran last, `-1` when there is
    /// none (see [`AhxEngine::live_plist_state`]). A scalar, not a `Vec`: the
    /// worklet reads it every render quantum.
    pub fn preview_plist_row(&self) -> i32 {
        self.engine
            .live_plist_state()
            .map_or(-1, |(_, row)| row as i32)
    }

    /// The instrument (1-based) that row belongs to, `0` when there is none.
    pub fn preview_plist_instrument(&self) -> u32 {
        self.engine
            .live_plist_state()
            .map_or(0, |(instrument, _)| instrument as u32)
    }

    /// Keep the wave phase across instrument triggers (the 68k behaviour)
    /// instead of restarting it at 0; see
    /// [`AhxEngine::set_continue_phase_on_trigger`]. Off by default, so a bare
    /// `AhxPlayer` renders the reference goldens; the app's worklet turns it
    /// on for every song it loads.
    pub fn set_continue_phase_on_trigger(&mut self, on: bool) {
        self.engine.set_continue_phase_on_trigger(on);
    }

    pub fn continue_phase_on_trigger(&self) -> bool {
        self.engine.continue_phase_on_trigger()
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Linear output gain, clamped to `[0, 2]`; NaN is ignored.
    pub fn set_gain(&mut self, gain: f32) {
        if !gain.is_nan() {
            self.gain = gain.clamp(0.0, 2.0);
        }
    }

    /// Fills `left`/`right` with planar `f32` and returns the number of
    /// frames written (`min(left.len(), right.len())`). While paused the
    /// engine does not advance and the output is zeroed.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        let frames = left.len().min(right.len());
        if !self.playing {
            left[..frames].fill(0.0);
            right[..frames].fill(0.0);
            return frames;
        }
        if self.scratch.len() < frames * 2 {
            self.scratch.resize(frames * 2, 0);
        }
        self.engine.render_block(&mut self.scratch[..frames * 2]);
        deinterleave_to_f32(&self.scratch[..frames * 2], left, right, self.gain)
    }

    /// Per-voice waveform capture for oscilloscopes; off by default and
    /// bit-neutral to the mix (see [`AhxEngine::enable_capture`]).
    pub fn enable_capture(&mut self, on: bool) {
        self.engine.enable_capture(on);
    }

    pub fn capture_enabled(&self) -> bool {
        self.engine.capture_enabled()
    }

    /// Band-limited ("hi-fi") oscillators instead of the reference's aliasing
    /// ones; see [`AhxEngine::set_hifi`]. Off by default here, and off is the
    /// reference render byte for byte. The setting belongs to this player and
    /// is kept across `rewind`.
    ///
    /// Turning it on *prewarms* a song player (see
    /// [`AhxEngine::prewarm_hifi`]): every table the song needs is built before
    /// this returns, so `render` never builds one. That blocks the caller for
    /// the duration (measured in `tests/ahx_hifi.rs`); call it before `play`,
    /// or accept one hiccup. A *preview* player has no song to walk: its bank
    /// starts empty and locked, and each
    /// [`preview_note_on`](Self::preview_note_on) prewarms the pressed
    /// instrument at the pressed pitch, in that call, so `render` still never
    /// builds -- a table the note reaches that the prewarm did not is a miss
    /// ([`hifi_miss_count`](Self::hifi_miss_count)), degraded for that tick.
    pub fn set_hifi(&mut self, on: bool) {
        let was = self.engine.hifi_enabled();
        self.engine.set_hifi(on);
        if on && !was && !self.engine.live_enabled() {
            self.engine.prewarm_hifi();
        }
    }

    pub fn hifi_enabled(&self) -> bool {
        self.engine.hifi_enabled()
    }

    /// Mip tables cached (0 with hi-fi off); diagnostics.
    pub fn hifi_table_count(&self) -> usize {
        self.engine.hifi_table_count()
    }

    /// Lookups since the prewarm that the exact table could not serve. Zero:
    /// the render thread built nothing and degraded nowhere; diagnostics.
    pub fn hifi_miss_count(&self) -> f64 {
        self.engine.hifi_misses() as f64
    }

    /// Whether the render path is locked out of building tables.
    pub fn hifi_locked(&self) -> bool {
        self.engine.hifi_locked()
    }

    /// Live per-voice mute and solo as bit masks (bit `i` = voice `i`); see
    /// [`AhxEngine::set_mute_solo`]. The state belongs to this player and is
    /// kept across `rewind`; all zero (the default) leaves the mix untouched.
    pub fn set_mute_solo(&mut self, mute: u32, solo: u32) {
        self.engine.set_mute_solo(mute, solo);
    }

    /// Fills `out` with `voice`'s latest waveform (oldest first, `i16`, full
    /// scale `+-8192`) and returns the number of points written; 0 when
    /// capture is off or `voice` is out of range. Reuses the caller's buffer,
    /// so a per-report call allocates nothing on the Rust side.
    pub fn read_channel_snapshot(&self, voice: usize, out: &mut [i16]) -> usize {
        self.engine.read_channel_snapshot(voice, out)
    }

    pub fn sample_rate(&self) -> u32 {
        self.engine.sample_rate()
    }

    pub fn channels(&self) -> usize {
        self.engine.channels()
    }

    /// Song channels the engine does not play: 0 for every real file (only a
    /// malformed HVL wider than the reference's 16-voice array is cut).
    pub fn dropped_channels(&self) -> usize {
        self.engine.dropped_channels()
    }

    pub fn position(&self) -> i32 {
        self.engine.pos_nr()
    }

    pub fn row(&self) -> i32 {
        self.engine.note_nr()
    }

    /// Ticks per row (the song's current speed).
    pub fn tempo(&self) -> i32 {
        self.engine.tempo()
    }

    /// Ticks played so far (50 Hz x speed multiplier).
    pub fn ticks(&self) -> u32 {
        self.engine.ticks_played()
    }

    /// Set once the song has reached its end (it then loops from its restart
    /// position, as the reference does); cleared by [`restart`](Self::restart).
    pub fn song_end_reached(&self) -> bool {
        self.engine.song_end_reached()
    }

    pub fn song_name(&self) -> String {
        self.engine.song().name.clone()
    }

    pub fn position_count(&self) -> usize {
        self.engine.song().position_nr
    }

    pub fn track_length(&self) -> usize {
        self.engine.song().track_length as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i16_conversion_hits_the_full_scale_endpoints_exactly() {
        assert_eq!(i16_to_f32(i16::MIN), -1.0);
        assert_eq!(i16_to_f32(0), 0.0);
        assert_eq!(i16_to_f32(16384), 0.5);
        assert!(i16_to_f32(i16::MAX) < 1.0);
        assert_eq!(i16_to_f32(i16::MAX), 32767.0 / 32768.0);
    }

    #[test]
    fn deinterleave_splits_channels_and_applies_gain() {
        let src = [100i16, -200, 300, -400, 500, -600];
        let (mut l, mut r) = ([9.0f32; 3], [9.0f32; 3]);
        assert_eq!(deinterleave_to_f32(&src, &mut l, &mut r, 1.0), 3);
        assert_eq!(l, [100.0 / 32768.0, 300.0 / 32768.0, 500.0 / 32768.0]);
        assert_eq!(r, [-200.0 / 32768.0, -400.0 / 32768.0, -600.0 / 32768.0]);
        deinterleave_to_f32(&src, &mut l, &mut r, 0.5);
        assert_eq!(l[0], 50.0 / 32768.0);
    }

    #[test]
    fn deinterleave_stops_at_the_shortest_buffer_and_leaves_the_rest() {
        let src = [1i16, 2, 3, 4, 5, 6];
        let (mut l, mut r) = ([7.0f32; 4], [7.0f32; 2]);
        assert_eq!(deinterleave_to_f32(&src, &mut l, &mut r, 1.0), 2);
        assert_eq!(l[2], 7.0);
        assert_eq!(deinterleave_to_f32(&src[..1], &mut l, &mut r, 1.0), 0);
    }
}
