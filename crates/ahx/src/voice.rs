//! One AHX/HVL channel: waveform + envelope + filter + square-wave-mod +
//! ring-mod state, rendered per sample block. Fixed topology (no generic
//! graph), matching `struct hvl_voice` (`hvl_replay.h:91-188`).
//!
//! Ported from three sites in `hvl_replay.c`: the instrument-trigger branch
//! of `hvl_process_step` (878-964), the DSP body of `hvl_process_frame`
//! (1140-1552, i.e. everything after the note-delay/pattern-effect
//! dispatch that `engine.rs` owns), and `hvl_set_audio` (1555-1633).

use super::envelope::AdsrState;
use super::filter_sweep::{bound_bounce_step, FilterSweep};
use super::format::Instrument;
use super::hifi::{HifiBank, HifiOsc};
use super::plist;
use super::waveform::{
    FILTER_ROW_SIZE, WAVELENGTH_OFFSETS, WO_SAWTOOTH_04, WO_SQUARES, WO_TRIANGLE_04, WO_WHITENOISE,
};
use super::wrap_i16;

/// `hvl_replay.h:28`: `Period2Freq(period) = (AMIGA_PAULA_PAL_CLK*65536.f)/period`.
/// `AMIGA_PAULA_PAL_CLK = ((28375160/4)/2) = 3546895` (`hvl_replay.h:19,21,25`,
/// folded at compile time in the reference).
const AMIGA_PAULA_PAL_CLK: f32 = 3_546_895.0;

/// `hvl_process_frame:1573`. The multiply-then-divide is done in C `float`
/// (32-bit) per the macro's `65536.f` literal; only the final result widens
/// to `double`. Replicated here with an explicit `f32` intermediate so the
/// rounding matches bit-for-bit rather than silently gaining f64 precision.
fn period_to_freq(period: i32) -> f64 {
    let numerator: f32 = AMIGA_PAULA_PAL_CLK * 65536.0f32;
    (numerator / period as f32) as f64
}

/// `vib_tab[64]`, `hvl_tables.c:11-17`.
#[rustfmt::skip]
pub const VIB_TAB: [i16; 64] = [
    0, 24, 49, 74, 97, 120, 141, 161, 180, 197, 212, 224, 235, 244, 250, 253,
    255, 253, 250, 244, 235, 224, 212, 197, 180, 161, 141, 120, 97, 74, 49, 24,
    0, -24, -49, -74, -97, -120, -141, -161, -180, -197, -212, -224, -235, -244, -250, -253,
    -255, -253, -250, -244, -235, -224, -212, -197, -180, -161, -141, -120, -97, -74, -49, -24,
];

/// `period_tab[61]`, `hvl_tables.c:19-29`. 5-octave pitch table (P0
/// correction in `.ai/ahx/verdict.md`), clamp at note 60.
#[rustfmt::skip]
pub const PERIOD_TAB: [u16; 61] = [
    0x0000, 0x0D60, 0x0CA0, 0x0BE8, 0x0B40, 0x0A98, 0x0A00, 0x0970,
    0x08E8, 0x0868, 0x07F0, 0x0780, 0x0714, 0x06B0, 0x0650, 0x05F4,
    0x05A0, 0x054C, 0x0500, 0x04B8, 0x0474, 0x0434, 0x03F8, 0x03C0,
    0x038A, 0x0358, 0x0328, 0x02FA, 0x02D0, 0x02A6, 0x0280, 0x025C,
    0x023A, 0x021A, 0x01FC, 0x01E0, 0x01C5, 0x01AC, 0x0194, 0x017D,
    0x0168, 0x0153, 0x0140, 0x012E, 0x011D, 0x010D, 0x00FE, 0x00F0,
    0x00E2, 0x00D6, 0x00CA, 0x00BE, 0x00B4, 0x00AA, 0x00A0, 0x0097,
    0x008F, 0x0087, 0x007F, 0x0078, 0x0071,
];

/// `stereopan_left`/`stereopan_right`, `hvl_tables.c:31-32`.
pub const STEREOPAN_LEFT: [i32; 5] = [128, 96, 64, 32, 0];
pub const STEREOPAN_RIGHT: [i32; 5] = [128, 160, 193, 225, 255];

/// `hvl_GenPanningTables`, `hvl_tables.c:387-406`, generated once. Mixed
/// precision, as in the reference: the start angle is computed in C `float`
/// (`3.14159265f*2.0f/4.0f`, then widened), but the accumulators are
/// `float64` and the step uses the *double* literal `3.14159265`, `sin()` is
/// the double one, and `255.0f` widens exactly.
static PANNING: std::sync::OnceLock<([i32; 256], [i32; 256])> = std::sync::OnceLock::new();

// The truncated literal 3.14159265 is the reference's, not `PI`: see above.
#[allow(clippy::approx_constant)]
fn panning_tables() -> &'static ([i32; 256], [i32; 256]) {
    PANNING.get_or_init(|| {
        let mut left = [0i32; 256];
        let mut right = [0i32; 256];
        let mut aa: f64 = ((3.141_592_7_f32 * 2.0f32) / 4.0f32) as f64;
        let mut ab: f64 = 0.0;
        let step: f64 = (3.14159265f64 * 2.0f64 / 4.0f64) / 256.0f64;
        for i in 0..256usize {
            left[i] = (aa.sin() * 255.0f64) as u32 as i32;
            right[i] = (ab.sin() * 255.0f64) as u32 as i32;
            aa += step;
            ab += step;
        }
        left[255] = 0;
        right[0] = 0;
        (left, right)
    })
}

pub fn panning_left(pan: usize) -> i32 {
    panning_tables().0[pan]
}

pub fn panning_right(pan: usize) -> i32 {
    panning_tables().1[pan]
}

/// Where a voice's current audio (or ring-mod) source lives: either an
/// absolute byte offset into the shared `waveform::WAVES` table, or "use
/// this voice's own `square_temp_buffer`" (the runtime-computed square-wave
/// slice `hvl_process_frame`'s `CalcSquare` block builds per voice, since
/// squares can't be precomputed the way triangle/sawtooth/noise are --
/// `ht_WaveformTab[2] = voice->vc_SquareTempBuffer`, `hvl_replay.c:1433`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSourceRef {
    Waves(usize),
    SquareTemp,
}

/// `vc_Square*` fields on `struct hvl_voice` (`hvl_replay.h:133-141`).
/// Ported from the trigger setup (`hvl_process_step:915-929`) and the
/// per-frame walk (`hvl_process_frame:1324-1362`); reuses
/// `filter_sweep::bound_bounce_step` for the bound-reversal core (see that
/// function's doc comment for why it's shared with the filter sweep).
#[derive(Debug, Clone, Copy, Default)]
pub struct SquareSweep {
    pub on: bool,
    pub init: bool,
    pub wait: i32,
    pub speed: i32,
    pub lower_limit: i32,
    pub upper_limit: i32,
    pub pos: i32,
    pub sign: i32,
    pub sliding_in: bool,
    pub reverse: bool,
    /// `vc_IgnoreSquare`: consumed by PList command 3
    /// (`hvl_plist_command_parse` case 3) the next time it runs, or by
    /// pattern effect `9xx` (`hvl_process_stepfx_2` case 0x9) which sets it.
    pub ignore: bool,
}

impl SquareSweep {
    /// `hvl_process_step:915-929`. `vc_SquarePos` itself is deliberately
    /// NOT reset here -- the reference only ever initializes it in
    /// `hvl_reset_some_stuff` at engine startup, so it persists across
    /// instrument triggers on the same channel.
    pub fn trigger(&mut self, ins: &Instrument, wave_length: i32) {
        self.ignore = false;
        self.sliding_in = false;
        self.wait = 0;
        self.on = false;
        self.speed = ins.square_speed as i32;
        let shift = (5 - wave_length).max(0) as u32;
        let mut lower = (ins.square_lower_limit as i32) >> shift;
        let mut upper = (ins.square_upper_limit as i32) >> shift;
        if upper < lower {
            std::mem::swap(&mut lower, &mut upper);
        }
        self.lower_limit = lower;
        self.upper_limit = upper;
    }

    /// PList command 4's square toggle (`hvl_plist_command_parse:1019-1029`).
    pub fn toggle(&mut self, sign: i32) {
        self.on = !self.on;
        self.init = self.on;
        self.sign = sign;
    }

    /// `hvl_process_frame:1324-1362` — one tick, gated by the caller on
    /// `waveform == SQUARE && self.on`. Returns `true` if the position
    /// walked this frame (`vc_PlantSquare = 1`).
    pub fn step(&mut self) -> bool {
        self.wait -= 1;
        if self.wait > 0 {
            return false;
        }
        if self.init {
            self.init = false;
            if self.pos <= self.lower_limit {
                self.sliding_in = true;
                self.sign = 1;
            } else if self.pos >= self.upper_limit {
                self.sliding_in = true;
                self.sign = -1;
            }
        }
        self.pos = bound_bounce_step(
            self.pos,
            &mut self.sign,
            &mut self.sliding_in,
            self.lower_limit,
            self.upper_limit,
        );
        self.wait = self.speed;
        true
    }
}

/// Waveform indices, matching `vc_Waveform`'s 0-based encoding
/// (`ple_Waveform - 1`, `hvl_process_frame:1288`).
pub const WAVEFORM_TRIANGLE: i32 = 0;
pub const WAVEFORM_SAWTOOTH: i32 = 1;
pub const WAVEFORM_SQUARE: i32 = 2;
pub const WAVEFORM_NOISE: i32 = 3;

/// One AHX/HVL channel. Field names mirror `struct hvl_voice`
/// (`hvl_replay.h:91-188`) in `snake_case`; grouped by the reference's own
/// grouping (period/pitch, envelope, square, filter, PList, ring-mod, pan).
pub struct Voice {
    pub track: usize,
    pub next_track: usize,
    pub transpose: i32,
    pub next_transpose: i32,
    /// `vc_OverrideTranspose`; `1000` is the reference's "unset" sentinel
    /// (`hvl_replay.c:873,1496,1527`, "1.5" comments).
    pub override_transpose: i32,

    pub instrument_idx: u8,
    pub adsr: AdsrState,

    pub sample_pos: u32,
    pub delta: u32,

    pub instr_period: i32,
    pub track_period: i32,
    /// Signed here; `uint16` in the reference (see `calc_period`).
    pub vibrato_period: i32,
    pub wave_length: i32,

    pub note_max_volume: i32,
    pub perf_sub_volume: i32,

    pub new_waveform: bool,
    pub waveform: i32,
    pub plant_period: bool,
    pub voice_volume: i32,
    pub plant_square: bool,
    pub fixed_note: bool,

    pub volume_slide_up: i32,
    pub volume_slide_down: i32,

    pub hard_cut: i32,
    pub hard_cut_release: bool,
    pub hard_cut_release_f: i32,

    pub period_slide_on: bool,
    pub period_slide_speed: i32,
    pub period_slide_period: i32,
    pub period_slide_limit: i32,
    pub period_slide_with_limit: bool,

    pub period_perf_slide_speed: i32,
    pub period_perf_slide_period: i32,
    pub period_perf_slide_on: bool,

    pub vibrato_delay: i32,
    pub vibrato_speed: i32,
    pub vibrato_current: i32,
    pub vibrato_depth: i32,

    pub square: SquareSweep,
    pub filter: FilterSweep,

    pub perf_current: i32,
    pub perf_speed: i32,
    pub perf_wait: i32,
    /// The PList row the last step ran (`-1`: none since the trigger). Unlike
    /// `perf_current`, which is the row that runs *next* and holds the target
    /// after a Jump. Write-only as far as the render goes: only the editor's
    /// playhead reads it (`AhxEngine::live_plist_state`).
    pub perf_row: i32,

    pub audio_source: AudioSourceRef,
    pub audio_period: i32,
    pub audio_volume: i32,

    pub note_delay_on: bool,
    pub note_delay_wait: i32,
    pub note_cut_on: bool,
    pub note_cut_wait: i32,

    /// `int32 vc_WNRandom` (`hvl_replay.h:164`) -- *signed*, so the `>> 8` in
    /// the noise-seed update is an arithmetic shift, not a rotate (see the
    /// `p3-report.md` note on this deviation from AHX's 68k `ror.l`).
    pub wn_random: i32,

    pub voice_buffer: Vec<i8>,
    /// The band-limited oscillator hi-fi mode picked for this tick (see
    /// `hifi.rs`). Always `None` with hi-fi off, and for noise voices.
    pub hifi: Option<HifiOsc>,
    pub square_temp_buffer: [i8; 0x80],

    pub track_on: bool,
    pub track_master_volume: i32,
    pub voice_period: i32,

    pub pan: u32,
    pub set_pan: u32,
    pub pan_mult_left: i32,
    pub pan_mult_right: i32,

    pub ring_sample_pos: u32,
    pub ring_delta: u32,
    pub ring_plant_period: bool,
    pub ring_base_period: i32,
    pub ring_audio_period: i32,
    pub ring_audio_source: Option<AudioSourceRef>,
    /// `vc_RingMixSource != NULL`: what the mixer actually keys ring
    /// modulation on. Distinct from `ring_audio_source` -- an instrument
    /// trigger clears only this one (`hvl_replay.c:960`), while PList
    /// commands 7/8's off-branch clears both (`:1058-1059`).
    pub ring_mix_active: bool,
    pub ring_new_waveform: bool,
    pub ring_waveform: i32,
    pub ring_fixed_period: bool,
    pub ring_voice_buffer: Vec<i8>,
}

impl Voice {
    /// `hvl_reset_some_stuff`, `hvl_replay.c:26-64`.
    pub fn new() -> Self {
        Voice {
            track: 0,
            next_track: 0,
            transpose: 0,
            next_transpose: 0,
            override_transpose: 1000,
            instrument_idx: 0,
            adsr: AdsrState::default(),
            sample_pos: 0,
            delta: 1,
            instr_period: 0,
            track_period: 0,
            vibrato_period: 0,
            wave_length: 0,
            note_max_volume: 0,
            perf_sub_volume: 0,
            new_waveform: false,
            waveform: 0,
            plant_period: false,
            voice_volume: 0,
            plant_square: false,
            fixed_note: false,
            volume_slide_up: 0,
            volume_slide_down: 0,
            hard_cut: 0,
            hard_cut_release: false,
            hard_cut_release_f: 0,
            period_slide_on: false,
            period_slide_speed: 0,
            period_slide_period: 0,
            period_slide_limit: 0,
            period_slide_with_limit: false,
            period_perf_slide_speed: 0,
            period_perf_slide_period: 0,
            period_perf_slide_on: false,
            vibrato_delay: 0,
            vibrato_speed: 0,
            vibrato_current: 0,
            vibrato_depth: 0,
            square: SquareSweep::default(),
            filter: FilterSweep::default(),
            perf_current: 0,
            perf_speed: 0,
            perf_wait: 0,
            perf_row: -1,
            audio_source: AudioSourceRef::Waves(WO_TRIANGLE_04),
            audio_period: 0,
            audio_volume: 0,
            note_delay_on: false,
            note_delay_wait: 0,
            note_cut_on: false,
            note_cut_wait: 0,
            wn_random: 0x280,
            voice_buffer: vec![0i8; 0x281],
            hifi: None,
            square_temp_buffer: [0i8; 0x80],
            track_on: true,
            track_master_volume: 0x40,
            voice_period: 0,
            pan: 128,
            set_pan: 128,
            pan_mult_left: panning_left(128),
            pan_mult_right: panning_right(128),
            ring_sample_pos: 0,
            ring_delta: 0,
            ring_plant_period: false,
            ring_base_period: 0,
            ring_audio_period: 0,
            ring_audio_source: None,
            ring_mix_active: false,
            ring_new_waveform: false,
            ring_waveform: 0,
            ring_fixed_period: false,
            ring_voice_buffer: vec![0i8; 0x281],
        }
    }

    /// Sets this channel's default pan from the song's stereo-separation
    /// choice, alternating L/R/R/L across channel groups of 4
    /// (`hvl_InitSubsong:90-108`).
    pub fn set_default_pan(&mut self, channel_idx: usize, defstereo: usize) {
        let pan = match channel_idx % 4 {
            0 | 3 => STEREOPAN_LEFT[defstereo],
            _ => STEREOPAN_RIGHT[defstereo],
        } as u32;
        self.pan = pan;
        self.set_pan = pan;
        self.pan_mult_left = panning_left(pan as usize);
        self.pan_mult_right = panning_right(pan as usize);
    }

    /// The instrument-trigger branch of `hvl_process_step`, `hvl_replay.c:878-964`
    /// (everything gated by `Instr && Instr <= InstrumentNr`).
    ///
    /// `continue_phase` is the one deliberate departure from the reference
    /// (see `AhxEngine::set_continue_phase_on_trigger`). `false` is
    /// `hvl_replay.c:893` exactly: the wave read pointer restarts at 0.
    /// `true` leaves `sample_pos` where the free-running mixer has it, which
    /// is what the 68k player does.
    ///
    /// 68k evidence (`.ai/ahx/68k-investigation.md` sections 2a/4): AUDxLC
    /// is written once at init and Paula free-runs over the 640-byte buffer;
    /// an instrument trigger never touches the read pointer, so a new note
    /// starts wherever the previous phase happened to be. `sample_pos = 0` is
    /// a Hively-inherited deviation that starts every note at wave[0], the
    /// worst-case step for a saw or square.
    ///
    /// Pos semantics stay simple: `sample_pos` keeps its value. The 68k's
    /// caveat about the phase inside a *new* wave length is a non-issue here,
    /// because `voice_buffer` is the same 640-byte domain and the waveform is
    /// planted repeated to fill it (`set_audio`: `wave_loops` copies of a
    /// `4 << wave_length` block, and every such block length divides 640), so
    /// the continuing position already lands at `pos mod newLen`.
    /// `ring_sample_pos` is still cleared: the ring modulator is not part of
    /// the 68k evidence.
    pub fn trigger_instrument(
        &mut self,
        instrument_idx: u8,
        ins: &Instrument,
        continue_phase: bool,
    ) {
        self.pan = self.set_pan;
        self.pan_mult_left = panning_left(self.pan as usize);
        self.pan_mult_right = panning_right(self.pan as usize);

        self.period_slide_speed = 0;
        self.period_slide_period = 0;
        self.period_slide_limit = 0;

        self.perf_sub_volume = 0x40;
        self.adsr = AdsrState::trigger(&ins.envelope);
        self.instrument_idx = instrument_idx;
        if !continue_phase {
            self.sample_pos = 0;
        }

        // The decoder masks `wave_length` to 3 bits (0..=7) but the reference's
        // `Offsets[]`/`5 - WaveLength` shifts are only defined for 0..=5
        // (UB beyond). Clamp instead of indexing out of range.
        self.wave_length = (ins.wave_length as i32).min(5);
        self.note_max_volume = ins.volume as i32;

        self.vibrato_current = 0;
        self.vibrato_delay = ins.vibrato_delay as i32;
        self.vibrato_depth = ins.vibrato_depth as i32;
        self.vibrato_speed = ins.vibrato_speed as i32;
        self.vibrato_period = 0;

        self.hard_cut_release = ins.hard_cut_release;
        self.hard_cut = ins.hard_cut_release_frames as i32;

        self.square.trigger(ins, self.wave_length);

        self.filter = FilterSweep::trigger(ins);

        self.perf_wait = 0;
        self.perf_current = 0;
        self.perf_row = -1;
        self.perf_speed = ins.plist.speed as i32;

        // hvl_replay.c:960 -- only the mix source is cleared; vc_RingAudioSource
        // (and so the ring-period calc below) deliberately survives a trigger.
        self.ring_mix_active = false;
        self.ring_sample_pos = 0;
        self.ring_plant_period = false;
        self.ring_new_waveform = false;
    }

    /// The DSP body of `hvl_process_frame`, `hvl_replay.c:1140-1552`
    /// (everything after the note-delay reentry, which `engine.rs` handles
    /// before calling this). `ins` is the currently active instrument (its
    /// PList entries drive the PList stepper); `next_inst` is the upcoming
    /// row's instrument number for this channel, used by hard-cut
    /// (`engine.rs` precomputes this since it needs `Song`/track access this
    /// method deliberately doesn't have).
    pub fn process_frame_dsp(&mut self, ins: &Instrument, waves: &[i8], tempo: i32, next_inst: u8) {
        if !self.track_on {
            return;
        }

        // HardCut (1140-1166).
        if self.hard_cut != 0 && next_inst != 0 {
            let mut d1 = tempo - self.hard_cut;
            if d1 < 0 {
                d1 = 0;
            }
            if !self.note_cut_on {
                self.note_cut_on = true;
                self.note_cut_wait = d1;
                self.hard_cut_release_f = -(d1 - tempo);
            } else {
                self.hard_cut = 0;
            }
        }

        // NoteCutOn (1168-1187).
        if self.note_cut_on {
            if self.note_cut_wait <= 0 {
                self.note_cut_on = false;
                if self.hard_cut_release {
                    self.adsr
                        .hard_cut_release(&ins.envelope, self.hard_cut_release_f);
                } else {
                    self.note_max_volume = 0;
                }
            } else {
                self.note_cut_wait -= 1;
            }
        }

        // ADSR envelope (1189-1214).
        self.adsr.step(&ins.envelope);

        // VolumeSlide (1216-1222).
        self.note_max_volume =
            (self.note_max_volume + self.volume_slide_up - self.volume_slide_down).clamp(0, 0x40);

        // Portamento (1224-1255).
        if self.period_slide_on {
            if self.period_slide_with_limit {
                let d0 = self.period_slide_period - self.period_slide_limit;
                let mut d2 = self.period_slide_speed;
                if d0 > 0 {
                    d2 = -d2;
                }
                if d0 != 0 {
                    let d3 = (d0 + d2) ^ d0;
                    let new_period = if d3 >= 0 {
                        self.period_slide_period + d2
                    } else {
                        self.period_slide_limit
                    };
                    self.period_slide_period = wrap_i16(new_period);
                    self.plant_period = true;
                }
            } else {
                self.period_slide_period =
                    wrap_i16(self.period_slide_period + self.period_slide_speed);
                self.plant_period = true;
            }
        }

        // Vibrato (1257-1268).
        if self.vibrato_depth != 0 {
            if self.vibrato_delay <= 0 {
                self.vibrato_period =
                    (VIB_TAB[self.vibrato_current as usize] as i32 * self.vibrato_depth) >> 7;
                self.plant_period = true;
                self.vibrato_current = (self.vibrato_current + self.vibrato_speed) & 0x3f;
            } else {
                self.vibrato_delay -= 1;
            }
        }

        // PList (1270-1313).
        if self.instrument_idx != 0 {
            if (self.perf_current as usize) < ins.plist.entries.len() {
                let signed_overflow = self.perf_wait == 128;
                self.perf_wait -= 1;
                if signed_overflow || (self.perf_wait as i8) <= 0 {
                    let cur = self.perf_current as usize;
                    self.perf_row = cur as i32;
                    self.perf_current += 1;
                    self.perf_wait = self.perf_speed;

                    let entry = ins.plist.entries[cur];
                    if entry.waveform != 0 {
                        self.waveform = entry.waveform as i32 - 1;
                        self.new_waveform = true;
                        self.period_perf_slide_speed = 0;
                        self.period_perf_slide_period = 0;
                    }
                    self.period_perf_slide_on = false;

                    for k in 0..2 {
                        plist::process_command(self, entry.fx[k] as i32, entry.fx_param[k] as i32);
                    }

                    if entry.note != 0 {
                        self.instr_period = entry.note as i32;
                        self.plant_period = true;
                        self.fixed_note = entry.fixed;
                    }
                }
            } else if self.perf_wait != 0 {
                self.perf_wait -= 1;
            } else {
                self.period_perf_slide_speed = 0;
            }
        }

        // PerfPortamento (1316-1322).
        if self.period_perf_slide_on {
            self.period_perf_slide_period =
                wrap_i16(self.period_perf_slide_period - self.period_perf_slide_speed);
            if self.period_perf_slide_period != 0 {
                self.plant_period = true;
            }
        }

        // Square sweep (1324-1362), gated on waveform==SQUARE && SquareOn.
        if self.waveform == WAVEFORM_SQUARE && self.square.on && self.square.step() {
            self.plant_square = true;
        }

        // Filter sweep (1364-1409).
        if self.filter.step() {
            self.new_waveform = true;
        }

        // CalcSquare (1411-1444).
        if self.waveform == WAVEFORM_SQUARE || self.plant_square {
            self.calc_square(waves);
        }

        // Noise forces a re-plant every frame (1446-1447).
        if self.waveform == WAVEFORM_NOISE {
            self.new_waveform = true;
        }

        // Ring-mod waveform source (1449-1459).
        if self.ring_new_waveform {
            let ring_wave = self.ring_waveform.min(1);
            let base = if ring_wave == 0 {
                WO_TRIANGLE_04
            } else {
                WO_SAWTOOTH_04
            };
            let offset = base + WAVELENGTH_OFFSETS[self.wave_length as usize];
            self.ring_audio_source = Some(AudioSourceRef::Waves(offset));
        }

        // Main AudioSource resolution (1462-1487).
        if self.new_waveform {
            self.audio_source = match self.waveform {
                WAVEFORM_SQUARE => AudioSourceRef::SquareTemp,
                w => {
                    let base = match w {
                        WAVEFORM_TRIANGLE => WO_TRIANGLE_04,
                        WAVEFORM_SAWTOOTH => WO_SAWTOOTH_04,
                        WAVEFORM_NOISE => WO_WHITENOISE,
                        _ => WO_TRIANGLE_04,
                    };
                    let mut off =
                        base as i64 + (self.filter.pos as i64 - 0x20) * FILTER_ROW_SIZE as i64;
                    if w < WAVEFORM_SQUARE {
                        off += WAVELENGTH_OFFSETS[self.wave_length as usize] as i64;
                    }
                    if w == WAVEFORM_NOISE {
                        // hvl_replay.c:1480-1483. int32 semantics: `>>` is
                        // arithmetic, `<<`/`+` wrap (gcc, no UB exploitation).
                        off += ((self.wn_random & (2 * 0x280 - 1)) & !1) as i64;
                        self.wn_random = self.wn_random.wrapping_add(2_239_384);
                        self.wn_random = (((self.wn_random >> 8)
                            | self.wn_random.wrapping_shl(24))
                        .wrapping_add(782_323)
                            ^ 75)
                            .wrapping_sub(6735);
                    }
                    AudioSourceRef::Waves(off as usize)
                }
            };
        }

        // Ring modulation period calculation (1489-1520).
        if self.ring_audio_source.is_some() {
            self.ring_audio_period =
                self.calc_period(self.ring_base_period, self.ring_fixed_period);
        }

        // Normal period calculation (1522-1550).
        self.audio_period = self.calc_period(self.instr_period, self.fixed_note);

        // Final volume (1552).
        self.audio_volume = wrap_i16(
            ((((((self.adsr.volume >> 8) * self.note_max_volume) >> 6) * self.perf_sub_volume)
                >> 6)
                * self.track_master_volume)
                >> 6,
        );
    }

    /// The period computation shared, line for line, by the ring-mod voice
    /// (`hvl_replay.c:1489-1520`, `base = vc_RingBasePeriod`, `fixed =
    /// vc_RingFixedPeriod`) and the main voice (`:1522-1550`, `base =
    /// vc_InstrPeriod`, `fixed = vc_FixedNote`). Each `+=` there stores into
    /// an `int16`, hence `wrap_i16`; `vc_VibratoPeriod` is `uint16` in the
    /// reference, but adding it and storing back to `int16` is identical
    /// mod 2^16 to adding it signed, so it stays a signed `i32` here.
    fn calc_period(&self, base: i32, fixed: bool) -> i32 {
        let mut period = base;
        if !fixed {
            let transpose = if self.override_transpose != 1000 {
                self.override_transpose
            } else {
                self.transpose
            };
            period = wrap_i16(period + transpose + self.track_period - 1);
        }
        period = period.clamp(0, 5 * 12);
        period = PERIOD_TAB[period as usize] as i32;
        if !fixed {
            period = wrap_i16(period + self.period_slide_period);
        }
        period = wrap_i16(period + (self.period_perf_slide_period + self.vibrato_period));
        period.clamp(0x0071, 0x0d60)
    }

    /// `CalcSquare`, `hvl_process_frame:1413-1444`.
    fn calc_square(&mut self, waves: &[i8]) {
        let mut ptr: i64 =
            WO_SQUARES as i64 + (self.filter.pos as i64 - 0x20) * FILTER_ROW_SIZE as i64;
        let shift = (5 - self.wave_length).max(0) as u32;
        let mut x = self.square.pos << shift;
        if x > 0x20 {
            x = 0x40 - x;
            self.square.reverse = true; // set-only in the reference, as well
        }
        if x > 0 {
            ptr += ((x - 1) as i64) << 7;
        }
        let delta = 32 >> self.wave_length;
        let n = (1usize << self.wave_length) * 4;
        for i in 0..n {
            self.square_temp_buffer[i] = waves[ptr as usize];
            ptr += delta as i64;
        }
        self.new_waveform = true;
        self.waveform = WAVEFORM_SQUARE;
        self.plant_square = false;
    }

    /// Hi-fi mode's per-tick pick, run after [`set_audio`](Self::set_audio):
    /// the band-limited oscillator for the cycle the voice is about to play
    /// (`voice_buffer[..4 << wave_length]`, which is whatever table the
    /// waveform, filter row and square duty resolved to) at its current pitch.
    /// Noise voices and silent tables get `None` and play the reference path.
    pub fn select_hifi(&mut self, bank: &mut HifiBank) {
        self.hifi = None;
        if !self.track_on || self.waveform == WAVEFORM_NOISE {
            return;
        }
        let n = 4usize << self.wave_length.clamp(0, 5);
        // `delta` is bytes of the 0x280 buffer per output sample, 16.16; a
        // cycle is `n` bytes.
        let f0 = self.delta as f64 / 65536.0 / n as f64;
        self.hifi = bank.oscillator(&self.voice_buffer[..n], f0);
    }

    /// `hvl_set_audio`, `hvl_replay.c:1555-1633`. `freq_hz` is the output
    /// sample rate (`ht_FreqF`).
    pub fn set_audio(&mut self, waves: &[i8], freq_hz: f64) {
        if !self.track_on {
            self.voice_volume = 0;
            return;
        }
        // `uint8 vc_VoiceVolume` (hvl_replay.h:112).
        self.voice_volume = self.audio_volume as u8 as i32;

        if self.plant_period {
            self.plant_period = false;
            self.voice_period = self.audio_period;
            let freq2 = period_to_freq(self.audio_period);
            let mut delta = (freq2 / freq_hz) as u32;
            if delta > (0x280 << 16) {
                delta -= 0x280 << 16;
            }
            if delta == 0 {
                delta = 1;
            }
            self.delta = delta;
        }

        // Note: the reference never clears vc_NewWaveform (grepped
        // hvl_replay.c: only hvl_reset_some_stuff writes it false), so a
        // triggered voice keeps replanting an identical buffer every frame.
        // Harmless (deterministic content) and ported as-is, not "fixed".
        if self.new_waveform {
            if self.waveform == WAVEFORM_NOISE {
                let source =
                    source_slice(self.audio_source, waves, &self.square_temp_buffer, 0x280);
                self.voice_buffer[0..0x280].copy_from_slice(source);
            } else {
                let block = 4usize * (1usize << self.wave_length);
                let wave_loops = (1usize << ((5 - self.wave_length).max(0) as u32)) * 5;
                let source =
                    source_slice(self.audio_source, waves, &self.square_temp_buffer, block);
                fill_cycles(&mut self.voice_buffer, source, block, wave_loops);
            }
            self.voice_buffer[0x280] = self.voice_buffer[0];
        }

        if self.ring_plant_period {
            self.ring_plant_period = false;
            let freq2 = period_to_freq(self.ring_audio_period);
            let mut delta = (freq2 / freq_hz) as u32;
            if delta > (0x280 << 16) {
                delta -= 0x280 << 16;
            }
            if delta == 0 {
                delta = 1;
            }
            self.ring_delta = delta;
        }

        if self.ring_new_waveform {
            if let Some(source) = self.ring_audio_source {
                let block = 4usize * (1usize << self.wave_length);
                let wave_loops = (1usize << ((5 - self.wave_length).max(0) as u32)) * 5;
                let source = source_slice(source, waves, &self.square_temp_buffer, block);
                fill_cycles(&mut self.ring_voice_buffer, source, block, wave_loops);
                self.ring_voice_buffer[0x280] = self.ring_voice_buffer[0];
                self.ring_mix_active = true;
            }
        }
    }
}

/// The `block` bytes of the wave an [`AudioSourceRef`] names. Takes the square
/// buffer as an argument, not the voice, so the caller can keep writing to its
/// other buffers while it holds the slice (no per-tick copy to get around the
/// borrow).
fn source_slice<'a>(
    source: AudioSourceRef,
    waves: &'a [i8],
    square_temp: &'a [i8],
    block: usize,
) -> &'a [i8] {
    match source {
        AudioSourceRef::Waves(offset) => &waves[offset..offset + block],
        AudioSourceRef::SquareTemp => &square_temp[0..block],
    }
}

/// `dst[..block * loops]` = `src[..block]` repeated `loops` times, by doubling
/// (`hvl_set_audio`'s per-cycle copy loop, with the same result: 160 copies of
/// 4 bytes become 8).
fn fill_cycles(dst: &mut [i8], src: &[i8], block: usize, loops: usize) {
    let total = block * loops;
    if total == 0 {
        return;
    }
    dst[..block].copy_from_slice(&src[..block]);
    let mut filled = block;
    while filled < total {
        let n = filled.min(total - filled);
        dst.copy_within(0..n, filled);
        filled += n;
    }
}

impl Default for Voice {
    fn default() -> Self {
        Voice::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Envelope;
    use crate::waveform;

    fn silent_instrument() -> Instrument {
        Instrument {
            volume: 0x40,
            wave_length: 3, // 8-sample triangle/sawtooth
            envelope: Envelope {
                a_frames: 0,
                a_volume: 0x40,
                d_frames: 0,
                d_volume: 0x40,
                s_frames: 0,
                r_frames: 0,
                r_volume: 0x40,
            },
            ..Default::default()
        }
    }

    #[test]
    fn trigger_resets_expected_fields() {
        let ins = silent_instrument();
        let mut v = Voice::new();
        v.trigger_instrument(1, &ins, false);
        assert_eq!(v.instrument_idx, 1);
        assert_eq!(v.sample_pos, 0);
        assert_eq!(v.wave_length, 3);
        assert_eq!(v.note_max_volume, 0x40);
        assert_eq!(v.filter.pos, 32);
    }

    // `calc_period` stores every `+=` into an `int16` (hvl_replay.c:1541-1544).
    // Unreachable in the fixtures, so pinned here instead of by the goldens.
    #[test]
    fn calc_period_wraps_int16_intermediates_like_the_reference() {
        let mut v = Voice::new();
        v.track_period = 25;
        v.period_slide_period = 0x7fff; // period_tab[25] + 0x7fff overflows int16
                                        // 0x0358 + 0x7fff = 0x8357 -> wraps to -31913 -> clamps up to 0x0071.
        assert_eq!(v.calc_period(1, false), 0x0071);
        // A fixed note skips the slide term entirely.
        assert_ne!(v.calc_period(1, true), 0x0071);
    }

    #[test]
    fn calc_period_negative_vibrato_is_signed_mod_2_16() {
        let mut v = Voice::new();
        v.track_period = 1;
        v.vibrato_period = -30; // uint16 65506 in the reference; same mod 2^16
        let base = PERIOD_TAB[1] as i32;
        assert_eq!(v.calc_period(1, false), base - 30);
    }

    /// The first sample the mixer would read after a trigger, for a voice whose
    /// free-running phase is `phase` samples into the 640-byte domain when the
    /// instrument triggers on `waveform`. `mix_chunk` reads
    /// `voice_buffer[sample_pos >> 16]`, so this is exactly that.
    fn first_sample_after_trigger(
        waveform: i32,
        phase: u32,
        continue_phase: bool,
    ) -> (i8, i8, u32) {
        let waves = &*waveform::WAVES;
        let mut ins = silent_instrument();
        ins.square_lower_limit = 0x20;
        ins.square_upper_limit = 0x3f;
        let mut v = Voice::new();
        v.sample_pos = phase << 16; // mid-cycle, as after a note that has been sounding
        v.trigger_instrument(1, &ins, continue_phase);
        v.square.pos = 8; // a 50% duty square (`8 << (5 - 3)` = 0x20), not the all-low row 0
        v.track_period = 25;
        v.instr_period = 25;
        v.waveform = waveform;
        v.new_waveform = true;
        v.process_frame_dsp(&ins, waves, 6, 0);
        v.set_audio(waves, 44100.0);
        (
            v.voice_buffer[(v.sample_pos >> 16) as usize],
            v.voice_buffer[0],
            v.sample_pos,
        )
    }

    #[test]
    fn phase_continue_keeps_the_wave_read_position_across_a_trigger() {
        for waveform in [WAVEFORM_SAWTOOTH, WAVEFORM_SQUARE] {
            // Off is hvl_replay.c:893: the pointer restarts, so the first sample is wave[0].
            let (off, wave0, pos_off) = first_sample_after_trigger(waveform, 20, false);
            assert_eq!(pos_off, 0);
            assert_eq!(off, wave0, "flag off starts at wave[0]");

            // On: the pointer stays mid-cycle, and there the wave is not at wave[0].
            let (on, wave0_on, pos_on) = first_sample_after_trigger(waveform, 20, true);
            assert_eq!(pos_on, 20 << 16, "the position is untouched");
            assert_eq!(wave0_on, wave0, "same table either way");
            assert_ne!(
                on, wave0_on,
                "waveform {waveform}: first sample is not wave[0]"
            );
            assert_ne!(
                on, off,
                "waveform {waveform}: differs from the flag-off result"
            );
        }
    }

    #[test]
    fn phase_continue_does_not_touch_the_ring_position() {
        let ins = silent_instrument();
        let mut v = Voice::new();
        v.ring_sample_pos = 7 << 16;
        v.trigger_instrument(1, &ins, true);
        assert_eq!(v.ring_sample_pos, 0);
    }

    #[test]
    fn trigger_clears_ring_mix_but_keeps_ring_audio_source() {
        // hvl_replay.c:960: only vc_RingMixSource is NULLed on a trigger.
        let ins = silent_instrument();
        let mut v = Voice::new();
        v.ring_audio_source = Some(AudioSourceRef::Waves(0));
        v.ring_mix_active = true;
        v.trigger_instrument(1, &ins, false);
        assert!(!v.ring_mix_active);
        assert!(v.ring_audio_source.is_some());
    }

    #[test]
    fn wave_length_above_five_is_clamped_not_ub() {
        let mut ins = silent_instrument();
        ins.wave_length = 7; // the 3-bit decoder can produce 6 and 7
        let mut v = Voice::new();
        v.trigger_instrument(1, &ins, false);
        assert_eq!(v.wave_length, 5);
    }

    #[test]
    fn plain_triangle_note_produces_nonzero_audio_period_and_volume() {
        let waves = &*waveform::WAVES;
        let ins = silent_instrument();
        let mut v = Voice::new();
        v.trigger_instrument(1, &ins, false);
        v.track_period = 25; // some mid-range note
        v.instr_period = 25;
        v.waveform = WAVEFORM_TRIANGLE;
        v.new_waveform = true;

        v.process_frame_dsp(&ins, waves, 6, 0);
        assert!(v.audio_period > 0);
        v.set_audio(waves, 44100.0);
        assert!(v.delta > 0);
        // triangle table content should have been planted, not left at 0.
        assert!(v.voice_buffer[..0x280].iter().any(|&b| b != 0));
    }
}
