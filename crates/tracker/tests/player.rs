//! The demo module (tests/fixtures/demo.mod, synthetic) through the parser
//! and the player.
//!
//! The command-log golden pins the replayer's semantics, which were checked
//! command for command against the another_synth DAW's TypeScript engine on
//! this module and 31 real ones; the audio golden pins the voices.

use cubed_tracker::{Command, Format, LoopKind, ModFlavor, Note, Observer, Player, PlayerOptions};

const RATE: f32 = 48_000.0;

fn demo() -> cubed_tracker::Module {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/demo.mod"
    ))
    .unwrap();
    cubed_tracker::load(&bytes).unwrap()
}

fn player(looped: bool) -> Player {
    Player::new(
        demo(),
        RATE,
        PlayerOptions {
            looped,
            ..Default::default()
        },
    )
}

#[derive(Default)]
struct Log {
    rows: Vec<(f64, usize, usize)>,
    commands: Vec<(f64, usize, u8, Command)>,
    filter: Vec<bool>,
    restarts: usize,
}

impl Observer for Log {
    fn row(&mut self, time: f64, order: usize, row: usize) {
        self.rows.push((time, order, row));
    }
    fn command(&mut self, time: f64, channel: usize, instrument: u8, command: &Command) {
        self.commands.push((time, channel, instrument, *command));
    }
    fn filter(&mut self, _time: f64, on: bool) {
        self.filter.push(on);
    }
    fn all_notes_off(&mut self, _time: f64) {
        self.restarts += 1;
    }
}

/// Plays `seconds`, logging; returns the log and the rendered audio.
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

#[test]
fn the_demo_parses() {
    let m = demo();
    assert_eq!(m.format, Format::Mod);
    assert_eq!(m.mod_flavor, Some(ModFlavor::ProTracker));
    assert_eq!(m.title, "cubed-tracker demo");
    assert_eq!(
        (m.channels, m.orders.clone(), m.patterns.len()),
        (4, vec![0, 1, 2, 1], 3)
    );
    assert!(
        m.flags.amiga_limits,
        "an M.K. file inside ProTracker's octaves"
    );
    assert!(!m.flags.vblank_timing);
    let square = &m.samples[0];
    assert_eq!(
        (square.name.as_str(), square.data.len(), square.volume),
        ("square", 32, 32)
    );
    assert_eq!(
        (square.loop_kind, square.loop_start, square.loop_len),
        (LoopKind::Forward, 0, 32)
    );
    assert_eq!(m.samples[2].finetune, 3);
    assert_eq!(m.samples[4].finetune, -2);
    assert_eq!(m.samples[3].loop_kind, LoopKind::Off);
    // C-2 on sample 1 with arpeggio 037 opens the song.
    let cell = m.patterns[0].cell(0, 0);
    assert_eq!(
        (cell.period, cell.instrument, cell.effect, cell.param),
        (428, 1, 0, 0x37)
    );
    // Period 428 is ProTracker's C-2, MIDI 36.
    assert_eq!(cell.note, Note::On(36));
    // MOD channels pan hard left, right, right, left, played at 75% width.
    assert_eq!(m.channel_pan, [Some(0.0), Some(1.0), Some(1.0), Some(0.0)]);
    assert_eq!(m.stereo_separation, 0.75);
}

#[test]
fn the_demo_plays_its_orders_and_ends() {
    let mut p = player(false);
    let (log, audio) = play(&mut p, 60.0);
    assert!(p.finished());
    assert_eq!(log.rows.len(), 241);
    // Order 1's rows 0-3 play three times (E62), order 2 breaks off at row
    // 30 with B03 (which beats the D16 beside it) into order 3.
    let order1: Vec<usize> = log
        .rows
        .iter()
        .filter(|r| r.1 == 1)
        .take(12)
        .map(|r| r.2)
        .collect();
    assert_eq!(order1, [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3]);
    let last2 = log.rows.iter().rfind(|r| r.1 == 2).unwrap();
    assert_eq!(last2.2, 30);
    let first3 = log.rows.iter().find(|r| r.1 == 3).unwrap();
    assert_eq!(first3.2, 0);
    // E00 and E01 switch the LED filter, each order 1 plays both.
    assert_eq!(log.filter, [true, false, true, false]);
    // The last row starts 20.28 s in (speeds 5 and 4, tempos 125 and 150,
    // a pattern delay); the song ends with it, then fades out.
    let end = log.rows.last().unwrap().0;
    assert!((end - 20.2833).abs() < 0.001, "last row at {end}");
    assert!(audio.len() / 2 < (20.5 * RATE as f64) as usize);
    // Loud (four full-volume chip voices), but bounded.
    assert!(audio.iter().all(|s| s.is_finite() && s.abs() < 4.0));
}

#[test]
fn a_looped_song_wraps_to_its_start() {
    let mut p = player(true);
    let (log, _) = play(&mut p, 45.0);
    assert!(!p.finished());
    assert_eq!(log.restarts, 2, "two passes ended in 45 s");
    let wraps = log
        .rows
        .windows(2)
        .filter(|w| w[1].1 == 0 && w[0].1 == 3)
        .count();
    assert_eq!(wraps, 2);
}

#[test]
fn seeking_starts_at_an_order_and_row() {
    let mut p = player(false);
    p.seek(2, 8);
    let (log, _) = play(&mut p, 1.0);
    assert_eq!((log.rows[0].1, log.rows[0].2), (2, 8));
    assert_eq!(p.position().order, 2);
    // Order 2 starts at the speed (5, order 0's F05) and tempo (150, order
    // 1's F96) the song reaches there.
    let row = log.rows[1].0 - log.rows[0].0;
    assert!((row - 2.5 / 150.0 * 5.0).abs() < 1e-9, "row length {row}");
}

#[test]
fn a_muted_channel_is_silent() {
    let mut a = player(false);
    let mut b = player(false);
    for c in 0..4 {
        b.set_channel_muted(c, c != 2);
    }
    let (_, all) = play(&mut a, 3.0);
    let (_, drums) = play(&mut b, 3.0);
    let energy = |x: &[f32]| x.iter().map(|s| s * s).sum::<f32>();
    assert!(energy(&drums) > 0.0);
    assert!(energy(&drums) < energy(&all) * 0.8);
}

/// The commands the demo's voices get, by kind, and a hash over all of them.
#[test]
fn command_golden() {
    let mut p = player(false);
    let (log, _) = play(&mut p, 60.0);
    let count = |f: fn(&Command) -> bool| log.commands.iter().filter(|c| f(&c.3)).count();
    let counts = (
        count(|c| matches!(c, Command::NoteOn { .. })),
        count(|c| matches!(c, Command::Pitch { .. })),
        count(|c| matches!(c, Command::Volume { .. })),
        count(|c| matches!(c, Command::Pan { .. })),
        count(|c| matches!(c, Command::Retrigger { .. })),
    );
    assert_eq!(counts, (154, 533, 312, 12, 4));
    let text: String = log
        .commands
        .iter()
        .map(|(t, ch, inst, c)| format!("{:.9} {ch} {inst} {c:?}\n", t))
        .collect();
    assert_eq!(
        fnv(text.bytes()),
        10617340058659009021,
        "command log hash changed"
    );
}

#[test]
fn audio_golden() {
    let mut p = player(false);
    let (_, audio) = play(&mut p, 10.0);
    let hash = fnv(audio.iter().flat_map(|s| s.to_bits().to_le_bytes()));
    assert_eq!(hash, 16244030596330059174, "audio hash changed");
}
