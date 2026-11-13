//! SID (GoatTracker) songs under live control: a [`SongPlayer`] over `cubed_sid`'s
//! `SidSongPlayer`.
//!
//! A song is three voices (the channels) with an orderlist each over one pool of patterns.
//! `SidSongPlayer` has no pause, so **stopped is its preview mode** (the sequencer is off, the notes
//! die away, the position stays, and auditions play on any voice); playing leaves it and the voices
//! read their rows again. The three voices run on orderlists of their own, so each has its own
//! position: the status carries every voice's (`SongStatus::voices`: orderlist position, pattern and
//! row, as the voice last started them) next to the old fields, which stay voice 0's. `Seek { order,
//! row }` is voice 0 reading row `row` of the pattern at orderlist position `order`, and
//! `SeekVoice { voice, order, row }` is the same for any voice (the others stand where the song has
//! them when that voice gets there). `Loop` repeats rows of voice 0's entries (and every voice goes
//! back with it); `VoiceLoop { voice, range }` repeats entries of one voice's orderlist alone while
//! the others play on (see `SidSongPlayer::set_voice_loop` for what is exact).
//!
//! The preview (`PreviewOn`) is the player's audition on voice 0, held until `PreviewOff` or for
//! `duration` frames; over a playing song voice 0 is taken for it. Note numbers are MIDI (the song's
//! note table index + 12).
//!
//! Live edits on the wire: `SetCell` (a row: `pattern` is the pattern number, `note` the note
//! 1-93 / 126 off / 127 on, `effect` the command, `param`), `SetInstrument` (see
//! [`SongInstrument`](crate::song::SongInstrument): the ten bytes the player takes) and
//! `SetTableRow`. A pattern's length, the orderlists, the instrument list, a table's length, the
//! start tempo, the speed multiplier and the chip model are a new song, swapped in by the host.
//!
//! Levels are the voices' peaks (the taps, at the mix's scale); the scopes are the voices' own
//! signals (before the filter), and cost one copy of three taps a block while on. The song is mono,
//! and the chip's output carries a constant offset while its volume register is up, silence included.

use cubed_sid::player::{SidLoop, SidSongPlayer};
use cubed_sid::song::{Instrument, LiveEdit, Row, SidSong, TableKind, TableRow};

use super::player::{SongPlayer, CHUNK};
use crate::song::{SongControl, SongLoop, SongStatus, VoicePos, SONG_MAX_VOICES};

/// The voice the preview plays on, whose position the song's old status fields report, and
/// that `Seek` and `Loop` refer to.
const VOICE: usize = 0;
/// MIDI note 12 is the note table's index 0 (C-0).
const MIDI_OF_INDEX_0: u8 = 12;

/// A SID song. See the module docs.
pub struct SidPlayer {
    player: SidSongPlayer,
    scopes: bool,
    /// The voices' own signals in the block just rendered (when the scopes are on).
    taps: [[f32; CHUNK]; 3],
    block: usize,
    /// Frames a timed preview note has left (0: held, or none).
    preview_left: u32,
}

impl SidPlayer {
    /// A stopped player of subsong `subsong` of `song`. Build it off the audio thread.
    pub fn new(song: SidSong, sample_rate: f32, subsong: usize) -> Result<SidPlayer, String> {
        let model = song.model;
        let mut player = SidSongPlayer::with_model(song, model, sample_rate as f64, subsong)
            .map_err(|e| e.to_string())?;
        player.set_preview(true);
        Ok(SidPlayer {
            player,
            scopes: false,
            taps: [[0.0; CHUNK]; 3],
            block: 0,
            preview_left: 0,
        })
    }

    /// The player (tests).
    pub fn player(&self) -> &SidSongPlayer {
        &self.player
    }

    fn playing(&self) -> bool {
        !self.player.preview()
    }

    /// Moves `voice` to `order` and `row` of its orderlist, also while stopped: the replay needs
    /// the sequencer, which preview mode (stopped) switches off. A place the song does not have
    /// leaves everything as it was.
    fn seek_voice(&mut self, voice: usize, order: u16, row: u16) {
        let subsong = self.player.subsong();
        let stopped = self.player.preview();
        if stopped {
            self.player.set_preview(false);
        }
        let _ = self
            .player
            .seek(subsong, voice, order as usize, row as usize);
        if stopped {
            self.player.set_preview(true);
        }
    }

    fn set_masks(&mut self, f: impl FnOnce(u8, u8) -> (u8, u8)) {
        let (mute, solo) = self.player.mute_solo();
        let (mute, solo) = f(mute, solo);
        self.player.set_mute_solo(mute, solo);
    }
}

fn table_kind(table: u8) -> Option<TableKind> {
    TableKind::ALL.get(table as usize).copied()
}

impl SongPlayer for SidPlayer {
    fn channels(&self) -> usize {
        3
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len()).min(CHUNK);
        if self.scopes {
            let [t0, t1, t2] = &mut self.taps;
            self.player
                .render_taps(&mut left[..n], [&mut t0[..n], &mut t1[..n], &mut t2[..n]]);
        } else {
            self.player.render(&mut left[..n]);
        }
        right[..n].copy_from_slice(&left[..n]);
        self.block = n;
        if self.preview_left > 0 {
            self.preview_left = self.preview_left.saturating_sub(n as u32);
            if self.preview_left == 0 {
                self.player.audition_off(VOICE);
            }
        }
    }

    fn ended(&self) -> bool {
        self.playing() && self.player.looped()
    }

    fn control(&mut self, control: SongControl) {
        match control {
            SongControl::Play => {
                if self.player.preview() {
                    self.player.set_preview(false);
                }
            }
            SongControl::Stop => {
                if !self.player.preview() {
                    self.player.set_preview(true);
                    self.preview_left = 0;
                }
            }
            SongControl::Seek { order, row } => self.seek_voice(VOICE, order, row),
            SongControl::SeekVoice { voice, order, row } => {
                self.seek_voice(voice as usize, order, row)
            }
            SongControl::Loop(repeat) => match repeat {
                SongLoop::None => self.player.set_loop(None),
                SongLoop::Pattern { order } => {
                    self.player
                        .loop_orders(VOICE, order as usize, order as usize);
                }
                SongLoop::Range { order, row0, row1 } => {
                    let subsong = self.player.subsong();
                    // Rows are counted from the top of the subsong: the entry's first row, then
                    // the rows into it.
                    if let Some((start, _)) = self.player.song().order_rows(
                        subsong,
                        VOICE,
                        order as usize,
                        order as usize,
                    ) {
                        self.player.set_loop(Some(SidLoop {
                            voice: VOICE as u8,
                            start: start + row0 as u64,
                            end: start + row1 as u64 + 1,
                        }));
                    }
                }
            },
            SongControl::VoiceLoop { voice, range } => {
                let _ = self
                    .player
                    .set_voice_loop(voice as usize, range.map(|(a, b)| (a as usize, b as usize)));
            }
            SongControl::Mute { channel, on } => {
                let bit = 1u8.checked_shl(channel as u32).unwrap_or(0);
                self.set_masks(|m, s| (if on { m | bit } else { m & !bit }, s));
            }
            SongControl::Solo { channel, on } => {
                let bit = 1u8.checked_shl(channel as u32).unwrap_or(0);
                self.set_masks(|m, s| (m, if on { s | bit } else { s & !bit }));
            }
            SongControl::MuteMask(mask) => self.set_masks(|_, s| (mask as u8, s)),
            SongControl::SoloMask(mask) => self.set_masks(|m, _| (m, mask as u8)),
            SongControl::PreviewOn {
                instrument,
                note,
                duration,
                ..
            } => {
                let index = note.saturating_sub(MIDI_OF_INDEX_0).min(92);
                if self.player.audition_on(VOICE, instrument as usize, index) {
                    self.preview_left = duration;
                }
            }
            SongControl::PreviewOff => {
                self.player.audition_off(VOICE);
                self.preview_left = 0;
            }
            SongControl::SetCell(cell) => {
                self.player.apply_live(&LiveEdit::Row {
                    pattern: cell.pattern as u8,
                    row: cell.row as u8,
                    cell: Row {
                        note: cell.note,
                        instrument: cell.instrument,
                        command: cell.effect,
                        param: cell.param,
                    },
                });
            }
            SongControl::SetInstrument(i) => {
                let f = &i.fm;
                self.player.apply_live(&LiveEdit::Instrument {
                    number: i.number,
                    instrument: Instrument {
                        name: Vec::new(),
                        attack: f[0] >> 4,
                        decay: f[0] & 15,
                        sustain: f[1] >> 4,
                        release: f[1] & 15,
                        first_wave: f[2],
                        gate_timer: f[3],
                        hard_restart: f[4] & 1 != 0,
                        no_gate_off: f[4] & 2 != 0,
                        vibrato_delay: f[5],
                        wave_ptr: f[6],
                        pulse_ptr: f[7],
                        filter_ptr: f[8],
                        speed_ptr: f[9],
                    },
                });
            }
            SongControl::SetTableRow {
                table,
                row,
                left,
                right,
            } => {
                if let Some(table) = table_kind(table) {
                    self.player.apply_live(&LiveEdit::TableRow {
                        table,
                        row,
                        value: TableRow { left, right },
                    });
                }
            }
            SongControl::SidChip { model, revision } => {
                let (model, revision) = crate::songs::sid_chip(model, revision);
                self.player.set_chip(model, revision);
            }
            // The adapter's, and other formats'.
            SongControl::Looped(_)
            | SongControl::Scopes(_)
            | SongControl::Timing { .. }
            | SongControl::SetOrder { .. }
            | SongControl::SetRestart(_) => {}
        }
    }

    fn status(&self) -> SongStatus {
        let s = self.player.status();
        let mut voices = [VoicePos::default(); SONG_MAX_VOICES];
        for (c, v) in voices.iter_mut().enumerate().take(3) {
            *v = VoicePos {
                order: s.order[c] as u16,
                pattern: s.pattern[c] as u16,
                row: s.row[c] as u16,
                valid: true,
            };
        }
        SongStatus {
            playing: !s.preview,
            preview: s.audition != 0,
            order: s.order[VOICE] as u16,
            pattern: s.pattern[VOICE] as u16,
            row: s.row[VOICE] as u16,
            tick: s.tick[VOICE],
            speed: s.tempo,
            tempo: (50 * self.player.song().speed_multiplier as u16).min(511),
            rows: s.song_row as u32,
            frames: s.frames,
            version: 0,
            voices,
        }
    }

    fn levels(&self, out: &mut [f32]) -> f32 {
        let peaks = self.player.peaks();
        for (o, p) in out.iter_mut().zip(peaks) {
            *o = p;
        }
        0.0
    }

    fn set_scopes(&mut self, on: bool) {
        self.scopes = on;
    }

    fn scope(&self, channel: usize, out: &mut [f32]) -> usize {
        let Some(tap) = self.taps.get(channel) else {
            return 0;
        };
        let n = out.len().min(self.block);
        out[..n].copy_from_slice(&tap[..n]);
        n
    }
}
