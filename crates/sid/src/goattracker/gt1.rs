//! GoatTracker 1 (`GTS!`) -> [`SidSong`], a port of the DAW's
//! `gt-sng-gt1.ts`. The GT2 readme documents only GTS5; its §1.2 says GT2
//! loads v1.xx songs and converts them ("some subtleties ... will not play
//! back exactly like in v1.xx", the arpeggio command becomes wavetable
//! programs) and §3.4.4 gives the old-parameter conversions for vibrato ($34
//! -> speed 03, depth 40) and portamento (4 x the old value). The conversion
//! follows GT2's own GT1 loader, gsong.c:329-845 (read for its facts only;
//! the file:line cites below point there): its tables, commands and arpeggio
//! instruments, byte for byte, except where GT reads memory it never wrote
//! (each such place says what stands in instead). Our tables share identical
//! programs where GT lays copies.
//!
//! Layout (measured, every corpus GTS! file consumes exactly):
//!   +0 'GTS!', +4/+36/+68 name/author/copyright (32 each), +100 subtunes,
//!   +101 orderlists (as GTS5); then exactly 31 instruments, each 8 bytes +
//!   16-byte name + (byte 7 / 2) wavetable (left, right) pairs inline; then a
//!   pattern count and patterns of (length in BYTES, 3-byte rows: note,
//!   instrument<<3 | command, parameter), the last row $FF; then an optional
//!   256-byte filter table (64 rows x 4 bytes; absent in 5 of 22 files).

use std::collections::HashMap;

use super::{
    check_tables, decode_subsongs, fail, gt_text, hex, latin1, read_orderlists, read_subtune_count,
    GtNote, GtNoteKind, GtReader, INSTRUMENT_NAME_LENGTH, MAX_TABLE_ROWS, SUBTUNE_COUNT_OFFSET,
};
use crate::song::{
    Instrument, Pattern, Row, SidSong, SongError, TableRow, Tables, MAX_INSTRUMENTS,
    MAX_INSTRUMENT_NAME, MAX_PATTERNS, MAX_PATTERN_ROWS, NOTE_KEY_OFF, NOTE_NONE, SID_CHANNELS,
};

const INSTRUMENTS: usize = 31;
const INSTRUMENT_HEADER: usize = 8;
const FILTER_TABLE_BYTES: usize = 256;
const FILTER_ROWS: usize = 64;
/// GT1 note bytes: $00-$5C notes C-0..G#7 (GT2's $60-$BC less $60,
/// gsong.c:559), $5E key off, $5F rest, $FF end (gcommon.h:44-54).
const NOTE_LAST: u8 = 0x5C;
const NOTE_GT1_KEY_OFF: u8 = 0x5E;
const NOTE_REST: u8 = 0x5F;
const PATTERN_END: u8 = 0xFF;
/// A table jump row (all four tables use left $FF, right = 1-based row, 0 = stop).
const JUMP: u8 = 0xFF;

fn row(left: u8, right: u8) -> TableRow {
    TableRow { left, right }
}

/// A step table under construction: programs appended once each, identical ones shared.
struct TableBuilder {
    name: &'static str,
    rows: Vec<TableRow>,
    seen: HashMap<Vec<(u8, u8)>, usize>,
}

impl TableBuilder {
    fn new(name: &'static str) -> Self {
        TableBuilder {
            name,
            rows: Vec::new(),
            seen: HashMap::new(),
        }
    }

    /// Appends `program` (its jump targets 1-based within itself, 0 = stop)
    /// and returns its first row, 1-based. An identical program already added
    /// is reused.
    fn add(&mut self, program: &[TableRow]) -> Result<usize, SongError> {
        let key: Vec<(u8, u8)> = program.iter().map(|r| (r.left, r.right)).collect();
        if let Some(&found) = self.seen.get(&key) {
            return Ok(found);
        }
        let start = self.rows.len() + 1;
        for &r in program {
            self.rows.push(if r.left == JUMP && r.right != 0 {
                row(JUMP, (start + r.right as usize - 1) as u8)
            } else {
                r
            });
        }
        if self.rows.len() > MAX_TABLE_ROWS {
            return Err(fail(format!(
                "converted to GoatTracker 2 tables, its {} table needs {} rows; a table holds {MAX_TABLE_ROWS}",
                self.name,
                self.rows.len()
            )));
        }
        self.seen.insert(key, start);
        Ok(start)
    }
}

/// Splits a modulation of `time` frames at `speed` into rows of at most $7F
/// frames (GT2's time range).
fn timed_rows(time: i32, speed: i32) -> Vec<TableRow> {
    let mut out = Vec::new();
    let mut left = time;
    while left > 0 {
        out.push(row(left.min(0x7F) as u8, (speed & 0xFF) as u8));
        left -= 0x7F;
    }
    out
}

struct Gt1Instrument<'a> {
    header: &'a [u8],
    name: Vec<u8>,
    wave: Vec<TableRow>,
    empty: bool,
    /// GT2 takes the program back: the instrument has no wave pointer there (gsong.c:407-416).
    no_wave: bool,
}

/// An arpeggio's conversion: a clone instrument playing its program, or the
/// program alone (for command 8).
#[derive(Clone, Copy)]
struct Arpeggio {
    clone: bool,
    value: usize,
}

/// Collects notes as the conversion goes.
struct Notes<'a>(&'a mut Vec<GtNote>);

impl Notes<'_> {
    fn convert(&mut self, message: String) {
        self.0.push(GtNote {
            kind: GtNoteKind::Gt1Convert,
            message,
        });
    }
    fn drop(&mut self, message: String) {
        self.0.push(GtNote {
            kind: GtNoteKind::Gt1Dropped,
            message,
        });
    }
}

pub(super) fn read(
    bytes: &[u8],
    notes: &mut Vec<GtNote>,
    mut song: SidSong,
) -> Result<SidSong, SongError> {
    let mut r = GtReader::new(bytes, SUBTUNE_COUNT_OFFSET);
    let subtunes = read_subtune_count(&mut r)?;
    let raw_lists = read_orderlists(&mut r, subtunes, SID_CHANNELS)?;

    let mut raw: Vec<Gt1Instrument> = Vec::with_capacity(INSTRUMENTS);
    // How many wave rows GT2's own loader has laid (gsong.c:385-416): it lays
    // every instrument's rows, no two shared, while the table has room, and
    // takes back a program of one blank row and its end. The arpeggio
    // programs it adds later fit or not by this count (gsong.c:738, 747),
    // whatever our de-duplicated table holds.
    let mut gt_wave_rows: usize = 0;
    for i in 0..INSTRUMENTS {
        let what = format!("instrument {}", i + 1);
        let header = r.take(INSTRUMENT_HEADER, &what)?;
        let name = gt_text(r.take(INSTRUMENT_NAME_LENGTH, &what)?);
        let pairs = (header[7] >> 1) as usize;
        let w = r.take(pairs * 2, &format!("instrument {}'s wavetable", i + 1))?;
        let wave: Vec<TableRow> = w
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[l, r]| row(l, r))
            .collect();
        let empty = name.is_empty()
            && header[..7].iter().all(|&b| b == 0)
            && wave
                .iter()
                .all(|r| r.left == 0 || (r.left == JUMP && r.right == 0));
        gt_wave_rows = MAX_TABLE_ROWS.min(gt_wave_rows + pairs);
        let no_wave = pairs == 2 && wave[0].left == 0 && wave[0].right == 0;
        if no_wave {
            gt_wave_rows -= 2;
        }
        raw.push(Gt1Instrument {
            header,
            name,
            wave,
            empty,
            no_wave,
        });
    }

    let pattern_count = r.byte("the pattern count")? as usize;
    if !(1..=MAX_PATTERNS).contains(&pattern_count) {
        return Err(fail(format!(
            "it declares {pattern_count} patterns; a song has 1-{MAX_PATTERNS}"
        )));
    }
    let mut raw_patterns = Vec::with_capacity(pattern_count);
    for p in 0..pattern_count {
        let what = format!("pattern {p}");
        let len = r.byte(&what)? as usize;
        if !len.is_multiple_of(3) {
            return Err(fail(format!(
                "pattern {p} is {len} bytes long, not a whole number of 3-byte rows"
            )));
        }
        raw_patterns.push(r.take(len, &what)?);
    }
    // The trailing filter table is optional on disk (measured: 5 of 22 corpus files end here).
    let filter_table = match r.left() {
        0 => None,
        FILTER_TABLE_BYTES => Some(r.take(FILTER_TABLE_BYTES, "the filter table")?),
        left => {
            return Err(fail(format!(
                "{left} bytes follow the last pattern; a GoatTracker 1 song ends there or after a {FILTER_TABLE_BYTES}-byte filter table"
            )))
        }
    };

    let mut wave = TableBuilder::new("wave");
    let mut pulse = TableBuilder::new("pulse");
    let mut speed = TableBuilder::new("speed");
    let mut notes = Notes(notes);
    // Every 3-byte row of a raw pattern, its end row included.
    let rows_of = |data: &'_ [u8]| -> Vec<[u8; 3]> { data.as_chunks::<3>().0.to_vec() };

    // --- The filter table: 64 rows of (b0, b1, b2, next row), converted as
    // GT2's loader does (gsong.c:602-669). Rows 1..n are laid out in order,
    // where n is the highest row anything names: an instrument filter byte
    // (gsong.c:380), a command-5 parameter (gsong.c:578, every row read, the
    // end row too) or any row's `next` byte, row 0's included (gsong.c:609);
    // at most 63 (gsong.c:612). Row 0 is never laid: its bytes 2-3 are the
    // funktempo (command 7 00).
    let mut filter_row_count = 0usize;
    for ins in &raw {
        filter_row_count = filter_row_count.max(ins.header[6] as usize);
    }
    for data in &raw_patterns {
        for [_, packed, param] in rows_of(data) {
            if packed & 0x07 == 5 {
                filter_row_count = filter_row_count.max(param as usize);
            }
        }
    }
    let mut filter_rows: Vec<TableRow> = Vec::new();
    // GT1 row -> 1-based GT2 row (gsong.c:616); 0 stays 0.
    let mut filter_map = [0usize; FILTER_ROWS];
    if let Some(ft) = filter_table {
        for c in 0..FILTER_ROWS {
            filter_row_count = filter_row_count.max(ft[c * 4 + 3] as usize);
        }
        filter_row_count = filter_row_count.min(FILTER_ROWS - 1);
        let mut jumps: Vec<(usize, usize)> = Vec::new();
        for c in 1..=filter_row_count {
            // Every row maps to where the output stands, an all-zero one
            // (which lays nothing) to whatever is laid next.
            filter_map[c] = filter_rows.len() + 1;
            let [b0, b1, b2, next] = [ft[c * 4], ft[c * 4 + 1], ft[c * 4 + 2], ft[c * 4 + 3]];
            if b0 | b1 | b2 | next == 0 {
                continue;
            }
            if b0 != 0 {
                // Set (gsong.c:621-631): b0 is SID $D417 as is (resonance<<4 |
                // channels), b1 is $D418 (bits 4-6 the passband, bit 7 voice 3
                // off, bits 0-3 the master volume). GT2 keeps the passband,
                // then sets the cutoff b2 when it is not 0.
                filter_rows.push(row(0x80 | (b1 & 0x70), b0));
                if b2 != 0 {
                    filter_rows.push(row(0x00, b2));
                }
                let mut lost = Vec::new();
                if b1 & 0x0F != 0x0F {
                    lost.push(format!("master volume {}", b1 & 0x0F));
                }
                if b1 & 0x80 != 0 {
                    lost.push("voice 3 off".to_string());
                }
                if !lost.is_empty() {
                    notes.drop(format!(
                        "filter table row {c}: {} has no filter-table equivalent",
                        lost.join(" and ")
                    ));
                }
            } else {
                // Modulation (gsong.c:633-647): b1 frames at signed b2, in rows
                // of at most 127 frames. 0 frames lays no row.
                filter_rows.extend(timed_rows(b1 as i32, b2 as i32));
            }
            // Falls through when `next` is the row after it (gsong.c:650).
            if next as usize != c + 1 {
                let mut to = next as usize;
                if to >= FILTER_ROWS {
                    // GT indexes its 64-entry map with it (gsong.c:664): out of bounds.
                    notes.drop(format!(
                        "filter table row {c}: next row ${} is past the table; the jump stops instead",
                        hex(next)
                    ));
                    to = 0;
                }
                jumps.push((filter_rows.len(), to));
                filter_rows.push(row(JUMP, 0));
            }
        }
        for (at, to) in jumps {
            filter_rows[at] = row(JUMP, filter_map[to] as u8);
        }
        if filter_rows.len() > MAX_TABLE_ROWS {
            return Err(fail(format!(
                "converted to GoatTracker 2 tables, its filter table needs {} rows; a table holds {MAX_TABLE_ROWS}",
                filter_rows.len()
            )));
        }
    }
    // A GT1 filter row an instrument or command 5 names -> its GT2 row
    // (gsong.c:667-669, 690-692). Without a filter table GT maps through a
    // table it built from 256 uninitialised stack bytes (gsong.c:341, 602),
    // which nothing can reproduce: a non-zero pointer is dropped then.
    let map_filter = |ptr: u8, place: &str, notes: &mut Notes| -> Option<usize> {
        if ptr == 0 {
            return Some(0);
        }
        if filter_table.is_none() {
            notes.drop(format!(
                "{place}: filter pointer ${} with no filter table in the file; dropped",
                hex(ptr)
            ));
            return None;
        }
        if ptr as usize >= FILTER_ROWS {
            notes.drop(format!(
                "{place}: filter pointer ${} is past the filter table; dropped",
                hex(ptr)
            ));
            return Some(0);
        }
        Some(filter_map[ptr as usize])
    };

    // --- Instruments.
    let mut used_instruments = 0usize;
    for data in &raw_patterns {
        for [note, packed, _] in rows_of(data) {
            if note != PATTERN_END {
                used_instruments = used_instruments.max((packed >> 3) as usize);
            }
        }
    }
    let mut count = INSTRUMENTS;
    while count > used_instruments && raw[count - 1].empty {
        count -= 1;
    }

    // Each instrument's wave rows up to its first jump, lefts only: an
    // arpeggio program's head (gsong.c:731-744).
    let mut arpeggio_heads: Vec<Vec<TableRow>> = Vec::with_capacity(count);
    let mut instruments: Vec<Instrument> = Vec::with_capacity(count);
    for (i, ins) in raw[..count].iter().enumerate() {
        let h = ins.header;
        let place = format!("instrument {}", i + 1);
        // Wave: GT2's wavetable encoding (readme §3.4.1) fits every corpus
        // byte (INFERRED); jumps are 1-based within the instrument's own program.
        let mut program: Vec<TableRow> = ins
            .wave
            .iter()
            .map(|&r| {
                if r.left == JUMP && r.right as usize > ins.wave.len() {
                    notes.drop(format!(
                        "{place}: its wavetable jumps to row {} of {}; the jump stops instead",
                        r.right,
                        ins.wave.len()
                    ));
                    row(JUMP, 0)
                } else if (0x08..=0x0F).contains(&r.left) {
                    // Lefts $08-$0F are pre-2.18 delays, GT2's $E8-$EF (gsong.c:396-397).
                    row(r.left | 0xE0, r.right)
                } else {
                    r
                }
            })
            .collect();
        if program.last().is_none_or(|r| r.left != JUMP) {
            notes.convert(format!(
                "{place}: its wavetable has no end jump; one is added"
            ));
            program.push(row(JUMP, 0));
        }
        // GT copies the left bytes and leaves the right ones blank (gsong.c:740).
        let head = program.iter().position(|r| r.left == JUMP).unwrap_or(0);
        arpeggio_heads.push(if ins.no_wave {
            Vec::new()
        } else {
            program[..head].iter().map(|r| row(r.left, 0)).collect()
        });
        // Pulse (gsong.c:375-382, 418-538): +2 start, +3 speed, +4/+5 low/high
        // limit, the width's top 8 bits. Bit 0 of the start is GT1's "no hard
        // restart" flag, not width (gsong.c:381-382); a start of 0 makes no
        // program. GT2 turns the limit-based sweep into timed steps: a rise to
        // the high limit, a fall to the low one, then a rise back either to
        // the start (when it lies between the two turns; the loop then
        // restarts at the first rise) or to the high limit (the loop restarts
        // at the fall). Each leg lasts floor(distance / speed) frames at
        // speed/2, which the pre-2.4 pass doubles and clamps to +127/-128
        // (gsong.c:816-830; a row of speed 0 is not doubled). That math applies
        // to every GTS! file: gsong.c:330 is the one GTS! branch, with no
        // sub-version switch (betaconv.c reads GTS2 only, betaconv.c:83).
        let no_hard_restart = h[2] & 0x01 != 0;
        let start = (h[2] & 0xFE) as i32;
        let (add, low, high) = (h[3] as i32, (h[4] as i32) << 4, (h[5] as i32) << 4);
        let mut pulse_ptr = 0;
        if start != 0 {
            let mut steps = vec![row(0x80 | (start >> 4) as u8, ((start << 4) & 0xFF) as u8)];
            if add == 0 {
                steps.push(row(JUMP, 0));
            } else {
                let twice = (add >> 1) * 2;
                let (up, down) = (twice.min(0x7F), -twice.min(0x80));
                if twice != add || twice > 0x7F {
                    notes.convert(format!(
                        "{place}: pulse speed ${} becomes +{up}/{down} per frame (GoatTracker halves and doubles it)",
                        hex(add as u8)
                    ));
                }
                // A leg of `dist` (> 0) at `speed`; the distance it covers.
                let leg = |steps: &mut Vec<TableRow>, dist: i32, speed: i32| -> i32 {
                    let time = dist / add;
                    steps.extend(timed_rows(time, speed));
                    time * add
                };
                let from = start << 4;
                let mut at = from;
                if high > at {
                    at += leg(&mut steps, high - at, up);
                }
                let fall = steps.len() + 1;
                if at > low {
                    at -= leg(&mut steps, at - low, down);
                }
                if from < high && from > at {
                    leg(&mut steps, from - at, up);
                    steps.push(row(JUMP, 2));
                } else {
                    if high > at {
                        leg(&mut steps, high - at, up);
                    }
                    steps.push(row(JUMP, fall as u8));
                }
            }
            pulse_ptr = pulse.add(&steps)?;
        }
        // Filter: +6 is a filter-table row (gsong.c:379).
        let filter_ptr = map_filter(h[6], &place, &mut notes).unwrap_or(0);
        let wave_ptr = wave.add(&program)?;
        instruments.push(Instrument {
            name: ins.name.clone(),
            attack: h[0] >> 4,
            decay: h[0] & 0x0F,
            sustain: h[1] >> 4,
            release: h[1] & 0x0F,
            // A GT1 instrument has no first-wave or gate-timer byte: GT2's
            // defaults for a new instrument (ginstr.c:210-221: $09, 2 at speed 1).
            first_wave: 0x09,
            gate_timer: 2,
            hard_restart: !no_hard_restart,
            no_gate_off: false,
            vibrato_delay: 0,
            wave_ptr: wave_ptr as u8,
            pulse_ptr: pulse_ptr as u8,
            filter_ptr: filter_ptr as u8,
            speed_ptr: 0,
        });
    }

    // --- Patterns.
    // Funktempo's speed row (gsong.c:698): GT packs filter row 0's bytes 2-3
    // as b2<<4 | (b3 & 15) and splits that as bits 4-7 / 0-3
    // (gtable.c:876-878), so the row is (b2 & 15, b3 & 15); a packed 0 is no
    // row (gtable.c:862). Without a filter table GT reads uninitialised stack
    // bytes (gsong.c:341); zeros stand in for them.
    let (funk2, funk3) = filter_table.map_or((0, 0), |ft| (ft[2], ft[3]));
    // GT2's arpeggio conversion (gsong.c:709-797), once per (instrument,
    // parameter): a wave program of the instrument's head, then the steps X,
    // Y and 0 semitones (bit 7 of the parameter the steps' left byte, a
    // 1-frame delay each: half speed), looping on X (gsong.c:746-760). The
    // instrument is then cloned onto that program, into the slots after the
    // highest one a pattern names (gsong.c:598, 762-776), and the row plays
    // the clone; with no slot left, the row starts the program with command 8
    // (gsong.c:777-780). `None`: the wave table has no room, and the row keeps
    // command 0 00.
    let mut arpeggios: HashMap<(usize, u8), Option<Arpeggio>> = HashMap::new();
    let mut next_slot = used_instruments + 1;
    let mut patterns = Vec::with_capacity(raw_patterns.len());
    for (p, data) in raw_patterns.iter().enumerate() {
        let n = data.len() / 3;
        if n < 2 || data[(n - 1) * 3] != PATTERN_END {
            return Err(fail(format!(
                "pattern {p} does not end with the pattern-end row"
            )));
        }
        if n - 1 > MAX_PATTERN_ROWS {
            return Err(fail(format!(
                "pattern {p} has {} rows; a pattern has at most {MAX_PATTERN_ROWS}",
                n - 1
            )));
        }
        let mut rows = Vec::with_capacity(n - 1);
        // The instrument the pattern last named (gsong.c:675, 678).
        let mut current = 0usize;
        for (i, [nb, packed, param]) in rows_of(data).into_iter().take(n - 1).enumerate() {
            let place = format!("pattern {p} row {i}");
            let note = match nb {
                0..=NOTE_LAST => nb + 1,
                NOTE_GT1_KEY_OFF => NOTE_KEY_OFF,
                NOTE_REST => NOTE_NONE,
                PATTERN_END => {
                    return Err(fail(format!(
                        "{place}: note byte ${} is not a GoatTracker 1 note",
                        hex(nb)
                    )))
                }
                _ => {
                    // Any other byte is a rest: GT adds GT2's first note, $60,
                    // and rests what lands past its last, $BC (gsong.c:559-560).
                    // Its note byte is 8 bits: $9F lands on its pattern end and
                    // from $A0 the sum wraps below its first note. Not a note
                    // either way: rested here, and reported.
                    let wrapped = nb.wrapping_add(0x60);
                    if nb as u16 + 0x60 >= PATTERN_END as u16 {
                        notes.convert(format!(
                            "{place}: note byte ${} becomes GoatTracker 2's ${}, {}; rested",
                            hex(nb),
                            hex(wrapped),
                            if wrapped == PATTERN_END {
                                "its pattern end"
                            } else {
                                "no note"
                            }
                        ));
                    }
                    NOTE_NONE
                }
            };
            let mut instrument = (packed >> 3) as usize;
            if instrument != 0 {
                current = instrument;
            }
            let cmd = packed & 0x07;
            let mut command = 0u8;
            let mut out = 0usize;
            match cmd {
                0 => {
                    // Arpeggio: GT2 converts it only on a note, and only once
                    // the pattern has named an instrument (gsong.c:701-707);
                    // anywhere else the parameter is cleared (gsong.c:800-802).
                    // The program the note starts loops on, so a later row's
                    // clearing loses nothing.
                    if param != 0 && nb <= NOTE_LAST {
                        if current == 0 {
                            notes.drop(format!(
                                "{place}: arpeggio ${} before the pattern names an instrument; dropped",
                                hex(param)
                            ));
                        } else {
                            let key = (current, param);
                            let arp = match arpeggios.get(&key) {
                                Some(&arp) => arp,
                                None => {
                                    let head = arpeggio_heads
                                        .get(current - 1)
                                        .cloned()
                                        .unwrap_or_default();
                                    gt_wave_rows = MAX_TABLE_ROWS.min(gt_wave_rows + head.len());
                                    let arp = if gt_wave_rows >= MAX_TABLE_ROWS - 3 {
                                        None
                                    } else {
                                        gt_wave_rows += 4;
                                        let delay = param >> 7;
                                        let mut program = head.clone();
                                        program.extend([
                                            row(delay, (param >> 4) & 0x07),
                                            row(delay, param & 0x0F),
                                            row(delay, 0x00),
                                            row(JUMP, (head.len() + 1) as u8),
                                        ]);
                                        let ptr = wave.add(&program)?;
                                        let source = instruments
                                            .get(current - 1)
                                            .cloned()
                                            .ok_or_else(|| {
                                                fail(format!(
                                                    "{place}: instrument {current} does not exist"
                                                ))
                                            })?;
                                        if next_slot <= MAX_INSTRUMENTS {
                                            let slot = next_slot;
                                            next_slot += 1;
                                            if let Some(replaced) = instruments.get(slot - 1) {
                                                if raw.get(slot - 1).is_some_and(|r| !r.empty) {
                                                    notes.convert(format!(
                                                        "instrument {slot} (\"{}\"), which no pattern plays, is replaced by an arpeggio instrument (as GoatTracker does)",
                                                        latin1(&replaced.name)
                                                    ));
                                                }
                                            }
                                            // GT appends "0XY" when the name has room (gsong.c:769-774).
                                            let mut name = source.name.clone();
                                            if name.len() < MAX_INSTRUMENT_NAME - 3 {
                                                name.push(b'0');
                                                name.extend_from_slice(
                                                    hex(param & 0x7F).as_bytes(),
                                                );
                                            }
                                            let clone = Instrument {
                                                name,
                                                wave_ptr: ptr as u8,
                                                ..source
                                            };
                                            if slot - 1 < instruments.len() {
                                                instruments[slot - 1] = clone;
                                            } else {
                                                instruments.push(clone);
                                            }
                                            notes.convert(format!(
                                                "arpeggio ${} of instrument {current} became wave program {ptr}, played by instrument {slot}",
                                                hex(param)
                                            ));
                                            Some(Arpeggio {
                                                clone: true,
                                                value: slot,
                                            })
                                        } else {
                                            notes.convert(format!(
                                                "arpeggio ${} of instrument {current} became wave program {ptr} (command 8: no instrument slot left)",
                                                hex(param)
                                            ));
                                            Some(Arpeggio {
                                                clone: false,
                                                value: ptr,
                                            })
                                        }
                                    };
                                    arpeggios.insert(key, arp);
                                    arp
                                }
                            };
                            match arp {
                                None => notes.drop(format!(
                                    "{place}: arpeggio ${} of instrument {current} does not fit the wave table; dropped",
                                    hex(param)
                                )),
                                Some(Arpeggio { clone: true, value }) => instrument = value,
                                Some(Arpeggio { clone: false, value }) => {
                                    command = 0x8;
                                    out = value;
                                }
                            }
                        }
                    }
                }
                1..=3 => {
                    // Portamento speed: 4 x the old parameter (readme §3.4.4); 3 with 0 is tie-note.
                    command = cmd;
                    if param != 0 {
                        let v = param as usize * 4;
                        out = speed.add(&[row((v >> 8) as u8, (v & 0xFF) as u8)])?;
                    }
                }
                4 => {
                    // Vibrato: old $XY -> speed X, depth Y<<4 (readme §3.4.4's "$34" example).
                    command = 0x4;
                    if param != 0 {
                        out = speed.add(&[row(param >> 4, (param & 0x0F) << 4)])?;
                    }
                }
                5 => {
                    // Filter-table pointer: GT2's AXY (gsong.c:576-578, 690-692).
                    if let Some(ptr) = map_filter(param, &place, &mut notes) {
                        command = 0xA;
                        out = ptr;
                    }
                }
                6 => {
                    // Set SR: GT2's own command 6, kept as is (gsong.c:574-590 has no case for it).
                    command = 0x6;
                    out = param as usize;
                }
                _ => {
                    // Tempo (gsong.c:581-589, 694-699): below $F0 GT2's FXY with
                    // the same parameter, $F0 up master volume DXY with the low
                    // nibble, 00 funktempo EXY.
                    if param >= 0xF0 {
                        command = 0xD;
                        out = (param & 0x0F) as usize;
                    } else if param != 0 {
                        command = 0xF;
                        out = param as usize;
                    } else {
                        command = 0xE;
                        out = if ((funk2 as u16) << 4 | (funk3 & 0x0F) as u16) == 0 {
                            0
                        } else {
                            speed.add(&[row(funk2 & 0x0F, funk3 & 0x0F)])?
                        };
                    }
                }
            }
            rows.push(Row {
                note,
                instrument: instrument as u8,
                command,
                param: out as u8,
            });
        }
        patterns.push(Pattern { rows });
    }

    song.subsongs = decode_subsongs(&raw_lists, patterns.len(), notes.0)?;

    // An all-zero last row maps one past what was laid: GT2's table is 255
    // rows, blank past its content, so the song gets that blank row.
    let mut filter_end = 0usize;
    for ins in &instruments {
        filter_end = filter_end.max(ins.filter_ptr as usize);
    }
    for r in &filter_rows {
        if r.left == JUMP {
            filter_end = filter_end.max(r.right as usize);
        }
    }
    for pattern in &patterns {
        for r in &pattern.rows {
            if r.command == 0xA {
                filter_end = filter_end.max(r.param as usize);
            }
        }
    }
    if filter_rows.len() < filter_end {
        filter_rows.resize(filter_end, TableRow::default());
    }

    song.patterns = patterns;
    song.instruments = instruments;
    song.tables = Tables {
        wave: wave.rows,
        pulse: pulse.rows,
        filter: filter_rows,
        speed: speed.rows,
    };
    check_tables(&song.tables)?;
    Ok(song)
}
