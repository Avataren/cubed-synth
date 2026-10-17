//! The replayer: walks the order list row by row and tick by tick, runs each
//! channel's effects and plays the result on one sample voice per channel.
//!
//! The row and tick logic is a port of the DAW's `engine.ts` (`scheduleAhead`)
//! and `row-scheduler.ts` (`scheduleRow`), with times kept the same way (in
//! seconds, row by row), so the two can be compared command for command.
//! Events land on the output frame their time falls on.

#[cfg(feature = "adlib")]
use crate::adlib::AdlibDriver;
use crate::effects::{js_round, Command, Commands, EffectState, VolumeCommand};
use crate::filter::AmigaFilter;
use crate::module::{Format, ModFlavor, Module, Note};
use crate::pitch::PAULA_TO_SYNTH_SCALE;
use crate::profile::{Decoded, Effect, EffectType, ExtSub, Profile};
use crate::protracker::period_to_frequency;
use crate::voice::{prepare_tables, PreparedSample, Voice, XmVoice, CUT_SECONDS, RELEASE_SECONDS};

/// The rate MOD samples are declared at; their root note makes up for it.
const MOD_SAMPLE_RATE: f64 = 44100.0;

/// Sees what the player does, as it does it: rows as they start, and every
/// command a channel's voice receives. Times are song seconds.
pub trait Observer {
    fn row(&mut self, time: f64, order: usize, row: usize) {
        let _ = (time, order, row);
    }
    /// `instrument` is the one the channel plays (1-based).
    fn command(&mut self, time: f64, channel: usize, instrument: u8, command: &Command) {
        let _ = (time, channel, instrument, command);
    }
    fn global_volume(&mut self, time: f64, volume: f64) {
        let _ = (time, volume);
    }
    /// The song looped back to its start and every note was cut.
    fn all_notes_off(&mut self, time: f64) {
        let _ = time;
    }
    /// The Amiga LED filter was switched.
    fn filter(&mut self, time: f64, on: bool) {
        let _ = (time, on);
    }
    /// A register write to the S3M AdLib chip.
    fn adlib_write(&mut self, time: f64, register: u16, value: u8) {
        let _ = (time, register, value);
    }
}

struct NoObserver;
impl Observer for NoObserver {}

/// Options for a [`Player`].
#[derive(Clone, Copy, Debug)]
pub struct PlayerOptions {
    /// Wrap to the first order at the end instead of stopping.
    pub looped: bool,
    /// Output gain on top of the format's own trim.
    pub gain: f32,
    /// MOD: play through the Amiga's output filter (the fixed RC low-pass,
    /// and the LED filter E0x switches).
    pub filter: bool,
    /// Stereo width, 0 (mono) to 1 (the pans as written); None plays the
    /// module's own [`stereo_separation`](Module::stereo_separation).
    pub stereo_separation: Option<f32>,
    /// Play what the another_synth DAW plays where it departs from the
    /// original trackers, so the two can be compared command for command.
    #[doc(hidden)]
    pub daw_quirks: bool,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        PlayerOptions {
            looped: true,
            gain: 1.0,
            filter: true,
            stereo_separation: None,
            daw_quirks: false,
        }
    }
}

/// One row's worth of a channel's cell, decoded the way the format reads it
/// (the DAW's playback `Step`).
#[derive(Clone, Copy, Debug, Default)]
struct Step {
    instrument: Option<u8>,
    midi: Option<f64>,
    frequency: Option<f64>,
    note_off: bool,
    /// 0-255.
    velocity: Option<u8>,
    pan: Option<f64>,
    effect: Option<Effect>,
    /// The cell's own command and parameter, where the format's commands
    /// share one parameter memory (S3M).
    raw_effect: Option<(u8, u8)>,
    speed: Option<u8>,
    tempo: Option<u8>,
    volume_command: Option<VolumeCommand>,
    volume_column_volume: bool,
    /// XM: an instrument number with no note puts the channel back at its
    /// sample's pan (0-1).
    reset_pan: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PositionCommand {
    Jump(usize),
    Break(usize),
}

struct Channel {
    fx: EffectState,
    voice: Voice,
    /// The voice a new note cut, fading out.
    tail: Voice,
    /// MOD: the sample number the channel last saw.
    latched_sample: u8,
    /// The last note a row stated: a tone portamento without one slides there.
    last_note: Option<(f64, Option<f64>)>,
    /// This row's work on ticks after the first.
    tick_effect: Option<Effect>,
    tick_volume: Option<VolumeCommand>,
    /// S3M: a tone portamento continuing through a row with no cell.
    continues: bool,
    muted: bool,
    /// The pan a note with none of its own plays at, 0-1.
    pan: f64,
    /// XM: this row's cell names an instrument (a note then takes its
    /// sample's pan).
    instrument_row: bool,
    /// This row's global volume slide (FT2 runs it every tick).
    global_slide: Option<u8>,
    last_global_slide: u8,
}

/// The transport's place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub order: usize,
    pub pattern: usize,
    pub row: usize,
}

/// Plays a [`Module`] into stereo blocks.
pub struct Player {
    module: Module,
    profile: Profile,
    samples: Vec<Option<PreparedSample>>,
    rate: f64,
    options: PlayerOptions,
    channels: Vec<Channel>,

    // Transport, as engine.ts keeps it.
    order: usize,
    last_row: isize,
    length: usize,
    next_row_time: f64,
    speed: u32,
    bpm: f64,
    pending_position: Option<PositionCommand>,
    loop_start: usize,
    loop_count: u8,
    loop_pending: bool,
    delay_count: u8,
    delay_repeating: bool,
    global_volume: f64,
    song_stop_pending: bool,
    end_time: Option<f64>,
    finished: bool,
    loop_pattern: bool,

    // The row playing.
    row: usize,
    row_time: f64,
    sec_per_tick: f64,
    ticks_per_row: u32,
    next_tick: u32,
    position: Position,

    frame: u64,
    master: f32,
    master_target: f32,
    output_gain: f32,
    filter: Option<AmigaFilter>,
    fade_out: Option<u32>,
    separation: f32,
    /// The song time of the event being run.
    last_event_time: f64,
    /// The song's own global volume, still to be reported.
    announce_global: bool,
    #[cfg(feature = "adlib")]
    adlib: Option<AdlibDriver>,
}

impl Player {
    /// A player for `module` at `sample_rate`. Builds everything it needs:
    /// rendering never allocates.
    pub fn new(module: Module, sample_rate: f32, options: PlayerOptions) -> Player {
        prepare_tables();
        let rate = sample_rate as f64;
        let quirks = options.daw_quirks;
        let mut profile = match module.format {
            Format::Mod => Profile::protracker(module.flags.amiga_limits),
            Format::S3m => Profile::s3m(
                module.flags.amiga_limits,
                module.flags.fast_volume_slides,
                quirks,
            ),
            Format::Xm => Profile::xm(module.flags.linear_frequency),
        };
        profile.daw_quirks = quirks;
        let samples = module
            .samples
            .iter()
            .map(|s| Some(PreparedSample::new(s, sample_rate_per_hz(module.format, s))))
            .collect();
        let format_trim_db = match module.format {
            // The DAW trims every PC MOD by 6 dB, too little for 28 channels.
            Format::Mod if module.channels > 4 && quirks => -6.0,
            // An Amiga MOD has two hard-panned channels a side; a PC MOD
            // centres all of its own, so its power grows with their number.
            Format::Mod => -4.0 - 10.0 * (module.channels.max(4) as f32 / 4.0).log10(),
            Format::S3m | Format::Xm => -6.0,
        };
        let channels = (0..module.channels)
            .map(|_| Channel {
                fx: EffectState::new(profile),
                voice: Voice::default(),
                tail: Voice::default(),
                latched_sample: 0,
                last_note: None,
                tick_effect: None,
                tick_volume: None,
                continues: false,
                muted: false,
                pan: 0.5,
                instrument_row: false,
                global_slide: None,
                last_global_slide: 0,
            })
            .collect();
        let filter =
            (module.format == Format::Mod && options.filter).then(|| AmigaFilter::new(rate));
        // Channels the file switches off are not heard (the DAW plays them).
        let mut channels: Vec<Channel> = channels;
        for (c, &off) in channels.iter_mut().zip(&module.muted) {
            c.muted = off && !quirks;
        }
        let separation = options
            .stereo_separation
            .unwrap_or(module.stereo_separation)
            .clamp(0.0, 1.0);
        #[cfg(feature = "adlib")]
        let adlib = AdlibDriver::new(&module, rate);
        let mut player = Player {
            #[cfg(feature = "adlib")]
            adlib,
            separation,
            last_event_time: 0.0,
            announce_global: false,
            speed: module.speed.max(1) as u32,
            bpm: (module.tempo as f64).clamp(32.0, 255.0),
            module,
            profile,
            samples,
            rate,
            options,
            channels,
            order: 0,
            last_row: -1,
            length: 64,
            next_row_time: 0.0,
            pending_position: None,
            loop_start: 0,
            loop_count: 0,
            loop_pending: false,
            delay_count: 0,
            delay_repeating: false,
            global_volume: 1.0,
            song_stop_pending: false,
            end_time: None,
            finished: false,
            loop_pattern: false,
            row: 0,
            row_time: 0.0,
            sec_per_tick: 0.0,
            ticks_per_row: 0,
            next_tick: 0,
            position: Position {
                order: 0,
                pattern: 0,
                row: 0,
            },
            frame: 0,
            master: 1.0,
            master_target: 1.0,
            output_gain: 10f32.powf(format_trim_db / 20.0) * options.gain,
            filter,
            fade_out: None,
        };
        player.seek(0, 0);
        player
    }

    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Where the song is: the row playing.
    pub fn position(&self) -> Position {
        self.position
    }

    /// Whether the song has played to its end (never, when looped).
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Song seconds played so far.
    pub fn time(&self) -> f64 {
        self.frame as f64 / self.rate
    }

    pub fn set_looped(&mut self, looped: bool) {
        self.options.looped = looped;
    }

    /// Repeat the current pattern instead of moving through the order list.
    pub fn set_pattern_loop(&mut self, looped: bool) {
        self.loop_pattern = looped;
    }

    /// Stereo width from now on: 0 (mono) to 1 (the pans as written).
    pub fn set_stereo_separation(&mut self, separation: f32) {
        self.separation = separation.clamp(0.0, 1.0);
    }

    pub fn stereo_separation(&self) -> f32 {
        self.separation
    }

    /// Silences a channel; it keeps playing underneath (an AdLib channel's
    /// note stops).
    pub fn set_channel_muted(&mut self, channel: usize, muted: bool) {
        if let Some(c) = self.channels.get_mut(channel) {
            c.muted = muted;
        }
        #[cfg(feature = "adlib")]
        if let (Some(adlib), true) = (&mut self.adlib, muted) {
            adlib.note_off(channel);
            adlib.flush();
        }
    }

    /// Starts again from `order` and `row`, from a clean state. Speed and
    /// tempo are those the song would have reached by playing there in order.
    pub fn seek(&mut self, order: usize, row: usize) {
        let order = order.min(self.module.orders.len().saturating_sub(1));
        self.reset_effect_states();
        for c in &mut self.channels {
            c.voice.active = false;
            c.tail.active = false;
        }
        #[cfg(feature = "adlib")]
        if let Some(adlib) = &mut self.adlib {
            adlib.reset();
        }
        self.order = order;
        self.load_pattern();
        let row = row.min(self.length.saturating_sub(1));
        self.last_row = row as isize - 1;
        self.next_row_time = self.frame as f64 / self.rate;
        self.next_tick = self.ticks_per_row;
        self.finished = self.module.orders.is_empty();
        self.end_time = None;
        self.song_stop_pending = false;
        self.fade_out = None;
        self.announce_global = self.module.global_volume != 1.0;
        self.apply_timing_for_start(order, row);
        self.prime_instrument_latches(order, row);
        for (c, &pan) in self.channels.iter_mut().zip(&self.module.channel_pan) {
            c.pan = pan.unwrap_or(0.5) as f64;
        }
    }

    fn load_pattern(&mut self) {
        let pattern = self.module.orders.get(self.order).copied().unwrap_or(0);
        self.length = self
            .module
            .patterns
            .get(pattern)
            .map_or(64, |p| p.rows().max(1));
    }

    /// Speed and tempo as the last rows before `order`/`row` set them.
    fn apply_timing_for_start(&mut self, order: usize, row: usize) {
        let (mut speed, mut bpm) = (self.module.speed as u32, self.module.tempo as f64);
        for index in 0..=order.min(self.module.orders.len().saturating_sub(1)) {
            let Some(pattern) = self.module.patterns.get(self.module.orders[index]) else {
                continue;
            };
            let limit = if index == order {
                row
            } else {
                pattern.rows().max(1)
            };
            for r in 0..limit.min(pattern.rows()) {
                for (ch, cell) in pattern.row(r).iter().enumerate() {
                    let _ = ch;
                    if let Some(step) = self.peek_step(cell) {
                        if let Some(s) = step.speed {
                            speed = s as u32;
                        }
                        if let Some(t) = step.tempo {
                            bpm = t as f64;
                        }
                    }
                }
            }
        }
        self.set_speed(speed);
        self.set_bpm(bpm);
    }

    /// The instrument each channel would hold had the song played to here.
    fn prime_instrument_latches(&mut self, order: usize, row: usize) {
        if order == 0 && row == 0 {
            return;
        }
        for index in 0..=order {
            let Some(pattern) = self.module.patterns.get(self.module.orders[index]) else {
                continue;
            };
            let limit = if index == order { row } else { pattern.rows() };
            for r in 0..limit.min(pattern.rows()) {
                for ch in 0..self.channels.len() {
                    let cell = *pattern.cell(r, ch);
                    let latched = self.channels[ch].latched_sample;
                    if let Some(i) = self
                        .read_step(&cell, ch, latched)
                        .and_then(|s| s.instrument)
                    {
                        self.channels[ch].fx.instrument = Some(i);
                    }
                    if cell.instrument > 0 {
                        self.channels[ch].latched_sample = cell.instrument;
                    }
                }
            }
        }
    }

    fn set_speed(&mut self, speed: u32) {
        self.speed = speed.clamp(1, 255);
    }

    fn set_bpm(&mut self, bpm: f64) {
        self.bpm = bpm.clamp(32.0, 255.0);
    }

    /// timing-system.ts's arithmetic, in its order, so times agree with it.
    fn ms_per_row(&self) -> f64 {
        (60_000.0 / self.bpm) / 4.0 * (self.speed as f64 / 6.0)
    }

    fn sec_per_row(&self) -> f64 {
        self.ms_per_row() / 1000.0
    }

    fn reset_effect_states(&mut self) {
        for c in &mut self.channels {
            c.fx = EffectState::new(self.profile);
            c.last_note = None;
            c.latched_sample = 0;
            c.pan = 0.5;
            c.tick_effect = None;
            c.tick_volume = None;
            c.continues = false;
            c.global_slide = None;
            c.last_global_slide = 0;
        }
        self.pending_position = None;
        self.loop_start = 0;
        self.loop_count = 0;
        self.loop_pending = false;
        self.delay_count = 0;
        self.delay_repeating = false;
        self.global_volume = self.module.global_volume as f64;
        self.master_target = self.global_volume as f32;
    }

    /// Renders the next `left.len()` frames over `left` and `right`. Returns
    /// false once the song has ended (what it wrote still plays).
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        self.render_observed(left, right, &mut NoObserver)
    }

    /// [`render`](Self::render), telling `observer` what happens.
    pub fn render_observed(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        observer: &mut dyn Observer,
    ) -> bool {
        let n = left.len().min(right.len());
        left[..n].fill(0.0);
        right[..n].fill(0.0);
        let mut at = 0;
        while at < n {
            if self.fade_out == Some(0) {
                break;
            }
            let now = self.frame + at as u64;
            if self.fade_out.is_none() {
                if let Some(time) = self.next_event_time() {
                    let event_frame = self.frame_at(time);
                    if event_frame <= now {
                        self.run_event(observer);
                        continue;
                    }
                    let k = ((event_frame - now) as usize).min(n - at);
                    self.mix(&mut left[at..at + k], &mut right[at..at + k]);
                    at += k;
                    continue;
                }
                // The song is over: fade what still sounds.
                self.fade_out = Some((0.01 * self.rate) as u32);
            }
            let k = (n - at).min(self.fade_out.unwrap_or(0) as usize);
            self.mix(&mut left[at..at + k], &mut right[at..at + k]);
            let frames = self.fade_out.unwrap_or(0);
            let total = (0.01 * self.rate).max(1.0) as f32;
            for i in 0..k {
                let g = (frames as f32 - i as f32) / total;
                left[at + i] *= g;
                right[at + i] *= g;
            }
            self.fade_out = Some(frames - k as u32);
            at += k;
        }
        self.frame += n as u64;
        if self.separation < 1.0 {
            // Mid/side: the side signal scaled, the mid kept.
            let width = self.separation;
            for (l, r) in left[..n].iter_mut().zip(right[..n].iter_mut()) {
                let (mid, side) = ((*l + *r) * 0.5, (*l - *r) * 0.5 * width);
                *l = mid + side;
                *r = mid - side;
            }
        }
        if let Some(filter) = &mut self.filter {
            filter.process(&mut left[..n], &mut right[..n]);
        }
        self.fade_out != Some(0)
    }

    fn frame_at(&self, time: f64) -> u64 {
        (time * self.rate - 1e-6).ceil().max(0.0) as u64
    }

    /// The song time of the next row or tick, or None once the song is over.
    fn next_event_time(&self) -> Option<f64> {
        if self.finished {
            return None;
        }
        if self.next_tick < self.ticks_per_row {
            return Some(self.row_time + self.next_tick as f64 * self.sec_per_tick);
        }
        if let Some(end) = self.end_time {
            return Some(end);
        }
        Some(self.next_row_time)
    }

    fn run_event(&mut self, observer: &mut dyn Observer) {
        self.last_event_time = self.next_event_time().unwrap_or(self.last_event_time);
        #[cfg(feature = "adlib")]
        self.report_adlib(observer);
        if self.next_tick < self.ticks_per_row {
            let tick = self.next_tick;
            self.next_tick += 1;
            self.run_tick(tick, observer);
        } else if self.end_time.is_some() {
            self.finished = true;
        } else {
            self.advance(observer);
        }
        #[cfg(feature = "adlib")]
        if let Some(adlib) = &mut self.adlib {
            adlib.flush();
            self.report_adlib(observer);
        }
        if self.module.format == Format::Xm {
            self.tick_voices();
        }
    }

    /// XM: every voice's envelopes and vibrato, a tick on.
    fn tick_voices(&mut self) {
        let frames = (self.sec_per_tick * self.rate).round() as u32;
        for channel in &mut self.channels {
            for voice in [&mut channel.voice, &mut channel.tail] {
                if let Some(instrument) = voice
                    .xm
                    .and_then(|x| self.module.instruments.get(x.instrument))
                {
                    if voice.active {
                        voice.tick_xm(instrument, frames);
                    }
                }
            }
        }
    }

    /// Hands the AdLib writes since the last report to the observer, at
    /// the time of the event that made them.
    #[cfg(feature = "adlib")]
    fn report_adlib(&mut self, observer: &mut dyn Observer) {
        let time = self.last_event_time;
        if let Some(adlib) = &mut self.adlib {
            let native = 1.0 / cubed_opl::NATIVE_RATE;
            for &(register, value, held) in adlib.take_logged() {
                observer.adlib_write(if held { time + native } else { time }, register, value);
            }
        }
    }

    /// One pass of `scheduleAhead`'s loop: settles where the song goes next
    /// and starts that row, or ends the song.
    fn advance(&mut self, observer: &mut dyn Observer) {
        if self.announce_global {
            self.announce_global = false;
            observer.global_volume(self.next_row_time, self.global_volume);
        }
        loop {
            let current = self.last_row + 1;
            let actual = (current as usize) % self.length;

            if let Some(command) = self.pending_position.filter(|_| !self.delay_repeating) {
                self.pending_position = None;
                let count = self.module.orders.len();
                match command {
                    PositionCommand::Jump(target) => {
                        // S3M counts positions in the file's order list.
                        let target = match &self.module.jump_targets {
                            Some(map) if !self.profile.daw_quirks => {
                                map.get(target).copied().unwrap_or(usize::MAX)
                            }
                            _ => target,
                        };
                        if target >= count {
                            self.finished = true;
                            return;
                        }
                        if !self.options.looped && self.order == count - 1 && target <= self.order {
                            self.end_time = Some(self.next_row_time);
                            return;
                        }
                        self.order = target;
                        self.load_pattern();
                        self.last_row = -1;
                    }
                    PositionCommand::Break(target) => {
                        self.order += 1;
                        if self.order >= count && !self.wrap_or_finish(observer) {
                            return;
                        }
                        self.load_pattern();
                        let row = target.min(self.length - 1);
                        self.last_row = row as isize - 1;
                    }
                }
                continue;
            }

            if actual == 0 && current > 0 && !self.loop_pattern {
                self.order += 1;
                if self.order >= self.module.orders.len() && !self.wrap_or_finish(observer) {
                    return;
                }
                self.load_pattern();
                self.loop_start = 0;
                self.loop_count = 0;
                self.loop_pending = false;
                self.last_row = -1;
                continue;
            }

            let time = self.next_row_time;
            self.schedule_row(actual, time, observer);

            if self.delay_count > 0 {
                self.delay_count -= 1;
                self.delay_repeating = true;
                self.next_row_time += self.sec_per_row();
                return;
            }
            self.delay_repeating = false;

            if self.loop_pending {
                self.loop_pending = false;
                self.last_row = self.loop_start as isize - 1;
                self.next_row_time += self.sec_per_row();
                return;
            }

            self.next_row_time += self.sec_per_row();
            self.last_row = current;
            if self.song_stop_pending {
                self.end_time = Some(self.next_row_time);
            }
            return;
        }
    }

    /// The order list ran out: wrap to the start (a restart, with every note
    /// cut) or end the song. False when the song ends.
    fn wrap_or_finish(&mut self, observer: &mut dyn Observer) -> bool {
        if self.options.looped {
            self.order = 0;
            let time = self.next_row_time;
            observer.all_notes_off(time);
            for c in &mut self.channels {
                c.voice.fade_out(CUT_SECONDS, self.rate);
            }
            #[cfg(feature = "adlib")]
            if let Some(adlib) = &mut self.adlib {
                adlib.all_notes_off();
            }
            self.reset_effect_states();
            observer.global_volume(time, self.global_volume);
            return true;
        }
        self.end_time = Some(self.next_row_time);
        false
    }

    /// The format's reading of one cell, or None for a cell that does
    /// nothing. Does not touch channel state.
    fn peek_step(&self, cell: &crate::module::Cell) -> Option<Step> {
        self.read_step(cell, 0, 0)
    }

    /// A cell, the way its format reads it.
    fn read_step(&self, cell: &crate::module::Cell, channel: usize, latched: u8) -> Option<Step> {
        match self.module.format {
            Format::Mod => self.mod_step(cell, channel, latched),
            Format::S3m => self.s3m_step(cell, channel, latched),
            Format::Xm => self.xm_step(cell),
        }
    }

    /// An XM cell, read the way the DAW's XM importer and song builder do.
    fn xm_step(&self, cell: &crate::module::Cell) -> Option<Step> {
        let note_off = cell.note == Note::Off;
        let midi = match cell.note {
            Note::On(m) => Some(m),
            _ => None,
        };
        let has_instrument = cell.instrument > 0;
        let has_effect = cell.effect != 0 || cell.param != 0;
        let volume = cell.volume;
        if midi.is_none() && !note_off && !has_instrument && !has_effect && volume == 0 {
            return None;
        }
        let tone_porta = matches!(cell.effect, 0x03 | 0x05) || volume & 0xf0 == 0xf0;
        let mut step = Step {
            note_off,
            ..Step::default()
        };
        // An instrument number switches the channel's instrument, note or
        // not, except under a key-off or a tone portamento.
        if has_instrument
            && !note_off
            && !tone_porta
            && self.xm_instrument(cell.instrument).is_some()
        {
            step.instrument = Some(cell.instrument);
        }
        if let Some(m) = midi {
            step.midi = Some(m as f64);
            let n = (m - 11) as f64 - 1.0;
            let period = if self.module.flags.linear_frequency {
                7680.0 - n * 64.0
            } else {
                1712.0 * 2f64.powf((48.0 - n) / 12.0)
            };
            step.frequency = Some(self.profile.pitch.frequency_from_period(period));
        }
        if (0x10..=0x50).contains(&volume) {
            step.velocity = Some(js_round((volume - 0x10) as f64 / 64.0 * 255.0) as u8);
            step.volume_column_volume = volume > 0x10;
        } else if let (Some(m), true) = (midi, has_instrument) {
            if let Some(sample) = self.xm_sample(cell.instrument, m as f64, true) {
                let v = self.module.samples[sample].volume as f64;
                step.velocity = Some(js_round(v / 64.0 * 255.0).min(255.0) as u8);
            }
        }
        if volume >= 0x60 {
            step.volume_command = VolumeCommand::decode(volume);
        }
        if has_effect {
            match self.profile.decode(cell.effect, cell.param) {
                Some(Decoded::Effect(e)) => step.effect = Some(e),
                Some(Decoded::Speed(s)) => step.speed = Some(s),
                Some(Decoded::Tempo(t)) => step.tempo = Some(t),
                None => {}
            }
        }
        let meaningful = step.midi.is_some()
            || step.note_off
            || step.velocity.is_some()
            || step.speed.is_some()
            || step.tempo.is_some()
            || step.effect.is_some()
            || step.volume_command.is_some();
        meaningful.then_some(step)
    }

    /// XM instrument `n` (1-based), if it has samples.
    fn xm_instrument(&self, n: u8) -> Option<&crate::module::Instrument> {
        self.module
            .instruments
            .get((n as usize).wrapping_sub(1))
            .filter(|i| !i.samples.is_empty())
    }

    /// The sample XM instrument `n` plays for MIDI note `midi`, through its
    /// keymap. `for_volume` reads the keymap as the DAW's importer does (a
    /// missing sample falls back to the first that has data); otherwise as
    /// its sampler does (a missing zone is the first).
    fn xm_sample(&self, n: u8, midi: f64, for_volume: bool) -> Option<usize> {
        let instrument = self.module.instruments.get((n as usize).wrapping_sub(1))?;
        let key = (js_round(midi) as i64 - 12).clamp(0, 95) as usize;
        let mapped = instrument.keymap[key] as usize;
        if for_volume {
            instrument.samples.get(mapped).copied().or_else(|| {
                let with_data = instrument
                    .samples
                    .iter()
                    .find(|&&s| !self.module.samples[s].data.is_empty());
                with_data.or(instrument.samples.first()).copied()
            })
        } else {
            let last = instrument.samples.len().checked_sub(1)?;
            instrument.samples.get(mapped.min(last)).copied()
        }
    }

    /// The sample a note on `instrument` plays.
    fn sample_for(&self, instrument: u8, midi: f64) -> Option<usize> {
        match self.module.format {
            Format::Xm => self.xm_sample(instrument, midi, false),
            _ => Some(instrument as usize - 1),
        }
    }

    /// An XM note's envelopes and vibrato, for the voice it starts.
    fn xm_voice(&self, instrument: u8, frequency: f64) -> Option<XmVoice> {
        if self.module.format != Format::Xm {
            return None;
        }
        let index = instrument as usize - 1;
        let instrument = self.module.instruments.get(index)?;
        let cents = self
            .profile
            .pitch
            .vibrato_depth_cents(frequency, instrument.vibrato.depth as f64);
        Some(XmVoice::new(index, instrument, cents))
    }

    /// An S3M cell, read the way the DAW's S3M importer and song builder do.
    fn s3m_step(&self, cell: &crate::module::Cell, channel: usize, latched: u8) -> Option<Step> {
        let note_off = cell.note == Note::Off;
        let midi = match cell.note {
            Note::On(m) => Some(m),
            _ => None,
        };
        let has_instrument = cell.instrument > 0;
        let has_volume = cell.volume != 0;
        let has_effect = cell.effect != 0;
        if midi.is_none() && !note_off && !has_instrument && !has_effect && !has_volume {
            return None;
        }
        let tone_porta = matches!(cell.effect, 0x07 | 0x0c);
        let mut step = Step {
            note_off,
            ..Step::default()
        };
        if let Some(m) = midi {
            step.midi = Some(m as f64);
            let index = (m as usize)
                .saturating_sub(24)
                .min(crate::pitch::S3M_PERIODS.len() - 1);
            step.frequency = Some(
                self.profile
                    .pitch
                    .frequency_from_period(crate::pitch::S3M_PERIODS[index] as f64),
            );
            step.pan = self
                .module
                .channel_pan
                .get(channel)
                .copied()
                .flatten()
                .map(f64::from);
            if !tone_porta {
                let n = if has_instrument {
                    cell.instrument
                } else {
                    latched
                };
                // The DAW only gives instruments that hold a sound a slot;
                // a note naming another keeps the channel's instrument.
                let playable = self
                    .module
                    .samples
                    .get((n as usize).wrapping_sub(1))
                    .is_some_and(|s| s.adlib.is_some() || !s.data.is_empty());
                // ST3 plays nothing for an empty instrument; the DAW plays on.
                if n > 0 && (playable || !self.profile.daw_quirks) {
                    step.instrument = Some(n);
                }
            }
        }
        if has_volume {
            step.velocity = Some(js_round((cell.volume - 0x10) as f64 / 64.0 * 255.0) as u8);
        } else if has_instrument && (midi.is_some() || !self.profile.daw_quirks) {
            // ST3 loads the default volume with any instrument number; the
            // DAW only with a note.
            if let Some(sample) = self.module.samples.get(cell.instrument as usize - 1) {
                // PCM and AdLib instruments have a default volume; empty
                // slots (no c2spd) do not.
                let known = sample.adlib.is_some() || sample.c2spd != 0;
                if known {
                    step.velocity =
                        Some(js_round(sample.volume as f64 / 64.0 * 255.0).min(255.0) as u8);
                }
            }
        }
        if has_effect {
            step.raw_effect = Some((cell.effect, cell.param));
            match self.profile.decode(cell.effect, cell.param) {
                Some(Decoded::Effect(e)) => step.effect = Some(e),
                Some(Decoded::Speed(s)) => step.speed = Some(s),
                Some(Decoded::Tempo(t)) => step.tempo = Some(t),
                None => {}
            }
        }
        let meaningful = step.midi.is_some()
            || step.note_off
            || step.velocity.is_some()
            || step.pan.is_some()
            || step.speed.is_some()
            || step.tempo.is_some()
            || step.effect.is_some()
            || step.raw_effect.is_some();
        meaningful.then_some(step)
    }

    /// A MOD cell, read the way the DAW's MOD importer and song builder do.
    fn mod_step(&self, cell: &crate::module::Cell, channel: usize, latched: u8) -> Option<Step> {
        let has_note = cell.period > 0;
        let has_sample = cell.instrument > 0;
        let has_effect = cell.effect != 0 || cell.param != 0;
        if !has_note && !has_sample && !has_effect {
            return None;
        }
        let tone_porta = matches!(cell.effect, 3 | 5);
        let mut step = Step::default();
        if has_note && !tone_porta {
            let n = if has_sample { cell.instrument } else { latched };
            if n > 0 {
                step.instrument = Some(n);
            }
        }
        if has_note {
            if let Note::On(midi) = cell.note {
                step.midi = Some(midi as f64);
            }
            step.frequency = period_to_frequency(cell.period);
        }
        // Cxx is a volume, not an effect; a sample number reloads its
        // sample's default volume (not in Soundtracker modules).
        let mut decoded = None;
        if cell.effect == 0x0c {
            step.velocity =
                Some(js_round((cell.param as f64 / 64.0).clamp(0.0, 1.0) * 255.0) as u8);
        } else if has_effect {
            decoded = self.profile.decode(cell.effect, cell.param);
        }
        // (The DAW leaves it out for Soundtracker modules.)
        if step.velocity.is_none()
            && has_sample
            && (self.module.mod_flavor != Some(ModFlavor::Soundtracker) || !self.profile.daw_quirks)
        {
            let volume = self
                .module
                .samples
                .get(cell.instrument as usize - 1)
                .map_or(64, |s| s.volume);
            step.velocity = Some(js_round(volume as f64 / 64.0 * 255.0).min(255.0) as u8);
        }
        if has_note {
            step.pan = self
                .module
                .channel_pan
                .get(channel)
                .copied()
                .flatten()
                .map(f64::from);
        }
        match decoded {
            Some(Decoded::Effect(e)) => step.effect = Some(e),
            Some(Decoded::Speed(s)) => step.speed = Some(s),
            Some(Decoded::Tempo(t)) if self.module.flags.vblank_timing => step.speed = Some(t),
            Some(Decoded::Tempo(t)) => step.tempo = Some(t),
            None => {}
        }
        let meaningful = step.midi.is_some()
            || step.velocity.is_some()
            || step.pan.is_some()
            || step.speed.is_some()
            || step.tempo.is_some()
            || step.effect.is_some();
        meaningful.then_some(step)
    }

    /// The channel's step for this row, with its latches updated.
    fn take_step(&mut self, pattern: usize, row: usize, ch: usize) -> Option<Step> {
        let cell = *self.module.patterns[pattern].cell(row, ch);
        let latched = self.channels[ch].latched_sample;
        let mut step = self.read_step(&cell, ch, latched);
        if cell.instrument > 0 {
            self.channels[ch].latched_sample = cell.instrument;
        }
        // FT2: an instrument number with no note resets the volume and pan to
        // the sample's defaults (the DAW leaves them).
        if self.module.format == Format::Xm
            && !self.profile.daw_quirks
            && cell.instrument > 0
            && cell.note == Note::None
            && !(0x10..=0x50).contains(&cell.volume)
        {
            let last = self.channels[ch].last_note.map(|n| n.0);
            if let Some(s) = last.and_then(|m| self.xm_sample(cell.instrument, m, false)) {
                let sample = &self.module.samples[s];
                let step = step.get_or_insert_with(Step::default);
                if self.xm_instrument(cell.instrument).is_some() {
                    step.instrument = Some(cell.instrument);
                }
                step.velocity = Some(js_round(sample.volume.min(64) as f64 / 64.0 * 255.0) as u8);
                step.reset_pan = Some(match sample.pan {
                    Some(p) if p != 128 => p as f64 / 255.0,
                    _ => 0.5,
                });
            }
        }
        if let Some(step) = &mut step {
            // A tone portamento with no note slides to the last note stated.
            let channel = &mut self.channels[ch];
            if !step.note_off {
                if let Some(midi) = step.midi {
                    channel.last_note = Some((midi, step.frequency));
                } else if matches!(
                    step.effect.map(|e| e.kind),
                    Some(EffectType::TonePorta | EffectType::TonePortaVol)
                ) {
                    if let Some((midi, frequency)) = channel.last_note {
                        step.midi = Some(midi);
                        if frequency.is_some() {
                            step.frequency = frequency;
                        }
                    }
                }
            }
        }
        step
    }

    /// `scheduleRow`: tick 0 of `row`, at `time`.
    fn schedule_row(&mut self, row: usize, time: f64, observer: &mut dyn Observer) {
        let repeat = self.delay_repeating;
        let pattern = self.module.orders[self.order];
        self.position = Position {
            order: self.order,
            pattern,
            row,
        };
        observer.row(time, self.order, row);

        let channel_count = self.channels.len();
        // Steps are rebuilt from the cells (and on a repeat, read again
        // without touching the latches).
        let mut steps = [None::<Step>; crate::protracker::MAX_CHANNELS];
        for (ch, step) in steps.iter_mut().enumerate().take(channel_count) {
            *step = if repeat {
                let cell = *self.module.patterns[pattern].cell(row, ch);
                // A repeat runs the row's effects again; its notes stay.
                self.read_step(&cell, ch, self.channels[ch].latched_sample)
            } else {
                self.take_step(pattern, row, ch)
            };
        }

        // First pass: speed, tempo and flow commands, and global volume.
        if !repeat {
            let mut row_has_delay = false;
            for c in &mut self.channels {
                c.global_slide = None;
            }
            for (ch, step) in steps.iter().enumerate().take(channel_count) {
                let Some(step) = step else {
                    continue;
                };
                if let Some(speed) = step.speed {
                    if speed == 0 && self.profile.f00_stops_song {
                        self.song_stop_pending = true;
                    } else {
                        self.set_speed(speed as u32);
                    }
                }
                if let Some(tempo) = step.tempo {
                    self.set_bpm(tempo as f64);
                }
                let Some(effect) = step.effect else {
                    continue;
                };
                match (effect.kind, effect.sub) {
                    (EffectType::PosJump, _) => {
                        self.pending_position =
                            Some(PositionCommand::Jump(effect.param() as usize));
                    }
                    (EffectType::PatBreak, _) => {
                        if !matches!(self.pending_position, Some(PositionCommand::Jump(_))) {
                            let target = effect.x as usize * 10 + effect.y as usize;
                            self.pending_position =
                                Some(PositionCommand::Break(target + row_has_delay as usize));
                        }
                    }
                    (EffectType::ExtEffect, Some(ExtSub::PatLoop)) => {
                        let count = effect.y;
                        if count == 0 {
                            self.loop_start = row;
                        } else {
                            if self.loop_count == 0 {
                                self.loop_count = count;
                            } else {
                                self.loop_count -= 1;
                            }
                            self.loop_pending = self.loop_count > 0;
                            if !self.loop_pending && self.profile.pattern_loop_end_resets_start {
                                self.loop_start = row + 1;
                            }
                        }
                    }
                    (EffectType::PatDelay, _) => {
                        if effect.y > 0 && self.delay_count == 0 {
                            self.delay_count = effect.y;
                            row_has_delay = true;
                        }
                    }
                    (EffectType::SetGlobalVol, _) => {
                        self.global_volume = (effect.param() as f64).clamp(0.0, 64.0) / 64.0;
                        self.master_target = self.global_volume as f32;
                        observer.global_volume(time, self.global_volume);
                    }
                    (EffectType::GlobalVolSlide, _) if !self.profile.daw_quirks => {
                        // FT2 slides every tick after the first (run_tick),
                        // remembering its parameter.
                        let channel = &mut self.channels[ch];
                        let param = if effect.param() == 0 {
                            channel.last_global_slide
                        } else {
                            effect.param()
                        };
                        channel.last_global_slide = param;
                        channel.global_slide = Some(param);
                    }
                    (EffectType::GlobalVolSlide, _) => {
                        // The DAW slides once a row; FT2 every tick (run_tick).
                        let (up, down) = (effect.x as f64, effect.y as f64);
                        if up > 0.0 && down == 0.0 {
                            self.global_volume = (self.global_volume + up / 64.0).min(1.0);
                        } else if down > 0.0 && up == 0.0 {
                            self.global_volume = (self.global_volume - down / 64.0).max(0.0);
                        }
                        self.master_target = self.global_volume as f32;
                        observer.global_volume(time, self.global_volume);
                    }
                    (EffectType::ExtEffect, Some(ExtSub::FilterToggle))
                        if self.profile.filter_toggle_command =>
                    {
                        let on = effect.y & 1 == 0;
                        if let Some(filter) = &mut self.filter {
                            filter.set_led(on);
                        }
                        observer.filter(time, on);
                    }
                    _ => {}
                }
            }
        }

        self.ticks_per_row = self.speed;
        self.sec_per_tick = self.ms_per_row() / self.ticks_per_row as f64 / 1000.0;
        self.row = row;
        self.row_time = time;
        self.next_tick = 1;
        let tpr = self.ticks_per_row;

        // Second pass: each channel's cell.
        for ch in 0..channel_count {
            let named = self.module.patterns[pattern].cell(row, ch).instrument > 0;
            let channel = &mut self.channels[ch];
            channel.tick_effect = None;
            channel.tick_volume = None;
            channel.continues = false;
            if !repeat {
                channel.instrument_row = named;
            }
            let Some(step) = steps[ch] else {
                continue;
            };
            let effect = self.shared_info_effect(&step, ch);
            let channel = &mut self.channels[ch];
            // ST3: any command but D, H, K, R and U restarts the next
            // vibrato's wave.
            if self.profile.st3_vibrato {
                if let Some((command, _)) = step.raw_effect {
                    if ![0x04, 0x08, 0x0b, 0x12, 0x15].contains(&command) {
                        channel.fx.vibrato_restart = true;
                    }
                }
            }
            if let Some(i) = step.instrument {
                channel.fx.instrument = Some(i);
            }
            let Some(instrument) = channel.fx.instrument else {
                continue;
            };
            let tick_effect = effect.filter(|e| e.kind.is_tick_based());
            if repeat {
                if self.profile.pattern_delay_repeats_tick_zero {
                    let mut out = Commands::default();
                    self.channels[ch].fx.tick0(
                        effect.as_ref(),
                        None,
                        None,
                        None,
                        None,
                        None,
                        false,
                        false,
                        &mut out,
                    );
                    self.dispatch(ch, instrument, time, &out, observer);
                } else {
                    if let Some(e) = &tick_effect {
                        let mut out = Commands::default();
                        self.channels[ch].fx.tick_n(Some(e), 0, tpr, &mut out);
                        self.dispatch(ch, instrument, time, &out, observer);
                    }
                    if let Some(v) = step.volume_command.filter(|v| v.is_tick_based()) {
                        let mut out = Commands::default();
                        self.channels[ch].fx.volume_column_tick_n(Some(v), &mut out);
                        self.dispatch(ch, instrument, time, &out, observer);
                    }
                }
            } else {
                if step.note_off {
                    let mut out = Commands::default();
                    out.push(Command::NoteOff);
                    self.channels[ch].fx.has_active_voice = false;
                    self.dispatch(ch, instrument, time, &out, observer);
                    if !self.profile.note_off_row_runs_cell
                        || (effect.is_none()
                            && step.velocity.is_none()
                            && step.volume_command.is_none())
                    {
                        continue;
                    }
                }
                let new_note = if step.note_off { None } else { step.midi };
                let adlib_instrument = self.is_adlib(instrument);
                let volume_tone_porta =
                    matches!(step.volume_command, Some(VolumeCommand::TonePorta(_)));
                let channel = &mut self.channels[ch];
                if new_note.is_some()
                    && !matches!(
                        effect.map(|e| e.kind),
                        Some(EffectType::TonePorta | EffectType::TonePortaVol)
                    )
                    && !volume_tone_porta
                {
                    // ST3 keeps the vibrato's phase across an AdLib note.
                    channel.fx.reset_for_note(adlib_instrument);
                }
                let (mut out, mut out2) = (Commands::default(), Commands::default());
                channel.fx.tick0(
                    effect.as_ref(),
                    new_note,
                    step.velocity,
                    step.frequency,
                    Some(tpr),
                    step.pan,
                    step.volume_column_volume,
                    volume_tone_porta,
                    &mut out,
                );
                channel
                    .fx
                    .volume_column_tick0(step.volume_command, &mut out2);
                self.dispatch(ch, instrument, time, &out, observer);
                self.dispatch(ch, instrument, time, &out2, observer);
                let stated = out
                    .as_slice()
                    .iter()
                    .chain(out2.as_slice())
                    .any(|c| matches!(c, Command::Volume { .. }));
                let deferred = effect.map(|e| e.kind) == Some(EffectType::NoteDelay)
                    && self.profile.note_delay_defers_cell;
                if let (Some(v), false, false) = (step.velocity, stated, deferred) {
                    let mut out = Commands::default();
                    out.push(Command::Volume {
                        volume: (v as f64 / 255.0).clamp(0.0, 1.0),
                        ramp: crate::effects::Ramp::Step,
                    });
                    self.dispatch(ch, instrument, time, &out, observer);
                }
            }
            let channel = &mut self.channels[ch];
            channel.tick_effect = tick_effect;
            channel.tick_volume = step.volume_command.filter(|v| v.is_tick_based());
            if let (Some(pan), false) = (step.reset_pan, repeat) {
                let mut out = Commands::default();
                out.push(Command::Pan {
                    pan: pan * 2.0 - 1.0,
                });
                self.channels[ch].fx.current_pan = pan * 2.0 - 1.0;
                self.dispatch(ch, instrument, time, &out, observer);
            }
        }

        // Channels with no cell this row.
        for ch in 0..channel_count {
            if steps[ch].is_some() {
                continue;
            }
            let channel = &mut self.channels[ch];
            let Some(instrument) = channel.fx.instrument else {
                continue;
            };
            if !channel.fx.has_active_voice {
                continue;
            }
            if self.profile.pitch_resets_after_effectless_row
                && (channel.fx.vibrato_applied || channel.fx.arpeggio_applied)
            {
                channel.fx.vibrato_applied = false;
                channel.fx.arpeggio_applied = false;
                channel.fx.vibrato_held_wave = 0.0;
                let mut out = Commands::default();
                out.push(Command::Pitch {
                    frequency: channel.fx.current_frequency,
                    source: None,
                });
                self.dispatch(ch, instrument, time, &out, observer);
            }
            let channel = &mut self.channels[ch];
            channel.continues = self.profile.tone_porta_continues_through_empty_rows
                && channel.fx.tone_porta_active
                && channel.fx.tone_porta_speed > 0.0;
        }
    }

    /// The row's effect with ST3's shared parameter memory: a zero
    /// parameter on D, E, F, I, J, K, L, Q, R or S plays the channel's last
    /// non-zero parameter of any command (`J00` after `D04` is `J04`).
    fn shared_info_effect(&mut self, step: &Step, ch: usize) -> Option<Effect> {
        let commands = self.profile.shared_effect_info_commands;
        let Some((command, param)) = step.raw_effect.filter(|_| !commands.is_empty()) else {
            return step.effect;
        };
        let fx = &mut self.channels[ch].fx;
        if param > 0 {
            fx.last_effect_info = param;
            return step.effect;
        }
        if fx.last_effect_info == 0 || !commands.contains(&command) {
            return step.effect;
        }
        match self.profile.decode(command, fx.last_effect_info) {
            Some(Decoded::Effect(e)) => Some(e),
            _ => step.effect,
        }
    }

    /// Tick `tick` (1 and on) of the row playing.
    fn run_tick(&mut self, tick: u32, observer: &mut dyn Observer) {
        let time = self.row_time + tick as f64 * self.sec_per_tick;
        let tpr = self.ticks_per_row;
        // FT2's Hxy: the up nibble first, a 64th per tick.
        for ch in 0..self.channels.len() {
            if let Some(param) = self.channels[ch].global_slide {
                let (up, down) = ((param >> 4) as f64, (param & 0x0f) as f64);
                self.global_volume = if up > 0.0 {
                    (self.global_volume + up / 64.0).min(1.0)
                } else {
                    (self.global_volume - down / 64.0).max(0.0)
                };
                self.master_target = self.global_volume as f32;
                observer.global_volume(time, self.global_volume);
            }
        }
        for ch in 0..self.channels.len() {
            let channel = &mut self.channels[ch];
            let Some(instrument) = channel.fx.instrument else {
                continue;
            };
            if let Some(effect) = channel.tick_effect {
                let mut out = Commands::default();
                channel.fx.tick_n(Some(&effect), tick, tpr, &mut out);
                self.dispatch(ch, instrument, time, &out, observer);
            }
            let channel = &mut self.channels[ch];
            if let Some(v) = channel.tick_volume {
                let mut out = Commands::default();
                channel.fx.volume_column_tick_n(Some(v), &mut out);
                self.dispatch(ch, instrument, time, &out, observer);
            }
            let channel = &mut self.channels[ch];
            if channel.continues {
                let mut out = Commands::default();
                channel.fx.tick_n(None, tick, tpr, &mut out);
                self.dispatch(ch, instrument, time, &out, observer);
            }
        }
    }

    /// Plays a channel's commands on its voice.
    fn dispatch(
        &mut self,
        ch: usize,
        instrument: u8,
        time: f64,
        commands: &Commands,
        observer: &mut dyn Observer,
    ) {
        let rate = self.rate;
        for command in commands.as_slice() {
            observer.command(time, ch, instrument, command);
            #[cfg(feature = "adlib")]
            if let Some(adlib) = &mut self.adlib {
                if adlib.handles(instrument) {
                    // A muted channel plays nothing on the chip.
                    if self.channels[ch].muted {
                        continue;
                    }
                    match *command {
                        Command::NoteOn {
                            velocity,
                            frequency,
                            ..
                        }
                        | Command::Retrigger {
                            velocity,
                            frequency,
                            ..
                        } => {
                            // One note to a channel, AdLib or PCM.
                            Self::cut(&mut self.channels[ch], rate);
                            adlib.note_on(instrument, velocity, ch, frequency);
                        }
                        Command::Pitch { frequency, source } => {
                            adlib.set_pitch(ch, frequency, source)
                        }
                        Command::Volume { volume, .. } => adlib.set_volume(ch, volume),
                        Command::NoteOff => adlib.note_off(ch),
                        Command::Pan { .. } | Command::EnvelopePosition { .. } => {}
                    }
                    continue;
                }
                if matches!(command, Command::NoteOn { .. } | Command::Retrigger { .. }) {
                    adlib.note_off(ch);
                }
            }
            let channel = &mut self.channels[ch];
            match *command {
                Command::NoteOn {
                    midi,
                    velocity,
                    frequency,
                    pan,
                    offset,
                } => {
                    let sample = self.sample_for(instrument, midi);
                    let xm = self.xm_voice(instrument, frequency);
                    let channel = &mut self.channels[ch];
                    // XM: a note on a row naming an instrument takes its sample's
                    // pan (the DAW resets it on every note).
                    let named = channel.instrument_row || self.profile.daw_quirks;
                    let pan = match (pan, xm, sample) {
                        (Some(p), _, _) => p,
                        (None, Some(_), Some(s)) if named => match self.module.samples[s].pan {
                            Some(p) if p != 128 => p as f64 / 255.0,
                            _ => 0.5,
                        },
                        (None, _, _) => channel.pan,
                    };
                    channel.pan = pan;
                    Self::cut(channel, rate);
                    if let Some(Some(prepared)) = sample.and_then(|s| self.samples.get(s)) {
                        channel.voice.start(
                            sample.unwrap_or(0),
                            prepared,
                            offset,
                            frequency,
                            velocity as f64 / 127.0,
                            pan,
                            rate,
                        );
                        if let Some(xm) = xm {
                            channel.voice.start_xm(xm);
                        }
                    }
                }
                Command::Retrigger {
                    midi,
                    velocity,
                    frequency,
                } => {
                    let sample = self.sample_for(instrument, midi);
                    let xm = self.xm_voice(instrument, frequency);
                    let quirks = self.profile.daw_quirks;
                    let channel = &mut self.channels[ch];
                    let pan = channel.pan;
                    // The channel's volume, not the note-on's 1/127 steps.
                    let volume = if quirks {
                        velocity as f64 / 127.0
                    } else {
                        channel.fx.current_volume
                    };
                    Self::cut(channel, rate);
                    if let Some(Some(prepared)) = sample.and_then(|s| self.samples.get(s)) {
                        channel.voice.start(
                            sample.unwrap_or(0),
                            prepared,
                            None,
                            frequency,
                            volume,
                            pan,
                            rate,
                        );
                        if let Some(xm) = xm {
                            channel.voice.start_xm(xm);
                        }
                    }
                }
                Command::NoteOff => {
                    // XM with a volume envelope: the envelope leaves its
                    // sustain and the fadeout runs. Otherwise a quick fade.
                    let enveloped = channel.voice.xm.is_some_and(|x| {
                        self.module
                            .instruments
                            .get(x.instrument)
                            .is_some_and(|i| i.volume_envelope.is_some())
                    });
                    match &mut channel.voice.xm {
                        Some(xm) if enveloped => xm.release(),
                        _ => channel.voice.fade_out(RELEASE_SECONDS, rate),
                    }
                }
                Command::Pitch { frequency, .. } => {
                    if channel.voice.active {
                        if let Some(Some(prepared)) = self.samples.get(channel.voice.sample) {
                            channel.voice.set_frequency(frequency, prepared, rate);
                        }
                    }
                }
                Command::Volume { volume, .. } => channel.voice.set_volume(volume, rate),
                Command::Pan { pan } => {
                    channel.pan = (pan + 1.0) / 2.0;
                    channel.voice.set_pan(channel.pan, rate);
                }
                Command::EnvelopePosition { tick } => {
                    if let Some(xm) = &mut channel.voice.xm {
                        xm.set_position(tick.min(u16::MAX as u32) as u16);
                    }
                }
            }
        }
    }

    /// Whether `instrument` plays on the AdLib chip.
    fn is_adlib(&self, instrument: u8) -> bool {
        #[cfg(feature = "adlib")]
        if let Some(adlib) = &self.adlib {
            return adlib.handles(instrument);
        }
        let _ = instrument;
        false
    }

    /// Moves the channel's voice to its tail, fading out.
    fn cut(channel: &mut Channel, rate: f64) {
        if channel.voice.active {
            channel.tail = channel.voice;
            channel.tail.fade_out(CUT_SECONDS, rate);
            channel.voice.active = false;
        }
    }

    fn mix(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len();
        if n == 0 {
            return;
        }
        for channel in &mut self.channels {
            for voice in [&mut channel.voice, &mut channel.tail] {
                if !voice.active {
                    continue;
                }
                let Some(Some(prepared)) = self.samples.get(voice.sample) else {
                    voice.active = false;
                    continue;
                };
                if channel.muted {
                    // Keep its clock running, silently.
                    let mut scratch_l = [0.0f32; 64];
                    let mut scratch_r = [0.0f32; 64];
                    let mut at = 0;
                    while at < n {
                        let k = (n - at).min(64);
                        voice.render(prepared, &mut scratch_l[..k], &mut scratch_r[..k]);
                        at += k;
                    }
                } else {
                    voice.render(prepared, left, right);
                }
            }
        }
        // Global volume, gliding like a voice's gain, and the output trim.
        let glide = 1.0 / (0.0013 * self.rate as f32).max(1.0);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if self.master != self.master_target {
                let d = self.master_target - self.master;
                self.master = if d.abs() <= glide {
                    self.master_target
                } else {
                    self.master + glide * d.signum()
                };
            }
            let g = self.master * self.output_gain;
            *l *= g;
            *r *= g;
        }
        // The AdLib chip, which (as in the DAW) the global volume leaves alone.
        #[cfg(feature = "adlib")]
        if let Some(adlib) = &mut self.adlib {
            adlib.render(left, right, self.output_gain);
        }
    }
}

/// Source frames per second, per musical Hz, for a sample of `format`: the
/// DAW's sampler arithmetic (a buffer declared at 44.1 kHz, a root note,
/// a detune), in its order.
fn sample_rate_per_hz(format: Format, sample: &crate::module::Sample) -> f64 {
    let (root_note, detune_cents) = match format {
        // The relative note and finetune transpose the sample, not the note.
        Format::Xm => (
            69.0 + 12.0 * (MOD_SAMPLE_RATE / XM_TO_SYNTH_SCALE / 440.0).log2()
                - sample.relative_note as f64,
            sample.finetune as f64 / 128.0 * 100.0,
        ),
        Format::Mod => (
            69.0 + 12.0 * (MOD_SAMPLE_RATE / PAULA_TO_SYNTH_SCALE / 440.0).log2(),
            sample.finetune as f64 / 8.0 * 100.0,
        ),
        Format::S3m => {
            // c2spd tunes the sample: middle C plays at c2spd Hz.
            let c2spd = if sample.c2spd > 0 {
                sample.c2spd as f64
            } else {
                S3M_C2FREQ
            };
            let root = 69.0 + 12.0 * (MOD_SAMPLE_RATE / S3M_TO_SYNTH_SCALE / 440.0).log2();
            (root - 12.0 * (c2spd / S3M_C2FREQ).log2(), 0.0)
        }
    };
    let root_hz = 440.0 * 2f64.powf((root_note - 69.0) / 12.0);
    MOD_SAMPLE_RATE / root_hz * 2f64.powf(detune_cents / 1200.0)
}

/// FT2's rates over musical Hz.
const XM_TO_SYNTH_SCALE: f64 = 32.0;
/// The rate an untuned S3M sample plays middle C at.
const S3M_C2FREQ: f64 = 8363.0;
/// ST3's rates over musical Hz.
const S3M_TO_SYNTH_SCALE: f64 = 16.0;
