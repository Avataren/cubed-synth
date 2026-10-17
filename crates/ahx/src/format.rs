//! Byte-exact AHX (`.ahx`) and HVL (`.hvl`) header/instrument/track/PList
//! decode. Mirrors `hvl_load_ahx`/`hvl_load_hvl`/`hvl_ParseTune` in the
//! vendored `.ai/ahx/references/hvl_replay.c` field-for-field (see
//! `.ai/p0-report.md` for the byte-offset table this was checked against,
//! including two real fixtures parsed by hand as a cross-check).
//!
//! D23 discipline: this module reports what the file says. It does not
//! interpret PList/envelope/filter semantics -- that is playback logic for a
//! later phase.

use std::fmt;

/// AHX's own format never exceeds 4 channels; HVL packs a channel count into
/// the header (`(buf[8]>>2)+4`) and the reference replayer sizes its voice
/// array at `MAX_CHANNELS == 16` -- see `.ai/p0-report.md` for the resulting
/// contradiction with `verdict.md`'s "AHX is always 4 channels" framing.
pub const MAX_CHANNELS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SongFormat {
    /// `"THX"` magic, version byte < 3.
    Ahx,
    /// `"HVL"` magic, version byte < 2.
    Hvl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AhxParseError {
    /// Fewer than 14 (AHX) / 16 (HVL) bytes -- not enough to read the fixed
    /// header fields at all.
    TooShort,
    /// Neither `"THX"` + version < 3 nor `"HVL"` + version < 2.
    BadMagic,
    /// Header dimensions the reference loader itself rejects:
    /// `position_nr > 1000 || track_length > 64 || instrument_nr > 64`
    /// (`hvl_replay.c`'s own "Do some validation" block).
    InvalidDimensions {
        position_nr: usize,
        track_length: u8,
        instrument_nr: u8,
    },
    /// A field read past the end of the buffer -- a truncated or corrupt
    /// file. `field` names the byte region being decoded when the read
    /// failed.
    Truncated { field: &'static str, offset: usize },
    /// A single-instrument wire form ([`parse_instrument`]) whose length is not
    /// the 22-byte core plus exactly the PList entries its length byte counts.
    BadInstrumentLength { expected: usize, got: usize },
}

impl fmt::Display for AhxParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AhxParseError::TooShort => write!(f, "buffer too short for a fixed header"),
            AhxParseError::BadMagic => write!(f, "not an AHX or HVL file (bad magic/version)"),
            AhxParseError::InvalidDimensions {
                position_nr,
                track_length,
                instrument_nr,
            } => write!(
                f,
                "invalid dimensions: position_nr={position_nr}, track_length={track_length}, instrument_nr={instrument_nr}"
            ),
            AhxParseError::Truncated { field, offset } => {
                write!(f, "truncated while reading {field} at byte offset {offset}")
            }
            AhxParseError::BadInstrumentLength { expected, got } => {
                write!(f, "instrument is {got} bytes, its header says {expected}")
            }
        }
    }
}

impl std::error::Error for AhxParseError {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Envelope {
    pub a_frames: u8,
    pub a_volume: u8,
    pub d_frames: u8,
    pub d_volume: u8,
    pub s_frames: u8,
    pub r_frames: u8,
    pub r_volume: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PListEntry {
    pub note: u8,
    /// 0 = no waveform override; 1..=5 = `waveform - 1` at playback
    /// (`ple_Waveform-1` in `hvl_process_frame`).
    pub waveform: u8,
    pub fixed: bool,
    pub fx: [u8; 2],
    pub fx_param: [u8; 2],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PList {
    pub speed: u8,
    pub entries: Vec<PListEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Instrument {
    pub name: String,
    pub volume: u8,
    pub wave_length: u8,
    pub filter_lower_limit: u8,
    pub filter_upper_limit: u8,
    pub filter_speed: u8,
    pub square_lower_limit: u8,
    pub square_upper_limit: u8,
    pub square_speed: u8,
    pub vibrato_delay: u8,
    pub vibrato_speed: u8,
    pub vibrato_depth: u8,
    pub hard_cut_release: bool,
    pub hard_cut_release_frames: u8,
    pub envelope: Envelope,
    pub plist: PList,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Step {
    pub note: u8,
    pub instrument: u8,
    pub fx: u8,
    pub fx_param: u8,
    /// Always 0 for AHX (single effect column); HVL's second effect column.
    pub fxb: u8,
    pub fxb_param: u8,
}

pub type Track = Vec<Step>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Position {
    pub track: Vec<u8>,
    pub transpose: Vec<i8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Song {
    pub format: SongFormat,
    /// Raw version byte (`buf[3]`). AHX: 0..=2. HVL: 0..=1.
    pub version: u8,
    pub name: String,
    /// Fixed at 4 for AHX; `(buf[8]>>2)+4` for HVL (can exceed 4 -- see
    /// `MAX_CHANNELS` doc comment).
    pub channels: usize,
    pub position_nr: usize,
    pub restart: u16,
    /// 1..=4; the "frame" rate divides the fixed 50 Hz PAL base by this
    /// (`hvl_DecodeFrame`: `samples = frequency/50/speed_multiplier`, called
    /// `speed_multiplier` times per decoded chunk). See `.ai/p0-report.md`
    /// for why 50 Hz, not 125 Hz, is the resolved base rate.
    pub speed_multiplier: u8,
    pub track_length: u8,
    pub track_nr: u8,
    pub instrument_nr: u8,
    pub subsong_nr: u8,
    pub subsongs: Vec<u16>,
    pub positions: Vec<Position>,
    /// Index 0..=track_nr, addressed directly by `Position::track` entries.
    pub tracks: Vec<Track>,
    /// Index 0 is an unused placeholder (AHX/HVL instrument numbering starts
    /// at 1, matching the reference's `ht_Instruments[1..=insn]`); indices
    /// 1..=instrument_nr are real.
    pub instruments: Vec<Instrument>,
    /// HVL only (`buf[14]`); `None` for AHX, where mix gain instead comes
    /// from a caller-supplied stereo-separation choice, not the file.
    pub mixgain_raw: Option<u8>,
    /// HVL only (`buf[15]`); `None` for AHX, same reasoning as `mixgain_raw`.
    pub defstereo: Option<u8>,
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn u8(&self, offset: usize, field: &'static str) -> Result<u8, AhxParseError> {
        self.buf
            .get(offset)
            .copied()
            .ok_or(AhxParseError::Truncated { field, offset })
    }

    fn i8(&self, offset: usize, field: &'static str) -> Result<i8, AhxParseError> {
        self.u8(offset, field).map(|v| v as i8)
    }

    fn u16be(&self, offset: usize, field: &'static str) -> Result<u16, AhxParseError> {
        let hi = self.u8(offset, field)? as u16;
        let lo = self.u8(offset + 1, field)? as u16;
        Ok((hi << 8) | lo)
    }
}

/// Reads a NUL-terminated string starting at `offset`. Returns an empty
/// string if `offset` is out of bounds (mirrors the reference's `nptr <
/// buf+buflen` guard for instrument names run off the end of a truncated
/// file, rather than treating it as fatal).
fn read_cstring(buf: &[u8], offset: usize) -> String {
    if offset >= buf.len() {
        return String::new();
    }
    let end = buf[offset..]
        .iter()
        .position(|&b| b == 0)
        .map(|i| offset + i)
        .unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[offset..end]).into_owned()
}

/// Shared 22-byte instrument core, identical byte layout in AHX and HVL
/// (`hvl_load_ahx:270-293` / `hvl_load_hvl:505-528`). Only the trailing
/// PList entries differ in size/bit-layout between the two formats.
struct InstrumentCore {
    volume: u8,
    wave_length: u8,
    filter_lower_limit: u8,
    filter_upper_limit: u8,
    filter_speed: u8,
    square_lower_limit: u8,
    square_upper_limit: u8,
    square_speed: u8,
    vibrato_delay: u8,
    vibrato_speed: u8,
    vibrato_depth: u8,
    hard_cut_release: bool,
    hard_cut_release_frames: u8,
    envelope: Envelope,
    plist_speed: u8,
    plist_length: u8,
}

fn parse_instrument_core(r: &Reader, pos: usize) -> Result<InstrumentCore, AhxParseError> {
    const F: &str = "instrument core";
    let b0 = r.u8(pos, F)?;
    let b1 = r.u8(pos + 1, F)?;
    let b2 = r.u8(pos + 2, F)?;
    let b3 = r.u8(pos + 3, F)?;
    let b4 = r.u8(pos + 4, F)?;
    let b5 = r.u8(pos + 5, F)?;
    let b6 = r.u8(pos + 6, F)?;
    let b7 = r.u8(pos + 7, F)?;
    let b8 = r.u8(pos + 8, F)?;
    let b12 = r.u8(pos + 12, F)?;
    let b13 = r.u8(pos + 13, F)?;
    let b14 = r.u8(pos + 14, F)?;
    let b15 = r.u8(pos + 15, F)?;
    let b16 = r.u8(pos + 16, F)?;
    let b17 = r.u8(pos + 17, F)?;
    let b18 = r.u8(pos + 18, F)?;
    let b19 = r.u8(pos + 19, F)?;
    let b20 = r.u8(pos + 20, F)?;
    let b21 = r.u8(pos + 21, F)?;

    Ok(InstrumentCore {
        volume: b0,
        filter_speed: ((b1 >> 3) & 0x1f) | ((b12 >> 2) & 0x20),
        wave_length: b1 & 0x07,
        envelope: Envelope {
            a_frames: b2,
            a_volume: b3,
            d_frames: b4,
            d_volume: b5,
            s_frames: b6,
            r_frames: b7,
            r_volume: b8,
        },
        filter_lower_limit: b12 & 0x7f,
        vibrato_delay: b13,
        hard_cut_release_frames: (b14 >> 4) & 0x07,
        hard_cut_release: (b14 & 0x80) != 0,
        vibrato_depth: b14 & 0x0f,
        vibrato_speed: b15,
        square_lower_limit: b16,
        square_upper_limit: b17,
        square_speed: b18,
        filter_upper_limit: b19 & 0x3f,
        plist_speed: b20,
        plist_length: b21,
    })
}

impl InstrumentCore {
    fn finish(self, name: String, entries: Vec<PListEntry>) -> Instrument {
        Instrument {
            name,
            volume: self.volume,
            wave_length: self.wave_length,
            filter_lower_limit: self.filter_lower_limit,
            filter_upper_limit: self.filter_upper_limit,
            filter_speed: self.filter_speed,
            square_lower_limit: self.square_lower_limit,
            square_upper_limit: self.square_upper_limit,
            square_speed: self.square_speed,
            vibrato_delay: self.vibrato_delay,
            vibrato_speed: self.vibrato_speed,
            vibrato_depth: self.vibrato_depth,
            hard_cut_release: self.hard_cut_release,
            hard_cut_release_frames: self.hard_cut_release_frames,
            envelope: self.envelope,
            plist: PList {
                speed: self.plist_speed,
                entries,
            },
        }
    }
}

/// AHX PList entry: 4 bytes, `hvl_load_ahx:299-323`. The FX nibble codes are
/// packed into 3 bits with a remap (`6 -> 12`, `7 -> 15`) to reach 4-bit
/// command space -- unlike HVL's PList entry, which has room for a direct
/// 4-bit code (see `parse_plist_entry_hvl`).
fn parse_plist_entry_ahx(b0: u8, b1: u8, b2: u8, b3: u8, ahx_version: u8) -> PListEntry {
    let remap = |v: u8| match v {
        6 => 12,
        7 => 15,
        other => other,
    };
    let k = remap((b0 >> 5) & 7);
    let l = remap((b0 >> 2) & 7);
    let waveform = ((b0 << 1) & 6) | (b1 >> 7);
    let fixed = ((b1 >> 6) & 1) != 0;
    let note = b1 & 0x3f;

    let mut fx_param = [b2, b3];
    // 1.6: strip "toggle filter" (FX 4) params down to the low nibble on
    // version-0 (pre-filter) AHX files, matching what AHX itself does
    // (`hvl_load_ahx:315-320`).
    if ahx_version == 0 && l == 4 && (b2 & 0xf0) != 0 {
        fx_param[0] &= 0x0f;
    }
    if ahx_version == 0 && k == 4 && (b3 & 0xf0) != 0 {
        fx_param[1] &= 0x0f;
    }

    PListEntry {
        note,
        waveform,
        fixed,
        fx: [l, k],
        fx_param,
    }
}

/// HVL PList entry: 5 bytes, `hvl_load_hvl:536-542`. Direct 4-bit FX codes,
/// no remap and no version-0 filter-strip quirk (that quirk is AHX-only).
fn parse_plist_entry_hvl(b0: u8, b1: u8, b2: u8, b3: u8, b4: u8) -> PListEntry {
    PListEntry {
        note: b2 & 0x3f,
        waveform: b1 & 7,
        fixed: ((b2 >> 6) & 1) != 0,
        fx: [b0 & 0xf, (b1 >> 3) & 0xf],
        fx_param: [b3, b4],
    }
}

fn parse_ahx(buf: &[u8]) -> Result<Song, AhxParseError> {
    let r = Reader { buf };
    let version = r.u8(3, "version")?;

    let b6 = r.u8(6, "header")?;
    let posn = (((b6 & 0x0f) as usize) << 8) | r.u8(7, "header")? as usize;
    let insn = r.u8(12, "header")? as usize;
    let ssn = r.u8(13, "header")? as usize;
    let trkl = r.u8(10, "header")?;
    let trkn = r.u8(11, "header")? as usize;

    if posn > 1000 || trkl > 64 || insn > 64 {
        return Err(AhxParseError::InvalidDimensions {
            position_nr: posn,
            track_length: trkl,
            instrument_nr: insn as u8,
        });
    }

    let restart_raw = r.u16be(8, "restart")?;
    let speed_multiplier = ((b6 >> 5) & 3) + 1;
    let blank_first_track = (b6 & 0x80) != 0;

    let name_offset = r.u16be(4, "name offset")? as usize;
    let name = read_cstring(buf, name_offset);
    let mut nptr = name_offset + name.len() + 1;

    let mut pos = 14usize;

    let mut subsongs = Vec::with_capacity(ssn);
    for _ in 0..ssn {
        let mut v = r.u16be(pos, "subsong")?;
        if v as usize >= posn {
            v = 0;
        }
        subsongs.push(v);
        pos += 2;
    }

    let mut positions = Vec::with_capacity(posn);
    for _ in 0..posn {
        let mut track = Vec::with_capacity(4);
        let mut transpose = Vec::with_capacity(4);
        for _ in 0..4 {
            track.push(r.u8(pos, "position track")?);
            pos += 1;
            transpose.push(r.i8(pos, "position transpose")?);
            pos += 1;
        }
        positions.push(Position { track, transpose });
    }

    let mut tracks = Vec::with_capacity(trkn + 1);
    for i in 0..=trkn {
        if blank_first_track && i == 0 {
            tracks.push(vec![Step::default(); trkl as usize]);
            continue;
        }
        let mut steps = Vec::with_capacity(trkl as usize);
        for _ in 0..trkl {
            let b0 = r.u8(pos, "track step")?;
            let b1 = r.u8(pos + 1, "track step")?;
            let b2 = r.u8(pos + 2, "track step")?;
            pos += 3;
            steps.push(Step {
                note: (b0 >> 2) & 0x3f,
                instrument: ((b0 & 0x3) << 4) | (b1 >> 4),
                fx: b1 & 0xf,
                fx_param: b2,
                fxb: 0,
                fxb_param: 0,
            });
        }
        tracks.push(steps);
    }

    let mut instruments = Vec::with_capacity(insn + 1);
    instruments.push(Instrument::default());
    for _ in 1..=insn {
        let iname = if nptr < buf.len() {
            let s = read_cstring(buf, nptr);
            nptr += s.len() + 1;
            s
        } else {
            String::new()
        };

        let core = parse_instrument_core(&r, pos)?;
        pos += 22;

        let mut entries = Vec::with_capacity(core.plist_length as usize);
        for _ in 0..core.plist_length {
            let b0 = r.u8(pos, "plist entry")?;
            let b1 = r.u8(pos + 1, "plist entry")?;
            let b2 = r.u8(pos + 2, "plist entry")?;
            let b3 = r.u8(pos + 3, "plist entry")?;
            pos += 4;
            entries.push(parse_plist_entry_ahx(b0, b1, b2, b3, version));
        }

        instruments.push(core.finish(iname, entries));
    }

    let restart = if restart_raw as usize >= posn {
        posn.saturating_sub(1) as u16
    } else {
        restart_raw
    };

    Ok(Song {
        format: SongFormat::Ahx,
        version,
        name,
        channels: 4,
        position_nr: posn,
        restart,
        speed_multiplier,
        track_length: trkl,
        track_nr: trkn as u8,
        instrument_nr: insn as u8,
        subsong_nr: ssn as u8,
        subsongs,
        positions,
        tracks,
        instruments,
        mixgain_raw: None,
        defstereo: None,
    })
}

fn parse_hvl(buf: &[u8]) -> Result<Song, AhxParseError> {
    let r = Reader { buf };
    let version = r.u8(3, "version")?;

    let b6 = r.u8(6, "header")?;
    let b8 = r.u8(8, "header")?;
    let posn = (((b6 & 0x0f) as usize) << 8) | r.u8(7, "header")? as usize;
    let insn = r.u8(12, "header")? as usize;
    let ssn = r.u8(13, "header")? as usize;
    let channels = ((b8 >> 2) as usize) + 4;
    let trkl = r.u8(10, "header")?;
    let trkn = r.u8(11, "header")? as usize;

    if posn > 1000 || trkl > 64 || insn > 64 {
        return Err(AhxParseError::InvalidDimensions {
            position_nr: posn,
            track_length: trkl,
            instrument_nr: insn as u8,
        });
    }

    let speed_multiplier = ((b6 >> 5) & 3) + 1;
    let blank_first_track = (b6 & 0x80) != 0;
    let restart_raw = (((b8 & 3) as u16) << 8) | r.u8(9, "header")? as u16;
    let mixgain_raw = r.u8(14, "header")?;
    let defstereo = r.u8(15, "header")?;

    let name_offset = r.u16be(4, "name offset")? as usize;
    let name = read_cstring(buf, name_offset);
    let mut nptr = name_offset + name.len() + 1;

    let mut pos = 16usize;

    let mut subsongs = Vec::with_capacity(ssn);
    for _ in 0..ssn {
        subsongs.push(r.u16be(pos, "subsong")?);
        pos += 2;
    }

    let mut positions = Vec::with_capacity(posn);
    for _ in 0..posn {
        let mut track = Vec::with_capacity(channels);
        let mut transpose = Vec::with_capacity(channels);
        for _ in 0..channels {
            track.push(r.u8(pos, "position track")?);
            pos += 1;
            transpose.push(r.i8(pos, "position transpose")?);
            pos += 1;
        }
        positions.push(Position { track, transpose });
    }

    let mut tracks = Vec::with_capacity(trkn + 1);
    for i in 0..=trkn {
        if blank_first_track && i == 0 {
            tracks.push(vec![Step::default(); trkl as usize]);
            continue;
        }
        let mut steps = Vec::with_capacity(trkl as usize);
        for _ in 0..trkl {
            let b0 = r.u8(pos, "track step")?;
            if b0 == 0x3f {
                pos += 1;
                steps.push(Step::default());
                continue;
            }
            let b1 = r.u8(pos + 1, "track step")?;
            let b2 = r.u8(pos + 2, "track step")?;
            let b3 = r.u8(pos + 3, "track step")?;
            let b4 = r.u8(pos + 4, "track step")?;
            pos += 5;
            steps.push(Step {
                note: b0,
                instrument: b1,
                fx: b2 >> 4,
                fx_param: b3,
                fxb: b2 & 0xf,
                fxb_param: b4,
            });
        }
        tracks.push(steps);
    }

    let mut instruments = Vec::with_capacity(insn + 1);
    instruments.push(Instrument::default());
    for _ in 1..=insn {
        let iname = if nptr < buf.len() {
            let s = read_cstring(buf, nptr);
            nptr += s.len() + 1;
            s
        } else {
            String::new()
        };

        let core = parse_instrument_core(&r, pos)?;
        pos += 22;

        let mut entries = Vec::with_capacity(core.plist_length as usize);
        for _ in 0..core.plist_length {
            let b0 = r.u8(pos, "plist entry")?;
            let b1 = r.u8(pos + 1, "plist entry")?;
            let b2 = r.u8(pos + 2, "plist entry")?;
            let b3 = r.u8(pos + 3, "plist entry")?;
            let b4 = r.u8(pos + 4, "plist entry")?;
            pos += 5;
            entries.push(parse_plist_entry_hvl(b0, b1, b2, b3, b4));
        }

        instruments.push(core.finish(iname, entries));
    }

    let restart = if restart_raw as usize >= posn {
        posn.saturating_sub(1) as u16
    } else {
        restart_raw
    };

    Ok(Song {
        format: SongFormat::Hvl,
        version,
        name,
        channels,
        position_nr: posn,
        restart,
        speed_multiplier,
        track_length: trkl,
        track_nr: trkn as u8,
        instrument_nr: insn as u8,
        subsong_nr: ssn as u8,
        subsongs,
        positions,
        tracks,
        instruments,
        mixgain_raw: Some(mixgain_raw),
        defstereo: Some(defstereo),
    })
}

/// Bytes of one PList entry in `format`'s layout: 4 in AHX, 5 in HVL.
pub fn plist_entry_bytes(format: SongFormat) -> usize {
    match format {
        SongFormat::Ahx => 4,
        SongFormat::Hvl => 5,
    }
}

/// Bytes of the instrument core in both formats.
pub const INSTRUMENT_CORE_BYTES: usize = 22;

/// Decodes ONE instrument from its wire form: the 22-byte core followed by its
/// PList entries in `format`'s own layout, with no name (names live in the
/// song's string table, so the caller supplies it, or keeps the one it has).
/// It is the file loader's own decode -- `parse_instrument_core` and the
/// `parse_plist_entry_*` functions `parse_ahx`/`parse_hvl` call -- applied to a
/// buffer that holds a single instrument, so an instrument written back
/// through `serializeAhxInstrument` (TypeScript) or a future `.ahx` writer
/// reads exactly as the file it came from would.
///
/// `version` is the song's raw version byte: version-0 AHX files strip the
/// high nibble of a "toggle filter" parameter (`parse_plist_entry_ahx`), and an
/// instrument decoded for such a song must be read the same way. The buffer
/// must be exactly as long as its own length byte says.
pub fn parse_instrument(
    bytes: &[u8],
    format: SongFormat,
    version: u8,
    name: String,
) -> Result<Instrument, AhxParseError> {
    let r = Reader { buf: bytes };
    let core = parse_instrument_core(&r, 0)?;
    let entry_bytes = plist_entry_bytes(format);
    let expected = INSTRUMENT_CORE_BYTES + core.plist_length as usize * entry_bytes;
    if bytes.len() != expected {
        return Err(AhxParseError::BadInstrumentLength {
            expected,
            got: bytes.len(),
        });
    }
    let mut entries = Vec::with_capacity(core.plist_length as usize);
    for row in 0..core.plist_length as usize {
        let at = INSTRUMENT_CORE_BYTES + row * entry_bytes;
        let byte = |i: usize| r.u8(at + i, "plist entry");
        entries.push(match format {
            SongFormat::Ahx => {
                parse_plist_entry_ahx(byte(0)?, byte(1)?, byte(2)?, byte(3)?, version)
            }
            SongFormat::Hvl => {
                parse_plist_entry_hvl(byte(0)?, byte(1)?, byte(2)?, byte(3)?, byte(4)?)
            }
        });
    }
    Ok(core.finish(name, entries))
}

/// Parses an AHX or HVL file from raw bytes. Auto-detects the format from
/// its magic bytes, mirroring `hvl_ParseTune` exactly: `"THX"` + version < 3
/// is AHX, `"HVL"` + version < 2 is HVL, anything else is rejected.
pub fn parse(buf: &[u8]) -> Result<Song, AhxParseError> {
    if buf.len() < 16 {
        return Err(AhxParseError::TooShort);
    }
    if &buf[0..3] == b"THX" && buf[3] < 3 {
        parse_ahx(buf)
    } else if &buf[0..3] == b"HVL" && buf[3] < 2 {
        parse_hvl(buf)
    } else {
        Err(AhxParseError::BadMagic)
    }
}

/// Returns `true` if `buf` starts with a recognized AHX/HVL magic + version,
/// without doing a full parse. Mirrors the `looksLikeXxx(bytes)` convention
/// the TS-side format parsers use (`ARCH-REVIEW-s3m.md`), kept here so a
/// future `formats/ahx.ts` (P1) has a native-decoder-verified reference for
/// its own sniff check.
pub fn looks_like_ahx_or_hvl(buf: &[u8]) -> bool {
    buf.len() >= 4 && ((&buf[0..3] == b"THX" && buf[3] < 3) || (&buf[0..3] == b"HVL" && buf[3] < 2))
}
