//! A mixer playing on the default output device through cpal.
//!
//! The mixer lives in the device's callback. The game thread talks to it
//! through two lock-free rings: commands one way, events and released objects
//! the other. Meters, the frame clock and timing are published through
//! atomics, so reading them never waits on the audio thread.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

use crate::{BusId, BusMeter, Capture, Command, Event, Mixer, MixerConfig, Returned};

/// The callback renders at most this many frames between ring checks.
const CHUNK: usize = 1024;

/// Sizes for [`NativeHost::open`]; the sample rate is the device's.
#[derive(Clone, Copy, Debug)]
pub struct HostConfig {
    pub max_voices: usize,
    pub max_buses: usize,
    pub max_clips: usize,
    /// Commands that can wait for the next callback.
    pub command_capacity: usize,
    /// Events and released objects that can wait for the next `poll`.
    pub return_capacity: usize,
}

impl Default for HostConfig {
    fn default() -> Self {
        let mixer = MixerConfig::default();
        Self {
            max_voices: mixer.max_voices,
            max_buses: mixer.max_buses,
            max_clips: mixer.max_clips,
            command_capacity: 4096,
            return_capacity: 4096,
        }
    }
}

/// The host's view of the audio thread.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HostStatus {
    pub sample_rate: u32,
    /// Frames rendered so far.
    pub frame: u64,
    pub voices: u32,
    /// The last callback's frames and how long it took.
    pub callback_frames: u32,
    pub callback_us: f32,
    /// The slowest callback since the previous `status` call.
    pub callback_max_us: f32,
    /// The last callback's time as a share of the audio it rendered (1 = no
    /// time to spare).
    pub load: f32,
    pub rejected_commands: u64,
    pub dropped_events: u64,
    /// Objects freed on the audio thread because a queue was full.
    pub freed_on_audio_thread: u64,
    /// Stream errors that did not stop the output (a reroute, denied
    /// realtime scheduling).
    pub warnings: u64,
    /// The device went away or the stream broke; the host must be reopened.
    pub failed: bool,
}

enum Back {
    Event(Event),
    Returned(Returned),
}

struct Shared {
    frame: AtomicU64,
    voices: AtomicU32,
    /// Per bus slot: exists (0/1), peak left, peak right, RMS left, RMS right
    /// (f32 bits).
    meters: Box<[AtomicU32]>,
    callback_frames: AtomicU32,
    callback_ns: AtomicU64,
    callback_max_ns: AtomicU64,
    rejected_commands: AtomicU64,
    dropped_events: AtomicU64,
    freed_on_audio_thread: AtomicU64,
    warnings: AtomicU64,
    failed: AtomicBool,
}

const METER_FIELDS: usize = 5;

impl Shared {
    fn new(max_buses: usize) -> Shared {
        Shared {
            frame: AtomicU64::new(0),
            voices: AtomicU32::new(0),
            meters: (0..max_buses * METER_FIELDS)
                .map(|_| AtomicU32::new(0))
                .collect(),
            callback_frames: AtomicU32::new(0),
            callback_ns: AtomicU64::new(0),
            callback_max_ns: AtomicU64::new(0),
            rejected_commands: AtomicU64::new(0),
            dropped_events: AtomicU64::new(0),
            freed_on_audio_thread: AtomicU64::new(0),
            warnings: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        }
    }
}

/// What the device callback owns.
struct Callback {
    mixer: Mixer,
    commands: rtrb::Consumer<Command>,
    back: rtrb::Producer<Back>,
    shared: Arc<Shared>,
    left: Vec<f32>,
    right: Vec<f32>,
    channels: usize,
    /// Released objects the full return ring could not take, kept for the
    /// next callback instead of being freed here.
    lost: u64,
}

impl Callback {
    fn fill<T: SizedSample + FromSample<f32>>(&mut self, data: &mut [T]) {
        let started = Instant::now();
        while let Ok(command) = self.commands.pop() {
            self.mixer.apply(command);
        }
        let channels = self.channels.max(1);
        let frames = data.len() / channels;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(CHUNK);
            self.mixer
                .process(&mut self.left[..n], &mut self.right[..n]);
            let out = &mut data[done * channels..(done + n) * channels];
            for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
                let (l, r) = (self.left[i], self.right[i]);
                if channels == 1 {
                    frame[0] = T::from_sample(0.5 * (l + r));
                    continue;
                }
                frame[0] = T::from_sample(l);
                frame[1] = T::from_sample(r);
                for extra in &mut frame[2..] {
                    *extra = T::from_sample(0.0);
                }
            }
            done += n;
        }

        let back = &mut self.back;
        let mut lost = 0;
        self.mixer.drain_events(|event| {
            if back.push(Back::Event(event)).is_err() {
                lost += 1;
            }
        });
        let mut freed = 0;
        self.mixer.take_returned(|item| {
            // A full ring hands the item back; it is then freed here, which is
            // what the ring is sized to avoid.
            if back.push(Back::Returned(item)).is_err() {
                freed += 1;
            }
        });
        self.lost += lost;
        self.publish(started, frames, freed);
    }

    fn publish(&self, started: Instant, frames: usize, freed: u64) {
        let shared = &self.shared;
        let stats = self.mixer.stats();
        shared.frame.store(self.mixer.frame(), Ordering::Relaxed);
        shared.voices.store(stats.voices, Ordering::Relaxed);
        for (i, cell) in shared
            .meters
            .as_chunks::<METER_FIELDS>()
            .0
            .iter()
            .enumerate()
        {
            let meter = self.mixer.meter(BusId(i as u16));
            let m = meter.unwrap_or_default();
            cell[0].store(meter.is_some() as u32, Ordering::Relaxed);
            cell[1].store(m.peak[0].to_bits(), Ordering::Relaxed);
            cell[2].store(m.peak[1].to_bits(), Ordering::Relaxed);
            cell[3].store(m.rms[0].to_bits(), Ordering::Relaxed);
            cell[4].store(m.rms[1].to_bits(), Ordering::Relaxed);
        }
        shared
            .rejected_commands
            .store(stats.rejected_commands, Ordering::Relaxed);
        shared
            .dropped_events
            .store(stats.dropped_events + self.lost, Ordering::Relaxed);
        shared
            .freed_on_audio_thread
            .fetch_add(freed, Ordering::Relaxed);
        let elapsed = started.elapsed().as_nanos() as u64;
        shared
            .callback_frames
            .store(frames as u32, Ordering::Relaxed);
        shared.callback_ns.store(elapsed, Ordering::Relaxed);
        shared.callback_max_ns.fetch_max(elapsed, Ordering::Relaxed);
    }
}

/// A mixer playing on the default output device.
pub struct NativeHost {
    _stream: cpal::Stream,
    commands: rtrb::Producer<Command>,
    back: rtrb::Consumer<Back>,
    shared: Arc<Shared>,
    sample_rate: u32,
    device_name: String,
}

impl NativeHost {
    /// Opens the default output device at its own sample rate and starts a
    /// mixer on it with only the master bus.
    pub fn open(config: HostConfig) -> Result<NativeHost, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no audio output device")?;
        let device_name = device
            .description()
            .map(|d| d.to_string())
            .unwrap_or_else(|_| "unknown device".to_string());
        let supported = device
            .default_output_config()
            .map_err(|e| format!("cannot query {device_name}: {e}"))?;
        let sample_rate = supported.sample_rate();
        let channels = supported.channels();
        let stream_config = cpal::StreamConfig {
            channels,
            sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };

        let mixer = Mixer::new(MixerConfig {
            sample_rate: sample_rate as f32,
            max_voices: config.max_voices,
            max_buses: config.max_buses,
            max_clips: config.max_clips,
            ..MixerConfig::default()
        });
        let max_buses = mixer.config().max_buses;
        let (commands, commands_rx) = rtrb::RingBuffer::new(config.command_capacity.max(1));
        let (back_tx, back) = rtrb::RingBuffer::new(config.return_capacity.max(1));
        let shared = Arc::new(Shared::new(max_buses));
        let mut callback = Callback {
            mixer,
            commands: commands_rx,
            back: back_tx,
            shared: shared.clone(),
            left: vec![0.0; CHUNK],
            right: vec![0.0; CHUNK],
            channels: channels as usize,
            lost: 0,
        };
        let errors = shared.clone();
        let on_error = move |error: cpal::Error| match error.kind() {
            cpal::ErrorKind::DeviceNotAvailable
            | cpal::ErrorKind::HostUnavailable
            | cpal::ErrorKind::StreamInvalidated => errors.failed.store(true, Ordering::Relaxed),
            _ => {
                errors.warnings.fetch_add(1, Ordering::Relaxed);
            }
        };

        let format = supported.sample_format();
        let stream = match format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| callback.fill(data),
                on_error,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                stream_config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| callback.fill(data),
                on_error,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_output_stream(
                stream_config,
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| callback.fill(data),
                on_error,
                None,
            ),
            cpal::SampleFormat::I32 => device.build_output_stream(
                stream_config,
                move |data: &mut [i32], _: &cpal::OutputCallbackInfo| callback.fill(data),
                on_error,
                None,
            ),
            other => {
                return Err(format!(
                    "{device_name} wants {other:?} samples, which are not supported"
                ))
            }
        }
        .map_err(|e| format!("cannot open {device_name}: {e}"))?;
        stream
            .play()
            .map_err(|e| format!("cannot start {device_name}: {e}"))?;

        Ok(NativeHost {
            _stream: stream,
            commands,
            back,
            shared,
            sample_rate,
            device_name,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Queues a command for the next callback; gives it back if the queue is
    /// full.
    // A command is large (a sound effect travels as plain data, so the audio
    // thread never frees it), and it comes back by value to be retried.
    #[allow(clippy::result_large_err)]
    pub fn send(&mut self, command: Command) -> Result<(), Command> {
        self.commands.push(command).map_err(|e| match e {
            rtrb::PushError::Full(command) => command,
        })
    }

    /// Hands over the events since the last call, frees what the mixer let
    /// go of, and passes finished recordings to `on_capture`.
    pub fn poll(
        &mut self,
        mut on_event: impl FnMut(Event),
        mut on_capture: impl FnMut(Box<Capture>),
    ) {
        while let Ok(item) = self.back.pop() {
            match item {
                Back::Event(event) => on_event(event),
                Back::Returned(Returned::Capture(capture)) => on_capture(capture),
                Back::Returned(other) => drop(other),
            }
        }
    }

    /// Frames rendered so far.
    pub fn frame(&self) -> u64 {
        self.shared.frame.load(Ordering::Relaxed)
    }

    /// The bus's latest meter, if it exists.
    pub fn meter(&self, bus: BusId) -> Option<BusMeter> {
        let start = bus.0 as usize * METER_FIELDS;
        let cell = self.shared.meters.get(start..start + METER_FIELDS)?;
        let f = |i: usize| f32::from_bits(cell[i].load(Ordering::Relaxed));
        (cell[0].load(Ordering::Relaxed) == 1).then(|| BusMeter {
            peak: [f(1), f(2)],
            rms: [f(3), f(4)],
        })
    }

    /// Timing and counters; resets the slowest-callback figure.
    pub fn status(&self) -> HostStatus {
        let s = &self.shared;
        let frames = s.callback_frames.load(Ordering::Relaxed);
        let ns = s.callback_ns.load(Ordering::Relaxed);
        let budget_ns = frames as f64 / self.sample_rate as f64 * 1e9;
        HostStatus {
            sample_rate: self.sample_rate,
            frame: s.frame.load(Ordering::Relaxed),
            voices: s.voices.load(Ordering::Relaxed),
            callback_frames: frames,
            callback_us: ns as f32 / 1000.0,
            callback_max_us: s.callback_max_ns.swap(0, Ordering::Relaxed) as f32 / 1000.0,
            load: if budget_ns > 0.0 {
                (ns as f64 / budget_ns) as f32
            } else {
                0.0
            },
            rejected_commands: s.rejected_commands.load(Ordering::Relaxed),
            dropped_events: s.dropped_events.load(Ordering::Relaxed),
            freed_on_audio_thread: s.freed_on_audio_thread.load(Ordering::Relaxed),
            warnings: s.warnings.load(Ordering::Relaxed),
            failed: s.failed.load(Ordering::Relaxed),
        }
    }

    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }
}
