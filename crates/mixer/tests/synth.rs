//! A synth patch as a mixer voice, played by note events.

use cubed_mixer::{
    BusId, BusSettings, Command, Event, Mixer, MixerConfig, PlaySource, SourceEvent, SourceId,
    SynthSource, VoiceId,
};

const RATE: f32 = 48_000.0;
const BUS: BusId = BusId(1);

fn patch() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dsp/tests/real_patch.json"
    ))
    .unwrap()
}

fn mixer_with(source: SynthSource) -> Mixer {
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE,
        ..MixerConfig::default()
    });
    mixer.apply(Command::SetBus(BUS, BusSettings::default()));
    mixer.apply(Command::PrepareSource(SourceId(0), Box::new(source)));
    mixer.apply(Command::PlaySource(PlaySource::new(
        VoiceId(1),
        SourceId(0),
        BUS,
    )));
    mixer
}

fn note(mixer: &mut Mixer, event: SourceEvent) {
    mixer.apply(Command::SourceEvent {
        voice: VoiceId(1),
        event,
    });
}

fn rms_of(mixer: &mut Mixer, frames: usize) -> f32 {
    // Odd block lengths, so the synth's own blocks do not line up.
    let mut sum = 0.0;
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(97);
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        mixer.process(&mut l, &mut r);
        sum += l.iter().map(|s| s * s).sum::<f32>();
        done += n;
    }
    (sum / frames as f32).sqrt()
}

#[test]
fn notes_play_and_release() {
    let mut mixer = mixer_with(SynthSource::from_patch(&patch(), RATE).unwrap());
    assert!(rms_of(&mut mixer, 4_800) < 1e-4, "silent before a note");
    note(
        &mut mixer,
        SourceEvent::NoteOn {
            note: 60,
            velocity: 1.0,
            duration: 0,
        },
    );
    let held = rms_of(&mut mixer, 12_000);
    assert!(held > 0.01, "the note is heard: {held}");
    note(&mut mixer, SourceEvent::NoteOff { note: 60 });
    rms_of(&mut mixer, 48_000);
    let after = rms_of(&mut mixer, 4_800);
    assert!(after < held * 0.1, "released: {after} vs {held}");
    // A held synth voice plays on until it is stopped.
    assert_eq!(mixer.stats().voices, 1);
}

#[test]
fn more_notes_than_voices_steal_the_oldest() {
    let mut mixer = mixer_with(SynthSource::from_patch(&patch(), RATE).unwrap());
    for k in 0..12 {
        note(
            &mut mixer,
            SourceEvent::NoteOn {
                note: 48 + k,
                velocity: 0.8,
                duration: 0,
            },
        );
        rms_of(&mut mixer, 512);
    }
    let chord = rms_of(&mut mixer, 9_600);
    assert!(chord.is_finite() && chord > 0.01, "{chord}");
    note(&mut mixer, SourceEvent::AllNotesOff);
    rms_of(&mut mixer, 96_000);
    assert!(rms_of(&mut mixer, 4_800) < chord * 0.1);
}

#[test]
fn a_one_shot_note_ends_its_voice() {
    let mut mixer = mixer_with(SynthSource::from_patch(&patch(), RATE).unwrap().one_shot());
    note(
        &mut mixer,
        SourceEvent::NoteOn {
            note: 72,
            velocity: 1.0,
            duration: 4_800,
        },
    );
    let mut ended = false;
    let (mut l, mut r) = (vec![0.0; 128], vec![0.0; 128]);
    for _ in 0..(RATE as usize * 10 / 128) {
        mixer.process(&mut l, &mut r);
        mixer.drain_events(|e| ended |= e == Event::Ended(VoiceId(1)));
        if ended {
            break;
        }
    }
    assert!(ended, "the voice ends after its note dies away");
}
