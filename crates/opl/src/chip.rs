//! The OPL3 (YMF262) chip at its native rate, one sample per `clock`.
//!
//! A port of ymfm's `fm_engine_base` / `fm_channel` / `ymf262` path
//! (`ymfm_fm.ipp`, `ymfm_opl.cpp`). OPL2 music runs on it in compatibility
//! mode (NEW, 0x105 bit 0, clear), which is what a Sound Blaster Pro 2 or
//! SB16 owner heard: 9 of the 18 channels addressable, waveforms 0..3, both
//! outputs on, no YM3014 DAC quantisation.
//!
//! Everything the music needs: 18 2-op channels or up to six 4-op pairs
//! (0x104), rhythm mode (0xBD), LFO AM/PM, both modes. Timers and status
//! are never needed for playback and are not modelled.

use super::operator::{EnvState, OpRegs, Operator};
use super::tables::{build_waveforms, WAVEFORM_LENGTH};

/// YMF262 master clock (Hz) and the divider to one output sample.
pub const OPL3_CLOCK: f64 = 14_318_180.0;
pub const OPL3_CLOCK_DIVIDER: f64 = 288.0;
/// ≈ 49 715.9 Hz.
pub const NATIVE_RATE: f64 = OPL3_CLOCK / OPL3_CLOCK_DIVIDER;

pub const CHANNELS: usize = 18;
pub const OPERATORS: usize = 36;
const ALL_OPS: u64 = (1 << OPERATORS) - 1;

/// Register offset of channel `ch` (0..17).
#[inline]
fn channel_offset(ch: usize) -> usize {
    (ch % 9) + 0x100 * (ch / 9)
}

/// Register offset of operator `op` (0..35).
#[inline]
fn operator_offset(op: usize) -> usize {
    (op % 18) + 2 * ((op % 18) / 6) + 0x100 * (op / 18)
}

/// Key-on sources an operator ORs together (ymfm `keyon_type`).
const KEYON_NORMAL: u32 = 1;
const KEYON_RHYTHM: u32 = 2;

/// Operators of each channel under a 0x104 four-op mask (ymfm
/// `operator_map`): a 4-op pair takes the partner channel's two operators,
/// and the partner (channel +3) has none.
fn operator_map(fourop: u8) -> [OpList; CHANNELS] {
    let mut map = [OpList::default(); CHANNELS];
    for bank in 0..2 {
        for c in 0..9 {
            let first = c + 3 * (c / 3) + 18 * bank;
            map[c + 9 * bank] = OpList::two(first, first + 3);
        }
        for pair in 0..3 {
            if fourop & (1 << (pair + 3 * bank)) != 0 {
                let (primary, partner) = (pair + 9 * bank, pair + 3 + 9 * bank);
                let first = pair + 18 * bank;
                map[primary] = OpList {
                    ops: [first, first + 3, first + 6, first + 9],
                    len: 4,
                };
                map[partner] = OpList::default();
            }
        }
    }
    map
}

#[derive(Clone, Copy, Default, Debug)]
struct OpList {
    ops: [usize; 4],
    len: usize,
}

impl OpList {
    fn two(a: usize, b: usize) -> OpList {
        OpList {
            ops: [a, b, 0, 0],
            len: 2,
        }
    }

    fn as_slice(&self) -> &[usize] {
        &self.ops[..self.len]
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct Channel {
    feedback: [i16; 2],
    feedback_in: i16,
    active: bool,
}

/// 4-op connections as ymfm encodes them (`s_algorithm_ops`, entries 8..11
/// for OPL3): bit 0 = op2's input, bits 1..3 op3's, bits 4..6 op4's (indices
/// into `opout`: 0 = none, n = operator n's output), bits 7..9 add
/// op1/op2/op3 to the output. OPL3 wires `opout` 1 to op2 at most, 2 to
/// op3, 3 to op4 — each connection C0 bit 0 of the pair's two channels.
const fn algorithm(
    op2in: u16,
    op3in: u16,
    op4in: u16,
    op1out: u16,
    op2out: u16,
    op3out: u16,
) -> u16 {
    op2in | (op3in << 1) | (op4in << 4) | (op1out << 7) | (op2out << 8) | (op3out << 9)
}
const OPL3_ALGORITHMS: [u16; 4] = [
    algorithm(1, 2, 3, 0, 0, 0), // FM-FM: O1 -> O2 -> O3 -> O4
    algorithm(0, 2, 3, 1, 0, 0), // AM-FM: O1 + (O2 -> O3 -> O4)
    algorithm(1, 0, 3, 0, 1, 0), // FM-AM: (O1 -> O2) + (O3 -> O4)
    algorithm(0, 2, 0, 1, 0, 1), // AM-AM: O1 + (O2 -> O3) + O4
];

pub struct Chip {
    regs: [u8; 0x200],
    ops: Vec<Operator>,
    chans: [Channel; CHANNELS],
    /// Operator assignment as of the last prepare; key-ons route through it.
    op_map: [OpList; CHANNELS],
    waveforms: Box<[[u16; WAVEFORM_LENGTH]; 8]>,
    env_counter: u32,
    modified: bool,
    /// Bit per operator whose registers or key-on changed since the last
    /// prepare. ymfm re-prepares every operator after any write; the others
    /// would recompute what they already hold.
    dirty_ops: u64,
    prepare_count: u32,
    lfo_am_counter: u16,
    lfo_pm_counter: u16,
    lfo_am: u8,
    noise_lfsr: u32,
    /// Bit per channel: 1 plays, 0 mutes (the chip still runs).
    channel_mask: u32,
    /// Each channel's output in the last `clock_sample`, after the mask:
    /// what it added to the mix (0 when silent or muted). For scopes.
    taps: [i32; CHANNELS],
}

impl Default for Chip {
    fn default() -> Self {
        Self::new()
    }
}

impl Chip {
    pub fn new() -> Chip {
        Chip {
            regs: [0; 0x200],
            ops: vec![Operator::default(); OPERATORS],
            chans: [Channel::default(); CHANNELS],
            op_map: operator_map(0),
            waveforms: build_waveforms(),
            env_counter: 0,
            modified: true,
            dirty_ops: ALL_OPS,
            prepare_count: 0,
            lfo_am_counter: 0,
            lfo_pm_counter: 0,
            lfo_am: 0,
            noise_lfsr: 1,
            channel_mask: (1 << CHANNELS) - 1,
            taps: [0; CHANNELS],
        }
    }

    #[inline]
    fn newflag(&self) -> bool {
        self.regs[0x105] & 1 != 0
    }

    /// Write a register, 0x000..0x1FF. In compatibility mode the second bank
    /// aliases the first, except 0x105 itself (ymfm `write_address_hi`,
    /// "tests reveal").
    pub fn write(&mut self, reg: u16, val: u8) {
        let mut reg = (reg & 0x1ff) as usize;
        if reg & 0x100 != 0 && !self.newflag() && reg != 0x105 {
            reg &= 0xff;
        }
        self.modified = true;
        self.dirty_ops |= self.ops_touched_by(reg);
        // The mode register ignores its low bits when bit 7 (IRQ reset) is set.
        if reg == 0x04 && val & 0x80 != 0 {
            self.regs[reg] |= 0x80;
        } else {
            self.regs[reg] = val;
        }
        if reg == 0xbd {
            // Rhythm key-ons; clearing bit 5 releases all five.
            let mask = if val & 0x20 != 0 { val & 0x1f } else { 0 };
            let bits = |b: u8| ((mask >> b) & 1) as u32;
            let bd = bits(4) * 3;
            self.key_channel(6, bd, KEYON_RHYTHM);
            self.key_channel(7, bits(0) | (bits(3) << 1), KEYON_RHYTHM);
            self.key_channel(8, bits(2) | (bits(1) << 1), KEYON_RHYTHM);
        } else if reg & 0xf0 == 0xb0 && reg & 0x0f < 9 {
            let ch = (reg & 0x0f) + 9 * (reg >> 8);
            let states = if val & 0x20 != 0 { 0xf } else { 0 };
            self.key_channel(ch, states, KEYON_NORMAL);
        }
    }

    /// Operators whose `prepare` a write to `reg` can change: an operator
    /// register reaches its operator, a channel's frequency and key-on its
    /// operators, 0xBD the rhythm channels' key-ons. The mode registers
    /// (0x08 NTS, 0x104 four-op, 0x105 NEW) and anything unclassified reach
    /// them all. C0 is read live at output and reaches none.
    fn ops_touched_by(&self, reg: usize) -> u64 {
        let (bank, lo) = (reg >> 8, reg & 0xff);
        let channel_ops = |ch: usize| {
            self.op_map[ch]
                .as_slice()
                .iter()
                .fold(0u64, |mask, &op| mask | 1 << op)
        };
        match lo {
            0x20..=0x9f | 0xe0..=0xff => {
                // Slots 0..5, 8..13, 16..21 are operators 0..17 of the bank.
                let slot = lo & 0x1f;
                if slot % 8 < 6 && slot < 22 {
                    1 << (slot - 2 * (slot / 8) + 18 * bank)
                } else {
                    0
                }
            }
            0xa0..=0xa8 | 0xb0..=0xb8 => channel_ops((lo & 0x0f) + 9 * bank),
            0xc0..=0xc8 => 0,
            0xbd if bank == 0 => channel_ops(6) | channel_ops(7) | channel_ops(8),
            _ => ALL_OPS,
        }
    }

    /// Set or clear one key-on source on each of a channel's operators, bit
    /// n of `states` for operator n.
    fn key_channel(&mut self, ch: usize, states: u32, source: u32) {
        let list = self.op_map[ch];
        for (n, &op) in list.as_slice().iter().enumerate() {
            let live = &mut self.ops[op].keyon_live;
            if states & (1 << n) != 0 {
                *live |= source;
            } else {
                *live &= !source;
            }
        }
    }

    /// The last value written to a register.
    pub fn written(&self, reg: u16) -> u8 {
        self.regs[(reg & 0x1ff) as usize]
    }

    pub fn set_channel_mask(&mut self, mask: u32) {
        self.channel_mask = mask & ((1 << CHANNELS) - 1);
    }

    fn op_regs(&self, ch: usize, op: usize) -> OpRegs {
        let (co, oo) = (channel_offset(ch), operator_offset(op));
        let block_freq =
            (((self.regs[0xb0 + co] & 0x1f) as u32) << 8) | self.regs[0xa0 + co] as u32;
        OpRegs {
            r20: self.regs[0x20 + oo],
            r40: self.regs[0x40 + oo],
            r60: self.regs[0x60 + oo],
            r80: self.regs[0x80 + oo],
            re0: self.regs[0xe0 + oo],
            block_freq,
            note_select: ((self.regs[0x08] >> 6) & 1) as u32,
            waveform_bits: if self.newflag() { 3 } else { 2 },
        }
    }

    /// Advance the noise LFSR and both LFOs; returns the raw PM value.
    fn clock_noise_and_lfo(&mut self) -> i32 {
        let l = self.noise_lfsr << 1;
        self.noise_lfsr = l | (((l >> 23) ^ (l >> 9) ^ (l >> 8) ^ (l >> 1)) & 1);

        // AM: a 210×64-step triangle, ≈ 3.7 Hz.
        let am_counter = self.lfo_am_counter as u32;
        self.lfo_am_counter = self.lfo_am_counter.wrapping_add(1);
        if am_counter >= 210 * 64 - 1 {
            self.lfo_am_counter = 0;
        }
        let am_depth = ((self.regs[0xbd] >> 7) & 1) as u32;
        let shift = 9 - 2 * am_depth;
        let tri = if am_counter < 105 * 64 {
            am_counter
        } else {
            210 * 64 + 63 - am_counter
        };
        self.lfo_am = (tri >> shift) as u8;

        // PM: 8 steps of 1024 samples, ≈ 6.1 Hz.
        let pm_counter = self.lfo_pm_counter as u32;
        self.lfo_pm_counter = self.lfo_pm_counter.wrapping_add(1);
        const PM_SCALE: [i32; 8] = [8, 4, 0, -4, -8, -4, 0, 4];
        let pm_depth = ((self.regs[0xbd] >> 6) & 1) as u32;
        PM_SCALE[((pm_counter >> 10) & 7) as usize] >> (pm_depth ^ 1)
    }

    /// Advance one native-rate sample (ymfm `fm_engine_base::clock`).
    fn clock(&mut self) {
        if self.modified || {
            let due = self.prepare_count >= 4096;
            self.prepare_count += 1;
            due
        } {
            // ymfm reads 0x104 whatever NEW says; it is only writable with NEW=1,
            // but the mask outlives clearing NEW.
            self.op_map = operator_map(self.regs[0x104] & 0x3f);
            let dirty = self.dirty_ops;
            for ch in 0..CHANNELS {
                let mut active = false;
                let list = self.op_map[ch];
                for &op in list.as_slice() {
                    active |= if dirty & (1 << op) != 0 {
                        let r = self.op_regs(ch, op);
                        self.ops[op].prepare(&r)
                    } else {
                        self.ops[op].is_sounding()
                    };
                }
                self.chans[ch].active = active;
            }
            self.modified = false;
            self.dirty_ops = 0;
            self.prepare_count = 0;
        }

        self.env_counter = self.env_counter.wrapping_add(4);
        let lfo_raw_pm = self.clock_noise_and_lfo();

        for c in &mut self.chans {
            c.feedback[0] = c.feedback[1];
            c.feedback[1] = c.feedback_in;
        }
        // Every operator belongs to exactly one channel's list, so this
        // clocks the same set ymfm does. A dormant operator's envelope
        // cannot move and its phase is never read until a key-on resets
        // it, except operators 13 and 17, whose phases pick the rhythm
        // voices' metallic phases whatever their own envelopes are doing.
        let env_counter = self.env_counter >> 2;
        for (i, op) in self.ops.iter_mut().enumerate() {
            if op.is_dormant() && i != 13 && i != 17 {
                continue;
            }
            op.clock(env_counter, lfo_raw_pm);
        }
    }

    /// Operator 1 with its self-feedback; stores the value for the next
    /// feedback step (every channel shape starts this way).
    fn op1_with_feedback(&mut self, ch: usize, op: usize, c0: u8) -> i32 {
        let feedback = ((c0 >> 1) & 7) as u32;
        let mut opmod = 0i32;
        if feedback != 0 {
            let c = &self.chans[ch];
            opmod = (c.feedback[0] as i32 + c.feedback[1] as i32) >> (10 - feedback);
        }
        let o = &self.ops[op];
        let v = o.compute_volume(
            o.phase_index().wrapping_add(opmod as u32),
            self.lfo_am as u32,
            &self.waveforms,
        );
        self.chans[ch].feedback_in = v as i16;
        v
    }

    #[inline]
    fn volume(&self, op: usize, modulation: i32) -> i32 {
        let o = &self.ops[op];
        o.compute_volume(
            o.phase_index().wrapping_add(modulation as u32),
            self.lfo_am as u32,
            &self.waveforms,
        )
    }

    /// Which outputs a channel feeds: C0 bits 4/5 under NEW=1, both otherwise.
    fn routing(&self, c0: u8) -> Option<(bool, bool)> {
        if !self.newflag() {
            return Some((true, true));
        }
        if c0 & 0xf0 == 0 {
            None
        } else {
            Some((c0 & 0x10 != 0, c0 & 0x20 != 0))
        }
    }

    /// A 2-op channel (ymfm `output_2op`; OPL3 has no modulator delay).
    fn output_2op(&mut self, ch: usize) -> Option<i32> {
        let c0 = self.regs[0xc0 + channel_offset(ch)];
        let [m, cr, ..] = self.op_map[ch].ops;
        let op1 = self.op1_with_feedback(ch, m, c0);
        self.routing(c0)?;
        Some(if c0 & 1 == 0 {
            self.volume(cr, op1 >> 1)
        } else {
            (op1 + self.volume(cr, 0)).clamp(-32768, 32767)
        })
    }

    /// A 4-op channel (ymfm `output_4op`). The connection is C0 bit 0 of
    /// this channel and of its partner (+3); `opout` is int16 as in ymfm.
    fn output_4op(&mut self, ch: usize) -> Option<i32> {
        let co = channel_offset(ch);
        let c0 = self.regs[0xc0 + co];
        let ops = self.op_map[ch].ops;
        let op1 = self.op1_with_feedback(ch, ops[0], c0);
        self.routing(c0)?;

        let alg = OPL3_ALGORITHMS[((c0 & 1) | ((self.regs[0xc3 + co] & 1) << 1)) as usize];
        // ymfm's table also holds pair sums (indices 5..7) for the OPN
        // algorithms; no OPL3 connection reads them.
        let mut opout = [0i16; 4];
        opout[1] = op1 as i16;
        opout[2] = self.volume(ops[1], opout[(alg & 1) as usize] as i32 >> 1) as i16;
        opout[3] = self.volume(ops[2], opout[((alg >> 1) & 7) as usize] as i32 >> 1) as i16;
        let mut result = self.volume(ops[3], opout[((alg >> 4) & 7) as usize] as i32 >> 1);
        for (bit, idx) in [(7, 1), (8, 2), (9, 3)] {
            if alg & (1 << bit) != 0 {
                result = (result + opout[idx] as i32).clamp(-32768, 32767);
            }
        }
        Some(result)
    }

    /// Bass drum on channel 6 (ymfm `output_rhythm_ch6`): a normal 2-op
    /// voice, except the additive connection drops operator 1; doubled.
    fn output_bass_drum(&mut self) -> i32 {
        let c0 = self.regs[0xc6];
        let [m, cr, ..] = self.op_map[6].ops;
        let op1 = self.op1_with_feedback(6, m, c0);
        let modulation = if c0 & 1 != 0 { 0 } else { op1 >> 1 };
        self.volume(cr, modulation) * 2
    }

    /// Hi-hat and snare on channel 7, doubled (ymfm `output_rhythm_ch7`).
    fn output_hihat_snare(&self, phase_select: u32) -> i32 {
        let [hh, sd, ..] = self.op_map[7].ops;
        let noise = (self.noise_lfsr >> 23) & 1;
        let am = self.lfo_am as u32;
        let hh_phase = (phase_select << 9) | (0xd0 >> (2 * (noise ^ phase_select)));
        let mut result = self.ops[hh].compute_volume(hh_phase, am, &self.waveforms);
        let op13 = self.ops[hh].phase_index();
        let sd_phase = (0x100 << ((op13 >> 8) & 1)) ^ (noise << 8);
        result += self.ops[sd].compute_volume(sd_phase, am, &self.waveforms);
        result.clamp(-32768, 32767) * 2
    }

    /// Tom-tom and top cymbal on channel 8, doubled (ymfm `output_rhythm_ch8`).
    fn output_tom_cymbal(&self, phase_select: u32) -> i32 {
        let [tt, tc, ..] = self.op_map[8].ops;
        let am = self.lfo_am as u32;
        let mut result =
            self.ops[tt].compute_volume(self.ops[tt].phase_index(), am, &self.waveforms);
        result += self.ops[tc].compute_volume(0x100 | (phase_select << 9), am, &self.waveforms);
        result.clamp(-32768, 32767) * 2
    }

    /// Render one native-rate stereo sample, clamped to 16 bits as the
    /// YMF262's serial output is.
    pub fn clock_sample(&mut self) -> (i16, i16) {
        self.clock();
        let rhythm = self.regs[0xbd] & 0x20 != 0;
        // Operators 13 (hi-hat) and 17 (cymbal) pick the metallic phases.
        let phase_select = if rhythm {
            let (p13, p17) = (self.ops[13].phase_index(), self.ops[17].phase_index());
            (((p13 >> 2) ^ (p13 >> 7)) & 1) | ((p13 >> 3) & 1) | (((p17 >> 5) ^ (p17 >> 3)) & 1)
        } else {
            0
        };
        let (mut l, mut r) = (0i32, 0i32);
        self.taps = [0; CHANNELS];
        for ch in 0..CHANNELS {
            if !self.chans[ch].active {
                continue;
            }
            let value = match ch {
                // The rhythm voices always compute (the bass drum's feedback
                // runs even when its outputs are off); routing decides below.
                6 if rhythm => Some(self.output_bass_drum()),
                7 if rhythm => Some(self.output_hihat_snare(phase_select)),
                8 if rhythm => Some(self.output_tom_cymbal(phase_select)),
                _ if self.op_map[ch].len == 4 => self.output_4op(ch),
                _ => self.output_2op(ch),
            };
            let Some(v) = value else { continue };
            if self.channel_mask & (1 << ch) == 0 {
                continue;
            }
            self.taps[ch] = v;
            let (to_l, to_r) = self
                .routing(self.regs[0xc0 + channel_offset(ch)])
                .unwrap_or((false, false));
            if to_l {
                l += v;
            }
            if to_r {
                r += v;
            }
        }
        (l.clamp(-32768, 32767) as i16, r.clamp(-32768, 32767) as i16)
    }

    /// Channel `ch`'s output in the last native sample (signed, one
    /// operator's full swing is ±8191), after the mask. For scopes.
    pub fn channel_output(&self, ch: usize) -> i32 {
        self.taps[ch]
    }

    /// Envelope state of operator `op`, for tests and scopes.
    pub fn operator_envelope(&self, op: usize) -> (EnvState, u32) {
        (self.ops[op].env_state, self.ops[op].env_attenuation)
    }
}
