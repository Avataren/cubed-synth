//! An Adlib Tracker II song steered and read while it plays in a mixer, the way
//! `song_control.rs` does for modules: stop and play, seek, loop, mute and solo, a note
//! preview, the position, levels and scopes, and live edits of cells, instruments and the
//! timing, also as wire bytes. And the instrument audition as a voice of its own.

#![cfg(feature = "songs")]

use std::sync::Arc;

use cubed_mixer::songs::{
    open_a2m, open_controlled_song, open_opl_audition, SongFormat, SongOptions,
};
use cubed_mixer::{
    wire, BusId, BusSettings, Command, Event, Mixer, MixerConfig, PlaySource, SongCell,
    SongControl, SongInstrument, SongLoop, SongReader, SourceEvent, SourceId, VoiceId,
    SONG_SCOPE_FRAMES,
};
use cubed_opl::edit::A2mDoc;
use serde_json::json;

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;
const VOICE: VoiceId = VoiceId(7);
const MUSIC: BusId = BusId(1);

fn fixture() -> Vec<u8> {
    std::fs::read(format!(
        "{}/../opl/tests/fixtures/a2m/NAB622/corridors of time.a2m",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

struct Rig {
    mixer: Mixer,
    reader: Option<Arc<dyn SongReader>>,
}

fn rig_of(source: Box<dyn cubed_mixer::Source>) -> Rig {
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE,
        max_block: BLOCK,
        ..MixerConfig::default()
    });
    mixer.apply(Command::SetBus(MUSIC, BusSettings::default()));
    let reader = source.song_reader();
    mixer.apply(Command::PrepareSource(SourceId(0), source));
    mixer.apply(Command::PlaySource(PlaySource::new(
        VOICE,
        SourceId(0),
        MUSIC,
    )));
    Rig { mixer, reader }
}

/// The fixture opened for control.
fn rig(options: SongOptions, stopped: bool) -> Rig {
    rig_of(open_controlled_song(SongFormat::A2m, &fixture(), RATE, options, stopped).unwrap())
}

/// A song built by operations: an empty OPL3 song with a plain instrument.
fn built(ops: &[serde_json::Value]) -> cubed_opl::a2::A2mSong {
    let mut doc = A2mDoc::new(true);
    for op in ops {
        doc.apply(op).unwrap_or_else(|e| panic!("{op}: {e}"));
    }
    doc.song().clone()
}

impl Rig {
    fn reader(&self) -> &Arc<dyn SongReader> {
        self.reader.as_ref().expect("a song has a reader")
    }

    fn control(&mut self, control: SongControl) {
        self.mixer.apply(Command::SourceEvent {
            voice: VOICE,
            event: SourceEvent::Song(control),
        });
    }

    /// The same, through the wire encoding the worklet decodes.
    fn control_by_wire(&mut self, control: SongControl) {
        let mut bytes = Vec::new();
        assert!(wire::encode(
            &Command::SourceEvent {
                voice: VOICE,
                event: SourceEvent::Song(control),
            },
            &mut bytes,
        ));
        for command in wire::decode(&bytes) {
            self.mixer.apply(command.unwrap());
        }
    }

    /// Renders `seconds`; returns the mono sum of the output.
    fn run(&mut self, seconds: f64) -> Vec<f32> {
        let mut out = Vec::new();
        let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
        for _ in 0..(seconds * RATE as f64) as usize / BLOCK {
            self.mixer.process(&mut l, &mut r);
            out.extend(l.iter().zip(&r).map(|(a, b)| a + b));
        }
        out
    }

    fn events(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        self.mixer.drain_events(|e| events.push(e));
        events
    }
}

fn energy(x: &[f32]) -> f32 {
    x.iter().map(|s| s * s).sum()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, s| m.max(s.abs()))
}

#[test]
fn a_song_opened_stopped_is_silent_until_played_and_keeps_its_voice() {
    let mut rig = rig(SongOptions::default(), true);
    let quiet = rig.run(0.5);
    assert_eq!(peak(&quiet), 0.0);
    let s = rig.reader().status();
    assert!(!s.playing);
    assert_eq!((s.order, s.row), (0, 0));
    assert!(
        s.speed > 0 && s.tempo > 0,
        "the song's timing is known: {s:?}"
    );
    let tracks = rig.reader().channels();
    assert!((9..=20).contains(&tracks), "{tracks} tracks");
    assert!(rig.reader().alive());
    rig.control(SongControl::Play);
    let sound = rig.run(1.0);
    assert!(energy(&sound) > 1.0);
    let s = rig.reader().status();
    assert!(s.playing && s.rows > 3 && s.frames > 0, "{s:?}");
    rig.control(SongControl::Stop);
    rig.run(0.2);
    assert!(!rig.reader().status().playing);
    assert_eq!(peak(&rig.run(0.2)), 0.0);
    assert!(rig.events().is_empty(), "the voice is still there");
    assert!(rig.mixer.is_playing(VOICE));
}

#[test]
fn a_song_opened_the_old_way_plays_at_once() {
    let mut rig = rig(SongOptions::default(), false);
    assert!(energy(&rig.run(0.5)) > 0.1);
    assert!(rig.reader().status().playing);
}

#[test]
fn seek_moves_the_position_while_stopped_and_play_starts_there() {
    let mut rig = rig(SongOptions::default(), true);
    rig.control(SongControl::Seek { order: 1, row: 12 });
    rig.run(0.1);
    let s = rig.reader().status();
    assert_eq!((s.order, s.row), (1, 12), "{s:?}");
    assert!(!s.playing);
    rig.control(SongControl::Play);
    rig.run(0.3);
    let s = rig.reader().status();
    assert_eq!(s.order, 1);
    assert!(s.row >= 12 && s.row < 30, "{s:?}");
}

#[test]
fn a_range_loop_repeats_its_rows_and_the_song_loop_is_the_hosts() {
    let mut rig = rig(SongOptions::default(), true);
    rig.control(SongControl::Seek { order: 0, row: 4 });
    rig.control(SongControl::Loop(SongLoop::Range {
        order: 0,
        row0: 4,
        row1: 9,
    }));
    rig.control(SongControl::Play);
    let mut rows = std::collections::BTreeSet::new();
    let before = rig.reader().status().rows;
    for _ in 0..40 {
        rig.run(0.1);
        let s = rig.reader().status();
        rows.insert((s.order, s.row));
    }
    assert!(
        rows.iter().all(|&(o, r)| o == 0 && (4..=9).contains(&r)),
        "{rows:?}"
    );
    assert!(rows.len() >= 5, "the rows keep coming round: {rows:?}");
    assert!(rig.reader().status().rows > before + 12);
}

#[test]
fn mute_and_solo_work_by_track() {
    let mut rig = rig(SongOptions::default(), true);
    let tracks = rig.reader().channels();
    rig.control(SongControl::Seek { order: 0, row: 0 });
    rig.control(SongControl::Play);
    let all = energy(&rig.run(2.0));
    assert!(all > 1.0);
    // Everything muted: silence (what rings out dies in a moment).
    rig.control_by_wire(SongControl::MuteMask((1u32 << tracks) - 1));
    rig.run(0.1);
    assert!(peak(&rig.run(0.5)) < 1e-4);
    rig.control_by_wire(SongControl::MuteMask(0));
    rig.control_by_wire(SongControl::SoloMask(0b0010));
    rig.control(SongControl::Seek { order: 0, row: 0 });
    let solo = energy(&rig.run(2.0));
    assert!(solo < all, "one track is quieter than all: {solo} vs {all}");
}

#[test]
fn levels_and_scopes_follow_the_sound() {
    let mut rig = rig(SongOptions::default(), true);
    let tracks = rig.reader().channels();
    rig.control(SongControl::Scopes(true));
    rig.control(SongControl::Play);
    rig.run(1.5);
    assert!(rig.reader().scopes_enabled());
    let mut levels = [0.0f32; 40];
    let n = rig.reader().levels(&mut levels);
    assert_eq!(n, tracks + 1, "the tracks and the preview");
    assert!(levels[..tracks].iter().any(|&l| l > 0.001), "{levels:?}");
    assert!(levels[..tracks].iter().all(|&l| (0.0..2.0).contains(&l)));
    assert_eq!(levels[tracks], 0.0, "no preview");
    // At least one track's scope has a wave in it, in the output's scale.
    let mut best = 0.0f32;
    let mut written = 0;
    for track in 0..tracks {
        let mut scope = [0.0f32; SONG_SCOPE_FRAMES];
        let got = rig.reader().scope(track, &mut scope);
        assert_eq!(got, SONG_SCOPE_FRAMES);
        best = best.max(peak(&scope));
        written += rig.reader().scope_position(track);
    }
    assert!(best > 0.001 && best < 1.0, "{best}");
    assert!(written > 0);
    // Off again: the rings stop moving.
    rig.control(SongControl::Scopes(false));
    rig.run(0.1);
    let at = rig.reader().scope_position(0);
    rig.run(0.2);
    assert_eq!(rig.reader().scope_position(0), at);
    assert!(!rig.reader().scopes_enabled());
}

#[test]
fn a_preview_sounds_over_a_stopped_song_with_the_songs_instrument() {
    let mut rig = rig(SongOptions::default(), true);
    let tracks = rig.reader().channels();
    // Nothing from the warm-up is left in the levels.
    let mut levels = [0.0f32; 40];
    rig.reader().levels(&mut levels);
    assert!(levels.iter().all(|&l| l == 0.0), "{levels:?}");
    assert_eq!(peak(&rig.run(0.2)), 0.0);
    rig.control_by_wire(SongControl::PreviewOn {
        instrument: 1,
        note: 60,
        velocity: 1.0,
        duration: 0,
    });
    let held = rig.run(0.05);
    assert!(energy(&held) > 0.1, "the preview is heard");
    let s = rig.reader().status();
    assert!(s.preview && !s.playing, "{s:?}");
    rig.reader().levels(&mut levels);
    assert!(levels[tracks] > 0.001, "the preview's level: {levels:?}");
    assert!(
        levels[..tracks].iter().all(|&l| l == 0.0),
        "the song is stopped"
    );
    rig.control(SongControl::PreviewOff);
    rig.run(4.0);
    assert!(!rig.reader().status().preview, "the release played out");
    assert!(peak(&rig.run(0.3)) < 1e-4);
    // A timed note lets go by itself.
    rig.control(SongControl::PreviewOn {
        instrument: 1,
        note: 64,
        velocity: 0.5,
        duration: 4_800,
    });
    assert!(energy(&rig.run(0.2)) > 0.01);
    rig.run(4.0);
    assert!(!rig.reader().status().preview);
    // No instrument is nothing; an empty slot is next to nothing.
    rig.control(SongControl::PreviewOn {
        instrument: 0,
        note: 60,
        velocity: 1.0,
        duration: 0,
    });
    assert_eq!(peak(&rig.run(0.3)), 0.0);
    rig.control(SongControl::PreviewOn {
        instrument: 255,
        note: 60,
        velocity: 1.0,
        duration: 0,
    });
    assert!(peak(&rig.run(0.3)) < 1e-3);
}

/// An empty song with instrument 1 a plain, sustained sine, and the rows it plays a second.
fn quiet_song() -> (cubed_opl::a2::A2mSong, f64) {
    let mut doc = A2mDoc::new(true);
    doc.apply(&json!({"op": "set_instrument", "instrument": 1, "name": "Sine"}))
        .unwrap();
    let info = doc.info();
    let rate = info["refresh"].as_f64().unwrap() / info["speed"].as_f64().unwrap();
    (doc.song().clone(), rate)
}

/// The OPL's quietest level is not silence (47 dB down): what a silenced instrument still
/// makes is a small part of what it made.
const SILENT: f32 = 0.01;

/// The instrument 1 of `quiet_song`, with the carrier at its quietest.
fn silenced() -> SongInstrument {
    SongInstrument {
        number: 1,
        fm: [
            0x21, 0x21, 0x3f, 0x3f, 0xf2, 0xf4, 0x24, 0x27, 0x00, 0x00, 0x00,
        ],
        panning: 0,
        finetune: 0,
        voice_type: 0,
    }
}

fn cell(row: u16, note: u8) -> SongControl {
    SongControl::SetCell(SongCell {
        pattern: 0,
        row,
        channel: 0,
        note,
        period: 0,
        instrument: 1,
        volume: 0,
        effect: 0,
        param: 0,
    })
}

#[test]
fn a_live_cell_edit_is_heard_when_the_song_gets_to_it() {
    let (song, rate) = quiet_song();
    let mut rig = rig_of(open_a2m(song, RATE, SongOptions::default(), true).unwrap());
    rig.control(SongControl::Play);
    assert_eq!(peak(&rig.run(1.0)), 0.0, "the song is empty");
    // A note three seconds on from the start; the song is at second 1.
    let at = (3.0 * rate) as u16;
    rig.control_by_wire(cell(at, 49));
    assert_eq!(peak(&rig.run(1.5)), 0.0, "not there yet");
    let heard = rig.run(1.5);
    assert!(energy(&heard) > 0.1, "the note sounds once its row comes");
    assert!(rig.reader().status().rows > at as u32);
}

#[test]
fn a_live_instrument_edit_changes_the_notes_that_follow() {
    let (song, rate) = quiet_song();
    let mut rig = rig_of(open_a2m(song, RATE, SongOptions::default(), true).unwrap());
    // A note at 0.5 s, let go at 2 s, and again at 4 s (and let go at 5.5 s).
    let row = |seconds: f64| (seconds * rate) as u16;
    for (seconds, note) in [(0.5, 49), (2.0, 255), (4.0, 49), (5.5, 255)] {
        rig.control(cell(row(seconds), note));
    }
    rig.control(SongControl::Play);
    rig.run(0.6);
    let first = energy(&rig.run(1.2));
    assert!(first > 0.1, "{first}");
    // In the silence between, the instrument is changed.
    rig.run(1.5);
    rig.control_by_wire(SongControl::SetInstrument(silenced()));
    rig.run(0.5);
    let second = energy(&rig.run(1.2));
    assert!(
        second < first * SILENT,
        "the second note takes the new instrument: {second} vs {first}"
    );
}

#[test]
fn a_preview_takes_an_instrument_edit_under_the_held_note() {
    let (song, _) = quiet_song();
    let mut rig = rig_of(open_a2m(song, RATE, SongOptions::default(), true).unwrap());
    rig.control(SongControl::PreviewOn {
        instrument: 1,
        note: 60,
        velocity: 1.0,
        duration: 0,
    });
    rig.run(0.2);
    let before = energy(&rig.run(0.3));
    assert!(before > 0.1);
    rig.control(SongControl::SetInstrument(silenced()));
    rig.run(0.1);
    let after = energy(&rig.run(0.3));
    assert!(
        after < before * SILENT,
        "the held note took the edit: {after} vs {before}"
    );
    // An edit of another instrument leaves the preview alone.
    let mut other = silenced();
    other.number = 2;
    rig.control(SongControl::SetInstrument(other));
}

#[test]
fn timing_is_set_live() {
    let (song, _) = quiet_song();
    let mut rig = rig_of(open_a2m(song, RATE, SongOptions::default(), true).unwrap());
    rig.control(SongControl::Play);
    rig.run(0.2);
    let a = rig.reader().status().rows;
    rig.run(1.0);
    let slow = rig.reader().status().rows - a;
    assert!(slow >= 6, "{slow}");
    rig.control_by_wire(SongControl::Timing {
        tempo: 100,
        speed: 3,
    });
    rig.run(0.1);
    let s = rig.reader().status();
    assert_eq!((s.tempo, s.speed), (100, 3));
    let b = s.rows;
    rig.run(1.0);
    let fast = rig.reader().status().rows - b;
    // Twice the timer, half the ticks a row: four times the rows.
    let ratio = fast as f64 / slow as f64;
    assert!((3.5..4.5).contains(&ratio), "{slow} then {fast}");
}

#[test]
fn a_controlled_song_stops_at_its_end_instead_of_ending() {
    // Four rows at 1 tick each, at tempo 255: a pass is 16 ms.
    let song = built(&[
        json!({"op": "set_pattern_len", "rows": 4}),
        json!({"op": "set_speed", "speed": 1}),
        json!({"op": "set_tempo", "tempo": 255}),
        json!({"op": "set_cell", "pattern": 0, "row": 0, "channel": 0, "note": 60, "instrument": 1}),
    ]);
    let mut rig = rig_of(
        open_a2m(
            song.clone(),
            RATE,
            SongOptions {
                looped: false,
                ..Default::default()
            },
            true,
        )
        .unwrap(),
    );
    rig.control(SongControl::Play);
    rig.run(0.5);
    let s = rig.reader().status();
    assert!(!s.playing, "it stopped at the end: {s:?}");
    assert_eq!((s.order, s.row), (0, 0), "at the top");
    assert!(rig.events().is_empty());
    assert!(rig.mixer.is_playing(VOICE), "and kept its voice");
    // Looped, it goes round.
    let mut rig = rig_of(
        open_a2m(
            song.clone(),
            RATE,
            SongOptions {
                looped: false,
                ..Default::default()
            },
            true,
        )
        .unwrap(),
    );
    rig.control(SongControl::Looped(true));
    rig.control(SongControl::Play);
    rig.run(0.5);
    assert!(rig.reader().status().playing);
    assert!(rig.reader().status().rows > 20);
    // Opened the old way (not controlled), it ends where the song does.
    let mut rig = rig_of(
        open_a2m(
            song,
            RATE,
            SongOptions {
                looped: false,
                ..Default::default()
            },
            false,
        )
        .unwrap(),
    );
    rig.run(0.5);
    assert!(rig.events().contains(&Event::Ended(VOICE)));
}

#[test]
fn a_song_that_is_not_one_is_refused_in_every_controlled_format() {
    // Every format is under live control now (the trait's README says how): bytes that are no song of the
    // format are an error, not a silent voice.
    for format in [SongFormat::A2m, SongFormat::Ahx, SongFormat::Sid] {
        assert!(
            open_controlled_song(format, b"not a song", RATE, SongOptions::default(), true)
                .is_err(),
            "{format:?}"
        );
    }
}

#[test]
fn the_audition_voice_plays_by_note_and_takes_patch_edits() {
    let plain = SongInstrument {
        number: 1,
        fm: [
            0x21, 0x21, 0x3f, 0x00, 0xf2, 0xf4, 0x24, 0x27, 0x00, 0x00, 0x00,
        ],
        ..Default::default()
    };
    let mut rig = rig_of(open_opl_audition(&plain, RATE, 4));
    assert!(rig.reader.is_none(), "an audition is not a song");
    assert_eq!(peak(&rig.run(0.2)), 0.0, "silent until a note");
    let note = |event: SourceEvent| Command::SourceEvent {
        voice: VOICE,
        event,
    };
    rig.mixer.apply(note(SourceEvent::NoteOn {
        note: 57,
        velocity: 1.0,
        duration: 0,
    }));
    rig.mixer.apply(note(SourceEvent::NoteOn {
        note: 64,
        velocity: 0.7,
        duration: 0,
    }));
    let chord = rig.run(0.5);
    assert!(energy(&chord) > 0.1);
    // The patch edited under the held notes: the carrier's level to the quietest.
    rig.run(0.2);
    let held = energy(&rig.run(0.3));
    let mut silent = plain;
    silent.fm[3] = 0x3f;
    rig.control_by_wire(SongControl::SetInstrument(silent));
    rig.run(0.1);
    let edited = energy(&rig.run(0.3));
    assert!(edited < held * SILENT, "{edited} vs {held}");
    // And back: the held notes sound again.
    rig.control_by_wire(SongControl::SetInstrument(plain));
    rig.run(0.1);
    assert!(energy(&rig.run(0.3)) > held * 0.5);
    rig.mixer.apply(note(SourceEvent::NoteOff { note: 57 }));
    rig.mixer.apply(note(SourceEvent::AllNotesOff));
    rig.run(3.0);
    assert!(peak(&rig.run(0.3)) < 1e-4, "the releases played out");
    assert!(rig.events().is_empty(), "the voice goes on");
    // A timed note ends by itself.
    rig.mixer.apply(note(SourceEvent::NoteOn {
        note: 60,
        velocity: 1.0,
        duration: 2_400,
    }));
    assert!(energy(&rig.run(0.3)) > 0.01);
    rig.run(3.0);
    assert!(peak(&rig.run(0.3)) < 1e-4);
}
