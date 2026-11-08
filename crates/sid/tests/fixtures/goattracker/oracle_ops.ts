// The DAW's SID edits as an oracle for tests/ops_oracle.rs: for each .sng (imported first) or
// .asid under the given folders, and each case of ops_cases.json, it applies the DAW's own edit
// (insertSidTableRow, deleteSidTableRow, cloneSidInstrument, deleteSidInstrument, newSidInstrument,
// removeUnusedSidTableRows) and records the .asid bytes it then saves, or the refusal. "first",
// "mid", "last" and "end" name a row or instrument of the song (the Rust test resolves them the same
// way), so one case set fits every song. Run it as oracle.ts is run:
//
//   DAW=~/src/rust/another_synth
//   $DAW/node_modules/.bin/esbuild oracle_ops.ts --bundle --platform=node --format=esm \
//       --alias:another_synth=$DAW --alias:src=$DAW/src --outfile=$TMPDIR/oracle_ops.mjs
//   node $TMPDIR/oracle_ops.mjs expected_ops.json . ../sid           # the fixtures here
//   node $TMPDIR/oracle_ops.mjs ~/corpus_ops.json $DAW/public/demos  # a corpus (absolute paths), then
//   CUBED_OPS_ORACLE=~/corpus_ops.json cargo test -p cubed-sid --test ops_oracle
import {
  cloneSidInstrument,
  deleteSidInstrument,
  importGtSong,
  parseSidFile,
  serializeSidFile,
} from 'another_synth/src/audio/tracker/sid-doc';
import { newSidInstrument } from 'another_synth/src/audio/tracker/sid-instrument-edit';
import { deleteSidTableRow, insertSidTableRow, removeUnusedSidTableRows } from 'another_synth/src/audio/tracker/sid-table-rows';
import { gtSongHintsFromName } from 'another_synth/src/audio/tracker/sid-doc/gt-sng-common';
import { readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const [, , out, ...roots] = process.argv;
const cases = JSON.parse(readFileSync(join(process.cwd(), 'ops_cases.json'), 'utf8')) as Array<{ name: string; daw: Record<string, string> }>;
const files: string[] = [];
const walk = (dir: string) => {
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) walk(path);
    else if (name.toLowerCase().endsWith('.sng') || name.endsWith('.asid')) files.push(path);
  }
};
roots.forEach((root) => walk(root));

type Doc = ReturnType<typeof parseSidFile>;
const pick = (spec: string, len: number): number => (spec === 'first' ? 0 : spec === 'mid' ? Math.floor(len / 2) : spec === 'last' ? len - 1 : len);
const results: Record<string, Record<string, unknown>> = {};
void dirname; void fileURLToPath;
for (const file of files) {
  const bytes = new Uint8Array(readFileSync(file));
  let doc: Doc;
  if (file.endsWith('.asid')) {
    try {
      doc = parseSidFile(bytes);
    } catch {
      continue;
    }
  } else {
    const r = importGtSong(bytes, gtSongHintsFromName(basename(file)));
    if (!r.ok) continue;
    doc = r.doc;
  }
  const key = roots.some((root) => isAbsolute(root)) ? resolve(file) : file.replace(/^\.\//, '');
  const record: Record<string, unknown> = {};
  for (const c of cases) {
    const d = c.daw;
    let r;
    if (d.fn === 'insert') r = insertSidTableRow(doc, d.table as never, pick(d.at!, doc.tables[d.table as 'wave'].length) + 1);
    else if (d.fn === 'delete') {
      const len = doc.tables[d.table as 'wave'].length;
      r = len === 0 ? { ok: false as const, reason: `The ${d.table} table has no row 1.` } : deleteSidTableRow(doc, d.table as never, pick(d.at!, len) + 1);
    } else if (d.fn === 'clone') {
      const n = doc.instruments.length;
      r = n === 0 ? { ok: false as const, reason: 'There is no instrument 1.' } : cloneSidInstrument(doc, pick(d.instrument!, n) + 1);
    } else if (d.fn === 'deleteInstrument') {
      const n = doc.instruments.length;
      r = n === 0 ? { ok: false as const, reason: 'There is no instrument 1.' } : deleteSidInstrument(doc, pick(d.instrument!, n) + 1, { clearUses: true });
    } else if (d.fn === 'newInstrument') r = newSidInstrument(doc, '');
    else r = removeUnusedSidTableRows(doc);
    record[c.name] = r.ok ? { ok: true, asid: Buffer.from(serializeSidFile(r.doc)).toString('hex') } : { ok: false, reason: r.reason };
  }
  results[key] = record;
}
writeFileSync(out, JSON.stringify(results, null, 1) + '\n');
console.log(`${Object.keys(results).length} songs x ${cases.length} cases`);
