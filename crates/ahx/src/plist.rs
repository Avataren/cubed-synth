//! Per-voice PList command dispatch: `hvl_plist_command_parse`
//! (`hvl_replay.c:981-1123`). Called twice per PList step (once per effect
//! column, `hvl_process_frame:1296-1297`) from `voice.rs`'s PList section.
//!
//! This is deliberately a free function taking `&mut Voice` rather than a
//! `Voice` method: it's dispatch over an *externally* numbered command set
//! (the PList entry's `FX`/`FXParam` bytes), not part of the voice's own
//! state machine, matching the reference's own separation of
//! `hvl_plist_command_parse` from `hvl_process_frame`.

use super::voice::{panning_left, panning_right, Voice};

/// `hvl_plist_command_parse`, `hvl_replay.c:981-1123`.
pub fn process_command(voice: &mut Voice, fx: i32, fx_param: i32) {
    match fx {
        0
            // Set filter position (981-997).
            if fx_param > 0 && fx_param < 0x40 => {
                if voice.filter.ignore != 0 {
                    voice.filter.pos = voice.filter.ignore;
                    voice.filter.ignore = 0;
                } else {
                    voice.filter.pos = fx_param;
                }
                voice.new_waveform = true;
            }
        1 => {
            // PerfPortamento up (999-1002).
            voice.period_perf_slide_speed = fx_param;
            voice.period_perf_slide_on = true;
        }
        2 => {
            // PerfPortamento down (1004-1007).
            voice.period_perf_slide_speed = -fx_param;
            voice.period_perf_slide_on = true;
        }
        3 => {
            // Set square position (1009-1014).
            if !voice.square.ignore {
                let shift = (5 - voice.wave_length).max(0) as u32;
                voice.square.pos = fx_param >> shift;
            } else {
                voice.square.ignore = false;
            }
        }
        4 => {
            // Toggle square/filter (1016-1039). Low nibble -> square, high
            // nibble -> filter; FXParam==0 toggles square only (positive
            // sign). A nibble value of 0xf selects the negative direction.
            if fx_param == 0 {
                voice.square.toggle(1);
            } else {
                if fx_param & 0x0f != 0 {
                    let sign = if fx_param & 0x0f == 0x0f { -1 } else { 1 };
                    voice.square.toggle(sign);
                }
                if fx_param & 0xf0 != 0 {
                    let sign = if fx_param & 0xf0 == 0xf0 { -1 } else { 1 };
                    voice.filter.on = !voice.filter.on;
                    voice.filter.init = voice.filter.on;
                    voice.filter.sign = sign;
                }
            }
        }
        5 => {
            // Jump PList position (1041-1043).
            voice.perf_current = fx_param;
        }
        7 => {
            // Ring modulate with triangle (1045-1065).
            set_ring_base(voice, fx_param, 0);
        }
        8 => {
            // Ring modulate with sawtooth (1067-1087).
            set_ring_base(voice, fx_param, 1);
        }
        9 => {
            // Set panning, HT 1.4+ (1090-1096).
            let mut p = fx_param;
            if p > 127 {
                p -= 256;
            }
            voice.pan = (p + 128) as u32;
            voice.pan_mult_left = panning_left(voice.pan as usize);
            voice.pan_mult_right = panning_right(voice.pan as usize);
        }
        12 => {
            // Volume (1098-1117): <=0x40 -> note volume, 0x50-0x90 ->
            // PerfSubVolume, 0xa0-0xe0 -> TrackMasterVolume (this voice
            // only -- unlike the pattern-effect 0xC's 0x50/0xa0 tiers,
            // which broadcast to every channel; see stepfx_3 in engine.rs).
            let mut p = fx_param;
            if p <= 0x40 {
                voice.note_max_volume = p;
                return;
            }
            p -= 0x50;
            if p < 0 {
                return;
            }
            if p <= 0x40 {
                voice.perf_sub_volume = p;
                return;
            }
            p -= 0xa0 - 0x50;
            if p < 0 {
                return;
            }
            if p <= 0x40 {
                voice.track_master_volume = p;
            }
        }
        15 => {
            // Set PerfSpeed (1119-1121).
            voice.perf_speed = fx_param;
            voice.perf_wait = fx_param;
        }
        _ => {}
    }
}

/// Shared body of PList commands 7 (triangle) and 8 (sawtooth) ring
/// modulation (`hvl_replay.c:1045-1087`): both only differ in which base
/// waveform they ring against (`voice_wave` 0 or 1) and are otherwise
/// byte-for-byte identical, so it's factored out rather than duplicated.
fn set_ring_base(voice: &mut Voice, fx_param: i32, ring_waveform: i32) {
    if (1..=0x3c).contains(&fx_param) {
        voice.ring_base_period = fx_param;
        voice.ring_fixed_period = true;
    } else if (0x81..=0xbc).contains(&fx_param) {
        voice.ring_base_period = fx_param - 0x80;
        voice.ring_fixed_period = false;
    } else {
        voice.ring_base_period = 0;
        voice.ring_fixed_period = false;
        voice.ring_new_waveform = false;
        voice.ring_audio_source = None; // turn it off (:1058-1059)
        voice.ring_mix_active = false;
        return;
    }
    voice.ring_waveform = ring_waveform;
    voice.ring_new_waveform = true;
    voice.ring_plant_period = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Instrument;

    #[test]
    fn command_0_sets_filter_pos_directly() {
        let mut v = Voice::new();
        v.filter = crate::filter_sweep::FilterSweep::trigger(&Instrument::default());
        process_command(&mut v, 0, 0x10);
        assert_eq!(v.filter.pos, 0x10);
        assert!(v.new_waveform);
    }

    #[test]
    fn command_0_prefers_pending_ignore_value() {
        let mut v = Voice::new();
        v.filter.ignore = 5;
        process_command(&mut v, 0, 0x10);
        assert_eq!(v.filter.pos, 5);
        assert_eq!(v.filter.ignore, 0);
    }

    #[test]
    fn command_4_zero_param_toggles_square_positive() {
        let mut v = Voice::new();
        process_command(&mut v, 4, 0);
        assert!(v.square.on);
        assert_eq!(v.square.sign, 1);
    }

    #[test]
    fn command_4_high_nibble_f_toggles_filter_negative() {
        let mut v = Voice::new();
        process_command(&mut v, 4, 0xf0);
        assert!(v.filter.on);
        assert_eq!(v.filter.sign, -1);
    }

    #[test]
    fn command_7_fixed_period_sets_ring_triangle() {
        let mut v = Voice::new();
        process_command(&mut v, 7, 12);
        assert_eq!(v.ring_base_period, 12);
        assert!(v.ring_fixed_period);
        assert_eq!(v.ring_waveform, 0);
        assert!(v.ring_new_waveform);
    }

    #[test]
    fn command_8_relative_period_sets_ring_sawtooth() {
        let mut v = Voice::new();
        process_command(&mut v, 8, 0x81 + 5);
        assert_eq!(v.ring_base_period, 6); // 0x81+5 - 0x80
        assert!(!v.ring_fixed_period);
        assert_eq!(v.ring_waveform, 1);
    }

    #[test]
    fn command_7_out_of_range_turns_ring_off() {
        let mut v = Voice::new();
        v.ring_audio_source = Some(crate::voice::AudioSourceRef::Waves(0));
        v.ring_mix_active = true;
        process_command(&mut v, 7, 0);
        assert!(v.ring_audio_source.is_none());
        assert!(!v.ring_mix_active);
        assert!(!v.ring_new_waveform);
    }

    #[test]
    fn command_12_tiers_route_to_the_right_field() {
        let mut v = Voice::new();
        process_command(&mut v, 12, 0x20);
        assert_eq!(v.note_max_volume, 0x20);

        process_command(&mut v, 12, 0x50 + 0x10);
        assert_eq!(v.perf_sub_volume, 0x10);

        process_command(&mut v, 12, 0xa0 + 0x05);
        assert_eq!(v.track_master_volume, 0x05);
    }

    #[test]
    fn command_9_sets_pan_from_signed_byte() {
        let mut v = Voice::new();
        process_command(&mut v, 9, 0x00); // centre
        assert_eq!(v.pan, 128);
        process_command(&mut v, 9, 0x80); // -128 -> hard left
        assert_eq!(v.pan, 0);
        process_command(&mut v, 9, 0x7f); // +127 -> hard right
        assert_eq!(v.pan, 255);
        assert_eq!(v.pan_mult_left, panning_left(255));
        // PList pan is transient: set_pan (restored on the next trigger) is untouched.
        assert_eq!(v.set_pan, 128);
    }

    #[test]
    fn command_15_sets_perf_speed_and_wait() {
        let mut v = Voice::new();
        process_command(&mut v, 15, 7);
        assert_eq!(v.perf_speed, 7);
        assert_eq!(v.perf_wait, 7);
    }
}
