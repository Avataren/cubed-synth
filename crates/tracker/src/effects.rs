//! Per-channel effect processing: what each effect does on tick 0 of its row
//! and on the ticks after, written as commands for the channel's voice.
//!
//! A port of the DAW's `effect-processor.ts`, kept close to it (names,
//! order, quirks) so the two can be checked against each other; the
//! references for each behaviour are in its comments and in
//! `format-profile.ts`.

use crate::profile::{Effect, EffectType, ExtSub, Profile, SlideNibbles};

/// Frames one unit of 9xx skips.
const SAMPLE_OFFSET_FRAMES_PER_UNIT: u32 = 256;
/// The vibrato and tremolo tables peak at 255.
const VIBRATO_TABLE_PEAK: f64 = 255.0;
const VIBRATO_DEPTH_DIVISOR: f64 = 128.0;
const TREMOLO_DEPTH_DIVISOR: f64 = 64.0;

/// ProTracker's and FT2's half-sine.
const SINE: [u8; 32] = [
    0, 24, 49, 74, 97, 120, 141, 161, 180, 197, 212, 224, 235, 244, 250, 253, 255, 253, 250, 244,
    235, 224, 212, 197, 180, 161, 141, 120, 97, 74, 49, 24,
];

/// ST3's half-sine (`vibsin`).
const ST3_SINE: [i16; 32] = [
    0x00, 0x18, 0x31, 0x4a, 0x61, 0x78, 0x8d, 0xa1, 0xb4, 0xc5, 0xd4, 0xe0, 0xeb, 0xf4, 0xfa, 0xfd,
    0xff, 0xfd, 0xfa, 0xf4, 0xeb, 0xe0, 0xd4, 0xc5, 0xb4, 0xa1, 0x8d, 0x78, 0x61, 0x4a, 0x31, 0x18,
];

/// Where a pitch came from: an arpeggio's table note, or a tone
/// portamento arriving on its target. None is a slide from the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PitchSource {
    Table,
    Target,
}

/// How a volume gets to its new value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ramp {
    /// Unqualified: a ramp from the previous change.
    Default,
    Linear,
    Step,
}

/// What a channel's voice is told to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Start a note, cutting the channel's previous one. `velocity` is
    /// 0-127; `pan` 0-1 when the row sets one; `offset` in sample frames.
    NoteOn {
        midi: f64,
        velocity: u8,
        frequency: f64,
        pan: Option<f64>,
        offset: Option<u32>,
    },
    NoteOff,
    /// A frequency in the player's musical Hz.
    Pitch {
        frequency: f64,
        source: Option<PitchSource>,
    },
    /// 0-1.
    Volume {
        volume: f64,
        ramp: Ramp,
    },
    /// -1 (left) to 1.
    Pan {
        pan: f64,
    },
    EnvelopePosition {
        tick: u32,
    },
    /// Restart the note from its start.
    Retrigger {
        midi: f64,
        velocity: u8,
        frequency: f64,
    },
}

/// A fixed-capacity command list: processing never allocates.
#[derive(Clone, Copy, Debug)]
pub struct Commands {
    items: [Command; Commands::CAPACITY],
    len: usize,
}

impl Default for Commands {
    fn default() -> Self {
        Commands {
            items: [Command::NoteOff; Commands::CAPACITY],
            len: 0,
        }
    }
}

impl Commands {
    const CAPACITY: usize = 8;

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn push(&mut self, command: Command) {
        debug_assert!(self.len < Self::CAPACITY, "command buffer full");
        if self.len < Self::CAPACITY {
            self.items[self.len] = command;
            self.len += 1;
        }
    }

    pub fn as_slice(&self) -> &[Command] {
        &self.items[..self.len]
    }

    fn has_pitch(&self) -> bool {
        self.as_slice()
            .iter()
            .any(|c| matches!(c, Command::Pitch { .. }))
    }

    fn pitch(&mut self, frequency: f64, source: Option<PitchSource>) {
        self.push(Command::Pitch { frequency, source });
    }

    fn volume(&mut self, volume: f64, ramp: Ramp) {
        self.push(Command::Volume { volume, ramp });
    }
}

/// An XM volume-column command (0x60-0xFF).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VolumeCommand {
    SlideDown(u8),
    SlideUp(u8),
    FineDown(u8),
    FineUp(u8),
    VibratoSpeed(u8),
    Vibrato(u8),
    SetPan(u8),
    PanSlideLeft(u8),
    PanSlideRight(u8),
    /// The speed, already times 16.
    TonePorta(u8),
}

impl VolumeCommand {
    pub(crate) fn decode(raw: u8) -> Option<VolumeCommand> {
        let v = raw & 0x0f;
        Some(match raw >> 4 {
            0x6 => VolumeCommand::SlideDown(v),
            0x7 => VolumeCommand::SlideUp(v),
            0x8 => VolumeCommand::FineDown(v),
            0x9 => VolumeCommand::FineUp(v),
            0xa => VolumeCommand::VibratoSpeed(v),
            0xb => VolumeCommand::Vibrato(v),
            0xc => VolumeCommand::SetPan(v),
            0xd => VolumeCommand::PanSlideLeft(v),
            0xe => VolumeCommand::PanSlideRight(v),
            0xf => VolumeCommand::TonePorta(v * 16),
            _ => return None,
        })
    }

    pub(crate) fn is_tick_based(self) -> bool {
        matches!(
            self,
            VolumeCommand::SlideDown(_)
                | VolumeCommand::SlideUp(_)
                | VolumeCommand::PanSlideLeft(_)
                | VolumeCommand::PanSlideRight(_)
                | VolumeCommand::Vibrato(_)
                | VolumeCommand::TonePorta(_)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlideMode {
    None,
    Normal,
    Fine,
}

#[derive(Clone, Copy, Debug)]
struct VolumeSlide {
    delta: f64,
    mode: SlideMode,
    first_tick: bool,
}

const NO_SLIDE: VolumeSlide = VolumeSlide {
    delta: 0.0,
    mode: SlideMode::None,
    first_tick: false,
};

/// A note waiting for its delay tick.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DelayedNote {
    midi: f64,
    /// 0-255.
    velocity: f64,
    frequency: Option<f64>,
}

/// One channel's effect state (the DAW's `TrackEffectState`).
#[derive(Clone, Debug)]
pub(crate) struct EffectState {
    pub profile: Profile,
    pub current_midi: f64,
    pub current_frequency: f64,
    target_midi: f64,
    pub target_frequency: f64,
    target_period: Option<f64>,
    last_tone_porta_target_freq: Option<f64>,
    last_tone_porta_target_period: Option<f64>,
    pub tone_porta_active: bool,
    /// 0-1.
    pub current_volume: f64,
    /// -1 to 1.
    pub current_pan: f64,
    portamento_speed: f64,
    pub tone_porta_speed: f64,
    pub current_period: Option<f64>,
    vibrato_speed: f64,
    vibrato_depth: f64,
    vibrato_pos: i64,
    pub vibrato_applied: bool,
    pub arpeggio_applied: bool,
    pub vibrato_restart: bool,
    pub vibrato_held_wave: f64,
    vibrato_waveform: u8,
    vibrato_retrigger: bool,
    tremolo_speed: f64,
    tremolo_depth: f64,
    tremolo_pos: i64,
    tremolo_waveform: u8,
    tremolo_retrigger: bool,
    tremolo_applied: bool,
    arpeggio_x: u8,
    arpeggio_y: u8,
    volume_slide: VolumeSlide,
    pan_slide_speed: f64,
    volume_column_slide: f64,
    volume_column_pan_slide: f64,
    retrigger_interval: u8,
    retrigger_tick: u8,
    retrigger_vol_change: u8,
    last_retrigger: u8,
    tremor_pos: u32,
    last_tremor: u8,
    glissando: bool,
    note_cut_tick: i32,
    note_delay_tick: i32,
    delayed_note: Option<DelayedNote>,
    delayed_volume: Option<f64>,
    pub has_active_voice: bool,
    /// The instrument the channel plays, for rows that name none.
    pub instrument: Option<u8>,
    last_porta_up: u8,
    last_porta_down: u8,
    last_tone_porta: u8,
    last_vibrato: u8,
    last_tremolo: u8,
    last_vol_slide: u8,
    last_arpeggio: u8,
    pub last_effect_info: u8,
    last_sample_offset: u32,
    last_fine_porta_up: u8,
    last_fine_porta_down: u8,
    last_fine_vol_up: u8,
    last_fine_vol_down: u8,
    last_extra_fine_porta_up: u8,
    last_extra_fine_porta_down: u8,
    carry_delayed_note: Option<DelayedNote>,
}

pub(crate) fn midi_to_frequency(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

fn frequency_to_midi(frequency: f64) -> f64 {
    69.0 + 12.0 * (frequency / 440.0).log2()
}

fn clamp_volume(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// The channel volume as a note-on velocity, 0-127.
fn velocity_from_volume(volume: f64) -> u8 {
    js_round(clamp_volume(volume) * 127.0) as u8
}

/// JavaScript's `Math.round`: halves go up, also below zero.
pub(crate) fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// FT2's `arpeggioTab`, overflow bytes included.
const FT2_ARPEGGIO: [u8; 32] = [
    0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 0x00, 0x18, 0x31, 0x4a, 0x61, 0x78, 0x8d, 0xa1,
    0xb4, 0xc5, 0xd4, 0xe0, 0xeb, 0xf4, 0xfa, 0xfd,
];

/// FT2: `arpeggioTab[song.tick & 31]`, its tick counting down from the speed.
pub(crate) fn ft2_arpeggio_step(tick: u32, ticks_per_row: u32) -> u8 {
    match FT2_ARPEGGIO[(ticks_per_row.wrapping_sub(tick) & 31) as usize] {
        0 => 0,
        1 => 1,
        _ => 2,
    }
}

/// The vibrato/tremolo waveform at `pos`, -1 to 1.
fn waveform_value(pos: i64, waveform: u8) -> f64 {
    let p = pos & 63;
    let index = (p & 31) as usize;
    let negative = p >= 32;
    match waveform & 3 {
        1 => {
            let value = if negative {
                -(255 - ((index as i64) << 3))
            } else {
                (index as i64) << 3
            };
            value as f64 / 255.0
        }
        2 | 3 => {
            if negative {
                -1.0
            } else {
                1.0
            }
        }
        _ => {
            let value = SINE[index] as f64;
            if negative {
                -value / 255.0
            } else {
                value / 255.0
            }
        }
    }
}

impl EffectState {
    pub(crate) fn new(profile: Profile) -> EffectState {
        EffectState {
            profile,
            current_midi: 60.0,
            current_frequency: 261.63,
            target_midi: 60.0,
            target_frequency: 261.63,
            target_period: None,
            last_tone_porta_target_freq: None,
            last_tone_porta_target_period: None,
            tone_porta_active: false,
            current_volume: 1.0,
            current_pan: 0.0,
            portamento_speed: 0.0,
            tone_porta_speed: 0.0,
            current_period: None,
            vibrato_speed: 0.0,
            vibrato_depth: 0.0,
            vibrato_pos: 0,
            vibrato_applied: false,
            arpeggio_applied: false,
            vibrato_restart: false,
            vibrato_held_wave: 0.0,
            vibrato_waveform: 0,
            vibrato_retrigger: true,
            tremolo_speed: 0.0,
            tremolo_depth: 0.0,
            tremolo_pos: 0,
            tremolo_waveform: 0,
            tremolo_retrigger: true,
            tremolo_applied: false,
            arpeggio_x: 0,
            arpeggio_y: 0,
            volume_slide: NO_SLIDE,
            pan_slide_speed: 0.0,
            volume_column_slide: 0.0,
            volume_column_pan_slide: 0.0,
            retrigger_interval: 0,
            retrigger_tick: 0,
            retrigger_vol_change: 0,
            last_retrigger: 0,
            tremor_pos: 0,
            last_tremor: 0,
            glissando: false,
            note_cut_tick: -1,
            note_delay_tick: -1,
            delayed_note: None,
            delayed_volume: None,
            has_active_voice: false,
            instrument: None,
            last_porta_up: 0,
            last_porta_down: 0,
            last_tone_porta: 0,
            last_vibrato: 0,
            last_tremolo: 0,
            last_vol_slide: 0,
            last_arpeggio: 0,
            last_effect_info: 0,
            last_sample_offset: 0,
            last_fine_porta_up: 0,
            last_fine_porta_down: 0,
            last_fine_vol_up: 0,
            last_fine_vol_down: 0,
            last_extra_fine_porta_up: 0,
            last_extra_fine_porta_down: 0,
            carry_delayed_note: None,
        }
    }

    /// Resets what a new note resets. `keep_vibrato_phase`: ST3 leaves the
    /// vibrato wave alone on an AdLib note.
    pub(crate) fn reset_for_note(&mut self, keep_vibrato_phase: bool) {
        if self.profile.st3_vibrato {
            if !keep_vibrato_phase {
                self.vibrato_pos = 0;
                self.vibrato_restart = false;
            }
        } else if self.vibrato_retrigger {
            self.vibrato_pos = 0;
        }
        self.vibrato_applied = false;
        self.vibrato_held_wave = 0.0;
        if self.tremolo_retrigger {
            self.tremolo_pos = 0;
        }
        self.tremolo_applied = false;
        self.retrigger_tick = 0;
        self.note_cut_tick = -1;
        self.note_delay_tick = -1;
        self.delayed_note = None;
        self.delayed_volume = None;
        self.tone_porta_active = false;
        self.has_active_voice = false;
    }

    fn update_pitch_from_period(&mut self, period: f64) {
        let pitch = self.profile.pitch;
        let clamped = pitch.clamp_period(period);
        self.current_period = Some(clamped);
        let frequency = pitch.frequency_from_period(clamped);
        self.current_frequency = frequency;
        self.current_midi = frequency_to_midi(frequency);
    }

    fn update_pitch_from_frequency(&mut self, frequency: f64) {
        self.current_frequency = frequency;
        self.current_midi = frequency_to_midi(frequency);
        if self.current_period.is_some() {
            let pitch = self.profile.pitch;
            self.current_period =
                Some(pitch.clamp_period(pitch.raw_period_from_frequency(frequency)));
        }
    }

    fn apply_fine_portamento(&mut self, units: f64, unit_scale: f64) {
        let delta = units * unit_scale;
        if let Some(period) = self.current_period {
            self.update_pitch_from_period(period - delta);
        } else {
            let ratio = 2f64.powf(units / (12.0 * 16.0));
            self.update_pitch_from_frequency(self.current_frequency * ratio);
        }
    }

    fn apply_portamento_step(&mut self) {
        let speed = self.portamento_speed;
        if speed == 0.0 {
            return;
        }
        if let Some(period) = self.current_period {
            let delta = speed.abs() * self.profile.portamento_unit_scale;
            let next = if speed > 0.0 {
                period - delta
            } else {
                period + delta
            };
            self.update_pitch_from_period(next);
        } else {
            let ratio = 2f64.powf(speed / (12.0 * 16.0));
            self.update_pitch_from_frequency(self.current_frequency * ratio);
        }
    }

    fn apply_st3_vibrato_info(&mut self, effect: &Effect, fine: bool) {
        let mut info = effect.param();
        if info == 0 {
            info = self.last_vibrato;
        }
        if info & 0xf0 == 0 {
            info = (self.last_vibrato & 0xf0) | (info & 0x0f);
        }
        self.last_vibrato = info;
        self.vibrato_speed = (info >> 4) as f64;
        self.vibrato_depth = (info & 0x0f) as f64 / if fine { 4.0 } else { 1.0 };
    }

    fn vibrato_tick(&mut self) -> f64 {
        if self.profile.st3_vibrato {
            self.advance_st3_vibrato()
        } else {
            self.advance_vibrato()
        }
    }

    fn vibrato_frequency(&self, wave: f64) -> f64 {
        let Some(period) = self.current_period else {
            let semitones = wave * self.vibrato_depth / 16.0;
            return self.current_frequency * 2f64.powf(-semitones / 12.0);
        };
        let pitch = self.profile.pitch;
        if self.profile.st3_vibrato {
            let dat = js_round(wave * VIBRATO_TABLE_PEAK);
            let delta = (dat * self.vibrato_depth / 32.0).floor();
            return pitch.frequency_from_period(pitch.clamp_period(period + delta));
        }
        let magnitude = js_round(wave.abs() * VIBRATO_TABLE_PEAK);
        let sign = if wave > 0.0 {
            1.0
        } else if wave < 0.0 {
            -1.0
        } else {
            0.0
        };
        let delta = sign
            * (magnitude * self.vibrato_depth * self.profile.portamento_unit_scale
                / VIBRATO_DEPTH_DIVISOR)
                .trunc();
        pitch.frequency_from_period(pitch.clamp_period(period + delta))
    }

    fn advance_vibrato(&mut self) -> f64 {
        let wave = waveform_value(self.vibrato_pos, self.vibrato_waveform);
        self.vibrato_applied = true;
        self.vibrato_held_wave = wave;
        let frequency = self.vibrato_frequency(wave);
        self.vibrato_pos += self.vibrato_speed as i64;
        frequency
    }

    fn advance_st3_vibrato(&mut self) -> f64 {
        let mut pos = self.vibrato_pos & 63;
        if self.vibrato_restart && self.vibrato_retrigger {
            pos = 0;
        }
        self.vibrato_restart = false;
        let dat = match self.vibrato_waveform & 3 {
            1 => {
                if pos == 0 {
                    0
                } else if pos < 32 {
                    -256 + pos * 8
                } else {
                    (pos - 32) * 8
                }
            }
            2 => {
                if pos < 32 {
                    0xff
                } else {
                    0
                }
            }
            _ => {
                let v = ST3_SINE[(pos & 31) as usize] as i64;
                if pos < 32 {
                    v
                } else {
                    -v
                }
            }
        };
        self.vibrato_applied = true;
        self.vibrato_held_wave = dat as f64 / VIBRATO_TABLE_PEAK;
        self.vibrato_pos = (pos + self.vibrato_speed as i64) & 63;
        self.vibrato_frequency(self.vibrato_held_wave)
    }

    fn resolve_tone_porta_speed(&mut self, effect: &Effect) -> f64 {
        let value = effect.param();
        if value > 0 {
            self.last_tone_porta = value;
            return value as f64;
        }
        if self.last_tone_porta > 0 {
            self.last_tone_porta as f64
        } else {
            self.tone_porta_speed
        }
    }

    fn apply_tone_porta_step(&mut self) -> f64 {
        if self.tone_porta_speed <= 0.0 {
            return self.current_frequency;
        }
        if self.current_period.is_none() && self.target_period.is_some() {
            self.current_period = Some(
                self.profile
                    .pitch
                    .period_from_frequency(self.current_frequency),
            );
        }
        if let (Some(current), Some(target)) = (self.current_period, self.target_period) {
            let step = self.tone_porta_speed * self.profile.portamento_unit_scale;
            let next = if current > target {
                target.max(current - step)
            } else if current < target {
                target.min(current + step)
            } else {
                current
            };
            self.update_pitch_from_period(next);
        } else {
            let ratio = 2f64.powf(self.tone_porta_speed / (12.0 * 16.0));
            let mut next = self.current_frequency;
            if self.current_frequency < self.target_frequency {
                next *= ratio;
                if next >= self.target_frequency {
                    next = self.target_frequency;
                }
            } else if self.current_frequency > self.target_frequency {
                next /= ratio;
                if next <= self.target_frequency {
                    next = self.target_frequency;
                }
            }
            self.update_pitch_from_frequency(next);
        }
        if self.glissando {
            if let Some(period) = self.current_period {
                let snapped = self.profile.pitch.snap_period(period);
                self.update_pitch_from_period(snapped);
            } else {
                let snapped = js_round(frequency_to_midi(self.current_frequency));
                self.update_pitch_from_frequency(midi_to_frequency(snapped));
            }
        }
        self.current_frequency
    }

    fn resolve_volume_slide(&mut self, x: u8, y: u8) -> VolumeSlide {
        let mut raw = ((x & 0x0f) << 4) | (y & 0x0f);
        if self.profile.volume_slide_has_memory {
            if raw == 0 {
                raw = self.last_vol_slide;
            }
            self.last_vol_slide = raw;
        }
        let scale = self.profile.volume_slide_unit;
        let up = (raw >> 4) & 0x0f;
        let down = raw & 0x0f;
        let slide = |delta: f64, mode, first_tick| VolumeSlide {
            delta,
            mode,
            first_tick,
        };
        if self.profile.volume_slide_nibbles == SlideNibbles::S3m {
            if down == 0x0f && up != 0 {
                return slide(up as f64 * scale, SlideMode::Fine, false);
            }
            if up == 0x0f && down != 0 {
                return slide(-(down as f64) * scale, SlideMode::Fine, false);
            }
            let first = self.profile.fast_volume_slides || raw == 0x0f || raw == 0xf0;
            if down != 0 {
                return slide(-(down as f64) * scale, SlideMode::Normal, first);
            }
            if up != 0 {
                return slide(up as f64 * scale, SlideMode::Normal, first);
            }
            return slide(0.0, SlideMode::Normal, false);
        }
        let first = self.profile.fast_volume_slides;
        if up != 0 {
            return slide(up as f64 * scale, SlideMode::Normal, first);
        }
        if down != 0 {
            return slide(-(down as f64) * scale, SlideMode::Normal, first);
        }
        slide(0.0, SlideMode::Normal, false)
    }

    fn set_slide(&mut self, resolved: VolumeSlide) {
        self.volume_slide = if resolved.delta == 0.0 {
            NO_SLIDE
        } else {
            resolved
        };
    }

    fn prime_volume_slide(&mut self, effect: &Effect) {
        match effect.kind {
            EffectType::VolSlide => {
                let unit = self.profile.volume_slide_unit;
                if effect.sub == Some(ExtSub::FineVolUp) {
                    let mut param = effect.y;
                    if self.profile.fine_slide_has_memory {
                        if param == 0 {
                            param = self.last_fine_vol_up;
                        }
                        self.last_fine_vol_up = param;
                    }
                    self.set_slide(VolumeSlide {
                        delta: param as f64 * unit,
                        mode: SlideMode::Fine,
                        first_tick: false,
                    });
                    return;
                }
                if effect.sub == Some(ExtSub::FineVolDown) {
                    let mut param = effect.y;
                    if self.profile.fine_slide_has_memory {
                        if param == 0 {
                            param = self.last_fine_vol_down;
                        }
                        self.last_fine_vol_down = param;
                    }
                    self.set_slide(VolumeSlide {
                        delta: -(param as f64) * unit,
                        mode: SlideMode::Fine,
                        first_tick: false,
                    });
                    return;
                }
                let resolved = self.resolve_volume_slide(effect.x, effect.y);
                self.set_slide(resolved);
            }
            EffectType::TonePortaVol | EffectType::VibratoVol => {
                let resolved = self.resolve_volume_slide(effect.x, effect.y);
                self.set_slide(resolved);
            }
            _ => {}
        }
    }

    fn emit_tick0_volume_slide(&mut self, out: &mut Commands, stated_this_row: bool) {
        if self.volume_slide.mode != SlideMode::Normal || self.volume_slide.delta == 0.0 {
            return;
        }
        if self.volume_slide.first_tick {
            self.current_volume = clamp_volume(self.current_volume + self.volume_slide.delta);
        }
        out.volume(
            self.current_volume,
            if stated_this_row {
                Ramp::Step
            } else {
                Ramp::Default
            },
        );
    }

    fn apply_volume_slide_if_needed(&mut self) -> Option<f64> {
        if self.volume_slide.mode != SlideMode::Normal || self.volume_slide.delta == 0.0 {
            return None;
        }
        self.current_volume = clamp_volume(self.current_volume + self.volume_slide.delta);
        Some(self.current_volume)
    }

    fn note_on(&self, out: &mut Commands, midi: f64, pan: Option<f64>, offset: Option<u32>) {
        out.push(Command::NoteOn {
            midi,
            velocity: velocity_from_volume(self.current_volume),
            frequency: self.current_frequency,
            pan,
            offset,
        });
    }

    /// Tick 0 of a row: the row's note, volume and effect take hold.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tick0(
        &mut self,
        effect: Option<&Effect>,
        new_note: Option<f64>,
        new_velocity: Option<u8>,
        note_frequency: Option<f64>,
        ticks_per_row: Option<u32>,
        pan: Option<f64>,
        volume_column_volume: bool,
        volume_column_tone_porta: bool,
        out: &mut Commands,
    ) {
        out.clear();
        let kind = effect.map(|e| e.kind);
        let has_note_delay = kind == Some(EffectType::NoteDelay);

        // 9xx rides along with the note it starts.
        let mut pending_offset = None;
        if let Some(e) = effect.filter(|e| e.kind == EffectType::SampleOffset) {
            let mut raw = e.param() as u32;
            if raw == 0 {
                raw = self.last_sample_offset;
            }
            self.last_sample_offset = raw;
            pending_offset = Some(raw * SAMPLE_OFFSET_FRAMES_PER_UNIT);
        }

        self.volume_slide = NO_SLIDE;

        // ST3's E/F fold a vibrato's offset into the note.
        if self.profile.st3_vibrato
            && new_note.is_none()
            && self.vibrato_applied
            && matches!(
                kind,
                Some(
                    EffectType::PortaUp
                        | EffectType::PortaDown
                        | EffectType::FinePortaUp
                        | EffectType::FinePortaDown
                )
            )
        {
            if let Some(period) = self.current_period {
                let dat = js_round(self.vibrato_held_wave * VIBRATO_TABLE_PEAK);
                self.update_pitch_from_period(period + (dat * self.vibrato_depth / 32.0).floor());
                self.vibrato_applied = false;
                self.vibrato_held_wave = 0.0;
            }
        }
        self.arpeggio_applied = false;

        let mut triggered = false;

        // ProTracker: an over-long EDx on the last row plays now.
        if effect.is_none() && new_note.is_none() {
            if let Some(carry) = self.carry_delayed_note.take() {
                self.current_midi = carry.midi;
                if let Some(frequency) = carry.frequency {
                    let period = self.profile.pitch.period_from_frequency(frequency);
                    self.update_pitch_from_period(period);
                    self.current_frequency = frequency;
                } else {
                    self.current_frequency = midi_to_frequency(carry.midi);
                }
                self.target_midi = carry.midi;
                self.target_frequency = self.current_frequency;
                self.target_period = None;
                self.current_volume = carry.velocity / 255.0;
                self.note_on(out, carry.midi, pan, pending_offset);
                self.has_active_voice = true;
                out.pitch(self.current_frequency, None);
                out.volume(self.current_volume, Ramp::Default);
                return;
            }
        }

        let defers_cell = has_note_delay && self.profile.note_delay_defers_cell;
        if let Some(v) = new_velocity.filter(|_| !defers_cell) {
            self.current_volume = v as f64 / 255.0;
        }

        if let Some(note) = new_note {
            let tone_porta = matches!(kind, Some(EffectType::TonePorta | EffectType::TonePortaVol))
                || volume_column_tone_porta;
            if tone_porta {
                self.target_midi = note;
                let target = note_frequency.unwrap_or_else(|| midi_to_frequency(note));
                self.target_frequency = target;
                self.target_period = if self.profile.pitch.is_amiga()
                    || note_frequency.is_some()
                    || self.current_period.is_some()
                {
                    Some(self.profile.pitch.period_from_frequency(target))
                } else {
                    None
                };
                self.last_tone_porta_target_freq = Some(self.target_frequency);
                self.last_tone_porta_target_period = self.target_period;
                self.tone_porta_active = self.tone_porta_speed > 0.0;
                if !self.has_active_voice {
                    self.update_pitch_from_frequency(target);
                    self.note_on(out, note, pan, pending_offset);
                    self.has_active_voice = true;
                    triggered = true;
                }
            } else {
                let pitch = self.profile.pitch;
                if let Some(frequency) = note_frequency {
                    self.update_pitch_from_period(pitch.raw_period_from_frequency(frequency));
                } else if pitch.is_amiga() {
                    self.update_pitch_from_period(
                        pitch.raw_period_from_frequency(midi_to_frequency(note)),
                    );
                } else {
                    self.current_period = None;
                    self.update_pitch_from_frequency(midi_to_frequency(note));
                }
                self.target_midi = note;
                self.target_frequency = self.current_frequency;
                self.target_period = self.current_period;
                if !has_note_delay {
                    self.note_on(out, note, pan, pending_offset);
                    self.has_active_voice = true;
                    triggered = true;
                }
            }
        }

        if triggered {
            out.volume(self.current_volume, Ramp::Step);
        }

        if let Some(e) = effect {
            self.tick0_effect(
                e,
                new_note,
                new_velocity,
                ticks_per_row,
                defers_cell,
                volume_column_volume,
                note_frequency,
                out,
            );
        }

        // A tremolo that stopped hands the channel volume back.
        if self.tremolo_applied
            && self.tremolo_depth > 0.0
            && kind != Some(EffectType::Tremolo)
            && !out
                .as_slice()
                .iter()
                .any(|c| matches!(c, Command::Volume { .. } | Command::NoteOn { .. }))
        {
            out.volume(self.current_volume, Ramp::Default);
            self.tremolo_applied = false;
        }

        // Re-state the pitch, holding a running vibrato's offset.
        let vibrato_row = matches!(
            kind,
            Some(EffectType::Vibrato | EffectType::VibratoVol | EffectType::FineVibrato)
        );
        let st3_holds = self.profile.st3_vibrato
            && effect.is_some()
            && kind != Some(EffectType::VolSlide)
            && !vibrato_row;
        let snaps_back =
            self.profile.pitch_resets_after_effectless_row && !vibrato_row && !st3_holds;
        let continues = new_note.is_none()
            && (vibrato_row || effect.is_none() || st3_holds)
            && !snaps_back
            && self.vibrato_applied
            && self.vibrato_depth > 0.0;
        if snaps_back && new_note.is_none() && self.vibrato_applied {
            self.vibrato_applied = false;
            self.vibrato_held_wave = 0.0;
        }
        let holds_for_delay = has_note_delay && new_note.is_some();
        if !holds_for_delay && !out.has_pitch() {
            let frequency = if continues {
                self.vibrato_frequency(self.vibrato_held_wave)
            } else {
                self.current_frequency
            };
            out.pitch(frequency, None);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tick0_effect(
        &mut self,
        effect: &Effect,
        new_note: Option<f64>,
        new_velocity: Option<u8>,
        ticks_per_row: Option<u32>,
        defers_cell: bool,
        volume_column_volume: bool,
        note_frequency: Option<f64>,
        out: &mut Commands,
    ) {
        match effect.kind {
            EffectType::PortaUp | EffectType::PortaDown => {
                let up = effect.kind == EffectType::PortaUp;
                let raw = effect.param();
                let resolved = if raw != 0 {
                    raw
                } else if up {
                    self.last_porta_up
                } else {
                    self.last_porta_down
                };
                if raw != 0 {
                    if up {
                        self.last_porta_up = raw;
                    } else {
                        self.last_porta_down = raw;
                    }
                }
                if self.profile.fine_porta_high_params && resolved >= 0xe0 {
                    if resolved >= 0xe1 {
                        let units =
                            (resolved & 0x0f) as f64 * if resolved >= 0xf1 { 4.0 } else { 1.0 };
                        if units > 0.0 {
                            self.apply_fine_portamento(if up { units } else { -units }, 1.0);
                            out.pitch(self.current_frequency, None);
                        }
                    }
                    self.portamento_speed = 0.0;
                    return;
                }
                if up {
                    self.portamento_speed = resolved as f64;
                    self.last_porta_up = resolved;
                } else {
                    self.portamento_speed = -(resolved as f64);
                    self.last_porta_down = resolved;
                }
            }
            EffectType::TonePorta => {
                self.tone_porta_speed = self.resolve_tone_porta_speed(effect);
                if let Some(f) = self.last_tone_porta_target_freq {
                    self.target_frequency = f;
                }
                if let Some(p) = self.last_tone_porta_target_period {
                    self.target_period = Some(p);
                }
                self.tone_porta_active = self.tone_porta_speed > 0.0;
            }
            EffectType::Vibrato => {
                if self.profile.st3_vibrato {
                    self.apply_st3_vibrato_info(effect, false);
                    return;
                }
                if effect.x != 0 {
                    self.vibrato_speed = effect.x as f64;
                }
                if effect.y != 0 {
                    self.vibrato_depth = effect.y as f64;
                }
                self.last_vibrato =
                    ((self.vibrato_speed as u8) << 4) | (self.vibrato_depth as u8 & 0x0f);
            }
            EffectType::TonePortaVol => {
                if self.last_tone_porta > 0 {
                    self.tone_porta_speed = self.last_tone_porta as f64;
                }
                if let Some(f) = self.last_tone_porta_target_freq {
                    self.target_frequency = f;
                }
                if let Some(p) = self.last_tone_porta_target_period {
                    self.target_period = Some(p);
                }
                self.tone_porta_active = self.tone_porta_speed > 0.0;
                self.prime_volume_slide(effect);
                self.emit_tick0_volume_slide(out, new_velocity.is_some());
            }
            EffectType::VibratoVol => {
                self.prime_volume_slide(effect);
                self.emit_tick0_volume_slide(out, new_velocity.is_some());
            }
            EffectType::Tremolo => {
                if effect.x != 0 {
                    self.tremolo_speed = effect.x as f64;
                }
                if effect.y != 0 {
                    self.tremolo_depth = effect.y as f64;
                }
                self.last_tremolo = ((self.tremolo_speed as u8) << 4) | self.tremolo_depth as u8;
            }
            EffectType::Arpeggio => {
                self.arpeggio_x = effect.x;
                self.arpeggio_y = effect.y;
                self.last_arpeggio = effect.param();
                out.pitch(self.current_frequency, None);
            }
            EffectType::VolSlide => {
                self.prime_volume_slide(effect);
                self.emit_tick0_volume_slide(out, new_velocity.is_some());
                if self.volume_slide.mode == SlideMode::Fine && self.volume_slide.delta != 0.0 {
                    self.current_volume =
                        clamp_volume(self.current_volume + self.volume_slide.delta);
                    out.volume(self.current_volume, Ramp::Step);
                    self.volume_slide = NO_SLIDE;
                }
            }
            EffectType::ExtEffect => match effect.sub {
                Some(ExtSub::GlissandoCtrl) => self.glissando = effect.param() != 0,
                Some(ExtSub::SetFinetune) => {
                    let semitones = (self.profile.finetune_from_nibble)(effect.y & 0x0f);
                    let ratio = 2f64.powf(semitones / 12.0);
                    self.target_frequency *= ratio;
                    self.target_midi = frequency_to_midi(self.target_frequency);
                    self.target_period = Some(
                        self.profile
                            .pitch
                            .period_from_frequency(self.target_frequency),
                    );
                    self.update_pitch_from_frequency(self.current_frequency * ratio);
                    out.pitch(self.current_frequency, None);
                }
                _ => {}
            },
            EffectType::SetVolume => {
                self.current_volume = (effect.param() as f64 / 64.0).min(1.0);
                out.volume(self.current_volume, Ramp::Step);
            }
            EffectType::SetPan => {
                self.current_pan = if effect.sub == Some(ExtSub::SetPan) {
                    (effect.y as f64 / 15.0) * 2.0 - 1.0
                } else {
                    (effect.param() as f64 - 128.0) / 128.0
                };
                out.push(Command::Pan {
                    pan: self.current_pan,
                });
            }
            EffectType::FinePortaUp | EffectType::FinePortaDown => {
                let up = effect.kind == EffectType::FinePortaUp;
                let mut param = effect.y;
                if self.profile.fine_slide_has_memory {
                    let memory = if up {
                        &mut self.last_fine_porta_up
                    } else {
                        &mut self.last_fine_porta_down
                    };
                    if param == 0 {
                        param = *memory;
                    }
                    *memory = param;
                }
                let units = param as f64;
                self.apply_fine_portamento(
                    if up { units } else { -units },
                    self.profile.portamento_unit_scale,
                );
                out.pitch(self.current_frequency, None);
            }
            EffectType::SetEnvelopePos => {
                out.push(Command::EnvelopePosition {
                    tick: effect.param() as u32,
                });
            }
            EffectType::ExtraFinePorta => {
                let mut param = effect.y;
                let up = match effect.x {
                    1 => true,
                    2 => false,
                    _ => return,
                };
                if self.profile.fine_slide_has_memory {
                    let memory = if up {
                        &mut self.last_extra_fine_porta_up
                    } else {
                        &mut self.last_extra_fine_porta_down
                    };
                    if param == 0 {
                        param = *memory;
                    }
                    *memory = param;
                }
                self.apply_fine_portamento(if up { param as f64 } else { -(param as f64) }, 1.0);
                out.pitch(self.current_frequency, None);
            }
            EffectType::SetVibratoWave => {
                self.vibrato_waveform = effect.y & 3;
                self.vibrato_retrigger = effect.y & 4 == 0;
            }
            EffectType::SetTremoloWave => {
                self.tremolo_waveform = effect.y & 3;
                self.tremolo_retrigger = effect.y & 4 == 0;
            }
            EffectType::NoteCut => {
                self.note_cut_tick = effect.y as i32;
                if self.note_cut_tick == 0 {
                    self.current_volume = 0.0;
                    out.volume(0.0, Ramp::Step);
                    self.note_cut_tick = -1;
                }
            }
            EffectType::NoteDelay => {
                self.note_delay_tick = effect.y as i32;
                if let Some(note) = new_note {
                    let velocity = match new_velocity {
                        Some(v) if defers_cell => v as f64,
                        _ => js_round(clamp_volume(self.current_volume) * 255.0),
                    };
                    let delayed = DelayedNote {
                        midi: note,
                        velocity,
                        frequency: note_frequency,
                    };
                    self.delayed_note = Some(delayed);
                    if let Some(tpr) = ticks_per_row {
                        if self.note_delay_tick >= tpr as i32 {
                            self.carry_delayed_note =
                                self.profile.note_delay_overflow_carries.then_some(delayed);
                            self.delayed_note = None;
                            self.note_delay_tick = -1;
                        }
                    }
                } else if let Some(v) = new_velocity.filter(|_| defers_cell) {
                    self.delayed_volume = Some(v as f64);
                }
            }
            EffectType::RetrigVol => {
                let extended = effect.sub == Some(ExtSub::Retrigger);
                let mut interval = effect.y;
                let mut change = if extended { 0 } else { effect.x };
                if !extended {
                    if interval == 0 {
                        interval = self.last_retrigger & 0x0f;
                    }
                    if change == 0 {
                        change = (self.last_retrigger >> 4) & 0x0f;
                    }
                    self.last_retrigger = (change << 4) | interval;
                }
                self.retrigger_interval = interval;
                self.retrigger_vol_change = change;
                if extended {
                    self.retrigger_tick = 0;
                    return;
                }
                self.retrigger_tick = if volume_column_volume { 0 } else { 1 };
                if self.retrigger_interval > 0 && self.retrigger_tick >= self.retrigger_interval {
                    self.retrigger_tick = 0;
                    out.push(Command::Retrigger {
                        midi: self.current_midi,
                        velocity: velocity_from_volume(self.current_volume),
                        frequency: self.current_frequency,
                    });
                }
            }
            EffectType::KeyOff => {
                if effect.param() == 0 {
                    out.push(Command::NoteOff);
                    self.has_active_voice = false;
                }
            }
            EffectType::FineVibrato => {
                if self.profile.st3_vibrato {
                    self.apply_st3_vibrato_info(effect, true);
                    return;
                }
                if effect.x != 0 {
                    self.vibrato_speed = effect.x as f64;
                }
                if effect.y != 0 {
                    self.vibrato_depth = effect.y as f64 / 4.0;
                }
            }
            EffectType::PanSlide => {
                if effect.x != 0 {
                    self.pan_slide_speed = effect.x as f64 * self.profile.pan_slide_unit;
                } else if effect.y != 0 {
                    self.pan_slide_speed = -(effect.y as f64) * self.profile.pan_slide_unit;
                }
            }
            EffectType::Tremor => {
                let raw = effect.param();
                if raw != 0 {
                    self.last_tremor = raw;
                }
            }
            _ => {}
        }
    }

    /// Ticks 1 and on of a row.
    pub(crate) fn tick_n(
        &mut self,
        effect: Option<&Effect>,
        tick: u32,
        ticks_per_row: u32,
        out: &mut Commands,
    ) {
        out.clear();
        if self.note_cut_tick == tick as i32 {
            self.current_volume = 0.0;
            out.volume(0.0, Ramp::Step);
            self.note_cut_tick = -1;
        }
        if self.note_delay_tick == tick as i32 {
            if let Some(delayed) = self.delayed_note.take() {
                let frequency = delayed
                    .frequency
                    .unwrap_or_else(|| midi_to_frequency(delayed.midi));
                out.push(Command::NoteOn {
                    midi: delayed.midi,
                    velocity: velocity_from_volume(delayed.velocity / 255.0),
                    frequency,
                    pan: None,
                    offset: None,
                });
                self.current_midi = delayed.midi;
                if let Some(f) = delayed.frequency {
                    let period = self.profile.pitch.period_from_frequency(f);
                    self.update_pitch_from_period(period);
                }
                self.current_frequency = frequency;
                self.target_midi = delayed.midi;
                self.target_frequency = self.current_frequency;
                self.target_period = None;
                self.current_volume = delayed.velocity / 255.0;
                self.has_active_voice = true;
                self.note_delay_tick = -1;
                out.pitch(self.current_frequency, None);
                out.volume(self.current_volume, Ramp::Default);
            } else if let Some(v) = self.delayed_volume.take() {
                self.current_volume = v / 255.0;
                self.note_delay_tick = -1;
                out.volume(self.current_volume, Ramp::Step);
            }
        }

        let Some(effect) = effect else {
            if self.profile.tone_porta_continues_through_empty_rows
                && self.tone_porta_active
                && self.tone_porta_speed > 0.0
            {
                self.tone_porta_tick(out);
            }
            return;
        };

        match effect.kind {
            EffectType::PortaUp | EffectType::PortaDown => {
                self.apply_portamento_step();
                out.pitch(self.current_frequency, None);
            }
            EffectType::TonePorta | EffectType::TonePortaVol => {
                self.tone_porta_tick(out);
                if effect.kind == EffectType::TonePortaVol {
                    if let Some(v) = self.apply_volume_slide_if_needed() {
                        out.volume(v, Ramp::Default);
                    }
                }
            }
            EffectType::Vibrato | EffectType::FineVibrato => {
                let f = self.vibrato_tick();
                out.pitch(f, None);
            }
            EffectType::VibratoVol => {
                let f = self.vibrato_tick();
                out.pitch(f, None);
                if let Some(v) = self.apply_volume_slide_if_needed() {
                    out.volume(v, Ramp::Default);
                }
            }
            EffectType::Tremolo => {
                self.tremolo_applied = true;
                let offset = waveform_value(self.tremolo_pos, self.tremolo_waveform);
                let amount =
                    offset * VIBRATO_TABLE_PEAK * self.tremolo_depth / TREMOLO_DEPTH_DIVISOR / 64.0;
                out.volume(clamp_volume(self.current_volume + amount), Ramp::Default);
                self.tremolo_pos += self.tremolo_speed as i64;
            }
            EffectType::Arpeggio => {
                let step = (self.profile.arpeggio_step)(tick, ticks_per_row);
                let offset = match step {
                    1 => self.arpeggio_x,
                    2 => self.arpeggio_y,
                    _ => 0,
                };
                self.arpeggio_applied = offset != 0;
                if let Some(period) = self.current_period {
                    let pitch = self.profile.pitch;
                    let p = pitch.arpeggio_period(period, offset as i32);
                    let f = if p == 0.0 {
                        0.0
                    } else {
                        pitch.frequency_from_period(p)
                    };
                    out.pitch(f, Some(PitchSource::Table));
                } else {
                    let note = self.current_midi + offset as f64;
                    out.pitch(midi_to_frequency(note), Some(PitchSource::Table));
                }
            }
            EffectType::VolSlide => {
                if self.volume_slide.mode == SlideMode::Normal {
                    if let Some(v) = self.apply_volume_slide_if_needed() {
                        out.volume(v, Ramp::Default);
                    }
                }
            }
            EffectType::PanSlide => {
                self.current_pan = (self.current_pan + self.pan_slide_speed).clamp(-1.0, 1.0);
                out.push(Command::Pan {
                    pan: self.current_pan,
                });
            }
            EffectType::RetrigVol => {
                self.retrigger_tick += 1;
                if self.retrigger_interval > 0 && self.retrigger_tick >= self.retrigger_interval {
                    self.retrigger_tick = 0;
                    if effect.sub != Some(ExtSub::Retrigger) {
                        let v = &mut self.current_volume;
                        match self.retrigger_vol_change {
                            1 => *v -= 1.0 / 64.0,
                            2 => *v -= 2.0 / 64.0,
                            3 => *v -= 4.0 / 64.0,
                            4 => *v -= 8.0 / 64.0,
                            5 => *v -= 16.0 / 64.0,
                            6 => *v *= 11.0 / 16.0,
                            7 => *v *= 0.5,
                            9 => *v += 1.0 / 64.0,
                            10 => *v += 2.0 / 64.0,
                            11 => *v += 4.0 / 64.0,
                            12 => *v += 8.0 / 64.0,
                            13 => *v += 16.0 / 64.0,
                            14 => *v *= 1.5,
                            15 => *v *= 2.0,
                            _ => {}
                        }
                        *v = clamp_volume(*v);
                    }
                    out.push(Command::Retrigger {
                        midi: self.current_midi,
                        velocity: velocity_from_volume(self.current_volume),
                        frequency: self.current_frequency,
                    });
                }
            }
            EffectType::Tremor => {
                let mut raw = effect.param();
                if raw == 0 {
                    raw = self.last_tremor;
                }
                let on = ((raw >> 4) & 0x0f) as u32 + 1;
                let off = (raw & 0x0f) as u32 + 1;
                let in_on = self.tremor_pos < on;
                self.tremor_pos = (self.tremor_pos + 1) % (on + off);
                out.volume(if in_on { self.current_volume } else { 0.0 }, Ramp::Default);
            }
            EffectType::KeyOff if tick == effect.param() as u32 => {
                out.push(Command::NoteOff);
                self.has_active_voice = false;
            }
            _ => {}
        }
    }

    fn tone_porta_tick(&mut self, out: &mut Commands) {
        let before = self.current_frequency;
        let f = self.apply_tone_porta_step();
        let arrived = self.target_frequency == self.current_frequency;
        if (f - before).abs() > 1e-9 {
            out.pitch(f, arrived.then_some(PitchSource::Target));
        }
        if arrived {
            self.tone_porta_active = false;
        }
    }

    /// Tick 0 of an XM volume-column command, after the effect column's.
    pub(crate) fn volume_column_tick0(
        &mut self,
        command: Option<VolumeCommand>,
        out: &mut Commands,
    ) {
        out.clear();
        self.volume_column_slide = 0.0;
        self.volume_column_pan_slide = 0.0;
        let Some(command) = command else {
            return;
        };
        let unit = self.profile.volume_slide_unit;
        match command {
            VolumeCommand::SlideDown(v) => {
                self.volume_column_slide = -(v as f64) * unit;
                if self.volume_column_slide != 0.0 {
                    out.volume(self.current_volume, Ramp::Default);
                }
            }
            VolumeCommand::SlideUp(v) => {
                self.volume_column_slide = v as f64 * unit;
                if self.volume_column_slide != 0.0 {
                    out.volume(self.current_volume, Ramp::Default);
                }
            }
            VolumeCommand::FineDown(v) => {
                self.current_volume = clamp_volume(self.current_volume - v as f64 * unit);
                out.volume(self.current_volume, Ramp::Step);
            }
            VolumeCommand::FineUp(v) => {
                self.current_volume = clamp_volume(self.current_volume + v as f64 * unit);
                out.volume(self.current_volume, Ramp::Step);
            }
            VolumeCommand::VibratoSpeed(v) => {
                if v != 0 {
                    self.vibrato_speed = v as f64;
                }
            }
            VolumeCommand::Vibrato(v) => {
                if v != 0 {
                    self.vibrato_depth = v as f64;
                }
            }
            VolumeCommand::SetPan(v) => {
                self.current_pan = ((v as f64) * 16.0) / 128.0 - 1.0;
                out.push(Command::Pan {
                    pan: self.current_pan,
                });
            }
            VolumeCommand::PanSlideLeft(v) => {
                self.volume_column_pan_slide = -(v as f64) * self.profile.pan_slide_unit;
            }
            VolumeCommand::PanSlideRight(v) => {
                self.volume_column_pan_slide = v as f64 * self.profile.pan_slide_unit;
            }
            VolumeCommand::TonePorta(v) => {
                if v > 0 {
                    self.tone_porta_speed = v as f64;
                    self.last_tone_porta = v;
                } else if self.last_tone_porta > 0 {
                    self.tone_porta_speed = self.last_tone_porta as f64;
                }
                if let Some(f) = self.last_tone_porta_target_freq {
                    self.target_frequency = f;
                }
                if let Some(p) = self.last_tone_porta_target_period {
                    self.target_period = Some(p);
                }
                self.tone_porta_active = self.tone_porta_speed > 0.0;
            }
        }
    }

    /// Ticks 1 and on of an XM volume-column command.
    pub(crate) fn volume_column_tick_n(
        &mut self,
        command: Option<VolumeCommand>,
        out: &mut Commands,
    ) {
        out.clear();
        let Some(command) = command else {
            return;
        };
        match command {
            VolumeCommand::SlideDown(_) | VolumeCommand::SlideUp(_) => {
                if self.volume_column_slide != 0.0 {
                    self.current_volume =
                        clamp_volume(self.current_volume + self.volume_column_slide);
                    out.volume(self.current_volume, Ramp::Default);
                }
            }
            VolumeCommand::PanSlideLeft(_) | VolumeCommand::PanSlideRight(_) => {
                if self.volume_column_pan_slide != 0.0 {
                    self.current_pan =
                        (self.current_pan + self.volume_column_pan_slide).clamp(-1.0, 1.0);
                    out.push(Command::Pan {
                        pan: self.current_pan,
                    });
                }
            }
            VolumeCommand::Vibrato(_) => {
                let f = self.advance_vibrato();
                out.pitch(f, None);
            }
            VolumeCommand::TonePorta(_) => {
                if !self.tone_porta_active || self.tone_porta_speed <= 0.0 {
                    return;
                }
                let before = self.current_frequency;
                let f = self.apply_tone_porta_step();
                if self.target_frequency == self.current_frequency {
                    self.tone_porta_active = false;
                }
                if (f - before).abs() > 1e-9 {
                    out.pitch(f, None);
                }
            }
            _ => {}
        }
    }
}
