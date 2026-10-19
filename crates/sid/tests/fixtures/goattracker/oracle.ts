// The DAW's GoatTracker import as an oracle for tests/goattracker.rs: for each .sng under the
// given folders it records the verdict, the notes and the .asid bytes another_synth's own
// importGtSong and serializeSidFile give. Run it with the DAW's esbuild and node:
//
//   DAW=~/src/rust/another_synth
//   $DAW/node_modules/.bin/esbuild oracle.ts --bundle --platform=node --format=esm \
//       --alias:another_synth=$DAW --outfile=/tmp/oracle.mjs
//   node /tmp/oracle.mjs expected.json .                    # the fixtures here (relative names)
//   node /tmp/oracle.mjs ~/corpus.json $DAW/public/demos     # a corpus (absolute paths), then
//   CUBED_SNG_ORACLE=~/corpus.json cargo test -p cubed-sid --test goattracker
import { importGtSong, serializeSidFile } from 'another_synth/src/audio/tracker/sid-doc';
import { gtSongHintsFromName } from 'another_synth/src/audio/tracker/sid-doc/gt-sng-common';
import { readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { basename, isAbsolute, join, resolve } from 'node:path';

const [, , out, ...roots] = process.argv;
const files: string[] = [];
const walk = (dir: string) => {
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) walk(path);
    else if (name.toLowerCase().endsWith('.sng') || name.endsWith('.xm')) files.push(path);
  }
};
// A relative root records names relative to it; an absolute one, absolute paths.
roots.forEach((root) => walk(isAbsolute(root) ? root : root));
const results: Record<string, unknown> = {};
for (const file of files) {
  const bytes = new Uint8Array(readFileSync(file));
  const r = importGtSong(bytes, gtSongHintsFromName(basename(file)));
  const key = roots.some((root) => isAbsolute(root)) ? resolve(file) : file.replace(/^\.\//, '');
  results[key] = r.ok
    ? {
        ok: true,
        variant: r.variant,
        notes: r.notes.map((n) => [n.kind, n.message]),
        asid: Buffer.from(serializeSidFile(r.doc)).toString('hex'),
      }
    : { ok: false, reason: r.reason };
}
writeFileSync(out, JSON.stringify(results, null, 1) + '\n');
console.log(`${files.length} songs, ${Object.values(results).filter((r) => (r as { ok: boolean }).ok).length} imported`);
