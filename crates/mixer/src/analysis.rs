//! What an editor shows of the sound: a scope (the latest frames of a bus), a
//! spectrum, level meters.
//!
//! [`Analysis`] is how a host reads them, from its main thread once per frame:
//! natively `NativeHost` implements it over the mixer's taps and meters, on the web
//! [`WebAnalysis`] implements it over the messages the worklet posts
//! (`cubed-worklet`'s `'taps'` and `'telemetry'`). [`Spectrum`] turns what
//! `scope` returned into bars; it is plain Rust, the same on both.

use crate::command::BusId;
use crate::mixer::BusMeter;

/// Reads the analysis of a running mixer. All calls are cheap and never wait on
/// the audio thread.
pub trait Analysis {
    /// The mixer's sample rate.
    fn sample_rate(&self) -> f32;

    /// Fills `left` and `right` with the latest frames of the bus's tap, oldest
    /// first, as many as fit in the shorter slice, and returns how many: 0 when
    /// the bus has no tap (enable one first) or has not rendered yet, fewer
    /// than asked while the tap fills up. A tap keeps 4096 frames natively; on
    /// the web the latest 2048 arrive about 40 times a second.
    fn scope(&self, bus: BusId, left: &mut [f32], right: &mut [f32]) -> usize;

    /// Frames the bus's tap has seen in all. It changes when new frames have
    /// come in, so a host can skip redrawing a scope that has not moved.
    fn scope_position(&self, bus: BusId) -> u64;

    /// The bus's level after its fader: peak (falling 24 dB a second) and RMS
    /// over the last 300 ms, per channel, as linear amplitudes; `None` if the
    /// bus does not exist. A UI does its own peak hold.
    fn levels(&self, bus: BusId) -> Option<BusMeter>;
}

/// Floats per bus in the worklet's meter telemetry: exists (0/1), peak left and
/// right, RMS left and right.
pub const METER_FIELDS: usize = 5;

/// The analysis a worklet's messages carry, held on the main thread so it can be
/// read like the native one (see [`Analysis`]). Feed it each `'taps'` message
/// with [`update_taps`](Self::update_taps) and each `'telemetry'` message's meters
/// with [`update_meters`](Self::update_meters). It allocates when a bus first
/// shows up and when a message is longer than the last, not otherwise.
#[derive(Debug)]
pub struct WebAnalysis {
    sample_rate: f32,
    taps: Vec<WebTap>,
    meters: Vec<f32>,
}

#[derive(Debug, Default, Clone)]
struct WebTap {
    left: Vec<f32>,
    right: Vec<f32>,
    position: u64,
}

impl WebAnalysis {
    /// `sample_rate` is the AudioContext's.
    pub fn new(sample_rate: f32) -> WebAnalysis {
        WebAnalysis {
            sample_rate,
            taps: Vec::new(),
            meters: Vec::new(),
        }
    }

    /// Takes a `'taps'` message: `buses` the bus slots it has frames for,
    /// `written` how many frames each tap had seen when it was cut (as the
    /// worklet's float64 array), `frames` the frames per bus and channel, and
    /// `data` all of them: for each bus in order its `frames` left frames, then
    /// its `frames` right frames. A bus that is not in the message keeps what it
    /// had (a tap that was switched off goes quiet when the host calls
    /// [`forget`](Self::forget)). A malformed message is ignored.
    pub fn update_taps(&mut self, buses: &[u32], written: &[f64], frames: usize, data: &[f32]) {
        if frames == 0 || data.len() < buses.len() * 2 * frames || written.len() < buses.len() {
            return;
        }
        for (i, &bus) in buses.iter().enumerate() {
            let bus = bus as usize;
            if bus >= u16::MAX as usize {
                continue;
            }
            if self.taps.len() <= bus {
                self.taps.resize(bus + 1, WebTap::default());
            }
            let tap = &mut self.taps[bus];
            let base = i * 2 * frames;
            tap.left.clear();
            tap.left.extend_from_slice(&data[base..base + frames]);
            tap.right.clear();
            tap.right
                .extend_from_slice(&data[base + frames..base + 2 * frames]);
            tap.position = written[i] as u64;
        }
    }

    /// Takes the meters of a `'telemetry'` message: [`METER_FIELDS`] floats per
    /// bus slot.
    pub fn update_meters(&mut self, meters: &[f32]) {
        self.meters.clear();
        self.meters.extend_from_slice(meters);
    }

    /// Drops what it holds for `bus` (its tap was switched off).
    pub fn forget(&mut self, bus: BusId) {
        if let Some(tap) = self.taps.get_mut(bus.0 as usize) {
            *tap = WebTap::default();
        }
    }
}

impl Analysis for WebAnalysis {
    fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    fn scope(&self, bus: BusId, left: &mut [f32], right: &mut [f32]) -> usize {
        let Some(tap) = self.taps.get(bus.0 as usize) else {
            return 0;
        };
        let n = left.len().min(right.len()).min(tap.left.len());
        let from = tap.left.len() - n;
        left[..n].copy_from_slice(&tap.left[from..]);
        right[..n].copy_from_slice(&tap.right[from..]);
        n
    }

    fn scope_position(&self, bus: BusId) -> u64 {
        self.taps.get(bus.0 as usize).map_or(0, |t| t.position)
    }

    fn levels(&self, bus: BusId) -> Option<BusMeter> {
        let cell = self
            .meters
            .get(bus.0 as usize * METER_FIELDS..(bus.0 as usize + 1) * METER_FIELDS)?;
        (cell[0] == 1.0).then(|| BusMeter {
            peak: [cell[1], cell[2]],
            rms: [cell[3], cell[4]],
        })
    }
}

/// A real FFT turned into the bars of a spectrum display.
///
/// `Spectrum::new(2048)` keeps its tables; [`log_bins`](Self::log_bins) takes
/// the latest `fft_size` frames of both channels, sums them to mono, windows
/// them with a Hann window, transforms and fills a slice of bars spaced
/// logarithmically between two frequencies.
#[derive(Debug)]
pub struct Spectrum {
    size: usize,
    window: Vec<f32>,
    /// Sum of the window, to scale a sine of amplitude A to magnitude A.
    window_sum: f32,
    cos: Vec<f32>,
    sin: Vec<f32>,
    reverse: Vec<u32>,
    re: Vec<f32>,
    im: Vec<f32>,
    /// Amplitude per FFT bin 0..=size/2 of the last call.
    magnitude: Vec<f32>,
}

impl Spectrum {
    /// A spectrum of `fft_size` frames, rounded up to a power of two (at least
    /// 16). Make it once; `log_bins` does not allocate.
    pub fn new(fft_size: usize) -> Spectrum {
        let size = fft_size.next_power_of_two().max(16);
        let window: Vec<f32> = (0..size)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos())
            .collect();
        let window_sum = window.iter().sum();
        let bits = size.trailing_zeros();
        Spectrum {
            size,
            window,
            window_sum,
            cos: (0..size / 2)
                .map(|k| (std::f32::consts::TAU * k as f32 / size as f32).cos())
                .collect(),
            sin: (0..size / 2)
                .map(|k| -(std::f32::consts::TAU * k as f32 / size as f32).sin())
                .collect(),
            reverse: (0..size as u32)
                .map(|i| i.reverse_bits() >> (32 - bits))
                .collect(),
            re: vec![0.0; size],
            im: vec![0.0; size],
            magnitude: vec![0.0; size / 2 + 1],
        }
    }

    /// The frames the transform takes: ask the scope for this many.
    pub fn fft_size(&self) -> usize {
        self.size
    }

    /// Transforms the latest `fft_size` frames of `left` + `right` (the end of
    /// each slice; a shorter one is padded with silence in front) at sample rate
    /// `rate`, and fills `bins` with bars from `min_hz` to `max_hz`, spaced
    /// logarithmically, each 0 to 1: the level in dBFS of the bar's loudest
    /// frequency, with `floor_db` (say -90) at 0 and full scale (a sine of
    /// amplitude 1) at 1. Silence is all 0. A bar narrower than an FFT bin
    /// reads between its two nearest bins, so a low-frequency end never has
    /// holes.
    pub fn log_bins(
        &mut self,
        left: &[f32],
        right: &[f32],
        rate: f32,
        bins: &mut [f32],
        min_hz: f32,
        max_hz: f32,
        floor_db: f32,
    ) {
        self.transform(left, right);
        let n = bins.len();
        if n == 0 {
            return;
        }
        let nyquist = rate * 0.5;
        let min_hz = min_hz.clamp(1.0, nyquist * 0.99);
        let max_hz = max_hz.clamp(min_hz * 1.01, nyquist);
        let per_hz = self.size as f32 / rate;
        let ratio = (max_hz / min_hz).powf(1.0 / n as f32);
        let floor_db = floor_db.min(-1.0);
        let top = self.magnitude.len() - 1;
        let mut lo = min_hz;
        for bar in bins.iter_mut() {
            let hi = lo * ratio;
            let (k_lo, k_hi) = (lo * per_hz, hi * per_hz);
            let amplitude = if k_hi - k_lo < 1.0 {
                // Between two bins: read at the bar's centre.
                let centre = 0.5 * (k_lo + k_hi);
                let i = (centre.floor() as usize).min(top);
                let t = centre - i as f32;
                let a = self.magnitude[i];
                let b = self.magnitude[(i + 1).min(top)];
                a + (b - a) * t
            } else {
                let first = (k_lo.ceil() as usize).min(top);
                let last = (k_hi.floor() as usize).clamp(first, top);
                self.magnitude[first..=last]
                    .iter()
                    .fold(0.0f32, |m, &a| m.max(a))
            };
            let db = 20.0 * amplitude.max(1e-10).log10();
            *bar = ((db - floor_db) / -floor_db).clamp(0.0, 1.0);
            lo = hi;
        }
    }

    /// The amplitude of each FFT bin from the last `log_bins` (0 to
    /// `fft_size / 2`, bin k at k * rate / fft_size Hz).
    pub fn magnitudes(&self) -> &[f32] {
        &self.magnitude
    }

    /// Windows the mono sum of the frames' ends and transforms it.
    fn transform(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len());
        let take = n.min(self.size);
        let pad = self.size - take;
        for i in 0..self.size {
            let sample = if i < pad {
                0.0
            } else {
                let j = n - take + (i - pad);
                0.5 * (left[j] + right[j])
            };
            // Bit-reversed order for the in-place transform.
            self.re[self.reverse[i] as usize] = sample * self.window[i];
        }
        self.im.fill(0.0);
        // Iterative radix-2 decimation in time.
        let mut half = 1;
        while half < self.size {
            let step = self.size / (half * 2);
            for start in (0..self.size).step_by(half * 2) {
                for k in 0..half {
                    let (c, s) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = self.re[b] * c - self.im[b] * s;
                    let ti = self.re[b] * s + self.im[b] * c;
                    self.re[b] = self.re[a] - tr;
                    self.im[b] = self.im[a] - ti;
                    self.re[a] += tr;
                    self.im[a] += ti;
                }
            }
            half *= 2;
        }
        // A sine of amplitude A reads as A/2 * window_sum at its bin and as much
        // at the mirror bin: scale one side to A.
        let scale = 2.0 / self.window_sum;
        for (k, m) in self.magnitude.iter_mut().enumerate() {
            *m = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt() * scale;
        }
    }
}

/// Smooths bars over time: each moves toward its new value by the fraction
/// `rise` (when it goes up) or `fall` (when it goes down) of the way, 0 to 1
/// (1 follows at once, 0 not at all). A fast rise and a slow fall (0.6 and 0.12
/// at 60 Hz, say) makes a spectrum that does not flicker. `state` holds the
/// previous bars and is resized to `bins` (allocating the first time); `bins` is
/// replaced by the smoothed ones.
pub fn smooth_bins(bins: &mut [f32], state: &mut Vec<f32>, rise: f32, fall: f32) {
    if state.len() != bins.len() {
        state.clear();
        state.extend_from_slice(bins);
        return;
    }
    for (bar, prev) in bins.iter_mut().zip(state.iter_mut()) {
        let k = if *bar > *prev { rise } else { fall };
        *prev += (*bar - *prev) * k.clamp(0.0, 1.0);
        *bar = *prev;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn sine(hz: f32, amplitude: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| amplitude * (std::f32::consts::TAU * hz * i as f32 / RATE).sin())
            .collect()
    }

    /// The bar a frequency falls in, for `bins` bars from 20 Hz to 20 kHz.
    fn bar_of(hz: f32, bins: usize) -> usize {
        ((hz / 20.0).ln() / (1000f32).ln() * bins as f32).floor() as usize
    }

    #[test]
    fn a_sine_lands_in_its_bar_at_its_level() {
        let mut spectrum = Spectrum::new(2048);
        assert_eq!(spectrum.fft_size(), 2048);
        for hz in [110.0, 440.0, 1_000.0, 5_000.0, 12_000.0] {
            let tone = sine(hz, 0.5, 2048);
            let mut bins = [0.0; 96];
            spectrum.log_bins(&tone, &tone, RATE, &mut bins, 20.0, 20_000.0, -90.0);
            let loudest = bins
                .iter()
                .enumerate()
                .fold((0, 0.0f32), |a, (i, &v)| if v > a.1 { (i, v) } else { a })
                .0;
            let expected = bar_of(hz, 96);
            assert!(
                loudest.abs_diff(expected) <= 1,
                "{hz} Hz is in bar {expected}, found {loudest}"
            );
            // 0.5 is -6 dBFS: (-6 + 90) / 90. The Hann window's scalloping loses a
            // little for a tone between bins.
            let level = bins[loudest];
            assert!((level - 84.0 / 90.0).abs() < 0.06, "{hz} Hz: {level}");
            // Far from the tone the bars are low.
            let far = bins[(loudest + 48) % 96];
            assert!(far < 0.5, "{far}");
        }
    }

    #[test]
    fn the_magnitude_of_a_bin_centred_sine_is_its_amplitude() {
        let mut spectrum = Spectrum::new(1024);
        let hz = 100.0 * RATE / 1024.0; // exactly bin 100
        let tone = sine(hz, 0.25, 1024);
        let mut bins = [0.0; 8];
        spectrum.log_bins(&tone, &tone, RATE, &mut bins, 100.0, 10_000.0, -90.0);
        let m = spectrum.magnitudes();
        assert!((m[100] - 0.25).abs() < 0.002, "{}", m[100]);
        assert!(m[100] > 20.0 * m[300]);
    }

    #[test]
    fn silence_is_zero_and_a_full_scale_sine_is_one() {
        let mut spectrum = Spectrum::new(2048);
        let silent = vec![0.0; 2048];
        let mut bins = [1.0; 64];
        spectrum.log_bins(&silent, &silent, RATE, &mut bins, 20.0, 20_000.0, -90.0);
        assert!(bins.iter().all(|&b| b == 0.0));

        let hz = 300.0 * RATE / 2048.0;
        let loud = sine(hz, 1.0, 2048);
        spectrum.log_bins(&loud, &loud, RATE, &mut bins, 20.0, 20_000.0, -90.0);
        let peak = bins.iter().cloned().fold(0.0, f32::max);
        assert!((peak - 1.0).abs() < 0.01, "{peak}");
    }

    #[test]
    fn left_and_right_are_summed_and_short_input_is_padded() {
        let mut spectrum = Spectrum::new(512);
        let tone = sine(1_000.0, 0.8, 512);
        let silent = vec![0.0; 512];
        let (mut both, mut one) = ([0.0; 32], [0.0; 32]);
        spectrum.log_bins(&tone, &tone, RATE, &mut both, 20.0, 20_000.0, -90.0);
        spectrum.log_bins(&tone, &silent, RATE, &mut one, 20.0, 20_000.0, -90.0);
        let (pb, po) = (
            both.iter().cloned().fold(0.0, f32::max),
            one.iter().cloned().fold(0.0, f32::max),
        );
        // One channel is half the mono sum: 6 dB, or 6/90 lower.
        assert!((pb - po - 6.02 / 90.0).abs() < 0.01, "{pb} vs {po}");
        // Fewer frames than the transform: no panic, the tone is still there.
        let mut short = [0.0; 32];
        spectrum.log_bins(
            &tone[..100],
            &tone[..100],
            RATE,
            &mut short,
            20.0,
            20_000.0,
            -90.0,
        );
        assert!(short.iter().cloned().fold(0.0, f32::max) > 0.3);
        // No frames at all: silence.
        spectrum.log_bins(&[], &[], RATE, &mut short, 20.0, 20_000.0, -90.0);
        assert!(short.iter().all(|&b| b == 0.0));
    }

    #[test]
    fn two_tones_make_two_peaks_and_a_low_end_has_no_holes() {
        let mut spectrum = Spectrum::new(4096);
        let a = sine(150.0, 0.4, 4096);
        let b = sine(3_000.0, 0.4, 4096);
        let both: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let mut bins = [0.0; 128];
        spectrum.log_bins(&both, &both, RATE, &mut bins, 20.0, 20_000.0, -90.0);
        let (pa, pb) = (bar_of(150.0, 128), bar_of(3_000.0, 128));
        assert!(bins[pa - 1..=pa + 1].iter().any(|&v| v > 0.8));
        assert!(bins[pb - 1..=pb + 1].iter().any(|&v| v > 0.8));
        // Noise-free elsewhere: the low bars (20 Hz is narrower than an FFT bin)
        // read between bins, so they are not stuck at one value.
        assert!(bins[..4].iter().all(|&v| v < 0.4));
    }

    #[test]
    fn smoothing_rises_fast_and_falls_slowly() {
        let mut state = Vec::new();
        let mut bars = [0.0, 0.0, 1.0];
        smooth_bins(&mut bars, &mut state, 0.5, 0.1);
        assert_eq!(
            bars,
            [0.0, 0.0, 1.0],
            "the first call takes the bars as they are"
        );
        let mut bars = [1.0, 0.0, 0.0];
        smooth_bins(&mut bars, &mut state, 0.5, 0.1);
        assert_eq!(bars, [0.5, 0.0, 0.9]);
        let mut bars = [1.0, 0.0, 0.0];
        smooth_bins(&mut bars, &mut state, 0.5, 0.1);
        assert_eq!(bars, [0.75, 0.0, 0.81]);
    }

    #[test]
    fn web_analysis_parses_a_taps_message_and_meters() {
        let mut web = WebAnalysis::new(44_100.0);
        assert_eq!(web.sample_rate(), 44_100.0);
        let (mut l, mut r) = ([0.0; 4], [0.0; 4]);
        assert_eq!(web.scope(BusId(0), &mut l, &mut r), 0);
        assert_eq!(web.levels(BusId(0)), None);

        // Two buses, 3 frames each: bus 0 then bus 2.
        let data = [
            1.0, 2.0, 3.0, -1.0, -2.0, -3.0, // bus 0: left, right
            4.0, 5.0, 6.0, -4.0, -5.0, -6.0, // bus 2
        ];
        web.update_taps(&[0, 2], &[300.0, 900.0], 3, &data);
        assert_eq!(web.scope(BusId(0), &mut l, &mut r), 3);
        assert_eq!(l[..3], [1.0, 2.0, 3.0]);
        assert_eq!(r[..3], [-1.0, -2.0, -3.0]);
        assert_eq!(web.scope_position(BusId(2)), 900);
        // A shorter read is the newest frames, like the native one.
        let (mut sl, mut sr) = ([0.0; 2], [0.0; 2]);
        assert_eq!(web.scope(BusId(2), &mut sl, &mut sr), 2);
        assert_eq!(sl, [5.0, 6.0]);
        assert_eq!(web.scope(BusId(1), &mut l, &mut r), 0, "no tap on bus 1");
        // A message that does not add up is ignored.
        web.update_taps(&[0], &[1.0], 3, &data[..4]);
        assert_eq!(web.scope_position(BusId(0)), 300);
        web.forget(BusId(0));
        assert_eq!(web.scope(BusId(0), &mut l, &mut r), 0);

        // Meters: five floats per bus.
        web.update_meters(&[1.0, 0.5, 0.4, 0.2, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let m = web.levels(BusId(0)).unwrap();
        assert_eq!((m.peak, m.rms), ([0.5, 0.4], [0.2, 0.1]));
        assert_eq!(web.levels(BusId(1)), None, "a bus that does not exist");
        assert_eq!(web.levels(BusId(7)), None);
    }
}
