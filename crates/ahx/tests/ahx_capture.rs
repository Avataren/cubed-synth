//! Per-voice waveform capture (`AhxEngine::enable_capture`): it must be
//! invisible to the mix, and what it records must be what the mixer used.
//! The corpus-wide proof that capture leaves every golden bit-exact lives in
//! `ahx_render_golden.rs` (each manifest row runs with capture off and on);
//! these tests cover the toggle, the snapshot contract and the ring.

use cubed_ahx::engine::{AhxEngine, CAPTURE_FRAMES};
use cubed_ahx::format;
use cubed_ahx::player::AhxPlayer;
use std::fs;
use std::path::Path;

const RATE: u32 = 44100;

fn bytes(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/songs")
            .join(name),
    )
    .unwrap()
}

fn engine(name: &str, cap: Option<usize>) -> AhxEngine {
    let song = format::parse(&bytes(name)).unwrap();
    match cap {
        Some(n) => AhxEngine::with_channel_cap(song, RATE, 2, n),
        None => AhxEngine::new(song, RATE, 2),
    }
    .unwrap()
}

/// Renders `frames` frames in blocks of the given sizes (cycled), so block
/// edges land at awkward places relative to ticks and the ring's wrap.
fn render(e: &mut AhxEngine, frames: usize, blocks: &[usize]) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames * 2);
    let mut done = 0;
    let mut k = 0;
    while done < frames {
        let n = blocks[k % blocks.len()].min(frames - done);
        let at = out.len();
        out.resize(at + n * 2, 0);
        e.render_block(&mut out[at..]);
        done += n;
        k += 1;
    }
    out
}

const BLOCKS: [usize; 5] = [128, 1, 333, 882, 4096];

#[test]
fn capture_never_changes_mixed_samples() {
    // Every kind of song: 4-voice AHX, and native 8-voice HVL (ring mod etc.).
    for name in ["karma.ahx", "sunspots.hvl"] {
        let frames = RATE as usize * 6;
        let plain = render(&mut engine(name, None), frames, &BLOCKS);

        let mut on = engine(name, None);
        on.enable_capture(true);
        assert_eq!(
            render(&mut on, frames, &BLOCKS),
            plain,
            "{name}: capture on"
        );

        // Toggled on and off mid-song, at block edges that are not tick edges.
        let mut toggled = engine(name, None);
        let mut got = Vec::new();
        for (i, chunk) in [
            7777usize,
            20000,
            1,
            50000,
            4096,
            frames - 7777 - 20000 - 1 - 50000 - 4096,
        ]
        .iter()
        .enumerate()
        {
            toggled.enable_capture(i % 2 == 1);
            got.extend(render(&mut toggled, *chunk, &BLOCKS));
        }
        assert_eq!(got, plain, "{name}: capture toggled mid-song");
    }
}

#[test]
fn off_by_default_and_nothing_to_read() {
    let mut e = engine("karma.ahx", None);
    assert!(!e.capture_enabled());
    render(&mut e, 4000, &BLOCKS);
    let mut out = [7i16; 64];
    assert_eq!(e.read_channel_snapshot(0, &mut out), 0);
    assert!(
        out.iter().all(|&v| v == 7),
        "an off engine must not touch the buffer"
    );

    e.enable_capture(true);
    assert!(e.capture_enabled());
    e.enable_capture(false);
    assert!(!e.capture_enabled());
    assert_eq!(e.read_channel_snapshot(0, &mut out), 0);
}

#[test]
fn snapshot_argument_edges() {
    let mut e = engine("karma.ahx", None);
    e.enable_capture(true);
    render(&mut e, 5000, &BLOCKS);
    let mut out = [0i16; 16];
    assert_eq!(
        e.read_channel_snapshot(e.channels(), &mut out),
        0,
        "voice out of range"
    );
    assert_eq!(e.read_channel_snapshot(0, &mut []), 0, "empty out");
    // Longer than the ring: capped at one point per captured frame.
    let mut big = vec![0i16; CAPTURE_FRAMES + 100];
    assert_eq!(e.read_channel_snapshot(0, &mut big), CAPTURE_FRAMES);
}

/// One voice only, so the mixed output is a function of that voice's captured
/// contribution `j`: `L = ((j * panl) >> 7) * mixgain >> 8`. With one point per
/// frame the snapshot must reproduce the tail of the left channel exactly.
/// Pan and volume are per-tick, so only the last whole tick is compared (the
/// run ends on a tick boundary, so `pan_mult_left` is that tick's).
#[test]
fn snapshot_is_what_the_mixer_used() {
    let mut e = engine("karma.ahx", Some(1));
    e.enable_capture(true);
    let tick = e.samples_per_tick();
    let out = render(&mut e, tick * 10, &BLOCKS); // 8820 frames: the write index has wrapped
    let mut snap = vec![0i16; CAPTURE_FRAMES];
    assert_eq!(e.read_channel_snapshot(0, &mut snap), CAPTURE_FRAMES);

    let panl = e.voice(0).pan_mult_left;
    let mixgain = 76 * 256 / 100; // AHX_DEFGAIN[2], stereo mode 2
    let tail = &out[out.len() - tick * 2..];
    let snap_tail = &snap[CAPTURE_FRAMES - tick..];
    let mut nonzero = 0;
    for f in 0..tick {
        let j = snap_tail[f] as i32;
        let want = (((j * panl) >> 7) * mixgain) >> 8;
        assert_eq!(tail[f * 2] as i32, want.clamp(-0x8000, 0x7fff), "frame {f}");
        nonzero += (j != 0) as usize;
    }
    assert!(
        nonzero > tick / 4,
        "the voice should be audible here ({nonzero} nonzero)"
    );
    assert!(
        snap.iter().all(|&v| v.abs() <= 8192),
        "full scale is s8 * volume 64"
    );
}

#[test]
fn decimated_snapshot_box_averages_the_full_one() {
    let mut e = engine("karma.ahx", Some(1));
    e.enable_capture(true);
    render(&mut e, 6001, &BLOCKS);
    let mut full = vec![0i16; CAPTURE_FRAMES];
    let mut short = [0i16; 256];
    assert_eq!(e.read_channel_snapshot(0, &mut full), CAPTURE_FRAMES);
    assert_eq!(e.read_channel_snapshot(0, &mut short), 256);
    for (k, &p) in short.iter().enumerate() {
        let sum: i32 = full[k * 8..k * 8 + 8].iter().map(|&v| v as i32).sum();
        assert_eq!(p as i32, sum / 8, "point {k}");
    }
    // A length that does not divide the ring uses the largest whole stride
    // and the most recent frames: 1000 points -> stride 2, window 2000.
    let mut odd = [0i16; 1000];
    assert_eq!(e.read_channel_snapshot(0, &mut odd), 1000);
    let sum: i32 = full[CAPTURE_FRAMES - 2..].iter().map(|&v| v as i32).sum();
    assert_eq!(
        odd[999] as i32,
        sum / 2,
        "newest point covers the newest frames"
    );
}

#[test]
fn a_fresh_capture_reads_silence_and_rewind_clears_it() {
    let mut e = engine("karma.ahx", None);
    e.enable_capture(true);
    let mut snap = vec![1i16; 512];
    assert_eq!(e.read_channel_snapshot(0, &mut snap), 512);
    assert!(snap.iter().all(|&v| v == 0), "nothing rendered yet");

    render(&mut e, 20000, &BLOCKS);
    let any_sound = |e: &AhxEngine, snap: &mut Vec<i16>| {
        (0..e.channels()).any(|v| {
            e.read_channel_snapshot(v, snap);
            snap.iter().any(|&s| s != 0)
        })
    };
    assert!(any_sound(&e, &mut snap), "playing voices must show up");
    assert!(e.init_subsong(0));
    assert!(
        !any_sound(&e, &mut snap),
        "a rewound song starts from a clean ring"
    );
}

#[test]
fn player_exposes_the_same_contract() {
    let mut p = AhxPlayer::new(&bytes("karma.ahx"), RATE, 2).unwrap();
    p.play();
    let mut out = [0i16; 128];
    assert!(!p.capture_enabled());
    assert_eq!(p.read_channel_snapshot(0, &mut out), 0);
    p.enable_capture(true);
    let (mut l, mut r) = ([0f32; 128], [0f32; 128]);
    for _ in 0..200 {
        p.render(&mut l, &mut r);
    }
    let mut heard = 0;
    for v in 0..p.channels() {
        assert_eq!(p.read_channel_snapshot(v, &mut out), 128);
        heard += out.iter().any(|&s| s != 0) as usize;
    }
    assert!(heard >= 1, "at least one voice is sounding after ~0.6 s");
    assert_eq!(p.read_channel_snapshot(p.channels(), &mut out), 0);
}
