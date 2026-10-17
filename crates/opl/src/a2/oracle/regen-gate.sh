#!/usr/bin/env bash
# Regenerates the O7 register gate (see gate.rs):
#   gate.tsv   - every corpus file: ticks AdPlug plays before its end flag
#                (capped at 120000), then the state hash per 512-tick chunk;
#   probes.tsv - every probes/*.py module (crafted by craft_a2m.py): 80
#                ticks, the parsed song in gate.rs's sparse form, the hashes.
# Needs AdPlug + libbinio under $ADPLUG_PREFIX (plan-opl.md, O6), python3 and
# a nightly cargo. The test itself needs none of them.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
prefix="${ADPLUG_PREFIX:?set ADPLUG_PREFIX to an AdPlug install prefix}"
crate="$here/../../.."
corpus="$crate/tests/fixtures/a2m"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
g++ -O1 -o "$tmp/trace-oracle" "$here/trace-oracle.cpp" -I"$prefix/include" -I"$prefix/include/adplug" -L"$prefix/lib" -ladplug -llibbinio
(cd "$crate" && cargo build -q --release --example a2m_tool)
tool="$crate/target/release/examples/a2m_tool"
export A2M_ORACLE="$tmp/trace-oracle"
{
  echo "# path	ticks	state hash per 512-tick chunk (AdPlug Ca2mv2Player via trace-oracle.cpp; regen-gate.sh)"
  (cd "$corpus" && find . -type f -iname '*.a2m' | sed 's|^\./||' | LC_ALL=C sort) | while IFS= read -r f; do
    printf '%s\t%s\n' "$f" "$("$tool" gate "$corpus/$f" 120000)"
  done
} > "$here/gate.tsv"
{
  echo "# probe	ticks	song (gate.rs sparse form)	state hash per 512-tick chunk (regen-gate.sh)"
  for p in "$here"/probes/*.py; do
    n="$(basename "$p" .py)"
    python3 "$here/craft_a2m.py" "$p" "$tmp/$n.a2m"
    printf '%s\t%s\n' "$n" "$("$tool" probegate "$tmp/$n.a2m" 80)"
    rm "$tmp/$n.a2m"
  done
} > "$here/probes.tsv"
echo "wrote $(grep -vc '^#' "$here/gate.tsv") songs, $(grep -vc '^#' "$here/probes.tsv") probes"
