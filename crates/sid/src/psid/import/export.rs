//! A SID song as a PSID `.sid` the way the DAW exports it (`sid-export/`):
//! the song packed the way GoatTracker's packer packs it, assembled with
//! GoatTracker's own playroutine (`player.s`, V2.73) by [`assemble6502`], in a
//! PSID v2 file. The same bytes GoatTracker 2.77's `gt2reloc` writes for the
//! song. A port of `sid-export/gt-pack.ts`, `gt-psid.ts` and `exportSid`.
//!
//! The importer uses it for two checks: whether an unpacked GoatTracker tune
//! packs again to the file's bytes ([`GtUnpack`](super::gt_unpack::GtUnpack)'s
//! exactness), and the fidelity of a transcription, which is measured by
//! running the exported song's 6502 player on the emulated C64, as the DAW does.
//!
//! GoatTracker's relocator (`greloc.c`, GPL) was read for its FACTS only, by
//! the DAW's author: which data it keeps, in what order, how each byte is
//! converted, and when it refuses. This follows it step by step, quirks
//! included, because the gate is byte equality with the `.sid` GoatTracker's
//! own `gt2reloc` writes for the same song.

use super::flat::find_table_duplicates;
use super::gt_unpack::asm::assemble6502;
use super::gt_unpack::GT_PLAYER_SOURCE;
use crate::goattracker::{export_problem, note_byte, orderlist_bytes};
use crate::gt_note_freq_reg;
use crate::song::{Instrument, Row, SidSong};
use crate::SidModel;

/// The player's options (GoatTracker's relocator's).
#[derive(Clone, Copy, Debug)]
pub struct GtPackOptions {
    /// The player's address: a page (`$xx00`). GT's default `$1000`.
    pub player_address: i64,
    /// Two zero-page bytes the player uses. GT's default `$FC`.
    pub zero_page: i64,
    /// GT's default (true): leave out the player code the song does not use.
    pub optimize: bool,
}

pub const GT_PACK_DEFAULTS: GtPackOptions = GtPackOptions {
    player_address: 0x1000,
    zero_page: 0xfc,
    optimize: true,
};

/// What the export dialog writes: GoatTracker's "disable optimization" build, every player feature on.
pub const SID_EXPORT_DEFAULTS: GtPackOptions = GtPackOptions {
    player_address: 0x1000,
    zero_page: 0xfc,
    optimize: false,
};

/// The first address the C64's I/O area takes (`$D000`); the tune must end below it.
const MEMORY_END: i64 = 0xd000;
/// CIA cycles of one PAL frame, GoatTracker's value.
pub const PAL_FRAME_CIA: i64 = 0x4cc7;
const PSID_HEADER_LENGTH: usize = 0x7c;

const MAX_INSTR: usize = 64;
const MAX_TABLELEN: usize = 255;
const MAX_NOTES: i32 = 96;
const WTBL: usize = 0;
const PTBL: usize = 1;
const FTBL: usize = 2;
const STBL: usize = 3;
const TABLE_WHAT: [&str; 4] = ["wave", "pulse", "filter", "speed"];
const REPEAT: i32 = 0xd0;
const TRANSDOWN: i32 = 0xe0;
const TRANSUP: i32 = 0xf0;
const FIRSTNOTE: i32 = 0x60;
const LASTNOTE: i32 = 0xbc;
const REST: i32 = 0xbd;
const KEYOFF: i32 = 0xbe;
const KEYON: i32 = 0xbf;
const FX: i32 = 0x40;
const FXONLY: i32 = 0x50;
const WAVELASTDELAY: i32 = 0x0f;
const WAVESILENT: i32 = 0xe0;
const WAVELASTSILENT: i32 = 0xef;
const WAVECMD: i32 = 0xf0;
const WAVELASTCMD: i32 = 0xfe;
const CMD_PORTAUP: i32 = 1;
const CMD_PORTADOWN: i32 = 2;
const CMD_TONEPORTA: i32 = 3;
const CMD_VIBRATO: i32 = 4;
const CMD_SETAD: i32 = 5;
const CMD_SETSR: i32 = 6;
const CMD_SETWAVE: i32 = 7;
const CMD_SETWAVEPTR: i32 = 8;
const CMD_SETPULSEPTR: i32 = 9;
const CMD_SETFILTERPTR: i32 = 10;
const CMD_SETFILTERCTRL: i32 = 11;
const CMD_SETFILTERCUTOFF: i32 = 12;
const CMD_SETMASTERVOL: i32 = 13;
const CMD_FUNKTEMPO: i32 = 14;
const CMD_SETTEMPO: i32 = 15;
/// GT's default hard-restart ADSR (`adparam`).
const ADPARAM: i32 = 0x0f00;
const SIDBASE: i32 = 0xd400;

const TABLE_LEFT: [&str; 4] = [
    "mt_wavetbl",
    "mt_pulsetimetbl",
    "mt_filttimetbl",
    "mt_speedlefttbl",
];
const TABLE_RIGHT: [&str; 4] = [
    "mt_notetbl",
    "mt_pulsespdtbl",
    "mt_filtspdtbl",
    "mt_speedrighttbl",
];

/// The feature switches the relocator sets; 1 = "the song does not use it".
const FLAG_NAMES: [&str; 30] = [
    "noeffects",
    "nogate",
    "nofilter",
    "nofiltermod",
    "nopulse",
    "nopulsemod",
    "nowavedelay",
    "nowavecmd",
    "norepeat",
    "notrans",
    "noportamento",
    "notoneporta",
    "novib",
    "noinsvib",
    "nosetad",
    "nosetsr",
    "nosetwave",
    "nosetwaveptr",
    "nosetpulseptr",
    "nosetfiltptr",
    "nosetfiltcutoff",
    "nosetfiltctrl",
    "nosetmastervol",
    "nofunktempo",
    "noglobaltempo",
    "nochanneltempo",
    "nofirstwavecmd",
    "nocalculatedspeed",
    "nonormalspeed",
    "nozerospeed",
];

fn flag_index(name: &str) -> usize {
    FLAG_NAMES.iter().position(|n| *n == name).expect("a flag")
}

/// The flags by name, `Flags::NOEFFECTS` and so on.
#[derive(Clone, Debug)]
struct Flags {
    v: [i32; 30],
}

macro_rules! flag_consts {
    ($($name:ident = $n:expr),* $(,)?) => {
        impl Flags { $(const $name: usize = $n;)* }
    };
}
flag_consts!(
    NOEFFECTS = 0,
    NOGATE = 1,
    NOFILTER = 2,
    NOFILTERMOD = 3,
    NOPULSE = 4,
    NOPULSEMOD = 5,
    NOWAVEDELAY = 6,
    NOWAVECMD = 7,
    NOREPEAT = 8,
    NOTRANS = 9,
    NOPORTAMENTO = 10,
    NOTONEPORTA = 11,
    NOVIB = 12,
    NOINSVIB = 13,
    NOSETAD = 14,
    NOSETSR = 15,
    NOSETWAVE = 16,
    NOSETWAVEPTR = 17,
    NOSETPULSEPTR = 18,
    NOSETFILTPTR = 19,
    NOSETFILTCUTOFF = 20,
    NOSETFILTCTRL = 21,
    NOSETMASTERVOL = 22,
    NOFUNKTEMPO = 23,
    NOGLOBALTEMPO = 24,
    NOCHANNELTEMPO = 25,
    NOFIRSTWAVECMD = 26,
    NOCALCULATEDSPEED = 27,
    NONORMALSPEED = 28,
    NOZEROSPEED = 29,
);

#[derive(Clone, Copy, Default)]
struct GtInstr {
    ad: i32,
    sr: i32,
    /// wave, pulse, filter, speed.
    ptr: [i32; 4],
    vibdelay: i32,
    gatetimer: i32,
    firstwave: i32,
}

/// GT's `packpattern`: `rows` 4-byte rows -> the player's pattern bytes, or `None` past 256. An
/// instrument equal to the previous one is dropped; table parameters are renumbered; a tempo's
/// parameter is one less (not funktempo); a command is written (FX + cmd before a note, FXONLY +
/// cmd on an empty row) only when it changes; runs of 2-64 empty rows become one byte.
fn pack_pattern(
    src: &[i32],
    rows: usize,
    f: &mut Flags,
    tablemap: &[Vec<i64>],
    instrmap: &[i32],
) -> Option<Vec<u8>> {
    let mut t: Vec<i32> = src[..rows * 4].to_vec();
    let mut instr = 0;
    for c in 0..rows {
        let ins = src[c * 4 + 1];
        if c != 0 && ins != 0 && ins == instr {
            t[c * 4 + 1] = 0;
        } else if ins != 0 {
            instr = ins;
        }
        let cmd = t[c * 4 + 2];
        let at = c * 4 + 3;
        let map = |table: usize, v: i32| -> i32 { tablemap[table][v as usize] as i32 };
        match cmd {
            CMD_PORTAUP | CMD_PORTADOWN => {
                f.v[Flags::NOPORTAMENTO] = 0;
                t[at] = map(STBL, t[at]);
            }
            CMD_TONEPORTA => {
                f.v[Flags::NOTONEPORTA] = 0;
                t[at] = map(STBL, t[at]);
            }
            CMD_VIBRATO => {
                f.v[Flags::NOVIB] = 0;
                t[at] = map(STBL, t[at]);
            }
            CMD_SETAD => f.v[Flags::NOSETAD] = 0,
            CMD_SETSR => f.v[Flags::NOSETSR] = 0,
            CMD_SETWAVE => f.v[Flags::NOSETWAVE] = 0,
            CMD_SETWAVEPTR => {
                f.v[Flags::NOSETWAVEPTR] = 0;
                t[at] = map(WTBL, t[at]);
            }
            CMD_SETPULSEPTR => {
                f.v[Flags::NOSETPULSEPTR] = 0;
                f.v[Flags::NOPULSE] = 0;
                t[at] = map(PTBL, t[at]);
            }
            CMD_SETFILTERPTR => {
                f.v[Flags::NOSETFILTPTR] = 0;
                f.v[Flags::NOFILTER] = 0;
                t[at] = map(FTBL, t[at]);
            }
            CMD_SETFILTERCTRL => {
                f.v[Flags::NOSETFILTCTRL] = 0;
                f.v[Flags::NOFILTER] = 0;
            }
            CMD_SETFILTERCUTOFF => {
                f.v[Flags::NOSETFILTCUTOFF] = 0;
                f.v[Flags::NOFILTER] = 0;
            }
            CMD_SETMASTERVOL => {
                f.v[Flags::NOSETMASTERVOL] = 0;
                // Without author info GT erases timing marks (a volume above $0F).
                if t[at] > 0x0f {
                    t[c * 4 + 2] = 0;
                    t[at] = 0;
                }
            }
            CMD_FUNKTEMPO => {
                f.v[Flags::NOFUNKTEMPO] = 0;
                t[at] = map(STBL, t[at]);
            }
            CMD_SETTEMPO => {
                if t[at] >= 0x80 {
                    f.v[Flags::NOCHANNELTEMPO] = 0;
                } else {
                    f.v[Flags::NOGLOBALTEMPO] = 0;
                }
                if (t[at] & 0x7f) >= 3 {
                    t[at] -= 1;
                }
            }
            _ => {}
        }
    }

    let mut command: i32 = -1;
    let mut databyte: i32 = -1;
    if f.v[Flags::NOEFFECTS] != 0 {
        command = 0;
        databyte = 0;
    }
    let mut im: Vec<i32> = Vec::new();
    for c in 0..rows {
        let note = t[c * 4];
        let ins = t[c * 4 + 1];
        let cmd = t[c * 4 + 2];
        let param = t[c * 4 + 3];
        if ins != 0 {
            im.push(instrmap[ins as usize]);
        }
        let changed = cmd != command || param != databyte;
        if note == REST {
            if changed {
                command = cmd;
                databyte = param;
                im.push(FXONLY + command);
                if command != 0 {
                    im.push(databyte);
                }
            } else {
                im.push(REST);
            }
        } else {
            if changed {
                command = cmd;
                databyte = param;
                im.push(FX + command);
                if command != 0 {
                    im.push(databyte);
                }
            }
            im.push(note);
        }
    }

    // Runs of plain rests -> one packed-rest byte (never the first row).
    let mut dest: Vec<i32> = Vec::new();
    let mut c = 0usize;
    while c < im.len() {
        let mut packok = c != 0;
        if im[c] < FX {
            dest.push(im[c]);
            c += 1;
            packok = false;
        }
        if c < im.len() && im[c] >= FXONLY && im[c] < FIRSTNOTE {
            let fx = im[c] - FXONLY;
            dest.push(im[c]);
            c += 1;
            if fx != 0 {
                dest.push(im[c]);
                c += 1;
            }
            continue;
        }
        if c < im.len() && im[c] < FXONLY {
            let fx = im[c] - FX;
            dest.push(im[c]);
            c += 1;
            if fx != 0 {
                dest.push(im[c]);
                c += 1;
            }
            packok = false;
        }
        if im.get(c).copied() != Some(REST) {
            packok = false;
        }
        if !packok {
            if c < im.len() {
                dest.push(im[c]);
            }
            c += 1;
        } else {
            let mut d = c;
            while d < im.len() && im[d] == REST {
                d += 1;
                if d - c == 64 {
                    break;
                }
            }
            let d = d - c;
            if d > 1 {
                dest.push((-(d as i32)) & 0xff);
                c += d;
            } else {
                dest.push(im[c]);
                c += 1;
            }
        }
    }
    if dest.len() > 256 {
        return None;
    }
    if dest.len() < 256 {
        dest.push(0);
    }
    Some(dest.iter().map(|&b| (b & 0xff) as u8).collect())
}

/// The bytes `rows` pack to in GoatTracker's player (`packPattern`, with every effect on, as the
/// export dialog's build packs them), or more than 256 when the player cannot read the pattern.
pub fn gt_packed_pattern_size(rows: &[Row]) -> usize {
    let src: Vec<i32> = rows
        .iter()
        .flat_map(|r| {
            [
                note_byte(r.note) as i32,
                r.instrument as i32,
                r.command as i32,
                r.param as i32,
            ]
        })
        .collect();
    let mut f = Flags { v: [0; 30] };
    let identity: Vec<i64> = (0..=MAX_TABLELEN as i64).collect();
    let tablemap = vec![
        identity.clone(),
        identity.clone(),
        identity.clone(),
        identity,
    ];
    let instrmap: Vec<i32> = (0..MAX_INSTR as i32).collect();
    match pack_pattern(&src, rows.len(), &mut f, &tablemap, &instrmap) {
        Some(p) => p.len(),
        None => 257,
    }
}

/// GT's packer refused the song.
type Refusal = String;

/// GT's `gettablepartlen` is in `flat.rs`; this is the song's own tables as GT's packer holds them.
struct Tables {
    ltable: Vec<Vec<i32>>,
    rtable: Vec<Vec<i32>>,
    tableused: Vec<Vec<bool>>,
    tablemap: Vec<Vec<i64>>,
    tableerror: Option<bool>,
    first_table_error: Option<String>,
}

impl Tables {
    /// `exectable`: mark the rows a pointer reaches; the first error wins. (`tableerror`: true is a jump, false an overflow.)
    fn exec(&mut self, num: usize, start: i32) {
        let mut ptr = start;
        if num != STBL
            && ptr != 0
            && ptr <= MAX_TABLELEN as i32
            && self.ltable[num][ptr as usize - 1] == 0xff
        {
            self.tableerror = Some(true);
            return;
        }
        loop {
            if ptr == 0 {
                break;
            }
            if num != STBL && ptr > MAX_TABLELEN as i32 {
                self.tableerror = Some(false);
                break;
            }
            if self.tableused[num][ptr as usize] {
                break;
            }
            self.tableused[num][ptr as usize] = true;
            if num != STBL {
                ptr = if self.ltable[num][ptr as usize - 1] == 0xff {
                    self.rtable[num][ptr as usize - 1]
                } else {
                    ptr + 1
                };
            } else {
                break;
            }
        }
    }

    fn note_error(&mut self, whereat: &str) {
        if let Some(jump) = self.tableerror {
            if self.first_table_error.is_none() {
                self.first_table_error = Some(if jump {
                    format!("a table pointer lands on a jump row ({whereat}); GoatTracker's packer refuses that")
                } else {
                    format!("a table runs past its last row, 255, without a jump ({whereat}); GoatTracker's packer refuses that")
                });
            }
        }
    }
}

/// The packer's source for `song` (defines + `player.s` + data) and the number of subsongs it holds, or why GoatTracker's packer would refuse it.
fn pack(song: &SidSong, options: &GtPackOptions) -> Result<(String, usize), Refusal> {
    let multiplier = song.speed_multiplier as i32;

    // GT's in-memory song: the .sng as its loader leaves it.
    let songorder: Vec<Vec<Vec<u8>>> = song
        .subsongs
        .iter()
        .map(|s| s.orderlists.iter().map(orderlist_bytes).collect())
        .collect();
    let songlen: Vec<Vec<usize>> = songorder
        .iter()
        .map(|s| s.iter().map(|b| b.len() - 2).collect())
        .collect();
    let pattern: Vec<Vec<i32>> = song
        .patterns
        .iter()
        .map(|p| {
            p.rows
                .iter()
                .flat_map(|r| {
                    [
                        note_byte(r.note) as i32,
                        r.instrument as i32,
                        r.command as i32,
                        r.param as i32,
                    ]
                })
                .collect()
        })
        .collect();
    let pattlen: Vec<usize> = song.patterns.iter().map(|p| p.rows.len()).collect();
    let instr: Vec<GtInstr> = (0..MAX_INSTR)
        .map(|c| {
            let Some(ins) = (if c >= 1 {
                song.instruments.get(c - 1)
            } else {
                None
            }) else {
                return GtInstr::default();
            };
            gt_instr(ins)
        })
        .collect();
    let tables = [
        &song.tables.wave,
        &song.tables.pulse,
        &song.tables.filter,
        &song.tables.speed,
    ];
    let ltable: Vec<Vec<i32>> = tables
        .iter()
        .map(|t| {
            (0..MAX_TABLELEN)
                .map(|i| t.get(i).map_or(0, |r| r.left as i32))
                .collect()
        })
        .collect();
    let rtable: Vec<Vec<i32>> = tables
        .iter()
        .map(|t| {
            (0..MAX_TABLELEN)
                .map(|i| t.get(i).map_or(0, |r| r.right as i32))
                .collect()
        })
        .collect();

    let mut f = Flags { v: [1; 30] };
    let mut channels = 3;
    let mut fixedparams = 1;
    let mut simplepulse = 1;
    let mut firstnote = MAX_NOTES - 1;
    let mut lastnote = 0;
    let mut patternlastnote = 0;

    let mut pattused = vec![false; pattern.len()];
    let mut pattmap = vec![0usize; pattern.len()];
    let mut instrused = [false; MAX_INSTR];
    let mut instrmap = vec![0i32; MAX_INSTR];
    let mut chnused = [false; 3];
    let mut t = Tables {
        ltable,
        rtable,
        tableused: (0..4).map(|_| vec![false; MAX_TABLELEN + 1]).collect(),
        tablemap: (0..4).map(|_| vec![0i64; MAX_TABLELEN + 1]).collect(),
        tableerror: None,
        first_table_error: None,
    };
    let calcspeedtest = |f: &mut Flags, t: &Tables, pos: i32| {
        if pos == 0 {
            f.v[Flags::NOZEROSPEED] = 0;
            return;
        }
        if t.ltable[STBL][pos as usize - 1] >= 0x80 {
            f.v[Flags::NOCALCULATEDSPEED] = 0;
        } else {
            f.v[Flags::NONORMALSPEED] = 0;
        }
    };

    // 1. Orderlists.
    let mut transuprange = 0;
    let mut transdownrange = 0;
    let mut songs = 0usize;
    for (c, so) in songorder.iter().enumerate() {
        if !songlen[c].iter().all(|&n| n > 0) {
            continue;
        }
        for (d, bytes) in so.iter().enumerate() {
            for e in 0..songlen[c][d] {
                let v = bytes[e] as i32;
                if v < REPEAT {
                    if v as usize >= pattern.len() {
                        return Err(format!(
                            "subsong {c} voice {} plays pattern {v}, which the song does not have",
                            d + 1
                        ));
                    }
                    pattused[v as usize] = true;
                    for r in 0..pattlen[v as usize] {
                        let p = &pattern[v as usize];
                        if p[r * 4] != REST || p[r * 4 + 1] != 0 || p[r * 4 + 2] != 0 {
                            chnused[d] = true;
                        }
                    }
                } else if v >= TRANSDOWN {
                    f.v[Flags::NOTRANS] = 0;
                    if v < TRANSUP {
                        transdownrange = transdownrange.max(-(v - TRANSUP));
                    } else {
                        transuprange = transuprange.max(v - TRANSUP);
                    }
                } else {
                    f.v[Flags::NOREPEAT] = 0;
                }
            }
            if bytes[songlen[c][d] + 1] as usize >= songlen[c][d] {
                return Err(format!(
                    "subsong {c} voice {} restarts past the end of its orderlist",
                    d + 1
                ));
            }
        }
        songs += 1;
    }
    if !chnused[2] {
        channels = 2;
    }
    if !chnused[1] && !chnused[2] {
        channels = 1;
    }
    if songs == 0 {
        return Err("the song has no subsong with notes to play on every voice".to_string());
    }

    // 2. Patterns.
    let mut patterns = 0usize;
    instrused[1] = true;
    for c in 0..pattern.len() {
        if !pattused[c] {
            continue;
        }
        pattmap[c] = patterns;
        patterns += 1;
        let p = &pattern[c];
        for d in 0..pattlen[c] {
            t.tableerror = None;
            let note = p[d * 4];
            let ins = p[d * 4 + 1];
            let cmd = p[d * 4 + 2];
            let param = p[d * 4 + 3];
            if note == KEYOFF || note == KEYON {
                f.v[Flags::NOGATE] = 0;
            }
            if ins != 0 {
                instrused[ins as usize] = true;
            }
            if cmd != 0 {
                f.v[Flags::NOEFFECTS] = 0;
            }
            if (CMD_SETWAVEPTR..=CMD_SETFILTERPTR).contains(&cmd) {
                t.exec((cmd - CMD_SETWAVEPTR) as usize, param);
            }
            if (CMD_PORTAUP..=CMD_VIBRATO).contains(&cmd) {
                t.exec(STBL, param);
                calcspeedtest(&mut f, &t, param);
            }
            if cmd == CMD_FUNKTEMPO {
                t.exec(STBL, param);
                f.v[Flags::NOFUNKTEMPO] = 0;
                f.v[Flags::NOGLOBALTEMPO] = 0;
            }
            if cmd == CMD_SETTEMPO && (param & 0x7f) < 3 {
                f.v[Flags::NOFUNKTEMPO] = 0;
            }
            if (FIRSTNOTE..=LASTNOTE).contains(&note) {
                let newfirstnote = 0.max(note - FIRSTNOTE - transdownrange);
                let newlastnote = (MAX_NOTES - 1).min(note - FIRSTNOTE + transuprange);
                if newfirstnote < firstnote {
                    firstnote = newfirstnote;
                }
                if newlastnote > lastnote {
                    lastnote = newlastnote;
                    patternlastnote = lastnote;
                }
                if newfirstnote > lastnote {
                    lastnote = newfirstnote;
                    patternlastnote = lastnote;
                }
            }
            t.note_error(&format!("pattern {c}, row {d}"));
        }
    }

    // 3. Instruments: counted, then numbered normal, no-hard-restart, legato.
    let mut numlegato = 0;
    let mut numnohr = 0;
    let mut numnormal = 0;
    for c in 0..MAX_INSTR {
        if !instrused[c] {
            continue;
        }
        let g = instr[c].gatetimer;
        if g & 0x40 != 0 {
            numlegato += 1;
        } else if g & 0x80 != 0 {
            numnohr += 1;
        } else {
            numnormal += 1;
        }
        if instr[c].firstwave == 0 || instr[c].firstwave >= 0xfe {
            f.v[Flags::NOFIRSTWAVECMD] = 0;
        }
    }
    let mut freenormal = 1;
    let mut freenohr = freenormal + numnormal;
    let mut freelegato = freenohr + numnohr;
    let mut instruments = 0usize;
    for c in 0..MAX_INSTR {
        if !instrused[c] {
            continue;
        }
        let g = instr[c].gatetimer;
        if g & 0x40 != 0 {
            instrmap[c] = freelegato;
            freelegato += 1;
        } else if g & 0x80 != 0 {
            instrmap[c] = freenohr;
            freenohr += 1;
        } else {
            instrmap[c] = freenormal;
            freenormal += 1;
        }
        instruments += 1;
        for d in 0..4 {
            t.tableerror = None;
            t.exec(d, instr[c].ptr[d]);
            if d == STBL {
                calcspeedtest(&mut f, &t, instr[c].ptr[d]);
            }
            t.note_error(&format!("instrument {c}'s {} table", TABLE_WHAT[d]));
        }
    }

    // 4. Rows reached from wave-table commands.
    for c in 0..MAX_TABLELEN {
        if !t.tableused[WTBL][c + 1] {
            continue;
        }
        let l = t.ltable[WTBL][c];
        if !(WAVECMD..=WAVELASTCMD).contains(&l) {
            continue;
        }
        t.tableerror = None;
        let mut d: i64 = -1;
        match l - WAVECMD {
            CMD_PORTAUP | CMD_PORTADOWN | CMD_TONEPORTA | CMD_VIBRATO => {
                d = STBL as i64;
                let r = t.rtable[WTBL][c];
                calcspeedtest(&mut f, &t, r);
            }
            CMD_SETPULSEPTR => {
                d = PTBL as i64;
                f.v[Flags::NOPULSE] = 0;
            }
            CMD_SETFILTERPTR => {
                d = FTBL as i64;
                f.v[Flags::NOFILTER] = 0;
            }
            0 | CMD_SETWAVEPTR | CMD_FUNKTEMPO => {
                return Err(format!(
                    "wave table row {} has command ${:X}, which GoatTracker's packer refuses in a wave table (only its editor plays it)",
                    c + 1,
                    l
                ));
            }
            _ => {}
        }
        if d != -1 {
            let r = t.rtable[WTBL][c];
            t.exec(d as usize, r);
        }
        t.note_error(&format!("the command in wave table row {}", c + 1));
    }

    // 5. Table rows renumbered; errors; duplicates shared.
    for c in 0..4 {
        let mut e = 1;
        for d in 0..MAX_TABLELEN {
            if t.tableused[c][d + 1] {
                t.tablemap[c][d + 1] = e;
                e += 1;
            }
        }
    }
    if let Some(e) = t.first_table_error.take() {
        return Err(e);
    }
    for c in 0..4 {
        let (l, r) = (t.ltable[c].clone(), t.rtable[c].clone());
        find_table_duplicates(c, &l, &r, &mut t.tableused[c], &mut t.tablemap[c]);
    }

    // 6. Options.
    if !options.optimize {
        fixedparams = 0;
        if numlegato == 0 {
            numlegato += 1;
        }
        simplepulse = 0;
        firstnote = 0;
        lastnote = MAX_NOTES - 1;
        f.v = [0; 30];
    }

    // 7. Orderlists and patterns packed.
    let mut song_data: Vec<Vec<Vec<i32>>> = Vec::new();
    for c in 0..songs {
        song_data.push(
            songorder[c]
                .iter()
                .enumerate()
                .map(|(d, bytes)| {
                    let mut out: Vec<i32> = Vec::new();
                    let len = songlen[c][d];
                    let mut e = 0;
                    while e < len {
                        let v = bytes[e] as i32;
                        if v < REPEAT {
                            out.push(pattmap[v as usize] as i32);
                        } else if v >= TRANSDOWN {
                            out.push(v);
                        } else if v > REPEAT {
                            // A repeat is written after its pattern.
                            if (bytes[e + 1] as i32) < REPEAT {
                                out.push(pattmap[bytes[e + 1] as usize] as i32);
                                out.push(v);
                                e += 1;
                            } else {
                                out.push(v);
                            }
                        }
                        e += 1;
                    }
                    out.push(bytes[e] as i32);
                    out.push(bytes[e + 1] as i32);
                    out
                })
                .collect(),
        );
    }
    let mut packed: Vec<Vec<u8>> = Vec::new();
    for c in 0..pattern.len() {
        if !pattused[c] {
            continue;
        }
        match pack_pattern(&pattern[c], pattlen[c], &mut f, &t.tablemap, &instrmap) {
            Some(b) => packed.push(b),
            None => {
                return Err(format!(
                    "pattern {c} packs to more than 256 bytes; GoatTracker's player can't read it"
                ))
            }
        }
    }

    // 8. Instrument columns (GT's order: by new number).
    let col = || vec![0i32; instruments];
    let (mut insad, mut inssr, mut inswave, mut inspulse, mut insfilt) =
        (col(), col(), col(), col(), col());
    let (mut insvibparam, mut insvibdelay, mut insgate, mut insfirst) =
        (col(), col(), col(), col());
    for c in 1..MAX_INSTR {
        if !instrused[c] {
            continue;
        }
        let i = &instr[c];
        let d = instrmap[c] as usize - 1;
        insad[d] = i.ad;
        inssr[d] = i.sr;
        inswave[d] = t.tablemap[WTBL][i.ptr[WTBL] as usize] as i32;
        inspulse[d] = t.tablemap[PTBL][i.ptr[PTBL] as usize] as i32;
        insfilt[d] = t.tablemap[FTBL][i.ptr[FTBL] as usize] as i32;
        if i.vibdelay != 0 {
            insvibparam[d] = t.tablemap[STBL][i.ptr[STBL] as usize] as i32;
            insvibdelay[d] = i.vibdelay - 1;
        }
        insgate[d] = i.gatetimer & 0x3f;
        insfirst[d] = i.firstwave;
        if i.ptr[STBL] != 0 {
            f.v[Flags::NOVIB] = 0;
            f.v[Flags::NOINSVIB] = 0;
        }
        if i.ptr[PTBL] != 0 {
            f.v[Flags::NOPULSE] = 0;
        }
        if i.ptr[FTBL] != 0 {
            f.v[Flags::NOFILTER] = 0;
        }
        if i.gatetimer != instr[1].gatetimer || i.firstwave != instr[1].firstwave {
            fixedparams = 0;
        }
        if i.firstwave == 0 || i.firstwave >= 0xfe {
            fixedparams = 0;
        }
    }
    if multiplier > 1 {
        fixedparams = 0;
        numlegato += 1;
        numnohr += 1;
    }

    // Tables: the flags and the note range they need.
    for c in 0..MAX_TABLELEN {
        if !t.tableused[WTBL][c + 1] {
            continue;
        }
        let l = t.ltable[WTBL][c];
        let r = t.rtable[WTBL][c];
        if (0x01..=WAVELASTDELAY).contains(&l) {
            f.v[Flags::NOWAVEDELAY] = 0;
        }
        if (WAVECMD..=WAVELASTCMD).contains(&l) {
            f.v[Flags::NOWAVECMD] = 0;
            f.v[Flags::NOEFFECTS] = 0;
            let which = match l - WAVECMD {
                CMD_PORTAUP | CMD_PORTADOWN => Some(Flags::NOPORTAMENTO),
                CMD_TONEPORTA => Some(Flags::NOTONEPORTA),
                CMD_VIBRATO => Some(Flags::NOVIB),
                CMD_SETAD => Some(Flags::NOSETAD),
                CMD_SETSR => Some(Flags::NOSETSR),
                CMD_SETWAVE => Some(Flags::NOSETWAVE),
                CMD_SETPULSEPTR => Some(Flags::NOSETPULSEPTR),
                CMD_SETFILTERPTR => Some(Flags::NOSETFILTPTR),
                CMD_SETFILTERCUTOFF => Some(Flags::NOSETFILTCUTOFF),
                CMD_SETFILTERCTRL => Some(Flags::NOSETFILTCTRL),
                CMD_SETMASTERVOL => Some(Flags::NOSETMASTERVOL),
                _ => None,
            };
            if let Some(w) = which {
                f.v[w] = 0;
            }
        }
        if l < WAVECMD {
            if r <= 0x80 {
                let newlastnote = (MAX_NOTES - 1).min(r + patternlastnote);
                if r >= 0x20 {
                    firstnote = 0;
                }
                if newlastnote > lastnote {
                    lastnote = newlastnote;
                }
            } else {
                let n = r & 0x7f;
                if n < firstnote {
                    firstnote = n;
                }
                if (MAX_NOTES - 1).min(n) > lastnote {
                    lastnote = (MAX_NOTES - 1).min(n);
                }
            }
        }
    }
    for c in 0..MAX_TABLELEN {
        if !t.tableused[PTBL][c + 1] {
            continue;
        }
        let l = t.ltable[PTBL][c];
        let r = t.rtable[PTBL][c];
        if l >= 0x80 && l != 0xff && r & 0xf != 0 {
            simplepulse = 0;
        }
        if l < 0x80 {
            f.v[Flags::NOPULSEMOD] = 0;
            if r & 0xf != 0 {
                simplepulse = 0;
            }
        }
    }
    for c in 0..MAX_TABLELEN {
        if t.tableused[FTBL][c + 1] && t.ltable[FTBL][c] < 0x80 {
            f.v[Flags::NOFILTERMOD] = 0;
        }
    }

    if lastnote < firstnote {
        lastnote = firstnote;
    }
    if firstnote < 0 {
        firstnote = 0;
    }
    if f.v[Flags::NOCALCULATEDSPEED] == 0 {
        lastnote += 1;
    }
    if lastnote > MAX_NOTES - 1 {
        lastnote = MAX_NOTES - 1;
    }

    // 9. The source.
    let mut out: Vec<String> = Vec::new();
    let mut define = |name: &str, value: i64| out.push(format!("{name:<16} = {value}"));
    define("base", options.player_address);
    define("zpbase", options.zero_page);
    define("SIDBASE", SIDBASE as i64);
    define("SOUNDSUPPORT", 0);
    define("VOLSUPPORT", 0);
    define("BUFFEREDWRITES", 0);
    define("GHOSTREGS", 0);
    define("ZPGHOSTREGS", 0);
    define("FIXEDPARAMS", fixedparams as i64);
    define("SIMPLEPULSE", simplepulse as i64);
    define("PULSEOPTIMIZATION", 1);
    define("REALTIMEOPTIMIZATION", 1);
    define("NOAUTHORINFO", 1);
    // GT's define names are the flags' in capitals, but for the instrument vibrato's.
    for (i, n) in FLAG_NAMES.iter().enumerate() {
        let name = if *n == "noinsvib" {
            "NOINSTRVIB".to_string()
        } else {
            n.to_uppercase()
        };
        define(&name, f.v[i] as i64);
    }
    define("NUMCHANNELS", channels);
    define("NUMSONGS", songs as i64);
    define("FIRSTNOTE", firstnote as i64);
    define("FIRSTNOHRINSTR", numnormal as i64 + 1);
    define("FIRSTLEGATOINSTR", (numnormal + numnohr) as i64 + 1);
    define("NUMHRINSTR", numnormal as i64);
    define("NUMNOHRINSTR", numnohr as i64);
    define("NUMLEGATOINSTR", numlegato as i64);
    define("ADPARAM", (ADPARAM >> 8) as i64);
    define("SRPARAM", (ADPARAM & 0xff) as i64);
    let hidden = &instr[MAX_INSTR - 1];
    define(
        "DEFAULTTEMPO",
        if hidden.ad >= 2 && hidden.ptr[WTBL] == 0 {
            hidden.ad as i64 - 1
        } else {
            multiplier as i64 * 6 - 1
        },
    );
    if fixedparams != 0 {
        define("FIRSTWAVEPARAM", instr[1].firstwave as i64);
        define("GATETIMERPARAM", (instr[1].gatetimer & 0x3f) as i64);
    }
    out.push(GT_PLAYER_SOURCE.replace("\r\n", "\n"));

    fn label(out: &mut Vec<String>, name: &str) {
        out.push(format!("{name}:"));
    }
    fn bytes(out: &mut Vec<String>, values: &[i32]) {
        for chunk in values.chunks(16) {
            let list: Vec<String> = chunk.iter().map(|v| format!("${:02x}", v & 0xff)).collect();
            out.push(format!("                .BYTE ({})", list.join(",")));
        }
    }
    fn expr(out: &mut Vec<String>, e: &str) {
        out.push(format!("                .BYTE ({e})"));
    }
    // `define` borrows `out`: it is not used from here.
    let freqs: Vec<i32> = (0..(lastnote - firstnote + 1))
        .map(|i| gt_note_freq_reg((firstnote + i) as u8) as i32)
        .collect();
    label(&mut out, "mt_freqtbllo");
    bytes(
        &mut out,
        &freqs.iter().map(|v| v & 0xff).collect::<Vec<_>>(),
    );
    label(&mut out, "mt_freqtblhi");
    bytes(&mut out, &freqs.iter().map(|v| v >> 8).collect::<Vec<_>>());
    label(&mut out, "mt_songtbllo");
    for c in 0..songs * 3 {
        expr(&mut out, &format!("mt_song{c} % 256"));
    }
    label(&mut out, "mt_songtblhi");
    for c in 0..songs * 3 {
        expr(&mut out, &format!("mt_song{c} / 256"));
    }
    label(&mut out, "mt_patttbllo");
    for c in 0..patterns {
        expr(&mut out, &format!("mt_patt{c} % 256"));
    }
    label(&mut out, "mt_patttblhi");
    for c in 0..patterns {
        expr(&mut out, &format!("mt_patt{c} / 256"));
    }

    label(&mut out, "mt_insad");
    bytes(&mut out, &insad);
    label(&mut out, "mt_inssr");
    bytes(&mut out, &inssr);
    label(&mut out, "mt_inswaveptr");
    bytes(&mut out, &inswave);
    if f.v[Flags::NOPULSE] == 0 {
        label(&mut out, "mt_inspulseptr");
        bytes(&mut out, &inspulse);
    }
    if f.v[Flags::NOFILTER] == 0 {
        label(&mut out, "mt_insfiltptr");
        bytes(&mut out, &insfilt);
    }
    if f.v[Flags::NOINSVIB] == 0 {
        label(&mut out, "mt_insvibparam");
        bytes(&mut out, &insvibparam);
        label(&mut out, "mt_insvibdelay");
        bytes(&mut out, &insvibdelay);
    }
    if fixedparams == 0 {
        label(&mut out, "mt_insgatetimer");
        bytes(&mut out, &insgate);
        label(&mut out, "mt_insfirstwave");
        bytes(&mut out, &insfirst);
    }

    let speed_zero = f.v[Flags::NOVIB] == 0
        || f.v[Flags::NOFUNKTEMPO] == 0
        || f.v[Flags::NOPORTAMENTO] == 0
        || f.v[Flags::NOTONEPORTA] == 0;
    for c in 0..4 {
        if (c == PTBL && f.v[Flags::NOPULSE] != 0) || (c == FTBL && f.v[Flags::NOFILTER] != 0) {
            continue;
        }
        if c == STBL && speed_zero {
            bytes(&mut out, &[0]);
        }
        label(&mut out, TABLE_LEFT[c]);
        let mut left: Vec<i32> = Vec::new();
        for d in 0..MAX_TABLELEN {
            if !t.tableused[c][d + 1] {
                continue;
            }
            let l = t.ltable[c][d];
            if c == WTBL {
                let mut wave = l;
                if (WAVESILENT..=WAVELASTSILENT).contains(&l) {
                    wave &= 0xf;
                }
                if l > WAVELASTDELAY && l <= WAVELASTSILENT && f.v[Flags::NOWAVEDELAY] == 0 {
                    wave += 0x10;
                }
                left.push(wave);
            } else if c == PTBL {
                left.push(if simplepulse != 0 && l != 0xff && l > 0x80 {
                    0x80
                } else {
                    l
                });
            } else if c == FTBL {
                left.push(if l != 0xff && l > 0x80 {
                    ((l & 0x70) >> 1) | 0x80
                } else {
                    l
                });
            } else {
                left.push(l);
            }
        }
        bytes(&mut out, &left);
        if c == STBL && speed_zero {
            bytes(&mut out, &[0]);
        }
        label(&mut out, TABLE_RIGHT[c]);
        let mut right: Vec<i32> = Vec::new();
        for d in 0..MAX_TABLELEN {
            if !t.tableused[c][d + 1] {
                continue;
            }
            let l = t.ltable[c][d];
            let r = t.rtable[c][d];
            if l == 0xff && c != STBL {
                right.push(t.tablemap[c][r as usize] as i32);
            } else if c == WTBL {
                if (WAVECMD..=WAVELASTCMD).contains(&l) {
                    let cmd = l - WAVECMD;
                    if (CMD_PORTAUP..=CMD_VIBRATO).contains(&cmd) {
                        right.push(t.tablemap[STBL][r as usize] as i32);
                    } else if cmd == CMD_SETPULSEPTR {
                        right.push(t.tablemap[PTBL][r as usize] as i32);
                    } else if cmd == CMD_SETFILTERPTR {
                        right.push(t.tablemap[FTBL][r as usize] as i32);
                    } else {
                        right.push(r);
                    }
                } else {
                    right.push(r ^ 0x80);
                }
            } else if c == PTBL && simplepulse != 0 {
                if l >= 0x80 {
                    right.push((l & 0x0f) | (r & 0xf0));
                } else {
                    let mut speed = r >> 4;
                    if r & 0x80 != 0 {
                        speed |= 0xf0;
                        speed -= 1;
                    }
                    speed &= 0xff;
                    right.push(((speed & 0x0f) << 4) | ((speed & 0xf0) >> 4));
                }
            } else {
                right.push(r);
            }
        }
        bytes(&mut out, &right);
    }

    for c in 0..songs {
        for d in 0..3 {
            label(&mut out, &format!("mt_song{}", c * 3 + d));
            bytes(&mut out, &song_data[c][d]);
        }
    }
    for (c, p) in packed.iter().enumerate() {
        label(&mut out, &format!("mt_patt{c}"));
        bytes(&mut out, &p.iter().map(|&b| b as i32).collect::<Vec<_>>());
    }
    out.push(String::new());
    Ok((out.join("\n"), songs))
}

fn gt_instr(ins: &Instrument) -> GtInstr {
    GtInstr {
        ad: ((ins.attack << 4) | ins.decay) as i32,
        sr: ((ins.sustain << 4) | ins.release) as i32,
        ptr: [
            ins.wave_ptr as i32,
            ins.pulse_ptr as i32,
            ins.filter_ptr as i32,
            ins.speed_ptr as i32,
        ],
        vibdelay: ins.vibrato_delay as i32,
        gatetimer: (if ins.hard_restart { 0 } else { 0x80 })
            | (if ins.no_gate_off { 0x40 } else { 0 })
            | ins.gate_timer as i32,
        firstwave: ins.first_wave as i32,
    }
}

/// The PSID v2 wrapper, laid out as GoatTracker's packer writes it: the 124-byte header, the load
/// address, at a speed above 1x ten bytes in front of the player that set the CIA 1 timer A latch to the
/// frame's cycles / speed.
fn psid_bytes(song: &SidSong, songs: usize, address: i64, code: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let word = |out: &mut Vec<u8>, v: i64| {
        out.push(((v >> 8) & 0xff) as u8);
        out.push((v & 0xff) as u8);
    };
    let text = |out: &mut Vec<u8>, s: &[u8]| {
        for i in 0..32 {
            out.push(s.get(i).copied().unwrap_or(0));
        }
    };
    let cia = song.speed_multiplier > 1;
    let load = if cia { address - 10 } else { address };
    out.extend_from_slice(b"PSID");
    word(&mut out, 2);
    word(&mut out, PSID_HEADER_LENGTH as i64);
    word(&mut out, 0);
    word(&mut out, load);
    word(&mut out, address + 3);
    word(&mut out, songs as i64);
    word(&mut out, 1);
    let speed = if cia { 0xff } else { 0 };
    out.extend_from_slice(&[speed, speed, speed, speed]);
    text(&mut out, &song.name);
    text(&mut out, &song.author);
    text(&mut out, &song.copyright);
    // Flags: PAL (bits 2-3 = 01), SID model (bits 4-5: 01 = 6581, 10 = 8580).
    out.push(0x00);
    out.push(
        0x04 | if song.model == SidModel::Sid8580 {
            0x20
        } else {
            0x10
        },
    );
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.push((load & 0xff) as u8);
    out.push((load >> 8) as u8);
    if cia {
        let latch = PAL_FRAME_CIA / song.speed_multiplier as i64;
        // ldx #lo / stx $dc04 / ldx #hi / stx $dc05, then the player.
        out.extend_from_slice(&[
            0xa2,
            (latch & 0xff) as u8,
            0x8e,
            0x04,
            0xdc,
            0xa2,
            (latch >> 8) as u8,
            0x8e,
            0x05,
            0xdc,
        ]);
    }
    out.extend_from_slice(code);
    out
}

/// `song` as a PSID `.sid`, or why it can't be one.
pub fn export_sid(song: &SidSong, options: &GtPackOptions) -> Result<Vec<u8>, String> {
    if let Some(problem) = export_problem(song) {
        return Err(problem);
    }
    let (source, songs) = pack(song, options)?;
    let asm = assemble6502(&source).map_err(|e| {
        format!(
            "the player could not be assembled (line {}: {})",
            e.line, e.reason
        )
    })?;
    let end = asm.origin + asm.bytes.len() as i64;
    if end > MEMORY_END {
        return Err(format!(
            "the player and song take {} bytes from ${:04X} and run to ${:04X}, into the C64's I/O area at ${:04X}",
            asm.bytes.len(),
            asm.origin,
            end - 1,
            MEMORY_END
        ));
    }
    Ok(psid_bytes(song, songs, asm.origin, &asm.bytes))
}

/// Keeps `flag_index` for callers that name a flag.
#[allow(dead_code)]
fn _flag_index(name: &str) -> usize {
    flag_index(name)
}
