//! Renders a `.sid` tune to a 16-bit mono WAV and reports what the player did.
//!
//! cargo run --release -p cubed-sid --example psid_play -- tune.sid [subsong] [seconds] [out.wav]
//!
//! subsong is 1-based like the header's (0 or absent: the start song); seconds
//! defaults to 20. Without an out.wav nothing is written, which makes it a speed
//! test: how many times faster than real time the tune runs, the 6510 and the
//! chip together.

use cubed_sid::psid::{PsidOptions, PsidPlayer};
use std::io::Write;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: psid_play tune.sid [subsong (1-based)] [seconds] [out.wav]");
        std::process::exit(2);
    };
    let subsong: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let seconds: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let rate = 44_100.0;
    let bytes = std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let started = Instant::now();
    let options = PsidOptions {
        subsong: subsong.checked_sub(1),
        model: None,
    };
    let mut player = PsidPlayer::new(&bytes, rate, options).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let built = started.elapsed();
    let file = player.file().clone();
    println!(
        "{} / {} / {}: {:?} v{}, {} subsong(s), playing {} on {} SID(s), {:?}, {:?}",
        file.name,
        file.author,
        file.released,
        file.kind,
        file.version,
        file.songs,
        player.subsong() + 1,
        player.sid_count(),
        player.runner().clock(),
        player.chip(0).model(),
    );
    let n = (seconds * rate) as usize;
    let mut out = vec![0f32; n];
    let t = Instant::now();
    for chunk in out.chunks_mut(128) {
        player.render(chunk);
    }
    let took = t.elapsed();
    let peak = out.iter().fold(0f32, |m, x| m.max(x.abs()));
    let rms = (out.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / n.max(1) as f64).sqrt();
    println!(
        "init {:.1} ms; {seconds} s rendered in {:.1} ms ({:.0}x real time); peak {peak:.3}, rms {rms:.4}; {}",
        built.as_secs_f64() * 1e3,
        took.as_secs_f64() * 1e3,
        seconds / took.as_secs_f64(),
        player.ended().map_or("still playing".to_string(), |e| e.to_string()),
    );
    if let Some(wav) = args.get(3) {
        let mut f = std::io::BufWriter::new(std::fs::File::create(wav).expect("create the wav"));
        let data_len = (n * 2) as u32;
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&(rate as u32).to_le_bytes()).unwrap();
        f.write_all(&(rate as u32 * 2).to_le_bytes()).unwrap();
        f.write_all(&2u16.to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        for x in &out {
            f.write_all(&((x.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
                .unwrap();
        }
        println!("wrote {wav}");
    }
}
