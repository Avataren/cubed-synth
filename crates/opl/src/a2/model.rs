//! The `.a2m` container and the decoded song (`A2mSong`).
//!
//! Offsets below are the ones `techinfo.htm` gives; "MEASURED" marks what the
//! corpus showed where the document is silent or wrong.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{aplib, lzh, sixpack, DepackError};

pub const ID: &[u8; 10] = b"_A2module_";
pub const INSTRUMENT_NAME_LEN_V1_8: usize = 32;
pub const INSTRUMENT_NAME_LEN_V9: usize = 42;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packer {
    SixPack,
    Aplib,
    Lzh,
    None,
}

impl fmt::Display for Packer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Packer::SixPack => "SixPack",
            Packer::Aplib => "aPLib",
            Packer::Lzh => "LZH",
            Packer::None => "stored",
        })
    }
}

/// Everything that makes a version's layout differ.
#[derive(Debug, Clone, Copy)]
pub(super) struct Layout {
    pub(super) packer: Packer,
    /// Length fields in the header: 5 or 9 u16, or 17 u32.
    pub(super) length_fields: usize,
    pub(super) wide_lengths: bool,
    pub(super) patterns_per_block: usize,
    pub(super) max_patterns: usize,
    pub(super) rows: usize,
    pub(super) channels: usize,
    pub(super) cell_bytes: usize,
    /// Rows outermost (v1–4) or channels outermost (v5+).
    pub(super) row_major: bool,
    pub(super) songdata_len: usize,
}

pub(super) fn layout(version: u8) -> Result<Layout, A2mError> {
    let old = |packer, fields, per_block, channels, row_major, songdata_len| Layout {
        packer,
        length_fields: fields,
        wide_lengths: false,
        patterns_per_block: per_block,
        max_patterns: per_block * (fields - 1),
        rows: 64,
        channels,
        cell_bytes: 4,
        row_major,
        songdata_len,
    };
    let new = |packer, songdata_len| Layout {
        packer,
        length_fields: 17,
        wide_lengths: true,
        patterns_per_block: 8,
        max_patterns: 128,
        rows: 256,
        channels: 20,
        cell_bytes: 6,
        row_major: false,
        songdata_len,
    };
    let unsupported = |why: &'static str| Err(A2mError::UnsupportedVersion { version, why });
    Ok(match version {
        1 => old(Packer::SixPack, 5, 16, 9, true, 0x2dc4),
        4 => old(Packer::None, 5, 16, 9, true, 0x2dc4),
        5 => old(Packer::SixPack, 9, 8, 18, false, 0x2dc5),
        8 => old(Packer::None, 9, 8, 18, false, 0x2dc5),
        2 | 6 => return unsupported("its LZW packer is not implemented (no corpus file uses it)"),
        3 | 7 => return unsupported("its LZSS packer is not implemented (no corpus file uses it)"),
        // techinfo.htm gives v9 32-character instrument names and a songdata
        // block ending at 0x111eaf. The files have v10's 42-character names
        // and offsets, ending just before v10's 4-op and lock flags (MEASURED,
        // all 38 v9 modules: 0x1128a5 bytes, names aligned to 43-byte slots).
        9 => new(Packer::Aplib, 0x1128a5),
        10 => new(Packer::Aplib, 0x1128ba),
        11 => new(Packer::Aplib, 0x115a1e),
        // techinfo.htm says v12 is aPLib; the files are the LZH container
        // (MEASURED, all 8 v12 modules).
        12 | 13 => new(Packer::Lzh, 0x115e9f),
        14 => new(Packer::Lzh, 0x115ea2),
        _ => return unsupported("it is newer than the documented versions 1–14"),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum A2mError {
    TooShort {
        len: usize,
    },
    NotA2m,
    TinyModule,
    UnsupportedVersion {
        version: u8,
        why: &'static str,
    },
    BadPatternCount {
        patterns: usize,
        max: usize,
    },
    MissingBlock {
        block: usize,
    },
    BlockPastEnd {
        block: usize,
    },
    TrailingBytes {
        count: usize,
    },
    Depack {
        block: usize,
        packer: Packer,
        error: DepackError,
    },
    PackedTail {
        block: usize,
        packer: Packer,
        unused: usize,
    },
    BlockSize {
        block: usize,
        expected: usize,
        got: usize,
    },
    BadString {
        field: &'static str,
        len: u8,
        max: usize,
    },
}

impl fmt::Display for A2mError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            A2mError::TooShort { len } => write!(f, "{len} bytes is too short for an A2M header"),
            A2mError::NotA2m => write!(f, "not an Adlib Tracker II module (no _A2module_ id)"),
            A2mError::TinyModule => write!(
                f,
                "A2T tiny modules are not supported (no corpus file is one)"
            ),
            A2mError::UnsupportedVersion { version, why } => {
                write!(f, "A2M version {version} is not supported: {why}")
            }
            A2mError::BadPatternCount { patterns, max } => {
                write!(
                    f,
                    "header claims {patterns} patterns; this version holds at most {max}"
                )
            }
            A2mError::MissingBlock { block } => write!(
                f,
                "data block {block} is empty although the pattern count needs it"
            ),
            A2mError::BlockPastEnd { block } => {
                write!(f, "data block {block} runs past the end of the file")
            }
            A2mError::TrailingBytes { count } => {
                write!(f, "{count} bytes follow the last data block")
            }
            A2mError::Depack {
                block,
                packer,
                error,
            } => write!(f, "data block {block} ({packer}): {error}"),
            A2mError::PackedTail {
                block,
                packer,
                unused,
            } => {
                write!(
                    f,
                    "data block {block} ({packer}) ends {unused} bytes before its stated length"
                )
            }
            A2mError::BlockSize {
                block,
                expected,
                got,
            } => {
                write!(
                    f,
                    "data block {block} unpacks to {got} bytes, expected {expected}"
                )
            }
            A2mError::BadString { field, len, max } => {
                write!(
                    f,
                    "{field} has length byte {len}, more than its {max} characters"
                )
            }
        }
    }
}

/// One pattern cell. Effect numbers are stored as the file has them; v1–8
/// use a narrower effect set than v9+ (`techinfo.htm`: 0–15 for v1–4, 0–35 for
/// v5–8), which the player maps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// 0 = none, 1–96 notes, +0x90 fixed note (v9+), 255 = key off.
    pub note: u8,
    pub instrument: u8,
    /// `(command, data)`; the second is always (0, 0) before v9.
    pub effects: [(u8, u8); 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PatternRepr", into = "PatternRepr")]
pub struct Pattern {
    pub rows: usize,
    pub channels: usize,
    /// Row-major: `cells[row * channels + channel]`.
    pub cells: Vec<Cell>,
}

impl Pattern {
    pub fn cell(&self, row: usize, channel: usize) -> &Cell {
        &self.cells[row * self.channels + channel]
    }
}

/// Instrument: the 11 FM register bytes and the extras of each version.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instrument {
    pub name: Vec<u8>,
    /// AM/VIB/EG, KSL/TL, AR/DR, SL/RR, WS for modulator then carrier,
    /// interleaved as the file stores them (`[mod, car]` per register),
    /// then FB/connection.
    pub fm: [u8; 11],
    /// 0 = centre, 1 = left, 2 = right (v5+; 0 before).
    pub panning: u8,
    pub finetune: i8,
    /// 0 melodic, 1–5 BD, SD, TT, TC, HH (v9+; 0 before).
    pub voice_type: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FmMacroStep {
    pub fm: [u8; 11],
    pub freq_slide: i16,
    pub panning: u8,
    pub duration: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FmMacro {
    pub length: u8,
    pub loop_begin: u8,
    pub loop_length: u8,
    pub keyoff_pos: u8,
    pub arpeggio_table: u8,
    pub vibrato_table: u8,
    #[serde(with = "trimmed")]
    pub steps: Vec<FmMacroStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArpeggioMacro {
    pub length: u8,
    pub speed: u8,
    pub loop_begin: u8,
    pub loop_length: u8,
    pub keyoff_pos: u8,
    /// 0 = base note, 1–96 semitones up, +0x80 fixed note.
    #[serde(with = "trimmed")]
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VibratoMacro {
    pub length: u8,
    pub speed: u8,
    pub delay: u8,
    pub loop_begin: u8,
    pub loop_length: u8,
    pub keyoff_pos: u8,
    /// Signed frequency units to add.
    #[serde(with = "trimmed")]
    pub data: Vec<i8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2mSong {
    pub version: u8,
    pub name: Vec<u8>,
    pub composer: Vec<u8>,
    /// 250 before v9, 255 from v9.
    pub instruments: Vec<Instrument>,
    /// v9+: one per instrument; empty before.
    pub fm_macros: Vec<FmMacro>,
    /// v9+: 255 arpeggio/vibrato table pairs; empty before.
    pub arpeggio_macros: Vec<ArpeggioMacro>,
    pub vibrato_macros: Vec<VibratoMacro>,
    /// Raw order bytes as stored (128 entries). Their meaning (pattern index,
    /// jump, end) is the player's business.
    #[serde(with = "order_bytes")]
    pub order: [u8; 128],
    pub tempo: u8,
    pub speed: u8,
    /// v5+ song flags (update speed, locks, depths, percussion, volume
    /// scaling); 0 before.
    pub flags: u8,
    /// Rows per pattern: 64 before v9, from the file after.
    pub pattern_len: u16,
    /// Tracks in use: 9 (v1–4), 18 (v5–8), from the file (v9+).
    pub tracks: u8,
    pub macro_speedup: u16,
    /// v10+: 4-op track pairs; 0 before.
    pub four_op_tracks: u8,
    /// v10+: initial lock flags per track; zeros before.
    pub lock_flags: [u8; 20],
    /// v11+: 128 pattern names; empty before.
    pub pattern_names: Vec<Vec<u8>>,
    /// v11+: 28 disabled-FM-column flags per instrument; empty before.
    pub disabled_fm_columns: Vec<[u8; 28]>,
    /// v12+: count then 128 flags of 4-op instrument pairs; zeros before.
    pub four_op_instruments: Vec<u8>,
    /// v14 only.
    pub rows_per_beat: Option<u8>,
    pub tempo_finetune: Option<i16>,
    /// `patterns.len()` is the header's pattern count.
    pub patterns: Vec<Pattern>,
    /// The rest of the last pattern block, past the header's count. An order
    /// may name one, and AdPlug plays it from the block (MEASURED, Nula's
    /// `cracker.a2m`).
    pub spare_patterns: Vec<Pattern>,
}

/// Reads a Pascal string: a length byte then `max` bytes of room.
fn pascal(data: &[u8], at: usize, max: usize, field: &'static str) -> Result<Vec<u8>, A2mError> {
    let len = data[at];
    if len as usize > max {
        return Err(A2mError::BadString { field, len, max });
    }
    Ok(data[at + 1..at + 1 + len as usize].to_vec())
}

fn u16le(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn depack(packer: Packer, src: &[u8], limit: usize) -> Result<(Vec<u8>, usize), DepackError> {
    match packer {
        Packer::SixPack => sixpack::depack(src, limit),
        Packer::Aplib => aplib::depack(src, limit),
        Packer::Lzh => lzh::depack(src, limit),
        Packer::None => Ok((src.to_vec(), src.len())),
    }
}

/// Parses a whole `.a2m` file, or refuses it with one true sentence.
pub fn parse(file: &[u8]) -> Result<A2mSong, A2mError> {
    let (version, npat, blocks) = unpack(file)?;
    let lay = layout(version)?;
    let mut song = songdata(version, &blocks[0])?;
    song.patterns = patterns(
        &lay,
        npat.div_ceil(lay.patterns_per_block) * lay.patterns_per_block,
        &blocks[1..],
    );
    song.spare_patterns = song.patterns.split_off(npat);
    Ok(song)
}

/// The container step alone: version, pattern count and every data block
/// unpacked (block 0 is the songdata). Each block is checked for its exact
/// unpacked size and for using exactly its stated packed length.
pub fn unpack(file: &[u8]) -> Result<(u8, usize, Vec<Vec<u8>>), A2mError> {
    if file.len() >= 15 && file.starts_with(b"_A2tiny_module_") {
        return Err(A2mError::TinyModule);
    }
    if file.len() < 16 {
        return Err(A2mError::TooShort { len: file.len() });
    }
    if &file[..10] != ID {
        return Err(A2mError::NotA2m);
    }
    // 0x0a: a 32-bit check value. Not a plain CRC-32 of any obvious range
    // (tried: the header on, packed blocks, unpacked blocks), so unchecked.
    let version = file[14];
    let lay = layout(version)?;
    let npat = file[15] as usize;
    if npat == 0 || npat > lay.max_patterns {
        return Err(A2mError::BadPatternCount {
            patterns: npat,
            max: lay.max_patterns,
        });
    }

    let field = if lay.wide_lengths { 4 } else { 2 };
    let data_start = 16 + lay.length_fields * field;
    if file.len() < data_start {
        return Err(A2mError::TooShort { len: file.len() });
    }
    let lengths: Vec<usize> = (0..lay.length_fields)
        .map(|i| {
            let at = 16 + i * field;
            if lay.wide_lengths {
                u32::from_le_bytes([file[at], file[at + 1], file[at + 2], file[at + 3]]) as usize
            } else {
                u16le(file, at) as usize
            }
        })
        .collect();
    // Only the blocks the pattern count needs exist. Later length fields can
    // hold leftovers (MEASURED: nonzero in v1 files), so they are ignored.
    let pattern_blocks = npat.div_ceil(lay.patterns_per_block);
    let mut blocks: Vec<Vec<u8>> = Vec::with_capacity(1 + pattern_blocks);
    let mut at = data_start;
    for (block, &len) in lengths.iter().enumerate().take(1 + pattern_blocks) {
        if len == 0 {
            return Err(A2mError::MissingBlock { block });
        }
        let end = at
            .checked_add(len)
            .filter(|&e| e <= file.len())
            .ok_or(A2mError::BlockPastEnd { block })?;
        // A pattern block always holds a full set of patterns, even the last
        // (MEASURED, every block in the corpus).
        let expected = if block == 0 {
            lay.songdata_len
        } else {
            lay.patterns_per_block * lay.rows * lay.channels * lay.cell_bytes
        };
        let (out, used) =
            depack(lay.packer, &file[at..end], expected).map_err(|error| A2mError::Depack {
                block,
                packer: lay.packer,
                error,
            })?;
        // A stored v4 songdata block carries one spare byte (11717, the v5-8
        // size): AdPlug copies a stored block only when it is at least that
        // big, so `write` pads it. AT2 reads either.
        let mut out = out;
        if lay.packer == Packer::None && block == 0 && out.len() == expected + 1 {
            out.truncate(expected);
        }
        if used != len {
            return Err(A2mError::PackedTail {
                block,
                packer: lay.packer,
                unused: len - used,
            });
        }
        if out.len() != expected {
            return Err(A2mError::BlockSize {
                block,
                expected,
                got: out.len(),
            });
        }
        blocks.push(out);
        at = end;
    }
    if at != file.len() {
        return Err(A2mError::TrailingBytes {
            count: file.len() - at,
        });
    }
    Ok((version, npat, blocks))
}

fn songdata(version: u8, d: &[u8]) -> Result<A2mSong, A2mError> {
    let name = pascal(d, 0x00, 42, "song name")?;
    let composer = pascal(d, 0x2b, 42, "composer")?;
    let (count, name_len, names_at, ins_at, ins_len) = match version {
        1..=8 => (250, INSTRUMENT_NAME_LEN_V1_8, 0x56, 0x2090, 13),
        _ => (255, INSTRUMENT_NAME_LEN_V9, 0x56, 0x2b2b, 14),
    };
    let mut instruments = Vec::with_capacity(count);
    for i in 0..count {
        let r = &d[ins_at + i * ins_len..ins_at + (i + 1) * ins_len];
        let mut fm = [0u8; 11];
        fm.copy_from_slice(&r[..11]);
        instruments.push(Instrument {
            name: pascal(
                d,
                names_at + i * (name_len + 1),
                name_len,
                "instrument name",
            )?,
            fm,
            // v1–4: byte 11 is "miscellaneous (unused)".
            panning: if version >= 5 { r[11] } else { 0 },
            finetune: r[12] as i8,
            voice_type: if version >= 9 { r[13] } else { 0 },
        });
    }

    let mut song = A2mSong {
        version,
        name,
        composer,
        instruments,
        fm_macros: Vec::new(),
        arpeggio_macros: Vec::new(),
        vibrato_macros: Vec::new(),
        order: [0; 128],
        tempo: 0,
        speed: 0,
        flags: 0,
        pattern_len: 64,
        tracks: if version <= 4 { 9 } else { 18 },
        macro_speedup: 1,
        four_op_tracks: 0,
        lock_flags: [0; 20],
        pattern_names: Vec::new(),
        disabled_fm_columns: Vec::new(),
        four_op_instruments: Vec::new(),
        rows_per_beat: None,
        tempo_finetune: None,
        patterns: Vec::new(),
        spare_patterns: Vec::new(),
    };

    if version <= 8 {
        song.order.copy_from_slice(&d[0x2d42..0x2d42 + 128]);
        song.tempo = d[0x2dc2];
        song.speed = d[0x2dc3];
        if version >= 5 {
            song.flags = d[0x2dc4];
        }
        return Ok(song);
    }

    let (fm_at, av_at, order_at) = (0x391d, 0xf2126, 0x11281d);
    for i in 0..255 {
        let m = &d[fm_at + i * 3831..fm_at + (i + 1) * 3831];
        let steps = (0..255)
            .map(|s| {
                let r = &m[6 + s * 15..6 + (s + 1) * 15];
                let mut fm = [0u8; 11];
                fm.copy_from_slice(&r[..11]);
                FmMacroStep {
                    fm,
                    freq_slide: u16le(r, 11) as i16,
                    panning: r[13],
                    duration: r[14],
                }
            })
            .collect();
        song.fm_macros.push(FmMacro {
            length: m[0],
            loop_begin: m[1],
            loop_length: m[2],
            keyoff_pos: m[3],
            arpeggio_table: m[4],
            vibrato_table: m[5],
            steps,
        });
    }
    for i in 0..255 {
        let m = &d[av_at + i * 521..av_at + (i + 1) * 521];
        song.arpeggio_macros.push(ArpeggioMacro {
            length: m[0],
            speed: m[1],
            loop_begin: m[2],
            loop_length: m[3],
            keyoff_pos: m[4],
            data: m[5..260].to_vec(),
        });
        let v = &m[260..];
        song.vibrato_macros.push(VibratoMacro {
            length: v[0],
            speed: v[1],
            delay: v[2],
            loop_begin: v[3],
            loop_length: v[4],
            keyoff_pos: v[5],
            data: v[6..261].iter().map(|&b| b as i8).collect(),
        });
    }
    let o = order_at;
    song.order.copy_from_slice(&d[o..o + 128]);
    song.tempo = d[o + 0x80];
    song.speed = d[o + 0x81];
    song.flags = d[o + 0x82];
    song.pattern_len = u16le(d, o + 0x83);
    song.tracks = d[o + 0x85];
    song.macro_speedup = u16le(d, o + 0x86);
    if version >= 10 {
        song.four_op_tracks = d[o + 0x88];
        song.lock_flags.copy_from_slice(&d[o + 0x89..o + 0x89 + 20]);
    }
    if version >= 11 {
        for i in 0..128 {
            song.pattern_names
                .push(pascal(d, 0x1128ba + i * 43, 42, "pattern name")?);
        }
        for i in 0..255 {
            let mut cols = [0u8; 28];
            cols.copy_from_slice(&d[0x113e3a + i * 28..0x113e3a + (i + 1) * 28]);
            song.disabled_fm_columns.push(cols);
        }
    }
    if version >= 12 {
        song.four_op_instruments = d[0x115a1e..0x115a1e + 129].to_vec();
    }
    if version >= 14 {
        song.rows_per_beat = Some(d[0x115e9f]);
        song.tempo_finetune = Some(u16le(d, 0x115ea0) as i16);
    }
    Ok(song)
}

fn patterns(lay: &Layout, npat: usize, blocks: &[Vec<u8>]) -> Vec<Pattern> {
    let pattern_bytes = lay.rows * lay.channels * lay.cell_bytes;
    (0..npat)
        .map(|p| {
            let block = &blocks[p / lay.patterns_per_block];
            let base = (p % lay.patterns_per_block) * pattern_bytes;
            let mut cells = vec![Cell::default(); lay.rows * lay.channels];
            for row in 0..lay.rows {
                for ch in 0..lay.channels {
                    let index = if lay.row_major {
                        row * lay.channels + ch
                    } else {
                        ch * lay.rows + row
                    };
                    let c =
                        &block[base + index * lay.cell_bytes..base + (index + 1) * lay.cell_bytes];
                    cells[row * lay.channels + ch] = Cell {
                        note: c[0],
                        instrument: c[1],
                        effects: [
                            (c[2], c[3]),
                            if lay.cell_bytes == 6 {
                                (c[4], c[5])
                            } else {
                                (0, 0)
                            },
                        ],
                    };
                }
            }
            Pattern {
                rows: lay.rows,
                channels: lay.channels,
                cells,
            }
        })
        .collect()
}

/// Code page 437 for the upper half; names are DOS text.
pub fn cp437(bytes: &[u8]) -> String {
    const HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                HIGH.chars().nth((b - 0x80) as usize).unwrap_or('?')
            }
        })
        .collect()
}

/// How a pattern crosses the JSON boundary: only the cells that hold
/// something, as `(row, channel, note, instrument, fx1, param1, fx2, param2)`.
/// A full pattern is 5 120 cells, and a song has up to 128.
#[derive(Serialize, Deserialize)]
struct PatternRepr {
    rows: usize,
    channels: usize,
    cells: Vec<(u16, u8, u8, u8, u8, u8, u8, u8)>,
}

impl From<Pattern> for PatternRepr {
    fn from(p: Pattern) -> Self {
        let mut cells = Vec::new();
        for row in 0..p.rows {
            for ch in 0..p.channels {
                let c = p.cells[row * p.channels + ch];
                if c != Cell::default() {
                    cells.push((
                        row as u16,
                        ch as u8,
                        c.note,
                        c.instrument,
                        c.effects[0].0,
                        c.effects[0].1,
                        c.effects[1].0,
                        c.effects[1].1,
                    ));
                }
            }
        }
        PatternRepr {
            rows: p.rows,
            channels: p.channels,
            cells,
        }
    }
}

impl TryFrom<PatternRepr> for Pattern {
    type Error = String;
    fn try_from(r: PatternRepr) -> Result<Self, String> {
        if r.rows == 0 || r.rows > 256 || r.channels == 0 || r.channels > 20 {
            return Err(format!(
                "a pattern of {} rows by {} channels is out of range",
                r.rows, r.channels
            ));
        }
        let mut cells = vec![Cell::default(); r.rows * r.channels];
        for (row, ch, note, instrument, f1, p1, f2, p2) in r.cells {
            let (row, ch) = (row as usize, ch as usize);
            if row >= r.rows || ch >= r.channels {
                return Err(format!(
                    "cell at row {row}, channel {ch} is outside its pattern"
                ));
            }
            cells[row * r.channels + ch] = Cell {
                note,
                instrument,
                effects: [(f1, p1), (f2, p2)],
            };
        }
        Ok(Pattern {
            rows: r.rows,
            channels: r.channels,
            cells,
        })
    }
}

/// The order list is 128 bytes, past what serde's array impls cover.
mod order_bytes {
    use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(a: &[u8; 128], s: S) -> Result<S::Ok, S::Error> {
        a.as_slice().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 128], D::Error> {
        let v = Vec::<u8>::deserialize(d)?;
        let len = v.len();
        v.try_into()
            .map_err(|_| D::Error::custom(format!("the order list has {len} entries, not 128")))
    }
}

/// Macro tables are always 255 entries long in the file and mostly default;
/// the JSON drops the trailing default entries and puts them back.
mod trimmed {
    use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};

    pub const LEN: usize = 255;

    pub fn serialize<S, T>(v: &[T], s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize + Default + PartialEq,
    {
        let end = v
            .iter()
            .rposition(|x| *x != T::default())
            .map_or(0, |i| i + 1);
        v[..end].serialize(s)
    }

    pub fn deserialize<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de> + Default + Clone,
    {
        let mut v = Vec::<T>::deserialize(d)?;
        if v.len() > LEN {
            return Err(D::Error::custom(format!(
                "a macro table has {} entries, at most {LEN}",
                v.len()
            )));
        }
        v.resize(LEN, T::default());
        Ok(v)
    }
}
