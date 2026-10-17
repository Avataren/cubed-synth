//! Per-format playback semantics, as data the effect processor reads (the
//! DAW's `format-profile.ts`), and the decoding of a cell's effect bytes.

use crate::pitch::PitchModel;

/// What an effect does, independent of the format's numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffectType {
    Arpeggio,
    PortaUp,
    PortaDown,
    TonePorta,
    Vibrato,
    TonePortaVol,
    VibratoVol,
    Tremolo,
    SetPan,
    SampleOffset,
    VolSlide,
    PosJump,
    SetVolume,
    PatBreak,
    ExtEffect,
    SetGlobalVol,
    GlobalVolSlide,
    KeyOff,
    PanSlide,
    RetrigVol,
    Tremor,
    FineVibrato,
    SetVibratoWave,
    SetTremoloWave,
    FinePortaUp,
    FinePortaDown,
    ExtraFinePorta,
    SetEnvelopePos,
    NoteCut,
    NoteDelay,
    PatDelay,
}

impl EffectType {
    /// Whether the effect has work on ticks after the first.
    pub(crate) fn is_tick_based(self) -> bool {
        use EffectType::*;
        matches!(
            self,
            PortaUp
                | PortaDown
                | TonePorta
                | Vibrato
                | TonePortaVol
                | VibratoVol
                | Tremolo
                | Arpeggio
                | VolSlide
                | PanSlide
                | RetrigVol
                | Tremor
                | FineVibrato
                | NoteCut
                | NoteDelay
                | KeyOff
        )
    }
}

/// Extended (Exy / Sxy) subcommands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExtSub {
    FilterToggle,
    FinePortaUp,
    FinePortaDown,
    GlissandoCtrl,
    VibratoWave,
    SetFinetune,
    PatLoop,
    TremoloWave,
    SetPan,
    Retrigger,
    FineVolUp,
    FineVolDown,
    NoteCut,
    NoteDelay,
    PatDelay,
    InvertLoop,
}

impl ExtSub {
    fn effect_type(self) -> EffectType {
        use EffectType as E;
        match self {
            ExtSub::FinePortaUp => E::FinePortaUp,
            ExtSub::FinePortaDown => E::FinePortaDown,
            ExtSub::VibratoWave => E::SetVibratoWave,
            ExtSub::TremoloWave => E::SetTremoloWave,
            ExtSub::SetPan => E::SetPan,
            ExtSub::Retrigger => E::RetrigVol,
            ExtSub::FineVolUp | ExtSub::FineVolDown => E::VolSlide,
            ExtSub::NoteCut => E::NoteCut,
            ExtSub::NoteDelay => E::NoteDelay,
            ExtSub::PatDelay => E::PatDelay,
            ExtSub::FilterToggle
            | ExtSub::GlissandoCtrl
            | ExtSub::SetFinetune
            | ExtSub::PatLoop
            | ExtSub::InvertLoop => E::ExtEffect,
        }
    }
}

/// A decoded effect: its behaviour and the parameter's two nibbles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Effect {
    pub kind: EffectType,
    pub x: u8,
    pub y: u8,
    pub sub: Option<ExtSub>,
}

impl Effect {
    pub(crate) fn param(&self) -> u8 {
        (self.x << 4) | self.y
    }
}

/// What a cell's effect bytes mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decoded {
    Effect(Effect),
    /// Ticks per row (0 is ProTracker's stop).
    Speed(u8),
    Tempo(u8),
}

/// The shared MOD/XM Exy numbering.
const MOD_EXTENDED: [Option<ExtSub>; 16] = [
    Some(ExtSub::FilterToggle),
    Some(ExtSub::FinePortaUp),
    Some(ExtSub::FinePortaDown),
    Some(ExtSub::GlissandoCtrl),
    Some(ExtSub::VibratoWave),
    Some(ExtSub::SetFinetune),
    Some(ExtSub::PatLoop),
    Some(ExtSub::TremoloWave),
    Some(ExtSub::SetPan),
    Some(ExtSub::Retrigger),
    Some(ExtSub::FineVolUp),
    Some(ExtSub::FineVolDown),
    Some(ExtSub::NoteCut),
    Some(ExtSub::NoteDelay),
    Some(ExtSub::PatDelay),
    Some(ExtSub::InvertLoop),
];

/// ST3's Sxy numbering (st3play's `ssoncejmp`); the subcommands ST3 itself
/// ignores have no entry.
const S3M_EXTENDED: [Option<ExtSub>; 16] = [
    Some(ExtSub::FilterToggle),
    Some(ExtSub::GlissandoCtrl),
    Some(ExtSub::SetFinetune),
    Some(ExtSub::VibratoWave),
    Some(ExtSub::TremoloWave),
    None,
    None,
    None,
    Some(ExtSub::SetPan),
    None,
    None,
    Some(ExtSub::PatLoop),
    Some(ExtSub::NoteCut),
    Some(ExtSub::NoteDelay),
    Some(ExtSub::PatDelay),
    None,
];

/// S3M's commands, the letter minus 0x40. A (speed), J (arpeggio), S
/// (extended) and T (tempo) are decoded on their own; M, N, Y and Z have no
/// reading here.
const S3M_COMMANDS: &[(u8, EffectType)] = &[
    (0x02, EffectType::PosJump),
    (0x03, EffectType::PatBreak),
    (0x04, EffectType::VolSlide),
    (0x05, EffectType::PortaDown),
    (0x06, EffectType::PortaUp),
    (0x07, EffectType::TonePorta),
    (0x08, EffectType::Vibrato),
    (0x09, EffectType::Tremor),
    (0x0b, EffectType::VibratoVol),
    (0x0c, EffectType::TonePortaVol),
    (0x0f, EffectType::SampleOffset),
    (0x10, EffectType::PanSlide),
    (0x11, EffectType::RetrigVol),
    (0x12, EffectType::Tremolo),
    (0x15, EffectType::FineVibrato),
    (0x16, EffectType::SetGlobalVol),
    (0x17, EffectType::GlobalVolSlide),
    (0x18, EffectType::SetPan),
];

/// ProTracker's command numbers, with FT2's continuation past 0x0F.
const MOD_COMMANDS: &[(u8, EffectType)] = &[
    (0x01, EffectType::PortaUp),
    (0x02, EffectType::PortaDown),
    (0x03, EffectType::TonePorta),
    (0x04, EffectType::Vibrato),
    (0x05, EffectType::TonePortaVol),
    (0x06, EffectType::VibratoVol),
    (0x07, EffectType::Tremolo),
    (0x08, EffectType::SetPan),
    (0x09, EffectType::SampleOffset),
    (0x0a, EffectType::VolSlide),
    (0x0b, EffectType::PosJump),
    (0x0c, EffectType::SetVolume),
    (0x0d, EffectType::PatBreak),
    (0x10, EffectType::SetGlobalVol),
    (0x11, EffectType::GlobalVolSlide),
    (0x14, EffectType::KeyOff),
    (0x15, EffectType::SetEnvelopePos),
    (0x19, EffectType::PanSlide),
    (0x1b, EffectType::RetrigVol),
    (0x1d, EffectType::Tremor),
    (0x1e, EffectType::FineVibrato),
    (0x21, EffectType::ExtraFinePorta),
];

/// How the nibbles of a volume-slide parameter read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlideNibbles {
    /// The up nibble wins; no fine slides on Axy.
    ModXm,
    /// A 0xF nibble makes the slide fine; the down nibble wins.
    S3m,
}

/// One format's semantics. See the DAW's `format-profile.ts` for the
/// references behind each field.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Profile {
    pub pitch: PitchModel,
    /// A volume-slide unit, as a fraction of full volume.
    pub volume_slide_unit: f64,
    /// A00 repeats the last slide (FT2, ST3; not ProTracker).
    pub volume_slide_has_memory: bool,
    /// Period units one portamento step moves.
    pub portamento_unit_scale: f64,
    /// An EDx past the row's end plays at the start of the next row.
    pub note_delay_overflow_carries: bool,
    /// E0x drives the Amiga LED filter.
    pub filter_toggle_command: bool,
    /// Semitones an E5x nibble tunes by.
    pub finetune_from_nibble: fn(u8) -> f64,
    /// Which arpeggio note (0 base, 1 x, 2 y) a tick plays.
    pub arpeggio_step: fn(u32, u32) -> u8,
    /// E1x/E2x/EAx/EBx/Xxy remember their parameter.
    pub fine_slide_has_memory: bool,
    /// Pan a pan-slide unit moves per tick, on the -1..1 scale.
    pub pan_slide_unit: f64,
    pub f00_stops_song: bool,
    pub volume_slide_nibbles: SlideNibbles,
    /// Volume slides also step on tick 0.
    pub fast_volume_slides: bool,
    commands: &'static [(u8, EffectType)],
    arpeggio_command: u8,
    speed_tempo_command: Option<u8>,
    tempo_command: Option<u8>,
    extended_command: Option<u8>,
    extended_map: &'static [Option<ExtSub>; 16],
    /// S3M: E/F parameters from 0xE0 are one-shot fine slides.
    pub fine_porta_high_params: bool,
    pub tone_porta_continues_through_empty_rows: bool,
    pub pitch_resets_after_effectless_row: bool,
    pub pattern_delay_repeats_tick_zero: bool,
    pub pattern_loop_end_resets_start: bool,
    /// Commands whose zero parameter reuses the last non-zero one of any.
    pub shared_effect_info_commands: &'static [u8],
    pub note_delay_defers_cell: bool,
    pub note_off_row_runs_cell: bool,
    pub st3_vibrato: bool,
    /// The command that sets the speed alone, whatever its parameter (0 is
    /// ignored): ST3's A.
    speed_command: Option<u8>,
    /// Play what the DAW plays where it departs from the original tracker
    /// (`PlayerOptions::daw_quirks`).
    pub daw_quirks: bool,
}

/// ProTracker: the tick counts up and `tick % 3` picks the note.
fn protracker_arpeggio(tick: u32, _ticks_per_row: u32) -> u8 {
    (tick % 3) as u8
}

impl Profile {
    /// ProTracker, inside its three octaves (`amiga_limits`) or with the PC
    /// trackers' seven.
    pub(crate) fn protracker(amiga_limits: bool) -> Profile {
        Profile {
            pitch: PitchModel::Amiga {
                amiga_limits,
                wraps_to_dc: amiga_limits,
            },
            volume_slide_unit: 1.0 / 64.0,
            volume_slide_has_memory: false,
            portamento_unit_scale: 1.0,
            note_delay_overflow_carries: true,
            filter_toggle_command: true,
            finetune_from_nibble: |n| (if n < 8 { n as f64 } else { n as f64 - 16.0 }) / 8.0,
            arpeggio_step: protracker_arpeggio,
            fine_slide_has_memory: false,
            pan_slide_unit: 1.0 / 64.0,
            f00_stops_song: true,
            volume_slide_nibbles: SlideNibbles::ModXm,
            fast_volume_slides: false,
            commands: MOD_COMMANDS,
            arpeggio_command: 0x00,
            speed_tempo_command: Some(0x0f),
            tempo_command: None,
            extended_command: Some(0x0e),
            extended_map: &MOD_EXTENDED,
            fine_porta_high_params: false,
            tone_porta_continues_through_empty_rows: false,
            pitch_resets_after_effectless_row: false,
            pattern_delay_repeats_tick_zero: false,
            pattern_loop_end_resets_start: false,
            shared_effect_info_commands: &[],
            note_delay_defers_cell: false,
            note_off_row_runs_cell: false,
            st3_vibrato: false,
            speed_command: None,
            daw_quirks: false,
        }
    }

    /// Scream Tracker 3. `amiga_limits` and `fast_volume_slides` are the
    /// file's header flags.
    pub(crate) fn s3m(amiga_limits: bool, fast_volume_slides: bool, daw_quirks: bool) -> Profile {
        Profile {
            pitch: PitchModel::S3m { amiga_limits },
            volume_slide_has_memory: true,
            // Every ST3 slide moves `info << 2` period units a tick.
            portamento_unit_scale: 4.0,
            filter_toggle_command: false,
            fine_slide_has_memory: true,
            pan_slide_unit: 2.0 / 255.0,
            f00_stops_song: false,
            volume_slide_nibbles: SlideNibbles::S3m,
            fast_volume_slides,
            commands: S3M_COMMANDS,
            arpeggio_command: 0x0a,
            // ST3's A sets the speed, 1-255, and ignores A00; the DAW reads
            // it as ProTracker's F (A20 and up a tempo, A00 speed 1).
            speed_tempo_command: daw_quirks.then_some(0x01),
            speed_command: (!daw_quirks).then_some(0x01),
            daw_quirks,
            tempo_command: Some(0x14),
            extended_command: Some(0x13),
            extended_map: &S3M_EXTENDED,
            fine_porta_high_params: true,
            tone_porta_continues_through_empty_rows: true,
            pitch_resets_after_effectless_row: true,
            pattern_delay_repeats_tick_zero: true,
            pattern_loop_end_resets_start: true,
            // GET_LAST_NFO: D E F I J K L Q R S.
            shared_effect_info_commands: &[
                0x04, 0x05, 0x06, 0x09, 0x0a, 0x0b, 0x0c, 0x11, 0x12, 0x13,
            ],
            note_delay_defers_cell: true,
            note_off_row_runs_cell: true,
            st3_vibrato: true,
            ..Profile::protracker(false)
        }
    }

    /// FastTracker 2, on the linear frequency table or the Amiga one.
    pub(crate) fn xm(linear: bool) -> Profile {
        Profile {
            pitch: if linear {
                PitchModel::Linear
            } else {
                PitchModel::XmAmiga
            },
            volume_slide_has_memory: true,
            portamento_unit_scale: 4.0,
            // An unsigned position in FT2's -128..127 finetune range.
            finetune_from_nibble: |n| (n as f64 - 8.0) / 8.0,
            arpeggio_step: crate::effects::ft2_arpeggio_step,
            fine_slide_has_memory: true,
            pan_slide_unit: 2.0 / 255.0,
            f00_stops_song: false,
            filter_toggle_command: false,
            ..Profile::protracker(false)
        }
    }

    /// What `command` with `param` means in this format.
    pub(crate) fn decode(&self, command: u8, param: u8) -> Option<Decoded> {
        let (x, y) = (param >> 4, param & 0x0f);
        let effect = |kind, sub| Some(Decoded::Effect(Effect { kind, x, y, sub }));
        if command == self.arpeggio_command {
            return if param != 0 {
                effect(EffectType::Arpeggio, None)
            } else {
                None
            };
        }
        if Some(command) == self.speed_command {
            return (param > 0).then_some(Decoded::Speed(param));
        }
        if Some(command) == self.speed_tempo_command {
            return Some(match param {
                0x01..=0x1f => Decoded::Speed(param),
                0x20.. => Decoded::Tempo(param),
                0 => Decoded::Speed(0),
            });
        }
        if Some(command) == self.tempo_command {
            return (param >= 0x20).then_some(Decoded::Tempo(param));
        }
        if Some(command) == self.extended_command {
            let sub = self.extended_map[x as usize]?;
            return effect(sub.effect_type(), Some(sub));
        }
        let kind = self.commands.iter().find(|(c, _)| *c == command)?.1;
        effect(kind, None)
    }
}
