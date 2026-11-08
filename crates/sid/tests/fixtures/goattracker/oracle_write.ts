// The DAW's GoatTracker writer as an oracle for tests/sng_write.rs: for each .sng (imported
// first, as the DAW imports it) or .asid (parsed) under the given folders it records the
// verdict and the .sng bytes (inputs that are no song are left out) another_synth's own exportGtSong gives. Run it as oracle.ts is run:
//
//   DAW=~/src/rust/another_synth
//   $DAW/node_modules/.bin/esbuild oracle_write.ts --bundle --platform=node --format=esm \
//       --alias:another_synth=$DAW --outfile=$TMPDIR/oracle_write.mjs
//   node $TMPDIR/oracle_write.mjs expected_sng.json .                  # the fixtures here
//   node $TMPDIR/oracle_write.mjs ~/corpus_sng.json $DAW/public/demos  # a corpus (absolute paths), then
//   CUBED_SNG_WRITE_ORACLE=~/corpus_sng.json cargo test -p cubed-sid --test sng_write
import { exportGtSong, importGtSong, parseSidFile } from 'another_synth/src/audio/tracker/sid-doc';
import { gtSongHintsFromName } from 'another_synth/src/audio/tracker/sid-doc/gt-sng-common';
import { readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { basename, isAbsolute, join, resolve } from 'node:path';

const [, , out, ...roots] = process.argv;
const files: string[] = [];
const walk = (dir: string) => {
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) walk(path);
    else if (name.toLowerCase().endsWith('.sng') || name.endsWith('.asid')) files.push(path);
  }
};
roots.forEach((root) => walk(root));
const results: Record<string, unknown> = {};
for (const file of files) {
  const bytes = new Uint8Array(readFileSync(file));
  const key = roots.some((root) => isAbsolute(root)) ? resolve(file) : file.replace(/^\.\//, '');
  let doc;
  if (file.endsWith('.asid')) {
    try {
      doc = parseSidFile(bytes);
    } catch {
      continue; // not a song: nothing to write
    }
  } else {
    const r = importGtSong(bytes, gtSongHintsFromName(basename(file)));
    if (!r.ok) continue; // not a song: nothing to write
    doc = r.doc;
  }
  const w = exportGtSong(doc);
  results[key] = w.ok
    ? { ok: true, notes: w.notes, sng: Buffer.from(w.bytes).toString('hex') }
    : { ok: false, reason: w.reason };
}
writeFileSync(out, JSON.stringify(results, null, 1) + '\n');
console.log(`${files.length} songs, ${Object.values(results).filter((r) => (r as { ok: boolean }).ok).length} written`);
