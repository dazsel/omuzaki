#!/usr/bin/env bash
# Reproduces every accuracy number in README.md: the Rust tracker against the Python MediaPipe reference
# and the prototype's JS tracker, on 200 independent synthetic stills and on one coherent 90-frame clip.
#
#   ORT_CPU_LIB=/path/libonnxruntime.so.1.31.0 tools/run_parity.sh [OUT_DIR]
#
# Needs: target/release/track built, the models (models/PROVENANCE.md), the two synthface datasets, the mediapipe venv
# (default $TMPDIR/venvs/mp), node with playwright (tools/js_tracker_bench.mjs) and ffmpeg. Results go to OUT_DIR
# (default under the models folder); the comparison tables are small text.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
ASSETS=${OMUZAKI_ASSETS:-$REPO/assets-local}
OUT=${1:-$ASSETS/models/mediapipe-face/spike-results}
: "${ORT_CPU_LIB:?set ORT_CPU_LIB to libonnxruntime.so}"
MODELS=${MODELS:-$ASSETS/models/mediapipe-face}
STILLS=${STILLS:-$ASSETS/datasets/synthface/spike01}
CLIP=${CLIP:-$ASSETS/datasets/synthface/clip01}
PY=${PY:-$TMPDIR/venvs/mp/bin/python}
TRACK=${TRACK:-$REPO/target/release/track}
mkdir -p "$OUT"

# the tracker prints a long latency summary; keep it in a log and show only the first line
rust() { timeout 600 "$TRACK" --ort-lib "$ORT_CPU_LIB" --models "$MODELS" --landmarks "$@" > "$OUT/track.log"; head -1 "$OUT/track.log"; }
compare() { python3 -I "$HERE/compare.py" "$@" > /dev/null; }

echo "== stills (200 independent faces, 256x256), image mode"
"$PY" -I "$HERE/ref_mediapipe.py" "$STILLS" "$OUT/py-stills-image.json" --mode image --landmarks 2>&1 | tail -n 1
rust --input "$STILLS" --out "$OUT/rust-stills-image.json" --mode image
compare "$OUT/py-stills-image.json" "$OUT/rust-stills-image.json" --frame-size 256x256 \
    --label "Rust (ort, CPU) vs Python MediaPipe: 200 stills, image mode" --markdown "$OUT/compare-stills-image.md" --json "$OUT/compare-stills-image.json"

echo "== clip (one identity, 90 frames at 30 fps, 480x480)"
# track_face.py reads a video: encode the PNGs losslessly (FFV1, RGB) so it sees the same pixels the others do
ffmpeg -y -v error -framerate 30 -i "$CLIP/clip_%06d.png" -c:v ffv1 -pix_fmt bgr0 "$OUT/clip01.mkv"
"$PY" -I "${TRACK_FACE_PY:?set TRACK_FACE_PY to the track_face.py reference tracker}" "$OUT/clip01.mkv" "$OUT/py-clip-trackface.json" --fps 30 --conf 0.5 > "$OUT/track_face.log" 2>&1
grep "detect rate" "$OUT/track_face.log"
"$PY" -I "$HERE/ref_mediapipe.py" "$CLIP" "$OUT/py-clip-image.json" --mode image --landmarks 2>&1 | tail -n 1
node "$HERE/js_tracker_bench.mjs" "$CLIP" "$OUT/js-clip.json" --passes 3 2>&1 | tail -n 1
rust --input "$CLIP" --out "$OUT/rust-clip-video.json" --mode video
rust --input "$CLIP" --out "$OUT/rust-clip-image.json" --mode image
rm -f "$OUT/clip01.mkv"
compare "$OUT/py-clip-trackface.json" "$OUT/rust-clip-video.json" --frame-size 480x480 \
    --label "Rust vs track_face.py (Python MediaPipe): clip, video mode" --markdown "$OUT/compare-clip-video-vs-trackface.md" --json "$OUT/compare-clip-video-vs-trackface.json"
compare "$OUT/py-clip-trackface.json" "$OUT/js-clip.json" \
    --label "JS tracker (MediaPipe 0.10.14, WASM) vs track_face.py: clip, video mode (the gap between two MediaPipe builds)" --markdown "$OUT/compare-clip-js-vs-trackface.md" --json "$OUT/compare-clip-js-vs-trackface.json"
compare "$OUT/js-clip.json" "$OUT/rust-clip-video.json" \
    --label "Rust vs the prototype's JS tracker: clip, video mode" --markdown "$OUT/compare-clip-video-vs-js.md" --json "$OUT/compare-clip-video-vs-js.json"
compare "$OUT/py-clip-image.json" "$OUT/rust-clip-image.json" --frame-size 480x480 \
    --label "Rust vs Python MediaPipe: clip, image mode (detector on every frame)" --markdown "$OUT/compare-clip-image.md" --json "$OUT/compare-clip-image.json"
echo "tables written to $OUT/compare-*.md"
