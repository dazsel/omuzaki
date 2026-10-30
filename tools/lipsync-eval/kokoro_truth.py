#!/usr/bin/env python3
"""Ground truth for the lip-sync evaluation (#2351): Kokoro-82M speaks each line, and its own duration predictor says
how long each phoneme token lasts, so every phone's span in the audio is known exactly.

Kokoro's ONNX graph outputs only the waveform; the per-token durations are an inner tensor (`/encoder/Gather_output_0`,
whole frames after rounding and the clip to at least one). This script adds that tensor as a second output in memory,
runs the graph with onnxruntime exactly as the `kokoro` runtime does (ids padded with 0 at both ends, the voice's style
row for the line's length, speed 1), and writes for each line:

- `NN.f32`: the waveform, 24 kHz mono, little-endian float32;
- `NN.json`: the phonemes, and each token's character with its start and end in seconds.

Usage (the model files are the pinned ones of `crates/omuzaki-speech/models/kokoro-82m.model.toml`; dev only, nothing
here is shipped or committed):

    cargo run -q -p omuzaki-speech --example phonemize -- <dir>/us_gold.json < sentences.txt > <out>/phonemes.txt
    python3 kokoro_truth.py --model <dir> --voice af_heart --phonemes <out>/phonemes.txt --out <out>
"""

import argparse
import json
import sys
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort

RATE = 24_000
STYLE = 256
ROWS = 510
DURATIONS = "/encoder/Gather_output_0"


def session(model: Path) -> ort.InferenceSession:
    graph = onnx.load(str(model))
    if not any(o.name == DURATIONS for o in graph.graph.output):
        graph.graph.output.append(onnx.helper.make_tensor_value_info(DURATIONS, onnx.TensorProto.INT64, None))
    return ort.InferenceSession(graph.SerializeToString(), providers=["CPUExecutionProvider"])


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--model", type=Path, required=True, help="folder with model.onnx, tokenizer.json and voices")
    p.add_argument("--voice", default="af_heart")
    p.add_argument("--phonemes", type=Path, required=True, help="one line of phonemes per utterance")
    p.add_argument("--out", type=Path, required=True)
    args = p.parse_args()

    vocab = json.loads((args.model / "tokenizer.json").read_text())["model"]["vocab"]
    voice = np.fromfile(args.model / f"{args.voice}.bin", dtype="<f4").reshape(ROWS, STYLE)
    run = session(args.model / "model.onnx")
    args.out.mkdir(parents=True, exist_ok=True)
    lines = [l.rstrip("\n") for l in args.phonemes.read_text().splitlines() if l.strip()]
    per_frame = None
    for n, phonemes in enumerate(lines):
        chars = [c for c in phonemes if c in vocab]
        if len(chars) + 2 > ROWS:
            print(f"line {n} is too long for one piece", file=sys.stderr)
            return 1
        ids = np.array([[0] + [vocab[c] for c in chars] + [0]], dtype=np.int64)
        style = voice[min(len(chars), ROWS - 1)][None, :]
        wave, durations = run.run(None, {"input_ids": ids, "style": style, "speed": np.array([1.0], np.float32)})
        wave = wave.reshape(-1).astype("<f4")
        durations = durations.reshape(-1).astype(np.int64)
        assert len(durations) == ids.shape[1], (len(durations), ids.shape)
        frames = int(durations.sum())
        if per_frame is None:
            per_frame = len(wave) / frames
        assert abs(len(wave) / frames - per_frame) < 1e-6, "every frame is the same number of samples"
        edges = np.concatenate([[0], np.cumsum(durations)]) * per_frame / RATE
        tokens = [[c, float(edges[i + 1]), float(edges[i + 2])] for i, c in enumerate(chars)]
        wave.tofile(args.out / f"{n:02}.f32")
        record = {"phonemes": "".join(chars), "rate": RATE, "voice": args.voice, "tokens": tokens}
        (args.out / f"{n:02}.json").write_text(json.dumps(record, ensure_ascii=False, indent=1))
    print(f"{len(lines)} lines, {per_frame:g} samples a duration frame")
    return 0


if __name__ == "__main__":
    sys.exit(main())
