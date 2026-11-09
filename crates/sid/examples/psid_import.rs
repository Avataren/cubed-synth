//! Imports `.sid` tunes as GoatTracker songs and writes what came out as JSON, in the shape
//! `tests/fixtures/psid/import_oracle.ts` writes for the DAW's own importer, so the two can be
//! compared tune by tune (`tests/fixtures/psid/compare_import.py`).
//!
//! cargo run --release -p cubed-sid --example psid_import -- list.txt out.json [import|start|capture|gt] [max_seconds] [shard shards]
//!
//! `list.txt` has one `.sid` path per line. `import` (the default) imports every subsong, `start`
//! only the start song, `capture` records only the start song's register trace, `gt` only the GoatTracker unpack. `KERNAL_DAW=1` runs the tunes on the DAW's stand-in KERNAL. With `shard n`
//! only every `shards`-th tune from the n-th is done, to run several at once.

use cubed_sid::psid::import::capture::{capture_sid, CaptureOptions};
use cubed_sid::psid::import::{import_file, ImportOptions};
use cubed_sid::psid::PsidFile;
use serde_json::{json, Map, Value};
use std::time::Instant;

fn fnv(bytes: impl IntoIterator<Item = u8>) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in bytes {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: psid_import list.txt out.json [import|start|capture|gt] [max_seconds] [shard shards]");
        std::process::exit(2);
    }
    let list = std::fs::read_to_string(&args[0]).expect("list");
    let mode = args.get(2).map(String::as_str).unwrap_or("import");
    let max_seconds: Option<f64> = args.get(3).and_then(|s| s.parse().ok());
    let shard: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let shards: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut results = Map::new();
    for (n, path) in list.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        if n % shards != shard {
            continue;
        }
        let t0 = Instant::now();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                results.insert(
                    path.to_string(),
                    json!({"ok": false, "reason": e.to_string()}),
                );
                continue;
            }
        };
        let file = match PsidFile::parse(&bytes) {
            Ok(f) => f,
            Err(e) => {
                results.insert(
                    path.to_string(),
                    json!({"ok": false, "reason": e.to_string()}),
                );
                continue;
            }
        };
        let r = std::panic::catch_unwind(|| run(&file, mode, max_seconds));
        let mut v = match r {
            Ok(v) => v,
            Err(_) => json!({"ok": false, "threw": "panic"}),
        };
        v["ms"] = json!(t0.elapsed().as_secs_f64() * 1000.0);
        results.insert(path.to_string(), v);
    }
    std::fs::write(
        &args[1],
        serde_json::to_string(&Value::Object(results)).unwrap(),
    )
    .unwrap();
}

fn run(file: &PsidFile, mode: &str, max_seconds: Option<f64>) -> Value {
    if mode == "gtfid" {
        // What the unpacked song scores against the file itself (how the import decides to keep it).
        let Ok(u) = cubed_sid::psid::import::gt_unpack::unpack_gt_sid(file) else {
            return json!({"ok": false, "reason": "not a GoatTracker tune"});
        };
        let trace = capture_sid(
            file,
            &CaptureOptions {
                subsong: 0,
                max_ticks: None,
                max_seconds: Some(120.0),
                cancel: None,
            },
        );
        let Ok(trace) = trace else {
            return json!({"ok": false, "reason": "capture failed"});
        };
        return match cubed_sid::psid::import::fidelity::measure_fidelity(
            &trace,
            &u.song,
            0,
            3000,
            cubed_sid::psid::Kernal::default(),
        ) {
            Err(e) => json!({"ok": false, "reason": e}),
            Ok(f) => json!({"ok": true, "score": f.score, "filter": f.filter, "offset": f.offset,
                "voiceOffsets": f.voice_offsets, "frames": f.frames, "ticks": trace.ticks,
                "voices": f.voices.iter().map(|v| json!([v.gate, v.onsets, v.pitch, v.wave, v.envelope, v.level, v.pulse, v.score])).collect::<Vec<_>>()}),
        };
    }
    if mode == "gt" {
        return match cubed_sid::psid::import::gt_unpack::unpack_gt_sid(file) {
            Err(reason) => json!({"ok": false, "reason": reason}),
            Ok(u) => {
                json!({"ok": true, "exact": u.exact, "notes": u.notes, "asid": base64(&u.song.to_bytes())})
            }
        };
    }
    if mode == "capture" {
        let c = capture_sid(
            file,
            &CaptureOptions {
                subsong: file.start_subsong(),
                max_ticks: None,
                max_seconds,
                cancel: None,
            },
        );
        return match c {
            Err(reason) => json!({"ok": false, "reason": reason}),
            Ok(t) => json!({
                "ok": true,
                "mode": format!("{:?}", t.mode).to_lowercase(),
                "ticks": t.ticks,
                "tickCycles": t.tick_cycles,
                "loopTick": t.loop_tick,
                "end": format!("{:?}", t.end).to_lowercase(),
                "digi": t.digi,
                "extra": t.extra_sid_writes,
                "writesPerTick": t.writes_per_tick,
                "regs": fnv(t.regs.iter().copied()),
                "events": fnv(t.voice_events.iter().copied()),
            }),
        };
    }
    let opts = ImportOptions {
        subsongs: if mode == "start" {
            Some(vec![file.start_subsong()])
        } else {
            None
        },
        max_seconds,
        kernal: if std::env::var_os("KERNAL_DAW").is_some() {
            cubed_sid::psid::Kernal::Daw
        } else {
            cubed_sid::psid::Kernal::default()
        },
        ..Default::default()
    };
    match import_file(file, &opts) {
        Err(reason) => json!({"ok": false, "reason": reason}),
        Ok(r) => {
            let fid = r.report.fidelity.as_ref().map(|f| {
                json!({
                    "score": f.score, "filter": f.filter, "frames": f.frames, "offset": f.offset,
                    "voiceOffsets": f.voice_offsets,
                    "voices": f.voices.iter().map(|v| json!({
                        "gate": v.gate, "onsets": v.onsets, "pitch": v.pitch, "wave": v.wave,
                        "envelope": v.envelope, "level": v.level, "pulse": v.pulse, "score": v.score,
                    })).collect::<Vec<_>>(),
                })
            });
            let reports: Vec<Value> = r
                .report
                .subsongs
                .iter()
                .map(|s| {
                    json!({
                        "subsong": s.subsong,
                        "gtSubsong": s.gt_subsong,
                        "reason": s.reason,
                        "frames": s.frames,
                        "notes": s.notes,
                        "folded": s.folded,
                        "loop": s.loop_kind.map(|k| k.name()),
                        "rows": s.rows,
                        "cutRows": s.cut_rows,
                        "grid": s.grid.as_ref().map(|g| json!({
                            "starts": g.starts.len(),
                            "startsHash": fnv(g.starts.iter().map(|&x| (x + 1) as u8)),
                            "first": g.starts[0], "delays": g.delays, "coverage": g.coverage, "lead": g.lead,
                        })),
                        "trace": s.trace.as_ref().map(|t| json!({
                            "mode": format!("{:?}", t.mode).to_lowercase(),
                            "ticks": t.ticks, "tickCycles": t.tick_cycles, "loopTick": t.loop_tick,
                            "end": format!("{:?}", t.end).to_lowercase(), "digi": t.digi,
                            "regs": fnv(t.regs.iter().copied()),
                        })),
                    })
                })
                .collect();
            json!({
                "ok": true,
                "method": if r.report.used_gt_unpack { "unpacked" } else { "transcribed" },
                "exact": r.report.exact.unwrap_or(false),
                "notes": r.report.notes,
                "fidelity": fid,
                "reports": reports,
                "asid": base64(&r.song.to_bytes()),
                "warnings": r.report.warnings,
            })
        }
    }
}
