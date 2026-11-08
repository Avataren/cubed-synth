// The cubed-mixer AudioWorkletProcessor: runs cubed_worklet.wasm (the page
// sends its bytes in the 'init' message) and plays the mix it renders.
//
// Register it with `audioWorklet.addModule(<this file>)`, then create
// `new AudioWorkletNode(context, 'cubed-mixer', {numberOfInputs: 0,
// outputChannelCount: [2]})` and talk to its port.
//
// From the page, in order (messages before 'ready' are queued):
//   {type: 'init', wasm: ArrayBuffer, maxVoices, maxBuses, maxClips, seedLo, seedHi, tapHz?}
//   {type: 'commands', bytes: ArrayBuffer}  cubed_mixer::wire commands
//   {type: 'clip', id, sampleRate, left: Float32Array, right?: Float32Array}
//   {type: 'song', source, format (0 AHX/HVL, 1 A2M, 2 SID, 3 MOD, 4 S3M, 5 XM), bytes: ArrayBuffer,
//    subsong, looped, separation?}  prepares source slot `source` for a
//    PlaySource command; separation (0-1) is a module's stereo width
//   {type: 'effect', bus, slot, kind (EffectKind index; -1 empties the slot),
//    params: Float32Array}
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
//   {type: 'error', message}

// Telemetry goes out every this many render quanta (about 21 ms at 48 kHz),
// and at once when there are events.
const TELEMETRY_BLOCKS = 8;
const METER_FIELDS = 5;
const TAP_FRAMES = 2048;
const TAP_HZ = 40;

class CubedMixerProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.exports = null;
    this.pending = [];
    this.blocks = 0;
    this.tapsOn = false;
    this.warming = false;
    this.tapEvery = 1;
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
        const ok = x.cw_prepare_song(
          message.source >>> 0,
          message.format >>> 0,
          (message.subsong ?? 0) >>> 0,
          message.looped === false ? 0 : 1,
          typeof message.separation === 'number' ? message.separation : -1,
        ) === 1;
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
        const paramsAt = x.cw_effect_params();
        const room = new Float32Array(x.memory.buffer, paramsAt, 6);
        room.fill(0);
        room.set(params.subarray(0, 6));
        const kind = message.kind >= 0 ? message.kind >>> 0 : 0xffffffff;
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
