//! The part of `xtask` other crates' tests reuse: the registration scan behind the link gate (ADR 0015).
//!
//! Each App runs the gate in its `tests/registrations.rs`, so the scan is a library as well as the `cargo xtask
//! registry` and `cargo xtask lint-links` commands. It reads manifests and sources from disk and never runs cargo, so
//! it works inside test binaries on every platform.
#![deny(missing_docs)]

pub mod registry;
