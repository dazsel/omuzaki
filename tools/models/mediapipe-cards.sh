#!/usr/bin/env bash
# Archives the licence record of the face-tracking models: downloads Google's three MediaPipe model cards (BlazeFace
# short range, Face Mesh V2, Blendshape V2), the only place Google states the networks' Apache-2.0 licence (the
# `.task` bundle carries no licence file), checks each against the SHA-256 and size pinned below and writes
# `SHA256SUMS` beside them. Nothing is written into the repo.
#
#   tools/models/mediapipe-cards.sh --out <dir>      (the archive is $OMUZAKI_ASSETS/licences/mediapipe)
#
# The pins are the cards read for docs/research/2026-10-face-model-licences.md (section 3); each line also records the
# object generation the bucket served. A card that no longer matches means Google changed it: the script fails and
# the licence record has to be read again before anything ships.
set -euo pipefail

BASE="https://storage.googleapis.com/mediapipe-assets"
# file name | SHA-256 | bytes | object generation
CARDS=(
  "MediaPipe BlazeFace Model Card (Short Range).pdf|cd335c06fc0de7807cd815a0777a697932598bcdb28fa98adaaabf847485f758|434425|1683223301065412"
  "Model Card MediaPipe Face Mesh V2.pdf|c6add060f4ebfb37b2690136b6c711c7e5fcb7038baa2649ae3338b83979565a|3476420|1683910257859728"
  "Model Card Blendshape V2.pdf|c8e9cf60a39998f4b341740623917590e050d1c97004e2de4568d84e026445ae|2324099|1683910254913832"
)

out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="${2:?--out needs a folder}"; shift 2 ;;
    *) echo "usage: $0 --out <dir>" >&2; exit 2 ;;
  esac
done
[ -n "$out" ] || { echo "usage: $0 --out <dir>" >&2; exit 2; }
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd -P)"
out_abs="$(realpath -m "$out")"
case "$out_abs/" in "$repo"/*) echo "$out_abs is inside the repo; the cards are kept with the assets, never in git" >&2; exit 2 ;; esac
mkdir -p "$out"

sums="$out/SHA256SUMS"
: > "$sums.tmp"
for row in "${CARDS[@]}"; do
  IFS='|' read -r name sha size generation <<< "$row"
  file="$out/$name"
  if [ ! -f "$file" ] || [ "$(sha256sum "$file" | cut -d' ' -f1)" != "$sha" ]; then
    url="$BASE/$(python3 -I -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1]))' "$name")"
    curl --fail --silent --show-error --location --output "$file" "$url?generation=$generation"
  fi
  if [ "$(sha256sum "$file" | cut -d' ' -f1)" != "$sha" ] || [ "$(stat -c %s "$file")" != "$size" ]; then
    echo "$name does not match its pinned SHA-256 / size: the card changed, read the licence record again" >&2
    rm -f "$file" "$sums.tmp"
    exit 1
  fi
  printf '%s  %s\n' "$sha" "$name" >> "$sums.tmp"
done
mv "$sums.tmp" "$sums"
echo "model cards archived and verified: $out"
