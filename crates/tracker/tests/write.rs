//! The writers: parse(write(parse(x))) equals parse(x) on the synthetic
//! fixtures, and on real songs when `CUBED_MODULES` names a directory (searched
//! recursively) of them. The bytes written for the fixtures are pinned by
//! hashes.

use std::path::{Path, PathBuf};

use cubed_tracker::{load, sniff, write, Format};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn round_trip(name: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let a = load(bytes).map_err(|e| format!("{name}: {e}"))?;
    let out = write(&a).map_err(|e| format!("{name}: cannot write: {e}"))?;
    let b = load(&out).map_err(|e| format!("{name}: the written file does not load: {e}"))?;
    let d = a.differences(&b);
    if !d.is_empty() {
        return Err(format!("{name}: {}", d.join("\n    ")));
    }
    // Writing again changes nothing.
    let again = write(&b).map_err(|e| format!("{name}: second write: {e}"))?;
    if again != out {
        return Err(format!("{name}: the second write differs from the first"));
    }
    Ok(out)
}

#[test]
fn fixtures_round_trip() {
    for (name, format) in [
        ("demo.mod", Format::Mod),
        ("demo.s3m", Format::S3m),
        ("demo.xm", Format::Xm),
    ] {
        let bytes = fixture(name);
        assert_eq!(sniff(&bytes), Some(format));
        let out = round_trip(name, &bytes).unwrap();
        assert_eq!(sniff(&out), Some(format), "{name}");
        // The mixer opens a module song by format and bytes (`open_module_song`).
        assert_eq!(
            cubed_tracker::load_format(format, &out).unwrap(),
            load(&out).unwrap(),
            "{name}"
        );
    }
}

/// The bytes written for the fixtures stay as they are (a change here is a
/// change to the file format output: look at it before updating the hashes).
#[test]
fn fixture_bytes_are_stable() {
    let mut got = Vec::new();
    for name in ["demo.mod", "demo.s3m", "demo.xm"] {
        let out = write(&load(&fixture(name)).unwrap()).unwrap();
        got.push((name, out.len(), fnv(&out)));
    }
    eprintln!("{got:?}");
    assert_eq!(
        got,
        vec![
            ("demo.mod", 13316, 1924245640518841266),
            ("demo.s3m", 5440, 3272881148193816907),
            ("demo.xm", 6979, 6429884249828061974),
        ]
    );
}

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

/// Real songs: `CUBED_MODULES=~/Downloads/mods cargo test -p cubed-tracker --test write real -- --nocapture`.
#[test]
fn real_songs_round_trip() {
    let Some(dir) = std::env::var_os("CUBED_MODULES") else {
        return;
    };
    let mut files = Vec::new();
    collect(Path::new(&dir), &mut files);
    files.sort();
    let (mut ok, mut skipped, mut failures) = (0, 0, Vec::new());
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    for path in files {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if sniff(&bytes).is_none() {
            skipped += 1;
            continue;
        }
        if load(&bytes).is_err() {
            skipped += 1;
            continue;
        }
        let m = load(&bytes).unwrap();
        let kind = format!(
            "{:?} {:?}{}",
            m.format,
            m.mod_flavor,
            if m.jump_targets
                .as_ref()
                .is_some_and(|t| t.iter().enumerate().any(|(i, &v)| i != v))
            {
                " with separators"
            } else {
                ""
            }
        );
        *kinds.entry(kind).or_default() += 1;
        match round_trip(&path.display().to_string(), &bytes) {
            Ok(_) => ok += 1,
            Err(e) => failures.push(e),
        }
    }
    eprintln!(
        "{ok} round trips, {skipped} skipped, {} failures; {kinds:?}",
        failures.len()
    );
    for f in &failures {
        eprintln!("FAIL {f}");
    }
    assert!(
        failures.is_empty(),
        "{} songs do not round trip",
        failures.len()
    );
}
