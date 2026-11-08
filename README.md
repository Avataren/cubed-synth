# cubed-synth

The Rust audio code shared by the another_synth web DAW and the wgpu-cube engine:
chip emulators, module players and a modular synth engine. It is plain Rust that
builds on stable, natively and for `wasm32-unknown-unknown`, so the same code can run
in a browser AudioWorklet and in a native audio callback.

| Crate | What it is |
|---|---|
| `cubed-sid` | MOS 6581/8580 SID chip (with measured 6581 revisions), a GoatTracker-style song player for the `ASID` song file, GoatTracker 1 and 2 `.sng` import (`goattracker`, another_synth's importer ported byte for byte), and a register-stream chip player (`stream`) |
| `cubed-ahx` | AHX / HivelyTracker (`.ahx`, `.hvl`) parser and replayer, bit-exact against `hvl_replay.c`, with a band-limited "hifi" path |
| `cubed-opl` | Yamaha OPL2/OPL3 FM chip (a port of ymfm's OPL path), an Adlib Tracker II (`.a2m`) parser, writer and player (which takes live control: seek, loops, mute and solo, position, levels, live edits), `edit::A2mDoc` and `edit::OplInstrumentDoc` (a song and an instrument as data, with JSON operations and undo), and `audition::OplAudition`, an OPL instrument played by hand |
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
- `edit` (`cubed-tracker`, on by default): `cubed_tracker::edit`, the module edit layer
  (needs `serde_json`; nothing in a player calls it, so a worklet drops it at link time).
- `edit` (`cubed-opl`, on by default): `cubed_opl::edit`, the A2M song and OPL instrument edit layer
  (nothing in a player calls it, so a worklet drops it at link time).

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

## Editing Adlib Tracker II songs and OPL instruments (an Adlib editor's data layer)

`cubed_opl::edit::A2mDoc` holds an `A2mSong` (an `.a2m` file's song: 250 or 255 instruments, FM/arpeggio/vibrato
macro tables from version 9, patterns, the order list, the header) and edits it the way Adlib Tracker II does, in
the style of `ModuleDoc`: typed methods and one JSON entry point, `apply(&Value) -> Result<Value, String>`. Every
change is checked against what the song's *version* holds and a change that does not fit is refused with a message
and leaves the song as it was (a failed `batch` leaves none of its operations). `A2mDoc::new(opl3)` makes an empty
version-14 song (9 or 18 tracks), `from_bytes` reads one, `to_bytes()` writes it (the document converts nothing between
versions), `song()` borrows the `A2mSong` to play (`A2Player::new(&doc.to_bytes()?, rate)`).

**Numbers.** Patterns, rows, tracks (`channel`) and order positions count from 0; instruments, macros and macro
entries from 1 (0 is none in a cell). Every pattern of an A2M song has the same length (`pattern_len`, 64 before
version 9) and `tracks` tracks (9 in versions 1-4, 18 in 5-8, 1-20 from 9). **A cell** is `[note, instrument, effect,
param, effect2, param2]`: `note` 0 none, 255 key off, else a MIDI number from 12 (C-0, AT2's note 1) to 108 (C-8, note
97, which real songs use and AT2 plays as block 7's top), `60` is `C-4` (`info().note.octave_offset` is 1, as for
XM). A fixed note (version 9 up) is the number plus 128 (`set_cell` takes `fixed: true`; they are not transposed);
a note byte the format does not define reads as 256 plus the byte, so nothing is lost. `effect` is the command number
as the file holds it (0-9, then `A`=10 .. `Z`=35, then ``&%!@=#$~^`><``=36-47; versions 1-4 stop at `F`; the operations also
take the letter) in the song's own version's numbering, with its parameter byte; the second column is version 9 up.

Reads: `info()` (version, title, tracks, patterns, orders, restart, tempo, speed, timer rate, flags, limits, history
state, `playable`), `rows(pattern, row0, count, flat)` (`cells[row][track]`), `order()` (the pattern numbers before the
first jump marker, where that marker jumps to as `restart`, the `tail`), `instruments()` (every slot in brief),
`instrument(n)` (name, the eleven FM bytes, `voice` with both operators' fields, panning, fine-tune, voice type, the
FM macro summary), `macros()`, `macro(kind, n)`, `validate()` (what the version cannot hold; `validate_content()` skips
the writer), `copy_range`, `live_edits()`.

**Operations** (`{"op": ..., fields}`):

| group | ops |
|---|---|
| cells | `set_cell` (`pattern`, `row`, `channel`, `cell: [..]` or any of `note`, `instrument`, `effect`, `param`, `effect2`, `param2`; notes may be `"C-4"`, `"###"`, effects letters), `clear_range` (+ `columns`), `insert_row` / `delete_row`, `transpose` (range, `semitones`, `clamp`, `instrument`), `copy_range` -> clip, `paste` (`cells`, `mode`: `overwrite` or `mix`) |
| patterns | `set_pattern_len` (alias `set_pattern_rows`: the song's length, 1-256 from version 9; shortening over data needs `force`), `new_pattern`, `duplicate_pattern`, `delete_pattern` (its orders go, later numbers shift), `set_pattern_name` (version 11 up) |
| orders | `set_order`, `insert_order`, `delete_order`, `move_order`, `set_restart` (the first jump marker's target), `set_orders` (`orders`, `restart`) |
| song | `set_title`, `set_composer`, `set_tempo`, `set_speed`, `set_flag` (`update_speed`, `track_volume_lock`, `volume_peak_lock`, `tremolo_depth`, `vibrato_depth`, `track_panning_lock`, `percussion`, `volume_scaling`; version 5 up), `set_tracks` (version 9 up; dropping tracks with data needs `force`), `set_macro_speedup`, `set_four_op_tracks` (`mask` or `pair` + `on`; version 10 up), `set_lock_flags`, `set_four_op_instrument` (version 12 up), `set_rows_per_beat`, `set_tempo_finetune` (version 14) |
| instruments | `set_instrument` (`name`, `panning`, `finetune`, `voice_type`, `fm`, and the voice: `modulator` / `carrier` objects of `tremolo`, `vibrato`, `sustain`, `ksr`, `multiplier`, `key_scale_level` (`ksl`), `total_level` or `level` (63 loudest, as the DAW's slider), `attack`, `decay`, `sustain_level`, `release`, `waveform`, plus `feedback` and `connection`: `fm` or `additive`), `set_operator` (`operator` and the fields flat), `swap_operators`, `reset_instrument` (the plain sine voice), `clear_instrument`, `copy_instrument` (with its FM macro), `set_disabled_columns` (version 11 up) |
| macros | `set_fm_macro` (header fields and `steps: [{step, fm or voice fields, freq_slide, panning, duration}]`), `set_arpeggio_macro`, `set_vibrato_macro` (header fields and `data: [..]` from entry 1 or `entries: [{entry, value}]`), `clear_macro` |
| history | `begin_group`, `end_group`, `undo`, `redo`, `batch` (`ops`: one undo step, all or none) |

Every JSON edit returns `{"changed": {"patterns": [{pattern, row0, row1}], "layout", "orders", "header", "timing", "song",
"names", "instruments": [n], "macros": [{kind, macro}]}}` and its own fields. Every operation is checked: a range
(attack 0-15, level 0-63, key scale 0-3, multiplier 0-15, waveform 0-7, feedback 0-7), a version's own limits
(250 instruments and one effect column before 9, no panning before 5, no voice types before 9, macros from 9, pattern
names and disabled columns from 11, 4-op instruments from 12, rows per beat in 14), the 128 order positions, 64 or 128
patterns, a macro's loop and key-off positions against its length.

**Undo and redo.** The inverse-change log of `ModuleDoc` (`edit/history.rs`): an edit records the change that puts
back what it overwrites (a rectangle of cells, the order list, an instrument, one macro table, the header, the
names), so typing a note costs one cell and not the 1 MB songdata. `begin_group` / `end_group` nest; `undo()` / `redo()`
return a `Changes`; bounded by steps (1000) and bytes (128 MiB); `revision()` counts every change.

**Writer.** `to_bytes()` is `a2::write::write`: versions 9-11 aPLib, 12-14 LZH, 4 and 8 stored; versions 1 and 5
(SIXPACK) are written as the stored 4 and 8. *Checked* (`tests/a2m_doc.rs`): on all 266 fixtures (and every `.a2m` in
`CUBED_A2M=<dir>`) `A2mDoc::from_bytes(x).to_bytes()` loads back to the same song and saving that gives the same
bytes; a random-operation soak on one song of each version (1, 5, 9, 10, 11, 12, 13, 14) and on new ones saves and
reloads after every few steps, undoes everything to the start and redoes to the end; the bytes of two songs built by
operations are pinned by hashes (`cargo run -p cubed-opl --release --example a2m_edit -- in.a2m ops.json out.a2m` applies an operation list to a file or to `new`/`new3`). 19 fixtures have an order list that names patterns past the file's pattern count (Adlib
Tracker II plays them as empty, or from the rest of the file's last block): the document shows them, so such a song
saves with the larger count; no other song changes.

**Live edits.** A playing `A2Player` takes what a document's `live_edits()` returns after an edit
(`Option<Vec<LiveEdit>>`) with `A2Player::apply_edit(&edit)`, in place and without allocating: cells (read when the row
comes), instrument data (a track that holds the edited instrument loads it again at its next instrument cell), FM,
arpeggio and vibrato macro tables (read as they run) and the tempo and speed. `None` means the shape moved (the
pattern list, the order list, the pattern length, the track count, the flags, the macro speed-up, lock flags, 4-op
pairs, or more than 1024 cells): open the edited song as a new player, seek it to where the old one is and swap them.
A note sounding now goes on with what it loaded; `OplAudition` is how an edit is *heard* at once.

**The instrument alone.** `OplInstrumentDoc` is the DAW's Adlib instrument editor without a song:
`OplInstrumentDoc::from_s3m(&Adlib::registers)` (or `new`, `from_a2m`) takes the same `set_instrument`, `set_operator`,
`swap_operators`, `reset`, `clear`, `undo`, `batch` operations; `s3m_registers()` is what to give
`cubed_tracker::edit::ModuleDoc`'s `set_sample` (`adlib.registers`) and `patch()` what the audition plays. An S3M's
OPL2 has the waveforms 0-3 and no name, panning, fine-tune or voice type: those are refused. `cubed_opl::fm` has the
shared types (`Voice`, `Operator`, `FmPatch`: the eleven FM bytes are the same in A2M and S3M).

**Audition.** `audition::OplAudition` plays an instrument with no song: a chip of its own with up to 18 voices, notes
loaded the way the song player loads them (same register order, the AT2 note table and fine-tune, the empty-envelope
workaround). `note_on(note, velocity, duration)`, `note_off(note)`, `all_notes_off()`, `set_patch(&FmPatch)` (a live
edit: sounding notes are rewritten at once), `render(&mut left, &mut right)` (writes over the buffers, allocates nothing),
`peak()`. It maps one to one onto `cubed_mixer::Source` (`render` -> true) and `SourceEvent::{NoteOn, NoteOff,
AllNotesOff}`; a patch edit needs a message of 11 FM bytes, panning and fine-tune. Percussion voice types and 4-op
pairs play as plain 2-op voices.

**Playback control** (`A2Player`, beside `seek`, `set_mute_solo`, taps): `status()` (order, pattern, row, tick, speed, tempo,
timer rate, a rows counter that a seek bumps, frames, loops, ended), `set_loop(A2Loop::{None, Pattern, Range})`,
`set_mute(track, on)` / `set_solo(track, on)`, `set_levels_enabled` + `track_levels(&mut out)` (each track's peak,
falling 24 dB a second, as the tracker `Probe`'s), `apply_edit`. `seek` and everything above allocate nothing now
(`Chip::reset`, `Resampler::reset`, `A2Engine::restart` reset in place): `cubed-rt-check` runs them on the audio-thread
path.

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
