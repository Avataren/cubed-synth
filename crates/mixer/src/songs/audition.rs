//! The OPL instrument audition as a mixer voice, and the audition inside an Adlib song.

use cubed_opl::audition::OplAudition;
use cubed_opl::fm::FmPatch;

use crate::song::{SongControl, SongInstrument};
use crate::source::{Source, SourceEvent};

/// An audition is idle (and not clocked) once nothing is keyed and the last block was
/// quieter than this.
const IDLE_PEAK: f32 = 1e-5;

/// The patch a [`SongInstrument`] makes.
pub(super) fn patch_of(instrument: &SongInstrument) -> FmPatch {
    FmPatch {
        fm: instrument.fm,
        panning: instrument.panning.min(2),
        finetune: instrument.finetune,
    }
}

/// An [`OplAudition`] that costs nothing while no note sounds: its chip is clocked from a
/// note until the note's release has played out.
pub(super) struct Audition {
    inner: OplAudition,
    live: bool,
    left: [f32; super::player::CHUNK],
    right: [f32; super::player::CHUNK],
}

impl Audition {
    pub(super) fn new(rate: f64, patch: FmPatch, voices: usize) -> Audition {
        Audition {
            inner: OplAudition::with_voices(rate, patch, voices),
            live: false,
            left: [0.0; super::player::CHUNK],
            right: [0.0; super::player::CHUNK],
        }
    }

    pub(super) fn set_depths(&mut self, deep_tremolo: bool, deep_vibrato: bool) {
        self.inner.set_depths(deep_tremolo, deep_vibrato);
    }

    pub(super) fn note_on(&mut self, note: u8, velocity: f32, duration: u32) {
        self.inner.note_on(note, velocity, duration);
        self.live = true;
    }

    pub(super) fn note_off(&mut self, note: u8) {
        self.inner.note_off(note);
    }

    pub(super) fn all_notes_off(&mut self) {
        self.inner.all_notes_off();
    }

    pub(super) fn set_patch(&mut self, patch: FmPatch) {
        self.inner.set_patch(patch);
        // The edit's register writes are applied by the next block.
        self.live |= self.inner.active_notes() > 0;
    }

    /// A note is sounding or releasing.
    pub(super) fn sounding(&self) -> bool {
        self.live
    }

    /// The loudest frame of the last block, 0 while idle.
    pub(super) fn peak(&self) -> f32 {
        if self.live {
            self.inner.peak()
        } else {
            0.0
        }
    }

    /// Adds the next block (at most `CHUNK` frames) into `left` and `right`.
    pub(super) fn add_to(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.live {
            return;
        }
        let n = left.len().min(right.len()).min(self.left.len());
        self.inner.render(&mut self.left[..n], &mut self.right[..n]);
        for i in 0..n {
            left[i] += self.left[i];
            right[i] += self.right[i];
        }
        if self.inner.active_notes() == 0 && self.inner.peak() < IDLE_PEAK {
            self.live = false;
        }
    }
}

/// One Adlib instrument played by hand as a voice of its own: `SourceEvent::NoteOn`,
/// `NoteOff` and `AllNotesOff` play it (up to the voices it was opened with at once), and
/// `SongControl::SetInstrument` changes the instrument, under the notes already sounding
/// too, so a slider moved in an instrument editor is heard on a held note. It never ends by
/// itself and costs nothing while it is silent. Opened by `songs::open_opl_audition`.
pub struct OplAuditionSource {
    audition: Audition,
}

impl OplAuditionSource {
    pub(super) fn new(rate: f32, instrument: &SongInstrument, voices: usize) -> OplAuditionSource {
        OplAuditionSource {
            audition: Audition::new(rate as f64, patch_of(instrument), voices),
        }
    }
}

impl Source for OplAuditionSource {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let n = left.len().min(right.len());
        left[..n].fill(0.0);
        right[..n].fill(0.0);
        let mut at = 0;
        while at < n {
            let k = (n - at).min(super::player::CHUNK);
            self.audition
                .add_to(&mut left[at..at + k], &mut right[at..at + k]);
            at += k;
        }
        true
    }

    fn event(&mut self, event: SourceEvent) {
        match event {
            SourceEvent::NoteOn {
                note,
                velocity,
                duration,
            } => self.audition.note_on(note, velocity, duration),
            SourceEvent::NoteOff { note } => self.audition.note_off(note),
            SourceEvent::AllNotesOff => self.audition.all_notes_off(),
            SourceEvent::Song(SongControl::SetInstrument(instrument)) => {
                self.audition.set_patch(patch_of(&instrument));
            }
            _ => {}
        }
    }
}
