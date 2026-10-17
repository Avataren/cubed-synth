//! aPLib decompressor (Adlib Tracker II modules, versions 9–11).
//!
//! Written from the aPLib bitstream as decoded by apultra's `src/expand.c`
//! (Emmanuel Marty, 2019), whose license asks for this notice:
//!
//! > This software is provided 'as-is', without any express or implied
//! > warranty. In no event will the authors be held liable for any damages
//! > arising from the use of this software.
//! >
//! > Permission is granted to anyone to use this software for any purpose,
//! > including commercial applications, and to alter it and redistribute it
//! > freely, subject to the following restrictions:
//! >
//! > 1. The origin of this software must not be misrepresented; you must not
//! >    claim that you wrote the original software. If you use this software
//! >    in a product, an acknowledgment in the product documentation would be
//! >    appreciated but is not required.
//! > 2. Altered source versions must be plainly marked as such, and must not be
//! >    misrepresented as being the original software.
//! > 3. This notice may not be removed or altered from any source distribution.
//!
//! This is a rewrite in Rust, not a copy: bounds are checked on every read and
//! every copy, and a stream that ends without its end marker is an error.
//!
//! **Adlib Tracker II's aPLib differs from today's in one rule** (MEASURED:
//! all 646 v9–11 blocks in the corpus decode byte for byte like AdPlug's
//! depacker, run as a black box, only with it). Current aPLib subtracts 2
//! from a match's gamma-coded high offset right after another match and 3
//! otherwise, which frees one code. AT2's streams always subtract 3, so a high
//! value of 2 always means "repeat the last offset". Presumably the stream of
//! the older aPLib release AT2 linked (INFERRED).

use super::DepackError;

struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    tag: u8,
    left: u8,
}

impl<'a> Bits<'a> {
    fn byte(&mut self) -> Result<u8, DepackError> {
        let b = *self.src.get(self.pos).ok_or(DepackError::InputEnded)?;
        self.pos += 1;
        Ok(b)
    }

    /// Tag bits come MSB first from their own bytes, interleaved with the
    /// literal and offset bytes in stream order.
    fn bit(&mut self) -> Result<u32, DepackError> {
        if self.left == 0 {
            self.tag = self.byte()?;
            self.left = 8;
        }
        let bit = (self.tag >> 7) as u32;
        self.tag <<= 1;
        self.left -= 1;
        Ok(bit)
    }

    /// Elias-gamma variant: starts at 1, each step appends a data bit, then a
    /// continue bit. Always ≥ 2.
    fn gamma(&mut self) -> Result<u32, DepackError> {
        let mut v: u32 = 1;
        loop {
            v = v
                .checked_mul(2)
                .ok_or(DepackError::Corrupt("gamma code overflows"))?
                + self.bit()?;
            if self.bit()? == 0 {
                return Ok(v);
            }
        }
    }
}

/// Decompresses one aPLib stream (no header). Stops at the end marker and
/// fails if the output would exceed `max_out`.
pub fn depack(src: &[u8], max_out: usize) -> Result<(Vec<u8>, usize), DepackError> {
    let mut bits = Bits {
        src,
        pos: 0,
        tag: 0,
        left: 0,
    };
    let mut out: Vec<u8> = Vec::with_capacity(max_out.min(1 << 21));
    out.push(bits.byte()?);
    let mut rep_offset: usize = 0;

    loop {
        if bits.bit()? == 0 {
            out.push(bits.byte()?);
        } else if bits.bit()? == 0 {
            // '10': gamma-coded high offset + one low byte, or a repeat.
            let hi = bits.gamma()?;
            let len;
            if hi >= 3 {
                let offset = (((hi - 3) as usize) << 8) | bits.byte()? as usize;
                let mut l = bits.gamma()? as usize;
                if !(128..32000).contains(&offset) {
                    l += 2;
                } else if offset >= 1280 {
                    l += 1;
                }
                rep_offset = offset;
                len = l;
            } else {
                len = bits.gamma()? as usize;
            }
            copy_match(&mut out, rep_offset, len, max_out)?;
        } else if bits.bit()? == 0 {
            // '110': 7-bit offset, 1-bit length; offset 0 ends the stream.
            let cmd = bits.byte()?;
            if cmd == 0 {
                break;
            }
            rep_offset = (cmd >> 1) as usize;
            copy_match(&mut out, rep_offset, 2 + (cmd & 1) as usize, max_out)?;
        } else {
            // '111': a single byte from 1..15 back, or a zero.
            let mut offset = 0usize;
            for _ in 0..4 {
                offset = (offset << 1) | bits.bit()? as usize;
            }
            if offset == 0 {
                out.push(0);
            } else {
                copy_match(&mut out, offset, 1, max_out)?;
            }
        }
        if out.len() > max_out {
            return Err(DepackError::TooLong { limit: max_out });
        }
    }
    Ok((out, bits.pos))
}

fn copy_match(
    out: &mut Vec<u8>,
    offset: usize,
    len: usize,
    max_out: usize,
) -> Result<(), DepackError> {
    if offset == 0 || offset > out.len() {
        return Err(DepackError::Corrupt(
            "match offset before the start of the output",
        ));
    }
    if out.len() + len > max_out {
        return Err(DepackError::TooLong { limit: max_out });
    }
    let start = out.len() - offset;
    for i in 0..len {
        let b = out[start + i];
        out.push(b);
    }
    Ok(())
}

/// A tag-bit writer: the same stream `Bits` reads, with each tag byte
/// reserved where the reader will first want it.
struct BitWriter {
    out: Vec<u8>,
    tag_at: usize,
    left: u8,
}

impl BitWriter {
    fn bit(&mut self, b: u32) {
        if self.left == 0 {
            self.tag_at = self.out.len();
            self.out.push(0);
            self.left = 8;
        }
        self.left -= 1;
        if b != 0 {
            self.out[self.tag_at] |= 1 << self.left;
        }
    }

    fn gamma(&mut self, v: u32) {
        debug_assert!(v >= 2);
        let top = 31 - v.leading_zeros();
        for i in (0..top).rev() {
            self.bit((v >> i) & 1);
            self.bit(u32::from(i > 0));
        }
    }
}

const MAX_OFFSET: usize = 31_999;
const MAX_MATCH: usize = 1 << 14;
const CHAIN: usize = 48;

/// Compresses `src` into a stream `depack` (and AT2's own reader) takes. A
/// greedy hash-chain matcher: nothing like aPLib's optimal parse, but a run
/// of zeros (most of a song's data) costs a few bytes, and the output is the
/// plain aPLib bitstream. Not byte-identical to what AT2 writes. `src` must
/// not be empty.
pub fn pack(src: &[u8]) -> Vec<u8> {
    let mut w = BitWriter {
        out: Vec::with_capacity(src.len() / 4 + 16),
        tag_at: 0,
        left: 0,
    };
    w.out.push(src[0]);
    let n = src.len();
    let hash = |i: usize| -> usize {
        ((src[i] as usize) << 16 | (src[i + 1] as usize) << 8 | src[i + 2] as usize)
            .wrapping_mul(0x9E37_79B1)
            >> 12
            & 0xFFFFF
    };
    let mut head = vec![usize::MAX; 1 << 20];
    let mut prev = vec![usize::MAX; n];
    let insert = |head: &mut Vec<usize>, prev: &mut Vec<usize>, i: usize| {
        if i + 3 <= n {
            let h = hash(i);
            prev[i] = head[h];
            head[h] = i;
        }
    };
    insert(&mut head, &mut prev, 0);
    let mut pos = 1;
    while pos < n {
        let (mut best_len, mut best_off) = (0usize, 0usize);
        if pos + 3 <= n {
            let mut cand = head[hash(pos)];
            let mut tries = 0;
            let limit = (n - pos).min(MAX_MATCH);
            while cand != usize::MAX && tries < CHAIN {
                let off = pos - cand;
                if off > MAX_OFFSET {
                    break;
                }
                let mut l = 0;
                while l < limit && src[cand + l] == src[pos + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_off = off;
                }
                cand = prev[cand];
                tries += 1;
            }
        }
        // The shortest length each offset class can code.
        let min_len = if best_off < 128 {
            2
        } else if best_off >= 1280 {
            3
        } else {
            2
        };
        if best_len >= min_len.max(3) {
            let (len, off) = (best_len, best_off);
            if off < 128 && len <= 3 {
                w.bit(1);
                w.bit(1);
                w.bit(0);
                w.out.push(((off << 1) | (len - 2)) as u8);
            } else {
                w.bit(1);
                w.bit(0);
                w.gamma((off >> 8) as u32 + 3);
                w.out.push((off & 255) as u8);
                let adj = if off < 128 {
                    2
                } else if off >= 1280 {
                    1
                } else {
                    0
                };
                w.gamma((len - adj) as u32);
            }
            for i in pos..pos + len {
                insert(&mut head, &mut prev, i);
            }
            pos += len;
        } else {
            w.bit(0);
            w.out.push(src[pos]);
            insert(&mut head, &mut prev, pos);
            pos += 1;
        }
    }
    w.bit(1);
    w.bit(1);
    w.bit(0);
    w.out.push(0);
    w.out
}
