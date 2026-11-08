//! The PSID player (`cubed_sid::psid::PsidPlayer`) runs a tune's 6510 code inside
//! `render`, so everything it does while rendering is checked here: no allocation
//! or free in any 128-frame block, for tunes driven by the host's play calls, by
//! their own CIA and raster interrupts, with two chips, and for the worst writer
//! there is (a main loop that stores to the SID as fast as the CPU can).
//!
//! cargo test -p cubed-rt-check --test psid_alloc
//!
//! With `CUBED_RT_TRACE=1` every counted allocation prints a backtrace.

use cubed_rt_check::{allocations_during, trace_allocations, CountingAllocator};
use cubed_sid::psid::{PsidOptions, PsidPlayer};
use cubed_sid::SidModel;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const RATE: f64 = 48_000.0;
const BLOCK: usize = 128;
const BLOCKS_PER_SECOND: usize = RATE as usize / BLOCK;

const ARP: &[u8] = include_bytes!("../../sid/tests/fixtures/psid/arp.sid");
const IRQ: &[u8] = include_bytes!("../../sid/tests/fixtures/psid/irq.sid");
const RASTER: &[u8] = include_bytes!("../../sid/tests/fixtures/psid/raster.sid");
const TWO: &[u8] = include_bytes!("../../sid/tests/fixtures/psid/two.sid");
const CIA2X: &[u8] = include_bytes!("../../sid/tests/fixtures/psid/cia2x.sid");

/// An RSID whose init never returns: a main loop storing to three SID registers as fast as it can.
fn flood() -> Vec<u8> {
    let code: [u8; 15] = [
        0xa9, 0x01, // LDA #1
        0x8d, 0x00, 0xd4, // loop: STA $D400
        0x8d, 0x01, 0xd4, // STA $D401
        0x8d, 0x02, 0xd4, // STA $D402
        0x4c, 0x02, 0x10, // JMP loop
        0x60,
    ];
    let mut b = vec![0u8; 0x7c];
    b[..4].copy_from_slice(b"RSID");
    b[5] = 2;
    b[7] = 0x7c;
    b[8..10].copy_from_slice(&0x1000u16.to_be_bytes());
    b[10..12].copy_from_slice(&0x1000u16.to_be_bytes());
    b[15] = 1;
    b[17] = 1;
    b[0x77] = 0x14;
    b.extend_from_slice(&code);
    b
}

/// Calls `render(block)` for a second of warm-up, then for `seconds` more,
/// failing at the first block that allocates or frees.
fn assert_no_alloc(name: &str, seconds: usize, mut render: impl FnMut(usize)) {
    trace_allocations(std::env::var_os("CUBED_RT_TRACE").is_some());
    for block in 0..BLOCKS_PER_SECOND {
        render(block);
    }
    for block in BLOCKS_PER_SECOND..BLOCKS_PER_SECOND * (1 + seconds) {
        let events = allocations_during(|| render(block));
        assert_eq!(
            events, 0,
            "{name}: block {block} made {events} allocations or frees"
        );
    }
}

#[test]
fn psid_player_renders_without_allocating() {
    let flood = flood();
    for (name, bytes) in [
        ("arp", ARP),
        ("irq", IRQ),
        ("raster", RASTER),
        ("two sids", TWO),
        ("cia 2x", CIA2X),
        ("flood", &flood[..]),
    ] {
        for model in [SidModel::Sid6581, SidModel::Sid8580] {
            let mut p = PsidPlayer::new(
                bytes,
                RATE,
                PsidOptions {
                    subsong: None,
                    model: Some(model),
                },
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut out = [0f32; BLOCK];
            let (mut a, mut b, mut c) = ([0f32; BLOCK], [0f32; BLOCK], [0f32; BLOCK]);
            let mut right = [0f32; BLOCK];
            assert_no_alloc(&format!("{name} {model:?}"), 10, |block| match block % 3 {
                0 => p.render(&mut out),
                1 => p.render_taps(&mut out, [&mut a, &mut b, &mut c]),
                _ => p.render_stereo(&mut out, &mut right),
            });
            assert!(out.iter().all(|x| x.is_finite()), "{name}: finite samples");
            assert_eq!(p.ended(), None, "{name} keeps playing");
        }
    }
}

#[test]
fn psid_player_controls_do_not_allocate() {
    let mut p = PsidPlayer::new(ARP, RATE, PsidOptions::default()).unwrap();
    let mut out = [0f32; BLOCK];
    assert_no_alloc("controls", 10, |block| {
        match block % 8 {
            1 => p.set_mute_solo(1 << (block % 3), 0),
            2 => p.set_mute_solo(0, 0),
            3 => p.set_gain(0.5),
            4 => p.set_gain(1.0),
            5 => p.set_model(if block % 2 == 0 {
                SidModel::Sid8580
            } else {
                SidModel::Sid6581
            }),
            6 => {
                let _ = (
                    p.position_seconds(),
                    p.frames(),
                    p.ended(),
                    p.subsong(),
                    p.sid_count(),
                );
            }
            _ => {}
        }
        p.render(&mut out);
    });
}
