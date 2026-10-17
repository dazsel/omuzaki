# omuzaki-ai-ort: no per-run allocations at any port count, and float16 tensors

- `Session::run` of the `onnx` runtime now allocates nothing on the Rust side after warm-up for any number of inputs and outputs and any port name length; before, a model with more than 6 inputs or 4 outputs cost 7 allocations per run and an 800-byte port name cost 1 (#840).
- Float16 tensors load and run: they travel as raw bits (`TensorData::F16`), and a round trip is bit for bit.
- Unchanged: ONNX Runtime's `IoBinding` was measured and not used (2 allocations per run with one output).
