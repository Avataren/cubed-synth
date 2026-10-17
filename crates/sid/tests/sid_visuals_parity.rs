//! S4 (`.ai/plan-sid-tracking.md`): the parity fixture for the SID instrument
//! page's drawings (`src/audio/tracker/sid-instrument-visuals.ts`), dumped
//! from the real Rust, as the AHX editor's `ahx-visuals-parity.json` is.
//!
//! The TS helpers are ports of the code below them; this test is where the
//! numbers they must reproduce come from:
//!   - `envelopes`: `Envelope` clocked cycle by cycle (gate on, then off),
//!     its level sampled every frame (19 656 cycles, one PAL frame);
//!   - `cutoff` / `q`: the per-model maps `cutoff_hz_for` / `resonance_q_for`;
//!   - `waves`: one cycle of `waveform_output` (the combined-waveform models
//!     included) at 64 accumulator points;
//!   - `instruments`: the S3 chain song's instruments played by the real
//!     player's preview voice (`SidSongPlayer::preview_note_on`), the voice
//!     and filter registers read after every frame: what the instrument's
//!     tables and vibrato do, frame by frame.
//!
//! `UPDATE_SID_VISUALS_FIXTURE=1 cargo test -p cubed-sid --test
//! sid_visuals_parity` rewrites `tests/fixtures/sid-visuals-parity.json`
//! (another_synth's TS tests read the same file);
//! without it the test fails when the Rust no longer produces the committed
//! file (so a change to the chip or the player cannot leave the drawings
//! silently behind).

use cubed_sid::envelope::Envelope;
use cubed_sid::filter::{cutoff_hz_for, resonance_q_for};
use cubed_sid::waveform::waveform_output;
use cubed_sid::{SidModel, SidSong, SidSongPlayer};
use serde_json::{json, Value};

const CHAIN: &[u8] = include_bytes!("fixtures/sid/s3-chain.asid");
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/sid-visuals-parity.json"
);
/// Chip cycles per player frame: one PAL vertical blank (GT-parity 0925b;
/// was 50 Hz's 19 705).
const CYCLES_PER_FRAME: u32 = cubed_sid::player::PAL_FRAME_CYCLES;

fn model_name(model: SidModel) -> &'static str {
    match model {
        SidModel::Sid8580 => "8580",
        SidModel::Sid6581 => "6581",
    }
}

fn envelopes() -> Value {
    let cases: [(u8, u8, u32, u32); 6] = [
        (0x00, 0xF0, 10, 30),
        (0x09, 0x00, 50, 60),
        (0x49, 0x8A, 40, 120),
        (0xA6, 0x5B, 60, 200),
        (0x22, 0xC4, 25, 80),
        (0xFF, 0xFF, 150, 150),
    ];
    Value::Array(
        cases
            .iter()
            .map(|&(ad, sr, gate_frames, frames)| {
                let mut env = Envelope::default();
                env.set_ad(ad);
                env.set_sr(sr);
                env.set_gate(true);
                let mut levels = Vec::new();
                for f in 0..frames {
                    if f == gate_frames {
                        env.set_gate(false);
                    }
                    for _ in 0..CYCLES_PER_FRAME {
                        env.clock();
                    }
                    levels.push(env.level());
                }
                json!({ "ad": ad, "sr": sr, "gateFrames": gate_frames, "frames": frames, "levels": levels })
            })
            .collect(),
    )
}

fn maps() -> (Value, Value) {
    let regs = [0u16, 1, 100, 256, 511, 700, 1023, 1024, 1500, 2000, 2047];
    let mut cutoff = serde_json::Map::new();
    let mut q = serde_json::Map::new();
    for model in [SidModel::Sid8580, SidModel::Sid6581] {
        cutoff.insert(
            model_name(model).into(),
            json!(regs
                .iter()
                .map(|&r| json!([r, cutoff_hz_for(model, r)]))
                .collect::<Vec<_>>()),
        );
        q.insert(
            model_name(model).into(),
            json!((0u8..16)
                .map(|r| json!([r, resonance_q_for(model, r)]))
                .collect::<Vec<_>>()),
        );
    }
    (Value::Object(cutoff), Value::Object(q))
}

fn waves() -> Value {
    let cases: [(u8, u16); 8] = [
        (0x10, 0),
        (0x20, 0),
        (0x40, 0x800),
        (0x40, 0x200),
        (0x30, 0),
        (0x50, 0x800),
        (0x60, 0x400),
        (0x70, 0x800),
    ];
    let mut out = Vec::new();
    for model in [SidModel::Sid8580, SidModel::Sid6581] {
        for &(control, pw) in &cases {
            let values: Vec<u16> = (0..64u32)
                .map(|i| {
                    let acc = i * (1 << 24) / 64;
                    waveform_output(model, control, acc, 0, pw, 0).unwrap_or(0)
                })
                .collect();
            out.push(json!({ "model": model_name(model), "control": control, "pulseWidth": pw, "values": values }));
        }
    }
    Value::Array(out)
}

fn instruments() -> Value {
    let song = SidSong::parse(CHAIN).expect("the chain file parses");
    let count = song.instruments.len();
    let mut out = Vec::new();
    for (instrument, note) in (1..=count).map(|i| (i, [57u8, 48, 45, 60][(i - 1) % 4])) {
        let mut p = SidSongPlayer::new(song.clone(), 44_100.0).expect("player");
        p.set_preview(true);
        assert!(p.preview_note_on(instrument, note));
        let mut rows = Vec::new();
        for _ in 0..96 {
            p.frame();
            let v = p.chip().voice(0);
            let f = p.chip().filter();
            rows.push(json!([
                v.frequency(),
                v.pulse_width(),
                v.control(),
                f.cutoff_reg(),
                f.resonance(),
                f.mode() >> 4
            ]));
        }
        out.push(json!({ "instrument": instrument, "note": note, "rows": rows }));
    }
    Value::Array(out)
}

#[test]
fn sid_visuals_parity_fixture_is_what_the_rust_does() {
    let (cutoff, q) = maps();
    let fixture = json!({
        "comment": "Dumped by rust-wasm/tests/sid_visuals_parity.rs from the real SID code; regenerate with UPDATE_SID_VISUALS_FIXTURE=1 (see there).",
        "cyclesPerFrame": CYCLES_PER_FRAME,
        "envelopes": envelopes(),
        "cutoff": cutoff,
        "q": q,
        "waves": waves(),
        "instruments": instruments(),
    });
    let text = serde_json::to_string_pretty(&fixture).unwrap() + "\n";
    if std::env::var("UPDATE_SID_VISUALS_FIXTURE").is_ok() {
        std::fs::write(FIXTURE, &text).expect("fixture written");
        return;
    }
    let committed = std::fs::read_to_string(FIXTURE)
        .expect("fixture exists (regenerate with UPDATE_SID_VISUALS_FIXTURE=1)");
    assert!(committed == text, "the Rust no longer produces src/tests/fixtures/sid-visuals-parity.json: regenerate it and re-check the TS ports");
}
