//! A SID song steered and read while it plays in a mixer, the way `a2m_song.rs` and `ahx_song.rs` do
//! for the other formats: stop (the player's preview mode) and play, seek, loop, mute, a preview, the
//! position, levels and scopes, and live edits of a row and an instrument, also as wire bytes.

#![cfg(feature = "songs")]

use std::sync::Arc;

use cubed_mixer::songs::{open_controlled_song, open_sid, SongFormat, SongOptions};
use cubed_mixer::{
    wire, BusId, BusSettings, Command, Event, Mixer, MixerConfig, PlaySource, SongCell,
    SongControl, SongInstrument, SongLoop, SongReader, SourceEvent, SourceId, VoiceId,
    SONG_SCOPE_FRAMES,
};
use cubed_sid::edit::SidDoc;
use serde_json::json;

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;
const VOICE: VoiceId = VoiceId(7);
const MUSIC: BusId = BusId(1);

fn fixture() -> Vec<u8> {
    std::fs::read(format!(
        "{}/../sid/tests/fixtures/sid/s3-chain.asid",
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

fn rig(stopped: bool) -> Rig {
    rig_of(
        open_controlled_song(
            SongFormat::Sid,
            &fixture(),
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

/// The sound's movement: the chip's output carries a constant offset, silence included.
fn centred(x: &[f32]) -> Vec<f32> {
    let mean = x.iter().sum::<f32>() / x.len().max(1) as f32;
    x.iter().map(|s| s - mean).collect()
}

fn energy(x: &[f32]) -> f32 {
    centred(x).iter().map(|s| s * s).sum()
}

fn peak(x: &[f32]) -> f32 {
    centred(x).iter().fold(0.0, |m, s| m.max(s.abs()))
}

#[test]
fn a_song_opened_stopped_is_silent_until_played_and_keeps_its_voice() {
    let mut rig = rig(true);
    // The offset the chip starts with settles (the voice fades in).
    rig.run(0.3);
    assert!(peak(&rig.run(0.5)) < 1e-4);
    let s = rig.reader.status();
    assert!(!s.playing, "{s:?}");
    assert_eq!(rig.reader.channels(), 3);
    assert!(rig.reader.alive());
    rig.control(SongControl::Play);
    assert!(energy(&rig.run(1.5)) > 0.01);
    let s = rig.reader.status();
    assert!(
        s.playing && s.rows > 2 && s.frames > 0 && s.speed > 0,
        "{s:?}"
    );
    let mut levels = [0.0f32; 40];
    assert_eq!(rig.reader.levels(&mut levels), 4);
    assert!(levels[..3].iter().any(|&l| l > 0.0), "{levels:?}");
    rig.control(SongControl::Stop);
    rig.run(1.0);
    assert!(!rig.reader.status().playing);
    assert!(peak(&rig.run(0.3)) < 1e-4, "stopped is silent");
    assert!(rig.events().is_empty());
    assert!(rig.mixer.is_playing(VOICE));
    // Played at once when asked.
    let mut again = rig_of(
        open_controlled_song(
            SongFormat::Sid,
            &fixture(),
            RATE,
            SongOptions::default(),
            false,
        )
        .unwrap(),
    );
    assert!(energy(&again.run(1.0)) > 0.01);
}

#[test]
fn seek_and_a_rows_loop_keep_voice_zero_in_its_rows() {
    let mut rig = rig(true);
    rig.control(SongControl::Seek { order: 0, row: 4 });
    rig.control(SongControl::Loop(SongLoop::Range {
        order: 0,
        row0: 4,
        row1: 9,
    }));
    rig.control(SongControl::Play);
    let mut rows = std::collections::BTreeSet::new();
    for _ in 0..60 {
        rig.run(0.1);
        let s = rig.reader.status();
        rows.insert((s.order, s.row));
    }
    assert!(
        rows.iter().all(|&(o, r)| o == 0 && (4..=9).contains(&r)),
        "{rows:?}"
    );
    assert!(rows.len() >= 4, "{rows:?}");
}

#[test]
fn mute_solo_and_scopes_work_by_voice() {
    let mut rig = rig(true);
    rig.control(SongControl::Scopes(true));
    rig.control(SongControl::Play);
    let all = energy(&rig.run(2.0));
    assert!(all > 0.01);
    assert!(rig.reader.scopes_enabled());
    let mut best = 0.0f32;
    let mut written = 0;
    for voice in 0..3 {
        let mut scope = [0.0f32; SONG_SCOPE_FRAMES];
        assert_eq!(rig.reader.scope(voice, &mut scope), SONG_SCOPE_FRAMES);
        best = best.max(peak(&scope));
        written += rig.reader.scope_position(voice);
    }
    assert!(best > 0.0 && written > 0, "{best} {written}");
    rig.control_by_wire(SongControl::MuteMask(0b111));
    rig.run(0.2);
    assert!(peak(&rig.run(0.5)) < 1e-4);
    rig.control_by_wire(SongControl::MuteMask(0));
    rig.control_by_wire(SongControl::SoloMask(0b001));
    rig.control(SongControl::Seek { order: 0, row: 0 });
    let solo = energy(&rig.run(2.0));
    assert!(solo > 0.0 && solo < all, "{solo} vs {all}");
}

#[test]
fn a_preview_sounds_over_a_stopped_song_and_a_timed_one_lets_go() {
    let mut rig = rig(true);
    rig.control_by_wire(SongControl::PreviewOn {
        instrument: 1,
        note: 60,
        velocity: 1.0,
        duration: 0,
    });
    assert!(energy(&rig.run(0.3)) > 0.001, "the preview is heard");
    let s = rig.reader.status();
    assert!(s.preview && !s.playing, "{s:?}");
    rig.control(SongControl::PreviewOff);
    rig.run(2.0);
    assert!(!rig.reader.status().preview || peak(&rig.run(0.3)) < 1e-3);
    // A timed note lets go by itself.
    rig.control(SongControl::PreviewOn {
        instrument: 1,
        note: 64,
        velocity: 1.0,
        duration: 4_800,
    });
    assert!(energy(&rig.run(0.2)) > 0.001);
    rig.run(3.0);
    assert!(peak(&rig.run(0.3)) < 1e-3);
}

/// A new song (one instrument, a plain pulse), and the track length of what a voice plays.
fn quiet_song() -> cubed_sid::song::SidSong {
    SidDoc::new().song().clone()
}

fn row(pattern: u16, row: u16, note: u8, instrument: u8) -> SongControl {
    SongControl::SetCell(SongCell {
        pattern,
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
fn a_live_row_is_heard_when_the_song_gets_to_it_and_an_instrument_edit_changes_the_next_note() {
    let mut rig = rig_of(open_sid(quiet_song(), RATE, SongOptions::default(), true).unwrap());
    rig.run(0.3);
    rig.control(SongControl::Play);
    assert!(peak(&rig.run(0.5)) < 1e-4, "the song is empty");
    let at = rig.reader.status().row;
    rig.control_by_wire(row(0, at + 8, 49, 1));
    let heard = energy(&rig.run(2.0));
    assert!(heard > 0.001, "the note sounds once its row comes");
    // A quieter instrument for the notes that follow: sustain 0, release 0, no decay.
    rig.control(SongControl::Stop);
    rig.run(1.0);
    rig.control(SongControl::PreviewOn {
        instrument: 1,
        note: 60,
        velocity: 1.0,
        duration: 9_600,
    });
    let loud = energy(&rig.run(0.4));
    rig.run(1.0);
    let mut fm = [0u8; 11];
    // attack 0, decay 0, sustain 0, release 0; the first wave and the pointers as they were.
    fm[2] = 0x09;
    fm[3] = 2;
    fm[4] = 1;
    fm[6] = 1;
    fm[7] = 1;
    rig.control_by_wire(SongControl::SetInstrument(SongInstrument {
        number: 1,
        fm,
        ..Default::default()
    }));
    rig.control(SongControl::PreviewOn {
        instrument: 1,
        note: 60,
        velocity: 1.0,
        duration: 9_600,
    });
    let quiet = energy(&rig.run(0.4));
    assert!(quiet < loud * 0.5, "{quiet} vs {loud}");
    // A table row rides the wire too.
    rig.control_by_wire(SongControl::SetTableRow {
        table: 1,
        row: 0,
        left: 0x80,
        right: 0,
    });
    rig.run(0.1);
}

#[test]
fn a_controlled_song_stops_at_its_end_and_an_uncontrolled_one_ends() {
    // Four rows of 3 frames: a pass is 0.24 s.
    let mut doc = SidDoc::new();
    for op in [
        json!({"op": "set_pattern_rows", "pattern": 0, "rows": 4, "force": true}),
        json!({"op": "set_pattern_rows", "pattern": 1, "rows": 4, "force": true}),
        json!({"op": "set_pattern_rows", "pattern": 2, "rows": 4, "force": true}),
    ] {
        doc.apply(&op).unwrap_or_else(|e| panic!("{op}: {e}"));
    }
    let song = doc.song().clone();
    let options = SongOptions {
        looped: false,
        ..Default::default()
    };
    let mut rig = rig_of(open_sid(song.clone(), RATE, options, true).unwrap());
    rig.control(SongControl::Play);
    rig.run(1.5);
    let s = rig.reader.status();
    assert!(!s.playing, "stopped at the end: {s:?}");
    assert!(rig.mixer.is_playing(VOICE));
    let mut rig = rig_of(open_sid(song.clone(), RATE, options, true).unwrap());
    rig.control(SongControl::Looped(true));
    rig.control(SongControl::Play);
    rig.run(1.5);
    assert!(rig.reader.status().playing);
    let mut rig = rig_of(open_sid(song, RATE, options, false).unwrap());
    rig.run(1.5);
    assert!(rig.events().contains(&Event::Ended(VOICE)));
}
