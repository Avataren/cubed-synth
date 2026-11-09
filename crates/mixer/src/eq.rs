//! The mixer's equaliser: ten bands (a low shelf, eight peaking bands and a
//! high shelf by default), each with its own frequency, gain, Q and switch,
//! in series, with a click-free bypass and an optional "graphic" mode.
//!
//! It is the DAW's post-fx equaliser (another_synth's `eq-math.ts` and
//! `eq-stage.ts`) as a mixer insert, and its defaults are that equaliser:
//! bands at 31, 62, 125, 250, 500, 1k, 2k, 4k, 8k and 16 kHz, peaking bands
//! at Q 0.7, shelves whose slope is the Web Audio one (Q 1/sqrt 2), all flat.
//!
//! * **Filters** are the RBJ cookbook biquads (the form Web Audio's
//!   `BiquadFilterNode` computes), run as transposed direct form II in `f64`.
//! * **Live changes never click.** A band's gain, frequency and Q glide to
//!   their new values (time constant 6.7 ms, the DAW's); while anything moves
//!   the filter is redesigned every 32 frames and its coefficients ramp
//!   from one design to the next, sample by sample; a band at 0 dB costs
//!   nothing and a flat equaliser passes the signal through untouched. A
//!   change of a band's *type* is not smoothed (the filter's shape changes at
//!   once), so switch a band flat first when that matters.
//! * **Bypass** crosses between the filtered and the dry signal in 5 ms
//!   (the DAW's). The filters keep running while bypassed so that switching
//!   back finds them warm.
//! * **Graphic mode** (`graphic` = 1) is the DAW's curve editor: the band gains
//!   are the *curve you see*. Octave-spaced bands overlap, so raw band gains
//!   stack (a flat +6 on every band comes out near +9 with scallops between the
//!   centres); the gains actually applied are the weighted least-squares fit
//!   that makes the combined response follow a smooth curve through the band
//!   gains (`eqEffectiveGains`). Gains are limited to +-12 dB in this mode, as
//!   in the DAW, and the fitted gains to +-24 dB. It works for any bands'
//!   frequencies (sorted; disabled bands are left out).
//!
//! Nothing here allocates once the insert is built: `set_param` only records
//! the value, and the next `process` retargets the bands (and solves the fit,
//! about 0.1 ms) once for however many parameters changed.

use std::f64::consts::PI;

use crate::effects::{clamp_param, param, EffectKind, ParamInfo};
use crate::insert::Insert;

/// How many bands the equaliser has.
pub const BANDS: usize = 10;
/// Parameters per band: `freq_hz`, `gain_db`, `q`, `enabled`, `type`.
const FIELDS: usize = 5;
/// The equaliser's parameter count (`bypass`, `graphic` and the bands').
pub const PARAM_COUNT: usize = 2 + FIELDS * BANDS;

const BYPASS: usize = 0;
const GRAPHIC: usize = 1;
const FREQ: usize = 0;
const GAIN: usize = 1;
const Q: usize = 2;
const ENABLED: usize = 3;
const TYPE: usize = 4;

/// The parameter index of `field` of `band` (0-based).
const fn index(band: usize, field: usize) -> usize {
    2 + band * FIELDS + field
}

/// A band's `type` parameter: 0 low shelf, 1 peaking, 2 high shelf.
const TYPE_LOW_SHELF: f32 = 0.0;
const TYPE_PEAKING: f32 = 1.0;
const TYPE_HIGH_SHELF: f32 = 2.0;
/// Q of the DAW's peaking bands.
const PEAKING_Q: f32 = 0.7;
/// Q at which a shelf has Web Audio's slope (S = 1).
const SHELF_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

macro_rules! eq_params {
    ($(($n:literal, $freq:expr, $q:expr, $kind:expr)),* $(,)?) => {
        [
            param("bypass", 0.0, 1.0, 0.0, ""),
            param("graphic", 0.0, 1.0, 0.0, ""),
            $(
                param(concat!("band", $n, "_freq_hz"), 20.0, 20_000.0, $freq, "Hz"),
                param(concat!("band", $n, "_gain_db"), -24.0, 24.0, 0.0, "dB"),
                param(concat!("band", $n, "_q"), 0.1, 10.0, $q, ""),
                param(concat!("band", $n, "_enabled"), 0.0, 1.0, 1.0, ""),
                param(concat!("band", $n, "_type"), 0.0, 2.0, $kind, ""),
            )*
        ]
    };
}

/// The equaliser's parameters, as `EffectKind::Eq.params()` lists them.
pub(crate) const PARAMS: [ParamInfo; PARAM_COUNT] = eq_params![
    (1, 31.0, SHELF_Q, TYPE_LOW_SHELF),
    (2, 62.0, PEAKING_Q, TYPE_PEAKING),
    (3, 125.0, PEAKING_Q, TYPE_PEAKING),
    (4, 250.0, PEAKING_Q, TYPE_PEAKING),
    (5, 500.0, PEAKING_Q, TYPE_PEAKING),
    (6, 1_000.0, PEAKING_Q, TYPE_PEAKING),
    (7, 2_000.0, PEAKING_Q, TYPE_PEAKING),
    (8, 4_000.0, PEAKING_Q, TYPE_PEAKING),
    (9, 8_000.0, PEAKING_Q, TYPE_PEAKING),
    (10, 16_000.0, SHELF_Q, TYPE_HIGH_SHELF),
];

/// Frames between coefficient refreshes while a band moves.
const SUB: usize = 32;
/// Time constant of every glide, seconds (the DAW's `GAIN_SMOOTH_SECONDS / 3`).
const GLIDE_SECONDS: f64 = 0.02 / 3.0;
/// Length of the bypass crossfade, seconds.
const BYPASS_SECONDS: f64 = 0.005;
/// A gain this close to its target (dB) has arrived.
const GAIN_SETTLE_DB: f64 = 1e-4;
/// A frequency or Q this close to its target (relative) has arrived.
const RATIO_SETTLE: f64 = 1e-5;
/// Filter states smaller than this are zeroed: they could only become
/// denormal numbers, which are slow on some CPUs.
const FLUSH: f64 = 1e-24;

// The graphic mode's fit (the DAW's constants).
const GRAPHIC_LIMIT_DB: f64 = 12.0;
const FIT_LIMIT_DB: f64 = 24.0;
const FLAT_DB: f64 = 0.05;
const POINTS_PER_OCTAVE: f64 = 8.0;
const NODE_WEIGHT: f64 = 20.0;
const RIDGE: f64 = 1e-3;
const GAUSS_NEWTON_STEPS: usize = 4;
/// The most points a fit uses: 8 a octave over the 10 octaves from 20 Hz to
/// 20 kHz, and the band centres.
const MAX_POINTS: usize = 96;
const MAX_STEPS: usize = MAX_POINTS - 1 - BANDS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    LowShelf,
    Peaking,
    HighShelf,
}

impl Kind {
    fn from_param(value: f32) -> Kind {
        match value.round() as i32 {
            0 => Kind::LowShelf,
            2 => Kind::HighShelf,
            _ => Kind::Peaking,
        }
    }
}

/// Biquad coefficients, normalised so that a0 = 1.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Coeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coeffs {
    /// Each coefficient's change per step when moving from `self` to `to` in `steps`.
    fn slope_to(&self, to: &Coeffs, steps: usize) -> Coeffs {
        let k = 1.0 / steps as f64;
        Coeffs {
            b0: (to.b0 - self.b0) * k,
            b1: (to.b1 - self.b1) * k,
            b2: (to.b2 - self.b2) * k,
            a1: (to.a1 - self.a1) * k,
            a2: (to.a2 - self.a2) * k,
        }
    }

    fn advance(&mut self, by: &Coeffs) {
        self.b0 += by.b0;
        self.b1 += by.b1;
        self.b2 += by.b2;
        self.a1 += by.a1;
        self.a2 += by.a2;
    }
}

const THROUGH: Coeffs = Coeffs {
    b0: 1.0,
    b1: 0.0,
    b2: 0.0,
    a1: 0.0,
    a2: 0.0,
};

/// The RBJ cookbook biquad, as the DAW's `biquadCoefficients` computes it.
/// A shelf's `q` sets alpha = sin(w0) / (2 q); at q = 1/sqrt 2 that is the
/// shelf of slope 1 which Web Audio uses whatever the Q.
fn design(kind: Kind, freq: f64, q: f64, gain_db: f64, rate: f64) -> Coeffs {
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * freq / rate;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * q);
    let (b0, b1, b2, a0, a1, a2);
    match kind {
        Kind::Peaking => {
            b0 = 1.0 + alpha * a;
            b1 = -2.0 * cos;
            b2 = 1.0 - alpha * a;
            a0 = 1.0 + alpha / a;
            a1 = -2.0 * cos;
            a2 = 1.0 - alpha / a;
        }
        Kind::LowShelf => {
            let k = 2.0 * a.sqrt() * alpha;
            b0 = a * (a + 1.0 - (a - 1.0) * cos + k);
            b1 = 2.0 * a * (a - 1.0 - (a + 1.0) * cos);
            b2 = a * (a + 1.0 - (a - 1.0) * cos - k);
            a0 = a + 1.0 + (a - 1.0) * cos + k;
            a1 = -2.0 * (a - 1.0 + (a + 1.0) * cos);
            a2 = a + 1.0 + (a - 1.0) * cos - k;
        }
        Kind::HighShelf => {
            let k = 2.0 * a.sqrt() * alpha;
            b0 = a * (a + 1.0 + (a - 1.0) * cos + k);
            b1 = -2.0 * a * (a - 1.0 + (a + 1.0) * cos);
            b2 = a * (a + 1.0 + (a - 1.0) * cos - k);
            a0 = a + 1.0 - (a - 1.0) * cos + k;
            a1 = 2.0 * (a - 1.0 - (a + 1.0) * cos);
            a2 = a + 1.0 - (a - 1.0) * cos - k;
        }
    }
    Coeffs {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    }
}

/// The cosines and sines of a frequency's w and 2w, for evaluating a
/// biquad's response there.
#[derive(Clone, Copy, Default)]
struct Probe {
    c1: f64,
    s1: f64,
    c2: f64,
    s2: f64,
}

impl Probe {
    fn at(freq: f64, rate: f64) -> Probe {
        let w = 2.0 * PI * freq / rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        Probe { c1, s1, c2, s2 }
    }
}

impl Coeffs {
    /// The magnitude response in dB (the DAW's `eqResponseDb` per band).
    fn response_db(&self, p: Probe) -> f64 {
        let nr = self.b0 + self.b1 * p.c1 + self.b2 * p.c2;
        let ni = -(self.b1 * p.s1 + self.b2 * p.s2);
        let dr = 1.0 + self.a1 * p.c1 + self.a2 * p.c2;
        let di = -(self.a1 * p.s1 + self.a2 * p.s2);
        let mag2 = (nr * nr + ni * ni) / (dr * dr + di * di);
        10.0 * mag2.max(1e-12).log10()
    }
}

/// How a band is set up, apart from its gain: what the graphic fit depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Slot {
    kind: Kind,
    freq: f64,
    q: f64,
    enabled: bool,
}

const FLAT_SLOT: Slot = Slot {
    kind: Kind::Peaking,
    freq: 1_000.0,
    q: 0.7,
    enabled: false,
};

/// One band: the filter, where its parameters are heading, and its state.
#[derive(Clone, Copy)]
struct Band {
    kind: Kind,
    freq: f64,
    q: f64,
    gain: f64,
    target_freq: f64,
    target_q: f64,
    target_gain: f64,
    coeffs: Coeffs,
    /// Transposed direct form II state of the left and right filters.
    z: [[f64; 2]; 2],
}

impl Band {
    fn new() -> Band {
        Band {
            kind: Kind::Peaking,
            freq: 1_000.0,
            q: 0.7,
            gain: 0.0,
            target_freq: 1_000.0,
            target_q: 0.7,
            target_gain: 0.0,
            coeffs: THROUGH,
            z: [[0.0; 2]; 2],
        }
    }

    fn refresh(&mut self, rate: f64) {
        self.coeffs = if self.gain == 0.0 {
            THROUGH
        } else {
            design(self.kind, self.freq, self.q, self.gain, rate)
        };
    }

    /// Jumps to the targets.
    fn snap(&mut self, rate: f64) {
        self.freq = self.target_freq;
        self.q = self.target_q;
        self.gain = self.target_gain;
        self.refresh(rate);
    }

    /// Moves a fraction `a` of the way to the targets and refreshes the
    /// coefficients. A band that is flat and stays flat has nothing to
    /// refresh: its frequency and Q just arrive.
    fn glide(&mut self, a: f64, rate: f64) {
        if self.gain == self.target_gain && self.freq == self.target_freq && self.q == self.target_q
        {
            return;
        }
        if self.gain == 0.0 && self.target_gain == 0.0 {
            self.freq = self.target_freq;
            self.q = self.target_q;
            return;
        }
        self.gain += (self.target_gain - self.gain) * a;
        if (self.target_gain - self.gain).abs() < GAIN_SETTLE_DB {
            self.gain = self.target_gain;
        }
        self.freq = approach(self.freq, self.target_freq, a);
        self.q = approach(self.q, self.target_q, a);
        self.refresh(rate);
    }

    /// Filters `l` and `r` in place (transposed direct form II), the coefficients
    /// moving in a straight line from `from` (what they were when the chunk
    /// began) to the band's now. Interpolated coefficients of stable filters
    /// are stable (the stable ones are a convex set), and a chunk's staircase
    /// of steps becomes a ramp.
    fn run(&mut self, from: Coeffs, l: &mut [f64], r: &mut [f64]) {
        let to = self.coeffs;
        let slope = if from == to {
            None
        } else {
            Some(from.slope_to(&to, l.len()))
        };
        for (z, x) in [(0, l), (1, r)] {
            let [mut z1, mut z2] = self.z[z];
            let mut c = from;
            for s in x.iter_mut() {
                if let Some(slope) = &slope {
                    c.advance(slope);
                }
                let input = *s;
                let y = c.b0 * input + z1;
                z1 = c.b1 * input - c.a1 * y + z2;
                z2 = c.b2 * input - c.a2 * y;
                *s = y;
            }
            self.z[z] = [z1, z2];
        }
    }

    /// Zeroes states that decayed to nothing (or broke): see [`FLUSH`].
    fn flush(&mut self) {
        for z in self.z.iter_mut().flatten() {
            if !z.is_finite() || z.abs() < FLUSH {
                *z = 0.0;
            }
        }
    }
}

/// `value` moved the fraction `a` of the way to `target` on a log scale.
fn approach(value: f64, target: f64, a: f64) -> f64 {
    let next = value * (target / value).powf(a);
    if (next / target - 1.0).abs() < RATIO_SETTLE {
        target
    } else {
        next
    }
}

fn glide_fraction(frames: usize, rate: f64) -> f64 {
    1.0 - (-(frames as f64) / (GLIDE_SECONDS * rate)).exp()
}

/// The graphic mode's solver: the band gains whose combined response follows
/// a smooth curve through the gains the user set (the DAW's `buildFit` and
/// `eqEffectiveGains`). It depends on the bands' frequencies, Qs and types
/// only, so it is rebuilt when one of those changes, not when a gain does.
struct Fit {
    /// The bands' set-up it was built for.
    slots: [Slot; BANDS],
    built: bool,
    /// The enabled bands in order of frequency; fewer than two make no fit.
    nodes: usize,
    order: [usize; BANDS],
    points: usize,
    probes: [Probe; MAX_POINTS],
    /// For each point: the node to its left and the smoothstep of its
    /// position toward the next one.
    left: [usize; MAX_POINTS],
    blend: [f64; MAX_POINTS],
    /// The pseudo-inverse: nodes x points.
    solve: [[f64; MAX_POINTS]; BANDS],
    /// Scratch for building it: each node's response to 1 dB, and the
    /// augmented system.
    unit: [[f64; MAX_POINTS]; BANDS],
    system: [[f64; BANDS + MAX_POINTS]; BANDS],
}

impl Fit {
    fn new() -> Box<Fit> {
        Box::new(Fit {
            slots: [FLAT_SLOT; BANDS],
            built: false,
            nodes: 0,
            order: [0; BANDS],
            points: 0,
            probes: [Probe::default(); MAX_POINTS],
            left: [0; MAX_POINTS],
            blend: [0.0; MAX_POINTS],
            solve: [[0.0; MAX_POINTS]; BANDS],
            unit: [[0.0; MAX_POINTS]; BANDS],
            system: [[0.0; BANDS + MAX_POINTS]; BANDS],
        })
    }

    fn build(&mut self, slots: &[Slot; BANDS], rate: f64) {
        self.slots = *slots;
        self.built = true;
        let mut order = [0usize; BANDS];
        let mut nodes = 0;
        for (band, slot) in slots.iter().enumerate() {
            if slot.enabled {
                order[nodes] = band;
                nodes += 1;
            }
        }
        // Insertion sort by frequency (stable: equal ones keep their order).
        for i in 1..nodes {
            let mut j = i;
            while j > 0 && slots[order[j - 1]].freq > slots[order[j]].freq {
                order.swap(j - 1, j);
                j -= 1;
            }
        }
        self.order = order;
        self.nodes = nodes;
        self.points = 0;
        if nodes < 2 {
            return;
        }
        let node_freq = |j: usize| slots[order[j]].freq;
        let lo = node_freq(0).log2();
        let hi = node_freq(nodes - 1).log2();
        let steps = (((hi - lo) * POINTS_PER_OCTAVE).round() as usize).clamp(1, MAX_STEPS);
        let points = steps + 1 + nodes;
        let mut weight = [1.0f64; MAX_POINTS];
        let mut freqs = [0.0f64; MAX_POINTS];
        for (i, f) in freqs.iter_mut().enumerate().take(steps + 1) {
            *f = (lo + (hi - lo) * i as f64 / steps as f64).exp2();
        }
        for j in 0..nodes {
            freqs[steps + 1 + j] = node_freq(j);
            weight[steps + 1 + j] = NODE_WEIGHT;
        }
        for k in 0..points {
            let f = freqs[k];
            self.probes[k] = Probe::at(f, rate);
            let mut left = 0;
            while left < nodes - 2 && f >= node_freq(left + 1) {
                left += 1;
            }
            let a = node_freq(left).log2();
            let b = node_freq(left + 1).log2();
            let x = if b - a > 1e-12 {
                (f.log2() - a) / (b - a)
            } else {
                0.0
            };
            self.left[k] = left;
            self.blend[k] = x * x * (3.0 - 2.0 * x);
        }
        // Each node's response to 6 dB, per dB: close to linear in the gain.
        for j in 0..nodes {
            let s = slots[order[j]];
            let c = design(s.kind, s.freq, s.q, 6.0, rate);
            for k in 0..points {
                self.unit[j][k] = c.response_db(self.probes[k]) / 6.0;
            }
        }
        // (UtWU + ridge) X = UtW, solved for X by Gauss-Jordan elimination.
        let n = nodes;
        for i in 0..n {
            for j in 0..n {
                let mut sum = if i == j { RIDGE } else { 0.0 };
                for k in 0..points {
                    sum += weight[k] * self.unit[i][k] * self.unit[j][k];
                }
                self.system[i][j] = sum;
            }
            for k in 0..points {
                self.system[i][n + k] = weight[k] * self.unit[i][k];
            }
        }
        let width = n + points;
        for c in 0..n {
            let mut pivot = c;
            for r in c + 1..n {
                if self.system[r][c].abs() > self.system[pivot][c].abs() {
                    pivot = r;
                }
            }
            self.system.swap(c, pivot);
            let d = self.system[c][c];
            for k in c..width {
                self.system[c][k] /= d;
            }
            for r in 0..n {
                if r == c {
                    continue;
                }
                let f = self.system[r][c];
                if f == 0.0 {
                    continue;
                }
                for k in c..width {
                    let v = self.system[c][k];
                    self.system[r][k] -= f * v;
                }
            }
        }
        for i in 0..n {
            for k in 0..points {
                self.solve[i][k] = self.system[i][n + k];
            }
        }
        self.points = points;
    }

    /// Turns the gains the user set (limited to +-12 dB, 0 for a disabled
    /// band) into the gains to apply: the DAW's `eqEffectiveGains`.
    fn effective(&self, gains: &mut [f64; BANDS], rate: f64) {
        if self.nodes < 2 {
            return;
        }
        let n = self.nodes;
        let mut set = [0.0f64; BANDS];
        for j in 0..n {
            set[j] = gains[self.order[j]];
        }
        if set[..n].iter().all(|g| g.abs() < FLAT_DB) {
            *gains = [0.0; BANDS];
            return;
        }
        let mut want = [0.0f64; MAX_POINTS];
        for k in 0..self.points {
            let (left, s) = (self.left[k], self.blend[k]);
            want[k] = set[left] * (1.0 - s) + set[left + 1] * s;
        }
        let mut g = set;
        for _ in 0..GAUSS_NEWTON_STEPS {
            let mut response = [0.0f64; MAX_POINTS];
            for j in 0..n {
                // The DAW leaves out bands this flat; so do we.
                if g[j].abs() < 1e-4 {
                    continue;
                }
                let s = self.slots[self.order[j]];
                let c = design(s.kind, s.freq, s.q, g[j], rate);
                for k in 0..self.points {
                    response[k] += c.response_db(self.probes[k]);
                }
            }
            for j in 0..n {
                let mut delta = 0.0;
                for k in 0..self.points {
                    delta += self.solve[j][k] * (want[k] - response[k]);
                }
                g[j] = (g[j] + delta).clamp(-FIT_LIMIT_DB, FIT_LIMIT_DB);
            }
        }
        *gains = [0.0; BANDS];
        for j in 0..n {
            gains[self.order[j]] = g[j];
        }
    }
}

/// The equaliser as a mixer insert (`EffectKind::Eq`).
pub(crate) struct EqInsert {
    rate: f64,
    /// The parameters, clamped to their ranges.
    params: [f32; PARAM_COUNT],
    /// A parameter changed since the bands were last retargeted.
    dirty: bool,
    bands: [Band; BANDS],
    fit: Box<Fit>,
    /// How much of the filtered signal is heard: 1 engaged, 0 bypassed.
    wet: f64,
    wet_target: f64,
    wet_step: f64,
    sub_fraction: f64,
}

impl EqInsert {
    /// An equaliser with the parameters in `params` (in the order of
    /// `EffectKind::Eq.params()`; missing ones are the defaults), settled
    /// on them. Allocates: build it off the audio thread.
    pub(crate) fn new(sample_rate: f32, params: &[f32]) -> EqInsert {
        let rate = (sample_rate as f64).max(1.0);
        let mut values = [0.0f32; PARAM_COUNT];
        for (i, value) in values.iter_mut().enumerate() {
            *value = match params.get(i) {
                Some(&v) => clamp_param(EffectKind::Eq, i, v),
                None => PARAMS[i].default,
            };
        }
        let mut eq = EqInsert {
            rate,
            params: values,
            dirty: true,
            bands: [Band::new(); BANDS],
            fit: Fit::new(),
            wet: 1.0,
            wet_target: 1.0,
            wet_step: 1.0 / (BYPASS_SECONDS * rate),
            sub_fraction: glide_fraction(SUB, rate),
        };
        eq.retarget();
        eq.snap();
        eq
    }

    fn snap(&mut self) {
        for band in &mut self.bands {
            band.snap(self.rate);
        }
        self.wet = self.wet_target;
    }

    /// Reads the parameters into the bands' targets (and solves the graphic
    /// fit when that is on).
    fn retarget(&mut self) {
        self.dirty = false;
        let p = &self.params;
        let graphic = p[GRAPHIC] >= 0.5;
        self.wet_target = if p[BYPASS] >= 0.5 { 0.0 } else { 1.0 };
        let nyquist = self.rate * 0.49;
        let mut slots = [FLAT_SLOT; BANDS];
        let mut gains = [0.0f64; BANDS];
        for band in 0..BANDS {
            let enabled = p[index(band, ENABLED)] >= 0.5;
            slots[band] = Slot {
                kind: Kind::from_param(p[index(band, TYPE)]),
                freq: (p[index(band, FREQ)] as f64).min(nyquist),
                q: p[index(band, Q)] as f64,
                enabled,
            };
            gains[band] = if enabled {
                p[index(band, GAIN)] as f64
            } else {
                0.0
            };
        }
        if graphic {
            for g in &mut gains {
                *g = g.clamp(-GRAPHIC_LIMIT_DB, GRAPHIC_LIMIT_DB);
            }
            if !self.fit.built || self.fit.slots != slots {
                self.fit.build(&slots, self.rate);
            }
            self.fit.effective(&mut gains, self.rate);
        }
        for (band, state) in self.bands.iter_mut().enumerate() {
            let slot = slots[band];
            state.target_freq = slot.freq;
            state.target_q = slot.q;
            state.target_gain = gains[band];
            if state.kind != slot.kind {
                state.kind = slot.kind;
                state.refresh(self.rate);
            }
        }
    }

    /// The combined response in dB at `hz`, of the filters as they are now.
    fn response_db(&self, hz: f64) -> f64 {
        let probe = Probe::at(hz, self.rate);
        self.bands
            .iter()
            .filter(|b| b.gain != 0.0)
            .map(|b| b.coeffs.response_db(probe))
            .sum()
    }
}

impl Insert for EqInsert {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.dirty {
            self.retarget();
        }
        let n = left.len().min(right.len());
        let mut at = 0;
        while at < n {
            let m = (n - at).min(SUB);
            let fraction = if m == SUB {
                self.sub_fraction
            } else {
                glide_fraction(m, self.rate)
            };
            let mut before = [THROUGH; BANDS];
            for (band, was) in self.bands.iter_mut().zip(&mut before) {
                *was = band.coeffs;
                band.glide(fraction, self.rate);
            }
            if self.bands.iter().all(|b| b.gain == 0.0) {
                // Flat: the signal goes through as it is.
                for band in &mut self.bands {
                    band.z = [[0.0; 2]; 2];
                }
                let d = self.wet_target - self.wet;
                self.wet += d.clamp(-self.wet_step * m as f64, self.wet_step * m as f64);
                at += m;
                continue;
            }
            let (mut l, mut r) = ([0.0f64; SUB], [0.0f64; SUB]);
            for j in 0..m {
                l[j] = left[at + j] as f64;
                r[j] = right[at + j] as f64;
            }
            for (band, was) in self.bands.iter_mut().zip(before) {
                if band.gain == 0.0 {
                    band.z = [[0.0; 2]; 2];
                } else {
                    band.run(was, &mut l[..m], &mut r[..m]);
                }
            }
            let mut wet = self.wet;
            for j in 0..m {
                if wet != self.wet_target {
                    let d = self.wet_target - wet;
                    wet += d.clamp(-self.wet_step, self.wet_step);
                }
                if wet >= 1.0 {
                    left[at + j] = l[j] as f32;
                    right[at + j] = r[j] as f32;
                } else {
                    let (dl, dr) = (left[at + j] as f64, right[at + j] as f64);
                    left[at + j] = (dl + wet * (l[j] - dl)) as f32;
                    right[at + j] = (dr + wet * (r[j] - dr)) as f32;
                }
            }
            self.wet = wet;
            at += m;
        }
        for band in &mut self.bands {
            band.flush();
        }
    }

    fn reset(&mut self) {
        for band in &mut self.bands {
            band.z = [[0.0; 2]; 2];
        }
        if self.dirty {
            self.retarget();
        }
        self.snap();
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index < PARAM_COUNT {
            self.params[index] = clamp_param(EffectKind::Eq, index, value);
            self.dirty = true;
        }
    }
}

/// The equaliser's magnitude response in dB at each frequency of `hz`, for
/// the parameters in `params` (in the order of `EffectKind::Eq.params()`;
/// missing ones are the defaults), at `sample_rate`, as if engaged: what an
/// editor draws as the curve. Allocates.
pub fn response_db(params: &[f32], sample_rate: f32, hz: &[f32], out: &mut [f32]) {
    let eq = EqInsert::new(sample_rate, params);
    for (o, &f) in out.iter_mut().zip(hz) {
        *o = eq.response_db(f as f64) as f32;
    }
}

/// What the DAW's equaliser computes (shared with `tests/eq`).
#[cfg(test)]
#[path = "../tests/eq/reference.rs"]
mod reference;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::Effect;

    use super::reference;

    fn effect_with(gains: &[f32], graphic: bool) -> EqInsert {
        let mut effect = Effect::new(EffectKind::Eq);
        effect.set("graphic", graphic as u8 as f32);
        for (band, &g) in gains.iter().enumerate() {
            effect.set(&format!("band{}_gain_db", band + 1), g);
        }
        EqInsert::new(48_000.0, &effect.params)
    }

    /// The gains the graphic fit applies are the DAW's `eqEffectiveGains`.
    #[test]
    fn the_fitted_gains_are_the_daws() {
        let mut worst = 0.0f64;
        for case in &reference::GRAPHIC {
            let eq = effect_with(&case.set, true);
            for (band, &want) in case.applied.iter().enumerate() {
                let got = eq.bands[band].target_gain;
                worst = worst.max((got - want).abs());
                assert!(
                    (got - want).abs() < 2e-3,
                    "{} band {band}: {got} dB, the DAW's {want} dB",
                    case.name
                );
            }
        }
        println!("worst deviation of a fitted gain from the DAW's: {worst} dB");
    }

    /// A peaking band has its gain at its centre and a shelf its gain far from its corner.
    #[test]
    fn the_designs_have_their_gains() {
        let rate = 48_000.0;
        let peak = design(Kind::Peaking, 1_000.0, 2.0, 9.0, rate);
        assert!((peak.response_db(Probe::at(1_000.0, rate)) - 9.0).abs() < 1e-9);
        let low = design(Kind::LowShelf, 200.0, SHELF_Q as f64, -7.0, rate);
        assert!((low.response_db(Probe::at(1.0, rate)) + 7.0).abs() < 0.01);
        assert!(low.response_db(Probe::at(10_000.0, rate)).abs() < 0.01);
        let high = design(Kind::HighShelf, 4_000.0, SHELF_Q as f64, 5.0, rate);
        assert!((high.response_db(Probe::at(23_000.0, rate)) - 5.0).abs() < 0.05);
        // The gain's sign is symmetric: +g and -g cancel.
        let up = design(Kind::Peaking, 800.0, 1.3, 6.0, rate);
        let down = design(Kind::Peaking, 800.0, 1.3, -6.0, rate);
        for hz in [100.0, 800.0, 3_000.0] {
            let p = Probe::at(hz, rate);
            assert!((up.response_db(p) + down.response_db(p)).abs() < 1e-9);
        }
    }

    /// The fit is only rebuilt when the layout changes, and handles odd layouts.
    #[test]
    fn the_fit_handles_odd_layouts() {
        let mut effect = Effect::new(EffectKind::Eq);
        effect.set("graphic", 1.0);
        // One band alone: no curve to fit, its gain is its own.
        for band in 1..BANDS {
            effect.set(&format!("band{}_enabled", band + 1), 0.0);
        }
        effect.set("band1_gain_db", 5.0);
        let eq = EqInsert::new(48_000.0, &effect.params);
        assert_eq!(eq.bands[0].target_gain, 5.0);
        // Two bands on one frequency, unsorted bands: finite, and flat gains stay flat.
        let mut effect = Effect::new(EffectKind::Eq);
        effect.set("graphic", 1.0);
        effect.set("band1_freq_hz", 5_000.0);
        effect.set("band2_freq_hz", 5_000.0);
        effect.set("band3_freq_hz", 20.0);
        effect.set("band10_freq_hz", 20_000.0);
        effect.set("band2_gain_db", 6.0);
        let eq = EqInsert::new(48_000.0, &effect.params);
        assert!(eq.bands.iter().all(|b| b.target_gain.is_finite()));
        assert!(eq.bands[1].target_gain > 3.0);
        // Gains of 0 are 0 whatever the layout.
        let eq = effect_with(&[0.0; BANDS], true);
        assert!(eq.bands.iter().all(|b| b.target_gain == 0.0));
    }
}
