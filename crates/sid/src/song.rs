//! The SID song file, S3 of `.ai/plan-sid-tracking.md`: the bytes a `.cmod`
//! carries for a SID song (`data.sidFile`) and this player reads. The app
//! writes them from its `SidDoc` (`src/audio/tracker/sid-doc/sid-file-codec.ts`);
//! this module is the same layout read and written in Rust, so a song goes
//! from the store to the chip without any other representation.
//!
//! Layout (little-endian, no padding; the TS codec's header has the same
//! table):
//!   'ASID', version (2), chip model (0 = 8580, 1 = 6581), channels (3),
//!   speed multiplier (1..16), tempo (1..127), then three texts (length byte
//!   0..32 + latin-1): name, author, copyright.
//!   subsong count 1..32; per subsong, per channel: entry count E 1..254,
//!   restart < E, E x (pattern, transpose i8, repeat 1..16).
//!   pattern count 1..208; per pattern: row count 1..128, rows x (note,
//!   instrument, command, param).
//!   instrument count 0..63; per instrument: name (length byte 0..16 +
//!   latin-1), AD, SR, first-frame waveform, hard-restart<<7 |
//!   no-gate-off<<6 | gate timer, vibrato delay, wave, pulse, filter, speed
//!   table pointers. An instrument is GoatTracker's: it has no waveform,
//!   pulse width or filter of its own, its tables set them (version 1, which
//!   had those fields, is not read: plan-sid-authoring.md D1).
//!   four tables (wave, pulse, filter, speed): count 0..255, the left
//!   column, then the right.
//!   Nothing after.
//!
//! Canonical, as on the TS side: every value has one encoding and the reader
//! refuses what the model refuses, so `to_bytes(parse(b)) == b` for every
//! accepted `b` (pinned by the chain test against the app's own output).

use super::SidModel;
use std::fmt;

pub const MAGIC: [u8; 4] = *b"ASID";
pub const SONG_FILE_VERSION: u8 = 2;
pub const SID_CHANNELS: usize = 3;
pub const MAX_PATTERN_ROWS: usize = 128;
pub const MAX_PATTERNS: usize = 208;
pub const MAX_INSTRUMENTS: usize = 63;
pub const MAX_ORDER_ENTRIES: usize = 254;
pub const MAX_SUBSONGS: usize = 32;
pub const MAX_TEXT: usize = 32;
pub const MAX_INSTRUMENT_NAME: usize = 16;
pub const MAX_REPEAT: u8 = 16;
pub const MIN_TRANSPOSE: i8 = -64;
pub const MAX_TRANSPOSE: i8 = 63;
pub const MAX_SPEED_MULTIPLIER: u8 = 16;
pub const MAX_TEMPO: u8 = 127;

/// Row notes: 0 empty, 1..=93 C-0..G#7 (note table index + 1), key off, key on.
pub const NOTE_NONE: u8 = 0;
pub const NOTE_FIRST: u8 = 1;
pub const NOTE_LAST: u8 = 93;
pub const NOTE_KEY_OFF: u8 = 126;
pub const NOTE_KEY_ON: u8 = 127;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Row {
    pub note: u8,
    pub instrument: u8,
    pub command: u8,
    pub param: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderEntry {
    pub pattern: u8,
    pub transpose: i8,
    pub repeat: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orderlist {
    pub entries: Vec<OrderEntry>,
    pub restart: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subsong {
    pub orderlists: Vec<Orderlist>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TableRow {
    pub left: u8,
    pub right: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tables {
    pub wave: Vec<TableRow>,
    pub pulse: Vec<TableRow>,
    pub filter: Vec<TableRow>,
    pub speed: Vec<TableRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Instrument {
    pub name: Vec<u8>,
    pub attack: u8,
    pub decay: u8,
    pub sustain: u8,
    pub release: u8,
    pub first_wave: u8,
    pub gate_timer: u8,
    pub hard_restart: bool,
    /// GT's gate-timer bit $40: a note of this instrument (named by the row
    /// read `gate_timer` ticks early) gets no early gate-off or hard restart.
    pub no_gate_off: bool,
    pub vibrato_delay: u8,
    pub wave_ptr: u8,
    pub pulse_ptr: u8,
    pub filter_ptr: u8,
    pub speed_ptr: u8,
}

impl Instrument {
    /// The chip's attack/decay register.
    pub fn ad(&self) -> u8 {
        (self.attack << 4) | self.decay
    }
    /// The chip's sustain/release register.
    pub fn sr(&self) -> u8 {
        (self.sustain << 4) | self.release
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidSong {
    pub version: u8,
    pub model: SidModel,
    pub channels: u8,
    pub speed_multiplier: u8,
    pub tempo: u8,
    pub name: Vec<u8>,
    pub author: Vec<u8>,
    pub copyright: Vec<u8>,
    pub subsongs: Vec<Subsong>,
    pub patterns: Vec<Pattern>,
    pub instruments: Vec<Instrument>,
    pub tables: Tables,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongError(pub String);

impl fmt::Display for SongError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SongError {}

fn err<T>(msg: impl Into<String>) -> Result<T, SongError> {
    Err(SongError(msg.into()))
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self, what: &str) -> Result<u8, SongError> {
        match self.bytes.get(self.at) {
            Some(&b) => {
                self.at += 1;
                Ok(b)
            }
            None => err(format!("the file ends inside {what}")),
        }
    }
    fn text(&mut self, max: usize, what: &str) -> Result<Vec<u8>, SongError> {
        let n = self.byte(what)? as usize;
        if n > max {
            return err(format!("{what} is longer than {max} characters"));
        }
        (0..n).map(|_| self.byte(what)).collect()
    }
}

fn is_note(n: u8) -> bool {
    n == NOTE_NONE || n == NOTE_KEY_OFF || n == NOTE_KEY_ON || (NOTE_FIRST..=NOTE_LAST).contains(&n)
}

impl SidSong {
    /// Reads a SID song file, refusing (with the reason) anything the app's
    /// model would refuse.
    pub fn parse(bytes: &[u8]) -> Result<SidSong, SongError> {
        let mut r = Reader { bytes, at: 0 };
        for m in MAGIC {
            if r.byte("the header")? != m {
                return err("not a SID song file (no ASID magic)");
            }
        }
        let version = r.byte("the header")?;
        if version != SONG_FILE_VERSION {
            return err(format!("version {version} is not supported"));
        }
        let model = match r.byte("the header")? {
            0 => SidModel::Sid8580,
            1 => SidModel::Sid6581,
            c => return err(format!("chip model {c} is unknown")),
        };
        let channels = r.byte("the header")?;
        if channels as usize != SID_CHANNELS {
            return err(format!("a SID song has {SID_CHANNELS} channels"));
        }
        let speed_multiplier = r.byte("the header")?;
        if !(1..=MAX_SPEED_MULTIPLIER).contains(&speed_multiplier) {
            return err("the speed multiplier is not 1-16");
        }
        let tempo = r.byte("the header")?;
        if !(1..=MAX_TEMPO).contains(&tempo) {
            return err("the tempo is not 1-127");
        }
        let name = r.text(MAX_TEXT, "the song name")?;
        let author = r.text(MAX_TEXT, "the author")?;
        let copyright = r.text(MAX_TEXT, "the copyright")?;

        let subsong_count = r.byte("the subsong count")? as usize;
        if !(1..=MAX_SUBSONGS).contains(&subsong_count) {
            return err("a song has 1-32 subsongs");
        }
        let mut subsongs = Vec::with_capacity(subsong_count);
        for _ in 0..subsong_count {
            let mut orderlists = Vec::with_capacity(SID_CHANNELS);
            for _ in 0..SID_CHANNELS {
                let count = r.byte("an orderlist")? as usize;
                let restart = r.byte("an orderlist")?;
                if !(1..=MAX_ORDER_ENTRIES).contains(&count) || restart as usize >= count {
                    return err("an orderlist has 1-254 entries and a restart inside them");
                }
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let pattern = r.byte("an orderlist")?;
                    let transpose = r.byte("an orderlist")? as i8;
                    let repeat = r.byte("an orderlist")?;
                    if !(MIN_TRANSPOSE..=MAX_TRANSPOSE).contains(&transpose) {
                        return err("an orderlist transpose is out of range");
                    }
                    if !(1..=MAX_REPEAT).contains(&repeat) {
                        return err("an orderlist repeat is not 1-16");
                    }
                    entries.push(OrderEntry {
                        pattern,
                        transpose,
                        repeat,
                    });
                }
                orderlists.push(Orderlist { entries, restart });
            }
            subsongs.push(Subsong { orderlists });
        }

        let pattern_count = r.byte("the pattern count")? as usize;
        if !(1..=MAX_PATTERNS).contains(&pattern_count) {
            return err("a song has 1-208 patterns");
        }
        let mut patterns = Vec::with_capacity(pattern_count);
        for _ in 0..pattern_count {
            let count = r.byte("a pattern")? as usize;
            if !(1..=MAX_PATTERN_ROWS).contains(&count) {
                return err("a pattern has 1-128 rows");
            }
            let mut rows = Vec::with_capacity(count);
            for _ in 0..count {
                rows.push(Row {
                    note: r.byte("a pattern")?,
                    instrument: r.byte("a pattern")?,
                    command: r.byte("a pattern")?,
                    param: r.byte("a pattern")?,
                });
            }
            patterns.push(Pattern { rows });
        }

        let instrument_count = r.byte("the instrument count")? as usize;
        if instrument_count > MAX_INSTRUMENTS {
            return err("a song has 0-63 instruments");
        }
        let mut instruments = Vec::with_capacity(instrument_count);
        for _ in 0..instrument_count {
            let name = r.text(MAX_INSTRUMENT_NAME, "an instrument name")?;
            let mut b = [0u8; 9];
            for v in b.iter_mut() {
                *v = r.byte("an instrument")?;
            }
            instruments.push(Instrument {
                name,
                attack: b[0] >> 4,
                decay: b[0] & 0x0F,
                sustain: b[1] >> 4,
                release: b[1] & 0x0F,
                first_wave: b[2],
                gate_timer: b[3] & 0x3F,
                hard_restart: b[3] & 0x80 != 0,
                no_gate_off: b[3] & 0x40 != 0,
                vibrato_delay: b[4],
                wave_ptr: b[5],
                pulse_ptr: b[6],
                filter_ptr: b[7],
                speed_ptr: b[8],
            });
        }

        let mut read_table = |what: &str| -> Result<Vec<TableRow>, SongError> {
            let n = r.byte(what)? as usize;
            let left: Vec<u8> = (0..n).map(|_| r.byte(what)).collect::<Result<_, _>>()?;
            left.into_iter()
                .map(|l| {
                    Ok(TableRow {
                        left: l,
                        right: r.byte(what)?,
                    })
                })
                .collect()
        };
        let tables = Tables {
            wave: read_table("the wave table")?,
            pulse: read_table("the pulse table")?,
            filter: read_table("the filter table")?,
            speed: read_table("the speed table")?,
        };
        if r.at != bytes.len() {
            return err(format!("{} bytes follow the song", bytes.len() - r.at));
        }

        let song = SidSong {
            version,
            model,
            channels,
            speed_multiplier,
            tempo,
            name,
            author,
            copyright,
            subsongs,
            patterns,
            instruments,
            tables,
        };
        song.check_references()?;
        Ok(song)
    }

    /// The cross-references the reader cannot check field by field.
    fn check_references(&self) -> Result<(), SongError> {
        for s in &self.subsongs {
            for list in &s.orderlists {
                if list
                    .entries
                    .iter()
                    .any(|e| e.pattern as usize >= self.patterns.len())
                {
                    return err("an orderlist plays a pattern that does not exist");
                }
            }
        }
        for p in &self.patterns {
            for row in &p.rows {
                if !is_note(row.note) {
                    return err(format!("note {} is not a note", row.note));
                }
                if row.instrument as usize > self.instruments.len() {
                    return err(format!("instrument {} does not exist", row.instrument));
                }
                if row.command > 0x0F {
                    return err(format!("command {} is not 0-F", row.command));
                }
            }
        }
        let t = &self.tables;
        for ins in &self.instruments {
            if ins.wave_ptr as usize > t.wave.len()
                || ins.pulse_ptr as usize > t.pulse.len()
                || ins.filter_ptr as usize > t.filter.len()
                || ins.speed_ptr as usize > t.speed.len()
            {
                return err("an instrument points past a table");
            }
        }
        Ok(())
    }

    /// The file of this song: the inverse of `parse`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.push(self.version);
        out.push(match self.model {
            SidModel::Sid8580 => 0,
            SidModel::Sid6581 => 1,
        });
        out.extend_from_slice(&[self.channels, self.speed_multiplier, self.tempo]);
        for text in [&self.name, &self.author, &self.copyright] {
            out.push(text.len() as u8);
            out.extend_from_slice(text);
        }
        out.push(self.subsongs.len() as u8);
        for s in &self.subsongs {
            for list in &s.orderlists {
                out.push(list.entries.len() as u8);
                out.push(list.restart);
                for e in &list.entries {
                    out.extend_from_slice(&[e.pattern, e.transpose as u8, e.repeat]);
                }
            }
        }
        out.push(self.patterns.len() as u8);
        for p in &self.patterns {
            out.push(p.rows.len() as u8);
            for r in &p.rows {
                out.extend_from_slice(&[r.note, r.instrument, r.command, r.param]);
            }
        }
        out.push(self.instruments.len() as u8);
        for ins in &self.instruments {
            out.push(ins.name.len() as u8);
            out.extend_from_slice(&ins.name);
            out.extend_from_slice(&[
                ins.ad(),
                ins.sr(),
                ins.first_wave,
                if ins.hard_restart { 0x80 } else { 0 }
                    | if ins.no_gate_off { 0x40 } else { 0 }
                    | ins.gate_timer,
                ins.vibrato_delay,
                ins.wave_ptr,
                ins.pulse_ptr,
                ins.filter_ptr,
                ins.speed_ptr,
            ]);
        }
        for table in [
            &self.tables.wave,
            &self.tables.pulse,
            &self.tables.filter,
            &self.tables.speed,
        ] {
            out.push(table.len() as u8);
            out.extend(table.iter().map(|r| r.left));
            out.extend(table.iter().map(|r| r.right));
        }
        out
    }
}
