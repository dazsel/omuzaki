//! Shared foundations for every Omuzaki crate (spec v3 §9).
//!
//! `omuzaki-core` is an API-level crate (§39 rule 1): everything here is a stable contract that implementation crates
//! build on, so it stays small and changes rarely. It holds only what *every* subsystem needs:
//! - [`assets`]: where heavy assets live (`$OMUZAKI_ASSETS`, default `assets-local`) and the skip-when-absent check
//! - [`id`]: validated, human-readable ids used in project files and paths
//! - [`colour`]: the sRGB colour type palettes and materials use, and its exact conversion to linear light
//! - [`mesh`]: the CPU-side triangle mesh that parts, importers and the renderer exchange
//! - [`rng`]: a seedable RNG, because replays and golden renders must be bit-identical (§8)
//! - [`time`]: a [`time::Clock`] trait with a fake for tests (§39 rule 6)
//! - [`text`]: the edit distance and nearest-name lookup behind every "did you mean" suggestion
//! - [`link`]: the record every self-registration leaves, so a binary can be checked for registrations the linker
//!   dropped (ADR 0015)
//! - [`glam`]: the one math library, re-exported so the whole workspace shares a single version
#![deny(missing_docs)]

pub mod assets;
pub mod colour;
pub mod fsx;
pub mod id;
pub mod link;
pub mod mesh;
pub mod pack;
pub mod rng;
pub mod text;
pub mod time;

pub use glam;

// Lets `slug_id!` refer to `::omuzaki_core` from inside this crate's own tests, as it does everywhere else.
#[cfg(test)]
extern crate self as omuzaki_core;

// Re-exports used by `slug_id!` so callers don't need serde or schemars in their own manifests.
#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use schemars as __schemars;
#[doc(hidden)]
pub use serde as __serde;
