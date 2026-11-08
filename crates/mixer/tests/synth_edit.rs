//! A synth patch edited while it plays: parameter edits as events, a rebuilt
//! synth swapped in with a crossfade, macros and the instrument gain.

use cubed_dsp::catalog;
use cubed_dsp::patch_edit::PatchDoc;
use cubed_dsp::traits::PortId;
use cubed_mixer::{
    swap_commands, wire, BusId, BusSettings, Command, Event, HeldNote, Mixer, MixerConfig,
    PlaySource, SourceEvent, SourceId, SynthBuild, SynthSource, VoiceId,
};
use serde_json::json;

const RATE: f32 = 48_000.0;
const BUS: BusId = BusId(1);

fn mixer() -> Mixer {
    let mut mixer = Mixer::new(MixerConfig {
        sample_rate: RATE,
        ..MixerConfig::default()
    });
    mixer.apply(Command::SetBus(BUS, BusSettings::default()));
    mixer
}

fn play(mixer: &mut Mixer, doc: &PatchDoc, voice: u64, slot: u32) {
    let source = SynthSource::from_patch(&doc.to_json(), RATE).unwrap();
    mixer.apply(Command::PrepareSource(SourceId(slot), Box::new(source)));
    mixer.apply(Command::PlaySource(PlaySource::new(
        VoiceId(voice),
        SourceId(slot),
        BUS,
    )));
}

fn event(mixer: &mut Mixer, voice: u64, event: SourceEvent) {
    mixer.apply(Command::SourceEvent {
        voice: VoiceId(voice),
        event,
    });
}

fn note_on(mixer: &mut Mixer, voice: u64, note: u8) {
    event(
        mixer,
        voice,
        SourceEvent::NoteOn {
            note,
            velocity: 1.0,
            duration: 0,
        },
    );
}

/// The RMS of `blocks` 128-frame blocks, one value each.
fn block_levels(mixer: &mut Mixer, blocks: usize) -> Vec<f32> {
    (0..blocks)
        .map(|_| {
            let (mut l, mut r) = (vec![0.0; 128], vec![0.0; 128]);
            mixer.process(&mut l, &mut r);
            (l.iter().map(|s| s * s).sum::<f32>() / 128.0).sqrt()
        })
        .collect()
}

fn level(mixer: &mut Mixer, blocks: usize) -> f32 {
    let levels = block_levels(mixer, blocks);
    levels.iter().sum::<f32>() / levels.len() as f32
}

/// A sustained sine pad: the init patch with a sine and a steady envelope.
fn pad() -> (PatchDoc, String) {
    let mut doc = PatchDoc::new("pad");
    let osc = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "oscillator")
        .unwrap()
        .id;
    let env = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "envelope")
        .unwrap()
        .id;
    doc.set_param(&osc, "waveform", &json!(0)).unwrap();
    doc.set_param(&osc, "gain", &json!(0.6)).unwrap();
    doc.set_param(&env, "sustain", &json!(1.0)).unwrap();
    doc.set_param(&env, "attack", &json!(0.02)).unwrap();
    doc.set_param("10005", "active", &json!(false)).unwrap();
    (doc, osc)
}

#[test]
fn parameter_events_change_a_playing_synth() {
    let (mut doc, osc) = pad();
    let mut mixer = mixer();
    play(&mut mixer, &doc, 1, 0);
    note_on(&mut mixer, 1, 57);
    block_levels(&mut mixer, 100);
    let loud = level(&mut mixer, 40);
    assert!(loud > 0.05, "{loud}");

    // The oscillator's gain down: the level follows.
    let edit = doc
        .set_param(&osc, "gain", &json!(0.15))
        .unwrap()
        .edit
        .unwrap();
    event(&mut mixer, 1, SourceEvent::Param(edit));
    block_levels(&mut mixer, 40);
    let quiet = level(&mut mixer, 40);
    assert!(quiet < 0.4 * loud && quiet > 0.0, "{quiet} vs {loud}");

    // The instrument gain: half again.
    let edit = doc.set_instrument_gain(0.5).unwrap().edit.unwrap();
    event(&mut mixer, 1, SourceEvent::Param(edit));
    block_levels(&mut mixer, 10);
    let halved = level(&mut mixer, 40);
    assert!((halved / quiet - 0.5).abs() < 0.1, "{halved} vs {quiet}");

    // A node that is not there: nothing happens.
    event(
        &mut mixer,
        1,
        SourceEvent::Param(cubed_dsp::live::ParamEdit::new(77, 0, 1.0)),
    );
    let still = level(&mut mixer, 40);
    assert!((still / halved - 1.0).abs() < 0.1);
}

#[test]
fn a_macro_in_the_patch_is_heard_and_moves_by_event() {
    let (mut doc, _) = pad();
    let mixer_node = doc
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "mixer")
        .unwrap()
        .id;
    doc.set_macro_route(0, &mixer_node, PortId::GainMod, 1.0, 0, 0)
        .unwrap();
    doc.set_macro_value(0, 0.0).unwrap();
    let mut mixer = mixer();
    play(&mut mixer, &doc, 1, 0);
    note_on(&mut mixer, 1, 57);
    block_levels(&mut mixer, 100);
    let closed = level(&mut mixer, 40);
    assert!(closed < 1e-3, "the macro is closed: {closed}");
    event(
        &mut mixer,
        1,
        SourceEvent::Param(cubed_dsp::live::ParamEdit::macro_value(0, 1.0)),
    );
    block_levels(&mut mixer, 20);
    let open = level(&mut mixer, 40);
    assert!(open > 0.05, "{open}");
}

#[test]
fn a_rebuilt_synth_replaces_the_playing_one_without_a_gap() {
    let (doc, osc) = pad();
    let mut mixer = mixer();
    play(&mut mixer, &doc, 1, 0);
    note_on(&mut mixer, 1, 57);
    block_levels(&mut mixer, 200);
    let before = level(&mut mixer, 40);
    assert!(before > 0.05);

    // A structural edit: a second oscillator, an octave up, joins the mixer.
    let mut edited = doc.clone();
    let mixer_node = edited
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "mixer")
        .unwrap()
        .id;
    let env_node = edited
        .nodes()
        .into_iter()
        .find(|n| n.type_name == "envelope")
        .unwrap()
        .id;
    let osc2 = edited.add_node("oscillator").unwrap();
    edited.set_param(&osc2, "waveform", &json!(0)).unwrap();
    edited.set_param(&osc2, "detune_oct", &json!(1)).unwrap();
    edited.set_param(&osc2, "gain", &json!(0.6)).unwrap();
    edited
        .connect(
            &osc2,
            PortId::AudioOutput0,
            &mixer_node,
            PortId::AudioInput0,
            1.0,
            2,
            0,
        )
        .unwrap();
    let _ = (&osc, &env_node);

    let build = SynthBuild {
        held: vec![HeldNote {
            note: 57,
            velocity: 1.0,
        }],
        warm_blocks: 80,
        ..SynthBuild::default()
    };
    let rebuilt = SynthSource::from_patch_with(&edited.to_json(), RATE, &build).unwrap();
    mixer.apply(Command::PrepareSource(SourceId(1), Box::new(rebuilt)));
    for command in swap_commands(
        VoiceId(1),
        PlaySource::new(VoiceId(2), SourceId(1), BUS),
        2_400,
    ) {
        mixer.apply(command);
    }

    // Through the crossfade the level never falls away.
    let during = block_levels(&mut mixer, 60);
    for (i, level) in during.iter().enumerate() {
        assert!(
            *level > 0.5 * before,
            "block {i} of the crossfade: {level} vs {before}"
        );
    }
    let mut ended = false;
    mixer.drain_events(|e| ended |= e == Event::Ended(VoiceId(1)));
    assert!(ended, "the old voice ended");
    assert_eq!(mixer.stats().voices, 1);
    // Afterwards: the new patch's sound (a second oscillator is louder), still
    // holding the note, and editing the new voice works.
    let after = level(&mut mixer, 40);
    assert!(after > 1.2 * before, "{after} vs {before}");
    let edit = edited
        .set_param(&osc2, "gain", &json!(0.0))
        .unwrap()
        .edit
        .unwrap();
    event(&mut mixer, 2, SourceEvent::Param(edit));
    block_levels(&mut mixer, 40);
    let without = level(&mut mixer, 40);
    assert!(without < 0.8 * after, "{without} vs {after}");
}

#[test]
fn a_synth_built_with_held_notes_is_already_sounding() {
    let (doc, _) = pad();
    let build = SynthBuild {
        held: vec![
            HeldNote {
                note: 48,
                velocity: 1.0,
            },
            HeldNote {
                note: 55,
                velocity: 0.7,
            },
        ],
        warm_blocks: 60,
        ..SynthBuild::default()
    };
    let synth = SynthSource::from_patch_with(&doc.to_json(), RATE, &build).unwrap();
    let mut mixer = mixer();
    mixer.apply(Command::PrepareSource(SourceId(0), Box::new(synth)));
    mixer.apply(Command::PlaySource(PlaySource::new(
        VoiceId(1),
        SourceId(0),
        BUS,
    )));
    let first = block_levels(&mut mixer, 4);
    assert!(first[0] > 0.03, "sounding from the first block: {first:?}");
}

#[test]
fn param_events_survive_the_wire() {
    let edit = cubed_dsp::live::ParamEdit::new(u128::MAX - 5, 11, 0.33);
    let command = Command::SourceEvent {
        voice: VoiceId(4),
        event: SourceEvent::Param(edit),
    };
    let mut bytes = Vec::new();
    assert!(wire::encode(&command, &mut bytes));
    assert_eq!(bytes.len(), 1 + 8 + 16 + 2 + 4);
    let decoded: Vec<_> = wire::decode(&bytes).collect();
    assert_eq!(decoded.len(), 1);
    match decoded[0].as_ref().unwrap() {
        Command::SourceEvent { voice, event } => {
            assert_eq!(*voice, VoiceId(4));
            assert_eq!(*event, SourceEvent::Param(edit));
        }
        other => panic!("{other:?}"),
    }
    // A cut message is an error, not a wrong edit.
    assert!(wire::decode(&bytes[..bytes.len() - 1]).any(|c| c.is_err()));
    let _ = catalog::VERSION;
}
