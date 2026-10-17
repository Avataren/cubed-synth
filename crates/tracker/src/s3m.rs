//! Scream Tracker 3 modules (`.s3m`): PCM samples (8- and 16-bit), AdLib
//! instruments, and the header's channel layout, pans and flags.
//!
//! Patterns keep the enabled channels only, in order. Stereo and packed
//! samples (which no player decodes) come through silent.

use crate::module::{Adlib, Cell, Flags, Format, LoopKind, Module, Note, Pattern, Sample};

const SIGNATURE_AT: usize = 0x2c;
const MIN_SIZE: usize = 0x70;
const KEY_OFF: u8 = 0xfe;
const NO_NOTE: u8 = 0xff;
const NO_VOLUME: u8 = 0xff;
const ROWS: usize = 64;

/// Whether `bytes` look like an S3M.
pub fn looks_like_s3m(bytes: &[u8]) -> bool {
    bytes.len() >= MIN_SIZE
        && &bytes[SIGNATURE_AT..SIGNATURE_AT + 4] == b"SCRM"
        && bytes[0x1d] == 0x10
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

fn ascii(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end]
        .iter()
        .map(|&b| b as char)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// A cell as the file stores it: each field present or not.
#[derive(Clone, Copy, Debug, Default)]
struct Raw {
    note: Option<u8>,
    instrument: u8,
    volume: Option<u8>,
    effect: u8,
    param: u8,
}

fn decode_pattern(bytes: &[u8], at: usize, packed: usize) -> Vec<[Raw; 32]> {
    let end = bytes
        .len()
        .min(if packed > 0 { at + packed } else { bytes.len() });
    let mut at = at;
    let mut rows = Vec::with_capacity(ROWS);
    for _ in 0..ROWS {
        let mut cells = [Raw::default(); 32];
        while at < end {
            let flag = bytes[at];
            at += 1;
            if flag == 0 {
                break;
            }
            let cell = &mut cells[(flag & 0x1f) as usize];
            let mut take = || {
                let b = bytes.get(at).copied().unwrap_or(0);
                at += 1;
                b
            };
            if flag & 0x20 != 0 {
                cell.note = Some(take());
                cell.instrument = take();
            }
            if flag & 0x40 != 0 {
                cell.volume = Some(take());
            }
            if flag & 0x80 != 0 {
                cell.effect = take();
                cell.param = take();
            }
        }
        rows.push(cells);
    }
    rows
}

/// Parses an S3M file.
pub fn parse(bytes: &[u8]) -> Result<Module, String> {
    if !looks_like_s3m(bytes) {
        return Err("not an S3M file".into());
    }
    let title = ascii(&bytes[..28]);
    let order_count = le16(bytes, 0x20).min(256);
    let instrument_count = le16(bytes, 0x22).min(256);
    let pattern_count = le16(bytes, 0x24).min(256);
    let flags = le16(bytes, 0x26);
    let tracker_version = le16(bytes, 0x28) as u16;
    let format_version = le16(bytes, 0x2a);
    let global_volume = bytes[0x30];
    let initial_speed = bytes[0x31];
    let initial_tempo = bytes[0x32];
    let master_volume = bytes[0x33];
    let has_pan_table = bytes[0x35] == 0xfc;
    let settings: [u8; 32] = std::array::from_fn(|i| bytes.get(0x40 + i).copied().unwrap_or(0xff));
    let orders_raw = bytes.get(0x60..0x60 + order_count).ok_or("truncated S3M")?;
    let pointer = |at: usize| -> Result<usize, String> {
        if at + 2 > bytes.len() {
            return Err("truncated S3M".into());
        }
        Ok(le16(bytes, at) * 16)
    };
    let instrument_base = 0x60 + order_count;
    let instrument_pointers = (0..instrument_count)
        .map(|i| pointer(instrument_base + i * 2))
        .collect::<Result<Vec<_>, _>>()?;
    let pattern_base = instrument_base + instrument_count * 2;
    let pattern_pointers = (0..pattern_count)
        .map(|i| pointer(pattern_base + i * 2))
        .collect::<Result<Vec<_>, _>>()?;
    let pans: [u8; 32] = std::array::from_fn(|i| {
        bytes
            .get(pattern_base + pattern_count * 2 + i)
            .copied()
            .unwrap_or(0)
    });

    // Channels: those the header enables, in order.
    let enabled: Vec<usize> = (0..32).filter(|&c| settings[c] != 0xff).collect();
    let is_adlib = |c: usize| settings[c] != 0xff && (16..=29).contains(&(settings[c] & 0x7f));
    let master_stereo = master_volume & 0x80 != 0;
    let channel_pan = |c: usize| -> Option<f32> {
        if is_adlib(c) {
            return None;
        }
        if has_pan_table && pans[c] & 0x20 != 0 {
            let raw = ((pans[c] & 0x0f) as f32 * 256.0 + 8.0) / 15.0;
            return Some((raw / 255.0).min(1.0) * 255.0).map(|p| p.round() / 255.0);
        }
        master_stereo.then(|| {
            if settings[c] & 0x08 != 0 {
                204.0 / 255.0
            } else {
                51.0 / 255.0
            }
        })
    };

    let mut samples = Vec::with_capacity(instrument_count);
    for &at in &instrument_pointers {
        samples.push(parse_instrument(bytes, at, format_version == 1));
    }

    // Scream Tracker plays channels that share a setting on one hardware
    // channel: their cells merge into the first of them.
    let merge_groups: Vec<Vec<usize>> = if tracker_version >> 12 == 1 {
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for &c in &enabled {
            if settings[c] >= 0x80 {
                continue;
            }
            match groups.iter_mut().find(|g| settings[g[0]] == settings[c]) {
                Some(g) => g.push(c),
                None => groups.push(vec![c]),
            }
        }
        groups.retain(|g| g.len() > 1);
        groups
    } else {
        Vec::new()
    };
    let merged_away: Vec<usize> = merge_groups.iter().flat_map(|g| g[1..].to_vec()).collect();

    let patterns = pattern_pointers
        .iter()
        .map(|&at| {
            let mut pattern = Pattern::new(ROWS, enabled.len());
            if at == 0 || at + 2 > bytes.len() {
                return pattern;
            }
            let rows = decode_pattern(bytes, at + 2, le16(bytes, at));
            for (row, cells) in rows.iter().enumerate() {
                let mut merged: Vec<(usize, Option<Raw>)> = Vec::new();
                let mut split: Vec<usize> = Vec::new();
                for group in &merge_groups {
                    match merge_cells(cells, group) {
                        Some(cell) => merged.push((group[0], cell)),
                        None => split.extend(group),
                    }
                }
                for (track, &c) in enabled.iter().enumerate() {
                    if merged_away.contains(&c) && !split.contains(&c) {
                        continue;
                    }
                    let raw = match merged.iter().find(|(first, _)| *first == c) {
                        Some((_, cell)) => *cell,
                        None => Some(cells[c]),
                    };
                    if let Some(raw) = raw {
                        *pattern.cell_mut(row, track) = cell_from_raw(raw);
                    }
                }
            }
            pattern
        })
        .collect::<Vec<_>>();

    // 254 separates songs and is skipped, 255 ends the list.
    let listed: Vec<u8> = orders_raw
        .iter()
        .copied()
        .take_while(|&o| o != 255)
        .collect();
    let playable = |o: u8| o != 254 && (o as usize) < patterns.len();
    let orders: Vec<usize> = listed
        .iter()
        .copied()
        .filter(|&o| playable(o))
        .map(usize::from)
        .collect();
    // A jump to a separator plays on from the next pattern after it.
    let jump_targets = (0..listed.len())
        .map(|raw| listed[..raw].iter().filter(|&&o| playable(o)).count())
        .collect();

    let global = global_volume.min(64);
    Ok(Module {
        format: Format::S3m,
        title,
        channels: enabled.len(),
        orders,
        patterns,
        samples,
        instruments: Vec::new(),
        speed: if initial_speed == 0 || initial_speed == 255 {
            6
        } else {
            initial_speed
        },
        tempo: if initial_tempo < 33 {
            125
        } else {
            initial_tempo
        },
        channel_pan: enabled.iter().map(|&c| channel_pan(c)).collect(),
        muted: enabled.iter().map(|&c| settings[c] & 0x80 != 0).collect(),
        jump_targets: Some(jump_targets),
        adlib_channels: enabled
            .iter()
            .map(|&c| {
                let kind = settings[c] & 0x7f;
                (is_adlib(c) && kind <= 24).then(|| kind - 16)
            })
            .collect(),
        adlib_gain: adlib_gain(master_volume, format_version),
        global_volume: if global == 0 && tracker_version < 0x1320 {
            1.0
        } else {
            global as f32 / 64.0
        },
        stereo_separation: 1.0,
        flags: Flags {
            amiga_limits: flags & 0x10 != 0,
            vblank_timing: false,
            fast_volume_slides: tracker_version == 0x1300 || flags & 0x40 != 0,
            linear_frequency: false,
        },
        mod_flavor: None,
        tracker_version,
    })
}

/// ST3's sample pre-amp from the header's master volume byte.
fn sample_preamp(master_volume: u8, format_version: usize) -> f32 {
    let mv = master_volume;
    if format_version == 1 && mv < 8 {
        return ((mv as u32 + 1) * 0x10).min(0x7f) as f32;
    }
    if mv == 2 || mv == 0x12 {
        return 0x20 as f32;
    }
    if mv & 0x7f == 0 {
        return 48.0;
    }
    (mv & 0x7f).max(0x10) as f32
}

/// The OPL's level against the samples: OpenMPT's OPL mix level, against
/// the sample pre-amp, at equal power.
fn adlib_gain(master_volume: u8, format_version: usize) -> f32 {
    let openmpt_opl = 32768.0 * 6169.0 * (36.0 / 48.0) / 2f32.powi(27);
    std::f32::consts::FRAC_1_SQRT_2 * openmpt_opl * 128.0
        / sample_preamp(master_volume, format_version)
}

/// A group's cells as one, or None to keep them apart this row (a note
/// delay after another channel's note would otherwise be lost).
fn merge_cells(cells: &[Raw; 32], group: &[usize]) -> Option<Option<Raw>> {
    let mut merged: Option<Raw> = None;
    for &c in group {
        let cell = cells[c];
        if merged.is_some_and(|m| m.note.is_some_and(|n| n != NO_NOTE))
            && cell.effect == 0x13
            && cell.param >> 4 == 0xd
            && cell.param & 0x0f > 0
        {
            return None;
        }
        let present = cell.note.is_some()
            || cell.instrument > 0
            || cell.volume.is_some()
            || cell.effect != 0
            || cell.param != 0;
        if !present {
            continue;
        }
        let m = merged.get_or_insert_with(Raw::default);
        if cell.note.is_some() {
            m.note = cell.note;
            m.instrument = cell.instrument;
        }
        if cell.volume.is_some() {
            m.volume = cell.volume;
        }
        if cell.effect != 0 || cell.param != 0 {
            m.effect = cell.effect;
            m.param = cell.param;
        }
    }
    Some(merged)
}

fn cell_from_raw(raw: Raw) -> Cell {
    let note = match raw.note {
        Some(KEY_OFF) => Note::Off,
        Some(NO_NOTE) | None => Note::None,
        Some(n) => Note::On((n & 0x0f) + 12 * (n >> 4) + 24),
    };
    // Volumes 0-64 (higher plays as 64); 128-192 are pans no ST3 reads.
    let volume = match raw.volume {
        Some(v) if v != NO_VOLUME && v < 128 => 0x10 + v.min(64),
        _ => 0,
    };
    // A pattern break to a row that is not two decimal digits is dropped,
    // and so is a parameter with no command.
    let bad_break = raw.effect == 0x03 && (raw.param >> 4 > 9 || raw.param & 0x0f > 9);
    let (effect, param) = if raw.effect == 0 || bad_break {
        (0, 0)
    } else {
        (raw.effect, raw.param)
    };
    Cell {
        note,
        period: 0,
        instrument: raw.instrument,
        volume,
        effect,
        param,
    }
}

fn parse_instrument(bytes: &[u8], at: usize, signed: bool) -> Sample {
    if at + 80 > bytes.len() {
        return Sample::default();
    }
    let kind = bytes[at];
    let name = ascii(&bytes[at + 48..at + 76]);
    let c2spd = (le32(bytes, at + 32) & 0xffff) as u32;
    if kind >= 2 {
        let mut registers = [0u8; 12];
        registers.copy_from_slice(&bytes[at + 16..at + 28]);
        return Sample {
            name,
            volume: bytes[at + 28],
            c2spd,
            adlib: Some(Adlib {
                registers,
                melodic: kind == 2,
            }),
            ..Sample::default()
        };
    }
    let length = le32(bytes, at + 16);
    let loop_start = le32(bytes, at + 20);
    let loop_end = le32(bytes, at + 24);
    let volume = bytes[at + 28].min(64);
    let packed = bytes[at + 30] == 1;
    let flags = bytes[at + 31];
    let (stereo, bits16) = (flags & 0x02 != 0, flags & 0x04 != 0);
    if kind != 1 {
        // An empty slot: no sound, and no default volume either.
        return Sample {
            name,
            ..Sample::default()
        };
    }
    let mut sample = Sample {
        name,
        volume,
        c2spd,
        ..Sample::default()
    };
    let data_at = ((bytes[at + 14] as usize) << 4)
        | ((bytes[at + 15] as usize) << 12)
        | ((bytes[at + 13] as usize) << 20);
    if length > 0 && !stereo && !packed {
        let available = bytes.len().saturating_sub(data_at);
        sample.data = if bits16 {
            let frames = length.min(available / 2);
            (0..frames)
                .map(|i| {
                    i16::from_le_bytes([bytes[data_at + i * 2], bytes[data_at + i * 2 + 1]]) as f32
                        / 32768.0
                })
                .collect()
        } else {
            let frames = length.min(available);
            bytes[data_at..data_at + frames]
                .iter()
                .map(|&b| {
                    if signed {
                        b as i8 as f32 / 128.0
                    } else {
                        (b as f32 - 128.0) / 128.0
                    }
                })
                .collect()
        };
    }
    if flags & 0x01 != 0 && loop_end > loop_start {
        sample.loop_kind = LoopKind::Forward;
        sample.loop_start = loop_start;
        sample.loop_len = loop_end - loop_start;
    }
    sample
}
