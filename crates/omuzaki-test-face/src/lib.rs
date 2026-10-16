//! The face area's cross-crate and parity tests (ADR 0054 §6), and the harness they share:
//!
//! - [`face_parity`]: the Rust face painter against the JS prototype's isolated face sections, one enforcement call per
//!   section (each section's test is `tests/face_parity_<section>.rs`)
//! - `tests/raster_probes.rs`: the CPU rasteriser against Chrome's canvas, on probes committed under `fixtures/raster`
//!
//! It depends only on the face area and the harness `omuzaki-test` (`cargo xtask lint-deps`), so a change elsewhere
//! doesn't put these tests in scope.
#![deny(missing_docs)]

pub mod face_parity;
pub mod probes;

// The face parity harness paints with `omuzaki-face`; crates that link this one reach its registrations through here
// (ADR 0015). `build.rs` writes the calls from the dependencies that set `registers = true` (ADR 0054 section 6).
omuzaki_core::define_link! {
    include!(concat!(env!("OUT_DIR"), "/link_calls.rs"));
}
