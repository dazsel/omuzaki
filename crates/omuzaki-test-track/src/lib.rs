//! The track area's real-model tests and bench (ADR 0054 §6); they are all under `tests/` and `benches/`.
//!
//! They live here, not in `omuzaki-track`, because an implementation crate may not depend on the `onnx` runtime
//! (`omuzaki-ai-ort`) even as a dev-dependency, and not in the harness `omuzaki-test`, so that a change elsewhere doesn't
//! put them in scope. They need the model folder `tools/models/mediapipe-face.sh` writes (`OMUZAKI_TRACK_MODELS`) and
//! the pinned ONNX Runtime archive (`OMUZAKI_ORT_CACHE`); without either they print a line and pass.
