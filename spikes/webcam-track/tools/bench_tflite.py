#!/usr/bin/env python3
"""Raw inference latency of the three TFLite models under TensorFlow Lite's XNNPACK CPU delegate (what MediaPipe's own
CPU path uses): the reference for `model_bench` (ONNX Runtime). Needs TensorFlow (the conversion venv has it).

    python -I tools/bench_tflite.py --models $OMUZAKI_ASSETS/models/mediapipe-face [--threads 1] [--iters 300]
"""
import argparse
import os
import statistics
import time

os.environ.setdefault("TF_CPP_MIN_LOG_LEVEL", "3")
import numpy as np  # noqa: E402
import tensorflow as tf  # noqa: E402

MODELS = ["face_detector", "face_landmarks_detector", "face_blendshapes"]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--models", required=True)
    ap.add_argument("--threads", type=int, default=1)
    ap.add_argument("--iters", type=int, default=300)
    args = ap.parse_args()
    rng = np.random.default_rng(254)
    for name in MODELS:
        interp = tf.lite.Interpreter(model_path=os.path.join(args.models, f"{name}.tflite"), num_threads=args.threads)
        interp.allocate_tensors()
        detail = interp.get_input_details()[0]
        x = rng.random(detail["shape"]).astype(np.float32)
        for _ in range(20):
            interp.set_tensor(detail["index"], x)
            interp.invoke()
        ms = []
        for _ in range(args.iters):
            t = time.perf_counter()
            interp.set_tensor(detail["index"], x)
            interp.invoke()
            ms.append((time.perf_counter() - t) * 1000)
        ms.sort()
        print(f"{name + '.tflite':<32} mean {statistics.mean(ms):>7.3f} ms  p50 {ms[len(ms) // 2]:>7.3f}  "
              f"p95 {ms[int(0.95 * len(ms)) - 1]:>7.3f}  max {ms[-1]:>7.3f}")


if __name__ == "__main__":
    main()
