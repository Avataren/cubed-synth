//! The SID player's control hooks (`cubed_sid::SidSongPlayer`: seek by place, loops,
//! mute and solo, auditions, the status, the peaks, the probe, live edits) are called
//! between audio blocks on the audio thread, so none of them may allocate or free.
//! Every block below is checked, and every control is exercised in some block.
//!
//! cargo test -p cubed-rt-check --test sid_controls
//!
//! With `CUBED_RT_TRACE=1` every counted allocation prints a backtrace.

use cubed_rt_check::{allocations_during, trace_allocations, CountingAllocator};
use cubed_sid::edit::SidDoc;
use cubed_sid::song::{Instrument, LiveEdit, Row, TableKind, TableRow};
use cubed_sid::{SidLoop, SidModel, SidProbe, SidSongPlayer};
use serde_json::json;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const RATE: f64 = 48_000.0;
const BLOCK: usize = 128;

/// A song with every kind of thing to control: three voices over two subsongs, orderlists with
/// repeats, a tempo change, instruments with wave, pulse, filter and speed tables.
fn song() -> cubed_sid::SidSong {
    let mut d = SidDoc::new();
    let ops = [
        json!({"op": "set_instrument", "instrument": 1, "decay": 0, "sustain": 15, "release": 6, "vibrato_delay": 4}),
        json!({"op": "add_instrument", "name": "second"}),
        json!({"op": "set_table_row", "table": "speed", "row": 0, "left": 4, "right": 12}),
        json!({"op": "set_table_row", "table": "filter", "row": 0, "left": 0x90, "right": 0xF1}),
        json!({"op": "set_table_row", "table": "filter", "row": 1, "left": 0x00, "right": 0x30}),
        json!({"op": "set_table_row", "table": "filter", "row": 2, "left": 0x20, "right": 0x04}),
        json!({"op": "set_table_row", "table": "filter", "row": 3, "left": 0xFF, "right": 0}),
        json!({"op": "set_instrument", "instrument": 2, "filter_ptr": 1, "speed_ptr": 1}),
    ];
    for op in ops {
        d.apply(&op).unwrap_or_else(|e| panic!("{op}: {e}"));
    }
    for v in 0..3 {
        for (r, note) in [(0, 49), (3, 53), (6, 56), (9, 61), (12, 40)] {
            let instrument = 1 + (r / 3 + v) % 2;
            d.apply(&json!({"op": "set_cell", "pattern": v, "row": r, "cell": [note + v as usize, instrument, 0, 0]})).unwrap();
        }
        d.apply(&json!({"op": "set_pattern_rows", "pattern": v, "rows": 16, "force": true}))
            .unwrap();
        d.apply(&json!({"op": "insert_order", "voice": v, "index": 1, "pattern": v, "transpose": 5, "repeat": 3})).unwrap();
    }
    d.apply(&json!({"op": "set_cell", "pattern": 0, "row": 8, "command": 4, "param": 1}))
        .unwrap();
    d.apply(&json!({"op": "set_cell", "pattern": 1, "row": 8, "command": 15, "param": 5}))
        .unwrap();
    d.apply(&json!({"op": "add_subsong", "rows": 8})).unwrap();
    d.apply(&json!({"op": "set_cell", "pattern": 3, "row": 0, "cell": [60, 1, 0, 0]}))
        .unwrap();
    d.song().clone()
}

#[test]
fn the_sid_controls_do_not_allocate() {
    trace_allocations(std::env::var_os("CUBED_RT_TRACE").is_some());
    let mut player = SidSongPlayer::new(song(), RATE).expect("player builds");
    let probe = SidProbe::new();
    player.attach_probe(probe.clone());
    let (mut a, mut b, mut c) = ([0f32; BLOCK], [0f32; BLOCK], [0f32; BLOCK]);
    let mut out = [0f32; BLOCK];

    // Live edits, built before the blocks that apply them (an edit's instrument carries no name).
    let edits = [
        LiveEdit::Row {
            pattern: 1,
            row: 4,
            cell: Row {
                note: 60,
                instrument: 2,
                command: 0,
                param: 0,
            },
        },
        LiveEdit::Instrument {
            number: 2,
            instrument: Instrument {
                attack: 2,
                decay: 3,
                sustain: 12,
                release: 5,
                first_wave: 0x41,
                gate_timer: 2,
                hard_restart: true,
                wave_ptr: 1,
                pulse_ptr: 1,
                filter_ptr: 1,
                speed_ptr: 1,
                ..Default::default()
            },
        },
        LiveEdit::TableRow {
            table: TableKind::Filter,
            row: 1,
            value: TableRow {
                left: 0x00,
                right: 0x50,
            },
        },
        LiveEdit::TableRow {
            table: TableKind::Wave,
            row: 0,
            value: TableRow {
                left: 0x41,
                right: 0,
            },
        },
        LiveEdit::Model(SidModel::Sid8580),
        LiveEdit::Model(SidModel::Sid6581),
    ];
    let (start, end) = player.song().order_rows(0, 0, 1, 1).unwrap();

    // 20 seconds of blocks; one control per block, every block counted.
    let blocks = 20 * RATE as usize / BLOCK;
    for block in 0..blocks {
        let events = allocations_during(|| {
            match block % 40 {
                1 => {
                    player.seek(0, 0, 1, 4).unwrap();
                }
                3 => player.seek_row(20),
                5 => {
                    player.seek(1, 0, 0, 3).unwrap();
                }
                7 => {
                    player.seek(0, 2, 0, 11).unwrap();
                }
                9 => player.set_mute_solo(0b001, 0),
                10 => player.set_mute_solo(0, 0b100),
                11 => player.set_mute_solo(0, 0),
                13 => player.set_loop(Some(SidLoop {
                    voice: 0,
                    start,
                    end,
                })),
                17 => player.set_loop_rows(Some((4, 9))),
                19 => player.set_loop(None),
                // Loops of one voice alone: one set while the voice is before, inside or past
                // its range (the last one replays the song's start to the range's, in a block
                // of its own, with nothing allocated), one with no such entries, and all cleared.
                14 => assert!(player.set_voice_loop(0, Some((1, 1)))),
                15 => assert!(player.set_voice_loop(2, Some((0, 0)))),
                16 => assert!(!player.set_voice_loop(1, Some((0, 7)))),
                18 => assert!(player.set_voice_loop(1, Some((0, 1)))),
                20 => {
                    for v in 0..3 {
                        assert!(player.set_voice_loop(v, None));
                    }
                }
                21 => {
                    player.set_preview(true);
                    assert!(player.audition_on(1, 2, 40));
                }
                22 => {
                    assert!(player.audition_on(0, 1, 52));
                    assert!(player.audition_on(2, 2, 59));
                }
                24 => {
                    player.audition_off(1);
                    player.audition_off(0);
                    player.audition_off(2);
                }
                26 => {
                    player.set_preview(false);
                    assert!(player.audition_on(2, 1, 44));
                }
                28 => player.audition_off(2),
                30 => {
                    let s = player.status();
                    assert!(s.order[0] < 3);
                    let _ = player.peaks();
                    let _ = probe.status();
                    let _ = probe.levels();
                }
                31..=36 => {
                    let e = &edits[block % 40 - 31];
                    assert!(player.apply_live(e));
                }
                _ => {}
            }
            if block % 2 == 0 {
                player.render(&mut out);
            } else {
                player.render_taps(&mut out, [&mut a, &mut b, &mut c]);
            }
        });
        assert_eq!(
            events,
            0,
            "block {block} (control {}) made {events} allocations or frees",
            block % 40
        );
    }
    // The probe followed it all.
    assert!(probe.status().frames > 1000);
    assert!(probe.status().version > 0);
}
