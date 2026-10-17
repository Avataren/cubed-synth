//! The single fixed-order filter sweep: `vc_FilterPos` walks between two
//! bounds set by the instrument, bouncing direction on hitting either one.
//! Ported from `hvl_process_step:931-954` (trigger/setup) and
//! `hvl_process_frame:1364-1409` (the per-frame walk). Per the P0 correction
//! in `.ai/ahx/verdict.md`, there is no runtime IIR filter here at all: the
//! "filtering" is a static lookup into `waveform::WAVES`'s precomputed
//! lowpass/highpass rows (`voice.rs`'s job); this module only owns the
//! `FilterPos` position walk that picks *which* row.
//!
//! `bound_bounce_step` is the reusable core of that walk (hit either bound,
//! flip direction) -- `voice.rs`'s square-wave sweep (`vc_SquarePos`,
//! `hvl_process_frame:1324-1362`) is the exact same algorithm applied to a
//! different pair of bounds and a different clamp/wait policy, so it reuses
//! this helper instead of re-deriving it.

use super::format::Instrument;

/// One step of the reference's "hit a bound, flip sign" walk, shared by
/// filter (`hvl_replay.c:1391-1398`) and square (`hvl_replay.c:1349-1357`).
/// `sliding_in` absorbs the first bound-hit after `Init` without reversing
/// (the instrument's initial position can already be past a limit; the
/// reference treats that first arrival as "sliding into range", not a
/// bounce).
pub(crate) fn bound_bounce_step(
    pos: i32,
    sign: &mut i32,
    sliding_in: &mut bool,
    lower: i32,
    upper: i32,
) -> i32 {
    if lower == pos || upper == pos {
        if *sliding_in {
            *sliding_in = false;
        } else {
            *sign = -*sign;
        }
    }
    pos + *sign
}

/// Mirrors the filter-related `vc_*` fields on `struct hvl_voice`
/// (`hvl_replay.h:142-151`).
#[derive(Debug, Clone, Copy, Default)]
pub struct FilterSweep {
    pub on: bool,
    pub init: bool,
    pub wait: i32,
    pub speed: i32,
    pub upper_limit: i32,
    pub lower_limit: i32,
    pub pos: i32,
    pub sign: i32,
    pub sliding_in: bool,
    /// `vc_IgnoreFilter`: a pending direct position set by pattern effect
    /// `E4x`/PList's filter-override, consumed (or not) the next time PList
    /// command 0 runs (`hvl_plist_command_parse` case 0,
    /// `hvl_replay.c:988-994`).
    pub ignore: i32,
}

impl FilterSweep {
    /// `hvl_process_step:931-954`. The `d3&0x80`/`d4&0x80` speed-bit folding
    /// there is checked here too even though `Instrument::filter_lower_limit`
    /// /`filter_upper_limit` are already masked to 7/6 bits at decode time
    /// (`format.rs`'s `parse_instrument_core`, mirroring the load-time mask
    /// at `hvl_replay.c:282,291`) -- meaning the `&0x80` checks below are
    /// always false in practice. Ported anyway for bit-exactness; flagged in
    /// `p3-report.md` as an observed-dead branch in the reference itself,
    /// not something this port should silently drop.
    pub fn trigger(ins: &Instrument) -> Self {
        let mut speed = ins.filter_speed as i32;
        let mut lower = ins.filter_lower_limit as i32;
        let mut upper = ins.filter_upper_limit as i32;

        if lower & 0x80 != 0 {
            speed |= 0x20;
        }
        if upper & 0x80 != 0 {
            speed |= 0x40;
        }
        lower &= !0x80;
        upper &= !0x80;
        if lower > upper {
            std::mem::swap(&mut lower, &mut upper);
        }

        FilterSweep {
            on: false,
            init: false,
            wait: 0,
            speed,
            upper_limit: upper,
            lower_limit: lower,
            pos: 32,
            sign: 0,
            sliding_in: false,
            ignore: 0,
        }
    }

    /// PList command 4's filter toggle (`hvl_plist_command_parse:1031-1037`):
    /// flips `on`, arms `init`, and sets the bounce direction from the
    /// nibble's sign bit.
    pub fn toggle(&mut self, sign: i32) {
        self.on = !self.on;
        self.init = self.on;
        self.sign = sign;
    }

    /// `hvl_process_frame:1364-1409` — one tick. Returns `true` when the
    /// walk ran this frame (the reference's unconditional `vc_NewWaveform =
    /// 1` inside the block), so `voice.rs` knows to re-plant the audio
    /// source.
    pub fn step(&mut self) -> bool {
        if !self.on {
            return false;
        }
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

        let f_max = if self.speed < 4 { 5 - self.speed } else { 1 };
        let mut pos = self.pos;
        for _ in 0..f_max {
            pos = bound_bounce_step(
                pos,
                &mut self.sign,
                &mut self.sliding_in,
                self.lower_limit,
                self.upper_limit,
            );
        }

        pos = pos.clamp(1, 63);
        self.pos = pos;
        self.wait = (self.speed - 3).max(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Instrument;

    fn instrument(speed: u8, lower: u8, upper: u8) -> Instrument {
        Instrument {
            filter_speed: speed,
            filter_lower_limit: lower,
            filter_upper_limit: upper,
            ..Default::default()
        }
    }

    #[test]
    fn trigger_starts_centered_with_swapped_bounds_if_needed() {
        let ins = instrument(10, 40, 10); // lower > upper -> swap
        let f = FilterSweep::trigger(&ins);
        assert_eq!(f.pos, 32);
        assert_eq!(f.lower_limit, 10);
        assert_eq!(f.upper_limit, 40);
    }

    #[test]
    fn walk_bounces_between_bounds_after_init_slides_in() {
        let ins = instrument(1, 30, 34); // speed<4 -> FMax = 5-1 = 4 steps/tick
        let mut f = FilterSweep::trigger(&ins);
        f.toggle(1);
        assert!(f.on);
        assert!(f.init);

        // pos starts at 32 (inside [30,34]), so Init doesn't arm sliding_in;
        // first step just walks FMax=4 steps with the initial sign toggled
        // on by `toggle`. Confirm it stays within bounds indefinitely.
        for _ in 0..64 {
            f.step();
            assert!(f.pos >= 30 && f.pos <= 34, "pos {} escaped [30,34]", f.pos);
        }
    }

    #[test]
    fn walk_clamps_final_position_to_1_63() {
        let ins = instrument(4, 1, 63); // speed>=4 -> FMax = 1
        let mut f = FilterSweep::trigger(&ins);
        f.toggle(-1);
        f.pos = 1;
        f.step();
        assert!(f.pos >= 1 && f.pos <= 63);
    }
}
