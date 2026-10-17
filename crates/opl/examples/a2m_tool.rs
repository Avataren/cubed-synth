//! Developer tool for the A2M player (plan-opl.md, O7).
//!   cargo run -p cubed-opl --example a2m_tool -- dump <file.a2m> [pattern...]
use cubed_opl::a2::engine::A2Engine;
use cubed_opl::a2::{parse, A2mSong};

/// AdPlug's writes per tick: index 0 is the rewind, then one per update().
fn oracle_trace(file: &str, ticks: usize) -> Vec<Vec<(u16, u8)>> {
    let bin = std::env::var("A2M_ORACLE").unwrap_or_else(|_| "trace-oracle".into());
    let out = std::process::Command::new(bin)
        .args(["trace", file, &ticks.to_string()])
        .env("A2M_PLAYER", "v2")
        .output()
        .expect("run trace-oracle");
    let mut ticks_out: Vec<Vec<(u16, u8)>> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[0] {
            "T" => ticks_out.push(Vec::new()),
            "W" if f.len() == 4 => {
                let chip: u16 = f[1].parse().unwrap();
                let reg = u16::from_str_radix(f[2], 16).unwrap() | chip << 8;
                let val = u8::from_str_radix(f[3], 16).unwrap();
                ticks_out.last_mut().unwrap().push((reg, val));
            }
            _ => {}
        }
    }
    ticks_out
}

fn fmt_writes(w: &[(u16, u8)]) -> String {
    w.iter()
        .map(|(r, v)| format!("{r:03x}={v:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compares the engine with the oracle tick by tick. Returns the number of
/// ticks that matched before the first difference.
fn compare(file: &str, ticks: usize, show: usize, state_only: bool) -> usize {
    let song = parse(&std::fs::read(file).unwrap()).unwrap();
    let want = oracle_trace(file, ticks);
    let mut engine = A2Engine::new(song);
    engine.adplug_quirks = true;
    let mut regs_a = [0u8; 512];
    let mut regs_b = [0u8; 512];
    let mut shown = 0;
    let mut first_bad = None;
    for (i, w) in want.iter().enumerate() {
        let mut got: Vec<(u16, u8)> = Vec::new();
        let pos = engine.position();
        if i == 0 {
            engine.reset(&mut got);
        } else {
            engine.update(&mut got);
        }
        for &(r, v) in w {
            regs_a[r as usize & 511] = v;
        }
        for &(r, v) in &got {
            regs_b[r as usize & 511] = v;
        }
        let same = if state_only {
            regs_a == regs_b
        } else {
            *w == got
        };
        if !same {
            first_bad.get_or_insert(i);
            if shown < show {
                println!(
                    "tick {} (order {} pattern {} row {}):",
                    i as i64 - 1,
                    pos.0,
                    pos.1,
                    pos.2
                );
                let same = w.iter().zip(got.iter()).take_while(|(a, b)| a == b).count();
                let from = same.saturating_sub(4);
                let end = |v: &[(u16, u8)]| (same + 24).min(v.len());
                println!("  ({same} writes agree; showing from {from})");
                println!("  want {}", fmt_writes(&w[from..end(w)]));
                println!("  got  {}", fmt_writes(&got[from..end(&got)]));
                shown += 1;
            }
            if state_only {
                regs_b = regs_a;
            }
        }
    }
    let n = first_bad.unwrap_or(want.len());
    println!(
        "{file}: {} of {} ticks match before the first difference",
        n,
        want.len()
    );
    n
}

fn dump(song: &A2mSong, pats: &[usize]) {
    println!(
        "v{} tempo {} speed {} flags {:08b} len {} tracks {} speedup {} 4op {:06b} rpb {:?} tfine {:?}",
        song.version, song.tempo, song.speed, song.flags, song.pattern_len, song.tracks,
        song.macro_speedup, song.four_op_tracks, song.rows_per_beat, song.tempo_finetune
    );
    println!("locks {:02x?}", song.lock_flags);
    println!("order {:02x?}", &song.order);
    for (i, ins) in song.instruments.iter().enumerate() {
        if ins.fm.iter().any(|&b| b != 0) {
            print!(
                "ins {:3} fm {:02x?} pan {} fine {} type {}",
                i + 1,
                ins.fm,
                ins.panning,
                ins.finetune,
                ins.voice_type
            );
            if let Some(m) = song.fm_macros.get(i) {
                print!(
                    " | macro len {} loop {}+{} ko {} arp {} vib {}",
                    m.length,
                    m.loop_begin,
                    m.loop_length,
                    m.keyoff_pos,
                    m.arpeggio_table,
                    m.vibrato_table
                );
            }
            println!();
        }
    }
    for &p in pats {
        let pat = &song.patterns[p];
        println!("pattern {p}");
        for r in 0..pat.rows.min(song.pattern_len as usize) {
            let mut line = format!("{r:3} |");
            for c in 0..pat.channels {
                let cell = pat.cell(r, c);
                if !(cell.note == 0 && cell.instrument == 0 && cell.effects == [(0, 0); 2]) {
                    line += &format!(
                        " c{c}:{:02x} {:02x} {:02x}{:02x} {:02x}{:02x}|",
                        cell.note,
                        cell.instrument,
                        cell.effects[0].0,
                        cell.effects[0].1,
                        cell.effects[1].0,
                        cell.effects[1].1
                    );
                }
            }
            if line.len() > 5 {
                println!("{line}");
            }
        }
    }
}

fn write_wav(path: &str, rate: u32, pcm: &[u8]) {
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(pcm);
    std::fs::write(path, out).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("dump") => {
            let song = parse(&std::fs::read(&args[2]).unwrap()).unwrap();
            let pats: Vec<usize> = args[3..].iter().map(|s| s.parse().unwrap()).collect();
            dump(&song, &pats);
        }
        Some("info") => {
            for f in &args[2..] {
                let song = parse(&std::fs::read(f).unwrap()).unwrap();
                println!(
                    "{}\tv{}\ttempo {}\tspeed {}\tflags {:08b}\tspeedup {}\trpb {:?}\ttfine {:?}",
                    f,
                    song.version,
                    song.tempo,
                    song.speed,
                    song.flags,
                    song.macro_speedup,
                    song.rows_per_beat,
                    song.tempo_finetune
                );
            }
        }
        Some("effects") => {
            // version-group, effect, subnibble for extended -> file count
            let mut counts: std::collections::BTreeMap<(u8, u8, u8), usize> = Default::default();
            for f in &args[2..] {
                let song = parse(&std::fs::read(f).unwrap()).unwrap();
                let group = if song.version < 5 {
                    1
                } else if song.version < 9 {
                    5
                } else {
                    9
                };
                let mut seen = std::collections::BTreeSet::new();
                for &o in song.order.iter() {
                    if (o as usize) < song.patterns.len() {
                        let pat = &song.patterns[o as usize];
                        for cell in &pat.cells {
                            for &(e, d) in &cell.effects {
                                if e == 0 && d == 0 {
                                    continue;
                                }
                                let sub = if (group == 9 && matches!(e, 0x23 | 0x24 | 0x29))
                                    || (group == 5 && e == 0x23)
                                    || (group == 1 && e == 0x0f)
                                {
                                    d >> 4
                                } else {
                                    0xff
                                };
                                let sub = if group == 9 && e == 0x23 && (sub == 0xe || sub == 0xf) {
                                    d
                                } else {
                                    sub
                                };
                                seen.insert((group, e, sub));
                            }
                        }
                    }
                }
                for k in seen {
                    *counts.entry(k).or_default() += 1;
                }
            }
            for ((g, e, s), n) in counts {
                println!("v{g}+\t{e:02x}\t{s:02x}\t{n}");
            }
        }
        Some("cmp") => {
            let ticks = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(2000);
            let show = args.get(4).map(|s| s.parse().unwrap()).unwrap_or(3);
            let state = std::env::var("A2M_STATE").is_ok();
            compare(&args[2], ticks, show, state);
        }
        Some("gate") | Some("probegate") => {
            // gate <file> <cap>: AdPlug's state hashes until its end flag
            // (or <cap> ticks); probegate <file> <ticks>: a fixed length,
            // with the song's sparse form. Lines for oracle/*.tsv.
            let file = &args[2];
            let cap: usize = args[3].parse().unwrap();
            let probe = args[1] == "probegate";
            let song = parse(&std::fs::read(file).unwrap()).unwrap();
            let bin = std::env::var("A2M_ORACLE").unwrap_or_else(|_| "trace-oracle".into());
            let out = std::process::Command::new(bin)
                .args(["trace", file, &(cap + 1).to_string()])
                .env("A2M_PLAYER", "v2")
                .output()
                .expect("run trace-oracle");
            let mut h = cubed_opl::a2::gate::StateHasher::default();
            let mut ticks = 0usize;
            let mut started = false;
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let f: Vec<&str> = line.split_whitespace().collect();
                match f[0] {
                    "T" => {
                        if started {
                            h.end_tick();
                        }
                        let tick: i64 = f[1].parse().unwrap();
                        let ended = f[7] == "1";
                        if tick >= 0 && ((!probe && ended) || tick as usize >= cap) {
                            break;
                        }
                        if tick >= 0 {
                            ticks = tick as usize + 1;
                        }
                        started = true;
                    }
                    "W" if f.len() == 4 => {
                        let chip: u16 = f[1].parse().unwrap();
                        h.write(
                            u16::from_str_radix(f[2], 16).unwrap() | chip << 8,
                            u8::from_str_radix(f[3], 16).unwrap(),
                        );
                    }
                    _ => {}
                }
            }
            let hashes: Vec<String> = h.finish().iter().map(|x| format!("{x:016x}")).collect();
            if probe {
                let enc = cubed_opl::a2::gate::encode(&song);
                let mut back = cubed_opl::a2::gate::decode(&enc);
                let mut want = song.clone();
                for s in [&mut back, &mut want] {
                    s.name.clear();
                    s.composer.clear();
                    s.pattern_names.clear();
                    s.instruments.iter_mut().for_each(|i| i.name.clear());
                }
                assert!(back == want, "sparse form does not round-trip");
                println!("{ticks}\t{enc}\t{}", hashes.join(","));
            } else {
                println!("{ticks}\t{}", hashes.join(","));
            }
        }
        Some("macros") => {
            let song = parse(&std::fs::read(&args[2]).unwrap()).unwrap();
            for (i, m) in song.fm_macros.iter().enumerate() {
                if m.length == 0 && m.arpeggio_table == 0 && m.vibrato_table == 0 {
                    continue;
                }
                println!(
                    "ins {} len {} loop {}+{} ko {} arp {} vib {}",
                    i + 1,
                    m.length,
                    m.loop_begin,
                    m.loop_length,
                    m.keyoff_pos,
                    m.arpeggio_table,
                    m.vibrato_table
                );
                for (k, st) in m.steps.iter().enumerate().take(m.length as usize) {
                    println!(
                        "  {:3} fm {:02x?} slide {} pan {} dur {}",
                        k + 1,
                        st.fm,
                        st.freq_slide,
                        st.panning,
                        st.duration
                    );
                }
                if let Some(d) = song.disabled_fm_columns.get(i) {
                    if d.iter().any(|&b| b != 0) {
                        println!("  disabled {:?}", d);
                    }
                }
            }
            for (i, a) in song.arpeggio_macros.iter().enumerate() {
                if a.length != 0 {
                    println!(
                        "arp {} len {} speed {} loop {}+{} ko {} data {:?}",
                        i + 1,
                        a.length,
                        a.speed,
                        a.loop_begin,
                        a.loop_length,
                        a.keyoff_pos,
                        &a.data[..a.length as usize]
                    );
                }
            }
            for (i, a) in song.vibrato_macros.iter().enumerate() {
                if a.length != 0 {
                    println!(
                        "vib {} len {} speed {} delay {} loop {}+{} ko {} data {:?}",
                        i + 1,
                        a.length,
                        a.speed,
                        a.delay,
                        a.loop_begin,
                        a.loop_length,
                        a.keyoff_pos,
                        &a.data[..a.length as usize]
                    );
                }
            }
        }
        Some("macrotl") => {
            // step-1 TL vs instrument TL, for macros whose TL columns are enabled
            for f in &args[2..] {
                let song = parse(&std::fs::read(f).unwrap()).unwrap();
                for (i, m) in song.fm_macros.iter().enumerate() {
                    if m.length == 0 {
                        continue;
                    }
                    let dis = song.disabled_fm_columns.get(i).copied().unwrap_or([0; 28]);
                    let ins = &song.instruments[i];
                    let st = &m.steps[0];
                    if dis[5] == 0 {
                        println!("mod ins {:2} step {:2}", ins.fm[2] & 0x3f, st.fm[2] & 0x3f);
                    }
                    if dis[17] == 0 {
                        println!("car ins {:2} step {:2}", ins.fm[3] & 0x3f, st.fm[3] & 0x3f);
                    }
                }
            }
        }
        Some("features") => {
            for f in &args[2..] {
                let song = parse(&std::fs::read(f).unwrap()).unwrap();
                let lock4 = song.lock_flags.iter().any(|&l| l & 0x40 != 0);
                let pairs = song.four_op_instruments.first().copied().unwrap_or(0);
                println!(
                    "{}\tv{}\t4op {:06b}\tins-pairs {}\tlock4 {}\tperc {}",
                    f.rsplit("/a2m/").next().unwrap(),
                    song.version,
                    song.four_op_tracks,
                    pairs,
                    lock4,
                    song.flags & 0x40 != 0
                );
            }
        }
        Some("fx4op") => {
            // effects (and extended subcommands) used on 4-op tracks
            for f in &args[2..] {
                let song = parse(&std::fs::read(f).unwrap()).unwrap();
                const PAIRS: [(usize, usize); 6] =
                    [(0, 1), (2, 3), (4, 5), (9, 10), (11, 12), (13, 14)];
                let tracks: Vec<usize> = PAIRS
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| song.four_op_tracks >> i & 1 != 0)
                    .flat_map(|(_, &(a, b))| [a, b])
                    .collect();
                if tracks.is_empty() {
                    continue;
                }
                let mut seen = std::collections::BTreeSet::new();
                for &o in song.order.iter() {
                    if let Some(pat) = song.patterns.get(o as usize) {
                        for r in 0..pat.rows {
                            for &t in &tracks {
                                if t >= pat.channels {
                                    continue;
                                }
                                for &(e, d) in &pat.cell(r, t).effects {
                                    if (e, d) != (0, 0) {
                                        let sub = if matches!(e, 0x23 | 0x24 | 0x29) {
                                            d >> 4
                                        } else {
                                            0xff
                                        };
                                        seen.insert((e, sub));
                                    }
                                }
                            }
                        }
                    }
                }
                let lock4 = tracks
                    .iter()
                    .any(|&t| song.lock_flags.get(t).copied().unwrap_or(0) & 0x40 != 0);
                println!(
                    "{}\tlock4 {}\tflags {:08b}\t{}",
                    f.rsplit("/a2m/").next().unwrap(),
                    lock4,
                    song.flags,
                    seen.iter()
                        .map(|(e, s)| if *s == 0xff {
                            format!("{e:02x}")
                        } else {
                            format!("{e:02x}.{s:x}")
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        }
        Some("demometa") => {
            // demometa <root> <rel>...: what the demo browser lists for each
            // module the player takes (title, composer, tracks, version), as
            // JSON keyed by path; refused ones are left out and named on stderr.
            // Read by scripts/build-demo-manifest.mjs (public/demos/a2m/meta.json).
            use cubed_opl::a2::model::cp437;
            let root = std::path::Path::new(&args[2]);
            let json = |t: &str| {
                let mut o = String::from("\"");
                for ch in t.trim().chars() {
                    match ch {
                        '"' => o += "\\\"",
                        '\\' => o += "\\\\",
                        c if (c as u32) < 0x20 => o += &format!("\\u{:04x}", c as u32),
                        c => o.push(c),
                    }
                }
                o + "\""
            };
            let mut rows = Vec::new();
            for rel in &args[3..] {
                let song = parse(&std::fs::read(root.join(rel)).unwrap()).unwrap();
                if let Some(why) = A2Engine::refusal(&song) {
                    eprintln!("skip {rel}: {why}");
                    continue;
                }
                let e = A2Engine::new(song.clone());
                rows.push(format!(
                    "  {}: {{ \"title\": {}, \"composer\": {}, \"tracks\": {}, \"version\": {} }}",
                    json(rel),
                    json(&cp437(&song.name)),
                    json(&cp437(&song.composer)),
                    e.track_count(),
                    song.version
                ));
            }
            println!("{{\n{}\n}}", rows.join(",\n"));
        }
        Some("writes") => {
            // writes <file> <ticks> [quirks]: the engine's register writes in
            // trace-oracle's replay form: "T <refresh>" before each tick
            // (the rate the tick was reached at), then "W <reg> <val>" (hex);
            // the reset's writes come first, under "T -1".
            let song = parse(&std::fs::read(&args[2]).unwrap()).unwrap();
            let ticks: usize = args[3].parse().unwrap();
            let mut e = A2Engine::new(song);
            e.adplug_quirks = args.get(4).is_some_and(|q| q == "quirks");
            let mut out = String::from("T -1\n");
            let mut w: Vec<(u16, u8)> = Vec::new();
            e.reset(&mut w);
            for t in 0..=ticks {
                for (r, v) in w.drain(..) {
                    out += &format!("W {r:03x} {v:02x}\n");
                }
                if t == ticks {
                    break;
                }
                out += &format!("T {:.6}\n", e.refresh());
                e.update(&mut w);
            }
            print!("{out}");
        }
        Some("replaychip") => {
            // replaychip <writes.txt> <seconds> <out.wav>: `writes` output
            // through this crate's Chip with trace-oracle replay's loop
            // (samples, then the tick), at the native rate: the chip alone.
            use cubed_opl::chip::{Chip, NATIVE_RATE};
            let text = std::fs::read_to_string(&args[2]).unwrap();
            let seconds: f64 = args[3].parse().unwrap();
            let total = (seconds * NATIVE_RATE) as usize;
            // REPLAY_SPACING=n: each write lands n samples after the one
            // before it (a write queue), instead of all at the tick's instant.
            let spacing: usize = std::env::var("REPLAY_SPACING").map_or(0, |v| v.parse().unwrap());
            let mut queue: std::collections::VecDeque<(u16, u8)> = Default::default();
            let mut wait = 0usize;
            let mut chip = Chip::new();
            let mut pcm: Vec<u8> = Vec::new();
            let (mut pending, mut done) = (0f64, 0usize);
            for line in text.lines() {
                if done >= total {
                    break;
                }
                let mut f = line.split(' ');
                match f.next() {
                    Some("W") => {
                        let r = u16::from_str_radix(f.next().unwrap(), 16).unwrap();
                        let v = u8::from_str_radix(f.next().unwrap(), 16).unwrap();
                        if spacing == 0 {
                            chip.write(r, v);
                        } else {
                            queue.push_back((r, v));
                        }
                    }
                    Some("T") => {
                        let refresh: f64 = f.next().unwrap().parse().unwrap();
                        if refresh < 0.0 {
                            continue;
                        }
                        pending += NATIVE_RATE / refresh;
                        let n = (pending as usize).min(total - done);
                        pending -= pending.floor();
                        for _ in 0..n {
                            if spacing > 0 {
                                if wait == 0 {
                                    if let Some((r, v)) = queue.pop_front() {
                                        chip.write(r, v);
                                        wait = spacing;
                                    }
                                }
                                wait = wait.saturating_sub(1);
                            }
                            let (l, r) = chip.clock_sample();
                            pcm.extend_from_slice(&l.to_le_bytes());
                            pcm.extend_from_slice(&r.to_le_bytes());
                        }
                        done += n;
                    }
                    _ => {}
                }
            }
            write_wav(&args[4], 49716, &pcm);
        }
        Some("render") => {
            // render <file> <seconds> <out.wav> [rate] [quirks]: A2Player's
            // output as 16-bit stereo, for the audio A/B against
            // trace-oracle's render of the same file.
            use cubed_opl::a2::player::A2Player;
            let bytes = std::fs::read(&args[2]).unwrap();
            let seconds: f64 = args[3].parse().unwrap();
            let rate: f64 = args.get(5).map_or(49716.0, |r| r.parse().unwrap());
            let mut p = A2Player::new(&bytes, rate).unwrap();
            p.set_adplug_quirks(args.get(6).is_some_and(|q| q == "quirks"));
            p.play();
            let total = (seconds * rate) as usize;
            let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
            let mut pcm: Vec<u8> = Vec::with_capacity(total * 4);
            let mut done = 0;
            while done < total {
                let n = (total - done).min(512);
                p.render(&mut l[..n], &mut r[..n]);
                for i in 0..n {
                    for v in [l[i], r[i]] {
                        let s = (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
                        pcm.extend_from_slice(&s.to_le_bytes());
                    }
                }
                done += n;
            }
            let rate = rate as u32;
            let mut out = Vec::with_capacity(44 + pcm.len());
            out.extend_from_slice(b"RIFF");
            out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
            out.extend_from_slice(b"WAVEfmt ");
            {
                let v = 16u32;
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&2u16.to_le_bytes());
            out.extend_from_slice(&rate.to_le_bytes());
            out.extend_from_slice(&(rate * 4).to_le_bytes());
            out.extend_from_slice(&4u16.to_le_bytes());
            out.extend_from_slice(&16u16.to_le_bytes());
            out.extend_from_slice(b"data");
            out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
            out.extend_from_slice(&pcm);
            std::fs::write(&args[4], out).unwrap();
        }
        Some("mine") => {
            // mine <file> <ticks> <reg hex>... : the engine's writes to these registers
            let song = parse(&std::fs::read(&args[2]).unwrap()).unwrap();
            let ticks: usize = args[3].parse().unwrap();
            let regs: Vec<u16> = args[4..]
                .iter()
                .map(|r| u16::from_str_radix(r, 16).unwrap())
                .collect();
            let mut e = A2Engine::new(song);
            e.adplug_quirks = true;
            let mut w = Vec::new();
            e.reset(&mut w);
            for t in 0..ticks {
                let mut w: Vec<(u16, u8)> = Vec::new();
                e.update(&mut w);
                for (r, v) in w {
                    if regs.contains(&r) {
                        print!("{t}:{r:03x}={v:02x} ");
                    }
                }
            }
            println!();
        }
        Some("raw") => {
            // raw <file> <block> <offset hex> <len>
            let (_, _, blocks) = cubed_opl::a2::unpack(&std::fs::read(&args[2]).unwrap()).unwrap();
            let b = &blocks[args[3].parse::<usize>().unwrap()];
            let at = usize::from_str_radix(&args[4], 16).unwrap();
            let n: usize = args[5].parse().unwrap();
            for (i, chunk) in b[at..at + n].chunks(16).enumerate() {
                println!("{:06x}: {:02x?}", at + i * 16, chunk);
            }
        }
        Some("percuse") => {
            for f in &args[2..] {
                let song = parse(&std::fs::read(f).unwrap()).unwrap();
                if song.flags & 0x40 == 0 {
                    continue;
                }
                let mut fx = std::collections::BTreeMap::<(usize, u8), usize>::new();
                let mut notes = [0usize; 5];
                for &o in song.order.iter() {
                    if (o as usize) >= song.patterns.len() {
                        continue;
                    }
                    let pat = &song.patterns[o as usize];
                    for r in 0..song.pattern_len as usize {
                        for t in 15..20 {
                            let c = pat.cell(r, t);
                            if c.note != 0 {
                                notes[t - 15] += 1;
                            }
                            for &(e, _) in &c.effects {
                                if e != 0 {
                                    *fx.entry((t, e)).or_default() += 1;
                                }
                            }
                        }
                    }
                }
                println!("{f}: notes BD..HH {:?} fx {:?}", notes, fx);
            }
        }
        _ => eprintln!("usage: a2m_tool dump <file> [pattern...]"),
    }
}
