// The cubed-mixer AudioWorkletProcessor: runs cubed_worklet.wasm (the page
// sends its bytes in the 'init' message) and plays the mix it renders.
//
// Register it with `audioWorklet.addModule(<this file>)`, then create
// `new AudioWorkletNode(context, 'cubed-mixer', {numberOfInputs: 0,
// outputChannelCount: [2]})` and talk to its port.
//
// From the page, in order (messages before 'ready' are queued):
//   {type: 'init', wasm: ArrayBuffer, maxVoices, maxBuses, maxClips, seedLo, seedHi}
//   {type: 'commands', bytes: ArrayBuffer}  cubed_mixer::wire commands
//   {type: 'clip', id, sampleRate, left: Float32Array, right?: Float32Array}
//   {type: 'song', source, format (0 AHX/HVL, 1 A2M, 2 SID, 3 MOD, 4 S3M, 5 XM), bytes: ArrayBuffer,
//    subsong, looped, separation?}  prepares source slot `source` for a
//    PlaySource command; separation (0-1) is a module's stereo width
//   {type: 'effect', bus, slot, kind (EffectKind index; -1 empties the slot),
//    params: Float32Array}
//   {type: 'synth', source, bytes: ArrayBuffer (the patch JSON, UTF-8), oneShot}
//    prepares source slot `source` with a synth patch, played by
//    SourceEvent commands
// To the page:
//   {type: 'ready'}
//   {type: 'song', source, ok, error?}  after each 'song' or 'synth' message
//   {type: 'telemetry', frame, voices, rejected, badBatches,
//    events: ArrayBuffer (wire events), meters: Float32Array (5 per bus)}
//   {type: 'error', message}

// Telemetry goes out every this many render quanta (about 21 ms at 48 kHz),
// and at once when there are events.
const TELEMETRY_BLOCKS = 8;
const METER_FIELDS = 5;

class CubedMixerProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.exports = null;
    this.pending = [];
    this.blocks = 0;
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
          new Float32Array(x.memory.buffer, x.cw_clip_right(), frames).set(right.subarray(0, frames));
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
        const ok = x.cw_prepare_synth(message.source >>> 0, message.oneShot ? 1 : 0) === 1;
        const reply = { type: 'song', source: message.source, ok };
        if (!ok) {
          const why = new Uint8Array(x.memory.buffer, x.cw_error(), x.cw_error_len());
          reply.error = String.fromCharCode(...why);
        }
        this.port.postMessage(reply);
      } else if (message.type === 'effect') {
        const params = message.params ?? new Float32Array(0);
        const room = new Float32Array(x.memory.buffer, x.cw_effect_params(), 6);
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
    return true;
  }
}

registerProcessor('cubed-mixer', CubedMixerProcessor);
