//! Compares two directories of modules by what they contain, not by bytes:
//! for every file name in both, loads each and prints how the modules differ.
//! Used to check a writer against another one (the DAW's).
//!
//! cargo run -p cubed-tracker --release --example tracker_diff -- ours/ theirs/

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [a, b] = &args[..] else {
        panic!("usage: tracker_diff <dir> <dir>");
    };
    let mut names: Vec<_> = std::fs::read_dir(a)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    names.sort();
    let (mut same, mut total) = (0, 0);
    let mut kinds: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for name in names {
        let (pa, pb) = (Path::new(a).join(&name), Path::new(b).join(&name));
        let (Ok(x), Ok(y)) = (std::fs::read(&pa), std::fs::read(&pb)) else {
            continue;
        };
        let (Ok(mx), Ok(my)) = (cubed_tracker::load(&x), cubed_tracker::load(&y)) else {
            kinds
                .entry("one does not load".into())
                .or_default()
                .push(name.to_string_lossy().into());
            continue;
        };
        total += 1;
        let d = mx.differences(&my);
        if d.is_empty() {
            same += 1;
            continue;
        }
        for line in d.iter().take(3) {
            let key: String = line.chars().take(70).collect();
            kinds
                .entry(key)
                .or_default()
                .push(name.to_string_lossy().into());
        }
    }
    println!("{same} of {total} load to the same module");
    for (k, v) in kinds {
        println!("  {} x {k}   e.g. {}", v.len(), v[0]);
    }
}
