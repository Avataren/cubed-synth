//! The S3M demo module (tests/fixtures/demo.s3m, synthetic: four PCM
//! samples and an AdLib organ) through the parser and the player.
//!
//! The command-log golden pins the replayer's semantics, checked command for
//! command (AdLib register writes included) against the another_synth DAW's
//! TypeScript engine on this module and 37 real ones.

use cubed_tracker::{Command, Format, LoopKind, Note, Observer, Player, PlayerOptions};

const RATE: f32 = 48_000.0;

fn demo() -> cubed_tracker::Module {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/demo.s3m"
    ))
    .unwrap();
    assert_eq!(cubed_tracker::sniff(&bytes), Some(Format::S3m));
    cubed_tracker::load(&bytes).unwrap()
}

#[derive(Default)]
struct Log {
    rows: Vec<(f64, usize, usize)>,
    commands: Vec<(f64, usize, u8, Command)>,
    adlib: Vec<(f64, u16, u8)>,
    global: Vec<f64>,
}

impl Observer for Log {
    fn row(&mut self, time: f64, order: usize, row: usize) {
        self.rows.push((time, order, row));
    }
    fn command(&mut self, time: f64, channel: usize, instrument: u8, command: &Command) {
        self.commands.push((time, channel, instrument, *command));
    }
    fn adlib_write(&mut self, time: f64, register: u16, value: u8) {
        self.adlib.push((time, register, value));
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

fn energy(x: &[f32]) -> f32 {
    x.iter().map(|s| s * s).sum()
}

#[test]
fn the_demo_parses() {
    let m = demo();
    assert_eq!(m.format, Format::S3m);
    assert_eq!(m.tracker_version, 0x1320);
    assert_eq!(
        (m.channels, m.orders.clone(), m.patterns.len()),
        (5, vec![0, 1, 2, 1], 3)
    );
    assert_eq!((m.speed, m.tempo, m.global_volume), (6, 125, 1.0));
    // Channel 5 is AdLib melody channel 1, and has no pan of its own; the
    // others take theirs from the panning table.
    assert_eq!(m.adlib_channels, [None, None, None, None, Some(0)]);
    assert_eq!(m.channel_pan[4], None);
    let expect =
        |nibble: f32| (((nibble * 256.0 + 8.0) / 15.0 / 255.0).min(1.0) * 255.0).round() / 255.0;
    assert_eq!(
        m.channel_pan[..4],
        [
            Some(expect(3.0)),
            Some(expect(12.0)),
            Some(expect(4.0)),
            Some(expect(11.0))
        ]
    );
    assert_eq!(m.stereo_separation, 1.0, "S3M pans play as written");
    let pulse = &m.samples[0];
    assert_eq!(
        (pulse.data.len(), pulse.volume, pulse.c2spd),
        (64, 40, 8363)
    );
    assert_eq!(
        (pulse.loop_kind, pulse.loop_start, pulse.loop_len),
        (LoopKind::Forward, 0, 64)
    );
    assert_eq!(m.samples[1].c2spd, 16726, "tri plays an octave up");
    assert_eq!(m.samples[2].loop_kind, LoopKind::Off);
    let organ = m.samples[4].adlib.expect("instrument 5 is AdLib");
    assert!(organ.melodic);
    assert_eq!(organ.registers[10], 0x06);
    // C-4 on instrument 1 with J37 (arpeggio) opens the song; volumes come
    // out of the volume column XM-style.
    let cell = m.patterns[0].cell(0, 0);
    assert_eq!(
        (cell.note, cell.instrument, cell.effect, cell.param),
        (Note::On(72), 1, 0x0a, 0x37)
    );
    assert_eq!(m.patterns[0].cell(2, 2).volume, 0x10 + 24);
    assert_eq!(m.patterns[0].cell(24, 4).note, Note::Off);
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
    assert_eq!(log.rows.len(), 241);
    // SB0 / SB2 loop order 1's first rows, B03 breaks order 2 off at row 30.
    let order1: Vec<usize> = log
        .rows
        .iter()
        .filter(|r| r.1 == 1)
        .take(12)
        .map(|r| r.2)
        .collect();
    assert_eq!(order1, [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3]);
    assert_eq!(log.rows.iter().rfind(|r| r.1 == 2).unwrap().2, 30);
    // V30 and V40, on each pass of order 1.
    assert_eq!(log.global, [0.75, 1.0, 0.75, 1.0]);
    assert!(audio.iter().all(|s| s.is_finite() && s.abs() < 4.0));
}

#[test]
fn the_adlib_channel_plays_on_the_chip() {
    let module = demo();
    let mut p = Player::new(
        module,
        RATE,
        PlayerOptions {
            looped: false,
            ..Default::default()
        },
    );
    for c in 0..4 {
        p.set_channel_muted(c, true);
    }
    let (log, audio) = play(&mut p, 4.0);
    assert!(
        energy(&audio) > 1.0,
        "the organ is heard: {}",
        energy(&audio)
    );
    // The organ's carrier level goes into operator 3 (0x43), key-ons into
    // 0xB0 with bit 5.
    assert!(log.adlib.iter().any(|w| w.1 == 0x43));
    assert!(log.adlib.iter().any(|w| w.1 == 0xb0 && w.2 & 0x20 != 0));
}

/// Muting the AdLib channel silences the chip too.
#[test]
fn a_muted_adlib_channel_is_silent() {
    let mut p = Player::new(
        demo(),
        RATE,
        PlayerOptions {
            looped: false,
            ..Default::default()
        },
    );
    for c in 0..5 {
        p.set_channel_muted(c, true);
    }
    let (log, audio) = play(&mut p, 4.0);
    assert_eq!(energy(&audio), 0.0);
    assert!(
        !log.adlib.iter().any(|w| w.1 == 0xb0 && w.2 & 0x20 != 0),
        "no key-on"
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
        .chain(
            log.adlib
                .iter()
                .map(|(t, r, v)| format!("{t:.9} opl {r} {v}\n")),
        )
        .collect();
    assert_eq!(
        fnv(text.bytes()),
        5432806834515190559,
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
        10418215041496704840,
        "audio hash changed"
    );
}
