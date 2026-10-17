#!/usr/bin/env bash
# Regenerates blocks.tsv (AdPlug's unpacking of every A2M block in the corpus)
# and song-info.tsv (the title, author and instrument names its API reports).
# Needs AdPlug + libbinio installed under $ADPLUG_PREFIX (see plan-opl.md, O6:
# built from github.com/adplug/adplug and github.com/adplug/libbinio).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
prefix="${ADPLUG_PREFIX:?set ADPLUG_PREFIX to an AdPlug install prefix}"
corpus="$here/../../../tests/fixtures/a2m"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
for tool in block-oracle song-info; do
  g++ -O1 -o "$tmp/$tool" "$here/$tool.cpp" -I"$prefix/include" -I"$prefix/include/adplug" -L"$prefix/lib" -ladplug -llibbinio
done
{
  echo "# path	block	packer	size	fnv1a64 (AdPlug's depackers, run by block-oracle.cpp; regen.sh)"
  cd "$corpus"
  find . -type f -iname '*.a2m' | sed 's|^\./||' | LC_ALL=C sort | while IFS= read -r f; do "$tmp/block-oracle" "$f"; done
} > "$here/blocks.tsv"
{
  echo "# path	type	title	author	instruments	name...  (hex; AdPlug's player API, run by song-info.cpp; regen.sh)"
  cd "$corpus"
  find . -type f -iname '*.a2m' | sed 's|^\./||' | LC_ALL=C sort | while IFS= read -r f; do "$tmp/song-info" "$f"; done
} > "$here/song-info.tsv"
echo "wrote $(grep -vc '^#' "$here/blocks.tsv") blocks, $(grep -vc '^#' "$here/song-info.tsv") songs"
