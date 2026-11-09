// The cubed-mixer AudioWorkletProcessor: runs cubed_worklet.wasm (the page
// sends its bytes in the 'init' message) and plays the mix it renders.
//
// Register it with `audioWorklet.addModule(<this file>)`, then create
// `new AudioWorkletNode(context, 'cubed-mixer', {numberOfInputs: 0,
// outputChannelCount: [2]})` and talk to its port.
//
// From the page, in order (messages before 'ready' are queued):
//   {type: 'init', wasm: ArrayBuffer, maxVoices, maxBuses, maxClips, seedLo, seedHi, tapHz?, songHz?}
//   {type: 'commands', bytes: ArrayBuffer}  cubed_mixer::wire commands
//   {type: 'clip', id, sampleRate, left: Float32Array, right?: Float32Array}
//   {type: 'song', source, format (0 AHX/HVL, 1 A2M, 2 SID, 3 MOD, 4 S3M, 5 XM), bytes: ArrayBuffer,
//    subsong, looped, separation?, stopped?, follow?, start?}  prepares source slot `source` for a
//    PlaySource command; separation (0-1) is a module's stereo width. For a module
//    (3, 4, 5), an Adlib song (1), a SID song (2) or an AHX/HVL song (0), `stopped` opens it under live control and stopped (silent until a
//    SongControl 'Play' command, its voice never ends by itself) and `follow` (also
//    implied by `stopped`) makes the worklet post its position, levels and channel
//    scopes ('songpos', 'songscope'). The controls are ordinary 'commands': wire tag 30
//    (`cubed_mixer::SongControl`: play, stop, seek, loop, mute, solo, note preview, scopes);
//    live edits are wire tags 31 (a cell) and 32 (an Adlib instrument).
//   {type: 'effect', bus, slot, kind (EffectKind index; -1 empties the slot),
//    params: Float32Array}  the effect's parameters in the order of
//    `EffectKind::params()`, as many as cw_effect_param_count() (52 since the equaliser,
//    kind 9; the other effects have at most 6); those left out are the effect's defaults
//   {type: 'tap', bus, enable}  switches the analysis tap of a bus on or off
//    (the bus must exist); answered by {type: 'tap', bus, enable, ok}
//   {type: 'synth', source, bytes: ArrayBuffer (the patch JSON, UTF-8), oneShot,
//    held?: [[note, velocity], ...], warmBlocks?}
//    prepares source slot `source` with a synth patch, played by
//    SourceEvent commands. To swap a rebuilt synth in for a playing one, give
//    the notes the old one holds as `held` and a few `warmBlocks` (each costs a
//    block's render time, here on the audio thread: at most 32 are used), then
//    send one 'commands' batch with PlaySource (gain 0), FadeVoice and Stop for
//    the old voice. Parameter edits are SourceEvent commands too (wire tag 29).
//   {type: 'swap', source, bytes (patch JSON), play: ArrayBuffer (the wire-encoded
//    PlaySource command of the voice that takes over: its voice id, `source`, bus,
//    gain, pan), oldVoice (the voice to replace), fadeFrames, held: [[note, velocity],
//    ...], warmBlocks, sliceBlocks?}  swaps a rebuilt synth in for a playing one
//    without stalling the render: the synth is built now, warmed up `sliceBlocks`
//    (default 4, at most 32) blocks of 128 frames per render quantum while the old
//    voice plays on, and takes over with a crossfade when it has caught up with the
//    clock, as if its notes had been held `warmBlocks` blocks longer than the old
//    synth's were when this message came. Commands for the old voice keep working
//    meanwhile (a pending swap hears its events) and, afterwards, reach the new voice
//    under the old id. A newer 'swap' replaces a pending one; stopping the old voice
//    gives it up.
// To the page:
//   {type: 'ready'}
//   {type: 'song', source, ok, error?}  after each 'song' or 'synth' message
//   {type: 'telemetry', frame, voices, rejected, badBatches,
//    events: ArrayBuffer (wire events), meters: Float32Array (5 per bus)}
//   {type: 'swap', phase: 'started' | 'failed' | 'done' | 'cancelled', error?, old?,
//    new?, frame?}  'started' (the synth is built, warming begins) or 'failed' answers
//    a 'swap' message at once; 'done' (with the voices) comes when the new voice took
//    over, 'cancelled' if the old voice ended first
//   {type: 'taps', frame, frames: 2048, buses: Uint32Array, written: Float64Array,
//    data: Float32Array}  about 40 times a second (init's `tapHz`) while a tap is
//    on: for each bus in `buses`, its latest `frames` frames, left then right,
//    in `data`; `written` is how many frames each tap had seen
//   {type: 'songpos', frame, sources: Uint32Array, status: Uint32Array, levels: Float32Array,
//    voices: Uint32Array}
//    about 60 times a second (init's `songHz`) for the followed module songs that moved since
//    the last one (and once more when one is gone): for song i, its source slot
//    `sources[i]`, its status words `status[i * 12 ..]` (see `cubed_mixer::SongStatus::to_words`:
//    flags playing/preview/alive/scopes, order, pattern, row, tick, speed, tempo, rows,
//    frames lo and hi, version, channels) and its levels `levels[i * 33 ..]` (the channels'
//    peaks, then the preview's). `voices[i * 6 ..]` holds the song's voices' own positions, for a
//    format whose voices have them (SID: each voice runs its own order list), two words per voice
//    (`cubed_mixer::SongStatus::voice_words`: `pattern << 16 | order`, then `valid << 16 | row`;
//    a voice that is not reported reads 0); a page that predates `voices` ignores it, a worklet
//    that predates it posts none. Not a reply to a 'song' message: that one has `ok`.
//   {type: 'songscope', frame, frames: 512, sources: Uint32Array, channels: Uint32Array,
//    written: Float64Array, data: Float32Array}  with each 'songpos', for the songs that
//    have their scopes on (SongControl 'Scopes'): per song its channel count, then in `data`
//    each channel's latest 512 mono frames, oldest first (`written` is how many frames each
//    channel's ring had seen, for each channel of each song in order)
//   {type: 'error', message}

// Telemetry goes out every this many render quanta (about 21 ms at 48 kHz),
// and at once when there are events.
const TELEMETRY_BLOCKS = 8;
const METER_FIELDS = 5;
const TAP_FRAMES = 2048;
const TAP_HZ = 40;
const SONG_HZ = 60;

class CubedMixerProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.exports = null;
    this.pending = [];
    this.blocks = 0;
    this.tapsOn = false;
    this.songsOn = false;
    this.warming = false;
    this.tapEvery = 1;
    this.songEvery = 1;
    this.port.onmessage = (event) => this.receive(event.data);
  }

  receive(message) {
    if (message.type === 'init') {
      this.init(message);
    } else if (this.exports) {
      this.handle(message);
    } else {
      this.pending.push(message);
    }
  }

  async init(message) {
    try {
      const { instance } = await WebAssembly.instantiate(message.wasm, {});
      const x = instance.exports;
      x.cw_init(
        sampleRate,
        message.maxVoices >>> 0,
        message.maxBuses >>> 0,
        message.maxClips >>> 0,
        message.seedLo >>> 0,
        message.seedHi >>> 0,
      );
      this.exports = x;
      // Taps go out about `tapHz` times a second (a render quantum is 128 frames).
      this.tapEvery = Math.max(1, Math.round(sampleRate / 128 / (message.tapHz > 0 ? message.tapHz : TAP_HZ)));
      this.songEvery = Math.max(1, Math.round(sampleRate / 128 / (message.songHz > 0 ? message.songHz : SONG_HZ)));
      for (const queued of this.pending) this.handle(queued);
      this.pending = [];
      this.port.postMessage({ type: 'ready' });
    } catch (error) {
      this.port.postMessage({ type: 'error', message: String(error) });
    }
  }

  // Memory can grow on any call that allocates, which detaches old views,
  // so every view is made right before it is used.
  handle(message) {
    const x = this.exports;
    try {
      if (message.type === 'commands') {
        const bytes = new Uint8Array(message.bytes);
        const at = x.cw_inbox(bytes.length);
        new Uint8Array(x.memory.buffer, at, bytes.length).set(bytes);
        x.cw_apply(bytes.length);
      } else if (message.type === 'clip') {
        const { left, right } = message;
        const frames = left.length;
        const channels = right ? 2 : 1;
        const at = x.cw_clip_buffer(frames, channels);
        new Float32Array(x.memory.buffer, at, frames).set(left);
        if (right) {
          const rightAt = x.cw_clip_right();
          new Float32Array(x.memory.buffer, rightAt, frames).set(right.subarray(0, frames));
        }
        x.cw_add_clip(message.id >>> 0, message.sampleRate, frames, channels);
      } else if (message.type === 'song') {
        const bytes = new Uint8Array(message.bytes);
        const at = x.cw_file_buffer(bytes.length);
        new Uint8Array(x.memory.buffer, at, bytes.length).set(bytes);
        const separation = typeof message.separation === 'number' ? message.separation : -1;
        // The formats that can be put under live control: AHX/HVL, A2M, SID, MOD, S3M and XM (all of them).
        const module = (message.format >= 3 && message.format <= 5) || message.format <= 2;
        let ok;
        if (module && (message.stopped || message.follow)) {
          // Where a PSID tune starts, in seconds: its code is run to there while it builds.
          x.cw_prepare_start(typeof message.start === 'number' ? message.start : 0);
          ok = x.cw_prepare_module(
            message.source >>> 0,
            message.format >>> 0,
            (message.subsong ?? 0) >>> 0,
            message.looped === false ? 0 : 1,
            separation,
            message.stopped ? 1 : 0,
          ) === 1;
          this.songsOn = x.cw_song_count() > 0;
        } else {
          ok = x.cw_prepare_song(
            message.source >>> 0,
            message.format >>> 0,
            (message.subsong ?? 0) >>> 0,
            message.looped === false ? 0 : 1,
            separation,
          ) === 1;
          this.songsOn = x.cw_song_count() > 0;
        }
        const reply = { type: 'song', source: message.source, ok };
        if (!ok) {
          // No TextDecoder in a worklet scope; the reason is ASCII.
          const why = new Uint8Array(x.memory.buffer, x.cw_error(), x.cw_error_len());
          reply.error = String.fromCharCode(...why);
        }
        this.port.postMessage(reply);
      } else if (message.type === 'synth') {
        const bytes = new Uint8Array(message.bytes);
        const at = x.cw_file_buffer(bytes.length);
        new Uint8Array(x.memory.buffer, at, bytes.length).set(bytes);
        let ok;
        if ((message.held && message.held.length > 0) || message.warmBlocks > 0) {
          const held = message.held ?? [];
          const room = x.cw_held_buffer(held.length);
          const view = new Float32Array(x.memory.buffer, room, held.length * 2);
          held.forEach(([note, velocity], i) => {
            view[i * 2] = note;
            view[i * 2 + 1] = velocity;
          });
          ok = x.cw_prepare_synth_held(
            message.source >>> 0,
            message.oneShot ? 1 : 0,
            Math.min(32, message.warmBlocks >>> 0),
            held.length,
          ) === 1;
        } else {
          ok = x.cw_prepare_synth(message.source >>> 0, message.oneShot ? 1 : 0) === 1;
        }
        const reply = { type: 'song', source: message.source, ok };
        if (!ok) {
          const why = new Uint8Array(x.memory.buffer, x.cw_error(), x.cw_error_len());
          reply.error = String.fromCharCode(...why);
        }
        this.port.postMessage(reply);
      } else if (message.type === 'swap') {
        const patch = new Uint8Array(message.bytes);
        // The pointer first: asking for room can grow the wasm memory, which
        // detaches `x.memory.buffer` as it was.
        const patchAt = x.cw_file_buffer(patch.length);
        new Uint8Array(x.memory.buffer, patchAt, patch.length).set(patch);
        const play = new Uint8Array(message.play);
        const playAt = x.cw_swap_play_buffer(play.length);
        new Uint8Array(x.memory.buffer, playAt, play.length).set(play);
        const held = message.held ?? [];
        const room = x.cw_held_buffer(held.length);
        const view = new Float32Array(x.memory.buffer, room, held.length * 2);
        held.forEach(([note, velocity], i) => {
          view[i * 2] = note;
          view[i * 2 + 1] = velocity;
        });
        const ok = x.cw_swap_begin(
          message.source >>> 0,
          message.oneShot ? 1 : 0,
          message.oldVoice,
          message.fadeFrames >>> 0,
          message.warmBlocks >>> 0,
          message.sliceBlocks >>> 0,
          held.length,
        ) === 1;
        // A refused swap leaves one that is pending where it was.
        this.warming = x.cw_swap_pending() === 1;
        const reply = { type: 'swap', phase: ok ? 'started' : 'failed' };
        if (!ok) {
          const why = new Uint8Array(x.memory.buffer, x.cw_error(), x.cw_error_len());
          reply.error = String.fromCharCode(...why);
        }
        this.port.postMessage(reply);
      } else if (message.type === 'tap') {
        const ok = x.cw_set_tap(message.bus >>> 0, message.enable ? 1 : 0) === 1;
        this.tapsOn = x.cw_tap_count() > 0;
        this.port.postMessage({ type: 'tap', bus: message.bus, enable: !!message.enable, ok });
      } else if (message.type === 'effect') {
        const params = message.params ?? new Float32Array(0);
        // 6 floats before the equaliser; a wasm built then has no count.
        const count = x.cw_effect_param_count ? x.cw_effect_param_count() : 6;
        const kind = message.kind >= 0 ? message.kind >>> 0 : 0xffffffff;
        // The defaults first (a wasm built before the equaliser has none: zeros).
        const defaults = x.cw_effect_defaults ? x.cw_effect_defaults(kind) : 0;
        const room = new Float32Array(x.memory.buffer, x.cw_effect_params(), count);
        if (!defaults) room.fill(0);
        room.set(params.subarray(0, count));
        x.cw_set_effect(message.bus >>> 0, message.slot >>> 0, kind);
      }
    } catch (error) {
      this.port.postMessage({ type: 'error', message: String(error) });
    }
  }

  process(inputs, outputs) {
    const x = this.exports;
    const out = outputs[0];
    if (!x || !out || out.length === 0) return true;
    const frames = out[0].length;
    if (this.warming) this.warm(x, frames);
    x.cw_process(frames);
    const memory = x.memory.buffer;
    out[0].set(new Float32Array(memory, x.cw_left(), frames));
    if (out.length > 1) out[1].set(new Float32Array(memory, x.cw_right(), frames));

    this.blocks += 1;
    const eventBytes = x.cw_events_len();
    if (eventBytes > 0 || this.blocks % TELEMETRY_BLOCKS === 0) {
      const events = new Uint8Array(memory, x.cw_events(), eventBytes).slice();
      x.cw_clear_events();
      const meters = new Float32Array(memory, x.cw_meters(), x.cw_meter_count() * METER_FIELDS).slice();
      this.port.postMessage(
        {
          type: 'telemetry',
          frame: x.cw_frame(),
          voices: x.cw_voices(),
          rejected: x.cw_rejected(),
          badBatches: x.cw_bad_batches(),
          events: events.buffer,
          meters,
        },
        [events.buffer, meters.buffer],
      );
    }
    if (this.tapsOn && this.blocks % this.tapEvery === 0) this.postTaps(x);
    if (this.songsOn && this.blocks % this.songEvery === 0) this.postSongs(x);
    return true;
  }

  // A pending swap's slice of warm-up, ahead of the quantum; tells the page how
  // it ended.
  warm(x, frames) {
    const step = x.cw_warm_step(frames);
    if (step === 1) return;
    this.warming = false;
    if (step === 2) {
      this.port.postMessage({ type: 'swap', phase: 'done', old: x.cw_swap_old(), new: x.cw_swap_new(), frame: x.cw_frame() });
    } else {
      this.port.postMessage({ type: 'swap', phase: 'cancelled' });
    }
  }

  // The followed module songs that moved: their position and levels, and the channel
  // scopes of those that have them on. Once a song is gone there is nothing left to follow.
  postSongs(x) {
    const count = x.cw_songs_prepare();
    if (count === 0) {
      this.songsOn = x.cw_song_count() > 0;
      return;
    }
    // Every pointer first, then the views (none of these calls allocates, but memory
    // can grow on any call that does).
    const words = x.cw_song_status_words();
    const slots = x.cw_song_level_slots();
    const voiceWords = x.cw_song_voice_words();
    const at = {
      sources: x.cw_song_sources(),
      status: x.cw_song_status(),
      voices: x.cw_song_voices(),
      levels: x.cw_song_levels(),
      scopeSources: x.cw_song_scope_sources(),
      channels: x.cw_song_scope_channels(),
      written: x.cw_song_scope_written(),
      data: x.cw_song_scope_data(),
    };
    const scoped = x.cw_song_scope_count();
    const dataLen = x.cw_song_scope_len();
    const frame = x.cw_frame();
    const frames = x.cw_song_scope_frames();
    const memory = x.memory.buffer;
    const sources = new Uint32Array(memory, at.sources, count).slice();
    const status = new Uint32Array(memory, at.status, count * words).slice();
    const levels = new Float32Array(memory, at.levels, count * slots).slice();
    const voices = new Uint32Array(memory, at.voices, count * voiceWords).slice();
    this.port.postMessage(
      { type: 'songpos', frame, sources, status, levels, voices },
      [sources.buffer, status.buffer, levels.buffer, voices.buffer],
    );
    if (scoped > 0) {
      const scopeSources = new Uint32Array(memory, at.scopeSources, scoped).slice();
      const channels = new Uint32Array(memory, at.channels, scoped).slice();
      const total = channels.reduce((a, b) => a + b, 0);
      const written = new Float64Array(memory, at.written, total).slice();
      const data = new Float32Array(memory, at.data, dataLen).slice();
      this.port.postMessage(
        {
          type: 'songscope',
          frame,
          frames,
          sources: scopeSources,
          channels,
          written,
          data,
        },
        [scopeSources.buffer, channels.buffer, written.buffer, data.buffer],
      );
    }
  }

  postTaps(x) {
    const count = x.cw_taps_prepare(TAP_FRAMES);
    if (count === 0) return;
    const memory = x.memory.buffer;
    const buses = new Uint32Array(memory, x.cw_tap_buses(), count).slice();
    const written = new Float64Array(memory, x.cw_tap_written(), count).slice();
    const data = new Float32Array(memory, x.cw_tap_data(), count * 2 * TAP_FRAMES).slice();
    this.port.postMessage(
      { type: 'taps', frame: x.cw_frame(), frames: TAP_FRAMES, buses, written, data },
      [buses.buffer, written.buffer, data.buffer],
    );
  }
}

registerProcessor('cubed-mixer', CubedMixerProcessor);
