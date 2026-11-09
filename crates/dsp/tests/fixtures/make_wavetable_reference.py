#!/usr/bin/env python3
"""Regenerates wavetable_reference.json: numbers from the DAW's own wavetable code.

Runs the DAW's `WavetableUtils.ts` and the preset method of `WavetableEditor.vue`
(cut out of the .vue file as it is) under Node (22.6 or newer, which strips the
TypeScript types) and writes what they compute: waveforms from harmonics, warps,
`processWaveform`, the twelve presets as frames, a whole export with its 44-byte
header. `tests/wavetable_reference.rs` compares cubed-dsp's `wavetable_edit` with it.

    python3 make_wavetable_reference.py [path to the another_synth checkout]
"""
import os
import shutil
import subprocess
import sys
import tempfile

DAW = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.expanduser("~"), "src", "rust", "another_synth")
WAVE_TABLE = os.path.join(DAW, "src", "components", "WaveTable")

REF_TS = r"""
import { handlePresetChange } from './presets.ts';
import {
  generateWaveformFromHarmonics, applyWaveWarp, generateWaveformAtPositionForWav,
  processWaveform, exportWavetableToWav,
} from './WavetableUtils.ts';

const f32 = (a: ArrayLike<number>) => Array.from(a, (x) => Math.fround(x));
const out: any = {};

// 1. harmonics -> waveform, 16 points, f64
out.harm16 = Array.from(generateWaveformFromHarmonics([
  { amplitude: 1, phase: 0 }, { amplitude: 0.5, phase: 0.3 }, { amplitude: 0.25, phase: -1 },
], 16));

// 2. warp on a ramp of 16
const ramp = new Float64Array(16).map((_, i) => (i / 15) * 2 - 1);
out.warp_sym = Array.from(applyWaveWarp(ramp, { xAmount: 2, yAmount: 1, asymmetric: false }));
out.warp_asym = Array.from(applyWaveWarp(ramp, { xAmount: -1.5, yAmount: 3, asymmetric: true }));

// presets
function preset(name: string, n = 32) {
  const ctx: any = { selectedNumHarmonics: n, keyframes: [], waveWarpKeyframes: [
    { time: 0, params: { xAmount: 0, yAmount: 0, asymmetric: false } }], scrubPosition: 0, selectedKeyframe: 0, selectedPreset: '' };
  handlePresetChange.call(ctx, name);
  return ctx;
}
const names = ['sine','harmonicSeries','square','sawtooth','triangle','pulsePWM','analogPWM','brightBrass','warmPad','vocalFormant','metallicBell','evolvingTexture'];
out.presets = {};
for (const name of names) {
  const ctx = preset(name, 32);
  // generate 9 frames of 64 samples with the editor's Apply settings (removeDC false, normalize true)
  const frames: number[][] = [];
  const nf = 5, len = 32;
  for (let i = 0; i < nf; i++) {
    const position = (i / (nf - 1)) * 100;
    let w = generateWaveformAtPositionForWav(position, ctx.keyframes, ctx.waveWarpKeyframes, len);
    w = processWaveform(w, false, true);
    frames.push(f32(w));
  }
  out.presets[name] = { keyframes: ctx.keyframes.length, frames };
}
// keyframes of two presets at index: harmonic amplitudes/phases of keyframe 100 of pulsePWM
{
  const ctx = preset('pulsePWM', 32);
  out.pwm_kf100 = ctx.keyframes[100].harmonics.slice(0, 4);
  out.pwm_kf_time = [ctx.keyframes[0].time, ctx.keyframes[100].time, ctx.keyframes[255].time];
  const a = preset('analogPWM', 32);
  out.apwm_kf200 = a.keyframes[200].harmonics.slice(0, 4);
  const t = preset('triangle', 8);
  out.tri8 = t.keyframes[0].harmonics;
}
// 4. removeDC + normalize bug-compatibility: a waveform with DC offset
{
  const w = new Float32Array([0.5, 1.5, 1.0, 0.0, 2.0, -1.0]);
  out.dc_norm = f32(processWaveform(w, true, true));
  out.dc_only = f32(processWaveform(w, true, false));
  out.norm_only = f32(processWaveform(w, false, true));
}
// 5. a full export: two keyframes + a warp, 5 frames of 16, removeDC true normalize true; check the header bytes too
const kf = [
  { time: 0, harmonics: [{ amplitude: 1, phase: 0 }, { amplitude: 0, phase: 0 }, { amplitude: 0, phase: 0 }] },
  { time: 100, harmonics: [{ amplitude: 0.3, phase: 0.5 }, { amplitude: 0.6, phase: 1 }, { amplitude: 0.2, phase: 2 }] },
];
const wk = [
  { time: 0, params: { xAmount: 0, yAmount: 0, asymmetric: false } },
  { time: 100, params: { xAmount: 1, yAmount: 2, asymmetric: true } },
];
const buf = await exportWavetableToWav(kf as any, wk as any, 5, 16, true, true);
const view = new DataView(buf);
const samples: number[] = [];
for (let i = 0; i < 5 * 16; i++) samples.push(view.getFloat32(44 + i * 4, true));
out.export5x16 = { bytes: buf.byteLength, samples };
console.log(JSON.stringify(out));

"""


def extract_method(vue_text, signature):
    start = vue_text.index(signature)
    i = vue_text.index("{", start)
    depth = 0
    j = i
    while True:
        c = vue_text[j]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                break
        j += 1
    return vue_text[i : j + 1]


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    work = tempfile.mkdtemp(prefix="wt_ref_")
    try:
        shutil.copy(os.path.join(WAVE_TABLE, "WavetableUtils.ts"), work)
        with open(os.path.join(WAVE_TABLE, "WavetableEditor.vue"), encoding="utf-8") as f:
            body = extract_method(f.read(), "handlePresetChange(newVal: string) {")
        with open(os.path.join(work, "presets.ts"), "w", encoding="utf-8") as f:
            f.write("export function handlePresetChange(this: any, newVal: string) " + body + "\n")
        with open(os.path.join(work, "ref.ts"), "w", encoding="utf-8") as f:
            f.write(REF_TS)
        out = subprocess.run(["node", "ref.ts"], cwd=work, check=True, capture_output=True, text=True).stdout
        with open(os.path.join(here, "wavetable_reference.json"), "w", encoding="utf-8") as f:
            f.write(out)
    finally:
        shutil.rmtree(work, ignore_errors=True)


main()
