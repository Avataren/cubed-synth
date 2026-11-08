//! Writing `.ahx` and `.hvl` files: [`format::parse`](crate::format::parse)
//! read in reverse, byte for byte (and another_synth's `serializeAhx`, which
//! this follows, with its limits and its refusals).
//!
//! Nothing is clamped. A value that does not fit its bits, a song that
//! disagrees with itself (`track_nr` against `tracks.len()`), or a step the
//! format cannot hold is an error naming it, and an error never leaves partial
//! bytes: the file is built in memory and returned at the end.
//!
//! # What the model does not carry
//!
//! The parser forgets a few things a file has. [`Origin`] holds them, read from
//! the file the song was parsed from ([`Origin::from_bytes`]), and nothing else:
//!
//! - the blank-first-track flag (header byte 6, bit 7): the parse makes an empty
//!   track 0 and forgets whether the file stored one (a few version-0 files store
//!   an explicit all-zero track 0 with the flag clear). Without an origin the
//!   flag is "track 0 is all zero";
//! - the bits the loader ignores in an instrument core (bytes 9 to 11, the top
//!   two bits of byte 19). They are inert to the engine, so they are copied back
//!   onto the instrument with the same number and written as zero otherwise.
//!
//! Parse normalisations that do not round-trip are not undone: a restart at or
//! past the position count is stored as the last position, an AHX subsong start
//! past the end as 0, a version-0 AHX filter-toggle (PList command 4) parameter
//! loses its high nibble, and an HVL step that is all zero is written as the
//! one-byte blank escape (`0x3f`) wherever the file spelled it out. The
//! [`format::parse`](crate::format::parse) of what this writes equals the
//! parse of the song it was given (`tests/ahx_write.rs` checks it on every
//! fixture), and for a file that came from a tracker the bytes usually equal
//! the original too.
//!
//! Names are Latin-1, a byte per character; the parser reads them that way.

use crate::format::{
    Instrument, PListEntry, Song, SongFormat, Step, INSTRUMENT_CORE_BYTES, MAX_CHANNELS,
};
use std::fmt;

/// The most positions a song holds (the header's count is 12 bits and the
/// loader rejects more than 1000).
pub const MAX_POSITIONS: usize = 1000;
/// The most rows a track has.
pub const MAX_TRACK_LENGTH: usize = 64;
/// The most tracks a song holds (`track_nr` is a byte).
pub const MAX_TRACKS: usize = 256;
/// The most instruments a song can use: a step's instrument is 6 bits in AHX
/// and HivelyTracker has 63. (The loader reads up to 64.)
pub const MAX_INSTRUMENTS: usize = 63;
/// The most PList rows an instrument holds (its length is one byte).
pub const MAX_PLIST_ROWS: usize = 255;
/// The most subsongs (the count is a byte).
pub const MAX_SUBSONGS: usize = 255;
/// The most channels an HVL header can say (`(buf[8] >> 2) + 4`).
pub const HVL_HEADER_MAX_CHANNELS: usize = 67;
/// The highest restart position an HVL header holds (10 bits).
pub const HVL_RESTART_MAX: usize = 0x3ff;
/// The file's 16-bit string table offset: everything before the names must fit
/// under it.
pub const SIZE_LIMIT: usize = 0xffff;
/// Header bytes before the subsong list: AHX, HVL (which adds mix gain and
/// default stereo).
pub const AHX_HEADER_BYTES: usize = 14;
pub const HVL_HEADER_BYTES: usize = 16;
/// An HVL step that is entirely zero is written as this one byte.
pub const HVL_BLANK_STEP: u8 = 0x3f;
/// PList commands an AHX entry's 3-bit code can hold (6 and 7 are 12 and 15).
pub const AHX_PLIST_COMMANDS: [u8; 8] = [0, 1, 2, 3, 4, 5, 12, 15];

/// A song the format cannot hold, or a model that disagrees with itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteError(pub String);

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot write this AHX/HVL song: {}", self.0)
    }
}

impl std::error::Error for WriteError {}

impl From<WriteError> for String {
    fn from(e: WriteError) -> String {
        e.to_string()
    }
}

fn fail<T>(problem: impl Into<String>) -> Result<T, WriteError> {
    Err(WriteError(problem.into()))
}

/// What a file has that the parsed [`Song`] does not (see the module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origin {
    /// The blank-first-track flag as the file had it; `None` means "track 0 is
    /// all zero".
    pub blank_first_track: Option<bool>,
    /// Per instrument (`inert[n - 1]` is instrument `n`): bytes 9, 10 and 11
    /// of its core and `byte 19 & 0xc0`.
    pub inert: Vec<[u8; 4]>,
}

impl Origin {
    /// Reads what `base` (an AHX or HVL file) holds that the parse does not,
    /// walking the file with the parser's own field arithmetic. An error for
    /// bytes that are not an AHX or HVL file the loader accepts.
    pub fn from_bytes(base: &[u8]) -> Result<Origin, WriteError> {
        let at = |i: usize| base.get(i).copied().unwrap_or(0);
        let is_ahx = base.starts_with(b"THX");
        let is_hvl = base.starts_with(b"HVL");
        if base.len() < 16
            || !(if is_ahx {
                at(3) < 3
            } else {
                is_hvl && at(3) < 2
            })
        {
            return fail("base is not an AHX/HVL file");
        }
        let b6 = at(6);
        let positions = (((b6 & 0x0f) as usize) << 8) | at(7) as usize;
        let track_length = at(10) as usize;
        let track_nr = at(11) as usize;
        let instrument_nr = at(12) as usize;
        let subsong_nr = at(13) as usize;
        if positions > MAX_POSITIONS || track_length > MAX_TRACK_LENGTH || instrument_nr > 64 {
            return fail("base has header dimensions the loader rejects");
        }
        let blank_first = b6 & 0x80 != 0;
        let channels = if is_ahx { 4 } else { (at(8) >> 2) as usize + 4 };
        let mut pos = if is_ahx {
            AHX_HEADER_BYTES
        } else {
            HVL_HEADER_BYTES
        } + subsong_nr * 2
            + positions * channels * 2;
        for track in 0..=track_nr {
            if blank_first && track == 0 {
                continue;
            }
            if is_ahx {
                pos += track_length * 3;
                continue;
            }
            for _ in 0..track_length {
                pos += if at(pos) == HVL_BLANK_STEP { 1 } else { 5 };
            }
        }
        let entry_bytes = if is_ahx { 4 } else { 5 };
        let mut inert = Vec::with_capacity(instrument_nr);
        for _ in 1..=instrument_nr {
            if pos + INSTRUMENT_CORE_BYTES > base.len() {
                break;
            }
            inert.push([at(pos + 9), at(pos + 10), at(pos + 11), at(pos + 19) & 0xc0]);
            pos += INSTRUMENT_CORE_BYTES + at(pos + 21) as usize * entry_bytes;
        }
        Ok(Origin {
            blank_first_track: Some(blank_first),
            inert,
        })
    }
}

/// Whether a step is entirely zero.
pub fn is_blank_step(s: &Step) -> bool {
    *s == Step::default()
}

/// Whether every step of a track is blank.
pub fn is_blank_track(t: &[Step]) -> bool {
    t.iter().all(is_blank_step)
}

/// Whether the file leaves track 0 out: the origin's flag, else "it is blank".
pub fn omits_first_track(song: &Song, origin: &Origin) -> bool {
    origin
        .blank_first_track
        .unwrap_or_else(|| song.tracks.first().is_some_and(|t| is_blank_track(t)))
}

/// Bytes of a PList entry in `format`'s layout.
pub fn plist_entry_bytes(format: SongFormat) -> usize {
    crate::format::plist_entry_bytes(format)
}

/// What one instrument takes in the file apart from its name.
pub fn instrument_bytes(ins: &Instrument, format: SongFormat) -> usize {
    INSTRUMENT_CORE_BYTES + plist_entry_bytes(format) * ins.plist.entries.len()
}

/// The size of the file's body: header, subsong list, positions, tracks and
/// instruments, i.e. what the 16-bit string table offset counts. It equals the
/// written file's `name_offset` exactly. `instruments` is what every real
/// instrument (`1..`) takes; index 0 of a song's list is a placeholder.
pub fn used_bytes(song: &Song, origin: &Origin) -> usize {
    let omitted = omits_first_track(song, origin) as usize;
    let shared = 2 * song.subsongs.len() + 2 * song.channels * song.positions.len();
    let instruments: usize = song
        .instruments
        .iter()
        .skip(1)
        .map(|i| instrument_bytes(i, song.format))
        .sum();
    let tracks = song.tracks.len().saturating_sub(omitted);
    match song.format {
        SongFormat::Ahx => {
            AHX_HEADER_BYTES + shared + 3 * song.track_length as usize * tracks + instruments
        }
        SongFormat::Hvl => {
            let mut track_bytes = 0;
            for t in song.tracks.iter().skip(omitted) {
                track_bytes += t
                    .iter()
                    .map(|s| if is_blank_step(s) { 1 } else { 5 })
                    .sum::<usize>();
            }
            HVL_HEADER_BYTES + shared + track_bytes + instruments
        }
    }
}

/// `text` as Latin-1 bytes plus the NUL, or why it cannot be one.
fn push_cstring(out: &mut Vec<u8>, text: &str, what: &str) -> Result<(), WriteError> {
    for c in text.chars() {
        match c as u32 {
            0 => {
                return fail(format!(
                    "{what} contains a NUL, which would end the string early"
                ))
            }
            n if n > 0xff => {
                return fail(format!(
                    "{what} contains a character above U+00FF, which the format cannot store"
                ))
            }
            n => out.push(n as u8),
        }
    }
    out.push(0);
    Ok(())
}

/// `text` as the file can hold it: NUL removed, anything above U+00FF replaced
/// by `?`. The second value says whether it changed.
pub fn to_latin1(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut altered = false;
    for c in text.chars() {
        match c as u32 {
            0 => altered = true,
            n if n > 0xff => {
                out.push('?');
                altered = true;
            }
            _ => out.push(c),
        }
    }
    (out, altered)
}

/// Why `ins` is not an instrument `format` can write, or `None` when it is.
pub fn instrument_problem(ins: &Instrument, format: SongFormat) -> Option<String> {
    let bounded: [(&str, u8, u8); 5] = [
        ("wave_length", ins.wave_length, 7),
        ("filter_lower_limit", ins.filter_lower_limit, 0x7f),
        ("filter_upper_limit", ins.filter_upper_limit, 0x3f),
        ("filter_speed", ins.filter_speed, 0x3f),
        ("vibrato_depth", ins.vibrato_depth, 0x0f),
    ];
    for (field, value, max) in bounded {
        if value > max {
            return Some(format!("{field} is {value}, not 0..{max}"));
        }
    }
    if ins.hard_cut_release_frames > 7 {
        return Some(format!(
            "hard_cut_release_frames is {}, not 0..7",
            ins.hard_cut_release_frames
        ));
    }
    if ins.plist.entries.len() > MAX_PLIST_ROWS {
        return Some(format!("the PList has more than {MAX_PLIST_ROWS} rows"));
    }
    for (row, e) in ins.plist.entries.iter().enumerate() {
        if let Some(p) = plist_entry_problem(e, format) {
            return Some(format!("PList row {row}: {p}"));
        }
    }
    None
}

/// Why `e` cannot be a PList entry of `format`.
pub fn plist_entry_problem(e: &PListEntry, format: SongFormat) -> Option<String> {
    if e.note > 0x3f {
        return Some(format!("note is {}, not 0..63", e.note));
    }
    if e.waveform > 7 {
        return Some(format!("waveform is {}, not 0..7", e.waveform));
    }
    for fx in e.fx {
        if !plist_command_ok(fx, format) {
            return Some(format!(
                "command {fx:X} is not one {} PList can hold",
                if format == SongFormat::Hvl {
                    "an HVL"
                } else {
                    "an AHX"
                }
            ));
        }
    }
    None
}

/// Whether `format`'s PList can hold command `fx`: any 0-F in HVL, 0-5, C and
/// F in AHX.
pub fn plist_command_ok(fx: u8, format: SongFormat) -> bool {
    match format {
        SongFormat::Hvl => fx <= 15,
        SongFormat::Ahx => AHX_PLIST_COMMANDS.contains(&fx),
    }
}

/// `ins` as the engine plays it in a song of `format` and `version`: a
/// version-0 AHX file drops the high nibble of a filter-toggle (PList command
/// 4) parameter when it loads, so that is what is written.
pub fn normalize_for_version(ins: &Instrument, format: SongFormat, version: u8) -> Instrument {
    let mut ins = ins.clone();
    if format == SongFormat::Ahx && version == 0 {
        for e in &mut ins.plist.entries {
            for k in 0..2 {
                if e.fx[k] == 4 {
                    e.fx_param[k] &= 0x0f;
                }
            }
        }
    }
    ins
}

/// The instrument's wire form: its 22-byte core followed by its PList entries
/// in `format`'s layout, no name. The inverse of
/// [`format::parse_instrument`](crate::format::parse_instrument); the three core
/// bytes the loader skips are zero. `version` is the song's, see
/// [`normalize_for_version`].
pub fn write_instrument(
    ins: &Instrument,
    format: SongFormat,
    version: u8,
) -> Result<Vec<u8>, WriteError> {
    if let Some(p) = instrument_problem(ins, format) {
        return fail(format!("instrument: {p}"));
    }
    let ins = normalize_for_version(ins, format, version);
    let mut out = vec![0u8; instrument_bytes(&ins, format)];
    let e = &ins.envelope;
    out[0] = ins.volume;
    out[1] = (ins.wave_length & 7) | ((ins.filter_speed & 0x1f) << 3);
    out[2..9].copy_from_slice(&[
        e.a_frames, e.a_volume, e.d_frames, e.d_volume, e.s_frames, e.r_frames, e.r_volume,
    ]);
    // Bytes 9..=11 are not read by the loader.
    out[12] = ins.filter_lower_limit | ((ins.filter_speed & 0x20) << 2);
    out[13] = ins.vibrato_delay;
    out[14] = ((ins.hard_cut_release as u8) << 7)
        | (ins.hard_cut_release_frames << 4)
        | ins.vibrato_depth;
    out[15] = ins.vibrato_speed;
    out[16] = ins.square_lower_limit;
    out[17] = ins.square_upper_limit;
    out[18] = ins.square_speed;
    out[19] = ins.filter_upper_limit;
    out[20] = ins.plist.speed;
    out[21] = ins.plist.entries.len() as u8;
    // The inverse of the loader's 3-bit remap (6 -> 12, 7 -> 15).
    let code = |fx: u8| match fx {
        12 => 6,
        15 => 7,
        n => n,
    };
    let width = plist_entry_bytes(format);
    for (row, entry) in ins.plist.entries.iter().enumerate() {
        let at = INSTRUMENT_CORE_BYTES + row * width;
        let fixed = entry.fixed as u8;
        match format {
            SongFormat::Hvl => {
                out[at] = entry.fx[0];
                out[at + 1] = entry.waveform | (entry.fx[1] << 3);
                out[at + 2] = (fixed << 6) | entry.note;
                out[at + 3] = entry.fx_param[0];
                out[at + 4] = entry.fx_param[1];
            }
            SongFormat::Ahx => {
                out[at] =
                    (code(entry.fx[1]) << 5) | (code(entry.fx[0]) << 2) | (entry.waveform >> 1);
                out[at + 1] = ((entry.waveform & 1) << 7) | (fixed << 6) | entry.note;
                out[at + 2] = entry.fx_param[0];
                out[at + 3] = entry.fx_param[1];
            }
        }
    }
    Ok(out)
}

fn write_step(out: &mut Vec<u8>, s: &Step, format: SongFormat, at: &str) -> Result<(), WriteError> {
    if s.fx > 15 || s.fxb > 15 {
        return fail(format!("{at} has an effect above F"));
    }
    match format {
        SongFormat::Ahx => {
            if s.note > 63 {
                return fail(format!("{at} note must be in 0..63 (got {})", s.note));
            }
            if s.instrument > 63 {
                return fail(format!(
                    "{at} instrument must be in 0..63 (got {})",
                    s.instrument
                ));
            }
            if s.fxb != 0 || s.fxb_param != 0 {
                return fail(format!(
                    "{at} has a second effect column, which AHX has no room for"
                ));
            }
            out.extend([
                (s.note << 2) | (s.instrument >> 4),
                ((s.instrument & 15) << 4) | s.fx,
                s.fx_param,
            ]);
        }
        SongFormat::Hvl => {
            if is_blank_step(s) {
                out.push(HVL_BLANK_STEP);
            } else {
                if s.note == HVL_BLANK_STEP {
                    return fail(format!(
                        "{at} has note 63 (0x3f), which HVL reserves for a blank step"
                    ));
                }
                out.extend([
                    s.note,
                    s.instrument,
                    (s.fx << 4) | s.fxb,
                    s.fx_param,
                    s.fxb_param,
                ]);
            }
        }
    }
    Ok(())
}

/// `song` as an AHX or HVL file. `origin` fills what the model cannot carry
/// (see the module docs; `&Origin::default()` guesses). An error, and no
/// bytes, for anything the format cannot hold.
pub fn write_with(song: &Song, origin: &Origin) -> Result<Vec<u8>, WriteError> {
    let format = song.format;
    let ahx = format == SongFormat::Ahx;
    let version_max = if ahx { 2 } else { 1 };
    if song.version > version_max {
        return fail(format!(
            "version must be an integer in 0..{version_max} (got {})",
            song.version
        ));
    }
    if !(1..=4).contains(&song.speed_multiplier) {
        return fail(format!(
            "speed_multiplier must be an integer in 1..4 (got {})",
            song.speed_multiplier
        ));
    }
    let position_nr = song.positions.len();
    if song.position_nr != position_nr {
        return fail(format!(
            "position_nr is {} but the song has {position_nr} positions",
            song.position_nr
        ));
    }
    if position_nr > MAX_POSITIONS {
        return fail(format!(
            "position_nr must be in 0..{MAX_POSITIONS} (got {position_nr})"
        ));
    }
    if song.track_length as usize > MAX_TRACK_LENGTH {
        return fail(format!(
            "track_length must be in 0..{MAX_TRACK_LENGTH} (got {})",
            song.track_length
        ));
    }
    if song.tracks.is_empty() || song.tracks.len() > MAX_TRACKS {
        return fail(format!(
            "a song has 1..{MAX_TRACKS} tracks (got {})",
            song.tracks.len()
        ));
    }
    if song.track_nr as usize != song.tracks.len() - 1 {
        return fail(format!(
            "track_nr is {} ({} tracks) but the song has {} tracks",
            song.track_nr,
            song.track_nr as usize + 1,
            song.tracks.len()
        ));
    }
    if song.instruments.is_empty() || song.instruments.len() > 65 {
        return fail(format!(
            "the song has {} instrument slots (index 0 is a placeholder, at most 64 instruments)",
            song.instruments.len()
        ));
    }
    let instrument_nr = song.instruments.len() - 1;
    if song.instrument_nr as usize != instrument_nr {
        return fail(format!(
            "instrument_nr is {} but the song has {} instrument slots (index 0 is a placeholder)",
            song.instrument_nr,
            song.instruments.len()
        ));
    }
    if song.subsongs.len() > MAX_SUBSONGS || song.subsong_nr as usize != song.subsongs.len() {
        return fail(format!(
            "subsong_nr is {} but the song has {} subsong starts",
            song.subsong_nr,
            song.subsongs.len()
        ));
    }
    let channels = song.channels;
    if ahx && channels != 4 {
        return fail(format!("AHX has exactly 4 channels (got {channels})"));
    }
    if !ahx && !(4..=HVL_HEADER_MAX_CHANNELS).contains(&channels) {
        return fail(format!(
            "channels must be in 4..{HVL_HEADER_MAX_CHANNELS} (got {channels})"
        ));
    }
    let restart_max = if ahx { 0xffff } else { HVL_RESTART_MAX };
    if song.restart as usize > restart_max {
        return fail(format!(
            "restart must be in 0..{restart_max} (got {})",
            song.restart
        ));
    }
    if ahx && (song.mixgain_raw.is_some() || song.defstereo.is_some()) {
        return fail("mixgain_raw/defstereo are HVL fields; AHX has no room for them");
    }
    if !ahx && (song.mixgain_raw.is_none() || song.defstereo.is_none()) {
        return fail("an HVL song needs mixgain_raw and defstereo");
    }

    let track0_blank = song.tracks.first().is_some_and(|t| is_blank_track(t));
    let blank_first = origin.blank_first_track.unwrap_or(track0_blank);
    if blank_first && !track0_blank {
        return fail(
            "the blank-first-track flag is set but track 0 has content, which the file would drop",
        );
    }

    let mut body: Vec<u8> = Vec::new();
    for &start in &song.subsongs {
        body.extend(start.to_be_bytes());
    }
    for (index, p) in song.positions.iter().enumerate() {
        if p.track.len() != channels || p.transpose.len() != channels {
            return fail(format!(
                "position {index} does not have exactly {channels} channels of track and transpose"
            ));
        }
        for ch in 0..channels {
            body.push(p.track[ch]);
            body.push(p.transpose[ch] as u8);
        }
    }
    for (index, track) in song.tracks.iter().enumerate() {
        if track.len() != song.track_length as usize {
            return fail(format!(
                "track {index} does not have track_length ({}) steps",
                song.track_length
            ));
        }
        if blank_first && index == 0 {
            continue;
        }
        for (row, step) in track.iter().enumerate() {
            write_step(&mut body, step, format, &format!("track {index} row {row}"))?;
        }
    }

    let mut names: Vec<u8> = Vec::new();
    push_cstring(&mut names, &song.name, "song name")?;
    for n in 1..=instrument_nr {
        let ins = &song.instruments[n];
        if let Some(p) = instrument_problem(ins, format) {
            return fail(format!("instrument {n}: {p}"));
        }
        push_cstring(&mut names, &ins.name, &format!("instrument {n} name"))?;
        let mut wire = write_instrument(ins, format, song.version)?;
        if let Some(inert) = origin.inert.get(n - 1) {
            // The inert bits the model does not carry, copied from the file.
            wire[9..12].copy_from_slice(&inert[..3]);
            wire[19] |= inert[3] & 0xc0;
        }
        body.extend(wire);
    }

    let header = if ahx {
        AHX_HEADER_BYTES
    } else {
        HVL_HEADER_BYTES
    };
    let name_offset = header + body.len();
    if name_offset > SIZE_LIMIT {
        return fail(format!(
            "song too large: the string table would start at byte {name_offset}, past the 16-bit limit of {SIZE_LIMIT}"
        ));
    }

    let mut out = vec![0u8; name_offset + names.len()];
    out[..3].copy_from_slice(if ahx { b"THX" } else { b"HVL" });
    out[3] = song.version;
    out[4] = (name_offset >> 8) as u8;
    out[5] = name_offset as u8;
    out[6] =
        ((blank_first as u8) << 7) | ((song.speed_multiplier - 1) << 5) | (position_nr >> 8) as u8;
    out[7] = position_nr as u8;
    if ahx {
        out[8] = (song.restart >> 8) as u8;
        out[9] = song.restart as u8;
    } else {
        out[8] = (((channels - 4) as u8) << 2) | (song.restart >> 8) as u8;
        out[9] = song.restart as u8;
    }
    out[10] = song.track_length;
    out[11] = song.track_nr;
    out[12] = song.instrument_nr;
    out[13] = song.subsong_nr;
    if !ahx {
        out[14] = song.mixgain_raw.unwrap_or(0);
        out[15] = song.defstereo.unwrap_or(0);
    }
    out[header..header + body.len()].copy_from_slice(&body);
    out[name_offset..].copy_from_slice(&names);
    Ok(out)
}

/// [`write_with`] for a song parsed from `base` (the bytes it came from), which
/// supplies what the model cannot carry; with `None` the writer guesses.
pub fn write(song: &Song, base: Option<&[u8]>) -> Result<Vec<u8>, WriteError> {
    let origin = match base {
        Some(b) => Origin::from_bytes(b)?,
        None => Origin::default(),
    };
    write_with(song, &origin)
}

/// An HVL song as an AHX song when the tune fits: only channels 0..3 carry data
/// (a channel above them that holds nothing but blank tracks is dropped), no
/// second effect column, no note or instrument above 63, only instruments AHX
/// can write, and no `EF1` in a version-1 HVL song (the engine reads it there
/// and never in AHX, so the tune would sound different). The mix gain and
/// default stereo have no AHX field and are dropped. The result is written once
/// to prove it holds. Whenever this is unsure it refuses, in a plain reason
/// (another_synth's `convertHvlToAhx`, which this follows).
pub fn convert_hvl_to_ahx(song: &Song) -> Result<Song, String> {
    const INSTEAD: &str = "Export it as HVL instead.";
    if song.format != SongFormat::Hvl {
        return Err("Only HVL songs can be converted.".into());
    }
    let blank: Vec<bool> = song.tracks.iter().map(|t| is_blank_track(t)).collect();
    let is_blank = |i: usize| blank.get(i).copied().unwrap_or(true);
    let mut width = 0;
    for p in &song.positions {
        for (ch, &t) in p.track.iter().enumerate() {
            if !is_blank(t as usize) {
                width = width.max(ch + 1);
            }
        }
    }
    if width > 4 {
        return Err(format!(
            "AHX files have 4 tracks; this song reaches track {width}. {INSTEAD}"
        ));
    }
    let positions: Vec<_> = song
        .positions
        .iter()
        .map(|p| crate::format::Position {
            track: p.track[..4].to_vec(),
            transpose: p.transpose[..4].to_vec(),
        })
        .collect();
    let mut kept: Vec<u8> = positions.iter().flat_map(|p| p.track.clone()).collect();
    kept.sort_unstable();
    kept.dedup();
    for t in kept {
        for s in song.tracks.get(t as usize).into_iter().flatten() {
            if s.fxb != 0 || s.fxb_param != 0 {
                return Err(format!(
                    "This song uses a second effect column, which AHX files don't have. {INSTEAD}"
                ));
            }
            if s.note > 63 || s.instrument > 63 {
                return Err(format!(
                    "This song has notes or instruments AHX files can't hold. {INSTEAD}"
                ));
            }
            if song.version >= 1 && s.fx == 0xe && s.fx_param == 0xf1 {
                return Err(format!(
                    "This song uses an effect (EF1) that only HVL plays. {INSTEAD}"
                ));
            }
        }
    }
    for n in 1..song.instruments.len() {
        if instrument_problem(&song.instruments[n], SongFormat::Ahx).is_some() {
            return Err(format!(
                "Instrument {n} uses settings AHX files don't have. {INSTEAD}"
            ));
        }
    }
    let mut converted = song.clone();
    converted.format = SongFormat::Ahx;
    // The engine plays every AHX file the same; this version keeps filter
    // toggles whole.
    converted.version = 1;
    converted.channels = 4;
    converted.positions = positions;
    converted.mixgain_raw = None;
    converted.defstereo = None;
    // The checks above only read the tracks a kept channel plays; the writer
    // writes them all.
    if let Err(e) = write_with(&converted, &Origin::default()) {
        return Err(format!(
            "AHX files can't hold this song: {}. {INSTEAD}",
            e.0
        ));
    }
    Ok(converted)
}

/// The most channels the engine plays (`format::MAX_CHANNELS`); a document
/// refuses wider songs.
pub const ENGINE_MAX_CHANNELS: usize = MAX_CHANNELS;
