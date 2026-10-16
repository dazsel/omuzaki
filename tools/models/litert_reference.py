#!/usr/bin/env python3
"""Writes the reference outputs the `tflite` runtime's parity test compares against (#1751, design §7.2).

    python -I tools/models/litert_reference.py --models <face model folder> --out <scratch folder>

For each of the three face graphs from `tools/models/mediapipe-face.sh`, the inputs are:
- `seeded0`..`seeded2`: random tensors drawn as the converter's own check draws them (`convert_mediapipe_face.py`);
- for the detector and the landmark graph, `face0`..`face2`: a synthetic face drawn here from ellipses (skin, eyes,
  brows, nose, mouth) at three positions and sizes. No photograph and no real person is involved.

Each input is run through:
- the official `ai-edge-litert` wheel at the version the runtime builds (2.3.0), XNNPACK on, 1 thread: the parity
  reference (|ours - wheel| <= 1e-4);
- `onnxruntime` on the converted `.onnx` graph: today's `onnx` path, for the converter's ONNX gate.

It writes every tensor's raw little-endian bytes and an index, `reference.txt`, one tensor per line:

    <graph> <case> <role> <index> <dtype> <shape> <file>

with `role` one of `in`, `wheel`, `onnx`, `dtype` `f32`, and `shape` like `1x256x256x3`. The Rust test reads the
same input bytes, so both runtimes see identical inputs. Run it with `python -I` in a throwaway venv that holds the
hash-pinned `tools/litert/requirements-check.txt` and `onnxruntime`; the model files are checked against the folder's
`manifest.json` first. Nothing it writes goes in the repository.
"""

import argparse
import hashlib
import json
import sys
from pathlib import Path

GRAPHS = ("face_detector", "face_landmarks_detector", "face_blendshapes")
# The graphs whose input is an image, which get the synthetic faces too.
IMAGE_GRAPHS = ("face_detector", "face_landmarks_detector")
SEEDED_CASES = 3
# (centre x, centre y, half-width), as fractions of the image: centred, off to one side, small.
FACES = ((0.50, 0.52, 0.30), (0.40, 0.45, 0.24), (0.58, 0.60, 0.16))


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def seeded_input(graph: str, shape, rng):
    """In each graph's working range, as `convert_mediapipe_face.py`'s `sample_input` and `build.py`'s check draw it."""
    import numpy as np

    if graph == "face_blendshapes":
        return (rng.random(shape) * 256).astype(np.float32)
    values = rng.random(shape).astype(np.float32)
    return values * 2 - 1 if graph == "face_detector" else values


def synthetic_face(height: int, width: int, cx: float, cy: float, half: float):
    """An RGB face in [0, 1] drawn from ellipses on a soft background: skin, two eyes with brows, a nose, a mouth."""
    import numpy as np

    y, x = np.mgrid[0:height, 0:width].astype(np.float32)
    u = (x / width - cx) / half
    v = (y / height - cy) / (half * 1.3)
    image = np.empty((height, width, 3), np.float32)
    image[..., 0] = 0.35 + 0.25 * y / height
    image[..., 1] = 0.45 + 0.15 * x / width
    image[..., 2] = 0.55

    def ellipse(u0, v0, a, b):
        return ((u - u0) / a) ** 2 + ((v - v0) / b) ** 2 <= 1.0

    def paint(mask, rgb):
        image[mask] = rgb

    paint(ellipse(0.0, 0.0, 1.0, 1.0), (0.87, 0.70, 0.58))  # skin
    for side in (-1, 1):
        paint(ellipse(0.38 * side, -0.22, 0.20, 0.08), (0.97, 0.97, 0.95))  # eye white
        paint(ellipse(0.38 * side, -0.22, 0.08, 0.08), (0.20, 0.13, 0.08))  # iris
        paint(ellipse(0.38 * side, -0.40, 0.24, 0.04), (0.30, 0.20, 0.12))  # brow
    paint(ellipse(0.0, 0.08, 0.07, 0.20), (0.80, 0.60, 0.50))  # nose
    paint(ellipse(0.0, 0.45, 0.32, 0.07), (0.70, 0.30, 0.30))  # mouth
    return image


def face_input(graph: str, shape, case: int):
    """The synthetic face at the graph's input size, in its range ([-1, 1] for the detector, [0, 1] otherwise)."""
    import numpy as np

    _, height, width, _ = (int(d) for d in shape)
    image = synthetic_face(height, width, *FACES[case])[np.newaxis]
    return image * 2 - 1 if graph == "face_detector" else image


def write(out: Path, lines: list, graph: str, case: str, role: str, index: int, tensor) -> None:
    import numpy as np

    tensor = np.ascontiguousarray(tensor, dtype="<f4")
    name = f"{graph}.{case}.{role}{index}.bin"
    (out / name).write_bytes(tensor.tobytes())
    shape = "x".join(str(int(d)) for d in tensor.shape) or "-"
    lines.append(f"{graph} {case} {role} {index} f32 {shape} {name}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--models", required=True, help="the folder tools/models/mediapipe-face.sh wrote")
    parser.add_argument("--out", required=True, help="a scratch folder outside the repository")
    args = parser.parse_args()

    import numpy as np
    import onnxruntime
    from ai_edge_litert.interpreter import Interpreter

    models, out = Path(args.models), Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((models / "manifest.json").read_text(encoding="utf-8"))
    lines = []
    for graph in GRAPHS:
        for suffix in (".tflite", ".onnx"):
            path = models / f"{graph}{suffix}"
            if sha256(path) != manifest["files"][path.name]["sha256"]:
                print(f"{path.name} differs from manifest.json", file=sys.stderr)
                return 1
        wheel = Interpreter(model_path=str(models / f"{graph}.tflite"), num_threads=1)
        wheel.allocate_tensors()
        onnx = onnxruntime.InferenceSession(str(models / f"{graph}.onnx"), providers=["CPUExecutionProvider"])
        (detail,) = wheel.get_input_details()
        rng = np.random.default_rng(0)
        cases = [(f"seeded{i}", seeded_input(graph, detail["shape"], rng)) for i in range(SEEDED_CASES)]
        if graph in IMAGE_GRAPHS:
            cases += [(f"face{i}", face_input(graph, detail["shape"], i)) for i in range(len(FACES))]
        for case, tensor in cases:
            tensor = tensor.astype(np.float32)
            write(out, lines, graph, case, "in", 0, tensor)
            wheel.set_tensor(detail["index"], tensor)
            wheel.invoke()
            for index, output in enumerate(wheel.get_output_details()):
                write(out, lines, graph, case, "wheel", index, wheel.get_tensor(output["index"]))
            for index, value in enumerate(onnx.run(None, {onnx.get_inputs()[0].name: tensor})):
                write(out, lines, graph, case, "onnx", index, value)
    (out / "reference.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {len(lines)} tensors and {out / 'reference.txt'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
