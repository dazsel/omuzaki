#!/usr/bin/env python3
"""Turn MediaPipe's `hand_landmarker.task` and `pose_landmarker_<variant>.task` bundles into the ONNX files the Rust
body tracker (`omuzaki_track::body`) runs.

Each bundle is a zip of two TFLite models. This script, like `convert_mediapipe_face.py`,
  1. hashes the bundles and everything inside them,
  2. densifies the pose detector's block-sparse weights, which tf2onnx cannot read (see `densify`),
  3. converts each TFLite model to ONNX with tf2onnx (Apache-2.0, dev-time tool only),
  4. pins the dynamic batch dimension to 1 and prunes the outputs the tracker never reads (`KEEP_OUTPUTS`),
  5. rewrites every PReLU as relu(x) + slope * (x - relu(x)) (see `decompose_prelu` in the face converter),
  6. checks every ONNX output against the original TFLite graph on random inputs,
  7. writes `SHA256SUMS` and `manifest.json` next to the outputs (the provenance record).

The pose bundles both hold `pose_detector.tflite` and `pose_landmarks_detector.tflite`; the outputs are named
`pose_detector.onnx` and `pose_landmarks_<variant>.onnx`, so the folder can hold more than one landmark variant.

Run it in a throwaway venv (`tools/models/mediapipe-body.sh` does), never the repo's.
"""
import argparse
import hashlib
import json
import os
import sys
import tempfile
import zipfile

os.environ.setdefault("TF_CPP_MIN_LOG_LEVEL", "3")

import numpy as np  # noqa: E402
import onnx  # noqa: E402
import onnx.utils  # noqa: E402
import onnxruntime as ort  # noqa: E402
import tensorflow as tf  # noqa: E402
import tf2onnx  # noqa: E402
from onnx import helper  # noqa: E402
from onnx.tools import update_model_dims  # noqa: E402
from tensorflow.lite.python import schema_py_generated as schema  # noqa: E402
from tensorflow.lite.tools import flatbuffer_utils  # noqa: E402

BASE = "https://storage.googleapis.com/mediapipe-models"
BUNDLES = {
    "hand_landmarker.task": f"{BASE}/hand_landmarker/hand_landmarker/float16/1/hand_landmarker.task",
    "pose_landmarker_lite.task": f"{BASE}/pose_landmarker/pose_landmarker_lite/float16/1/pose_landmarker_lite.task",
    "pose_landmarker_full.task": f"{BASE}/pose_landmarker/pose_landmarker_full/float16/1/pose_landmarker_full.task",
}
# The input range of each graph, from its model metadata (NormalizationOptions): only the pose detector reads -1..1.
SIGNED_INPUT = {"pose_detector"}
# Largest acceptable |ONNX - TFLite| per output, relative to the output's largest magnitude (at least 1).
TOLERANCE = 2e-3
# Outputs kept per graph; the rest are pruned from the ONNX graph with the nodes only they need. The pose landmark
# graphs' segmentation mask and heatmap are two thirds of their run time and the tracker reads neither.
KEEP_OUTPUTS = {"pose_landmarks_detector": ["Identity", "Identity_1", "Identity_4"]}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def pin_batch(model: onnx.ModelProto) -> onnx.ModelProto:
    def fixed(values):
        return {v.name: [d.dim_value if d.dim_value > 0 else 1 for d in v.type.tensor_type.shape.dim] for v in values}

    return update_model_dims.update_inputs_outputs_dims(model, fixed(model.graph.input), fixed(model.graph.output))


def decompose_prelu(model: onnx.ModelProto) -> int:
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


def densify(src: str, dst: str) -> int:
    """Writes `src` to `dst` with every DENSIFY op folded into a dense constant; returns how many were folded.

    The pose detector stores its 1x1 convolution weights block-sparse, expanded at run time by DENSIFY ops, and
    tf2onnx reads constants as dense buffers. TFLite's own interpreter, keeping every tensor, computes each DENSIFY
    output once; that output becomes a constant buffer and the op is dropped. The graph's outputs are unchanged bit
    for bit (the comparison in `convert` runs against `src`).
    """
    model = flatbuffer_utils.read_model(src)
    interp = tf.lite.Interpreter(model_path=src, experimental_preserve_all_tensors=True)
    interp.allocate_tensors()
    detail = interp.get_input_details()[0]
    interp.set_tensor(detail["index"], np.zeros(detail["shape"], np.float32))
    interp.invoke()
    codes = [max(code.builtinCode, code.deprecatedBuiltinCode) for code in model.operatorCodes]
    folded = 0
    for graph in model.subgraphs:
        kept = []
        for op in graph.operators:
            if codes[op.opcodeIndex] != schema.BuiltinOperator.DENSIFY:
                kept.append(op)
                continue
            sparse, dense = op.inputs[0], op.outputs[0]
            buffer = schema.BufferT()
            buffer.data = np.frombuffer(interp.get_tensor(dense).tobytes(), np.uint8)
            model.buffers.append(buffer)
            graph.tensors[dense].buffer = len(model.buffers) - 1
            graph.tensors[sparse].buffer = 0
            graph.tensors[sparse].sparsity = None
            folded += 1
        graph.operators = kept
    flatbuffer_utils.write_model(model, dst)
    return folded


def convert(graph: str, tflite_path: str, onnx_path: str) -> tuple[float, int, str]:
    dense_path = tflite_path.removesuffix(".tflite") + ".dense.tflite"
    source = dense_path if densify(tflite_path, dense_path) else tflite_path
    model, _ = tf2onnx.convert.from_tflite(source, opset=17)
    model = pin_batch(model)
    if graph in KEEP_OUTPUTS:
        inputs = [i.name for i in model.graph.input]
        model = onnx.utils.Extractor(model).extract_model(inputs, KEEP_OUTPUTS[graph])
    raw_sha = sha256(model.SerializeToString())
    rewritten = decompose_prelu(model)
    onnx.checker.check_model(model)
    onnx.save(model, onnx_path)

    interp = tf.lite.Interpreter(model_path=tflite_path)
    interp.allocate_tensors()
    sess = ort.InferenceSession(onnx_path, providers=["CPUExecutionProvider"])
    in_detail = interp.get_input_details()[0]
    rng = np.random.default_rng(254)
    worst = 0.0
    for _ in range(5):
        x = rng.random(in_detail["shape"]).astype(np.float32)
        if graph in SIGNED_INPUT:
            x = x * 2 - 1
        interp.set_tensor(in_detail["index"], x)
        interp.invoke()
        got = {o.name: v for o, v in zip(sess.get_outputs(), sess.run(None, {sess.get_inputs()[0].name: x}))}
        for detail in interp.get_output_details():
            if detail["name"] not in got:
                continue  # pruned
            ref = interp.get_tensor(detail["index"])
            onnx_out = got[detail["name"]]
            # Relative to the output's own scale: the landmark and segmentation outputs are in pixels and logits
            # (up to several hundred), where fp32 accumulation order alone moves the last digits.
            scale = max(1.0, float(np.abs(ref).max()))
            worst = max(worst, float(np.abs(ref.reshape(onnx_out.shape) - onnx_out).max()) / scale)
    if worst > TOLERANCE:
        sys.exit(f"{graph}: ONNX deviates from TFLite by {worst}")
    return worst, rewritten, raw_sha


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bundles", required=True, help="folder holding the downloaded .task bundles")
    ap.add_argument("--out", required=True, help="output directory (outside git)")
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)

    manifest = {"bundles": {}, "files": {},
                "tools": {"tensorflow": tf.__version__, "tf2onnx": tf2onnx.__version__, "onnx": onnx.__version__,
                          "onnxruntime": ort.__version__}}
    sums = []
    for bundle_name, url in BUNDLES.items():
        with open(os.path.join(args.bundles, bundle_name), "rb") as fh:
            bundle = fh.read()
        manifest["bundles"][bundle_name] = {"source_url": url, "sha256": sha256(bundle), "bytes": len(bundle)}
        variant = bundle_name.removesuffix(".task").removeprefix("pose_landmarker_")
        with zipfile.ZipFile(os.path.join(args.bundles, bundle_name)) as z, tempfile.TemporaryDirectory() as tmp:
            for info in z.infolist():
                graph = info.filename.removesuffix(".tflite")
                out_name = f"pose_landmarks_{variant}.onnx" if graph == "pose_landmarks_detector" else f"{graph}.onnx"
                if out_name in manifest["files"]:
                    continue  # the pose detector is the same graph in every pose bundle
                data = z.read(info.filename)
                tflite = os.path.join(tmp, info.filename)
                with open(tflite, "wb") as fh:
                    fh.write(data)
                onnx_path = os.path.join(args.out, out_name)
                worst, rewritten, raw_sha = convert(graph, tflite, onnx_path)
                with open(onnx_path, "rb") as fh:
                    converted = fh.read()
                manifest["files"][out_name] = {
                    "sha256": sha256(converted), "bytes": len(converted),
                    "from": f"{bundle_name}:{info.filename} ({sha256(data)}) via tf2onnx, batch pinned, unread outputs pruned, PReLU rewritten",
                    "prelu_rewritten": rewritten, "tf2onnx_output_sha256": raw_sha, "max_rel_diff_vs_tflite": worst}
                sums.append((sha256(converted), out_name))
                print(f"{out_name:32} {len(converted):>9} bytes  {rewritten} PReLU rewritten  max |onnx - tflite| / scale = {worst:.2e}")

    with open(os.path.join(args.out, "SHA256SUMS"), "w") as fh:
        for digest, fname in sorted(sums, key=lambda t: t[1]):
            fh.write(f"{digest}  {fname}\n")
    with open(os.path.join(args.out, "manifest.json"), "w") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")
    print("wrote", os.path.join(args.out, "manifest.json"))


if __name__ == "__main__":
    main()
