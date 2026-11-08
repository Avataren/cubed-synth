//! The PSID/RSID file (`.sid`), versions 1-4, as HVSC's `SID_file_format.txt`
//! lays it out; a port of another_synth's `psid/psid-file.ts`. All header words
//! are big-endian:
//!
//! ```text
//! $00 magic 'PSID' or 'RSID'      $0E songs (1-256)
//! $04 version (1-4)               $10 start song (1-based)
//! $06 data offset ($76 / $7C)     $12 speed: bit n = song n+1 CIA-timed
//! $08 load address (0: the data's first two bytes, little-endian)
//! $0A init address (0: the load address)
//! $0C play address (0: init installs its own interrupt)
//! $16/$36/$56 name, author, released: 32 latin-1 bytes each
//! v2+: $76 flags, $78 relocation start page, $79 pages,
//!      $7A second SID ($42-$FE: $D420-$DFE0), $7B third SID (v4)
//! ```
//!
//! Flags: bit 0 the data is Compute!'s Sidplayer MUS (PSID) or reserved (RSID),
//! bit 1 PlaySID-specific (PSID) or C64 BASIC (RSID), bits 2-3 the clock (01
//! PAL, 10 NTSC, 11 either), bits 4-5 / 6-7 / 8-9 the model of SID 1 / 2 / 3
//! (01 6581, 10 8580, 11 either).

use std::fmt;

/// Why a `.sid` cannot be played. The messages read as the end of "it ...".
#[derive(Clone, Debug, PartialEq)]
pub enum PsidError {
    /// No `PSID`/`RSID` magic.
    NotPsid,
    /// Shorter than a v1 header.
    TooShort(usize),
    BadVersion(u16),
    /// An RSID header of version 1, which the format does not have.
    RsidV1,
    BadDataOffset {
        found: u16,
        expected: u16,
        version: u16,
    },
    /// The file ends inside its header.
    TruncatedHeader,
    /// Nothing after the header.
    NoData,
    /// The data runs past the end of the C64's memory.
    RunsPastMemory {
        len: usize,
        load: u16,
    },
    BadSongCount(u16),
    /// Compute!'s Sidplayer data (a `.mus` wrapped in a PSID): not a program to run.
    Mus,
    /// An RSID that is a C64 BASIC program: it needs the C64's BASIC ROM.
    Basic,
    /// The init routine stopped the CPU (a JAM opcode).
    InitJam {
        addr: u16,
        pc: u16,
    },
    InitBrk {
        addr: u16,
    },
    InitTimeout {
        addr: u16,
        million_cycles: u32,
    },
    /// The sample rate the chip cannot render at.
    SampleRate(f64),
}

fn hex4(v: u16) -> String {
    format!("${v:04X}")
}

impl fmt::Display for PsidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PsidError::NotPsid => f.write_str("it is not a SID file (no PSID/RSID magic)"),
            PsidError::TooShort(n) => {
                write!(f, "the file is {n} bytes, shorter than a SID header")
            }
            PsidError::BadVersion(v) => {
                write!(f, "its header is version {v}; SID files are versions 1-4")
            }
            PsidError::RsidV1 => {
                f.write_str("it is an RSID file of version 1, which the format does not have")
            }
            PsidError::BadDataOffset {
                found,
                expected,
                version,
            } => write!(
                f,
                "its data starts at {}; a version {version} file's starts at {}",
                hex4(*found),
                hex4(*expected)
            ),
            PsidError::TruncatedHeader => f.write_str("the file ends inside its header"),
            PsidError::NoData => f.write_str("the file has no C64 data"),
            PsidError::RunsPastMemory { len, load } => write!(
                f,
                "its {len} bytes load at {} and run past the end of the C64's memory",
                hex4(*load)
            ),
            PsidError::BadSongCount(n) => {
                write!(f, "its header says {n} songs; a SID file has 1-256")
            }
            PsidError::Mus => f.write_str(
                "it holds Compute!'s Sidplayer (MUS) data, not a program to run",
            ),
            PsidError::Basic => f.write_str(
                "it is a C64 BASIC program; running it needs the C64's BASIC ROM, which this player does not have",
            ),
            PsidError::InitJam { addr, pc } => write!(
                f,
                "the tune's init routine ({}) stopped the CPU (a JAM opcode at {})",
                hex4(*addr),
                hex4(*pc)
            ),
            PsidError::InitBrk { addr } => {
                write!(f, "the tune's init routine ({}) hit a BRK", hex4(*addr))
            }
            PsidError::InitTimeout {
                addr,
                million_cycles,
            } => write!(
                f,
                "the tune's init routine ({}) did not return in {million_cycles} million cycles",
                hex4(*addr)
            ),
            PsidError::SampleRate(rate) => {
                write!(f, "unsupported sample rate {rate} Hz (8000..=192000)")
            }
        }
    }
}

impl std::error::Error for PsidError {}

/// `PSID` (a player is called by the host) or `RSID` (a real C64 environment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PsidType {
    Psid,
    Rsid,
}

/// The video standard the header names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PsidClock {
    Unknown,
    Pal,
    Ntsc,
    Any,
}

/// The SID model the header names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PsidSidModel {
    Unknown,
    Sid6581,
    Sid8580,
    Any,
}

const V1_HEADER: usize = 0x76;
const V2_HEADER: usize = 0x7c;
const TEXT: usize = 32;

/// The magic of a PSID or RSID file.
pub fn looks_like_psid(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && (&bytes[..4] == b"PSID" || &bytes[..4] == b"RSID")
}

fn word(b: &[u8], at: usize) -> u16 {
    ((b[at] as u16) << 8) | b[at + 1] as u16
}

/// A header text: latin-1, cut at the first zero (a full 32 bytes needs none).
fn text(b: &[u8], at: usize) -> String {
    b[at..at + TEXT]
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as char)
        .collect()
}

fn clock_of(v: u16) -> PsidClock {
    [
        PsidClock::Unknown,
        PsidClock::Pal,
        PsidClock::Ntsc,
        PsidClock::Any,
    ][(v & 3) as usize]
}

fn model_of(v: u16) -> PsidSidModel {
    [
        PsidSidModel::Unknown,
        PsidSidModel::Sid6581,
        PsidSidModel::Sid8580,
        PsidSidModel::Any,
    ][(v & 3) as usize]
}

/// A second/third SID's address from its header byte: `$42-$7F` and `$E0-$FE`
/// (even) are `$D420-$D7F0` and `$DE00-$DFE0`; anything else is no SID.
fn extra_sid_address(v: u8) -> Option<u16> {
    if v & 1 != 0 {
        return None;
    }
    if (0x42..=0x7f).contains(&v) || (0xe0..=0xfe).contains(&v) {
        return Some(0xd000 | ((v as u16) << 4));
    }
    None
}

/// A parsed `.sid` file.
#[derive(Clone, Debug)]
pub struct PsidFile {
    pub kind: PsidType,
    pub version: u16,
    /// Where `data` goes in the C64's memory.
    pub load_address: u16,
    /// Called with the subsong (0-based) in A. The load address when the header says 0.
    pub init_address: u16,
    /// Called once per tick; 0: the init routine installs its own interrupt handler.
    pub play_address: u16,
    /// 1-256.
    pub songs: u16,
    /// 1-based, within `songs` (the header's 0 or out-of-range value reads as 1).
    pub start_song: u16,
    /// The raw speed word: bit n set = song n+1 (songs past 32: bit 31) is timed by CIA 1 timer A.
    pub speed: u32,
    pub name: String,
    pub author: String,
    pub released: String,
    /// The raw flags word (0 in a v1 file).
    pub flags: u16,
    pub clock: PsidClock,
    /// The first SID's model.
    pub sid_model: PsidSidModel,
    /// The models of the second and third SID, when the file has them.
    pub extra_models: [PsidSidModel; 2],
    /// The addresses of a second and third SID, if the file names them (v3/v4).
    pub extra_sids: Vec<u16>,
    /// Relocation hints (v2+): the free memory range a player may use; 0 = none said.
    pub reloc_start_page: u8,
    pub reloc_pages: u8,
    /// The C64 bytes, without the load-address prefix.
    pub data: Vec<u8>,
}

impl PsidFile {
    /// `bytes` as a PSID/RSID file, or the true reason it is not one this player can run.
    pub fn parse(bytes: &[u8]) -> Result<PsidFile, PsidError> {
        if !looks_like_psid(bytes) {
            return Err(PsidError::NotPsid);
        }
        if bytes.len() < V1_HEADER {
            return Err(PsidError::TooShort(bytes.len()));
        }
        let kind = if &bytes[..4] == b"PSID" {
            PsidType::Psid
        } else {
            PsidType::Rsid
        };
        let version = word(bytes, 4);
        if !(1..=4).contains(&version) {
            return Err(PsidError::BadVersion(version));
        }
        if kind == PsidType::Rsid && version < 2 {
            return Err(PsidError::RsidV1);
        }
        let data_offset = word(bytes, 6);
        let expected = if version == 1 { V1_HEADER } else { V2_HEADER } as u16;
        if data_offset != expected {
            return Err(PsidError::BadDataOffset {
                found: data_offset,
                expected,
                version,
            });
        }
        if bytes.len() < data_offset as usize {
            return Err(PsidError::TruncatedHeader);
        }

        let mut load_address = word(bytes, 8);
        let mut data = &bytes[data_offset as usize..];
        if load_address == 0 {
            if data.len() < 2 {
                return Err(PsidError::NoData);
            }
            load_address = data[0] as u16 | ((data[1] as u16) << 8);
            data = &data[2..];
        }
        if data.is_empty() {
            return Err(PsidError::NoData);
        }
        if load_address as usize + data.len() > 0x10000 {
            return Err(PsidError::RunsPastMemory {
                len: data.len(),
                load: load_address,
            });
        }
        let init = word(bytes, 10);
        let init_address = if init != 0 { init } else { load_address };
        let play_address = word(bytes, 12);
        let songs = word(bytes, 14);
        if !(1..=256).contains(&songs) {
            return Err(PsidError::BadSongCount(songs));
        }
        let start = word(bytes, 16);
        let start_song = if (1..=songs).contains(&start) {
            start
        } else {
            1
        };
        let speed = u32::from_be_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);

        let flags = if version >= 2 { word(bytes, 0x76) } else { 0 };
        if kind == PsidType::Psid && flags & 1 != 0 {
            return Err(PsidError::Mus);
        }
        if kind == PsidType::Rsid && flags & 2 != 0 {
            return Err(PsidError::Basic);
        }
        let mut extra_sids = Vec::new();
        if version >= 3 {
            if let Some(second) = extra_sid_address(bytes[0x7a]) {
                extra_sids.push(second);
                if version >= 4 {
                    if let Some(third) = extra_sid_address(bytes[0x7b]) {
                        if third != second {
                            extra_sids.push(third);
                        }
                    }
                }
            }
        }
        Ok(PsidFile {
            kind,
            version,
            load_address,
            init_address,
            play_address,
            songs,
            start_song,
            speed,
            name: text(bytes, 0x16),
            author: text(bytes, 0x36),
            released: text(bytes, 0x56),
            flags,
            clock: clock_of(flags >> 2),
            sid_model: model_of(flags >> 4),
            extra_models: [model_of(flags >> 6), model_of(flags >> 8)],
            extra_sids,
            reloc_start_page: if version >= 2 { bytes[0x78] } else { 0 },
            reloc_pages: if version >= 2 { bytes[0x79] } else { 0 },
            data: data.to_vec(),
        })
    }

    /// Whether song `song` (1-based) is timed by CIA 1 timer A (else by the
    /// vertical blank). RSID: always its own timing.
    pub fn uses_cia(&self, song: u16) -> bool {
        if self.kind == PsidType::Rsid {
            return false;
        }
        let bit = (song.max(1) - 1).min(31);
        (self.speed >> bit) & 1 == 1
    }

    /// Whether the file's PSID flags say PlaySID-specific data (samples through PlaySID's own registers).
    pub fn is_play_sid_specific(&self) -> bool {
        self.kind == PsidType::Psid && self.flags & 2 != 0
    }

    /// The header's start song as a 0-based subsong.
    pub fn start_subsong(&self) -> usize {
        self.start_song as usize - 1
    }
}
