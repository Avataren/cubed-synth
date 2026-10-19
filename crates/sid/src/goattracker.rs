//! GoatTracker songs (`.sng`) as [`SidSong`]s: a port of the another_synth
//! DAW's importer (`src/audio/tracker/sid-doc/gt-sng-common.ts`,
//! `gt-sng-read.ts` and `gt-sng-gt1.ts`), which wrote it from GoatTracker
//! v2.72's readme (§6.1 the format, §3.1 orderlist semantics) and the bytes of
//! its curated corpus, and corrected the GoatTracker 1 conversion and the
//! repeat count against GoatTracker 2's own source (read for its facts only).
//! The comments keep the TypeScript's citations.
//!
//! [`import`] dispatches on the magic bytes, never the file name:
//!   - `GTS5`: GoatTracker 2's format, readme §6.1;
//!   - `GTS!`: GoatTracker 1's, converted as GoatTracker 2 converts it
//!     ([`gt1`]);
//!   - `GTS2`/`GTS3`/`GTS4` (GoatTracker 2 betas): refused with the reason;
//!     anything else is not a GoatTracker song.
//!
//! A file is read whole or refused whole. The song it gives is the song the
//! DAW's import gives: `to_bytes` writes the `.asid` the DAW would save, byte
//! for byte (`tests/goattracker.rs` checks it against the DAW's output).
//! What a `.sng` does not say (the chip model and the speed multiplier,
//! GoatTracker's `-E` and `-S` options) comes from [`GtSongHints`], which
//! [`hints_from_name`] reads from the tags GoatTracker authors put in file
//! names.

mod gt1;

use crate::song::{
    Instrument, OrderEntry, Orderlist, Pattern, Row, SidSong, SongError, Subsong, TableRow, Tables,
    MAX_INSTRUMENTS, MAX_PATTERNS, MAX_PATTERN_ROWS, MAX_SUBSONGS, NOTE_KEY_OFF, NOTE_KEY_ON,
    NOTE_NONE, SID_CHANNELS, SONG_FILE_VERSION,
};
use crate::SidModel;

/// Readme §6.1.1: the 4-byte identification string at +0.
pub const MAGIC_GTS5: [u8; 4] = *b"GTS5";
/// GoatTracker v1's song identification (not in the v2 readme; measured on 22 corpus files).
pub const MAGIC_GT1: [u8; 4] = *b"GTS!";
/// Readme §6.1.1: song name +4, author +36, copyright +68, each 32 bytes padded with zeros.
const TEXT_OFFSETS: [usize; 3] = [4, 36, 68];
const TEXT_LENGTH: usize = 32;
/// Readme §6.1.1: the number of subtunes, the byte at +100; the orderlists follow at +101.
const SUBTUNE_COUNT_OFFSET: usize = 100;
/// Readme §6.1.3: the instrument name is the last 16 of an instrument's 25 bytes.
const INSTRUMENT_NAME_LENGTH: usize = 16;
/// Readme §6.1.3: +0 AD, +1 SR, +2..+5 wave/pulse/filter/speed pointers, +6
/// vibrato delay, +7 gate timer, +8 first wave, +9 name.
const INSTRUMENT_BYTES: usize = 9 + INSTRUMENT_NAME_LENGTH;
/// Readme §6.1.2: orderlist values. A repeat byte $D0 + k plays the next
/// pattern k + 1 times, $D0 once to $DF 16 times: GT stores k and replays the
/// pattern while it counts down (gplay.c:977-986).
const ORDER_REPEAT: u8 = 0xD0;
const ORDER_TRANSPOSE: u8 = 0xE0;
const ORDER_END: u8 = 0xFF;
/// Readme §3.1: transpose 0 is $F0 (up 0-14 = $F0-$FE, down 1-15 = $EF-$E1; $E0 is -16, INFERRED).
const TRANSPOSE_ZERO: i16 = 0xF0;
/// Readme §6.1.6: note bytes $60-$BC are C-0..G#7, the song's notes 1..93.
const NOTE_FIRST: u8 = 0x60;
const NOTE_LAST: u8 = 0xBC;
const NOTE_REST: u8 = 0xBD;
const NOTE_GT_KEY_OFF: u8 = 0xBE;
const NOTE_GT_KEY_ON: u8 = 0xBF;
const PATTERN_END: u8 = 0xFF;
/// Readme §3.3: HR/Gate Timer bit $80 disables hard restart, $40 disables gate-off.
const GATE_NO_HARD_RESTART: u8 = 0x80;
const GATE_NO_GATEOFF: u8 = 0x40;
/// The most rows a table holds.
const MAX_TABLE_ROWS: usize = 255;
/// A `.sng` has no tempo field: GoatTracker starts every song at 6 (readme §3.2 FXY). INFERRED default.
const START_TEMPO: u8 = 6;
/// The chip a song plays on when its name does not say: the DAW's house default.
const DEFAULT_MODEL: SidModel = SidModel::Sid6581;
const TABLE_NAMES: [&str; 4] = ["wave", "pulse", "filter", "speed"];

/// Which GoatTracker wrote the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GtVariant {
    /// GoatTracker 2 (`GTS5`, 2.59 on).
    Gts5,
    /// GoatTracker 1 (`GTS!`), converted.
    Gt1,
}

/// What kind of liberty an import took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GtNoteKind {
    /// An instrument pointed past its table's stored rows: the table was padded with blank rows.
    TablePadded,
    /// An orderlist loops to a point where GT's running transpose/repeat differs from the first pass.
    LoopTranspose,
    /// A GoatTracker 1 conversion (each INFERRED from the corpus bytes).
    Gt1Convert,
    /// GoatTracker 1 data the song cannot hold, dropped.
    Gt1Dropped,
}

impl GtNoteKind {
    /// The DAW's name for it.
    pub fn as_str(self) -> &'static str {
        match self {
            GtNoteKind::TablePadded => "table-padded",
            GtNoteKind::LoopTranspose => "loop-transpose",
            GtNoteKind::Gt1Convert => "gt1-convert",
            GtNoteKind::Gt1Dropped => "gt1-dropped",
        }
    }
}

/// Something the import did that the `.sng` does not say, or could not carry into the song.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GtNote {
    pub kind: GtNoteKind,
    pub message: String,
}

/// The playback facts a `.sng` does not carry (GoatTracker takes them from its command line).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GtSongHints {
    /// GT's `-E` option; default the 6581.
    pub model: Option<SidModel>,
    /// GT's `-S` option (1 = 50 Hz), 2..16; default 1.
    pub speed_multiplier: Option<u8>,
}

/// A song read from a `.sng`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GtImport {
    pub song: SidSong,
    pub variant: GtVariant,
    /// What the import inferred or could not carry, per occurrence.
    pub notes: Vec<GtNote>,
}

/// Whether `bytes` start like a GoatTracker song (any `GTS` magic, so a beta file gets its refusal).
pub fn looks_like_gt_song(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..3] == b"GTS" && matches!(bytes[3], b'!' | b'2'..=b'5')
}

/// The song a `.sng` holds, or why it holds none. `hints` carries what the
/// file cannot (GT's command-line chip model and speed multiplier).
pub fn import(bytes: &[u8], hints: GtSongHints) -> Result<GtImport, SongError> {
    let magic = bytes.get(..4);
    let mut notes = Vec::new();
    let (song, variant) = if magic == Some(&MAGIC_GTS5[..]) {
        (read_gts5(bytes, &mut notes, hints)?, GtVariant::Gts5)
    } else if magic == Some(&MAGIC_GT1[..]) {
        let header = song_header(bytes, hints)?;
        (gt1::read(bytes, &mut notes, header)?, GtVariant::Gt1)
    } else if matches!(magic, Some([b'G', b'T', b'S', b'2'..=b'4'])) {
        let magic = String::from_utf8_lossy(&bytes[..4]);
        return Err(SongError(format!(
            "it is a GoatTracker 2 beta song ({magic}); only GTS5 (GoatTracker 2.59 on) and GTS! (GoatTracker 1) songs are read. Load and save it in a current GoatTracker to convert it"
        )));
    } else {
        return Err(SongError(
            "it is not a GoatTracker song (no GTS5 or GTS! identification string)".into(),
        ));
    };
    // The song's own checks (the DAW's makeSidDoc): a song that breaks them is refused whole.
    let song = SidSong::parse(&song.to_bytes())
        .map_err(|e| SongError(format!("its data does not make a valid SID song ({e})")))?;
    Ok(GtImport {
        song,
        variant,
        notes,
    })
}

/// The hints a file NAME gives: GoatTracker authors tag what the file cannot
/// hold in the file name (`defunkt_final_fv_po_ro_6581_ffff.sng`,
/// `space_2x.sng`, `mw title remix, 2x-speed.sng`). A `6581`/`8580` token
/// names the chip, an `Nx` token (N 2..16) the speed multiplier. INFERRED: a
/// naming convention of the corpus, not part of any format. A name may be a
/// path, and percent-encoded (a URL's).
pub fn hints_from_name(name: &str) -> GtSongHints {
    let decoded = percent_decode(name).unwrap_or_else(|| name.to_string());
    let lower = decoded.to_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or("");
    let base = base.strip_suffix(".sng").unwrap_or(base).as_bytes();
    let boundary = |at: usize| at == 0 || !base[at - 1].is_ascii_alphanumeric();
    let ends = |at: usize| at == base.len() || !base[at].is_ascii_alphanumeric();
    let mut hints = GtSongHints::default();
    for at in 0..base.len() {
        if boundary(at) && base.len() - at >= 4 && ends(at + 4) {
            match &base[at..at + 4] {
                b"6581" => hints.model = Some(SidModel::Sid6581),
                b"8580" => hints.model = Some(SidModel::Sid8580),
                _ => continue,
            }
            break;
        }
    }
    for at in 0..base.len() {
        if !boundary(at) {
            continue;
        }
        // One or two digits, then 'x' and no letter or digit after it.
        let found = [2, 1].into_iter().find(|&digits| {
            base.len() > at + digits
                && base[at..at + digits].iter().all(u8::is_ascii_digit)
                && base[at + digits] == b'x'
                && ends(at + digits + 1)
        });
        if let Some(digits) = found {
            let n: u8 = std::str::from_utf8(&base[at..at + digits])
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if (2..=16).contains(&n) {
                hints.speed_multiplier = Some(n);
            }
            break;
        }
    }
    hints
}

/// `decodeURIComponent`: `None` when the escapes are malformed or not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    if !text.contains('%') {
        return Some(text.to_string());
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

// --------------------------------------------------------------------- shared

/// A bounds-checked byte reader whose failures say where the file ended.
struct GtReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> GtReader<'a> {
    fn new(bytes: &'a [u8], at: usize) -> Self {
        GtReader { bytes, at }
    }
    fn byte(&mut self, what: &str) -> Result<u8, SongError> {
        let v = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| fail(format!("the file ends inside {what}")))?;
        self.at += 1;
        Ok(v)
    }
    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8], SongError> {
        if self.at + n > self.bytes.len() {
            return Err(fail(format!("the file ends inside {what}")));
        }
        let out = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(out)
    }
    fn left(&self) -> usize {
        self.bytes.len() - self.at
    }
}

fn fail(message: impl Into<String>) -> SongError {
    SongError(message.into())
}

/// A byte as two upper-case hex digits (for refusal texts).
fn hex(b: u8) -> String {
    format!("{b:02X}")
}

/// Latin-1 bytes as text (for messages).
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// The text of a zero-padded field: up to the first NUL (readme §6.1.1 "padded with zeros").
fn gt_text(field: &[u8]) -> Vec<u8> {
    field.iter().copied().take_while(|&b| b != 0).collect()
}

/// The fields every variant shares: the §6.1.1 header texts, and the hinted playback facts.
fn song_header(bytes: &[u8], hints: GtSongHints) -> Result<SidSong, SongError> {
    if bytes.len() < SUBTUNE_COUNT_OFFSET + 1 {
        return Err(fail("the file ends inside the song header"));
    }
    let [name, author, copyright] = TEXT_OFFSETS.map(|at| gt_text(&bytes[at..at + TEXT_LENGTH]));
    Ok(SidSong {
        version: SONG_FILE_VERSION,
        model: hints.model.unwrap_or(DEFAULT_MODEL),
        channels: SID_CHANNELS as u8,
        speed_multiplier: hints.speed_multiplier.unwrap_or(1),
        tempo: START_TEMPO,
        name,
        author,
        copyright,
        subsongs: Vec::new(),
        patterns: Vec::new(),
        instruments: Vec::new(),
        tables: Tables::default(),
    })
}

/// The subtune count at +100, refused outside 1-32.
fn read_subtune_count(r: &mut GtReader) -> Result<usize, SongError> {
    let subtunes = r.byte("the song header")? as usize;
    if !(1..=MAX_SUBSONGS).contains(&subtunes) {
        return Err(fail(format!(
            "it declares {subtunes} subtunes; a song has 1-{MAX_SUBSONGS}"
        )));
    }
    Ok(subtunes)
}

/// Reads the orderlists (readme §6.1.2): for each subtune, for each channel, a
/// length byte n, then n+1 bytes: n bytes of data whose last is the RST
/// endmark $FF (measured: n counts the endmark), then the restart position.
fn read_orderlists<'a>(
    r: &mut GtReader<'a>,
    subtunes: usize,
    channels: usize,
) -> Result<Vec<Vec<&'a [u8]>>, SongError> {
    (0..subtunes)
        .map(|s| {
            (0..channels)
                .map(|c| {
                    let what = format!("subtune {s} channel {}'s orderlist", c + 1);
                    let n = r.byte(&what)? as usize;
                    r.take(n + 1, &what)
                })
                .collect()
        })
        .collect()
}

/// One raw orderlist as the song's: GoatTracker's commands are state (readme
/// §3.1: a TRANSPOSE holds until the next one; a REPEAT applies to the
/// pattern after it), the song's entries each carry their own, so the first
/// pass is played through and every pattern gets the transpose and repeat it
/// plays with. The restart byte indexes the orderlist BYTES; it becomes the
/// entry whose command run (or pattern byte) it points into.
///
/// The `bool`: GT keeps the running transpose when it loops (§3.1 "Transpose
/// is automatically reset only when starting the song, not when looping"), so
/// a loop that does not restate it plays the looped entries with the
/// end-of-list transpose. The song can only hold the first pass's; this says
/// when the second pass would differ.
fn decode_orderlist(
    data: &[u8],
    place: &str,
    pattern_count: usize,
) -> Result<(Orderlist, bool), SongError> {
    let n = data.len() - 1;
    if n < 1 || data[n - 1] != ORDER_END {
        return Err(fail(format!("{place}'s orderlist has no RST endmark")));
    }
    let restart_byte = data[n] as usize;
    if restart_byte >= n - 1 {
        return Err(fail(format!(
            "{place}'s orderlist restarts at byte {restart_byte}, past its end"
        )));
    }
    let transpose_of = |v: u8| (v as i16 - TRANSPOSE_ZERO) as i8;
    let repeat_of = |v: u8| (v & 0x0F) + 1;
    let mut entries = Vec::new();
    let mut pattern_at = Vec::new();
    let mut transpose = 0i8;
    let mut repeat = 1u8;
    let mut in_run = false;
    for (k, &v) in data[..n - 1].iter().enumerate() {
        in_run = true;
        if v == ORDER_END {
            return Err(fail(format!(
                "{place}'s orderlist has an endmark before its end"
            )));
        }
        if v >= ORDER_TRANSPOSE {
            transpose = transpose_of(v);
        } else if v >= ORDER_REPEAT {
            repeat = repeat_of(v);
        } else {
            if v as usize >= pattern_count {
                return Err(fail(format!(
                    "{place}'s orderlist plays pattern {v}, but the song has {pattern_count}"
                )));
            }
            entries.push(OrderEntry {
                pattern: v,
                transpose,
                repeat,
            });
            pattern_at.push(k);
            repeat = 1;
            in_run = false;
        }
    }
    if entries.is_empty() {
        return Err(fail(format!("{place}'s orderlist plays no pattern")));
    }
    if in_run {
        return Err(fail(format!(
            "{place}'s orderlist ends with a command, not a pattern (readme §3.1)"
        )));
    }
    // The list ends with a pattern, so one stands at or after the restart byte.
    let restart = pattern_at
        .iter()
        .position(|&at| at >= restart_byte)
        .unwrap_or(0);
    // The second pass, from the restart byte with the running state GT keeps.
    let mut loop_transpose = transpose;
    let mut loop_repeat = 1u8;
    let mut e = restart;
    let mut loop_differs = false;
    for &v in &data[restart_byte..n - 1] {
        if v >= ORDER_TRANSPOSE {
            loop_transpose = transpose_of(v);
        } else if v >= ORDER_REPEAT {
            loop_repeat = repeat_of(v);
        } else {
            let first = entries[e];
            if first.transpose != loop_transpose || first.repeat != loop_repeat {
                loop_differs = true;
            }
            loop_repeat = 1;
            e += 1;
        }
    }
    Ok((
        Orderlist {
            entries,
            restart: restart as u8,
        },
        loop_differs,
    ))
}

/// The subsongs of raw orderlists, with a note for each loop the song cannot hold.
fn decode_subsongs(
    lists: &[Vec<&[u8]>],
    pattern_count: usize,
    notes: &mut Vec<GtNote>,
) -> Result<Vec<Subsong>, SongError> {
    let mut subsongs = Vec::with_capacity(lists.len());
    for (sub, lists) in lists.iter().enumerate() {
        let mut orderlists = Vec::with_capacity(lists.len());
        for (c, data) in lists.iter().enumerate() {
            let place = format!("subtune {sub} channel {}", c + 1);
            let (list, loop_differs) = decode_orderlist(data, &place, pattern_count)?;
            if loop_differs {
                notes.push(GtNote {
                    kind: GtNoteKind::LoopTranspose,
                    message: format!(
                        "{place}: its loop replays with a running transpose/repeat the doc cannot hold; the first pass's is kept"
                    ),
                });
            }
            orderlists.push(list);
        }
        subsongs.push(Subsong { orderlists });
    }
    Ok(subsongs)
}

// --------------------------------------------------------------------- GTS5

/// Readme §6.1.6 note byte -> the song's note.
fn gt_note(b: u8, place: &str) -> Result<u8, SongError> {
    match b {
        NOTE_FIRST..=NOTE_LAST => Ok(b - NOTE_FIRST + 1),
        NOTE_REST => Ok(NOTE_NONE),
        NOTE_GT_KEY_OFF => Ok(NOTE_KEY_OFF),
        NOTE_GT_KEY_ON => Ok(NOTE_KEY_ON),
        _ => Err(fail(format!(
            "{place}: note byte ${} is not a GoatTracker note",
            hex(b)
        ))),
    }
}

struct Gts5Structure<'a> {
    orderlists: Vec<Vec<&'a [u8]>>,
    instruments: Vec<&'a [u8]>,
    tables: Vec<(&'a [u8], &'a [u8])>,
    patterns: Vec<&'a [u8]>,
}

/// The §6.1 sections, read blind (byte counts only), for `channels` orderlists per subtune.
fn read_gts5_structure(bytes: &[u8], channels: usize) -> Result<Gts5Structure<'_>, SongError> {
    let mut r = GtReader::new(bytes, SUBTUNE_COUNT_OFFSET);
    let subtunes = read_subtune_count(&mut r)?;
    let orderlists = read_orderlists(&mut r, subtunes, channels)?;
    let instrument_count = r.byte("the instrument count")? as usize;
    if instrument_count > MAX_INSTRUMENTS {
        return Err(fail(format!(
            "it declares {instrument_count} instruments; a song has at most {MAX_INSTRUMENTS}"
        )));
    }
    let instruments = (0..instrument_count)
        .map(|i| r.take(INSTRUMENT_BYTES, &format!("instrument {}", i + 1)))
        .collect::<Result<Vec<_>, _>>()?;
    let tables = TABLE_NAMES
        .iter()
        .map(|name| {
            let what = format!("the {name} table");
            let n = r.byte(&what)? as usize;
            Ok((r.take(n, &what)?, r.take(n, &what)?))
        })
        .collect::<Result<Vec<_>, SongError>>()?;
    let pattern_count = r.byte("the pattern count")? as usize;
    if !(1..=MAX_PATTERNS).contains(&pattern_count) {
        return Err(fail(format!(
            "it declares {pattern_count} patterns; a song has 1-{MAX_PATTERNS}"
        )));
    }
    let patterns = (0..pattern_count)
        .map(|p| {
            let what = format!("pattern {p}");
            let m = r.byte(&what)? as usize;
            r.take(m * 4, &what)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if r.left() != 0 {
        return Err(fail(format!("{} bytes follow the last pattern", r.left())));
    }
    Ok(Gts5Structure {
        orderlists,
        instruments,
        tables,
        patterns,
    })
}

fn read_gts5(
    bytes: &[u8],
    notes: &mut Vec<GtNote>,
    hints: GtSongHints,
) -> Result<SidSong, SongError> {
    let mut song = song_header(bytes, hints)?;
    let s = match read_gts5_structure(bytes, SID_CHANNELS) {
        Ok(s) => s,
        // The S7 probe: a stereo (6-orderlist) song reads exactly as one. Say
        // so rather than blame the bytes.
        Err(error) => {
            if read_gts5_structure(bytes, 2 * SID_CHANNELS).is_ok() {
                return Err(fail(
                    "it is a dual-SID (6-channel) song; dual SID is not supported yet (plan-sid-tracking.md S7)",
                ));
            }
            return Err(error);
        }
    };

    for (p, data) in s.patterns.iter().enumerate() {
        let m = data.len() / 4;
        // Measured: the stored length counts the $FF pattern-end row (readme §6.1.6 "Value $FF is pattern end").
        if m < 2 || data[(m - 1) * 4] != PATTERN_END {
            return Err(fail(format!(
                "pattern {p} does not end with the pattern-end row"
            )));
        }
        if m - 1 > MAX_PATTERN_ROWS {
            return Err(fail(format!(
                "pattern {p} has {} rows; a pattern has at most {MAX_PATTERN_ROWS}",
                m - 1
            )));
        }
        let mut rows = Vec::with_capacity(m - 1);
        for (i, &[n, instrument, command, param]) in
            data[..(m - 1) * 4].as_chunks::<4>().0.iter().enumerate()
        {
            let place = format!("pattern {p} row {i}");
            if instrument as usize > s.instruments.len() {
                return Err(fail(format!(
                    "{place}: instrument {instrument} does not exist (the song has {})",
                    s.instruments.len()
                )));
            }
            if command > 0x0F {
                return Err(fail(format!(
                    "{place}: command byte ${} is not 0-F",
                    hex(command)
                )));
            }
            rows.push(Row {
                note: gt_note(n, &place)?,
                instrument,
                command,
                param,
            });
        }
        song.patterns.push(Pattern { rows });
    }

    song.subsongs = decode_subsongs(&s.orderlists, song.patterns.len(), notes)?;

    // Readme §6.1.4: 4 tables, each n left bytes then n right bytes.
    let mut tables: Vec<Vec<TableRow>> = s
        .tables
        .iter()
        .map(|(left, right)| {
            left.iter()
                .zip(right.iter())
                .map(|(&left, &right)| TableRow { left, right })
                .collect()
        })
        .collect();
    for (i, b) in s.instruments.iter().enumerate() {
        let gate = b[7];
        for (t, &ptr) in b[2..6].iter().enumerate() {
            let table = &mut tables[t];
            if ptr as usize > table.len() {
                // GoatTracker's tables are 255 rows in memory and the file
                // stores the used prefix: past it are blank rows. INFERRED
                // from the two corpus instruments that point there.
                notes.push(GtNote {
                    kind: GtNoteKind::TablePadded,
                    message: format!(
                        "instrument {}: its {} pointer {ptr} is past the table's {} rows; blank rows added",
                        i + 1,
                        TABLE_NAMES[t],
                        table.len()
                    ),
                });
                table.resize(ptr as usize, TableRow::default());
            }
        }
        song.instruments.push(Instrument {
            name: gt_text(&b[9..9 + INSTRUMENT_NAME_LENGTH]),
            attack: b[0] >> 4,
            decay: b[0] & 0x0F,
            sustain: b[1] >> 4,
            release: b[1] & 0x0F,
            // A GT instrument has no waveform, pulse width or filter of its
            // own: its tables set them.
            first_wave: b[8],
            gate_timer: gate & 0x3F,
            hard_restart: gate & GATE_NO_HARD_RESTART == 0,
            no_gate_off: gate & GATE_NO_GATEOFF != 0,
            vibrato_delay: b[6],
            wave_ptr: b[2],
            pulse_ptr: b[3],
            filter_ptr: b[4],
            speed_ptr: b[5],
        });
    }
    let [wave, pulse, filter, speed]: [Vec<TableRow>; 4] = tables
        .try_into()
        .unwrap_or_else(|_| unreachable!("four tables"));
    song.tables = Tables {
        wave,
        pulse,
        filter,
        speed,
    };
    check_tables(&song.tables)?;
    Ok(song)
}

/// Tables longer than a song holds (the DAW's model refuses them; the file
/// would count their rows in one byte).
fn check_tables(tables: &Tables) -> Result<(), SongError> {
    for (name, table) in
        TABLE_NAMES
            .iter()
            .zip([&tables.wave, &tables.pulse, &tables.filter, &tables.speed])
    {
        if table.len() > MAX_TABLE_ROWS {
            return Err(fail(format!(
                "its data does not make a valid SID song (the {name} table has {} rows; a table holds {MAX_TABLE_ROWS})",
                table.len()
            )));
        }
    }
    Ok(())
}
