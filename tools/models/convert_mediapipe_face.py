#!/usr/bin/env python3
"""Turn MediaPipe's `face_landmarker.task` bundle into the ONNX files the Rust spike runs.

The bundle is a zip of three TFLite models plus a protobuf with the canonical face mesh. This script
  1. hashes the bundle and everything inside it,
  2. converts each TFLite model to ONNX with tf2onnx (Apache-2.0, dev-time tool only),
  3. pins the dynamic batch dimension to 1 (a fixed shape lets ONNX Runtime plan memory once),
  4. rewrites every PReLU as relu(x) + slope * (x - relu(x)) (same function, about a third faster on ONNX Runtime's CPU
     provider; see `decompose_prelu`),
  5. checks every ONNX output against the TFLite interpreter on random inputs,
  6. writes `SHA256SUMS` and `manifest.json` next to the outputs (the provenance record).

Run it in a throwaway venv, never the repo's (it pulls in TensorFlow):

    uv venv $TMPDIR/webcam-track-venv && uv pip install --python $TMPDIR/webcam-track-venv/bin/python \
        "tensorflow-cpu==2.16.2" "tf2onnx==1.16.1" "onnx==1.16.2" "numpy<2" onnxruntime "protobuf<5"
    $TMPDIR/webcam-track-venv/bin/python -I tools/convert_models.py \
        --task face_landmarker.task --out $OMUZAKI_ASSETS/models/mediapipe-face
"""
import argparse
import hashlib
import json
import os
import sys
import zipfile

os.environ.setdefault("TF_CPP_MIN_LOG_LEVEL", "3")

import numpy as np  # noqa: E402
import onnx  # noqa: E402
import onnxruntime as ort  # noqa: E402
import tensorflow as tf  # noqa: E402
import tf2onnx  # noqa: E402
from onnx import helper  # noqa: E402
from onnx.tools import update_model_dims  # noqa: E402

# Where the official bundle comes from (the same URL the JS prototype uses; see models/PROVENANCE.md).
BUNDLE_URL = "https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task"
MODELS = ["face_detector", "face_landmarks_detector", "face_blendshapes"]
# Largest acceptable |ONNX - TFLite| per output. The models run in fp32 after conversion; differences come from
# operator fusion and accumulation order only.
TOLERANCE = 2e-3


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sample_input(name: str, shape, rng):
    """A plausible input for each model so the comparison exercises realistic activations."""
    if name == "face_blendshapes":
        return (rng.random(shape) * 256).astype(np.float32)  # landmark pixel coordinates
    x = rng.random(shape).astype(np.float32)  # image in [0, 1]
    return x * 2 - 1 if name == "face_detector" else x  # the detector expects [-1, 1]


def pin_batch(model: onnx.ModelProto) -> onnx.ModelProto:
    """Replace the symbolic batch dimension of every input and output with 1."""

    def fixed(values):
        out = {}
        for v in values:
            dims = [d.dim_value if d.dim_value > 0 else 1 for d in v.type.tensor_type.shape.dim]
            out[v.name] = dims
        return out

    return update_model_dims.update_inputs_outputs_dims(model, fixed(model.graph.input), fixed(model.graph.output))


def decompose_prelu(model: onnx.ModelProto) -> int:
    """Replaces each PRelu node by Relu, Sub, Mul and Add, in place; returns how many were replaced.

    ONNX Runtime's CPU provider runs convolutions in a blocked channel layout (NCHWc) and PRelu is not layout aware,
    so every PRelu costs a pair of layout conversions around it (about 27% of the face mesh model's run time in the
    profile). relu(x) + slope * (x - relu(x)) equals PRelu(x, slope) for every x (x for x > 0, slope * x otherwise) and
    is built from operators that keep the layout, which cut the model's latency by about a third in the benchmark.
    """
    nodes, replaced = [], 0
    for node in model.graph.node:
        if node.op_type != "PRelu":
            nodes.append(node)
            continue
        x, slope = node.input
        tag = f"{node.name or node.output[0]}_prelu"
        pos, neg, scaled = f"{tag}_pos", f"{tag}_neg", f"{tag}_scaled"
        nodes += [
            helper.make_node("Relu", [x], [pos], name=f"{tag}_relu"),
            helper.make_node("Sub", [x, pos], [neg], name=f"{tag}_sub"),
            helper.make_node("Mul", [neg, slope], [scaled], name=f"{tag}_mul"),
            helper.make_node("Add", [pos, scaled], [node.output[0]], name=f"{tag}_add"),
        ]
        replaced += 1
    del model.graph.node[:]
    model.graph.node.extend(nodes)
    return replaced


def convert(name: str, tflite_path: str, onnx_path: str, keep_prelu: bool) -> tuple[float, int, str]:
    """Converts one model; returns (worst difference vs TFLite, PReLUs rewritten, SHA-256 of the raw tf2onnx output)."""
    model, _ = tf2onnx.convert.from_tflite(tflite_path, opset=17)
    model = pin_batch(model)
    raw_sha = sha256(model.SerializeToString())
    rewritten = 0 if keep_prelu else decompose_prelu(model)
    onnx.checker.check_model(model)
    onnx.save(model, onnx_path)

    interp = tf.lite.Interpreter(model_path=tflite_path)
    interp.allocate_tensors()
    sess = ort.InferenceSession(onnx_path, providers=["CPUExecutionProvider"])
    in_detail = interp.get_input_details()[0]
    rng = np.random.default_rng(254)
    worst = 0.0
    for _ in range(5):
        x = sample_input(name, in_detail["shape"], rng)
        interp.set_tensor(in_detail["index"], x)
        interp.invoke()
        got = sess.run(None, {sess.get_inputs()[0].name: x})
        for detail, onnx_out in zip(interp.get_output_details(), got):
            ref = interp.get_tensor(detail["index"])
            worst = max(worst, float(np.abs(ref.reshape(onnx_out.shape) - onnx_out).max()))
    if worst > TOLERANCE:
        sys.exit(f"{name}: ONNX deviates from TFLite by {worst}")
    return worst, rewritten, raw_sha


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--task", required=True, help="face_landmarker.task downloaded from BUNDLE_URL")
    ap.add_argument("--out", required=True, help="output directory (outside git: $OMUZAKI_ASSETS/models/...)")
    ap.add_argument("--keep-prelu", action="store_true",
                    help="skip the PReLU rewrite (the plain tf2onnx graph), to reproduce the latency comparison in README.md")
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)

    with open(args.task, "rb") as fh:
        bundle = fh.read()
    manifest = {"source_url": BUNDLE_URL, "bundle": {"file": os.path.basename(args.task), "sha256": sha256(bundle), "bytes": len(bundle)},
                "tools": {"tensorflow": tf.__version__, "tf2onnx": tf2onnx.__version__, "onnx": onnx.__version__,
                          "onnxruntime": ort.__version__}, "files": {}}
    sums = []
    with zipfile.ZipFile(args.task) as z:
        for info in z.infolist():
            data = z.read(info.filename)
            path = os.path.join(args.out, info.filename)
            with open(path, "wb") as fh:
                fh.write(data)
            manifest["files"][info.filename] = {"sha256": sha256(data), "bytes": len(data), "from": "bundle"}
            sums.append((sha256(data), info.filename))

    for name in MODELS:
        tflite = os.path.join(args.out, f"{name}.tflite")
        onnx_path = os.path.join(args.out, f"{name}.onnx")
        worst, rewritten, raw_sha = convert(name, tflite, onnx_path, args.keep_prelu)
        with open(onnx_path, "rb") as fh:
            data = fh.read()
        manifest["files"][f"{name}.onnx"] = {"sha256": sha256(data), "bytes": len(data),
                                             "from": f"{name}.tflite via tf2onnx, batch pinned, PReLU rewritten",
                                             "prelu_rewritten": rewritten, "tf2onnx_output_sha256": raw_sha,
                                             "max_abs_diff_vs_tflite": worst}
        sums.append((sha256(data), f"{name}.onnx"))
        print(f"{name}.onnx  {len(data):>9} bytes  {rewritten} PReLU rewritten  max |onnx - tflite| = {worst:.2e}")

    with open(os.path.join(args.out, "SHA256SUMS"), "w") as fh:
        for digest, fname in sorted(sums, key=lambda t: t[1]):
            fh.write(f"{digest}  {fname}\n")
    with open(os.path.join(args.out, "manifest.json"), "w") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")
    print("wrote", os.path.join(args.out, "manifest.json"))


if __name__ == "__main__":
    main()
