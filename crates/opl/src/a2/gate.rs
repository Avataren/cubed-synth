//! The O7 register gate (`.ai/plan-opl.md`): what the engine's register
//! state must equal, tick by tick, to match AdPlug's `Ca2mv2Player`.
//!
//! The references are made by `oracle/regen-gate.sh`, which runs AdPlug as a
//! black box through `examples/a2m_tool.rs`; the test (`tests.rs`) needs
//! neither AdPlug nor Python. Two things live here because both sides use
//! them:
//!
//! - [`StateHasher`]: a hash of the chip's register image after every tick.
//!   Per tick it folds in the registers whose value changed, in register
//!   order, so two players that leave the same state hash alike whatever
//!   order they wrote in. The hash is cut into chunks of [`CHUNK`] ticks,
//!   so a mismatch names the stretch where it starts.
//! - [`encode`] / [`decode`]: a sparse text form of an [`A2mSong`], for the
//!   synthetic probe modules. The crafted files are about a megabyte of
//!   uncompressed aPLib literals each; their parsed songs fit in a line.

use super::model::{
    A2mSong, ArpeggioMacro, Cell, FmMacro, FmMacroStep, Instrument, Pattern, VibratoMacro,
};

/// Ticks per hash chunk.
pub const CHUNK: usize = 512;

pub struct StateHasher {
    image: [u8; 512],
    last: [u8; 512],
    touched: Vec<u16>,
    hash: u64,
    ticks: usize,
    pub chunks: Vec<u64>,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv(h: u64, b: u8) -> u64 {
    (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
}

impl Default for StateHasher {
    fn default() -> Self {
        StateHasher {
            image: [0; 512],
            last: [0; 512],
            touched: Vec::new(),
            hash: FNV_OFFSET,
            ticks: 0,
            chunks: Vec::new(),
        }
    }
}

impl StateHasher {
    /// One register write (bank 1 as `0x100 | reg`).
    pub fn write(&mut self, reg: u16, val: u8) {
        let r = reg & 511;
        self.image[r as usize] = val;
        self.touched.push(r);
    }

    /// Closes a tick: folds in the registers it changed.
    pub fn end_tick(&mut self) {
        self.touched.sort_unstable();
        self.touched.dedup();
        for &r in &self.touched {
            let v = self.image[r as usize];
            if v != self.last[r as usize] {
                self.last[r as usize] = v;
                self.hash = fnv(fnv(fnv(self.hash, r as u8), (r >> 8) as u8), v);
            }
        }
        self.touched.clear();
        self.hash = fnv(self.hash, 0xff);
        self.ticks += 1;
        if self.ticks.is_multiple_of(CHUNK) {
            self.chunks.push(self.hash);
            self.hash = FNV_OFFSET;
        }
    }

    /// The chunk hashes, the last one partial.
    pub fn finish(mut self) -> Vec<u64> {
        if !self.ticks.is_multiple_of(CHUNK) {
            self.chunks.push(self.hash);
        }
        self.chunks
    }
}

impl super::engine::RegisterSink for StateHasher {
    fn write(&mut self, reg: u16, val: u8) {
        StateHasher::write(self, reg, val);
    }
}

/// Runs the engine as the gate does (AdPlug quirks on): the reset, then
/// `ticks` updates.
pub fn engine_hashes(song: A2mSong, ticks: usize) -> Vec<u64> {
    let mut e = super::engine::A2Engine::new(song);
    e.adplug_quirks = true;
    let mut h = StateHasher::default();
    e.reset(&mut h);
    h.end_tick();
    for _ in 0..ticks {
        e.update(&mut h);
        h.end_tick();
    }
    h.finish()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

/// The fields the engine reads, sparse, one item per `;`-separated record.
/// Names are left out.
pub fn encode(s: &A2mSong) -> String {
    let (rows, channels) = s.patterns.first().map_or((0, 0), |p| (p.rows, p.channels));
    let mut out = vec![
        format!(
            "v {} {} {} {} {} {} {} {} {} {}",
            s.version,
            s.tempo,
            s.speed,
            s.flags,
            s.pattern_len,
            s.tracks,
            s.macro_speedup,
            s.four_op_tracks,
            s.rows_per_beat.map_or("-".into(), |v| v.to_string()),
            s.tempo_finetune.map_or("-".into(), |v| v.to_string()),
        ),
        format!(
            "n {} {} {} {} {} {} {} {} {}",
            s.instruments.len(),
            s.fm_macros.len(),
            s.arpeggio_macros.len(),
            s.vibrato_macros.len(),
            s.patterns.len(),
            s.spare_patterns.len(),
            rows,
            channels,
            s.disabled_fm_columns.len(),
        ),
        format!("o {}", hex(&s.order)),
        format!("l {}", hex(&s.lock_flags)),
        format!("f {}", hex(&s.four_op_instruments)),
    ];
    for (i, ins) in s.instruments.iter().enumerate() {
        if ins.fm != [0; 11] || ins.panning != 0 || ins.finetune != 0 || ins.voice_type != 0 {
            out.push(format!(
                "i {i} {} {} {} {}",
                hex(&ins.fm),
                ins.panning,
                ins.finetune,
                ins.voice_type
            ));
        }
    }
    for (i, m) in s.fm_macros.iter().enumerate() {
        if (
            m.length,
            m.loop_begin,
            m.loop_length,
            m.keyoff_pos,
            m.arpeggio_table,
            m.vibrato_table,
        ) != (0, 0, 0, 0, 0, 0)
        {
            out.push(format!(
                "m {i} {} {} {} {} {} {}",
                m.length,
                m.loop_begin,
                m.loop_length,
                m.keyoff_pos,
                m.arpeggio_table,
                m.vibrato_table
            ));
        }
        for (k, st) in m.steps.iter().enumerate() {
            if *st != FmMacroStep::default() {
                out.push(format!(
                    "s {i} {k} {} {} {} {}",
                    hex(&st.fm),
                    st.freq_slide,
                    st.panning,
                    st.duration
                ));
            }
        }
    }
    for (i, a) in s.arpeggio_macros.iter().enumerate() {
        if a.length != 0 || a.data.iter().any(|&d| d != 0) {
            out.push(format!(
                "a {i} {} {} {} {} {} {}",
                a.length,
                a.speed,
                a.loop_begin,
                a.loop_length,
                a.keyoff_pos,
                hex(&a.data)
            ));
        }
    }
    for (i, v) in s.vibrato_macros.iter().enumerate() {
        if v.length != 0 || v.data.iter().any(|&d| d != 0) {
            let data: Vec<u8> = v.data.iter().map(|&d| d as u8).collect();
            out.push(format!(
                "b {i} {} {} {} {} {} {} {}",
                v.length,
                v.speed,
                v.delay,
                v.loop_begin,
                v.loop_length,
                v.keyoff_pos,
                hex(&data)
            ));
        }
    }
    for (i, d) in s.disabled_fm_columns.iter().enumerate() {
        if d.iter().any(|&b| b != 0) {
            out.push(format!("d {i} {}", hex(d)));
        }
    }
    for (p, pat) in s.patterns.iter().chain(&s.spare_patterns).enumerate() {
        for (k, c) in pat.cells.iter().enumerate() {
            if *c != Cell::default() {
                out.push(format!(
                    "c {p} {k} {}",
                    hex(&[
                        c.note,
                        c.instrument,
                        c.effects[0].0,
                        c.effects[0].1,
                        c.effects[1].0,
                        c.effects[1].1
                    ])
                ));
            }
        }
    }
    out.join(";")
}

pub fn decode(text: &str) -> A2mSong {
    let mut s = A2mSong {
        version: 0,
        name: Vec::new(),
        composer: Vec::new(),
        instruments: Vec::new(),
        fm_macros: Vec::new(),
        arpeggio_macros: Vec::new(),
        vibrato_macros: Vec::new(),
        order: [0; 128],
        tempo: 0,
        speed: 0,
        flags: 0,
        pattern_len: 64,
        tracks: 18,
        macro_speedup: 1,
        four_op_tracks: 0,
        lock_flags: [0; 20],
        pattern_names: Vec::new(),
        disabled_fm_columns: Vec::new(),
        four_op_instruments: Vec::new(),
        rows_per_beat: None,
        tempo_finetune: None,
        patterns: Vec::new(),
        spare_patterns: Vec::new(),
    };
    let mut npat = 0;
    for rec in text.split(';') {
        let f: Vec<&str> = rec.split(' ').collect();
        let n = |i: usize| f[i].parse::<i64>().unwrap();
        let u = |i: usize| f[i].parse::<usize>().unwrap();
        match f[0] {
            "v" => {
                s.version = n(1) as u8;
                s.tempo = n(2) as u8;
                s.speed = n(3) as u8;
                s.flags = n(4) as u8;
                s.pattern_len = n(5) as u16;
                s.tracks = n(6) as u8;
                s.macro_speedup = n(7) as u16;
                s.four_op_tracks = n(8) as u8;
                s.rows_per_beat = f[9].parse().ok();
                s.tempo_finetune = f[10].parse().ok();
            }
            "n" => {
                s.instruments = vec![Instrument::default(); u(1)];
                s.fm_macros = vec![
                    FmMacro {
                        length: 0,
                        loop_begin: 0,
                        loop_length: 0,
                        keyoff_pos: 0,
                        arpeggio_table: 0,
                        vibrato_table: 0,
                        steps: vec![FmMacroStep::default(); 255],
                    };
                    u(2)
                ];
                s.arpeggio_macros = vec![
                    ArpeggioMacro {
                        length: 0,
                        speed: 0,
                        loop_begin: 0,
                        loop_length: 0,
                        keyoff_pos: 0,
                        data: vec![0; 255],
                    };
                    u(3)
                ];
                s.vibrato_macros = vec![
                    VibratoMacro {
                        length: 0,
                        speed: 0,
                        delay: 0,
                        loop_begin: 0,
                        loop_length: 0,
                        keyoff_pos: 0,
                        data: vec![0; 255],
                    };
                    u(4)
                ];
                npat = u(5);
                let empty = Pattern {
                    rows: u(7),
                    channels: u(8),
                    cells: vec![Cell::default(); u(7) * u(8)],
                };
                s.patterns = vec![empty.clone(); npat];
                s.spare_patterns = vec![empty; u(6)];
                s.disabled_fm_columns = vec![[0; 28]; u(9)];
            }
            "o" => s.order.copy_from_slice(&unhex(f[1])),
            "l" => s.lock_flags.copy_from_slice(&unhex(f[1])),
            "f" => s.four_op_instruments = unhex(f[1]),
            "i" => {
                let ins = &mut s.instruments[u(1)];
                ins.fm.copy_from_slice(&unhex(f[2]));
                ins.panning = n(3) as u8;
                ins.finetune = n(4) as i8;
                ins.voice_type = n(5) as u8;
            }
            "m" => {
                let m = &mut s.fm_macros[u(1)];
                m.length = n(2) as u8;
                m.loop_begin = n(3) as u8;
                m.loop_length = n(4) as u8;
                m.keyoff_pos = n(5) as u8;
                m.arpeggio_table = n(6) as u8;
                m.vibrato_table = n(7) as u8;
            }
            "s" => {
                let st = &mut s.fm_macros[u(1)].steps[u(2)];
                st.fm.copy_from_slice(&unhex(f[3]));
                st.freq_slide = n(4) as i16;
                st.panning = n(5) as u8;
                st.duration = n(6) as u8;
            }
            "a" => {
                let a = &mut s.arpeggio_macros[u(1)];
                (a.length, a.speed, a.loop_begin, a.loop_length, a.keyoff_pos) =
                    (n(2) as u8, n(3) as u8, n(4) as u8, n(5) as u8, n(6) as u8);
                a.data = unhex(f[7]);
            }
            "b" => {
                let v = &mut s.vibrato_macros[u(1)];
                (
                    v.length,
                    v.speed,
                    v.delay,
                    v.loop_begin,
                    v.loop_length,
                    v.keyoff_pos,
                ) = (
                    n(2) as u8,
                    n(3) as u8,
                    n(4) as u8,
                    n(5) as u8,
                    n(6) as u8,
                    n(7) as u8,
                );
                v.data = unhex(f[8]).into_iter().map(|b| b as i8).collect();
            }
            "d" => s.disabled_fm_columns[u(1)].copy_from_slice(&unhex(f[2])),
            "c" => {
                let (p, k) = (u(1), u(2));
                let b = unhex(f[3]);
                let pat = if p < npat {
                    &mut s.patterns[p]
                } else {
                    &mut s.spare_patterns[p - npat]
                };
                pat.cells[k] = Cell {
                    note: b[0],
                    instrument: b[1],
                    effects: [(b[2], b[3]), (b[4], b[5])],
                };
            }
            _ => panic!("unknown record {rec}"),
        }
    }
    s
}
