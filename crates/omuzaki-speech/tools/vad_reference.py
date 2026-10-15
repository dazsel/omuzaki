"""Reference Silero VAD probabilities from the stock onnxruntime, for the Rust golden test.

Re-implements the model wrapper from snakers4/silero-vad (`OnnxWrapper`: a 64-sample context prepended to each 512
sample frame at 16 kHz, LSTM state carried across frames) so the Rust wrapper can be checked against an independent
implementation. Run it with `python3 -I`:

    python3 -I tools/vad_reference.py silero_vad.onnx clip.wav > tests/golden/vad_probabilities_<clip>.txt

Needs numpy and onnxruntime. Prints one probability per 512-sample frame, final partial frame zero-padded.
"""
import sys
import wave

import numpy as np
import onnxruntime as ort

FRAME, CONTEXT = 512, 64


def main(model_path: str, wav_path: str) -> None:
    with wave.open(wav_path, "rb") as w:
        assert w.getframerate() == 16000 and w.getnchannels() == 1 and w.getsampwidth() == 2
        audio = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float32) / 32768.0
    options = ort.SessionOptions()
    options.intra_op_num_threads = 1
    options.inter_op_num_threads = 1
    session = ort.InferenceSession(model_path, options, providers=["CPUExecutionProvider"])
    state = np.zeros((2, 1, 128), dtype=np.float32)
    context = np.zeros((1, CONTEXT), dtype=np.float32)
    sr = np.array(16000, dtype=np.int64)
    for start in range(0, len(audio), FRAME):
        frame = np.zeros((1, FRAME), dtype=np.float32)
        chunk = audio[start : start + FRAME]
        frame[0, : len(chunk)] = chunk
        x = np.concatenate([context, frame], axis=1)
        out, state = session.run(None, {"input": x, "state": state, "sr": sr})
        context = x[:, -CONTEXT:]
        print(f"{float(out[0, 0]):.7f}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
