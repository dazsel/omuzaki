//! A tiny GLB and VRM 1.0 importer and exporter implementing `omuzaki-io-api`, for tests and the walking skeleton
//! (spec v3 §39 rule 6: a mock for every API).
//!
//! It is a real, if small, glTF 2.0 implementation, not a stub that returns canned data: it parses and writes the GLB
//! container, accessors with bounds checks, nodes, meshes with morph targets, skins, plain PBR materials, and the
//! `VRMC_vrm` 1.0 `meta`, `humanoid`, morph-bind `expressions`, `lookAt` and `firstPerson`, plus the `VRMC_springBone`
//! colliders, groups and chains. It does **not** do images and textures, `.gltf` with external files, sparse or
//! interleaved accessors, `MToon` or node constraints, nor the `VRMC_springBone_extended_collider` and
//! `VRMC_springBone_limit` extensions, which it reports as dropped. It refuses what would come out wrong and reports
//! what it drops. That is enough to exercise every part of the contract: registration and sniffing, limits,
//! invariants, preflight rules, determinism, round trips, and perfect-sync baking through [`MockBaker`].
//!
//! The rule-by-rule tests for `omuzaki-io-api` live in this crate's `tests/` because they need a full avatar fixture
//! ([`fixtures::tiny_avatar`]), and an API crate must not depend on its own mock.
#![deny(missing_docs)]

mod baker;
pub mod fixtures;
mod formats;
mod glb;
mod json;
mod read;
mod spring;
mod vrm;
mod write;

pub use baker::MockBaker;
pub use formats::{MockGlbExporter, MockGlbImporter, MockVrm1Exporter, MockVrm1Importer};

omuzaki_core::define_link!();
