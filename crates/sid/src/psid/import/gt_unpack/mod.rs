//! A `.sid` that GoatTracker made, unpacked into the song it was made from: the
//! inverse of GoatTracker's packer. A port of `psid/gt-unpack/index.ts`.
//!
//! The file is GoatTracker's player (`player.s`, the build its relocator chose
//! for the song) with the song data after it. [`player_match`] recovers the
//! build from the code: the defines and the address of every table the code
//! reads. The data is then read back through each of the packer's steps:
//! orderlists (a repeat is written after its pattern), patterns (an unchanged
//! command and a repeated instrument are left out, runs of rests packed, a
//! tempo's parameter one less), the instruments renumbered into normal,
//! no-hard-restart and legato groups, the tables as the build encodes them.
//! What comes out is a GoatTracker 2 song file (GTS5), read like any `.sng`, so
//! the song is the one a `.sng` import makes.
//!
//! What the packer dropped does not come back: instrument names, instruments
//! and table rows nothing used, the instrument written again on a note that
//! already had it. None of it is heard.
//!
//! One difference from the DAW's: the DAW then packs the song again and calls
//! the unpack `exact` when that gives the file's bytes. That packer (and the
//! 6502 assembler it needs) is not ported; the caller (`import`) accepts the
//! unpacked song when it plays like the file (fidelity of 0.98 or more) instead,
//! which the DAW does for the unpacks it cannot reproduce byte for byte.

pub mod asm;
pub mod player_match;

use super::flat::gt_table_duplicate_rows;
use crate::goattracker::{self, GtSongHints};
use crate::gt_note_freq_reg;
use crate::psid::file::{PsidFile, PsidSidModel, PsidType};
use crate::song::{SidSong, MAX_INSTRUMENTS};
use crate::SidModel;
use asm::{parse_asm_tree, Stmt};
use player_match::{match_player, PlayerMatch};
use std::collections::HashMap;
use std::sync::OnceLock;

/// GoatTracker's playroutine source, as shipped (V2.73; its header puts it outside the GPL: "Use it, or
/// song binaries created from it freely for any purpose").
pub const GT_PLAYER_SOURCE: &str = include_str!("player.s");

/// A GoatTracker 2.x multispeed front's timer period at 1x (`PAL_FRAME_CIA`).
const PAL_FRAME_CIA: i64 = 0x4cc7;

/// What an unpack made.
#[derive(Clone, Debug)]
pub struct GtUnpack {
    pub song: SidSong,
    /// Things the user should know.
    pub notes: Vec<String>,
}

fn tree() -> Result<&'static [Stmt], String> {
    static TREE: OnceLock<Result<Vec<Stmt>, String>> = OnceLock::new();
    TREE.get_or_init(|| {
        parse_asm_tree(GT_PLAYER_SOURCE)
            .map_err(|e| format!("player.s does not parse (line {}: {})", e.line, e.reason))
    })
    .as_ref()
    .map(|v| v.as_slice())
    .map_err(|e| e.clone())
}

fn hex4(v: i64) -> String {
    format!("${:04X}", v)
}

/// GoatTracker's own multispeed front (`gt-psid.ts`): `ldx #lo / stx $dc04 / ldx #hi / stx $dc05`.
fn speed_multiplier_of(file: &PsidFile, base: i64) -> Option<u8> {
    let init = file.init_address as i64;
    if init == base {
        return Some(1);
    }
    if init != base - 10 {
        return None;
    }
    let at = |a: i64| -> i32 {
        let i = a - file.load_address as i64;
        if i >= 0 {
            file.data.get(i as usize).map_or(-1, |&b| b as i32)
        } else {
            -1
        }
    };
    let s = base - 10;
    let expect: [i32; 10] = [0xa2, -1, 0x8e, 0x04, 0xdc, 0xa2, -1, 0x8e, 0x05, 0xdc];
    if !expect
        .iter()
        .enumerate()
        .all(|(i, &b)| b < 0 || at(s + i as i64) == b)
    {
        return None;
    }
    let latch = (at(s + 1) | (at(s + 6) << 8)) as i64;
    if latch == 0 {
        return None;
    }
    let mult = (PAL_FRAME_CIA as f64 / latch as f64).round() as i64;
    if (2..=16).contains(&mult) && PAL_FRAME_CIA / mult == latch {
        Some(mult as u8)
    } else {
        None
    }
}

/// Unpack `file` if GoatTracker made it; the reason it is not one otherwise. Never panics.
pub fn unpack_gt_sid(file: &PsidFile) -> Result<GtUnpack, String> {
    if file.kind != PsidType::Psid || file.play_address == 0 {
        return Err("it is not a PSID with a play address".to_string());
    }
    let base = file.play_address as i64 - 3;
    if base < file.load_address as i64 {
        return Err("its play address is not 3 bytes into a player".to_string());
    }
    let Some(mult) = speed_multiplier_of(file, base) else {
        return Err("its init is not GoatTracker's".to_string());
    };
    let load = file.load_address as i64;
    let mem = |a: i64| -> Option<u8> {
        if a >= load && a < load + file.data.len() as i64 {
            Some(file.data[(a - load) as usize])
        } else {
            None
        }
    };
    let known: HashMap<String, i64> = [("base".to_string(), base)].into_iter().collect();
    let Some(matched) = match_player(tree()?, &mem, &known, Some("mt_freqtbllo")) else {
        return Err("its code is not GoatTracker 2's player (V2.73)".to_string());
    };
    unpack(file, &matched, &mem, mult)
}

struct Table {
    left: Vec<i32>,
    right: Vec<i32>,
}

fn unpack(
    file: &PsidFile,
    m: &PlayerMatch,
    mem: &dyn Fn(i64) -> Option<u8>,
    mult: u8,
) -> Result<GtUnpack, String> {
    let sym = &m.symbols;
    let mut notes: Vec<String> = Vec::new();
    let need = |name: &str| -> Result<i64, String> {
        sym.get(name)
            .copied()
            .ok_or_else(|| format!("its player does not show where {name} is"))
    };
    let flag = |name: &str| -> bool { sym.get(name) == Some(&1) };
    let byte = |a: i64| -> Result<i64, String> {
        mem(a)
            .map(|b| b as i64)
            .ok_or_else(|| format!("its song data runs past the file's end ({})", hex4(a)))
    };
    let bytes =
        |a: i64, n: i64| -> Result<Vec<i64>, String> { (0..n).map(|i| byte(a + i)).collect() };

    // The frequency table: GoatTracker's, from the song's first note.
    let first_note = sym.get("FIRSTNOTE").copied().unwrap_or(0);
    let freq_lo = need("mt_freqtbllo")?;
    let freq_hi = need("mt_freqtblhi")?;
    let note_count = freq_hi - freq_lo;
    if !(1..=96).contains(&note_count) {
        return Err("its frequency table is not one".to_string());
    }
    let mut tuned = true;
    for i in 0..note_count {
        let reg = gt_note_freq_reg((first_note + i) as u8) as i64;
        if byte(freq_lo + i)? != (reg & 0xff) || byte(freq_hi + i)? != (reg >> 8) {
            tuned = false;
        }
    }
    if !tuned {
        notes.push("Its note frequencies are not GoatTracker's standard (PAL) table; the song plays in that tuning here.".to_string());
    }

    // Song and pattern tables.
    let song_lo = need("mt_songtbllo")?;
    let song_hi = need("mt_songtblhi")?;
    let patt_lo = need("mt_patttbllo")?;
    let patt_hi = need("mt_patttblhi")?;
    if song_lo != freq_hi + note_count
        || song_hi <= song_lo
        || (song_hi - song_lo) % 3 != 0
        || patt_lo != song_hi + (song_hi - song_lo)
    {
        return Err("its song table is not where the player reads it".to_string());
    }
    let songs = (song_hi - song_lo) / 3;
    let patterns = patt_hi - patt_lo;
    if patterns < 1 || patt_hi + patterns != need("mt_insad")? {
        return Err("its pattern table is not where the player reads it".to_string());
    }
    let order_at: Vec<i64> = (0..songs * 3)
        .map(|i| Ok(byte(song_lo + i)? | (byte(song_hi + i)? << 8)))
        .collect::<Result<_, String>>()?;
    let pattern_at: Vec<i64> = (0..patterns)
        .map(|i| Ok(byte(patt_lo + i)? | (byte(patt_hi + i)? << 8)))
        .collect::<Result<_, String>>()?;

    // Instrument columns, in the packer's order.
    let ins_ad = need("mt_insad")?;
    let count = need("mt_inssr")? - ins_ad;
    if count < 1 || count > MAX_INSTRUMENTS as i64 {
        return Err(format!("its instrument columns hold {count} instruments"));
    }
    let mut at = ins_ad;
    let column = |name: Option<&str>, at: &mut i64| -> Result<Vec<i64>, String> {
        if let Some(name) = name {
            if let Some(&s) = sym.get(name) {
                if s != *at {
                    return Err(format!("its {name} is not where the packer puts it"));
                }
            }
        }
        let out = bytes(*at, count)?;
        *at += count;
        Ok(out)
    };
    let ad = column(Some("mt_insad"), &mut at)?;
    let sr = column(Some("mt_inssr"), &mut at)?;
    let wave_ptr = column(Some("mt_inswaveptr"), &mut at)?;
    let pulse_ptr = if !flag("NOPULSE") {
        Some(column(Some("mt_inspulseptr"), &mut at)?)
    } else {
        None
    };
    let filt_ptr = if !flag("NOFILTER") {
        Some(column(Some("mt_insfiltptr"), &mut at)?)
    } else {
        None
    };
    let vib_param = if !flag("NOINSTRVIB") {
        Some(column(Some("mt_insvibparam"), &mut at)?)
    } else {
        None
    };
    let vib_delay = if !flag("NOINSTRVIB") {
        Some(column(Some("mt_insvibdelay"), &mut at)?)
    } else {
        None
    };
    let fixed = flag("FIXEDPARAMS");
    let gate_col = if fixed {
        None
    } else {
        Some(column(Some("mt_insgatetimer"), &mut at)?)
    };
    let first_col = if fixed {
        None
    } else {
        Some(column(Some("mt_insfirstwave"), &mut at)?)
    };

    // The four tables, each left bytes then right bytes; before the speed table's
    // halves a zero byte when an effect reads row 0 of it.
    let read_table = |left: &str, right: &str, zero: bool, at: &mut i64| -> Result<Table, String> {
        if zero {
            if byte(*at)? != 0 {
                return Err(format!("{left} has no zero byte before it"));
            }
            *at += 1;
        }
        let l = sym.get(left).copied().unwrap_or(*at);
        if l != *at {
            return Err(format!("its {left} is not where the packer puts it"));
        }
        let n: i64 = match sym.get(right) {
            Some(&r) => r - l - if zero { 1 } else { 0 },
            None => {
                // A table the code never reads: the rest up to the orderlists, halved.
                let rest = order_at[0] - *at - if zero { 1 } else { 0 };
                if rest % 2 != 0 {
                    return Err(format!("its {left} does not split into halves"));
                }
                rest / 2
            }
        };
        if !(0..=255).contains(&n) {
            return Err(format!("its {left} has {n} rows"));
        }
        let left_bytes: Vec<i32> = bytes(l, n)?.into_iter().map(|b| b as i32).collect();
        *at = l + n;
        if zero {
            if byte(*at)? != 0 {
                return Err(format!("{right} has no zero byte before it"));
            }
            *at += 1;
        }
        let right_bytes: Vec<i32> = bytes(*at, n)?.into_iter().map(|b| b as i32).collect();
        *at += n;
        Ok(Table {
            left: left_bytes,
            right: right_bytes,
        })
    };
    let speed_zero =
        !flag("NOVIB") || !flag("NOFUNKTEMPO") || !flag("NOPORTAMENTO") || !flag("NOTONEPORTA");
    let wave = read_table("mt_wavetbl", "mt_notetbl", false, &mut at)?;
    let pulse = if !flag("NOPULSE") {
        read_table("mt_pulsetimetbl", "mt_pulsespdtbl", false, &mut at)?
    } else {
        Table {
            left: vec![],
            right: vec![],
        }
    };
    let filter = if !flag("NOFILTER") {
        read_table("mt_filttimetbl", "mt_filtspdtbl", false, &mut at)?
    } else {
        Table {
            left: vec![],
            right: vec![],
        }
    };
    let speed = read_table("mt_speedlefttbl", "mt_speedrighttbl", speed_zero, &mut at)?;
    if at != order_at[0] {
        return Err(format!(
            "its tables end at {}, not where its orderlists start ({})",
            hex4(at),
            hex4(order_at[0])
        ));
    }

    // The tables as the song has them (the packer's conversions, undone).
    let wave_delay = !flag("NOWAVEDELAY");
    let mut wave_rows: Vec<(i32, i32)> = wave
        .left
        .iter()
        .zip(&wave.right)
        .map(|(&l, &r)| {
            let mut left = l;
            if l >= 0xf0 {
                left = l;
            } else if wave_delay {
                if l >= 0x20 {
                    left = l - 0x10;
                } else if l >= 0x10 {
                    left = 0xe0 | (l & 0x0f);
                }
            } else if (0x01..=0x0f).contains(&l) {
                left = 0xe0 | l;
            }
            let right = if left == 0xff || (0xf0..=0xfe).contains(&left) {
                r
            } else {
                r ^ 0x80
            };
            (left, right)
        })
        .collect();
    let simple = flag("SIMPLEPULSE");
    let mut pulse_rows: Vec<(i32, i32)> = pulse
        .left
        .iter()
        .zip(&pulse.right)
        .map(|(&l, &r)| {
            if !simple || l == 0xff {
                return (l, r);
            }
            if l >= 0x80 {
                return (0x80 | (r & 0x0f), r & 0xf0);
            }
            // A speed nybble-swapped, a negative one less.
            let mut s = ((r & 0x0f) << 4) | (r >> 4);
            if s & 0x80 != 0 {
                s = (s + 1) & 0xff;
            }
            (l, (s & 0x0f) << 4)
        })
        .collect();
    let mut filter_rows: Vec<(i32, i32)> = filter
        .left
        .iter()
        .zip(&filter.right)
        .map(|(&l, &r)| {
            (
                if l != 0xff && l > 0x80 {
                    0x80 | ((l & 0x38) << 1)
                } else {
                    l
                },
                r,
            )
        })
        .collect();
    let speed_rows: Vec<(i32, i32)> = speed
        .left
        .iter()
        .cloned()
        .zip(speed.right.iter().cloned())
        .collect();

    // Orderlists: back to the .sng's (a repeat before its pattern).
    let mut orderlists: Vec<Vec<i64>> = Vec::new();
    for (i, &start) in order_at.iter().enumerate() {
        let mut packed: Vec<i64> = Vec::new();
        let mut a = start;
        loop {
            let v = byte(a)?;
            if v == 0xff {
                packed.push(0xff);
                packed.push(byte(a + 1)?);
                break;
            }
            if packed.len() > 254 {
                return Err(format!("orderlist {i} has no end"));
            }
            packed.push(v);
            a += 1;
        }
        let mut out: Vec<i64> = Vec::new();
        let mut k = 0usize;
        while k < packed.len() - 2 {
            let v = packed[k];
            let next = packed[k + 1];
            if v < 0xd0 && next > 0xd0 && next < 0xe0 && k + 1 < packed.len() - 2 {
                out.push(next);
                out.push(v);
                k += 1;
            } else {
                out.push(v);
            }
            k += 1;
        }
        out.push(0xff);
        out.push(packed[packed.len() - 1]);
        orderlists.push(out);
    }

    // Patterns: rows of note, instrument, command, parameter.
    let no_effects = flag("NOEFFECTS");
    let funk_tempo = !flag("NOFUNKTEMPO");
    let mut pattern_rows: Vec<Vec<[i32; 4]>> = Vec::new();
    for (p, &start) in pattern_at.iter().enumerate() {
        let mut rows: Vec<[i32; 4]> = Vec::new();
        let mut command: i32 = if no_effects { 0 } else { -1 };
        let mut param: i32 = if no_effects { 0 } else { -1 };
        let mut instrument: i32 = 0;
        let mut a = start;
        loop {
            if a - start >= 256 {
                break;
            }
            let b = byte(a)?;
            a += 1;
            if b == 0x00 {
                break;
            }
            let mut row_note: Option<i32> = None;
            if b < 0x40 {
                instrument = b as i32;
            } else if b < 0x60 {
                command = (b & 0x0f) as i32;
                param = if command != 0 {
                    let v = byte(a)? as i32;
                    a += 1;
                    v
                } else {
                    0
                };
                if b >= 0x50 {
                    row_note = Some(0xbd);
                }
            } else if b < 0xc0 {
                row_note = Some(b as i32);
            } else {
                for _ in 0..(0x100 - b) {
                    if command < 0 {
                        return Err(format!("pattern {p} plays a note before any command"));
                    }
                    rows.push([0xbd, instrument, command, param]);
                    instrument = 0;
                }
            }
            if let Some(note) = row_note {
                if command < 0 {
                    return Err(format!("pattern {p} plays a note before any command"));
                }
                rows.push([note, instrument, command, param]);
                instrument = 0;
            }
            if rows.len() > 128 {
                return Err(format!("pattern {p} has more than 128 rows"));
            }
        }
        if rows.is_empty() {
            return Err(format!("pattern {p} has no rows"));
        }
        for r in rows.iter_mut() {
            // A tempo's parameter is written one less from 3 up; 0-2 as they are. So 2 is F02 or F03,
            // which play alike (3 ticks a row); F02 makes the packer keep the funktempo code.
            let t = r[3] & 0x7f;
            if r[2] == 0x0f && t >= 2 && !(t == 2 && funk_tempo) {
                r[3] += 1;
            }
        }
        pattern_rows.push(rows);
    }

    // Instruments: the packer's groups (hard restart, none, legato) back into their gate-timer
    // bits. The code compares with the groups' first numbers only when the song mixes them; a
    // song of one group has the hard-restart code or not. At multispeed the packer counts one
    // no-hard-restart instrument more than there is.
    let normal: i64;
    let legato_from: i64;
    if let Some(&no_hr_from) = sym.get("FIRSTNOHRINSTR") {
        normal = no_hr_from - 1;
        legato_from = match sym.get("FIRSTLEGATOINSTR") {
            Some(&l) => l - if mult > 1 { 1 } else { 0 },
            None => count + 1,
        };
    } else {
        let hr = sym
            .get("NUMHRINSTR")
            .copied()
            .or_else(|| m.ranges.get("NUMHRINSTR").map(|r| r.lo))
            .unwrap_or(1);
        normal = if hr > 0 { count } else { 0 };
        legato_from = count + 1;
    }
    let mut instruments: Vec<[i32; 9]> = (0..count as usize)
        .map(|i| {
            let n = i as i64 + 1;
            let gate = (gate_col
                .as_ref()
                .map(|c| c[i])
                .unwrap_or_else(|| sym.get("GATETIMERPARAM").copied().unwrap_or(0)))
                & 0x3f;
            // A legato instrument's hard-restart bit is not packed (no gate-off precedes its notes,
            // so it is never heard): GoatTracker's legato instruments set both ($C0).
            let kind: i64 = if n <= normal {
                0
            } else if n < legato_from {
                0x80
            } else {
                0xc0
            };
            let vp = vib_param.as_ref().map_or(0, |c| c[i]);
            let vd = vib_delay.as_ref().map_or(0, |c| c[i]);
            let vib = vp != 0 || vd != 0;
            [
                ad[i] as i32,
                sr[i] as i32,
                wave_ptr[i] as i32,
                pulse_ptr.as_ref().map_or(0, |c| c[i]) as i32,
                filt_ptr.as_ref().map_or(0, |c| c[i]) as i32,
                if vib { vp as i32 } else { 0 },
                if vib { vd as i32 + 1 } else { 0 },
                (kind | gate) as i32,
                first_col
                    .as_ref()
                    .map(|c| c[i])
                    .unwrap_or_else(|| sym.get("FIRSTWAVEPARAM").copied().unwrap_or(0))
                    as i32,
            ]
        })
        .collect();
    // Speed-table rows nothing points to: an instrument's vibrato row whose delay was 0 (it
    // never vibrates, so the packer writes neither; the row stays in the table). Each goes
    // back to an instrument without vibrato, where it is not heard either.
    let mut speed_used: std::collections::HashSet<i32> = std::collections::HashSet::new();
    for ins in &instruments {
        if ins[6] > 0 {
            speed_used.insert(ins[5]);
        }
    }
    for rows in &pattern_rows {
        for r in rows {
            if (1..=4).contains(&r[2]) || r[2] == 0xe {
                speed_used.insert(r[3]);
            }
        }
    }
    for r in &wave_rows {
        if (0xf1..=0xf4).contains(&r.0) {
            speed_used.insert(r.1);
        }
    }
    let mut orphans: Vec<i32> = (1..=speed_rows.len() as i32)
        .filter(|row| !speed_used.contains(row))
        .collect();
    for ins in instruments.iter_mut() {
        if orphans.is_empty() {
            break;
        }
        if ins[5] == 0 && ins[6] == 0 {
            ins[5] = orphans.remove(0);
        }
    }

    // GoatTracker's packer leaves out a table part equal to an earlier one when its scan, which
    // steps from part to part through the table as the song had it (unused rows too), lands on
    // it. The packed table is those parts closed up, where the scan would land on parts the
    // song's own layout kept apart: a blank row (never reached) before such a part keeps it.
    fn separate(
        num: usize,
        rows: &mut Vec<(i32, i32)>,
        column: usize,
        command: i32,
        wave_command: Option<i32>,
        instruments: &mut [[i32; 9]],
        pattern_rows: &mut [Vec<[i32; 4]>],
        wave_rows: &mut [(i32, i32)],
        rows_is_wave: bool,
    ) {
        let mut used: Vec<bool> = std::iter::once(false)
            .chain(rows.iter().map(|_| true))
            .collect();
        for _ in 0..255 {
            let l: Vec<i32> = rows.iter().map(|r| r.0).collect();
            let r: Vec<i32> = rows.iter().map(|r| r.1).collect();
            let dup = gt_table_duplicate_rows(num, &l, &r, &used);
            if dup.is_empty() || rows.len() >= 255 {
                return;
            }
            let at = dup[0] as i32;
            let shift = |v: i32| -> i32 {
                if v >= at {
                    v + 1
                } else {
                    v
                }
            };
            rows.insert(at as usize - 1, (0, 0));
            used.insert(at as usize, false);
            for r in rows.iter_mut() {
                if r.0 == 0xff {
                    r.1 = shift(r.1);
                }
            }
            for ins in instruments.iter_mut() {
                ins[column] = shift(ins[column]);
            }
            for pattern in pattern_rows.iter_mut() {
                for r in pattern.iter_mut() {
                    if r[2] == command {
                        r[3] = shift(r[3]);
                    }
                }
            }
            if let Some(wc) = wave_command {
                // The wave rows are a different table than `rows` here (num 1, 2).
                debug_assert!(!rows_is_wave);
                for r in wave_rows.iter_mut() {
                    if r.0 == wc {
                        r.1 = shift(r.1);
                    }
                }
            }
        }
    }
    let mut none: Vec<(i32, i32)> = Vec::new();
    separate(
        0,
        &mut wave_rows,
        2,
        0x8,
        None,
        &mut instruments,
        &mut pattern_rows,
        &mut none,
        true,
    );
    separate(
        1,
        &mut pulse_rows,
        3,
        0x9,
        Some(0xf9),
        &mut instruments,
        &mut pattern_rows,
        &mut wave_rows,
        false,
    );
    separate(
        2,
        &mut filter_rows,
        4,
        0xa,
        Some(0xfa),
        &mut instruments,
        &mut pattern_rows,
        &mut wave_rows,
        false,
    );

    // GoatTracker's start tempo other than 6 a tick: the hidden last instrument's attack/decay says it.
    if let Some(&tempo) = sym.get("DEFAULTTEMPO") {
        if tempo != mult as i64 * 6 - 1 {
            while instruments.len() < MAX_INSTRUMENTS {
                instruments.push([0; 9]);
            }
            instruments[MAX_INSTRUMENTS - 1][0] = tempo as i32 + 1;
        }
    }
    if sym.get("ADPARAM").copied().unwrap_or(0x0f) != 0x0f
        || sym.get("SRPARAM").copied().unwrap_or(0x00) != 0x00
    {
        notes.push("Its hard restart uses another envelope than GoatTracker's default ($0F00); here it gets the default.".to_string());
    }

    let sng = gts_bytes(
        &file.name,
        &file.author,
        &file.released,
        songs as u8,
        &orderlists,
        &instruments,
        [&wave_rows, &pulse_rows, &filter_rows, &speed_rows],
        &pattern_rows,
    );
    let hints = GtSongHints {
        model: Some(if file.sid_model == PsidSidModel::Sid8580 {
            SidModel::Sid8580
        } else {
            SidModel::Sid6581
        }),
        speed_multiplier: Some(mult),
    };
    let imported = goattracker::import(&sng, hints)
        .map_err(|e| format!("its data does not make a GoatTracker song ({e})"))?;
    Ok(GtUnpack {
        song: imported.song,
        notes,
    })
}

/// A GoatTracker 2 song file (GTS5, readme §6.1) of the parts.
#[allow(clippy::too_many_arguments)]
fn gts_bytes(
    name: &str,
    author: &str,
    copyright: &str,
    songs: u8,
    orderlists: &[Vec<i64>],
    instruments: &[[i32; 9]],
    tables: [&Vec<(i32, i32)>; 4],
    patterns: &[Vec<[i32; 4]>],
) -> Vec<u8> {
    let mut out: Vec<u8> = b"GTS5".to_vec();
    let text = |out: &mut Vec<u8>, s: &str, n: usize| {
        let chars: Vec<u32> = s.chars().map(|c| c as u32).collect();
        for i in 0..n {
            out.push(if i < chars.len() {
                (chars[i] & 0xff) as u8
            } else {
                0
            });
        }
    };
    text(&mut out, name, 32);
    text(&mut out, author, 32);
    text(&mut out, copyright, 32);
    out.push(songs);
    for list in orderlists {
        out.push((list.len() - 1) as u8);
        out.extend(list.iter().map(|&v| v as u8));
    }
    out.push(instruments.len() as u8);
    for (i, ins) in instruments.iter().enumerate() {
        out.extend(ins.iter().map(|&v| v as u8));
        let named = ins[2] != 0 || ins[0] != 0 || ins[1] != 0;
        text(
            &mut out,
            &if named {
                format!("Instrument {}", i + 1)
            } else {
                String::new()
            },
            16,
        );
    }
    for t in tables {
        out.push(t.len() as u8);
        out.extend(t.iter().map(|r| r.0 as u8));
        out.extend(t.iter().map(|r| r.1 as u8));
    }
    out.push(patterns.len() as u8);
    for rows in patterns {
        out.push((rows.len() + 1) as u8);
        for r in rows {
            out.extend(r.iter().map(|&v| v as u8));
        }
        out.extend([0xff, 0, 0, 0]);
    }
    out
}
