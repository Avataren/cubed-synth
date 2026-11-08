//! Loads every module in a directory (recursively) and writes it back with
//! the crate's writers, one `<name>.out` per song, for byte comparison with
//! another writer (the DAW's, say) and for round-trip checks.
//!
//! cargo run -p cubed-tracker --release --example tracker_write -- songs/ out/

use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = &args[..] else {
        panic!("usage: tracker_write <songs dir> <out dir>");
    };
    std::fs::create_dir_all(output).unwrap();
    let mut files = Vec::new();
    collect(Path::new(input), &mut files);
    files.sort();
    let (mut written, mut refused) = (0, 0);
    for path in files {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(module) = cubed_tracker::load(&bytes) else {
            continue;
        };
        let name = path
            .strip_prefix(input)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace(['/', '\\'], "__");
        match cubed_tracker::write(&module) {
            Ok(out) => {
                std::fs::write(Path::new(output).join(format!("{name}.out")), out).unwrap();
                written += 1;
            }
            Err(e) => {
                eprintln!("{name}: {e}");
                refused += 1;
            }
        }
    }
    eprintln!("{written} written, {refused} refused");
}
