//! Adlib Tracker II's "own LZH" (modules, versions 12–14).
//!
//! It is Haruhiko Okumura's public-domain ar002 (the LHA `-lh5-` family:
//! blocks of static canonical Huffman codes over literals, match lengths and
//! distance classes), with wider header fields and a shorter minimum match:
//!
//! | ar002 | stock | AT2 |
//! |---|---|---|
//! | `DICBIT` (distance classes = DICBIT + 1) | 13 | 14 |
//! | `THRESHOLD` (shortest match) | 3 | 2 |
//! | `CBIT` (bits of the literal/length count) | 9 | 16 |
//! | `PBIT` (bits of the distance-class count) | 4 | 14 |
//! | `TBIT` (bits of the code-length count) | 5 | 15 |
//!
//! The five values, and the meaning of the flag byte below, come from AdPlug's
//! `unlzh.c` (LGPL, read under plan decision D6). Only those facts were used:
//! this decoder ports a Python ar002 reader written from scratch before that
//! file was opened. The corpus checks them: every v12–14 block decodes byte for
//! byte like AdPlug's, and its last code ends in the block's last byte. See
//! `.ai/plan-opl.md`, O6.
//!
//! Container (MEASURED): one flag byte (bit 0 set = "ultra", never seen), the
//! unpacked size as a little-endian u32, then the bitstream.

use super::DepackError;

const DICBIT: usize = 14;
const THRESHOLD: usize = 2;
const MAXMATCH: usize = 256;
const NC: usize = 255 + MAXMATCH + 2 - THRESHOLD;
const CBIT: u32 = 16;
const NP: usize = DICBIT + 1;
const PBIT: u32 = 14;
const NT: usize = 16 + 3;
const TBIT: u32 = 15;
const MAX_CODE_LEN: usize = 16;

struct Bits<'a> {
    src: &'a [u8],
    bit: usize,
}

impl<'a> Bits<'a> {
    fn get(&mut self, n: u32) -> Result<u32, DepackError> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.src.get(self.bit >> 3).ok_or(DepackError::InputEnded)?;
            v = (v << 1) | ((byte >> (7 - (self.bit & 7))) & 1) as u32;
            self.bit += 1;
        }
        Ok(v)
    }
}

/// A canonical Huffman code: shorter codes first, equal lengths by symbol.
/// `Single` is the "one symbol, zero bits" form a table header can declare.
enum Code {
    Single(usize),
    Canonical {
        /// Number of codes of each length 1..=16.
        count: [u16; MAX_CODE_LEN + 1],
        /// Symbols in code order.
        symbols: Vec<u16>,
    },
}

impl Code {
    fn from_lengths(lens: &[u8], what: &'static str) -> Result<Code, DepackError> {
        let mut count = [0u16; MAX_CODE_LEN + 1];
        for &l in lens {
            if l as usize > MAX_CODE_LEN {
                return Err(DepackError::Corrupt(what));
            }
            count[l as usize] += 1;
        }
        count[0] = 0;
        // Kraft sum must be exactly 1: an incomplete or oversubscribed table is
        // a corrupt stream (ar002 prints "Bad table" and carries on).
        let mut room: i64 = 1;
        for &c in count.iter().skip(1) {
            room = room * 2 - c as i64;
            if room < 0 {
                return Err(DepackError::Corrupt(what));
            }
        }
        if room != 0 {
            return Err(DepackError::Corrupt(what));
        }
        let mut symbols = Vec::new();
        for len in 1..=MAX_CODE_LEN as u8 {
            for (sym, &l) in lens.iter().enumerate() {
                if l == len {
                    symbols.push(sym as u16);
                }
            }
        }
        Ok(Code::Canonical { count, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Result<usize, DepackError> {
        match self {
            Code::Single(sym) => Ok(*sym),
            Code::Canonical { count, symbols } => {
                let mut code: u32 = 0;
                let mut first: u32 = 0;
                let mut index: u32 = 0;
                for &n in &count[1..] {
                    code |= bits.get(1)?;
                    let n = n as u32;
                    if code < first + n {
                        return Ok(symbols[(index + code - first) as usize] as usize);
                    }
                    index += n;
                    first = (first + n) << 1;
                    code <<= 1;
                }
                Err(DepackError::Corrupt("Huffman code longer than 16 bits"))
            }
        }
    }
}

/// Code lengths of the code-length code (`nn`=NT, with the 2-bit zero run after
/// the third entry) and of the distance classes (`nn`=NP, no run).
fn read_pt(
    bits: &mut Bits,
    nn: usize,
    nbit: u32,
    special: Option<usize>,
    what: &'static str,
) -> Result<Code, DepackError> {
    let n = bits.get(nbit)? as usize;
    if n == 0 {
        let sym = bits.get(nbit)? as usize;
        if sym >= nn {
            return Err(DepackError::Corrupt(what));
        }
        return Ok(Code::Single(sym));
    }
    if n > nn {
        return Err(DepackError::Corrupt(what));
    }
    let mut lens = vec![0u8; nn];
    let mut i = 0;
    while i < n {
        let mut c = bits.get(3)?;
        if c == 7 {
            while bits.get(1)? == 1 {
                c += 1;
                if c as usize > MAX_CODE_LEN {
                    return Err(DepackError::Corrupt(what));
                }
            }
        }
        lens[i] = c as u8;
        i += 1;
        if Some(i) == special {
            for _ in 0..bits.get(2)? {
                if i >= nn {
                    return Err(DepackError::Corrupt(what));
                }
                lens[i] = 0;
                i += 1;
            }
        }
    }
    Code::from_lengths(&lens, what)
}

/// Code lengths of the literal/length alphabet, themselves coded with `pt`.
fn read_c(bits: &mut Bits, pt: &Code) -> Result<Code, DepackError> {
    const WHAT: &str = "bad literal/length table";
    let n = bits.get(CBIT)? as usize;
    if n == 0 {
        let sym = bits.get(CBIT)? as usize;
        if sym >= NC {
            return Err(DepackError::Corrupt(WHAT));
        }
        return Ok(Code::Single(sym));
    }
    if n > NC {
        return Err(DepackError::Corrupt(WHAT));
    }
    let mut lens = vec![0u8; NC];
    let mut i = 0;
    while i < n {
        let c = pt.decode(bits)?;
        let zeros = match c {
            0 => 1,
            1 => bits.get(4)? as usize + 3,
            2 => bits.get(CBIT)? as usize + 20,
            _ => {
                lens[i] = (c - 2) as u8;
                i += 1;
                continue;
            }
        };
        if i + zeros > NC {
            return Err(DepackError::Corrupt(WHAT));
        }
        i += zeros;
    }
    Code::from_lengths(&lens, WHAT)
}

/// Decompresses one AT2 LZH block (flag byte, u32 size, bitstream). Returns
/// the output and the number of input bytes the bitstream used.
pub fn depack(src: &[u8], max_out: usize) -> Result<(Vec<u8>, usize), DepackError> {
    if src.len() < 5 {
        return Err(DepackError::InputEnded);
    }
    if src[0] & 1 != 0 {
        return Err(DepackError::Unsupported(
            "LZH 'ultra' mode (flag bit 0), which no corpus file uses",
        ));
    }
    let size = u32::from_le_bytes([src[1], src[2], src[3], src[4]]) as usize;
    if size > max_out {
        return Err(DepackError::TooLong { limit: max_out });
    }
    let mut bits = Bits {
        src: &src[5..],
        bit: 0,
    };
    let mut out: Vec<u8> = Vec::with_capacity(size);
    while out.len() < size {
        let block = bits.get(16)? as usize;
        if block == 0 {
            return Err(DepackError::Corrupt(
                "LZH stream ended before its stated size",
            ));
        }
        let pt = read_pt(&mut bits, NT, TBIT, Some(3), "bad code-length table")?;
        let ct = read_c(&mut bits, &pt)?;
        let dt = read_pt(&mut bits, NP, PBIT, None, "bad distance table")?;
        for _ in 0..block {
            let c = ct.decode(&mut bits)?;
            if c < 256 {
                out.push(c as u8);
            } else {
                let len = c - 256 + THRESHOLD;
                let class = dt.decode(&mut bits)?;
                let dist = if class == 0 {
                    0
                } else {
                    (1usize << (class - 1)) + bits.get(class as u32 - 1)? as usize
                };
                if dist >= out.len() {
                    return Err(DepackError::Corrupt(
                        "match distance before the start of the output",
                    ));
                }
                let start = out.len() - dist - 1;
                for i in 0..len {
                    let b = out[start + i];
                    out.push(b);
                }
            }
            if out.len() >= size {
                break;
            }
        }
    }
    if out.len() != size {
        return Err(DepackError::Corrupt("LZH match ran past the stated size"));
    }
    Ok((out, 5 + bits.bit.div_ceil(8)))
}

// ---------------------------------------------------------------------------
// The writing side
// ---------------------------------------------------------------------------

struct BitSink {
    out: Vec<u8>,
    bits: usize,
}

impl BitSink {
    fn put(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            if self.bits & 7 == 0 {
                self.out.push(0);
            }
            if (value >> i) & 1 != 0 {
                let last = self.out.len() - 1;
                self.out[last] |= 0x80 >> (self.bits & 7);
            }
            self.bits += 1;
        }
    }
}

/// `(code, length)` per symbol of a complete canonical code, as `Code::decode`
/// reads it.
fn canonical(lens: &[u8]) -> Vec<(u32, u8)> {
    let mut codes = vec![(0u32, 0u8); lens.len()];
    let mut first = 0u32;
    for len in 1..=MAX_CODE_LEN as u8 {
        let mut next = first;
        for (sym, &l) in lens.iter().enumerate() {
            if l == len {
                codes[sym] = (next, len);
                next += 1;
            }
        }
        first = next << 1;
    }
    codes
}

/// The two fixed codes every block uses. The literal/length alphabet has 511
/// symbols: 510 of 9 bits and one (symbol 0) of 8 fill the code exactly. The
/// 15 distance classes get one 3-bit and fourteen 4-bit codes. Nothing is
/// tuned to the data, which the format allows (each block carries its own
/// tables); runs of zeros, the bulk of a song, still match at 256 a time.
fn static_c_lens() -> Vec<u8> {
    let mut lens = vec![9u8; NC];
    lens[0] = 8;
    lens
}

fn static_p_lens() -> Vec<u8> {
    let mut lens = vec![4u8; NP];
    lens[0] = 3;
    lens
}

enum Token {
    Literal(u8),
    /// Match length 3..=256 and distance - 1.
    Match(usize, usize),
}

const WINDOW: usize = 1 << DICBIT;
const CHAIN: usize = 48;

fn tokenize(src: &[u8]) -> Vec<Token> {
    let n = src.len();
    let hash = |i: usize| -> usize {
        (((src[i] as usize) << 16 | (src[i + 1] as usize) << 8 | src[i + 2] as usize)
            .wrapping_mul(0x9E37_79B1)
            >> 12)
            & 0xFFFFF
    };
    let mut head = vec![usize::MAX; 1 << 20];
    let mut prev = vec![usize::MAX; n];
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < n {
        let (mut best_len, mut best_dist) = (0usize, 0usize);
        if pos + 3 <= n {
            let mut cand = head[hash(pos)];
            let limit = (n - pos).min(MAXMATCH);
            let mut tries = 0;
            while cand != usize::MAX && tries < CHAIN {
                let dist = pos - cand;
                if dist > WINDOW {
                    break;
                }
                let mut l = 0;
                while l < limit && src[cand + l] == src[pos + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = dist - 1;
                }
                cand = prev[cand];
                tries += 1;
            }
        }
        let take = if best_len >= 3 { best_len } else { 1 };
        for i in pos..pos + take {
            if i + 3 <= n {
                let h = hash(i);
                prev[i] = head[h];
                head[h] = i;
            }
        }
        tokens.push(if best_len >= 3 {
            Token::Match(best_len, best_dist)
        } else {
            Token::Literal(src[pos])
        });
        pos += take;
    }
    tokens
}

/// Compresses `src` into one AT2 LZH block (flag byte, u32 size, bitstream)
/// that `depack` reads. Static codes and a greedy matcher: valid, compact on
/// song data, not byte-identical to AT2's own packer. `src` must not be empty.
pub fn pack(src: &[u8]) -> Vec<u8> {
    let c_lens = static_c_lens();
    let c_codes = canonical(&c_lens);
    let p_lens = static_p_lens();
    let p_codes = canonical(&p_lens);
    let mut sink = BitSink {
        out: Vec::new(),
        bits: 0,
    };
    let tokens = tokenize(src);
    for block in tokens.chunks(65_535) {
        sink.put(block.len() as u32, 16);
        // Code-length code: symbols 10 and 11 (a length of 8 and 9, plus 2),
        // one bit each.
        sink.put(12, TBIT);
        for i in 0..12 {
            sink.put(u32::from(i >= 10), 3);
            if i + 1 == 3 {
                sink.put(0, 2);
            }
        }
        sink.put(NC as u32, CBIT);
        for &l in &c_lens {
            sink.put(u32::from(l == 9), 1);
        }
        sink.put(NP as u32, PBIT);
        for &l in &p_lens {
            sink.put(l as u32, 3);
        }
        for token in block {
            match *token {
                Token::Literal(b) => {
                    let (code, len) = c_codes[b as usize];
                    sink.put(code, len as u32);
                }
                Token::Match(len, dist) => {
                    let (code, clen) = c_codes[256 + len - THRESHOLD];
                    sink.put(code, clen as u32);
                    let class = if dist == 0 {
                        0
                    } else {
                        usize::BITS as usize - dist.leading_zeros() as usize
                    };
                    let (pcode, plen) = p_codes[class];
                    sink.put(pcode, plen as u32);
                    if class > 1 {
                        sink.put((dist - (1 << (class - 1))) as u32, class as u32 - 1);
                    }
                }
            }
        }
    }
    let mut out = vec![0u8];
    out.extend_from_slice(&(src.len() as u32).to_le_bytes());
    out.extend_from_slice(&sink.out);
    out
}
