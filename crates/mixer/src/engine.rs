//! A vehicle's sound synthesised from its engine's state, as physically informed engine
//! synthesis does it (Baldan et al., "Physically informed car engine sound synthesis",
//! 2015): no recordings, so the sound follows the revs continuously and changes with the
//! load the way a real engine's does.
//!
//! - **Combustion**: each cylinder fires at its crank angle (the firing order: even for an
//!   inline four, uneven for a cross-plane V8 or a boxer's unequal headers). A firing sends a
//!   pressure pulse into the exhaust: a sharp blowdown spike as the valve opens and a broader
//!   hump as the piston pushes the rest out, both measured in crank angle, so the pulses get
//!   shorter (brighter) as the engine revs. The throttle sets how hard it fires; no two
//!   firings are alike (cycle-to-cycle variation, more at idle), and on a closed throttle at
//!   speed the engine barely fires and can pop and crackle.
//! - **Exhaust**: the pulses pass a collector resonance, then the exhaust pipe as a
//!   waveguide (its open end reflects them back, inverted and dulled, giving the pipe's
//!   quarter-wave drone at fixed frequencies the firing harmonics sweep through), then the
//!   muffler (a low-pass).
//! - **Intake**: noise shaped by each cylinder's intake stroke through the intake's
//!   resonance, louder with throttle and revs.
//! - **Mechanics**: valve train clicks and the engine block's ring, and a starter motor's
//!   whine while it cranks.
//! - **Tyres and air**: tyre squeal (tonal, wandering noise) as the tyres slide, road
//!   rumble growing with the speed and wind with its square.
//!
//! [`EngineSound`] is the recipe (plain data: one command starts the voice, natively and
//! in the AudioWorklet alike); [`EngineControls`] are what the host sends every frame
//! (`Command::SetEngine`), smoothed per sample.

use std::f32::consts::{PI, TAU};

/// The most cylinders a recipe has.
pub const MAX_CYLINDERS: usize = 12;
/// The exhaust pipe's delay line (its round trip), in frames.
const PIPE_FRAMES: usize = 1024;
/// The speed of sound in hot exhaust gas (m/s).
const EXHAUST_SOUND_SPEED: f32 = 450.0;

/// An engine's sound: what it is made of (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineSound {
    /// 1..12.
    pub cylinders: u8,
    /// The crank angle (degrees) each cylinder fires at: in the 720° cycle of a four-stroke,
    /// the 360° of a two-stroke. Only the first `cylinders` count.
    pub firing: [f32; MAX_CYLINDERS],
    pub two_stroke: bool,
    /// How far apart (ms) the cylinders' pulses reach the collector at most: unequal
    /// header lengths, a boxer's burble. 0 for equal headers.
    pub header_spread: f32,
    /// The exhaust pipe's length (m): where its drone sits (a quarter wave: 2 m drones at
    /// about 56 Hz and its odd multiples).
    pub pipe_length: f32,
    /// How strongly the pipe resonates (0..0.95).
    pub resonance: f32,
    /// 0 an open pipe (bright, raw) .. 1 a heavily muffled one (dull, quiet).
    pub muffler: f32,
    /// The parts' levels (1 is the recipe's balance).
    pub exhaust: f32,
    pub intake: f32,
    pub mechanical: f32,
    /// Cycle-to-cycle variation of the combustion (0..1): the lumpiness of an idle.
    pub roughness: f32,
    /// Pops and crackles on a closed throttle at speed (0..1).
    pub crackle: f32,
    /// Tyre squeal, road rumble and wind (0 leaves each out).
    pub tyres: f32,
    pub road: f32,
    pub wind: f32,
    pub seed: u64,
    /// Linear gain of the whole.
    pub level: f32,
}

impl EngineSound {
    /// Firing angles evenly spaced over the cycle (inline fours and sixes, V12s).
    pub fn even(cylinders: u8, two_stroke: bool) -> [f32; MAX_CYLINDERS] {
        let n = cylinders.clamp(1, MAX_CYLINDERS as u8) as usize;
        let cycle = if two_stroke { 360.0 } else { 720.0 };
        let mut firing = [0.0; MAX_CYLINDERS];
        for (i, angle) in firing.iter_mut().take(n).enumerate() {
            *angle = cycle * i as f32 / n as f32;
        }
        firing
    }
}

impl Default for EngineSound {
    /// A four-cylinder petrol car with a stock exhaust.
    fn default() -> Self {
        Self {
            cylinders: 4,
            firing: Self::even(4, false),
            two_stroke: false,
            header_spread: 0.0,
            pipe_length: 2.6,
            resonance: 0.45,
            muffler: 0.55,
            exhaust: 1.0,
            intake: 1.0,
            mechanical: 1.0,
            roughness: 0.3,
            crackle: 0.15,
            tyres: 1.0,
            road: 1.0,
            wind: 1.0,
            seed: 1,
            level: 1.0,
        }
    }
}

/// What the engine and the vehicle are doing: sent every frame, smoothed per sample.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct EngineControls {
    /// Crankshaft revolutions per minute (0: stopped).
    pub rpm: f32,
    /// The throttle the engine gets, 0..1 (the idle control's included).
    pub throttle: f32,
    /// The engine's torque as a share of its peak: positive driving, negative braking.
    pub load: f32,
    /// The vehicle's speed (m/s): road and wind noise.
    pub speed: f32,
    /// How far past their grip the tyres slide, 0..1+: squeal.
    pub skid: f32,
}

/// A biquad filter (RBJ cookbook), direct form I.
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn set(&mut self, b: [f32; 3], a: [f32; 3]) {
        let inv = 1.0 / a[0];
        self.b0 = b[0] * inv;
        self.b1 = b[1] * inv;
        self.b2 = b[2] * inv;
        self.a1 = a[1] * inv;
        self.a2 = a[2] * inv;
    }

    fn omega(hz: f32, rate: f32) -> (f32, f32) {
        let w = TAU * (hz / rate).clamp(1e-5, 0.49);
        (w.cos(), w.sin())
    }

    fn lowpass(&mut self, hz: f32, q: f32, rate: f32) {
        let (cos, sin) = Self::omega(hz, rate);
        let alpha = sin / (2.0 * q);
        self.set(
            [(1.0 - cos) * 0.5, 1.0 - cos, (1.0 - cos) * 0.5],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        );
    }

    fn highpass(&mut self, hz: f32, q: f32, rate: f32) {
        let (cos, sin) = Self::omega(hz, rate);
        let alpha = sin / (2.0 * q);
        self.set(
            [(1.0 + cos) * 0.5, -(1.0 + cos), (1.0 + cos) * 0.5],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        );
    }

    /// Band-pass with a peak gain of 1.
    fn bandpass(&mut self, hz: f32, q: f32, rate: f32) {
        let (cos, sin) = Self::omega(hz, rate);
        let alpha = sin / (2.0 * q);
        self.set([alpha, 0.0, -alpha], [1.0 + alpha, -2.0 * cos, 1.0 - alpha]);
    }

    /// A peak of `db` at `hz`.
    fn peaking(&mut self, hz: f32, q: f32, db: f32, rate: f32) {
        let (cos, sin) = Self::omega(hz, rate);
        let alpha = sin / (2.0 * q);
        let a = 10f32.powf(db / 40.0);
        self.set(
            [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
            [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
        );
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        // Flush denormals in long silences.
        self.y1 = if y.abs() < 1e-20 { 0.0 } else { y };
        self.y1
    }
}

/// One cylinder's exhaust pulse as it plays.
#[derive(Clone, Copy, Debug, Default)]
struct Pulse {
    /// 0..1 across the pulse; negative when none plays.
    position: f32,
    amplitude: f32,
    /// The pulse's length as a share of the cycle.
    width: f32,
    /// How much noise rides on it (a pop's crackle).
    grit: f32,
}

/// A playing engine sound.
pub(crate) struct EngineVoice {
    sound: EngineSound,
    rate: f32,
    target: EngineControls,
    rpm: f32,
    throttle: f32,
    load: f32,
    speed: f32,
    skid: f32,
    /// Where the crank is in its cycle, 0..1.
    phase: f32,
    cylinders: usize,
    /// Each cylinder's firing point in the cycle (0..1) and its header delay (s).
    firing: [f32; MAX_CYLINDERS],
    header: [f32; MAX_CYLINDERS],
    /// Each cylinder's lasting difference in strength (a real engine's imbalance).
    imbalance: [f32; MAX_CYLINDERS],
    pulses: [Pulse; MAX_CYLINDERS],
    random: u64,
    /// The radiated pressure: the flow's rate of change below about 250 Hz (a leaky
    /// differentiator, which also drops the flow's DC part): input and output last frame.
    radiate: (f32, f32),
    radiate_coeff: f32,
    /// The pulses' low body (the tailpipe's slow pressure swings: an idle's blub), kept
    /// apart from the differentiated part and high-passed of its DC.
    body: Biquad,
    body_dc: Biquad,
    /// Combustion roar: noise riding the firings, through the block.
    roar: Biquad,
    /// Each cylinder's next firing jitter (share of the cycle).
    jitter: [f32; MAX_CYLINDERS],
    collector: Biquad,
    pipe: [f32; PIPE_FRAMES],
    pipe_at: usize,
    pipe_delay: usize,
    pipe_damp: f32,
    pipe_damp_coeff: f32,
    muffler: [Biquad; 2],
    intake: Biquad,
    intake_low: Biquad,
    click: f32,
    click_decay: f32,
    valves: Biquad,
    block: [Biquad; 2],
    starter_phase: f32,
    squeal: [Biquad; 2],
    squeal_drift: f32,
    scrub: Biquad,
    road: Biquad,
    tread: Biquad,
    wind: Biquad,
    /// Smoothing coefficients per frame: fast (revs), slow (the rest).
    fast: f32,
    slow: f32,
    /// Afterfire: pops that come a moment after their firing.
    afterfire: [(u32, f32); 4],
}

/// A 64-bit mix (splitmix64) for seeds.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl EngineVoice {
    pub(crate) fn new(sound: EngineSound, rate: f32) -> EngineVoice {
        let rate = rate.max(8000.0);
        let cylinders = sound.cylinders.clamp(1, MAX_CYLINDERS as u8) as usize;
        let cycle = if sound.two_stroke { 360.0 } else { 720.0 };
        let mut firing = [0.0; MAX_CYLINDERS];
        let mut header = [0.0; MAX_CYLINDERS];
        let mut imbalance = [0.0; MAX_CYLINDERS];
        let mut seed = mix(sound.seed);
        for i in 0..cylinders {
            firing[i] = (sound.firing[i] / cycle).rem_euclid(1.0);
            seed = mix(seed);
            header[i] =
                sound.header_spread.max(0.0) * 1e-3 * ((seed >> 40) as f32 / (1u64 << 24) as f32);
            seed = mix(seed);
            imbalance[i] = ((seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.08;
        }
        let pipe_delay = ((2.0 * sound.pipe_length.max(0.1) / EXHAUST_SOUND_SPEED) * rate)
            .round()
            .clamp(8.0, (PIPE_FRAMES - 1) as f32) as usize;
        let per_frame = |seconds: f32| 1.0 - (-1.0 / (seconds * rate)).exp();
        let mut voice = EngineVoice {
            sound,
            rate,
            target: EngineControls::default(),
            rpm: 0.0,
            throttle: 0.0,
            load: 0.0,
            speed: 0.0,
            skid: 0.0,
            phase: 0.0,
            cylinders,
            firing,
            header,
            imbalance,
            pulses: [Pulse {
                position: -1.0,
                ..Pulse::default()
            }; MAX_CYLINDERS],
            random: mix(seed ^ 0x5EED),
            radiate: (0.0, 0.0),
            body: Biquad::default(),
            body_dc: Biquad::default(),
            roar: Biquad::default(),
            jitter: [0.0; MAX_CYLINDERS],
            radiate_coeff: {
                let rc = 1.0 / (TAU * 250.0);
                rc / (rc + 1.0 / rate)
            },
            collector: Biquad::default(),
            pipe: [0.0; PIPE_FRAMES],
            pipe_at: 0,
            pipe_delay,
            pipe_damp: 0.0,
            pipe_damp_coeff: 1.0 - (-TAU * 1800.0 / rate).exp(),
            muffler: [Biquad::default(); 2],
            intake: Biquad::default(),
            intake_low: Biquad::default(),
            click: 0.0,
            click_decay: (-1.0 / (0.0004 * rate)).exp(),
            valves: Biquad::default(),
            block: [Biquad::default(); 2],
            starter_phase: 0.0,
            squeal: [Biquad::default(); 2],
            squeal_drift: 0.0,
            scrub: Biquad::default(),
            road: Biquad::default(),
            tread: Biquad::default(),
            wind: Biquad::default(),
            fast: per_frame(0.012),
            slow: per_frame(0.04),
            afterfire: [(0, 0.0); 4],
        };
        voice.collector.peaking(320.0, 1.2, 4.0, rate);
        voice.body.lowpass(140.0, 0.7, rate);
        voice.body_dc.highpass(22.0, 0.7, rate);
        voice.roar.bandpass(420.0, 0.9, rate);
        voice.intake_low.lowpass(260.0, 0.7, rate);
        voice.valves.bandpass(2600.0, 2.5, rate);
        voice.block[0].bandpass(900.0, 14.0, rate);
        voice.block[1].bandpass(1950.0, 18.0, rate);
        voice.scrub.bandpass(450.0, 1.2, rate);
        voice.road.lowpass(220.0, 0.8, rate);
        voice.tread.bandpass(780.0, 3.0, rate);
        voice.wind.highpass(600.0, 0.6, rate);
        voice
    }

    pub(crate) fn set_controls(&mut self, controls: EngineControls) {
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        self.target = EngineControls {
            rpm: finite(controls.rpm).clamp(0.0, 30_000.0),
            throttle: finite(controls.throttle).clamp(0.0, 1.0),
            load: finite(controls.load).clamp(-2.0, 2.0),
            speed: finite(controls.speed).abs().min(200.0),
            skid: finite(controls.skid).clamp(0.0, 4.0),
        };
    }

    /// A uniform random number in -1..1.
    #[inline]
    fn noise(&mut self) -> f32 {
        // xorshift64*.
        self.random ^= self.random >> 12;
        self.random ^= self.random << 25;
        self.random ^= self.random >> 27;
        let bits = self.random.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (bits >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }

    /// About normally distributed (the sum of three uniforms).
    #[inline]
    fn gauss(&mut self) -> f32 {
        (self.noise() + self.noise() + self.noise()) * 0.577
    }

    /// Light a cylinder: its pulse starts, as hard as the throttle makes it.
    fn fire(&mut self, cylinder: usize) {
        let rpm = self.rpm;
        if rpm < 40.0 {
            return;
        }
        let throttle = self.throttle;
        // Cranking: compression strokes only, no combustion yet.
        let cranking = rpm < 450.0 && throttle < 0.05;
        let overrun = ((rpm - 1300.0) / 1500.0).clamp(0.0, 1.0)
            * (1.0 - throttle / 0.06).clamp(0.0, 1.0)
            * (-self.load * 4.0 + 0.2).clamp(0.0, 1.0);
        let rough = self.sound.roughness.max(0.0) * (0.25 + 0.75 * (1.0 - throttle));
        // The blowdown is faster and harder at high revs.
        let pace = (rpm / 2000.0).sqrt().clamp(0.5, 2.0);
        let mut amplitude = (0.2 + 0.8 * throttle.powf(0.8))
            * pace
            * (1.0 + self.imbalance[cylinder])
            * (1.0 + rough * 0.35 * self.gauss()).max(0.05);
        let mut width = 0.24;
        let mut grit = 0.15 + 0.2 * throttle;
        if cranking {
            amplitude = 0.05 * (1.0 + 0.2 * self.gauss());
            width = 0.3;
            grit = 0.05;
        } else if overrun > 0.0 {
            amplitude *= 1.0 - overrun * 0.75;
            // Unburnt fuel lighting in the hot exhaust.
            let chance = self.sound.crackle.clamp(0.0, 1.0) * 0.35 * overrun;
            if (self.noise() * 0.5 + 0.5) < chance {
                let strength = 0.7 + 0.6 * (self.noise() * 0.5 + 0.5);
                if self.noise() > 0.0 {
                    amplitude = strength;
                    width = 0.1;
                    grit = 0.9;
                } else {
                    let delay = (0.004 + 0.03 * (self.noise() * 0.5 + 0.5)) * self.rate;
                    if let Some(slot) = self.afterfire.iter_mut().find(|(t, _)| *t == 0) {
                        *slot = (delay as u32 + 1, strength);
                    }
                }
            }
        }
        self.pulses[cylinder] = Pulse {
            position: 0.0,
            amplitude,
            width,
            grit,
        };
    }

    /// Writes the next `out.len()` frames. An engine sound never ends by itself.
    pub(crate) fn render(&mut self, out: &mut [f32]) -> bool {
        let rate = self.rate;
        let sound = self.sound;
        let cycle_rpm = if sound.two_stroke { 60.0 } else { 120.0 };
        // Filters that follow the revs and the slide, once a block. The muffler passes more
        // as the exhaust's flow grows.
        let rpm_now = self.target.rpm.max(self.rpm * 0.5);
        let open = 1.0 + 0.9 * (rpm_now / 6000.0).min(1.5) * (0.4 + 0.6 * self.target.throttle);
        let cutoff = (260.0 + 3400.0 * (1.0 - sound.muffler.clamp(0.0, 1.0)).powi(2)) * open;
        for stage in &mut self.muffler {
            stage.lowpass(cutoff, 0.6, rate);
        }
        self.intake.bandpass(220.0 + rpm_now * 0.12, 1.6, rate);
        let squeal_hz = 950.0 + 260.0 * self.squeal_drift + 120.0 * self.target.skid.min(2.0);
        self.squeal[0].bandpass(squeal_hz, 14.0, rate);
        self.squeal[1].bandpass(squeal_hz * 2.02, 18.0, rate);
        let exhaust_gain = sound.exhaust.max(0.0) * 0.75;
        let intake_gain = sound.intake.max(0.0) * 0.35;
        let mechanical_gain = sound.mechanical.max(0.0) * 0.25;
        let resonance = sound.resonance.clamp(0.0, 0.95);
        for sample in out.iter_mut() {
            // Smooth the controls toward what the host sent.
            let t = self.target;
            self.rpm += (t.rpm - self.rpm) * self.fast;
            self.throttle += (t.throttle - self.throttle) * self.fast;
            self.load += (t.load - self.load) * self.slow;
            self.speed += (t.speed - self.speed) * self.slow;
            self.skid += (t.skid - self.skid) * self.slow;
            self.squeal_drift = (self.squeal_drift + self.noise() * 0.0004).clamp(-1.0, 1.0);

            // The crank turns; cylinders fire as it passes their angle.
            let cycle_hz = self.rpm / cycle_rpm;
            let step = cycle_hz / rate;
            let before = self.phase;
            self.phase = (self.phase + step).rem_euclid(1.0);
            let wrapped = self.phase < before;
            let mut intake_open = 0.0;
            for i in 0..self.cylinders {
                let at =
                    (self.firing[i] + self.header[i] * cycle_hz + self.jitter[i]).rem_euclid(1.0);
                let crossed = if wrapped {
                    at > before || at <= self.phase
                } else {
                    at > before && at <= self.phase
                };
                if crossed && step > 0.0 {
                    self.fire(i);
                    // The next firing a little early or late: no engine fires like a clock.
                    let wobble =
                        self.sound.roughness.max(0.0) * (0.002 + 0.006 * (1.0 - self.throttle));
                    self.jitter[i] = self.gauss() * wobble;
                    // The valves closing a moment later click.
                    self.click = 1.0;
                }
                // The intake stroke: half a cycle after the firing, a quarter long.
                let since = (self.phase - self.firing[i]).rem_euclid(1.0);
                if (0.5..0.75).contains(&since) {
                    intake_open += (PI * (since - 0.5) * 4.0).sin();
                }
            }

            // The exhaust pulses' flow.
            let mut flow = 0.0;
            let mut spikes = 0.0;
            for i in 0..self.cylinders {
                let pulse = self.pulses[i];
                if pulse.position < 0.0 {
                    continue;
                }
                let x = pulse.position;
                let spike = (-x / 0.07).exp() * (1.0 - (-x / 0.008).exp());
                let hump = (PI * x).sin().powi(2) * 0.3;
                let grain = 1.0 + pulse.grit * self.noise();
                flow += pulse.amplitude * (spike + hump) * grain;
                spikes += pulse.amplitude * spike;
                let next = x + step / pulse.width.max(0.01);
                self.pulses[i].position = if next >= 1.0 { -1.0 } else { next };
            }
            for slot in &mut self.afterfire {
                if slot.0 > 0 {
                    slot.0 -= 1;
                    if slot.0 == 0 {
                        // Released into the first free pulse.
                        if let Some(free) = self.pulses[..self.cylinders]
                            .iter_mut()
                            .find(|p| p.position < 0.0)
                        {
                            *free = Pulse {
                                position: 0.0,
                                amplitude: slot.1,
                                width: 0.08,
                                grit: 1.0,
                            };
                        }
                    }
                }
            }

            // Exhaust: radiated pressure, collector, the pipe's waveguide, the muffler.
            let pressure = self.radiate_coeff * (self.radiate.1 + flow - self.radiate.0);
            self.radiate = (flow, pressure);
            let low = self.body.process(flow);
            let body = self.body_dc.process(low);
            let hiss = self.noise();
            let roar = self.roar.process(hiss) * spikes * (0.5 + self.throttle);
            let collected = self.collector.process(pressure + body * 0.9 + roar * 0.6);
            let read = (self.pipe_at + PIPE_FRAMES - self.pipe_delay) % PIPE_FRAMES;
            let returning = self.pipe[read];
            self.pipe_damp += (returning - self.pipe_damp) * self.pipe_damp_coeff;
            let travelling = collected - resonance * self.pipe_damp;
            self.pipe[self.pipe_at] = travelling;
            self.pipe_at = (self.pipe_at + 1) % PIPE_FRAMES;
            let first = self.muffler[0].process(travelling);
            let muffled = self.muffler[1].process(first);
            let exhaust = muffled * exhaust_gain;

            // Intake: noise through its resonance on each intake stroke, and the induction's
            // low pulse.
            let revs = (self.rpm / 6000.0).min(1.5);
            let breath = self.noise();
            let intake = (self.intake.process(breath) * intake_open * (0.2 + self.throttle)
                + self.intake_low.process(flow) * 0.6 * self.throttle)
                * (0.15 + revs)
                * intake_gain;

            // Mechanics: valve clicks and the block ringing under the firings.
            self.click *= self.click_decay;
            let rattle = self.noise() * self.click;
            let tick = self.valves.process(rattle);
            let ring = self.block[0].process(spikes) + self.block[1].process(spikes) * 0.6;
            let mut mechanical =
                (tick * (0.1 + revs) * 0.6 + ring * (0.3 + self.throttle) * 0.5) * mechanical_gain;
            // The starter motor while cranking.
            if self.rpm > 30.0 && self.rpm < 480.0 && self.throttle < 0.2 {
                let crank = ((self.rpm - 30.0) / 60.0).clamp(0.0, 1.0)
                    * ((480.0 - self.rpm) / 80.0).clamp(0.0, 1.0);
                let whine_hz = 420.0 * (self.rpm / 230.0).clamp(0.3, 2.0);
                self.starter_phase = (self.starter_phase + whine_hz / rate).rem_euclid(1.0);
                let saw = 2.0 * self.starter_phase - 1.0;
                let chug = 0.6 + 0.4 * (TAU * self.phase * self.cylinders as f32).sin();
                mechanical += (saw * 0.12 + self.noise() * 0.04) * crank * chug;
            }

            // Tyres, road and wind.
            let moving = (self.speed / 2.0).min(1.0);
            let slide = (self.skid / 1.0).clamp(0.0, 1.0);
            let slide = slide * slide * (3.0 - 2.0 * slide) * moving;
            let mut tyres = 0.0;
            if slide > 0.0 && sound.tyres > 0.0 {
                let n = self.noise();
                tyres = (self.squeal[0].process(n) * 3.0
                    + self.squeal[1].process(n) * 1.5
                    + self.scrub.process(n) * 0.4)
                    * slide
                    * sound.tyres
                    * 0.5;
            }
            let pace = self.speed / 30.0;
            let road = if sound.road > 0.0 && self.speed > 0.1 {
                let n = self.noise();
                (self.road.process(n) * 0.5 + self.tread.process(n) * 0.06)
                    * pace.powf(1.3).min(3.0)
                    * sound.road
                    * 0.35
            } else {
                0.0
            };
            let wind = if sound.wind > 0.0 && self.speed > 1.0 {
                let n = self.noise();
                self.wind.process(n) * (pace * pace).min(6.0) * sound.wind * 0.05
            } else {
                0.0
            };

            let raw = (exhaust + intake + mechanical + tyres + road + wind) * sound.level.max(0.0);
            // A soft clipper (a rational tanh): never past 1.
            let x = raw.clamp(-3.0, 3.0);
            *sample = x * (27.0 + x * x) / (27.0 + 9.0 * x * x);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn render(voice: &mut EngineVoice, seconds: f32) -> Vec<f32> {
        let mut out = vec![0.0; (seconds * RATE) as usize];
        for block in out.chunks_mut(128) {
            voice.render(block);
        }
        out
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
    }

    fn running(rpm: f32, throttle: f32) -> EngineVoice {
        let mut voice = EngineVoice::new(EngineSound::default(), RATE);
        voice.set_controls(EngineControls {
            rpm,
            throttle,
            load: throttle,
            ..Default::default()
        });
        // Let the smoothing reach the controls.
        render(&mut voice, 0.5);
        voice
    }

    /// The period (s) the signal repeats at most strongly between `min` and `max`
    /// seconds: its autocorrelation's highest peak.
    fn period(samples: &[f32], min: f32, max: f32) -> f32 {
        let (lo, hi) = ((min * RATE) as usize, (max * RATE) as usize);
        let n = samples.len() - hi;
        let mut best = (0, f32::MIN);
        for lag in lo..hi {
            let c: f32 = (0..n).map(|i| samples[i] * samples[i + lag]).sum();
            if c > best.1 {
                best = (lag, c);
            }
        }
        best.0 as f32 / RATE
    }

    #[test]
    fn a_stopped_engine_on_a_parked_car_is_silent() {
        let mut voice = EngineVoice::new(EngineSound::default(), RATE);
        let out = render(&mut voice, 0.5);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_four_cylinder_fires_twice_a_revolution() {
        // 3000 rpm: 50 revolutions a second, 100 firings: a 10 ms period.
        let mut voice = running(3000.0, 0.5);
        let out = render(&mut voice, 0.4);
        // The envelope's period (the firings), not the pipe's ring.
        let envelope: Vec<f32> = out.iter().map(|s| s.abs()).collect();
        let found = period(&envelope, 0.006, 0.016);
        assert!((found - 0.010).abs() < 0.0006, "period {found} s");
    }

    #[test]
    fn it_is_louder_with_throttle_and_revs_and_stays_bounded() {
        let idle = rms(&render(&mut running(800.0, 0.1), 1.0));
        let cruise = rms(&render(&mut running(2500.0, 0.3), 1.0));
        let full = rms(&render(&mut running(2500.0, 1.0), 1.0));
        let high = render(&mut running(6000.0, 1.0), 1.0);
        let screaming = rms(&high);
        assert!(idle > 0.01, "idle {idle}");
        assert!(
            full > cruise * 1.3,
            "full throttle {full} vs cruising {cruise}"
        );
        assert!(screaming > full, "6000 rpm {screaming} vs 2500 rpm {full}");
        let peak = high.iter().fold(0f32, |p, s| p.max(s.abs()));
        assert!(peak < 1.0, "peak {peak}");
        assert!(screaming < 0.5, "6000 rpm rms {screaming}");
    }

    #[test]
    fn tyres_squeal_road_and_wind_follow_the_vehicle() {
        let quiet = |controls: EngineControls| {
            let mut voice = EngineVoice::new(EngineSound::default(), RATE);
            voice.set_controls(controls);
            render(&mut voice, 0.5);
            rms(&render(&mut voice, 0.5))
        };
        let parked = quiet(EngineControls::default());
        let rolling = quiet(EngineControls {
            speed: 25.0,
            ..Default::default()
        });
        let sliding = quiet(EngineControls {
            speed: 25.0,
            skid: 1.0,
            ..Default::default()
        });
        assert!(parked == 0.0);
        assert!(rolling > 0.003, "road and wind at 90 km/h: {rolling}");
        assert!(
            sliding > rolling * 2.0,
            "squeal {sliding} vs rolling {rolling}"
        );
    }

    #[test]
    fn the_seed_and_the_rpm_sweep_render_finite_and_repeatably() {
        let sweep = |seed: u64| {
            let mut voice = EngineVoice::new(
                EngineSound {
                    seed,
                    ..EngineSound::default()
                },
                RATE,
            );
            let mut out = Vec::new();
            for step in 0..60 {
                let rpm = 300.0 + step as f32 * 110.0;
                voice.set_controls(EngineControls {
                    rpm,
                    throttle: if step % 20 < 14 { 1.0 } else { 0.0 },
                    load: if step % 20 < 14 { 0.9 } else { -0.3 },
                    speed: step as f32,
                    skid: 0.0,
                });
                out.extend(render(&mut voice, 0.05));
            }
            out
        };
        let a = sweep(3);
        assert!(a.iter().all(|s| s.is_finite() && s.abs() < 1.0));
        assert_eq!(a, sweep(3));
        assert_ne!(a, sweep(4));
    }
}
