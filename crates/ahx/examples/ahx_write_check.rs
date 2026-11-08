//! Writes every `.ahx`/`.hvl` below a directory back and says which come out
//! different from the file they were read from, and how (`--nobase` writes
//! without the file the song came from, as a song made from scratch is).
//!
//! cargo run --example ahx_write_check -- <dir> [out-dir] [--nobase]
//!
//! With `out-dir`, the written files are put there (same names), so another
//! writer's output can be compared with `cmp`.

use cubed_ahx::{format, write};
use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut e: Vec<_> = read.flatten().map(|e| e.path()).collect();
    e.sort();
    for p in e {
        if p.is_dir() {
            collect(&p, out);
        } else if matches!(
            p.extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("ahx" | "hvl")
        ) {
            out.push(p);
        }
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let nobase = args.iter().any(|a| a == "--nobase");
    args.retain(|a| a != "--nobase");
    let mut args = args.into_iter();
    let dir = PathBuf::from(args.next().expect("a directory of songs"));
    let out_dir = args.next().map(PathBuf::from);
    if let Some(d) = &out_dir {
        std::fs::create_dir_all(d).unwrap();
    }
    let mut paths = Vec::new();
    collect(&dir, &mut paths);
    let (mut same, mut differ, mut refused) = (0, 0, 0);
    for p in paths {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&p).unwrap();
        let song = match format::parse(&bytes) {
            Ok(s) => s,
            Err(e) => {
                println!("{name}: does not parse: {e}");
                continue;
            }
        };
        match write::write(&song, if nobase { None } else { Some(&bytes) }) {
            Ok(out) => {
                if let Some(d) = &out_dir {
                    std::fs::write(d.join(&name), &out).unwrap();
                }
                if out == bytes {
                    same += 1;
                } else {
                    differ += 1;
                    let first = out.iter().zip(&bytes).position(|(a, b)| a != b);
                    println!(
                        "{name}: {} bytes written, {} read; first difference at {:?}",
                        out.len(),
                        bytes.len(),
                        first
                    );
                }
            }
            Err(e) => {
                refused += 1;
                println!("{name}: {e}");
            }
        }
    }
    println!("{same} identical, {differ} different, {refused} refused");
}
