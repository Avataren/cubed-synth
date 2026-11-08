// The DAW's own PSID runner as an oracle for tests/psid.rs: for each .sid in a list it records
// what another_synth's PsidRunner (src/audio/tracker/psid/psid-runner.ts) does with the start
// song in the first N seconds: the verdict, the writes to the first SID (cycle, register,
// value: a 32-bit FNV-1a hash of all of them, their count, the first 32), and why it stopped.
// Run it with the DAW's esbuild and node:
//
//   DAW=~/src/rust/another_synth
//   $DAW/node_modules/.bin/esbuild oracle.ts --bundle --platform=node --format=esm \
//       --alias:another_synth=$DAW --outfile=$SCRATCH/psid-oracle.mjs
//   node $SCRATCH/psid-oracle.mjs out.json 10 list.txt         # 10 seconds; list.txt: one .sid path per line
//   CUBED_PSID_ORACLE=out.json CUBED_PSID_SECONDS=10 cargo test --release -p cubed-sid --test psid oracle
import { parsePsid } from 'another_synth/src/audio/tracker/psid/psid-file';
import { PsidRunner } from 'another_synth/src/audio/tracker/psid/psid-runner';
import { readFileSync, writeFileSync } from 'node:fs';

const [, , out, secondsArg, listFile] = process.argv;
const seconds = Number(secondsArg);
const paths = readFileSync(listFile!, 'utf8').split('\n').filter((l) => l.trim() !== '');

interface Verdict {
  ok: boolean;
  reason?: string;
  mode?: string;
  tickCycles?: number;
  clockHz?: number;
  count?: number;
  hash?: number;
  first?: number[][];
  ended?: string | null;
  extra?: number;
  songs?: number;
  start?: number;
}

const results: Record<string, Verdict> = {};
for (const path of paths) {
  const parsed = parsePsid(new Uint8Array(readFileSync(path)));
  if (!parsed.ok) {
    results[path] = { ok: false, reason: parsed.reason };
    continue;
  }
  const file = parsed.file;
  const made = PsidRunner.create(file, file.startSong - 1);
  if (!made.ok) {
    results[path] = { ok: false, reason: made.reason, songs: file.songs, start: file.startSong };
    continue;
  }
  const runner = made.runner;
  const cycles = Math.floor(runner.clockHz * seconds);
  runner.advance(cycles);
  let hash = 0x811c9dc5;
  let count = 0;
  const first: number[][] = [];
  const mix = (b: number) => {
    hash = Math.imul(hash ^ (b & 0xff), 0x01000193) >>> 0;
  };
  runner.drain(cycles, (cycle, reg, value) => {
    count++;
    if (first.length < 32) first.push([cycle, reg, value]);
    mix(cycle);
    mix(cycle >>> 8);
    mix(cycle >>> 16);
    mix(cycle >>> 24);
    mix(reg);
    mix(value);
  });
  results[path] = {
    ok: true,
    mode: runner.mode,
    tickCycles: runner.tickCycles,
    clockHz: runner.clockHz,
    count,
    hash,
    first,
    ended: runner.ended,
    extra: runner.extraSidWrites,
    songs: file.songs,
    start: file.startSong,
  };
}
writeFileSync(out!, JSON.stringify(results));
