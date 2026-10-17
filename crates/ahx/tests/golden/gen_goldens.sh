#!/usr/bin/env bash
# Regenerates every *.txt golden next to this script, one per row of
# cases.manifest (which the Rust test also reads), from the vendored C
# reference (.ai/ahx/references/hvl_replay.c, compiled natively with gcc).
# Needs gcc; cargo test itself never runs this and needs no C toolchain.
# Why each row is there, and the column meanings, are in cases.manifest.
# Every golden is C-generated; nothing under this directory is hand-edited.
set -euo pipefail
cd "$(dirname "$0")"
REF=../../../.ai/ahx/references
DEMOS=../../tests/fixtures/songs
gcc -O0 -fwrapv -fcommon -w -I"$REF" -o /tmp/hvl_golden hvl_golden.c "$REF/hvl_replay.c" "$REF/hvl_tables.c" -lm

# Drop goldens whose manifest row was removed, so none outlives its source.
rm -f ./*.txt

while read -r fixture freq stereo frames chunk cap; do
  [[ -z "${fixture}" || "${fixture}" == \#* ]] && continue
  out="${fixture%.*}.${freq}.s${stereo}.cap${cap}.txt"
  args=("$DEMOS/$fixture" "$freq" "$stereo" "$frames" "$chunk")
  [[ "$cap" != 0 ]] && args+=("$cap")
  /tmp/hvl_golden "${args[@]}" > "$out"
  echo "wrote $out"
done < cases.manifest
