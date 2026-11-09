// The DAW's own `.sid` importer as an oracle for tests/psid_import.rs: for each .sid in a list it
// records what another_synth's `importPsid` (src/audio/tracker/psid/index.ts) makes of it: the
// verdict, how it was made, the notes, the fidelity, per subsong report, and the song as the DAW's
// own `.asid` file (`serializeSidFile`, base64), which `SidSong::to_bytes` writes byte for byte.
//
// Build it with build_import_oracle.mjs (it needs the DAW's node_modules for esbuild), then:
//   node $SCRATCH/psid-import-oracle.mjs out.json list.txt [capture|import|start] [maxSeconds]
//
//  - `import` (default): the whole import, every subsong, as the app does it;
//  - `start`: the same with only the start subsong imported (`subsongs: [start]`), much faster;
//  - `capture`: only the capture of the start song (ticks, loop, a hash of the registers);
//  - `gt`: only the GoatTracker unpack (`unpackGtSid`): whether the file is one, what it unpacks to, and
//    whether packing that again gives the file's bytes (`exact`).
//
//   CUBED_PSID_IMPORT_ORACLE=out.json cargo test --release -p cubed-sid --test psid_import oracle
import { parsePsid } from 'src/audio/tracker/psid/psid-file';
import { captureSid } from 'src/audio/tracker/psid/sid-capture';
import { importPsid } from 'src/audio/tracker/psid';
import { unpackGtSid } from 'src/audio/tracker/psid/gt-unpack';
import { serializeSidFile } from 'src/audio/tracker/sid-doc/sid-file-codec';
import { readFileSync, writeFileSync } from 'node:fs';

const [, , out, listFile, modeArg, secondsArg] = process.argv;
const mode = modeArg ?? 'import';
const maxSeconds = secondsArg === undefined ? undefined : Number(secondsArg);
const paths = readFileSync(listFile!, 'utf8').split('\n').filter((l) => l.trim() !== '');

const fnv = (bytes: ArrayLike<number>): number => {
  let h = 0x811c9dc5;
  for (let i = 0; i < bytes.length; i++) h = Math.imul(h ^ (bytes[i]! & 0xff), 0x01000193) >>> 0;
  return h;
};

const results: Record<string, unknown> = {};
for (const path of paths) {
  const t0 = performance.now();
  const bytes = new Uint8Array(readFileSync(path));
  const parsed = parsePsid(bytes);
  if (!parsed.ok) {
    results[path] = { ok: false, reason: parsed.reason };
    continue;
  }
  const file = parsed.file;
  try {
    if (mode === 'gt') {
      const u = unpackGtSid(file);
      results[path] = u.ok ? { ok: true, exact: u.exact, notes: u.notes, asid: Buffer.from(serializeSidFile(u.doc)).toString('base64'), ms: performance.now() - t0 } : { ok: false, reason: u.reason };
      continue;
    }
    if (mode === 'capture') {
      const c = captureSid(file, maxSeconds === undefined ? { subsong: file.startSong - 1 } : { subsong: file.startSong - 1, maxSeconds });
      if (!c.ok) {
        results[path] = { ok: false, reason: c.reason };
        continue;
      }
      const t = c.trace;
      results[path] = {
        ok: true,
        mode: t.mode,
        ticks: t.ticks,
        tickCycles: t.tickCycles,
        loopTick: t.loopTick,
        end: t.end,
        digi: t.digi,
        extra: t.extraSidWrites,
        writesPerTick: t.writesPerTick,
        regs: fnv(t.regs),
        events: fnv(t.voiceEvents),
        ms: performance.now() - t0,
      };
      continue;
    }
    const options = { ...(maxSeconds === undefined ? {} : { maxSeconds }), ...(mode === 'start' ? { subsongs: [file.startSong - 1] } : {}) };
    const r = importPsid(bytes, options);
    if (!r.ok) {
      results[path] = { ok: false, reason: r.reason, ms: performance.now() - t0 };
      continue;
    }
    results[path] = {
      ok: true,
      method: r.method,
      exact: r.exact,
      notes: r.notes,
      fidelity: r.fidelity === null ? null : { score: r.fidelity.score, filter: r.fidelity.filter, frames: r.fidelity.frames, offset: r.fidelity.offset, voiceOffsets: r.fidelity.voiceOffsets, voices: r.fidelity.voices },
      reports: r.reports.map((s) => ({
        subsong: s.subsong,
        gtSubsong: s.gtSubsong,
        reason: s.reason ?? null,
        frames: s.frames ?? null,
        notes: s.notes ?? null,
        folded: s.folded ?? null,
        loop: s.loop ?? null,
        rows: s.rows ?? null,
        cutRows: s.cutRows ?? null,
        grid: s.grid === undefined ? null : { starts: s.grid.starts.length, startsHash: fnv(s.grid.starts.map((x) => x + 1)), first: s.grid.starts[0], delays: s.grid.delays, coverage: s.grid.coverage, lead: s.grid.lead },
        trace:
          s.trace === undefined
            ? null
            : { mode: s.trace.mode, ticks: s.trace.ticks, tickCycles: s.trace.tickCycles, loopTick: s.trace.loopTick, end: s.trace.end, digi: s.trace.digi, regs: fnv(s.trace.regs) },
      })),
      asid: Buffer.from(serializeSidFile(r.doc)).toString('base64'),
      ms: performance.now() - t0,
    };
  } catch (e) {
    results[path] = { ok: false, threw: String((e as Error).stack ?? e) };
  }
}
writeFileSync(out!, JSON.stringify(results));
