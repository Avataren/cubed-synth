//! Adlib Tracker II songs under live control: a [`SongPlayer`] over `cubed_opl`'s `A2Player`.
//!
//! The transport maps one to one (`A2Player` has them all: play and pause, a seek that
//! replays silently to the row, loop a pattern or rows, mute and solo by track). The
//! instrument preview is an `OplAudition` of its own beside the song (a second chip, clocked
//! only while a note sounds), loaded with the song's instrument `n` at `PreviewOn` and
//! re-loaded by `SetInstrument`, so an instrument edited under a held note is heard at once.
//! Cells, instruments and the timing take live edits (`LiveEdit`); macro tables and
//! everything that changes the song's shape are a new song, swapped in by the host.
//!
//! Channels are the song's tracks. Levels are each track's OPL channel, scaled to the
//! output (`cubed_opl`'s taps read one operator's full swing as 1, which is a quarter of the
//! chip's full scale); a scope is the same channel's frames. The two tracks of a 4-op pair
//! read the same, as they sound on one channel.

use cubed_opl::a2::live::LiveEdit;
use cubed_opl::a2::model::Cell;
use cubed_opl::a2::player::{A2Loop, A2Player};
use cubed_opl::a2::A2mSong;
use cubed_opl::fm::FmPatch;

use super::audition::{patch_of, Audition};
use super::player::{SongPlayer, CHUNK};
use crate::song::{SongControl, SongLoop, SongStatus};

/// Output frames played (and thrown away) before the song starts, so the buffers that grow
/// on first use have grown before the audio thread plays it.
const WARM_UP_FRAMES: usize = 8_192;
/// One operator's full swing on a tap is a quarter of the chip's full scale.
const TAP_TO_OUTPUT: f32 = 0.25;
/// Notes the preview plays at once.
const PREVIEW_VOICES: usize = 9;

/// An A2M song and its preview. See the module docs.
pub struct A2mPlayer {
    player: A2Player,
    audition: Audition,
    /// The instrument the preview plays (0 none): `SetInstrument` of it reloads the preview.
    preview_instrument: u8,
}

impl A2mPlayer {
    /// A paused player at the start of `song`; the song's first 170 ms are played and thrown
    /// away to warm it up (make it off the audio thread).
    pub fn new(song: A2mSong, sample_rate: f32) -> Result<A2mPlayer, String> {
        let flags = song.flags;
        let mut player = A2Player::from_song(song, sample_rate as f64)?;
        player.set_levels_enabled(true);
        player.play();
        let (mut left, mut right) = ([0.0; CHUNK], [0.0; CHUNK]);
        for _ in 0..WARM_UP_FRAMES / CHUNK {
            player.render(&mut left, &mut right);
        }
        player.pause();
        player.seek(0, 0);
        // What the warm-up measured is not the song's.
        player.set_levels_enabled(false);
        player.set_levels_enabled(true);
        let mut audition = Audition::new(sample_rate as f64, FmPatch::plain(), PREVIEW_VOICES);
        audition.set_depths(flags & 0x08 != 0, flags & 0x10 != 0);
        Ok(A2mPlayer {
            player,
            audition,
            preview_instrument: 0,
        })
    }

    /// The player (tests).
    pub fn player(&self) -> &A2Player {
        &self.player
    }

    fn preview_on(&mut self, instrument: u8, note: u8, velocity: f32, duration: u32) {
        let Some(slot) = (instrument as usize).checked_sub(1) else {
            return;
        };
        let Some(found) = self.player.engine().song().instruments.get(slot) else {
            return;
        };
        self.audition.set_patch(FmPatch::from_a2m(found));
        self.preview_instrument = instrument;
        self.audition.note_on(note, velocity, duration);
    }
}

impl SongPlayer for A2mPlayer {
    fn channels(&self) -> usize {
        self.player.track_count()
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.player.render(left, right);
        self.audition.add_to(left, right);
    }

    fn ended(&self) -> bool {
        self.player.song_end_reached()
    }

    fn control(&mut self, control: SongControl) {
        let p = &mut self.player;
        match control {
            SongControl::Play => p.play(),
            SongControl::Stop => p.pause(),
            SongControl::Seek { order, row } => {
                p.seek(order as usize, row as usize);
            }
            SongControl::Loop(repeat) => p.set_loop(match repeat {
                SongLoop::None => A2Loop::None,
                SongLoop::Pattern { order } => A2Loop::Pattern(order as usize),
                SongLoop::Range { order, row0, row1 } => A2Loop::Range {
                    order: order as usize,
                    row0: row0 as usize,
                    row1: row1 as usize,
                },
            }),
            SongControl::Mute { channel, on } => p.set_mute(channel as usize, on),
            SongControl::Solo { channel, on } => p.set_solo(channel as usize, on),
            SongControl::MuteMask(mask) => p.set_mute_solo(mask, p.solo_mask()),
            SongControl::SoloMask(mask) => p.set_mute_solo(p.mute_mask(), mask),
            SongControl::PreviewOn {
                instrument,
                note,
                velocity,
                duration,
            } => self.preview_on(instrument, note, velocity, duration),
            SongControl::PreviewOff => self.audition.all_notes_off(),
            SongControl::SetCell(cell) => {
                p.apply_edit(&LiveEdit::Cell {
                    pattern: cell.pattern,
                    row: cell.row,
                    track: cell.channel,
                    cell: Cell {
                        note: cell.note,
                        instrument: cell.instrument,
                        effects: [
                            (cell.effect, cell.param),
                            ((cell.period >> 8) as u8, cell.period as u8),
                        ],
                    },
                });
            }
            SongControl::SetInstrument(instrument) => {
                p.apply_edit(&LiveEdit::Instrument {
                    number: instrument.number,
                    fm: instrument.fm,
                    panning: instrument.panning,
                    finetune: instrument.finetune,
                    voice_type: instrument.voice_type,
                });
                if instrument.number == self.preview_instrument {
                    self.audition.set_patch(patch_of(&instrument));
                }
            }
            SongControl::Timing { tempo, speed } => {
                p.apply_edit(&LiveEdit::Timing { tempo, speed });
            }
            // The adapter's, and other formats' (an Adlib order list edit is a new song).
            SongControl::Looped(_)
            | SongControl::Scopes(_)
            | SongControl::SetOrder { .. }
            | SongControl::SetRestart(_)
            | SongControl::SetTableRow { .. }
            | SongControl::SeekVoice { .. }
            | SongControl::VoiceLoop { .. }
            | SongControl::SidChip { .. } => {}
        }
    }

    fn status(&self) -> SongStatus {
        let s = self.player.status();
        SongStatus {
            playing: s.playing,
            preview: self.audition.sounding(),
            order: s.order,
            pattern: s.pattern,
            row: s.row,
            tick: s.tick.min(255) as u8,
            speed: s.speed,
            tempo: s.tempo as u16,
            rows: s.rows,
            frames: s.frames,
            version: 0,
            voices: Default::default(),
        }
    }

    fn levels(&self, out: &mut [f32]) -> f32 {
        let n = self.player.track_levels(out);
        for level in &mut out[..n] {
            *level *= TAP_TO_OUTPUT;
        }
        self.audition.peak()
    }

    fn set_scopes(&mut self, on: bool) {
        self.player.set_taps_enabled(on);
    }

    fn scope(&self, channel: usize, out: &mut [f32]) -> usize {
        if channel >= self.player.track_count() {
            return 0;
        }
        self.player
            .read_tap(self.player.track_channel(channel), out);
        for v in out.iter_mut() {
            *v *= TAP_TO_OUTPUT;
        }
        out.len()
    }
}
