//! The flat SID song the transcriber builds and compiles: a sequence of
//! song-wide patterns, one cell per voice, from which GoatTracker's per-voice
//! structure (orderlists over a shared pool of patterns) is compiled. The
//! compile half of the DAW's `sid-doc/flat.ts` (`compileSidFlatSong`) and GoatTracker's
//! packer's table-duplicate search (`sid-export/gt-pack.ts`).

use crate::goattracker::{
    orderlist_byte_length, GT_MAX_ORDERLIST_BYTES, GT_MAX_TRANSPOSE, GT_MIN_TRANSPOSE,
};
use crate::song::{
    OrderEntry, Orderlist, Pattern, Row, SidSong, Subsong, MAX_PATTERNS, MAX_PATTERN_ROWS,
    MAX_REPEAT, MAX_SUBSONGS,
};
use std::collections::HashMap;

/// One voice of a flat pattern: its rows (as many as the pattern's), the
/// transpose they play under, and whether the voice starts a GoatTracker
/// pattern here.
#[derive(Clone, Debug)]
pub struct FlatCell {
    pub transpose: i32,
    pub rows: Vec<Row>,
    pub start: bool,
}

/// A song-wide pattern: `rows` rows, one cell per voice.
#[derive(Clone, Debug)]
pub struct FlatPattern {
    pub rows: usize,
    pub cells: Vec<FlatCell>,
}

/// One subsong, flat: the patterns, the sequence of their indices, the
/// per-voice loop points (the sequence index each voice loops back to).
#[derive(Clone, Debug)]
pub struct FlatSubsong {
    pub patterns: Vec<FlatPattern>,
    pub sequence: Vec<usize>,
    pub restarts: Vec<usize>,
}

/// Why subsong `s` of a flat song cannot be compiled, before anything is built, or `None`.
fn flat_subsong_problem(flat: &FlatSubsong, s: usize, channels: usize) -> Option<String> {
    let at = if s == 0 {
        String::new()
    } else {
        format!(" (subsong {s})")
    };
    if flat.sequence.is_empty() {
        return Some(format!(
            "The song needs at least one pattern in its sequence{at}."
        ));
    }
    for &id in &flat.sequence {
        let Some(pattern) = flat.patterns.get(id) else {
            return Some(format!(
                "The sequence names a pattern the song does not have{at}."
            ));
        };
        if pattern.rows < 1 || pattern.rows > MAX_PATTERN_ROWS {
            return Some(format!(
                "A SID pattern has 1-{MAX_PATTERN_ROWS} rows (one has {}){at}.",
                pattern.rows
            ));
        }
        if pattern.cells.len() != channels {
            return Some(format!("A SID pattern has {channels} voices{at}."));
        }
        for cell in &pattern.cells {
            if cell.rows.len() != pattern.rows {
                return Some(format!(
                    "A voice of a pattern has not the pattern's {} rows{at}.",
                    pattern.rows
                ));
            }
            if cell.transpose < GT_MIN_TRANSPOSE as i32 || cell.transpose > GT_MAX_TRANSPOSE as i32
            {
                return Some(format!(
                    "A GoatTracker orderlist transposes {GT_MIN_TRANSPOSE}..+{GT_MAX_TRANSPOSE} semitones, not {}{at}.",
                    cell.transpose
                ));
            }
        }
    }
    None
}

/// One voice's run of positions compiled as one GT pattern: its rows (concatenated), transpose, first position.
struct Piece {
    rows: Vec<Row>,
    transpose: i32,
    first: usize,
}

fn row_key(rows: &[Row]) -> Vec<[u8; 4]> {
    rows.iter()
        .map(|r| [r.note, r.instrument, r.command, r.param])
        .collect()
}

/// The GoatTracker song of a flat song: `base`'s everything (texts, chip,
/// speed, instruments, tables) with its patterns and orderlists built from
/// `subsongs`.
///
/// Each voice is cut into GT patterns where its cells say it starts one
/// (`FlatCell::start`), and also at the first position, at its restart (an
/// orderlist loops to an entry), where the transpose changes, and where a
/// pattern would pass 128 rows. Identical patterns are one (shared across
/// voices and subsongs); consecutive identical entries are one repeated entry
/// (never across the restart).
///
/// Refused, with the true reason, when the song does not fit GoatTracker:
/// more than 208 distinct voice patterns, or an orderlist over 254 bytes.
pub fn compile_flat_song(base: &SidSong, subsongs: &[FlatSubsong]) -> Result<SidSong, String> {
    if subsongs.is_empty() || subsongs.len() > MAX_SUBSONGS {
        return Err(format!("A song has 1-{MAX_SUBSONGS} subsongs."));
    }
    let channels = base.channels as usize;
    for (s, flat) in subsongs.iter().enumerate() {
        if let Some(problem) = flat_subsong_problem(flat, s, channels) {
            return Err(problem);
        }
    }
    fn cell_of(flat: &FlatSubsong, i: usize, c: usize) -> &FlatCell {
        &flat.patterns[flat.sequence[i]].cells[c]
    }
    // Per subsong, per voice: its GT patterns in order.
    let pieces: Vec<Vec<Vec<Piece>>> = subsongs
        .iter()
        .map(|flat| {
            let n = flat.sequence.len();
            (0..channels)
                .map(|c| {
                    let restart = flat.restarts.get(c).copied().unwrap_or(0).min(n - 1);
                    let mut out: Vec<Piece> = Vec::new();
                    for i in 0..n {
                        let cell = cell_of(flat, i, c);
                        if let Some(open) = out.last_mut() {
                            if !cell.start
                                && i != restart
                                && open.transpose == cell.transpose
                                && open.rows.len() + cell.rows.len() <= MAX_PATTERN_ROWS
                            {
                                open.rows.extend_from_slice(&cell.rows);
                                continue;
                            }
                        }
                        out.push(Piece {
                            rows: cell.rows.clone(),
                            transpose: cell.transpose,
                            first: i,
                        });
                    }
                    out
                })
                .collect()
        })
        .collect();

    let mut pool: HashMap<Vec<[u8; 4]>, usize> = HashMap::new();
    let mut patterns: Vec<Pattern> = Vec::new();
    let mut built: Vec<Subsong> = Vec::new();
    for (s, flat) in subsongs.iter().enumerate() {
        let mut orderlists: Vec<Orderlist> = Vec::new();
        for c in 0..channels {
            let restart = flat
                .restarts
                .get(c)
                .copied()
                .unwrap_or(0)
                .min(flat.sequence.len() - 1);
            let mut entries: Vec<OrderEntry> = Vec::new();
            let mut restart_entry = 0usize;
            for piece in &pieces[s][c] {
                let key = row_key(&piece.rows);
                let index = match pool.get(&key) {
                    Some(&i) => i,
                    None => {
                        let i = patterns.len();
                        pool.insert(key, i);
                        patterns.push(Pattern {
                            rows: piece.rows.clone(),
                        });
                        i
                    }
                };
                if let Some(last) = entries.last_mut() {
                    if piece.first != restart
                        && last.pattern as usize == index
                        && last.transpose as i32 == piece.transpose
                        && last.repeat < MAX_REPEAT
                    {
                        last.repeat += 1;
                        continue;
                    }
                }
                if piece.first == restart {
                    restart_entry = entries.len();
                }
                entries.push(OrderEntry {
                    pattern: index as u8,
                    transpose: piece.transpose as i8,
                    repeat: 1,
                });
            }
            let list = Orderlist {
                entries,
                restart: restart_entry as u8,
            };
            let bytes = orderlist_byte_length(&list);
            if bytes > GT_MAX_ORDERLIST_BYTES {
                let at = if subsongs.len() > 1 {
                    format!(" in subsong {s}")
                } else {
                    String::new()
                };
                return Err(format!(
                    "Voice {}'s GoatTracker orderlist{at} would take {bytes} bytes; GoatTracker's holds {GT_MAX_ORDERLIST_BYTES}. Use fewer or longer patterns.",
                    c + 1
                ));
            }
            orderlists.push(list);
        }
        built.push(Subsong { orderlists });
    }
    if patterns.len() > MAX_PATTERNS {
        return Err(format!(
            "The song needs {} different voice patterns; GoatTracker holds {MAX_PATTERNS}. Reuse patterns, or make them longer.",
            patterns.len()
        ));
    }
    let mut song = base.clone();
    song.patterns = patterns;
    song.subsongs = built;
    match song.validate() {
        Ok(()) => Ok(song),
        Err(e) => Err(format!("That edit would break the song: {e}.")),
    }
}

// ---------------------------------------------------------------------------
// GoatTracker's packer: what decides how long a pattern may be
// ---------------------------------------------------------------------------

const MAX_TABLELEN: usize = 255;

/// GT's `gettablepartlen`: rows from `pos` (0-based) up to and including the next jump.
fn table_part_length(ltable: &[i32], num: usize, pos: i64) -> i64 {
    if pos < 0 {
        return 0;
    }
    if num == 3 {
        return 1;
    }
    let mut c = pos;
    while c < MAX_TABLELEN as i64 {
        if ltable[c as usize] == 0xff {
            c += 1;
            break;
        }
        c += 1;
    }
    c - pos
}

/// GT's `findtableduplicates` for table `num` (0 wave, 1 pulse, 2 filter, 3 speed): a speed-table
/// row equal to an earlier one, and a self-contained wave/pulse/filter part (used throughout, jumps
/// only inside, nothing jumps into it) equal to an earlier one, are left out and mapped onto the
/// first. `used` is 1-based (index 0 unused), `lt`/`rt` the table's columns (0-based).
pub(crate) fn find_table_duplicates(
    num: usize,
    lt: &[i32],
    rt: &[i32],
    used: &mut [bool],
    map: &mut [i64],
) {
    let max = MAX_TABLELEN;
    if num == 3 {
        for c in 1..=max {
            if !used[c] {
                continue;
            }
            for d in c + 1..=max {
                if used[d] && lt[d - 1] == lt[c - 1] && rt[d - 1] == rt[c - 1] {
                    used[d] = false;
                    for e in d..=max {
                        if used[e] {
                            map[e] -= 1;
                        }
                    }
                    map[d] = map[c];
                }
            }
        }
        return;
    }
    let self_contained = |used: &[bool], start: usize| -> bool {
        let len = table_part_length(lt, num, start as i64 - 1);
        let end = start as i64 + len - 1;
        if len == 1 {
            return false;
        }
        for c in start as i64..=end {
            if !used[c as usize] {
                return false;
            }
        }
        let tail = rt[(end - 1) as usize];
        if tail != 0 && (tail < start as i32 || tail as i64 > end) {
            return false;
        }
        for c in 1..=max {
            if c as i64 >= start as i64 && c as i64 <= end {
                continue;
            }
            if used[c] && lt[c - 1] == 0xff && rt[c - 1] >= start as i32 && rt[c - 1] as i64 <= end
            {
                return false;
            }
        }
        true
    };
    for c in 1..=max {
        if !self_contained(used, c) {
            continue;
        }
        let mut d = c as i64 + table_part_length(lt, num, c as i64 - 1);
        while d <= max as i64 {
            let len = table_part_length(lt, num, d - 1);
            if self_contained(used, d as usize) {
                let mut e = 0i64;
                while e < len {
                    let di = (d + e - 1) as usize;
                    let ci = (c as i64 + e - 1) as usize;
                    if e < len - 1 {
                        if lt[di] != lt[ci] || rt[di] != rt[ci] {
                            break;
                        }
                    } else {
                        if lt[di] != lt[ci] {
                            break;
                        }
                        if rt[di] == 0 {
                            if rt[ci] != 0 {
                                break;
                            }
                        } else if rt[di] as i64 - d != rt[ci] as i64 - c as i64 {
                            break;
                        }
                    }
                    e += 1;
                }
                if e == len {
                    for e in 0..len {
                        used[(d + e) as usize] = false;
                    }
                    let mut e = d;
                    while e < max as i64 {
                        if used[e as usize] {
                            map[e as usize] -= len;
                        }
                        e += 1;
                    }
                    for e in 0..len {
                        map[(d + e) as usize] = map[(c as i64 + e) as usize];
                    }
                }
            }
            d += len;
        }
    }
}

/// The rows GoatTracker's packer would leave out of table `num` (0 wave, 1
/// pulse, 2 filter, 3 speed) as duplicates of earlier ones, given the rows
/// something reaches (`used`, 1-based). Which parts it compares depends on
/// where the table's parts sit, unused rows included; the `.sid` unpacker lays
/// a table out so that none are, as in the song that was packed.
pub fn gt_table_duplicate_rows(
    num: usize,
    left: &[i32],
    right: &[i32],
    used: &[bool],
) -> Vec<usize> {
    let pad = |t: &[i32]| -> Vec<i32> {
        (0..MAX_TABLELEN)
            .map(|i| t.get(i).copied().unwrap_or(0))
            .collect()
    };
    let lt = pad(left);
    let rt = pad(right);
    let mut tableused: Vec<bool> = (0..=MAX_TABLELEN)
        .map(|i| used.get(i).copied().unwrap_or(false))
        .collect();
    let mut map: Vec<i64> = (0..=MAX_TABLELEN as i64).collect();
    find_table_duplicates(num, &lt, &rt, &mut tableused, &mut map);
    let mut out = Vec::new();
    for r in 1..=MAX_TABLELEN {
        if used.get(r).copied().unwrap_or(false) && !tableused[r] {
            out.push(r);
        }
    }
    out
}
