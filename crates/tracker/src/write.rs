//! Writers: a [`Module`] back to the bytes of its format.
//!
//! [`write_mod`], [`write_s3m`] and [`write_xm`] are the inverses of the
//! parsers: for any module a parser produced, parsing what the writer wrote
//! gives the same module. They never drop what the format cannot hold: a note
//! outside a MOD's periods, a loop kind the format lacks, more channels than
//! the layout takes, a pattern of the wrong length are errors that say where.
//!
//! What a writer chooses where the model is finer than the file (the file
//! stores less than the module holds, or more than the module says):
//!
//! - Samples are written at the narrowest width that keeps every value exactly
//!   (S3M, XM: 8-bit when the data sits on the 8-bit grid, else 16-bit; a MOD
//!   is always 8-bit and its data must be on that grid, which parsing
//!   guarantees and the edit layer keeps).
//! - Text fields are Latin-1, cut to the field's length (MOD 20/22, S3M 28,
//!   XM 20/22); a character outside Latin-1 is written as `?`.
//! - MOD: the signature is `M.K.` for a ProTracker-limited 4-channel module,
//!   `N.T.` for NoiseTracker's, `4CHN`, `6CHN`, `8CHN`, `10CH` and so on
//!   otherwise (the channel count lives in it); the Soundtracker flavours are
//!   written in their own 15-sample layout. The restart byte is 0x7F. The
//!   header has no speed or tempo (they are `Fxx` effects in the song), no
//!   pans and no flags: those are re-derived when the file is loaded again.
//! - S3M: the file is written with unsigned samples (format 2), the header's
//!   channel settings, pan table and master volume are rebuilt from the
//!   module's channel pans, mutes and AdLib channels, and its order list is
//!   rebuilt from `orders` and `jump_targets` (separators come back as 254,
//!   the list ends with 255).
//! - XM: version 1.04, tracker name `FastTracker v2.00`, patterns packed as
//!   the DAW packs them (a 0x80 flag byte then the present fields).
//!
//! [`Module::differences`] compares a module with another, which is how a
//! writer's output is checked: load the bytes and look at what differs.

use crate::module::{
    Cell, Envelope, Format, Instrument, LoopKind, ModFlavor, Module, Note, Sample,
};
use crate::pitch::{MOD_EXTENDED_PERIODS, PT_PERIODS};

type Res<T> = Result<T, String>;

/// Writes a module in its own format.
pub fn write(module: &Module) -> Res<Vec<u8>> {
    match module.format {
        Format::Mod => write_mod(module),
        Format::S3m => write_s3m(module),
        Format::Xm => write_xm(module),
    }
}

// ---------------------------------------------------------------------------
// shared pieces

/// A fixed-width text field: Latin-1, NUL padded (or `pad`), cut to `len`.
fn text_field(out: &mut Vec<u8>, text: &str, len: usize, pad: u8) {
    let mut written = 0;
    for ch in text.chars().take(len) {
        let code = ch as u32;
        out.push(if code == 0 {
            b' '
        } else if code <= 0xff {
            code as u8
        } else {
            b'?'
        });
        written += 1;
    }
    out.extend(std::iter::repeat_n(pad, len - written));
}

fn put_u16(out: &mut Vec<u8>, v: usize) {
    out.extend_from_slice(&(v as u16).to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: usize) {
    out.extend_from_slice(&(v as u32).to_le_bytes());
}

fn pad_to(out: &mut Vec<u8>, align: usize) {
    while !out.len().is_multiple_of(align) {
        out.push(0);
    }
}

/// Whether `value` is exactly an 8-bit sample value.
fn on_8bit_grid(value: f32) -> bool {
    let k = value * 128.0;
    k == k.round() && (-128.0..=127.0).contains(&k)
}

fn quantize(value: f32, scale: f32) -> i32 {
    (value * scale).round().clamp(-scale, scale - 1.0) as i32
}

fn where_cell(pattern: usize, row: usize, channel: usize) -> String {
    format!("pattern {pattern}, row {row}, channel {}", channel + 1)
}

/// Checks that the patterns, orders and channels have the shape every format
/// needs, with the row counts `rows` allows.
fn check_structure(m: &Module, rows: impl Fn(usize) -> bool, rows_text: &str) -> Res<()> {
    if m.channels == 0 {
        return Err("a module needs at least one channel".into());
    }
    if m.patterns.is_empty() {
        return Err("a module needs at least one pattern".into());
    }
    if m.orders.is_empty() {
        return Err("a module needs at least one order".into());
    }
    for (i, p) in m.patterns.iter().enumerate() {
        if p.channels() != m.channels {
            return Err(format!(
                "pattern {i} has {} channels, the module {}",
                p.channels(),
                m.channels
            ));
        }
        if !rows(p.rows()) {
            return Err(format!("pattern {i} has {} rows; {rows_text}", p.rows()));
        }
    }
    for (i, &o) in m.orders.iter().enumerate() {
        if o >= m.patterns.len() {
            return Err(format!(
                "order {i} plays pattern {o}, which does not exist (the module has {})",
                m.patterns.len()
            ));
        }
    }
    if m.channel_pan.len() != m.channels
        || m.muted.len() != m.channels
        || m.adlib_channels.len() != m.channels
    {
        return Err("the per-channel lists do not match the channel count".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MOD

/// The period a MOD cell stores for its note: the cell's own when it has
/// one, else ProTracker's table (the PC trackers' seven octaves when the
/// module is not limited to ProTracker's three).
pub(crate) fn mod_period(cell: &Cell, amiga_limits: bool) -> Option<u16> {
    if cell.period != 0 {
        return Some(cell.period);
    }
    match cell.note {
        Note::On(n) => mod_period_for_note(n, amiga_limits),
        _ => None,
    }
}

/// The table period of a MIDI note (C-1 is 24), if the table has one.
pub(crate) fn mod_period_for_note(note: u8, amiga_limits: bool) -> Option<u16> {
    let n = note as usize;
    if amiga_limits {
        (24..60).contains(&n).then(|| PT_PERIODS[n - 24])
    } else {
        MOD_EXTENDED_PERIODS.get(n).copied()
    }
}

/// A MOD cell's signature for its layout: `M.K.` and friends by channel count.
fn mod_signature(m: &Module) -> [u8; 4] {
    let flavor = m.mod_flavor.unwrap_or(ModFlavor::ProTracker);
    if m.channels == 4 {
        if flavor == ModFlavor::NoiseTracker {
            return *b"N.T.";
        }
        if m.flags.amiga_limits {
            return *b"M.K.";
        }
        return *b"4CHN";
    }
    let text = if m.channels < 10 {
        format!("{}CHN", m.channels)
    } else {
        format!("{}CH", m.channels)
    };
    let mut sig = *b"    ";
    for (d, s) in sig.iter_mut().zip(text.bytes()) {
        *d = s;
    }
    // "10CH" is four bytes; "1CHN" too; both fit.
    sig
}

/// An Ultimate Soundtracker command from ProTracker's numbering (the inverse
/// of the loader's translation).
fn ust_effect_inverse(effect: u8, param: u8) -> Res<(u8, u8)> {
    match effect {
        0 if param != 0 => Ok((1, param)),
        0 => Ok((0, 0)),
        1 if param <= 0x0f => Ok((2, param)),
        2 if param <= 0x0f => Ok((2, param << 4)),
        1 | 2 => Err("Ultimate Soundtracker's pitch bend takes one nibble (0-F)".into()),
        1..=0x0f => Ok((effect, param)),
        _ => Err("effects are 0-F".into()),
    }
}

/// Writes a MOD (see the module docs for what is chosen).
pub fn write_mod(m: &Module) -> Res<Vec<u8>> {
    if m.format != Format::Mod {
        return Err("not a MOD module".into());
    }
    let flavor = m.mod_flavor.unwrap_or(ModFlavor::ProTracker);
    let layout31 = matches!(flavor, ModFlavor::ProTracker | ModFlavor::NoiseTracker);
    let ust = flavor == ModFlavor::UltimateSoundtracker;
    let (sample_slots, header_size, max_patterns) = if layout31 {
        (31, 1084, 128)
    } else {
        (15, 600, 64)
    };
    if m.channels > 32 {
        return Err(format!("a MOD holds 1-32 channels, not {}", m.channels));
    }
    if !layout31 && m.channels != 4 {
        return Err("a Soundtracker module has four channels".into());
    }
    check_structure(m, |r| r == 64, "a MOD pattern always has 64 rows")?;
    if m.patterns.len() > max_patterns {
        return Err(format!(
            "this MOD layout holds {max_patterns} patterns, not {}",
            m.patterns.len()
        ));
    }
    if m.orders.len() > 128 {
        return Err(format!(
            "a MOD holds 128 order positions, not {}",
            m.orders.len()
        ));
    }
    if m.samples.len() > sample_slots {
        return Err(format!(
            "this MOD layout holds {sample_slots} samples, not {}",
            m.samples.len()
        ));
    }
    // The pattern count in the file is the highest number the order table
    // names; a pattern past the last one played needs an entry just beyond
    // the song's end to be kept.
    let highest = m.orders.iter().copied().max().unwrap_or(0);
    let mut table = [0u8; 128];
    for (slot, &o) in table.iter_mut().zip(&m.orders) {
        *slot = o as u8;
    }
    if m.patterns.len() - 1 > highest {
        if m.orders.len() >= 128 {
            return Err(format!(
                "pattern {} is not in the order list and the list is full: use it or delete it",
                m.patterns.len() - 1
            ));
        }
        table[m.orders.len()] = (m.patterns.len() - 1) as u8;
    }

    let mut out = Vec::with_capacity(header_size + m.patterns.len() * 64 * m.channels * 4);
    text_field(&mut out, &m.title, 20, 0);
    for slot in 0..sample_slots {
        let empty = Sample::default();
        let s = m.samples.get(slot).unwrap_or(&empty);
        let n = slot + 1;
        if s.data.len() % 2 != 0 {
            return Err(format!(
                "sample {n} has an odd number of frames ({}); a MOD stores whole words",
                s.data.len()
            ));
        }
        if s.data.len() > 0x1fffe {
            return Err(format!(
                "sample {n} is {} frames; a MOD holds at most 131070",
                s.data.len()
            ));
        }
        if s.volume > 64 {
            return Err(format!("sample {n}: volume {} is over 64", s.volume));
        }
        if !(-8..=7).contains(&s.finetune) {
            return Err(format!(
                "sample {n}: finetune {} is not -8 to 7",
                s.finetune
            ));
        }
        if s.adlib.is_some() {
            return Err(format!("sample {n} is an AdLib instrument; a MOD has none"));
        }
        if !s.data.iter().all(|&v| on_8bit_grid(v)) {
            return Err(format!(
                "sample {n} has values off the 8-bit grid; load or convert it as 8-bit first"
            ));
        }
        let (loop_start, loop_len) = match s.loop_kind {
            LoopKind::Off => (0, 2),
            LoopKind::PingPong => {
                return Err(format!("sample {n}: a MOD has no ping-pong loops"));
            }
            LoopKind::Forward => {
                if s.loop_len <= 2 {
                    return Err(format!(
                        "sample {n}: a loop of 2 frames or less is no loop in a MOD"
                    ));
                }
                if !ust && (s.loop_start % 2 != 0 || s.loop_len % 2 != 0) {
                    return Err(format!(
                        "sample {n}: a MOD stores loops in whole words (even start and length)"
                    ));
                }
                if s.loop_len % 2 != 0 {
                    return Err(format!("sample {n}: loop length must be even"));
                }
                (s.loop_start, s.loop_len)
            }
        };
        text_field(&mut out, &s.name, 22, 0);
        out.extend_from_slice(&((s.data.len() / 2) as u16).to_be_bytes());
        out.push((s.finetune as u8) & 0x0f);
        out.push(s.volume);
        // Ultimate Soundtracker counts the loop start in bytes.
        let start_word = if ust { loop_start } else { loop_start / 2 };
        if start_word > 0xffff || loop_len / 2 > 0xffff {
            return Err(format!("sample {n}: the loop does not fit a MOD header"));
        }
        out.extend_from_slice(&(start_word as u16).to_be_bytes());
        out.extend_from_slice(&((loop_len / 2) as u16).to_be_bytes());
    }
    out.push(m.orders.len() as u8);
    out.push(if layout31 { 0x7f } else { 0x78 });
    out.extend_from_slice(&table);
    if layout31 {
        out.extend_from_slice(&mod_signature(m));
    }
    debug_assert_eq!(out.len(), header_size);

    for (p, pattern) in m.patterns.iter().enumerate() {
        for row in 0..64 {
            for ch in 0..m.channels {
                let cell = pattern.cell(row, ch);
                let at = where_cell(p, row, ch);
                if cell.note == Note::Off {
                    return Err(format!(
                        "{at}: a MOD has no key-off; use a note cut (EC0) instead"
                    ));
                }
                if cell.volume != 0 {
                    return Err(format!("{at}: a MOD has no volume column; use effect C"));
                }
                if cell.effect > 0x0f {
                    return Err(format!(
                        "{at}: effect {:#x} is not a MOD effect (0-F)",
                        cell.effect
                    ));
                }
                let period = mod_period(cell, m.flags.amiga_limits).unwrap_or(0);
                if matches!(cell.note, Note::On(_)) && period == 0 {
                    return Err(format!(
                        "{at}: the note has no period (MOD notes run C-1 to B-3{})",
                        if m.flags.amiga_limits {
                            ""
                        } else {
                            ", or the PC trackers' seven octaves"
                        }
                    ));
                }
                if period > 0x0fff {
                    return Err(format!("{at}: period {period} does not fit 12 bits"));
                }
                if m.flags.amiga_limits && period != 0 && !(113..=856).contains(&period) {
                    return Err(format!(
                        "{at}: period {period} is outside ProTracker's C-1 to B-3, and the module is limited to them"
                    ));
                }
                let (effect, param) = if ust {
                    ust_effect_inverse(cell.effect, cell.param).map_err(|e| format!("{at}: {e}"))?
                } else {
                    (cell.effect, cell.param)
                };
                out.push((cell.instrument & 0xf0) | (period >> 8) as u8);
                out.push(period as u8);
                out.push(((cell.instrument & 0x0f) << 4) | effect);
                out.push(param);
            }
        }
    }
    for s in m.samples.iter().take(sample_slots) {
        out.extend(s.data.iter().map(|&v| quantize(v, 128.0) as i8 as u8));
    }
    // The 15-sample layouts have no signature: Soundtracker and Ultimate
    // Soundtracker (whose loops and commands differ) are told apart by
    // looking at the data, and an edit can make the file look like the other.
    if !layout31 {
        let back = crate::protracker::parse(&out)
            .map_err(|e| format!("the Soundtracker file would not load back: {e}"))?;
        if back.mod_flavor != m.mod_flavor {
            return Err(format!(
                "this {} module would load back as {}: the 15-sample layout cannot tell them apart once the loops or effects change; convert it to ProTracker first (set_mod_flavor)",
                flavor_name(flavor),
                flavor_name(back.mod_flavor.unwrap_or(ModFlavor::ProTracker))
            ));
        }
    }
    Ok(out)
}

fn flavor_name(f: ModFlavor) -> &'static str {
    match f {
        ModFlavor::ProTracker => "ProTracker",
        ModFlavor::NoiseTracker => "NoiseTracker",
        ModFlavor::Soundtracker => "Soundtracker",
        ModFlavor::UltimateSoundtracker => "Ultimate Soundtracker",
    }
}

// ---------------------------------------------------------------------------
// S3M

/// The loader's pan (0-1) for a table nibble, the way it computes it.
pub(crate) fn s3m_nibble_pan(nibble: u8) -> f32 {
    let raw = (nibble as f32 * 256.0 + 8.0) / 15.0;
    ((raw / 255.0).min(1.0) * 255.0).round() / 255.0
}

/// The pan table nibble nearest to a pan.
pub(crate) fn s3m_pan_nibble(pan: f32) -> u8 {
    (0..16u8)
        .min_by(|&a, &b| {
            let da = (s3m_nibble_pan(a) - pan).abs();
            let db = (s3m_nibble_pan(b) - pan).abs();
            da.total_cmp(&db)
        })
        .unwrap_or(8)
}

const S3M_LEFT_DEFAULT: f32 = 51.0 / 255.0;
const S3M_RIGHT_DEFAULT: f32 = 204.0 / 255.0;

/// How a PCM channel's pan reaches the file.
#[derive(Clone, Copy, PartialEq)]
enum PanClass {
    /// The default of ST3's stereo mode for the setting's side.
    Default(bool),
    /// An entry of the pan table.
    Table(u8),
    /// No pan stored (mono).
    Mono,
}

/// The order list as the file holds it: `orders` with the separators the
/// loader's `jump_targets` remembers.
fn s3m_raw_orders(m: &Module) -> Res<Vec<u8>> {
    let numbers: Vec<u8> = m
        .orders
        .iter()
        .map(|&o| {
            if o > 253 {
                Err(format!(
                    "order plays pattern {o}; S3M patterns number 0-253"
                ))
            } else {
                Ok(o as u8)
            }
        })
        .collect::<Res<_>>()?;
    let Some(map) = &m.jump_targets else {
        return Ok(numbers);
    };
    let n = map.len();
    let total = numbers.len();
    let mut raw = Vec::with_capacity(n);
    for i in 0..n {
        let before = map[i];
        let playable = if i + 1 < n {
            map[i + 1] == before + 1
        } else {
            total == before + 1
        };
        if i + 1 < n && map[i + 1] != before && map[i + 1] != before + 1 {
            return Err("the jump targets do not follow the order list".into());
        }
        if playable {
            raw.push(
                *numbers
                    .get(before)
                    .ok_or("the jump targets do not follow the order list")?,
            );
        } else {
            raw.push(254);
        }
    }
    if raw.iter().filter(|&&o| o != 254).count() != total {
        return Err("the jump targets do not follow the order list".into());
    }
    Ok(raw)
}

/// The header's master volume byte that gives `gain` (the AdLib level), with
/// the stereo bit as asked where several bytes would.
fn s3m_master_volume(gain: f32, stereo: bool) -> u8 {
    let stereo_bit = if stereo { 0x80 } else { 0 };
    let mut best = (f32::INFINITY, 0x30 | stereo_bit);
    for low in 0..=0x7fu8 {
        let mv = low | stereo_bit;
        let g = crate::s3m::adlib_gain(mv, 2);
        let d = (g - gain).abs();
        if d < best.0 {
            best = (d, mv);
        }
        if d == 0.0 {
            break;
        }
    }
    best.1
}

fn s3m_sample_bytes(s: &Sample) -> (bool, Vec<u8>) {
    let wide = !s.data.iter().all(|&v| on_8bit_grid(v));
    let bytes = if wide {
        s.data
            .iter()
            .flat_map(|&v| (quantize(v, 32768.0) as i16).to_le_bytes())
            .collect()
    } else {
        s.data
            .iter()
            .map(|&v| (quantize(v, 128.0) + 128).clamp(0, 255) as u8)
            .collect()
    };
    (wide, bytes)
}

/// Writes an S3M (see the module docs for what is chosen).
pub fn write_s3m(m: &Module) -> Res<Vec<u8>> {
    if m.format != Format::S3m {
        return Err("not an S3M module".into());
    }
    if m.channels > 32 {
        return Err(format!("an S3M holds 1-32 channels, not {}", m.channels));
    }
    check_structure(m, |r| r == 64, "an S3M pattern always has 64 rows")?;
    if m.patterns.len() > 254 {
        return Err(format!(
            "an S3M holds 254 patterns, not {}",
            m.patterns.len()
        ));
    }
    if m.samples.len() > 255 {
        return Err(format!(
            "an S3M holds 99 instruments (255 at most), not {}",
            m.samples.len()
        ));
    }
    if !(1..=254).contains(&m.speed) {
        return Err(format!("the initial speed {} is not 1-254", m.speed));
    }
    if m.tempo < 33 {
        return Err(format!("the initial tempo {} is not 33-255", m.tempo));
    }
    let version = if m.tracker_version == 0 {
        0x1320
    } else {
        m.tracker_version
    };
    if version == 0x1300 && !m.flags.fast_volume_slides {
        return Err(
            "ST3.00 modules always slide volume on every tick: set tracker_version or the flag"
                .into(),
        );
    }
    let mut raw_orders = s3m_raw_orders(m)?;
    if raw_orders.len() > 255 {
        return Err(format!(
            "an S3M order list holds 255 entries, not {}",
            raw_orders.len()
        ));
    }
    raw_orders.push(255);
    if raw_orders.len() % 2 != 0 {
        raw_orders.push(255);
    }

    // Channel settings, pans.
    let mut classes = vec![PanClass::Mono; m.channels];
    for c in 0..m.channels {
        if m.adlib_channels[c].is_some() {
            continue;
        }
        classes[c] = match m.channel_pan[c] {
            None => PanClass::Mono,
            Some(p) if p == S3M_LEFT_DEFAULT => PanClass::Default(false),
            Some(p) if p == S3M_RIGHT_DEFAULT => PanClass::Default(true),
            Some(p) => PanClass::Table(s3m_pan_nibble(p)),
        };
    }
    let any_mono =
        (0..m.channels).any(|c| m.adlib_channels[c].is_none() && classes[c] == PanClass::Mono);
    if any_mono {
        // Defaults need the stereo bit, which would turn the mono channels
        // into pans: write what is nearest as table entries.
        for c in 0..m.channels {
            if let PanClass::Default(right) = classes[c] {
                classes[c] = PanClass::Table(s3m_pan_nibble(if right {
                    S3M_RIGHT_DEFAULT
                } else {
                    S3M_LEFT_DEFAULT
                }));
            }
        }
    }
    let stereo = !any_mono;
    let needs_table = classes.iter().any(|c| matches!(c, PanClass::Table(_)));
    let mut used = [false; 16];
    let mut settings = [0xffu8; 32];
    let mut pcm_order: Vec<usize> = (0..m.channels)
        .filter(|&c| m.adlib_channels[c].is_none())
        .collect();
    // Channels whose side is fixed by their default pan first.
    pcm_order.sort_by_key(|&c| !matches!(classes[c], PanClass::Default(_)));
    for &c in &pcm_order {
        let right = match classes[c] {
            PanClass::Default(r) => r,
            _ => m.channel_pan[c].is_some_and(|p| p >= 0.5),
        };
        let side = if right { 8 } else { 0 };
        let other = 8 - side;
        let free = (0..8)
            .map(|k| side + k)
            .find(|&v| !used[v])
            .or_else(|| {
                if matches!(classes[c], PanClass::Default(_)) {
                    None
                } else {
                    (0..8).map(|k| other + k).find(|&v| !used[v])
                }
            })
            .unwrap_or(side + c % 8);
        used[free] = true;
        settings[c] = free as u8;
    }
    for c in 0..m.channels {
        if let Some(k) = m.adlib_channels[c] {
            if k > 8 {
                return Err(format!("channel {}: AdLib channel {k} is not 0-8", c + 1));
            }
            settings[c] = 16 + k;
        }
        if m.muted[c] {
            settings[c] |= 0x80;
        }
    }

    // ST3 plays channels that share a hardware channel as one: their cells
    // merge when the file is loaded. A channel that was merged away on load
    // is empty and harmless; one with notes would be silently combined.
    if version >> 12 == 1 {
        for c in 0..m.channels {
            if settings[c] >= 0x80 || settings[c] == 0xff {
                continue;
            }
            if let Some(first) = (0..c).find(|&e| settings[e] == settings[c]) {
                let used = m
                    .patterns
                    .iter()
                    .any(|p| (0..p.rows()).any(|r| !p.cell(r, c).is_empty()));
                if used {
                    return Err(format!(
                        "channel {} shares a hardware channel with channel {} (ST3 plays them as one, so their notes would merge): an ST3 module has 8 left and 8 right PCM channels",
                        c + 1,
                        first + 1
                    ));
                }
            }
        }
    }

    // Layout.
    let n_inst = m.samples.len();
    let n_pat = m.patterns.len();
    let header_end =
        0x60 + raw_orders.len() + 2 * n_inst + 2 * n_pat + if needs_table { 32 } else { 0 };
    let inst_at = header_end.div_ceil(16) * 16;
    let patterns_at = inst_at + 80 * n_inst;

    // Patterns, packed.
    let mut packed: Vec<Vec<u8>> = Vec::with_capacity(n_pat);
    for (p, pattern) in m.patterns.iter().enumerate() {
        let mut data = Vec::new();
        for row in 0..64 {
            for ch in 0..m.channels {
                let cell = pattern.cell(row, ch);
                let at = where_cell(p, row, ch);
                let note_byte = match cell.note {
                    Note::None => 0xffu8,
                    Note::Off => 0xfe,
                    Note::On(n) => {
                        if n < 24 || (n - 24) / 12 > 15 {
                            return Err(format!("{at}: note {n} is outside an S3M's C-0 to B-7"));
                        }
                        let k = n - 24;
                        ((k / 12) << 4) | (k % 12)
                    }
                };
                if cell.period != 0 {
                    return Err(format!("{at}: an S3M cell has no period"));
                }
                if cell.volume != 0 && !(0x10..=0x50).contains(&cell.volume) {
                    return Err(format!(
                        "{at}: volume column value {:#x} is not 0x10-0x50",
                        cell.volume
                    ));
                }
                if cell.effect == 0 && cell.param != 0 {
                    return Err(format!("{at}: a parameter without an effect"));
                }
                let mut flags = 0u8;
                if cell.note != Note::None || cell.instrument != 0 {
                    flags |= 0x20;
                }
                if cell.volume != 0 {
                    flags |= 0x40;
                }
                if cell.effect != 0 {
                    flags |= 0x80;
                }
                if flags == 0 {
                    continue;
                }
                data.push(flags | ch as u8);
                if flags & 0x20 != 0 {
                    data.push(note_byte);
                    data.push(cell.instrument);
                }
                if flags & 0x40 != 0 {
                    data.push(cell.volume - 0x10);
                }
                if flags & 0x80 != 0 {
                    data.push(cell.effect);
                    data.push(cell.param);
                }
            }
            data.push(0);
        }
        packed.push(data);
    }
    let mut pattern_at = Vec::with_capacity(n_pat);
    let mut at = patterns_at;
    for data in &packed {
        pattern_at.push(at);
        at += (2 + data.len()).div_ceil(16) * 16;
    }
    let data_start = at;
    if data_start / 16 > 0xffff {
        return Err(format!(
            "the instruments and patterns take {} bytes; an S3M's pointers reach 1 MiB",
            data_start
        ));
    }

    // Sample data placement.
    let mut sample_bytes: Vec<(bool, Vec<u8>)> = Vec::with_capacity(n_inst);
    let mut data_at = Vec::with_capacity(n_inst);
    let mut at = data_start;
    for (i, s) in m.samples.iter().enumerate() {
        if s.loop_kind == LoopKind::PingPong {
            return Err(format!("sample {}: an S3M has no ping-pong loops", i + 1));
        }
        if s.c2spd > 0xffff {
            return Err(format!(
                "sample {}: c2spd {} is over 65535 (ST3 reads 16 bits)",
                i + 1,
                s.c2spd
            ));
        }
        if s.adlib.is_none() && !s.data.is_empty() {
            let (wide, bytes) = s3m_sample_bytes(s);
            data_at.push(at);
            at += bytes.len().div_ceil(16) * 16;
            sample_bytes.push((wide, bytes));
        } else {
            data_at.push(0);
            sample_bytes.push((false, Vec::new()));
        }
    }
    if at / 16 > 0xff_ffff {
        return Err("the sample data is too big for an S3M".into());
    }

    // Write.
    let mut out = Vec::with_capacity(at);
    text_field(&mut out, &m.title, 28, 0);
    out.push(0x1a);
    out.push(0x10);
    out.extend_from_slice(&[0, 0]);
    put_u16(&mut out, raw_orders.len());
    put_u16(&mut out, n_inst);
    put_u16(&mut out, n_pat);
    let flags = (if m.flags.amiga_limits { 0x10 } else { 0 })
        | (if m.flags.fast_volume_slides { 0x40 } else { 0 });
    put_u16(&mut out, flags);
    put_u16(&mut out, version as usize);
    put_u16(&mut out, 2);
    out.extend_from_slice(b"SCRM");
    out.push((m.global_volume * 64.0).round().clamp(0.0, 64.0) as u8);
    out.push(m.speed);
    out.push(m.tempo);
    out.push(s3m_master_volume(m.adlib_gain, stereo));
    out.push(0);
    out.push(if needs_table { 0xfc } else { 0 });
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&settings);
    out.extend_from_slice(&raw_orders);
    for i in 0..n_inst {
        put_u16(&mut out, (inst_at + i * 80) / 16);
    }
    for &p in &pattern_at {
        put_u16(&mut out, p / 16);
    }
    if needs_table {
        for c in 0..32 {
            out.push(match classes.get(c) {
                Some(PanClass::Table(n)) if c < m.channels && m.adlib_channels[c].is_none() => {
                    0x20 | n
                }
                _ => 0,
            });
        }
    }
    pad_to(&mut out, 16);
    debug_assert_eq!(out.len(), inst_at);

    for (i, s) in m.samples.iter().enumerate() {
        let start = out.len();
        let wide = sample_bytes[i].0;
        if let Some(adlib) = &s.adlib {
            out.push(if adlib.melodic { 2 } else { 3 });
            out.extend_from_slice(&[0; 12]);
            out.extend_from_slice(&[0; 3]);
            out.extend_from_slice(&adlib.registers);
            out.push(s.volume);
            out.extend_from_slice(&[0, 0, 0]);
            put_u32(&mut out, s.c2spd as usize);
            out.extend_from_slice(&[0; 12]);
        } else if s.data.is_empty() && s.volume == 0 && s.c2spd == 0 {
            out.push(0);
            out.extend_from_slice(&[0; 31]);
            put_u32(&mut out, 0);
            out.extend_from_slice(&[0; 12]);
        } else {
            out.push(1);
            out.extend_from_slice(&[0; 12]);
            let para = data_at[i] / 16;
            out.push((para >> 16) as u8);
            put_u16(&mut out, para & 0xffff);
            put_u32(&mut out, s.data.len());
            let (ls, le) = if s.loop_kind == LoopKind::Forward {
                (s.loop_start, s.loop_start + s.loop_len)
            } else {
                (0, 0)
            };
            put_u32(&mut out, ls);
            put_u32(&mut out, le);
            out.push(s.volume);
            out.push(0);
            out.push(0);
            let mut sample_flags = 0u8;
            if s.loop_kind == LoopKind::Forward {
                sample_flags |= 1;
            }
            if wide {
                sample_flags |= 4;
            }
            out.push(sample_flags);
            put_u32(&mut out, s.c2spd as usize);
            out.extend_from_slice(&[0; 12]);
        }
        text_field(&mut out, &s.name, 28, 0);
        out.extend_from_slice(if s.adlib.is_some() { b"SCRI" } else { b"SCRS" });
        debug_assert_eq!(out.len() - start, 80);
    }
    for (p, data) in packed.iter().enumerate() {
        debug_assert_eq!(out.len(), pattern_at[p]);
        put_u16(&mut out, 2 + data.len());
        out.extend_from_slice(data);
        pad_to(&mut out, 16);
    }
    for (i, (_, bytes)) in sample_bytes.iter().enumerate() {
        if bytes.is_empty() {
            continue;
        }
        debug_assert_eq!(out.len(), data_at[i]);
        out.extend_from_slice(bytes);
        pad_to(&mut out, 16);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// XM

const XM_ENVELOPE_POINTS: usize = 12;

fn xm_envelope(
    env: &Option<Envelope>,
    which: &str,
    n: usize,
) -> Res<(Vec<u8>, u8, u8, u8, u8, u8)> {
    // (12 points of u16 pairs, count, sustain, loop start, loop end, type)
    let mut points = Vec::with_capacity(XM_ENVELOPE_POINTS * 4);
    let Some(env) = env else {
        points.extend_from_slice(&[0; XM_ENVELOPE_POINTS * 4]);
        return Ok((points, 0, 0, 0, 0, 0));
    };
    if env.points.is_empty() {
        return Err(format!(
            "instrument {n}: the {which} envelope is on but has no points"
        ));
    }
    if env.points.len() > XM_ENVELOPE_POINTS {
        return Err(format!(
            "instrument {n}: the {which} envelope has {} points; XM holds {XM_ENVELOPE_POINTS}",
            env.points.len()
        ));
    }
    for i in 0..XM_ENVELOPE_POINTS {
        let (tick, value) = env.points.get(i).copied().unwrap_or((0, 0));
        points.extend_from_slice(&tick.to_le_bytes());
        points.extend_from_slice(&value.to_le_bytes());
    }
    let mut kind = 1u8;
    let sustain = match env.sustain {
        Some(s) => {
            kind |= 2;
            s.min(255) as u8
        }
        None => 0,
    };
    let (ls, le) = match env.looped {
        Some((a, b)) => {
            kind |= 4;
            (a.min(255) as u8, b.min(255) as u8)
        }
        None => (0, 0),
    };
    Ok((points, env.points.len() as u8, sustain, ls, le, kind))
}

fn xm_pack_pattern(m: &Module, p: usize) -> Res<Vec<u8>> {
    let pattern = &m.patterns[p];
    let mut out = Vec::new();
    for row in 0..pattern.rows() {
        for ch in 0..m.channels {
            let cell = pattern.cell(row, ch);
            let at = where_cell(p, row, ch);
            let note = match cell.note {
                Note::None => 0u8,
                Note::Off => 97,
                Note::On(n) => {
                    if !(12..=107).contains(&n) {
                        return Err(format!("{at}: note {n} is outside an XM's C-0 to B-7"));
                    }
                    n - 11
                }
            };
            if cell.period != 0 {
                return Err(format!("{at}: an XM cell has no period"));
            }
            let mut flags = 0x80u8;
            if note != 0 {
                flags |= 0x01;
            }
            if cell.instrument != 0 {
                flags |= 0x02;
            }
            if cell.volume != 0 {
                flags |= 0x04;
            }
            if cell.effect != 0 {
                flags |= 0x08;
            }
            if cell.param != 0 {
                flags |= 0x10;
            }
            out.push(flags);
            if note != 0 {
                out.push(note);
            }
            if cell.instrument != 0 {
                out.push(cell.instrument);
            }
            if cell.volume != 0 {
                out.push(cell.volume);
            }
            if cell.effect != 0 {
                out.push(cell.effect);
            }
            if cell.param != 0 {
                out.push(cell.param);
            }
        }
    }
    Ok(out)
}

fn xm_instrument_is_plain(inst: &Instrument) -> bool {
    inst.keymap.iter().all(|&k| k == 0)
        && inst.volume_envelope.is_none()
        && inst.panning_envelope.is_none()
        && inst.fadeout == 0
        && inst.vibrato == Default::default()
}

/// Writes an XM (see the module docs for what is chosen).
pub fn write_xm(m: &Module) -> Res<Vec<u8>> {
    if m.format != Format::Xm {
        return Err("not an XM module".into());
    }
    if m.channels > 32 {
        return Err(format!("an XM holds 1-32 channels, not {}", m.channels));
    }
    check_structure(
        m,
        |r| (1..=256).contains(&r),
        "an XM pattern has 1-256 rows",
    )?;
    if m.patterns.len() > 256 {
        return Err(format!(
            "an XM holds 256 patterns, not {}",
            m.patterns.len()
        ));
    }
    if m.orders.len() > 256 {
        return Err(format!(
            "an XM holds 256 order positions, not {}",
            m.orders.len()
        ));
    }
    if m.instruments.len() > 128 {
        return Err(format!(
            "an XM holds 128 instruments, not {}",
            m.instruments.len()
        ));
    }
    if m.speed == 0 || m.tempo == 0 {
        return Err("speed and tempo are at least 1".into());
    }
    if m.restart >= m.orders.len() {
        return Err(format!(
            "the restart position {} is past the order list ({} orders)",
            m.restart,
            m.orders.len()
        ));
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"Extended Module: ");
    text_field(&mut out, &m.title, 20, 0x20);
    out.push(0x1a);
    text_field(&mut out, "FastTracker v2.00", 20, 0x20);
    put_u16(&mut out, 0x0104);
    put_u32(&mut out, 276);
    put_u16(&mut out, m.orders.len());
    put_u16(&mut out, m.restart);
    put_u16(&mut out, m.channels);
    put_u16(&mut out, m.patterns.len());
    put_u16(&mut out, m.instruments.len());
    put_u16(&mut out, usize::from(m.flags.linear_frequency));
    put_u16(&mut out, m.speed as usize);
    put_u16(&mut out, m.tempo as usize);
    let mut orders = [0u8; 256];
    for (slot, &o) in orders.iter_mut().zip(&m.orders) {
        *slot = o as u8;
    }
    out.extend_from_slice(&orders);

    for p in 0..m.patterns.len() {
        let packed = xm_pack_pattern(m, p)?;
        if packed.len() > 0xffff {
            return Err(format!(
                "pattern {p} packs to {} bytes; an XM pattern holds 65535",
                packed.len()
            ));
        }
        put_u32(&mut out, 9);
        out.push(0);
        put_u16(&mut out, m.patterns[p].rows());
        put_u16(&mut out, packed.len());
        out.extend_from_slice(&packed);
    }

    for (n, inst) in m.instruments.iter().enumerate() {
        let n = n + 1;
        if inst.samples.len() > 16 {
            return Err(format!(
                "instrument {n} has {} samples; an XM instrument holds 16",
                inst.samples.len()
            ));
        }
        if inst.samples.is_empty() {
            if !xm_instrument_is_plain(inst) {
                return Err(format!(
                    "instrument {n} has envelopes, a keymap, a fadeout or a vibrato but no sample; an XM keeps them only with a sample"
                ));
            }
            put_u32(&mut out, 33);
            text_field(&mut out, &inst.name, 22, 0x20);
            out.push(0);
            put_u16(&mut out, 0);
            put_u32(&mut out, 40);
            continue;
        }
        let samples: Vec<&Sample> = inst
            .samples
            .iter()
            .map(|&i| {
                m.samples
                    .get(i)
                    .ok_or_else(|| format!("instrument {n} names sample {i}, which does not exist"))
            })
            .collect::<Res<_>>()?;
        put_u32(&mut out, 263);
        text_field(&mut out, &inst.name, 22, 0x20);
        out.push(0);
        put_u16(&mut out, samples.len());
        put_u32(&mut out, 40);
        out.extend_from_slice(&inst.keymap);
        let (vp, vc, vs, vls, vle, vt) = xm_envelope(&inst.volume_envelope, "volume", n)?;
        let (pp, pc, ps, pls, ple, pt) = xm_envelope(&inst.panning_envelope, "panning", n)?;
        out.extend_from_slice(&vp);
        out.extend_from_slice(&pp);
        out.extend_from_slice(&[vc, pc, vs, vls, vle, ps, pls, ple, vt, pt]);
        out.extend_from_slice(&[
            inst.vibrato.kind,
            inst.vibrato.sweep,
            inst.vibrato.depth,
            inst.vibrato.rate,
        ]);
        out.extend_from_slice(&inst.fadeout.to_le_bytes());
        out.extend_from_slice(&[0; 22]);

        let mut encoded: Vec<Vec<u8>> = Vec::with_capacity(samples.len());
        for (k, s) in samples.iter().enumerate() {
            let label = format!("instrument {n}, sample {}", k + 1);
            if s.adlib.is_some() {
                return Err(format!("{label} is an AdLib instrument; an XM has none"));
            }
            let wide = !s.data.iter().all(|&v| on_8bit_grid(v));
            let mut bytes = Vec::with_capacity(s.data.len() * if wide { 2 } else { 1 });
            let mut previous = 0i32;
            for &v in &s.data {
                if wide {
                    let q = quantize(v, 32768.0);
                    bytes.extend_from_slice(&((q - previous) as u16).to_le_bytes());
                    previous = q;
                } else {
                    let q = quantize(v, 128.0);
                    bytes.push((q - previous) as u8);
                    previous = q;
                }
            }
            let width = if wide { 2 } else { 1 };
            put_u32(&mut out, bytes.len());
            let (ls, ll, kind) = match s.loop_kind {
                LoopKind::Off => (0, 0, 0u8),
                LoopKind::Forward => (s.loop_start, s.loop_len, 1),
                LoopKind::PingPong => (s.loop_start, s.loop_len, 2),
            };
            put_u32(&mut out, ls * width);
            put_u32(&mut out, ll * width);
            out.push(s.volume);
            out.push(s.finetune as u8);
            out.push(kind | if wide { 0x10 } else { 0 });
            out.push(s.pan.unwrap_or(128));
            out.push(s.relative_note as u8);
            out.push(0);
            text_field(&mut out, &s.name, 22, 0x20);
            encoded.push(bytes);
        }
        for bytes in encoded {
            out.extend_from_slice(&bytes);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// comparing modules

impl Module {
    /// What differs between two modules, one line each (at most a few dozen):
    /// empty when they are equal. A writer is checked by loading its bytes and
    /// asking for the differences from the module it wrote.
    pub fn differences(&self, other: &Module) -> Vec<String> {
        const LIMIT: usize = 40;
        let mut d: Vec<String> = Vec::new();
        macro_rules! field {
            ($name:literal, $a:expr, $b:expr) => {
                if $a != $b {
                    d.push(format!("{}: {:?} against {:?}", $name, $a, $b));
                }
            };
        }
        field!("format", self.format, other.format);
        field!("title", self.title, other.title);
        field!("channels", self.channels, other.channels);
        field!("orders", self.orders, other.orders);
        field!("restart", self.restart, other.restart);
        field!("speed", self.speed, other.speed);
        field!("tempo", self.tempo, other.tempo);
        field!("channel_pan", self.channel_pan, other.channel_pan);
        field!("adlib_channels", self.adlib_channels, other.adlib_channels);
        field!("muted", self.muted, other.muted);
        field!("jump_targets", self.jump_targets, other.jump_targets);
        field!("global_volume", self.global_volume, other.global_volume);
        field!("adlib_gain", self.adlib_gain, other.adlib_gain);
        field!(
            "stereo_separation",
            self.stereo_separation,
            other.stereo_separation
        );
        field!("flags", self.flags, other.flags);
        field!("mod_flavor", self.mod_flavor, other.mod_flavor);
        field!(
            "tracker_version",
            self.tracker_version,
            other.tracker_version
        );
        field!("pattern count", self.patterns.len(), other.patterns.len());
        for (i, (a, b)) in self.patterns.iter().zip(&other.patterns).enumerate() {
            if d.len() >= LIMIT {
                break;
            }
            if a.rows() != b.rows() || a.channels() != b.channels() {
                d.push(format!(
                    "pattern {i}: {}x{} against {}x{}",
                    a.rows(),
                    a.channels(),
                    b.rows(),
                    b.channels()
                ));
                continue;
            }
            let mut shown = 0;
            for row in 0..a.rows() {
                for ch in 0..a.channels() {
                    if a.cell(row, ch) != b.cell(row, ch) && shown < 3 {
                        shown += 1;
                        d.push(format!(
                            "pattern {i} row {row} channel {}: {:?} against {:?}",
                            ch + 1,
                            a.cell(row, ch),
                            b.cell(row, ch)
                        ));
                    }
                }
            }
        }
        field!("sample count", self.samples.len(), other.samples.len());
        for (i, (a, b)) in self.samples.iter().zip(&other.samples).enumerate() {
            if d.len() >= LIMIT {
                break;
            }
            if a != b {
                let mut what = Vec::new();
                macro_rules! sf {
                    ($f:ident) => {
                        if a.$f != b.$f {
                            what.push(stringify!($f));
                        }
                    };
                }
                sf!(name);
                sf!(volume);
                sf!(finetune);
                sf!(c2spd);
                sf!(relative_note);
                sf!(pan);
                sf!(adlib);
                sf!(loop_kind);
                sf!(loop_start);
                sf!(loop_len);
                if a.data != b.data {
                    what.push("data");
                }
                d.push(format!("sample {}: {} differ", i + 1, what.join(", ")));
            }
        }
        field!(
            "instrument count",
            self.instruments.len(),
            other.instruments.len()
        );
        for (i, (a, b)) in self.instruments.iter().zip(&other.instruments).enumerate() {
            if d.len() >= LIMIT {
                break;
            }
            if a != b {
                d.push(format!("instrument {}: {:?} against {:?}", i + 1, a, b));
            }
        }
        d.truncate(LIMIT);
        d
    }
}
