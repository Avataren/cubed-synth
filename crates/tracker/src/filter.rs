//! The Amiga 500's output chain: a fixed RC low-pass, and the "LED" filter
//! E0x switches in (E00 on, E01 off), crossfaded over 5 ms.

use std::f64::consts::PI;

const STATIC_CUTOFF_HZ: f64 = 4900.0;
const LED_CUTOFF_HZ: f64 = 3275.0;
const LED_RESONANCE_DB: f64 = -0.7;
const CROSSFADE_SECONDS: f64 = 0.005;

#[derive(Clone, Copy, Debug, Default)]
struct OnePole {
    b0: f64,
    b1: f64,
    a1: f64,
    x1: f64,
    y1: f64,
}

impl OnePole {
    fn new(rate: f64, cutoff: f64) -> OnePole {
        let k = (PI * cutoff.min(rate * 0.49) / rate).tan();
        let b0 = k / (1.0 + k);
        OnePole {
            b0,
            b1: b0,
            a1: (k - 1.0) / (1.0 + k),
            ..Default::default()
        }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 - self.a1 * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    /// The LED filter: a 2-pole Butterworth-like low-pass by the bilinear
    /// transform (the DAW's `deriveLedCoefficients`).
    fn led(rate: f64) -> Biquad {
        let fs = rate;
        let res = 10f64.powf(-LED_RESONANCE_DB / 10.0 / 2.0);
        let wp = 2.0 * fs * (PI * LED_CUTOFF_HZ.min(fs * 0.49) / fs).tan();
        let (b1d, b2d) = (2f64.sqrt() * res / wp, 1.0 / (wp * wp));
        let bd = 4.0 * b2d * fs * fs + 2.0 * b1d * fs + 1.0;
        Biquad {
            b0: 1.0 / bd,
            b1: 2.0 / bd,
            b2: 1.0 / bd,
            a1: (2.0 - 8.0 * b2d * fs * fs) / bd,
            a2: (4.0 * b2d * fs * fs - 2.0 * b1d * fs + 1.0) / bd,
            ..Default::default()
        }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// The filter on one stereo output.
#[derive(Clone, Debug)]
pub(crate) struct AmigaFilter {
    rc: [OnePole; 2],
    led: [Biquad; 2],
    /// 0 with the LED filter out, 1 with it in.
    mix: f64,
    target: f64,
    step: f64,
}

impl AmigaFilter {
    pub(crate) fn new(rate: f64) -> AmigaFilter {
        AmigaFilter {
            rc: [OnePole::new(rate, STATIC_CUTOFF_HZ); 2],
            led: [Biquad::led(rate); 2],
            mix: 0.0,
            target: 0.0,
            step: 1.0 / (CROSSFADE_SECONDS * rate).max(1.0),
        }
    }

    pub(crate) fn set_led(&mut self, on: bool) {
        self.target = if on { 1.0 } else { 0.0 };
    }

    pub(crate) fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if self.mix != self.target {
                self.mix = if self.mix < self.target {
                    (self.mix + self.step).min(self.target)
                } else {
                    (self.mix - self.step).max(self.target)
                };
            }
            for (c, s) in [l, r].into_iter().enumerate() {
                let rc = self.rc[c].process(*s as f64);
                let led = self.led[c].process(rc);
                *s = (rc + (led - rc) * self.mix) as f32;
            }
        }
    }
}
