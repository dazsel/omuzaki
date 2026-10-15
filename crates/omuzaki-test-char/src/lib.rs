//! The character area's cross-crate and parity tests (ADR 0054 §6); they are all under `tests/`.
//!
//! They live here, not in the implementation crates, because an implementation may depend neither on another
//! implementation (`omuzaki-char` on `omuzaki-rig`) nor on the harness `omuzaki-test`, an App crate. This crate depends
//! only on the character area (`cargo xtask lint-deps`), so a change elsewhere doesn't put these tests in scope.
