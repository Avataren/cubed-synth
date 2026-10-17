#!/usr/bin/env bash
# The writer's black-box check against AdPlug (plan-opl.md, A2M writer): every
# corpus file is parsed and rewritten by `write::write`, then AdPlug's own
# player (Ca2mv2Player, through trace-oracle) must make the same register
# writes for the first 4000 ticks from the rewrite as from the original.
# Needs ADPLUG_PREFIX (see regen.sh), a nightly cargo on PATH and python3.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
prefix="${ADPLUG_PREFIX:?set ADPLUG_PREFIX to an AdPlug install prefix}"
corpus="$here/../../../tests/fixtures/a2m"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
g++ -O1 -o "$tmp/trace-oracle" "$here/trace-oracle.cpp" -I"$prefix/include" -I"$prefix/include/adplug" -L"$prefix/lib" -ladplug -llibbinio
(cd "$here/../../.." && A2M_DUMP_DIR="$tmp/written" cargo test --release --lib -- --ignored a2::tests::dump_rewritten >/dev/null)
CORPUS="$corpus" TMP="$tmp" python3 - <<'PY'
import os, subprocess
root, tmp = os.environ['CORPUS'], os.environ['TMP']
files = sorted(os.path.relpath(os.path.join(d, f), root) for d, _, fs in os.walk(root) for f in fs if f.lower().endswith('.a2m'))
env = dict(os.environ, A2M_PLAYER='v2')
trace = lambda p: subprocess.run([tmp + '/trace-oracle', 'trace', p, '4000'], capture_output=True, env=env).stdout
bad = [f for i, f in enumerate(files) if trace(os.path.join(root, f)) != trace('%s/written/%03d.a2m' % (tmp, i))]
print('%d files, %d differ' % (len(files), len(bad)))
for f in bad: print('  ' + f)
raise SystemExit(1 if bad else 0)
PY
