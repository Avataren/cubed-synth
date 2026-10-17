//! `A2mSong` back to an `.a2m` file.
//!
//! The inverse of `model::parse`, and checked against it: every corpus file
//! parses, writes and parses again to the same song. What it writes:
//!
//! - **v9–11** aPLib and **v12–14** LZH blocks, from the encoders in
//!   `aplib::pack` / `lzh::pack` (valid streams, not AT2's own bytes).
//! - **v1 and v5** (SIXPACK) come out as **v4 and v8**, which are the same
//!   layouts stored uncompressed (`.ai/at2-src/formats/pas/a2m_file_loader.pas`
//!   reads both). Those files are at most 18 tracks of 64 rows, so they stay
//!   small, and no SIXPACK encoder is needed.
//! - The header's check value is the CRC-32 AT2's loader and saver compute
//!   (`_a2m_saver.pas`, `parserio/Update32.c`): reflected, init `0xFFFFFFFF`,
//!   no final xor, over the packed blocks and then the low two bytes of every
//!   length field. Verified on the corpus by `crc_matches_corpus`.

use super::model::{layout, parse, A2mSong, Cell, Layout, Packer, Pattern, ID};
use super::{aplib, lzh};
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut x = i as u32;
        for _ in 0..8 {
            x = if x & 1 != 0 {
                (x >> 1) ^ 0xEDB8_8320
            } else {
                x >> 1
            };
        }
        *slot = x;
    }
    table
}

/// AT2's `Update32`: the CRC-32 table step with no initial or final inversion
/// of its own (the caller starts from `0xFFFFFFFF`).
pub fn update32(table: &[u32; 256], mut crc: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        crc = table[((crc & 0xFF) as u8 ^ b) as usize] ^ (crc >> 8);
    }
    crc
}

/// The header check value for `blocks` (packed) and the `length_fields` the
/// header holds, unused ones included.
pub fn header_crc(blocks: &[&[u8]], length_fields: &[u32]) -> u32 {
    let table = crc_table();
    let mut crc = 0xFFFF_FFFF;
    for block in blocks {
        crc = update32(&table, crc, block);
    }
    for len in length_fields {
        crc = update32(&table, crc, &len.to_le_bytes()[..2]);
    }
    crc
}

fn put_pascal(d: &mut [u8], at: usize, max: usize, text: &[u8], what: &str) -> Result<(), String> {
    if text.len() > max {
        return Err(format!(
            "{what} is {} characters, at most {max}",
            text.len()
        ));
    }
    d[at] = text.len() as u8;
    d[at + 1..at + 1 + text.len()].copy_from_slice(text);
    Ok(())
}

fn put_u16(d: &mut [u8], at: usize, v: u16) {
    d[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

/// The songdata block, unpacked: the inverse of `model::songdata`.
fn songdata(song: &A2mSong, version: u8, lay: &Layout) -> Result<Vec<u8>, String> {
    let mut d = vec![0u8; lay.songdata_len];
    put_pascal(&mut d, 0x00, 42, &song.name, "the song name")?;
    put_pascal(&mut d, 0x2b, 42, &song.composer, "the composer")?;
    let (count, name_len, names_at, ins_at, ins_len) = match version {
        1..=8 => (
            250,
            super::model::INSTRUMENT_NAME_LEN_V1_8,
            0x56,
            0x2090,
            13,
        ),
        _ => (255, super::model::INSTRUMENT_NAME_LEN_V9, 0x56, 0x2b2b, 14),
    };
    if song.instruments.len() != count {
        return Err(format!(
            "version {version} holds {count} instruments, the song has {}",
            song.instruments.len()
        ));
    }
    for (i, ins) in song.instruments.iter().enumerate() {
        put_pascal(
            &mut d,
            names_at + i * (name_len + 1),
            name_len,
            &ins.name,
            "an instrument name",
        )?;
        let r = &mut d[ins_at + i * ins_len..ins_at + (i + 1) * ins_len];
        r[..11].copy_from_slice(&ins.fm);
        if version >= 5 {
            r[11] = ins.panning;
        }
        r[12] = ins.finetune as u8;
        if version >= 9 {
            r[13] = ins.voice_type;
        }
    }

    if version <= 8 {
        d[0x2d42..0x2d42 + 128].copy_from_slice(&song.order);
        d[0x2dc2] = song.tempo;
        d[0x2dc3] = song.speed;
        if version >= 5 {
            d[0x2dc4] = song.flags;
        }
        return Ok(d);
    }

    let (fm_at, av_at, order_at) = (0x391d, 0xf2126, 0x11281d);
    if song.fm_macros.len() != 255
        || song.arpeggio_macros.len() != 255
        || song.vibrato_macros.len() != 255
    {
        return Err("a version 9+ song has 255 FM, arpeggio and vibrato macros".into());
    }
    for (i, m) in song.fm_macros.iter().enumerate() {
        if m.steps.len() != 255 {
            return Err(format!(
                "FM macro {} has {} steps, not 255",
                i + 1,
                m.steps.len()
            ));
        }
        let o = &mut d[fm_at + i * 3831..fm_at + (i + 1) * 3831];
        o[..6].copy_from_slice(&[
            m.length,
            m.loop_begin,
            m.loop_length,
            m.keyoff_pos,
            m.arpeggio_table,
            m.vibrato_table,
        ]);
        for (s, step) in m.steps.iter().enumerate() {
            let r = &mut o[6 + s * 15..6 + (s + 1) * 15];
            r[..11].copy_from_slice(&step.fm);
            put_u16(r, 11, step.freq_slide as u16);
            r[13] = step.panning;
            r[14] = step.duration;
        }
    }
    for i in 0..255 {
        let a = &song.arpeggio_macros[i];
        let v = &song.vibrato_macros[i];
        if a.data.len() != 255 || v.data.len() != 255 {
            return Err(format!(
                "arpeggio/vibrato macro {} is not 255 entries long",
                i + 1
            ));
        }
        let o = &mut d[av_at + i * 521..av_at + (i + 1) * 521];
        o[..5].copy_from_slice(&[a.length, a.speed, a.loop_begin, a.loop_length, a.keyoff_pos]);
        o[5..260].copy_from_slice(&a.data);
        o[260..266].copy_from_slice(&[
            v.length,
            v.speed,
            v.delay,
            v.loop_begin,
            v.loop_length,
            v.keyoff_pos,
        ]);
        for (k, &b) in v.data.iter().enumerate() {
            o[266 + k] = b as u8;
        }
    }
    let o = order_at;
    d[o..o + 128].copy_from_slice(&song.order);
    d[o + 0x80] = song.tempo;
    d[o + 0x81] = song.speed;
    d[o + 0x82] = song.flags;
    put_u16(&mut d, o + 0x83, song.pattern_len);
    d[o + 0x85] = song.tracks;
    put_u16(&mut d, o + 0x86, song.macro_speedup);
    if version >= 10 {
        d[o + 0x88] = song.four_op_tracks;
        d[o + 0x89..o + 0x89 + 20].copy_from_slice(&song.lock_flags);
    }
    if version >= 11 {
        if song.pattern_names.len() != 128 || song.disabled_fm_columns.len() != 255 {
            return Err(
                "a version 11+ song has 128 pattern names and 255 disabled-column rows".into(),
            );
        }
        for (i, name) in song.pattern_names.iter().enumerate() {
            put_pascal(&mut d, 0x1128ba + i * 43, 42, name, "a pattern name")?;
        }
        for (i, cols) in song.disabled_fm_columns.iter().enumerate() {
            d[0x113e3a + i * 28..0x113e3a + (i + 1) * 28].copy_from_slice(cols);
        }
    }
    if version >= 12 {
        if song.four_op_instruments.len() != 129 {
            return Err("a version 12+ song has 129 four-op instrument bytes".into());
        }
        d[0x115a1e..0x115a1e + 129].copy_from_slice(&song.four_op_instruments);
    }
    if version >= 14 {
        d[0x115e9f] = song.rows_per_beat.unwrap_or(4);
        put_u16(&mut d, 0x115ea0, song.tempo_finetune.unwrap_or(0) as u16);
    }
    Ok(d)
}

/// One block of `per_block` patterns, unpacked: the inverse of `model::patterns`.
fn pattern_block(lay: &Layout, patterns: &[&Pattern]) -> Result<Vec<u8>, String> {
    let pattern_bytes = lay.rows * lay.channels * lay.cell_bytes;
    let mut block = vec![0u8; lay.patterns_per_block * pattern_bytes];
    let empty = Cell::default();
    for (p, pattern) in patterns.iter().enumerate() {
        if pattern.rows != lay.rows || pattern.channels != lay.channels {
            return Err(format!(
                "a pattern is {} rows by {} channels, this version stores {} by {}",
                pattern.rows, pattern.channels, lay.rows, lay.channels
            ));
        }
        for row in 0..lay.rows {
            for ch in 0..lay.channels {
                let c = pattern.cells.get(row * lay.channels + ch).unwrap_or(&empty);
                let index = if lay.row_major {
                    row * lay.channels + ch
                } else {
                    ch * lay.rows + row
                };
                let at = p * pattern_bytes + index * lay.cell_bytes;
                let o = &mut block[at..at + lay.cell_bytes];
                o[0] = c.note;
                o[1] = c.instrument;
                o[2] = c.effects[0].0;
                o[3] = c.effects[0].1;
                if lay.cell_bytes == 6 {
                    o[4] = c.effects[1].0;
                    o[5] = c.effects[1].1;
                }
            }
        }
    }
    Ok(block)
}

fn pack(packer: Packer, block: &[u8]) -> Vec<u8> {
    match packer {
        Packer::Aplib => aplib::pack(block),
        Packer::Lzh => lzh::pack(block),
        Packer::None => block.to_vec(),
        Packer::SixPack => unreachable!("version 1 and 5 are written as 4 and 8"),
    }
}

/// The version `write` stores a song of `version` as.
pub fn output_version(version: u8) -> u8 {
    match version {
        1 => 4,
        5 => 8,
        v => v,
    }
}

/// The song as an `.a2m` file. Fails with one sentence when the song does not
/// fit its version's layout (a pattern of the wrong size, too many patterns).
pub fn write(song: &A2mSong) -> Result<Vec<u8>, String> {
    let version = output_version(song.version);
    let lay = layout(version).map_err(|e| e.to_string())?;
    let all: Vec<&Pattern> = song
        .patterns
        .iter()
        .chain(song.spare_patterns.iter())
        .collect();
    if song.patterns.is_empty() || song.patterns.len() > lay.max_patterns {
        return Err(format!(
            "version {version} holds 1 to {} patterns, the song has {}",
            lay.max_patterns,
            song.patterns.len()
        ));
    }
    let blocks_needed = song.patterns.len().div_ceil(lay.patterns_per_block);
    let mut raw: Vec<Vec<u8>> = vec![songdata(song, version, &lay)?];
    let blank = Pattern {
        rows: lay.rows,
        channels: lay.channels,
        cells: Vec::new(),
    };
    for b in 0..blocks_needed {
        let from = b * lay.patterns_per_block;
        let mut group: Vec<&Pattern> = (from..from + lay.patterns_per_block)
            .map(|i| all.get(i).copied().unwrap_or(&blank))
            .collect();
        group.truncate(lay.patterns_per_block);
        raw.push(pattern_block(&lay, &group)?);
    }
    if version == 4 {
        // AdPlug copies a stored songdata block only if it is 11717 bytes
        // or more; v4's is 11716. One spare zero byte; `unpack` accepts it.
        raw[0].push(0);
    }
    let packed: Vec<Vec<u8>> = raw.iter().map(|b| pack(lay.packer, b)).collect();

    let mut lengths = vec![0u32; lay.length_fields];
    for (i, p) in packed.iter().enumerate() {
        lengths[i] = p.len() as u32;
    }
    if !lay.wide_lengths && lengths.iter().any(|&l| l > 0xFFFF) {
        return Err("a block is larger than this version's 64 KiB length field".into());
    }
    let refs: Vec<&[u8]> = packed.iter().map(|p| p.as_slice()).collect();
    let crc = header_crc(&refs, &lengths);

    let mut out = Vec::new();
    out.extend_from_slice(ID);
    out.extend_from_slice(&crc.to_le_bytes());
    out.push(version);
    out.push(song.patterns.len() as u8);
    for &len in &lengths {
        if lay.wide_lengths {
            out.extend_from_slice(&len.to_le_bytes());
        } else {
            out.extend_from_slice(&(len as u16).to_le_bytes());
        }
    }
    for p in &packed {
        out.extend_from_slice(p);
    }
    Ok(out)
}

/// A new, empty song: one empty pattern, an order list that plays it and
/// loops, the AT2 defaults (tempo 50, speed 6, 64 rows, version 14) and one
/// usable instrument. `opl3` picks 18 tracks over 9 (OPL2).
pub fn new_song(opl3: bool) -> A2mSong {
    use super::model::{ArpeggioMacro, FmMacro, FmMacroStep, Instrument, VibratoMacro};
    let tracks: u8 = if opl3 { 18 } else { 9 };
    // AT2's own defaults are 255 blank instruments (`init_songdata`). Slot 1
    // gets a plain two-operator voice so a new song makes a sound at once.
    let mut instruments = vec![Instrument::default(); 255];
    instruments[0] = Instrument {
        name: b"New instrument".to_vec(),
        fm: [
            0x21, 0x21, 0x3f, 0x00, 0xf2, 0xf4, 0x24, 0x27, 0x00, 0x00, 0x00,
        ],
        ..Instrument::default()
    };
    let empty_fm = FmMacro {
        length: 0,
        loop_begin: 0,
        loop_length: 0,
        keyoff_pos: 0,
        arpeggio_table: 0,
        vibrato_table: 0,
        steps: vec![FmMacroStep::default(); 255],
    };
    let empty_arp = ArpeggioMacro {
        length: 0,
        speed: 1,
        loop_begin: 0,
        loop_length: 0,
        keyoff_pos: 0,
        data: vec![0; 255],
    };
    let empty_vib = VibratoMacro {
        length: 0,
        speed: 1,
        delay: 0,
        loop_begin: 0,
        loop_length: 0,
        keyoff_pos: 0,
        data: vec![0; 255],
    };
    let blank = Pattern {
        rows: 256,
        channels: 20,
        cells: vec![Cell::default(); 256 * 20],
    };
    let mut order = [0x80u8; 128];
    order[0] = 0;
    A2mSong {
        version: 14,
        name: Vec::new(),
        composer: Vec::new(),
        instruments,
        fm_macros: vec![empty_fm; 255],
        arpeggio_macros: vec![empty_arp; 255],
        vibrato_macros: vec![empty_vib; 255],
        order,
        tempo: 50,
        speed: 6,
        flags: 0,
        pattern_len: 64,
        tracks,
        macro_speedup: 1,
        four_op_tracks: 0,
        lock_flags: [0; 20],
        pattern_names: vec![Vec::new(); 128],
        disabled_fm_columns: vec![[0; 28]; 255],
        four_op_instruments: vec![0; 129],
        rows_per_beat: Some(4),
        tempo_finetune: Some(0),
        patterns: vec![blank.clone()],
        // What parsing the file finds in the rest of its one pattern block.
        spare_patterns: vec![blank; 7],
    }
}

// ---------------------------------------------------------------------------
// The app's door: the song as JSON (`src/audio/tracker/a2m-codec.ts`). The
// app edits the model and hands it back to be written, so the editor, the
// player (which takes the bytes) and the exporter all see the same song.
// ---------------------------------------------------------------------------

/// The song in `bytes` as JSON, or the reason it cannot be read.
#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub fn a2m_to_json(bytes: &[u8]) -> Result<String, String> {
    let song = parse(bytes).map_err(|e| e.to_string())?;
    serde_json::to_string(&song).map_err(|e| e.to_string())
}

/// The `.a2m` file for the song in `json`, or the reason it cannot be written.
#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub fn a2m_from_json(json: &str) -> Result<Vec<u8>, String> {
    let song: A2mSong = serde_json::from_str(json).map_err(|e| format!("not an A2M song: {e}"))?;
    write(&song)
}

/// A new empty song as JSON: 9 tracks (OPL2) or 18 (OPL3).
#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub fn a2m_new_json(opl3: bool) -> Result<String, String> {
    serde_json::to_string(&new_song(opl3)).map_err(|e| e.to_string())
}
