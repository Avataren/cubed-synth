//! Replacing one instrument of a loaded song (`AhxPlayer::replace_instrument`):
//! the wire form round-trips through the file loader's own decode, and an edit
//! reaches both the song player and the keyboard preview, from the same song
//! data, without a reload. The goldens (`ahx_render_golden.rs`) pin the
//! untouched render; this only covers what the command adds.

use cubed_ahx::format::{self, Envelope, Instrument, PList, PListEntry, SongFormat};
use cubed_ahx::player::AhxPlayer;
use std::fs;
use std::path::Path;

const RATE: usize = 44100;

fn demos() -> Vec<String> {
    let mut names: Vec<String> =
        fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/songs"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
    names.sort();
    names
}

fn bytes(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/songs")
            .join(name),
    )
    .unwrap()
}

/// The wire form, written the way the TypeScript `serializeAhxInstrument` does
/// (`packages/tracker-playback/src/formats/ahx-instrument-codec.ts`): the file
/// loader's read in reverse.
fn encode(ins: &Instrument, format: SongFormat) -> Vec<u8> {
    let entry_bytes = if format == SongFormat::Hvl { 5 } else { 4 };
    let mut out = vec![0u8; 22 + ins.plist.entries.len() * entry_bytes];
    let e = &ins.envelope;
    out[0] = ins.volume;
    out[1] = (ins.wave_length & 7) | ((ins.filter_speed & 0x1f) << 3);
    out[2..9].copy_from_slice(&[
        e.a_frames, e.a_volume, e.d_frames, e.d_volume, e.s_frames, e.r_frames, e.r_volume,
    ]);
    out[12] = ins.filter_lower_limit | ((ins.filter_speed & 0x20) << 2);
    out[13] = ins.vibrato_delay;
    out[14] = ((ins.hard_cut_release as u8) << 7)
        | (ins.hard_cut_release_frames << 4)
        | ins.vibrato_depth;
    out[15] = ins.vibrato_speed;
    out[16] = ins.square_lower_limit;
    out[17] = ins.square_upper_limit;
    out[18] = ins.square_speed;
    out[19] = ins.filter_upper_limit;
    out[20] = ins.plist.speed;
    out[21] = ins.plist.entries.len() as u8;
    let code = |fx: u8| match fx {
        12 => 6,
        15 => 7,
        other => other,
    };
    for (row, en) in ins.plist.entries.iter().enumerate() {
        let at = 22 + row * entry_bytes;
        let fixed = en.fixed as u8;
        if format == SongFormat::Hvl {
            out[at] = en.fx[0];
            out[at + 1] = en.waveform | (en.fx[1] << 3);
            out[at + 2] = (fixed << 6) | en.note;
            out[at + 3] = en.fx_param[0];
            out[at + 4] = en.fx_param[1];
        } else {
            out[at] = (code(en.fx[1]) << 5) | (code(en.fx[0]) << 2) | (en.waveform >> 1);
            out[at + 1] = ((en.waveform & 1) << 7) | (fixed << 6) | en.note;
            out[at + 2] = en.fx_param[0];
            out[at + 3] = en.fx_param[1];
        }
    }
    out
}

fn render(p: &mut AhxPlayer, seconds: f32) -> (Vec<f32>, Vec<f32>) {
    let frames = (seconds * RATE as f32) as usize;
    let (mut l, mut r) = (vec![0f32; frames], vec![0f32; frames]);
    for start in (0..frames).step_by(128) {
        let end = (start + 128).min(frames);
        p.render(&mut l[start..end], &mut r[start..end]);
    }
    (l, r)
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

fn crossings(x: &[f32]) -> usize {
    x.windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count()
}

fn sound(seconds: f32, mut p: AhxPlayer) -> Vec<f32> {
    p.play();
    render(&mut p, seconds).0
}

/// A plain tone: instant attack, full sustain, `waveform` (1 = triangle,
/// 2 = sawtooth, 3 = square, 4 = noise) picked by PList row 0.
fn tone(waveform: u8, decay_level: u8) -> Instrument {
    Instrument {
        name: "test".into(),
        volume: 64,
        wave_length: 5,
        envelope: Envelope {
            a_frames: 1,
            a_volume: 64,
            d_frames: 20,
            d_volume: decay_level,
            s_frames: 255,
            r_frames: 8,
            r_volume: 0,
        },
        plist: PList {
            speed: 1,
            entries: vec![PListEntry {
                note: 0,
                waveform,
                fixed: false,
                fx: [0, 0],
                fx_param: [0, 0],
            }],
        },
        ..Instrument::default()
    }
}

fn song_instrument(name: &str, idx: usize) -> (Instrument, SongFormat, u8) {
    let song = format::parse(&bytes(name)).unwrap();
    (song.instruments[idx].clone(), song.format, song.version)
}

#[test]
fn every_demo_instrument_survives_the_wire_form() {
    let mut count = 0;
    for name in demos() {
        let song = format::parse(&bytes(&name)).unwrap();
        for idx in 1..=song.instrument_nr as usize {
            let ins = &song.instruments[idx];
            let wire = encode(ins, song.format);
            let back = format::parse_instrument(&wire, song.format, song.version, ins.name.clone())
                .unwrap_or_else(|e| panic!("{name} instrument {idx}: {e}"));
            assert_eq!(&back, ins, "{name} instrument {idx}");
            count += 1;
        }
    }
    assert!(count > 300, "only {count} instruments checked");
}

#[test]
fn a_wire_form_of_the_wrong_length_is_refused_with_the_song_untouched() {
    let (ins, format, version) = song_instrument("karma.ahx", 1);
    let wire = encode(&ins, format);
    assert!(matches!(
        format::parse_instrument(&wire[..wire.len() - 1], format, version, String::new()),
        Err(format::AhxParseError::BadInstrumentLength { .. })
    ));
    assert!(format::parse_instrument(&wire[..10], format, version, String::new()).is_err());
    let mut long = wire.clone();
    long.push(0);
    assert!(format::parse_instrument(&long, format, version, String::new()).is_err());

    let mut p = AhxPlayer::new(&bytes("karma.ahx"), RATE as u32, 2).unwrap();
    assert!(p.replace_instrument(1, &wire[..wire.len() - 1]).is_err());
    assert!(
        p.replace_instrument(0, &wire).is_err(),
        "instruments are 1-based"
    );
    assert!(p
        .replace_instrument(p.instrument_count() + 1, &wire)
        .is_err());
    assert!(p.replace_instrument(1, &[]).is_err());
    // Nothing above changed the song.
    let mut fresh = AhxPlayer::new(&bytes("karma.ahx"), RATE as u32, 2).unwrap();
    fresh.play();
    p.play();
    assert!(render(&mut fresh, 1.0).0 == render(&mut p, 1.0).0);
}

#[test]
fn replacing_an_instrument_with_itself_changes_nothing_in_the_song() {
    for name in ["karma.ahx", "sunspots.hvl"] {
        for hifi in [false, true] {
            let mut plain = AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap();
            let mut edited = AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap();
            plain.set_hifi(hifi);
            edited.set_hifi(hifi);
            let song = format::parse(&bytes(name)).unwrap();
            for idx in 1..=song.instrument_nr as usize {
                edited
                    .replace_instrument(idx, &encode(&song.instruments[idx], song.format))
                    .unwrap();
            }
            plain.play();
            edited.play();
            let (a, b) = (render(&mut plain, 4.0), render(&mut edited, 4.0));
            assert!(
                a == b,
                "{name} (hifi {hifi}): a no-op replace changed the render"
            );
        }
    }
}

#[test]
fn an_edit_reaches_the_song_and_leaves_the_transport_where_it_was() {
    // Every instrument silenced: no volume, no PList, no envelope.
    let name = "karma.ahx";
    let song = format::parse(&bytes(name)).unwrap();
    let mut baseline = AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap();
    let mut edited = AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap();
    baseline.play();
    edited.play();
    let before = (render(&mut baseline, 1.0), render(&mut edited, 1.0));
    assert!(before.0 == before.1);
    let (position, row, ticks) = (edited.position(), edited.row(), edited.ticks());

    for idx in 1..=song.instrument_nr as usize {
        let silent = Instrument {
            name: String::new(),
            envelope: Envelope::default(),
            ..Instrument::default()
        };
        edited
            .replace_instrument(idx, &encode(&silent, song.format))
            .unwrap();
    }
    // The replace itself neither moves the song nor restarts it.
    assert_eq!(
        (edited.position(), edited.row(), edited.ticks()),
        (position, row, ticks)
    );

    let (a, b) = (render(&mut baseline, 8.0).0, render(&mut edited, 8.0).0);
    assert!(edited.is_playing());
    // A voice already holding an instrument keeps what a trigger copied onto it
    // (its volume) until its next trigger, so the edit is complete once every
    // voice has been retriggered: from the fourth second on in this song.
    let settled = RATE * 4..;
    assert!(
        rms(&a[settled.clone()]) > 0.005,
        "the baseline is not audible: {}",
        rms(&a[settled.clone()])
    );
    assert!(
        rms(&b[settled.clone()]) < rms(&a[settled.clone()]) * 0.01,
        "silenced instruments still sound: {}",
        rms(&b[settled.clone()])
    );
    assert!(
        rms(&b[..RATE]) > rms(&b[settled.clone()]) * 10.0,
        "the old notes should carry on until their next trigger"
    );
    // The song itself carried on unaffected.
    assert_eq!(baseline.position(), edited.position());
    assert_eq!(baseline.row(), edited.row());
    assert_eq!(baseline.ticks(), edited.ticks());
}

#[test]
fn a_single_instrument_edit_changes_only_what_plays_it() {
    // Instrument 16 of karma (one of the four the song plays in its first 8 s),
    // one of its own fields changed: the song's sound differs, and putting the
    // original back restores the original render.
    let name = "karma.ahx";
    let (original, format, _) = song_instrument(name, 16);
    let mut quieter = original.clone();
    quieter.volume = 8;
    let (a, mut b, mut c) = (
        AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap(),
        AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap(),
        AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap(),
    );
    b.replace_instrument(16, &encode(&quieter, format)).unwrap();
    c.replace_instrument(16, &encode(&quieter, format)).unwrap();
    c.replace_instrument(16, &encode(&original, format))
        .unwrap();
    let (a, b, c) = (sound(8.0, a), sound(8.0, b), sound(8.0, c));
    assert!(
        a != b,
        "instrument 16 is played in the first 8 s, and volume 8 must change it"
    );
    assert!(
        a == c,
        "putting the original back must restore the original render"
    );
}

fn preview(name: &str, hifi: bool) -> AhxPlayer {
    let mut p = AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap();
    p.set_hifi(hifi);
    p.enable_preview();
    p
}

/// RMS of `seconds` of a preview note held on instrument `idx` at `note`.
fn preview_note(p: &mut AhxPlayer, idx: usize, note: i32, skip: f32, seconds: f32) -> Vec<f32> {
    assert!(p.preview_note_on(idx, note, 127));
    let all = render(p, skip + seconds).0;
    p.preview_note_off();
    all[(skip * RATE as f32) as usize..].to_vec()
}

#[test]
fn a_preview_envelope_edit_changes_the_decay_and_a_waveform_edit_changes_the_timbre() {
    let name = "karma.ahx";
    let format = format::parse(&bytes(name)).unwrap().format;
    for hifi in [false, true] {
        let mut p = preview(name, hifi);
        p.replace_instrument(1, &encode(&tone(3, 64), format))
            .unwrap();
        let held = preview_note(&mut p, 1, 30, 0.6, 0.5);
        let loud = rms(&held);
        assert!(
            loud > 0.02,
            "hifi {hifi}: the reference tone is not audible ({loud})"
        );

        // Envelope: the same tone, decaying to a quarter of the level.
        p.preview_note_off();
        render(&mut p, 0.5);
        p.replace_instrument(1, &encode(&tone(3, 16), format))
            .unwrap();
        let quieter = rms(&preview_note(&mut p, 1, 30, 0.6, 0.5));
        assert!(
            quieter < loud * 0.5 && quieter > loud * 0.1,
            "hifi {hifi}: decay to 16/64 should be about a quarter: {quieter} vs {loud}"
        );

        // Waveform: square -> noise, same pitch, envelope and volume.
        p.preview_note_off();
        render(&mut p, 0.5);
        p.replace_instrument(1, &encode(&tone(3, 64), format))
            .unwrap();
        let square = preview_note(&mut p, 1, 30, 0.3, 0.5);
        p.preview_note_off();
        render(&mut p, 0.5);
        p.replace_instrument(1, &encode(&tone(4, 64), format))
            .unwrap();
        let noise = preview_note(&mut p, 1, 30, 0.3, 0.5);
        assert!(
            crossings(&noise) > crossings(&square) * 3,
            "hifi {hifi}: noise should cross zero far more than a square ({} vs {})",
            crossings(&noise),
            crossings(&square)
        );
    }
}

#[test]
fn after_an_edit_the_preview_builds_what_the_new_instrument_reaches_and_never_in_render() {
    let name = "karma.ahx";
    let format = format::parse(&bytes(name)).unwrap().format;
    let mut p = preview(name, true);
    p.replace_instrument(1, &encode(&tone(3, 64), format))
        .unwrap();
    preview_note(&mut p, 1, 30, 0.2, 0.3);
    let tables = p.hifi_table_count();
    assert!(tables > 0 && p.hifi_miss_count() == 0.0);
    // A different waveform is a different table: not served by the old one.
    p.preview_note_off();
    render(&mut p, 0.5);
    p.replace_instrument(1, &encode(&tone(2, 64), format))
        .unwrap();
    preview_note(&mut p, 1, 30, 0.2, 1.5);
    assert!(
        p.hifi_table_count() > tables,
        "the sawtooth needed tables the square did not"
    );
    assert_eq!(
        p.hifi_miss_count(),
        0.0,
        "the render path had to degrade a table"
    );
}

#[test]
fn after_an_edit_a_hifi_song_player_has_the_new_instruments_tables_before_it_plays() {
    let name = "karma.ahx";
    let song = format::parse(&bytes(name)).unwrap();
    let mut p = AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap();
    p.set_hifi(true);
    assert!(p.hifi_locked());
    let before = p.hifi_table_count();
    // A noise-free but table-hungry instrument: a sawtooth on a square sweep.
    let mut sweeper = tone(2, 64);
    sweeper.square_lower_limit = 8;
    sweeper.square_upper_limit = 60;
    sweeper.square_speed = 1;
    sweeper.plist.entries = vec![
        PListEntry {
            note: 0,
            waveform: 3,
            fixed: false,
            fx: [4, 0],
            fx_param: [0, 0],
        },
        PListEntry {
            note: 0,
            waveform: 0,
            fixed: false,
            fx: [0, 0],
            fx_param: [0, 0],
        },
    ];
    for idx in 1..=song.instrument_nr as usize {
        p.replace_instrument(idx, &encode(&sweeper, song.format))
            .unwrap();
    }
    assert!(p.hifi_locked());
    assert!(
        p.hifi_table_count() > before,
        "the sweeping instruments' tables were not built"
    );
    p.play();
    render(&mut p, 6.0);
    assert_eq!(
        p.hifi_miss_count(),
        0.0,
        "the render path met a table the re-prewarm did not build"
    );
}

#[test]
fn the_prewarm_hold_is_reported_and_bounded() {
    let p = preview("karma.ahx", true);
    let n = p.instrument_count();
    assert!(n > 0);
    assert_eq!(p.preview_warm_hold_ticks(0), 0);
    assert_eq!(p.preview_warm_hold_ticks(n + 1), 0);
    for i in 1..=n {
        let hold = p.preview_warm_hold_ticks(i);
        assert!((16..=1000).contains(&hold), "instrument {i}: {hold}");
    }
    // A plain tone can produce nothing new after its own PList and envelope.
    let mut q = preview("karma.ahx", true);
    q.replace_instrument(
        1,
        &encode(
            &tone(3, 64),
            format::parse(&bytes("karma.ahx")).unwrap().format,
        ),
    )
    .unwrap();
    assert!(
        q.preview_warm_hold_ticks(1) < 100,
        "{}",
        q.preview_warm_hold_ticks(1)
    );
}

#[test]
fn an_edit_that_reaches_no_table_costs_a_hifi_song_player_no_rebuild() {
    let name = "karma.ahx";
    let song = format::parse(&bytes(name)).unwrap();
    let mut p = AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap();
    p.set_hifi(true);
    let before = p.hifi_table_count();
    for idx in 1..=song.instrument_nr as usize {
        let mut ins = song.instruments[idx].clone();
        ins.volume = 9;
        ins.envelope.d_frames = ins.envelope.d_frames.wrapping_add(4);
        ins.hard_cut_release = true;
        ins.hard_cut_release_frames = 2;
        p.replace_instrument(idx, &encode(&ins, song.format))
            .unwrap();
    }
    assert_eq!(
        p.hifi_table_count(),
        before,
        "volume, envelope and hard-cut edits must not rebuild tables"
    );
    assert!(p.hifi_locked());
    p.play();
    render(&mut p, 5.0);
    assert_eq!(p.hifi_miss_count(), 0.0);
}

#[test]
fn a_preview_note_with_no_attack_and_no_decay_holds_at_silence() {
    // What `ahxEnvelopeNeverRises` warns about, as the keyboard hears it: the
    // replayer only moves the envelope in a stage that has frames, so the note
    // never rises. (In a *song* the release then swings the volume, see
    // `envelope.rs`'s `a_release_after_no_attack_or_decay_swings_negative`.)
    let name = "karma.ahx";
    let format = format::parse(&bytes(name)).unwrap().format;
    let mut ins = tone(3, 64);
    ins.envelope.a_frames = 0;
    ins.envelope.d_frames = 0;
    let mut p = preview(name, false);
    p.replace_instrument(1, &encode(&ins, format)).unwrap();
    assert!(p.preview_note_on(1, 30, 127));
    let held = render(&mut p, 0.6).0;
    assert!(
        rms(&held) < 1e-6,
        "the held note should be silent: {}",
        rms(&held)
    );
}

// ---------------------------------------------------------------------------
// B2.1: the walk an edit owes is skipped, batched, and never loops on "play pattern".
// ---------------------------------------------------------------------------

use cubed_ahx::engine::AhxEngine;

fn sweeper() -> Instrument {
    let mut sweeper = tone(2, 64);
    sweeper.square_lower_limit = 8;
    sweeper.square_upper_limit = 60;
    sweeper.square_speed = 1;
    sweeper.plist.entries = vec![
        PListEntry {
            note: 0,
            waveform: 3,
            fixed: false,
            fx: [4, 0],
            fx_param: [0, 0],
        },
        PListEntry {
            note: 0,
            waveform: 0,
            fixed: false,
            fx: [0, 0],
            fx_param: [0, 0],
        },
    ];
    sweeper
}

#[test]
fn prewarming_while_a_position_loops_ends_like_it_does_without_and_keeps_the_setting() {
    let song = format::parse(&bytes("karma.ahx")).unwrap();
    let mut plain = AhxEngine::new(song.clone(), RATE as u32, 2).unwrap();
    plain.set_hifi(true);
    let baseline = plain.prewarm_hifi();
    assert!(baseline.converged);

    let mut looping = AhxEngine::new(song, RATE as u32, 2).unwrap();
    looping.set_hifi(true);
    looping.set_loop_position(true);
    let pre = looping.prewarm_hifi();
    assert!(looping.loop_position(), "the setting must survive the walk");
    assert!(
        pre.converged,
        "a looped position must not send the walk to its tick cap"
    );
    assert_eq!(
        pre.ticks, baseline.ticks,
        "the walk is of the song's own flow either way"
    );
    assert_eq!(pre.tables, baseline.tables);
}

#[test]
fn an_edit_of_an_instrument_no_step_triggers_is_recognised() {
    let mut song = format::parse(&bytes("karma.ahx")).unwrap();
    let e = AhxEngine::new(song.clone(), RATE as u32, 2).unwrap();
    let used: Vec<usize> = (1..=song.instrument_nr as usize)
        .filter(|&i| e.instrument_is_triggered(i))
        .collect();
    assert!(!used.is_empty());
    let target = used[0];
    assert!(!e.instrument_is_triggered(0));
    assert!(!e.instrument_is_triggered(song.instrument_nr as usize + 1));
    for track in song.tracks.iter_mut() {
        for step in track.iter_mut() {
            if step.instrument as usize == target {
                step.instrument = 0;
            }
        }
    }
    let e = AhxEngine::new(song, RATE as u32, 2).unwrap();
    assert!(!e.instrument_is_triggered(target));
    assert!(used.iter().skip(1).all(|&i| e.instrument_is_triggered(i)));
}

#[test]
fn a_batch_of_edits_is_one_walk_and_lands_where_one_by_one_edits_do() {
    let name = "karma.ahx";
    let song = format::parse(&bytes(name)).unwrap();
    let wire = encode(&sweeper(), song.format);
    let mut one_by_one = AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap();
    let mut batched = AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap();
    for p in [&mut one_by_one, &mut batched] {
        p.set_hifi(true);
    }
    let before = batched.hifi_table_count();
    for idx in 1..=song.instrument_nr as usize {
        one_by_one.replace_instrument(idx, &wire).unwrap();
        batched.replace_instrument_deferred(idx, &wire).unwrap();
    }
    assert_eq!(
        batched.hifi_table_count(),
        before,
        "a deferred edit must not walk the song"
    );
    batched.finish_instrument_edits();
    // One by one, every intermediate song (some instruments edited, some not)
    // is walked too, and the tables only *it* reaches stay in the bank: the
    // batch has the final song's tables and no more than that.
    assert!(batched.hifi_table_count() <= one_by_one.hifi_table_count());
    assert!(batched.hifi_table_count() > before);
    // Nothing is owed a second time.
    let settled = batched.hifi_table_count();
    batched.finish_instrument_edits();
    assert_eq!(batched.hifi_table_count(), settled);
    one_by_one.play();
    batched.play();
    let a = render(&mut one_by_one, 4.0);
    let b = render(&mut batched, 4.0);
    assert!(
        a == b,
        "the batch played differently from the same edits one by one"
    );
    assert_eq!(batched.hifi_miss_count(), 0.0);
}

#[test]
fn a_deferred_edit_the_song_refuses_owes_nothing_and_a_good_one_after_it_still_lands() {
    let name = "karma.ahx";
    let song = format::parse(&bytes(name)).unwrap();
    let mut p = AhxPlayer::new(&bytes(name), RATE as u32, 2).unwrap();
    p.set_hifi(true);
    assert!(p.replace_instrument_deferred(1, &[1, 2, 3]).is_err());
    assert!(p
        .replace_instrument_deferred(
            song.instrument_nr as usize + 1,
            &encode(&sweeper(), song.format)
        )
        .is_err());
    let before = p.hifi_table_count();
    p.finish_instrument_edits();
    assert_eq!(p.hifi_table_count(), before);
    p.replace_instrument_deferred(1, &encode(&sweeper(), song.format))
        .unwrap();
    p.finish_instrument_edits();
    assert!(p.hifi_locked());
}
