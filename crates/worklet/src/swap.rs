//! Swapping a rebuilt synth in for a playing one without stalling the audio
//! thread.
//!
//! A synth rebuilt from an edited patch has to sound as if its notes had been
//! held as long as the old synth's: a bass patch that decayed a second ago must
//! not attack again. That takes rendering the notes' whole age (seconds of
//! audio) before the swap. Natively a thread does it; in a worklet the render
//! callback is the only thread, so the warm-up is cut into slices, a few blocks
//! per render quantum, while the old voice goes on playing. The new synth
//! catches up with the clock (it renders faster than real time), then replaces
//! the old voice with a crossfade in one batch, at a block boundary.
//!
//! The old voice's events keep going to it meanwhile (the page need not know a
//! swap is pending); the worklet copies them to the warming synth at the moment
//! they would sound there. After the swap, commands that name the old voice
//! reach the new one (an alias), so the page can go on using the old id.

use cubed_mixer::{
    swap_commands, Command, Mixer, PlaySource, Source, SourceEvent, SourceId, SynthSource, VoiceId,
    EFFECT_TAIL_BLOCKS,
};

use crate::BLOCK;

/// Events kept for a warming synth.
const MAX_LOG: usize = 512;
/// Voice aliases kept (an old id and the voice that took over).
const MAX_ALIASES: usize = 8;
/// Blocks warmed per render quantum unless the page says.
pub const DEFAULT_SLICE: u32 = 4;
/// The most blocks warmed per render quantum.
pub const MAX_SLICE: u32 = 32;

/// What a call to [`Swaps::step`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// No swap is pending.
    Idle,
    /// A synth is warming up.
    Warming,
    /// The swap happened: the new synth replaced the old voice.
    Done { old: VoiceId, new: VoiceId },
    /// The swap was given up: the old voice ended or was stopped.
    Cancelled,
}

/// A rebuilt synth warming up to take over from a playing voice.
struct Warming {
    /// Boxed when the swap begins (in the message handler), so that handing it
    /// to the mixer allocates nothing.
    synth: Box<SynthSource>,
    slot: SourceId,
    old: VoiceId,
    play: PlaySource,
    fade: u32,
    slice: u32,
    /// How many blocks into its notes the old synth was when the swap began.
    base: u64,
    /// Frames of real time since.
    elapsed_frames: u64,
    /// Blocks the new synth has rendered (its first, planning one included).
    rendered: u64,
    next: usize,
}

impl Warming {
    /// The block the new synth must have rendered to be level with the clock.
    fn target(&self) -> u64 {
        self.base + self.elapsed_frames / BLOCK as u64
    }

    fn deliver_due(&mut self, log: &[(u64, SourceEvent)]) {
        while self.next < log.len() && log[self.next].0 <= self.rendered {
            self.synth.event(log[self.next].1);
            self.next += 1;
        }
    }
}

/// The pending swap and the voices that have been swapped.
pub struct Swaps {
    warming: Option<Warming>,
    /// The old voice's events since the swap began, with the block of the new
    /// synth's clock they belong at. Kept here, allocated once, so that a swap
    /// finishing on the audio thread frees nothing.
    log: Vec<(u64, SourceEvent)>,
    /// A swap given up on the audio thread, to be freed by the next message.
    trash: Option<Warming>,
    /// (old voice, voice that took over): commands for the old one are rewritten.
    aliases: Vec<(VoiceId, VoiceId)>,
}

impl Swaps {
    pub fn new() -> Swaps {
        Swaps {
            warming: None,
            log: Vec::with_capacity(MAX_LOG),
            trash: None,
            aliases: Vec::with_capacity(MAX_ALIASES),
        }
    }

    /// Whether a synth is warming up.
    pub fn pending(&self) -> bool {
        self.warming.is_some()
    }

    /// Starts warming `synth` (built with its held notes started and its first
    /// block rendered) to take over from `old`; `base` is how many blocks old
    /// its notes are. A swap already pending is dropped. Allocates (the event
    /// log): call it from the message handler.
    pub fn begin(
        &mut self,
        synth: SynthSource,
        slot: SourceId,
        old: VoiceId,
        play: PlaySource,
        fade: u32,
        base: u64,
        slice: u32,
    ) {
        self.trash = None;
        self.log.clear();
        self.warming = Some(Warming {
            synth: Box::new(synth),
            slot,
            old,
            play,
            fade,
            slice: if slice == 0 { DEFAULT_SLICE } else { slice }.min(MAX_SLICE),
            base,
            elapsed_frames: 0,
            rendered: 1,
            next: 0,
        });
    }

    /// Takes a command on its way to the mixer: rewrites the voice of a command
    /// for a voice that has been swapped, copies a pending swap's events and gives
    /// the swap up if the old voice is stopped.
    pub fn route(&mut self, command: &mut Command) {
        self.trash = None;
        if let Some(voice) = voice_of(command) {
            if let Some((_, new)) = self.aliases.iter().find(|(old, _)| *old == *voice) {
                *voice = *new;
            }
        }
        let Some(w) = self.warming.as_mut() else {
            return;
        };
        match command {
            Command::SourceEvent { voice, event } if *voice == w.old => {
                if self.log.len() < self.log.capacity() {
                    self.log.push((w.target(), *event));
                }
            }
            Command::Stop(voice, _) if *voice == w.old => self.warming = None,
            Command::StopAll(_) => self.warming = None,
            _ => {}
        }
    }

    /// Warms a slice, once per render quantum of `frames`, before the quantum is
    /// rendered; when the new synth has caught up with the clock, swaps it in.
    /// Allocates nothing.
    pub fn step(&mut self, mixer: &mut Mixer, frames: usize) -> Step {
        let Some(w) = self.warming.as_mut() else {
            return Step::Idle;
        };
        if !mixer.is_playing(w.old) {
            self.trash = self.warming.take();
            return Step::Cancelled;
        }
        w.elapsed_frames += frames as u64;
        let target = w.target();
        let mut budget = w.slice;
        while budget > 0 && w.rendered < target {
            w.deliver_due(&self.log);
            // The effects only for the last stretch before the swap.
            let tail = if target - w.rendered <= EFFECT_TAIL_BLOCKS as u64 {
                1
            } else {
                0
            };
            w.synth.advance(1, tail);
            w.rendered += 1;
            budget -= 1;
        }
        if w.rendered < target {
            return Step::Warming;
        }
        w.deliver_due(&self.log);
        let Warming {
            synth,
            slot,
            old,
            play,
            fade,
            ..
        } = self.warming.take().expect("warming");
        let new = play.voice;
        mixer.apply(Command::PrepareSource(slot, synth));
        for command in swap_commands(old, play, fade) {
            mixer.apply(command);
        }
        // Whatever pointed at the old voice (or at the voice it replaced) now
        // points at the new one.
        for (_, to) in self.aliases.iter_mut() {
            if *to == old {
                *to = new;
            }
        }
        if self.aliases.len() < self.aliases.capacity() {
            self.aliases.push((old, new));
        } else {
            self.aliases.remove(0);
            self.aliases.push((old, new));
        }
        Step::Done { old, new }
    }
}

impl Default for Swaps {
    fn default() -> Self {
        Swaps::new()
    }
}

/// The voice a command is about, if it is about one that already exists.
fn voice_of(command: &mut Command) -> Option<&mut VoiceId> {
    match command {
        Command::Stop(voice, _)
        | Command::SetVoiceGain(voice, _)
        | Command::SetVoicePitch(voice, _)
        | Command::SetVoicePan(voice, _)
        | Command::SetVoiceSpatial(voice, _)
        | Command::FadeVoice { voice, .. }
        | Command::SourceEvent { voice, .. }
        | Command::SetVoicePosition { voice, .. }
        | Command::SetVoiceOcclusion { voice, .. }
        | Command::SetEngine { voice, .. } => Some(voice),
        _ => None,
    }
}
