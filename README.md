# cubed-synth

The Rust audio code shared by the another_synth web DAW and the wgpu-cube engine:
chip emulators, module players and a modular synth engine. It is plain Rust that
builds on stable, natively and for `wasm32-unknown-unknown`, so the same code can run
in a browser AudioWorklet and in a native audio callback.

| Crate | What it is |
|---|---|
| `cubed-sid` | MOS 6581/8580 SID chip (with measured 6581 revisions), a GoatTracker-style song player for the `ASID` song file, GoatTracker 1 and 2 `.sng` import (`goattracker`, another_synth's importer ported byte for byte), and a register-stream chip player (`stream`) |
| `cubed-ahx` | AHX / HivelyTracker (`.ahx`, `.hvl`) parser and replayer, bit-exact against `hvl_replay.c`, with a band-limited "hifi" path. The writer (`write`: `.ahx` and `.hvl`, byte-identical to the DAW's on all 127 fixtures) and `edit::AhxDoc`, which edits a song as data with JSON operations, format limits and undo; the player takes live control (seek, loops, mute and solo, an audition voice, levels, a `AhxProbe`) and live edits |
| `cubed-opl` | Yamaha OPL2/OPL3 FM chip (a port of ymfm's OPL path) and an Adlib Tracker II (`.a2m`) parser and player |
| `cubed-tracker` | Tracker modules: ProTracker MOD (Soundtracker, multi-channel), Scream Tracker 3 S3M (AdLib instruments on `cubed-opl`) and FastTracker 2 XM. An editable module model, the parsers, and a replayer ported from another_synth's TypeScript `tracker-playback` (checked against it command for command), with one voice per channel, 32-tap windowed-sinc interpolation, XM envelopes, fadeout and auto-vibrato, the Amiga output filter and stereo separation. The writers (`write_mod`, `write_s3m`, `write_xm`) and `edit::ModuleDoc`, which edits a module as data with JSON operations, format limits and undo. An `Observer` sees every row and command; a `Probe` shows the position, per-channel levels and scopes of a playing player, which also takes live control (stop and play, seek, loops, mute and solo, note preview) |
| `cubed-dsp` | The synth engine: audio graph, oscillators (analog, wavetable), envelopes, LFOs, filters, chorus, delay, reverb, convolver, compressor, limiter, EQ, voices, automation and patch loading; a catalogue of its node types, patch documents that are edited as data, and live parameter edits |
| `cubed-simd` | The slice of `std::simd` the crates above use, on stable Rust |
| `cubed-mixer` | A game audio mixer: named buses with faders, mute/solo, inserts (built-in `effects` with live parameters), sends and ducking; clip voices and sources (chip songs, streams, synth patches played by note events), 2D/3D placement with doppler, long fades, meters; nothing allocates while it runs. `native` adds a cpal output host and streams fed from another thread; `songs` plays AHX/HVL, A2M and SID songs and MOD/S3M/XM modules as sources (a module can be steered and read while it plays); `wire` encodes its commands as bytes |
| `cubed-worklet` | `cubed-mixer` as a wasm module for a browser AudioWorklet (raw exports, no JS glue) and its processor, `js/cubed-worklet.js` |
| `cubed-rt-check` | Tests only: every render path checked for allocations with a counting allocator |

## Features

- `wasm` (every crate): the wasm-bindgen classes another_synth's worklets use. Off by
  default; native hosts and other wasm hosts don't need it.
- `nightly-simd` (`cubed-ahx`, `cubed-dsp`): use `std::simd` itself instead of the
  stable fallback in `cubed-simd` (nightly toolchain only). Both give the same results
  lane for lane.
- `native-host` (`cubed-dsp`, on by default): the native `AudioEngine`, the
  counterpart of the wasm one.

- `native` (`cubed-mixer`): `NativeHost`, the mixer on the default output device
  through cpal, and `stream`, a source a decoding thread feeds through a lock-free ring.
- `songs` (`cubed-mixer`; on by default in `cubed-worklet`): `songs::open_song`, the
  chip-music and module players as mixer sources.
- `adlib` (`cubed-tracker`, on by default): S3M AdLib instruments, on a `cubed-opl`
  chip.
- `edit` (`cubed-tracker`, `cubed-ahx`, on by default): `cubed_tracker::edit` and `cubed_ahx::edit`, the
  module and song edit layers (need `serde_json`; nothing in a player calls them, so a worklet drops them
  at link time).

Every crate builds for `wasm32-unknown-unknown` with its default features and no JS
glue. There `cubed-dsp` seeds its random number generators from
`cubed_dsp::seed::set_seed`, which the host calls with entropy from its main thread
(the `wasm` feature uses the browser's crypto instead).

## Editing a synth: the catalogue, patch documents, live edits

A patch editor (the engine's Synth Studio) works on the DAW's patch JSON as data and
changes it while it plays. Four pieces in `cubed-dsp` and `cubed-mixer` do that; all
of it is plain Rust, native and `wasm32`.

**Catalogue** (`cubed_dsp::catalog`, `catalog_json()`). What an editor needs to know
about the node types the loader builds: the ports of each (numbers from `PortId`,
audio or control, which take modulation, which the engine wires itself), the parameters
(the serde keys of the patch's state structs, kind, range, default, unit, curve, the port
that modulates it), the eight effects, the modulation types and transforms. Tests check
it against the loader's node types, the state structs and the ports of built nodes;
`crates/dsp/tests/fixtures/catalog.json` is a copy that a test keeps equal
(`CUBED_UPDATE_CATALOG=1 cargo test -p cubed-dsp catalog` rewrites it).

**Patch documents** (`cubed_dsp::patch_edit::PatchDoc`). The patch JSON as a value:
`add_node(type) -> id` (with its default state and the wires the engine makes itself),
`remove_node`, `set_param(id, key, value)` (typed, clamped; returns the stored value and a
`ParamEdit` for the running synth), `connect(from, from_port, to, to_port, amount,
modulation, transform)` (an upsert; refuses unknown nodes and ports, loops, a control
signal into an audio input and the inputs the engine wires), `disconnect`, `set_amount`,
macro values and routes, `set_instrument_gain`, `set_voice_count`, effects
(`add_node` of an effect type, `remove_node`, `move_effect`), `set_editor`, `validate`,
`to_json`; `apply(&Value)` runs the same operations from a JSON description. Two keys are
the editor's: `editor` at the top (positions, view; kept untouched through edits, saving and
loading) and `synthState.effectChain` (the effects in processing order as ids; a patch
without it has the DAW's fixed chain, `"10000"` to `"10007"`). A connection may carry
`fromPort` (a `PortId` number) naming the output it leaves by; one without it takes the
node's first output as before. The native loader also reads `macros` (routes as the DAW's
`connect_macro`: a route replaces the macro's earlier one to that input, an amount of 0
removes it) and `instrumentGain`.

**Live edits.** `AudioEngine::edit_param(ParamEdit)` takes a change between blocks and never
allocates. The engine keeps a typed copy of each editable node's state and gives the changed
state to the node's update path, the one a patch takes at load. A small step of a number glides
over 8 blocks (a delay time over 32); a big step, and every parameter that is not a smooth number
(a waveform, an on/off, a filter type), ducks the output, applies the change while it is silent
and brings it back: about 8 ms. The instrument gain and the macros glide per sample over a block
(`process_with_frame_instrument`, `fill_macro_values`). `SynthSource` takes the edit as
`SourceEvent::Param(ParamEdit)` and plays through both. `ParamEdit` is `{ node: u128 (a UUID's
bits, or an effect's decimal id), param: u16 (the index in the catalogue), value: f32 }`; node 0
is the patch (parameter 0 the instrument gain, 1 to 4 the macros).

**Structural changes** (a node or a connection added or removed, an effect moved) build a new
synth from the edited document where allocating is allowed, and swap it in:
`SynthSource::from_patch_with(json, rate, &SynthBuild { held, warm_blocks, one_shot })` starts
the notes the old synth holds and renders `warm_blocks` blocks of 128 frames so their envelopes
are as far along as the old ones (a bass that decayed a second ago must not attack again; the
effects are skipped for all but the last `EFFECT_TAIL_BLOCKS`, which makes a long warm-up about
twice as fast), `PrepareSource` puts it in a slot, and `swap_commands(old, PlaySource,
fade_frames)` gives the batch `PlaySource` (gain 0), `FadeVoice` and `Stop(old)`; 1,500 to 2,500
frames hide it. The crossfade is linear, so two voices playing one note in different phases thin
out for a moment. Natively build and warm up on a thread. In a worklet the render callback is the
only thread, so the swap is sliced (below).

**Swapping in a worklet without a stall.** Send `{type: 'swap', source, bytes, play, oldVoice,
fadeFrames, held: [[note, velocity], ...], warmBlocks, sliceBlocks}`: `bytes` is the edited
patch JSON, `play` the wire-encoded `PlaySource` of the voice that takes over (its voice id, the
slot `source`, bus, gain, pan: `wire::encode` it), `oldVoice` the voice to replace (a number), `held` the
notes the old synth holds and `warmBlocks` how long they have been held, in blocks of 128 frames
(10 s is 3,750; the page can cap it where the patch has settled). The worklet builds the synth at
once (one build, a few milliseconds, in the message handler) and answers `{type: 'swap', phase:
'started'}` (or `'failed'` with `error`, e.g. the old voice is not playing). Then, each render
quantum, before rendering it, it warms the new synth `sliceBlocks` blocks (default 4, at most 32)
while the old voice plays on; the new synth renders faster than real time, so it catches up with
the clock in about `warmBlocks / (sliceBlocks - 1)` quanta (4 blocks a quantum: 10 s of notes in
3.3 s). Heavy patches want a small slice: there is no clock in a worklet to measure the load.
When it has caught up it takes over, in one batch at a block boundary (`swap_commands`), and the
page gets `{type: 'swap', phase: 'done', old, new, frame}`. In the meantime the page keeps
sending notes and edits for the old voice as usual: the worklet copies them to the warming
synth at the moment they would sound there, and after the swap commands that name the old id
(events, `Stop`, gain, ...) are rewritten to reach the new voice, so the page may keep using
the old id or switch to `new`. A newer `'swap'` replaces a pending one; `Stop` of the old voice,
or the old voice ending, gives it up (`{phase: 'cancelled'}` if the voice ended). Natively the
same pieces are `SynthSource::from_patch_with` and `swap_commands`.

**Wire and worklet.** New: command tag 29, `SOURCE_PARAM` (voice `u64`, node `u128`, param `u16`,
value `f32`, little endian); tag 24 (`SOURCE_EVENT`, notes) is unchanged, so older senders keep
working. `cw_held_buffer(count)` gives room for held notes (a note number and a velocity, two
floats each) and `cw_prepare_synth_held(source, one_shot, warm_blocks, held_count)` builds the
synth with them (`cw_prepare_synth` is unchanged). The processor's `'synth'` message takes
`held: [[note, velocity], ...]` and `warmBlocks` (at most 32 are used). Parameter edits and the
native swap's `PlaySource`, `FadeVoice` and `Stop` are ordinary `'commands'`. For the sliced swap:
`cw_swap_play_buffer`, `cw_swap_begin(source, one_shot, old_voice: f64, fade_frames, warm_blocks,
slice_blocks, held_count)`, `cw_warm_step(frames)` (call it ahead of `cw_process` each quantum:
0 idle, 1 warming, 2 took over, 3 given up), `cw_swap_old`, `cw_swap_new`, `cw_swap_pending`.

## Analysis: taps, spectrum, levels

What an editor shows of the sound (a scope, a spectrum, level meters) comes from
`cubed-mixer`, the same API natively and on the web.

**Taps** (`cubed_mixer::Tap`). A bus can keep its latest frames, after its fader (what its
meter reads), in a single-writer ring of atomics: written once per block on the audio thread
(a copy, no allocation, no lock), read from any thread by copying out and checking the writer
did not lap the read. A bus has no tap until the host gives it one with `Command::SetTap(bus,
Some(tap))`, so a bus nobody looks at costs nothing; `SetTap(bus, None)` takes it away. A tap
replaced, removed or on a removed bus comes back as `Returned::Tap`, to be freed off the audio
thread. `Tap::new(frames)` makes one (4,096 frames is the default, `TAP_FRAMES`); `TapSet` is the
host's table of them.

**Reading** (`cubed_mixer::Analysis`), from the main thread, once per frame:

```rust
trait Analysis {
    fn sample_rate(&self) -> f32;
    fn scope(&self, bus: BusId, left: &mut [f32], right: &mut [f32]) -> usize; // latest frames, oldest first
    fn scope_position(&self, bus: BusId) -> u64;       // frames seen so far: changes when there is new data
    fn levels(&self, bus: BusId) -> Option<BusMeter>;  // peak and RMS per channel, linear
}
```

`scope` fills as many frames as fit in the shorter slice and returns that many (0 for a bus
without a tap, fewer while a tap fills up). Natively `NativeHost` implements it (4,096 frames
after `NativeHost::enable_tap(bus, true)`, which needs the bus to exist). On the web the worklet
posts a `'taps'` message about 40 times a second (init's `tapHz`) while any tap is on, with the
latest 2,048 frames of each (a transferable `Float32Array`: messages, not a `SharedArrayBuffer`,
which would need cross-origin isolation); `WebAnalysis` parses it (`update_taps(buses, written,
frames, data)`, and `update_meters(meters)` for the telemetry message's meters) and implements
the same trait over the latest message. Switch a tap on with the processor's `{type: 'tap',
bus, enable}` message (`cw_set_tap`); call `WebAnalysis::forget(bus)` when switching one off.
`levels` is the existing bus meter (peak falling 24 dB a second, RMS over 300 ms): the UI does
its own peak hold.

**Spectrum** (`cubed_mixer::Spectrum`). `Spectrum::new(2048)` (a radix-2 FFT of its own, nothing
to depend on); `log_bins(&mut self, left, right, rate, bins: &mut [f32], min_hz, max_hz,
floor_db)` sums the ends of the two channels to mono, applies a Hann window, and fills the bars
logarithmically spaced between the two frequencies with 0 to 1 (the bar's loudest frequency in
dBFS, `floor_db` at 0, a full-scale sine at 1; silence is 0; a bar narrower than an FFT bin reads
between its two nearest bins). It does not allocate. `smooth_bins(bins, &mut state, rise, fall)` is
the usual fast-rise slow-fall smoothing for a display.

## Live control of a playing module (a tracker editor's transport)

A pattern editor plays a MOD, S3M or XM while the user works on it: it starts and stops the song
from any row, loops a pattern or a few rows, mutes and solos channels, plays a note of an
instrument on the side, follows the playhead, and shows each channel's level and waveform. All of
it is in `cubed-tracker`'s `Player` and `cubed-mixer`, natively and in the worklet.

**The player** (`cubed_tracker::Player`; plain Rust, nothing allocates after `Player::new`).

| Call | What it does |
|---|---|
| `stop()` / `play()` | Stops where it is (notes die away in 3 ms, the position stays, a preview goes on) and starts again from that row. A song that has played to its end starts from the top. A new player is playing. |
| `seek(order, row)` | Starts again from there in a clean state; speed and tempo are those the song reaches there. Works while stopped: the position moves, `play()` starts there. A live seek fades the notes. Sustained notes of earlier rows are not carried over (as in the DAW). |
| `set_loop(Loop::{None, Pattern(order), Range{order,row0,row1}})` | Repeats a pattern, or rows `row0..=row1`, of the pattern at `order`. It does not move the playhead: seek first. The song plays on until it reaches the loop's pattern, then wraps there; a row sought to beyond the range plays once first. While a loop covers the pattern a position jump (`Bxx`) is ignored and a break (`Dxx`) goes to its row inside the loop (or the loop's first row); E6x and pattern delays work as usual. |
| `set_mute(ch, bool)`, `set_solo(ch, bool)`, `set_mute_mask(u32)`, `set_solo_mask(u32)` | A muted channel keeps running unheard (an AdLib channel's note stops). With any solo, only soloed channels are heard, muted or not (the DAW's rule). Channels an S3M file switches off start muted (`mute_mask()`). |
| `preview_on(instrument, midi, velocity, duration) -> bool` / `preview_off()` | Plays an instrument (a cell's 1-based number: a MOD or S3M sample, an XM instrument, whose keymap picks the sample by the note) at a MIDI note on a spare channel, over the song or alone while it is stopped, whatever is muted. `velocity` 0-1 scales the sample's own default volume (1 plays as in a song); `duration` frames after it starts it lets go (0 holds until `preview_off`); XM envelopes and fadeout run, other formats fade in 10 ms; a new preview cuts the last. AdLib instruments play on a free chip channel (none is free when a song uses all nine). Returns false when the instrument has no sample for the note. |
| `set_cell(pattern, row, channel, Cell) -> bool` | A live edit of what the song will play when it reaches the cell. Shape changes (a pattern's rows, the order list, samples) need a new player. |
| `status() -> Status` | `{playing, preview, order, pattern, row, tick, speed, tempo, rows, frames, version}`: the row playing (or the stopped song's), the tick of the row, ticks per row, BPM, rows started so far (a follower that polls slower than the song counts the rows it missed), frames rendered. |
| `attach_probe(Arc<Probe>)` | Shares the state with other threads (below). |

Notes are MIDI numbers in the player's API: a MOD plays the period its table has for the note (ProTracker's
C-1 to B-3, or seven octaves when the file is not limited to them), an S3M C-0 to B-7, an XM C-0 to B-7.

**The probe** (`cubed_tracker::Probe`, made with `Probe::new(channels)` off the audio thread, attached
with `attach_probe`): every field is an atomic, so any thread reads it without waiting, and the position
is one packed word, so a row, a tick and a tempo always belong together. `status()` is published at every
tick; `levels(&mut [f32])` gives each channel's peak (the loudest frame of the latest block after the
master volume and trim, falling 24 dB a second after it, 0 below -60 dB; a muted channel reads 0; an
AdLib channel's is an estimate from its key and volume) and then the preview's; `scope(channel, &mut
[f32])` gives the latest `SCOPE_FRAMES` = 512 frames of a channel (mono, oldest first, before the master
volume; an AdLib channel's is flat) and `scope_position(channel)` how many it has written. The scope
rings are written (one copy per channel per block) only while `set_scopes(true)`. `status().version`
changes whenever anything shown has moved and stays put while everything is still.

**In the mixer** (`cubed_mixer`, feature `songs`): `songs::open_module_song(format, bytes, rate,
options, stopped)` (or `open_module(module, ...)` for a module the host holds) makes a source that
takes `SourceEvent::Song(SongControl)`. With `stopped` it opens under transport control: silent until
`Play`, and its voice never ends by itself (at the end of a song that does not repeat, the transport
stops at the top). `open_song` is unchanged: it plays at once and ends with the song. The host asks the
source for its reader before handing it over: `let reader: Arc<dyn SongReader> =
source.song_reader().unwrap();`, then `PrepareSource` and `PlaySource` as for any source.

```rust
enum SongControl {
    Play, Stop, Seek { order: u16, row: u16 },
    Loop(SongLoop),                                   // None | Pattern { order } | Range { order, row0, row1 }
    Mute { channel: u8, on: bool }, Solo { channel: u8, on: bool },
    MuteMask(u32), SoloMask(u32),                     // every channel at once, a bit each
    PreviewOn { instrument: u8, note: u8, velocity: f32, duration: u32 }, PreviewOff,
    Looped(bool),                                     // restart at the end of the song
    Scopes(bool),                                     // fill the channel scope rings
    SetCell(SongCell),                                // a live cell edit
}
trait SongReader: Send + Sync {                       // natively: the song's probe; on the web: WebSong
    fn status(&self) -> SongStatus;                   // as Player::status, plus `version`
    fn channels(&self) -> usize;
    fn levels(&self, out: &mut [f32]) -> usize;       // the channels' peaks, then the preview's
    fn scope(&self, channel: usize, out: &mut [f32]) -> usize;   // mono frames, oldest first, <= 512
    fn scope_position(&self, channel: usize) -> u64;
    fn scopes_enabled(&self) -> bool;
    fn alive(&self) -> bool;                          // false once the song's voice has been freed
}
```

**Wire** (every control is an ordinary `Command::SourceEvent`, so a worklet takes it in a `'commands'`
batch): tag 30, 24 bytes, little endian: `u8 tag, u64 voice, u8 kind, u16 a, u16 b, u16 c, f32 value,
u32 frames`. Kinds: 0 Play, 1 Stop, 2 Seek (a order, b row), 3 Loop none, 4 Loop pattern (a order), 5 Loop
range (a order, b row0, c row1), 6 Mute (a channel, b on), 7 Solo (a, b), 8 MuteMask (frames), 9 SoloMask
(frames), 10 PreviewOn (a instrument, b note, value velocity, frames duration), 11 PreviewOff, 12 Looped
(b on), 13 Scopes (b on). Tag 31, 21 bytes: `u8 tag, u64 voice, u16 pattern, u16 row, u8 channel, u8 note
(0 none, 255 key off, else MIDI), u16 period, u8 instrument, u8 volume, u8 effect, u8 param` (SetCell). An
unknown kind stops the batch at it (`WireError::UnknownTag(30)`); the other tags are unchanged.

**The worklet.** The page opens a module with the processor's existing `'song'` message plus `stopped:
true` (open it stopped and under control) or `follow: true` (follow it while it plays), for format 3, 4
or 5; the reply is the usual `{type: 'song', source, ok, error?}`. Then it plays the source and steers it
with `'commands'`. The worklet posts, about 60 times a second (init's `songHz`) and only for the followed
songs that moved:

- `{type: 'songpos', frame, sources: Uint32Array, status: Uint32Array, levels: Float32Array}`: for song i
  its source slot, 12 status words (flags playing/preview/alive/scopes on, order, pattern, row, tick,
  speed, tempo, rows, frames low and high, version, channels; `SongStatus::to_words`) and 33 levels. (The
  type is not `'song'`, which is the reply to a prepare and has an `ok`.) A song gone is reported once
  more with alive 0: its voice was freed by the first message handler after it ended; a page that tracks
  its voices sees `Ended` sooner.
- `{type: 'songscope', frame, frames: 512, sources, channels: Uint32Array, written: Float64Array, data:
  Float32Array}`, with each `'songpos'` for the songs whose scopes are on: per song its channel count, in
  `data` each channel's 512 latest frames, in `written` how many each ring has seen.

On the page, `WebSongs` (one `WebSong` per source slot; `update_positions(sources, status, levels)`,
`update_scopes(frames, sources, channels, written, data)`, `get(source) -> Option<&WebSong>`, `forget`)
parses them, and a `WebSong` is a `SongReader`, so the editor reads natively and on the web the same
way. Exports for hosts that drive the wasm themselves: `cw_prepare_module(source, format, subsong,
looped, separation, flags)` (flags bit 0 = stopped), `cw_songs_prepare()` (call it in the render
callback's thread: allocation-free) with `cw_song_sources`, `cw_song_status`, `cw_song_levels` and
`cw_song_scope_*` pointers, `cw_song_count`, and the size getters `cw_song_status_words`,
`cw_song_level_slots`, `cw_song_scope_frames`. As everywhere in the processor, every pointer is
taken before a view of `memory.buffer` is built.

**Editing while it plays.** A cell edit is a `SetCell`. Anything that changes the module's shape is
a new song: build the edited module (`open_module(.., stopped = true)`), `PrepareSource` it in another
slot, and send one batch: `swap_commands(old, PlaySource{..}, fade)` followed by `Seek` to the old
song's position, the same `Loop`, mute and solo masks and `Play` for the new voice. The old voice
fades out under the new one, which starts the row from a clean state.

## Editing and writing tracker modules (a pattern editor's data layer)

`cubed_tracker::edit::ModuleDoc` holds a `Module` (MOD, S3M or XM) and edits it the way the original
trackers did, in the style of `PatchDoc`: typed methods and one JSON entry point, `apply(&Value) ->
Result<Value, String>`. Every change is checked against the format's limits and a change that does not
fit is refused with a message and leaves the module as it was (a failed `batch` leaves none of its
operations). `to_bytes(format)` writes the module (the module's own format only: nothing converts
between formats), `from_bytes` reads one, `ModuleDoc::new(format)` makes an empty one, `module()`
borrows the `Module` to play (`cubed_mixer::songs::open_module(doc.module().clone(), ...)`).

**Numbers.** Patterns, rows, channels, orders count from 0; instruments and samples as a cell names them
from 1 (0 is none); inside an XM instrument its samples count from 0. Ranges (`row0..row1`,
`channel0..channel1`) include both ends. **A cell** is `[note, instrument, volume, effect, param]`:

| field | meaning |
|---|---|
| `note` | 0 none, 255 key off, else a MIDI number (60 is middle C) in every format. MOD: C-1 (24) to B-3 (59), or seven octaves down to 1 when the module is not limited to ProTracker's periods. S3M: C-0 (24) to B-7 (119). XM: C-0 (12) to B-7 (107). Trackers count octaves differently: `info().note.octave_offset` is what to take from `note / 12` (MOD, XM 1: 60 is `C-4`; S3M 2: 72 is `C-4`, as ST3 shows it). `note_name`/`parse_note` do the text. MIDI 0 cannot be told from an empty note: a 7-octave MOD's lowest note is 1. |
| `instrument` | MOD, S3M: the sample number; XM: the instrument. Must exist. |
| `volume` | The volume column byte as the file holds it, 0 for none: 0x10-0x50 is volume 0-64; an XM's 0x60-0xFF are its volume-column commands. A MOD has none (use `C`). |
| `effect`, `param` | The command number as the file holds it: MOD and XM `0`-`9`, `A`=10 .. (XM: `G`=16, `H`=17, `K`=20, `L`=21, `P`=25, `R`=27, `T`=29, `X`=33), S3M `A`=1 .. `Z`=26. Operations also take the letter as text. A MOD's are 0-F; an S3M's `C` takes two decimal digits (ST3 drops others). |

A MOD cell also keeps the Amiga period; it follows the note (ProTracker's table) and `copy_range` adds the exact
period as a sixth number when a cell's is not the table's. Reads: `info()` (format, title, sizes, speed and tempo,
flags, note range, limits, history state), `rows(pattern, row0, count, flat)` (`cells[row][channel]`, or one flat
array), `order()`, `samples()`, `instruments()` (a MOD's or S3M's sample slots are instruments of one sample),
`sample_peaks` (min/max buckets for a waveform), `sample_wav`, `validate()` (what the format cannot hold, and what
the writer refuses). Every JSON edit returns `{"changed": {"patterns": [{pattern, row0, row1}], "layout",
"orders", "header", "channels", "instruments": [n], "instrument_list"}}` and its own fields.

**Operations** (`{"op": ..., fields}`):

| group | ops |
|---|---|
| cells | `set_cell` (`pattern`, `row`, `channel`, `cell: [..]` or any of `note`, `instrument`, `volume`, `effect`, `param`; notes may be `"C-4"`, `"off"`, effects letters), `clear_range` (+ `columns`), `insert_row` / `delete_row` (`row`, `count`, optional channel range; rows pushed off the end are lost), `transpose` (range, `semitones`, `clamp`, `instrument`; a note out of the format's range refuses the edit unless `clamp`), `copy_range` -> clip, `paste` (`cells`, `mode`: `overwrite` or `mix` which fills only empty fields; a cell the format cannot hold refuses the paste, one running past the pattern is cut and `clipped` says so) |
| patterns | `set_pattern_rows` (XM 1-256; MOD and S3M are 64; shortening over data needs `force`), `new_pattern` (appends), `duplicate_pattern`, `delete_pattern` (its orders go, later numbers shift) |
| orders | `set_order`, `insert_order`, `delete_order`, `move_order`, `set_restart` (XM; a MOD or S3M has none: use `Bxx`) |
| song | `set_speed`, `set_tempo` (a MOD has no header field: they become `Fxx` on the first row), `set_title`, `set_global_volume` (S3M), `set_flag` (`linear_frequency` XM; `amiga_limits`, `fast_volume_slides` S3M; `amiga_limits`, `vblank_timing` MOD), `set_channels` (MOD 1-32 by signature, 4 in the Soundtracker layouts; S3M, XM 1-32; dropping channels with data needs `force`), `set_channel` (S3M pan 0-15, mute), `set_mod_flavor` |
| samples | `set_sample` (`name`, `volume`, `finetune` MOD -8..7 / XM -128..127, `relative_note`, `pan`, `c2spd` S3M, `loop_kind` `off`/`forward`/`pingpong` (XM only), `loop_start`, `loop_len`, `adlib` registers; loops are fitted into the data, to whole words in a MOD), `import_sample`, `normalize_sample`, `reverse_sample`, `clear_sample`, `generate_wave` (sine, triangle, saw, square, pulse, noise) |
| instruments | `add_instrument` (XM with one empty sample, S3M an empty slot; a MOD's 31 are fixed), `remove_instrument` (cells that play it are cleared and later ones renumbered; refused while any does unless `force`), XM: `add_sample`, `remove_sample`, `set_instrument` (`name`, `fadeout` 0-4095, `vibrato_*`), `set_envelope` (`kind`, `enabled`, `points` as [tick, value] at most 12 from tick 0, `sustain`, `loop`), `set_keymap` (`from`..`to` as MIDI notes, `sample`, or all 96) |
| history | `begin_group`, `end_group`, `undo`, `redo`, `batch` (`ops`: one undo step, all or none) |

**Loading audio** (`import_sample`, Rust `import_sample(SampleRef, bytes, &ImportOptions)`): the audio is a WAV file
(PCM 8/16/24/32-bit, float; any channels, mixed down or one side via `mix`; the `smpl` loop is kept) or raw PCM
(`pcm8`, `pcm8u`, `pcm16`, with `rate` and `channels`); `data` is base64 in JSON. A MOD sample is resampled
(windowed sinc, or `linear` as the DAW does) to the Paula rate of the note it should sound at its own pitch
at (`note`, default C-2, 8287 Hz), 8-bit, whole words. An S3M keeps the rate as `c2spd` (or resamples to
`target_rate`; 8-bit by default, 16 with `bits`). An XM keeps the rate too, in the relative note and finetune
exactly as the DAW's `fromPcm` does (16-bit by default). Audio longer than the format holds is refused
(`truncate` cuts it); `normalize` scales to the grid's full scale.

**Undo and redo.** An inverse-change log (`edit/history.rs`): before an edit overwrites anything it records the
change that puts it back (a rectangle of cells, one pattern, the order list, a sample's fields, its data moved
out rather than copied), so typing a note costs one cell and a volume tweak on a 2 MB sample costs nothing for
its data. Applying a change returns its own inverse, which is how redo comes for free. `begin_group` /
`end_group` nest: one user action is one step; `undo()` / `redo()` return a `Changes` (pattern and rows,
`orders`, `header`, `instruments` ..., and the exact `cells`) to redraw, and do nothing while a group is open. The log is bounded by steps
(1000) and bytes (128 MiB), the oldest dropped first (`set_history_limits`); `revision()` counts every change.

**Live edits.** A song playing the module takes cell edits without reopening: after an edit,
`doc.live_edits() -> Option<Vec<CellEdit>>` is `Some` with the current content of each cell that changed when the
edit changed nothing but cell content (typing, clear, paste, transpose, insert or delete rows, a MOD's speed, and
undo or redo of those) and `None` for anything that moves the module's shape (patterns, orders, header, channels,
samples, instruments, or more than 1024 cells): open the edited module as a new song. `CellEdit` is field for field
`cubed_mixer::SongCell` (`pattern` number, `row`, `channel`, `note` 0/255/MIDI, `period`, `instrument`, `volume`,
`effect`, `param`), and `CellEdit::cell()` is what `Player::set_cell` takes. `{"op": "live_edits"}` returns the same
as `{"cells": [..] | null}`. A test keeps a `Player` in step with a `ModuleDoc` through an edit session and its
undo this way.

**Writers** (`write_mod`, `write_s3m`, `write_xm`, `write(&Module)`) are the inverses of the parsers:
`load(write(m)) == m` for every module a parser produced and every state an operation can reach (up to the
MOD's flags the file cannot hold, below). What a file cannot hold is an error naming the cell or sample: a
note outside a MOD's periods, effects over F in a MOD, a MOD pattern that is not 64 rows, a ping-pong loop in a
MOD or S3M, an odd-length MOD sample, more channels or patterns than the format, envelopes on an XM instrument
with no sample. Choices: samples are written at the narrowest width that keeps every value (S3M, XM: 8-bit on the
8-bit grid, else 16; a MOD's are 8-bit, which parsing guarantees); text is Latin-1 cut to the field; a MOD gets
`M.K.` (4 channels, limited to ProTracker's octaves), `N.T.`, `4CHN`, `6CHN`, `8CHN`, `10CH` ..., or its
Soundtracker layout (15 samples, 600-byte header; the file cannot say Soundtracker from Ultimate Soundtracker
or ProTracker-shaped, so the writer refuses a flavor that would load back as another: `set_mod_flavor` converts
to 31 samples first); its restart byte is 0x7F and a pattern past the last one played is kept by an order-table
entry beyond the song's end. An S3M is written with unsigned samples (format 2), its channel settings, pan table
and master volume rebuilt from the channel pans, mutes and AdLib channels, its order list from `orders` and
`jump_targets` (separators as 254, ended by 255; the first order edit folds the map into the `Bxx` jumps). ST3 plays
channels that share a hardware channel as one: a channel merged away on load stays empty and writes fine, one
with notes is refused. An XM is version 1.04, tracker name `FastTracker v2.00`, patterns packed as the DAW packs
them. `Module::differences(&other)` lists how two modules differ (it is how the writers are checked), and
`restart` is new in `Module` (the XM restart position, which `Player` now loops back to).

*Checked* (`tests/write.rs`, `tests/edit.rs`): `load(write(load(x)))` equals `load(x)` on the three synthetic fixtures
(their written bytes are pinned by hashes) and on 330 real songs (`CUBED_MODULES=<dir>`, searched recursively:
ProTracker, both Soundtracker layouts, S3M with separators and AdLib, XM); a random-operation soak (every operation
kind, grouped and not, on the fixtures, on empty modules, and with `CUBED_MODULES` on every real song) saves and
reloads after every step, undoes everything to the start and redoes to the end. `cargo run --example tracker_write`
writes a directory back, `tracker_diff` compares two directories by content, `tracker_edit` applies an operation list.

*Against the DAW's exporters* (`mod-writer.ts`, `xm-writer.ts`; the DAW's S3M exporter is a placeholder, so
`write_s3m` follows the format and the OpenMPT/ST3 files above): on 135 MODs 106 are byte-identical and 115 load to
the same module; the others differ where the DAW changes the song (it writes a Soundtracker module in the
31-sample layout, so the flavor and the sample count differ; it re-derives a few loops and periods; it keeps the
order table beyond the song's end and a truncated sample's header length where this writes what is there). On 113
XMs none is byte-identical and all 113 load to the same module: the DAW keeps the source's tracker name, each
sample's bit depth (this writes the narrowest exact width) and the points of a *disabled* envelope, which
`Module` does not hold (an envelope is `None` when off, so loading an XM drops them: a known limit).

*Not covered*: converting between formats (a MOD to an XM; `to_bytes` writes the module's own format);
S3M drum channels (channel settings 25-29 load as plain channels); the points of a disabled XM envelope; a MOD's
`vblank_timing` and `amiga_limits` are not in the file (loading derives them from the song, so an edit can make a
reload differ in those flags; `info().flags` and `set_flag` show and set them); an S3M pan that is not on the
pan table's 16 steps (or ST3's default left/right) is written as the nearest step.

## Editing and writing AHX / HVL songs (a tracker editor's data layer)

`cubed_ahx::edit::AhxDoc` holds a `format::Song` (AHX `THX` or HivelyTracker `HVL`) and edits it the way the trackers
do, in the style of `ModuleDoc`: typed methods and one JSON entry point, `apply(&Value) -> Result<Value, String>`.
Every change is checked against the format's limits and a change that does not fit is refused with a message and
leaves the song as it was (a failed `batch` leaves none of its operations). `to_bytes()` writes the song,
`from_bytes` reads one, `AhxDoc::new(format)` / `create(&json)` make an empty one, `song()` borrows the `Song` to play
(`AhxPlayer::from_song(doc.song().clone(), rate, stereo)`).

**The model.** A song is a list of *positions*; each names, for every channel (AHX 4, HVL 4 to 16), a *track* and a
*transpose* (-128..127 semitones). A track is `track_length` (1-64) rows of steps; tracks are shared, so two positions
can play one and editing it edits both (`make_unique`, or `"unique": true` on an edit, gives a cell a track of its
own). Track 0 is the track every empty cell points at: while it is blank it takes no writes. *Instruments* (1 to 63,
numbered without gaps) have a volume, wave length, envelope (attack, decay, sustain, release as frames and levels),
filter sweep limits and speed, square sweep limits and speed, vibrato, hard cut, and a *PList* (up to 255 rows, one
program of rows that picks pitch, waveform and two commands per tick, with its own speed). Limits: 1000 positions, 256
tracks, 63 instruments, 255 subsongs and the file's 16-bit string table offset, which caps the whole song at 65,535
bytes before its names (`info().size`; growth past it is refused, shrinking never is).

Positions, tracks, rows, channels and PList rows count from 0; instruments from 1 (0 is none). Ranges include both
ends. **A step** is `[note, instrument, fx, param, fxb, fxb_param]`, always six numbers (`fxb`, HVL's second effect
column, is 0 in AHX, which refuses anything else): `note` 0 none, 1-60 C-1 to B-5 (MIDI is `note + 23`), 61-63 C-6 to
D-6 (the engine plays them as 60; an HVL step cannot hold 63, that byte marks a blank step in the file); `fx` 0-F,
`param` 0-255. Operations take `"C-3"`, `"C#2"`, `"---"` as well as numbers (`note_name`, `parse_note`). **A PList row**
is `[note, waveform, fixed, fx0, param0, fx1, param1]`: waveform 0 (keep) to 4 (triangle, sawtooth, square, noise),
commands 0-5, C, F in AHX and 0-F in HVL (a version-0 AHX file keeps only the low nibble of command 4's parameter).

Reads: `info()` (format, version, name, channels, counts, speed multiplier, `limits`, `size`, history state),
`rows(track, row0, count, flat)`, `position_rows(position, row0, count, flat)` (`cells[row][channel]` plus the tracks and
transposes: what the pattern grid draws), `positions(first, count)`, `tracks()` (usage and blank flags),
`instruments()` (with how many steps play each), `instrument(n)`, `plist(n, row0, count, flat)`, `validate()`,
`instrument_bytes(n)` (the wire form `AhxPlayer::replace_instrument` takes). What an instrument's controls draw is
simulated with the replayer's own code, so a drawing cannot drift from what plays: `envelope_curve(n)` (the level
per tick and where each stage ends), `waveform_shape(waveform, wave_length, square_pos)` (one cycle before any filter),
`sweep_trace(n, kind, frames, sign, start)` (the filter's or the square's position per tick) and `vibrato_curve(n,
frames)`. Every JSON edit returns `{"changed":
{"tracks": [{track, row0, row1}], "positions": {first, last}, "position_list", "layout", "header", "restart",
"instruments": [n], "plists": [{instrument, row0, row1}], "instrument_list"}}` and its own fields.

**Operations** (`{"op": ..., fields}`; `<track>` is `"track": n` or `"position": p, "channel": c`, optionally with
`"unique": true`):

| group | ops |
|---|---|
| steps | `set_step` (`<track>`, `row`, `cell` or any of `note`, `instrument`, `fx`, `param`, `fxb`, `fxb_param`), `clear_range` (+ `columns`), `insert_row` / `delete_row` (rows pushed off the end are lost), `transpose` (range, `semitones`, `clamp`, `instrument`; a note outside C-1 to B-5 refuses the edit unless `clamp`), `copy_range` -> clip, `paste` (`cells`, `mode` `overwrite` or `mix`; a step the format cannot hold refuses the paste, rows past the end are cut and `clipped` says so) |
| tracks | `set_track_length` (shortening over steps needs `force`), `alloc_track` (`copy_of`, `append`: reuses an unused blank track unless `append`), `compact_tracks` (drops the unused ones and renumbers) |
| positions | `assign_track`, `set_transpose`, `make_unique`, `insert_position` (`blank`, `duplicate`, `duplicate_unique`) / `delete_position` / `move_position` (each returns `map`: how old positions moved, and the restart and subsong starts follow), `set_restart`, `set_subsongs` |
| song | `set_name` (Latin-1: NUL removed, anything above U+00FF becomes `?`), `set_speed_multiplier` (1-4), `set_mix` (HVL: `mixgain`, `defstereo`) |
| instruments | `add_instrument` (fields over the DAW's default saw note), `set_instrument` (`n` and any fields, the envelope as an object), `copy_instrument` (`from`, `to` or a new one), `clear_instrument`, `rename_instrument` |
| PList | `set_plist_row`, `insert_plist_row` / `delete_plist_row` / `duplicate_plist_row`, `clear_plist_range`, `copy_plist` / `paste_plist` (rows past the end are appended), `transpose_plist`, and the editor's shortcuts `set_start_waveform` (row 0's waveform is "the instrument's waveform"), `set_start_filter`, `enable_sweep` (PList command 4 on row 0, and the square wave for a square sweep) |
| history | `begin_group`, `end_group`, `undo`, `redo`, `batch` (one undo step, all or none; a multi-channel selection is a batch of one op per channel) |

**Undo and redo** are an inverse-change log (`edit/history.rs`), as in `ModuleDoc`: before an edit overwrites anything it
records the change that puts it back (rows of a track, a position's entry, the header, an instrument's fields,
rows of a PList), so typing a note costs one step, not the song. `begin_group` / `end_group` nest; `undo()` /
`redo()` return a `Changes` to redraw; the log is bounded by steps (1000) and bytes (128 MiB), the oldest dropped first
(`set_history_limits`).

**Live edits.** After an edit, `doc.live_edits() -> Option<LiveEdits>` says whether a player that holds the song as it
was can follow without being reopened, and `doc.apply_live(&mut player, &edits)` does it (call it from a message
handler: it allocates what it appends):

| what changed | live? | what the player does |
|---|---|---|
| a step (typed, cleared, pasted, transposed, rows inserted or deleted) | yes | `AhxPlayer::set_step`: read when the song reaches the row (the row playing now is not retriggered) |
| a position's track or transpose | yes | `set_position_entry`: refreshed at the next row |
| the restart position | yes | `set_restart` |
| an instrument's fields, envelope, sweeps, vibrato, PList | yes | `replace_instrument`: a voice holding it takes the PList rows and the envelope's stage lengths and levels at once, what a trigger copies (volume, wave length, vibrato, hard cut, PList speed, sweep set-up) at its next trigger |
| a track or an instrument added at the end (a cell given its own track), and their undo | yes | `push_track` / `pop_track`, `push_instrument` / `pop_instrument` (room for 256 tracks and 64 instruments is reserved when the engine is made) |
| a rename, the song's name | yes, nothing to do | names make no sound |
| the track length, `compact_tracks`, positions inserted, deleted or moved, the speed multiplier, the HVL mix, the subsong list, or more than 1024 steps at once | no (`None`) | `AhxPlayer::reload_song(doc.song().clone(), place)` swaps the edited song in place and carries the playhead through `PositionMap::apply`; or open a new player |

A test keeps a player level with the document through an edit session (every kind of edit, its undo and redo) and checks
that after a seek it renders what a fresh player of the edited song renders.

**Writers** (`write::write(&song, base)`, `write_with(&song, &Origin)`, `write_instrument`, `convert_hvl_to_ahx`) are
the parser read in reverse, and port another_synth's `serializeAhx` and `convertHvlToAhx`. HVL is read and written by
the same model as AHX (`Song::format`), so `AhxDoc` edits both and `to_bytes()` writes the song's own format;
`to_ahx_bytes()` converts an HVL song to AHX when it fits (4 tracks, one effect column, notes and instruments up to 63,
AHX-writable instruments, no `EF1` in a version-1 song) and says why when not. `Origin` holds what the parse forgets
(the blank-first-track flag, the loader-ignored instrument bits) and is read from the file a song came from
(`AhxDoc::from_bytes` does it); without one the writer guesses. A parse normalisation is not undone: a restart at or past
the position count is stored as the last position, an AHX subsong start past the end as 0, a version-0 filter-toggle
parameter loses its high nibble, an HVL step written out in full but all zero becomes the one-byte blank escape. Names
are Latin-1 (the parser used `from_utf8_lossy` before, which turned every byte over 0x7f into U+FFFD and read the next
instrument names from the wrong place).

*Checked* (`tests/ahx_write.rs`, `tests/ahx_edit.rs`): `parse(write(parse(x))) == parse(x)` on all 127 fixtures with and
without the file, and 124 of 127 come back byte for byte (the rest differ in a trailing byte, a missing final NUL, and a
version-0 filter nibble); a random-operation soak (every operation kind, grouped and not, refused ones included) on a new
AHX, a new HVL, `karma.ahx` and an HVL fixture saves and reads back as itself after every step, undoes to the start and
redoes to the end. Against the DAW's `ahx-writer.ts` (`examples/ahx_write_check` writes a directory; the DAW's writer was
run over the same files with its own esbuild bundle and `cmp`): all 127 outputs are byte-identical, both with the base
file and without it, and the conversion refuses the same 22 HVL songs with the same words. `CUBED_AHX_SONGS=<dir>` adds
your songs to the round-trip tests.

*Not covered*: the DAW's instrument presets (`ahx-presets.ts`; a host adds one with `add_instrument` and its fields);
removing or swapping instruments (they would renumber every step: clear the slot instead); changing an HVL song's
channel count; HVL songs wider than 16 channels have no document (the engine never plays past 16; they still parse and
write); subsongs are a list of start positions only.

## Live control of a playing AHX / HVL song

Everything below is in `cubed-ahx` (`AhxPlayer`, `AhxEngine`); `cubed-mixer` and the worklet do not forward it yet.
Nothing allocates after the first block of a size (`tests/ahx_control.rs` in `cubed-rt-check` runs every control
through blocks, with and without hi-fi, and counts).

| Call | What it does |
|---|---|
| `play()` / `pause()` / `resume()` | `resume` starts a song that has played to its end from the top. A paused song is silent apart from an audition |
| `seek(position, row)` | (existing) replays the subsong to there without mixing, now in place: no allocation |
| `loop_positions(first, last)`, `loop_rows(position, row0, row1)`, `loop_off()`; `set_loop(Loop::{None, Position, Positions, Rows})` | Repeats a position, a run of positions (the last wraps to the first) or rows of one position. It does not move the playhead: seek first. The song plays on until it reaches the loop. A `Bxx` out of the loop wraps to its start, one inside it is kept, `Dxx` keeps its row inside a rows loop, and a looping song never reaches its end. Kept across seeks and rewinds. (The old `set_loop_position(true)`, loop whatever position is playing, is unchanged.) |
| `set_mute(ch, on)`, `set_solo(ch, on)` (and the existing mask call) | A muted channel keeps running unheard; with any solo only soloed channels are heard |
| `play_position()`, `play_row()`, `play_tick()`, `rows_started()`, `status() -> AhxStatus` | Where the tick rendered last was (the transport itself has already moved on at a row's last tick). `status`: playing, audition, song end, position, row, tick, speed (ticks per row), speed multiplier, rows started, frames, version |
| `set_levels(true)`, `levels(&mut [f32])` | Each channel's peak, linear (1.0 full scale), then the audition's: the loudest wave sample the channel could make of its volume, pan and the mix gain at the latest tick (a bound), falling 24 dB a second; a muted channel reads 0 |
| `audition_on(instrument, midi, velocity, duration_frames) -> bool`, `audition_off()`, `audition_active()`, `audition_plist_row()` | Plays an instrument (1-based) at a MIDI note (24 is C-1, 83 is B-5) on a voice of its own, mixed over the song or alone while it is stopped, whatever is muted. It has its own tick clock, plays through the plain oscillators (hi-fi or not), is centred, runs the envelope, sweeps, vibrato and PList, and ends after its release; `duration_frames` 0 holds it until `audition_off`; a new note cuts the last |
| `attach_probe(Arc<AhxProbe>)` | `render` publishes the status and levels at the end of every block into atomics (`AhxProbe::status()`, `levels()`, `alive()`), readable from any thread; the position is one packed word so a row, tick and speed always belong together |
| `set_step`, `set_position_entry`, `set_restart`, `push_track`, `pop_track`, `push_instrument`, `pop_instrument`, `replace_instrument` | The live edits above |
| `reload_song(song, place)` | The rebuild: a new engine for an edited song, keeping mute and solo, loop, levels, hi-fi and the transport state, put at `place` |

## Building, testing, benchmarks

```sh
cargo test --workspace
cargo test -p cubed-dsp --no-default-features --features wasm   # another_synth's configuration
cargo bench -p cubed-sid --bench sid_render
cargo bench -p cubed-opl --bench opl_render
cargo bench -p cubed-ahx --bench ahx_render
cargo bench -p cubed-dsp --bench patch_render
cargo bench -p cubed-tracker --bench module_render   # CUBED_MODULES=<dir> adds yours

# The worklet in a real browser (headless Brave or Chromium):
cargo build -p cubed-worklet --target wasm32-unknown-unknown --release
node crates/worklet/test/run.mjs
```

The benches render in 128-frame blocks (an AudioWorklet quantum) and report the cost
of a block against its deadline. On a Ryzen 9 5950X, AHX/HVL songs take 1–3.3 µs per
block (800–2600× realtime), and 8 held voices of a two-oscillator unison/FM patch
through a 24 dB filter take about 270 µs dry and 365 µs with chorus, delay and
reverb (10% and 14% of the 2.67 ms block). The stable SIMD fallback matches
`std::simd` on the synth. On AHX it is about 30% slower, which is 0.01% of a block.
Modules take about 10 µs a block for 4 channels and 20–70 µs for 8–32 (an S3M with
AdLib channels 14–21 µs).

Test fixtures live under each crate's `tests/fixtures`: AHX/HVL songs (`cubed-ahx`),
Adlib Tracker II modules (`cubed-opl`), SID song files plus the instrument-visuals
parity fixture another_synth's TypeScript tests share and synthetic GoatTracker songs
with the DAW's import of them (`cubed-sid`, `tests/fixtures/goattracker`: its
`oracle.ts` reruns the DAW's importer over any corpus), and synthetic
MOD, S3M and XM modules with the scripts that write them (`cubed-tracker`).

One test fails, as it does in another_synth:
`cubed-ahx engine::tests::the_bounded_hold_builds_every_table_the_full_hold_does`
finds that the bounded live-prewarm hold misses tables for instrument 23 of
`shattered_crystal_drops.ahx`.

## History

Extracted from another_synth's `rust-wasm` crate (`audio_processor`) at commit
`f88f9962` (v0.4.77) on 2026-10-17. Each top-level module became a crate. Changes
beyond paths and the stable SIMD fallback:

- `cubed-dsp`: the native engine no longer prints every voice's parameters on every
  block (it cost 20% of the render time); a test module that had lost its
  `use super::*` (never built under the DAW's default `wasm` feature) builds again;
  the `native_demo` and `composition` binaries, which no longer built, were left out.

Paths of the form `.ai/...` in comments refer to design notes in the another_synth
repository.

## Licences

MIT. The OPL chip derives from ymfm (BSD-3-Clause, notice in `crates/opl/src/lib.rs`).
