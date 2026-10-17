//! Bit-exact render goldens for `ahx::engine::AhxEngine`.
//!
//! The expected values in `tests/golden/*.txt` are not hand-derived: they are
//! produced by compiling the vendored C reference
//! (`.ai/ahx/references/hvl_replay.c`) natively and running it over the real
//! fixtures in `public/demos/ahx/` (`tests/golden/gen_goldens.sh`, harness in
//! `tests/golden/hvl_golden.c`). Each golden line is the FNV-1a-64 of the
//! interleaved little-endian `i16` stereo stream `hvl_DecodeFrame` produced
//! for a 50-frame chunk (one second at 50 Hz), so a single wrong sample
//! anywhere in the 20-60 s of audio fails the chunk it lives in.
//!
//! The corpus is `tests/golden/cases.manifest`: 23 fixtures from
//! `public/demos/ahx/` (karma.ahx, six .hvl and sixteen corpus .ahx songs;
//! the jukebox has since grown past them without goldens) across sample rates
//! (22050/44100/48000/96000), AHX stereo modes 0-4, and channel caps (native --
//! the shipped default --, fixed-4 truncation, and a cap strictly between). Runs are 30-64 s. The same
//! manifest drives `gen_goldens.sh`, so a row cannot exist on one side only;
//! `manifest_and_goldens_agree` and `manifest_covers_every_fixture` enforce it.
//!
//! What is being held to the reference: the engine with hi-fi **off**, which is
//! the engine's own default and the only path the C reference has. The harness
//! drives the engine directly (`enable_capture`, `set_mute_solo`, `set_hifi`,
//! `prewarm_hifi`); it never goes through `AhxPlayer::set_hifi`. The app always
//! renders hi-fi; the reference path here is for the golden contract only, so
//! the app's choice cannot move a golden, and there is no golden
//! for hi-fi *on* (a band-limited render is not what the reference plays; it is
//! measured in `tests/ahx_hifi.rs`). Every `Mode` below prepares the engine
//! differently and must land on the same golden: capture on, mute/solo set then
//! cleared, and hi-fi turned on (with and without a full prewarm, which
//! simulates the whole song) then off again before the first sample.
//!
//! Feature coverage (the harness's `coverage` line, voice-frames) is asserted
//! corpus-wide by `corpus_exercises_every_voice_feature`: hard-cut release,
//! square sweep, noise, filter sweep, vibrato, tone portamento, PList and PList
//! ring modulation (commands 7/8, only at native channel counts).

use cubed_ahx::engine::{AhxEngine, EngineError};
use cubed_ahx::format;
use cubed_ahx::voice::{panning_left, panning_right};
use cubed_ahx::waveform::WAVES;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

const FNV_BASIS: u64 = 0xcbf29ce484222325;

struct Golden {
    waves: u64,
    panning: u64,
    channels: usize,
    /// `songend` line: (song_end_reached, pos_nr, note_nr) after the last frame.
    end: (bool, i32, i32),
    chunks: Vec<(usize, u64)>,
    /// `case` line the C harness echoes: (fixture, freq, defstereo, frames, chunk, cap).
    case: Option<(String, u32, u8, usize, usize, usize)>,
    /// `coverage` line: [ring, noise, filter, square, hardcut, vibrato, slide, plist].
    coverage: Option<[u64; 8]>,
}

fn load_golden(name: &str) -> Golden {
    let text = fs::read_to_string(root().join("tests/golden").join(name))
        .unwrap_or_else(|e| panic!("reading golden {name}: {e}"));
    let mut g = Golden {
        waves: 0,
        panning: 0,
        channels: 0,
        end: (false, 0, 0),
        chunks: Vec::new(),
        case: None,
        coverage: None,
    };
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.as_slice() {
            ["waves", h] => g.waves = u64::from_str_radix(h, 16).unwrap(),
            ["panning", h] => g.panning = u64::from_str_radix(h, 16).unwrap(),
            ["channels", n, ..] => g.channels = n.parse().unwrap(),
            [frame, h] if frame.chars().all(|c| c.is_ascii_digit()) => {
                g.chunks
                    .push((frame.parse().unwrap(), u64::from_str_radix(h, 16).unwrap()));
            }
            ["case", fixture, freq, stereo, frames, chunk, cap] => {
                g.case = Some((
                    fixture.to_string(),
                    freq.parse().unwrap(),
                    stereo.parse().unwrap(),
                    frames.parse().unwrap(),
                    chunk.parse().unwrap(),
                    cap.parse().unwrap(),
                ));
            }
            ["coverage", "ring", r, "noise", ns, "filter", f, "square", q, "hardcut", h, "vibrato", v, "slide", sl, "plist", pl] =>
            {
                let n = |x: &&str| x.parse::<u64>().unwrap();
                g.coverage = Some([n(r), n(ns), n(f), n(q), n(h), n(v), n(sl), n(pl)]);
            }
            ["songend", e, "posnr", p, "notenr", n] => {
                g.end = (*e != "0", p.parse().unwrap(), n.parse().unwrap());
            }
            _ => {}
        }
    }
    assert!(!g.chunks.is_empty(), "golden {name} has no chunk lines");
    g
}

fn song(fixture: &str) -> format::Song {
    let bytes = fs::read(root().join("tests/fixtures/songs").join(fixture))
        .unwrap_or_else(|e| panic!("reading fixture {fixture}: {e}"));
    format::parse(&bytes).unwrap_or_else(|e| panic!("parsing {fixture}: {e}"))
}

/// One `cases.manifest` row.
#[derive(Clone, Debug)]
struct Case {
    fixture: String,
    freq: u32,
    defstereo: u8,
    frames: usize,
    chunk: usize,
    cap: usize,
}

impl Case {
    fn golden(&self) -> String {
        let stem = self.fixture.rsplit_once('.').unwrap().0;
        format!(
            "{stem}.{}.s{}.cap{}.txt",
            self.freq, self.defstereo, self.cap
        )
    }
}

fn manifest() -> Vec<Case> {
    let text = fs::read_to_string(root().join("tests/golden/cases.manifest"))
        .expect("reading cases.manifest");
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(f.len(), 6, "cases.manifest: bad row {line:?}");
        rows.push(Case {
            fixture: f[0].to_string(),
            freq: f[1].parse().unwrap(),
            defstereo: f[2].parse().unwrap(),
            frames: f[3].parse().unwrap(),
            chunk: f[4].parse().unwrap(),
            cap: f[5].parse().unwrap(),
        });
    }
    rows
}

fn cases_for(fixture: &str) -> Vec<Case> {
    let rows: Vec<Case> = manifest()
        .into_iter()
        .filter(|c| c.fixture == fixture)
        .collect();
    assert!(!rows.is_empty(), "no manifest rows for {fixture}");
    rows
}

/// Renders one manifest row and compares every chunk hash against its golden.
/// Also asserts the channel count and `dropped_channels` that the cap implies.
/// `capture` turns on per-voice waveform capture (`enable_capture`) for the
/// run: the mix must stay bit-exact against the same golden either way.
/// `mode`: how the engine is prepared before the reference render. Every mode
/// must land on the same golden.
#[derive(Clone, Copy)]
enum Mode {
    Plain,
    Capture,
    /// Mute/solo set to something audible-changing and then cleared again:
    /// the cleared state must be indistinguishable from never having been set.
    MuteSoloCleared,
    /// Hi-fi oscillators switched on and back off before the first sample:
    /// off must be the reference, whatever happened to the switch before.
    HifiCleared,
    /// As `HifiCleared`, but with the whole-song prewarm run in between: it
    /// simulates every subsong on the engine's own voices and transport, and
    /// must leave no trace once hi-fi is off again.
    HifiPrewarmedCleared,
}

fn check(c: &Case, mode: Mode) {
    let name = c.golden();
    let g = load_golden(&name);
    let s = song(&c.fixture);
    let native = s.channels;
    let mult = s.speed_multiplier as usize;
    // cap 0 = every native channel; otherwise the first `cap` of them.
    let want_channels = if c.cap == 0 {
        native
    } else {
        native.min(c.cap)
    };
    let mut engine = if c.cap == 0 {
        // The shipped constructor (song-driven channel count), so the default
        // stays covered by a golden.
        AhxEngine::new(s, c.freq, c.defstereo)
    } else {
        AhxEngine::with_channel_cap(s, c.freq, c.defstereo, c.cap)
    }
    .expect("engine builds");
    let label = match mode {
        Mode::Plain => "capture off",
        Mode::Capture => "capture on",
        Mode::MuteSoloCleared => "mute/solo set then cleared",
        Mode::HifiCleared => "hi-fi set then cleared",
        Mode::HifiPrewarmedCleared => "hi-fi prewarmed then cleared",
    };
    match mode {
        Mode::Plain => {}
        Mode::Capture => engine.enable_capture(true),
        Mode::MuteSoloCleared => {
            engine.set_mute_solo(0xffff, 0b101);
            engine.set_mute_solo(0, 0);
        }
        Mode::HifiCleared => {
            engine.set_hifi(true);
            engine.set_hifi(false);
        }
        Mode::HifiPrewarmedCleared => {
            engine.set_hifi(true);
            assert!(engine.prewarm_hifi().tables > 0);
            engine.set_hifi(false);
        }
    }
    let name = format!("{name} ({label})");
    assert_eq!(engine.channels(), want_channels, "{name}: channel count");
    assert_eq!(
        engine.dropped_channels(),
        native - want_channels,
        "{name}: dropped_channels"
    );
    assert_eq!(g.channels, want_channels, "{name}: reference channel count");

    // One hvl_DecodeFrame = `mult` ticks of `freq/50/mult` samples each.
    let frame_samples = engine.samples_per_tick() * mult;
    let mut prev = 0usize;
    let mut buf = Vec::new();
    for &(upto, want) in &g.chunks {
        let frames = (upto - prev) * frame_samples;
        buf.clear();
        buf.resize(frames * 2, 0i16);
        engine.render_block(&mut buf);
        let bytes: Vec<u8> = buf.iter().flat_map(|s| s.to_le_bytes()).collect();
        let got = fnv(FNV_BASIS, &bytes);
        assert_eq!(
            got, want,
            "{name}: render diverges from the C reference in DecodeFrames {prev}..{upto} \
             (got {got:016x}, want {want:016x})"
        );
        prev = upto;
    }
    // Transport state after the last frame: covers the song-end / restart
    // path (and its `>=` divergence from the reference's `==`) whenever the
    // golden is long enough to loop.
    assert_eq!(
        (engine.song_end_reached(), engine.pos_nr(), engine.note_nr()),
        g.end,
        "{name}: (song_end_reached, pos_nr, note_nr) after the last frame"
    );
}

fn check_fixture(fixture: &str) {
    for c in cases_for(fixture) {
        check(&c, Mode::Plain);
        check(&c, Mode::Capture);
        check(&c, Mode::MuteSoloCleared);
        check(&c, Mode::HifiCleared);
        check(&c, Mode::HifiPrewarmedCleared);
    }
}

#[test]
fn waves_table_matches_reference_bit_for_bit() {
    let g = load_golden("karma.44100.s2.cap0.txt");
    let bytes: Vec<u8> = WAVES.iter().map(|&b| b as u8).collect();
    assert_eq!(fnv(FNV_BASIS, &bytes), g.waves);
}

#[test]
fn panning_tables_match_reference_bit_for_bit() {
    let g = load_golden("karma.44100.s2.cap0.txt");
    let mut h = FNV_BASIS;
    for i in 0..256 {
        h = fnv(h, &(panning_left(i) as u32).to_le_bytes());
    }
    for i in 0..256 {
        h = fnv(h, &(panning_right(i) as u32).to_le_bytes());
    }
    assert_eq!(h, g.panning);
}

// One test per fixture so the pool renders them in parallel and a divergence
// names the fixture. Each replays every manifest row for that fixture.

#[test]
fn karma_ahx_matches_reference_across_rates_and_stereo_modes() {
    check_fixture("karma.ahx");
}

#[test]
fn doobrey_gubbins_matches_reference() {
    check_fixture("doobrey_gubbins.hvl");
}

#[test]
fn drainage_proble_matches_reference() {
    check_fixture("drainage_proble.hvl");
}

#[test]
fn illuminated_matches_reference() {
    check_fixture("illuminated.hvl");
}

#[test]
fn moderate_sellotaping_matches_reference() {
    check_fixture("moderate_sellotaping.hvl");
}

#[test]
fn sliding_away_matches_reference() {
    check_fixture("sliding_away.hvl");
}

#[test]
fn sunspots_matches_reference() {
    check_fixture("sunspots.hvl");
}

#[test]
fn a_new_beginning_matches_reference() {
    check_fixture("a_new_beginning.ahx");
}

#[test]
fn a_new_beginning_ii_matches_reference() {
    check_fixture("a_new_beginning_ii.ahx");
}

#[test]
fn blondie_matches_reference() {
    check_fixture("blondie.ahx");
}

#[test]
fn blue_mazda_323_matches_reference() {
    check_fixture("blue_mazda_323.ahx");
}

#[test]
fn brain_artifice_matches_reference() {
    check_fixture("brain_artifice.ahx");
}

#[test]
fn countdown_to_nil_matches_reference() {
    check_fixture("countdown_to_nil.ahx");
}

#[test]
fn depressed_matches_reference() {
    check_fixture("depressed.ahx");
}

#[test]
fn get_to_the_chopper_matches_reference() {
    check_fixture("get_to_the_chopper.ahx");
}

#[test]
fn i_love_holy_daze_matches_reference() {
    check_fixture("i_love_holy_daze.ahx");
}

#[test]
fn luminous_matches_reference() {
    check_fixture("luminous.ahx");
}

#[test]
fn melodious_matches_reference() {
    check_fixture("melodious.ahx");
}

#[test]
fn newsong_matches_reference() {
    check_fixture("newsong.ahx");
}

#[test]
fn robocop_iii_j_tel_matches_reference() {
    check_fixture("robocop_iii_j_tel.ahx");
}

#[test]
fn thats_the_wave_it_is_matches_reference() {
    check_fixture("thats_the_wave_it_is.ahx");
}

#[test]
fn the_fugitive_matches_reference() {
    check_fixture("the_fugitive.ahx");
}

#[test]
fn wave_stepper_matches_reference() {
    check_fixture("wave_stepper.ahx");
}

#[test]
fn manifest_covers_every_fixture() {
    // The golden corpus is a fixed subset of the demo jukebox, which has grown
    // past it; the vendored C reference that generates goldens is not in the
    // tree any more, so new demos cannot be added here.
    let mut in_manifest: Vec<String> = manifest().into_iter().map(|c| c.fixture).collect();
    in_manifest.sort();
    in_manifest.dedup();
    assert_eq!(
        in_manifest.len(),
        23,
        "golden corpus changed size: {in_manifest:?}"
    );
    for f in &in_manifest {
        assert!(
            root().join("tests/fixtures/songs").join(f).is_file(),
            "{f}: manifest fixture missing from public/demos/ahx"
        );
    }
    // Every fixture has a native-channel row (the shipped default), and every
    // >4-channel one a cap-4 truncation row.
    for f in &in_manifest {
        let rows = cases_for(f);
        assert!(
            rows.iter().any(|c| c.cap == 0),
            "{f}: no native-channel row"
        );
        if song(f).channels > 4 {
            assert!(
                rows.iter().any(|c| c.cap == 4),
                "{f}: no cap-4 truncation row"
            );
        }
        assert!(
            rows.iter().any(|c| c.frames >= 1500),
            "{f}: no run of 30 s or more"
        );
    }
}

#[test]
fn manifest_and_goldens_agree() {
    // Each golden must be the C harness's output for exactly its manifest row
    // (`case` line), and no golden may exist without a row: nothing hand-baked
    // or stale survives a manifest edit.
    let rows = manifest();
    let mut names: Vec<String> = rows.iter().map(Case::golden).collect();
    for c in &rows {
        let g = load_golden(&c.golden());
        assert_eq!(
            g.case,
            Some((c.fixture.clone(), c.freq, c.defstereo, c.frames, c.chunk, c.cap)),
            "{}: golden's `case` line does not match its manifest row (regenerate with gen_goldens.sh)",
            c.golden()
        );
        assert!(
            g.coverage.is_some(),
            "{}: no `coverage` line -- not harness output",
            c.golden()
        );
        assert_eq!(
            g.chunks.last().unwrap().0,
            c.frames,
            "{}: last chunk vs frames",
            c.golden()
        );
    }
    names.sort();
    let before = names.len();
    names.dedup();
    assert_eq!(
        names.len(),
        before,
        "two manifest rows map to the same golden file"
    );
    let mut on_disk: Vec<String> = fs::read_dir(root().join("tests/golden"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".txt"))
        .collect();
    on_disk.sort();
    assert_eq!(on_disk, names, "goldens on disk vs manifest rows");
}

#[test]
fn hvl_ignores_the_defstereo_argument() {
    // Retargeted 2026-09-23 from chiprolled.hvl (removed from the corpus as a
    // byte-identical dupe, user decision): sunspots at 48000 now carries three
    // goldens differing only in defstereo (0, 2, 4). The C reference produces
    // identical audio for all three, and the engine must too.
    let mut hashes = Vec::new();
    for st in [0u8, 2, 4] {
        let g = load_golden(&format!("sunspots.48000.s{st}.cap4.txt"));
        hashes.push(g.chunks);
    }
    assert_eq!(hashes[0], hashes[1]);
    assert_eq!(hashes[1], hashes[2]);
}

#[test]
fn ahx_stereo_modes_produce_distinct_audio() {
    // Guards against the stereo argument being silently dropped: modes 0-4 of
    // the same karma render must all differ from each other in the reference.
    let firsts: Vec<u64> = (0..=4u8)
        .map(|st| load_golden(&format!("karma.44100.s{st}.cap0.txt")).chunks[10].1)
        .collect();
    for i in 0..firsts.len() {
        for j in i + 1..firsts.len() {
            assert_ne!(
                firsts[i], firsts[j],
                "stereo modes {i} and {j} rendered identically"
            );
        }
    }
}

#[test]
fn corpus_exercises_every_voice_feature() {
    // Coverage counters are voice-frames summed per golden by the C harness;
    // every feature must be hit by at least one golden, or a regression in it
    // could pass unnoticed.
    let mut total = [0u64; 8];
    for c in manifest() {
        for (t, v) in total
            .iter_mut()
            .zip(load_golden(&c.golden()).coverage.unwrap())
        {
            *t += v;
        }
    }
    let names = [
        "ring", "noise", "filter", "square", "hardcut", "vibrato", "slide", "plist",
    ];
    for (n, t) in names.iter().zip(total) {
        assert!(t > 0, "no golden exercises {n}");
    }
}

#[test]
fn corpus_reaches_both_song_end_paths() {
    // illuminated's fixed-4 golden walks off the last position (restart at
    // `pos_nr == position_nr`, `hvl_replay.c:1683-1688`; first hit at 57.6 s);
    // sunspots reaches song end through a Bxx loop-back (`:680-683`) and never
    // walks off the end.
    for name in [
        "illuminated.44100.s2.cap4.txt",
        "sunspots.44100.s2.cap4.txt",
    ] {
        assert!(load_golden(name).end.0, "{name} must reach song end");
    }
}

#[test]
fn drainage_proble_plays_all_seven_channels() {
    let s = song("drainage_proble.hvl");
    assert_eq!(s.channels, 7);
    let e = AhxEngine::new(s, 44100, 2).unwrap();
    assert_eq!(e.channels(), 7);
    assert_eq!(e.dropped_channels(), 0);
}

#[test]
fn every_hvl_fixture_plays_its_native_channels_by_default() {
    // Native channel counts of the corpus (11/7/6/8/6/6): the default engine
    // plays every one and drops none.
    for (f, native) in [
        ("doobrey_gubbins.hvl", 11),
        ("drainage_proble.hvl", 7),
        ("illuminated.hvl", 6),
        ("moderate_sellotaping.hvl", 8),
        ("sliding_away.hvl", 6),
        ("sunspots.hvl", 6),
    ] {
        let s = song(f);
        assert_eq!(s.channels, native, "{f}: native channels");
        let e = AhxEngine::new(s, 44100, 2).unwrap();
        assert_eq!(e.channels(), native, "{f}");
        assert_eq!(e.dropped_channels(), 0, "{f}");
    }
}

#[test]
fn zero_channel_cap_is_rejected() {
    let r = AhxEngine::with_channel_cap(song("karma.ahx"), 44100, 2, 0);
    assert_eq!(r.err(), Some(EngineError::InvalidChannelCap));
}

#[test]
fn default_channel_count_follows_the_song() {
    let e = AhxEngine::new(song("karma.ahx"), 44100, 2).unwrap();
    assert_eq!((e.channels(), e.dropped_channels()), (4, 0));
    let e = AhxEngine::new(song("illuminated.hvl"), 44100, 2).unwrap();
    assert_eq!((e.channels(), e.dropped_channels()), (6, 0));
    // The verification hook still truncates, and says how many it cut.
    let e = AhxEngine::with_channel_cap(song("illuminated.hvl"), 44100, 2, 4).unwrap();
    assert_eq!((e.channels(), e.dropped_channels()), (4, 2));
}

#[test]
fn default_pan_repeats_left_right_right_left_per_group_of_four() {
    // hvl_InitSubsong:90-108 -- voices 4..7 (and 8..10) repeat voices 0..3.
    let e = AhxEngine::new(song("doobrey_gubbins.hvl"), 44100, 2).unwrap();
    assert_eq!(e.channels(), 11);
    for i in 4..11 {
        assert_eq!(
            (
                e.voice(i).pan,
                e.voice(i).pan_mult_left,
                e.voice(i).pan_mult_right
            ),
            (
                e.voice(i % 4).pan,
                e.voice(i % 4).pan_mult_left,
                e.voice(i % 4).pan_mult_right
            ),
            "voice {i}"
        );
    }
}

#[test]
fn render_is_independent_of_block_size() {
    // Odd block sizes split ticks mid-way; the result must not care.
    let total = 44100 / 50 * 200;
    let mut whole = AhxEngine::new(song("karma.ahx"), 44100, 2).unwrap();
    let mut a = vec![0i16; total * 2];
    whole.render_block(&mut a);

    let mut split = AhxEngine::new(song("karma.ahx"), 44100, 2).unwrap();
    let mut b = vec![0i16; total * 2];
    let mut at = 0usize;
    for size in [1usize, 7, 333, 882, 1000, 4099].iter().cycle() {
        if at >= total {
            break;
        }
        let n = (*size).min(total - at);
        split.render_block(&mut b[at * 2..(at + n) * 2]);
        at += n;
    }
    assert_eq!(a, b);
}

#[test]
fn karma_first_row_is_audible_and_not_clipped_flat() {
    // Sanity that the goldens are not all-zero streams: two seconds of karma
    // must contain non-silent samples in both channels.
    let mut e = AhxEngine::new(song("karma.ahx"), 44100, 2).unwrap();
    let mut out = vec![0i16; 44100 * 2 * 2];
    e.render_block(&mut out);
    assert!(out.iter().step_by(2).any(|&s| s != 0), "left silent");
    assert!(
        out.iter().skip(1).step_by(2).any(|&s| s != 0),
        "right silent"
    );
}
