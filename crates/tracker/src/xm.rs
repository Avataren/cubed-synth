//! FastTracker 2 modules (`.xm`): instruments with keymapped samples (8- and
//! 16-bit, delta-coded), envelopes, fadeout and auto-vibrato; patterns of
//! any length.

use crate::module::{
    AutoVibrato, Cell, Envelope, Flags, Format, Instrument, LoopKind, Module, Note, Pattern, Sample,
};

const SIGNATURE: &[u8] = b"Extended Module: ";
const HEADER_AT: usize = 60;
const MIN_SIZE: usize = 336;
const KEY_OFF: u8 = 97;

/// Whether `bytes` look like an XM.
pub fn looks_like_xm(bytes: &[u8]) -> bool {
    bytes.len() >= MIN_SIZE && bytes.starts_with(SIGNATURE) && bytes[37] == 0x1a
}

fn le16(bytes: &[u8], at: usize) -> usize {
    bytes
        .get(at..at + 2)
        .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) as usize)
}

fn le32(bytes: &[u8], at: usize) -> usize {
    bytes
        .get(at..at + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

fn byte(bytes: &[u8], at: usize) -> u8 {
    bytes.get(at).copied().unwrap_or(0)
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

/// A raw XM cell: note, instrument, volume column, effect, parameter.
#[derive(Clone, Copy, Default)]
struct Raw([u8; 5]);

fn decode_pattern(
    bytes: &[u8],
    at: usize,
    packed: usize,
    rows: usize,
    channels: usize,
) -> Vec<Raw> {
    let mut cells = vec![Raw::default(); rows * channels];
    let (mut at, end) = (at, at + packed);
    for cell in cells.iter_mut() {
        if packed == 0 || at >= end {
            break;
        }
        let flags = byte(bytes, at);
        at += 1;
        let mut take = || {
            let b = byte(bytes, at);
            at += 1;
            b
        };
        if flags & 0x80 != 0 {
            for (bit, field) in cell.0.iter_mut().enumerate() {
                if flags & (1 << bit) != 0 {
                    *field = take();
                }
            }
        } else {
            cell.0 = [flags, take(), take(), take(), take()];
        }
    }
    cells
}

fn envelope(
    bytes: &[u8],
    points_at: usize,
    count: u8,
    sustain: u8,
    loop_start: u8,
    loop_end: u8,
    flags: u8,
) -> Option<Envelope> {
    let points: Vec<(u16, u16)> = (0..count.min(12) as usize)
        .map(|i| {
            (
                le16(bytes, points_at + i * 4) as u16,
                le16(bytes, points_at + i * 4 + 2) as u16,
            )
        })
        .collect();
    if flags & 0x01 == 0 || points.is_empty() {
        return None;
    }
    Some(Envelope {
        sustain: (flags & 0x02 != 0).then_some(sustain as usize),
        looped: (flags & 0x04 != 0).then_some((loop_start as usize, loop_end as usize)),
        points,
    })
}

/// Parses an XM file.
pub fn parse(bytes: &[u8]) -> Result<Module, String> {
    if !looks_like_xm(bytes) {
        return Err("not an XM file".into());
    }
    let title = ascii(&bytes[17..37]);
    let header_size = le32(bytes, HEADER_AT);
    let song_length = le16(bytes, 64);
    let channels = le16(bytes, 68);
    let pattern_count = le16(bytes, 70).min(256);
    let instrument_count = le16(bytes, 72).min(256);
    let flags = le16(bytes, 74);
    let speed = le16(bytes, 76);
    let bpm = le16(bytes, 78);
    if !(1..=32).contains(&channels) {
        return Err(format!("unsupported XM channel count {channels}"));
    }
    let orders_raw: Vec<usize> = (0..256).map(|i| byte(bytes, 80 + i) as usize).collect();

    let mut at = HEADER_AT + header_size;
    let mut patterns = Vec::with_capacity(pattern_count);
    for _ in 0..pattern_count {
        if at + 9 > bytes.len() {
            break;
        }
        let size = le32(bytes, at);
        if size < 9 {
            break;
        }
        let rows = le16(bytes, at + 5).clamp(1, 256);
        let packed = le16(bytes, at + 7);
        let raw = decode_pattern(bytes, at + size, packed, rows, channels);
        let mut pattern = Pattern::new(rows, channels);
        for (i, r) in raw.iter().enumerate() {
            let [note, instrument, volume, effect, param] = r.0;
            *pattern.cell_mut(i / channels, i % channels) = Cell {
                note: match note {
                    1..=96 => Note::On(note + 11),
                    KEY_OFF => Note::Off,
                    _ => Note::None,
                },
                period: 0,
                instrument,
                volume,
                effect,
                param,
            };
        }
        patterns.push(pattern);
        at += size + packed;
    }

    let mut samples = Vec::new();
    let mut instruments = Vec::with_capacity(instrument_count);
    for _ in 0..instrument_count {
        if at + 29 > bytes.len() {
            break;
        }
        let size = le32(bytes, at);
        let name = ascii(&bytes[at + 4..at + 26]);
        let sample_count = le16(bytes, at + 27).min(16);
        let mut instrument = Instrument {
            name,
            keymap: [0; 96],
            ..Instrument::default()
        };
        let mut sample_header_size = 40;
        if sample_count > 0 && at + 243 <= bytes.len() {
            sample_header_size = match le32(bytes, at + 29) {
                0 => 40,
                n => n,
            };
            for (n, k) in instrument.keymap.iter_mut().enumerate() {
                *k = byte(bytes, at + 33 + n);
            }
            let b = |o: usize| byte(bytes, at + o);
            instrument.volume_envelope =
                envelope(bytes, at + 129, b(225), b(227), b(228), b(229), b(233));
            instrument.panning_envelope =
                envelope(bytes, at + 177, b(226), b(230), b(231), b(232), b(234));
            instrument.vibrato = AutoVibrato {
                kind: b(235),
                sweep: b(236),
                depth: b(237),
                rate: b(238),
            };
            instrument.fadeout = le16(bytes, at + 239) as u16;
        }
        let mut header_at = at + size;
        let mut headers = Vec::with_capacity(sample_count);
        for _ in 0..sample_count {
            if header_at + 18 > bytes.len() {
                break;
            }
            let length = le32(bytes, header_at);
            let kind = byte(bytes, header_at + 14);
            let bits16 = kind & 0x10 != 0;
            let width = if bits16 { 2 } else { 1 };
            let loop_bits = kind & 0x03;
            headers.push((
                length,
                bits16,
                Sample {
                    name: ascii(bytes.get(header_at + 18..header_at + 40).unwrap_or(&[])),
                    volume: byte(bytes, header_at + 12),
                    finetune: byte(bytes, header_at + 13) as i8,
                    relative_note: byte(bytes, header_at + 16) as i8,
                    pan: Some(byte(bytes, header_at + 15)),
                    loop_kind: if loop_bits & 2 != 0 {
                        LoopKind::PingPong
                    } else if loop_bits == 1 {
                        LoopKind::Forward
                    } else {
                        LoopKind::Off
                    },
                    loop_start: le32(bytes, header_at + 4) / width,
                    loop_len: le32(bytes, header_at + 8) / width,
                    ..Sample::default()
                },
            ));
            header_at += sample_header_size;
        }
        let mut data_at = header_at;
        for (length, bits16, mut sample) in headers {
            if length > 0 && data_at + length <= bytes.len() {
                sample.data = decode_delta(&bytes[data_at..data_at + length], bits16);
            }
            data_at += length;
            if sample.loop_len == 0 {
                sample.loop_kind = LoopKind::Off;
            }
            if sample.loop_kind == LoopKind::Off {
                sample.loop_start = 0;
                sample.loop_len = 0;
            }
            instrument.samples.push(samples.len());
            samples.push(sample);
        }
        instruments.push(instrument);
        at = data_at;
    }

    let order_length = song_length.min(256);
    Ok(Module {
        format: Format::Xm,
        title,
        channels,
        orders: orders_raw[..order_length]
            .iter()
            .copied()
            .filter(|&p| p < patterns.len())
            .collect(),
        patterns,
        samples,
        instruments,
        speed: if speed == 0 { 6 } else { speed.min(255) as u8 },
        tempo: if bpm == 0 { 125 } else { bpm.min(255) as u8 },
        channel_pan: vec![None; channels],
        adlib_channels: vec![None; channels],
        muted: vec![false; channels],
        jump_targets: None,
        global_volume: 1.0,
        adlib_gain: 0.0,
        stereo_separation: 1.0,
        flags: Flags {
            linear_frequency: flags & 0x01 != 0,
            ..Flags::default()
        },
        mod_flavor: None,
        tracker_version: le16(bytes, 58) as u16,
    })
}

/// XM's delta-coded PCM: each value is the difference from the last.
fn decode_delta(bytes: &[u8], bits16: bool) -> Vec<f32> {
    if bits16 {
        let mut running = 0u16;
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| {
                running = running.wrapping_add(u16::from_le_bytes([b[0], b[1]]));
                running as i16 as f32 / 32768.0
            })
            .collect()
    } else {
        let mut running = 0u8;
        bytes
            .iter()
            .map(|&b| {
                running = running.wrapping_add(b);
                running as i8 as f32 / 128.0
            })
            .collect()
    }
}
