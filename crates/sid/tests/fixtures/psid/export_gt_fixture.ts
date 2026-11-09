// Writes `.sid` files the way the DAW exports a SID song (GoatTracker's own player with the song packed
// after it) from the `.asid` fixtures of tests/fixtures/sid/ (songs made for these tests), to test the
// GoatTracker unpack of `psid::import`. Build it with build_import_oracle.mjs (pass this file as the entry
// point's name: `ENTRY=export_gt_fixture.ts`) and run:
//   node $SCRATCH/export-gt-fixture.mjs ../sid/s59-drum-example.asid gt_drum.sid
import { parseSidFile } from 'src/audio/tracker/sid-doc/sid-file-codec';
import { exportSid } from 'src/audio/tracker/sid-export';
import { readFileSync, writeFileSync } from 'node:fs';

const [, , input, output] = process.argv;
const doc = parseSidFile(new Uint8Array(readFileSync(input!)));
const e = exportSid(doc);
if (!e.ok) throw new Error(e.reason);
writeFileSync(output!, e.bytes);
console.log(`${output}: ${e.bytes.length} bytes`);
