//! S5.16 tests: what GoatTracker leaves alone on a note, and two filter-table
//! rows the player mis-read. Checked against GT's own playroutine (gplay.c) on
//! "Coconut Conundrum", register by register.
//!
//! The pulse width carries across notes, as in GoatTracker.
//!
//! GT sets a channel's pulse width only from its pulse table (gplay.c:375-381,
//! 868-873), never on a note, so an instrument with no pulse table plays at
//! whatever width the channel last had. A GT instrument has no width of its
//! own. Found in "Coconut Conundrum" (Stinsen): its bass alternates
//! an instrument with a pulse table and one without, and the second restarted
//! at width 0 (a pulse of width 0 is DC) and was heard as a "tock".

use super::player::SidSongPlayer;
use super::song::*;
use super::*;

/// One PAL frame at 44.1 kHz is 879.8 samples (`player::frame_cycles`, GT
/// parity 0925b): a buffer that holds one; `samples_in_next_frame` says how
/// much of it a frame is.
const SPF: usize = 880;

fn t(l: u8, r: u8) -> TableRow {
    TableRow { left: l, right: r }
}

fn row(note: u8, instrument: u8) -> Row {
    Row {
        note,
        instrument,
        command: 0,
        param: 0,
    }
}

/// Instrument 1 sets the width to 0x640 with its pulse table (row 1: left
/// 0x86 = set, high nibble 6, right 0x40); instrument 2 has a pulse waveform
/// and no pulse table, like GT's.
fn song(rows: Vec<Row>) -> SidSong {
    let n = rows.len();
    let base = |wave_ptr: u8, pulse_ptr: u8| Instrument {
        name: b"t".to_vec(),
        sustain: 15,
        first_wave: 0x09,
        wave_ptr,
        pulse_ptr,
        ..Default::default()
    };
    let list = |pattern: u8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose: 0,
            repeat: 1,
        }],
        restart: 0,
    };
    let s = SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid8580,
        channels: 3,
        speed_multiplier: 1,
        tempo: 6,
        name: b"s516".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![
            Pattern { rows },
            Pattern {
                rows: vec![Row::default(); n],
            },
        ],
        instruments: vec![base(1, 1), base(3, 0)],
        tables: Tables {
            // Row 1: pulse wave, note 0, stop. Row 3 is the same for instrument 2.
            wave: vec![t(0x41, 0x00), t(0xFF, 0x00), t(0x41, 0x00), t(0xFF, 0x00)],
            pulse: vec![t(0x86, 0x40), t(0xFF, 0x00)],
            ..Default::default()
        },
    };
    SidSong::parse(&s.to_bytes()).expect("parses")
}

/// Voice 1's pulse width register at the end of each frame of `frames`.
fn widths(s: &SidSong, frames: usize) -> Vec<u16> {
    let mut p = SidSongPlayer::new(s.clone(), DEFAULT_SAMPLE_RATE).expect("player builds");
    let mut out = vec![0.0f32; SPF];
    (0..frames)
        .map(|_| {
            let n = p.samples_in_next_frame();
            p.render(&mut out[..n]);
            p.chip().voice(0).pulse_width()
        })
        .collect()
}

#[test]
fn an_instrument_without_a_pulse_table_keeps_the_channels_width() {
    // Tempo 6: row r starts at frame 6r. Row 0 (instrument 1) sets 0x640 via
    // its table; row 1 (instrument 2, no width, no table) must leave it.
    let w = widths(&song(vec![row(49, 1), row(49, 2)]), 12);
    assert_eq!(w[5], 0x640, "instrument 1's table set the width");
    assert_eq!(
        w[11], 0x640,
        "instrument 2 has none, so the channel keeps it (GT)"
    );
}

#[test]
fn the_first_note_of_a_song_still_starts_at_width_0() {
    // GT's channel starts at 0 and nothing has set it yet.
    let w = widths(&song(vec![row(49, 2)]), 6);
    assert_eq!(w[5], 0);
}

/// Like `song`, but with instrument 1's pulse table modulating for 16 frames
/// (+0x10 a frame) and instrument 2 (no pulse table) on the second row.
fn modulating_song(rows: Vec<Row>) -> SidSong {
    let mut s = song(rows);
    s.tables.pulse = vec![t(0x86, 0x40), t(0x10, 0x10), t(0xFF, 0x00)];
    SidSong::parse(&s.to_bytes()).expect("parses")
}

#[test]
fn an_instrument_without_a_pulse_table_leaves_the_running_one_going() {
    // GT sets a channel's pulse pointer only from an instrument that has a
    // table (gplay.c:375-378), so instrument 2's note keeps instrument 1's
    // modulation running: the width goes on rising through row 1 (frames
    // 6..11). Before S5.16 the pointer was cleared and the width froze.
    let w = widths(&modulating_song(vec![row(49, 1), row(49, 2)]), 12);
    assert!(
        w[5] >= 0x640,
        "instrument 1's table set the width and started modulating"
    );
    for f in 7..12 {
        assert!(
            w[f] > w[f - 1],
            "frame {f}: the width keeps rising under instrument 2 ({:#x} -> {:#x})",
            w[f - 1],
            w[f]
        );
    }
}

/// Instrument 1 with a filter table of `rows`; channel 1 plays it once.
fn filter_song(rows: Vec<TableRow>) -> SidSong {
    let mut s = song(vec![row(49, 1)]);
    s.instruments[0].filter_ptr = 1;
    s.tables.filter = rows;
    SidSong::parse(&s.to_bytes()).expect("parses")
}

/// The filter's (cutoff register, resonance nibble, mode bits) after `frames`.
fn filter_after(s: &SidSong, frames: usize) -> (u16, u8, u8) {
    let mut p = SidSongPlayer::new(s.clone(), DEFAULT_SAMPLE_RATE).expect("player builds");
    let mut out = vec![0.0f32; SPF];
    for _ in 0..frames {
        let n = p.samples_in_next_frame();
        p.render(&mut out[..n]);
    }
    let f = p.chip().filter();
    (f.cutoff_reg(), f.resonance(), f.mode())
}

#[test]
fn a_filter_set_row_takes_any_left_byte_from_0x80_to_0xfe() {
    // Only 0xFF is a jump (gplay.c:265). 0xF1 sets mode (0xF1 & 0x70 = LP+BP+HP)
    // and resonance 3 / voice 1; the player used to skip it (its range ended at
    // 0xF0), leaving the resonance of whatever instrument came before.
    // (Frame 1: the note's table is heard from the frame after it, S5.19.)
    let (_, res, mode) = filter_after(&filter_song(vec![t(0xF1, 0x31), t(0xFF, 0x00)]), 2);
    assert_eq!(res, 3);
    assert_eq!(mode, 0x70);
}

#[test]
fn a_cutoff_row_straight_after_a_set_row_is_taken_on_the_same_frame() {
    // gplay.c:271-275 ("Can be combined with cutoff set"): set + cutoff in one
    // frame, so the cutoff is 0x25 << 3 with the mode, on the table's first
    // frame (frame 1: the note's table is heard from the frame after it, S5.19).
    let (cutoff, res, mode) = filter_after(
        &filter_song(vec![t(0x91, 0xF1), t(0x00, 0x25), t(0xFF, 0x00)]),
        2,
    );
    assert_eq!((cutoff, res, mode), (0x25 << 3, 15, 0x10));
}

// ---------------------------------------------------------------------------
// The GT-reference 6581 profile (`revision::GT_REF`) and R4AR beside it
// ---------------------------------------------------------------------------

use super::filter::{cutoff_hz_6581_with, resonance_q_6581_with};
use super::revision::{DieRevision, GT_REF, R3, R4AR};
use super::Chip;

#[test]
fn the_default_6581_is_r3_and_gt_ref_and_r4ar_are_still_profiles_a_chip_can_take() {
    let d = Chip::new(SidModel::Sid6581).unwrap();
    let r = Chip::with_profile(SidModel::Sid6581, DEFAULT_SAMPLE_RATE, &R4AR).unwrap();
    let g = Chip::with_profile(SidModel::Sid6581, DEFAULT_SAMPLE_RATE, &GT_REF).unwrap();
    assert_eq!(d.filter().cutoff(), cutoff_hz_6581_with(&R3, 0));
    assert_eq!(g.filter().cutoff(), cutoff_hz_6581_with(&GT_REF, 0));
    assert_eq!(r.filter().cutoff(), cutoff_hz_6581_with(&R4AR, 0));
    assert_eq!(GT_REF.revision, DieRevision::GtRef);
}

#[test]
fn gt_ref_cutoff_hits_the_measured_points_and_stays_below_r4ars_chords_at_the_low_end() {
    // Measured f0 of GT's 6581 low-pass (two-pole fit, res 0), Hz.
    for (reg, hz) in [
        (0u16, 219.0),
        (0x100, 248.0),
        (0x200, 417.0),
        (0x280, 778.0),
        (0x300, 1_628.0),
        (0x380, 3_331.0),
    ] {
        assert!(
            (cutoff_hz_6581_with(&GT_REF, reg) - hz).abs() < 1e-9,
            "reg {reg:#05x}"
        );
    }
    // Between anchors it is log-linear: 0x160 is sqrt(266 * 299).
    assert!((cutoff_hz_6581_with(&GT_REF, 0x160) - (266.0f64 * 299.0).sqrt()).abs() < 1e-9);
    // R4AR's four chords ran 15-25% high across 0x080..=0x1C0; GtRef is under them.
    for reg in [0x080u16, 0x0C0, 0x100, 0x140, 0x180, 0x1C0] {
        assert!(
            cutoff_hz_6581_with(&GT_REF, reg) < cutoff_hz_6581_with(&R4AR, reg) * 0.95,
            "reg {reg:#05x}"
        );
    }
    // Above the measurement the high piece is R4AR's.
    assert_eq!(
        cutoff_hz_6581_with(&GT_REF, 0x500),
        cutoff_hz_6581_with(&R4AR, 0x500)
    );
}

#[test]
fn gt_ref_resonance_is_linear_and_r4ars_is_the_old_exponential() {
    // Measured Q: 0.72 at 0, 1.76 at 15, linear in between.
    assert!((resonance_q_6581_with(&GT_REF, 0) - 0.707).abs() < 1e-12);
    assert!((resonance_q_6581_with(&GT_REF, 15) - (0.707 + 15.0 * 0.0698)).abs() < 1e-12);
    for r in 1..16u8 {
        let step = resonance_q_6581_with(&GT_REF, r) - resonance_q_6581_with(&GT_REF, r - 1);
        assert!((step - 0.0698).abs() < 1e-12, "res {r}");
    }
    // R4AR keeps 0.707 * 2^(res/12): 1.414 at 12.
    assert!((resonance_q_6581_with(&R4AR, 12) - 1.414).abs() < 1e-12);
}

#[test]
fn an_8580_ignores_the_profile() {
    let a = Chip::with_profile(SidModel::Sid8580, DEFAULT_SAMPLE_RATE, &R4AR).unwrap();
    let b = Chip::with_profile(SidModel::Sid8580, DEFAULT_SAMPLE_RATE, &GT_REF).unwrap();
    assert_eq!(a.filter().cutoff(), b.filter().cutoff());
    assert_eq!(a.filter().q(), b.filter().q());
}

// ---------------------------------------------------------------------------
// Spaced register writes: the ADSR delay bug at a note-on (found on the
// snare of "Coconut Conundrum": reSID's hit starts ~12-30 ms after its gate,
// ours started at once)
// ---------------------------------------------------------------------------

/// A voice parked in a hard restart (AD 0x0f, SR 0x00, gate off, envelope at
/// zero), then GT's note-on writes for one frame: SR, AD, frequency, then the
/// control register with the gate. `spaced` writes them 9 cycles apart the way
/// GT does (+4 before the control register); otherwise all in one instant.
/// Returns the envelope level after `cycles` more cycles.
fn envelope_after_note_on(spaced: bool, cycles: u64) -> u8 {
    let mut c = Chip::new(SidModel::Sid6581).unwrap();
    c.write(0x18, 0x0f);
    c.write(0x05, 0x0f);
    c.write(0x06, 0x00);
    c.write(0x04, 0x40); // pulse, gate off: release with rate period 9
    c.clock_cycles(30_000); // long enough to reach zero and freeze
    let writes: [(u8, u8); 5] = [
        (0x06, 0xF7),
        (0x05, 0x00),
        (0x00, 0x14),
        (0x01, 0x03),
        (0x04, 0x09),
    ];
    let mut at = 0u64;
    for (reg, val) in writes {
        if reg == 0x04 {
            at += 4;
        }
        if spaced {
            c.write_after(at, reg, val);
        } else {
            c.write(reg, val);
        }
        at += 9;
    }
    c.clock_cycles(cycles);
    c.voice(0).envelope_level()
}

#[test]
fn write_after_applies_a_write_on_its_cycle_not_before() {
    let mut c = Chip::new(SidModel::Sid6581).unwrap();
    c.write_after(10, 0x00, 0xAB);
    c.write_after(10, 0x01, 0x12);
    c.clock_cycles(10);
    assert_eq!(c.voice(0).frequency(), 0, "cycle 10 is not reached yet");
    c.clock_cycles(1);
    assert_eq!(
        c.voice(0).frequency(),
        0x12AB,
        "both writes landed once it is"
    );
    c.write_after(5, 0x00, 0xCD);
    c.flush_writes();
    assert_eq!(
        c.voice(0).frequency(),
        0x12CD,
        "flush applies what is pending at once"
    );
}

#[test]
fn a_volume_write_that_lands_mid_render_is_heard_from_its_sample() {
    // Voice 1 as DC: a pulse of width 0 is always high, held at full envelope.
    // VOL 0, so the chip is silent until the scheduled $18 write, which lands
    // ~90 samples into a single 200-sample render (2000 cycles / 22.34 per
    // sample). The mix must follow it there, not at the next render call.
    let mut c = Chip::new(SidModel::Sid8580).unwrap();
    c.write(0x05, 0x00);
    c.write(0x06, 0xF0);
    c.write(0x04, 0x41);
    c.clock_cycles(20_000);
    assert_eq!(c.voice(0).envelope_level(), 255);
    c.write_after(2_000, 0x18, 0x0F);
    let mut out = [0f32; 200];
    c.render(&mut out);
    assert!(
        out[..85].iter().all(|&s| s == 0.0),
        "silent before the write lands"
    );
    assert!(
        out[95..].iter().all(|&s| s.abs() > 0.1),
        "audible from the write on"
    );
}

#[test]
fn a_spaced_note_on_lands_on_the_adsr_delay_bug_and_an_instant_one_does_not() {
    // Instant writes: the rate counter is still under the attack period (9)
    // when the gate opens, so the 2 ms attack starts at once: 5 ms later the
    // level is at the top (255 after ~2.3 ms).
    assert_eq!(envelope_after_note_on(false, 5_000), 255);
    // Spaced writes (GT's): SR=0xF7 changes the release period to 313 first;
    // by the time the gate opens the counter is past the attack period 9, so
    // it must run on to 0x7FFF and wrap: the attack is ~32 ms late. 5 ms
    // after the gate the envelope has not moved; 40 ms after it has.
    assert_eq!(envelope_after_note_on(true, 5_000), 0);
    assert_eq!(envelope_after_note_on(true, 40_000), 255);
}

/// Voice 1 plays a pulse note twice, six frames (one row) apart, on an
/// instrument with a hard restart 2 frames before the next note (AD 0x00,
/// SR 0xF7, first-frame waveform 0x09): GT's usual note-on. Returns voice 1's
/// envelope level at the end of each of the first 9 rendered frames.
fn hard_restart_note_levels() -> Vec<u8> {
    let ins = Instrument {
        name: b"hr".to_vec(),
        sustain: 15,
        release: 7,
        first_wave: 0x09,
        gate_timer: 2,
        hard_restart: true,
        wave_ptr: 1,
        ..Default::default()
    };
    let n = 16;
    let list = |pattern: u8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose: 0,
            repeat: 1,
        }],
        restart: 0,
    };
    let mut rows = vec![Row::default(); n];
    rows[0] = row(49, 1);
    rows[1] = row(49, 1);
    let s = SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid6581,
        channels: 3,
        speed_multiplier: 1,
        tempo: 6,
        name: b"s516hr".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![
            Pattern { rows },
            Pattern {
                rows: vec![Row::default(); n],
            },
        ],
        instruments: vec![ins],
        tables: Tables {
            wave: vec![t(0x41, 0x00), t(0xFF, 0x00)],
            ..Default::default()
        },
    };
    let s = SidSong::parse(&s.to_bytes()).expect("parses");
    let mut p = SidSongPlayer::new(s, DEFAULT_SAMPLE_RATE).expect("player builds");
    let mut out = vec![0.0f32; SPF];
    (0..9)
        .map(|_| {
            let n = p.samples_in_next_frame();
            p.render(&mut out[..n]);
            p.chip().voice(0).envelope_level()
        })
        .collect()
}

#[test]
fn the_players_note_on_after_a_hard_restart_is_delayed_by_the_adsr_bug_as_in_gt() {
    // The second note starts on frame 6 (row 1). Its gate opens in the first
    // ~250 cycles of that frame; with GT's spaced writes (SR before AD before
    // the gate) the attack is ~32 ms late, so at the end of frame 6 (20 ms
    // after the gate) the attack has not started: the envelope sits at the
    // floor the hard restart's release left it at (0 or 1: S5.17's AD 0x0F
    // keeps the note up ~33 ms, so the release ends on the way in), and by the
    // end of frame 7 it has run. Written all at once (the old behaviour) it
    // was at the top already at the end of frame 6. GT-parity 0925b: at
    // PAL frames (19 656 cycles, 49 fewer each) the release is ~100 cycles
    // short of its last step at the bottom (one step per 30 x 313 cycles), so
    // the floor is 2 (was 0 or 1): frames 5-7 read 2, 2, 255.
    let l = hard_restart_note_levels();
    assert!(
        l[6] <= 2 && l[6] <= l[5],
        "frame 6: still waiting on the rate counter, levels {l:?}"
    );
    assert_eq!(l[7], 255, "frame 7: the attack has run, levels {l:?}");
}

// ---------------------------------------------------------------------------
// Output level: trimmed to GoatTracker's playback (S5.16)
// ---------------------------------------------------------------------------

use super::chip::GAIN_TRIM_8580;

/// AC RMS of a steady unfiltered triangle (pitch reg 0x1D45, sustain 15,
/// volume 15), 2 s after note-on, on `c`.
fn steady_tri_rms(mut c: Chip) -> f64 {
    c.write(0x18, 0x0F);
    c.write(0x00, 0x45);
    c.write(0x01, 0x1D);
    c.write(0x06, 0xF0);
    c.write(0x04, 0x11);
    let mut out = vec![0.0f32; 88_200];
    c.render(&mut out);
    let tail = &out[44_100..];
    let mean = tail.iter().map(|&v| v as f64).sum::<f64>() / tail.len() as f64;
    (tail.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / tail.len() as f64).sqrt()
}

#[test]
fn the_trim_is_a_pure_scale_of_the_reference_level() {
    for (model, trim) in [
        (SidModel::Sid8580, GAIN_TRIM_8580),
        (SidModel::Sid6581, GT_REF.gain_trim),
    ] {
        let mut reference = Chip::new(model).unwrap();
        reference.set_gain_trim(1.0);
        let r = steady_tri_rms(reference);
        let d = steady_tri_rms(Chip::new(model).unwrap());
        assert!(
            (d / r - trim).abs() < 1e-9,
            "{model:?}: {d} / {r} = {}, want {trim}",
            d / r
        );
    }
}

#[test]
fn r4ar_keeps_the_reference_level_and_gt_refs_level_follows_its_own_dc_and_trim() {
    use super::chip::CHIP_GAIN;
    assert_eq!(R4AR.gain_trim, 1.0);
    const { assert!(GAIN_TRIM_8580 < 1.0) };
    let r4ar =
        steady_tri_rms(Chip::with_profile(SidModel::Sid6581, DEFAULT_SAMPLE_RATE, &R4AR).unwrap());
    let gt = steady_tri_rms(Chip::new(SidModel::Sid6581).unwrap());
    // The AC level of a steady tone scales with headroom gain x trim; the
    // headroom gain follows the profile's DC terms (0.5625 vs 0.25 voice DC).
    let want = GT_REF.chip_gain(CHIP_GAIN) * GT_REF.gain_trim / R4AR.chip_gain(CHIP_GAIN);
    assert!((gt / r4ar - want).abs() < 1e-9, "{} vs {want}", gt / r4ar);
    // R4AR's own level is the S2 reference: its trim changes nothing.
    let mut r = Chip::with_profile(SidModel::Sid6581, DEFAULT_SAMPLE_RATE, &R4AR).unwrap();
    r.set_gain_trim(1.0);
    assert_eq!(steady_tri_rms(r), r4ar);
}

#[test]
fn gt_ref_carries_the_measured_dc_and_r4ar_keeps_its_own() {
    assert_eq!(GT_REF.voice_dc, 0.5625);
    assert_eq!(R4AR.voice_dc, 0.25);
    // The DC is per chip, so the note-start step (an open envelope on a
    // no-waveform note: GT's first-frame `09`) differs. Unfiltered, at the
    // level of a full-volume voice, the largest 5 ms mean after the note
    // starts is -0.051 in GoatTracker's playback (reSID), -0.053 in ours at
    // GtRef's DC and -0.105 at R4AR's.
    let step = |p: &'static super::revision::RevisionProfile| {
        let mut c = Chip::with_profile(SidModel::Sid6581, DEFAULT_SAMPLE_RATE, p).unwrap();
        c.write(0x18, 0x0F);
        c.write(0x00, 0x14);
        c.write(0x01, 0x03);
        c.write(0x03, 0x08);
        c.write(0x06, 0xF7);
        let mut idle = vec![0.0f32; 3 * SPF]; // the volume write's mixer-DC step decays first
        c.render(&mut idle);
        c.write(0x04, 0x09); // test + gate, no waveform
        let mut out = vec![0.0f32; 3 * SPF];
        c.render(&mut out);
        out.chunks(220)
            .filter(|w| w.len() == 220)
            .map(|w| w.iter().map(|&v| v as f64).sum::<f64>() / 220.0)
            .fold(0.0f64, |m, v| if v.abs() > m.abs() { v } else { m })
    };
    let (g, r) = (step(&GT_REF), step(&R4AR));
    assert!((g + 0.051).abs() < 0.008, "GtRef step {g}");
    assert!(r < -0.09, "R4AR step {r}");
}

#[test]
fn the_two_chips_are_equally_loud_as_resids_are() {
    // Measured against GoatTracker's playback: on every waveform and pitch
    // tried its 6581 and 8580 have the same RMS (0.098 for a full-volume
    // triangle). Ours differed 1.16 vs 1.65 times that before the trims;
    // after them the chips agree to a couple of percent.
    let a = steady_tri_rms(Chip::new(SidModel::Sid6581).unwrap());
    let b = steady_tri_rms(Chip::new(SidModel::Sid8580).unwrap());
    assert!((a / b - 1.0).abs() < 0.03, "6581 {a} vs 8580 {b}");
    // And both sit at GT's level, 0.098 (measured), within 3%.
    assert!((a / 0.098 - 1.0).abs() < 0.03, "6581 {a}");
    assert!((b / 0.098 - 1.0).abs() < 0.03, "8580 {b}");
}

// ---------------------------------------------------------------------------
// Key off / key on in the next row act at the gate-timer prefetch
// ---------------------------------------------------------------------------

/// Voice 1: a note on row 0, then `later` (row index, note) rows, on an
/// instrument with gate timer 2, tempo 6. Returns voice 1's gate bit after each
/// of the first `frames` rendered frames.
fn gate_bits_around(later: &[(usize, u8)], frames: usize) -> Vec<bool> {
    let ins = Instrument {
        name: b"g".to_vec(),
        sustain: 15,
        // GT's default first-frame byte; $00 would leave a fresh channel's
        // gate shut (S5.19).
        first_wave: 0x09,
        gate_timer: 2,
        hard_restart: true,
        wave_ptr: 1,
        ..Default::default()
    };
    let n = 16;
    let list = |pattern: u8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose: 0,
            repeat: 1,
        }],
        restart: 0,
    };
    let mut rows = vec![Row::default(); n];
    rows[0] = row(49, 1);
    for &(r, note) in later {
        rows[r] = row(note, 0);
    }
    let s = SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid8580,
        channels: 3,
        speed_multiplier: 1,
        tempo: 6,
        name: b"s516g".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![
            Pattern { rows },
            Pattern {
                rows: vec![Row::default(); n],
            },
        ],
        instruments: vec![ins],
        tables: Tables {
            wave: vec![t(0x41, 0x00), t(0xFF, 0x00)],
            ..Default::default()
        },
    };
    let s = SidSong::parse(&s.to_bytes()).expect("parses");
    let mut p = SidSongPlayer::new(s, DEFAULT_SAMPLE_RATE).expect("player builds");
    let mut out = vec![0.0f32; SPF];
    (0..frames)
        .map(|_| {
            let n = p.samples_in_next_frame();
            p.render(&mut out[..n]);
            p.chip().voice(0).control() & 1 != 0
        })
        .collect()
}

#[test]
fn a_key_off_in_the_next_row_clears_the_gate_gatetimer_frames_early() {
    // Row 1 (the key off) starts on frame 6; with gate timer 2 GT reads it two
    // ticks earlier, on frame 4, and drops the gate then (gplay.c:920). Before
    // S5.16 the gate stayed up until the row itself.
    let g = gate_bits_around(&[(1, 126)], 8);
    assert_eq!(
        g,
        vec![true, true, true, true, false, false, false, false],
        "{g:?}"
    );
}

#[test]
fn a_key_on_in_the_next_row_raises_the_gate_gatetimer_frames_early_too() {
    // Key off on row 1 (down from frame 4), key on on row 2, which starts on
    // frame 12: read two ticks earlier, on frame 10, the gate is up again then.
    let g = gate_bits_around(&[(1, 126), (2, 127)], 14);
    let want = [
        true, true, true, true, false, false, false, false, false, false, true, true, true, true,
    ];
    assert_eq!(g, want.to_vec(), "{g:?}");
}

// ---------------------------------------------------------------------------
// The filtered path is inverted, as in GoatTracker's playback
// ---------------------------------------------------------------------------

/// Correlation of a saw with its open low-pass-filtered copy on `c` (a small
/// lag search: the filter delays). +1 = same polarity, -1 = inverted.
fn filtered_vs_direct_correlation(mut make: impl FnMut() -> Chip) -> f64 {
    let render = |mut c: Chip, fc: u16, mode: u8, route: u8| {
        c.write(0x18, 0x0F | (mode << 4));
        c.write(0x15, (fc & 7) as u8);
        c.write(0x16, (fc >> 3) as u8);
        c.write(0x17, route);
        c.write(0x00, 1500u16 as u8);
        c.write(0x01, (1500u16 >> 8) as u8);
        c.write(0x03, 0x08);
        c.write(0x06, 0xF0);
        c.write(0x04, 0x21);
        let mut out = vec![0.0f32; 44_100 * 2];
        c.render(&mut out);
        let tail: Vec<f64> = out[44_100..].iter().map(|&v| v as f64).collect();
        let mean = tail.iter().sum::<f64>() / tail.len() as f64;
        tail.iter().map(|v| v - mean).collect::<Vec<f64>>()
    };
    let direct = render(make(), 0, 0, 0);
    let lp = render(make(), 1000, 1, 1);
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let norm = (dot(&direct, &direct) * dot(&lp, &lp)).sqrt();
    (0..60)
        .map(|lag| dot(&direct[..direct.len() - lag], &lp[lag..]) / norm)
        .fold(0.0f64, |m, v| if v.abs() > m.abs() { v } else { m })
}

#[test]
fn the_filtered_path_is_inverted_against_the_direct_path_on_both_chips() {
    for model in [SidModel::Sid6581, SidModel::Sid8580] {
        let r = filtered_vs_direct_correlation(|| Chip::new(model).unwrap());
        assert!(r < -0.99, "{model:?}: correlation {r}");
        // The reference (the S1/S2 pins) keeps the filter non-inverting.
        let r = filtered_vs_direct_correlation(|| {
            let mut c = Chip::new(model).unwrap();
            c.set_filter_sign(1.0);
            c
        });
        assert!(r > 0.99, "{model:?} reference: correlation {r}");
    }
}

#[test]
fn gt_refs_filter_is_level_independent_and_r4ars_compresses() {
    use super::filter::{cutoff_hz_6581_with, Filter, LP};
    // A resonant low-pass peak at three drive levels. reSID's filter core is
    // linear, and its bass was 8% louder than ours through the old limit
    // (filtered level ours / reSID: sat 4 -> 0.919, effectively off -> 0.999).
    let peak = |p: &'static super::revision::RevisionProfile, amp: f64| {
        let reg = 0x300;
        let hz = cutoff_hz_6581_with(p, reg);
        let mut f = Filter::with_profile(SidModel::Sid6581, p, 44_100.0);
        f.set_mode(LP);
        f.set(reg, 15);
        let mut m: f64 = 0.0;
        for i in 0..44_100 {
            let y = f.process(amp * (2.0 * std::f64::consts::PI * hz * i as f64 / 44_100.0).sin());
            if i > 22_050 {
                m = m.max(y.abs());
            }
        }
        m / amp
    };
    let (a, b) = (peak(&GT_REF, 0.01), peak(&GT_REF, 3.0));
    assert!((b / a - 1.0).abs() < 1e-3, "GtRef {a} vs {b}");
    let (a, b) = (peak(&R4AR, 0.01), peak(&R4AR, 3.0));
    assert!(b < a * 0.85, "R4AR compresses: {a} vs {b}");
}
