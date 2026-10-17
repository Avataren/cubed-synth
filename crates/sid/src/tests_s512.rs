//! S5.12 tests: the tick-0 remainder of GoatTracker's realtime optimisation.
//!
//! With `optimizerealtime` on (GT2's default, goattrk2.c:55) the tick-N
//! effects block is skipped on tick 0 of every row (gplay.c:728), so the
//! slides (commands 1, 2) and the portamento (3 with a speed) advance on
//! ticks 1..tempo-1 only: tempo - 1 steps per row, the tick-0 frame holding.
//! S5.10 applied the skip to the vibratos; these pin it for 1-3. The `3 00`
//! tie phase (S5.6) is pinned in tests_s5.rs and stays as it was.

use super::player::SidSongPlayer;
use super::song::*;
use super::*;

/// One PAL frame at 44.1 kHz is 879.8 samples (`player::frame_cycles`, GT
/// parity 0925b): a buffer that holds one; `samples_in_next_frame` says how
/// much of it a frame is.
const SPF: usize = 880;

fn t(l: u8, r: u8) -> TableRow {
    TableRow { left: l, right: r }
}

fn row(note: u8, instrument: u8, command: u8, param: u8) -> Row {
    Row {
        note,
        instrument,
        command,
        param,
    }
}

/// A sustaining triangle whose wave table sets the note on frame 1.
fn ins() -> Instrument {
    Instrument {
        name: b"t".to_vec(),
        sustain: 15,
        first_wave: 0x09,
        wave_ptr: 1,
        ..Default::default()
    }
}

/// Channel 1 plays `rows` once at `tempo` with one speed row `speed`;
/// channels 2-3 are silent.
fn song(rows: Vec<Row>, tempo: u8, speed: TableRow) -> SidSong {
    let n = rows.len();
    let list = |pattern: u8| Orderlist {
        entries: vec![OrderEntry {
            pattern,
            transpose: 0,
            repeat: 1,
        }],
        restart: 0,
    };
    let s = SidSong {
        version: SONG_FILE_VERSION,
        model: SidModel::Sid8580,
        channels: 3,
        speed_multiplier: 1,
        tempo,
        name: b"s512".to_vec(),
        author: Vec::new(),
        copyright: Vec::new(),
        subsongs: vec![Subsong {
            orderlists: vec![list(0), list(1), list(1)],
        }],
        patterns: vec![
            Pattern { rows },
            Pattern {
                rows: vec![Row::default(); n],
            },
        ],
        instruments: vec![ins()],
        tables: Tables {
            wave: vec![t(0x41, 0x00), t(0xFF, 0x00)],
            speed: vec![speed],
            ..Default::default()
        },
    };
    SidSong::parse(&s.to_bytes()).expect("parses")
}

/// Voice 1's frequency register after each of `frames` frames.
fn freqs(s: &SidSong, frames: usize) -> Vec<i32> {
    let mut p = SidSongPlayer::new(s.clone(), DEFAULT_SAMPLE_RATE).expect("player builds");
    let mut out = vec![0.0f32; SPF];
    (0..frames)
        .map(|_| {
            let n = p.samples_in_next_frame();
            p.render(&mut out[..n]);
            p.chip().voice(0).frequency() as i32
        })
        .collect()
}

/// Per-frame deltas of rows 1 and 2 (frames 6..18 at tempo 6) from the
/// held note of row 0 (frame 5).
fn slide_deltas(f: &[i32]) -> Vec<i32> {
    f[6..18].iter().map(|v| v - f[5]).collect()
}

const TEMPO: u8 = 6;
/// Speed row (left 0, right 4): a step of 4 register units per tick.
const S: i32 = 4;

#[test]
fn slide_up_skips_tick_0_of_every_row() {
    // Row 0 sets the note (command 0); rows 1-2 are `1 01` with no note.
    let rows = vec![row(49, 1, 0, 0), row(0, 0, 1, 1), row(0, 0, 1, 1)];
    let f = freqs(&song(rows, TEMPO, t(0, 4)), 18);
    // Tick 0 of each row holds; ticks 1..5 step: 5*S per row, 10*S over two.
    let gt: Vec<i32> = vec![0, 1, 2, 3, 4, 5, 5, 6, 7, 8, 9, 10]
        .into_iter()
        .map(|k| k * S)
        .collect();
    assert_eq!(slide_deltas(&f), gt);
    assert_eq!(f[11] - f[5], 5 * S);
    assert_eq!(f[17] - f[5], 10 * S);
}

#[test]
fn slide_down_skips_tick_0_of_every_row() {
    let rows = vec![row(49, 1, 0, 0), row(0, 0, 2, 1), row(0, 0, 2, 1)];
    let f = freqs(&song(rows, TEMPO, t(0, 4)), 18);
    let gt: Vec<i32> = vec![0, 1, 2, 3, 4, 5, 5, 6, 7, 8, 9, 10]
        .into_iter()
        .map(|k| -k * S)
        .collect();
    assert_eq!(slide_deltas(&f), gt);
    assert_eq!(f[11] - f[5], -5 * S);
    assert_eq!(f[17] - f[5], -10 * S);
}

#[test]
fn portamento_to_a_target_skips_tick_0_of_every_row() {
    // Row 1 glides toward C-5 (an octave up, far more than 10 steps away)
    // with `3 01`; row 2 restates `3 01` with no note. The glide row's
    // tick 0 neither triggers nor slides, and neither does row 2's.
    let rows = vec![row(49, 1, 0, 0), row(61, 0, 3, 1), row(0, 0, 3, 1)];
    let f = freqs(&song(rows, TEMPO, t(0, 4)), 18);
    let gt: Vec<i32> = vec![0, 1, 2, 3, 4, 5, 5, 6, 7, 8, 9, 10]
        .into_iter()
        .map(|k| k * S)
        .collect();
    assert_eq!(slide_deltas(&f), gt);
    assert_eq!(f[11] - f[5], 5 * S);
    assert_eq!(f[17] - f[5], 10 * S);
}

#[test]
fn a_wave_table_slide_steps_on_tick_0_too() {
    // The skip is the tick-N effects block's (gplay.c:728); a wave-table
    // command runs in WAVEEXEC before it, on any tick (gplay.c:529-555,
    // player.s:1520-1537). Wave table: the note, then `F1 01` every frame
    // (`FF 02` jumps back). The rows' own frames 6 and 12 step like the rest.
    let rows = vec![row(49, 1, 0, 0), row(0, 0, 0, 0), row(0, 0, 0, 0)];
    let mut s = song(rows, TEMPO, t(0, 4));
    s.tables.wave = vec![t(0x41, 0x00), t(0xF1, 0x01), t(0xFF, 0x02)];
    let f = freqs(&s, 14);
    let steps: Vec<i32> = f[2..14].windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(
        steps,
        vec![S; 11],
        "one step every frame, row starts included: {f:?}"
    );
}
