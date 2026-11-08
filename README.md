# cubed-synth

The Rust audio code shared by the another_synth web DAW and the wgpu-cube engine:
chip emulators, module players and a modular synth engine. It is plain Rust that
builds on stable, natively and for `wasm32-unknown-unknown`, so the same code can run
in a browser AudioWorklet and in a native audio callback.

| Crate | What it is |
|---|---|
| `cubed-sid` | MOS 6581/8580 SID chip (with measured 6581 revisions), a GoatTracker-style song player for the `ASID` song file, GoatTracker 1 and 2 `.sng` import (`goattracker`, another_synth's importer ported byte for byte), and a register-stream chip player (`stream`) |
| `cubed-ahx` | AHX / HivelyTracker (`.ahx`, `.hvl`) parser and replayer, bit-exact against `hvl_replay.c`, with a band-limited "hifi" path |
| `cubed-opl` | Yamaha OPL2/OPL3 FM chip (a port of ymfm's OPL path) and an Adlib Tracker II (`.a2m`) parser and player |
| `cubed-tracker` | Tracker modules: ProTracker MOD (Soundtracker, multi-channel), Scream Tracker 3 S3M (AdLib instruments on `cubed-opl`) and FastTracker 2 XM. An editable module model, the parsers, and a replayer ported from another_synth's TypeScript `tracker-playback` (checked against it command for command), with one voice per channel, 32-tap windowed-sinc interpolation, XM envelopes, fadeout and auto-vibrato, the Amiga output filter and stereo separation. An `Observer` sees every row and command |
| `cubed-dsp` | The synth engine: audio graph, oscillators (analog, wavetable), envelopes, LFOs, filters, chorus, delay, reverb, convolver, compressor, limiter, EQ, voices, automation and patch loading; a catalogue of its node types, patch documents that are edited as data, and live parameter edits |
| `cubed-simd` | The slice of `std::simd` the crates above use, on stable Rust |
| `cubed-mixer` | A game audio mixer: named buses with faders, mute/solo, inserts (built-in `effects` with live parameters), sends and ducking; clip voices and sources (chip songs, streams, synth patches played by note events), 2D/3D placement with doppler, long fades, meters; nothing allocates while it runs. `native` adds a cpal output host and streams fed from another thread; `songs` plays AHX/HVL, A2M and SID songs and MOD/S3M/XM modules as sources; `wire` encodes its commands as bytes |
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
the notes the old synth holds and renders a few blocks so their envelopes are past the attack,
`PrepareSource` puts it in a slot, and `swap_commands(old, PlaySource, fade_frames)` gives the
batch `PlaySource` (gain 0), `FadeVoice` and `Stop(old)`; 1,500 to 2,500 frames hide it. The crossfade
is linear, so two voices playing one note in different phases thin out for a moment. On the web
the build runs in the worklet's message handler, on the audio thread (about 6 ms, more with warm-up
blocks): do it with a short fade and few warm-up blocks.

**Wire and worklet.** New: command tag 29, `SOURCE_PARAM` (voice `u64`, node `u128`, param `u16`,
value `f32`, little endian); tag 24 (`SOURCE_EVENT`, notes) is unchanged, so older senders keep
working. `cw_held_buffer(count)` gives room for held notes (a note number and a velocity, two
floats each) and `cw_prepare_synth_held(source, one_shot, warm_blocks, held_count)` builds the
synth with them (`cw_prepare_synth` is unchanged). The processor's `'synth'` message takes
`held: [[note, velocity], ...]` and `warmBlocks` (at most 32 are used). Parameter edits and the
swap's `PlaySource`, `FadeVoice` and `Stop` are ordinary `'commands'`.

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
