//! Sources (sounds made as they play), long gain fades, and ducking.

use std::sync::Arc;

use cubed_mixer::{
    BusId, BusSettings, Clip, ClipId, Command, Duck, Event, Mixer, MixerConfig, Play, PlaySfx,
    PlaySource, Returned, Sfx, SfxPreset, Source, SourceId, VoiceId,
};

const RATE: f32 = 48_000.0;
const SFX: BusId = BusId(1);
const MUSIC: BusId = BusId(2);

fn mixer() -> Mixer {
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE,
        ..MixerConfig::default()
    });
    for bus in [SFX, MUSIC] {
        mixer.apply(Command::SetBus(bus, BusSettings::default()));
    }
    mixer
}

/// A constant level on both channels for `frames` frames (forever if `None`).
struct Constant {
    level: f32,
    frames: Option<usize>,
}

impl Source for Constant {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let n = left.len();
        let k = self.frames.map_or(n, |f| f.min(n));
        left[..k].fill(self.level);
        right[..k].fill(self.level);
        left[k..].fill(0.0);
        right[k..].fill(0.0);
        match &mut self.frames {
            None => true,
            Some(f) => {
                *f -= k;
                *f > 0
            }
        }
    }
}

fn constant(level: f32, frames: Option<usize>) -> Box<dyn Source> {
    Box::new(Constant { level, frames })
}

fn play_source(mixer: &mut Mixer, voice: u64, slot: u32, bus: BusId, source: Box<dyn Source>) {
    mixer.apply(Command::PrepareSource(SourceId(slot), source));
    mixer.apply(Command::PlaySource(PlaySource::new(
        VoiceId(voice),
        SourceId(slot),
        bus,
    )));
}

fn render(mixer: &mut Mixer, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    mixer.process(&mut l, &mut r);
    (l, r)
}

fn events(mixer: &mut Mixer) -> Vec<Event> {
    let mut out = Vec::new();
    mixer.drain_events(|e| out.push(e));
    out
}

fn returned_sources(mixer: &mut Mixer) -> usize {
    let mut n = 0;
    mixer.take_returned(|r| n += matches!(r, Returned::Source(_)) as usize);
    n
}

#[test]
fn a_source_plays_until_it_ends_and_comes_back() {
    let mut mixer = mixer();
    play_source(&mut mixer, 1, 0, SFX, constant(0.25, Some(300)));
    assert!(!mixer.is_prepared(SourceId(0)), "playing empties the slot");
    let (l, r) = render(&mut mixer, 512);
    assert!(l[..300].iter().chain(&r[..300]).all(|&s| s == 0.25));
    assert!(l[300..].iter().all(|&s| s == 0.0));
    assert_eq!(events(&mut mixer), vec![Event::Ended(VoiceId(1))]);
    assert_eq!(returned_sources(&mut mixer), 1);
    assert_eq!(mixer.stats().voices, 0);
}

#[test]
fn an_unprepared_or_unplayable_source_is_rejected() {
    let mut mixer = mixer();
    mixer.apply(Command::PlaySource(PlaySource::new(
        VoiceId(1),
        SourceId(0),
        SFX,
    )));
    assert_eq!(events(&mut mixer), vec![Event::Rejected(VoiceId(1))]);

    // A bus that cannot take it: the prepared source is let go.
    mixer.apply(Command::SetBus(
        SFX,
        BusSettings {
            voice_limit: 0,
            ..BusSettings::default()
        },
    ));
    play_source(&mut mixer, 2, 1, SFX, constant(1.0, None));
    assert_eq!(events(&mut mixer), vec![Event::Rejected(VoiceId(2))]);
    assert_eq!(returned_sources(&mut mixer), 1);

    // A slot out of range hands the source straight back.
    mixer.apply(Command::PrepareSource(
        SourceId(10_000),
        constant(1.0, None),
    ));
    assert_eq!(returned_sources(&mut mixer), 1);
}

#[test]
fn stopping_a_source_fades_it_and_returns_it() {
    let mut mixer = mixer();
    play_source(&mut mixer, 1, 0, MUSIC, constant(1.0, None));
    render(&mut mixer, 1024);
    mixer.apply(Command::Stop(VoiceId(1), 256));
    let (l, _) = render(&mut mixer, 512);
    assert_eq!(l[0], 1.0);
    assert!(l[128] < 0.6 && l[128] > 0.4, "{}", l[128]);
    assert!(l[256..].iter().all(|&s| s == 0.0));
    assert_eq!(events(&mut mixer), vec![Event::Ended(VoiceId(1))]);
    assert_eq!(returned_sources(&mut mixer), 1);
}

#[test]
fn a_crossfade_swaps_two_songs_at_constant_sum() {
    let mut mixer = mixer();
    play_source(&mut mixer, 1, 0, MUSIC, constant(1.0, None));
    render(&mut mixer, 4800);
    // The new song starts silent and fades in as the old one fades out.
    let fade = 48_000;
    mixer.apply(Command::PrepareSource(SourceId(1), constant(1.0, None)));
    mixer.apply(Command::PlaySource(PlaySource {
        gain: 0.0,
        ..PlaySource::new(VoiceId(2), SourceId(1), MUSIC)
    }));
    mixer.apply(Command::FadeVoice {
        voice: VoiceId(2),
        gain: 1.0,
        frames: fade,
    });
    mixer.apply(Command::FadeVoice {
        voice: VoiceId(1),
        gain: 0.0,
        frames: fade,
    });
    let (l, _) = render(&mut mixer, fade as usize);
    let quarter = l[fade as usize / 4];
    assert!((quarter - 1.0).abs() < 0.02, "sum stays 1: {quarter}");
    assert!(l.iter().all(|&s| (s - 1.0).abs() < 0.02));
    // At the end only the new one is heard; the old one can then stop.
    mixer.apply(Command::Stop(VoiceId(1), 0));
    let (l, _) = render(&mut mixer, 4800);
    assert!((l[4799] - 1.0).abs() < 1e-4, "{}", l[4799]);
    assert_eq!(events(&mut mixer), vec![Event::Ended(VoiceId(1))]);
}

#[test]
fn a_fade_also_moves_a_clip_voice_and_set_gain_cancels_it() {
    let mut mixer = mixer();
    mixer.apply(Command::AddClip(
        ClipId(0),
        Arc::new(Clip::mono(RATE, vec![1.0; 480_000])),
    ));
    mixer.apply(Command::Play(Play::new(VoiceId(1), ClipId(0), SFX)));
    mixer.apply(Command::FadeVoice {
        voice: VoiceId(1),
        gain: 0.0,
        frames: 48_000,
    });
    let (l, _) = render(&mut mixer, 24_000);
    assert!((l[23_999] - 0.5).abs() < 0.02, "halfway: {}", l[23_999]);
    mixer.apply(Command::SetVoiceGain(VoiceId(1), 0.5));
    let (l, _) = render(&mut mixer, 48_000);
    assert!((l[47_999] - 0.5).abs() < 1e-4, "{}", l[47_999]);
}

#[test]
fn a_ducked_bus_goes_down_while_its_key_is_loud() {
    let mut mixer = mixer();
    mixer.apply(Command::SetBus(
        MUSIC,
        BusSettings {
            duck: Some(Duck::new(SFX, 12.0)),
            ..BusSettings::default()
        },
    ));
    play_source(&mut mixer, 1, 0, MUSIC, constant(0.5, None));
    let (l, _) = render(&mut mixer, 4800);
    assert_eq!(l[4799], 0.5, "nothing on the key: no ducking");

    // Effects for half a second: the music goes 12 dB down.
    play_source(&mut mixer, 2, 1, SFX, constant(0.25, Some(24_000)));
    let (l, _) = render(&mut mixer, 24_000);
    let ducked = 0.5 * 10f32.powf(-12.0 / 20.0);
    assert!(
        (l[23_999] - (0.25 + ducked)).abs() < 1e-3,
        "{} vs {}",
        l[23_999],
        0.25 + ducked
    );
    // The attack is quick (a 20 ms time constant) ...
    assert!((l[4800] - (0.25 + ducked)).abs() < 0.01, "{}", l[4800]);

    // ... the release slower (400 ms), and then it is back.
    let (l, _) = render(&mut mixer, 192_000);
    assert!(l[4800] < 0.25, "still coming back: {}", l[4800]);
    assert!((l[191_999] - 0.5).abs() < 1e-3, "{}", l[191_999]);
}

#[test]
fn a_bus_cannot_duck_under_itself() {
    let mut mixer = mixer();
    mixer.apply(Command::SetBus(
        MUSIC,
        BusSettings {
            duck: Some(Duck::new(MUSIC, 6.0)),
            ..BusSettings::default()
        },
    ));
    assert_eq!(mixer.stats().rejected_commands, 1);
}

#[cfg(feature = "songs")]
mod songs {
    use super::*;
    use cubed_mixer::songs::{open_song, SongFormat, SongOptions};

    fn fixture(path: &str) -> Vec<u8> {
        let path = format!("{}/../{path}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn songs_of_each_format_play_on_a_bus() {
        for (path, ext) in [
            ("ahx/tests/fixtures/songs/a_new_beginning.ahx", "ahx"),
            ("ahx/tests/fixtures/songs/a_little_cyberfunk.hvl", "hvl"),
            ("opl/tests/fixtures/a2m/Kvee/ym3812 funk.a2m", "a2m"),
            ("sid/tests/fixtures/sid/s3-chain.asid", "asid"),
            ("tracker/tests/fixtures/demo.mod", "MOD"),
            ("tracker/tests/fixtures/demo.s3m", "s3m"),
            ("tracker/tests/fixtures/demo.xm", "xm"),
        ] {
            let format = SongFormat::from_extension(ext).expect("known extension");
            let song = open_song(format, &fixture(path), RATE, SongOptions::default())
                .unwrap_or_else(|e| panic!("{path}: {e}"));
            let mut mixer = mixer();
            play_source(&mut mixer, 1, 0, MUSIC, song);
            let (l, r) = render(&mut mixer, 96_000);
            let level = rms(&l).max(rms(&r));
            assert!(level > 0.01, "{path}: rms {level}");
            let peak = l.iter().chain(&r).fold(0f32, |p, s| p.max(s.abs()));
            assert!(peak <= 1.5, "{path}: peak {peak}");
            assert_eq!(mixer.stats().voices, 1, "{path} still playing");
        }
    }

    /// A module ends with its song when it is not looped (and is handed
    /// back to be freed), and can start at a later order.
    #[test]
    fn a_module_ends_or_starts_later() {
        let bytes = fixture("tracker/tests/fixtures/demo.mod");
        let once = SongOptions {
            looped: false,
            ..SongOptions::default()
        };
        let song = open_song(SongFormat::Mod, &bytes, RATE, once).unwrap();
        let mut mixer = mixer();
        play_source(&mut mixer, 1, 0, MUSIC, song);
        // The demo is 20.4 s long.
        let _ = render(&mut mixer, 20 * 48_000);
        assert_eq!(mixer.stats().voices, 1);
        let _ = render(&mut mixer, 48_000);
        assert_eq!(mixer.stats().voices, 0);
        assert_eq!(events(&mut mixer), vec![Event::Ended(VoiceId(1))]);
        assert_eq!(returned_sources(&mut mixer), 1);

        // From order 3 (the last) at speed 4 and tempo 150, as the song gets
        // there: 64 rows, 8 more from a pattern loop, 1 from a delay, 4.9 s.
        let late = SongOptions { subsong: 3, ..once };
        let song = open_song(SongFormat::Mod, &bytes, RATE, late).unwrap();
        play_source(&mut mixer, 2, 0, MUSIC, song);
        let _ = render(&mut mixer, 4 * 48_000 + 36_000);
        assert_eq!(mixer.stats().voices, 1);
        let _ = render(&mut mixer, 48_000);
        assert_eq!(mixer.stats().voices, 0);
    }

    /// Stereo separation narrows a module's Amiga panning to mono at 0.
    #[test]
    fn a_module_plays_at_a_stereo_separation() {
        let bytes = fixture("tracker/tests/fixtures/demo.mod");
        let mut sides = Vec::new();
        for separation in [None, Some(1.0), Some(0.0)] {
            let options = SongOptions {
                stereo_separation: separation,
                ..SongOptions::default()
            };
            let song = open_song(SongFormat::Mod, &bytes, RATE, options).unwrap();
            let mut mixer = mixer();
            play_source(&mut mixer, 1, 0, MUSIC, song);
            let (l, r) = render(&mut mixer, 48_000);
            let side: Vec<f32> = l.iter().zip(&r).map(|(l, r)| (l - r) * 0.5).collect();
            sides.push(rms(&side));
        }
        // The default is three quarters of full width; mono has no side.
        assert!((sides[0] / sides[1] - 0.75).abs() < 0.01, "{sides:?}");
        assert!(sides[2] < 1e-6, "{sides:?}");
    }

    #[test]
    fn unknown_or_broken_files_are_refused() {
        assert_eq!(SongFormat::from_extension("HVL"), Some(SongFormat::Ahx));
        assert_eq!(SongFormat::from_extension("ogg"), None);
        for format in [
            SongFormat::Ahx,
            SongFormat::A2m,
            SongFormat::Sid,
            SongFormat::Mod,
            SongFormat::S3m,
            SongFormat::Xm,
        ] {
            assert!(open_song(format, b"not a song", RATE, SongOptions::default()).is_err());
        }
    }
}

#[test]
fn a_sound_effect_plays_from_plain_data_and_ends() {
    let mut mixer = mixer();
    let sfx = SfxPreset::Laser.sfx().normalized(0.5, RATE);
    mixer.apply(Command::PlaySfx(PlaySfx::new(VoiceId(1), sfx, SFX)));
    let (l, r) = render(&mut mixer, 24_000);
    let peak = l.iter().chain(&r).fold(0f32, |p, s| p.max(s.abs()));
    assert!((peak - 0.5).abs() < 0.01, "normalized: {peak}");
    // A quarter-second laser ends by itself; nothing comes back to free.
    assert_eq!(events(&mut mixer), vec![Event::Ended(VoiceId(1))]);
    assert_eq!(returned_sources(&mut mixer), 0);

    // Pitch shortens nothing but raises the sound; another seed changes the noise.
    let explosion = SfxPreset::Explosion.sfx();
    let play = |mixer: &mut Mixer, voice, sfx: Sfx, pitch| {
        mixer.apply(Command::PlaySfx(PlaySfx {
            pitch,
            ..PlaySfx::new(VoiceId(voice), sfx, SFX)
        }));
        render(mixer, 48_000).0
    };
    let a = play(&mut mixer, 2, explosion, 1.0);
    let b = play(
        &mut mixer,
        3,
        Sfx {
            seed: 5,
            ..explosion
        },
        1.0,
    );
    assert_ne!(a, b);
}
