//! `PsidPlayer`: a `.sid` file played as the C64 would play it, on the crate's
//! SID chip.
//!
//! **Design: the CPU runs live, in `render`.** The tune's 6510 code is emulated
//! in step with the audio, as the DAW's worklet does it, rather than captured
//! offline into a register stream first. A 6510 is cheap (about a million cycles
//! a second of audio, a few percent of a core), a tune has no length (nearly all
//! loop for ever, so there is nothing to capture to the end of), a subsong
//! change or a seek only has to run the code again, and nothing needs a limit.
//! The price is that `render` runs a tune's code, so a tune's init routine (up
//! to 60 million cycles) runs in [`PsidPlayer::new`]: build the player off the
//! audio thread. Everything after that, `render` included, allocates nothing.
//!
//! How a block is played. The audio is cut into stretches of a few hundred
//! cycles (a few dozen samples). For each, the runner ([`PsidRunner`]) runs the
//! tune up to the stretch's last cycle, the SID writes it made come out stamped
//! with the cycle they were made on, they are scheduled on the chip
//! ([`Chip::write_after`]) at those cycles, and the chip renders the stretch. So
//! every write lands on its own cycle, as it does on a real C64 (the envelope's
//! rate counter keeps running between two writes: the ADSR delay bug), and the
//! queues in between never hold more than a stretch's worth.
//!
//! Chips: the first SID is `$D400`; a v3/v4 file names a second and a third, and
//! each gets a chip of its own (its model from the header, else the first's).
//! They are summed into one mono signal, divided by the number of chips, so
//! three chips never clip where one would not.

use super::c64::Kernal;
use super::file::{PsidError, PsidFile, PsidSidModel};
use super::runner::{EndReason, PsidRunner};
use crate::chip::{Chip, ALL_VOICES};
use crate::revision::{DieRevision, SID6581_REVISION};
use crate::{SidError, SidModel};

/// The cycles a stretch of audio spans, at most (a stretch is whole samples).
const STRETCH_CYCLES: f64 = 600.0;
/// The samples a stretch spans, at most.
const MAX_STRETCH_SAMPLES: usize = 64;
/// Scheduled writes a chip has room for: a stretch of back-to-back stores.
const CHIP_WRITE_ROOM: usize = 512;
/// The longest seek, seconds: the tune is run up to there.
pub const SEEK_MAX_SECONDS: f64 = 1_200.0;
/// A seek renders this many seconds before its target for real; before that,
/// the tune's writes are applied to the chip at once.
const SEEK_RENDERED_SECONDS: f64 = 1.0;
/// Cycles a seek runs the tune between two applications of its writes.
const SEEK_STEP_CYCLES: u64 = 20_000;

/// How a tune plays.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PsidOptions {
    /// The subsong (0-based) to play; the header's start song when `None`.
    pub subsong: Option<usize>,
    /// The chip to play on; the file's own when `None`, and a 6581 when the
    /// file says nothing (or "either"), as HVSC's players do.
    pub model: Option<SidModel>,
    /// Which 6581 a 6581 chip is (its filter, DC and volume DAC); the default
    /// revision (`SID6581_REVISION`) when `None`. An 8580 ignores it.
    pub revision: Option<DieRevision>,
    /// The stand-in KERNAL (see [`Kernal`]): the DAW's plus two entries of the real
    /// handler by default.
    pub kernal: Kernal,
}

/// Why a seek did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PsidSeekError {
    /// Past [`SEEK_MAX_SECONDS`], or not a time.
    OutOfRange,
}

impl std::fmt::Display for PsidSeekError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PsidSeekError::OutOfRange => write!(f, "a seek is to 0..={SEEK_MAX_SECONDS} seconds"),
        }
    }
}

impl std::error::Error for PsidSeekError {}

fn model_of(tag: PsidSidModel, fallback: SidModel) -> SidModel {
    match tag {
        PsidSidModel::Sid6581 => SidModel::Sid6581,
        PsidSidModel::Sid8580 => SidModel::Sid8580,
        PsidSidModel::Unknown | PsidSidModel::Any => fallback,
    }
}

/// The model the file names for chip `i`: the first's tag, else a 6581; a later chip's own
/// tag, else the first's.
fn own_model(file: &PsidFile, i: usize) -> SidModel {
    let first = model_of(file.sid_model, SidModel::Sid6581);
    match i {
        0 => first,
        i => file
            .extra_models
            .get(i - 1)
            .map_or(first, |tag| model_of(*tag, first)),
    }
}

pub struct PsidPlayer {
    file: PsidFile,
    runner: PsidRunner,
    chips: Vec<Chip>,
    sample_rate: f64,
    subsong: usize,
    /// The model the user chose (kept across a restart), if any.
    chosen_model: Option<SidModel>,
    /// The 6581 revision the user chose (kept across a restart), if any.
    chosen_revision: Option<DieRevision>,
    kernal: Kernal,
    /// Time in cycles from the start of playback: where the runner has been run to.
    cycle: u64,
    /// Samples rendered since the start of playback.
    frames: u64,
    stretch_samples: usize,
    gain: f32,
    mute: u32,
    solo: u32,
}

impl PsidPlayer {
    /// A player for the `.sid` in `bytes`, at `sample_rate`. Runs the tune's init
    /// routine: build it off the audio thread.
    pub fn new(
        bytes: &[u8],
        sample_rate: f64,
        options: PsidOptions,
    ) -> Result<PsidPlayer, PsidError> {
        PsidPlayer::from_file(PsidFile::parse(bytes)?, sample_rate, options)
    }

    /// [`PsidPlayer::new`] for a file that is already parsed.
    pub fn from_file(
        file: PsidFile,
        sample_rate: f64,
        options: PsidOptions,
    ) -> Result<PsidPlayer, PsidError> {
        let subsong = options
            .subsong
            .unwrap_or_else(|| file.start_subsong())
            .min(file.songs as usize - 1);
        let (runner, chips) = Self::build(
            &file,
            subsong,
            sample_rate,
            options.model,
            options.revision,
            options.kernal,
        )?;
        let cps = runner.clock_hz() as f64 / sample_rate;
        let stretch_samples = ((STRETCH_CYCLES / cps) as usize).clamp(1, MAX_STRETCH_SAMPLES);
        let mut player = PsidPlayer {
            file,
            runner,
            chips,
            sample_rate,
            subsong,
            chosen_model: options.model,
            chosen_revision: options.revision,
            kernal: options.kernal,
            cycle: 0,
            frames: 0,
            stretch_samples,
            gain: 1.0,
            mute: 0,
            solo: 0,
        };
        player.apply_init_writes();
        Ok(player)
    }

    /// The runner and chips of `subsong`: the tune initialised, the chips powered on.
    fn build(
        file: &PsidFile,
        subsong: usize,
        sample_rate: f64,
        model: Option<SidModel>,
        revision: Option<DieRevision>,
        kernal: Kernal,
    ) -> Result<(PsidRunner, Vec<Chip>), PsidError> {
        let runner = PsidRunner::with_kernal(file, subsong, kernal)?;
        let mut chips = Vec::with_capacity(runner.sid_count());
        for i in 0..runner.sid_count() {
            // The user's choice is for every chip; the file's tag for each otherwise.
            let m = model.unwrap_or_else(|| own_model(file, i));
            let mut chip = Chip::with_sample_rate(m, sample_rate).map_err(|e| match e {
                SidError::UnsupportedSampleRate(r) => PsidError::SampleRate(r),
                SidError::ModelNotImplemented { .. } => PsidError::SampleRate(sample_rate),
            })?;
            if let Some(revision) = revision {
                chip.set_profile(revision.profile());
            }
            chip.set_clock_hz(runner.clock_hz() as f64);
            chip.reserve_writes(CHIP_WRITE_ROOM);
            chips.push(chip);
        }
        Ok((runner, chips))
    }

    /// What init wrote before playback began is the chips' state at cycle 0.
    fn apply_init_writes(&mut self) {
        let chips = &mut self.chips;
        let n = chips.len();
        self.runner
            .drain(0, n, |w| chips[w.chip as usize].write(w.reg, w.value));
        self.apply_mask();
    }

    fn apply_mask(&mut self) {
        let heard = if self.solo & ALL_VOICES as u32 != 0 {
            self.solo
        } else {
            ALL_VOICES as u32
        };
        let mask = (heard & !self.mute) as u8;
        for chip in &mut self.chips {
            chip.set_voice_mask(mask);
        }
    }

    // --- Reading the tune ---------------------------------------------------

    /// The parsed file.
    pub fn file(&self) -> &PsidFile {
        &self.file
    }

    /// The subsong playing (0-based).
    pub fn subsong(&self) -> usize {
        self.subsong
    }

    /// The subsongs the file has.
    pub fn subsongs(&self) -> usize {
        self.file.songs as usize
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// The runner, for inspection.
    pub fn runner(&self) -> &PsidRunner {
        &self.runner
    }

    /// Why the tune stopped (its code jammed the CPU, hit a BRK or ran away), or
    /// `None` while it plays. The chip goes on sounding what it holds.
    pub fn ended(&self) -> Option<EndReason> {
        self.runner.ended()
    }

    /// `ended().is_some()`: a tune has no length, so this is the only end it has.
    pub fn song_end_reached(&self) -> bool {
        self.runner.ended().is_some()
    }

    /// Samples rendered since the tune started (or was last seeked to).
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Seconds of the tune played.
    pub fn position_seconds(&self) -> f64 {
        self.cycle as f64 / self.runner.clock_hz() as f64
    }

    /// How many SID chips play (1 to 3).
    pub fn sid_count(&self) -> usize {
        self.chips.len()
    }

    /// Writes the tune made to a SID chip this player does not have (none, as
    /// every chip the file names has one).
    pub fn extra_sid_writes(&self) -> u64 {
        self.runner.extra_sid_writes()
    }

    /// Chip `i` (0 is the one at `$D400`).
    pub fn chip(&self, i: usize) -> &Chip {
        &self.chips[i]
    }

    /// Chip `i` to change its settings (the 6581 revision, say). Do not render
    /// it or clock it yourself: the player keeps the chips in step with the tune.
    pub fn chip_mut(&mut self, i: usize) -> &mut Chip {
        &mut self.chips[i]
    }

    // --- Controls (cheap: fine between audio blocks) -------------------------

    /// A gain on the output (1.0 is the chip's own level).
    pub fn set_gain(&mut self, gain: f32) {
        self.gain = if gain.is_finite() { gain.max(0.0) } else { 1.0 };
    }

    /// Bit masks, bit `i` = voice `i` (of every chip): muted voices, and (when
    /// non-zero) the only voices heard. The rule of `SidChipPlayer::set_mute_solo`.
    pub fn set_mute_solo(&mut self, mute: u32, solo: u32) {
        self.mute = mute;
        self.solo = solo;
        self.apply_mask();
    }

    /// Play on the 8580 or the 6581 from the next sample on (every chip); the
    /// tune's notes keep sounding.
    pub fn set_model(&mut self, model: SidModel) {
        self.chosen_model = Some(model);
        for chip in &mut self.chips {
            let _ = chip.set_model(model);
        }
    }

    /// Play every chip as `model` (`None`: the model the file names for it, a 6581
    /// when it names none) and every 6581 as `revision` (`None`: the default,
    /// `SID6581_REVISION`), from the next sample on: the tune's notes keep sounding.
    /// Kept across a restart. Allocates nothing: fine between audio blocks.
    pub fn set_chip(&mut self, model: Option<SidModel>, revision: Option<DieRevision>) {
        self.chosen_model = model;
        self.chosen_revision = revision;
        let profile = revision.unwrap_or(SID6581_REVISION).profile();
        for (i, chip) in self.chips.iter_mut().enumerate() {
            let _ = chip.set_model(model.unwrap_or_else(|| own_model(&self.file, i)));
            chip.set_profile(profile);
        }
    }

    /// The 6581 revision chosen ([`set_chip`](Self::set_chip), the options), if any.
    pub fn chosen_revision(&self) -> Option<DieRevision> {
        self.chosen_revision
    }

    // --- Restarting and seeking (run the tune: not for the audio thread) -----

    /// Start again, on `subsong` (0-based; the same one when `None`): the tune's
    /// init runs again, so call it off the audio thread. The chips are new (their
    /// model and 6581 revision as the options or [`set_chip`](PsidPlayer::set_chip)
    /// chose; a revision set with [`chip_mut`](PsidPlayer::chip_mut) is not kept).
    pub fn restart(&mut self, subsong: Option<usize>) -> Result<(), PsidError> {
        let subsong = subsong
            .unwrap_or(self.subsong)
            .min(self.file.songs as usize - 1);
        let (runner, chips) = Self::build(
            &self.file,
            subsong,
            self.sample_rate,
            self.chosen_model,
            self.chosen_revision,
            self.kernal,
        )?;
        self.runner = runner;
        self.chips = chips;
        self.subsong = subsong;
        self.cycle = 0;
        self.frames = 0;
        self.apply_init_writes();
        Ok(())
    }

    /// Move to `seconds` into the tune. A tune is code, so this runs it from the
    /// start (or on from here, when the target is ahead) as fast as it will go,
    /// applying its writes to the chips at once, and plays only the last second
    /// before the target, so the sound there is the tune's own and what began
    /// longer ago is the chips' state as its writes left it (a note's attack
    /// long past is not heard again). Not for the audio thread: it takes time
    /// in proportion to `seconds`, and a seek back starts the tune again.
    pub fn seek(&mut self, seconds: f64) -> Result<(), PsidSeekError> {
        if !(0.0..=SEEK_MAX_SECONDS).contains(&seconds) {
            return Err(PsidSeekError::OutOfRange);
        }
        let hz = self.runner.clock_hz() as f64;
        let target = (seconds * hz) as u64;
        // Init worked once; it only fails now if the tune is not deterministic: stay where we are then.
        if target < self.cycle && self.restart(None).is_err() {
            return Ok(());
        }
        let rendered = (SEEK_RENDERED_SECONDS * hz) as u64;
        let skip_to = target.saturating_sub(rendered).max(self.cycle);
        let n = self.chips.len();
        while self.cycle < skip_to {
            let end = (self.cycle + SEEK_STEP_CYCLES).min(skip_to);
            self.runner.advance(end);
            let chips = &mut self.chips;
            self.runner
                .drain(end, n, |w| chips[w.chip as usize].write(w.reg, w.value));
            self.cycle = end;
        }
        // The rest is played for real, into a scratch block.
        let mut scratch = [0f32; MAX_STRETCH_SAMPLES];
        while self.cycle < target {
            let samples = self.stretch_samples;
            // The last stretch may go past the target by less than a stretch.
            let cycles = self.chips[0].cycles_in_next(samples);
            self.stretch(cycles);
            for chip in &mut self.chips {
                chip.render(&mut scratch[..samples]);
            }
        }
        self.frames = (seconds * self.sample_rate) as u64;
        Ok(())
    }

    // --- Rendering -------------------------------------------------------------

    /// Fills `out` with the next samples: the chips' mix, mono. Allocates nothing.
    pub fn render(&mut self, out: &mut [f32]) {
        self.render_inner(out, None);
    }

    /// `render`, and the first chip's three voices into `taps` (each at least as
    /// long as `out`). The mix is the one `render` produces, sample for sample.
    pub fn render_taps(&mut self, out: &mut [f32], taps: [&mut [f32]; 3]) {
        self.render_inner(out, Some(taps));
    }

    /// `render` into both channels (the chips are mono).
    pub fn render_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        self.render_inner(&mut left[..n], None);
        right[..n].copy_from_slice(&left[..n]);
    }

    /// Run the tune up to the end of a stretch of `cycles` cycles and schedule
    /// what it wrote on the chips.
    #[inline]
    fn stretch(&mut self, cycles: u64) {
        let start = self.cycle;
        let end = start + cycles;
        self.runner.advance(end);
        let chips = &mut self.chips;
        let n = chips.len();
        self.runner.drain(end, n, |w| {
            chips[w.chip as usize].write_after(w.cycle.saturating_sub(start), w.reg, w.value)
        });
        self.cycle = end;
    }

    fn render_inner(&mut self, out: &mut [f32], mut taps: Option<[&mut [f32]; 3]>) {
        let mut len = out.len();
        if let Some(t) = taps.as_ref() {
            len = len.min(t[0].len()).min(t[1].len()).min(t[2].len());
        }
        let mut done = 0;
        while done < len {
            let n = (len - done).min(self.stretch_samples);
            let cycles = self.chips[0].cycles_in_next(n);
            self.stretch(cycles);
            let o = &mut out[done..done + n];
            match taps.as_mut() {
                Some([a, b, c]) => self.chips[0].render_taps(
                    o,
                    [
                        &mut a[done..done + n],
                        &mut b[done..done + n],
                        &mut c[done..done + n],
                    ],
                ),
                None => self.chips[0].render(o),
            }
            if self.chips.len() > 1 {
                let mut scratch = [0f32; MAX_STRETCH_SAMPLES];
                for chip in &mut self.chips[1..] {
                    chip.render(&mut scratch[..n]);
                    for (x, s) in o.iter_mut().zip(&scratch[..n]) {
                        *x += *s;
                    }
                }
                let share = 1.0 / self.chips.len() as f32;
                for x in o.iter_mut() {
                    *x *= share;
                }
            }
            if self.gain != 1.0 {
                for x in o.iter_mut() {
                    *x *= self.gain;
                }
                if let Some(t) = taps.as_mut() {
                    for tap in t.iter_mut() {
                        for x in tap[done..done + n].iter_mut() {
                            *x *= self.gain;
                        }
                    }
                }
            }
            done += n;
        }
        self.frames += len as u64;
    }
}
