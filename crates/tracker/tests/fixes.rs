//! Where cubed-tracker plays as the original trackers do and the another_synth
//! DAW does not, each on a module built in code. `daw_quirks` plays the DAW's
//! way, so each case is checked both ways.

use cubed_tracker::{
    Cell, Command, Flags, Format, Instrument, LoopKind, ModFlavor, Module, Note, Observer, Pattern,
    Player, PlayerOptions, Sample,
};

const RATE: f32 = 48_000.0;

/// A looped sine sample at full default volume.
fn sine(volume: u8) -> Sample {
    Sample {
        name: "sine".into(),
        data: (0..64)
            .map(|i| (i as f32 / 64.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect(),
        volume,
        c2spd: 8363,
        loop_kind: LoopKind::Forward,
        loop_start: 0,
        loop_len: 64,
        ..Sample::default()
    }
}

fn module(format: Format, channels: usize, rows: usize) -> Module {
    Module {
        format,
        title: String::new(),
        channels,
        orders: vec![0],
        patterns: vec![Pattern::new(rows, channels)],
        samples: vec![sine(64), sine(32)],
        instruments: Vec::new(),
        speed: 6,
        tempo: 125,
        channel_pan: vec![Some(0.5); channels],
        adlib_channels: vec![None; channels],
        muted: vec![false; channels],
        jump_targets: None,
        global_volume: 1.0,
        adlib_gain: 0.0,
        stereo_separation: 1.0,
        flags: Flags::default(),
        mod_flavor: (format == Format::Mod).then_some(ModFlavor::ProTracker),
        tracker_version: 0x1320,
    }
}

fn cell(note: Option<u8>, instrument: u8, effect: u8, param: u8) -> Cell {
    Cell {
        note: note.map_or(Note::None, Note::On),
        instrument,
        effect,
        param,
        ..Cell::default()
    }
}

#[derive(Default)]
struct Log {
    rows: Vec<(f64, usize, usize)>,
    commands: Vec<(f64, usize, Command)>,
}

impl Observer for Log {
    fn row(&mut self, time: f64, order: usize, row: usize) {
        self.rows.push((time, order, row));
    }
    fn command(&mut self, time: f64, channel: usize, _instrument: u8, command: &Command) {
        self.commands.push((time, channel, *command));
    }
}

fn play(module: Module, quirks: bool, seconds: f64) -> (Log, Vec<f32>) {
    let options = PlayerOptions {
        looped: false,
        filter: false,
        daw_quirks: quirks,
        ..Default::default()
    };
    let mut player = Player::new(module, RATE, options);
    let mut log = Log::default();
    let (mut l, mut r) = ([0.0f32; 128], [0.0f32; 128]);
    let mut audio = Vec::new();
    while player.time() < seconds && player.render_observed(&mut l, &mut r, &mut log) {
        for i in 0..128 {
            audio.push(l[i]);
            audio.push(r[i]);
        }
    }
    (log, audio)
}

fn energy(audio: &[f32], from: f64, to: f64) -> f32 {
    let (a, b) = (
        (from * RATE as f64) as usize * 2,
        (to * RATE as f64) as usize * 2,
    );
    audio[a.min(audio.len())..b.min(audio.len())]
        .iter()
        .map(|s| s * s)
        .sum()
}

/// ST3's A sets the speed whatever its parameter, and ignores A00. The DAW
/// reads A20 and up as a tempo (which the next A then never resets) and A00
/// as speed 1.
#[test]
fn s3m_a_sets_the_speed() {
    let mut m = module(Format::S3m, 1, 4);
    *m.patterns[0].cell_mut(0, 0) = cell(None, 0, 0x01, 0x20);
    *m.patterns[0].cell_mut(1, 0) = cell(None, 0, 0x01, 0x00);
    *m.patterns[0].cell_mut(2, 0) = cell(None, 0, 0x01, 0x06);
    let length = |log: &Log| -> Vec<f64> {
        log.rows
            .windows(2)
            .map(|w| ((w[1].0 - w[0].0) * 1e4).round() / 1e4)
            .collect()
    };
    let (faithful, _) = play(m.clone(), false, 10.0);
    // Speed 32 at 125 BPM is 0.64 s; A00 keeps it; A06 goes back to 0.12 s.
    assert_eq!(length(&faithful), [0.64, 0.64, 0.12]);
    let (daw, _) = play(m, true, 10.0);
    // A tempo of 32 BPM at speed 6, speed 1, then speed 6 still at 32 BPM.
    assert_eq!(length(&daw), [0.4688, 0.0781, 0.4688]);
}

/// ST3's Bxx counts positions in the file's order list, separators (+++)
/// included; a jump to a separator plays on from the next pattern.
#[test]
fn s3m_jumps_count_the_files_positions() {
    let mut m = module(Format::S3m, 1, 2);
    m.patterns = vec![Pattern::new(2, 1), Pattern::new(2, 1), Pattern::new(2, 1)];
    // The file's list: 0, +++, 1, 2; Bxx 02 means pattern 1.
    m.orders = vec![0, 1, 2];
    m.jump_targets = Some(vec![0, 1, 1, 2]);
    *m.patterns[0].cell_mut(0, 0) = cell(None, 0, 0x02, 0x02);
    let orders = |log: &Log| -> Vec<usize> { log.rows.iter().map(|r| r.1).collect() };
    let (faithful, _) = play(m.clone(), false, 10.0);
    assert_eq!(orders(&faithful), [0, 1, 1, 2, 2]);
    let (daw, _) = play(m, true, 10.0);
    assert_eq!(orders(&daw), [0, 2, 2]);
}

/// A note on an empty S3M instrument plays nothing in ST3; the DAW plays the
/// channel's last sample at the new pitch.
#[test]
fn s3m_empty_instruments_are_silent() {
    let mut m = module(Format::S3m, 1, 8);
    m.samples.push(Sample::default());
    *m.patterns[0].cell_mut(0, 0) = cell(Some(60), 1, 0, 0);
    *m.patterns[0].cell_mut(4, 0) = cell(Some(64), 3, 0, 0);
    let (_, faithful) = play(m.clone(), false, 1.0);
    let (_, daw) = play(m, true, 1.0);
    assert!(energy(&faithful, 0.0, 0.4) > 1.0);
    assert!(
        energy(&faithful, 0.55, 0.9) < 1e-6,
        "cut by the empty instrument"
    );
    assert!(energy(&daw, 0.55, 0.9) > 1.0, "the DAW plays on");
}

/// Channels an S3M switches off in its header are not heard.
#[test]
fn s3m_disabled_channels_are_silent() {
    let mut m = module(Format::S3m, 2, 4);
    m.muted = vec![false, true];
    *m.patterns[0].cell_mut(0, 1) = cell(Some(60), 1, 0, 0);
    let (_, faithful) = play(m.clone(), false, 0.4);
    let (_, daw) = play(m, true, 0.4);
    assert!(energy(&faithful, 0.0, 0.4) < 1e-6);
    assert!(energy(&daw, 0.0, 0.4) > 1.0);
}

/// ST3 and FT2 load the default volume on an instrument number with no
/// note; the DAW only with a note.
#[test]
fn a_bare_instrument_number_loads_its_volume() {
    let mut m = module(Format::S3m, 1, 4);
    *m.patterns[0].cell_mut(0, 0) = Cell {
        volume: 0x10 + 10,
        ..cell(Some(60), 1, 0, 0)
    };
    *m.patterns[0].cell_mut(2, 0) = cell(None, 1, 0, 0);
    let volumes = |log: &Log| -> Vec<f64> {
        log.commands
            .iter()
            .filter_map(|c| match c.2 {
                Command::Volume { volume, .. } => Some((volume * 64.0).round()),
                _ => None,
            })
            .collect()
    };
    let (faithful, _) = play(m.clone(), false, 1.0);
    assert_eq!(volumes(&faithful).last(), Some(&64.0));
    let (daw, _) = play(m, true, 1.0);
    assert_eq!(volumes(&daw).last(), Some(&10.0));
}

/// Soundtracker modules load a sample's default volume with its number,
/// as every later tracker does; the DAW skips it there.
#[test]
fn soundtracker_samples_load_their_volume() {
    let mut m = module(Format::Mod, 1, 2);
    m.mod_flavor = Some(ModFlavor::Soundtracker);
    *m.patterns[0].cell_mut(0, 0) = Cell {
        period: 428,
        ..cell(Some(36), 2, 0, 0)
    };
    let first_volume = |log: &Log| {
        log.commands.iter().find_map(|c| match c.2 {
            Command::Volume { volume, .. } => Some((volume * 64.0).round()),
            _ => None,
        })
    };
    let (faithful, _) = play(m.clone(), false, 0.2);
    assert_eq!(first_volume(&faithful), Some(32.0), "sample 2's volume");
    let (daw, _) = play(m, true, 0.2);
    assert_eq!(
        first_volume(&daw),
        Some(64.0),
        "the channel's starting volume"
    );
}

/// An XM note without an instrument number keeps the channel's pan (8xx);
/// the DAW resets it to the sample's on every note.
#[test]
fn xm_notes_without_an_instrument_keep_the_pan() {
    let mut m = module(Format::Xm, 1, 8);
    m.flags.linear_frequency = true;
    m.channel_pan = vec![None];
    m.samples = vec![Sample {
        pan: Some(128),
        ..sine(64)
    }];
    m.instruments = vec![Instrument {
        name: "sine".into(),
        samples: vec![0],
        ..Instrument::default()
    }];
    *m.patterns[0].cell_mut(0, 0) = cell(Some(60), 1, 0x08, 0x00);
    *m.patterns[0].cell_mut(4, 0) = cell(Some(64), 0, 0, 0);
    let right = |audio: &[f32]| -> f32 {
        let start = (0.6 * RATE as f64) as usize * 2;
        audio[start..start + 4800]
            .chunks(2)
            .map(|f| f[1] * f[1])
            .sum()
    };
    let (_, faithful) = play(m.clone(), false, 0.8);
    let (_, daw) = play(m, true, 0.8);
    assert!(right(&faithful) < 1e-6, "still hard left");
    assert!(right(&daw) > 1.0, "back in the centre");
}
