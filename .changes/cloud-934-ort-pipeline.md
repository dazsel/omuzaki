# omuzaki-ai-ort: multi-graph models

- The `onnx` runtime runs a manifest's `[[stages]]` as one session (#934): one ONNX Runtime session per stage, stage inputs read from slot inputs or earlier stages' outputs, intermediate tensors held in buffers reused by every run.
- `probe` reports the slot inputs the stages read and every stage output as `<stage>.<port>`; wiring errors name the stage, input and source. The manifest's `io` maps slot outputs to those names.
