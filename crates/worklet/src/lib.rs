//! cubed-mixer in a browser AudioWorklet.
//!
//! The module has raw `extern "C"` exports and no wasm-bindgen glue, so it
//! instantiates in an AudioWorkletGlobalScope (which has no `fetch` or
//! `TextDecoder`) with an empty import object. `js/cubed-worklet.js` is the
//! AudioWorkletProcessor that drives it:
//!
//! 1. `cw_init` once, with the context's sample rate and a seed from the page.
//! 2. Commands arrive as [`cubed_mixer::wire`] bytes: write them at
//!    `cw_inbox(len)` and call `cw_apply(len)`.
//! 3. Clips arrive as PCM: write it at `cw_clip_buffer(frames, channels)`
//!    (and `cw_clip_right()` for stereo), then `cw_add_clip`.
//! 4. Each render quantum, `cw_process(frames)` renders into `cw_left()` and
//!    `cw_right()`; events (`cw_events`, encoded with `wire::encode_event`),
//!    meters (`cw_meters`, 5 floats per bus: exists, peak L/R, RMS L/R) and
//!    counters are read afterwards.
//!
//! Only `cw_process` runs in the render callback, and it never allocates;
//! the others run in the port's message handler.

use std::cell::UnsafeCell;
use std::sync::Arc;

use cubed_mixer::{wire, BusId, Clip, ClipId, Command, Mixer, MixerConfig, Returned};

/// The Web Audio render quantum.
pub const BLOCK: usize = 128;
/// Floats per bus in `cw_meters`.
pub const METER_FIELDS: usize = 5;
/// Events kept between `cw_clear_events` calls.
const MAX_EVENTS: usize = 512;

struct State {
    mixer: Mixer,
    left: Vec<f32>,
    right: Vec<f32>,
    inbox: Vec<u8>,
    clip_left: Vec<f32>,
    clip_right: Vec<f32>,
    events: Vec<u8>,
    meters: Vec<f32>,
    /// Objects the mixer let go of during rendering, freed by the next
    /// message instead of in the render callback.
    released: Vec<Returned>,
    /// Command batches that did not decode fully.
    bad_batches: u32,
}

struct Global(UnsafeCell<Option<State>>);

// SAFETY: wasm32 without threads; the worklet calls in from one thread.
unsafe impl Sync for Global {}

static STATE: Global = Global(UnsafeCell::new(None));

fn state() -> &'static mut State {
    // SAFETY: single-threaded, and no export keeps the reference past its
    // return. Calling anything before `cw_init` traps.
    unsafe { (*STATE.0.get()).as_mut().expect("cw_init first") }
}

/// Starts a mixer with only the master bus at `sample_rate`, and seeds the
/// synth's random number generators.
#[no_mangle]
pub extern "C" fn cw_init(
    sample_rate: f32,
    max_voices: u32,
    max_buses: u32,
    max_clips: u32,
    seed_lo: u32,
    seed_hi: u32,
) {
    cubed_dsp::seed::set_seed((seed_hi as u64) << 32 | seed_lo as u64);
    let defaults = MixerConfig::default();
    let pick = |v: u32, default: usize| if v == 0 { default } else { v as usize };
    let mixer = Mixer::new(MixerConfig {
        sample_rate,
        max_block: BLOCK,
        max_voices: pick(max_voices, defaults.max_voices),
        max_buses: pick(max_buses, defaults.max_buses),
        max_clips: pick(max_clips, defaults.max_clips),
        ..defaults
    });
    let buses = mixer.config().max_buses;
    let released = mixer.config().max_returned;
    let state = State {
        mixer,
        left: vec![0.0; BLOCK],
        right: vec![0.0; BLOCK],
        inbox: Vec::new(),
        clip_left: Vec::new(),
        clip_right: Vec::new(),
        events: Vec::with_capacity(MAX_EVENTS * wire::EVENT_SIZE),
        meters: vec![0.0; buses * METER_FIELDS],
        released: Vec::with_capacity(released),
        bad_batches: 0,
    };
    // SAFETY: see `state`.
    unsafe { *STATE.0.get() = Some(state) };
}

/// Renders `frames` (at most 128) into `cw_left`/`cw_right`, then updates
/// the events and meters.
#[no_mangle]
pub extern "C" fn cw_process(frames: u32) {
    let State {
        mixer,
        left,
        right,
        events,
        meters,
        released,
        ..
    } = state();
    let n = (frames as usize).min(BLOCK);
    mixer.process(&mut left[..n], &mut right[..n]);
    left[n..].fill(0.0);
    right[n..].fill(0.0);
    mixer.drain_events(|event| {
        if events.len() + wire::EVENT_SIZE <= events.capacity() {
            wire::encode_event(event, events);
        }
    });
    mixer.take_returned(|item| {
        if released.len() < released.capacity() {
            released.push(item);
        }
    });
    for (i, cell) in meters
        .as_chunks_mut::<METER_FIELDS>()
        .0
        .iter_mut()
        .enumerate()
    {
        match mixer.meter(BusId(i as u16)) {
            Some(m) => cell.copy_from_slice(&[1.0, m.peak[0], m.peak[1], m.rms[0], m.rms[1]]),
            None => cell.fill(0.0),
        }
    }
}

#[no_mangle]
pub extern "C" fn cw_left() -> *const f32 {
    state().left.as_ptr()
}

#[no_mangle]
pub extern "C" fn cw_right() -> *const f32 {
    state().right.as_ptr()
}

/// Room for `len` command bytes; write them there, then call `cw_apply`.
#[no_mangle]
pub extern "C" fn cw_inbox(len: u32) -> *mut u8 {
    let inbox = &mut state().inbox;
    inbox.resize(len as usize, 0);
    inbox.as_mut_ptr()
}

/// Applies the first `len` bytes of the inbox. Returns how many commands
/// were applied; a batch that ends in bytes that do not decode is counted
/// in `cw_bad_batches`.
#[no_mangle]
pub extern "C" fn cw_apply(len: u32) -> u32 {
    let State {
        mixer,
        inbox,
        released,
        bad_batches,
        ..
    } = state();
    released.clear();
    let mut applied = 0;
    let len = (len as usize).min(inbox.len());
    for command in wire::decode(&inbox[..len]) {
        match command {
            Ok(command) => {
                mixer.apply(command);
                applied += 1;
            }
            Err(_) => *bad_batches += 1,
        }
    }
    applied
}

/// Room for a clip of `frames` frames: returns where its left (or only)
/// channel goes; a stereo clip's right channel goes at `cw_clip_right`.
#[no_mangle]
pub extern "C" fn cw_clip_buffer(frames: u32, channels: u32) -> *mut f32 {
    let s = state();
    s.clip_left.resize(frames as usize, 0.0);
    s.clip_right
        .resize(if channels >= 2 { frames as usize } else { 0 }, 0.0);
    s.clip_left.as_mut_ptr()
}

#[no_mangle]
pub extern "C" fn cw_clip_right() -> *mut f32 {
    state().clip_right.as_mut_ptr()
}

/// Makes the PCM written at `cw_clip_buffer` playable as clip `id`.
#[no_mangle]
pub extern "C" fn cw_add_clip(id: u32, sample_rate: f32, frames: u32, channels: u32) {
    let s = state();
    s.released.clear();
    let frames = (frames as usize).min(s.clip_left.len());
    let left = s.clip_left[..frames].to_vec();
    let clip = if channels >= 2 && s.clip_right.len() >= frames {
        Clip::stereo(sample_rate, left, s.clip_right[..frames].to_vec())
    } else {
        Clip::mono(sample_rate, left)
    };
    s.mixer.apply(Command::AddClip(ClipId(id), Arc::new(clip)));
}

/// The encoded events since `cw_clear_events`.
#[no_mangle]
pub extern "C" fn cw_events() -> *const u8 {
    state().events.as_ptr()
}

#[no_mangle]
pub extern "C" fn cw_events_len() -> u32 {
    state().events.len() as u32
}

#[no_mangle]
pub extern "C" fn cw_clear_events() {
    state().events.clear();
}

/// Per bus slot: exists (0/1), peak left, peak right, RMS left, RMS right.
#[no_mangle]
pub extern "C" fn cw_meters() -> *const f32 {
    state().meters.as_ptr()
}

/// Bus slots in `cw_meters`.
#[no_mangle]
pub extern "C" fn cw_meter_count() -> u32 {
    (state().meters.len() / METER_FIELDS) as u32
}

/// Frames rendered so far (exact up to 2^53).
#[no_mangle]
pub extern "C" fn cw_frame() -> f64 {
    state().mixer.frame() as f64
}

#[no_mangle]
pub extern "C" fn cw_voices() -> u32 {
    state().mixer.stats().voices
}

/// Commands the mixer refused (unknown ids, bad slots, routing loops).
#[no_mangle]
pub extern "C" fn cw_rejected() -> u32 {
    state().mixer.stats().rejected_commands as u32
}

#[no_mangle]
pub extern "C" fn cw_bad_batches() -> u32 {
    state().bad_batches
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubed_mixer::{BusSettings, Play, VoiceId};

    /// The exports driven the way the worklet does: init, a bus, a clip, a
    /// play, render, read events and meters.
    #[test]
    fn the_exports_play_a_clip() {
        cw_init(48_000.0, 0, 0, 0, 1, 2);
        let mut bytes = Vec::new();
        wire::encode(
            &Command::SetBus(BusId(1), BusSettings::default()),
            &mut bytes,
        );
        wire::encode(
            &Command::Play(Play::new(VoiceId(9), ClipId(3), BusId(1))),
            &mut bytes,
        );

        let frames = 200;
        let ptr = cw_clip_buffer(frames, 1);
        // SAFETY: cw_clip_buffer made room for `frames` floats.
        unsafe { std::slice::from_raw_parts_mut(ptr, frames as usize) }.fill(0.25);
        cw_add_clip(3, 48_000.0, frames, 1);

        let inbox = cw_inbox(bytes.len() as u32);
        // SAFETY: cw_inbox made room for the bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), inbox, bytes.len()) };
        assert_eq!(cw_apply(bytes.len() as u32), 2);

        cw_process(128);
        // SAFETY: the output buffers hold BLOCK floats.
        let left = unsafe { std::slice::from_raw_parts(cw_left(), BLOCK) };
        assert!(left.iter().all(|&s| s == 0.25));
        assert_eq!(cw_voices(), 1);
        let meters = unsafe { std::slice::from_raw_parts(cw_meters(), METER_FIELDS * 2) };
        assert_eq!(meters[METER_FIELDS], 1.0, "bus 1 exists");
        assert_eq!(meters[METER_FIELDS + 1], 0.25);

        cw_process(128);
        let events = unsafe { std::slice::from_raw_parts(cw_events(), cw_events_len() as usize) };
        let decoded: Vec<_> = wire::decode_events(events).collect();
        assert_eq!(decoded, [cubed_mixer::Event::Ended(VoiceId(9))]);
        cw_clear_events();
        assert_eq!(cw_events_len(), 0);
        assert_eq!(cw_frame(), 256.0);
        assert_eq!(cw_bad_batches(), 0);
    }
}
