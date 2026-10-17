//! One FM operator: phase generator, envelope generator and output.
//!
//! A port of ymfm's `fm_operator` (`ymfm_fm.ipp`) with the OPL register
//! decoding of `opl_registers_base::cache_operator_data` (`ymfm_opl.cpp`),
//! trimmed to what an OPL needs (no SSG-EG, detune, depress or reverb).

use super::tables::{
    attenuation_increment, attenuation_to_volume, key_scale_atten, WAVEFORM_LENGTH,
};

/// Above this attenuation an operator is treated as silent (ymfm `EG_QUIET`).
pub const EG_QUIET: u32 = 0x380;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnvState {
    Attack = 1,
    Decay = 2,
    Sustain = 3,
    Release = 4,
}

/// Register-derived values, refreshed by `prepare` after any write.
#[derive(Clone, Copy, Default, Debug)]
pub struct OpCache {
    pub waveform: usize,
    /// Phase increment per sample (10.10), or `None` when PM LFO is on.
    pub phase_step: Option<u32>,
    /// TL × 8 plus KSL, in envelope units.
    pub total_level: u32,
    /// Block (3 bits) and FNUM (10 bits).
    pub block_freq: u32,
    /// Frequency multiple as x.1 (1 means 0.5).
    pub multiple: u32,
    pub eg_sustain: u32,
    /// Effective rates, indexed by `EnvState as usize`.
    pub eg_rate: [u32; 5],
}

/// The operator's view of its registers (offsets 0x20/0x40/0x60/0x80/0xE0)
/// and its channel's A0/B0 frequency, plus the chip-wide bits it needs.
#[derive(Clone, Copy, Debug)]
pub struct OpRegs {
    pub r20: u8,
    pub r40: u8,
    pub r60: u8,
    pub r80: u8,
    pub re0: u8,
    pub block_freq: u32,
    pub note_select: u32,
    /// Bits of waveform select honoured: 2 in OPL2 mode, 3 in OPL3 mode.
    pub waveform_bits: u32,
}

#[derive(Clone, Debug)]
pub struct Operator {
    /// 10.10 fixed-point phase.
    pub phase: u32,
    /// 10-bit attenuation, 0 loudest, 0x3ff silent.
    pub env_attenuation: u32,
    pub env_state: EnvState,
    key_state: u32,
    pub keyon_live: u32,
    pub cache: OpCache,
    /// AM LFO enable (0x20 bit 7) and PM LFO enable (bit 6).
    pub am: bool,
    pub pm: bool,
}

impl Default for Operator {
    fn default() -> Self {
        Operator {
            phase: 0,
            env_attenuation: 0x3ff,
            env_state: EnvState::Release,
            key_state: 0,
            keyon_live: 0,
            cache: OpCache::default(),
            am: false,
            pm: false,
        }
    }
}

/// OPL phase step: FNUM as a 12-bit fraction, PM applied from its top 3
/// bits, shifted by block, scaled by the multiple (ymfm `opl_compute_phase_step`).
///
/// One departure from ymfm: it keeps the vibrato-adjusted FNUM to 12 bits
/// (`fnum &= 0xfff`), so a note near the top of the FNUM range wraps to almost
/// nothing on vibrato's upper swing and stutters at the LFO rate (ST3 puts G-4
/// at FNUM 1021: "authentic adlib attempt.s3m", order 0). The chip does not
/// wrap. Nuked-OPL3 `OPL3_PhaseGenerate`, fetched 2026-09-28, adds the offset
/// to a 16-bit FNUM with no mask:
///
///   uint16_t f_num; ... f_num += range;
///   basefreq = (f_num << slot->channel->block) >> 1;
///
/// The offset can never take FNUM below zero: its magnitude is at most
/// `7 * (fnum >> 7) / 2` in these quarter-FNUM units, well under `fnum << 2`.
#[inline]
pub fn compute_phase_step(block_freq: u32, multiple: u32, lfo_raw_pm: i32) -> u32 {
    let mut fnum = ((block_freq & 0x3ff) << 2) as i32;
    fnum += (lfo_raw_pm * ((block_freq >> 7) & 7) as i32) >> 1;
    let fnum = fnum as u32;
    let block = (block_freq >> 10) & 7;
    let step = (fnum << block) >> 2;
    (step * multiple) >> 1
}

#[inline]
fn effective_rate(rawrate: u32, ksr: u32) -> u32 {
    if rawrate == 0 {
        0
    } else {
        (rawrate + ksr).min(63)
    }
}

impl Operator {
    /// Refresh the cache from the registers and clock the key state; returns
    /// whether the operator is still sounding (ymfm `fm_operator::prepare`).
    pub fn prepare(&mut self, r: &OpRegs) -> bool {
        self.cache_registers(r);
        self.clock_keystate((self.keyon_live != 0) as u32);
        self.is_sounding()
    }

    /// What `prepare` reports: not yet released below `EG_QUIET`.
    #[inline]
    pub fn is_sounding(&self) -> bool {
        self.env_state != EnvState::Release || self.env_attenuation < EG_QUIET
    }

    fn cache_registers(&mut self, r: &OpRegs) {
        let c = &mut self.cache;
        c.waveform = (r.re0 as u32 & ((1 << r.waveform_bits) - 1)) as usize;
        let block_freq = r.block_freq;
        c.block_freq = block_freq;

        // 4-bit keycode: block, plus FNUM bit 9 or 8 chosen by NTS.
        let keycode = (((block_freq >> 10) & 7) << 1) | ((block_freq >> (9 - r.note_select)) & 1);

        // 0,1,2,3,4,5,6,7,8,9,10,10,12,12,15,15 as x.1; 0 means 0.5.
        let multiple = r.r20 as u32 & 0xf;
        c.multiple = ((multiple & 0xe) | ((0xc2aa >> multiple) & 1)) * 2;
        if c.multiple == 0 {
            c.multiple = 1;
        }

        self.am = r.r20 & 0x80 != 0;
        self.pm = r.r20 & 0x40 != 0;
        c.phase_step = if self.pm {
            None
        } else {
            Some(compute_phase_step(block_freq, c.multiple, 0))
        };

        c.total_level = (r.r40 as u32 & 0x3f) << 3;
        // KSL's two bits are stored swapped.
        let ksl_raw = (r.r40 as u32 >> 6) & 3;
        let ksl = ((ksl_raw >> 1) & 1) | ((ksl_raw & 1) << 1);
        if ksl != 0 {
            c.total_level +=
                key_scale_atten((block_freq >> 10) & 7, (block_freq >> 6) & 0xf) << ksl;
        }

        // 4-bit sustain level where 15 means 31.
        let mut sl = (r.r80 as u32 >> 4) & 0xf;
        sl |= (sl + 1) & 0x10;
        c.eg_sustain = sl << 5;

        let ksr_bit = (r.r20 as u32 >> 4) & 1;
        let ksrval = keycode >> (2 * (ksr_bit ^ 1));
        let (ar, dr, rr) = (
            (r.r60 as u32 >> 4) & 0xf,
            r.r60 as u32 & 0xf,
            r.r80 as u32 & 0xf,
        );
        let sustaining = r.r20 & 0x20 != 0;
        c.eg_rate[EnvState::Attack as usize] = effective_rate(ar * 4, ksrval);
        c.eg_rate[EnvState::Decay as usize] = effective_rate(dr * 4, ksrval);
        c.eg_rate[EnvState::Sustain as usize] = if sustaining {
            0
        } else {
            effective_rate(rr * 4, ksrval)
        };
        c.eg_rate[EnvState::Release as usize] = effective_rate(rr * 4, ksrval);
    }

    fn clock_keystate(&mut self, keystate: u32) {
        if keystate != self.key_state {
            self.key_state = keystate;
            if keystate != 0 {
                self.start_attack();
            } else if self.env_state < EnvState::Release {
                self.env_state = EnvState::Release;
            }
        }
    }

    fn start_attack(&mut self) {
        if self.env_state == EnvState::Attack {
            return;
        }
        self.env_state = EnvState::Attack;
        self.phase = 0;
        if self.cache.eg_rate[EnvState::Attack as usize] >= 62 {
            self.env_attenuation = 0;
        }
    }

    /// Released all the way to silence: `clock` would leave the envelope
    /// where it is, and only a key-on (which resets the phase) wakes it.
    #[inline]
    pub fn is_dormant(&self) -> bool {
        self.env_state == EnvState::Release && self.env_attenuation == 0x3ff
    }

    /// One sample: envelope (env_counter is the x.2 counter already shifted
    /// down by 2), then phase.
    pub fn clock(&mut self, env_counter: u32, lfo_raw_pm: i32) {
        self.clock_envelope(env_counter);
        let step = match self.cache.phase_step {
            Some(step) => step,
            None => compute_phase_step(self.cache.block_freq, self.cache.multiple, lfo_raw_pm),
        };
        self.phase = self.phase.wrapping_add(step);
    }

    fn clock_envelope(&mut self, env_counter: u32) {
        if self.env_state == EnvState::Attack && self.env_attenuation == 0 {
            self.env_state = EnvState::Decay;
        }
        if self.env_state == EnvState::Decay && self.env_attenuation >= self.cache.eg_sustain {
            self.env_state = EnvState::Sustain;
        }

        let rate = self.cache.eg_rate[self.env_state as usize];
        let rate_shift = rate >> 2;
        let counter = env_counter << rate_shift;
        if counter & 0x7ff != 0 {
            return;
        }
        let relevant = (counter >> if rate_shift <= 11 { 11 } else { rate_shift }) & 7;
        let increment = attenuation_increment(rate, relevant);

        if self.env_state == EnvState::Attack {
            // Rates 62/63 only jump at key-on; changed later they never move.
            // ymfm's `att += (~att * inc) >> 4` on a uint16_t: the NOT
            // promotes to a negative int, the shift is arithmetic, the store
            // truncates to 16 bits.
            if rate < 62 {
                let att = self.env_attenuation as i32;
                let step = ((!att) * increment as i32) >> 4;
                self.env_attenuation = (att + step) as u16 as u32;
            }
        } else {
            self.env_attenuation += increment;
            if self.env_attenuation >= 0x400 {
                self.env_attenuation = 0x3ff;
            }
        }
    }

    /// Envelope attenuation plus AM and total level, clamped to 10 bits.
    #[inline]
    fn envelope_attenuation(&self, am_offset: u32) -> u32 {
        let mut result = self.env_attenuation;
        if self.am {
            result += am_offset;
        }
        (result + self.cache.total_level).min(0x3ff)
    }

    /// Signed 14-bit output for a phase already offset by any modulation.
    #[inline]
    pub fn compute_volume(
        &self,
        phase: u32,
        am_offset: u32,
        waveforms: &[[u16; WAVEFORM_LENGTH]; 8],
    ) -> i32 {
        if self.env_attenuation > EG_QUIET {
            return 0;
        }
        let sin_att =
            waveforms[self.cache.waveform][(phase as usize) & (WAVEFORM_LENGTH - 1)] as u32;
        let env_att = self.envelope_attenuation(am_offset) << 2;
        let result = attenuation_to_volume((sin_att & 0x7fff) + env_att);
        if sin_att & 0x8000 != 0 {
            -result
        } else {
            result
        }
    }

    /// The top 10 bits of the 10.10 phase.
    #[inline]
    pub fn phase_index(&self) -> u32 {
        self.phase >> 10
    }
}
