//! A synth patch as a source: cubed-dsp's `AudioEngine` playing a patch saved
//! by the DAW, driven by note events while it plays.

use cubed_dsp::audio_engine::native::AudioEngine;
use cubed_dsp::AutomationFrame;

use crate::source::{Source, SourceEvent};

/// The engine renders blocks of this many frames.
const ENGINE_BLOCK: usize = 128;
/// The engine's macro lanes per voice.
const MACROS: usize = 4;
/// A one-shot synth ends after this long below [`SILENCE`] with no note held.
const SILENT_SECONDS: f32 = 0.3;
const SILENCE: f32 = 1e-4;

/// One of the engine's voices, as notes are handed out to them.
#[derive(Clone, Copy, Debug, Default)]
struct VoiceState {
    note: Option<u8>,
    gate: bool,
    velocity: f32,
    frequency: f32,
    /// When it last started a note, for stealing the oldest.
    started: u64,
    /// Frames until a note with a duration is released.
    release_in: Option<u64>,
}

/// A patch played by note events (`SourceEvent::NoteOn` / `NoteOff`). A free
/// voice takes each new note (one that is released, the longest-released
/// first), else the oldest note is stolen.
pub struct SynthSource {
    engine: AudioEngine,
    frame: AutomationFrame,
    voices: Vec<VoiceState>,
    notes_started: u64,
    /// The last engine block and how much of it has been handed out.
    left: Vec<f32>,
    right: Vec<f32>,
    read: usize,
    /// End once every note is released and the sound has died away.
    one_shot: bool,
    silent_frames: usize,
    sample_rate: f32,
}

impl SynthSource {
    /// A patch (the DAW's patch JSON) ready to play at `sample_rate`. Build
    /// it off the audio thread.
    pub fn from_patch(json: &str, sample_rate: f32) -> Result<SynthSource, String> {
        let mut engine = AudioEngine::new_with_block_size(sample_rate, 8, ENGINE_BLOCK);
        let voices = engine.init_with_patch(json)?;
        Ok(SynthSource::new(engine, voices, sample_rate))
    }

    /// An engine already set up (by code, or from a patch). Build it off the
    /// audio thread: it renders one silent block, so the engine sizes its
    /// buffers and plans its graphs before the audio thread runs it.
    pub fn new(engine: AudioEngine, voices: usize, sample_rate: f32) -> SynthSource {
        let mut source = SynthSource::idle(engine, voices, sample_rate);
        source.render_block();
        source.read = ENGINE_BLOCK;
        source
    }

    fn idle(engine: AudioEngine, voices: usize, sample_rate: f32) -> SynthSource {
        let voices = voices.max(1);
        SynthSource {
            engine,
            frame: AutomationFrame::with_dimensions(voices, MACROS, ENGINE_BLOCK),
            voices: vec![VoiceState::default(); voices],
            notes_started: 0,
            left: vec![0.0; ENGINE_BLOCK],
            right: vec![0.0; ENGINE_BLOCK],
            read: ENGINE_BLOCK,
            one_shot: false,
            silent_frames: 0,
            sample_rate: sample_rate.max(1.0),
        }
    }

    /// Ends the voice once no note is held and the sound has died away (a
    /// sound effect), instead of waiting for a stop.
    pub fn one_shot(mut self) -> SynthSource {
        self.one_shot = true;
        self
    }

    fn note_on(&mut self, note: u8, velocity: f32, duration: u32) {
        self.notes_started += 1;
        let slot = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.gate)
            .min_by_key(|(_, v)| v.started)
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.started)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.voices[slot] = VoiceState {
            note: Some(note),
            gate: true,
            velocity: velocity.clamp(0.0, 1.0),
            frequency: 440.0 * 2f32.powf((note as f32 - 69.0) / 12.0),
            started: self.notes_started,
            release_in: (duration > 0).then_some(duration as u64),
        };
        self.silent_frames = 0;
    }

    fn note_off(&mut self, note: u8) {
        for voice in &mut self.voices {
            if voice.note == Some(note) && voice.gate {
                voice.gate = false;
                voice.release_in = None;
            }
        }
    }

    /// Renders the next engine block into `left`/`right`.
    fn render_block(&mut self) {
        for (i, voice) in self.voices.iter_mut().enumerate() {
            if let Some(frames) = voice.release_in.as_mut() {
                if *frames <= ENGINE_BLOCK as u64 {
                    voice.gate = false;
                    voice.release_in = None;
                } else {
                    *frames -= ENGINE_BLOCK as u64;
                }
            }
            let gate = if voice.gate { 1.0 } else { 0.0 };
            let frequency = if voice.frequency > 0.0 {
                voice.frequency
            } else {
                440.0
            };
            self.frame
                .set_voice_values(i, gate, frequency, voice.velocity, 1.0);
        }
        self.engine
            .process_with_frame(&self.frame, 1.0, &mut self.left, &mut self.right);
        self.read = 0;
    }

    fn held(&self) -> bool {
        self.voices.iter().any(|v| v.gate)
    }
}

impl Source for SynthSource {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let n = left.len().min(right.len());
        let mut at = 0;
        let mut loudest = 0f32;
        while at < n {
            if self.read >= ENGINE_BLOCK {
                self.render_block();
            }
            let k = (n - at).min(ENGINE_BLOCK - self.read);
            let (l, r) = (
                &self.left[self.read..self.read + k],
                &self.right[self.read..self.read + k],
            );
            left[at..at + k].copy_from_slice(l);
            right[at..at + k].copy_from_slice(r);
            loudest = l.iter().chain(r).fold(loudest, |peak, s| peak.max(s.abs()));
            self.read += k;
            at += k;
        }
        if !self.one_shot {
            return true;
        }
        if self.held() || loudest > SILENCE {
            self.silent_frames = 0;
            true
        } else {
            self.silent_frames += n;
            (self.silent_frames as f32) < SILENT_SECONDS * self.sample_rate
        }
    }

    fn event(&mut self, event: SourceEvent) {
        match event {
            SourceEvent::NoteOn {
                note,
                velocity,
                duration,
            } => self.note_on(note, velocity, duration),
            SourceEvent::NoteOff { note } => self.note_off(note),
            SourceEvent::AllNotesOff => {
                for voice in &mut self.voices {
                    voice.gate = false;
                    voice.release_in = None;
                }
            }
        }
    }
}
