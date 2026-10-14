#!/usr/bin/env bash
# Raw per-model latency, ONNX Runtime (this crate's `model_bench`) against TFLite/XNNPACK (`bench_tflite.py`), interleaved
# over several rounds and thread counts. Prints one block per round; README.md quotes the medians.
#
#   ORT_CPU_LIB=/path/libonnxruntime.so.1.31.0 TFLITE_PY=/path/to/venv/python tools/bench_models.sh [ROUNDS] > out.txt
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
: "${ORT_CPU_LIB:?set ORT_CPU_LIB}"; : "${TFLITE_PY:?set TFLITE_PY to a python with TensorFlow}"
MODELS=${MODELS:-${OMUZAKI_ASSETS:-$REPO/assets-local}/models/mediapipe-face}
BENCH=${BENCH:-$REPO/target/release/model_bench}
echo "machine load average (1 min) at start: $(cut -d' ' -f1 /proc/loadavg)"
for round in $(seq 1 "${1:-3}"); do
  for threads in 1 2 4; do
    echo "-- round $round, $threads thread(s)"
    echo "ORT, workers spin:";    "$BENCH" --ort-lib "$ORT_CPU_LIB" --models "$MODELS" --threads "$threads" --iters 200
    echo "ORT, workers sleep:";   "$BENCH" --ort-lib "$ORT_CPU_LIB" --models "$MODELS" --threads "$threads" --iters 200 --no-spin
    echo "TFLite XNNPACK:";       TF_CPP_MIN_LOG_LEVEL=3 "$TFLITE_PY" -I "$HERE/bench_tflite.py" --models "$MODELS" --threads "$threads" --iters 200 2>&1 | grep mean
  done
done
