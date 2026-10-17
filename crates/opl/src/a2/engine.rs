//! The A2M replay engine (batch O7 of `.ai/plan-opl.md`): song position,
//! rows, ticks, effects and macros, producing OPL3 register writes.
//!
//! AdPlug's port of the AT2 player is LGPL and was not read (plan decision
//! D1). Every behaviour here was pinned by running AdPlug's `Ca2mv2Player`
//! as a black box on corpus modules and on synthetic ones
//! (`oracle/craft_a2m.py`), and comparing its register writes tick by tick
//! (`oracle/trace-oracle.cpp`). "MEASURED" marks a rule read off those
//! traces. For the last divergences D1 was relaxed: Adlib Tracker II's own
//! source (GPL 3+) was read to form hypotheses, which were then probed; the
//! code here is written from those rules, not from AT2's. Names such as
//! `effect_table` or `output_note` refer to AT2 routines and tables.

use super::model::{A2mSong, Cell, FmMacroStep};

/// OPL channel of each track (MEASURED: the reset keys off in this order, and
/// track 1's instrument lands on channel 3's operators). 4-op pairs (tracks
/// 1+2, 3+4, 5+6, 10+11, ...) sit on the chip's pairs (3+0, 4+1, 5+2, ...).
pub const TRACK_CHANNEL: [usize; 18] =
    [3, 0, 4, 1, 5, 2, 6, 7, 8, 12, 9, 13, 10, 14, 11, 15, 16, 17];

/// The same with percussion mode on (MEASURED: the reset's key-off order).
/// Tracks 7-9 move to the second bank so that 16-20 get channels 6-8, the
/// chip's rhythm channels: BD on 6, SD and HH on 7, TT and TC on 8.
pub const TRACK_CHANNEL_PERC: [usize; 20] = [
    3, 0, 4, 1, 5, 2, 15, 16, 17, 12, 9, 13, 10, 14, 11, 6, 7, 8, 8, 7,
];

/// Modulator operator offset of channel `ch % 9`; the carrier is 3 higher.
const OP_OFFSET: [u16; 9] = [0x00, 0x01, 0x02, 0x08, 0x09, 0x0a, 0x10, 0x11, 0x12];

/// F-numbers of C..B (MEASURED: note 1 is C in block 0 at 0x157).
pub const FNUM: [u16; 12] = [
    0x157, 0x16b, 0x181, 0x198, 0x1b0, 0x1ca, 0x1e5, 0x202, 0x220, 0x241, 0x263, 0x287,
];

/// Slide range within a block, and the lowest and highest frequency
/// (MEASURED: a move down to 0x156 or below goes down a block adding 0x158,
/// a move up to 0x2AE or above goes up a block subtracting it, and moves
/// stop at block 0 0x157 and block 7 0x2AE). The document gives 156h and 2AEh as "octave frequency start/end".
const FREQ_LO: u16 = 0x156;
const FREQ_HI: u16 = 0x2ae;
const FREQ_MIN: u16 = 0x157;
const FREQ_MAX: u16 = 7 << 10 | 0x2ae;

/// The document's default vibrato/tremolo table: half a sine, 32 steps.
const VIB_TABLE: [u8; 32] = [
    0, 24, 49, 74, 97, 120, 141, 161, 180, 197, 212, 224, 235, 244, 250, 253, 255, 253, 250, 244,
    235, 224, 212, 197, 180, 161, 141, 120, 97, 74, 49, 24,
];

mod fx {
    pub const ARPEGGIO: u8 = 0x00;
    pub const SLIDE_UP: u8 = 0x01;
    pub const SLIDE_DOWN: u8 = 0x02;
    pub const PORTA: u8 = 0x03;
    pub const VIBRATO: u8 = 0x04;
    pub const PORTA_VSLIDE: u8 = 0x05;
    pub const VIB_VSLIDE: u8 = 0x06;
    pub const FINE_UP: u8 = 0x07;
    pub const FINE_DOWN: u8 = 0x08;
    pub const SET_MOD_VOL: u8 = 0x09;
    pub const VSLIDE: u8 = 0x0a;
    pub const POS_JUMP: u8 = 0x0b;
    pub const SET_INS_VOL: u8 = 0x0c;
    pub const PAT_BREAK: u8 = 0x0d;
    pub const SET_TEMPO: u8 = 0x0e;
    pub const SET_SPEED: u8 = 0x0f;
    pub const PORTA_VSLIDE_FINE: u8 = 0x10;
    pub const VIB_VSLIDE_FINE: u8 = 0x11;
    pub const SET_CAR_VOL: u8 = 0x12;
    pub const SET_WAVEFORM: u8 = 0x13;
    pub const VSLIDE_FINE: u8 = 0x14;
    pub const RETRIG: u8 = 0x15;
    pub const TREMOLO: u8 = 0x16;
    pub const TREMOR: u8 = 0x17;
    pub const ARP_VSLIDE: u8 = 0x18;
    pub const ARP_VSLIDE_FINE: u8 = 0x19;
    pub const MULTI_RETRIG: u8 = 0x1a;
    pub const SLIDE_UP_VSLIDE: u8 = 0x1b;
    pub const SLIDE_DOWN_VSLIDE: u8 = 0x1c;
    pub const FINE_UP_VSLIDE: u8 = 0x1d;
    pub const FINE_DOWN_VSLIDE: u8 = 0x1e;
    pub const SLIDE_UP_VSLIDE_FINE: u8 = 0x1f;
    pub const SLIDE_DOWN_VSLIDE_FINE: u8 = 0x20;
    pub const FINE_UP_VSLIDE_FINE: u8 = 0x21;
    pub const FINE_DOWN_VSLIDE_FINE: u8 = 0x22;
    pub const EXTENDED: u8 = 0x23;
    pub const EXTENDED2: u8 = 0x24;
    pub const GLOBAL_VOL: u8 = 0x25;
    pub const SWAP_ARP: u8 = 0x26;
    pub const SWAP_VIB: u8 = 0x27;
    pub const FORCE_INS_VOL: u8 = 0x28;
    pub const EXTENDED3: u8 = 0x29;
    pub const EXTRA_FINE_ARP: u8 = 0x2a;
    pub const EXTRA_FINE_VIB: u8 = 0x2b;
    pub const EXTRA_FINE_TREM: u8 = 0x2c;
    pub const CUSTOM_SPEED_TAB: u8 = 0x2d;
    pub const GLOBAL_SLIDE_UP: u8 = 0x2e;
    pub const GLOBAL_SLIDE_DOWN: u8 = 0x2f;
    /// Internal: version 5-8's 16xy, which has no v9 equivalent.
    pub const OLD_RAW_FINE: u8 = 0xf0;
    /// Internal: a plain arpeggio in `effect_table` (AT2 `ef_Arpeggio +
    /// ef_fix1`), so that it does not read as "no effect".
    pub const ARP_FIX: u8 = 0xf1;
}

/// Where a register write goes.
pub trait RegisterSink {
    fn write(&mut self, reg: u16, val: u8);
}

impl RegisterSink for Vec<(u16, u8)> {
    fn write(&mut self, reg: u16, val: u8) {
        self.push((reg, val));
    }
}

/// One effect column's running state.
#[derive(Debug, Clone, Copy, Default)]
struct Column {
    /// This row's effect and parameter.
    fx: u8,
    param: u8,
    porta_speed: u8,
    vib_speed: u8,
    vib_depth: u8,
    vib_pos: u8,
    trem_speed: u8,
    trem_depth: u8,
    trem_pos: u8,
    /// ZFD / ZFE in the other column: the vibrato / tremolo steps once per
    /// row instead of every tick (AT2 `vibr_table.fine`, `trem_table.fine`).
    vib_fine: bool,
    trem_fine: bool,
    arp_phase: u8,
    /// Shared amount of the frequency slides (set by 01, 02, 07 and 08,
    /// used by the combined slides; MEASURED).
    slide_mem: u8,
    /// The last plain arpeggio's parameter, for 18 and 19.
    arp_mem: u8,
    /// Retrigger (15, 1A) tick counter.
    retrig_count: u8,
    /// Tremor (17): ticks into the current on or off stretch, and whether
    /// the note is silenced now.
    tremor_count: u8,
    tremor_off: bool,
    /// The track's levels when the tremor started, which "on" restores
    /// (AT2 `tremor_table.volume`), and whether it has run since.
    tremor_saved: (u8, u8),
    tremor_active: bool,
    /// Write the arpeggio's base pitch on this row's first tick.
    arp_restore: bool,
    /// The previous row's effect and parameter.
    last_fx: u8,
    last_param: u8,
    /// AT2 `effect_table` as a row leaves it: the running effect and its
    /// parameter; a one-shot effect keeps only the parameter byte, an empty
    /// cell clears both.
    cur_table: (u8, u8),
    /// AT2 `last_effect`: the last non-zero `effect_table`, which decides
    /// whether a zero parameter reuses the last one.
    table_fx: u8,
    table_param: u8,
    /// 03 runs on this row (AT2: it is in `effect_table`): with a note, or
    /// without one when the column's last effect was 03 (MEASURED: after
    /// an empty row it carries on, after any other effect, a one-shot
    /// included, it does nothing and ignores its parameter).
    porta_on: bool,
    /// A global slide (2E/2F) from an earlier track's same column, which
    /// runs here on this row because this column is empty (MEASURED).
    glide: (u8, u8),
    /// Tone portamento target (AT2 `porta_table.freq`): set only by a note
    /// with 03 in this column; 05 and 10 slide to the last one.
    porta_target: u16,
}

#[derive(Debug, Clone, Copy, Default)]
struct Track {
    /// Instrument register image: AM/VIB/EG/KSR/MULT, KSL/TL, AR/DR, SL/RR,
    /// WS for modulator then carrier, and FB/CON (the file's order).
    fm: [u8; 11],
    /// 1-based instrument number, 0 before any.
    instrument: u8,
    /// Current volume as a total level, 0 loud .. 63 silent.
    vol_mod: u8,
    vol_car: u8,
    /// Base frequency: F-number (bits 0-9) and block (bits 10-12).
    freq: u16,
    /// Frequency last written to the chip (with arpeggio or vibrato).
    out_freq: u16,
    key_on: bool,
    /// 0 centre, 1 left, 2 right.
    panning: u8,
    finetune: i8,
    note: u8,
    cols: [Column; 2],
    fm_macro: MacroState,
    arp_macro: MacroState,
    vib_macro: MacroState,
    /// ZE1/ZE0: macros hold at, and loop back to, their key-off position.
    koff_loop: bool,
    /// A note off or ZF0 happened since the last key-on.
    keyed_off: bool,
    /// ZF2/ZF3: an instrument does not reset the volume.
    vol_lock: bool,
    /// ZF9/ZFA: an instrument does not set the panning.
    pan_lock: bool,
    /// ZE3/ZE2: a tone portamento note also keys the note on.
    porta_fk: bool,
    /// The pitch an FM macro step with bit 0x20 silenced, for the next step.
    zero_freq: u16,
    /// ZF4/ZF5: a volume slide up stops at the instrument's own level.
    peak_lock: bool,
    /// Lock flag bit 6, ZE5/ZE6: in a 4-op pair, 0C and the volume slides
    /// act on the pair's output operators.
    lock4: bool,
    /// Volume slides act on: 0 the default (carrier, both when additive),
    /// 1 the carrier, 2 the modulator, 3 both (lock flags bits 2-3; ZF6-8).
    vslide_mode: u8,
    /// The next instrument reloads in full and writes its volume twice
    /// (after ZF0 or an empty instrument; MEASURED).
    forced_reload: bool,
    /// &2x: this row's note waits x ticks; &3x cuts it after x ticks.
    note_delay: u8,
    delayed_note: u8,
    note_cut: u8,
    /// &4x/&5x: added to this row's note only.
    fine_once: i8,
    /// ZC/ZD pattern loop: start row and remaining count.
    loop_row: usize,
    loop_count: u8,
    /// The vibrato table's offset now in the pitch.
    /// The pitch a vibrato table moves around (AT2 `vib_freq`): set when
    /// the table starts and whenever an effect or the arpeggio table sets
    /// the pitch.
    vib_freq: u16,
    /// An effect wrote the pitch during this tick.
    pitch_touched: bool,
    /// The arpeggio table's base note (AT2 `arpg_note`): the note when the
    /// tables started, not later ones (a delayed note, a porta target).
    arp_note: u8,
    /// Arpeggio and vibrato macro tables in use (1-based, 0 none).
    arp_table: u8,
    vib_table: u8,
    /// 63 minus the level last written by a volume change, modulator and
    /// carrier (AT2 `modulator_vol`/`carrier_vol`; an instrument load or
    /// ZF0 does not count). Both 0: the global volume passes it by.
    loud: [u8; 2],
}

/// A running instrument macro (FM register, arpeggio or vibrato table).
#[derive(Debug, Clone, Copy, Default)]
struct MacroState {
    active: bool,
    /// 1-based step.
    pos: u8,
    /// Ticks counted towards the next step.
    count: u16,
    /// Vibrato tables: ticks still to wait before the first step.
    delay: u8,
    /// Set by a key-off: the macro has left its pre-key-off part.
    released: bool,
    /// Step to take next instead of the usual one (a key-off's jump).
    jump: u8,
}

/// Next step of a macro with this length, loop and key-off position
/// (MEASURED on FM, arpeggio and vibrato tables alike): the loop wraps at
/// `loop_begin + loop_length - 1`; before a key-off, reaching the key-off
/// position stops the macro, or holds it there under ZE1 ("key-off loop");
/// past the length it stops, or under ZE1 after a key-off goes back to the
/// key-off position.
fn macro_next(
    m: &MacroState,
    length: u8,
    loop_begin: u8,
    loop_length: u8,
    keyoff: u8,
    koff_loop: bool,
) -> Next {
    if m.jump != 0 {
        return if m.jump <= length || m.released {
            Next::Step(m.jump)
        } else {
            Next::Stop
        };
    }
    let loop_end = (loop_begin as u16 + loop_length as u16).saturating_sub(1);
    let next = if loop_length > 0 && loop_begin > 0 && m.pos as u16 == loop_end {
        loop_begin
    } else {
        match m.pos.checked_add(1) {
            Some(n) => n,
            None => return Next::Stop,
        }
    };
    if !m.released && keyoff > 0 && next >= keyoff {
        return if koff_loop { Next::Hold } else { Next::Stop };
    }
    if next > length {
        // ZE1: after a key-off, the part from the key-off position loops.
        return if m.released && koff_loop && keyoff > 0 {
            Next::Step(keyoff)
        } else {
            Next::Stop
        };
    }
    Next::Step(next)
}

enum Next {
    Step(u8),
    /// ZE1: wait at the key-off position for the key-off.
    Hold,
    Stop,
}

pub struct A2Engine {
    /// Reproduce AdPlug's known departures from Adlib Tracker II, for the
    /// register-trace gate only (see the O7 landing record). Off for play.
    pub adplug_quirks: bool,
    song: A2mSong,
    tracks: [Track; 20],
    order_pos: usize,
    pattern: usize,
    row: usize,
    speed: u8,
    tempo: u8,
    tick: u8,
    macro_tick: u32,
    global_volume: u8,
    /// Set by a position jump or pattern break on this row.
    jump: Option<(usize, usize)>,
    ended: bool,
    /// Current 0xBD value (depth bits and rhythm).
    bd: u8,
    /// Row ticks since the start; the extra-fine effects run when it is 3
    /// mod 4 (MEASURED).
    row_ticks: u32,
    /// Ticks the current row lasts: speed, plus pattern delays.
    row_len: u16,
    row_tick: u16,
    /// &0x and &1x on this row.
    frame_delay: u8,
    row_delay: u8,
    /// 1-based number of the last instrument with FM data.
    last_fm_instrument: usize,
    /// Order, pattern and row of the next row to play.
    next_pos: (usize, usize, usize),
    /// Times the song has passed an order marker.
    loops: u32,
}

impl A2Engine {
    /// Why this song cannot be played, checked at load: the player refuses
    /// rather than play silence (plan O7, step 5).
    pub fn refusal(song: &A2mSong) -> Option<String> {
        let first = song.order.iter().position(|&o| o < 0x80);
        match first {
            None => Some(
                "its order list holds only jump markers, no pattern to play \
                 (an instrument collection saved as a song)"
                    .into(),
            ),
            // A jump chain from order 0 that never reaches a pattern.
            _ if Self::new(song.clone()).ended => {
                Some("its order list loops through jump markers without reaching a pattern".into())
            }
            _ => None,
        }
    }

    pub fn new(mut song: A2mSong) -> A2Engine {
        let spare = std::mem::take(&mut song.spare_patterns);
        song.patterns.extend(spare);
        // An order past the file's patterns plays empty rows (MEASURED; AT2
        // keeps zeroed memory for every pattern).
        let top = song
            .order
            .iter()
            .filter(|&&o| o < 0x80)
            .max()
            .copied()
            .unwrap_or(0) as usize;
        if let Some(p) = song.patterns.first() {
            let empty = super::model::Pattern {
                rows: p.rows,
                channels: p.channels,
                cells: vec![Cell::default(); p.cells.len()],
            };
            while song.patterns.len() <= top {
                song.patterns.push(empty.clone());
            }
        }
        // A "fixed" note (0x90 + note, v9+) plays as the note; the flag only
        // stops the editor from transposing it (AT2 `play_line`).
        for cell in song.patterns.iter_mut().flat_map(|p| p.cells.iter_mut()) {
            if (0x91..=0x90 + 97).contains(&cell.note) {
                cell.note -= 0x90;
            }
        }
        if song.version < 9 {
            let version = song.version;
            for cell in song.patterns.iter_mut().flat_map(|p| p.cells.iter_mut()) {
                for e in cell.effects.iter_mut() {
                    *e = convert_old_effect(version, *e);
                }
            }
        }
        let mut e = A2Engine {
            adplug_quirks: false,
            speed: song.speed,
            tempo: song.tempo,
            song,
            tracks: [Track::default(); 20],
            order_pos: 0,
            pattern: 0,
            row: 0,
            tick: 0,
            macro_tick: 0,
            global_volume: 63,
            jump: None,
            ended: false,
            bd: 0,
            row_ticks: 0,
            row_len: 0,
            row_tick: 0,
            frame_delay: 0,
            row_delay: 0,
            next_pos: (0, 0, 0),
            last_fm_instrument: 0,
            loops: 0,
        };
        // MEASURED: each track's lock byte gives its volume slide type
        // (bits 2-3) always, its panning (bits 0-1) under song flag bit 5,
        // its volume lock (bit 4) under bit 1 and its peak lock (bit 5)
        // under bit 2.
        e.last_fm_instrument = e
            .song
            .instruments
            .iter()
            .rposition(|i| i.fm != [0; 11])
            .map_or(0, |i| i + 1);
        let flags = e.song.flags;
        for (t, tr) in e.tracks.iter_mut().enumerate() {
            let lock = e.song.lock_flags.get(t).copied().unwrap_or(0);
            tr.vslide_mode = (lock >> 2) & 3;
            if flags & 0x20 != 0 {
                tr.pan_lock = true;
                tr.panning = (lock & 3).min(2);
            }
            tr.vol_lock = flags & 0x02 != 0 && lock & 0x10 != 0;
            tr.peak_lock = flags & 0x04 != 0 && lock & 0x20 != 0;
            tr.lock4 = lock & 0x40 != 0;
        }
        // MEASURED: before any instrument a track's levels are 0 (full),
        // though under volume scaling they apply to instrument t+1's.
        match e.resolve_order(0) {
            Some((order, pattern, _)) => e.next_pos = (order, pattern, 0),
            None => e.ended = true,
        }
        e
    }

    pub fn song(&self) -> &A2mSong {
        &self.song
    }

    /// Timer rate in Hz: tempo × macro speed-up, where tempos of 18 and
    /// below run the PC timer at its slowest rate, 18.2 Hz (AdPlug's
    /// `getrefresh` agrees for 18 and above; below 18 it reports tempo ×
    /// speed-up while counting ticks as AT2 does at 18.2, so it plays those
    /// songs too slowly; see [`row_tick_every`](Self::row_tick_every)).
    pub fn refresh(&self) -> f64 {
        let tempo = if self.tempo <= 18 {
            18.2
        } else {
            self.tempo as f64
        };
        tempo * self.song.macro_speedup.max(1) as f64
    }

    /// Timer ticks per row tick: the macro speed-up, or for tempos below 18
    /// `floor(18 × speed-up / tempo)` (MEASURED: tempo 10 with speed-up 20
    /// advances rows every 36 ticks, tempo 1 every 18).
    fn row_tick_every(&self) -> u32 {
        let su = self.song.macro_speedup.max(1) as u32;
        if self.tempo < 18 && self.tempo > 0 {
            (18 * su / self.tempo as u32).max(1)
        } else {
            su
        }
    }

    pub fn position(&self) -> (usize, usize, usize) {
        (self.order_pos, self.pattern, self.row)
    }

    /// Order, pattern and row the next row will be read from.
    pub fn next_position(&self) -> (usize, usize, usize) {
        self.next_pos
    }

    /// Makes the next row come from `order` (following jump markers) and
    /// `row` instead: the player's order loop. False when no pattern is
    /// reachable from there.
    pub fn redirect(&mut self, order: usize, row: usize) -> bool {
        match self.resolve_order(order.min(127)) {
            Some((order, pattern, _)) => {
                let row = if row < self.song.pattern_len as usize {
                    row
                } else {
                    0
                };
                self.next_pos = (order, pattern, row);
                self.ended = false;
                true
            }
            None => false,
        }
    }

    /// Tracks the song plays (its declared count).
    pub fn track_count(&self) -> usize {
        self.tracks_in_use()
    }

    /// The OPL channel (0..17) track `t` is heard on. In a 4-op pair the
    /// registers are per track, but the chip outputs the whole voice from
    /// the second track's channel and nothing from the first's, so both
    /// tracks report that one (mute, solo and scope taps follow it).
    pub fn track_channel(&self, t: usize) -> usize {
        let t = t.min(19);
        self.channel(self.pair(t).map_or(t, |(_, second)| second))
    }

    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Times the song has come round (passed an order marker).
    pub fn loops(&self) -> u32 {
        self.loops
    }

    /// The tracks the player runs: the song's declared count (AT2 plays
    /// tracks 1..nm_tracks; 9 or 18 before v9), so percussion tracks 19-20
    /// play only in 20-track songs.
    fn tracks_in_use(&self) -> usize {
        (self.song.tracks as usize).clamp(1, 20)
    }

    /// Song flag bit 6, "percussion track extension".
    fn percussion(&self) -> bool {
        self.song.flags & 0x40 != 0
    }

    fn channel(&self, t: usize) -> usize {
        if self.percussion() {
            TRACK_CHANNEL_PERC[t]
        } else {
            TRACK_CHANNEL[t]
        }
    }

    /// The 4-op pair holding track `t`, as (first, second) track, when the
    /// song joins it (tracks 1+2, 3+4, 5+6, 10+11, 12+13, 14+15).
    fn pair(&self, t: usize) -> Option<(usize, usize)> {
        let first = match t {
            0..=5 => t & !1,
            9..=14 => 9 + ((t - 9) & !1),
            _ => return None,
        };
        let bit = if first < 6 {
            first / 2
        } else {
            3 + (first - 9) / 2
        };
        (self.song.four_op_tracks >> bit & 1 != 0).then_some((first, first + 1))
    }

    /// A0/B0 register offset for track `t`'s pitch and key. Both tracks of
    /// a 4-op pair play on the pair's first chip channel, the second
    /// track's (MEASURED: notes on either key channel 0 for tracks 1+2).
    fn key_reg(&self, t: usize) -> u16 {
        let t = self.pair(t).map_or(t, |(_, second)| second);
        reg_channel(self.channel(t))
    }

    /// The operators a locked 4-op pair's volume effects act on: those the
    /// algorithm outputs, by the connections of the first chip channel
    /// (second track) and the other (MEASURED order: the first track's,
    /// then the second's; O1/O2 are the second track's modulator/carrier).
    fn out_ops(&self, (first, second): (usize, usize)) -> Vec<(usize, usize)> {
        let (a, b) = (self.additive(second), self.additive(first));
        let o1 = (second, 0);
        let o2 = (second, 1);
        let o3 = (first, 0);
        let o4 = (first, 1);
        match (a, b) {
            (false, false) => vec![o4],
            (true, false) => vec![o4, o1],
            (false, true) => vec![o4, o2],
            (true, true) => vec![o3, o4, o1],
        }
    }

    /// The two tracks of a 4-op pair share one pitch and key state (MEASURED: a key-off
    /// on the pair's other track, which never played, writes this track's
    /// frequency), so a pitch written for one is the other's too.
    fn share_pitch(&mut self, t: usize) {
        if let Some((a, b)) = self.pair(t) {
            let o = if t == a { b } else { a };
            self.tracks[o].freq = self.tracks[t].freq;
            self.tracks[o].out_freq = self.tracks[t].out_freq;
            // The key bit lives in the same word (AT2 `freq_table`).
            self.tracks[o].key_on = self.tracks[t].key_on;
        }
    }

    fn locked_pair(&self, t: usize) -> Option<(usize, usize)> {
        self.pair(t).filter(|_| self.tracks[t].lock4)
    }

    /// Writes one output operator of a locked pair at level `l` (0 loud):
    /// as a volume on the instrument's own level when `relative` or under
    /// volume scaling, else as it is; then the global volume.
    fn write_op_level(
        &mut self,
        u: usize,
        op: usize,
        l: u8,
        relative: bool,
        out: &mut impl RegisterSink,
    ) {
        let (m, c) = self.ops(u);
        let r = if op == 0 { m } else { c };
        let rel = if relative || self.volume_scaling() {
            let ins = self
                .song
                .instruments
                .get(self.voice(u) - 1)
                .map_or(0, |d| d.fm[2 + op] & 0x3f);
            scale_level(ins, 63 - l.min(63))
        } else {
            l
        };
        let v = (self.tracks[u].fm[2 + op] & 0xc0) | self.scaled(rel);
        self.put_level(u, op, r, v, out);
    }

    /// 0xBD: tremolo depth (flag bit 3), vibrato depth (bit 4) and rhythm
    /// mode (bit 6) (MEASURED from the reset writes).
    fn bd_value(&self) -> u8 {
        let f = self.song.flags;
        (f & 0x08) << 4 | (f & 0x10) << 2 | (f & 0x40) >> 1
    }

    /// 0x104: the 4-op pairs (MEASURED: the track-extension flags byte, six
    /// bits, is written as is).
    fn four_op_value(&self) -> u8 {
        self.song.four_op_tracks & 0x3f
    }

    /// The writes that put the chip into the state the player starts from
    /// (MEASURED: AdPlug's rewind, in this order).
    pub fn reset(&mut self, out: &mut impl RegisterSink) {
        out.write(0x001, 0x00);
        for t in 0..18 {
            out.write(0xb0 + reg_channel(self.channel(t)), 0);
        }
        for op in (0x00..0x0e).chain(0x10..0x16) {
            out.write(0x80 + op, 0xff);
        }
        out.write(0x001, 0x20);
        out.write(0x008, 0x40);
        out.write(0x105, 0x01);
        out.write(0x104, self.four_op_value());
        for t in [16, 17] {
            let ch = reg_channel(self.channel(t));
            out.write(0xa0 + ch, 0);
            out.write(0xb0 + ch, 0);
        }
        self.bd = self.bd_value();
        out.write(0x0bd, self.bd);
    }

    /// One timer tick. Row ticks come every `macro_speedup`-th tick; the
    /// first row plays on the `speed`-th row tick (MEASURED: tick
    /// `(speed - 1) × speedup`), and each row lasts its speed plus any
    /// pattern delay. Effects run on every row tick, the row's own
    /// included; macros on every tick, after them.
    pub fn update(&mut self, out: &mut impl RegisterSink) {
        if self.ended {
            return;
        }
        let every = self.row_tick_every();
        if self.macro_tick >= every {
            self.macro_tick = 0;
        }
        if self.macro_tick == 0 {
            if self.row_len == 0 {
                self.tick += 1;
                if self.tick >= self.speed {
                    self.start_row(out);
                }
            } else {
                self.row_tick += 1;
                if self.row_tick >= self.row_len {
                    self.start_row(out);
                }
            }
            if self.row_len != 0 {
                let xf = self.row_ticks % 4 == 3;
                for t in 0..self.tracks_in_use() {
                    self.tick_effects(t, xf, out);
                }
                // MEASURED: the next position is settled on the row's own
                // tick (a restart's writes land there).
                if self.row_tick == 0 {
                    self.after_row(out);
                }
            }
            self.row_ticks += 1;
        }
        for t in 0..self.tracks_in_use() {
            self.run_macros(t, out);
        }
        // A tempo set on this tick already counts (MEASURED: tempo 9 set
        // at tempo 19 waits two ticks for the next row tick).
        self.macro_tick = (self.macro_tick + 1) % self.row_tick_every();
    }

    fn start_row(&mut self, out: &mut impl RegisterSink) {
        self.row_tick = 0;
        self.frame_delay = 0;
        self.row_delay = 0;
        (self.order_pos, self.pattern, self.row) = self.next_pos;
        self.play_row(out);
        // MEASURED: a speed change counts from the row that carries it.
        let speed = self.speed.max(1) as u16;
        self.row_len = speed * (1 + self.row_delay as u16) + self.frame_delay as u16;
    }

    fn play_row(&mut self, out: &mut impl RegisterSink) {
        let tracks = self.tracks_in_use();
        // Versions 1-4 store 9 tracks and 5-8 store 18; the rest are empty.
        let pat = &self.song.patterns[self.pattern];
        let cells: Vec<Cell> = (0..tracks)
            .map(|t| {
                if t < pat.channels {
                    *pat.cell(self.row, t)
                } else {
                    Cell::default()
                }
            })
            .collect();
        for (t, cell) in cells.iter().enumerate() {
            for (c, &(fx, param)) in cell.effects.iter().enumerate() {
                let col = &mut self.tracks[t].cols[c];
                col.last_fx = col.fx;
                col.last_param = col.param;
                if col.cur_table != (0, 0) {
                    (col.table_fx, col.table_param) = col.cur_table;
                }
                col.cur_table.0 = 0;
                col.fx = fx;
                col.param = param;
                // MEASURED: a combined volume-slide effect with 00 right after
                // the same effect reuses its parameter (plain 0A does not).
                let combo = matches!(
                    fx,
                    fx::PORTA_VSLIDE
                        | fx::VIB_VSLIDE
                        | fx::PORTA_VSLIDE_FINE
                        | fx::VIB_VSLIDE_FINE
                        | fx::ARP_VSLIDE
                        | fx::ARP_VSLIDE_FINE
                ) || (fx::SLIDE_UP_VSLIDE..=fx::FINE_DOWN_VSLIDE_FINE).contains(&fx);
                // AT2 `play_line`: 00 reuses the parameter of the last effect
                // of the same family (05/10, 06/11 from `effect_table`
                // itself, 18/19, 1B-22). With none, current AT2 drops the
                // effect; AdPlug runs it with 00, and play follows AdPlug.
                if combo && param == 0 {
                    let (family, from) = match fx {
                        fx::PORTA_VSLIDE | fx::PORTA_VSLIDE_FINE => {
                            ([fx::PORTA_VSLIDE, fx::PORTA_VSLIDE_FINE], col.table_param)
                        }
                        fx::VIB_VSLIDE | fx::VIB_VSLIDE_FINE => {
                            ([fx::VIB_VSLIDE, fx::VIB_VSLIDE_FINE], col.cur_table.1)
                        }
                        fx::ARP_VSLIDE | fx::ARP_VSLIDE_FINE => {
                            ([fx::ARP_VSLIDE, fx::ARP_VSLIDE_FINE], col.table_param)
                        }
                        _ => ([0xff, 0xff], col.table_param),
                    };
                    let same = family.contains(&col.table_fx)
                        || (family[0] == 0xff
                            && (fx::SLIDE_UP_VSLIDE..=fx::FINE_DOWN_VSLIDE_FINE)
                                .contains(&col.table_fx));
                    if same && from != 0 {
                        col.param = from;
                    }
                }
                let param = col.param;
                // MEASURED: when a plain or extra-fine arpeggio gives way to
                // another effect, the note's pitch comes back on the next
                // row's first tick unless the last step was the base, and
                // that counts as a step.
                // 18 and 19 count as the arpeggio they carry (so 18 → 18
                // restores too; MEASURED).
                let was_arp = (matches!(col.last_fx, fx::ARPEGGIO | fx::EXTRA_FINE_ARP)
                    && col.last_param != 0)
                    || matches!(col.last_fx, fx::ARP_VSLIDE | fx::ARP_VSLIDE_FINE);
                let is_arp = matches!(fx, fx::ARPEGGIO | fx::EXTRA_FINE_ARP) && param != 0;
                col.arp_restore = was_arp && !is_arp && col.arp_phase != 1;
                // MEASURED: an arpeggio, vibrato or tremolo starts from its
                // first step when the column enters it from another effect
                // (04 → 06 → 11 carries on; 0A → 04 restarts).
                // An arpeggio's step is kept until a note, or until one
                // starts after a last effect that was no arpeggio (AT2
                // `arpgg_table.state`; MEASURED: an arpeggio left, then
                // resumed after rows with notes only, goes on from x).
                let arp_row = (fx == fx::ARPEGGIO && param != 0)
                    || matches!(
                        fx,
                        fx::EXTRA_FINE_ARP | fx::ARP_VSLIDE | fx::ARP_VSLIDE_FINE
                    );
                if arp_row
                    && !matches!(
                        col.table_fx,
                        fx::ARP_FIX | fx::EXTRA_FINE_ARP | fx::ARP_VSLIDE | fx::ARP_VSLIDE_FINE
                    )
                {
                    col.arp_phase = 0;
                }
                if group(fx) != group(col.last_fx) {
                    col.vib_pos = 0;
                    col.trem_pos = 0;
                    col.tremor_count = 0;
                }
                // MEASURED: a row without a vibrato-type effect forgets the
                // vibrato's speed and depth (06, or 04 00, after an empty
                // row or another effect do nothing; 04 → 06 carries on).
                // Porta, slide, tremolo and arpeggio amounts are kept.
                if group(fx) != 2 {
                    col.vib_speed = 0;
                    col.vib_depth = 0;
                }
                // AT2 `play_line`: ZFD beside a vibrato-type effect, or ZFE
                // beside a tremolo, makes it fine; the flag lasts while the
                // column keeps that kind of effect and is cleared with its
                // table otherwise (MEASURED on Bad Apple, order 70).
                let other = cell.effects[1 - c];
                if group(fx) != 2 {
                    col.vib_fine = false;
                } else if other == (fx::EXTENDED, 0xfd) {
                    col.vib_fine = true;
                }
                if group(fx) != 3 {
                    col.trem_fine = false;
                } else if other == (fx::EXTENDED, 0xfe) {
                    col.trem_fine = true;
                }
                // MEASURED: the retrigger counter restarts only when a
                // retrigger follows a non-empty row that was not one (15 00
                // counts as not one); empty rows, notes and new parameters
                // leave it running.
                // AT2 also clears it on any row whose effect is not a
                // retrigger (so a retrigger after a stale one counts from 0).
                if !matches!(fx, fx::RETRIG | fx::MULTI_RETRIG) {
                    col.retrig_count = 0;
                }
                // MEASURED: a retrigger counts from 1 when the column's last
                // effect (AT2 `last_effect`, as the row starts) was not one:
                // after a one-shot that followed a running effect, yes; after
                // a one-shot that followed empty rows, no (psycho3x).
                let retrig = |(f, p): (u8, u8)| {
                    (f == fx::RETRIG && p != 0) || (f == fx::MULTI_RETRIG && p >> 4 != 0)
                };
                if retrig((fx, param)) && !retrig((col.table_fx, col.table_param)) {
                    col.retrig_count = 1;
                }
                match fx {
                    fx::SLIDE_UP | fx::SLIDE_DOWN | fx::FINE_UP | fx::FINE_DOWN => {
                        col.slide_mem = param
                    }
                    fx::ARPEGGIO if param != 0 => col.arp_mem = param,
                    _ => {}
                }
            }
        }
        for t in 0..tracks {
            let tr = &mut self.tracks[t];
            tr.note_delay = 0;
            tr.note_cut = 0;
            tr.fine_once = 0;
        }
        for (t, cell) in cells.iter().enumerate() {
            if cell.instrument != 0 {
                self.set_instrument(t, cell.instrument, out);
            }
        }
        // A tremor that has given way to another effect puts back the levels
        // it started from, after the row's instruments and before its
        // effects (AT2 `play_line`; MEASURED for a stop while silent).
        for t in 0..tracks {
            for c in 0..2 {
                let col = self.tracks[t].cols[c];
                if col.tremor_active && col.fx != fx::TREMOR {
                    let tr = &mut self.tracks[t];
                    tr.cols[c].tremor_active = false;
                    tr.cols[c].tremor_off = false;
                    (tr.vol_mod, tr.vol_car) = col.tremor_saved;
                    self.write_volume(t, out);
                }
            }
        }
        // MEASURED: the first effect column of every track, then the second.
        for c in 0..2 {
            for (t, cell) in cells.iter().enumerate() {
                self.row_effect(t, c, cell, out);
            }
        }
        // MEASURED: a global slide also runs, for its row, on every later
        // track whose same column is free: empty, an extended command (23,
        // 24, 29) or a 03 that does not run. Current AT2 gives it to every
        // later track; play follows AdPlug.
        for c in 0..2 {
            let mut glide = (0, 0);
            for (t, cell) in cells.iter().enumerate() {
                let e = cell.effects[c];
                if matches!(e.0, fx::GLOBAL_SLIDE_UP | fx::GLOBAL_SLIDE_DOWN) {
                    glide = e;
                }
                let col = &mut self.tracks[t].cols[c];
                let free = e == (0, 0)
                    || matches!(e.0, fx::EXTENDED | fx::EXTENDED2 | fx::EXTENDED3)
                    || (e.0 == fx::PORTA && !col.porta_on);
                col.glide = if free { glide } else { (0, 0) };
            }
        }
        for (t, cell) in cells.iter().enumerate() {
            for (c, &(fx, param)) in cell.effects.iter().enumerate() {
                let col = &mut self.tracks[t].cols[c];
                if (fx, param) == (0, 0) {
                    col.cur_table = (0, 0);
                } else if !one_shot(fx, param) && (fx != fx::PORTA || col.porta_on) {
                    let lo = if col.fx == fx::ARPEGGIO {
                        fx::ARP_FIX
                    } else {
                        col.fx
                    };
                    col.cur_table = (lo, col.param);
                }
            }
        }
        for (t, cell) in cells.iter().enumerate() {
            if cell.note != 0 {
                if self.tracks[t].note_delay != 0 && cell.note == 0xff {
                    // AT2 tests for a key-off before a note delay: it keys
                    // off now, and the delay's end only rewrites the pitch
                    // (MEASURED).
                    self.play_note(t, 0xff, out);
                    self.tracks[t].delayed_note = 0xfe;
                } else if self.tracks[t].note_delay != 0 {
                    // AT2 records a delayed note as the track's note at
                    // once (an FM macro retrigger plays it early).
                    self.tracks[t].delayed_note = cell.note;
                    if cell.note != 0xff {
                        self.tracks[t].note = cell.note;
                        self.tracks[t].keyed_off = false;
                    }
                } else {
                    self.play_note(t, cell.note, out);
                }
            }
            if cell.note == 0 && self.tracks[t].note_delay != 0 {
                // A delay without a note strikes the track's last note
                // (AT2 `event_table` note; MEASURED); after a key-off it
                // only rewrites the pitch.
                let tr = &mut self.tracks[t];
                tr.delayed_note = if tr.keyed_off { 0xfe } else { tr.note };
            }
            if cell.note == 0 && self.tracks[t].fine_once != 0 {
                // AT2 `output_note` with no note: &4x/&5x move the pitch
                // that is sounding, and it stays there (MEASURED). Current
                // AT2 skips this under a running portamento or a note delay;
                // AdPlug does not, and play follows AdPlug (O7 record).
                let tr = &mut self.tracks[t];
                let f = (tr.freq as i32 + tr.fine_once as i32) as u16 & 0x1fff;
                tr.freq = f;
                self.write_freq(t, f, out);
            }
            self.note_pass_effects(t, out);
            self.swap_tables(t, cell);
        }
    }

    /// 26 and 27, after the row's note (whose key-on restarts the
    /// instrument's own tables): the new table starts from its top, or with
    /// ZFF in the other column carries on from the same step (AT2
    /// `play_line`).
    fn swap_tables(&mut self, t: usize, cell: &Cell) {
        for c in 0..2 {
            let (fx, param) = cell.effects[c];
            let keep = cell.effects[1 - c] == (fx::EXTENDED, 0xff);
            match fx {
                fx::SWAP_ARP => {
                    let len = self
                        .song
                        .arpeggio_macros
                        .get((param as usize).wrapping_sub(1))
                        .map_or(0, |a| a.length);
                    let tr = &mut self.tracks[t];
                    tr.arp_table = param;
                    tr.arp_note = tr.note;
                    if keep {
                        keep_macro(&mut tr.arp_macro, len);
                    } else {
                        tr.arp_macro = MacroState {
                            active: len > 0,
                            ..Default::default()
                        };
                    }
                }
                fx::SWAP_VIB => {
                    let (len, delay) = self
                        .song
                        .vibrato_macros
                        .get((param as usize).wrapping_sub(1))
                        .map_or((0, 0), |v| (v.length, v.delay));
                    let tr = &mut self.tracks[t];
                    tr.vib_table = param;
                    if keep {
                        keep_macro(&mut tr.vib_macro, len);
                    } else {
                        tr.vib_macro = MacroState {
                            active: len > 0,
                            delay,
                            count: 1,
                            ..Default::default()
                        };
                        tr.vib_freq = tr.freq;
                    }
                }
                _ => {}
            }
        }
    }

    /// Works out the position after this row. Passing an order marker
    /// back to order 0 from a later one restarts the song: the global volume returns to full
    /// and the levels are rewritten (MEASURED); any marker ends a pass.
    fn after_row(&mut self, out: &mut impl RegisterSink) {
        let (mut order, mut row) = (self.order_pos, self.row + 1);
        if let Some((o, r)) = self.jump.take() {
            order = o;
            row = r;
        } else if row >= self.song.pattern_len as usize {
            row = 0;
            order += 1;
        }
        let Some((order, pattern, marker)) = self.resolve_order(order) else {
            self.ended = true;
            return;
        };
        if row >= self.song.pattern_len as usize {
            row = 0;
        }
        if marker {
            self.loops += 1;
            // MEASURED: not when the song is one order long (0 → 0).
            if order == 0 && self.order_pos > 0 {
                self.global_volume = 63;
                self.write_global_levels(out);
            }
        }
        self.next_pos = (order, pattern, row);
    }

    /// The order entry at or after `order` that holds a pattern, following
    /// markers (0x80 + n: continue at n), and whether a marker was taken.
    fn resolve_order(&self, mut order: usize) -> Option<(usize, usize, bool)> {
        let mut marker = false;
        for _ in 0..=128 {
            if order >= 128 {
                order = 0;
                marker = true;
            }
            let o = self.song.order[order];
            if o < 0x80 {
                return ((o as usize) < self.song.patterns.len())
                    .then_some((order, o as usize, marker));
            }
            order = (o & 0x7f) as usize;
            marker = true;
        }
        None
    }

    fn set_instrument(&mut self, t: usize, ins: u8, out: &mut impl RegisterSink) {
        let Some(data) = self.song.instruments.get(ins as usize - 1).cloned() else {
            return;
        };
        // MEASURED: an instrument with nothing in it (finetune included)
        // silences the channel first, like ZF0, and always reloads.
        if is_empty(&data) {
            self.release_sound(t, out);
        }
        let forced = self.tracks[t].forced_reload;
        let reload = self.tracks[t].instrument != ins || forced;
        let tr = &mut self.tracks[t];
        tr.forced_reload = false;
        if reload {
            tr.instrument = ins;
            tr.fm = data.fm;
            // Under a pan lock the reload takes the track's locked panning
            // (AT2 `set_ins_data`), undoing any ZB since.
            tr.panning = if tr.pan_lock {
                (self.song.lock_flags.get(t).copied().unwrap_or(0) & 3).min(2)
            } else {
                data.panning
            };
            tr.finetune = data.finetune;
            self.load_fm(t, out);
        }
        let (lm, lc) = (
            self.ins_level(t, &data.fm, 0),
            self.ins_level(t, &data.fm, 1),
        );
        let tr = &mut self.tracks[t];
        if !tr.vol_lock || reload {
            tr.vol_mod = lm;
            tr.vol_car = lc;
        }
        if reload {
            self.write_volume(t, out);
        }
        if (!reload && !self.tracks[t].vol_lock) || forced {
            self.write_volume(t, out);
        }
        // MEASURED: a new instrument starts its macros; with no key-on yet
        // (an instrument alone, or a delayed note) they start as if keyed
        // off, so a vibrato table plays from its key-off position. A key-on
        // starts them again.
        if reload {
            self.start_macros(t);
            if !self.tracks[t].key_on {
                self.release_macros(t);
            }
        }
    }

    /// ZF0 "release sustaining sound": silence both operators, key off,
    /// fastest envelope, and forget the instrument (MEASURED writes).
    fn release_sound(&mut self, t: usize, out: &mut impl RegisterSink) {
        let (m, c) = self.ops(t);
        let ch = self.key_reg(t);
        out.write(0x40 + m, 0x3f);
        out.write(0x40 + c, 0x3f);
        out.write(0xb0 + ch, 0);
        out.write(0x60 + m, 0xff);
        out.write(0x60 + c, 0xff);
        out.write(0x80 + m, 0xff);
        out.write(0x80 + c, 0xff);
        let f = self.tracks[t].freq;
        out.write(0xa0 + ch, f as u8);
        out.write(0xb0 + ch, (f >> 8) as u8);
        self.tracks[t].key_on = false;
        self.share_pitch(t);
        self.tracks[t].keyed_off = true;
        self.tracks[t].forced_reload = true;
    }

    /// The 20/23, C0 and 40/43 writes that follow a change of connection,
    /// feedback, panning or an operator's flags (MEASURED order).
    fn write_connection_group(&mut self, t: usize, out: &mut impl RegisterSink) {
        let (m, c) = self.ops(t);
        let fm = self.tracks[t].fm;
        out.write(0x20 + m, fm[0]);
        out.write(0x20 + c, fm[1]);
        out.write(
            0xc0 + reg_channel(self.channel(t)),
            fm[10] | pan_bits(self.tracks[t].panning),
        );
        self.write_volume(t, out);
    }

    /// AR/DR, SL/RR and waveform of one operator (0 modulator, 1 carrier).
    fn write_envelope_group(&mut self, t: usize, op: usize, out: &mut impl RegisterSink) {
        let (m, c) = self.ops(t);
        let r = if op == 0 { m } else { c };
        let fm = self.tracks[t].fm;
        out.write(0x60 + r, fm[4 + op]);
        out.write(0x80 + r, fm[6 + op]);
        out.write(0xe0 + r, fm[8 + op]);
    }

    fn write_global_levels(&mut self, out: &mut impl RegisterSink) {
        for u in 0..self.tracks_in_use() {
            // AT2 `set_global_volume` passes by a track whose last levels
            // were both silent (MEASURED at a song restart).
            if self.tracks[u].loud != [0, 0] {
                if self.additive(u) {
                    self.write_mod_level(u, out);
                }
                self.write_car_level(u, out);
            }
        }
    }

    fn global_volume_slide(&mut self, by: i32, out: &mut impl RegisterSink) {
        self.global_volume = (self.global_volume as i32 + by).clamp(0, 63) as u8;
        self.write_global_levels(out);
    }

    fn load_fm(&mut self, t: usize, out: &mut impl RegisterSink) {
        let ch = self.channel(t);
        let (m, c) = self.ops(t);
        let fm = self.tracks[t].fm;
        out.write(0x20 + m, fm[0]);
        out.write(0x20 + c, fm[1]);
        out.write(0x40 + m, fm[2] | 0x3f);
        out.write(0x40 + c, fm[3] | 0x3f);
        out.write(0x60 + m, fm[4]);
        out.write(0x60 + c, fm[5]);
        out.write(0x80 + m, fm[6]);
        out.write(0x80 + c, fm[7]);
        out.write(0xe0 + m, fm[8]);
        out.write(0xe0 + c, fm[9]);
        out.write(
            0xc0 + reg_channel(ch),
            fm[10] | pan_bits(self.tracks[t].panning),
        );
    }

    /// The instrument whose data a track's volume rules read: the last one
    /// set, or before any the track's own number (MEASURED: a track with no
    /// instrument treats its levels as instrument t+1's, and ZF1 restores
    /// that instrument's levels; its registers stay unloaded).
    fn voice(&self, t: usize) -> usize {
        match self.tracks[t].instrument {
            0 => t + 1,
            i => i as usize,
        }
    }

    fn voice_has_data(&self, t: usize) -> bool {
        self.song
            .instruments
            .get(self.voice(t) - 1)
            .is_some_and(|d| !is_empty(d))
    }

    /// Whether both operators sound: the instrument's connection bit, not
    /// the register image (MEASURED: after 29 01 on an FM instrument, 0C
    /// still sets only the carrier; after 29 00 on an additive one, both).
    fn additive(&self, t: usize) -> bool {
        self.song
            .instruments
            .get(self.voice(t) - 1)
            .is_some_and(|d| d.fm[10] & 1 != 0)
    }

    /// Modulator and carrier register offsets of track `t`. In percussion
    /// mode tracks 17-20 (SD, TT, TC, HH) are one operator each, the
    /// instrument's modulator half; AdPlug sends the carrier half to offset
    /// 0xFF, which lands on unused registers (MEASURED: 0x11F, 0x13F, ...).
    fn ops(&self, t: usize) -> (u16, u16) {
        match self.single_op(t) {
            Some(op) => (op, 0xff),
            None => op_regs(self.channel(t)),
        }
    }

    /// The one operator of a percussion track, by track (MEASURED: a
    /// melodic instrument on track 17 still plays operator 0x14).
    fn single_op(&self, t: usize) -> Option<u16> {
        if !self.percussion() {
            return None;
        }
        match t {
            16 => Some(0x14),
            17 => Some(0x12),
            18 => Some(0x15),
            19 => Some(0x11),
            _ => None,
        }
    }

    /// Scales a total level by the global volume (MEASURED:
    /// `63 - round((63 - tl) × gv / 63)`).
    fn scaled(&self, tl: u8) -> u8 {
        scale_level(tl, self.global_volume)
    }

    /// A volume write to operator `op` (0 modulator, 1 carrier) of track
    /// `t` at register offset `r`.
    fn put_level(&mut self, t: usize, op: usize, r: u16, v: u8, out: &mut impl RegisterSink) {
        // AT2's "OPL3 emulation workaround" (`set_ins_volume.c`): with the
        // channel's ADSR empty and no FM macro, both levels go to 63. Without
        // it, a key-on with attack rate 0 freezes the envelope where the
        // previous note left it, so that note hangs at full level. AdPlug
        // lacks it, so the register gate keeps it off.
        let v = if !self.adplug_quirks && self.adsr_empty_no_macro(t) {
            (v & 0xc0) | 0x3f
        } else {
            v
        };
        self.tracks[t].loud[op] = 63 - (v & 0x3f);
        out.write(0x40 + r, v);
    }

    /// AT2 `is_chan_adsr_data_empty` on the track's current registers, and
    /// its instrument has no FM macro.
    fn adsr_empty_no_macro(&self, t: usize) -> bool {
        self.tracks[t].fm[4..8].iter().all(|&b| b == 0)
            && self
                .song
                .fm_macros
                .get(self.voice(t).wrapping_sub(1))
                .is_none_or(|m| m.length == 0)
    }

    fn write_volume(&mut self, t: usize, out: &mut impl RegisterSink) {
        let tr = self.tracks[t];
        self.write_levels(t, tr.vol_mod, tr.vol_car, out);
    }

    /// Song flag bit 7, "volume scaling".
    fn volume_scaling(&self) -> bool {
        self.song.flags & 0x80 != 0
    }

    /// Whether operator `op` (0 modulator, 1 carrier) is heard directly
    /// rather than modulating the other.
    fn sounding(&self, t: usize, op: usize) -> bool {
        op == 1 || self.additive(t) || self.single_op(t).is_some()
    }

    /// A track's level for one operator (0 modulator, 1 carrier) as the
    /// total level it plays at, before the global volume. With volume
    /// scaling the level of a sounding operator is a volume applied to the
    /// instrument's own level (MEASURED: `63 - round((63 - ins) × (63 -
    /// level) / 63)`, where the instrument is the track's voice even before
    /// it is loaded); an FM modulator's level stays absolute.
    fn relative(&self, t: usize, op: usize, level: u8) -> u8 {
        if !self.volume_scaling() || !self.sounding(t, op) {
            return level;
        }
        let ins = self
            .song
            .instruments
            .get(self.voice(t) - 1)
            .map_or(0, |d| d.fm[2 + op] & 0x3f);
        scale_level(ins, 63 - level.min(63))
    }

    /// The level an instrument sets: its own total level, or full volume
    /// (0) under volume scaling for the carrier and an additive modulator
    /// (AT2 `reset_ins_volume`: the connection decides, so a percussion
    /// modulator keeps its level and is then scaled by it again).
    fn ins_level(&self, t: usize, fm: &[u8; 11], op: usize) -> u8 {
        if self.volume_scaling() && (op == 1 || self.additive(t)) {
            0
        } else {
            fm[2 + op] & 0x3f
        }
    }

    /// The modulator's output level: global volume only when it sounds.
    fn mod_out(&self, t: usize, level: u8) -> u8 {
        let l = self.relative(t, 0, level);
        if self.additive(t) || self.single_op(t).is_some() {
            self.scaled(l)
        } else {
            l
        }
    }

    fn car_out(&self, t: usize, level: u8) -> u8 {
        self.scaled(self.relative(t, 1, level))
    }

    fn write_levels(&mut self, t: usize, tl_mod: u8, tl_car: u8, out: &mut impl RegisterSink) {
        let (m, c) = self.ops(t);
        let tr = self.tracks[t];
        let (vm, vc) = (
            (tr.fm[2] & 0xc0) | self.mod_out(t, tl_mod),
            (tr.fm[3] & 0xc0) | self.car_out(t, tl_car),
        );
        self.put_level(t, 0, m, vm, out);
        self.put_level(t, 1, c, vc, out);
    }

    /// Writes the levels that follow the global volume: the carrier, then
    /// the modulator when the connection is additive (MEASURED order).
    fn write_scaled_levels(&mut self, t: usize, out: &mut impl RegisterSink) {
        self.write_car_level(t, out);
        if self.additive(t) {
            self.write_mod_level(t, out);
        }
    }

    fn write_mod_level(&mut self, t: usize, out: &mut impl RegisterSink) {
        let (m, _) = self.ops(t);
        let tr = self.tracks[t];
        let v = (tr.fm[2] & 0xc0) | self.mod_out(t, tr.vol_mod);
        self.put_level(t, 0, m, v, out);
    }

    fn write_car_level(&mut self, t: usize, out: &mut impl RegisterSink) {
        let (_, c) = self.ops(t);
        let tr = self.tracks[t];
        let v = (tr.fm[3] & 0xc0) | self.car_out(t, tr.vol_car);
        self.put_level(t, 1, c, v, out);
    }

    /// AT2 `nFreq(note - 1) + finetune`: the finetune is added to block and
    /// F-number as one number, and notes from 97 up play block 7's top.
    fn note_freq(&self, t: usize, note: u8) -> u16 {
        let n = (note & 0x7f) as usize - 1;
        let base = if n >= 96 {
            FREQ_MAX
        } else {
            FNUM[n % 12] | ((n / 12) as u16) << 10
        };
        (base as i32 + self.tracks[t].finetune as i32) as u16
    }

    fn is_porta(fx: u8) -> bool {
        matches!(fx, fx::PORTA | fx::PORTA_VSLIDE | fx::PORTA_VSLIDE_FINE)
    }

    fn play_note(&mut self, t: usize, note: u8, out: &mut impl RegisterSink) {
        let ch = self.key_reg(t);
        if note == 0xff {
            let f = self.tracks[t].freq;
            out.write(0xa0 + ch, f as u8);
            out.write(0xb0 + ch, (f >> 8) as u8);
            self.tracks[t].key_on = false;
            self.share_pitch(t);
            self.tracks[t].keyed_off = true;
            // MEASURED: a note off restarts an arpeggio on its own row (AT2
            // reads a key-off as the last note with the key-off flag; rows
            // without an arpeggio keep its step). A retrigger keeps counting.
            for col in self.tracks[t].cols.iter_mut() {
                if (col.fx == fx::ARPEGGIO && col.param != 0)
                    || matches!(
                        col.fx,
                        fx::EXTRA_FINE_ARP | fx::ARP_VSLIDE | fx::ARP_VSLIDE_FINE
                    )
                {
                    col.arp_phase = 0;
                }
            }
            self.release_macros(t);
            return;
        }
        let freq = {
            let f = self.note_freq(t, note);
            (f as i32 + self.tracks[t].fine_once as i32) as u16
        };
        let cols = self.tracks[t].cols;
        if cols.iter().any(|c| Self::is_porta(c.fx)) {
            // MEASURED: after a key-off (note off or ZF0) a portamento note
            // first keys on again at the track's last note, which it leaves
            // as the last note; otherwise it becomes the last note (silently,
            // on a track that never played). The target is 03's own
            // (`row_effect`).
            // Neither restarts the tables (AT2 `output_note` with
            // restart_macro off). ZE3 ("force key") keys on at the new note.
            if self.tracks[t].keyed_off {
                let last = self.tracks[t].note;
                if last != 0 {
                    let f = self.note_freq(t, last);
                    self.key_on_at(t, f, false, out);
                }
            } else {
                self.tracks[t].note = note;
                if self.tracks[t].porta_fk {
                    self.key_on_at(t, freq, false, out);
                }
            }
            return;
        }
        self.tracks[t].note = note;
        self.key_on_at(t, freq, true, out);
    }

    /// Keys the note on at `freq`. ZFF ("no restart") on the row keeps the
    /// tables running, and with a table swap (26/27) in the other column
    /// also leaves the envelope alone (no key-off first; AT2 `play_line`
    /// and `output_note`; MEASURED).
    fn key_on_at(&mut self, t: usize, freq: u16, restart: bool, out: &mut impl RegisterSink) {
        let ch = self.key_reg(t);
        let cols = self.tracks[t].cols;
        let keep = cols.iter().any(|c| (c.fx, c.param) == (fx::EXTENDED, 0xff));
        let legato = keep
            && cols
                .iter()
                .any(|c| matches!(c.fx, fx::SWAP_ARP | fx::SWAP_VIB));
        // AT2 `output_note` also gives the note to track t-1 when t is in a
        // 4-op pair: its partner for the second track, but for the first
        // the previous track, whose key-off it forgets (MEASURED).
        if t > 0 && self.pair(t).is_some() {
            let n = self.tracks[t].note;
            self.tracks[t - 1].note = n;
            self.tracks[t - 1].keyed_off = false;
        }
        let tr = &mut self.tracks[t];
        tr.keyed_off = false;
        tr.freq = freq;
        tr.key_on = true;
        tr.out_freq = freq;
        // MEASURED: a note restarts an arpeggio on its own row (not one
        // that carries on after a note row without it), but not a tremolo.
        for c in tr.cols.iter_mut() {
            let arp = match c.fx {
                fx::ARPEGGIO | fx::EXTRA_FINE_ARP => c.param != 0,
                fx::ARP_VSLIDE | fx::ARP_VSLIDE_FINE => true,
                _ => false,
            };
            if arp {
                c.arp_phase = 0;
            }
        }
        if !legato {
            out.write(0xb0 + ch, 0);
        }
        out.write(0xa0 + ch, freq as u8);
        out.write(0xb0 + ch, 0x20 | (freq >> 8) as u8);
        self.share_pitch(t);
        // AT2 `output_note` ends in `change_frequency`: the vibrato table
        // restarts around the new pitch (MEASURED for a porta key-on).
        self.tracks[t].pitch_touched = true;
        if restart && !keep {
            self.start_macros(t);
        } else if keep {
            let tr = &mut self.tracks[t];
            tr.arp_note = tr.note;
        }
    }

    fn start_macros(&mut self, t: usize) {
        let ins = self.tracks[t].instrument as usize;
        let tr = &mut self.tracks[t];
        tr.fm_macro = MacroState::default();
        tr.arp_macro = MacroState::default();
        tr.vib_macro = MacroState::default();
        tr.arp_note = tr.note;
        // MEASURED: AdPlug keeps macros only up to the last instrument
        // with FM data (its instrument count); later ones never run, and
        // like an instrument without tables they stop the track's tables.
        tr.arp_table = 0;
        tr.vib_table = 0;
        if ins > self.last_fm_instrument {
            return;
        }
        let Some(m) = self.song.fm_macros.get(ins.wrapping_sub(1)) else {
            return;
        };
        tr.arp_table = m.arpeggio_table;
        tr.vib_table = m.vibrato_table;
        tr.vib_freq = tr.freq;
        if m.length > 0 {
            tr.fm_macro = MacroState {
                active: true,
                pos: 0,
                ..Default::default()
            };
        }
        if let Some(a) = self
            .song
            .arpeggio_macros
            .get((tr.arp_table as usize).wrapping_sub(1))
        {
            if a.length > 0 {
                tr.arp_macro = MacroState {
                    active: true,
                    ..Default::default()
                };
            }
        }
        if let Some(v) = self
            .song
            .vibrato_macros
            .get((tr.vib_table as usize).wrapping_sub(1))
        {
            if v.length > 0 {
                tr.vib_macro = MacroState {
                    active: true,
                    delay: v.delay,
                    count: 1,
                    ..Default::default()
                };
            }
        }
    }

    /// A key-off sends each running macro to its key-off position, taken
    /// when the current step's time is up (MEASURED). A vibrato table also
    /// restarts from there (from the top when it has none), even if it had
    /// finished, around the pitch of the moment.
    fn release_macros(&mut self, t: usize) {
        let ins = self.tracks[t].instrument as usize;
        let tr = &mut self.tracks[t];
        let jump = |st: &mut MacroState, keyoff: u8| {
            if st.active && keyoff > 0 {
                st.jump = keyoff;
                st.released = true;
            }
        };
        if let Some(m) = self.song.fm_macros.get(ins.wrapping_sub(1)) {
            jump(&mut tr.fm_macro, m.keyoff_pos);
        }
        if let Some(a) = self
            .song
            .arpeggio_macros
            .get((tr.arp_table as usize).wrapping_sub(1))
        {
            jump(&mut tr.arp_macro, a.keyoff_pos);
        }
        if let Some(v) = self
            .song
            .vibrato_macros
            .get((tr.vib_table as usize).wrapping_sub(1))
        {
            if v.length > 0 {
                tr.vib_macro.active = true;
                tr.vib_macro.released = true;
                tr.vib_macro.jump = v.keyoff_pos.max(1);
                tr.vib_macro.count = 1;
                tr.vib_freq = tr.freq;
            }
        }
    }

    /// A pitch set by an effect or the arpeggio table (AT2
    /// `change_frequency`): the vibrato table restarts around it, in a 4-op
    /// pair on both tracks (MEASURED).
    fn write_freq(&mut self, t: usize, freq: u16, out: &mut impl RegisterSink) {
        self.tracks[t].pitch_touched = true;
        if let Some((a, b)) = self.pair(t) {
            self.tracks[if t == a { b } else { a }].pitch_touched = true;
        }
        self.put_freq(t, freq, out);
    }

    /// A pitch write that leaves the vibrato table alone (AT2 `change_freq`,
    /// used by the vibrato table itself).
    fn put_freq(&mut self, t: usize, freq: u16, out: &mut impl RegisterSink) {
        let freq = freq & 0x1fff;
        let ch = self.key_reg(t);
        let key = if self.tracks[t].key_on { 0x20 } else { 0 };
        self.tracks[t].out_freq = freq;
        out.write(0xa0 + ch, freq as u8);
        out.write(0xb0 + ch, key | (freq >> 8) as u8);
        self.share_pitch(t);
    }

    /// Effects applied once, when the row is read, before its notes.
    fn row_effect(&mut self, t: usize, c: usize, cell: &Cell, out: &mut impl RegisterSink) {
        let (fx, param) = cell.effects[c];
        // MEASURED: the pitch an arpeggio leaves behind is restored here,
        // before the row's notes (a note off on the same row comes after).
        if self.tracks[t].cols[c].arp_restore {
            let base = self.arp_freq(t, 0);
            let col = &mut self.tracks[t].cols[c];
            col.arp_restore = false;
            // MEASURED: the base counts as the first step, so an 18 or 19
            // taking over carries on with x.
            col.arp_phase = 1;
            // AT2 `change_frequency`: the note becomes the pitch that slides
            // move from (MEASURED: a slide after it starts at the note).
            self.tracks[t].freq = base;
            self.write_freq(t, base, out);
        }
        match fx {
            fx::SET_MOD_VOL => {
                self.tracks[t].vol_mod = 63 - (param & 0x3f);
                self.write_mod_level(t, out);
            }
            fx::SET_CAR_VOL => {
                self.tracks[t].vol_car = 63 - (param & 0x3f);
                self.write_car_level(t, out);
            }
            // MEASURED: 0C and 28 do nothing while the track's voice is an
            // empty instrument (or none yet and instrument t+1 is empty).
            fx::SET_INS_VOL | fx::FORCE_INS_VOL if !self.voice_has_data(t) => {}
            fx::SET_INS_VOL if self.single_op(t).is_some() => {
                // MEASURED: on a one-operator drum it sets that operator.
                self.tracks[t].vol_mod = 63 - (param & 0x3f);
                self.write_mod_level(t, out);
            }
            fx::FORCE_INS_VOL if self.single_op(t).is_some() => {
                // MEASURED: both levels, the operator written scaled.
                let tl = 63 - (param & 0x3f);
                self.tracks[t].vol_mod = tl;
                self.tracks[t].vol_car = tl;
                let (m, _) = self.ops(t);
                let v = (self.tracks[t].fm[2] & 0xc0) | self.mod_out(t, tl);
                self.put_level(t, 0, m, v, out);
            }
            fx::SET_INS_VOL if self.locked_pair(t).is_some() => {
                // MEASURED: each output operator keeps 63 - v as its level
                // but is written as the instrument's own level at volume v.
                let l = 63 - (param & 0x3f);
                for (u, op) in self.out_ops(self.locked_pair(t).unwrap()) {
                    let tr = &mut self.tracks[u];
                    if op == 0 {
                        tr.vol_mod = l;
                    } else {
                        tr.vol_car = l;
                    }
                    self.write_op_level(u, op, l, true, out);
                }
            }
            fx::SET_INS_VOL => {
                let tl = 63 - (param & 0x3f);
                self.tracks[t].vol_car = tl;
                if self.additive(t) {
                    self.tracks[t].vol_mod = tl;
                    self.write_volume(t, out);
                } else {
                    self.write_car_level(t, out);
                }
            }
            fx::FORCE_INS_VOL => {
                // MEASURED: the carrier gets 63 - v; the modulator too when
                // additive, else the instrument's own modulator level scaled
                // by v.
                let v = param & 0x3f;
                let ins_mod = self.tracks[t].fm[2] & 0x3f;
                let additive = self.additive(t);
                let tr = &mut self.tracks[t];
                tr.vol_car = 63 - v;
                tr.vol_mod = if additive {
                    63 - v
                } else {
                    scale_level(ins_mod, v)
                };
                self.write_volume(t, out);
            }
            fx::SET_WAVEFORM => {
                // MEASURED: high nibble carrier, low nibble modulator; 8 and
                // up leave it alone. Each goes out with its operator's AR/DR
                // and SL/RR, carrier first.
                let (m, car) = self.ops(t);
                if param >> 4 < 8 {
                    let tr = &mut self.tracks[t];
                    tr.fm[9] = param >> 4;
                    out.write(0x60 + car, tr.fm[5]);
                    out.write(0x80 + car, tr.fm[7]);
                    out.write(0xe0 + car, tr.fm[9]);
                }
                if param & 15 < 8 {
                    let tr = &mut self.tracks[t];
                    tr.fm[8] = param & 15;
                    out.write(0x60 + m, tr.fm[4]);
                    out.write(0x80 + m, tr.fm[6]);
                    out.write(0xe0 + m, tr.fm[8]);
                }
            }
            fx::GLOBAL_VOL => {
                self.global_volume = param & 0x3f;
                self.write_global_levels(out);
            }
            fx::EXTENDED => self.extended(t, c, param, out),
            fx::EXTENDED2 => match param >> 4 {
                0x0 => self.frame_delay = param & 15,
                0x1 => self.row_delay = param & 15,
                0x2 => self.tracks[t].note_delay = param & 15,
                0x3 => self.tracks[t].note_cut = param & 15,
                // Both columns add up (AT2 `ftune_table`).
                0x4 => self.tracks[t].fine_once += (param & 15) as i8,
                0x5 => self.tracks[t].fine_once -= (param & 15) as i8,
                _ => {}
            },
            fx::EXTENDED3 => {
                let v = param & 15;
                let tr = &mut self.tracks[t];
                let set = |b: &mut u8, mask: u8, val: u8| *b = (*b & !mask) | (val & mask);
                match param >> 4 {
                    0x0 => set(&mut tr.fm[10], 0x01, v),
                    0x1 => set(&mut tr.fm[0], 0x0f, v),
                    0x2 => set(&mut tr.fm[2], 0xc0, v << 6),
                    0x3 => set(&mut tr.fm[0], 0x80, v << 7),
                    0x4 => set(&mut tr.fm[0], 0x40, v << 6),
                    0x5 => set(&mut tr.fm[0], 0x10, v << 4),
                    0x6 => set(&mut tr.fm[0], 0x20, v << 5),
                    0x7 => set(&mut tr.fm[1], 0x0f, v),
                    0x8 => set(&mut tr.fm[3], 0xc0, v << 6),
                    0x9 => set(&mut tr.fm[1], 0x80, v << 7),
                    0xa => set(&mut tr.fm[1], 0x40, v << 6),
                    0xb => set(&mut tr.fm[1], 0x10, v << 4),
                    0xc => set(&mut tr.fm[1], 0x20, v << 5),
                    _ => return,
                }
                self.write_connection_group(t, out);
            }
            // A tremor remembers the levels as they are now (after this
            // row's instrument, before column 2) unless the column's last
            // effect (AT2 `last_effect`, across empty rows) was a tremor:
            // then it keeps the old ones (MEASURED; its count restarts on
            // the previous-row rule all the same).
            fx::TREMOR if param >> 4 != 0 && param & 15 != 0 => {
                let tr = &mut self.tracks[t];
                if tr.cols[c].table_fx != fx::TREMOR {
                    tr.cols[c].tremor_saved = (tr.vol_mod, tr.vol_car);
                }
            }
            fx::SET_SPEED if param != 0 => {
                self.speed = param;
            }
            fx::SET_TEMPO if param != 0 => {
                self.tempo = param;
            }
            fx::POS_JUMP => {
                self.jump = Some(((param & 0x7f) as usize, 0));
            }
            fx::PAT_BREAK => {
                self.jump = Some((self.order_pos + 1, param as usize));
            }
            fx::PORTA => {
                let target = (1..=97)
                    .contains(&cell.note)
                    .then(|| self.note_freq(t, cell.note));
                let col = &mut self.tracks[t].cols[c];
                col.porta_on = target.is_some() || col.table_fx == fx::PORTA;
                if col.porta_on {
                    // A zero parameter reuses the last 03's; else the speed
                    // is 0 (AT2 `porta_table.speed`, shared with 05 and 10).
                    let p = if param == 0 && col.table_fx == fx::PORTA {
                        col.table_param
                    } else {
                        param
                    };
                    col.param = p;
                    col.porta_speed = p;
                }
                if let Some(f) = target {
                    col.porta_target = f;
                }
            }
            // MEASURED: a zero parameter reuses the last one; otherwise both
            // nibbles are taken (04 20 is speed 2, depth 0).
            fx::VIBRATO | fx::EXTRA_FINE_VIB => {
                let col = &mut self.tracks[t].cols[c];
                // Reused only after 04 or 2B themselves (empty rows between
                // included), not after 06, 11 or a one-shot effect (AT2
                // `play_line` takes `last_effect`'s; MEASURED).
                let p = if param == 0 && matches!(col.table_fx, fx::VIBRATO | fx::EXTRA_FINE_VIB) {
                    col.table_param
                } else {
                    param
                };
                col.param = p;
                col.vib_speed = p >> 4;
                col.vib_depth = p & 15;
            }
            fx::TREMOLO | fx::EXTRA_FINE_TREM => {
                let col = &mut self.tracks[t].cols[c];
                let p = if param == 0 && matches!(col.table_fx, fx::TREMOLO | fx::EXTRA_FINE_TREM) {
                    col.table_param
                } else {
                    param
                };
                col.param = p;
                col.trem_speed = p >> 4;
                col.trem_depth = p & 15;
            }
            _ => {}
        }
    }

    /// Steps the track's macros by one timer tick (every tick, at the macro
    /// speed-up rate; MEASURED after all effects of the tick).
    fn run_macros(&mut self, t: usize, out: &mut impl RegisterSink) {
        self.run_fm_macro(t, out);
        self.run_arp_macro(t, out);
        // Effects this tick and the tables before it count (AT2
        // `change_frequency` resets the vibrato table).
        let touched = std::mem::take(&mut self.tracks[t].pitch_touched);
        self.run_vib_macro(t, touched, out);
    }

    fn run_fm_macro(&mut self, t: usize, out: &mut impl RegisterSink) {
        let st = self.tracks[t].fm_macro;
        if !st.active {
            return;
        }
        let ins = self.tracks[t].instrument as usize - 1;
        let m = &self.song.fm_macros[ins];
        // A step lasts its duration in ticks; a zero duration is skipped
        // but still takes a tick (MEASURED).
        let dur = if st.pos == 0 {
            0
        } else {
            m.steps[st.pos as usize - 1].duration as u16
        };
        if st.pos != 0 && st.count + 1 < dur {
            self.tracks[t].fm_macro.count += 1;
            return;
        }
        let (pos, count) = match macro_next(
            &st,
            m.length,
            m.loop_begin,
            m.loop_length,
            m.keyoff_pos,
            self.tracks[t].koff_loop,
        ) {
            Next::Step(p) => (p, 0),
            Next::Hold => return,
            Next::Stop => {
                self.tracks[t].fm_macro.active = false;
                return;
            }
        };
        let tr = &mut self.tracks[t].fm_macro;
        tr.pos = pos;
        tr.count = count;
        tr.jump = 0;
        let step = m.steps[pos as usize - 1];
        if step.duration > 0 {
            self.apply_fm_step(t, ins, &step, out);
        }
    }

    fn apply_fm_step(
        &mut self,
        t: usize,
        ins: usize,
        step: &FmMacroStep,
        out: &mut impl RegisterSink,
    ) {
        let off = self
            .song
            .disabled_fm_columns
            .get(ins)
            .copied()
            .unwrap_or([0; 28]);
        let on = |col: usize| off[col] == 0;
        let (m, c) = self.ops(t);
        // The step's level column is a volume: 63 is loudest (MEASURED, and
        // the corpus agrees: instruments at TL 0 carry 63 there).
        if on(5) {
            self.tracks[t].vol_mod = 63 - (step.fm[2] & 0x3f);
            self.write_mod_level(t, out);
        }
        if on(17) {
            self.tracks[t].vol_car = 63 - (step.fm[3] & 0x3f);
            self.write_car_level(t, out);
        }
        let tr = &mut self.tracks[t];
        let mut merge = |byte: usize, col: usize, mask: u8| {
            if on(col) {
                tr.fm[byte] = (tr.fm[byte] & !mask) | (step.fm[byte] & mask);
            }
        };
        for (op, base) in [(0usize, 0usize), (1, 12)] {
            merge(4 + op, base, 0xf0);
            merge(4 + op, base + 1, 0x0f);
            merge(6 + op, base + 2, 0xf0);
            merge(6 + op, base + 3, 0x0f);
            merge(8 + op, base + 4, 0x07);
            merge(2 + op, base + 6, 0xc0);
            merge(op, base + 7, 0x0f);
            merge(op, base + 8, 0x80);
            merge(op, base + 9, 0x40);
            merge(op, base + 10, 0x10);
            merge(op, base + 11, 0x20);
        }
        merge(10, 24, 0x01);
        merge(10, 25, 0x0e);
        if on(27) && !tr.pan_lock {
            tr.panning = step.panning;
        }
        let fm = tr.fm;
        out.write(0x60 + m, fm[4]);
        out.write(0x80 + m, fm[6]);
        out.write(0xe0 + m, fm[8]);
        out.write(0x60 + c, fm[5]);
        out.write(0x80 + c, fm[7]);
        out.write(0xe0 + c, fm[9]);
        out.write(0x20 + m, fm[0]);
        out.write(0x20 + c, fm[1]);
        let pan = pan_bits(self.tracks[t].panning);
        out.write(0xc0 + reg_channel(self.channel(t)), (fm[10] & 0x0f) | pan);
        self.write_volume(t, out);
        // MEASURED: the first step also keys the note on again when the
        // instrument itself has no envelope (AR/DR and SL/RR all zero) and
        // the macro may set one; so does the retrigger bit (0x80) of any
        // step. Bit 0x40 restarts the envelope only, bit 0x20 silences the
        // pitch until a step without it (AT2 `macro_poll_proc`).
        let bare = self.song.instruments[ins].fm[4..8] == [0; 4]
            && ![0, 1, 2, 3, 12, 13, 14, 15].iter().all(|&c| !on(c));
        let first = self.tracks[t].fm_macro.pos == 1;
        let flags = step.fm[10];
        if flags & 0x80 != 0 || (first && bare) {
            // A 4-op pair retriggers from its second track only, which
            // also restarts the first track's tables from nothing.
            match self.pair(t) {
                Some((a, _)) if a == t => {}
                pair => {
                    self.retrigger(t, out);
                    if let Some((a, _)) = pair {
                        self.start_macros(a);
                        self.tracks[a].vib_freq = 0;
                    }
                }
            }
        } else if flags & 0x40 != 0 {
            let ch = self.key_reg(t);
            out.write(0xb0 + ch, 0);
            let f = self.tracks[t].freq;
            self.write_freq(t, f, out);
        } else if flags & 0x20 != 0 {
            let f = self.tracks[t].freq;
            if f != 0 {
                self.tracks[t].zero_freq = f;
                self.tracks[t].freq = 0;
                self.write_freq(t, 0, out);
            }
        } else if self.tracks[t].zero_freq != 0 {
            let f = std::mem::take(&mut self.tracks[t].zero_freq);
            self.tracks[t].freq = f;
            self.write_freq(t, f, out);
        }
        if on(26) && step.freq_slide != 0 {
            self.slide(t, step.freq_slide as i32, out);
        }
    }

    /// Strikes the track's note again (the FM macro's retrigger): at the
    /// note's pitch when keyed on, else the pitch is only rewritten.
    /// (AT2 `output_note` with the track's last note: nothing before any
    /// note, only the pitch after a key-off, else a key-on at the note.)
    fn retrigger(&mut self, t: usize, out: &mut impl RegisterSink) {
        let ch = self.key_reg(t);
        let note = self.tracks[t].note;
        if note == 0 && !self.tracks[t].keyed_off {
            return;
        }
        if !self.tracks[t].keyed_off {
            let f = self.note_freq(t, note);
            self.tracks[t].freq = f;
            self.tracks[t].out_freq = f;
            out.write(0xb0 + ch, 0);
            out.write(0xa0 + ch, f as u8);
            out.write(0xb0 + ch, 0x20 | (f >> 8) as u8);
            self.share_pitch(t);
        } else {
            let f = self.tracks[t].freq;
            self.write_freq(t, f, out);
        }
    }

    fn run_arp_macro(&mut self, t: usize, out: &mut impl RegisterSink) {
        let st = self.tracks[t].arp_macro;
        if !st.active {
            return;
        }
        let a = &self.song.arpeggio_macros[self.tracks[t].arp_table as usize - 1];
        let tr = &mut self.tracks[t].arp_macro;
        tr.count += 1;
        if a.speed == 0 || tr.count < a.speed as u16 {
            return;
        }
        tr.count = 0;
        let koff_loop = self.tracks[t].koff_loop;
        let tr = &mut self.tracks[t].arp_macro;
        let pos = match macro_next(
            &st,
            a.length,
            a.loop_begin,
            a.loop_length,
            a.keyoff_pos,
            koff_loop,
        ) {
            Next::Step(p) => p,
            Next::Hold => return,
            Next::Stop => {
                tr.active = false;
                return;
            }
        };
        tr.pos = pos;
        tr.jump = 0;
        let v = a.data[pos as usize - 1];
        // AdPlug tests element `pos` but adds element `pos + 1` for a
        // relative step (an off-by-one: a table 0, 3, 7 plays 0, 7, 0; the
        // corpus's chord tables show AT2 means 0, 3, 7). Fixed notes line up.
        let add = if self.adplug_quirks {
            a.data.get(pos as usize).copied().unwrap_or(0)
        } else {
            v
        };
        let note = self.tracks[t].arp_note & 0x7f;
        let f = if v & 0x80 != 0 {
            self.note_freq(t, (v & 0x7f).clamp(1, 97))
        } else if v == 0 || add == 0 {
            // MEASURED: before any note (a delayed note's instrument starts
            // the table) the base is note 0, which AdPlug plays as the top
            // pitch, block 7 F-number 0x2AE.
            if note == 0 {
                FREQ_MAX
            } else {
                self.note_freq(t, note)
            }
        } else {
            self.note_freq(t, (note + (add & 0x7f)).clamp(1, 97))
        };
        self.tracks[t].freq = f;
        self.write_freq(t, f, out);
    }

    /// One tick of the vibrato table (AT2 `macro_poll_proc`): a step every
    /// `speed` ticks after the delay, each writing the base pitch moved by
    /// the step's value (towards note 97 or note 1). A pitch set by an
    /// effect this tick becomes the base and restarts the table.
    fn run_vib_macro(&mut self, t: usize, touched: bool, out: &mut impl RegisterSink) {
        let table = self.tracks[t].vib_table as usize;
        let Some(v) = self.song.vibrato_macros.get(table.wrapping_sub(1)) else {
            return;
        };
        if !(self.tracks[t].vib_macro.active
            || (touched && v.length > 0 && st_ran(&self.tracks[t].vib_macro)))
        {
            return;
        }
        if touched {
            let tr = &mut self.tracks[t];
            tr.vib_freq = tr.out_freq;
            tr.vib_macro.active = true;
            tr.vib_macro.count = 1;
            tr.vib_macro.pos = 0;
            tr.vib_macro.jump = 0;
        }
        if v.speed == 0 {
            return;
        }
        let st = self.tracks[t].vib_macro;
        if st.count != v.speed as u16 {
            self.tracks[t].vib_macro.count += 1;
            return;
        }
        if st.delay > 0 {
            self.tracks[t].vib_macro.delay -= 1;
            return;
        }
        let koff_loop = self.tracks[t].koff_loop;
        let tr = &mut self.tracks[t].vib_macro;
        tr.count = 1;
        let pos = match macro_next(
            &st,
            v.length,
            v.loop_begin,
            v.loop_length,
            v.keyoff_pos,
            koff_loop,
        ) {
            Next::Step(p) => p,
            Next::Hold => return,
            Next::Stop => {
                tr.active = false;
                return;
            }
        };
        tr.pos = pos;
        tr.jump = 0;
        let d = v.data[pos as usize - 1];
        let tr = &mut self.tracks[t];
        let base = tr.vib_freq & 0x1fff;
        let f = match d {
            0 => base,
            d if d > 0 => shift_up(base, d as u16).min(FREQ_MAX),
            d => shift_down(base, (-(d as i16)) as u16).max(FREQ_MIN),
        } & 0x1fff;
        tr.freq = f;
        self.put_freq(t, f, out);
    }

    /// The Z (0x23) commands.
    fn extended(&mut self, t: usize, c: usize, param: u8, out: &mut impl RegisterSink) {
        let v = param & 15;
        match param >> 4 {
            // MEASURED: each sets its own depth bit and takes the other
            // from the song flags, not from the last write.
            0x0 => {
                self.bd = (self.bd_value() & !0x80) | (v & 1) << 7;
                out.write(0xbd, self.bd);
            }
            0x1 => {
                self.bd = (self.bd_value() & !0x40) | (v & 1) << 6;
                out.write(0xbd, self.bd);
            }
            0x2..=0x9 => {
                // AR, DR, SL, RR of the modulator, then of the carrier.
                let n = (param >> 4) - 2;
                let op = (n / 4) as usize;
                let byte = if n % 4 < 2 { 4 + op } else { 6 + op };
                let tr = &mut self.tracks[t];
                tr.fm[byte] = if n.is_multiple_of(2) {
                    (tr.fm[byte] & 0x0f) | v << 4
                } else {
                    (tr.fm[byte] & 0xf0) | v
                };
                self.write_envelope_group(t, op, out);
            }
            0xa => {
                let tr = &mut self.tracks[t];
                tr.fm[10] = (tr.fm[10] & !0x0e) | (v & 7) << 1;
                self.write_connection_group(t, out);
            }
            0xb => {
                self.tracks[t].panning = v.min(2);
                self.write_connection_group(t, out);
            }
            0xc | 0xd => {
                let tr = &mut self.tracks[t];
                if v == 0 {
                    tr.loop_row = self.row;
                } else if tr.loop_count == 0 {
                    tr.loop_count = v;
                    self.jump = Some((self.order_pos, tr.loop_row));
                } else {
                    tr.loop_count -= 1;
                    if tr.loop_count > 0 {
                        self.jump = Some((self.order_pos, tr.loop_row));
                    }
                }
                let _ = c;
            }
            0xe => {
                if v == 0 || v == 1 {
                    self.tracks[t].koff_loop = v == 1;
                }
                if v == 2 || v == 3 {
                    self.tracks[t].porta_fk = v == 3;
                }
                if v == 5 || v == 6 {
                    // MEASURED: for both tracks of the pair.
                    if let Some((a, b)) = self.pair(t) {
                        self.tracks[a].lock4 = v == 6;
                        self.tracks[b].lock4 = v == 6;
                    }
                }
                if v == 4 {
                    // Restart the envelope: key off and on at the pitch.
                    let ch = self.key_reg(t);
                    let f = self.tracks[t].freq;
                    out.write(0xb0 + ch, 0);
                    out.write(0xa0 + ch, f as u8);
                    out.write(0xb0 + ch, 0x20 | (f >> 8) as u8);
                    self.tracks[t].key_on = true;
                    self.share_pitch(t);
                }
            }
            _ => match v {
                0x0 => self.release_sound(t, out),
                0x1 => {
                    if let Some(d) = self.song.instruments.get(self.voice(t) - 1) {
                        self.tracks[t].vol_mod = self.ins_level(t, &d.fm, 0);
                        self.tracks[t].vol_car = self.ins_level(t, &d.fm, 1);
                        self.write_volume(t, out);
                    }
                }
                0x2 => self.tracks[t].vol_lock = true,
                0x3 => self.tracks[t].vol_lock = false,
                0x4 => self.tracks[t].peak_lock = true,
                0x5 => self.tracks[t].peak_lock = false,
                0x6 => self.tracks[t].vslide_mode = 2,
                0x7 => self.tracks[t].vslide_mode = 1,
                0x8 => self.tracks[t].vslide_mode = 0,
                0x9 => self.tracks[t].pan_lock = true,
                0xa => self.tracks[t].pan_lock = false,
                0xb => {
                    let f = self.tracks[t].freq;
                    self.write_freq(t, f, out);
                }
                0xc => self.write_volume(t, out),
                _ => {}
            },
        }
    }

    /// Effect parts that run once per row, right after the track's note
    /// (MEASURED: they land between this track's key-on and the next
    /// track's): fine slides and fine volume slides.
    fn note_pass_effects(&mut self, t: usize, out: &mut impl RegisterSink) {
        for c in 0..2 {
            let Column {
                fx,
                param,
                vib_fine,
                trem_fine,
                ..
            } = self.tracks[t].cols[c];
            let mem = self.tracks[t].cols[c].slide_mem as i32;
            match fx {
                // AT2 `update_fine_effects`: a fine (ZFD/ZFE) vibrato or
                // tremolo steps here, once per row, and not on the ticks.
                fx::VIBRATO | fx::VIB_VSLIDE if vib_fine => self.vibrato(t, c, out),
                fx::VIB_VSLIDE_FINE if vib_fine => {
                    self.vslide(t, param, out);
                    self.vibrato(t, c, out);
                }
                fx::TREMOLO if trem_fine => self.tremolo(t, c, out),
                fx::FINE_UP => self.slide(t, param as i32, out),
                fx::FINE_DOWN => self.slide_down(t, param, out),
                fx::OLD_RAW_FINE => {
                    let by = if param >> 4 != 0 {
                        (param >> 4) as i32
                    } else {
                        -((param & 15) as i32)
                    };
                    let f = ((self.tracks[t].freq as i32 + by) & 0x1fff) as u16;
                    self.tracks[t].freq = f;
                    self.write_freq(t, f, out);
                }
                // MEASURED: 18 and 19 do nothing at all, their volume part
                // included, on a track that never played.
                fx::ARP_VSLIDE_FINE if self.tracks[t].freq == 0 => {}
                fx::VSLIDE_FINE
                | fx::PORTA_VSLIDE_FINE
                | fx::VIB_VSLIDE_FINE
                | fx::ARP_VSLIDE_FINE
                | fx::SLIDE_UP_VSLIDE_FINE
                | fx::SLIDE_DOWN_VSLIDE_FINE => self.vslide(t, param, out),
                fx::FINE_UP_VSLIDE => self.slide(t, mem, out),
                fx::FINE_DOWN_VSLIDE => self.slide_down(t, mem as u8, out),
                fx::FINE_UP_VSLIDE_FINE => {
                    self.slide(t, mem, out);
                    self.vslide(t, param, out);
                }
                fx::FINE_DOWN_VSLIDE_FINE => {
                    self.slide_down(t, mem as u8, out);
                    self.vslide(t, param, out);
                }
                _ => {}
            }
        }
    }

    /// Effects run on every row tick, the row's own included. The order of
    /// the parts inside a combined effect is MEASURED per effect.
    fn tick_effects(&mut self, t: usize, xf: bool, out: &mut impl RegisterSink) {
        for c in 0..2 {
            let col = self.tracks[t].cols[c];
            let (fx, param) = if col.glide != (0, 0) {
                col.glide
            } else {
                (col.fx, col.param)
            };
            let first = self.row_tick == 0;
            let (vib_fine, trem_fine) = (col.vib_fine, col.trem_fine);

            let mem = col.slide_mem as i32;
            let arp_param = col.arp_mem;
            if c == 0 {
                let tr = self.tracks[t];
                if tr.note_delay != 0 && self.row_tick == tr.note_delay as u16 {
                    self.tracks[t].note_delay = 0;
                    match tr.delayed_note {
                        0 => {}
                        0xfe => self.write_freq(t, tr.freq, out),
                        n => self.play_note(t, n, out),
                    }
                }
                if tr.note_cut != 0 && self.row_tick == tr.note_cut as u16 {
                    self.tracks[t].note_cut = 0;
                    let f = tr.freq;
                    self.tracks[t].key_on = false;
                    self.tracks[t].keyed_off = true;
                    self.write_freq(t, f, out);
                    // AT2's tables watch the key bit: a cut releases them
                    // like a note off (MEASURED).
                    self.release_macros(t);
                }
            }
            match fx {
                fx::EXTENDED2 => match param >> 4 {
                    0x6 => self.global_volume_slide((param & 15) as i32, out),
                    0x7 => self.global_volume_slide(-((param & 15) as i32), out),
                    0x8 if first => self.global_volume_slide((param & 15) as i32, out),
                    0x9 if first => self.global_volume_slide(-((param & 15) as i32), out),
                    0xa if xf => self.global_volume_slide((param & 15) as i32, out),
                    0xb if xf => self.global_volume_slide(-((param & 15) as i32), out),
                    0xc if xf => self.vslide(t, (param & 15) << 4, out),
                    0xd if xf => self.vslide(t, param & 15, out),
                    0xe if xf => self.slide(t, (param & 15) as i32, out),
                    0xf if xf => self.slide_down(t, param & 15, out),
                    _ => {}
                },
                fx::EXTRA_FINE_ARP if xf => self.arpeggio(t, c, param, out),
                // A fine (ZFD/ZFE) extra-fine vibrato or tremolo never runs:
                // AT2 skips it here and has no row case for it.
                fx::EXTRA_FINE_VIB if xf && !vib_fine => self.vibrato(t, c, out),
                fx::EXTRA_FINE_TREM if xf && !trem_fine => self.tremolo(t, c, out),
                fx::GLOBAL_SLIDE_UP => self.slide(t, param as i32, out),
                fx::GLOBAL_SLIDE_DOWN => self.slide_down(t, param, out),
                fx::ARPEGGIO if param != 0 => self.arpeggio(t, c, param, out),
                fx::SLIDE_UP => self.slide(t, param as i32, out),
                fx::SLIDE_DOWN => self.slide_down(t, param, out),
                fx::PORTA | fx::PORTA_VSLIDE_FINE => self.porta(t, c, out),
                fx::VIBRATO | fx::VIB_VSLIDE_FINE if !vib_fine => self.vibrato(t, c, out),
                fx::PORTA_VSLIDE => {
                    self.vslide(t, param, out);
                    self.porta(t, c, out);
                }
                fx::VIB_VSLIDE => {
                    self.vslide(t, param, out);
                    if !vib_fine {
                        self.vibrato(t, c, out);
                    }
                }
                fx::TREMOLO if !trem_fine => self.tremolo(t, c, out),
                fx::RETRIG => self.retrig(t, c, param, 0, out),
                fx::MULTI_RETRIG => self.retrig(t, c, param >> 4, param & 15, out),
                fx::TREMOR => self.tremor(t, c, param, out),
                fx::VSLIDE | fx::FINE_UP_VSLIDE | fx::FINE_DOWN_VSLIDE => {
                    self.vslide(t, param, out)
                }
                fx::ARP_VSLIDE | fx::ARP_VSLIDE_FINE if self.tracks[t].freq == 0 => {}
                fx::ARP_VSLIDE => {
                    self.vslide(t, param, out);
                    self.arpeggio(t, c, arp_param, out);
                }
                fx::ARP_VSLIDE_FINE => self.arpeggio(t, c, arp_param, out),
                fx::SLIDE_UP_VSLIDE => {
                    self.slide(t, mem, out);
                    self.vslide(t, param, out);
                }
                fx::SLIDE_DOWN_VSLIDE => {
                    self.slide_down(t, mem as u8, out);
                    self.vslide(t, param, out);
                }
                fx::SLIDE_UP_VSLIDE_FINE => self.slide(t, mem, out),
                fx::SLIDE_DOWN_VSLIDE_FINE => self.slide_down(t, mem as u8, out),
                _ => {}
            }
        }
    }

    /// Key off and on again every `every + 1` ticks, the first after
    /// `every - 1` (MEASURED); 1A first moves the volume by `vol` (the
    /// ProTracker steps: 1-5 down 1..16, 6 ×2/3, 7 ×1/2, 9-D up 1..16,
    /// E ×3/2, F ×2).
    fn retrig(&mut self, t: usize, c: usize, every: u8, vol: u8, out: &mut impl RegisterSink) {
        if every == 0 {
            return;
        }
        let col = &mut self.tracks[t].cols[c];
        if col.retrig_count < every {
            col.retrig_count += 1;
            return;
        }
        col.retrig_count = 0;
        if vol != 0 && vol != 8 {
            let additive = self.additive(t);
            let tr = &mut self.tracks[t];
            let step = |tl: u8| -> u8 {
                let v = 63 - tl as i32;
                let v = match vol {
                    1..=5 => v - (1 << (vol - 1)),
                    6 => v * 2 / 3,
                    7 => v / 2,
                    9..=0xd => v + (1 << (vol - 9)),
                    0xe => v * 3 / 2,
                    _ => v * 2,
                };
                63 - v.clamp(0, 63) as u8
            };
            tr.vol_car = step(tr.vol_car);
            if additive {
                tr.vol_mod = step(tr.vol_mod);
            }
            self.write_scaled_levels(t, out);
        }
        // After a key-off AT2 passes the note with its key-off flag, out of
        // range: the pitch is only rewritten (MEASURED).
        if self.tracks[t].keyed_off {
            let f = self.tracks[t].freq;
            self.write_freq(t, f, out);
            return;
        }
        // MEASURED: it strikes the row note again, at the note's own pitch.
        let note = self.tracks[t].note;
        let f = if note != 0 {
            self.note_freq(t, note)
        } else {
            self.tracks[t].freq
        };
        let ch = self.key_reg(t);
        self.tracks[t].freq = f;
        self.tracks[t].out_freq = f;
        out.write(0xb0 + ch, 0);
        out.write(0xa0 + ch, f as u8);
        out.write(0xb0 + ch, 0x20 | (f >> 8) as u8);
        self.tracks[t].key_on = true;
        self.share_pitch(t);
        // AT2 `output_note` with restart_macro: the instrument's tables start
        // again, unless ZFF is on the row (MEASURED).
        if note != 0 {
            let keep = self.tracks[t]
                .cols
                .iter()
                .any(|c| (c.fx, c.param) == (fx::EXTENDED, 0xff));
            if keep {
                self.tracks[t].arp_note = note;
            } else {
                self.start_macros(t);
            }
        }
    }

    /// On for x ticks, off (silent) for y (MEASURED: off writes the carrier,
    /// and the modulator when additive, at level 63; on writes both back).
    fn tremor(&mut self, t: usize, c: usize, param: u8, out: &mut impl RegisterSink) {
        // AT2 runs a tremor only when both counts are set.
        let (on, off) = (param >> 4, param & 15);
        if on == 0 || off == 0 {
            return;
        }
        let additive = self.additive(t);
        let col = &mut self.tracks[t].cols[c];
        col.tremor_active = true;
        col.tremor_count += 1;
        if !col.tremor_off && col.tremor_count > on {
            col.tremor_off = true;
            col.tremor_count = 1;
            let (m, car) = self.ops(t);
            let tr = &mut self.tracks[t];
            tr.vol_car = 63;
            let (vc, vm) = ((tr.fm[3] & 0xc0) | 0x3f, (tr.fm[2] & 0xc0) | 0x3f);
            self.put_level(t, 1, car, vc, out);
            if additive {
                self.tracks[t].vol_mod = 63;
                self.put_level(t, 0, m, vm, out);
            }
        } else if col.tremor_off && col.tremor_count > off {
            col.tremor_off = false;
            col.tremor_count = 1;
            let tr = &mut self.tracks[t];
            (tr.vol_mod, tr.vol_car) = tr.cols[c].tremor_saved;
            self.write_volume(t, out);
        }
    }

    fn arpeggio(&mut self, t: usize, c: usize, param: u8, out: &mut impl RegisterSink) {
        let phase = self.tracks[t].cols[c].arp_phase;
        let add = match phase {
            0 => 0,
            1 => param >> 4,
            _ => param & 15,
        };
        self.tracks[t].cols[c].arp_phase = (phase + 1) % 3;
        let f = self.arp_freq(t, add);
        // AT2 `arpeggio` goes through `change_frequency`: each step is the
        // pitch a slide moves from afterwards (MEASURED).
        self.tracks[t].freq = f;
        self.write_freq(t, f, out);
    }

    /// The row note's pitch plus `add` semitones (MEASURED: an arpeggio
    /// works from the note, even when a portamento has moved the pitch).
    fn arp_freq(&self, t: usize, add: u8) -> u16 {
        let tr = &self.tracks[t];
        if tr.note == 0 {
            return tr.freq;
        }
        let n = (tr.note & 0x7f) + add;
        self.note_freq(t, n.min(97))
    }

    fn slide(&mut self, t: usize, by: i32, out: &mut impl RegisterSink) {
        // MEASURED: a track that never played is left alone.
        if self.tracks[t].freq == 0 {
            return;
        }
        let Some(f) = step_freq(self.tracks[t].freq, by) else {
            return;
        };
        self.tracks[t].freq = f;
        self.write_freq(t, f, out);
    }

    /// A downward slide by `by` (kept apart so a slide down by 0 still
    /// takes AT2's downward path).
    fn slide_down(&mut self, t: usize, by: u8, out: &mut impl RegisterSink) {
        let Some(f) = porta_down(self.tracks[t].freq, by as u16, FREQ_MIN) else {
            return;
        };
        self.tracks[t].freq = f;
        self.write_freq(t, f, out);
    }

    fn porta(&mut self, t: usize, c: usize, out: &mut impl RegisterSink) {
        let col = self.tracks[t].cols[c];
        let speed = col.porta_speed as i32;
        let (cur, target) = (self.tracks[t].freq, col.porta_target);
        // MEASURED: with no target, 03 does nothing while 05 and 10 write the
        // pitch unchanged; an idle channel is left alone.
        if cur == 0 || (col.fx == fx::PORTA && !col.porta_on) {
            return;
        }
        if target == 0 {
            if self.tracks[t].cols[c].fx != fx::PORTA {
                self.write_freq(t, cur, out);
            }
            return;
        }
        if cur == target {
            return;
        }
        // AT2 `tone_portamento`: a slide towards the target that stops at
        // it (compared as block and F-number together).
        let f = if cur & 0x1fff < target {
            porta_up(cur, speed as u16, target)
        } else {
            porta_down(cur, speed as u16, target)
        };
        let Some(f) = f else {
            return;
        };
        self.tracks[t].freq = f;
        self.write_freq(t, f, out);
    }

    fn vibrato(&mut self, t: usize, c: usize, out: &mut impl RegisterSink) {
        // The position moves even on an idle track (only the write is
        // skipped there).
        let col = &mut self.tracks[t].cols[c];
        col.vib_pos = col.vib_pos.wrapping_add(col.vib_speed) & 63;
        let amount = (VIB_TABLE[(col.vib_pos & 31) as usize] as i32 * col.vib_depth as i32) >> 7;
        // AT2 `vibrato`: a porta by the table amount towards note 1 or 97,
        // written without moving the base pitch.
        let cur = self.tracks[t].freq;
        let f = if col.vib_pos & 32 == 0 {
            porta_down(cur, amount as u16, FREQ_MIN)
        } else {
            porta_up(cur, amount as u16, FREQ_MAX)
        };
        if let Some(f) = f {
            self.write_freq(t, f, out);
            // AT2 puts back only this track's base pitch: in a 4-op pair
            // the other track's base keeps the vibrato's pitch (MEASURED).
            if let Some((a, b)) = self.pair(t) {
                let o = if t == a { b } else { a };
                self.tracks[o].freq = f;
            }
        }
    }

    fn tremolo(&mut self, t: usize, c: usize, out: &mut impl RegisterSink) {
        let col = &mut self.tracks[t].cols[c];
        col.trem_pos = col.trem_pos.wrapping_add(col.trem_speed) & 63;
        let amount = (VIB_TABLE[(col.trem_pos & 31) as usize] as i32 * col.trem_depth as i32) >> 7;
        // AdPlug never turns a tremolo in the second column: it only ever
        // makes it louder (MEASURED for 16 and 2C, not for vibrato). AT2
        // treats both columns alike, which play follows.
        let louder_only = c == 1 && self.adplug_quirks;
        let by = if col.trem_pos & 32 == 0 && !louder_only {
            amount
        } else {
            -amount
        };
        let tr = self.tracks[t];
        let m = (tr.vol_mod as i32 + by).clamp(0, 63) as u8;
        let car = (tr.vol_car as i32 + by).clamp(0, 63) as u8;
        let (mr, cr) = self.ops(t);
        let v = (tr.fm[3] & 0xc0) | self.car_out(t, car);
        self.put_level(t, 1, cr, v, out);
        if self.additive(t) || self.single_op(t).is_some() {
            let v = (tr.fm[2] & 0xc0) | self.mod_out(t, m);
            self.put_level(t, 0, mr, v, out);
        }
    }

    /// Volume slide: high nibble up (louder), else low nibble down. It
    /// moves the carrier, and the modulator too when additive, unless ZF6-8
    /// or the track's lock flags choose (MEASURED: carrier written first).
    fn vslide(&mut self, t: usize, param: u8, out: &mut impl RegisterSink) {
        let by = if param >> 4 != 0 {
            -((param >> 4) as i32)
        } else if param & 15 != 0 {
            (param & 15) as i32
        } else {
            return;
        };
        if let Some(pair) = self.locked_pair(t) {
            // MEASURED: each output operator's own level moves, and is
            // written as it is (relative only under volume scaling).
            for (u, op) in self.out_ops(pair) {
                let tr = &mut self.tracks[u];
                let v = if op == 0 {
                    &mut tr.vol_mod
                } else {
                    &mut tr.vol_car
                };
                *v = (*v as i32 + by).clamp(0, 63) as u8;
                let l = *v;
                self.write_op_level(u, op, l, false, out);
            }
            return;
        }
        let additive = self.additive(t);
        let both = additive || self.single_op(t).is_some();
        let (car, modu) = match self.tracks[t].vslide_mode {
            1 => (true, false),
            2 => (false, true),
            3 => (true, true),
            _ => (true, both),
        };
        // Peak lock: no louder than the instrument's own levels (MEASURED).
        let (floor_mod, floor_car) = match self
            .song
            .instruments
            .get((self.tracks[t].instrument as usize).wrapping_sub(1))
        {
            Some(d) if self.tracks[t].peak_lock => {
                ((d.fm[2] & 0x3f) as i32, (d.fm[3] & 0x3f) as i32)
            }
            _ => (0, 0),
        };
        let tr = &mut self.tracks[t];
        if car {
            tr.vol_car = (tr.vol_car as i32 + by).clamp(floor_car.min(tr.vol_car as i32), 63) as u8;
            self.write_car_level(t, out);
        }
        if modu {
            let tr = &mut self.tracks[t];
            tr.vol_mod = (tr.vol_mod as i32 + by).clamp(floor_mod.min(tr.vol_mod as i32), 63) as u8;
            self.write_mod_level(t, out);
        }
    }
}

/// An instrument with nothing in it, finetune included.
fn is_empty(d: &super::model::Instrument) -> bool {
    d.fm == [0; 11] && d.panning == 0 && d.finetune == 0 && d.voice_type == 0
}

/// The v9+ effect a version 1-8 effect plays as (MEASURED, each old number
/// and extended command against the new ones in AdPlug's AT2 player,
/// `oracle/dev/mapfx.py` and `mapext.py`). Versions 1-4 have 16 effects,
/// the last one extended; 5-8 have the v9 set up to 0x23, with one
/// difference (0x16) and no pattern loop.
fn convert_old_effect(version: u8, (fx, p): (u8, u8)) -> (u8, u8) {
    let (x, y) = (p >> 4, p & 15);
    if version >= 5 {
        return match fx {
            // Once per row, x up else y down, added to the block and
            // F-number as one number (no wrap at the octave edges).
            0x16 if p != 0 => (fx::OLD_RAW_FINE, p),
            0x16 => (0, 0),
            fx::EXTENDED if x == 0xc || x == 0xd => (0, 0),
            _ => (fx, p),
        };
    }
    match fx {
        0x0 => (fx::ARPEGGIO, p),
        0x1 => (fx::SLIDE_UP, p),
        0x2 => (fx::SLIDE_DOWN, p),
        0x3 => (fx::FINE_UP, p),
        0x4 => (fx::FINE_DOWN, p),
        0x5 => (fx::PORTA, p),
        0x6 => (fx::PORTA_VSLIDE, p),
        0x7 => (fx::VIBRATO, p),
        0x8 => (fx::VIB_VSLIDE, p),
        // Nibble volumes in steps of 4: the carrier's, else the modulator's.
        0x9 if x != 0 => (fx::SET_CAR_VOL, 4 * x + 3),
        0x9 if y != 0 => (fx::SET_MOD_VOL, 4 * y + 3),
        0x9 => (0, 0),
        0xa => (fx::SET_INS_VOL, p),
        0xb => (fx::PAT_BREAK, p),
        0xc => (fx::POS_JUMP, p),
        0xd => (fx::SET_SPEED, p),
        0xe => (fx::SET_TEMPO, p),
        _ => match x {
            0x0 | 0x1 => (fx::EXTENDED, p),
            0x2 => (fx::SET_WAVEFORM, y << 4 | 0x0f),
            0x3 => (fx::EXTENDED2, 0x40 | y),
            0x4 => (fx::EXTENDED2, 0x50 | y),
            0x5 => (fx::VSLIDE, y << 4),
            0x6 => (fx::VSLIDE, y),
            0x7 => (fx::VSLIDE_FINE, y << 4),
            0x8 => (fx::VSLIDE_FINE, y),
            0x9 => (fx::RETRIG, y + 1),
            0xa..=0xd => (fx::EXTENDED, (x - 8) << 4 | y),
            0xe => (fx::EXTENDED, 0xa0 | y),
            _ => (fx::EXTENDED, 0xf0),
        },
    }
}

/// Effects that act once when the row is read and leave no running
/// effect behind (AT2 sets no `effect_table` for them).
fn one_shot(fx: u8, param: u8) -> bool {
    fx != fx::PORTA
        && (fx != fx::EXTENDED2 || matches!(param >> 4, 0 | 1 | 4 | 5))
        && keeps_porta(fx, param)
}

/// Effects that leave a plain portamento armed: the empty cell, 03 itself
/// and the ones that act once when the row is read (MEASURED for 09, 0C, 12,
/// 13, 23-29 and 2D; 0B, 0D, 0E and 0F move the song or its clock, so a
/// probe cannot tell, and are taken to be the same).
fn keeps_porta(fx: u8, param: u8) -> bool {
    matches!(
        fx,
        fx::PORTA
            | fx::SET_MOD_VOL
            | fx::POS_JUMP
            | fx::SET_INS_VOL
            | fx::PAT_BREAK
            | fx::SET_TEMPO
            | fx::SET_SPEED
            | fx::SET_CAR_VOL
            | fx::SET_WAVEFORM
            | fx::EXTENDED
            | fx::EXTENDED2
            | fx::GLOBAL_VOL
            | fx::SWAP_ARP
            | fx::SWAP_VIB
            | fx::FORCE_INS_VOL
            | fx::EXTENDED3
            | fx::CUSTOM_SPEED_TAB
    ) || (fx, param) == (0, 0)
}

/// Effects that share running state: an arpeggio, vibrato or tremolo
/// carries on across rows while the column stays inside its group.
fn group(fx: u8) -> u8 {
    match fx {
        fx::ARPEGGIO | fx::EXTRA_FINE_ARP | fx::ARP_VSLIDE | fx::ARP_VSLIDE_FINE => 1,
        fx::VIBRATO | fx::EXTRA_FINE_VIB | fx::VIB_VSLIDE | fx::VIB_VSLIDE_FINE => 2,
        fx::TREMOLO | fx::EXTRA_FINE_TREM => 3,
        _ => 0x80 | fx,
    }
}

/// `63 - round((63 - tl) × vol / 63)`: a total level at volume `vol` (0..63).
fn scale_level(tl: u8, vol: u8) -> u8 {
    let v = (63 - tl.min(63) as u16) * vol.min(63) as u16;
    63 - ((v + 31) / 63) as u8
}

/// Raises the F-number by `shift`, moving up one block when it reaches the
/// range's top (AT2 `calc_freq_shift_up`: one wrap only, 16-bit arithmetic;
/// in block 7 it stops at the top).
fn shift_up(freq: u16, shift: u16) -> u16 {
    let mut a = (freq & 0x3ff).wrapping_add(shift);
    let mut b = freq & 0x1c00;
    if a >= FREQ_HI {
        if b != 7 << 10 {
            a = a.wrapping_sub(FREQ_HI - FREQ_LO);
            b += 1 << 10;
        } else {
            a = FREQ_HI;
        }
    }
    a.wrapping_add(b).wrapping_add(freq & 0xe000)
}

/// Lowers the F-number by `shift`, moving down one block at or below the
/// range's bottom, even for a shift of 0 (AT2 `calc_freq_shift_down`).
fn shift_down(freq: u16, shift: u16) -> u16 {
    let mut a = (freq & 0x3ff).wrapping_sub(shift);
    let mut b = freq & 0x1c00;
    if a <= FREQ_LO {
        if b != 0 {
            a = a.wrapping_add(FREQ_HI - FREQ_LO);
            b -= 1 << 10;
        } else {
            a = FREQ_LO;
        }
    }
    a.wrapping_add(b).wrapping_add(freq & 0xe000)
}

/// A slide up by `shift` that stops at `limit` (AT2 `portamento_up`); an
/// idle channel (frequency 0) does not move.
fn porta_up(freq: u16, shift: u16, limit: u16) -> Option<u16> {
    if freq & 0x1fff == 0 {
        return None;
    }
    // AT2 `change_freq` keeps 13 bits of whatever the shift produced.
    Some(shift_up(freq & 0x1fff, shift).min(limit) & 0x1fff)
}

fn porta_down(freq: u16, shift: u16, limit: u16) -> Option<u16> {
    if freq & 0x1fff == 0 {
        return None;
    }
    Some(shift_down(freq & 0x1fff, shift).max(limit) & 0x1fff)
}

/// Moves a frequency by `by` steps the way every AT2 slide does: towards
/// note 97 (block 7 top) or note 1 (C in block 0). `None` on an idle channel.
fn step_freq(freq: u16, by: i32) -> Option<u16> {
    if by >= 0 {
        porta_up(freq, by as u16, FREQ_MAX)
    } else {
        porta_down(freq, (-by) as u16, FREQ_MIN)
    }
}

/// A table swap under ZFF carries on from the same step, clamped to the new
/// table's length; a finished table stands at its end (AT2 keeps no
/// separate "running" state: a table runs while its position is in range).
fn keep_macro(m: &mut MacroState, len: u8) {
    if !m.active && m.pos != 0 {
        m.pos = len;
    }
    m.pos = m.pos.min(len);
    m.active = len > 0;
}

/// Whether a macro has been started at all (so a restart can revive it).
fn st_ran(m: &MacroState) -> bool {
    m.pos != 0 || m.released
}

/// Register offset of channel `ch` (0..17) for the A0/B0/C0 groups.
fn reg_channel(ch: usize) -> u16 {
    ((ch / 9) << 8 | (ch % 9)) as u16
}

/// Modulator and carrier register offsets of channel `ch`.
fn op_regs(ch: usize) -> (u16, u16) {
    let m = ((ch / 9) << 8) as u16 | OP_OFFSET[ch % 9];
    (m, m + 3)
}

/// C0 output bits for a panning value (MEASURED: 1 → 0x10, 2 → 0x20).
fn pan_bits(pan: u8) -> u8 {
    match pan {
        1 => 0x10,
        2 => 0x20,
        _ => 0x30,
    }
}
