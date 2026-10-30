#!/usr/bin/env bash
# Builds the body-tracking model folder `omuzaki_track::body::BodyModelDir` opens: downloads MediaPipe's public
# `hand_landmarker.task` and `pose_landmarker_{lite,full}.task`, verifies their SHA-256, converts the TFLite graphs to
# ONNX in a throwaway venv and writes `manifest.json` next to them. The converter's own TFLite-vs-ONNX comparison is
# the gate: it exits non-zero when a graph deviates. The venv is deleted at the end; nothing is written into the repo.
#
#   tools/models/mediapipe-body.sh --out <dir> [--venv <dir>]
#
# Downloads: the three `.task` bundles below and the pinned pip packages, nothing else. Licences:
# docs/licensing/deps/mediapipe-body-landmarkers.toml. Needs python3.12; set PYTHON to use another.
set -euo pipefail

BASE="https://storage.googleapis.com/mediapipe-models"
BUNDLES=(
  "hand_landmarker.task|$BASE/hand_landmarker/hand_landmarker/float16/1/hand_landmarker.task|fbc2a30080c3c557093b5ddfc334698132eb341044ccee322ccf8bcf3607cde1|7819105"
  "pose_landmarker_lite.task|$BASE/pose_landmarker/pose_landmarker_lite/float16/1/pose_landmarker_lite.task|59929e1d1ee95287735ddd833b19cf4ac46d29bc7afddbbf6753c459690d574a|5777746"
  "pose_landmarker_full.task|$BASE/pose_landmarker/pose_landmarker_full/float16/1/pose_landmarker_full.task|5134a3aad27a58b93da0088d431f366da362b44e3ccfbe3462b3827a839011b1|9398198"
)
# TensorFlow 2.16 segfaults reading the pose detector's sparse weights; 2.18 reads them.
PIP_PINS=("tensorflow-cpu==2.18.1" "tf2onnx==1.16.1" "onnx==1.17.0" "onnxruntime==1.26.0" "numpy==1.26.4" "protobuf<6")

out="" venv=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="${2:?--out needs a folder}"; shift 2 ;;
    --venv) venv="${2:?--venv needs a folder}"; shift 2 ;;
    *) echo "usage: $0 --out <dir> [--venv <dir>]" >&2; exit 2 ;;
  esac
done
[ -n "$out" ] || { echo "usage: $0 --out <dir> [--venv <dir>]" >&2; exit 2; }
venv="${venv:-$out/.venv}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd -P)"
out_abs="$(realpath -m "$out")"
venv_abs="$(realpath -m "$venv")"
for path in "$out_abs" "$venv_abs"; do
  case "$path/" in "$repo"/*) echo "$path is inside the repo; the model folder never goes in git" >&2; exit 2 ;; esac
done
case "$out_abs/" in "$venv_abs"/*) echo "--venv must not contain --out" >&2; exit 2 ;; esac
[ ! -e "$venv" ] || { echo "--venv $venv already exists; give a folder this script can create and delete" >&2; exit 2; }
mkdir -p "$out/bundles"

for entry in "${BUNDLES[@]}"; do
  IFS='|' read -r name url sha size <<<"$entry"
  file="$out/bundles/$name"
  if [ ! -f "$file" ] || [ "$(sha256sum "$file" | cut -d' ' -f1)" != "$sha" ]; then
    curl --fail --silent --show-error --location --output "$file" "$url"
  fi
  [ "$(sha256sum "$file" | cut -d' ' -f1)" = "$sha" ] && [ "$(stat -c %s "$file")" = "$size" ] \
    || { echo "$name does not match the pinned SHA-256 / size" >&2; rm -f "$file"; exit 1; }
done

trap 'rm -rf "$venv"' EXIT
"${PYTHON:-python3.12}" -m venv "$venv"
"$venv/bin/python" -m pip install --quiet --no-cache-dir "${PIP_PINS[@]}"
"$venv/bin/python" -I "$here/convert_mediapipe_body.py" --bundles "$out/bundles" --out "$out"
echo "model folder ready: $out"
