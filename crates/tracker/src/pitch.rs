//! How each format represents and slides pitch.
//!
//! The player works in "musical Hz": a format's playback rate divided by a
//! per-format scale, so notes land near their equal-tempered frequencies.
//! Slides move a period (or, for XM's linear table, a linear period) and the
//! frequency follows from it; the sample voices turn a musical frequency
//! back into a playback rate.

/// The PAL Amiga's Paula clock, doubled (Paula plays `clock / period`).
pub const AMIGA_CLOCK: f64 = 3_546_895.0 * 2.0;
/// Paula's rates over musical Hz: period 856 (C-1) is about 4144 Hz, C-1
/// in musical Hz about 32 Hz.
pub const PAULA_TO_SYNTH_SCALE: f64 = 128.0;

/// ProTracker's 36 periods, C-1 to B-3.
pub(crate) const PT_PERIODS: [u16; 36] = [
    856, 808, 762, 720, 678, 640, 604, 570, 538, 508, 480, 453, //
    428, 404, 381, 360, 339, 320, 302, 285, 269, 254, 240, 226, //
    214, 202, 190, 180, 170, 160, 151, 143, 135, 127, 120, 113,
];

/// The seven-octave extension PC trackers use for MODs, C-(-1) to B-5.
pub(crate) const MOD_EXTENDED_PERIODS: [u16; 84] = [
    3424, 3232, 3048, 2880, 2712, 2560, 2416, 2280, 2152, 2032, 1920, 1812, //
    1712, 1616, 1524, 1440, 1356, 1280, 1208, 1140, 1076, 1016, 960, 907, //
    856, 808, 762, 720, 678, 640, 604, 570, 538, 508, 480, 453, //
    428, 404, 381, 360, 339, 320, 302, 285, 269, 254, 240, 226, //
    214, 202, 190, 180, 170, 160, 151, 143, 135, 127, 120, 113, //
    107, 101, 95, 90, 85, 80, 75, 71, 67, 63, 60, 56, //
    53, 50, 47, 45, 42, 40, 37, 35, 33, 31, 30, 28,
];

/// ST3's periods for one octave (`notespd`); each octave down doubles them.
const ST3_NOTESPD: [u16; 12] = [
    27392, 25856, 24384, 23040, 21696, 20480, 19328, 18240, 17216, 16256, 15360, 14512,
];

/// ST3's period for each of its 96 notes (C-0 to B-7).
pub(crate) const S3M_PERIODS: [u16; 96] = {
    let mut table = [0u16; 96];
    let mut i = 0;
    while i < 96 {
        table[i] = ST3_NOTESPD[i % 12] >> (i / 12);
        i += 1;
    }
    table
};

/// ST3's `setspd`: `hz = 14317056 / period`.
const S3M_RATE_NUMERATOR: f64 = 14_317_056.0;
/// ST3's rates over musical Hz.
const S3M_TO_SYNTH_SCALE: f64 = 16.0;

/// One format's pitch domain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PitchModel {
    /// ProTracker periods. `amiga_limits` keeps them inside its three
    /// octaves; `wraps_to_dc` reproduces its arpeggio reading past the end
    /// of the table (period 0, silence).
    Amiga {
        amiga_limits: bool,
        wraps_to_dc: bool,
    },
    /// Scream Tracker 3 periods: 64-32767, or 453-3424 with the file's
    /// Amiga-limits flag. The sample's c2spd tunes its playback rate, not
    /// the period.
    S3m { amiga_limits: bool },
    /// FastTracker 2's linear table: 64 units a semitone, C-4 at 4608.
    Linear,
    /// FastTracker 2's Amiga table: C-4 at period 1712, an octave per
    /// doubling, 113-27392.
    XmAmiga,
}

const XM_REFERENCE_RATE: f64 = 8363.0;
const XM_REFERENCE_PERIOD: f64 = 4608.0;
const XM_UNITS_PER_SEMITONE: f64 = 64.0;
const XM_UNITS_PER_OCTAVE: f64 = 768.0;
/// FT2's rates over musical Hz.
const LINEAR_TO_SYNTH_SCALE: f64 = 32.0;
const XM_AMIGA_REFERENCE_PERIOD: f64 = 1712.0;
const XM_AMIGA_REFERENCE_NOTE: f64 = 48.0;

impl PitchModel {
    /// Whether periods rise as pitch falls (all but XM's linear table).
    pub(crate) fn is_amiga(&self) -> bool {
        !matches!(self, PitchModel::Linear)
    }

    fn table(&self) -> &'static [u16] {
        match *self {
            PitchModel::Amiga {
                amiga_limits: true, ..
            } => &PT_PERIODS,
            PitchModel::Amiga { .. } => &MOD_EXTENDED_PERIODS,
            PitchModel::S3m { .. } => &S3M_PERIODS,
            PitchModel::Linear | PitchModel::XmAmiga => &[],
        }
    }

    pub(crate) fn clamp_period(&self, period: f64) -> f64 {
        if !period.is_finite() {
            return period;
        }
        let (min, max) = match *self {
            PitchModel::Amiga {
                amiga_limits: true, ..
            } => (113.0, 856.0),
            PitchModel::Amiga { .. } => (14.0, 3424.0),
            PitchModel::S3m { amiga_limits: true } => (453.0, 3424.0),
            PitchModel::S3m { .. } => (64.0, 32767.0),
            PitchModel::Linear => (1.0, 31999.0),
            PitchModel::XmAmiga => (113.0, 27392.0),
        };
        period.clamp(min, max)
    }

    pub(crate) fn frequency_from_period(&self, period: f64) -> f64 {
        match self {
            PitchModel::Amiga { .. } => AMIGA_CLOCK / (2.0 * period * PAULA_TO_SYNTH_SCALE),
            PitchModel::S3m { .. } => S3M_RATE_NUMERATOR / period / S3M_TO_SYNTH_SCALE,
            PitchModel::Linear => {
                XM_REFERENCE_RATE * 2f64.powf((XM_REFERENCE_PERIOD - period) / XM_UNITS_PER_OCTAVE)
                    / LINEAR_TO_SYNTH_SCALE
            }
            PitchModel::XmAmiga => {
                XM_REFERENCE_RATE * XM_AMIGA_REFERENCE_PERIOD / period / LINEAR_TO_SYNTH_SCALE
            }
        }
    }

    pub(crate) fn raw_period_from_frequency(&self, frequency: f64) -> f64 {
        match self {
            PitchModel::Amiga { .. } => AMIGA_CLOCK / (2.0 * frequency * PAULA_TO_SYNTH_SCALE),
            PitchModel::S3m { .. } => S3M_RATE_NUMERATOR / (frequency * S3M_TO_SYNTH_SCALE),
            PitchModel::Linear => {
                XM_REFERENCE_PERIOD
                    - XM_UNITS_PER_OCTAVE
                        * (frequency * LINEAR_TO_SYNTH_SCALE / XM_REFERENCE_RATE).log2()
            }
            PitchModel::XmAmiga => {
                XM_REFERENCE_RATE * XM_AMIGA_REFERENCE_PERIOD / (frequency * LINEAR_TO_SYNTH_SCALE)
            }
        }
    }

    pub(crate) fn period_from_frequency(&self, frequency: f64) -> f64 {
        self.clamp_period(self.raw_period_from_frequency(frequency))
    }

    /// The period `semitones` table steps from the one nearest `base`.
    pub(crate) fn arpeggio_period(&self, base: f64, semitones: i32) -> f64 {
        match *self {
            PitchModel::Amiga { wraps_to_dc, .. } => {
                let table = self.table();
                let index = nearest_index(base, table) as i32 + semitones;
                if index < 0 || index >= table.len() as i32 {
                    if wraps_to_dc {
                        return 0.0;
                    }
                    return table[index.clamp(0, table.len() as i32 - 1) as usize] as f64;
                }
                table[index as usize] as f64
            }
            PitchModel::S3m { .. } => {
                let table = self.table();
                let index = nearest_index(base, table) as i32 + semitones;
                table[index.clamp(0, table.len() as i32 - 1) as usize] as f64
            }
            PitchModel::Linear => {
                self.clamp_period(base - semitones as f64 * XM_UNITS_PER_SEMITONE)
            }
            PitchModel::XmAmiga => self.clamp_period(base * 2f64.powf(-semitones as f64 / 12.0)),
        }
    }

    /// The table period nearest `period` (glissando).
    pub(crate) fn snap_period(&self, period: f64) -> f64 {
        match self {
            PitchModel::Linear => self.clamp_period(
                crate::effects::js_round(period / XM_UNITS_PER_SEMITONE) * XM_UNITS_PER_SEMITONE,
            ),
            PitchModel::XmAmiga => {
                let note =
                    XM_AMIGA_REFERENCE_NOTE - 12.0 * (period / XM_AMIGA_REFERENCE_PERIOD).log2();
                let note = crate::effects::js_round(note);
                self.clamp_period(
                    XM_AMIGA_REFERENCE_PERIOD * 2f64.powf((XM_AMIGA_REFERENCE_NOTE - note) / 12.0),
                )
            }
            _ => {
                let table = self.table();
                table[nearest_index(period, table)] as f64
            }
        }
    }

    /// Cents a vibrato of `depth` period units bends `frequency` by.
    pub(crate) fn vibrato_depth_cents(&self, frequency: f64, depth: f64) -> f64 {
        if depth <= 0.0 {
            return 0.0;
        }
        if *self == PitchModel::Linear {
            return depth * (1200.0 / XM_UNITS_PER_OCTAVE);
        }
        let base = self.raw_period_from_frequency(frequency);
        if base <= 0.0 || !base.is_finite() {
            return 0.0;
        }
        1200.0 * ((base + depth) / base).log2()
    }
}

/// The index of the table period nearest `period`, ties to the lower pitch.
fn nearest_index(period: f64, table: &[u16]) -> usize {
    let last = table.len() - 1;
    if !period.is_finite() || period >= table[0] as f64 {
        return 0;
    }
    if period <= table[last] as f64 {
        return last;
    }
    let (mut lo, mut hi) = (0, last);
    while hi - lo > 1 {
        let mid = (lo + hi) >> 1;
        if table[mid] as f64 > period {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    if table[lo] as f64 - period <= period - table[hi] as f64 {
        lo
    } else {
        hi
    }
}
