# cubed-synth

The Rust audio code shared by the another_synth web DAW and the wgpu-cube engine:
chip emulators, module players and a modular synth engine. It is plain Rust that
builds on stable, natively and for `wasm32-unknown-unknown`, so the same code can run
in a browser AudioWorklet and in a native audio callback.

| Crate | What it is |
|---|---|
| `cubed-sid` | MOS 6581/8580 SID chip (with measured 6581 revisions), a GoatTracker-style song player for the `ASID` song file, GoatTracker 1 and 2 `.sng` import (`goattracker`, another_synth's importer ported byte for byte) and a GTS5 `.sng` writer, `edit::SidDoc` (the song as data: JSON operations, GoatTracker's limits, undo), player controls for an editor (seek by place, loops, mute and solo, instrument audition, position and peaks, live edits), a register-stream chip player (`stream`), and a PSID/RSID player (`psid`: a 6510 and a small C64 that run a C64 `.sid` tune's own code in step with the audio, ported from the DAW's runner) |
| `cubed-ahx` | AHX / HivelyTracker (`.ahx`, `.hvl`) parser and replayer, bit-exact against `hvl_replay.c`, with a band-limited "hifi" path. The writer (`write`: `.ahx` and `.hvl`, byte-identical to the DAW's on all 127 fixtures) and `edit::AhxDoc`, which edits a song as data with JSON operations, format limits and undo; the player takes live control (seek, loops, mute and solo, an audition voice, levels, a `AhxProbe`) and live edits |
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
- `edit` (`cubed-tracker`, `cubed-ahx`, `cubed-opl`, `cubed-sid`, on by default): `cubed_tracker::edit`,
  `cubed_ahx::edit`, `cubed_opl::edit` and `cubed_sid::edit`, the module, song and instrument edit layers
  (need `serde_json`; nothing in a player calls them, so a worklet drops them at link time).

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
    SeekVoice { voice: u8, order: u16, row: u16 },    // SID: a seek by one voice's own place (wire kind 18)
    Loop(SongLoop),                                   // None | Pattern { order } | Range { order, row0, row1 }
    Mute { channel: u8, on: bool }, Solo { channel: u8, on: bool },
    MuteMask(u32), SoloMask(u32),                     // every channel at once, a bit each
    PreviewOn { instrument: u8, note: u8, velocity: f32, duration: u32 }, PreviewOff,
    Looped(bool),                                     // restart at the end of the song
    Scopes(bool),                                     // fill the channel scope rings
    SetCell(SongCell),                                // a live cell edit
    SetInstrument(SongInstrument),                    // Adlib: a live instrument edit
    Timing { tempo: u8, speed: u8 },                  // Adlib: tempo and speed now
    SetOrder { order, channel, pattern, transpose },  // AHX/HVL: a position's track and transpose for a channel
    SetRestart(u16),                                  // AHX/HVL: where the song starts over
    SetTableRow { table, row, left, right },          // SID: a row of a step table (wire kind 17)
}
trait SongReader: Send + Sync {                       // natively: the song's probe; on the web: WebSong
    fn status(&self) -> SongStatus;                   // as Player::status, plus `version` and `voices` (SID: each voice's own order, pattern, row)
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
(b on), 13 Scopes (b on), 14 Timing (a tempo, b speed), 15 SetOrder (a order, b channel | pattern << 8, c transpose as a byte), 16 SetRestart (a order), 17 SetTableRow (a table | row << 8, b left | right << 8), 18 SeekVoice (a order, b row, c voice). Tag 32, 24 bytes, SetInstrument: `u8 tag, u64 voice, u8
number, u8 fm[11], u8 panning, i8 finetune, u8 voice_type`. Tag 31, 21 bytes: `u8 tag, u64 voice, u16 pattern, u16 row, u8 channel, u8 note
(0 none, 255 key off, else MIDI; Adlib: the file's note byte), u16 period (Adlib: `effect2 << 8 | param2`), u8
instrument, u8 volume, u8 effect, u8 param` (SetCell). An
unknown kind stops the batch at it (`WireError::UnknownTag(30)`); the other tags are unchanged.

**The worklet.** The page opens a module with the processor's existing `'song'` message plus `stopped:
true` (open it stopped and under control) or `follow: true` (follow it while it plays), for format 3, 4
or 5, or an Adlib song (format 1); the reply is the usual `{type: 'song', source, ok, error?}`. Then it plays the source and steers it
with `'commands'`. The worklet posts, about 60 times a second (init's `songHz`) and only for the followed
songs that moved:

- `{type: 'songpos', frame, sources: Uint32Array, status: Uint32Array, levels: Float32Array, voices:
  Uint32Array}`: for song i its source slot, 12 status words (flags playing/preview/alive/scopes on, order,
  pattern, row, tick, speed, tempo, rows, frames low and high, version, channels; `SongStatus::to_words`),
  33 levels and 6 voice words (`SongStatus::voice_words`: the order, pattern and row each of up to
  `SONG_MAX_VOICES` = 3 voices is at, two words a voice, `pattern << 16 | order` then `valid << 16 | row`;
  all 0 for a format whose voices share one position; a separate array, so a page that predates it reads
  the message as before, and a worklet that predates it posts none: no voice is then reported). (The
  type is not `'song'`, which is the reply to a prepare and has an `ok`.) A song gone is reported once
  more with alive 0: its voice was freed by the first message handler after it ended; a page that tracks
  its voices sees `Ended` sooner.
- `{type: 'songscope', frame, frames: 512, sources, channels: Uint32Array, written: Float64Array, data:
  Float32Array}`, with each `'songpos'` for the songs whose scopes are on: per song its channel count, in
  `data` each channel's 512 latest frames, in `written` how many each ring has seen.

On the page, `WebSongs` (one `WebSong` per source slot; `update_positions(sources, status, levels)` or, with the
message's `voices`, `update_positions_voices(sources, status, levels, voices)`,
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

## Song formats under live control, and adding one

Every format an editor can drive plays as the same kind of voice: a source that takes `SourceEvent::Song(SongControl)`
and gives a `SongReader` (above). Modules are `songs::open_module` / `open_module_song`; every other format is a
`songs::SongPlayer` and `songs::PlayerSong<P>` makes it the voice. `songs::open_controlled_song(format, bytes, rate,
options, stopped)` opens whatever has an arm in it from a file (the worklet's `'song'` message does), so a host that
holds the song as bytes needs nothing per format:

| format | opened by | under control |
|---|---|---|
| MOD, S3M, XM | `open_module(module, ..)`, `open_module_song(format, bytes, ..)` | everything above |
| Adlib Tracker II (`.a2m`) | `open_a2m(song, ..)` (an `A2mSong` the host holds), `open_controlled_song(SongFormat::A2m, bytes, ..)` | everything above; see below |
| AHX, HVL (`.ahx`, `.hvl`) | `open_ahx(song, ..)` (a `cubed_ahx::format::Song` the host holds), `open_controlled_song(SongFormat::Ahx, bytes, ..)` | everything above except scopes; see below |
| SID (`.asid`) | `open_sid(song, ..)` (a `cubed_sid::song::SidSong` the host holds), `open_controlled_song(SongFormat::Sid, bytes, ..)` | everything above; see below |
| PSID, RSID (`.sid`) | `open_psid_at(bytes, rate, subsong, seconds, stopped)`; `open_controlled_song(SongFormat::Sid, bytes, ..)` and `open_controlled_song_at(.., start)` tell a `.sid` from a `.asid` by its magic | play-only: play, stop, mute and solo by voice, position in seconds, levels, scopes; see below |

**Adlib.** The channels are the song's tracks. `Play`/`Stop`, `Seek` (the song replays silently to the row: a far row costs
a few thousand ticks of register writes on the audio thread, no allocation), `Loop`, `Mute`/`Solo` and their masks map
to `A2Player`. `levels` are each track's OPL channel scaled to the output (the taps read one operator's full swing as 1,
a quarter of the chip's full scale) and a scope is the same channel's frames (the chip's channels are separable here, unlike
S3M's AdLib). `SetCell` carries the file's cell (note byte, instrument, effect and parameter in `effect`/`param`, the second
effect and parameter in `period`); `SetInstrument` the 11 FM bytes, panning, fine-tune and voice type, for the notes that
load the instrument from then on; `Timing` the tempo and speed (`status().tempo` is the timer tempo, the Hz of the timer at a
macro speed-up of 1, not a BPM). Macro tables (255 entries) and everything that changes the song's shape are a new song:
`open_a2m` the edited one, `swap_commands`, and the same `Seek`, `Loop`, masks and `Play` for the new voice, as for a module.
The **preview** (`PreviewOn { instrument, note, velocity, duration }`, `PreviewOff`) is an `OplAudition` beside the song, a
chip of its own clocked only while a note sounds, loaded with the song's instrument `instrument` (1-255; a note is MIDI,
60 is middle C) and re-loaded by `SetInstrument` of the same number, so a slider moved under a held note is heard at
once. `status().preview` says a note sounds or releases; the preview's level is the last slot of `levels`.
`songs::open_opl_audition(&instrument, rate, voices)` is the same audition as a voice of its own, for an instrument with no
song (`OplInstrumentDoc`): `SourceEvent::NoteOn`/`NoteOff`/`AllNotesOff` play it and `SongControl::SetInstrument` changes the
instrument under the notes that sound.

**AHX and HVL.** The channels are the song's (4, or the HVL header's up to 16). The transport is `AhxPlayer`'s (a seek that
replays silently to the row, loops of a position or of rows, mute and solo by channel). The preview is the player's own audition on
a voice of its own (`PreviewOn { instrument, note: MIDI, velocity 0-1, duration }`, 24 is C-1 and 83 B-5). `levels` are the player's
bound on each channel's loudest sample (falling 24 dB a second), then the audition's. `status().pattern` is the position (AHX has no
patterns), `speed` the ticks per row, `tempo` the timer's Hz (50 x the speed multiplier); a stopped song shows the row it resumes at.
Live edits on the wire: `SetCell` (a step: `pattern` is the *track*, `note` the file's 0-63, `effect`/`param` the first effect column,
the second in `period` as `fxb << 8 | fxb_param`), `SetOrder` (a position's track and transpose for a channel) and `SetRestart`.
An instrument is bigger than a wire message (its PList is up to 255 rows): an instrument edit, and everything that changes the
song's shape (the track length, positions inserted, deleted or moved, the speed multiplier), is a new song swapped in
(`open_ahx` of the edited `Song`, then the usual `Seek`, `Loop`, masks and `Play`). No scopes: the engine's per-voice capture
allocates when it is switched on, which an audio thread may not do.

**SID.** Three voices (the channels), an orderlist each over one pool of patterns. `SidSongPlayer` has no pause, so **stopped is
its preview mode** (the sequencer is off, the notes die away, the position stays, auditions play on any voice) and `Play` leaves
it; a song opens stopped. The voices run on orderlists of their own, so each has its own place at any time:
`status().voices[v]` is voice `v`'s orderlist position, pattern and row (`VoicePos`, the row it started last; right after a seek, the
row it will start; `valid` is true for the three voices). `Seek { order, row }`, `Loop` and the status's plain `order`, `pattern`,
`row` and `tick` stay **voice 0**'s: voice 0 reads row `row` of the pattern at its orderlist position `order`, a range loop repeats
rows of voice 0's entry. `SeekVoice { voice, order, row }` is the same for any voice (`Seek` is `voice: 0`): the song is replayed to
the moment that voice reads that row, so the other voices stand where the song has them then (voices that keep step with it show the
same row), not where they were; stopped or playing, and a place the song does not have moves nothing. (`pattern` is the pattern
number, `speed` the frames per row, `tempo` the timer's Hz, 50 x the speed multiplier, `rows` the song row of the reference voice;
`SONG_MAX_VOICES` is the voices a status can carry.) The preview is the player's audition on voice 0 (held until
`PreviewOff`, or for `duration` frames; over a playing song voice 0 is taken for it; a note is MIDI, 12 is C-0). `levels` are the
voices' peaks (the taps), then 0; the scopes are the voices' own signals before the filter. Live edits on the wire: `SetCell` (a
row: `pattern` is the pattern number, `note` 1-93 / 126 off / 127 on, `effect` the command), `SetInstrument` (the ten bytes in
`fm`: attack/decay, sustain/release, first wave, gate timer, flags (bit 0 hard restart, bit 1 no gate off), vibrato delay, the
wave, pulse, filter and speed pointers) and `SetTableRow`; a pattern's length, the orderlists, the instrument list, a table's
length, the start tempo, the speed multiplier and the chip model are a new song. The chip's output carries a constant offset
while its volume register is up, silence included.

**PSID and RSID.** A tune is a program (`cubed_sid::psid::PsidPlayer` runs its 6510 code in step with the audio), so it plays
and stops and that is all: `Play`, `Stop` (silent, the CPU does not run, the position stays), `Mute`/`Solo` by voice (bit `i`
silences voice `i` of every chip), the position, levels and scopes (the voices of the first SID; up to three SIDs play, mixed as
their average). **`Seek`, `Loop`, the previews and the live edits are ignored on the audio thread**: a start runs the tune's init
routine (up to 60 million cycles) and a seek runs its code from the start up to the target (a seek back starts it again), neither
of which belongs on an audio thread. A host that wants a tune at a place **builds it there**: `open_psid_at(bytes, rate, Some(subsong),
seconds, stopped)` (or `open_controlled_song_at(.., start)`) off the audio thread; the worklet's `'song'` message takes `start`
(seconds) with the subsong and runs it in its message handler (`cw_prepare_start`), so a big tune can glitch the sound once. The
position: `status().order` is the subsong (0-based), `row` the whole seconds, `tick` the hundredths after them (`rows` the whole
seconds, `frames` the samples), `speed` 1 and `tempo` 50; a tune whose code stops the CPU reads stopped and `pattern` is 1 (0
otherwise). A BASIC RSID, and a file the player cannot run, is an error that says why.

**Adding a song format** (the next one is one more of these):

1. Make the format's player a `songs::SongPlayer` (a small struct in `crates/mixer/src/songs/<format>.rs`, so the format crate
   needs no mixer): `channels()`; `render(left, right)` (silence while stopped; at most `PLAYER_CHUNK` = 256 frames);
   `ended()` (the song came to its end, or round once: with `Looped(false)` the transport then stops at the top; reset by a
   seek); `control(SongControl)` (Play, Stop, Seek, Loop, Mute, Solo and the masks, PreviewOn/Off, and the live edits the
   format has; ignore the rest. `Looped` and `Scopes` never arrive: the adapter keeps the first and calls `set_scopes`);
   `status()` (`SongStatus`: order, pattern, row, tick, speed, tempo, rows, frames; the adapter fills `version`);
   `levels(&mut [f32]) -> f32` (each channel's recent peak, then the preview's); optionally `set_scopes` and `scope(channel,
   out)` (the frames a channel made in the block just rendered; a format that cannot separate its channels returns 0).
   Nothing in these may allocate: build and warm the player up where it is made (`A2mPlayer::new` plays 170 ms and resets).
2. Open it: `PlayerSong::new(player, controlled, looped)` is the `Source`; add an arm to `open_controlled_song` and, for a
   host that holds the song as data, an `open_<format>(data, ..)` like `open_a2m` or `open_ahx`.
3. Edits that are small and fixed in size ride the wire as `SongControl` variants (a cell is tag 31, an Adlib instrument tag 32,
   timing kind 14: add a variant, `song_fields`/`song_control` or a tag in `wire.rs`, and a round trip in its test); anything
   bigger or that changes the song's shape is a new song swapped in (`swap_commands`), which the editor does for every format.
4. The worklet needs nothing for a format that is in `open_controlled_song`: its page already sends `{type: 'song', format,
   stopped: true}` and reads `'songpos'`/`'songscope'` (add the format number to the JS's list of controllable formats, `module`
   in `cubed-worklet.js`). Tests: a rig like `tests/a2m_song.rs`, `tests/ahx_song.rs`, `tests/sid_song.rs` or `tests/psid_song.rs`, an allocation test in `rt-check` (`mixer_a2m_song_control`, `mixer_ahx_song_control`, `mixer_sid_song_control`), and the
   worklet page's (`crates/worklet/test/index.html`, `adlibControlled`, `ahxControlled`, `sidControlled`).

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

**Live edits.** (The mixer plays a document's song with `songs::open_a2m` and takes these as `SongControl`s: see "Song formats
under live control".) A playing `A2Player` takes what a document's `live_edits()` returns after an edit
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

Everything below is in `cubed-ahx` (`AhxPlayer`, `AhxEngine`); `cubed-mixer` (`songs::open_ahx`, `AhxSongPlayer`) and the worklet forward it ("Song formats under live control").
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

## Editing and writing SID songs (a GoatTracker-style editor's data layer)

`cubed_sid::edit::SidDoc` holds a `SidSong` (the DAW's `.asid` document) and edits it in GoatTracker's
own model, in the style of `ModuleDoc`: typed methods and one JSON entry point, `apply(&Value) ->
Result<Value, String>`. A song is **three orderlists per subsong over one pool of patterns**: an
orderlist entry is `(pattern, transpose, repeat)`, the voices run independently, patterns are shared by
number, and **instruments point into four shared step tables** (wave, pulse, filter, speed). Every change
is checked against GoatTracker's limits (1-128 rows in a pattern, 208 patterns, 63 instruments, 255 rows
per table, 254 orderlist entries, 32 subsongs, repeat 1-16, commands 0-F, envelope nibbles 0-15, gate
timer 0-63) and a change that does not fit is refused with a message and leaves the song as it was (a
failed `batch` leaves none of its operations). Three voices only: the crate does not play GoatTracker's
six-channel (dual SID) songs and refuses to read them. `from_bytes` reads a `.asid` or a GoatTracker
`.sng` (GTS5, GTS!) by its magic (`from_sng` takes the chip and speed a `.sng` cannot say),
`to_asid_bytes()` and `to_sng_bytes()` write them, `SidDoc::new()` / `with_options` make GoatTracker's new
song (one subsong, a blank 64-row pattern per voice, instrument 1 a plain pulse), `song()` borrows the
`SidSong` to play (`SidSongPlayer::new(doc.song().clone(), rate)`).

**Numbers.** Patterns, rows, voices (0-2), subsongs, orderlist entries and **table rows count from 0**;
instruments count from 1, as a pattern row names them (0 is none). **Table pointers are GoatTracker's, 1-based
with 0 meaning none** (instruments' `wave_ptr`/`pulse_ptr`/`filter_ptr`/`speed_ptr`, the parameter of commands
8/9/A and 1-4/E, wave-table commands, `FF xx` jumps): table row 0 is what a pointer calls 1. Ranges include
both ends. **A pattern row** is `[note, instrument, command, param]`: `note` 0 none, 1-93 C-0 to G#7 (the note
table index plus one), 126 key off, 127 key on (a MIDI number is `note + 11`, `info().note.midi_offset`;
operations also take `"C-4"`, `"off"`, `"on"` or `midi`); `command` 0-15 (`info().commands` names them and says
what the parameter means; operations also take the hex digit as text); `param` 0-255.

Reads: `info()` (texts, chip, speed multiplier, tempo, counts, the note numbering, limits, command names,
history state), `rows(pattern, row0, count, flat)`, `patterns()` (length and how many entries play each),
`orderlist(subsong, voice)` / `orderlists(subsong)` (entries, restart and the rows each plays),
`instruments()` / `instrument(n)` (every field, and `uses`, the rows that name it), `tables()` /
`table(kind, row0, count)` (the tables belong to the song, not to a subsong), `validate()` (what the writers
refuse, and warnings for what plays oddly: a command naming a table row past the table, a volume above F),
`copy_range`. Every JSON edit returns `{"changed": {"patterns": [{pattern, row0, row1}], "layout", "orders":
[{subsong, voice}], "subsongs", "header", "instruments": [n], "instrument_list", "tables": [{table, row0, row1}],
"table_layout"}}` and its own fields.

**Operations** (`{"op": ..., fields}`; `subsong` defaults to the one shown, `select_subsong`; `voice` is 0-2
or `"all"`, and with `"all"` a `pattern` list gives one per voice):

| group | ops |
|---|---|
| cells | `set_cell` (`pattern`, `row`, `cell: [n,i,c,p]` or any of `note`, `midi`, `instrument`, `command`, `param`), `clear_range` (+ `columns`: `note`, `instrument`, `command`), `insert_row` / `delete_row` (`row`, `count`; rows shift inside the pattern and fall off the end, or with `grow` / `shrink` the pattern gets longer or shorter), `transpose` (range, `semitones`, `clamp`, `instrument`; a note that would leave C-0..G#7 refuses the edit unless `clamp`), `copy_range` -> clip, `paste` (`cells`, `mode`: `overwrite` or `mix` which fills only what is empty; cut at the pattern's end, `clipped` says so) |
| patterns | `set_pattern_rows` (1-128; cutting rows that hold data needs `force`), `new_pattern` (appends), `duplicate_pattern`, `delete_pattern` (later patterns shift down and every orderlist entry follows; refused while an orderlist plays it unless `force`, which removes those entries) |
| orderlists | `set_order` (`index`, any of `pattern`, `transpose` -64..63, `repeat`), `insert_order`, `delete_order`, `move_order` (`from`, `to`), `set_restart` (the entry a voice loops back to: GoatTracker's `RST`); the restart stays on its entry when entries come and go |
| subsongs | `add_subsong` (a copy of `copy_of`, or a new one with three blank patterns), `remove_subsong`, `select_subsong` (not an edit) |
| instruments | `set_instrument` (`name`, `attack`, `decay`, `sustain`, `release`, `first_wave`, `gate_timer`, `hard_restart`, `no_gate_off`, `vibrato_delay`, the four pointers), `rename_instrument`, `clear_instrument` (GoatTracker's new instrument, no tables), `add_instrument` (with a plain pulse's wave and pulse rows appended for it, or `tables: false`), `clone_instrument` (appended, or over `to`; its table rows are copied too, jumps inside the copy moved with it, so editing the copy never changes the original), `delete_instrument` (rows naming it refuse the delete unless `clear_uses`; later ones are renumbered in every pattern) |
| tables | `set_table_row` (`row` = the length appends), `insert_table_row` / `delete_table_row` (every instrument pointer, `FF xx` jump, wave-table command ($F1-F4, $F8-$FA, $FE) and pattern command (8/9/A, 1-4, E) that names a later row follows its data, as GoatTracker's table editor and the DAW's do; a reference to a deleted row names the row that took its place), `clear_table_row`, `remove_unused_table_rows` (rows nothing reaches go, the rest is renumbered) |
| header | `set_title` / `set_author` / `set_copyright` / `set_texts` (latin-1, 32 characters), `set_model` (`"6581"`, `"8580"`), `set_speed_multiplier` (1-16), `set_tempo` (1-127), `bake_tempo` (below) |
| history | `begin_group`, `end_group`, `undo`, `redo`, `clear_history`, `batch` (`ops`: one undo step, all or none) |

**Undo and redo** are an inverse-change log (`edit/history.rs`, as `ModuleDoc`'s): before an edit overwrites
anything it records the change that puts it back (a run of rows, one pattern, one orderlist, one instrument,
one table, the header), so typing a note stores one row and a table row insert the tables, instruments and
patterns it moved. Applying a change returns its own inverse, which is how redo comes for free. `begin_group` /
`end_group` nest; `undo()` / `redo()` return a `Changes` to redraw and do nothing while a group is open. Bounded
by steps (1000) and bytes (128 MiB); `revision()` counts every change. Choosing the subsong shown is not undone.

**Live edits.** A song that is playing the document takes some edits in place and needs a new player for the
rest. After an edit, `doc.live_edits() -> Option<Vec<LiveEdit>>` is `Some` when the change was one of these:

| live (give each to `SidSongPlayer::apply_live`) | needs a new player (`None`) |
|---|---|
| a pattern row (typing, clear, paste, transpose, insert or delete rows that keep the pattern's length, and their undos), heard when the voice next reads it | a pattern's length (`set_pattern_rows`, `grow`, `shrink`), the pattern count, an orderlist (entries, repeats, transposes, restart), a subsong |
| an instrument's fields (not its name), heard from the next note that starts (envelope, tables, vibrato) | the instrument list (an instrument added or deleted, which also moves rows) |
| the content of a table row, heard where the table next reads it | a table's length (rows inserted, deleted, appended, removed) |
| the chip model (switched in place) | the start tempo or the speed multiplier (read when the player is made) |
| the texts (nothing is played: `Some(vec![])`) | more than 1024 changed rows |

To rebuild: `SidSongPlayer::new(doc.song().clone(), rate)`, `seek(subsong, voice, order, row)` to the old player's
`status()` and swap it in (the old voice fades out under it, as with modules). `{"op": "live_edits"}` returns the
same as `{"edits": [..] | null}`. A test keeps a player in step with a document through 300 random edits this way.

**The `.sng` writer** (`goattracker::export(&SidSong) -> Result<GtExport {bytes, notes}, SongError>`,
`SidDoc::to_sng_bytes()`; GTS5, a port of the DAW's `gt-sng-write.ts`). A `.sng` is GoatTracker's memory image, so
nothing is repacked: the tables are written whole (count, left column, right column) and every pointer is written
as it is; patterns shared by number with a pattern-end row; instruments in number order (25 bytes: AD, SR, the
four pointers, vibrato delay, gate timer with its two flags, first-frame wave, name); orderlists compiled to
GoatTracker's byte stream, where a transpose is written when it changes (and at the restart entry, when the end of
the list would otherwise leave another transpose running into the loop: GoatTracker keeps it), a repeat when above 1,
then the pattern, the end mark and the restart byte. **What is lost or refused**, relative to the `.asid`: the chip
model and the speed multiplier have no field (written anyway; `notes` says so: GoatTracker takes them from `-E` and
`-S`); a start tempo other than 6 is refused (GoatTracker starts every song at 6), and `bake_tempo` moves it into
an `F` command on row 0 of voice 1's first pattern of every subsong (frames per row = tempo x multiplier, as the player starts it;
refused if that row has a command or the value does not fit 3-127; the first pass plays as before, and where a voice wraps the command sets the tempo again, as GoatTracker plays it); a transpose beyond -16..+14 is refused (the
document holds -64..63); an orderlist that needs more than 254 bytes with its repeats and transposes is refused; a
NUL in a text is refused; a text of the full 32 characters is written with no terminator (`notes`); a song that
breaks the model is refused first. Nothing else is lost: `import(export(song)) == song` given the two hints.

*Checked* (`tests/sng_write.rs`, `tests/ops_oracle.rs`, `tests/edit.rs`, `tests/edit_soak.rs`): the written bytes
are **byte-identical to the DAW's writer on every one of its 194 demo and fixture songs** and on the synthetic
fixtures and `.asid` songs here (`fixtures/goattracker/oracle_write.ts`, `expected_sng.json`;
`CUBED_SNG_WRITE_ORACLE=<json>` runs a corpus the same way), with the DAW's refusals; `load(write(load(x)))`
equals `load(x)` on the fixtures and on a corpus (`CUBED_GT_SONGS=<dir>`: 194 songs) and the bytes are pinned by
hashes; the table and instrument edits (insert/delete rows, clone, delete, new instrument, remove unused rows) save
the same `.asid` as the DAW's own edits on 24 cases x 194 songs (`oracle_ops.ts`, `ops_cases.json`,
`CUBED_OPS_ORACLE=<json>`): 4644 byte-identical, and 12 differ for two reasons that are fixes, not gaps: the DAW
remaps `FF xx` in the *speed* table as if it were a jump (there it is data: a fast slide), and on deleting a row it
clamps a pattern command that names a row past the table's end to the last row, which would give it a meaning
it did not have; a random-operation soak (every operation kind, grouped and not, valid and not, on the fixtures, on
empty songs and, with `CUBED_GT_SONGS`, on every song) checks after every step that the song saves as a `.asid` and
a `.sng` and loads back equal (or is refused for a reason a `.sng` really has), that a refused operation changed
nothing, and that undoing everything lands on every state between and redoing lands on the end.

**Playing it, and controlling the player** (`SidSongPlayer`, plain Rust, nothing allocates after
`SidSongPlayer::new`: `crates/rt-check/tests/sid_controls.rs` counts every audio block of a run that calls each of
these):

| Call | What it does |
|---|---|
| `seek(subsong, voice, order, row) -> Result<(), SeekError>` | Moves to the moment `voice` is about to read `row` of the pattern at orderlist position `order` (its first play): the song is replayed silently from the top, so the tempo, the other voices (they are where the song has them at that moment), the tables and the filter are right; the chip keeps running, so sounding notes carry on until a row retriggers them. About 0.02 us a frame replayed (a 5-minute song's end in 0.4 ms, release); `SEEK_MAX_FRAMES` bounds it. A place that does not exist, or is too far, moves nothing. `seek_row(song_row)` is the same by song row; both keep loops, masks and auditions |
| `set_loop(SidLoop {voice, start, end})`, `loop_orders(voice, first, last)`, `set_loop_rows`, `set_loop(None)` | Repeats a range of one voice's rows (`SidSong::order_rows` gives the rows of orderlist entries): at `end` every voice goes back to where it was when `voice` reached `start`, from a copy of the sequencer taken when the player first passes `start`, so a repeat is free and exact, tempo and tables included. A loop set inside or past its range replays to its start once. It does not move the playhead: seek first |
| `set_mute_solo(mute, solo)` | Masks of the three voices, a bit each; with any solo only soloed voices are heard (a voice both soloed and muted is not); muted voices read 0 in the taps and peaks; the song keeps playing all three |
| `audition_on(voice, instrument, note)` / `audition_off(voice)`, `set_preview(bool)` | Plays instrument `instrument` (1-based) at note table index `note` (0 = C-0 .. 92 = G#7) on a voice, as a song note: envelope, first-frame wave and its wave, pulse, filter and speed tables running, vibrato after its delay, then released by `audition_off`. In preview mode the sequencer is off and any voices can be auditioned at once (a chord): no song needed. Over a playing song the voice is taken (its rows are not read, its counter runs on) until `audition_off` |
| `status() -> SidStatus` | Per voice the orderlist position, pattern, row and tick (frames played of the row, 0 on its first) of the row it started last; the tempo (frames per row of the reference voice), `song_row` / `song_rows` (counted for the reference voice, `ref_voice()`), subsong, loop / preview / audition / mute / solo, frames played |
| `peaks() -> [f32; 3]` | Per voice the loudest recent sample of its own signal (the tap: after its gain, before the filter), falling 24 dB a second after it; the mix's scale (`chip().tap_full_scale()` is a full voice's) |
| `attach_probe(Arc<SidProbe>)` | Shares the status and peaks with other threads: `SidProbe::new()` (off the audio thread), then `probe.status()` and `probe.levels()` from anywhere; atomics, no lock; a sequence counter keeps a frame's three voices together; `status().version` changes whenever anything shown moved |
| `apply_live(&LiveEdit) -> bool` | One live edit (above) in place; `false` when the place does not exist or the change is not valid for the song |

`render_taps` hands the voices' taps to the caller (the channel scopes): the player always renders with taps,
which costs nothing measurable (47.9 against 47.8 us a block). The mixer and the worklet call these through `songs::SidPlayer` ("Song formats under live control").

*Not covered*: dual SID (six orderlists per subsong); GoatTracker 1 (`GTS!`) is read but not written; the
`.sng` of a song with a start tempo other than 6 (refused, `bake_tempo`); the DAW's flat "song-wide pattern"
view of a GoatTracker song (`flat.ts`, a UI projection that the DAW compiles back to orderlists): this layer is
GoatTracker's own orderlists, which a Lua editor can show per voice (PSID/C64 playback is `cubed_sid::psid`, below).

## Playing PSID and RSID tunes (C64 `.sid` files)

A C64 `.sid` is not a song but a program: 6502 code that, called once per video frame (PSID) or from
its own interrupts (RSID), writes the SID's registers. `cubed_sid::psid` runs that code on an emulated
C64 and plays the writes on the crate's SID chip. It is a port of another_synth's
`src/audio/tracker/psid/` (`mos6510.ts`, `c64.ts`, `psid-file.ts`, `psid-runner.ts`), which was tuned
against real tunes, so what it plays is what the DAW plays. Plain Rust, no allocation while it renders,
native and `wasm32`.

```rust
use cubed_sid::psid::{PsidOptions, PsidPlayer};

// Build off the audio thread: the tune's init routine runs here.
let mut player = PsidPlayer::new(&bytes, 48_000.0, PsidOptions::default())?;   // the start song
// On the audio thread, any block size:
player.render(&mut out);                                // mono, the chips' mix
player.render_taps(&mut out, [&mut v0, &mut v1, &mut v2]); // and the first chip's three voices
player.render_stereo(&mut left, &mut right);            // the mono signal in both
```

| | |
|---|---|
| `PsidFile::parse(bytes)` | The header of PSID and RSID, versions 1-4: load, init and play addresses, songs, start song, per-song speed bits (`uses_cia(song)`), PAL/NTSC, the SID model(s), the second and third SID's address, name/author/released. A file this cannot run is refused with a reason (`PsidError`, the DAW's words): no magic, bad header, Compute!'s Sidplayer data (MUS), an RSID that is a BASIC program, an init that jams, hits a BRK or runs past 60 million cycles |
| `PsidPlayer::new / from_file` | Options: `subsong` (0-based; the header's start song when `None`), `model` (`Sid6581`/`Sid8580`; the file's tag, else a 6581, as HVSC's players do) and `kernal` (`Kernal::Extended`, the default, or `Kernal::Daw`: below). The chip runs at the file's clock (985 248 Hz PAL, 1 022 727 NTSC), so pitch and speed are the machine's |
| `render`, `render_taps`, `render_stereo` | Mono mix of the chips (taps: the first chip's voices); `render_taps` gives the mix `render` does, sample for sample |
| `ended() -> Option<EndReason>`, `song_end_reached()` | A tune has no length (nearly all loop for ever); the only end is a play routine or an RSID that jams the CPU, hits a BRK, or does not return from a play call in 2 million cycles. The chips keep sounding what they hold |
| `position_seconds()`, `frames()`, `subsong()`, `subsongs()`, `file()`, `sid_count()`, `chip(i)`, `chip_mut(i)` | Reading it; `chip_mut` is for a settings change (`set_revision`, `set_model`), not for clocking |
| `set_gain`, `set_mute_solo(mute, solo)`, `set_model` | Cheap controls, fine between blocks (rule of `SidChipPlayer::set_mute_solo`: bit `i` is voice `i` of every chip) |
| `restart(subsong)`, `seek(seconds)` | They run the tune again, so not for the audio thread. A seek runs the code to the target as fast as it goes, applies the writes of all but the last second to the chips at once and plays that second for real; a seek back restarts. Up to 1200 s |

**Design: the CPU runs live, inside `render`.** The alternative is to capture the tune's writes offline into
a register stream and play that (`stream`, which the worklet already uses for the DAW's TypeScript runner).
Running live wins: a 6510 costs about a million cycles a second of audio (the whole player, chip included, runs
35-60x real time on the 6510-heavy tunes of the corpus, `examples/psid_play`), a tune has no length so there is
nothing to capture to the end of and no limit to choose, a subsong change or a seek only runs the code again,
and an RSID that sets up its own interrupts works the same way. The price is that a tune's init routine (up to
60 M cycles, a fraction of a second at worst) runs in `new`: build the player on a loading thread.

**How a block is played.** The audio is cut into stretches of at most 600 chip cycles (a few dozen samples).
For each, `PsidRunner` runs the tune up to the stretch's last cycle, the SID writes it made come out stamped
with the cycle they were made on, `Chip::write_after` schedules them at those cycles, and the chip renders the
stretch (`Chip::cycles_in_next` tells how many cycles it will clock). Every write lands on its own cycle, so the
envelope's rate counter keeps running between two writes (the ADSR delay bug) as on a real C64, and the queues
in between never hold more than a stretch's worth: a main loop that stores to the SID every 5 cycles renders
without one allocation (`cubed-rt-check --test psid_alloc`). A host-driven play call that straddles the end of
a stretch is stopped where the cycle falls and goes on in the next (the DAW's runner runs it to its end; the
writes are the same).

**How a tune is driven** (as HVSC's SID file environment says, and the DAW does): a PSID with a play address
gets `init` called with the subsong in A and `play` once per tick, per video frame (19 656 cycles PAL) or per
CIA 1 timer A period when the header's speed bit says so (the period is read again after every call: Rubicon
reprograms the timer in its first play call), with `$01` set for the call address as libsidplayfp does and
interrupts off. An RSID, or a PSID with play address 0, is left to its own interrupts after init: the stand-in
KERNAL's IRQ entry (`$FF48`, `JMP ($0314)`), CIA 1 and 2 timers (timer B may count timer A), the VIC raster
interrupt, an NMI from CIA 2. **The machine** (`psid::c64`) is 64 KB of RAM with the 6510's `$01` banking, the
SID (voice 3's `$D41B`/`$D41C` modelled, since players use them as random numbers), CIA 1/2, the VIC's raster
(PAL 312 x 63, NTSC 263 x 65) and a small KERNAL of our own (no ROM images): its vectors and exits return, every
other routine is an `RTS`. The DAW's stand-in (`Kernal::Daw`) leaves two places of the real IRQ handler as `RTS`,
`$EA34` (after the clock update) and `$EA7B` (the keyboard scan), which many RSID tunes' handlers end by
jumping to; the default `Kernal::Extended` has them lead on to the acknowledge at `$EA7E` and the exit, and a
tune that does not use them runs exactly as in the DAW. **The CPU** (`psid::cpu`) is every NMOS opcode, decimal-mode ADC/SBC included, the
undocumented ones tunes use (SLO RLA SRE RRA SAX LAX DCP ISC ANC ALR ARR SBX, the NOPs, LAS, SHA/SHX/SHY/TAS,
ANE/LXA with `$EE`), the twelve JAMs, and the documented cycle counts with page-crossing and branch penalties.
A write is stamped with the cycle its instruction started on.

**More than one SID.** A v3/v4 file names a second and a third SID (`$D420`-`$D7F0`, `$DE00`-`$DFE0`); each gets
a chip of its own, the model from the header (else the first's), and the player plays their sum divided by the
number of chips (mono, so three chips never clip where one would not). Writes to a SID the file does not name are
mirrors of the first.

**What is checked.**

- The CPU, `src/psid/tests_cpu.rs`: ADC and SBC for every input in binary and decimal (decimal against Bruce
  Clark's sequences for the NMOS 6502), flags of every group, the documented cycle table of all 151 official
  opcodes plus page-crossing and branch penalties, the undocumented read-modify-write opcodes against their two
  official instructions, interrupts, JAM.
- The machine and runner, `src/psid/tests_runner.rs` (the DAW's own tests, ported, and more): banking, CIA timers
  (continuous, one-shot, B counting A), NMI, the raster counter and interrupt, the KERNAL IRQ and BRK paths, the
  runner's modes, retiming, overruns, jams, and the header parser.
- End to end, `tests/psid.rs`, on synthetic tunes written by `tests/fixtures/psid/make_fixtures.py` (a few lines of
  6502 each; **no real tune is ever committed**): the register writes with the cycles their instructions take
  (hand-counted), the CIA and raster interrupt tunes' periods, two chips, and the audio: the player in any block
  size equals a bare chip fed the same writes sample for sample, the notes sound at their pitches.
- Against the DAW: `tests/fixtures/psid/oracle.ts` runs the DAW's own `PsidRunner` over a list of tunes and records
  the first N seconds of writes to the first SID (cycle, register, value) as a count and a hash. `CUBED_PSID_ORACLE`
  makes `tests/psid.rs` compare. On HVSC 85 (61 157 `.sid` files, 10 seconds of the start song of each) **every
  tune's write stream is identical to the DAW's** (`Kernal::Daw`): the same files refused, the same failures, the same
  writes at the same cycles; and identical on the extended KERNAL for every tune the DAW plays, bar one that the
  DAW plays by luck (`3_Oversample.sid`, a sample player: its handler jumps into `$EA7B`, the DAW's run returns to
  stack garbage and writes the volume register 7 500 times in ten seconds, the extended one 73 700, a 7.4 kHz
  sample).

```sh
cargo test -p cubed-sid --test psid                       # the synthetic tunes
cargo test -p cubed-rt-check --test psid_alloc            # render, taps, controls: no allocation
DAW=~/src/rust/another_synth; cd crates/sid/tests/fixtures/psid
$DAW/node_modules/.bin/esbuild oracle.ts --bundle --platform=node --format=esm \
    --alias:another_synth=$DAW --outfile=$SCRATCH/psid-oracle.mjs
find ~/hvsc -iname '*.sid' | sort > list.txt
node $SCRATCH/psid-oracle.mjs oracle.json 10 list.txt     # the DAW's writes, 10 s of each
CUBED_PSID_ORACLE=oracle.json cargo test --release -p cubed-sid --test psid oracle
CUBED_PSID=~/hvsc CUBED_PSID_STRIDE=3 cargo test --release -p cubed-sid --test psid corpus_smoke -- --nocapture
cargo run --release -p cubed-sid --example psid_play -- tune.sid 1 30 out.wav   # speed and a WAV
```

`corpus_smoke` runs every tune of a folder (`CUBED_PSID_SECONDS`, `CUBED_PSID_STRIDE`, `CUBED_PSID_AUDIO` render
every n-th for real) and prints the tunes that do not play, grouped; `CUBED_PSID_STRICT=1` makes it fail on any
but a refused file. Both tests skip when their variable is unset.

**Known gaps.** HVSC 85, 61 157 tunes: 589 (1.0%) are RSID BASIC programs and are refused (they need the C64's
BASIC ROM, which is not ours to ship). The DAW's runner stops 444 others in their first ten seconds (0.7%: 362 hit a
BRK, 79 stop the CPU, 3 never return from a play call), 441 of them RSIDs, and 353 of those play on with the extended
KERNAL (the handler ends in the real handler at `$EA34`/`$EA7B`); 91 (0.15%) still stop or write nothing: a tune that
calls into the real KERNAL or BASIC (here every routine is an `RTS`), reads ROM contents, jumps into a loader's
address, or leans on timing the model does not have. Not modelled: sample playback through NMI handlers in a PSID
that has a play address (the host-driven call runs with interrupts off, as the DAW's does; a PSID with play address 0
and every RSID run their interrupts and do play samples written to `$D418`), PlaySID's own sample registers, the
VIC's bad lines and sprite DMA (a write is where the instruction count puts it, not where the cycles stolen from
the CPU would), CIA TOD, serial and ports, the keyboard and joysticks. The unstable opcodes use their common
behaviour. A tune has no stored length (HVSC's Songlengths database is not read): it loops until stopped.

## Building, testing, benchmarks

```sh
cargo test --workspace
cargo test -p cubed-dsp --no-default-features --features wasm   # another_synth's configuration
cargo bench -p cubed-sid --bench sid_render
cargo test -p cubed-sid --test edit_soak            # CUBED_GT_SONGS=<dir> soaks a corpus too
cargo test -p cubed-sid --test sng_write            # CUBED_SNG_WRITE_ORACLE=<json>, CUBED_GT_SONGS=<dir>
cargo test -p cubed-rt-check --test sid_controls    # the controls allocate nothing
cargo test -p cubed-sid --test psid                 # PSID/RSID: CUBED_PSID=<dir>, CUBED_PSID_ORACLE=<json>
cargo test -p cubed-rt-check --test psid_alloc      # the PSID player allocates nothing
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
