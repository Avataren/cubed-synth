//! ProTracker-family MOD files: 4-channel M.K./N.T./FLT4 and friends, the
//! 15-sample Soundtracker layout, and multi-channel `xCHN`/`xxCH`/`TDZx`
//! variants up to 32 channels. Startrekker's FLT8 (two 4-channel pattern
//! halves) is refused rather than misread.

use crate::module::{Cell, Flags, Format, LoopKind, ModFlavor, Module, Note, Pattern, Sample};
use crate::pitch::{AMIGA_CLOCK, PAULA_TO_SYNTH_SCALE};

const PT_HEADER_SIZE: usize = 1084;
const ST_HEADER_SIZE: usize = 600;
const ROWS: usize = 64;
pub const MAX_CHANNELS: usize = 32;
/// The periods ProTracker's own table spans, C-1 to B-3.
const MAX_AMIGA_PERIOD: u16 = 856;
const MIN_AMIGA_PERIOD: u16 = 113;

/// The channel count a signature at offset 1080 implies.
fn channels_for_signature(sig: &[u8; 4]) -> Option<usize> {
    match sig {
        b"M.K." | b"M!K!" | b"M&K!" | b"N.T." | b"FLT4" | b"4CHN" => return Some(4),
        b"CD81" | b"OKTA" | b"OCTA" => return Some(8),
        _ => {}
    }
    let digit = |b: u8| b.is_ascii_digit().then(|| (b - b'0') as usize);
    if &sig[1..] == b"CHN" {
        return digit(sig[0]).filter(|&n| n >= 1);
    }
    if sig[2] == b'C' && (sig[3] == b'H' || sig[3] == b'N') {
        let n = digit(sig[0])? * 10 + digit(sig[1])?;
        return (1..=MAX_CHANNELS).contains(&n).then_some(n);
    }
    if &sig[..3] == b"TDZ" {
        return digit(sig[3]).filter(|n| (1..=3).contains(n));
    }
    None
}

fn signature(bytes: &[u8]) -> [u8; 4] {
    let at = |i: usize| bytes.get(i).copied().unwrap_or(0);
    [at(1080), at(1081), at(1082), at(1083)]
}

fn be16(bytes: &[u8], at: usize) -> usize {
    ((bytes[at] as usize) << 8) | bytes[at + 1] as usize
}

fn looks_like_protracker(bytes: &[u8]) -> bool {
    if bytes.len() < PT_HEADER_SIZE {
        return false;
    }
    let sig = signature(bytes);
    &sig == b"FLT8" || channels_for_signature(&sig).is_some()
}

fn looks_like_soundtracker(bytes: &[u8]) -> bool {
    if bytes.len() < ST_HEADER_SIZE {
        return false;
    }
    let sig = signature(bytes);
    if &sig == b"FLT8" || channels_for_signature(&sig).is_some() {
        return false;
    }
    let song_length = bytes[470];
    if !(1..=128).contains(&song_length) {
        return false;
    }
    let mut max_pattern = 0;
    for &p in &bytes[472..600] {
        if p > 63 {
            return false;
        }
        max_pattern = max_pattern.max(p as usize);
    }
    let sample_data = ST_HEADER_SIZE + (max_pattern + 1) * ROWS * 4 * 4;
    if sample_data > bytes.len() {
        return false;
    }
    let total: usize = (0..15).map(|i| be16(bytes, 20 + i * 30 + 22) * 2).sum();
    sample_data + total <= bytes.len()
}

/// Whether `bytes` look like a MOD this parser reads.
pub fn looks_like_mod(bytes: &[u8]) -> bool {
    looks_like_protracker(bytes) || looks_like_soundtracker(bytes)
}

/// A MOD's sample header.
struct Header {
    name: String,
    length: usize,
    finetune: i8,
    volume: u8,
    loop_start: usize,
    loop_length: usize,
}

/// Raw cell as stored: period, sample number, effect, parameter.
#[derive(Clone, Copy)]
struct Raw {
    period: u16,
    sample: u8,
    effect: u8,
    param: u8,
}

/// Parses a MOD file.
pub fn parse(bytes: &[u8]) -> Result<Module, String> {
    if !looks_like_mod(bytes) {
        return Err("not a MOD file".into());
    }
    let title = ascii(&bytes[..20]);
    let sig = signature(bytes);
    if &sig == b"FLT8" {
        return Err("Startrekker FLT8 modules are not supported".into());
    }
    let signature_channels = channels_for_signature(&sig);
    let protracker = signature_channels.is_some();
    let sample_count = if protracker { 31 } else { 15 };
    let header_size = if protracker {
        PT_HEADER_SIZE
    } else {
        ST_HEADER_SIZE
    };
    let (length_at, orders_at) = if protracker { (950, 952) } else { (470, 472) };

    let mut headers: Vec<Header> = (0..sample_count)
        .map(|i| {
            let at = 20 + i * 30;
            let ft = bytes[at + 24] & 0x0f;
            Header {
                name: ascii(&bytes[at..at + 22]),
                length: be16(bytes, at + 22) * 2,
                finetune: if ft < 8 { ft as i8 } else { ft as i8 - 16 },
                volume: bytes[at + 25],
                loop_start: be16(bytes, at + 26) * 2,
                loop_length: be16(bytes, at + 28) * 2,
            }
        })
        .collect();

    let song_length = bytes[length_at] as usize;
    let all_orders: Vec<usize> = bytes[orders_at..orders_at + 128]
        .iter()
        .map(|&p| p as usize)
        .collect();
    let pattern_count = all_orders.iter().max().copied().unwrap_or(0) + 1;
    let channels = signature_channels.unwrap_or(4);
    if !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(format!("unsupported MOD channel count {channels}"));
    }

    let mut flavor = if !protracker {
        ModFlavor::Soundtracker
    } else if &sig == b"N.T." {
        ModFlavor::NoiseTracker
    } else {
        ModFlavor::ProTracker
    };

    let pattern_size = ROWS * channels * 4;
    let mut raw: Vec<Vec<Raw>> = Vec::with_capacity(pattern_count);
    for p in 0..pattern_count {
        let base = header_size + p * pattern_size;
        let cells = (0..ROWS * channels)
            .map(|i| {
                let at = base + i * 4;
                match bytes.get(at..at + 4) {
                    Some(b) => Raw {
                        period: (((b[0] & 0x0f) as u16) << 8) | b[1] as u16,
                        sample: (b[0] & 0xf0) | (b[2] >> 4),
                        effect: b[2] & 0x0f,
                        param: b[3],
                    },
                    None => Raw {
                        period: 0,
                        sample: 0,
                        effect: 0,
                        param: 0,
                    },
                }
            })
            .collect();
        raw.push(cells);
    }

    // Ultimate Soundtracker shows itself only in the data: a loop that fits
    // when read as bytes but not as words, or arpeggio on command 1.
    if !protracker && (loops_look_like_bytes(&headers) || uses_ust_commands(&raw)) {
        flavor = ModFlavor::UltimateSoundtracker;
        for h in &mut headers {
            h.loop_start /= 2;
        }
    }

    let mut at = header_size + pattern_count * pattern_size;
    let samples = headers
        .iter()
        .map(|h| {
            let data = match bytes.get(at..at + h.length) {
                Some(pcm) if h.length > 0 => pcm.iter().map(|&b| b as i8 as f32 / 128.0).collect(),
                _ => Vec::new(),
            };
            at += h.length;
            // ProTracker marks "no loop" with a length of one word or less.
            let looped = h.loop_length > 2;
            Sample {
                name: h.name.clone(),
                data,
                volume: h.volume,
                finetune: h.finetune,
                c2spd: 0,
                relative_note: 0,
                pan: None,
                adlib: None,
                loop_kind: if looped {
                    LoopKind::Forward
                } else {
                    LoopKind::Off
                },
                loop_start: if looped { h.loop_start } else { 0 },
                loop_len: if looped { h.loop_length } else { 0 },
            }
        })
        .collect();

    let amiga_limits = uses_amiga_limits(&sig, &headers, &raw);
    let ust = flavor == ModFlavor::UltimateSoundtracker;
    let patterns = raw
        .iter()
        .map(|cells| {
            let mut pattern = Pattern::new(ROWS, channels);
            for (i, r) in cells.iter().enumerate() {
                let (effect, param) = if ust {
                    ust_effect(r.effect, r.param)
                } else {
                    (r.effect, r.param)
                };
                *pattern.cell_mut(i / channels, i % channels) = Cell {
                    note: period_to_midi(r.period).map_or(Note::None, Note::On),
                    period: r.period,
                    instrument: r.sample,
                    volume: 0,
                    effect,
                    param,
                };
            }
            pattern
        })
        .collect();

    let order_length = if song_length == 0 {
        128
    } else {
        song_length.min(128)
    };
    let mut module = Module {
        format: Format::Mod,
        title,
        channels,
        orders: all_orders[..order_length]
            .iter()
            .copied()
            .filter(|&p| p < pattern_count)
            .collect(),
        patterns,
        samples,
        instruments: Vec::new(),
        speed: 6,
        tempo: 125,
        channel_pan: (0..channels)
            .map(|c| Some(channel_pan(c, channels)))
            .collect(),
        adlib_channels: vec![None; channels],
        muted: vec![false; channels],
        jump_targets: None,
        global_volume: 1.0,
        adlib_gain: 0.0,
        // Full separation is the Amiga's; three quarters is easier listening.
        stereo_separation: if channels <= 4 { 0.75 } else { 1.0 },
        flags: Flags {
            amiga_limits,
            vblank_timing: false,
            fast_volume_slides: false,
            linear_frequency: false,
        },
        mod_flavor: Some(flavor),
        tracker_version: 0,
    };
    module.flags.vblank_timing = uses_vblank_timing(&module);
    Ok(module)
}

fn ascii(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end]
        .iter()
        .map(|&b| b as char)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn loops_look_like_bytes(headers: &[Header]) -> bool {
    headers.iter().any(|h| {
        h.length > 0
            && h.loop_length > 2
            && h.loop_start + h.loop_length > h.length
            && h.loop_start / 2 + h.loop_length <= h.length
    })
}

fn uses_ust_commands(patterns: &[Vec<Raw>]) -> bool {
    let mut one = false;
    for cell in patterns.iter().flatten() {
        if cell.param == 0 {
            continue;
        }
        if cell.effect == 0 {
            return false;
        }
        one |= cell.effect == 1;
    }
    one
}

/// Ultimate Soundtracker's commands in ProTracker's numbering: 1 is
/// arpeggio, 2 a pitch bend whose direction is the nibble set, 0 nothing.
fn ust_effect(effect: u8, param: u8) -> (u8, u8) {
    match effect {
        0 => (0, 0),
        1 => (0, param),
        2 => {
            let (down, up) = (param >> 4, param & 0x0f);
            if down != 0 {
                (2, down)
            } else if up != 0 {
                (1, up)
            } else {
                (0, 0)
            }
        }
        _ => (effect, param),
    }
}

/// OpenMPT's ProTracker-shape test (`ReadMOD`): an M.K./M!K!/PATT file
/// with no zero-length loops and every note inside ProTracker's octaves
/// plays inside them; anything else gets the seven-octave PC range.
fn uses_amiga_limits(sig: &[u8; 4], headers: &[Header], patterns: &[Vec<Raw>]) -> bool {
    if sig != b"M.K." && sig != b"M!K!" && sig != b"PATT" {
        return false;
    }
    if headers.iter().any(|h| h.length > 0 && h.loop_length == 0) {
        return false;
    }
    patterns
        .iter()
        .flatten()
        .all(|c| c.period == 0 || (MIN_AMIGA_PERIOD..=MAX_AMIGA_PERIOD).contains(&c.period))
}

/// Paula's rate for a period, in the player's musical Hz.
pub(crate) fn period_to_frequency(period: u16) -> Option<f64> {
    if period == 0 {
        return None;
    }
    Some(AMIGA_CLOCK / (2.0 * period as f64 * PAULA_TO_SYNTH_SCALE))
}

/// The nearest MIDI note to a period (C-1 is 24, as ProTracker names it).
fn period_to_midi(period: u16) -> Option<u8> {
    let freq = period_to_frequency(period)?;
    let midi = (69.0 + 12.0 * (freq / 440.0).log2()).round();
    (0.0..=127.0).contains(&midi).then_some(midi as u8)
}

/// Paula wires its voices hard left, right, right, left; more than four
/// channels means a PC tracker, whose channels start centred.
fn channel_pan(channel: usize, count: usize) -> f32 {
    match count {
        0 | 1 => 0.5,
        2 => [0.0, 1.0][channel],
        3 => [0.0, 0.5, 1.0][channel],
        4 if channel == 0 || channel == 3 => 0.0,
        4 => 1.0,
        _ => 0.5,
    }
}

/// Whether a 4-channel ProTracker module runs off the vertical blank:
/// it has tempo-range Fxx commands that make no sense as tempos (none at
/// 100 BPM or more), read as tempos it would run implausibly long, and read
/// as speeds it gets shorter.
fn uses_vblank_timing(module: &Module) -> bool {
    if module.channels != 4 {
        return false;
    }
    if !matches!(
        module.mod_flavor,
        Some(ModFlavor::ProTracker | ModFlavor::NoiseTracker)
    ) {
        return false;
    }
    let mut low_tempo = false;
    let mut seen = Vec::new();
    for &p in &module.orders {
        if seen.contains(&p) {
            continue;
        }
        seen.push(p);
        let Some(pattern) = module.patterns.get(p) else {
            continue;
        };
        for row in 0..pattern.rows() {
            for cell in pattern.row(row) {
                if cell.effect != 0x0f {
                    continue;
                }
                if cell.param >= 100 {
                    return false;
                }
                low_tempo |= cell.param >= 0x20;
            }
        }
    }
    if !low_tempo {
        return false;
    }
    let cia = estimate_seconds(module, false);
    cia >= 480.0 && estimate_seconds(module, true) < cia
}

/// The song's length, following jumps, breaks, loops and delays.
fn estimate_seconds(module: &Module, vblank: bool) -> f64 {
    let (mut speed, mut bpm, mut seconds) = (6.0, 125.0, 0.0);
    let (mut order, mut row, mut loop_start, mut loop_count) = (0usize, 0usize, 0usize, 0u8);
    let mut visited = vec![false; module.orders.len() * ROWS];
    for _ in 0..200_000 {
        if order >= module.orders.len() || visited[order * ROWS + row] {
            break;
        }
        visited[order * ROWS + row] = true;
        let pattern = &module.patterns[module.orders[order]];
        let (mut next_order, mut next_row, mut jumped, mut repeats, mut stopped) =
            (order, row + 1, false, 1.0, false);
        for cell in pattern.row(row) {
            let param = cell.param;
            match cell.effect {
                0x0f if param == 0 => stopped = true,
                0x0f if vblank || param < 0x20 => speed = param as f64,
                0x0f => bpm = param as f64,
                0x0b => {
                    next_order = param as usize;
                    next_row = 0;
                    jumped = true;
                }
                0x0d => {
                    next_order = order + 1;
                    next_row = (param >> 4) as usize * 10 + (param & 0x0f) as usize;
                    jumped = true;
                }
                0x0e if param >> 4 == 0x6 => {
                    let count = param & 0x0f;
                    if count == 0 {
                        loop_start = row;
                    } else {
                        loop_count = if loop_count == 0 {
                            count
                        } else {
                            loop_count - 1
                        };
                        if loop_count > 0 {
                            next_order = order;
                            next_row = loop_start;
                            jumped = true;
                        }
                    }
                }
                0x0e if param >> 4 == 0xe => repeats = 1.0 + (param & 0x0f) as f64,
                _ => {}
            }
        }
        seconds += speed * repeats * 2.5 / bpm;
        if stopped {
            break;
        }
        if !jumped && next_row >= ROWS {
            next_row = 0;
            next_order = order + 1;
        }
        order = next_order;
        row = next_row.min(ROWS - 1);
    }
    seconds
}
