//! A PSID tune (the synthetic ones in `cubed-sid`'s fixtures) played, stopped and read in a mixer, the way
//! the other formats' `*_song.rs` do, as far as a tune can be controlled: play and stop, the position in
//! seconds, mute and solo, levels and scopes, a start in the tune, and the controls a tune ignores.

#![cfg(feature = "songs")]

use std::sync::Arc;

use cubed_mixer::songs::{
    open_controlled_song, open_controlled_song_at, open_psid_at, SongFormat, SongOptions,
};
use cubed_mixer::{
    BusId, BusSettings, Command, Mixer, MixerConfig, PlaySource, SongControl, SongLoop, SongReader,
    SourceEvent, SourceId, VoiceId, SONG_SCOPE_FRAMES,
};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;
const VOICE: VoiceId = VoiceId(7);
const MUSIC: BusId = BusId(1);

fn tune(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../sid/tests/fixtures/psid/{name}",
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
    let reader = source.song_reader().expect("a tune has a reader");
    mixer.apply(Command::PrepareSource(SourceId(0), source));
    mixer.apply(Command::PlaySource(PlaySource::new(
        VOICE,
        SourceId(0),
        MUSIC,
    )));
    Rig { mixer, reader }
}

fn rig(name: &str) -> Rig {
    rig_of(
        open_controlled_song(
            SongFormat::Sid,
            &tune(name),
            RATE,
            SongOptions::default(),
            true,
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

    fn run(&mut self, seconds: f64) -> Vec<f32> {
        let mut out = Vec::new();
        let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
        for _ in 0..(seconds * RATE as f64) as usize / BLOCK {
            self.mixer.process(&mut l, &mut r);
            out.extend(l.iter().zip(&r).map(|(a, b)| a + b));
        }
        out
    }
}

/// The chip's output carries a constant offset: judge by the movement.
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

fn seconds(rig: &Rig) -> f64 {
    let s = rig.reader.status();
    s.row as f64 + s.tick as f64 / 100.0
}

#[test]
fn a_tune_opened_stopped_is_silent_until_played_and_keeps_its_place_when_stopped() {
    let mut rig = rig("arp.sid");
    rig.run(0.3);
    assert!(peak(&rig.run(0.5)) < 1e-4);
    let s = rig.reader.status();
    assert!(!s.playing && s.speed > 0, "{s:?}");
    assert_eq!(rig.reader.channels(), 3);
    assert!(rig.reader.alive());
    rig.control(SongControl::Play);
    assert!(energy(&rig.run(2.5)) > 0.01);
    let s = rig.reader.status();
    assert!(s.playing && s.frames > 0 && s.pattern == 0, "{s:?}");
    let at = seconds(&rig);
    assert!((2.0..3.0).contains(&at), "about 2.5 s in: {at}");
    let mut levels = [0.0f32; 40];
    assert_eq!(rig.reader.levels(&mut levels), 4);
    assert!(levels[..3].iter().any(|&l| l > 0.0), "{levels:?}");
    rig.control(SongControl::Stop);
    rig.run(0.2);
    let held = seconds(&rig);
    assert!(peak(&rig.run(0.5)) < 1e-4, "a stopped tune is silent");
    assert_eq!(seconds(&rig), held, "and does not move");
    rig.control(SongControl::Play);
    rig.run(1.0);
    assert!(seconds(&rig) > held + 0.9, "it goes on from where it was");
}

#[test]
fn a_tune_starts_where_the_host_builds_it_and_ignores_seek_and_loop() {
    let bytes = tune("arp.sid");
    let mut rig = rig_of(open_psid_at(&bytes, RATE, Some(0), 3.0, true).unwrap());
    rig.run(0.2);
    let at = seconds(&rig);
    assert!((2.9..3.1).contains(&at), "built at 3 s: {at}");
    assert_eq!(rig.reader.status().order, 0, "the subsong");
    // The controls a tune cannot take on the audio thread change nothing.
    rig.control(SongControl::Seek { order: 0, row: 0 });
    rig.control(SongControl::Loop(SongLoop::Pattern { order: 0 }));
    rig.control(SongControl::PreviewOn {
        instrument: 1,
        note: 60,
        velocity: 1.0,
        duration: 0,
    });
    rig.run(0.2);
    assert!((seconds(&rig) - at).abs() < 0.1);
    // Through the host helper that takes the start.
    let mut again = rig_of(
        open_controlled_song_at(
            SongFormat::Sid,
            &bytes,
            RATE,
            SongOptions::default(),
            true,
            1.5,
        )
        .unwrap(),
    );
    again.run(0.2);
    assert!((1.4..1.6).contains(&seconds(&again)));
}

#[test]
fn voices_mute_and_solo_and_the_scopes_read_the_voices_signals() {
    let mut rig = rig("arp.sid");
    rig.control(SongControl::Scopes(true));
    rig.control(SongControl::Play);
    let all = energy(&rig.run(2.0));
    assert!(all > 0.01);
    assert!(rig.reader.scopes_enabled());
    let mut best = 0.0f32;
    for voice in 0..3 {
        let mut scope = [0.0f32; SONG_SCOPE_FRAMES];
        assert_eq!(rig.reader.scope(voice, &mut scope), SONG_SCOPE_FRAMES);
        best = best.max(peak(&scope));
    }
    assert!(best > 0.0, "{best}");
    rig.control(SongControl::MuteMask(0b111));
    rig.run(0.3);
    assert!(peak(&rig.run(0.5)) < 1e-4);
    rig.control(SongControl::MuteMask(0));
    rig.control(SongControl::SoloMask(0b001));
    let solo = energy(&rig.run(2.0));
    assert!(solo < all, "{solo} vs {all}");
}

#[test]
fn a_tune_with_two_sids_plays_and_a_bad_file_says_why() {
    let mut rig = rig("two.sid");
    rig.control(SongControl::Play);
    assert!(energy(&rig.run(1.5)) > 0.001);
    let error = open_controlled_song(
        SongFormat::Sid,
        b"PSID not really a tune",
        RATE,
        SongOptions::default(),
        true,
    )
    .err()
    .expect("refused");
    assert!(!error.is_empty());
    // A subsong that does not exist is clamped, as the player does.
    assert!(open_psid_at(&tune("arp.sid"), RATE, Some(99), 0.0, true).is_ok());
}
