//! A module song steered and read while it plays in a mixer: stop and play,
//! seek, loop, mute and solo, a note preview, the position and levels and the
//! channel scopes, and the same controls arriving as wire bytes.

#![cfg(feature = "songs")]

use std::sync::Arc;

use cubed_mixer::songs::{open_module_song, open_song, SongFormat, SongOptions};
use cubed_mixer::{
    wire, BusId, BusSettings, Command, Event, Mixer, MixerConfig, PlaySource, SongControl,
    SongLoop, SongReader, SourceEvent, SourceId, VoiceId, SONG_LEVEL_SLOTS, SONG_SCOPE_FRAMES,
};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;
const VOICE: VoiceId = VoiceId(7);
const MUSIC: BusId = BusId(1);

fn module(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../tracker/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

struct Rig {
    mixer: Mixer,
    reader: Arc<dyn SongReader>,
}

/// A song opened for control on a bus and started as a voice.
fn rig(name: &str, format: SongFormat, options: SongOptions, stopped: bool) -> Rig {
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE,
        max_block: BLOCK,
        ..MixerConfig::default()
    });
    mixer.apply(Command::SetBus(MUSIC, BusSettings::default()));
    let source = open_module_song(format, &module(name), RATE, options, stopped).unwrap();
    let reader = source.song_reader().expect("a module song has a reader");
    mixer.apply(Command::PrepareSource(SourceId(0), source));
    mixer.apply(Command::PlaySource(PlaySource::new(
        VOICE,
        SourceId(0),
        MUSIC,
    )));
    Rig { mixer, reader }
}

impl Rig {
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
    let mut rig = rig("demo.mod", SongFormat::Mod, SongOptions::default(), true);
    let quiet = rig.run(0.5);
    assert_eq!(peak(&quiet), 0.0);
    let s = rig.reader.status();
    assert!(!s.playing);
    assert_eq!((s.order, s.row), (0, 0));
    assert_eq!(rig.reader.channels(), 4);
    assert!(rig.reader.alive());
    rig.control(SongControl::Play);
    let sound = rig.run(1.0);
    assert!(energy(&sound) > 1.0);
    let s = rig.reader.status();
    assert!(s.playing && s.rows > 5 && s.frames > 0);
    rig.control(SongControl::Stop);
    rig.run(0.2);
    assert!(!rig.reader.status().playing);
    assert_eq!(peak(&rig.run(0.2)), 0.0);
    assert!(rig.events().is_empty(), "the voice is still there");
    assert!(rig.mixer.is_playing(VOICE));
}

#[test]
fn a_song_opened_the_old_way_plays_at_once_and_ends() {
    let bytes = module("demo.mod");
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE,
        max_block: BLOCK,
        ..MixerConfig::default()
    });
    mixer.apply(Command::SetBus(MUSIC, BusSettings::default()));
    let source = open_song(
        SongFormat::Mod,
        &bytes,
        RATE,
        SongOptions {
            looped: false,
            ..Default::default()
        },
    )
    .unwrap();
    let reader = source.song_reader().unwrap();
    mixer.apply(Command::PrepareSource(SourceId(0), source));
    mixer.apply(Command::PlaySource(PlaySource::new(
        VOICE,
        SourceId(0),
        MUSIC,
    )));
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    mixer.process(&mut l, &mut r);
    assert!(reader.status().playing);
    assert!(peak(&l) > 0.0);
    // A song that does not control its transport still ends its voice.
    let mut ended = false;
    for _ in 0..(25.0 * RATE as f64) as usize / BLOCK {
        mixer.process(&mut l, &mut r);
        mixer.drain_events(|e| ended |= e == Event::Ended(VOICE));
    }
    assert!(ended);
}

#[test]
fn a_controlled_song_stops_at_its_end_instead_of_ending() {
    let mut rig = rig(
        "demo.mod",
        SongFormat::Mod,
        SongOptions {
            looped: false,
            ..Default::default()
        },
        true,
    );
    // Order 3 is the last; its last rows are a few seconds from the end.
    rig.control(SongControl::Seek { order: 3, row: 56 });
    rig.control(SongControl::Play);
    rig.run(3.0);
    let s = rig.reader.status();
    assert!(!s.playing, "stopped at the end");
    assert_eq!((s.order, s.row), (0, 0), "back at the top");
    assert!(rig.events().is_empty());
    assert!(rig.mixer.is_playing(VOICE));
    // And plays again from there.
    rig.control(SongControl::Play);
    assert!(energy(&rig.run(0.5)) > 0.1);
    rig.control(SongControl::Looped(true));
    rig.control(SongControl::Seek { order: 3, row: 56 });
    rig.run(4.0);
    assert!(rig.reader.status().playing, "looped: it goes on");
}

#[test]
fn seek_loop_mute_and_solo_steer_the_song() {
    let mut rig = rig("demo.mod", SongFormat::Mod, SongOptions::default(), true);
    // Seeking works while stopped, and the position reads at once.
    rig.control(SongControl::Seek { order: 2, row: 8 });
    let s = rig.reader.status();
    assert_eq!((s.order, s.pattern, s.row), (2, 2, 8));
    assert!(!s.playing);
    rig.run(0.05);
    assert_eq!(rig.reader.status().row, 8, "and stays put while stopped");

    // A range loop.
    rig.control(SongControl::Loop(SongLoop::Range {
        order: 2,
        row0: 8,
        row1: 11,
    }));
    rig.control(SongControl::Play);
    let mut rows = Vec::new();
    for _ in 0..400 {
        rig.run(0.01);
        let s = rig.reader.status();
        if rows.last() != Some(&(s.order, s.row)) {
            rows.push((s.order, s.row));
        }
    }
    assert!(rows.len() > 8);
    assert!(
        rows.iter().all(|&(o, r)| o == 2 && (8..=11).contains(&r)),
        "{rows:?}"
    );
    rig.control(SongControl::Loop(SongLoop::None));

    // Mute and solo, by command and by wire.
    rig.control(SongControl::Seek { order: 0, row: 0 });
    let all = energy(&rig.run(3.0));
    rig.control(SongControl::Seek { order: 0, row: 0 });
    rig.control_by_wire(SongControl::Mute {
        channel: 0,
        on: true,
    });
    let muted = energy(&rig.run(3.0));
    rig.control_by_wire(SongControl::Mute {
        channel: 0,
        on: false,
    });
    rig.control_by_wire(SongControl::SoloMask(0b0100));
    rig.control(SongControl::Seek { order: 0, row: 0 });
    let solo = energy(&rig.run(3.0));
    assert!(muted > 0.0 && muted < all);
    assert!(solo > 0.0 && solo < all * 0.8);
}

#[test]
fn a_note_previews_over_a_stopped_song_by_wire() {
    let mut rig = rig("demo.xm", SongFormat::Xm, SongOptions::default(), true);
    rig.control_by_wire(SongControl::PreviewOn {
        instrument: 2,
        note: 64,
        velocity: 1.0,
        duration: 12_000,
    });
    let sound = rig.run(0.6);
    assert!(energy(&sound) > 0.01);
    assert!(!rig.reader.status().playing);
    // It was 0.25 s long and a short release; then nothing.
    assert_eq!(peak(&sound[(0.4 * RATE) as usize..]), 0.0);
    assert!(!rig.reader.status().preview);
    // An instrument the module does not have plays nothing.
    rig.control_by_wire(SongControl::PreviewOn {
        instrument: 99,
        note: 60,
        velocity: 1.0,
        duration: 0,
    });
    assert_eq!(peak(&rig.run(0.2)), 0.0);
    // Held, then let go.
    rig.control_by_wire(SongControl::PreviewOn {
        instrument: 1,
        note: 60,
        velocity: 0.8,
        duration: 0,
    });
    assert!(peak(&rig.run(0.5)) > 0.01);
    assert!(rig.reader.status().preview);
    rig.control_by_wire(SongControl::PreviewOff);
    assert_eq!(peak(&rig.run(3.0)[(2.0 * RATE) as usize..]), 0.0);
}

#[test]
fn the_reader_gives_position_levels_and_scopes() {
    let mut rig = rig("demo.mod", SongFormat::Mod, SongOptions::default(), true);
    rig.control(SongControl::Scopes(true));
    rig.control(SongControl::Play);
    let mut ticks = std::collections::BTreeSet::new();
    let mut best = [0.0f32; SONG_LEVEL_SLOTS];
    let mut levels = [0.0f32; SONG_LEVEL_SLOTS];
    let mut last_version = rig.reader.status().version;
    let mut changes = 0;
    for _ in 0..(2.0 * RATE) as usize / BLOCK {
        rig.run(BLOCK as f64 / RATE as f64);
        let s = rig.reader.status();
        ticks.insert(s.tick);
        changes += (s.version != last_version) as usize;
        last_version = s.version;
        let n = rig.reader.levels(&mut levels);
        assert_eq!(n, 5);
        for i in 0..n {
            best[i] = best[i].max(levels[i]);
        }
    }
    assert!(ticks.len() >= 4, "{ticks:?}");
    assert!(changes > 50, "the version moves while it plays: {changes}");
    assert!(best[..4].iter().all(|&v| v > 0.005), "{best:?}");
    assert_eq!(best[4], 0.0);
    assert!(rig.reader.scopes_enabled());
    let mut ring = [0.0; SONG_SCOPE_FRAMES];
    assert_eq!(rig.reader.scope(0, &mut ring), SONG_SCOPE_FRAMES);
    assert!(peak(&ring) > 0.0);
    let at = rig.reader.scope_position(0);
    rig.run(0.05);
    assert!(rig.reader.scope_position(0) > at);
    // Stopped, the version settles once the levels have fallen away.
    rig.control(SongControl::Stop);
    rig.run(5.0);
    let v = rig.reader.status().version;
    rig.run(1.0);
    assert_eq!(
        rig.reader.status().version,
        v,
        "nothing moves, so nothing changes"
    );
    rig.control(SongControl::Scopes(false));
    rig.run(0.1);
    assert!(!rig.reader.scopes_enabled());
}

#[test]
fn the_reader_sees_the_song_go_when_its_voice_is_freed() {
    let mut rig = rig("demo.s3m", SongFormat::S3m, SongOptions::default(), true);
    let reader = rig.reader.clone();
    assert!(reader.alive());
    rig.mixer.apply(Command::Stop(VOICE, 0));
    rig.run(0.1);
    let mut freed = 0;
    rig.mixer.take_returned(|_| freed += 1);
    assert!(freed > 0);
    assert!(!reader.alive(), "the returned source was dropped");
}

#[test]
fn other_formats_have_no_reader_and_refuse_to_open_as_modules() {
    let bytes = std::fs::read(format!(
        "{}/../ahx/tests/fixtures/songs/a_new_beginning.ahx",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let source = open_song(SongFormat::Ahx, &bytes, RATE, SongOptions::default()).unwrap();
    assert!(source.song_reader().is_none());
    assert!(open_module_song(SongFormat::Ahx, &bytes, RATE, SongOptions::default(), true).is_err());
}

#[test]
fn the_scope_frame_count_is_the_trackers() {
    assert_eq!(SONG_SCOPE_FRAMES, cubed_tracker::SCOPE_FRAMES);
    assert_eq!(SONG_LEVEL_SLOTS, cubed_tracker::LEVEL_SLOTS);
}

#[test]
fn live_cell_edits_arrive_by_wire_and_are_played() {
    use cubed_mixer::SongCell;
    let mut rig = rig("demo.mod", SongFormat::Mod, SongOptions::default(), true);
    // Rows 0 to 3 of pattern 0 emptied, one cell at a time (21 bytes each).
    for row in 0..4 {
        for channel in 0..4 {
            rig.control_by_wire(SongControl::SetCell(SongCell {
                pattern: 0,
                row,
                channel,
                ..Default::default()
            }));
        }
    }
    rig.control(SongControl::Seek { order: 0, row: 0 });
    rig.control(SongControl::Play);
    assert_eq!(peak(&rig.run(0.3)), 0.0);
    // C-2 on sample 1 in row 1 of channel 1.
    rig.control_by_wire(SongControl::SetCell(SongCell {
        pattern: 0,
        row: 1,
        channel: 1,
        note: 36,
        period: 428,
        instrument: 1,
        ..Default::default()
    }));
    rig.control(SongControl::Seek { order: 0, row: 0 });
    assert!(energy(&rig.run(0.3)) > 0.01);
    // A cell that is not there changes nothing and breaks nothing.
    rig.control_by_wire(SongControl::SetCell(SongCell {
        pattern: 77,
        ..Default::default()
    }));
    assert!(rig.mixer.is_playing(VOICE));
}

#[test]
fn a_rebuilt_module_takes_over_at_the_place_of_the_old_one() {
    use cubed_mixer::swap_commands;
    let mut rig = rig("demo.mod", SongFormat::Mod, SongOptions::default(), true);
    rig.control(SongControl::Seek { order: 1, row: 4 });
    rig.control(SongControl::Loop(SongLoop::Pattern { order: 1 }));
    rig.control(SongControl::Play);
    rig.run(0.7);
    let at = rig.reader.status();
    assert!(at.playing);
    // The edited module (here the same file, as a host would hand over a rebuilt
    // one) is opened stopped in another slot; one batch crossfades it in and puts
    // it where the old one is.
    let new = open_module_song(
        SongFormat::Mod,
        &module("demo.mod"),
        RATE,
        SongOptions::default(),
        true,
    )
    .unwrap();
    let new_reader = new.song_reader().unwrap();
    rig.mixer.apply(Command::PrepareSource(SourceId(1), new));
    let new_voice = VoiceId(8);
    let mut batch = Vec::from(swap_commands(
        VOICE,
        PlaySource::new(new_voice, SourceId(1), MUSIC),
        2_400,
    ));
    for control in [
        SongControl::Seek {
            order: at.order,
            row: at.row,
        },
        SongControl::Loop(SongLoop::Pattern { order: at.order }),
        SongControl::Play,
    ] {
        batch.push(Command::SourceEvent {
            voice: new_voice,
            event: SourceEvent::Song(control),
        });
    }
    for command in batch {
        rig.mixer.apply(command);
    }
    let crossfade = rig.run(0.6);
    // The old song fades out under the new one, which starts the row it was at
    // (from a clean state: notes sustained from earlier rows are not carried over,
    // as a seek never does) and plays on.
    assert!(energy(&crossfade[..2_400]) > 0.0, "the old voice fades out");
    assert!(energy(&crossfade[2_400..]) > 0.0, "the new one plays");
    let s = new_reader.status();
    assert!(s.playing);
    assert_eq!(s.order, at.order, "the same pattern, looped");
    assert!(!rig.mixer.is_playing(VOICE), "the old voice has ended");
    assert!(rig.mixer.is_playing(new_voice));
    assert!(rig.events().contains(&Event::Ended(VOICE)));
}
