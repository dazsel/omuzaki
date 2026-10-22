# omuzaki-test-track: the face tracker's real-model tests and bench move out of the harness, and count native allocations

- New area test crate `omuzaki-test-track` holds `tests/track_models.rs`, `tests/track_support/` and `benches/track.rs`, moved from `omuzaki-test` (#311 part A). Without `OMUZAKI_TRACK_MODELS` (or `OMUZAKI_ORT_CACHE`) each test prints a skip line and passes, as before.
- The steady-state test now also counts ONNX Runtime's native (C++) allocations over 300 calls after warm-up and asserts the measured ceiling (#1600).
- `cargo xtask lint-deps`: the track test crate may depend on `omuzaki-ai-ort`; no other area's test crate may.
