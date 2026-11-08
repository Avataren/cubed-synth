//! An AHX / HVL song steered and read while it plays in a mixer, the way `a2m_song.rs` does for
//! Adlib songs: stop and play, seek, loop, mute and solo, a preview, the position and levels, and
//! live edits (a step, a position entry), also as wire bytes.

#![cfg(feature = "songs")]

use std::sync::Arc;

use cubed_ahx::edit::AhxDoc;
use cubed_ahx::format::SongFormat as AhxFormat;
use cubed_mixer::songs::{open_ahx, open_controlled_song, SongFormat, SongOptions};
use cubed_mixer::{
    wire, BusId, BusSettings, Command, Event, Mixer, MixerConfig, PlaySource, SongCell,
    SongControl, SongLoop, SongReader, SourceEvent, SourceId, VoiceId,
};
use serde_json::json;

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;
const VOICE: VoiceId = VoiceId(7);
const MUSIC: BusId = BusId(1);

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../ahx/tests/fixtures/songs/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

struct Rig {
    mixer: Mixer,
    reader: Arc<dyn SongReader>,
}

fn rig_of(source: Box<dyn cubed_mixer::Source>) -> Rig {
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE,
        max_block: BLOCK,
        ..MixerConfig::default()
    });
    mixer.apply(Command::SetBus(MUSIC, BusSettings::default()));
    let reader = source.song_reader().expect("a song has a reader");
    mixer.apply(Command::PrepareSource(SourceId(0), source));
    mixer.apply(Command::PlaySource(PlaySource::new(
        VOICE,
        SourceId(0),
        MUSIC,
    )));
    Rig { mixer, reader }
}

fn rig(name: &str, stopped: bool) -> Rig {
    rig_of(
        open_controlled_song(
            SongFormat::Ahx,
            &fixture(name),
            RATE,
            SongOptions::default(),
            stopped,
        )
        .unwrap(),
    )
}

impl Rig {
    fn control(&mut self, control: SongControl) {
        self.mixer.apply(Command::SourceEvent {
            voice: VOICE,
            event: SourceEvent::Song(control),
        });
    }

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

const SONG: &str = "a_new_beginning.ahx";

#[test]
fn a_song_opened_stopped_is_silent_until_played_and_keeps_its_voice() {
    let mut rig = rig(SONG, true);
    assert_eq!(peak(&rig.run(0.5)), 0.0);
    let s = rig.reader.status();
    assert!(!s.playing);
    assert_eq!((s.order, s.row), (0, 0));
    assert!(s.speed > 0 && s.tempo >= 50, "{s:?}");
    assert_eq!(rig.reader.channels(), 4);
    assert!(rig.reader.alive());
    let mut levels = [0.0f32; 40];
    rig.reader.levels(&mut levels);
    assert!(levels.iter().all(|&l| l == 0.0), "{levels:?}");
    rig.control(SongControl::Play);
    assert!(energy(&rig.run(1.0)) > 1.0);
    let s = rig.reader.status();
    assert!(s.playing && s.rows > 3 && s.frames > 0, "{s:?}");
    rig.reader.levels(&mut levels);
    assert!(levels[..4].iter().any(|&l| l > 0.001), "{levels:?}");
    rig.control(SongControl::Stop);
    rig.run(0.2);
    assert!(!rig.reader.status().playing);
    assert_eq!(peak(&rig.run(0.2)), 0.0);
    assert!(rig.events().is_empty());
    assert!(rig.mixer.is_playing(VOICE));
}

#[test]
fn an_hvl_song_has_its_own_channel_count() {
    let rig = rig("a_little_cyberfunk.hvl", true);
    let channels = rig.reader.channels();
    assert!((4..=16).contains(&channels), "{channels}");
}

#[test]
fn seek_and_a_rows_loop_move_the_position() {
    let mut rig = rig(SONG, true);
    rig.control(SongControl::Seek { order: 1, row: 12 });
    rig.run(0.1);
    let s = rig.reader.status();
    assert_eq!((s.order, s.row), (1, 12), "{s:?}");
    assert!(!s.playing);
    rig.control(SongControl::Loop(SongLoop::Range {
        order: 1,
        row0: 12,
        row1: 17,
    }));
    rig.control(SongControl::Play);
    let mut rows = std::collections::BTreeSet::new();
    for _ in 0..40 {
        rig.run(0.1);
        let s = rig.reader.status();
        rows.insert((s.order, s.row));
    }
    assert!(
        rows.iter().all(|&(o, r)| o == 1 && (12..=17).contains(&r)),
        "{rows:?}"
    );
    assert!(rows.len() >= 4, "{rows:?}");
}

#[test]
fn mute_and_solo_work_by_channel() {
    let mut rig = rig(SONG, true);
    rig.control(SongControl::Play);
    let all = energy(&rig.run(2.0));
    assert!(all > 1.0);
    rig.control_by_wire(SongControl::MuteMask(0b1111));
    rig.run(0.2);
    assert!(peak(&rig.run(0.5)) < 1e-4);
    rig.control_by_wire(SongControl::MuteMask(0));
    rig.control_by_wire(SongControl::SoloMask(0b0001));
    rig.control(SongControl::Seek { order: 0, row: 0 });
    let solo = energy(&rig.run(2.0));
    assert!(solo < all, "{solo} vs {all}");
}

#[test]
fn a_preview_sounds_over_a_stopped_song() {
    let mut rig = rig(SONG, true);
    let channels = rig.reader.channels();
    assert_eq!(peak(&rig.run(0.2)), 0.0);
    rig.control_by_wire(SongControl::PreviewOn {
        instrument: 1,
        note: 60,
        velocity: 1.0,
        duration: 0,
    });
    assert!(energy(&rig.run(0.3)) > 0.1, "the preview is heard");
    let s = rig.reader.status();
    assert!(s.preview && !s.playing, "{s:?}");
    let mut levels = [0.0f32; 40];
    rig.reader.levels(&mut levels);
    assert!(levels[channels] > 0.0, "{levels:?}");
    assert!(levels[..channels].iter().all(|&l| l == 0.0));
    rig.control(SongControl::PreviewOff);
    rig.run(3.0);
    assert!(!rig.reader.status().preview);
    // A timed note lets go by itself.
    rig.control(SongControl::PreviewOn {
        instrument: 1,
        note: 64,
        velocity: 0.5,
        duration: 4_800,
    });
    assert!(energy(&rig.run(0.2)) > 0.01);
    rig.run(3.0);
    assert!(!rig.reader.status().preview);
}

/// A new AHX song with a plain instrument and a track of its own at position 0, channel 0
/// (its number is returned): nothing plays until a step is put in.
fn quiet_song() -> (cubed_ahx::format::Song, u8) {
    let mut doc = AhxDoc::new(AhxFormat::Ahx);
    for op in [
        json!({"op": "add_instrument"}),
        json!({"op": "set_step", "position": 0, "channel": 0, "row": 60, "cell": [49, 1, 0, 0, 0, 0]}),
    ] {
        doc.apply(&op).unwrap_or_else(|e| panic!("{op}: {e}"));
    }
    let track = doc.song().positions[0].track[0];
    assert_ne!(track, 0);
    // The note is cleared again: the track stays.
    doc.apply(&json!({"op": "clear_range", "track": track, "row0": 60, "row1": 60, "channel0": 0, "channel1": 0}))
        .unwrap();
    (doc.song().clone(), track)
}

fn step(track: u8, row: u16, note: u8, instrument: u8) -> SongControl {
    SongControl::SetCell(SongCell {
        pattern: track as u16,
        row,
        channel: 0,
        note,
        period: 0,
        instrument,
        volume: 0,
        effect: 0,
        param: 0,
    })
}

#[test]
fn a_live_step_is_heard_when_the_song_gets_to_it() {
    let (song, track) = quiet_song();
    let mut rig = rig_of(open_ahx(song, RATE, SongOptions::default(), true).unwrap());
    rig.control(SongControl::Play);
    assert_eq!(peak(&rig.run(0.5)), 0.0, "the song is empty");
    let at = rig.reader.status().row;
    // A note a few rows on (a row is 6 ticks of 20 ms at the default speed).
    rig.control_by_wire(step(track, at + 8, 49, 1));
    let heard = rig.run(2.0);
    assert!(energy(&heard) > 0.1, "the note sounds once its row comes");
}

#[test]
fn a_position_entry_and_the_restart_ride_the_wire() {
    let (song, _) = quiet_song();
    let mut rig = rig_of(open_ahx(song, RATE, SongOptions::default(), true).unwrap());
    rig.control_by_wire(SongControl::SetOrder {
        order: 0,
        channel: 1,
        pattern: 1,
        transpose: -12,
    });
    rig.control_by_wire(SongControl::SetRestart(0));
    rig.control(SongControl::Play);
    rig.run(0.3);
    assert!(rig.reader.status().playing);
}

#[test]
fn a_controlled_song_stops_at_its_end_and_an_uncontrolled_one_ends() {
    // One position of 2 rows at speed 1 comes round in 40 ms.
    let mut doc = AhxDoc::new(AhxFormat::Ahx);
    doc.apply(&json!({"op": "set_track_length", "length": 2, "force": true}))
        .or_else(|_| doc.apply(&json!({"op": "set_track_length", "rows": 2, "force": true})))
        .unwrap();
    let song = doc.song().clone();
    let options = SongOptions {
        looped: false,
        ..Default::default()
    };
    let mut rig = rig_of(open_ahx(song.clone(), RATE, options, true).unwrap());
    rig.control(SongControl::Play);
    rig.run(1.0);
    let s = rig.reader.status();
    assert!(!s.playing, "stopped at the end: {s:?}");
    assert_eq!((s.order, s.row), (0, 0));
    assert!(rig.mixer.is_playing(VOICE));
    // Looped, it goes round.
    let mut rig = rig_of(open_ahx(song.clone(), RATE, options, true).unwrap());
    rig.control(SongControl::Looped(true));
    rig.control(SongControl::Play);
    rig.run(1.0);
    assert!(rig.reader.status().playing);
    // Opened the old way it ends with the song.
    let mut rig = rig_of(open_ahx(song, RATE, options, false).unwrap());
    rig.run(1.0);
    assert!(rig.events().contains(&Event::Ended(VOICE)));
}
