//! The stage area's cross-crate tests (ADR 0054 §6); they are all under `tests/`, with the `face` bench under
//! `benches/`.
//!
//! The stage is where the face and the renderer meet: it links both and draws the face's layers into the renderer's
//! textures. Tests of that seam with the real wgpu renderer live here, not in `omuzaki-stage`, because no crate but an
//! area test crate may depend on two implementations (`omuzaki-face` and `omuzaki-render`) for its tests. This crate
//! depends only on the stage area (`cargo xtask lint-deps`).
