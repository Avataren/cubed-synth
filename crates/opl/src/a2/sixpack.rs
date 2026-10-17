//! SixPack decompressor (Adlib Tracker II modules, versions 1 and 5).
//!
//! Philip G. Gage's SIXPACK (DDJ data compression contest, 1991): LZ77 copies
//! over a 6-range distance code, all symbols coded with one adaptive Huffman
//! tree. Written from the algorithm as laid out in sixpack-kotlin
//! (Benedikt Wüller, 2019, MIT license, "Copyright (c) 2019 Benedikt Wüller";
//! the MIT permission notice applies to the parts derived from it), with three
//! of its bugs fixed: copy-distance bits accumulate LSB first (`mask <<= 1`),
//! the frequency halving runs over the whole tree, and code 256 terminates.
//! Gage's original listing states no license and was not used.
//!
//! AT2's variant differs from Gage's in two ways, both taken from the public
//! header AdPlug installs (`sixdepack.h`: `MAXCOPY = 255`, input as
//! `unsigned short *`) and then MEASURED: every v1/v5 block in the corpus
//! decodes byte for byte like AdPlug's depacker run as a black box.
//! Copies run up to 255 bytes (Gage: 64), and bits come from 16-bit
//! little-endian words.

use super::DepackError;

const MAXFREQ: u16 = 2000;
const MINCOPY: usize = 3;
const MAXCOPY: usize = 255;
const COPYRANGES: usize = 6;
const COPYBITS: [u32; COPYRANGES] = [4, 6, 8, 10, 12, 14];
const CODESPERRANGE: usize = MAXCOPY - MINCOPY + 1;
const TERMINATE: usize = 256;
const FIRSTCODE: usize = 257;
const MAXCHAR: usize = FIRSTCODE + COPYRANGES * CODESPERRANGE - 1;
const SUCCMAX: usize = MAXCHAR + 1;
const TWICEMAX: usize = 2 * MAXCHAR + 1;
const ROOT: usize = 1;

struct Tree {
    dad: Vec<u16>,
    left: Vec<u16>,
    right: Vec<u16>,
    freq: Vec<u16>,
}

impl Tree {
    fn new() -> Tree {
        let mut t = Tree {
            dad: vec![0; TWICEMAX + 1],
            left: vec![0; MAXCHAR + 1],
            right: vec![0; MAXCHAR + 1],
            freq: vec![1; TWICEMAX + 1],
        };
        for i in 2..=TWICEMAX {
            t.dad[i] = (i / 2) as u16;
        }
        for i in 1..=MAXCHAR {
            t.left[i] = (2 * i) as u16;
            t.right[i] = (2 * i + 1) as u16;
        }
        t
    }

    fn sibling(&self, node: usize) -> usize {
        let parent = self.dad[node] as usize;
        if self.left[parent] as usize == node {
            self.right[parent] as usize
        } else {
            self.left[parent] as usize
        }
    }

    /// Re-sums frequencies from `a` (whose sibling is `b`) up to the root,
    /// halving every count once the root reaches MAXFREQ.
    fn update_freq(&mut self, mut a: usize, mut b: usize) {
        loop {
            let parent = self.dad[a] as usize;
            self.freq[parent] = self.freq[a] + self.freq[b];
            a = parent;
            if a == ROOT {
                break;
            }
            b = self.sibling(a);
        }
        if self.freq[ROOT] == MAXFREQ {
            for f in self.freq.iter_mut().skip(1) {
                *f >>= 1;
            }
        }
    }

    /// Counts one occurrence of `code` and swaps nodes to keep the tree
    /// ordered by frequency.
    fn update_model(&mut self, code: usize) {
        let mut a = code + SUCCMAX;
        self.freq[a] += 1;
        if self.dad[a] as usize == ROOT {
            return;
        }
        let mut ua = self.dad[a] as usize;
        let sib = self.sibling(a);
        self.update_freq(a, sib);
        loop {
            let uua = self.dad[ua] as usize;
            let b = if self.left[uua] as usize == ua {
                self.right[uua]
            } else {
                self.left[uua]
            } as usize;
            if self.freq[a] > self.freq[b] {
                if self.left[uua] as usize == ua {
                    self.right[uua] = a as u16;
                } else {
                    self.left[uua] = a as u16;
                }
                let c;
                if self.left[ua] as usize == a {
                    self.left[ua] = b as u16;
                    c = self.right[ua] as usize;
                } else {
                    self.right[ua] = b as u16;
                    c = self.left[ua] as usize;
                }
                self.dad[b] = ua as u16;
                self.dad[a] = uua as u16;
                self.update_freq(b, c);
                a = b;
            }
            a = self.dad[a] as usize;
            ua = self.dad[a] as usize;
            if ua == ROOT {
                break;
            }
        }
    }
}

/// Bits MSB first out of 16-bit little-endian words.
struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    buf: u16,
    left: u8,
}

impl<'a> Bits<'a> {
    fn bit(&mut self) -> Result<bool, DepackError> {
        if self.left == 0 {
            let lo = *self.src.get(self.pos).ok_or(DepackError::InputEnded)?;
            let hi = *self.src.get(self.pos + 1).ok_or(DepackError::InputEnded)?;
            self.pos += 2;
            self.buf = u16::from_le_bytes([lo, hi]);
            self.left = 16;
        }
        let bit = self.buf & 0x8000 != 0;
        self.buf <<= 1;
        self.left -= 1;
        Ok(bit)
    }
}

/// Decompresses one SixPack stream. Stops at the terminate code and fails if
/// the output would exceed `max_out`.
pub fn depack(src: &[u8], max_out: usize) -> Result<(Vec<u8>, usize), DepackError> {
    let mut copymin = [0usize; COPYRANGES];
    let mut d = 0usize;
    for i in 0..COPYRANGES {
        copymin[i] = d;
        d += 1 << COPYBITS[i];
    }

    let mut tree = Tree::new();
    let mut bits = Bits {
        src,
        pos: 0,
        buf: 0,
        left: 0,
    };
    let mut out: Vec<u8> = Vec::with_capacity(max_out);
    loop {
        let mut node = ROOT;
        while node <= MAXCHAR {
            node = if bits.bit()? {
                tree.right[node]
            } else {
                tree.left[node]
            } as usize;
        }
        let code = node - SUCCMAX;
        tree.update_model(code);
        if code < 256 {
            out.push(code as u8);
        } else if code == TERMINATE {
            break;
        } else {
            let index = (code - FIRSTCODE) / CODESPERRANGE;
            let len = code - FIRSTCODE + MINCOPY - index * CODESPERRANGE;
            let mut extra = 0usize;
            for i in 0..COPYBITS[index] {
                if bits.bit()? {
                    extra |= 1 << i;
                }
            }
            let dist = extra + len + copymin[index];
            if dist > out.len() {
                return Err(DepackError::Corrupt(
                    "copy distance before the start of the output",
                ));
            }
            if out.len() + len > max_out {
                return Err(DepackError::TooLong { limit: max_out });
            }
            let start = out.len() - dist;
            for i in 0..len {
                let b = out[start + i];
                out.push(b);
            }
        }
        if out.len() > max_out {
            return Err(DepackError::TooLong { limit: max_out });
        }
    }
    Ok((out, bits.pos))
}
