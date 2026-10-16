# omuzaki-ai-litert: the `tflite` runtime runs models

- The model scan checks every `.tflite` file against the allow-list of design §5 before LiteRT reads it, and `probe` is the scan alone (#1749).
- `load` and `run`: one LiteRT interpreter with the XNNPACK delegate per session, presenting the slot's signature, resizing an `any` input the slot fixes, checking every output before any caller buffer is written, and allocating nothing in a run after warm-up (#1750). Until LiteRT is published (#1748), `load` uses the library `load_archive` loaded and refuses without one.
- Gated tests: the session on the real library, no allocation, under 1 ms of idle CPU per second, outputs bit-identical over sessions and 1, 2 and 4 threads, within 1e-4 of the official wheel and inside the ONNX gate (#1751, `tools/models/litert_reference.py`).
- `fuzz/`: libFuzzer targets for the TFLite scan, the ONNX scan and the file-access path parser, with seed corpora from the tests (#1653).
