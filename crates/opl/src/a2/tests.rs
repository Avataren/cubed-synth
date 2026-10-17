//! O6 gate (`.ai/plan-opl.md`). The corpus is `tests/fixtures/a2m`.
//!
//! Two references, both produced by running AdPlug as a black box
//! (`oracle/regen.sh`; its source was not read):
//! - `oracle/blocks.tsv`: size and FNV-1a-64 of every data block as AdPlug's
//!   depackers unpack it. Every block of every file must match.
//! - `oracle/song-info.tsv`: title, author and instrument names from AdPlug's
//!   player API.
//!
//! The tier-1 table below is a third, separate reading: AdPlug-unpacked blocks
//! read at `techinfo.htm`'s offsets by a Python script, not by this parser.

use super::model::cp437;
use super::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/a2m")
}

fn oracle(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/a2/oracle")
            .join(name),
    )
    .unwrap()
}

fn read(rel: &str) -> Vec<u8> {
    std::fs::read(corpus_dir().join(rel)).unwrap()
}

fn corpus() -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("a2m"))
            {
                out.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut out = Vec::new();
    let root = corpus_dir();
    walk(&root, &root, &mut out);
    out.sort();
    out
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

/// (path, block) → "size\tfnv" as AdPlug unpacks it.
fn oracle_blocks() -> HashMap<(String, usize), String> {
    oracle("blocks.tsv")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (
                (f[0].to_string(), f[1].parse().unwrap()),
                format!("{}\t{}", f[3], f[4]),
            )
        })
        .collect()
}

fn block_key(block: &[u8]) -> String {
    format!("{}\t{:016x}", block.len(), fnv1a64(block))
}

/// E15: every file parses, or refuses with one true line. Today all 279 parse;
/// a refusal would have to be listed here with its reason to pass.
#[test]
fn every_corpus_file_parses_or_refuses_truthfully() {
    let expected_refusals: &[(&str, &str)] = &[];
    let files = corpus();
    assert_eq!(
        files.len(),
        279,
        "corpus size changed; update the README manifest and this test"
    );
    let mut refusals = Vec::new();
    for rel in &files {
        match parse(&read(rel)) {
            Ok(song) => {
                assert_eq!(song.patterns.len(), read(rel)[15] as usize, "{rel}");
                assert!(
                    song.order.len() == 128 && !song.instruments.is_empty(),
                    "{rel}"
                );
            }
            Err(e) => {
                let why = e.to_string();
                assert!(
                    !why.contains('\n') && !why.is_empty(),
                    "{rel}: refusal must be one line: {why:?}"
                );
                refusals.push((rel.clone(), why));
            }
        }
    }
    let got: Vec<(&str, &str)> = refusals
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    assert_eq!(got, expected_refusals);
}

#[test]
fn every_block_unpacks_like_adplug() {
    let reference = oracle_blocks();
    assert_eq!(reference.len(), 1013);
    let mut checked = 0;
    for rel in corpus() {
        let (_, _, blocks) = unpack(&read(&rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        for (i, block) in blocks.iter().enumerate() {
            let want = reference
                .get(&(rel.clone(), i))
                .unwrap_or_else(|| panic!("{rel} block {i} not in blocks.tsv"));
            assert_eq!(&block_key(block), want, "{rel} block {i}");
            checked += 1;
        }
    }
    assert_eq!(checked, reference.len());
}

/// Title, author and instrument names against AdPlug's player API, for every
/// file. Two differences are AdPlug display artifacts and are allowed, each
/// only in its exact shape:
/// - a name whose length byte is 32 comes back as 41 characters: those 32
///   plus stale bytes from the slot's tail (v11+ only);
/// - a name whose declared length covers NUL bytes comes back cut at the
///   first NUL (a C string).
///
/// AdPlug's instrument count is all 250 before v9 (every old name slot holds
/// text such as " st-001: ..."), and the last instrument with FM data after.
#[test]
fn names_match_adplug() {
    let mut artifacts = 0;
    for line in oracle("song-info.tsv")
        .lines()
        .filter(|l| !l.starts_with('#'))
    {
        let f: Vec<&str> = line.split('\t').collect();
        let song = parse(&read(f[0])).unwrap();
        assert_eq!(unhex(f[2]), song.name, "{} title", f[0]);
        assert_eq!(unhex(f[3]), song.composer, "{} author", f[0]);
        let count: usize = f[4].parse().unwrap();
        let last_fm = song
            .instruments
            .iter()
            .rposition(|i| i.fm != [0; 11])
            .map_or(0, |p| p + 1);
        let expected = if song.version <= 8 { 250 } else { last_fm };
        assert_eq!(count, expected, "{} instrument count", f[0]);
        for (i, hex) in f[5..].iter().enumerate() {
            let theirs = unhex(hex);
            let ours = &song.instruments[i].name;
            if &theirs == ours {
                continue;
            }
            let padded = ours.len() == 32
                && theirs.len() == 41
                && theirs.starts_with(ours)
                && song.version >= 11;
            let nul_cut =
                ours.contains(&0) && theirs == ours[..ours.iter().position(|&b| b == 0).unwrap()];
            assert!(
                padded || nul_cut,
                "{} instrument {i}: {:?} vs AdPlug {:?}",
                f[0],
                cp437(ours),
                cp437(&theirs)
            );
            artifacts += 1;
        }
    }
    assert_eq!(
        artifacts, 52,
        "the number of known AdPlug name artifacts changed"
    );
}

struct Tier1 {
    path: &'static str,
    version: u8,
    patterns: usize,
    name: &'static [u8],
    composer: &'static [u8],
    order: [u8; 12],
    tempo: u8,
    speed: u8,
    pattern_len: u16,
    tracks: u8,
    /// (pattern, row, channel, [note, instrument, fx1, data1, fx2, data2]):
    /// the first non-empty cell of the first and of the last pattern.
    cells: [(usize, usize, usize, [u8; 6]); 2],
}

const TIER1: &[Tier1] = &[
    Tier1 {
        path: "Subz3ro/intro-tune coop.a2m",
        version: 1,
        patterns: 10,
        name: b"Intro-Tune Coop",
        composer: b"NHP/AnArchY & HYDRA/dEViLs & subz3ro :-D",
        order: [7, 0, 0, 3, 1, 1, 2, 0, 9, 8, 8, 4],
        tempo: 50,
        speed: 6,
        pattern_len: 64,
        tracks: 9,
        cells: [
            (0, 0, 0, [13, 3, 13, 6, 0, 0]),
            (9, 0, 0, [13, 3, 13, 6, 0, 0]),
        ],
    },
    Tier1 {
        path: "Nula/super mario.a2m",
        version: 1,
        patterns: 12,
        name: b"",
        composer: b"",
        order: [6, 7, 8, 9, 10, 11, 0, 1, 2, 3, 4, 5],
        tempo: 50,
        speed: 6,
        pattern_len: 64,
        tracks: 9,
        cells: [
            (0, 0, 0, [39, 1, 13, 4, 0, 0]),
            (11, 0, 2, [40, 3, 0, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "Nula/prehistorik.a2m",
        version: 5,
        patterns: 25,
        name: b"prehistorik",
        composer: b"nula",
        order: [0, 1, 2, 3, 4, 5, 6, 5, 6, 7, 8, 9],
        tempo: 50,
        speed: 3,
        pattern_len: 64,
        tracks: 18,
        cells: [
            (0, 0, 0, [37, 5, 0, 0, 0, 0]),
            (24, 0, 0, [38, 5, 0, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "Malfunction/little boring trance.a2m",
        version: 5,
        patterns: 26,
        name: b"Little Boring Trance",
        composer: b"Malfunction/Altair",
        order: [0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5],
        tempo: 50,
        speed: 3,
        pattern_len: 64,
        tracks: 18,
        cells: [
            (0, 0, 0, [25, 9, 0, 0, 0, 0]),
            (25, 0, 0, [25, 17, 9, 35, 0, 0]),
        ],
    },
    Tier1 {
        path: "Nula/onward.a2m",
        version: 9,
        patterns: 19,
        name: b"onward",
        composer: b"nula (jugi)",
        order: [9, 10, 11, 12, 13, 14, 13, 14, 15, 16, 15, 17],
        tempo: 50,
        speed: 3,
        pattern_len: 64,
        tracks: 18,
        cells: [
            (0, 0, 0, [61, 1, 0, 0, 0, 0]),
            (18, 0, 0, [63, 1, 0, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "Subz3ro/1942.a2m",
        version: 9,
        patterns: 12,
        name: b"1942",
        composer: b"lizardking & someone // cover by subz3ro",
        order: [11, 11, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        tempo: 50,
        speed: 4,
        pattern_len: 64,
        tracks: 16,
        cells: [
            (0, 0, 0, [16, 3, 20, 2, 0, 0]),
            (11, 0, 0, [16, 3, 20, 2, 0, 0]),
        ],
    },
    Tier1 {
        path: "Subz3ro/psychedelic sound synthesis.a2m",
        version: 10,
        patterns: 1,
        name: b"PSYCHEDELiC SOUND SYNTHESiS",
        composer: b"subz3ro/Altair",
        order: [0, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
        tempo: 10,
        speed: 15,
        pattern_len: 12,
        tracks: 4,
        cells: [
            (0, 0, 2, [32, 2, 35, 225, 0, 0]),
            (0, 0, 2, [32, 2, 35, 225, 0, 0]),
        ],
    },
    Tier1 {
        path: "MadBrain/oskari the heimfanker.a2m",
        version: 11,
        patterns: 59,
        name: b"Oskari the Heimfanker",
        composer: b"Madbrain 18 dec 2010",
        order: [3, 2, 0, 1, 32, 33, 35, 36, 37, 38, 34, 39],
        tempo: 55,
        speed: 4,
        pattern_len: 64,
        tracks: 18,
        cells: [
            (0, 0, 0, [0, 61, 0, 0, 0, 0]),
            (58, 0, 13, [0, 13, 0, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "Diode Milliampere/samsara.a2m",
        version: 11,
        patterns: 49,
        name: b"Samsara",
        composer: b"Diode Milliampere",
        order: [11, 14, 15, 0, 4, 5, 6, 17, 18, 24, 19, 7],
        tempo: 140,
        speed: 3,
        pattern_len: 192,
        tracks: 18,
        cells: [
            (0, 0, 0, [50, 1, 12, 48, 9, 20]),
            (48, 0, 12, [49, 9, 0, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "Diode Milliampere/keys.a2m",
        version: 12,
        patterns: 81,
        name: b" new",
        composer: b"",
        order: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        tempo: 156,
        speed: 3,
        pattern_len: 192,
        tracks: 18,
        cells: [
            (0, 12, 4, [37, 8, 0, 0, 0, 0]),
            (80, 0, 9, [37, 0, 0, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "Diode Milliampere/endless scroll.a2m",
        version: 12,
        patterns: 89,
        name: b" new",
        composer: b"",
        order: [3, 4, 5, 6, 0, 1, 2, 7, 8, 9, 10, 11],
        tempo: 165,
        speed: 3,
        pattern_len: 192,
        tracks: 20,
        cells: [
            (0, 0, 1, [23, 6, 0, 0, 10, 4]),
            (88, 0, 0, [255, 0, 0, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "Malfunction/opl303.a2m",
        version: 13,
        patterns: 33,
        name: b"OPL303",
        composer: b"Malfunction/Altair",
        order: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        tempo: 59,
        speed: 3,
        pattern_len: 128,
        tracks: 18,
        cells: [
            (0, 0, 2, [37, 9, 0, 0, 0, 0]),
            (32, 0, 0, [37, 80, 9, 32, 12, 63]),
        ],
    },
    Tier1 {
        path: "Kvee/paradox #3.a2m",
        version: 13,
        patterns: 13,
        name: b"Paradox #3",
        composer: b"kvee [original by dubmood]",
        order: [2, 1, 3, 0, 10, 4, 11, 5, 6, 0, 7, 8],
        tempo: 69,
        speed: 7,
        pattern_len: 64,
        tracks: 10,
        cells: [
            (0, 0, 0, [25, 9, 0, 0, 0, 0]),
            (12, 0, 0, [25, 9, 0, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "Konakonaa/ETWARAWK.A2M",
        version: 14,
        patterns: 79,
        name: b"The Eternal Warrior Awakes",
        composer: b"konakona/konakonaa",
        order: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 13],
        tempo: 60,
        speed: 8,
        pattern_len: 64,
        tracks: 20,
        cells: [
            (0, 0, 0, [46, 1, 0, 0, 0, 0]),
            (78, 0, 5, [34, 86, 0, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "Malfunction/fm-tronikk (aka altair street dancer).a2m",
        version: 14,
        patterns: 18,
        name: b"FM-Tronikk (aka Altair Street Dancer)",
        composer: b"Malfunction/Altair",
        order: [0, 1, 16, 16, 2, 3, 4, 5, 7, 8, 9, 10],
        tempo: 108,
        speed: 6,
        pattern_len: 128,
        tracks: 18,
        cells: [
            (0, 0, 0, [255, 0, 12, 0, 0, 0]),
            (17, 0, 0, [51, 12, 0, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "Televicious/boom.a2m",
        version: 14,
        patterns: 17,
        name: b"Boom",
        composer: b"Televicious",
        order: [0, 1, 2, 3, 3, 4, 5, 5, 6, 8, 9, 9],
        tempo: 33,
        speed: 4,
        pattern_len: 64,
        tracks: 18,
        cells: [
            (0, 0, 0, [40, 20, 0, 0, 0, 0]),
            (16, 0, 0, [255, 0, 1, 0, 0, 0]),
        ],
    },
    Tier1 {
        path: "NAB622/corridors of time.a2m",
        version: 11,
        patterns: 37,
        name: b"Corridors of Time",
        composer: b"Dretz - Original by Yasunori Mitsuda",
        order: [0, 2, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        tempo: 90,
        speed: 6,
        pattern_len: 64,
        tracks: 18,
        cells: [
            (0, 0, 0, [0, 255, 37, 63, 35, 240]),
            (36, 0, 0, [0, 255, 35, 240, 0, 0]),
        ],
    },
];

/// Per-version spot checks against the independent reading (README tier 1:
/// every version, plus Corridors of Time).
#[test]
fn tier1_fields_match_an_independent_reading() {
    let mut versions: Vec<u8> = TIER1.iter().map(|t| t.version).collect();
    versions.sort();
    versions.dedup();
    assert_eq!(versions, [1, 5, 9, 10, 11, 12, 13, 14]);
    for t in TIER1 {
        let song = parse(&read(t.path)).unwrap();
        assert_eq!(song.version, t.version, "{}", t.path);
        assert_eq!(song.patterns.len(), t.patterns, "{}", t.path);
        assert_eq!(song.name, t.name, "{}", t.path);
        assert_eq!(song.composer, t.composer, "{}", t.path);
        assert_eq!(song.order[..12], t.order, "{}", t.path);
        assert_eq!((song.tempo, song.speed), (t.tempo, t.speed), "{}", t.path);
        assert_eq!(
            (song.pattern_len, song.tracks),
            (t.pattern_len, t.tracks),
            "{}",
            t.path
        );
        for &(p, row, ch, raw) in &t.cells {
            let cell = song.patterns[p].cell(row, ch);
            let got = [
                cell.note,
                cell.instrument,
                cell.effects[0].0,
                cell.effects[0].1,
                cell.effects[1].0,
                cell.effects[1].1,
            ];
            assert_eq!(got, raw, "{} pattern {p} row {row} channel {ch}", t.path);
            // ...and it is the first non-empty cell, as the reading chose it.
            let pat = &song.patterns[p];
            let first = (0..pat.rows)
                .flat_map(|r| (0..pat.channels).map(move |c| (r, c)))
                .find(|&(r, c)| *pat.cell(r, c) != Cell::default());
            assert_eq!(first, Some((row, ch)), "{} pattern {p}", t.path);
        }
    }
    // cp437 is what a UI would show; one composer needs it.
    let benjamin = parse(&read("Benjamin Gerardin/ballad.a2m")).unwrap();
    assert_eq!(cp437(&benjamin.composer), "Benjamin Gérardin");
}

/// One flipped bit per packer: the gate must notice, either as a refusal or
/// as a block that no longer matches AdPlug's.
#[test]
fn a_flipped_bit_is_caught_for_every_packer() {
    let reference = oracle_blocks();
    for (rel, packer) in [
        ("Nula/super mario.a2m", "SixPack"),
        ("NAB622/corridors of time.a2m", "aPLib"),
        ("Malfunction/opl303.a2m", "LZH"),
    ] {
        let clean = read(rel);
        let (version, _, _) = unpack(&clean).unwrap();
        let data_start = match version {
            1..=4 => 26,
            5..=8 => 34,
            _ => 84,
        };
        let block0_len = if version >= 9 {
            u32::from_le_bytes(clean[16..20].try_into().unwrap()) as usize
        } else {
            u16::from_le_bytes([clean[16], clean[17]]) as usize
        };
        // Past the LZH size header, a third of the way into block 0.
        let target = data_start + 5 + block0_len / 3;
        for bit in 0..8 {
            let mut bad = clean.clone();
            bad[target] ^= 1 << bit;
            match unpack(&bad) {
                Err(e) => assert!(!e.to_string().contains('\n'), "{rel}"),
                Ok((_, _, blocks)) => {
                    let changed = blocks
                        .iter()
                        .enumerate()
                        .any(|(i, b)| block_key(b) != reference[&(rel.to_string(), i)]);
                    assert!(
                        changed,
                        "{packer}: bit {bit} of byte {target} in {rel} slipped through"
                    );
                }
            }
        }
    }
}

#[test]
fn refusals_say_why() {
    let clean = read("Malfunction/opl303.a2m");
    let say = |bytes: &[u8]| parse(bytes).unwrap_err().to_string();

    assert_eq!(
        say(b"_A2module_"),
        "10 bytes is too short for an A2M header"
    );
    assert_eq!(
        say(b"Extended Module: not an a2m"),
        "not an Adlib Tracker II module (no _A2module_ id)"
    );
    assert_eq!(
        say(b"_A2tiny_module_\x00\x00\x00\x00"),
        "A2T tiny modules are not supported (no corpus file is one)"
    );

    let mut v = clean.clone();
    v[14] = 2;
    assert_eq!(say(&v), "A2M version 2 is not supported: its LZW packer is not implemented (no corpus file uses it)");
    v[14] = 7;
    assert_eq!(say(&v), "A2M version 7 is not supported: its LZSS packer is not implemented (no corpus file uses it)");
    v[14] = 15;
    assert_eq!(
        say(&v),
        "A2M version 15 is not supported: it is newer than the documented versions 1–14"
    );

    let mut v = clean.clone();
    v[15] = 0;
    assert_eq!(
        say(&v),
        "header claims 0 patterns; this version holds at most 128"
    );

    let cut = &clean[..clean.len() - 1];
    assert!(say(cut).starts_with("data block "), "{}", say(cut));
    assert!(say(cut).ends_with("runs past the end of the file"));

    let mut long = clean.clone();
    long.push(0);
    assert_eq!(say(&long), "1 bytes follow the last data block");

    // A pattern count that needs a block the header says is empty.
    let mut v = clean.clone();
    v[15] = 128;
    assert!(
        say(&v).contains("is empty although the pattern count needs it"),
        "{}",
        say(&v)
    );
}

/// O7 gate: the files the player refuses at load, with their reason. Every
/// other corpus file must play.
const REFUSED: &[(&str, &str)] = &[(
    "OxygenStar/oxygenstar's instrument set #001.a2m",
    "its order list holds only jump markers, no pattern to play \
     (an instrument collection saved as a song)",
)];

/// First differing chunk of two hash lists, as a tick range.
fn first_bad_chunk(want: &[&str], got: &[u64]) -> Option<String> {
    let n = want.len().max(got.len());
    (0..n)
        .find(|&i| want.get(i).copied() != got.get(i).map(|h| format!("{h:016x}")).as_deref())
        .map(|i| format!("ticks {}..{}", i * gate::CHUNK, (i + 1) * gate::CHUNK))
}

/// O7 gate: the register state after every tick equals AdPlug's for the
/// whole song as AdPlug plays it (its end flag, capped at 120000 ticks), on
/// every corpus file the player does not refuse (`oracle/gate.tsv`).
#[test]
fn engine_state_matches_adplug_for_whole_songs() {
    let mut bad = Vec::new();
    let mut played = 0;
    for line in oracle("gate.tsv").lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        let song = parse(&read(f[0])).unwrap();
        let refused = REFUSED.iter().find(|(p, _)| *p == f[0]).map(|(_, r)| *r);
        assert_eq!(
            engine::A2Engine::refusal(&song).as_deref(),
            refused,
            "{}",
            f[0]
        );
        if refused.is_some() {
            continue;
        }
        let want: Vec<&str> = f[2].split(',').collect();
        let got = gate::engine_hashes(song, f[1].parse().unwrap());
        if let Some(at) = first_bad_chunk(&want, &got) {
            bad.push(format!("{}: {at}", f[0]));
        }
        played += 1;
    }
    assert!(
        bad.is_empty(),
        "{} of {played} differ:\n{}",
        bad.len(),
        bad.join("\n")
    );
    assert_eq!(played, corpus().len() - REFUSED.len());
}

/// O7 gate: the synthetic probes (`oracle/probes/*.py`, one rule each; the
/// songs stored in `gate.rs`'s sparse form) match AdPlug for 80 ticks.
#[test]
fn engine_state_matches_adplug_on_probes() {
    let mut bad = Vec::new();
    let mut n = 0;
    for line in oracle("probes.tsv").lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        let want: Vec<&str> = f[3].split(',').collect();
        let got = gate::engine_hashes(gate::decode(f[2]), f[1].parse().unwrap());
        if first_bad_chunk(&want, &got).is_some() {
            bad.push(f[0].to_string());
        }
        n += 1;
    }
    assert!(
        bad.is_empty(),
        "{} of {n} probes differ: {}",
        bad.len(),
        bad.join(" ")
    );
    assert!(n >= 290);
}

// ---------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------

/// Header check value, recomputed from the file: AT2's own algorithm
/// (`_a2m_saver.pas`) reproduces every corpus file's stored value.
#[test]
fn crc_matches_corpus() {
    use super::model::{layout, unpack};
    let mut bad = Vec::new();
    for rel in corpus() {
        let file = read(&rel);
        let version = file[14];
        let lay = layout(version).unwrap();
        let (_, npat, blocks) = unpack(&file).unwrap();
        let field = if lay.wide_lengths { 4 } else { 2 };
        let lengths: Vec<u32> = (0..lay.length_fields)
            .map(|i| {
                let at = 16 + i * field;
                if lay.wide_lengths {
                    u32::from_le_bytes(file[at..at + 4].try_into().unwrap())
                } else {
                    u16::from_le_bytes(file[at..at + 2].try_into().unwrap()) as u32
                }
            })
            .collect();
        let start = 16 + lay.length_fields * field;
        let mut at = start;
        let mut packed: Vec<&[u8]> = Vec::new();
        for &len in lengths
            .iter()
            .take(1 + npat.div_ceil(lay.patterns_per_block))
        {
            packed.push(&file[at..at + len as usize]);
            at += len as usize;
        }
        let _ = blocks;
        let want = u32::from_le_bytes(file[10..14].try_into().unwrap());
        let got = write::header_crc(&packed, &lengths);
        if got != want {
            bad.push(format!("{rel} v{version}: {got:08x} != {want:08x}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} files:\n{}",
        bad.len(),
        bad[..bad.len().min(10)].join("\n")
    );
}

#[test]
fn packers_round_trip() {
    let mut samples: Vec<Vec<u8>> = vec![
        vec![0; 1],
        vec![7; 2],
        vec![0; 100_000],
        (0..70_000u32).map(|i| (i * 7 % 251) as u8).collect(),
        b"abcabcabcabcabcabcabc and then some different text abcabc".to_vec(),
    ];
    // Noise, so literals dominate and the 16-bit block count is exceeded.
    let mut x = 12345u32;
    samples.push(
        (0..200_000)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 24) as u8
            })
            .collect(),
    );
    for s in &samples {
        let a = aplib::pack(s);
        assert_eq!(
            aplib::depack(&a, s.len()).unwrap(),
            (s.clone(), a.len()),
            "aplib {}",
            s.len()
        );
        let l = lzh::pack(s);
        assert_eq!(
            lzh::depack(&l, s.len()).unwrap(),
            (s.clone(), l.len()),
            "lzh {}",
            s.len()
        );
    }
}

/// Real blocks, through each of the two encoders and back.
#[test]
fn corpus_blocks_survive_the_encoders() {
    use super::model::unpack;
    for rel in corpus() {
        let (version, _, blocks) = unpack(&read(&rel)).unwrap();
        if version < 9 {
            continue;
        }
        for (i, b) in blocks.iter().enumerate() {
            let a = aplib::pack(b);
            assert_eq!(
                aplib::depack(&a, b.len()).unwrap().0,
                *b,
                "{rel} block {i} aplib"
            );
            let l = lzh::pack(b);
            assert_eq!(
                lzh::depack(&l, b.len()).unwrap().0,
                *b,
                "{rel} block {i} lzh"
            );
        }
    }
}

/// parse -> write -> parse is the identity (the version aside: v1 and v5 are
/// written as the stored v4 and v8), through the JSON the app uses too.
#[test]
fn every_corpus_file_round_trips() {
    let mut bad = Vec::new();
    for rel in corpus() {
        let file = read(&rel);
        let song = match parse(&file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let written = write::write(&song).unwrap_or_else(|e| panic!("{rel}: {e}"));
        let again = parse(&written).unwrap_or_else(|e| panic!("{rel}: rewritten file: {e}"));
        let mut want = song.clone();
        want.version = write::output_version(song.version);
        if again != want {
            bad.push(rel.clone());
            continue;
        }
        let json = serde_json::to_string(&song).unwrap();
        let back: A2mSong = serde_json::from_str(&json).unwrap();
        assert_eq!(back, song, "{rel}: JSON");
    }
    assert!(
        bad.is_empty(),
        "{} files differ: {:?}",
        bad.len(),
        &bad[..bad.len().min(10)]
    );
}

/// What the model does not hold: the rewritten songdata equals the original
/// block except in the unused tail of each Pascal string (stale bytes), and
/// the reserved kilobyte of v12+ files. Anything else is listed and fails.
#[test]
fn the_model_holds_every_byte() {
    use super::model::unpack;
    let mut stale = 0usize;
    let mut unexplained = Vec::new();
    for rel in corpus() {
        let file = read(&rel);
        let Ok(song) = parse(&file) else { continue };
        let (version, _, a) = unpack(&file).unwrap();
        let (_, _, b) = unpack(&write::write(&song).unwrap()).unwrap();
        // (start, slot length) of every Pascal slot in songdata.
        let mut slots = vec![(0usize, 43usize), (0x2b, 43)];
        let (n, w) = if version < 9 { (250, 33) } else { (255, 43) };
        slots.extend((0..n).map(|i| (0x56 + i * w, w)));
        if version >= 11 {
            slots.extend((0..128).map(|i| (0x1128ba + i * 43, 43)));
        }
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            for k in (0..x.len()).filter(|&k| x[k] != y[k]) {
                let in_tail = i == 0
                    && slots
                        .iter()
                        .any(|&(at, len)| k > at && k < at + len && k > at + x[at] as usize);
                let in_reserved = i == 0 && version >= 12 && (0x115a9f..0x115e9f).contains(&k);
                if in_tail || in_reserved {
                    stale += 1;
                } else if unexplained.len() < 20 {
                    unexplained.push(format!("{rel} block {i} at {k:#x}"));
                }
            }
        }
    }
    println!("{stale} stale bytes ignored");
    assert!(
        unexplained.is_empty(),
        "bytes the model drops:\n{}",
        unexplained.join("\n")
    );
}

#[test]
fn a_new_song_parses_and_plays() {
    for opl3 in [false, true] {
        let song = write::new_song(opl3);
        let bytes = write::write(&song).unwrap();
        let again = parse(&bytes).unwrap();
        assert_eq!(again, song);
        assert_eq!(again.tracks, if opl3 { 18 } else { 9 });
        let mut player = player::A2Player::new(&bytes, 48_000.0).unwrap();
        player.play();
        let (mut l, mut r) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
        for _ in 0..4 {
            player.render(&mut l, &mut r);
        }
    }
}

/// Writes every corpus file's rewrite to `$A2M_DUMP_DIR` (for the AdPlug
/// black-box check in `oracle/check-written.sh`).
#[test]
#[ignore]
fn dump_rewritten_corpus() {
    let dir = std::env::var("A2M_DUMP_DIR").expect("set A2M_DUMP_DIR");
    std::fs::create_dir_all(&dir).unwrap();
    for (i, rel) in corpus().iter().enumerate() {
        let Ok(song) = parse(&read(rel)) else {
            continue;
        };
        let bytes = write::write(&song).unwrap();
        std::fs::write(Path::new(&dir).join(format!("{i:03}.a2m")), bytes).unwrap();
    }
}

#[test]
fn the_json_door_round_trips_and_stays_small() {
    let json = write::a2m_new_json(true).unwrap();
    assert!(
        json.len() < 120_000,
        "a new song's JSON is {} bytes",
        json.len()
    );
    let bytes = write::a2m_from_json(&json).unwrap();
    let back = write::a2m_to_json(&bytes).unwrap();
    assert_eq!(
        serde_json::from_str::<A2mSong>(&back).unwrap(),
        write::new_song(true)
    );
    assert!(write::a2m_from_json("{}")
        .unwrap_err()
        .contains("not an A2M song"));
    let big = read("NAB622/corridors of time.a2m");
    let json = write::a2m_to_json(&big).unwrap();
    println!(
        "corridors of time: {} bytes of JSON for {} bytes of file",
        json.len(),
        big.len()
    );
}

/// The registers a song writes over its first `ticks` ticks.
fn register_writes(song: &A2mSong, ticks: usize) -> Vec<(u16, u8)> {
    use super::engine::A2Engine;
    let mut engine = A2Engine::new(song.clone());
    let mut writes: Vec<(u16, u8)> = Vec::new();
    engine.reset(&mut writes);
    for _ in 0..ticks {
        engine.update(&mut writes);
    }
    writes
}

/// The instrument editor's promise: an edit changes what the chip is told.
/// A note on track 1 with instrument 1 loads that instrument's eleven bytes
/// into that track's OPL channel (AT2 puts track 1 on channel 3: operators
/// 0x08 and 0x0B, feedback register 0xC3); changing them in the model changes
/// those registers, and the change survives the write and re-parse that export
/// and re-import make.
#[test]
fn an_instrument_edit_changes_the_register_writes() {
    const MOD: u16 = 0x08;
    const CAR: u16 = 0x0b;
    let mut song = write::new_song(true);
    song.patterns[0].cells[0] = Cell {
        note: 49, // C-4
        instrument: 1,
        effects: [(0, 0), (0, 0)],
    };
    // The last write to a register in the first 12 ticks (the note's own).
    let last =
        |writes: &[(u16, u8)], reg: u16| writes.iter().rev().find(|w| w.0 == reg).map(|w| w.1);
    let play = |song: &A2mSong| register_writes(&parse(&write::write(song).unwrap()).unwrap(), 12);
    let before = play(&song);

    let mut edited = song.clone();
    edited.instruments[0].fm = [
        0x01, 0x32, 0x1d, 0x08, 0xa3, 0xc5, 0x4b, 0x66, 0x02, 0x05, 0x0d,
    ];
    let reread = parse(&write::write(&edited).unwrap()).unwrap();
    assert_eq!(
        reread.instruments[0].fm, edited.instruments[0].fm,
        "the edit survives export and re-import"
    );
    let after = register_writes(&reread, 12);

    // Flags, attack/decay, sustain/release and waveform go to the operators as stored.
    for (base, i) in [(0x20u16, 0usize), (0x60, 4), (0x80, 6), (0xe0, 8)] {
        for (op, k) in [(MOD, i), (CAR, i + 1)] {
            assert_eq!(
                last(&before, base + op),
                Some(song.instruments[0].fm[k]),
                "reg {:#x} before",
                base + op
            );
            assert_eq!(
                last(&after, base + op),
                Some(edited.instruments[0].fm[k]),
                "reg {:#x} after",
                base + op
            );
        }
    }
    // Feedback and connection: the low nibble of the channel's 0xC0 (OPL3 panning sits above it).
    assert_eq!(last(&before, 0xc3).map(|v| v & 0x0f), Some(0x00));
    assert_eq!(last(&after, 0xc3).map(|v| v & 0x0f), Some(0x0d));
    // The carrier's level goes through the note's volume, so it is not the raw byte, but it moves.
    assert_ne!(last(&before, 0x40 + CAR), last(&after, 0x40 + CAR));
    assert_ne!(before, after);
}
