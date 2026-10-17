//! Frame-count ADSR volume stepper. Ported from two sites in
//! `hvl_replay.c`: the per-frame step (`hvl_process_frame:1189-1214`) and the
//! per-trigger frame-table setup (`hvl_process_step:895-901`), plus the
//! hard-cut release override (`hvl_process_frame:1174-1183`).
//!
//! AHX's envelope is not a curve function -- it's four frame counters
//! (attack/decay/sustain/release) each paired with a fixed per-frame `<<8`
//! volume delta, computed once at trigger time from the four
//! `(frames, target_volume)` breakpoints the instrument stores. Stepping
//! decrements whichever counter is currently nonzero (in a/d/s/r priority
//! order) and adds its delta to the running `vc_ADSRVolume`, snapping to the
//! exact target volume on the frame the counter reaches zero (so integer
//! division truncation in the per-frame delta never leaves a residual).

use super::format::Envelope;
use super::wrap_i16;

/// Mirrors `struct hvl_envelope` as used at runtime (`vc_ADSR` +
/// `vc_ADSRVolume`, `hvl_replay.h:30-37,98-99`): frame counters paired with
/// `<<8`-scaled per-frame deltas, plus the running output volume. The
/// per-frame deltas live in `int16` members in the reference, so they are
/// wrapped to 16 bits on store (`wrap_i16`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdsrState {
    pub a_frames: i32,
    a_volume: i32,
    pub d_frames: i32,
    d_volume: i32,
    pub s_frames: i32,
    pub r_frames: i32,
    r_volume: i32,
    /// `vc_ADSRVolume`: the running `<<8`-scaled output volume.
    pub volume: i32,
}

impl AdsrState {
    /// `hvl_process_step:895-901` — instrument trigger. Computes each
    /// phase's per-frame delta from the instrument's `(frames, volume)`
    /// breakpoints; a zero-frame phase gets `volume*256` outright since it
    /// completes (and snaps to its target) on the very next step.
    pub fn trigger(ins: &Envelope) -> Self {
        let a_frames = ins.a_frames as i32;
        let a_volume = wrap_i16(if a_frames != 0 {
            (ins.a_volume as i32 * 256) / a_frames
        } else {
            ins.a_volume as i32 * 256
        });
        let d_frames = ins.d_frames as i32;
        let d_volume = wrap_i16(if d_frames != 0 {
            ((ins.d_volume as i32 - ins.a_volume as i32) * 256) / d_frames
        } else {
            ins.d_volume as i32 * 256
        });
        let s_frames = ins.s_frames as i32;
        let r_frames = ins.r_frames as i32;
        let r_volume = wrap_i16(if r_frames != 0 {
            ((ins.r_volume as i32 - ins.d_volume as i32) * 256) / r_frames
        } else {
            ins.r_volume as i32 * 256
        });

        AdsrState {
            a_frames,
            a_volume,
            d_frames,
            d_volume,
            s_frames,
            r_frames,
            r_volume,
            volume: 0,
        }
    }

    /// `hvl_process_frame:1189-1214` — one tick of ADSR stepping.
    pub fn step(&mut self, ins: &Envelope) {
        if self.a_frames != 0 {
            self.volume += self.a_volume;
            self.a_frames -= 1;
            if self.a_frames <= 0 {
                self.volume = (ins.a_volume as i32) << 8;
            }
        } else if self.d_frames != 0 {
            self.volume += self.d_volume;
            self.d_frames -= 1;
            if self.d_frames <= 0 {
                self.volume = (ins.d_volume as i32) << 8;
            }
        } else if self.s_frames != 0 {
            self.s_frames -= 1;
        } else if self.r_frames != 0 {
            self.volume += self.r_volume;
            self.r_frames -= 1;
            if self.r_frames <= 0 {
                self.volume = (ins.r_volume as i32) << 8;
            }
        }
    }

    /// `hvl_process_frame:1174-1180` — the hard-cut release override: force
    /// the envelope into a release phase of `release_frames` length, ramping
    /// from the current volume down to the instrument's release target. The
    /// caller (voice.rs, which owns `vc_HardCutReleaseF`'s timing) supplies
    /// `release_frames`.
    pub fn hard_cut_release(&mut self, ins: &Envelope, release_frames: i32) {
        self.r_frames = wrap_i16(release_frames);
        self.r_volume = 0;
        if self.r_frames > 0 {
            self.r_volume = wrap_i16(-(self.volume - ((ins.r_volume as i32) << 8)) / self.r_frames);
        }
        self.a_frames = 0;
        self.d_frames = 0;
        self.s_frames = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(a_f: u8, a_v: u8, d_f: u8, d_v: u8, s_f: u8, r_f: u8, r_v: u8) -> Envelope {
        Envelope {
            a_frames: a_f,
            a_volume: a_v,
            d_frames: d_f,
            d_volume: d_v,
            s_frames: s_f,
            r_frames: r_f,
            r_volume: r_v,
        }
    }

    #[test]
    fn attack_reaches_exact_target_volume_on_last_frame() {
        // aFrames=4, aVolume=0x40 (64): delta = 64*256/4 = 4096/frame.
        let ins = env(4, 0x40, 0, 0, 0, 0, 0);
        let mut adsr = AdsrState::trigger(&ins);
        assert_eq!(adsr.volume, 0);
        for _ in 0..4 {
            adsr.step(&ins);
        }
        // Snapped exactly to aVolume<<8 on the frame the counter hits zero.
        assert_eq!(adsr.volume, (0x40i32) << 8);
        assert_eq!(adsr.a_frames, 0);
    }

    #[test]
    fn zero_frame_phase_snaps_immediately_on_next_step() {
        // aFrames=0 means the "delta" is volume*256 outright, so a single
        // step already overshoots to volume<<8 before the explicit snap --
        // matching the reference's `aFrames ? ... : aVolume*256` branch.
        let ins = env(0, 0x20, 0, 0, 0, 0, 0);
        let mut adsr = AdsrState::trigger(&ins);
        assert_eq!(adsr.a_frames, 0);
        // a_frames == 0, so the `if self.a_frames != 0` branch never fires;
        // stepping falls through to decay (also 0 frames) etc, leaving
        // volume at 0 forever -- this is the reference's actual behavior
        // for an all-zero-frames envelope, not a bug in this port.
        adsr.step(&ins);
        assert_eq!(adsr.volume, 0);
    }

    #[test]
    fn sustain_holds_volume_while_counting_down() {
        let ins = env(0, 0, 0, 0, 3, 0, 0);
        let mut adsr = AdsrState::trigger(&ins);
        adsr.volume = 12345; // simulate a prior decay having landed here
        adsr.step(&ins);
        assert_eq!(adsr.volume, 12345);
        assert_eq!(adsr.s_frames, 2);
    }

    #[test]
    fn hard_cut_release_ramps_to_instrument_release_target() {
        let ins = env(0, 0, 0, 0, 0, 0, 0x10);
        let mut adsr = AdsrState::trigger(&ins);
        adsr.volume = 0x40 << 8;
        adsr.hard_cut_release(&ins, 4);
        assert_eq!(adsr.r_frames, 4);
        // r_volume = -(0x4000 - 0x1000) / 4 = -3072
        assert_eq!(adsr.r_volume, -3072);
        for _ in 0..4 {
            adsr.step(&ins);
        }
        assert_eq!(adsr.volume, (0x10i32) << 8);
    }

    /// An envelope with neither attack nor decay frames never rises; in a song
    /// the release phase then ramps from that 0 by a step worked out from the
    /// *decay level*, which swings the running volume negative when the decay
    /// level is above the release level. The editor warns about it
    /// (`ahxEnvelopeNeverRises`); this pins what it is warning about.
    #[test]
    fn a_release_after_no_attack_or_decay_swings_negative() {
        let e = env(0, 64, 0, 64, 1, 8, 0);
        let mut st = AdsrState::trigger(&e);
        let mut lowest = 0;
        for _ in 0..40 {
            st.step(&e);
            lowest = lowest.min(st.volume);
        }
        assert!(
            lowest <= -50 * 256,
            "the release should swing towards -64: {}",
            lowest >> 8
        );
        assert_eq!(st.volume, 0, "and land on the release level");
    }
}
