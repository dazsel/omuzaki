//! VRM 1.0 and 0.x import and export for Omuzaki (spec v3 §27 EX-01), implementing the `omuzaki-io-api` contract.
//!
//! A `.vrm` is a binary glTF with the VRM extension family on top. Everything that is glTF (the container, accessors,
//! meshes, skins, textures, plain materials, limits, deterministic writing, the `gltf.*` findings) is
//! `omuzaki-gltf`'s (ADR 0011); this crate plugs into it through hooks and adds the VRM layer:
//!
//! | Format id | Reads and writes | Spec files implemented |
//! |---|---|---|
//! | `vrm1` | `VRMC_vrm` 1.0: meta, humanoid, expressions (morph, colour and texture-transform binds), look-at, first-person; `VRMC_springBone`; `VRMC_node_constraint`; `VRMC_materials_mtoon` | `vrm-specification/specification/VRMC_vrm-1.0/*.md`, `VRMC_springBone-1.0`, `VRMC_node_constraint-1.0`, `VRMC_materials_mtoon-1.0` |
//! | `vrm0` | the 0.x `VRM` extension: meta, humanoid, blend shape groups, first-person and look-at, secondary animation (spring bones), `materialProperties` (`MToon` 0.x, unlit) | `vrm-specification/specification/0.0/README.md` |
//!
//! Both register themselves with `omuzaki_io_api::register_importer!` / `register_exporter!`; nothing else needs to
//! name this crate. Registration happens at link time, and a linker drops a crate nothing refers to, so a binary must
//! use at least one item of it (for example [`Vrm1Importer`]).
//!
//! **Orientation.** The scene is always in VRM 1.0 space (front facing +Z). A 0.x model faces −Z; the 0.x importer
//! rotates it 180° about Y and the 0.x exporter rotates it back (see [`orientation`]).
//!
//! **Rights.** Importers fill `scene.meta` with the file's terms and, for 0.x, `scene.provenance` straight from the
//! 0.x licence name (the 1.0 meta has already lost it); `omuzaki_io_api::run_import` derives the record for 1.0. Both
//! importers report `LicenceEvidence::Imported`. A creator's terms are never lost or widened on the way out, and the
//! rules for that are `omuzaki-io-api`'s, so every exporter shares them (see `Vrm0Meta::from_meta`, `VrmMeta::extras` and
//! `preflight`): a 0.x `licenseName` keeps its identity on a 0.x export, both 0.x URLs and unknown meta keys are
//! carried or reported, and a published export of an *imported* model is refused when its licence does not allow
//! redistribution of possibly modified copies (spec VF-08). This crate only places the result in the VRM extensions.
//!
//! **Messy files import with findings, not failures.** Optional avatar data that would break the scene's invariants (a
//! morph bind to a deleted target, a spring chain out of order, a collider on a missing node) is dropped or clamped and
//! reported; mandatory structure (mesh data, humanoid nodes) is left for `IoScene::check` to reject. No input makes an
//! importer panic: sizes are checked against the import limits before anything is allocated.
//!
//! **What 0.x conversion does** (following `UniVRM`'s migrator): thumb bones are renamed; blend shape weights go from
//! 0..100 to 0..1; `Joy`, `Sorrow` and `Fun` become `happy`, `sad` and `relaxed`; mesh references become the node that
//! holds the mesh; spring bone groups become chains (a node continues into its first child, other children start
//! chains of their own) with the implicit 7 cm tail made an explicit `<leaf>_end` node; `MToon` 0.x properties become
//! the 1.0 `MToon` fields (colours decoded from sRGB, the shading range re-expressed as centre and margin, outline
//! widths rescaled). Going back loses what 0.x cannot hold (node constraints, per-joint spring parameters, capsule
//! colliders, `corporation` use, a shading shift texture), each with a finding.
//!
//! **Findings** this crate adds, by area (ids are part of the contract): `gltf.layout_unsupported` (a VRM is one
//! binary file; the other `gltf.*` rules are `omuzaki-gltf`'s, listed in its documentation); `vrm.*` for the avatar layer (`humanoid_unknown_bone`, `expression_name_invalid`,
//! `bind_dropped`, `bind_weight_clamped`, `collider_shapeless`, `collider_dropped`, `limit_dropped`, `spring_chain_dropped`,
//! `spring_center_dropped`, `constraint_invalid`, `constraint_dropped`, `constraint_duplicate`, `annotation_dropped`,
//! `mtoon_invalid`, `mtoon_queue_clamped`, `meta_licence_condition`, `perfect_sync_complete`,
//! `perfect_sync_partial`, `tpose`); `vrm0.*` for 0.x (`meta_missing`, `meta_licence`, `meta_commercial`, `meta_content_flags`,
//! `meta_third_party_licenses`, `expression_renamed`, `bind_dropped`, `material_value_dropped`, `cull_front`,
//! `constraints_dropped`, `collider_capsule`, `collider_unsupported`, `limit_unsupported`, `spring_approximated`, `preset_surprised`, `annotation_dropped`,
//! `mtoon_shift_texture`, `texture_transform_shared`). The contract's own rules (`scene.*`, `avatar.*`, `vrm.*`
//! profile and `vrm.meta_unknown_keys`, `rights.*`, `perfect_sync.*`) come from `run_import` and `run_export`.
//!
//! **Tests.** The glTF reading and writing is tested in `omuzaki-gltf`; here the VRM layer is, through generated and hand-made
//! files. Besides the unit and integration tests, `OMUZAKI_FUZZ_FULL=1 cargo test -p omuzaki-io-vrm --test malformed`
//! tries every byte flip, truncation and JSON-value mutation (minutes), and `OMUZAKI_SMOKE_VRM=<file> cargo test -p
//! omuzaki-io-vrm --test private_smoke -- --nocapture` round-trips a model that must never be committed.
#![deny(missing_docs)]

mod avatar;
mod formats;
pub mod orientation;
mod pose;
mod vrm0;
mod vrm1;

pub use formats::{Vrm0Exporter, Vrm0Importer, Vrm1Exporter, Vrm1Importer};

/// A file index as the scene's `u32`. An index past `u32::MAX` can't be real; saturating makes it fail the scene's
/// range check instead of wrapping onto a valid one.
fn ix32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

omuzaki_core::define_link!();
