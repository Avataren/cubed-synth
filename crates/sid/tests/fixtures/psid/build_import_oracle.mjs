// Bundles import_oracle.ts with the DAW's esbuild (the DAW's `src/` and `@another-synth/tracker-playback`
// aliases, `?raw` imports as text, `.s` files as text).
//   node build_import_oracle.mjs ~/src/rust/another_synth $SCRATCH/psid-import-oracle.mjs
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';

const [, , daw, outfile] = process.argv;
const require = createRequire(join(resolve(daw), 'package.json'));
const esbuild = require('esbuild');
const here = dirname(fileURLToPath(import.meta.url));

const rawPlugin = {
  name: 'raw',
  setup(build) {
    build.onResolve({ filter: /\?raw$/ }, (a) => ({ path: resolve(a.resolveDir, a.path.replace(/\?raw$/, '')), namespace: 'raw' }));
    build.onLoad({ filter: /.*/, namespace: 'raw' }, (a) => ({ contents: `export default ${JSON.stringify(readFileSync(a.path, 'utf8'))};`, loader: 'js' }));
    build.onResolve({ filter: /^src\// }, (a) => {
      const base = join(resolve(daw), a.path);
      for (const ext of ['', '.ts', '/index.ts']) {
        try {
          readFileSync(base + ext);
          return { path: base + ext };
        } catch {
          /* next */
        }
      }
      return undefined;
    });
    build.onResolve({ filter: /^@another-synth\/tracker-playback$/ }, () => ({ path: join(resolve(daw), 'packages/tracker-playback/src/index.ts') }));
  },
};

await esbuild.build({
  entryPoints: [join(here, process.env.ENTRY ?? 'import_oracle.ts')],
  bundle: true,
  platform: 'node',
  format: 'esm',
  outfile,
  plugins: [rawPlugin],
  nodePaths: [join(resolve(daw), 'node_modules')],
  logLevel: 'warning',
});
