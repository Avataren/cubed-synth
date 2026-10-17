//! S4 tests: what the browser worklet needs from the chip and the player
//! (per-voice taps, the voice mask, the song-row transport, the preview
//! voice) and the wasm-facing `SidPlayer` shell (`wasm.rs`).
//!
//! As in `tests_s3.rs`, songs go through the file bytes and play on a real
//! chip; expected values are derived from the documented semantics above each
//! assertion, never captured from a run.

use super::chip::ALL_VOICES;
use super::player::SidSongPlayer;
use super::song::*;
use super::waveform::GATE;
use super::*;

/// One PAL frame at 44.1 kHz is 879.8 samples (`player::frame_cycles`, GT
/// parity 0925b): a buffer that holds one; `samples_in_next_frame` says how
/// much of it a frame is.
const SPF: usize = 880;

fn ins(waveform: u8) -> Instrument {
    Instrument {
        name: b"t".to_vec(),
        attack: 0,
        decay: 0,
        sustain: 15,
        release: 0,
        first_wave: waveform | GATE,
        ..Default::default()
    }
}

fn row(note: u8, instrument: u8) -> Row {
    Row {
        note,
        instrument,
        command: 0,
        param: 0,
    }
}

fn blank(rows: usize) -> Pattern {
    Pattern {
        rows: vec![Row::default(); rows],
    }
}

/// Three voices: A-4 triangle, E-5 saw, C-4 pulse, one note each on row 0 of
/// their own pattern. Voice 1 plays a 16-row pattern twice, voice 2 one
/// 32-row pattern, voice 3 an 8-row pattern once (so its orderlist loops
/// inside the song). The song is 32 rows long (the longest first pass).
fn chord() -> SidSong {
    let mut p0 = blank(16);
    p0.rows[0] = row(58, 1); // A-4 (index 57)
    let mut p1 = blank(32);
    p1.rows[0] = row(65, 2); // E-5 (index 64)
    let mut p2 = blank(8);
    p2.rows[0] = row(49, 3); // C-4 (index 48)
    let list = |pattern: u8, repeat: u8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose: 0,
            repeat,
        }],
        restart: 0,
    };
    SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid8580,
        channels: 3,
        speed_multiplier: 1,
        tempo: 6,
        name: b"s4".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0, 2), list(1, 1), list(2, 1)],
        }],
        patterns: vec![p0, p1, p2],
        instruments: vec![
            ins(0x10),
            ins(0x20),
            Instrument {
                pulse_ptr: 1,
                ..ins(0x40)
            },
        ],
        // The pulse voice's width, 0x800, from its pulse table.
        tables: Tables {
            pulse: vec![
                TableRow {
                    left: 0x88,
                    right: 0x00,
                },
                TableRow {
                    left: 0xFF,
                    right: 0x00,
                },
            ],
            ..Default::default()
        },
    }
}

fn player(s: &SidSong) -> SidSongPlayer {
    let bytes = s.to_bytes();
    SidSongPlayer::new(SidSong::parse(&bytes).expect("parses"), DEFAULT_SAMPLE_RATE)
        .expect("player builds")
}

/// Goertzel power of `x` at `hz`.
fn power(x: &[f32], hz: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * hz / DEFAULT_SAMPLE_RATE;
    let c = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s0 = v as f64 + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - c * s1 * s2
}

fn hz(index: u8) -> f64 {
    freq_reg_to_hz(gt_note_freq_reg(index))
}

/// Renders `frames` frames with taps: (mix, [tap0, tap1, tap2]).
fn render_taps(p: &mut SidSongPlayer, frames: usize) -> (Vec<f32>, [Vec<f32>; 3]) {
    let n = frames * SPF;
    let mut out = vec![0.0f32; n];
    let (mut a, mut b, mut c) = (vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]);
    // Worklet-sized quanta, as the browser calls it.
    for start in (0..n).step_by(128) {
        let end = (start + 128).min(n);
        p.render_taps(
            &mut out[start..end],
            [&mut a[start..end], &mut b[start..end], &mut c[start..end]],
        );
    }
    (out, [a, b, c])
}

// ---------------------------------------------------------------------------
// Chip: per-voice taps and the voice mask
// ---------------------------------------------------------------------------

#[test]
fn taps_leave_the_mix_bit_identical_to_render() {
    // A filtered voice too: the filter must see exactly the same input.
    let mut s = chord();
    // Instrument 2's filter table: LP, res 8, every voice routed, cutoff 0x60<<3.
    s.instruments[1].filter_ptr = 1;
    s.tables.filter = vec![
        TableRow {
            left: 0x90,
            right: 0x87,
        },
        TableRow {
            left: 0x00,
            right: 0x60,
        },
        TableRow {
            left: 0xFF,
            right: 0x00,
        },
    ];
    s.model = SidModel::Sid6581;
    let mut plain = player(&s);
    let mut tapped = player(&s);
    let mut mix = vec![0.0f32; 60 * SPF];
    for chunk in mix.chunks_mut(128) {
        plain.render(chunk);
    }
    let (with_taps, _) = render_taps(&mut tapped, 60);
    assert!(
        mix.iter()
            .zip(&with_taps)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "render_taps mix == render mix, bit for bit"
    );
}

#[test]
fn each_tap_carries_its_own_voice() {
    let mut p = player(&chord());
    let (mix, taps) = render_taps(&mut p, 25);
    let tail = |x: &[f32]| x[10 * SPF..].to_vec();
    let (a4, e5, c4) = (hz(57), hz(64), hz(48));
    // Tap i holds voice i's note and not the others' (well over 20 dB apart).
    for (i, own) in [a4, e5, c4].into_iter().enumerate() {
        let t = tail(&taps[i]);
        for (j, other) in [a4, e5, c4].into_iter().enumerate() {
            if i != j {
                assert!(
                    power(&t, own) > 100.0 * power(&t, other),
                    "tap {i} is voice {i}, not voice {j}"
                );
            }
        }
    }
    // No filter in play and an 8580 (no mixer DC): the mix is the three
    // voices summed, and each tap is its share through the same linear DC
    // blocker, so the taps add up to the mix (to f32 rounding).
    let worst = (0..mix.len())
        .map(|k| (taps[0][k] + taps[1][k] + taps[2][k] - mix[k]).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 1e-5, "sum of taps == mix, worst {worst}");
    assert!(mix
        .iter()
        .chain(taps.iter().flatten())
        .all(|s| s.is_finite() && s.abs() <= 1.0));
}

#[test]
fn tap_full_scale_is_what_a_full_voice_reaches_in_its_tap() {
    // Voice 2 plays a saw (the full 12-bit swing, -1..+1 in voice units) at
    // sustain 15 and the song's VOL 15: past the DC blocker's settling its
    // tap swings +-1 voice unit, times the chip's output gain. The scopes
    // divide by `tap_full_scale` to draw that at full height, on both chips.
    for model in [SidModel::Sid8580, SidModel::Sid6581] {
        let mut s = chord();
        s.model = model;
        let mut p = player(&s);
        let full = p.chip().tap_full_scale();
        assert!(
            full > 0.0 && full < 1.0,
            "{model:?}: full scale {full} is a voice's share of the mix"
        );
        let (_, taps) = render_taps(&mut p, 25);
        let peak = taps[1][10 * SPF..]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs())) as f64;
        let ratio = peak / full;
        assert!(
            (0.9..=1.1).contains(&ratio),
            "{model:?}: saw tap peak {peak} vs full scale {full} (ratio {ratio})"
        );
    }
}

#[test]
fn the_voice_mask_drops_a_voice_from_the_mix_and_its_tap() {
    let mut p = player(&chord());
    assert_eq!(p.chip().voice_mask(), ALL_VOICES);
    p.chip_mut().set_voice_mask(0b101); // voice 2 (E-5 saw) muted
    let (mix, taps) = render_taps(&mut p, 25);
    assert!(
        taps[1].iter().all(|&s| s == 0.0),
        "a masked voice's tap is silent"
    );
    let tail = &mix[10 * SPF..];
    assert!(
        power(tail, hz(57)) > 100.0 * power(tail, hz(64)),
        "the mix lost E-5, kept A-4"
    );
    // Every voice masked from the start: nothing at all reaches the output.
    let mut q = player(&chord());
    q.chip_mut().set_voice_mask(0);
    let (silent, _) = render_taps(&mut q, 5);
    assert!(silent.iter().all(|&s| s == 0.0));
}

// ---------------------------------------------------------------------------
// Player: song rows, seek, row loop, preview
// ---------------------------------------------------------------------------

#[test]
fn song_rows_is_the_longest_first_pass_and_song_row_counts_rows() {
    let mut p = player(&chord());
    // Voice 1: 16 rows x 2, voice 2: 32 x 1, voice 3: 8 x 1 -> 32.
    assert_eq!(p.song_rows(), 32);
    assert_eq!(p.song_row(), 0);
    // Tempo 6: a row every 6 frames, so 60 frames are 10 rows.
    for _ in 0..60 {
        p.frame();
    }
    assert_eq!(p.song_row(), 10);
    // It keeps counting past the end as the song loops on.
    for _ in 0..(40 * 6) {
        p.frame();
    }
    assert_eq!(p.song_row(), 50);
}

#[test]
fn seek_lands_on_the_registers_the_song_has_at_that_row() {
    // A song whose state at row 20 differs from its start: voice 1 plays a
    // new note there (C-5) and voice 3's orderlist has looped twice.
    let mut s = chord();
    s.patterns[0].rows[4] = row(61, 1); // C-5 (index 60) on row 4 of each half: song rows 4 and 20
    let mut natural = player(&s);
    while natural.song_row() < 20 {
        natural.frame();
    }
    natural.frame();
    let mut seeked = player(&s);
    for _ in 0..7 {
        seeked.frame(); // somewhere else first: row 1
    }
    seeked.seek_row(20);
    assert_eq!(seeked.song_row(), 20);
    seeked.frame();
    for v in 0..3 {
        let (a, b) = (natural.chip().voice(v), seeked.chip().voice(v));
        assert_eq!(a.frequency(), b.frequency(), "voice {v} frequency");
        assert_eq!(a.pulse_width(), b.pulse_width(), "voice {v} pulse width");
        assert_eq!(a.control(), b.control(), "voice {v} control");
        assert_eq!(
            natural.position(v),
            seeked.position(v),
            "voice {v} orderlist place"
        );
    }
    assert_eq!(seeked.chip().voice(0).frequency(), gt_note_freq_reg(60));
}

#[test]
fn seek_keeps_the_running_chip() {
    let mut p = player(&chord());
    for _ in 0..30 {
        let mut out = vec![0.0f32; SPF];
        let n = p.samples_in_next_frame();
        p.render(&mut out[..n]);
    }
    let cycles = p.chip().cycles();
    let mask = 0b011;
    p.chip_mut().set_voice_mask(mask);
    p.seek_row(3);
    // The same chip: its cycle count and voice mask carry over.
    assert_eq!(p.chip().cycles(), cycles);
    assert_eq!(p.chip().voice_mask(), mask);
}

#[test]
fn a_row_loop_plays_its_range_over_and_over() {
    let mut p = player(&chord());
    p.set_loop_rows(Some((4, 8)));
    let mut seen = Vec::new();
    for _ in 0..(20 * 6) {
        let mut out = vec![0.0f32; SPF];
        let n = p.samples_in_next_frame();
        p.render(&mut out[..n]);
        seen.push(p.song_row());
    }
    // Rows 4..8 only, once the player is past the start: it runs rows 0..8
    // first (it starts at the top), then 8 takes it back to 4 every time.
    assert!(seen.iter().all(|&r| r < 8), "never reaches row 8");
    let after = &seen[8 * 6..];
    assert!(
        after.iter().all(|&r| (4..8).contains(&r)),
        "loops 4..8: {after:?}"
    );
    // An empty range is no loop.
    p.set_loop_rows(Some((5, 5)));
    for _ in 0..(10 * 6) {
        p.frame();
    }
    assert!(p.song_row() >= 8);
}

#[test]
fn preview_plays_an_instrument_without_the_sequencer() {
    let mut p = player(&chord());
    p.set_preview(true);
    assert!(!p.preview_note_on(9, 57), "no instrument 9");
    assert!(p.preview_note_on(1, 57));
    let mut out = vec![0.0f32; 20 * SPF];
    p.render(&mut out);
    // Instrument 1 (triangle) at A-4 on voice 1, gated; the song's own rows
    // (E-5, C-4) never start: the sequencer is off.
    assert_eq!(p.chip().voice(0).frequency(), 7494);
    assert_eq!(p.chip().voice(0).control(), 0x10 | GATE);
    assert_eq!(p.chip().voice(1).control() & GATE, 0);
    assert_eq!(p.song_row(), 0);
    let tail = &out[5 * SPF..];
    assert!(power(tail, hz(57)) > 100.0 * power(tail, hz(64)));
    p.preview_note_off();
    p.frame();
    assert_eq!(
        p.chip().voice(0).control() & GATE,
        0,
        "note off clears the gate"
    );
}

#[test]
fn preview_routes_a_filter_meant_for_any_voice_to_voice_1() {
    // Instrument 1's filter table: low-pass, resonance 8, voice 3 only; cutoff 40.
    let mut song = chord();
    song.instruments[0].filter_ptr = 1;
    song.tables.filter = vec![
        TableRow {
            left: 0x90,
            right: 0x84,
        },
        TableRow {
            left: 0x00,
            right: 0x40,
        },
        TableRow {
            left: 0xFF,
            right: 0x00,
        },
    ];
    let mut p = player(&song);
    p.set_preview(true);
    assert!(p.preview_note_on(1, 57));
    p.frame();
    p.frame();
    // The preview plays on voice 1: routed there, resonance kept.
    assert_eq!(p.chip().written(0x17), 0x81);
    // Played in the song, the routing is the table's own.
    let mut q = player(&song);
    for _ in 0..3 {
        q.frame();
    }
    assert_eq!(q.chip().written(0x17), 0x84);
}

// ---------------------------------------------------------------------------
// The wasm shell
// ---------------------------------------------------------------------------

#[test]
fn sid_player_shell_plays_pauses_and_reports() {
    let bytes = chord().to_bytes();
    assert!(SidPlayer::new(b"nope", 44_100.0).is_err());
    let mut p = SidPlayer::new(&bytes, 44_100.0).expect("builds");
    assert_eq!(p.chip_model(), "8580");
    assert_eq!(p.channels(), 3);
    assert_eq!(p.song_rows(), 32);
    assert_eq!(p.instrument_count(), 3);
    let (mut out, mut a, mut b, mut c) = (
        vec![1.0f32; 128],
        vec![1.0f32; 128],
        vec![1.0f32; 128],
        vec![1.0f32; 128],
    );
    // Paused: silence everywhere, and the song does not move.
    assert_eq!(p.render(&mut out, &mut a, &mut b, &mut c), 128);
    assert!(out.iter().chain(&a).chain(&b).chain(&c).all(|&s| s == 0.0));
    assert_eq!(p.inner().frames(), 0);
    p.play();
    let mut peak = 0.0f32;
    for _ in 0..200 {
        p.render(&mut out, &mut a, &mut b, &mut c);
        peak = out.iter().fold(peak, |m, s| m.max(s.abs()));
    }
    assert!(peak > 0.05, "audible once playing: peak {peak}");
    // Solo voice 1: only it is heard (mask 0b001); a mute on top of it silences it.
    p.set_mute_solo(0, 0b001);
    assert_eq!(p.inner().chip().voice_mask(), 0b001);
    p.set_mute_solo(0b001, 0b001);
    assert_eq!(p.inner().chip().voice_mask(), 0);
    p.set_mute_solo(0b010, 0);
    assert_eq!(p.inner().chip().voice_mask(), 0b101);
    // The end: 32 rows at 6 frames.
    p.seek_row(31);
    assert!(!p.song_end_reached());
    for _ in 0..((6 * SPF) / 128 + 2) {
        p.render(&mut out, &mut a, &mut b, &mut c);
    }
    assert!(p.song_end_reached());
    assert_eq!(p.song_row(), 32);
    // Gain scales the mix and the taps.
    let mut q = SidPlayer::new(&bytes, 44_100.0).unwrap();
    let mut r = SidPlayer::new(&bytes, 44_100.0).unwrap();
    q.play();
    r.play();
    r.set_gain(0.5);
    let (mut o2, mut a2, mut b2, mut c2) = (
        vec![0.0f32; 128],
        vec![0.0f32; 128],
        vec![0.0f32; 128],
        vec![0.0f32; 128],
    );
    for _ in 0..50 {
        q.render(&mut out, &mut a, &mut b, &mut c);
        r.render(&mut o2, &mut a2, &mut b2, &mut c2);
    }
    assert!(out.iter().zip(&o2).all(|(x, y)| (x * 0.5 - y).abs() < 1e-7));
    assert!(a.iter().zip(&a2).all(|(x, y)| (x * 0.5 - y).abs() < 1e-7));
}

#[test]
fn sid_player_preview_sounds_without_play() {
    let bytes = chord().to_bytes();
    let mut p = SidPlayer::new(&bytes, 44_100.0).unwrap();
    p.enable_preview();
    assert!(p.preview_note_on(3, 48));
    let (mut out, mut a, mut b, mut c) = (
        vec![0.0f32; 128],
        vec![0.0f32; 128],
        vec![0.0f32; 128],
        vec![0.0f32; 128],
    );
    let mut peak = 0.0f32;
    for _ in 0..100 {
        p.render(&mut out, &mut a, &mut b, &mut c);
        peak = out.iter().fold(peak, |m, s| m.max(s.abs()));
    }
    assert!(peak > 0.05, "preview is audible: {peak}");
    assert_eq!(p.song_row(), 0);
}

#[test]
fn sid_player_shell_switches_the_6581_revision_while_playing() {
    let mut song = chord();
    song.model = SidModel::Sid6581;
    let bytes = song.to_bytes();
    let mut p = SidPlayer::new(&bytes, 44_100.0).unwrap();
    assert_eq!(p.revision(), "r3");
    p.play();
    let (mut out, mut a, mut b, mut c) = (
        vec![0.0; 4096],
        vec![0.0; 4096],
        vec![0.0; 4096],
        vec![0.0; 4096],
    );
    p.render(&mut out, &mut a, &mut b, &mut c);
    for name in ["r2", "r3", "r4", "r4ar", "gt"] {
        assert!(p.set_revision(name), "{name}");
        assert_eq!(p.revision(), name);
        assert!(p.is_playing());
        p.render(&mut out, &mut a, &mut b, &mut c);
    }
    assert!(!p.set_revision("r5"));
    assert_eq!(p.revision(), "gt");
}

#[test]
fn sid_player_shell_switches_the_chip_while_playing_and_keeps_its_place() {
    let mut song = chord();
    song.model = SidModel::Sid6581;
    let bytes = song.to_bytes();
    let mut p = SidPlayer::new(&bytes, 44_100.0).unwrap();
    p.play();
    let n = 44_100;
    let (mut out, mut a, mut b, mut c) = (vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    p.render(&mut out, &mut a, &mut b, &mut c);
    let row = p.song_row();
    let before = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(before > 0.05, "the 6581 chord sounds: {before}");
    p.set_chip_model(true);
    assert_eq!(p.chip_model(), "8580");
    assert!(p.is_playing());
    // No restart: the song carries on from where it was, and still sounds.
    p.render(&mut out, &mut a, &mut b, &mut c);
    assert!(p.song_row() >= row, "row {} then {}", row, p.song_row());
    let after = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(after > 0.05, "the 8580 chord sounds: {after}");
    p.set_chip_model(false);
    assert_eq!(p.chip_model(), "6581");
    p.render(&mut out, &mut a, &mut b, &mut c);
    assert!(p.song_row() >= row);
}
