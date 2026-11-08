//! Helpers for the PSID tests: a `.sid` file made from a header description and
//! the C64 bytes, as another_synth's `psid-builder.ts` does.

use super::file::PsidFile;
use super::runner::PsidRunner;

#[derive(Clone)]
pub struct Header {
    pub magic: &'static [u8; 4],
    pub version: u16,
    pub load: u16,
    pub init: u16,
    pub play: u16,
    pub songs: u16,
    pub start: u16,
    pub speed: u32,
    pub flags: u16,
    pub second: u8,
    pub third: u8,
}

impl Default for Header {
    /// A v2 PSID, PAL, 6581, loading at `$1000`, calling init there and play at `$1003`.
    fn default() -> Header {
        Header {
            magic: b"PSID",
            version: 2,
            load: 0x1000,
            init: 0x1000,
            play: 0x1003,
            songs: 1,
            start: 1,
            speed: 0,
            flags: 0x14,
            second: 0,
            third: 0,
        }
    }
}

pub fn build_psid(h: &Header, data: &[u8]) -> Vec<u8> {
    let data_offset: usize = if h.version == 1 { 0x76 } else { 0x7c };
    let mut out = vec![0u8; data_offset];
    out[..4].copy_from_slice(h.magic);
    let mut word = |at: usize, v: u16| out[at..at + 2].copy_from_slice(&v.to_be_bytes());
    word(4, h.version);
    word(6, data_offset as u16);
    word(8, h.load);
    word(10, h.init);
    word(12, h.play);
    word(14, h.songs);
    word(16, h.start);
    out[18..22].copy_from_slice(&h.speed.to_be_bytes());
    out[0x16..0x16 + 4].copy_from_slice(b"Test");
    out[0x36..0x36 + 5].copy_from_slice(b"Tests");
    if h.version >= 2 {
        out[0x76..0x78].copy_from_slice(&h.flags.to_be_bytes());
        out[0x7a] = h.second;
        out[0x7b] = h.third;
    }
    out.extend_from_slice(data);
    out
}

/// `code` placed at `offset` for each `(offset, code)`, as one run of bytes (gaps are 0).
pub fn place_code(parts: &[(usize, &[u8])]) -> Vec<u8> {
    let end = parts.iter().map(|(at, c)| at + c.len()).max().unwrap_or(0);
    let mut out = vec![0u8; end];
    for (at, c) in parts {
        out[*at..*at + c.len()].copy_from_slice(c);
    }
    out
}

pub fn file_of(bytes: &[u8]) -> PsidFile {
    PsidFile::parse(bytes).expect("a valid test file")
}

pub fn created(file: &PsidFile, subsong: usize) -> PsidRunner {
    PsidRunner::new(file, subsong).expect("the tune initialises")
}

/// `$F0` counts 1, 2, 3, 0, ... and goes to `$D400`.
pub const COUNT_TO_D400: [u8; 11] = [
    0xa6, 0xf0, 0xe8, 0x8a, 0x29, 0x03, 0x85, 0xf0, 0x8d, 0x00, 0xd4,
];

/// One write, as the tests compare them: cycle, register, value.
pub type W = (u64, u8, u8);

/// Everything `runner` writes to the first chip in its first `cycles` cycles,
/// advanced in steps of `step`.
pub fn writes_over(runner: &mut PsidRunner, cycles: u64, step: u64) -> Vec<W> {
    let mut out = vec![];
    let mut t = step;
    while t <= cycles {
        runner.advance(t);
        assert!(
            runner.horizon() >= t,
            "horizon {} below {t}",
            runner.horizon()
        );
        runner.drain(t, 1, |w| out.push((w.cycle, w.reg, w.value)));
        t += step;
    }
    out
}
