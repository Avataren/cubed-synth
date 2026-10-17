//! Dumps the SID player's register stream, the form GoatTracker's headless
//! playroutine (`gtref`, `.ai/sid-oracle/`) writes: one line per frame,
//! "frame r00 .. r24" in hex, each register as written by the end of that
//! frame. The register gate against GT (plan-sid-authoring.md) compares the two.
//!
//! cargo run --release -p cubed-sid --example sid_regs \
//!   -- song.asid FRAMES out.regs [SUBSONG]

use std::fmt::Write as _;

use cubed_sid::{SidSong, SidSongPlayer, DEFAULT_SAMPLE_RATE};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: sid_regs song.asid FRAMES out.regs [SUBSONG]");
        std::process::exit(2);
    }
    let bytes = std::fs::read(&args[1]).expect("reads the song file");
    let frames: usize = args[2].parse().expect("FRAMES is a number");
    let subsong: usize = args
        .get(4)
        .map_or(0, |s| s.parse().expect("SUBSONG is a number"));
    let song = SidSong::parse(&bytes).expect("parses as a SID song file");
    let model = song.model;
    let mut player = SidSongPlayer::with_model(song, model, DEFAULT_SAMPLE_RATE, subsong)
        .expect("player builds");
    let mut out = String::new();
    for f in 0..frames {
        player.frame();
        write!(out, "{f}").unwrap();
        for r in 0..25u8 {
            write!(out, " {:02x}", player.chip().written(r)).unwrap();
        }
        out.push('\n');
    }
    std::fs::write(&args[3], out).expect("writes the dump");
}
