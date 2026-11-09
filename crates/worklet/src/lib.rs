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
//!    (and `cw_clip_right()` for stereo), then `cw_add_clip`. Songs arrive
//!    as files: write one at `cw_file_buffer(len)`, then `cw_prepare_song`
//!    makes it a prepared source for a `PlaySource` command; a synth patch
//!    (the DAW's JSON) the same way with `cw_prepare_synth`, or, to swap a
//!    rebuilt synth in for a playing one, `cw_prepare_synth_held` with the notes
//!    the old one holds. A synth is edited while it plays by `SourceEvent`
//!    commands (notes, and parameter edits as wire tag 29). Effects arrive
//!    as a kind and parameters: write them at `cw_effect_params()`, then
//!    `cw_set_effect`.
//!    A module (MOD, S3M, XM) under live control: `cw_prepare_module` opens it
//!    stopped and registers it, `SourceEvent::Song` commands (wire tag 30) steer
//!    it, and `cw_songs_prepare` (about 60 times a second) cuts the position,
//!    levels and channel scopes of the songs that moved for the page, which
//!    reads them with `cubed_mixer::WebSong`.
//!    Analysis taps: `cw_set_tap(bus, on)` gives a bus a tap that keeps its latest
//!    frames; `cw_taps_prepare(frames)` cuts them (about 40 times a second) into
//!    `cw_tap_buses`, `cw_tap_written` and `cw_tap_data` for the page, which
//!    reads them with `cubed_mixer::WebAnalysis`.
//! 4. Each render quantum, `cw_process(frames)` renders into `cw_left()` and
//!    `cw_right()`; events (`cw_events`, encoded with `wire::encode_event`),
//!    meters (`cw_meters`, 5 floats per bus: exists, peak L/R, RMS L/R) and
//!    counters are read afterwards.
//!
//! Only `cw_process` runs in the render callback, and it never allocates;
//! the others run in the port's message handler.

use std::cell::UnsafeCell;
use std::sync::Arc;

mod swap;

use cubed_mixer::effects::MAX_PARAMS;
use cubed_mixer::{
    wire, BusId, Clip, ClipId, Command, Effect, EffectKind, HeldNote, Mixer, MixerConfig, Returned,
    SongReader, SynthBuild, SynthSource, TapSet, VoiceId, SONG_LEVEL_SLOTS, SONG_SCOPE_FRAMES,
    SONG_STATUS_WORDS, TAP_FRAMES,
};

/// The Web Audio render quantum.
pub const BLOCK: usize = 128;
/// Floats per bus in `cw_meters`.
pub const METER_FIELDS: usize = 5;
/// Events kept between `cw_clear_events` calls.
const MAX_EVENTS: usize = 512;
/// The most frames of each tap `cw_taps_prepare` hands out.
pub const TAP_SEND_FRAMES: usize = 2048;

struct State {
    mixer: Mixer,
    left: Vec<f32>,
    right: Vec<f32>,
    inbox: Vec<u8>,
    clip_left: Vec<f32>,
    clip_right: Vec<f32>,
    /// A song file on its way to `cw_prepare_song`.
    file: Vec<u8>,
    /// Seconds into a PSID tune the next `cw_prepare_module` starts it (`cw_prepare_start`), then 0.
    prepare_start: f64,
    /// Held notes on their way to `cw_prepare_synth_held`: a note number and a
    /// velocity each.
    held: Vec<f32>,
    effect_params: [f32; MAX_PARAMS],
    /// Why the last `cw_prepare_song` failed, as ASCII.
    error: Vec<u8>,
    events: Vec<u8>,
    meters: Vec<f32>,
    /// Objects the mixer let go of during rendering, freed by the next
    /// message instead of in the render callback.
    released: Vec<Returned>,
    /// Command batches that did not decode fully.
    bad_batches: u32,
    /// The buses' taps the page switched on, and what `cw_taps_prepare` cut from
    /// them: the bus slots, how many frames each had seen, and their frames
    /// (each bus's left frames then its right).
    taps: TapSet,
    tap_buses: Vec<u32>,
    tap_written: Vec<f64>,
    tap_data: Vec<f32>,
    /// A swap's PlaySource command on its way to `cw_swap_begin`.
    swap_play: Vec<u8>,
    swaps: swap::Swaps,
    /// The voices of the last swap that finished, for `cw_swap_old`/`_new`.
    swapped: (f64, f64),
    /// The module songs the page follows, by source slot, and what
    /// `cw_songs_prepare` cut from them: each reported song's slot, status words
    /// and levels; and for those with scopes on, the slot, channel count, frames
    /// written per channel and the frames.
    songs: Vec<Option<SongSlot>>,
    song_sources: Vec<u32>,
    song_status: Vec<u32>,
    song_levels: Vec<f32>,
    scope_sources: Vec<u32>,
    scope_channels: Vec<u32>,
    scope_written: Vec<f64>,
    scope_data: Vec<f32>,
}

/// A module song the page follows.
struct SongSlot {
    reader: Arc<dyn SongReader>,
    /// The version `cw_songs_prepare` last reported.
    last_version: u32,
    /// Reported as gone; dropped by the next message handler (freeing its probe
    /// is not for the render callback).
    gone: bool,
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
    let sources = mixer.config().max_sources;
    let released = mixer.config().max_returned;
    let state = State {
        mixer,
        left: vec![0.0; BLOCK],
        right: vec![0.0; BLOCK],
        inbox: Vec::new(),
        clip_left: Vec::new(),
        clip_right: Vec::new(),
        file: Vec::new(),
        prepare_start: 0.0,
        held: Vec::new(),
        effect_params: [0.0; MAX_PARAMS],
        error: Vec::new(),
        events: Vec::with_capacity(MAX_EVENTS * wire::EVENT_SIZE),
        meters: vec![0.0; buses * METER_FIELDS],
        released: Vec::with_capacity(released),
        bad_batches: 0,
        taps: TapSet::new(buses),
        tap_buses: Vec::with_capacity(buses),
        tap_written: Vec::with_capacity(buses),
        tap_data: Vec::new(),
        swap_play: Vec::new(),
        swaps: swap::Swaps::new(),
        swapped: (0.0, 0.0),
        songs: (0..sources).map(|_| None).collect(),
        song_sources: Vec::with_capacity(sources),
        song_status: Vec::with_capacity(sources * SONG_STATUS_WORDS),
        song_levels: Vec::with_capacity(sources * SONG_LEVEL_SLOTS),
        scope_sources: Vec::with_capacity(sources),
        scope_channels: Vec::with_capacity(sources),
        scope_written: Vec::new(),
        scope_data: Vec::new(),
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
        swaps,
        ..
    } = state();
    released.clear();
    // (A song reported gone is dropped by the next prepare, not here.)
    let mut applied = 0;
    let len = (len as usize).min(inbox.len());
    for command in wire::decode(&inbox[..len]) {
        match command {
            Ok(mut command) => {
                // Commands for a voice that was swapped reach its replacement,
                // and a swap that is warming up hears its old voice's events.
                swaps.route(&mut command);
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

/// Room for a song file of `len` bytes; write it there, then call
/// `cw_prepare_song`.
#[no_mangle]
pub extern "C" fn cw_file_buffer(len: u32) -> *mut u8 {
    let file = &mut state().file;
    file.resize(len as usize, 0);
    file.as_mut_ptr()
}

/// Builds the song written at `cw_file_buffer` as prepared source `source`.
/// `format` 0 is AHX/HVL, 1 A2M, 2 SID, 3 MOD, 4 S3M, 5 XM. `separation` is a module's
/// stereo width, 0-1; below 0 plays the module's own. Returns 1 on success;
/// on failure 0, with the reason at `cw_error`.
#[no_mangle]
pub extern "C" fn cw_prepare_song(
    source: u32,
    format: u32,
    subsong: u32,
    looped: u32,
    separation: f32,
) -> u32 {
    let s = state();
    s.released.clear();
    s.error.clear();
    forget_song(s, source);
    let options = SongOptions {
        subsong: subsong as usize,
        looped: looped != 0,
        stereo_separation: (separation >= 0.0).then_some(separation),
    };
    match prepare_song(s, source, format, options) {
        Ok(()) => 1,
        Err(why) => {
            s.error.extend(
                why.chars()
                    .map(|c| if c.is_ascii() { c as u8 } else { b'?' }),
            );
            0
        }
    }
}

/// Stops following the song in source slot `source` (the slot is about to hold
/// something else), and drops the songs already reported gone. Runs in a message
/// handler: freeing a song's probe is not for the render callback.
fn forget_song(s: &mut State, source: u32) {
    for slot in s.songs.iter_mut() {
        if slot.as_ref().is_some_and(|song| song.gone) {
            *slot = None;
        }
    }
    if let Some(slot) = s.songs.get_mut(source as usize) {
        *slot = None;
    }
}

/// Where in the PSID tune the next `cw_prepare_module` starts it, in seconds (the tune's code is run to there
/// while it builds; a seek is a rebuild). Used once, then 0. Other formats ignore it.
#[no_mangle]
pub extern "C" fn cw_prepare_start(seconds: f64) {
    state().prepare_start = if seconds.is_finite() {
        seconds.max(0.0)
    } else {
        0.0
    };
}

/// Builds the song written at `cw_file_buffer` as prepared source `source`, under live
/// control, and follows it: `format` 0 is an AHX or HVL song, 1 an Adlib Tracker II module, 2 a SID song, 3 MOD,
/// 4 S3M, 5 XM (the formats `cubed_mixer::songs::open_controlled_song` puts under control;
/// the others are refused); `separation` as for `cw_prepare_song`, for modules. `flags` bit 0 opens it stopped and
/// under transport control (no sound until a `SongControl::Play`; its voice never
/// ends by itself). Play it with a `PlaySource` command, steer it with
/// `SourceEvent::Song` commands (wire tag 30), and read it with `cw_songs_prepare`.
/// Returns 1 on success; on failure 0, with the reason at `cw_error`.
#[no_mangle]
pub extern "C" fn cw_prepare_module(
    source: u32,
    format: u32,
    subsong: u32,
    looped: u32,
    separation: f32,
    flags: u32,
) -> u32 {
    let s = state();
    s.released.clear();
    s.error.clear();
    forget_song(s, source);
    let options = SongOptions {
        subsong: subsong as usize,
        looped: looped != 0,
        stereo_separation: (separation >= 0.0).then_some(separation),
    };
    match prepare_module(s, source, format, options, flags & 1 != 0) {
        Ok(()) => 1,
        Err(why) => {
            s.error.extend(
                why.chars()
                    .map(|c| if c.is_ascii() { c as u8 } else { b'?' }),
            );
            0
        }
    }
}

#[cfg(feature = "songs")]
fn prepare_module(
    s: &mut State,
    source: u32,
    format: u32,
    options: SongOptions,
    stopped: bool,
) -> Result<(), String> {
    use cubed_mixer::songs::{open_controlled_song_at, SongFormat};
    let format = match format {
        0 => SongFormat::Ahx,
        1 => SongFormat::A2m,
        2 => SongFormat::Sid,
        3 => SongFormat::Mod,
        4 => SongFormat::S3m,
        5 => SongFormat::Xm,
        other => {
            return Err(format!(
                "format {other} is not under live control (0 AHX/HVL, 1 A2M, 2 SID, 3 MOD, 4 S3M, 5 XM)"
            ))
        }
    };
    if source as usize >= s.songs.len() {
        return Err(format!("no source slot {source}"));
    }
    let rate = s.mixer.config().sample_rate;
    let options = cubed_mixer::songs::SongOptions {
        subsong: options.subsong,
        looped: options.looped,
        stereo_separation: options.stereo_separation,
    };
    // A PSID tune is run to where it starts, here and not on the audio thread.
    let start = std::mem::take(&mut s.prepare_start);
    let song = open_controlled_song_at(format, &s.file, rate, options, stopped, start)?;
    let reader = song.song_reader().ok_or("the song has no reader")?;
    s.songs[source as usize] = Some(SongSlot {
        reader,
        last_version: u32::MAX,
        gone: false,
    });
    s.mixer
        .apply(Command::PrepareSource(cubed_mixer::SourceId(source), song));
    // Room for the scopes of every song followed, so `cw_songs_prepare` fills
    // them without allocating.
    let channels: usize = s.songs.iter().flatten().map(|x| x.reader.channels()).sum();
    s.scope_data.reserve(channels * SONG_SCOPE_FRAMES);
    s.scope_written.reserve(channels);
    Ok(())
}

#[cfg(not(feature = "songs"))]
fn prepare_module(_: &mut State, _: u32, _: u32, _: SongOptions, _: bool) -> Result<(), String> {
    Err("this build of cubed-worklet plays no songs".into())
}

/// Cuts what the page follows of its module songs into the buffers below, for
/// every one whose state moved since the last call (and once more when one has
/// gone), and returns how many that is. Call it from the render callback's thread
/// about 60 times a second; it allocates and frees nothing. For each of those
/// songs: its source slot (`cw_song_sources`, `u32`s), its status words
/// (`cw_song_status`, `cw_song_status_words()` `u32`s each, see
/// `cubed_mixer::SongStatus::to_words`) and its levels (`cw_song_levels`,
/// `cw_song_level_slots()` floats each: the channels', then the preview's). Of
/// those with their scopes on, also: how many there are
/// (`cw_song_scope_count`), their slots (`cw_song_scope_sources`), channel counts
/// (`cw_song_scope_channels`), frames written per channel (`cw_song_scope_written`,
/// `f64`s) and the frames (`cw_song_scope_data`: for each song, each channel's
/// `cw_song_scope_frames()` frames, oldest first).
#[no_mangle]
pub extern "C" fn cw_songs_prepare() -> u32 {
    let State {
        songs,
        song_sources,
        song_status,
        song_levels,
        scope_sources,
        scope_channels,
        scope_written,
        scope_data,
        ..
    } = state();
    song_sources.clear();
    song_status.clear();
    song_levels.clear();
    scope_sources.clear();
    scope_channels.clear();
    scope_written.clear();
    scope_data.clear();
    for (source, slot) in songs.iter_mut().enumerate() {
        let Some(slot) = slot.as_mut().filter(|slot| !slot.gone) else {
            continue;
        };
        let alive = slot.reader.alive();
        let status = slot.reader.status();
        if alive && status.version == slot.last_version {
            continue;
        }
        slot.last_version = status.version;
        slot.gone = !alive;
        let channels = slot.reader.channels();
        let scopes = slot.reader.scopes_enabled();
        // The capacities were reserved when the song was prepared, so none of
        // these grows (a song that would not fit is left for the next call).
        if song_sources.len() == song_sources.capacity() {
            slot.last_version = u32::MAX;
            continue;
        }
        song_sources.push(source as u32);
        song_status.extend_from_slice(&status.to_words(channels, alive, scopes));
        let at = song_levels.len();
        song_levels.resize(at + SONG_LEVEL_SLOTS, 0.0);
        slot.reader
            .levels(&mut song_levels[at..at + SONG_LEVEL_SLOTS]);
        let room = scope_data.capacity() - scope_data.len() >= channels * SONG_SCOPE_FRAMES
            && scope_written.capacity() - scope_written.len() >= channels;
        if scopes && alive && room {
            scope_sources.push(source as u32);
            scope_channels.push(channels as u32);
            for channel in 0..channels {
                let at = scope_data.len();
                scope_data.resize(at + SONG_SCOPE_FRAMES, 0.0);
                let frames = &mut scope_data[at..];
                let got = slot.reader.scope(channel, frames);
                if got < SONG_SCOPE_FRAMES {
                    // Newest last, silence before.
                    frames.copy_within(..got, SONG_SCOPE_FRAMES - got);
                    frames[..SONG_SCOPE_FRAMES - got].fill(0.0);
                }
                scope_written.push(slot.reader.scope_position(channel) as f64);
            }
        }
    }
    song_sources.len() as u32
}

/// How many module songs are followed (and not yet reported gone).
#[no_mangle]
pub extern "C" fn cw_song_count() -> u32 {
    state()
        .songs
        .iter()
        .flatten()
        .filter(|slot| !slot.gone)
        .count() as u32
}

/// `u32`s per song in `cw_song_status`.
#[no_mangle]
pub extern "C" fn cw_song_status_words() -> u32 {
    SONG_STATUS_WORDS as u32
}

/// Floats per song in `cw_song_levels`.
#[no_mangle]
pub extern "C" fn cw_song_level_slots() -> u32 {
    SONG_LEVEL_SLOTS as u32
}

/// Frames per channel in `cw_song_scope_data`.
#[no_mangle]
pub extern "C" fn cw_song_scope_frames() -> u32 {
    SONG_SCOPE_FRAMES as u32
}

/// The source slots `cw_songs_prepare` reported, as `u32`s.
#[no_mangle]
pub extern "C" fn cw_song_sources() -> *const u32 {
    state().song_sources.as_ptr()
}

#[no_mangle]
pub extern "C" fn cw_song_status() -> *const u32 {
    state().song_status.as_ptr()
}

#[no_mangle]
pub extern "C" fn cw_song_levels() -> *const f32 {
    state().song_levels.as_ptr()
}

/// How many of the reported songs have scope frames in `cw_song_scope_data`.
#[no_mangle]
pub extern "C" fn cw_song_scope_count() -> u32 {
    state().scope_sources.len() as u32
}

#[no_mangle]
pub extern "C" fn cw_song_scope_sources() -> *const u32 {
    state().scope_sources.as_ptr()
}

#[no_mangle]
pub extern "C" fn cw_song_scope_channels() -> *const u32 {
    state().scope_channels.as_ptr()
}

/// Frames written to each channel's scope ring so far, as `f64`s, for every
/// channel of every scope entry in order.
#[no_mangle]
pub extern "C" fn cw_song_scope_written() -> *const f64 {
    state().scope_written.as_ptr()
}

/// How many floats `cw_song_scope_data` holds.
#[no_mangle]
pub extern "C" fn cw_song_scope_len() -> u32 {
    state().scope_data.len() as u32
}

#[no_mangle]
pub extern "C" fn cw_song_scope_data() -> *const f32 {
    state().scope_data.as_ptr()
}

/// How a song plays, as the exports pass it on.
#[derive(Clone, Copy)]
struct SongOptions {
    subsong: usize,
    looped: bool,
    stereo_separation: Option<f32>,
}

#[cfg(feature = "songs")]
fn prepare_song(
    s: &mut State,
    source: u32,
    format: u32,
    options: SongOptions,
) -> Result<(), String> {
    use cubed_mixer::songs::{open_song, SongFormat};
    let format = match format {
        0 => SongFormat::Ahx,
        1 => SongFormat::A2m,
        2 => SongFormat::Sid,
        3 => SongFormat::Mod,
        4 => SongFormat::S3m,
        5 => SongFormat::Xm,
        other => return Err(format!("unknown song format {other}")),
    };
    let rate = s.mixer.config().sample_rate;
    let options = cubed_mixer::songs::SongOptions {
        subsong: options.subsong,
        looped: options.looped,
        stereo_separation: options.stereo_separation,
    };
    let song = open_song(format, &s.file, rate, options)?;
    s.mixer
        .apply(Command::PrepareSource(cubed_mixer::SourceId(source), song));
    Ok(())
}

#[cfg(not(feature = "songs"))]
fn prepare_song(_: &mut State, _: u32, _: u32, _: SongOptions) -> Result<(), String> {
    Err("this build of cubed-worklet plays no songs".into())
}

/// Builds the synth patch written at `cw_file_buffer` (the DAW's patch JSON,
/// UTF-8) as prepared source `source`; `one_shot` 1 ends its voice once its
/// notes have died away. Returns 1 on success; on failure 0, with the reason
/// at `cw_error`.
#[no_mangle]
pub extern "C" fn cw_prepare_synth(source: u32, one_shot: u32) -> u32 {
    prepare_synth(source, one_shot, 0, 0)
}

/// Room for `count` held notes (two floats each: the MIDI note number, then the
/// velocity 0-1); write them there, then call `cw_prepare_synth_held`.
#[no_mangle]
pub extern "C" fn cw_held_buffer(count: u32) -> *mut f32 {
    let held = &mut state().held;
    held.resize(count as usize * 2, 0.0);
    held.as_mut_ptr()
}

/// `cw_prepare_synth` for a rebuilt synth that takes over from a playing one:
/// the first `held_count` notes of `cw_held_buffer` are started and `warm_blocks`
/// blocks of 128 frames are rendered and thrown away, so the new synth sounds
/// like the old from its first block. The build runs in the message handler, on
/// the audio thread: every warm-up block costs a block's render time, so keep
/// it small there (a few). Then send `PlaySource` with gain 0, `FadeVoice` and
/// `Stop` for the old voice in one batch (`cubed_mixer::swap_commands`).
#[no_mangle]
pub extern "C" fn cw_prepare_synth_held(
    source: u32,
    one_shot: u32,
    warm_blocks: u32,
    held_count: u32,
) -> u32 {
    prepare_synth(source, one_shot, warm_blocks, held_count)
}

fn prepare_synth(source: u32, one_shot: u32, warm_blocks: u32, held_count: u32) -> u32 {
    let s = state();
    s.released.clear();
    s.error.clear();
    forget_song(s, source);
    let held: Vec<HeldNote> = s
        .held
        .as_chunks::<2>()
        .0
        .iter()
        .take(held_count as usize)
        .map(|pair| HeldNote {
            note: pair[0].clamp(0.0, 127.0) as u8,
            velocity: pair[1].clamp(0.0, 1.0),
        })
        .collect();
    let build = SynthBuild {
        one_shot: one_shot != 0,
        held,
        warm_blocks,
    };
    let result = std::str::from_utf8(&s.file)
        .map_err(|e| format!("the patch is not UTF-8: {e}"))
        .and_then(|json| SynthSource::from_patch_with(json, s.mixer.config().sample_rate, &build));
    match result {
        Ok(synth) => {
            s.mixer.apply(Command::PrepareSource(
                cubed_mixer::SourceId(source),
                Box::new(synth),
            ));
            1
        }
        Err(why) => {
            s.error.extend(
                why.chars()
                    .map(|c| if c.is_ascii() { c as u8 } else { b'?' }),
            );
            0
        }
    }
}

/// Room for the `PlaySource` command (wire bytes, as `cubed_mixer::wire`
/// encodes it) of the voice that will take over, for `cw_swap_begin`.
#[no_mangle]
pub extern "C" fn cw_swap_play_buffer(len: u32) -> *mut u8 {
    let play = &mut state().swap_play;
    play.resize(len as usize, 0);
    play.as_mut_ptr()
}

/// Starts swapping a rebuilt synth in for voice `old_voice` without stalling
/// the render callback. The patch is at `cw_file_buffer`, the `PlaySource` of
/// the new voice (its bus, gain, pan; its source slot is `source`) at
/// `cw_swap_play_buffer`, the notes the old synth holds in `cw_held_buffer`
/// (`held_count` of them). The synth is built now, in the message handler (one
/// build, a few milliseconds), and then warmed up `slice_blocks` blocks (0 for 4,
/// at most 32) per render quantum, while the old voice goes on playing, until it
/// is `warm_blocks` blocks (of 128 frames) further into its notes than the old
/// synth was when this was called plus the time since: when the held notes are
/// `warm_blocks` old, it sounds as if they had been held that long. Then it takes
/// over with a crossfade of `fade_frames`. Returns 1 if the swap is pending; on
/// failure 0, with the reason at `cw_error`. A swap already pending is dropped.
/// `cw_warm_step` does the rest.
#[no_mangle]
pub extern "C" fn cw_swap_begin(
    source: u32,
    one_shot: u32,
    old_voice: f64,
    fade_frames: u32,
    warm_blocks: u32,
    slice_blocks: u32,
    held_count: u32,
) -> u32 {
    let s = state();
    s.released.clear();
    s.error.clear();
    let fail = |s: &mut State, why: &str| {
        s.error
            .extend(why.bytes().map(|c| if c.is_ascii() { c } else { b'?' }));
        0
    };
    let old = VoiceId(old_voice as u64);
    if !s.mixer.is_playing(old) {
        return fail(s, "the voice to replace is not playing");
    }
    let decoded = wire::decode(&s.swap_play).next();
    let play = match decoded {
        Some(Ok(Command::PlaySource(play))) => play,
        _ => return fail(s, "the swap needs a PlaySource command"),
    };
    let held: Vec<HeldNote> = s
        .held
        .as_chunks::<2>()
        .0
        .iter()
        .take(held_count as usize)
        .map(|pair| HeldNote {
            note: pair[0].clamp(0.0, 127.0) as u8,
            velocity: pair[1].clamp(0.0, 1.0),
        })
        .collect();
    let build = SynthBuild {
        one_shot: one_shot != 0,
        held,
        warm_blocks: 1,
    };
    let built = std::str::from_utf8(&s.file)
        .map_err(|e| format!("the patch is not UTF-8: {e}"))
        .and_then(|json| SynthSource::from_patch_with(json, s.mixer.config().sample_rate, &build));
    match built {
        Ok(synth) => {
            s.swaps.begin(
                synth,
                cubed_mixer::SourceId(source),
                old,
                play,
                fade_frames,
                warm_blocks as u64,
                slice_blocks,
            );
            1
        }
        Err(why) => fail(s, &why),
    }
}

/// Warms the pending swap's synth a slice, before the render quantum of `frames`
/// frames is rendered (call it from `process`, ahead of `cw_process`). Returns
/// 0 if no swap is pending, 1 while one is warming up, 2 the moment the new
/// voice took over (`cw_swap_old` and `cw_swap_new` say which voices), 3 if the
/// swap was given up because the old voice ended or was stopped. It allocates
/// nothing.
#[no_mangle]
pub extern "C" fn cw_warm_step(frames: u32) -> u32 {
    let State {
        mixer,
        swaps,
        swapped,
        ..
    } = state();
    match swaps.step(mixer, frames as usize) {
        swap::Step::Idle => 0,
        swap::Step::Warming => 1,
        swap::Step::Done { old, new } => {
            *swapped = (old.0 as f64, new.0 as f64);
            2
        }
        swap::Step::Cancelled => 3,
    }
}

/// 1 while a swap is warming up.
#[no_mangle]
pub extern "C" fn cw_swap_pending() -> u32 {
    state().swaps.pending() as u32
}

/// The voice the last finished swap replaced.
#[no_mangle]
pub extern "C" fn cw_swap_old() -> f64 {
    state().swapped.0
}

/// The voice that took over in the last finished swap.
#[no_mangle]
pub extern "C" fn cw_swap_new() -> f64 {
    state().swapped.1
}

/// The reason the last `cw_prepare_song` failed (`cw_error_len` bytes).
#[no_mangle]
pub extern "C" fn cw_error() -> *const u8 {
    state().error.as_ptr()
}

#[no_mangle]
pub extern "C" fn cw_error_len() -> u32 {
    state().error.len() as u32
}

/// Where `cw_set_effect` reads its parameters: `MAX_PARAMS` floats, in the
/// order of the effect's parameter list.
#[no_mangle]
pub extern "C" fn cw_effect_params() -> *mut f32 {
    state().effect_params.as_mut_ptr()
}

/// Puts built-in effect `kind` (`EffectKind::index`) with the parameters at
/// `cw_effect_params` in insert `slot` of `bus`; any other kind empties the
/// slot. Returns 1 if an effect was built.
#[no_mangle]
pub extern "C" fn cw_set_effect(bus: u32, slot: u32, kind: u32) -> u32 {
    let s = state();
    s.released.clear();
    let insert = u8::try_from(kind)
        .ok()
        .and_then(EffectKind::from_index)
        .map(|kind| {
            Effect {
                kind,
                params: s.effect_params,
            }
            .build(s.mixer.config().sample_rate, BLOCK)
        });
    let built = insert.is_some() as u32;
    s.mixer.apply(Command::SetInsert {
        bus: BusId(bus as u16),
        slot: slot as u8,
        insert,
    });
    built
}

/// Switches the tap of bus slot `bus` on (`enable` 1) or off (0): the bus keeps
/// its latest frames after its fader, which `cw_taps_prepare` cuts for the page.
/// The bus must exist. Returns 1 on success. Allocates (it makes the tap and
/// room for what is sent): it runs in the message handler.
#[no_mangle]
pub extern "C" fn cw_set_tap(bus: u32, enable: u32) -> u32 {
    let s = state();
    s.released.clear();
    let id = BusId(bus.min(u16::MAX as u32) as u16);
    if enable == 0 {
        s.taps.disable(id);
        s.mixer.apply(Command::SetTap(id, None));
    } else {
        if s.mixer.meter(id).is_none() {
            return 0;
        }
        let Some(tap) = s.taps.enable(id, TAP_FRAMES) else {
            return 0;
        };
        s.mixer.apply(Command::SetTap(id, Some(tap)));
    }
    let wanted = s.taps.enabled().count() * 2 * TAP_SEND_FRAMES;
    s.tap_data.resize(wanted, 0.0);
    1
}

/// How many taps are switched on.
#[no_mangle]
pub extern "C" fn cw_tap_count() -> u32 {
    state().taps.enabled().count() as u32
}

/// Cuts the latest `frames` (at most 2048) of every tap that is on into the
/// buffers `cw_tap_buses`, `cw_tap_written` and `cw_tap_data` describe, and
/// returns how many buses that is. Runs in the render callback's thread and
/// does not allocate; a tap that has not seen that many frames yet is padded
/// with silence in front.
#[no_mangle]
pub extern "C" fn cw_taps_prepare(frames: u32) -> u32 {
    let State {
        taps,
        tap_buses,
        tap_written,
        tap_data,
        ..
    } = state();
    let frames = (frames as usize).clamp(1, TAP_SEND_FRAMES);
    tap_buses.clear();
    tap_written.clear();
    let mut at = 0;
    for bus in taps.enabled() {
        if at + 2 * frames > tap_data.len() {
            break;
        }
        let (left, rest) = tap_data[at..].split_at_mut(frames);
        let right = &mut rest[..frames];
        let got = taps.scope(bus, left, right);
        if got < frames {
            // Newest frames last, silence before them.
            left.copy_within(..got, frames - got);
            right.copy_within(..got, frames - got);
            left[..frames - got].fill(0.0);
            right[..frames - got].fill(0.0);
        }
        tap_buses.push(bus.0 as u32);
        tap_written.push(taps.position(bus) as f64);
        at += 2 * frames;
    }
    tap_buses.len() as u32
}

/// The bus slots `cw_taps_prepare` cut, as `u32`s.
#[no_mangle]
pub extern "C" fn cw_tap_buses() -> *const u32 {
    state().tap_buses.as_ptr()
}

/// How many frames each of those taps had seen in all, as `f64`s.
#[no_mangle]
pub extern "C" fn cw_tap_written() -> *const f64 {
    state().tap_written.as_ptr()
}

/// The frames `cw_taps_prepare` cut: for each bus in order, its left frames,
/// then its right frames (`frames` each).
#[no_mangle]
pub extern "C" fn cw_tap_data() -> *const f32 {
    state().tap_data.as_ptr()
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

    /// The exports share one global mixer, so the tests take turns.
    static EXPORTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn exports() -> std::sync::MutexGuard<'static, ()> {
        EXPORTS.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The exports driven the way the worklet does: init, a bus, a clip, a
    /// play, render, read events and meters.
    #[test]
    fn the_exports_play_a_clip() {
        let _turn = exports();
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

    fn send(command: Command) {
        let mut bytes = Vec::new();
        assert!(wire::encode(&command, &mut bytes));
        let inbox = cw_inbox(bytes.len() as u32);
        // SAFETY: cw_inbox made room for the bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), inbox, bytes.len()) };
        assert_eq!(cw_apply(bytes.len() as u32), 1);
    }

    /// A bass-like patch that decays to nothing: a sine with an envelope of no
    /// sustain, so a note held for seconds is silent, and a fresh one is loud.
    fn pluck(gain: f64) -> Vec<u8> {
        use cubed_dsp::patch_edit::PatchDoc;
        use serde_json::json;
        let mut doc = PatchDoc::new("pluck");
        let find = |doc: &PatchDoc, t: &str| {
            doc.nodes()
                .into_iter()
                .find(|n| n.type_name == t)
                .unwrap()
                .id
        };
        let (osc, env) = (find(&doc, "oscillator"), find(&doc, "envelope"));
        doc.set_param(&osc, "waveform", &json!(0)).unwrap();
        doc.set_param(&osc, "gain", &json!(gain)).unwrap();
        doc.set_param(&env, "attack", &json!(0.001)).unwrap();
        doc.set_param(&env, "decay", &json!(0.3)).unwrap();
        doc.set_param(&env, "sustain", &json!(0.0)).unwrap();
        doc.set_param("10005", "active", &json!(false)).unwrap();
        doc.to_json().into_bytes()
    }

    fn put(patch: &[u8]) {
        let at = cw_file_buffer(patch.len() as u32);
        // SAFETY: cw_file_buffer made room for the file.
        unsafe { std::ptr::copy_nonoverlapping(patch.as_ptr(), at, patch.len()) };
    }

    fn put_play(play: &cubed_mixer::PlaySource) {
        let mut bytes = Vec::new();
        assert!(wire::encode(&Command::PlaySource(*play), &mut bytes));
        let at = cw_swap_play_buffer(bytes.len() as u32);
        // SAFETY: cw_swap_play_buffer made room for the bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len()) };
    }

    fn put_held(notes: &[(f32, f32)]) {
        let room = cw_held_buffer(notes.len() as u32);
        // SAFETY: cw_held_buffer made room for two floats a note.
        let view = unsafe { std::slice::from_raw_parts_mut(room, notes.len() * 2) };
        for (i, (note, velocity)) in notes.iter().enumerate() {
            view[i * 2] = *note;
            view[i * 2 + 1] = *velocity;
        }
    }

    /// One render quantum, with a swap's warm-up step ahead of it as the
    /// processor does; the left channel's RMS and the step's outcome.
    fn quantum() -> (f32, u32) {
        let step = cw_warm_step(128);
        cw_process(128);
        // SAFETY: the output buffers hold BLOCK floats.
        let left = unsafe { std::slice::from_raw_parts(cw_left(), BLOCK) };
        (
            (left.iter().map(|s| s * s).sum::<f32>() / BLOCK as f32).sqrt(),
            step,
        )
    }

    fn start_pluck(note: u8) {
        use cubed_mixer::{PlaySource, SourceEvent, SourceId, VoiceId};
        send(Command::SetBus(BusId(1), BusSettings::default()));
        put(&pluck(0.8));
        assert_eq!(cw_prepare_synth(0, 0), 1);
        send(Command::PlaySource(PlaySource::new(
            VoiceId(1),
            SourceId(0),
            BusId(1),
        )));
        send(Command::SourceEvent {
            voice: VoiceId(1),
            event: SourceEvent::NoteOn {
                note,
                velocity: 1.0,
                duration: 0,
            },
        });
    }

    /// Begins swapping a rebuilt pluck in for voice 1 as voice 2.
    fn begin_swap(warm_blocks: u32, slice: u32, held: &[(f32, f32)]) -> u32 {
        use cubed_mixer::{PlaySource, SourceId, VoiceId};
        put(&pluck(0.7));
        put_play(&PlaySource::new(VoiceId(2), SourceId(1), BusId(1)));
        put_held(held);
        cw_swap_begin(1, 0, 1.0, 600, warm_blocks, slice, held.len() as u32)
    }

    /// The point of the slices: a decayed note stays decayed through a swap, the
    /// render callback never does more than a few blocks of warm-up, and the old
    /// voice keeps playing meanwhile.
    #[test]
    fn a_warmed_swap_keeps_a_decayed_note_decayed() {
        let _turn = exports();
        cw_init(48_000.0, 0, 0, 0, 11, 12);
        start_pluck(60);
        let first: Vec<f32> = (0..10).map(|_| quantum().0).collect();
        assert!(first[2] > 0.1, "the pluck sounds: {first:?}");
        for _ in 0..750 {
            quantum();
        }
        let decayed = quantum().0;
        assert!(decayed < 0.01, "after 2 s it has decayed: {decayed}");

        // The note has been held 760 blocks: swap in a rebuilt synth warmed that far.
        assert_eq!(begin_swap(760, 8, &[(60.0, 1.0)]), 1);
        assert_eq!(cw_swap_pending(), 1);
        let mut quanta = 0;
        let mut loudest = 0.0f32;
        loop {
            let (level, step) = quantum();
            quanta += 1;
            loudest = loudest.max(level);
            if step == 2 {
                break;
            }
            assert_eq!(step, 1, "warming until it is done");
            assert!(quanta < 400, "it never finished");
        }
        // 760 blocks at 8 a quantum against a clock that moves 1: about 110.
        assert!((90..=140).contains(&quanta), "{quanta} quanta");
        assert_eq!((cw_swap_old(), cw_swap_new()), (1.0, 2.0));
        assert_eq!(cw_swap_pending(), 0);
        // Through the crossfade and after, still decayed: no attack.
        for _ in 0..80 {
            loudest = loudest.max(quantum().0);
        }
        assert!(loudest < 0.02, "no retrigger: {loudest}");
        assert_eq!(cw_voices(), 1, "the old voice ended");

        // A note played now (for the old id) sounds, in the new synth.
        send(Command::SourceEvent {
            voice: cubed_mixer::VoiceId(1),
            event: cubed_mixer::SourceEvent::NoteOn {
                note: 67,
                velocity: 1.0,
                duration: 0,
            },
        });
        let heard = (0..10).map(|_| quantum().0).fold(0.0f32, f32::max);
        assert!(heard > 0.1, "the alias reaches the new voice: {heard}");
        // ... and a stop for the old id stops the new voice.
        send(Command::Stop(cubed_mixer::VoiceId(1), 0));
        for _ in 0..4 {
            quantum();
        }
        assert_eq!(cw_voices(), 0);
        assert_eq!(cw_bad_batches(), 0);
    }

    /// Without the warm-up the same swap attacks again: what the test above rules
    /// out is real.
    #[test]
    fn a_cold_swap_retriggers_the_attack() {
        let _turn = exports();
        cw_init(48_000.0, 0, 0, 0, 13, 14);
        start_pluck(60);
        for _ in 0..760 {
            quantum();
        }
        assert_eq!(begin_swap(0, 8, &[(60.0, 1.0)]), 1);
        let mut loudest = 0.0f32;
        for _ in 0..20 {
            loudest = loudest.max(quantum().0);
        }
        assert!(loudest > 0.1, "a cold synth attacks: {loudest}");
    }

    /// A note played while the new synth warms up is in it when it takes over, at
    /// the right point of its decay.
    #[test]
    fn notes_played_during_the_warm_up_carry_over() {
        use cubed_mixer::{SourceEvent, VoiceId};
        let _turn = exports();
        cw_init(48_000.0, 0, 0, 0, 15, 16);
        start_pluck(60);
        for _ in 0..400 {
            quantum();
        }
        assert_eq!(begin_swap(400, 6, &[(60.0, 1.0)]), 1);
        let mut levels = Vec::new();
        let mut swapped_at = None;
        for q in 0..400 {
            // A new note 30 quanta into the warm-up.
            if q == 30 {
                send(Command::SourceEvent {
                    voice: VoiceId(1),
                    event: SourceEvent::NoteOn {
                        note: 64,
                        velocity: 1.0,
                        duration: 0,
                    },
                });
            }
            let (level, step) = quantum();
            levels.push(level);
            if step == 2 {
                swapped_at = Some(q);
                break;
            }
        }
        let swapped_at = swapped_at.expect("swapped");
        assert!(swapped_at > 60, "warming took a while: {swapped_at}");
        // The note decays over 0.3 s (112 blocks): at the swap, the new voice is
        // where the old one is, not at the attack.
        let before = levels[swapped_at - 1];
        let after: Vec<f32> = (0..12).map(|_| quantum().0).collect();
        assert!(before > 0.005, "the note is still ringing: {before}");
        for level in &after {
            assert!(
                *level < before * 1.4 && *level > before * 0.3,
                "{level} vs {before}"
            );
        }
    }

    #[test]
    fn a_swap_gives_up_when_the_old_voice_stops_and_refuses_a_missing_one() {
        use cubed_mixer::VoiceId;
        let _turn = exports();
        cw_init(48_000.0, 0, 0, 0, 17, 18);
        start_pluck(60);
        quantum();
        // A voice that is not playing.
        put(&pluck(0.7));
        put_play(&cubed_mixer::PlaySource::new(
            VoiceId(2),
            cubed_mixer::SourceId(1),
            BusId(1),
        ));
        assert_eq!(cw_swap_begin(1, 0, 9.0, 600, 10, 4, 0), 0);
        assert!(cw_error_len() > 0);
        // Not a PlaySource.
        let at = cw_swap_play_buffer(3);
        // SAFETY: room for 3 bytes.
        unsafe { std::slice::from_raw_parts_mut(at, 3) }.copy_from_slice(&[1, 2, 3]);
        assert_eq!(cw_swap_begin(1, 0, 1.0, 600, 10, 4, 0), 0);

        assert_eq!(begin_swap(2_000, 4, &[(60.0, 1.0)]), 1);
        for _ in 0..5 {
            assert_eq!(quantum().1, 1);
        }
        send(Command::Stop(VoiceId(1), 0));
        let steps: Vec<u32> = (0..3).map(|_| quantum().1).collect();
        assert!(steps.contains(&3) || steps[0] == 0, "{steps:?}");
        assert_eq!(cw_swap_pending(), 0);
        for _ in 0..10 {
            quantum();
        }
        assert_eq!(cw_voices(), 0, "nothing took over");
    }

    /// A newer swap replaces a pending one.
    #[test]
    fn a_newer_swap_replaces_a_pending_one() {
        let _turn = exports();
        cw_init(48_000.0, 0, 0, 0, 19, 20);
        start_pluck(60);
        for _ in 0..50 {
            quantum();
        }
        assert_eq!(begin_swap(5_000, 2, &[(60.0, 1.0)]), 1);
        for _ in 0..10 {
            quantum();
        }
        assert_eq!(begin_swap(100, 16, &[(60.0, 1.0)]), 1);
        let mut done = false;
        for _ in 0..40 {
            done |= quantum().1 == 2;
        }
        assert!(done, "the second one finished, the first was dropped");
    }

    /// A bus's tap through the exports: switched on, cut after rendering, the
    /// frames in order, and the page's reader sees them.
    #[test]
    fn the_exports_cut_a_taps_message() {
        use cubed_mixer::{Analysis, BusSettings, Play, WebAnalysis};
        let _turn = exports();
        cw_init(48_000.0, 0, 0, 0, 7, 8);
        send(Command::SetBus(BusId(1), BusSettings::default()));
        assert_eq!(cw_set_tap(1, 1), 1);
        assert_eq!(cw_set_tap(0, 1), 1);
        assert_eq!(cw_set_tap(9, 1), 0, "a bus that does not exist");
        assert_eq!(cw_tap_count(), 2);

        // A ramp clip on bus 1: 0, 1/1000, 2/1000 ...
        let frames = 600;
        let ptr = cw_clip_buffer(frames, 1);
        // SAFETY: cw_clip_buffer made room for `frames` floats.
        let ramp = unsafe { std::slice::from_raw_parts_mut(ptr, frames as usize) };
        for (i, s) in ramp.iter_mut().enumerate() {
            *s = i as f32 / 1000.0;
        }
        cw_add_clip(0, 48_000.0, frames, 1);
        send(Command::Play(Play::new(
            cubed_mixer::VoiceId(1),
            cubed_mixer::ClipId(0),
            BusId(1),
        )));
        for _ in 0..2 {
            cw_process(128);
        }
        // Two blocks seen: 256 frames, the rest silence in front.
        assert_eq!(cw_taps_prepare(512), 2);
        let buses = unsafe { std::slice::from_raw_parts(cw_tap_buses(), 2) }.to_vec();
        let written = unsafe { std::slice::from_raw_parts(cw_tap_written(), 2) }.to_vec();
        let data = unsafe { std::slice::from_raw_parts(cw_tap_data(), 2 * 2 * 512) }.to_vec();
        assert_eq!(buses, [0, 1]);
        assert_eq!(written, [256.0, 256.0]);

        let mut web = WebAnalysis::new(48_000.0);
        web.update_taps(&buses, &written, 512, &data);
        let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
        assert_eq!(web.scope(BusId(1), &mut l, &mut r), 512);
        assert!(l[..256].iter().all(|&s| s == 0.0), "padded in front");
        // The bus is at unity and the clip is centred (a -3 dB pan law): the ramp's
        // last frame is 255/1000 times that.
        let last = l[511];
        assert!(last > 0.15 && last < 0.26, "{last}");
        assert!(l[300] < last && l[300] > 0.0, "rising in order");
        // The master has the same frames (bus 1 feeds it at unity).
        let mut master = vec![0.0; 512];
        web.scope(BusId(0), &mut master, &mut r);
        assert!((master[511] - last).abs() < 1e-4);

        // The meters come back by the same reader.
        let meters = unsafe { std::slice::from_raw_parts(cw_meters(), METER_FIELDS * 2) };
        web.update_meters(meters);
        assert!(web.levels(BusId(1)).unwrap().peak[0] > 0.1);

        // Switched off, a tap is gone from the next message.
        assert_eq!(cw_set_tap(1, 0), 1);
        assert_eq!(cw_tap_count(), 1);
        cw_process(128);
        assert_eq!(cw_taps_prepare(512), 1);
        assert_eq!(cw_bad_batches(), 0);
    }

    /// A synth patch through the exports: prepared, played, edited by a
    /// parameter event, and a rebuilt one swapped in with its held note.
    #[test]
    fn the_exports_swap_in_a_rebuilt_synth() {
        use cubed_dsp::live::ParamEdit;
        use cubed_mixer::{PlaySource, SourceEvent, SourceId, VoiceId};
        let _turn = exports();
        cw_init(48_000.0, 0, 0, 0, 5, 6);
        send(Command::SetBus(BusId(1), BusSettings::default()));
        let patch = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dsp/tests/real_patch.json"
        ))
        .unwrap();
        let put_patch = || {
            let at = cw_file_buffer(patch.len() as u32);
            // SAFETY: cw_file_buffer made room for the file.
            unsafe { std::ptr::copy_nonoverlapping(patch.as_ptr(), at, patch.len()) };
        };
        put_patch();
        assert_eq!(cw_prepare_synth(0, 0), 1);
        send(Command::PlaySource(PlaySource::new(
            VoiceId(1),
            SourceId(0),
            BusId(1),
        )));
        send(Command::SourceEvent {
            voice: VoiceId(1),
            event: SourceEvent::NoteOn {
                note: 60,
                velocity: 1.0,
                duration: 0,
            },
        });
        let render = |blocks: usize| {
            let mut energy = 0.0;
            for _ in 0..blocks {
                cw_process(128);
                let left = unsafe { std::slice::from_raw_parts(cw_left(), BLOCK) };
                energy += left.iter().map(|s| s * s).sum::<f32>();
            }
            energy / (blocks * BLOCK) as f32
        };
        let playing = render(60);
        assert!(playing > 1e-5, "the note is heard: {playing}");
        // A parameter edit for a node that is not there is accepted and does nothing.
        send(Command::SourceEvent {
            voice: VoiceId(1),
            event: SourceEvent::Param(ParamEdit::new(1, 0, 0.5)),
        });
        assert_eq!(cw_bad_batches(), 0);

        // The held note goes in the held buffer, two floats a note.
        let room = cw_held_buffer(1);
        // SAFETY: cw_held_buffer made room for two floats.
        unsafe { std::slice::from_raw_parts_mut(room, 2) }.copy_from_slice(&[60.0, 1.0]);
        put_patch();
        assert_eq!(cw_prepare_synth_held(1, 0, 8, 1), 1);
        for command in cubed_mixer::swap_commands(
            VoiceId(1),
            PlaySource::new(VoiceId(2), SourceId(1), BusId(1)),
            1_200,
        ) {
            send(command);
        }
        let during = render(20);
        assert!(
            during > 0.25 * playing,
            "no gap in the crossfade: {during} vs {playing}"
        );
        assert_eq!(cw_voices(), 1, "the old voice ended");
        // A garbage patch is an error with a reason.
        let bad = b"not json";
        let at = cw_file_buffer(bad.len() as u32);
        // SAFETY: cw_file_buffer made room for the bytes.
        unsafe { std::ptr::copy_nonoverlapping(bad.as_ptr(), at, bad.len()) };
        assert_eq!(cw_prepare_synth_held(2, 0, 0, 0), 0);
        assert!(cw_error_len() > 0);
        assert_eq!(cw_rejected(), 0);
    }

    /// A song file through the exports, on a bus with a built effect.
    #[cfg(feature = "songs")]
    #[test]
    fn the_exports_play_a_song_through_an_effect() {
        use cubed_mixer::{PlaySource, SourceId};
        let _turn = exports();
        cw_init(48_000.0, 0, 0, 0, 3, 4);
        send(Command::SetBus(BusId(2), BusSettings::default()));

        let song = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ahx/tests/fixtures/songs/a_new_beginning.ahx"
        ))
        .unwrap();
        let at = cw_file_buffer(song.len() as u32);
        // SAFETY: cw_file_buffer made room for the file.
        unsafe { std::ptr::copy_nonoverlapping(song.as_ptr(), at, song.len()) };
        assert_eq!(cw_prepare_song(0, 0, 0, 1, -1.0), 1);
        assert_eq!(cw_prepare_song(1, 9, 0, 1, -1.0), 0, "unknown format");
        let error = unsafe { std::slice::from_raw_parts(cw_error(), cw_error_len() as usize) };
        assert!(std::str::from_utf8(error).unwrap().contains("format"));

        // SAFETY: cw_effect_params holds MAX_PARAMS floats.
        let params = unsafe { std::slice::from_raw_parts_mut(cw_effect_params(), MAX_PARAMS) };
        params.copy_from_slice(&Effect::new(EffectKind::LowPass).params);
        assert_eq!(cw_set_effect(2, 0, EffectKind::LowPass.index() as u32), 1);

        send(Command::PlaySource(PlaySource::new(
            VoiceId(1),
            SourceId(0),
            BusId(2),
        )));
        let mut energy = 0.0;
        for _ in 0..400 {
            cw_process(128);
            let left = unsafe { std::slice::from_raw_parts(cw_left(), BLOCK) };
            energy += left.iter().map(|s| s * s).sum::<f32>();
        }
        assert!(energy > 1.0, "the song is heard: {energy}");
        assert_eq!(cw_voices(), 1);
        assert_eq!(cw_set_effect(2, 0, 255), 0, "empties the slot");
        assert_eq!(cw_rejected(), 0);

        // A MOD module is format 3.
        let module = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tracker/tests/fixtures/demo.mod"
        ))
        .unwrap();
        let at = cw_file_buffer(module.len() as u32);
        // SAFETY: cw_file_buffer made room for the file.
        unsafe { std::ptr::copy_nonoverlapping(module.as_ptr(), at, module.len()) };
        assert_eq!(cw_prepare_song(1, 3, 0, 1, 0.5), 1);
        send(Command::PlaySource(PlaySource::new(
            VoiceId(2),
            SourceId(1),
            BusId(2),
        )));
        cw_process(128);
        assert_eq!(cw_voices(), 2);
    }

    /// A module file into `cw_file_buffer`.
    fn put_module(name: &str) {
        let module = std::fs::read(format!(
            "{}/../tracker/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let at = cw_file_buffer(module.len() as u32);
        // SAFETY: cw_file_buffer made room for the file.
        unsafe { std::ptr::copy_nonoverlapping(module.as_ptr(), at, module.len()) };
    }

    fn song_control(voice: u64, control: cubed_mixer::SongControl) {
        send(Command::SourceEvent {
            voice: VoiceId(voice),
            event: cubed_mixer::SourceEvent::Song(control),
        });
    }

    /// What `cw_songs_prepare` cut: per song its slot, status and levels.
    struct Reported {
        source: u32,
        status: cubed_mixer::SongStatus,
        channels: usize,
        alive: bool,
        scopes: bool,
        levels: Vec<f32>,
    }

    fn songs_prepare() -> Vec<Reported> {
        let count = cw_songs_prepare() as usize;
        let words = cw_song_status_words() as usize;
        let slots = cw_song_level_slots() as usize;
        // SAFETY: the buffers hold what `cw_songs_prepare` just cut.
        let (sources, status, levels) = unsafe {
            (
                std::slice::from_raw_parts(cw_song_sources(), count),
                std::slice::from_raw_parts(cw_song_status(), count * words),
                std::slice::from_raw_parts(cw_song_levels(), count * slots),
            )
        };
        (0..count)
            .map(|i| {
                let (status, channels, alive, scopes) =
                    cubed_mixer::SongStatus::from_words(&status[i * words..]).unwrap();
                Reported {
                    source: sources[i],
                    status,
                    channels,
                    alive,
                    scopes,
                    levels: levels[i * slots..(i + 1) * slots].to_vec(),
                }
            })
            .collect()
    }

    #[test]
    fn a_module_is_opened_stopped_followed_steered_and_reported() {
        use cubed_mixer::{PlaySource, SongControl, SongLoop, SourceId};
        let _turn = exports();
        cw_init(48_000.0, 0, 0, 0, 5, 6);
        send(Command::SetBus(BusId(2), BusSettings::default()));
        put_module("demo.mod");
        assert_eq!(cw_prepare_module(3, 3, 0, 1, -1.0, 1), 1);
        assert_eq!(cw_song_count(), 1);
        assert_eq!(cw_prepare_module(99, 3, 0, 1, -1.0, 1), 0, "no such slot");
        assert_eq!(
            cw_prepare_module(1, 0, 0, 1, -1.0, 1),
            0,
            "AHX is not a module"
        );
        assert_eq!(cw_song_count(), 1);
        send(Command::PlaySource(PlaySource::new(
            VoiceId(1),
            SourceId(3),
            BusId(2),
        )));
        // Opened stopped: nothing sounds, and the first report says where it is.
        let mut energy = 0.0;
        for _ in 0..40 {
            cw_process(128);
            let left = unsafe { std::slice::from_raw_parts(cw_left(), BLOCK) };
            energy += left.iter().map(|s| s * s).sum::<f32>();
        }
        assert_eq!(energy, 0.0);
        let first = songs_prepare();
        assert_eq!(first.len(), 1);
        let song = &first[0];
        assert_eq!((song.source, song.channels, song.alive), (3, 4, true));
        assert!(!song.status.playing && !song.scopes);
        assert_eq!((song.status.order, song.status.row), (0, 0));
        assert!(
            songs_prepare().is_empty(),
            "nothing moved: nothing to report"
        );

        // Seek while stopped, then play: the position and levels move.
        song_control(1, SongControl::Scopes(true));
        song_control(1, SongControl::Seek { order: 2, row: 8 });
        let seeked = songs_prepare();
        assert_eq!(seeked.len(), 1);
        assert_eq!((seeked[0].status.order, seeked[0].status.row), (2, 8));
        assert!(seeked[0].scopes);
        song_control(1, SongControl::Play);
        let mut rows = std::collections::BTreeSet::new();
        let mut loudest = 0.0f32;
        let mut scope_peak = 0.0f32;
        for q in 0..600 {
            cw_process(128);
            if q % 5 == 0 {
                for r in songs_prepare() {
                    rows.insert(r.status.row);
                    loudest = r.levels[..4].iter().fold(loudest, |m, &l| m.max(l));
                    assert_eq!(r.source, 3);
                    assert!(r.status.playing);
                }
                let scopes = cw_song_scope_count() as usize;
                if scopes > 0 {
                    let frames = cw_song_scope_frames() as usize;
                    assert_eq!(scopes, 1);
                    // SAFETY: the buffers hold one song of four channels.
                    let (channels, written, data) = unsafe {
                        (
                            std::slice::from_raw_parts(cw_song_scope_channels(), 1),
                            std::slice::from_raw_parts(cw_song_scope_written(), 4),
                            std::slice::from_raw_parts(cw_song_scope_data(), 4 * frames),
                        )
                    };
                    assert_eq!(channels, [4]);
                    assert_eq!(cw_song_scope_len() as usize, 4 * frames);
                    assert!(written[0] > 0.0);
                    scope_peak = data.iter().fold(scope_peak, |m, s| m.max(s.abs()));
                }
            }
        }
        assert!(rows.len() > 8, "the rows move: {rows:?}");
        assert!(loudest > 0.01, "levels: {loudest}");
        assert!(scope_peak > 0.01, "scopes: {scope_peak}");

        // A loop, a preview and mutes, all through the wire.
        song_control(
            1,
            SongControl::Loop(SongLoop::Range {
                order: 2,
                row0: 8,
                row1: 9,
            }),
        );
        song_control(1, SongControl::MuteMask(0b1111));
        song_control(
            1,
            SongControl::PreviewOn {
                instrument: 1,
                note: 60,
                velocity: 1.0,
                duration: 0,
            },
        );
        let mut seen = std::collections::BTreeSet::new();
        let mut preview = false;
        for _ in 0..300 {
            cw_process(128);
            for r in songs_prepare() {
                seen.insert(r.status.row);
                preview |= r.status.preview && r.levels[4] > 0.0;
            }
        }
        assert!(preview, "the preview is reported with a level of its own");
        // The range loop wraps at the next row it reaches: at most the rows
        // between the seek point's neighbours were seen before it caught on.
        assert!(seen.contains(&8) && seen.contains(&9), "{seen:?}");
        assert_eq!(cw_rejected(), 0);
        assert_eq!(cw_bad_batches(), 0);

        // Stop the voice: the song is reported gone once, then forgotten. The
        // source is freed by the message handler after the voice ended (any
        // message does), which is when the probe sees it go.
        send(Command::Stop(VoiceId(1), 0));
        cw_process(128);
        send(Command::SetBusGain(BusId(2), 0.0));
        let mut gone = None;
        for _ in 0..4 {
            cw_process(128);
            if let Some(r) = songs_prepare().into_iter().find(|r| !r.alive) {
                gone = Some(r.source);
            }
        }
        assert_eq!(gone, Some(3), "reported gone");
        assert!(songs_prepare().is_empty());
        // The next prepare drops the song that went.
        put_module("demo.xm");
        assert_eq!(cw_prepare_module(0, 5, 0, 1, -1.0, 0), 1);
        assert_eq!(cw_song_count(), 1, "only the new one");
        // A song opened playing is followed too, and a plain prepare unfollows it.
        assert_eq!(songs_prepare().len(), 1);
        put_module("demo.mod");
        assert_eq!(cw_prepare_song(0, 3, 0, 1, -1.0), 1);
        assert_eq!(cw_song_count(), 0, "the slot holds another song now");
    }
    /// A patch that plays a sampler's sample (a sine of `hz`, `seconds` of 48 kHz stereo) and
    /// carries a 256-frame wavetable on a silent wavetable oscillator: about 6 MB of JSON, what
    /// an editor sends after a sample was loaded into a patch that has a big table.
    fn heavy_patch(hz: f32, seconds: usize) -> Vec<u8> {
        use cubed_dsp::patch_edit::PatchDoc;
        use cubed_dsp::traits::PortId;
        use cubed_dsp::wav::{base64_encode, encode_wav_i16};
        use serde_json::json;
        let mut doc = PatchDoc::new("heavy");
        let find = |doc: &PatchDoc, t: &str| {
            doc.nodes()
                .into_iter()
                .find(|n| n.type_name == t)
                .unwrap()
                .id
        };
        let (osc, mixer) = (find(&doc, "oscillator"), find(&doc, "mixer"));
        doc.set_param(&osc, "active", &json!(false)).unwrap();
        let sampler = doc.add_node("sampler").unwrap();
        doc.connect(
            &sampler,
            PortId::AudioOutput0,
            &mixer,
            PortId::AudioInput0,
            1.0,
            2,
            0,
        )
        .unwrap();
        let frames = 48_000 * seconds;
        let mut samples = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let x = 0.6 * (std::f32::consts::TAU * hz * n as f32 / 48_000.0).sin();
            samples.push(x);
            samples.push(x);
        }
        doc.apply(
            &json!({"op": "load_sample", "node": sampler, "root_note": 69,
            "data": base64_encode(&encode_wav_i16(&samples, 2, 48_000))}),
        )
        .unwrap();
        doc.apply(&json!({"op": "set_sample", "node": sampler, "loop_mode": "loop"}))
            .unwrap();
        let table = doc.add_node("wavetable_oscillator").unwrap();
        doc.set_param(&table, "gain", &json!(0.0)).unwrap();
        doc.apply(
            &json!({"op": "wavetable_preset", "node": table, "preset": "analogPWM", "frames": 256}),
        )
        .unwrap();
        doc.to_json().into_bytes()
    }

    /// Rising zero crossings a second in what was rendered.
    fn pitch_of(out: &[f32]) -> f32 {
        let crossings = out.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        crossings as f32 / (out.len() as f32 / 48_000.0)
    }

    fn render_quanta(n: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for _ in 0..n {
            cw_process(128);
            // SAFETY: the output buffers hold BLOCK floats.
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(cw_left(), BLOCK) });
        }
        out
    }

    /// A patch of many megabytes (a long stereo sample, a 256-frame wavetable) goes through
    /// `cw_file_buffer` and the sliced swap like a small one: the worklet's buffers take any
    /// size, the new synth plays the new sample, and the old voice keeps going until it takes over.
    #[test]
    fn a_patch_of_megabytes_is_prepared_and_swapped_in() {
        use cubed_mixer::{PlaySource, SourceEvent, SourceId, VoiceId};
        let _turn = exports();
        cw_init(48_000.0, 0, 0, 0, 15, 16);
        send(Command::SetBus(BusId(1), BusSettings::default()));
        let first = heavy_patch(1000.0, 12);
        assert!(first.len() > 5_000_000, "{} bytes of JSON", first.len());
        put(&first);
        let started = std::time::Instant::now();
        assert_eq!(cw_prepare_synth(0, 0), 1, "the patch builds");
        eprintln!(
            "prepare of {} MB of patch: {:?}",
            first.len() / 1_000_000,
            started.elapsed()
        );
        send(Command::PlaySource(PlaySource::new(
            VoiceId(1),
            SourceId(0),
            BusId(1),
        )));
        send(Command::SourceEvent {
            voice: VoiceId(1),
            event: SourceEvent::NoteOn {
                note: 69,
                velocity: 1.0,
                duration: 0,
            },
        });
        let before = render_quanta(300);
        let hz = pitch_of(&before[before.len() / 3..]);
        assert!(
            (hz - 1000.0).abs() < 40.0,
            "the sample is heard at 1 kHz: {hz}"
        );

        // Swap in a patch with another sample: a sliced warm-up, the old voice sounds meanwhile.
        put(&heavy_patch(2500.0, 12));
        put_play(&PlaySource::new(VoiceId(2), SourceId(1), BusId(1)));
        put_held(&[(69.0, 1.0)]);
        let started = std::time::Instant::now();
        assert_eq!(
            cw_swap_begin(1, 0, 1.0, 600, 400, 4, 1),
            1,
            "the swap begins"
        );
        eprintln!(
            "swap begin with {} MB of patch: {:?}",
            first.len() / 1_000_000,
            started.elapsed()
        );
        let mut played = Vec::new();
        let mut warming = 0;
        loop {
            let step = cw_warm_step(128);
            cw_process(128);
            // SAFETY: the output buffers hold BLOCK floats.
            played.extend_from_slice(unsafe { std::slice::from_raw_parts(cw_left(), BLOCK) });
            if step == 2 {
                break;
            }
            assert_eq!(step, 1);
            warming += 1;
            assert!(warming < 600, "it never finished");
        }
        let old_voice = pitch_of(&played[played.len() / 4..played.len() / 2]);
        assert!(
            (old_voice - 1000.0).abs() < 40.0,
            "the old voice plays on while it warms: {old_voice}"
        );
        let after = render_quanta(300);
        let hz = pitch_of(&after[after.len() / 2..]);
        assert!(
            (hz - 2500.0).abs() < 80.0,
            "the new sample is heard at 2.5 kHz: {hz}"
        );
        assert_eq!(cw_bad_batches(), 0);
    }
}
