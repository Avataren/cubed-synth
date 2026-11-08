// The DAW's new SID song as an oracle for tests/edit.rs (`a_new_song_is_what_the_daw_makes`): the
// .asid bytes createNewSidDoc gives for a few options, or its refusal. Run it as oracle.ts is run:
//
//   DAW=~/src/rust/another_synth
//   $DAW/node_modules/.bin/esbuild oracle_new.ts --bundle --platform=node --format=esm \
//       --alias:another_synth=$DAW --alias:src=$DAW/src --outfile=$TMPDIR/oracle_new.mjs
//   node $TMPDIR/oracle_new.mjs expected_new.json
import { createNewSidDoc, serializeSidFile } from 'another_synth/src/audio/tracker/sid-doc';
import { writeFileSync } from 'node:fs';

const cases: Array<Record<string, unknown>> = [
  {},
  { songName: 'Tune', author: 'Me', chipModel: '8580' },
  { speedMultiplier: 2 },
  { speedMultiplier: 2, tempo: 18, patternRows: 32 },
  { speedMultiplier: 4, tempo: 40, patternRows: 128 },
  { tempo: 9, patternRows: 1 },
  { tempo: 2 },
  { tempo: 4, speedMultiplier: 2 },
  { patternRows: 0 },
  { patternRows: 129 },
  { speedMultiplier: 17 },
];
const out = cases.map((options) => {
  try {
    return { options, ok: true, asid: Buffer.from(serializeSidFile(createNewSidDoc(options as never))).toString('hex') };
  } catch (e) {
    return { options, ok: false, reason: (e as Error).message };
  }
});
writeFileSync(process.argv[2]!, JSON.stringify(out, null, 1) + '\n');
console.log(`${out.length} cases`);
