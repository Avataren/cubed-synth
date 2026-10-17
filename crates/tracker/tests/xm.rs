//! The XM demo module (tests/fixtures/demo.xm, synthetic) through the
//! parser and the player: instruments with keymaps, envelopes, fadeout and
//! auto-vibrato, the volume column, FT2's commands.
//!
//! The command log was checked command for command against the another_synth
//! DAW's TypeScript engine on this module and 10 real ones.

use cubed_tracker::{Command, Format, LoopKind, Note, Observer, Player, PlayerOptions};

const RATE: f32 = 48_000.0;

fn demo() -> cubed_tracker::Module {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/demo.xm"
    ))
    .unwrap();
    assert_eq!(cubed_tracker::sniff(&bytes), Some(Format::Xm));
    cubed_tracker::load(&bytes).unwrap()
}

#[derive(Default)]
struct Log {
    rows: Vec<(f64, usize, usize)>,
    commands: Vec<(f64, usize, u8, Command)>,
    global: Vec<f64>,
}

impl Observer for Log {
    fn row(&mut self, time: f64, order: usize, row: usize) {
        self.rows.push((time, order, row));
    }
    fn command(&mut self, time: f64, channel: usize, instrument: u8, command: &Command) {
        self.commands.push((time, channel, instrument, *command));
    }
    fn global_volume(&mut self, _time: f64, volume: f64) {
        self.global.push(volume);
    }
}

fn play(player: &mut Player, seconds: f64) -> (Log, Vec<f32>) {
    let mut log = Log::default();
    let (mut l, mut r) = ([0.0f32; 128], [0.0f32; 128]);
    let mut audio = Vec::new();
    while player.time() < seconds {
        let more = player.render_observed(&mut l, &mut r, &mut log);
        for i in 0..128 {
            audio.push(l[i]);
            audio.push(r[i]);
        }
        if !more {
            break;
        }
    }
    (log, audio)
}

fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn the_demo_parses() {
    let m = demo();
    assert_eq!(m.format, Format::Xm);
    assert!(m.flags.linear_frequency);
    assert_eq!((m.channels, m.orders.clone()), (6, vec![0, 1, 2, 1]));
    assert_eq!(
        m.patterns.iter().map(|p| p.rows()).collect::<Vec<_>>(),
        [64, 48, 32]
    );
    assert_eq!(m.instruments.len(), 4);
    let pad = &m.instruments[0];
    let env = pad
        .volume_envelope
        .as_ref()
        .expect("the pad has a volume envelope");
    assert_eq!(env.points[1], (6, 64));
    assert_eq!((env.sustain, env.looped), (Some(2), Some((2, 4))));
    assert_eq!(pad.fadeout, 1200);
    assert_eq!(
        (pad.vibrato.depth, pad.vibrato.rate, pad.vibrato.sweep),
        (6, 12, 8)
    );
    // The keys: square below C-4, a 16-bit ping-pong sine an octave up above.
    let keys = &m.instruments[1];
    assert_eq!(keys.samples.len(), 2);
    assert_eq!((keys.keymap[47], keys.keymap[48]), (0, 1));
    let sine = &m.samples[keys.samples[1]];
    assert_eq!(
        (
            sine.data.len(),
            sine.loop_kind,
            sine.loop_start,
            sine.loop_len
        ),
        (128, LoopKind::PingPong, 16, 96)
    );
    assert_eq!(
        (sine.relative_note, sine.finetune, sine.pan),
        (12, -20, Some(192))
    );
    // C-4 is MIDI 60; a key-off is its own note.
    assert_eq!(m.patterns[0].cell(0, 0).note, Note::On(60));
    assert_eq!(m.patterns[0].cell(12, 0).note, Note::Off);
    assert_eq!(m.patterns[0].cell(0, 1).volume, 0x30);
}

#[test]
fn the_demo_plays_and_ends() {
    let mut p = Player::new(
        demo(),
        RATE,
        PlayerOptions {
            looped: false,
            ..Default::default()
        },
    );
    let (log, audio) = play(&mut p, 60.0);
    assert!(p.finished());
    assert_eq!(log.rows.len(), 217);
    // E60 / E63 repeat order 1's first four rows three more times.
    let order1: Vec<usize> = log
        .rows
        .iter()
        .filter(|r| r.1 == 1)
        .take(16)
        .map(|r| r.2)
        .collect();
    assert_eq!(order1, [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3]);
    // G30 sets 48/64; H40 then slides it up 4/64 on every tick after the
    // first (FT2; the DAW slides once a row), to full.
    assert_eq!(log.global[..5], [0.75, 0.8125, 0.875, 0.9375, 1.0]);
    assert!(audio.iter().all(|s| s.is_finite() && s.abs() < 4.0));
}

/// The pad's volume envelope fades it in over 6 ticks, holds at its sustain
/// point while the key is down, and after the key-off loops on while the
/// fadeout takes it down.
#[test]
fn envelopes_shape_the_pad() {
    let module = demo();
    let mut p = Player::new(
        module,
        RATE,
        PlayerOptions {
            looped: false,
            ..Default::default()
        },
    );
    for c in 1..6 {
        p.set_channel_muted(c, true);
    }
    let (_, audio) = play(&mut p, 1.6);
    let at = |seconds: f64, ms: f64| {
        let start = (seconds * RATE as f64) as usize * 2;
        rms(&audio[start..start + (ms * RATE as f64 / 1000.0) as usize * 2])
    };
    // Speed 6 at 125 BPM: a tick is 20 ms, row 12's key-off at 1.44 s.
    assert!(at(0.0, 20.0) < at(0.12, 20.0) * 0.5, "fades in");
    let held = at(1.0, 100.0);
    assert!(held > 0.01);
    let released = at(1.55, 40.0);
    assert!(
        released < held,
        "the fadeout takes over after the key-off: {released} vs {held}"
    );
}

#[test]
fn command_golden() {
    let mut p = Player::new(
        demo(),
        RATE,
        PlayerOptions {
            looped: false,
            ..Default::default()
        },
    );
    let (log, _) = play(&mut p, 60.0);
    let text: String = log
        .commands
        .iter()
        .map(|(t, ch, inst, c)| format!("{t:.9} {ch} {inst} {c:?}\n"))
        .collect();
    assert_eq!(
        fnv(text.bytes()),
        9682250042037348935,
        "command log hash changed"
    );
}

#[test]
fn audio_golden() {
    let mut p = Player::new(
        demo(),
        RATE,
        PlayerOptions {
            looped: false,
            ..Default::default()
        },
    );
    let (_, audio) = play(&mut p, 10.0);
    assert_eq!(
        fnv(audio.iter().flat_map(|s| s.to_bits().to_le_bytes())),
        16599015811359622663,
        "audio hash changed"
    );
}
