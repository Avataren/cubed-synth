//! The SID song player, S3 of `.ai/plan-sid-tracking.md`: plays a `SidSong`
//! (`song.rs`, the file the app's `SidDoc` saves) on one `Chip`, built with
//! the song's own model (the per-song tag S2 deferred), by writing the chip's
//! registers once per frame, as a C64 playroutine does.
//!
//! Timing: one frame is `frame_cycles(speed_multiplier)` chip cycles, the
//! real C64 rate a GoatTracker song is written for (GT-parity 0925b): at 1x
//! the PAL vertical blank, 312 lines x 63 = 19 656 cycles, 985 248 / 19 656
//! = 50.1245 Hz (GT exports a 1x tune on the VBI: greloc.c:1590-1596); at
//! multispeed m GT's CIA timer, latch $4CC7 / m, a period of latch + 1
//! (greloc.c:1551-1562), = 19 656 / m for m = 2, 3, 4, 6, 8. Until 0925b it
//! was exactly 50 Hz x m, which is GT's EDITOR (gsound.h:24, its mixer calls
//! the player every mixrate / 50 samples, bme_snd.c:386): 0.249 % slower than
//! the exported tune. The TS engine's BPM (`sidDocTiming`, 125 per 1x) stays
//! an approximation for the grid; this player owns the transport. A row
//! lasts the start tempo's frames (`start_tempo`: the doc's tempo times the
//! multiplier, as GT counts player calls; or GT's hidden tempo from
//! instrument 63) until a tempo command: each channel has GT's own tick counter and tempo (S5.18,
//! `tick_step`), so funktempo (E) alternates two row lengths and F with bit 7
//! sets one channel's alone. Frames
//! land on output-sample boundaries (the chip's own write granularity,
//! `chip.rs`), accumulated fractionally, so no frame drifts.
//!
//! Per frame, in this order (INFERRED ordering, a plain playroutine's):
//!   1. on the row's first frame, every channel reads its row: an instrument
//!      number selects the instrument; a note 1..=93 (transposed by the
//!      orderlist entry as GT's u8 arithmetic does, never clamped: S5.10,
//!      `note_index`) triggers it: the
//!      instrument's AD/SR and table pointers are loaded (a GT instrument has
//!      no waveform, pulse width or filter of its own: the channel keeps its
//!      own until a table sets them), the gate goes on and the first-frame
//!      waveform is held as the waveform (`trigger`); key off clears the gate, key on sets it; then
//!      the row's command;
//!   2. per channel the wave table, then, unless a wave step set a note
//!      (GT ends the frame there, gplay.c:722), the running command (1, 2,
//!      3, 4, or 0's instrument vibrato); the pulse table; then the (global)
//!      filter table;
//!   3. the hard restart: `gate_timer` frames before a row that triggers a
//!      note, the gate is cleared (unless that row's instrument has
//!      `no_gate_off`), and with that instrument's `hard_restart` AD $0F /
//!      SR 0 (S5.17, S5.19);
//!   4. every register is written: per voice frequency, pulse
//!      width, AD, SR, then control (`waveform & gate mask`, below); then
//!      cutoff, resonance/routing and mode/volume.
//!
//! The control byte (S5.9, GT2 source): the waveform is a whole control
//! byte, gate bit included, as GoatTracker keeps it (`cptr->wave`), and the
//! channel's gate is a MASK over it, 0xFF on, 0xFE off (gplay.c:129, 148,
//! 362, 916, 918, 926); the register is `waveform & mask` (gplay.c:945). So
//! a wave-table row or command 7 with the gate bit clear releases the note
//! even while the channel's gate is on (the classic drum ending, `$80` noise
//! or `$40` pulse), a gate-set row after it retriggers, and after a key off
//! no waveform byte raises the gate.
//!
//! Commands (the row's command nibble and parameter; GoatTracker's command
//! set, from its format documentation, interpreted here, not copied):
//!   0 none; 1/2 portamento up/down and 3 tone portamento at the 16-bit
//!   register speed held in speed-table row `param` (left<<8 | right);
//!   4 vibrato with speed-table row `param`, GoatTracker's (S5.10,
//!   `vibrato`: left = turn value, $80 up fine mode, right = register step);
//!   5 AD = param; 6 SR = param;
//!   7 waveform = param (the whole byte, gate bit included, gplay.c:433);
//!   8/9/A start the wave/pulse/filter table at row
//!   `param`; B resonance/routing ($17) = param; C cutoff high byte = param;
//!   D master volume = param & 15; E funktempo: the two row lengths of speed
//!   row `param` (minus 1: GT's `funktable`) alternate on every channel
//!   (gplay.c:483-492); F tempo: from 3 up the row length minus 1, 0-2 as
//!   they are (0 and 1 are funktempo), on all channels or, with bit 7, on this
//!   one (gplay.c:494-508). A row with command 3 and
//!   a note glides to it instead of triggering; with parameter 0 (GT's
//!   tie-note, readme §3.2) the pitch jumps to it on the row's second frame
//!   (tick 1; GT's realtime optimisation skips tick 0, goattrk2.c:55,
//!   gplay.c:728) and is re-asserted every tick the command stands
//!   (gplay.c:807-811), still without a trigger; wave-table note rows pass
//!   through under it and win their frame (gplay.c:714-722, 722). Pinned in
//!   S5, phase and passthrough aligned in S5.6 against the `.sng` corpus and
//!   the GT2 source.
//!   S5.10, GT's running command (gplay.c:351-358, 397-428): a new note sets
//!   it to 0 with the instrument's speed row and vibrato delay; commands 0-4
//!   replace it; 5-F leave it, so a vibrato or a slide runs on under them.
//!   Command 0 runs the instrument vibrato: a delay of 0 never, above 1
//!   counting down, at 1 swinging (gplay.c:767-772). Every tick effect (the
//!   vibratos 0 and 4, the slides 1-2, the portamento 3 and `3 00`) skips
//!   tick 0 of a row, as GT's realtime optimisation (on by default,
//!   goattrk2.c:55; readme "-R") does (gplay.c:728; player.s:971-979): S5.10
//!   for the vibratos, S5.12 for 1-3 (`tests_s512.rs`). Wave-table commands
//!   $F1-$F4 are not tick effects and run on any tick (gplay.c:529-691).
//!
//! Tables (1-based rows; left 0xFF = jump to row `right`, 0 = stop; one
//! jump per frame):
//!   wave: left 0x01..=0x0F waits that many frames, then sets the row's note
//!     (the row lasts left + 1 frames; pinned in S5); 0x10..=0xDF
//!     sets the waveform to `left` whole, 0xE0..=0xEF to `left & 0x0F`, the
//!     gate bit kept in both (gplay.c:525, 527; S5.9), 0x00 keeps it,
//!     0xF0..=0xFE (GT's table commands, `wave_command`): the pattern
//!     command of the low nibble with the right column as its parameter,
//!     for that frame (S5.17: $F5/$F6; S5.19: the rest). $F0, $F8 and $FE
//!     have no GT meaning: they are illegal (readme §3.4.1), GT's editor
//!     stops the song on them (gplay.c:534-538) and its packer refuses to
//!     export one (greloc.c:401-409); here the row only advances, as GT's
//!     does on that frame, and the song plays on. right, GT's arithmetic
//!     (S5.10, gplay.c:714-721): 0x00..=0x7F added to the channel's note,
//!     0x80 no change, 0x81..=0xFF the absolute note, then `& 0x7F`, into the
//!     128-entry table (96 notes, then zeros: `gt_note_freq_reg`). This right
//!     column is the arpeggio;
//!   pulse: left 0x80..=0xFE sets the width to (left & 0x0F)<<8 | right;
//!     0x01..=0x7F adds the signed `right` to it for `left` frames;
//!   filter: left 0x00 sets the cutoff high byte to `right`; 0x01..=0x7F
//!     adds the signed `right` to it for `left` frames; 0x80..=0xFE sets the
//!     mode to (left >> 4) & 7 (LP 1, BP 2, HP 4) and $17 (resonance<<4 |
//!     routing) to `right`;
//!   speed: data only, read by commands 1-4 and the instrument vibrato.
//! INFERRED throughout, from GoatTracker's documented table semantics; S5
//! pins them against real `.sng` files and adjusts this header with them.
//! No reference recording exists for a SID song played here (the S0-S2
//! caveat): what this player is proven to do is what its tests assert.
//!
//! S4 (the browser worklet, `wasm.rs`) adds what a transport needs:
//!   - the song row: rows started since the top (`song_row`), and the song's
//!     length in rows (`song_rows`, the longest channel's first pass, the
//!     app's grid length, `projectSidPatterns`);
//!   - `seek_row`: a fresh sequencer replayed to the row frame by frame
//!     without rendering (cheap: no chip cycles), then given the chip that was
//!     running. The registers are right at the row, and a note that sounded
//!     before carries on until a row retriggers it; the envelopes of notes
//!     started during the replay are NOT advanced (they start from wherever the
//!     running chip's are), so a seek lands "cold" on held notes;
//!   - a row-range loop (`set_loop_rows`, the tracker's "play pattern"): at
//!     the range's end the player seeks back to its start on the same chip;
//!   - a preview voice (`set_preview`): no sequencer; channel 1 is played by
//!     `preview_note_on` / `preview_note_off` with the song's instruments,
//!     tables and vibrato running as they do for a song note;
//!   - per-voice taps and mute/solo (`render_taps`, `Chip::set_voice_mask`).
//!
//! The editor's controls (the SID song document's transport, `edit::SidDoc`),
//! all allocation-free, safe to call between audio blocks:
//!   - `seek(subsong, voice, order, row)`: the song replayed silently to the moment a
//!     voice is about to read a row of an orderlist position, so the tempo, the other
//!     voices, the tables and the filter are where the song has them there; `seek_row`
//!     is the same by song row, in place (about 0.02 us a frame replayed, release);
//!   - `set_loop(SidLoop { voice, start, end })`: a range of one voice's rows repeated
//!     from a copy of the sequencer taken when the player first passes `start`, so a
//!     repeat costs nothing and plays the moment exactly (`SidSong::order_rows` turns
//!     orderlist positions into rows);
//!   - `set_mute_solo(mute, solo)`, `audition_on(voice, instrument, note)` /
//!     `audition_off(voice)` (an instrument with its tables running, with no song in
//!     preview mode, or taking a voice over a playing one), `status()` (per voice the
//!     orderlist position, pattern, row and tick), `peaks()` (per voice, falling 24 dB a
//!     second) and `attach_probe` (the same, shared with other threads, `probe.rs`);
//!   - `apply_live(&LiveEdit)`: the edits `SidDoc::live_edits` says a playing song can
//!     take (rows, instrument fields, the content of table rows, the chip model).

use super::chip::{Chip, ALL_VOICES, REG_FC_HI, REG_FC_LO, REG_MODE_VOL, REG_RES_FILT};
use super::probe::{SidProbe, SidStatus};
use std::sync::Arc;

/// Chip cycles between two register writes of a frame (GT: `lda $xxxx,x` 4 +
/// `sta $d400,x` 5, gsid.h `SIDWRITEDELAY`).
const WRITE_SPACING: u64 = 9;
/// Extra cycles before a control register (GT: `SIDWAVEDELAY`, the `and`).
const CONTROL_EXTRA: u64 = 4;
/// The AD a hard restart writes (GT's default `adparam` 0x0F00, gplay.c:929,
/// its high byte; SR is the low byte, 0). Decay 15 puts the envelope's rate
/// period at 31251 cycles while the gate is still on, so the counter is past
/// the release period (9) when the gate closes and the ADSR delay bug holds the
/// note at level for ~33 ms before it releases: GT's audible tail after a hit.
const HARD_RESTART_AD: u8 = 0x0F;
use super::song::{
    Instrument, LiveEdit, Row, SidSong, TableRow, NOTE_FIRST, NOTE_KEY_OFF, NOTE_KEY_ON, NOTE_LAST,
    SID_CHANNELS,
};
use super::waveform::GATE;
use super::{gt_note_freq_reg, SidError, SidModel, GT_NOTE_COUNT, PAL_CLOCK_HZ};

/// Chip cycles in one PAL video frame: 312 raster lines of 63 cycles (the
/// 6569 VIC-II). A 1x GoatTracker tune runs once per vertical blank.
pub const PAL_FRAME_CYCLES: u32 = 312 * 63;
/// The CIA timer latch GT's exported multispeed tune divides by its
/// multiplier (greloc.c:1554, `0x4cc7/multiplier`); the timer underflows
/// every latch + 1 cycles, so $4CC7 = 19 655 is one PAL frame.
const GT_CIA_LATCH: u32 = 0x4CC7;
/// PAL frames per second at multispeed 1: 985 248 / 19 656 = 50.1245 Hz
/// (not GT's editor's 50: see the header).
pub const FRAME_HZ: f64 = PAL_CLOCK_HZ / PAL_FRAME_CYCLES as f64;

/// Chip cycles between two player frames at multispeed `mult` (1..=16):
/// the vertical blank at 1x, GT's CIA period above.
pub fn frame_cycles(mult: u8) -> u32 {
    if mult <= 1 {
        PAL_FRAME_CYCLES
    } else {
        GT_CIA_LATCH / mult as u32 + 1
    }
}

#[derive(Debug, Clone, Default)]
struct Channel {
    // Sequencer.
    order: usize,
    repeat_left: u8,
    pattern: usize,
    row: usize,
    transpose: i8,
    // Voice.
    instrument: usize,
    /// GT's `cptr->note`: the row note's index under the transpose, a u8
    /// that wraps (gplay.c:350, 921), unmasked.
    base_note: u8,
    /// GT's `cptr->lastnote`: the 7-bit index last played (gplay.c:721),
    /// for the fine vibrato's step.
    last_note: u8,
    freq: u16,
    gate: bool,
    waveform: u8,
    ad: u8,
    sr: u8,
    pulse_width: u16,
    first_frame: bool,
    first_wave: u8,
    cmd: u8,
    param: u8,
    /// GT's running `command`/`cmddata` (gplay.c:351-358, 397-428): set by a
    /// new note (0 and the instrument's speed row) and by commands 0-4; 5-F
    /// leave them. The tick effects (portamento, vibrato) read these.
    run_cmd: u8,
    run_param: u8,
    // Tables.
    wave_ptr: u8,
    wave_wait: u8,
    pulse_ptr: u8,
    pulse_time: u8,
    pulse_speed: i8,
    // Vibrato (the command's or the instrument's): GT's `vibdelay` and u8
    // `vibtime` (gplay.c:352, 615-640).
    vib_delay: u8,
    vib_time: u8,
    // Timing (GT's per-channel `tick` and `tempo`, gplay.c:325-333). `tick`
    // counts down; the frame it reaches 0 starts a row. `tempo` is GT's
    // stored value (the row length minus 1; 0 and 1 are funktempo, which
    // alternates the two `funk` lengths and flips the low bit each row).
    tick: u8,
    tempo: u8,
    /// The doc's start tempo, literal frames per row until a tempo command
    /// (E, F) takes the channel over: GT's stored tempo cannot say 1 or 2.
    fixed: u8,
    /// The frames the current row lasts (for `tempo()`).
    period: u8,
    /// This frame started a row.
    at_row_start: bool,
    // Where the channel is, for display (`SidSongPlayer::status`): the row it
    // started last (the sequencer fields above run a frame ahead on the last
    // frame of a row), and the frames it has played of it.
    play_order: usize,
    play_pattern: usize,
    play_row: usize,
    row_tick: u8,
}

/// What a note trigger reads from its instrument. It is `Copy`, so taking it
/// does not clone the instrument (and its name) on the audio thread.
#[derive(Clone, Copy)]
struct NoteStart {
    ad: u8,
    sr: u8,
    first_wave: u8,
    wave_ptr: u8,
    pulse_ptr: u8,
    filter_ptr: u8,
}

impl NoteStart {
    fn of(ins: &Instrument) -> NoteStart {
        NoteStart {
            ad: ins.ad(),
            sr: ins.sr(),
            first_wave: ins.first_wave,
            wave_ptr: ins.wave_ptr,
            pulse_ptr: ins.pulse_ptr,
            filter_ptr: ins.filter_ptr,
        }
    }
}

/// Plays one subsong of a `SidSong` on a chip of the song's model.
#[derive(Debug, Clone)]
pub struct SidSongPlayer {
    song: SidSong,
    subsong: usize,
    chip: Chip,
    channels: [Channel; SID_CHANNELS],
    /// GT's `funktable`: the two row lengths minus 1 that funktempo alternates
    /// (shared by every channel; command E sets it from a speed-table row).
    funk: [u8; 2],
    /// The channel whose rows the song row counts: the longest first pass.
    ref_channel: usize,
    /// The voices (a bit each) whose row ended on the frame just played.
    rows_ended: u8,
    frames: u64,
    looped: bool,
    samples_per_frame: f64,
    samples_to_frame: f64,
    // Global filter state.
    filter_ptr: u8,
    filter_time: u8,
    filter_speed: i8,
    cutoff: u16,
    res_filt: u8,
    mode: u8,
    volume: u8,
    /// $15-$18 as this frame writes them: GT runs the filter table and sets
    /// these at the top of its frame, before the channels (gplay.c:252-302),
    /// so a row's filter command or a new instrument's filter table is heard
    /// from the next frame.
    filter_regs: [u8; 4],
    // Transport (S4).
    song_rows: u64,
    /// Rows each voice has moved on to since the top (`song_row` is the
    /// reference voice's).
    rows: [u64; SID_CHANNELS],
    loop_range: Option<SidLoop>,
    /// The sequencer as it stood at the start of the loop's range, so a loop
    /// goes back without replaying the song from the top.
    rewind: Option<Rewind>,
    preview: bool,
    /// The voices a note audition holds (`audition_on`): their rows are not read.
    audition: u8,
    /// The voices that have been auditioned since preview mode began: the ones
    /// a filter routed to any voice is heard on.
    audition_seen: u8,
    mute: u8,
    solo: u8,
    /// The most frames a seek replays (`SEEK_MAX_FRAMES`).
    seek_limit: u64,
    /// Per voice, the loudest recent sample of its tap, falling 24 dB a second.
    peaks: [f32; SID_CHANNELS],
    /// ln of the per-sample factor of that fall.
    peak_ln_fall: f32,
    probe: Option<Arc<SidProbe>>,
}

/// A range of rows of one voice to play over and over (`SidSongPlayer::set_loop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidLoop {
    /// The voice whose rows are counted (0-2).
    pub voice: u8,
    /// The first row of the range, counted from the top of the subsong.
    pub start: u64,
    /// The row after the last (exclusive).
    pub end: u64,
}

/// Why a seek did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekError {
    /// The subsong, voice, orderlist position or row does not exist.
    NoSuchPlace,
    /// The place is not reached within [`SEEK_MAX_FRAMES`] of the song.
    TooFar,
}

impl std::fmt::Display for SeekError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SeekError::NoSuchPlace => "there is no such place in the song",
            SeekError::TooFar => "the place is too far into the song to reach",
        })
    }
}

impl std::error::Error for SeekError {}

/// The most frames a seek replays (silently, without the chip) before it gives
/// up: about 23 hours of a 50 Hz song.
pub const SEEK_MAX_FRAMES: u64 = 1 << 22;

/// Samples a tap chunk the peaks are taken over (the player's stack buffers).
const PEAK_CHUNK: usize = 256;

/// The sequencer's state at one moment (everything a seek replay builds), copied
/// whole: no allocation.
#[derive(Debug, Clone)]
struct Rewind {
    subsong: usize,
    ref_channel: usize,
    song_rows: u64,
    channels: [Channel; SID_CHANNELS],
    funk: [u8; 2],
    looped: bool,
    filter_ptr: u8,
    filter_time: u8,
    filter_speed: i8,
    cutoff: u16,
    res_filt: u8,
    mode: u8,
    volume: u8,
    filter_regs: [u8; 4],
    rows: [u64; SID_CHANNELS],
}

/// GT's stored tempo for a tempo value (gplay.c:496-498): the row length
/// minus 1 from 3 up; 0-2 stay (0 and 1 select funktempo).
fn gt_tempo(value: u8) -> u8 {
    let t = value & 0x7F;
    if t >= 3 {
        t - 1
    } else {
        t
    }
}

/// The note index of row note `note` under `transpose`, as GoatTracker
/// computes it: `newnote = (u8)(note + trans)`, `cptr->note = (u8)(newnote -
/// FIRSTNOTE)` (gplay.c:921, 350), so it WRAPS: G#7 +5 is 97, C-0 -1 is 255.
/// Never clamped (S5.10; the old clamp to G#7 played up to 5 semitones flat).
/// The table reads it `& 0x7f` (`gt_note_freq_reg`). The app's display
/// (`sidNoteIndex`) still clamps: a grid cell can only name C-0..G#7.
pub fn note_index(note: u8, transpose: i8) -> u8 {
    (note as i32 - NOTE_FIRST as i32 + transpose as i32) as u8
}

/// The loudest sample of a tap chunk.
fn tap_peak(tap: &[f32]) -> f32 {
    tap.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

fn table_row(table: &[TableRow], ptr: u8) -> Option<TableRow> {
    if ptr == 0 {
        None
    } else {
        table.get(ptr as usize - 1).copied()
    }
}

impl SidSongPlayer {
    /// A player for subsong 0 at `sample_rate`, on a chip of the song's model.
    pub fn new(song: SidSong, sample_rate: f64) -> Result<SidSongPlayer, SidError> {
        let model = song.model;
        SidSongPlayer::with_model(song, model, sample_rate, 0)
    }

    /// A player for `subsong` on a chip of `model` (the song's own unless a
    /// caller overrides it, e.g. to compare the two chips on one song).
    pub fn with_model(
        song: SidSong,
        model: SidModel,
        sample_rate: f64,
        subsong: usize,
    ) -> Result<SidSongPlayer, SidError> {
        let chip = Chip::with_sample_rate(model, sample_rate)?;
        let subsong = subsong.min(song.subsongs.len().saturating_sub(1));
        let samples_per_frame =
            frame_cycles(song.speed_multiplier) as f64 * sample_rate / PAL_CLOCK_HZ;
        let mut player = SidSongPlayer {
            song,
            subsong,
            chip,
            channels: Default::default(),
            funk: [0; 2],
            ref_channel: 0,
            rows_ended: 0,
            frames: 0,
            looped: false,
            samples_per_frame,
            samples_to_frame: 0.0,
            filter_ptr: 0,
            filter_time: 0,
            filter_speed: 0,
            cutoff: 0,
            res_filt: 0,
            mode: 0,
            volume: 15,
            filter_regs: [0, 0, 0, 15],
            song_rows: 0,
            rows: [0; SID_CHANNELS],
            loop_range: None,
            rewind: None,
            preview: false,
            audition: 0,
            audition_seen: 0,
            mute: 0,
            solo: 0,
            seek_limit: SEEK_MAX_FRAMES,
            peaks: [0.0; SID_CHANNELS],
            // 24 dB a second: 10^(-24/20) per second.
            peak_ln_fall: -(24.0 / 20.0) * std::f32::consts::LN_10 / sample_rate as f32,
            probe: None,
        };
        player.reset_sequencer(subsong);
        Ok(player)
    }

    /// Puts the sequencer, the voices' own state and the filter's where a new
    /// player has them, on `subsong` (in place: nothing is allocated, the chip
    /// keeps running).
    fn reset_sequencer(&mut self, subsong: usize) {
        self.subsong = subsong;
        let mult = self.song.speed_multiplier.max(1);
        let start = Self::start_tempo(&self.song);
        let first_instrument = if self.song.instruments.is_empty() {
            0
        } else {
            1
        };
        for c in 0..SID_CHANNELS {
            let ch = &mut self.channels[c];
            *ch = Channel::default();
            ch.order = usize::MAX;
            let list = &self.song.subsongs[subsong].orderlists[c];
            Self::enter_order(ch, list, 0, &self.song);
            // GT starts every channel on instrument 1 (gplay.c:62), so its
            // gate timer brings the hard restart before a channel's first note.
            ch.instrument = first_instrument;
            // The first frame decrements to 0 and starts the first row.
            ch.tick = 1;
            ch.fixed = start;
            ch.period = start;
            ch.play_order = ch.order;
            ch.play_pattern = ch.pattern;
        }
        self.funk = [
            9u8.wrapping_mul(mult).wrapping_sub(1),
            6u8.wrapping_mul(mult).wrapping_sub(1),
        ];
        let (song_rows, ref_channel) = Self::first_pass_rows(&self.song, subsong);
        self.song_rows = song_rows;
        self.ref_channel = ref_channel;
        self.rows_ended = 0;
        self.looped = false;
        self.filter_ptr = 0;
        self.filter_time = 0;
        self.filter_speed = 0;
        self.cutoff = 0;
        self.res_filt = 0;
        self.mode = 0;
        self.volume = 15;
        self.filter_regs = [0, 0, 0, 15];
        self.rows = [0; SID_CHANNELS];
        self.rewind = None;
    }

    fn snapshot(&self) -> Rewind {
        Rewind {
            subsong: self.subsong,
            ref_channel: self.ref_channel,
            song_rows: self.song_rows,
            channels: self.channels.clone(),
            funk: self.funk,
            looped: self.looped,
            filter_ptr: self.filter_ptr,
            filter_time: self.filter_time,
            filter_speed: self.filter_speed,
            cutoff: self.cutoff,
            res_filt: self.res_filt,
            mode: self.mode,
            volume: self.volume,
            filter_regs: self.filter_regs,
            rows: self.rows,
        }
    }

    fn restore(&mut self, r: &Rewind) {
        self.subsong = r.subsong;
        self.ref_channel = r.ref_channel;
        self.song_rows = r.song_rows;
        self.channels = r.channels.clone();
        self.funk = r.funk;
        self.looped = r.looped;
        self.filter_ptr = r.filter_ptr;
        self.filter_time = r.filter_time;
        self.filter_speed = r.filter_speed;
        self.cutoff = r.cutoff;
        self.res_filt = r.res_filt;
        self.mode = r.mode;
        self.volume = r.volume;
        self.filter_regs = r.filter_regs;
        self.rows = r.rows;
    }

    /// The frames per row every channel starts at, until a tempo command
    /// (sid_decisions.md §4): the doc's tempo is the start tempo at 1x, and
    /// GoatTracker counts rows in player calls, so at multispeed it is
    /// `tempo * multiplier` (GT's `6 * multiplier - 1` stored, gplay.c:207-218).
    /// Except GT's hidden song tempo: instrument 63 with no wave table and an
    /// AD byte of 2 or more starts the song at that many frames, unscaled
    /// (gplay.c:220-221, stored `ad - 1`).
    fn start_tempo(song: &SidSong) -> u8 {
        if let Some(last) = song.instruments.get(62) {
            let ad = (last.attack << 4) | (last.decay & 0x0F);
            if last.wave_ptr == 0 && ad >= 2 {
                return ad;
            }
        }
        let scaled = song.tempo.max(1) as u32 * song.speed_multiplier.max(1) as u32;
        scaled.min(u8::MAX as u32) as u8
    }

    /// Rows of the longest channel's first pass through `subsong`'s orderlist,
    /// and that channel (the first, on a tie): the one whose rows the song
    /// row counts, since channels can run at their own tempo (command F $80+).
    fn first_pass_rows(song: &SidSong, subsong: usize) -> (u64, usize) {
        let mut best = (0u64, 0usize);
        for (c, list) in song.subsongs[subsong].orderlists.iter().enumerate() {
            let rows = list
                .entries
                .iter()
                .map(|e| {
                    let pattern = (e.pattern as usize).min(song.patterns.len() - 1);
                    song.patterns[pattern].rows.len() as u64 * e.repeat as u64
                })
                .sum::<u64>();
            if rows > best.0 {
                best = (rows, c);
            }
        }
        best
    }

    /// The song's length in rows: the longest channel's first pass, where the
    /// app's grid ends (`projectSidPatterns`). Shorter channels loop within it.
    pub fn song_rows(&self) -> u64 {
        self.song_rows
    }

    /// The row the player is on, counted from the top of the subsong (it
    /// keeps counting past `song_rows` as the song loops on).
    pub fn song_row(&self) -> u64 {
        self.rows[self.ref_channel]
    }

    /// The subsong this player plays.
    pub fn subsong(&self) -> usize {
        self.subsong
    }

    /// The voice whose rows `song_row` counts: the one with the longest first pass.
    pub fn ref_voice(&self) -> usize {
        self.ref_channel
    }

    /// Shows, for every voice that is about to start its next row, that row as its place (what
    /// `status` reports): after a seek the voices stand at the places they will read, and a stopped
    /// song's position is where it will start. Playing on, the first frame sets the same places.
    fn show_next_places(&mut self) {
        for ch in &mut self.channels {
            if ch.tick == 1 {
                ch.play_order = ch.order;
                ch.play_pattern = ch.pattern;
                ch.play_row = ch.row;
                ch.row_tick = 0;
            }
        }
    }

    /// Plays one frame without the chip: the sequencer, the voices' state and the
    /// tables move on, no register is written (a seek's replay).
    fn replay_frame(&mut self) {
        let frames = self.frames;
        self.frame_core(false);
        self.frames = frames;
    }

    /// Moves to the start of song row `row` (see the header: sequencer replay,
    /// same chip). The next frame reads that row. A row loop stays set.
    /// Nothing is allocated; the replay costs about a microsecond a frame of
    /// the song before the row (`SEEK_MAX_FRAMES` bounds it).
    pub fn seek_row(&mut self, row: u64) {
        self.reset_sequencer(self.subsong);
        let voice = self.ref_channel;
        self.replay_until(|p| p.rows[voice] >= row);
        self.show_next_places();
        self.samples_to_frame = 0.0;
        self.loop_boundary();
    }

    /// Replays frames until `done` holds, at most `SEEK_MAX_FRAMES`; whether it did.
    fn replay_until(&mut self, done: impl Fn(&SidSongPlayer) -> bool) -> bool {
        let mut n = 0;
        while !done(self) {
            if n >= self.seek_limit {
                return false;
            }
            self.replay_frame();
            n += 1;
        }
        true
    }

    /// Changes the most frames a seek replays before it gives up (`SEEK_MAX_FRAMES`).
    pub fn set_seek_limit(&mut self, frames: u64) {
        self.seek_limit = frames;
    }

    /// Moves to the moment voice `voice` is about to read row `row` of the
    /// pattern at orderlist position `order` (its first play, when the entry
    /// repeats) in subsong `subsong`: the song is replayed silently from the top
    /// to there, so the tempo, the other voices, the tables and the filter are
    /// where the song has them at that moment. The chip keeps running: notes that
    /// sounded carry on until a row retriggers them. Nothing is allocated; the
    /// replay costs about a microsecond a frame of the song before the place. On
    /// error nothing moved. A loop stays set; preview mode and auditions stay. The status then
    /// shows each voice that stands at the start of a row at that row (the sought voice at
    /// `order` and `row`), also while the song is stopped.
    pub fn seek(
        &mut self,
        subsong: usize,
        voice: usize,
        order: usize,
        row: usize,
    ) -> Result<(), SeekError> {
        let entry = self
            .song
            .subsongs
            .get(subsong)
            .and_then(|s| s.orderlists.get(voice))
            .and_then(|l| l.entries.get(order))
            .ok_or(SeekError::NoSuchPlace)?;
        let rows = self
            .song
            .patterns
            .get(entry.pattern as usize)
            .map_or(0, |p| p.rows.len());
        if row >= rows {
            return Err(SeekError::NoSuchPlace);
        }
        let before = self.snapshot();
        self.reset_sequencer(subsong);
        // About to read it: the channel's counter stands at 1 with the place named.
        let reached = self.replay_until(|p| {
            let ch = &p.channels[voice];
            ch.tick == 1 && ch.order == order && ch.row == row
        });
        if !reached {
            self.restore(&before);
            return Err(SeekError::TooFar);
        }
        self.show_next_places();
        self.samples_to_frame = 0.0;
        self.loop_boundary();
        Ok(())
    }

    /// Loops song rows `start..end` (`end` exclusive) of the reference voice:
    /// reaching `end`, the player goes back to `start`. `None` (or an empty
    /// range) plays on. See [`set_loop`](Self::set_loop).
    pub fn set_loop_rows(&mut self, range: Option<(u64, u64)>) {
        let voice = self.ref_channel as u8;
        self.set_loop(range.map(|(start, end)| SidLoop { voice, start, end }));
    }

    /// Loops a range of rows of one voice (counted from the top of the subsong:
    /// `SidSong::order_rows` gives the rows of orderlist entries): reaching
    /// `end`, every voice goes back to where it was when `voice` reached `start`,
    /// so the loop repeats the moment exactly, tempo and tables included. The
    /// player remembers that moment when it first passes `start` (or, for a range
    /// it is already inside or past, replays to it once, at the first `end`), so a
    /// loop costs nothing per repeat. `None` or an empty range plays on.
    pub fn set_loop(&mut self, range: Option<SidLoop>) {
        self.loop_range = range.filter(|l| l.end > l.start && (l.voice as usize) < SID_CHANNELS);
        self.rewind = None;
        self.loop_boundary();
    }

    /// Loops orderlist entries `first..=last` of `voice` (all their repeats): the
    /// editor's "play pattern" for the position the cursor is on. The rows are
    /// those of the subsong playing (`SidSong::order_rows`). `false` (no loop set)
    /// when the voice or the entries do not exist.
    pub fn loop_orders(&mut self, voice: usize, first: usize, last: usize) -> bool {
        match self.song.order_rows(self.subsong, voice, first, last) {
            Some((start, end)) if end > start => {
                self.set_loop(Some(SidLoop {
                    voice: voice as u8,
                    start,
                    end,
                }));
                true
            }
            _ => false,
        }
    }

    /// The loop in force, if any.
    pub fn loop_range(&self) -> Option<SidLoop> {
        self.loop_range
    }

    /// Remembers the sequencer now when it stands at the start of the loop's range.
    fn loop_boundary(&mut self) {
        let Some(l) = self.loop_range else { return };
        let v = l.voice as usize;
        if self.rewind.is_none() && self.rows[v] == l.start && self.channels[v].tick == 1 {
            self.rewind = Some(self.snapshot());
        }
    }

    /// Mutes and solos voices, a bit each (bit 0 voice 1): muted voices are not
    /// heard and read 0 in the peaks and taps; with any solo, only the soloed
    /// voices are heard (a voice both soloed and muted is not). The song keeps
    /// playing all three.
    pub fn set_mute_solo(&mut self, mute: u8, solo: u8) {
        self.mute = mute & ALL_VOICES;
        self.solo = solo & ALL_VOICES;
        let heard = if self.solo != 0 {
            self.solo
        } else {
            ALL_VOICES
        };
        self.chip.set_voice_mask(heard & !self.mute);
    }

    /// The (mute, solo) masks last set.
    pub fn mute_solo(&self) -> (u8, u8) {
        (self.mute, self.solo)
    }

    /// Preview mode: the sequencer stops; auditions (`audition_on`, and
    /// `preview_note_on` on voice 1) play the song's instruments with its tables
    /// running, no song needed. Leaving it, the voices read their rows again from
    /// their next row.
    pub fn set_preview(&mut self, on: bool) {
        self.preview = on;
        if on {
            self.audition = 0;
            self.audition_seen = 0;
            for ch in self.channels.iter_mut() {
                ch.gate = false;
                ch.cmd = 0;
                ch.param = 0;
                ch.run_cmd = 0;
                ch.run_param = 0;
            }
        } else {
            self.audition = 0;
        }
    }

    pub fn preview(&self) -> bool {
        self.preview
    }

    /// Plays instrument `instrument` (1-based) at note table index `note`
    /// (0 = C-0 .. 92 = G#7) on voice `voice` (0-2), as a song note of it: its
    /// envelope, first-frame waveform and wave, pulse, filter and speed tables
    /// run, its vibrato after its delay. In preview mode that is all the player
    /// does; over a playing song the voice is taken: its rows are not read (its
    /// row counter runs on) until [`audition_off`](Self::audition_off). Several
    /// voices can be auditioned at once (a chord). `false` (nothing changed) for a
    /// voice or an instrument that does not exist.
    pub fn audition_on(&mut self, voice: usize, instrument: usize, note: u8) -> bool {
        if voice >= SID_CHANNELS || instrument == 0 || instrument > self.song.instruments.len() {
            return false;
        }
        let note = note.min(GT_NOTE_COUNT - 1);
        self.audition |= 1 << voice;
        self.audition_seen |= 1 << voice;
        let ch = &mut self.channels[voice];
        ch.instrument = instrument;
        ch.cmd = 0;
        ch.param = 0;
        ch.run_cmd = 0;
        ch.run_param = 0;
        self.new_note(voice, note);
        self.trigger(voice, note);
        true
    }

    /// Releases an audition (the instrument's release plays out) and gives the
    /// voice back to the song from its next row.
    pub fn audition_off(&mut self, voice: usize) {
        if voice < SID_CHANNELS {
            self.channels[voice].gate = false;
            self.audition &= !(1 << voice);
        }
    }

    /// The voices an audition holds, a bit each.
    pub fn auditioning(&self) -> u8 {
        self.audition
    }

    /// Preview mode: triggers instrument `instrument` (1-based) at note table
    /// index `note` on channel 1. `false` (nothing changed) outside preview
    /// mode or for an instrument the song lacks.
    pub fn preview_note_on(&mut self, instrument: usize, note: u8) -> bool {
        self.preview && self.audition_on(0, instrument, note)
    }

    /// Preview mode: releases the preview note (the instrument's release).
    pub fn preview_note_off(&mut self) {
        if self.preview {
            self.audition_off(0);
        }
    }

    /// Whether voice `c` is not reading rows (preview mode, or an audition holds it).
    fn suspended(&self, c: usize) -> bool {
        self.preview || self.audition & (1 << c) != 0
    }

    fn enter_order(ch: &mut Channel, list: &super::song::Orderlist, index: usize, song: &SidSong) {
        let entry = list.entries[index];
        ch.order = index;
        ch.pattern = (entry.pattern as usize).min(song.patterns.len() - 1);
        ch.transpose = entry.transpose;
        ch.repeat_left = entry.repeat;
        ch.row = 0;
    }

    pub fn chip(&self) -> &Chip {
        &self.chip
    }

    /// The chip, for what is not the song's to decide (the voice mask).
    pub fn chip_mut(&mut self) -> &mut Chip {
        &mut self.chip
    }

    pub fn song(&self) -> &SidSong {
        &self.song
    }

    /// Frames played so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Output samples per frame (fractional).
    pub fn samples_per_frame(&self) -> f64 {
        self.samples_per_frame
    }

    /// The samples a `render` call from here must be given to play exactly
    /// the next frame and stop where the one after it starts (at most
    /// `samples_per_frame().ceil()`). A PAL frame is not a whole number of
    /// samples (879.8 at 44.1 kHz), so a fixed length drifts a frame every
    /// few calls; a caller stepping frame by frame (tests, dumps) uses this.
    pub fn samples_in_next_frame(&self) -> usize {
        let left = if self.samples_to_frame <= 0.0 {
            self.samples_to_frame + self.samples_per_frame
        } else {
            self.samples_to_frame
        };
        (left.ceil() as usize).max(1)
    }

    /// Whether any channel's orderlist has wrapped to its restart.
    pub fn looped(&self) -> bool {
        self.looped
    }

    /// The frames the song row's current row lasts (funktempo alternates).
    #[allow(clippy::misnamed_getters)]
    pub fn tempo(&self) -> u8 {
        self.channels[self.ref_channel].period
    }

    /// Channel `c`'s place: (orderlist entry, row) of the row it plays next
    /// or is playing.
    pub fn position(&self, c: usize) -> (usize, usize) {
        (self.channels[c].order, self.channels[c].row)
    }

    /// The frequency register channel `c` holds (vibrato included: GT's
    /// vibrato moves `cptr->freq` itself, gplay.c:636-639).
    pub fn channel_freq(&self, c: usize) -> u16 {
        self.channels[c].freq
    }

    /// The note index channel `c` last triggered (GT's `cptr->note`, unmasked).
    pub fn channel_note(&self, c: usize) -> u8 {
        self.channels[c].base_note
    }

    /// Where the player is, for a display: per voice the orderlist position, the
    /// pattern and the row of the row it started last, and the frames played of
    /// it (0 on its first); see [`SidStatus`]. Reads the state as of the last frame.
    pub fn status(&self) -> SidStatus {
        let mut s = SidStatus {
            subsong: self.subsong as u8,
            tempo: self.tempo(),
            preview: self.preview,
            audition: self.audition,
            looping: self.loop_range.is_some(),
            looped: self.looped,
            mute: self.mute,
            solo: self.solo,
            song_row: self.song_row(),
            song_rows: self.song_rows,
            frames: self.frames,
            ..SidStatus::default()
        };
        for (c, ch) in self.channels.iter().enumerate() {
            s.order[c] = ch.play_order.min(255) as u8;
            s.pattern[c] = ch.play_pattern.min(255) as u8;
            s.row[c] = ch.play_row.min(255) as u8;
            s.tick[c] = ch.row_tick;
        }
        s
    }

    /// Each voice's recent peak: the loudest sample of its own signal in the
    /// latest blocks (the tap: after the voice's gain, before the filter), falling
    /// 24 dB a second after it, so a display that polls slower than the song does
    /// not miss a hit. A muted voice reads 0. The scale is the mix's
    /// (`chip().tap_full_scale()` is a full voice's).
    pub fn peaks(&self) -> [f32; SID_CHANNELS] {
        self.peaks
    }

    /// Shares the player's position and peaks with other threads: the probe is
    /// written at every frame and block (atomics, no lock, no allocation) and any
    /// thread reads it. Make the probe off the audio thread (`SidProbe::new`). (A
    /// clone of the player shares it.)
    pub fn attach_probe(&mut self, probe: Arc<SidProbe>) {
        probe.publish(&self.status(), &self.peaks);
        self.probe = Some(probe);
    }

    /// Applies one change of the song the player is playing without making it
    /// again, for what [`LiveEdit`] covers; `false` (nothing changed) when the
    /// place does not exist or the change is not valid for the song. A row edit is
    /// heard when the voice next reads the row. An instrument's fields are read
    /// when a note starts (the envelope, the tables it points at, the vibrato) or
    /// as the gate timer runs, so it is heard from the next note; a table row is
    /// heard at once where the table next reads it. An instrument's name is not
    /// read. The start tempo and the multispeed are read when the player is made:
    /// a change of them needs a new player. Nothing is allocated (an instrument
    /// edit carries no name).
    pub fn apply_live(&mut self, edit: &LiveEdit) -> bool {
        match edit {
            LiveEdit::Row { pattern, row, cell } => {
                if cell.instrument as usize > self.song.instruments.len() || cell.command > 0x0F {
                    return false;
                }
                match self
                    .song
                    .patterns
                    .get_mut(*pattern as usize)
                    .and_then(|p| p.rows.get_mut(*row as usize))
                {
                    Some(r) => {
                        *r = *cell;
                        true
                    }
                    None => false,
                }
            }
            LiveEdit::Instrument { number, instrument } => {
                let t = &self.song.tables;
                if instrument.wave_ptr as usize > t.wave.len()
                    || instrument.pulse_ptr as usize > t.pulse.len()
                    || instrument.filter_ptr as usize > t.filter.len()
                    || instrument.speed_ptr as usize > t.speed.len()
                    || instrument.attack > 15
                    || instrument.decay > 15
                    || instrument.sustain > 15
                    || instrument.release > 15
                {
                    return false;
                }
                let Some(i) = (*number as usize)
                    .checked_sub(1)
                    .and_then(|n| self.song.instruments.get_mut(n))
                else {
                    return false;
                };
                // Every field but the name (which would allocate).
                i.attack = instrument.attack;
                i.decay = instrument.decay;
                i.sustain = instrument.sustain;
                i.release = instrument.release;
                i.first_wave = instrument.first_wave;
                i.gate_timer = instrument.gate_timer & 0x3F;
                i.hard_restart = instrument.hard_restart;
                i.no_gate_off = instrument.no_gate_off;
                i.vibrato_delay = instrument.vibrato_delay;
                i.wave_ptr = instrument.wave_ptr;
                i.pulse_ptr = instrument.pulse_ptr;
                i.filter_ptr = instrument.filter_ptr;
                i.speed_ptr = instrument.speed_ptr;
                true
            }
            LiveEdit::TableRow { table, row, value } => {
                match self.song.tables.get_mut(*table).get_mut(*row as usize) {
                    Some(r) => {
                        *r = *value;
                        true
                    }
                    None => false,
                }
            }
            LiveEdit::Model(model) => {
                self.song.model = *model;
                self.chip.set_model(*model).is_ok()
            }
        }
    }

    /// Fill `out` with the song, frame by frame.
    pub fn render(&mut self, out: &mut [f32]) {
        self.render_inner(out, None);
    }

    /// `render`, and each voice's own signal into `taps` (`Chip::render_taps`).
    /// Every tap must be at least `out.len()` long.
    pub fn render_taps(&mut self, out: &mut [f32], taps: [&mut [f32]; 3]) {
        self.render_inner(out, Some(taps));
    }

    fn render_inner(&mut self, out: &mut [f32], mut taps: Option<[&mut [f32]; 3]>) {
        let mut i = 0;
        // The peaks come from the voices' taps: the caller's, or ours on the stack.
        let mut local = [[0.0f32; PEAK_CHUNK]; SID_CHANNELS];
        while i < out.len() {
            if self.samples_to_frame <= 0.0 {
                self.frame_with_loop();
                self.samples_to_frame += self.samples_per_frame;
            }
            let n = (self.samples_to_frame.ceil() as usize).clamp(1, out.len() - i);
            let mut done = 0;
            while done < n {
                let m = (n - done).min(PEAK_CHUNK);
                let at = i + done;
                let peak = match taps.as_mut() {
                    Some([a, b, c]) => {
                        let t = [&mut a[at..at + m], &mut b[at..at + m], &mut c[at..at + m]];
                        self.chip.render_taps(&mut out[at..at + m], t);
                        [
                            tap_peak(&a[at..at + m]),
                            tap_peak(&b[at..at + m]),
                            tap_peak(&c[at..at + m]),
                        ]
                    }
                    None => {
                        let [a, b, c] = &mut local;
                        let t = [&mut a[..m], &mut b[..m], &mut c[..m]];
                        self.chip.render_taps(&mut out[at..at + m], t);
                        [tap_peak(&a[..m]), tap_peak(&b[..m]), tap_peak(&c[..m])]
                    }
                };
                let fall = (self.peak_ln_fall * m as f32).exp();
                for (held, now) in self.peaks.iter_mut().zip(peak) {
                    *held = (*held * fall).max(now);
                }
                done += m;
            }
            self.samples_to_frame -= n as f64;
            i += n;
        }
        if let Some(p) = &self.probe {
            p.publish_levels(&self.peaks);
        }
    }

    /// One frame; then, when it ended the loop's last row, straight back to the
    /// loop's start (so `song_row` never reports the row past it).
    fn frame_with_loop(&mut self) {
        self.frame_core(true);
        if let Some(l) = self.loop_range {
            let v = l.voice as usize;
            if !self.preview && self.rows_ended & (1 << v) != 0 {
                if self.rows[v] >= l.end {
                    let carried = self.samples_to_frame;
                    // What the display shows now: a voice that is about to start its row
                    // at the loop's start keeps showing the loop's last row for this frame,
                    // rather than the row before the range.
                    let shown: [(usize, usize, usize, u8); SID_CHANNELS] =
                        std::array::from_fn(|c| {
                            let ch = &self.channels[c];
                            (ch.play_order, ch.play_pattern, ch.play_row, ch.row_tick)
                        });
                    match self.rewind.take() {
                        Some(r) => {
                            self.restore(&r);
                            self.rewind = Some(r);
                        }
                        None => {
                            // Not seen the start yet (the loop was set inside or past
                            // its range): replay to it once.
                            self.reset_sequencer(self.subsong);
                            self.replay_until(|p| p.rows[v] >= l.start);
                            self.loop_boundary();
                        }
                    }
                    for (ch, shown) in self.channels.iter_mut().zip(shown) {
                        if ch.tick == 1 {
                            (ch.play_order, ch.play_pattern, ch.play_row, ch.row_tick) = shown;
                        }
                    }
                    self.samples_to_frame = carried;
                } else {
                    self.loop_boundary();
                }
            }
        }
        if let Some(p) = &self.probe {
            p.publish(&self.status(), &self.peaks);
        }
    }

    /// Plays one frame: sequencer, effects, tables, register writes, and
    /// the writes are applied on return (for a caller that does not render).
    /// `render` uses `frame_core`, whose writes stay spaced (`write_registers`).
    pub fn frame(&mut self) {
        self.frame_core(true);
        self.chip.flush_writes();
    }

    /// One frame of the song. `write`: the registers are written (a replay for a
    /// seek leaves them alone).
    fn frame_core(&mut self, write: bool) {
        self.rows_ended = 0;
        self.filter_step();
        // The preview voice is always channel 1, whichever voice an
        // instrument's filter table routes: it is heard filtered when any is.
        // (Auditions on other voices are filtered where they play.)
        let res_filt = if self.preview && self.res_filt & 0x07 != 0 {
            let voices = if self.audition_seen == 0 {
                0x01
            } else {
                self.audition_seen
            };
            (self.res_filt & 0xF8) | voices
        } else {
            self.res_filt
        };
        self.filter_regs = [
            (self.cutoff & 0x07) as u8,
            (self.cutoff >> 3) as u8,
            res_filt,
            (self.mode << 4) | self.volume,
        ];
        if !self.preview {
            // GT's per-channel tick, in channel order (gplay.c:319-333): a
            // command E/F on a row changes the tempo of channels after it in
            // this very frame, and of earlier ones from their next reload.
            for c in 0..SID_CHANNELS {
                if self.tick_step(c) {
                    let ch = &mut self.channels[c];
                    ch.play_order = ch.order;
                    ch.play_pattern = ch.pattern;
                    ch.play_row = ch.row;
                    // A voice an audition holds does not read its row.
                    if self.audition & (1 << c) == 0 {
                        let row = self.current_row(c);
                        self.read_row(c, row);
                    }
                }
            }
        }
        for c in 0..SID_CHANNELS {
            // GT runs the wave table first; a step that sets a note ends the
            // channel's frame before the tick effects (gplay.c:714-722
            // `goto PULSEEXEC`), so that frame neither slides nor vibrates.
            if !self.wave_step(c) {
                self.continuous(c);
            }
            if !self.pulse_skipped(c) {
                self.pulse_step(c);
            }
            self.hard_restart(c);
        }
        if write {
            self.write_registers();
        }
        for ch in self.channels.iter_mut() {
            ch.first_frame = false;
        }
        self.frames += 1;
        if self.preview {
            return;
        }
        // A channel whose counter stands at 1 starts its next row on the next
        // frame's decrement to 0: move it on to that row now.
        for c in 0..SID_CHANNELS {
            let ch = &mut self.channels[c];
            // A literal one-frame row (the doc's tempo 1, which GT's counter
            // cannot express) starts a row every frame.
            let one_frame = ch.at_row_start && ch.fixed == 1;
            if one_frame {
                ch.tick = 1;
            }
            if ch.tick == 1 {
                self.advance_row(c);
                self.rows[c] += 1;
                self.rows_ended |= 1 << c;
            }
        }
    }

    /// GT's tick counter for channel `c` (gplay.c:319-333): counts down, and
    /// the frame it reaches 0 starts a row (`true`). Below 0 (wrapped) it
    /// reloads from the channel's tempo, or, under funktempo (tempo 0/1), from
    /// the `funk` length its low bit picks, which it then flips.
    fn tick_step(&mut self, c: usize) -> bool {
        let funk = self.funk;
        let ch = &mut self.channels[c];
        ch.tick = ch.tick.wrapping_sub(1);
        ch.at_row_start = ch.tick == 0;
        ch.row_tick = if ch.at_row_start {
            0
        } else {
            ch.row_tick.saturating_add(1)
        };
        if ch.tick >= 0x80 {
            if ch.fixed != 0 {
                ch.tick = ch.fixed - 1;
            } else if ch.tempo >= 2 {
                ch.tick = ch.tempo;
            } else {
                ch.tick = funk[ch.tempo as usize];
                ch.tempo ^= 1;
            }
            ch.period = ch.tick.wrapping_add(1);
        }
        ch.at_row_start
    }

    fn current_row(&self, c: usize) -> Row {
        let ch = &self.channels[c];
        self.song.patterns[ch.pattern]
            .rows
            .get(ch.row)
            .copied()
            .unwrap_or_default()
    }

    /// The row channel `c` reads at the next row start, without moving it.
    fn next_row(&self, c: usize) -> Row {
        let ch = &self.channels[c];
        let rows = &self.song.patterns[ch.pattern].rows;
        if ch.row + 1 < rows.len() {
            return rows[ch.row + 1];
        }
        let list = &self.song.subsongs[self.subsong].orderlists[c];
        let pattern = if ch.repeat_left > 1 {
            ch.pattern
        } else {
            let next = if ch.order + 1 < list.entries.len() {
                ch.order + 1
            } else {
                list.restart as usize
            };
            list.entries[next].pattern as usize
        };
        self.song.patterns[pattern.min(self.song.patterns.len() - 1)].rows[0]
    }

    fn advance_row(&mut self, c: usize) {
        let len = self.song.patterns[self.channels[c].pattern].rows.len();
        let ch = &mut self.channels[c];
        ch.row += 1;
        if ch.row < len {
            return;
        }
        if ch.repeat_left > 1 {
            ch.repeat_left -= 1;
            ch.row = 0;
            return;
        }
        let list = &self.song.subsongs[self.subsong].orderlists[c];
        let next = if ch.order + 1 < list.entries.len() {
            ch.order + 1
        } else {
            self.looped = true;
            list.restart as usize
        };
        Self::enter_order(ch, list, next, &self.song);
    }

    fn instrument(&self, c: usize) -> Option<&Instrument> {
        let n = self.channels[c].instrument;
        if n == 0 {
            None
        } else {
            self.song.instruments.get(n - 1)
        }
    }

    fn speed_row(&self, ptr: u8) -> Option<TableRow> {
        table_row(&self.song.tables.speed, ptr)
    }

    fn read_row(&mut self, c: usize, row: Row) {
        if row.instrument > 0 {
            self.channels[c].instrument = row.instrument as usize;
        }
        let tone_porta = row.command == 3;
        if (NOTE_FIRST..=NOTE_LAST).contains(&row.note) {
            let note = note_index(row.note, self.channels[c].transpose);
            self.new_note(c, note);
            if !tone_porta {
                self.trigger(c, note);
            }
            // Tie-note and glide rows: no trigger, and no tick-0 pitch
            // write. GT's realtime optimisation (goattrk2.c:55
            // optimizerealtime = 1) skips the tick-N effects on tick 0
            // (gplay.c:728), so the `3 00` instant jump ("$00 ... move
            // pitch instantly to target note", readme §3.2; gplay.c:807-811,
            // "ST = 00 slides instantly", commands PDF p.1) lands on tick 1
            // and is re-asserted every tick the command stands (see
            // continuous, command 3). Pinned in S5: 4791 raw pattern rows
            // (not orderlist-expanded) in 56 of the 61 GTS5 corpus songs
            // are `3 00` with a real note (4930 with key-offs). Phase
            // aligned in S5.6 (.ai/sid-crosscheck-verdict.md (a), 1).
            // The glide's goal is the channel's note (`tone_porta`).
        } else if row.note == NOTE_KEY_OFF {
            self.channels[c].gate = false;
        } else if row.note == NOTE_KEY_ON {
            self.channels[c].gate = true;
        }
        let p = row.param;
        let speed_ptr = self.instrument(c).map(|i| i.speed_ptr).unwrap_or(0);
        let ch = &mut self.channels[c];
        ch.cmd = row.command;
        ch.param = p;
        match row.command {
            // GT's tick-0 commands (gplay.c:397-428): 0 hands the tick
            // effects to the instrument's vibrato (its speed row, on the
            // instrument the channel now has); 1 and 2 restart the vibrato
            // phase; 1-4 take over. 5-F below leave the running command.
            0x0 => {
                ch.run_cmd = 0;
                ch.run_param = speed_ptr;
            }
            0x1 | 0x2 => {
                ch.vib_time = 0;
                ch.run_cmd = row.command;
                ch.run_param = p;
            }
            0x3 | 0x4 => {
                ch.run_cmd = row.command;
                ch.run_param = p;
            }
            0x5 => ch.ad = p,
            0x6 => ch.sr = p,
            // CMD_SETWAVE (gcommon.h:11): the whole byte (gplay.c:433).
            0x7 => ch.waveform = p,
            0x8 => {
                ch.wave_ptr = p;
                ch.wave_wait = 0;
            }
            0x9 => {
                ch.pulse_ptr = p;
                ch.pulse_time = 0;
            }
            0xA => {
                self.filter_ptr = p;
                self.filter_time = 0;
            }
            // B 00 also stops the filter table (gplay.c:468-471).
            0xB => {
                self.res_filt = p;
                if p == 0 {
                    self.filter_ptr = 0;
                }
            }
            0xC => self.cutoff = (p as u16) << 3,
            // GT ignores a volume of $10 up (gplay.c:477-479).
            0xD if p < 0x10 => self.volume = p,
            // Funktempo (gplay.c:483-492): a speed-table row's two lengths
            // (each minus 1) become the alternating row lengths, and every
            // channel switches to funktempo.
            0xE => {
                if let Some(r) = self.speed_row(p).filter(|_| p != 0) {
                    self.funk = [r.left.wrapping_sub(1), r.right.wrapping_sub(1)];
                }
                for ch in self.channels.iter_mut() {
                    ch.tempo = 0;
                    ch.fixed = 0;
                }
            }
            // Tempo (gplay.c:494-508): 3 and up is the row length minus 1,
            // 0-2 as they stand (0 and 1 are funktempo); bit 7 sets this
            // channel's alone, otherwise all three.
            0xF => {
                let t = gt_tempo(p);
                if p >= 0x80 {
                    self.channels[c].tempo = t;
                    self.channels[c].fixed = 0;
                } else {
                    for ch in self.channels.iter_mut() {
                        ch.tempo = t;
                        ch.fixed = 0;
                    }
                }
            }
            _ => {}
        }
    }

    /// GT's new-note init (gplay.c:348-358), glide or trigger alike: the
    /// note, and the running command back to 0 with the instrument's speed
    /// row and vibrato delay.
    fn new_note(&mut self, c: usize, note: u8) {
        let (speed_ptr, delay) = self
            .instrument(c)
            .map(|i| (i.speed_ptr, i.vibrato_delay))
            .unwrap_or((0, 0));
        let ch = &mut self.channels[c];
        ch.base_note = note;
        ch.run_cmd = 0;
        ch.run_param = speed_ptr;
        ch.vib_delay = delay;
    }

    fn trigger(&mut self, c: usize, note: u8) {
        let ins = self.instrument(c).map(NoteStart::of);
        let ch = &mut self.channels[c];
        // GT sets no pitch on a note: its wave table's first step does, on the
        // frame after the note's (a row with note column $00 is
        // `cptr->note & 0x7f`, vibtime 0, gplay.c:714-721), so the note's
        // first frame sounds at the channel's old frequency (audible with a
        // first-frame byte without the test bit, e.g. $21). An instrument with
        // no wave table (none in GT) gets its pitch at once.
        ch.last_note = note & 0x7F;
        let gate_before = ch.gate;
        if ins.as_ref().is_none_or(|i| i.wave_ptr == 0) {
            ch.freq = gt_note_freq_reg(note);
        }
        ch.gate = true;
        ch.first_frame = true;
        ch.vib_time = 0;
        let Some(ins) = ins else {
            ch.first_wave = 0;
            return;
        };
        ch.ad = ins.ad;
        ch.sr = ins.sr;
        // GT holds the first-frame byte as the channel's waveform
        // (gplay.c:359-365) until the wave table sets one, so a table that
        // starts with `00` rows keeps $09 (test and gate) on. $00 leaves the
        // waveform and the gate as they are (after a hard restart's gate-off
        // the note stays off until something gates it), $FE/$FF set only the
        // gate. GT never sets the pulse width on a note (gplay.c:375-381):
        // only its pulse table does, so the channel's width carries over.
        match ins.first_wave {
            0 => ch.gate = gate_before,
            0xFE | 0xFF => ch.gate = ins.first_wave == 0xFF,
            fw => ch.waveform = fw,
        }
        ch.first_wave = ins.first_wave;
        ch.wave_ptr = ins.wave_ptr;
        ch.wave_wait = 0;
        // Same rule for the pulse table pointer (gplay.c:375-378): an
        // instrument with no pulse table leaves the running one going, so
        // its width keeps modulating under the new note.
        if ins.pulse_ptr != 0 {
            ch.pulse_ptr = ins.pulse_ptr;
            ch.pulse_time = 0;
        }
        // GT never touches the routing on a note: only its filter table and
        // command B do (gplay.c:268, 469).
        if ins.filter_ptr > 0 {
            self.filter_ptr = ins.filter_ptr;
            self.filter_time = 0;
        }
    }

    /// One vibrato frame, GoatTracker's (gplay.c:615-640, 776-799): speed
    /// row `ptr` gives the turn value (left) and the register step (right);
    /// a left of $80 up is the fine mode, whose step is the gap from the
    /// last note to the next, shifted right by `right`. `vibtime` is a u8:
    /// past the turn value (and below $80) it flips to its complement, then
    /// steps by 2; odd goes down, even up. A turn value k gives a first swing
    /// of k/2 + 1 frames, then k + 2 frames each way (even k).
    fn vibrato(&mut self, c: usize, ptr: u8) {
        let (mut turn, mut step) = self
            .speed_row(ptr)
            .map(|r| (r.left, r.right as u16))
            .unwrap_or((0, 0));
        let ch = &mut self.channels[c];
        if turn >= 0x80 {
            turn &= 0x7F;
            let at = |i: u8| if i < 0x80 { gt_note_freq_reg(i) } else { 0 };
            let shift = self
                .song
                .tables
                .speed
                .get(ptr as usize - 1)
                .map(|r| r.right)
                .unwrap_or(0);
            step = at(ch.last_note + 1)
                .wrapping_sub(at(ch.last_note))
                .checked_shr(shift as u32)
                .unwrap_or(0);
        }
        if ch.vib_time < 0x80 && ch.vib_time > turn {
            ch.vib_time ^= 0xFF;
        }
        ch.vib_time = ch.vib_time.wrapping_add(2);
        ch.freq = if ch.vib_time & 1 != 0 {
            ch.freq.wrapping_sub(step)
        } else {
            ch.freq.wrapping_add(step)
        };
    }

    /// A slide speed from speed-table row `ptr` (0: none), GT's (gplay.c:
    /// 733-741): left<<8 | right, or from $8000 up the fine speed, the gap from
    /// the last note to the next shifted right `right` times. As the C64
    /// player shifts (player.s mt_csloop, one `lsr` per count), a count of 16
    /// and up is 0; GT's editor, in C on x86, shifts by the count mod 32.
    fn speed(&self, c: usize, ptr: u8) -> u16 {
        let Some(r) = self.speed_row(ptr) else {
            return 0;
        };
        let v = (r.left as u16) << 8 | r.right as u16;
        if v < 0x8000 {
            return v;
        }
        let at = |i: u8| if i < 0x80 { gt_note_freq_reg(i) } else { 0 };
        let last = self.channels[c].last_note;
        at(last.wrapping_add(1))
            .wrapping_sub(at(last))
            .checked_shr(r.right as u32)
            .unwrap_or(0)
    }

    /// One tone-portamento frame toward the channel's note, GT's (gplay.c:
    /// 802-835): speed row `ptr`, 0 = straight to it; arriving (or jumping)
    /// restarts the vibrato phase. The step wraps the register as GT's does.
    fn tone_porta(&mut self, c: usize, ptr: u8) {
        let speed = if ptr == 0 { 0 } else { self.speed(c, ptr) };
        let ch = &mut self.channels[c];
        let target = gt_note_freq_reg(ch.base_note);
        if ptr == 0 {
            ch.freq = target;
            ch.vib_time = 0;
            return;
        }
        if ch.freq < target {
            ch.freq = ch.freq.wrapping_add(speed);
            if ch.freq > target {
                ch.freq = target;
                ch.vib_time = 0;
            }
        }
        if ch.freq > target {
            ch.freq = ch.freq.wrapping_sub(speed);
            if ch.freq < target {
                ch.freq = target;
                ch.vib_time = 0;
            }
        }
    }

    fn continuous(&mut self, c: usize) {
        if self.channels[c].first_frame {
            return;
        }
        let (cmd, param) = (self.channels[c].run_cmd, self.channels[c].run_param);
        // GT's realtime optimisation skips the tick effects on tick 0 of
        // every row (goattrk2.c:55, gplay.c:728): the vibratos (S5.10) and
        // the slides 1-2 and the portamento 3 (S5.12), so a slide steps
        // tempo - 1 times per row. The preview voice has no rows.
        let sequenced = !self.suspended(c);
        let tick0 = self.channels[c].at_row_start && sequenced;
        // Not on a row's first frame; and never in preview or an audition, which
        // have no rows.
        let ticking = sequenced && !self.channels[c].at_row_start;
        match cmd {
            // The slides wrap the 16-bit register as GT's do (gplay.c:744, 760).
            0x1 if !tick0 => {
                let s = self.speed(c, param);
                let ch = &mut self.channels[c];
                ch.freq = ch.freq.wrapping_add(s);
            }
            0x2 if !tick0 => {
                let s = self.speed(c, param);
                let ch = &mut self.channels[c];
                ch.freq = ch.freq.wrapping_sub(s);
            }
            0x3 => {
                if param == 0 {
                    // Tie-note pitch, GT phase (S5.6): the tick effects run
                    // from tick 1 (tick 0 is skipped by the realtime
                    // optimisation, goattrk2.c:55 + gplay.c:728) and re-assert
                    // the last note's table frequency every tick command 3
                    // stands (gplay.c:802-811: cmddata == 0 → freq =
                    // freqtbl[cptr->note], vibtime = 0; cptr->note is the
                    // persistent last note, gplay.c:350 — a `3 00` on a rest
                    // or key-off row snaps back to it too). A wave-table note
                    // row wins its own frame (gplay.c:714-722 jumps past the
                    // effects); the wave step runs after this and overwrites
                    // the same way.
                    if ticking {
                        self.tone_porta(c, 0);
                    }
                } else if !tick0 {
                    // Portamento toward the note: tick 1 on (see above).
                    self.tone_porta(c, param);
                }
            }
            0x4 if !tick0 => self.vibrato(c, param),
            // The instrument vibrato is command 0's fall-through into 4
            // (gplay.c:767-772): no speed row or a delay of 0 never
            // vibrates; a delay above 1 counts down a frame; at 1 it swings.
            0x0 if !tick0 => {
                let ch = &mut self.channels[c];
                if param == 0 || ch.vib_delay == 0 {
                    return;
                }
                if ch.vib_delay > 1 {
                    ch.vib_delay -= 1;
                    return;
                }
                self.vibrato(c, param);
            }
            _ => {}
        }
    }

    /// The wave table's frame; `true` when a step set a note (or ran a
    /// table command), which ends GT's frame before the tick effects.
    fn wave_step(&mut self, c: usize) -> bool {
        let ch = &self.channels[c];
        // A note's frame ends before the wave table in GT (gplay.c:509-512).
        if ch.first_frame {
            return false;
        }
        let table = &self.song.tables.wave;
        let ch = &mut self.channels[c];
        let mut jumped = false;
        let mut noted = false;
        let mut command = None;
        while let Some(row) = table_row(table, ch.wave_ptr) {
            match row.left {
                0xFF => {
                    if jumped || row.right == 0 || row.right as usize > table.len() {
                        ch.wave_ptr = 0;
                        return false;
                    }
                    ch.wave_ptr = row.right;
                    jumped = true;
                    continue;
                }
                0x01..=0x0F => {
                    // A delayed step (pinned in S5): `left` frames of waiting,
                    // then the step's note on the next, so the row lasts
                    // left + 1 frames (GT readme §3.4.1's "02 03 ... Each step
                    // takes 3 ticks"). Note after the wait, not before:
                    // CONFIRMED by GT2 gplay.c:693-722 (the delay branch skips
                    // to the tick effects until the wait ends, then the row's
                    // note is applied as the pointer advances).
                    if ch.wave_wait == 0 {
                        ch.wave_wait = row.left + 1;
                    }
                    ch.wave_wait -= 1;
                    if ch.wave_wait == 0 {
                        noted = Self::wave_note(ch, row.right);
                        ch.wave_ptr = ch.wave_ptr.wrapping_add(1);
                    }
                }
                l => {
                    match l {
                        // The gate bit is the row's (gplay.c:525, 527): a row
                        // with it clear releases the note (S5.9).
                        0x10..=0xDF => ch.waveform = l,
                        0xE0..=0xEF => ch.waveform = l & 0x0F,
                        _ => {}
                    }
                    // $F0-$FE run a table command (`wave_command`): its right
                    // column is the command's parameter, not a note, and GT
                    // skips the tick effects that frame (gplay.c:704-710).
                    if l >= 0xF0 {
                        command = Some((l & 0x0F, row.right));
                        noted = true;
                    } else {
                        noted = Self::wave_note(ch, row.right);
                    }
                    ch.wave_ptr = ch.wave_ptr.wrapping_add(1);
                }
            }
            break;
        }
        if ch.wave_ptr as usize > table.len() {
            ch.wave_ptr = 0;
        }
        if let Some((cmd, param)) = command {
            self.wave_command(c, cmd, param);
        }
        noted
    }

    /// A wave-table command row ($F0 + `cmd`, parameter `param`), GT's
    /// (gplay.c:529-680): the pattern command of that number run for this
    /// frame, the slides and the vibrato as tick effects. 0, 8 and E are
    /// illegal in GT (its editor stops the song, gplay.c:534-538); here they
    /// do nothing. D sets the volume when its own parameter is below $10,
    /// as GT's C64 player does (player.s:291-305; $10 up is its "timing
    /// mark", nothing audible). GT's editor tests the pattern row's
    /// `newcmddata` instead and stores the parameter unmasked (gplay.c:
    /// 686-689), so `$FD 1F` put $10 into $D418's filter-mode bits there.
    fn wave_command(&mut self, c: usize, cmd: u8, param: u8) {
        match cmd {
            0x1 => {
                let s = self.speed(c, param);
                let ch = &mut self.channels[c];
                ch.freq = ch.freq.wrapping_add(s);
            }
            0x2 => {
                let s = self.speed(c, param);
                let ch = &mut self.channels[c];
                ch.freq = ch.freq.wrapping_sub(s);
            }
            0x3 => self.tone_porta(c, param),
            0x4 => self.vibrato(c, param),
            // AD/SR stand until the next note or hard restart.
            0x5 => self.channels[c].ad = param,
            0x6 => self.channels[c].sr = param,
            0x7 => self.channels[c].waveform = param,
            0x9 => {
                let ch = &mut self.channels[c];
                ch.pulse_ptr = param;
                ch.pulse_time = 0;
            }
            0xA => {
                self.filter_ptr = param;
                self.filter_time = 0;
            }
            0xB => {
                self.res_filt = param;
                if param == 0 {
                    self.filter_ptr = 0;
                }
            }
            0xC => self.cutoff = (param as u16) << 3,
            0xD if param < 0x10 => self.volume = param,
            _ => {}
        }
    }

    /// A wave-table row's right column: the note it sets, under any command.
    /// GT's wave-note path has no command check (gplay.c:714-722): the note
    /// applies while a portamento stands too, and that frame skips the tick
    /// effects (gplay.c:722), so the wave note wins it. S5.6: the S3-era
    /// suppression under commands 1-3 is removed. S5.10: GT's arithmetic,
    /// mod 128 (gplay.c:714-721): $00-$7F is added to the channel's note
    /// ($7F is one down, $60 is 32 down only when that stays above C-0),
    /// $81-$FF is the absolute note `right & 0x7f`, then `& 0x7f`; never
    /// clamped. The step resets the vibrato phase and is the fine vibrato's
    /// last note. `false`: $80, no note.
    fn wave_note(ch: &mut Channel, right: u8) -> bool {
        if right == 0x80 {
            return false;
        }
        let note = if right < 0x80 {
            ch.base_note.wrapping_add(right)
        } else {
            right
        } & 0x7F;
        ch.freq = gt_note_freq_reg(note);
        ch.vib_time = 0;
        ch.last_note = note;
        true
    }

    /// Frames GT leaves the pulse table alone. A note's first frame ends the
    /// channel's frame before the tables (gplay.c:509-512). With GT's default
    /// pulse optimisation (goattrk2.c:54 `optimizepulse = 1`, which its
    /// packed player's option mirrors): the frame the next row is read, the
    /// gate timer's (gplay.c:846-849), and the first frame of a pattern's last
    /// row, where the sequencer has just moved on (gplay.c:853-857).
    fn pulse_skipped(&self, c: usize) -> bool {
        let ch = &self.channels[c];
        if ch.first_frame {
            return true;
        }
        if self.suspended(c) {
            return false;
        }
        let timer = self.instrument(c).map(|i| i.gate_timer).unwrap_or(0);
        if timer != 0 && ch.tick == timer {
            return true;
        }
        ch.at_row_start && ch.row + 1 == self.song.patterns[ch.pattern].rows.len()
    }

    /// GT's pulse table (gplay.c:851-897). A jump lands on its target and
    /// takes that row as data whatever it is (a second jump there is a width
    /// row: `$FF xx` sets $Fxx), a jump to 0 stops; a width row (left $80 up)
    /// sets the width; a left of 1-$7F adds the signed right that many frames
    /// from this frame on; a left of 0 stalls the table there. Past the stored
    /// rows GT's tables are zero, so the table stalls.
    fn pulse_step(&mut self, c: usize) {
        let table = &self.song.tables.pulse;
        let row_at = |ptr: u8| table.get(ptr as usize - 1).copied().unwrap_or_default();
        let ch = &mut self.channels[c];
        if ch.pulse_ptr == 0 {
            return;
        }
        let jump = row_at(ch.pulse_ptr);
        if jump.left == 0xFF {
            ch.pulse_ptr = jump.right;
            if ch.pulse_ptr == 0 {
                return;
            }
        }
        if ch.pulse_time == 0 {
            let row = row_at(ch.pulse_ptr);
            if row.left >= 0x80 {
                ch.pulse_width = ((row.left as u16 & 0x0F) << 8) | row.right as u16;
                ch.pulse_ptr = ch.pulse_ptr.wrapping_add(1);
            } else {
                ch.pulse_time = row.left;
                ch.pulse_speed = row.right as i8;
            }
        }
        if ch.pulse_time > 0 {
            ch.pulse_width = ((ch.pulse_width as i32 + ch.pulse_speed as i32) & 0xFFF) as u16;
            ch.pulse_time -= 1;
            if ch.pulse_time == 0 {
                ch.pulse_ptr = ch.pulse_ptr.wrapping_add(1);
            }
        }
    }

    /// GT's filter table (gplay.c:253-296), at the top of the frame. A jump
    /// lands on its target and takes that row as data; a left of $80 up sets
    /// the mode and $17, and a cutoff row straight after it is taken with it;
    /// 1-$7F adds the signed right to the cutoff that many frames from this
    /// one on (the high byte, wrapping as GT's u8); 0 sets the cutoff. The
    /// table stopping (pointer 0: a jump to 0, B 00) stops a sweep too. Past
    /// the stored rows the table stops; GT would read zero rows there and set
    /// the cutoff to 0 every frame, which only an unterminated table (an
    /// app-made one) reaches.
    fn filter_step(&mut self) {
        if self.filter_ptr == 0 {
            return;
        }
        let table = &self.song.tables.filter;
        let row_at = |ptr: u8| {
            if ptr == 0 {
                TableRow::default()
            } else {
                table.get(ptr as usize - 1).copied().unwrap_or_default()
            }
        };
        let jump = row_at(self.filter_ptr);
        if jump.left == 0xFF {
            self.filter_ptr = jump.right;
            if self.filter_ptr == 0 {
                return;
            }
        }
        if self.filter_time == 0 {
            if self.filter_ptr as usize > table.len() {
                self.filter_ptr = 0;
                return;
            }
            let row = row_at(self.filter_ptr);
            if row.left >= 0x80 {
                self.mode = (row.left >> 4) & 0x07;
                self.res_filt = row.right;
                self.filter_ptr = self.filter_ptr.wrapping_add(1);
                let next = row_at(self.filter_ptr);
                if self.filter_ptr != 0 && next.left == 0x00 {
                    self.cutoff = (next.right as u16) << 3;
                    self.filter_ptr = self.filter_ptr.wrapping_add(1);
                }
            } else if row.left != 0 {
                self.filter_time = row.left;
                self.filter_speed = row.right as i8;
            } else {
                self.cutoff = (row.right as u16) << 3;
                self.filter_ptr = self.filter_ptr.wrapping_add(1);
            }
        }
        if self.filter_time > 0 {
            let hi = ((self.cutoff >> 3) as u8).wrapping_add(self.filter_speed as u8);
            self.cutoff = (hi as u16) << 3 | (self.cutoff & 0x07);
            self.filter_time -= 1;
            if self.filter_time == 0 {
                self.filter_ptr = self.filter_ptr.wrapping_add(1);
            }
        }
    }

    fn hard_restart(&mut self, c: usize) {
        let Some(timer) = self.instrument(c).map(|i| i.gate_timer) else {
            return;
        };
        // GT compares its countdown with the gate timer (gplay.c:898):
        // `timer` frames before the row ends.
        if timer == 0 || self.suspended(c) || self.channels[c].tick != timer {
            return;
        }
        let next = self.next_row(c);
        // GT reads the next row `gatetimer` ticks early and acts on a key off or
        // key on there and then (gplay.c:920-923), not when the row starts; a
        // note gets the gate-off / hard restart below. Only the gate moves: a
        // key off leaves AD/SR alone.
        if next.note == NOTE_KEY_OFF || next.note == NOTE_KEY_ON {
            self.channels[c].gate = next.note == NOTE_KEY_ON;
            return;
        }
        if !(NOTE_FIRST..=NOTE_LAST).contains(&next.note) || next.command == 3 {
            return;
        }
        // GT switches the channel to the row's instrument as it reads the row,
        // before it tests the flags (gplay.c:907-908, 924-926): whether the
        // coming note is preceded by a gate-off and a hard restart is the NEXT
        // note's instrument's say, not the sounding one's. The timer that got
        // us here is the sounding one's.
        let flags = if next.instrument > 0 {
            self.song.instruments.get(next.instrument as usize - 1)
        } else {
            self.instrument(c)
        };
        let Some((no_gate_off, hr)) = flags.map(|i| (i.no_gate_off, i.hard_restart)) else {
            return;
        };
        if no_gate_off {
            return;
        }
        let ch = &mut self.channels[c];
        ch.gate = false;
        if hr {
            ch.ad = HARD_RESTART_AD;
            ch.sr = 0;
        }
    }

    /// Writes the frame's registers the way GoatTracker's SID interface does
    /// (gsid.cpp, sid_fillbuffer): one at a time in its `sidorder`, the filter
    /// first, then per voice pulse width, SR, AD, frequency and the control
    /// register last, each `WRITE_SPACING` cycles after the one before and
    /// `CONTROL_EXTRA` more before a control register (a real playroutine's
    /// `lda`/`sta` pair). The spacing is what lets the envelope's rate counter
    /// run on between SR/AD and the gate, so a note-on lands on the ADSR delay
    /// bug as it does in GT (the attack starts up to ~30 ms late) and a hard
    /// restart's gap is as long as GT's.
    fn write_registers(&mut self) {
        let mut at: u64 = 0;
        let mut put = |chip: &mut Chip, reg: u8, val: u8, control: bool| {
            if control {
                at += CONTROL_EXTRA;
            }
            chip.write_after(at, reg, val);
            at += WRITE_SPACING;
        };
        let [fc_lo, fc_hi, res_filt, mode_vol] = self.filter_regs;
        put(&mut self.chip, REG_FC_LO, fc_lo, false);
        put(&mut self.chip, REG_FC_HI, fc_hi, false);
        put(&mut self.chip, REG_MODE_VOL, mode_vol, false);
        put(&mut self.chip, REG_RES_FILT, res_filt, false);
        for (c, ch) in self.channels.iter().enumerate() {
            let base = (c * 7) as u8;
            let control = if ch.first_frame && ch.first_wave != 0 && ch.first_wave < 0xFE {
                ch.first_wave
            } else {
                // `wave & gate` (gplay.c:945): the channel's gate is a mask.
                ch.waveform & if ch.gate { 0xFF } else { !GATE }
            };
            // GT writes the low byte with bit 0 clear (gplay.c:943).
            put(
                &mut self.chip,
                base + 2,
                (ch.pulse_width & 0xFE) as u8,
                false,
            );
            put(&mut self.chip, base + 3, (ch.pulse_width >> 8) as u8, false);
            put(&mut self.chip, base + 6, ch.sr, false);
            put(&mut self.chip, base + 5, ch.ad, false);
            put(&mut self.chip, base, (ch.freq & 0xFF) as u8, false);
            put(&mut self.chip, base + 1, (ch.freq >> 8) as u8, false);
            put(&mut self.chip, base + 4, control, true);
        }
    }
}
