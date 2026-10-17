#!/usr/bin/env bash
# Regenerate the OPL golden renders from ymfm's YMF262.
#
#   rust-wasm/src/opl/golden/regen.sh [scratch-dir]
#
# Clones ymfm at the pinned commit into the scratch dir (default: a temp
# dir), builds opl3-oracle.cpp against it, and renders every *.txt script
# here to *.i16 (interleaved L/R int16, native rate). ymfm is BSD-3; nothing
# of it is vendored, only its output.
set -euo pipefail
YMFM_COMMIT=81aec25ccbb98f4873a255f7551ac4dadac59b4a
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="${1:-$(mktemp -d)}"
if [ ! -d "$WORK/ymfm" ]; then
  git clone -q https://github.com/aaronsgiles/ymfm.git "$WORK/ymfm"
fi
git -C "$WORK/ymfm" checkout -q "$YMFM_COMMIT"
Y="$WORK/ymfm/src"
g++ -O2 -std=c++17 -I"$Y" "$HERE/opl3-oracle.cpp" "$Y/ymfm_opl.cpp" "$Y/ymfm_adpcm.cpp" "$Y/ymfm_pcm.cpp" -o "$WORK/opl3-oracle"
for script in "$HERE"/*.txt; do
  "$WORK/opl3-oracle" < "$script" > "${script%.txt}.i16"
  echo "$(basename "$script") -> $(stat -c%s "${script%.txt}.i16") bytes"
done
