//! Contract for model import and export (spec v3 §9 `omuzaki-io`, §27 EX-01/EX-02, §19 FE-09, §7 provenance).
//!
//! `omuzaki-io-api` defines how Omuzaki reads and writes glTF and VRM without saying how any format is parsed. Format
//! crates (`omuzaki-io-gltf`, `omuzaki-io-vrm`, later FBX and `PNGTuber`) implement the traits against this crate only, and
//! the op layer wires them to `io.import` and `io.export`. The pieces:
//!
//! - **[`IoScene`]**, the intermediate scene: one canonical, glTF-shaped model every importer produces and every
//!   exporter consumes, built on the shared [`MeshData`] (see the [`scene`] docs for what is and isn't shared with
//!   the render and character models).
//! - **[`Importer`] and [`Exporter`]**, pure bytes-to-scene and scene-to-bytes traits, **self-registered by format**
//!   with [`register_importer!`] and [`register_exporter!`] (§39 rule 2). Callers use [`run_import`] and
//!   [`run_export`], which add the limits, invariants, rights rules and strict mode no format should repeat.
//! - **[`VrmMeta`] and the mapping to [`Provenance`](omuzaki_store_api::Provenance)**: a VRM's licence terms in 1.0
//!   vocabulary, and the conservative derivation of the §7 record the licence gate reads (see [`meta`]). Every importer
//!   also states in [`LicenceEvidence`] whether it read the file's licence data, left it behind or found none, and
//!   [`preflight`] refuses a published export of an imported model whose own terms forbid sharing it.
//! - **Perfect-sync baking**: the 52 `ARKit` names, the [`PerfectSync`] option, the [`ExpressionBaker`] seam the face
//!   side implements, and completeness checks (see [`perfect_sync`]).
//! - **[`IoAnimation`] and [`AnimationExporter`]**, a humanoid motion and the second, small registry that writes it
//!   (VRM Animation, EX-16), run through [`run_export_animation`] (D11).
//! - **[`Report`]**: findings with stable rule ids, locations and hints, for import warnings, export preflight and
//!   lossy-conversion notes.
//!
//! The reasoning, the VRM 0.x/1.0 mapping tables and the interfaces with the render, character, face and animation
//! contracts are in `docs/design/io-api.md`.
#![deny(missing_docs)]

pub mod animation;
mod animation_format;
mod check;
mod error;
mod format;
mod io;
mod licence;
pub mod meta;
pub mod perfect_sync;
mod report;
pub mod scene;

pub use animation::{AnimationSummary, Interpolation, IoAnimation, Keys, RestBone, RestHumanoid, Track, TrackTarget};
pub use animation_format::{
    AnimationExporter, AnimationExporterRegistration, animation_exporters, animation_formats, find_animation_exporter,
    preflight_animation, run_export_animation,
};
pub use check::{licence_forbids_derivatives, preflight, vrm_profile};
pub use error::IoError;
pub use format::{
    Confidence, Detection, ExporterRegistration, FormatId, FormatInfo, FormatSupport, ImporterRegistration, exporters,
    find_exporter, find_importer, formats, importers, sniff,
};
pub use io::{
    ExportCtx, ExportOptions, ExportOutcome, Exporter, ImportCtx, ImportInput, ImportOptions, ImportOutcome, Importer,
    Layout, Limits, NoProgress, NoResolver, OutputFile, Progress, Purpose, Resolver, run_export, run_import,
};
pub use licence::LicenceEvidence;
pub use meta::{
    AvatarPermission, CommercialUsage, CreditNotation, Modification, UseTerms, VRM_LICENSE_REF, VRM_LICENSE_URLS,
    Vrm0Licence, Vrm0Meta, VrmMeta, VrmVersion,
};
pub use perfect_sync::{
    ARKIT_52, BakeBudget, BakeReport, BakeRequest, ChannelBake, EXTRA_PREFIX, ExpressionBaker, PerfectSync,
    Realisation, extra_expression,
};
pub use report::{Finding, Report, Severity};
pub use scene::*;

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
