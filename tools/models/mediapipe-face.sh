#!/usr/bin/env bash
# Builds the face-tracking model folder `omuzaki_track::ModelDir` opens: downloads MediaPipe's one public
# `face_landmarker.task`, verifies its SHA-256, converts the three TFLite graphs to ONNX in a throwaway venv and writes
# `manifest.json` next to them. The converter's own TFLite-vs-ONNX comparison is the gate: it exits non-zero when a
# graph deviates. The venv is deleted at the end; nothing is written into the repo.
#
#   tools/models/mediapipe-face.sh --out <dir> [--venv <dir>]
#
# Downloads: the `.task` below and the pinned pip packages, nothing else. Pins and licences: spikes/webcam-track/models/PROVENANCE.md.
# Needs python3.12 (TensorFlow 2.16 has no wheel for newer interpreters); set PYTHON to use another.
set -euo pipefail

URL="https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task"
SHA256="64184e229b263107bc2b804c6625db1341ff2bb731874b0bcc2fe6544e0bc9ff"
SIZE=3758596
PIP_PINS=("tensorflow-cpu==2.16.2" "tf2onnx==1.16.1" "onnx==1.16.2" "onnxruntime==1.26.0" "numpy<2" "protobuf<5")

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
# The venv is deleted at the end, so it must be a folder this run creates: never one that already exists, never the
# output folder or one of its parents, and nothing in the repo may be written or deleted.
for path in "$out_abs" "$venv_abs"; do
  case "$path/" in "$repo"/*) echo "$path is inside the repo; the model folder never goes in git" >&2; exit 2 ;; esac
done
case "$out_abs/" in "$venv_abs"/*) echo "--venv must not contain --out" >&2; exit 2 ;; esac
[ ! -e "$venv" ] || { echo "--venv $venv already exists; give a folder this script can create and delete" >&2; exit 2; }
mkdir -p "$out"

task="$out/face_landmarker.task"
if [ ! -f "$task" ] || [ "$(sha256sum "$task" | cut -d' ' -f1)" != "$SHA256" ]; then
  curl --fail --silent --show-error --location --output "$task" "$URL"
fi
[ "$(sha256sum "$task" | cut -d' ' -f1)" = "$SHA256" ] && [ "$(stat -c %s "$task")" = "$SIZE" ] \
  || { echo "face_landmarker.task does not match the pinned SHA-256 / size" >&2; rm -f "$task"; exit 1; }

trap 'rm -rf "$venv"' EXIT
"${PYTHON:-python3.12}" -m venv "$venv"
"$venv/bin/python" -m pip install --quiet --no-cache-dir "${PIP_PINS[@]}"
"$venv/bin/python" -I "$here/convert_mediapipe_face.py" --task "$task" --out "$out"
echo "model folder ready: $out"
