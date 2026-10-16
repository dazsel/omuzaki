//! The op layer for model import and export (spec v3 §6, §27 EX-01/EX-02).
//!
//! `omuzaki-io-api` defines how glTF and VRM files are read and written without knowing anything about Omuzaki's runtime;
//! the format crates (`omuzaki-io-gltf`, `omuzaki-io-vrm`) parse and write bytes. This crate is the one place that knows
//! both sides (`docs/design/io-api.md`, section 2): it exposes the `io.*` ops, and converts between the file-side
//! types (`IoScene`, bone and expression names, linear colours) and the runtime ones (the character, face and
//! animation contracts). Everything an op needs from another implementation crate arrives through an API trait, so
//! this crate depends on API crates only.
//!
//! # Module map
//!
//! | Module | Holds |
//! |---|---|
//! | [`ops::list_formats`], [`ops::inspect`] | the `io.list_formats` and `io.inspect` queries |
//! | [`ops::import`] | `io.import`, the store-writing job |
//! | [`ops::export`] | `io.export`, the gather, bake, write job |
//! | [`ops::check_export`] | `io.check_export`, the export preflight query |
//! | [`ops::licence`] | `licence.set`, `licence.describe`, `licence.derive`: the character's licence record; `licence.set_source`: a part's source row |
//! | [`convert::bones`], [`convert::channels`], [`convert::colour`] | the bone, channel and colour tables |
//! | [`convert::material`], [`convert::texture`] | toon and `MToon` materials, PNG textures |
//! | [`gather::scene`], [`gather::skeleton`] | a built character into an `IoScene` |
//! | [`gather::springs`], [`gather::avatar`] | the VRM avatar block and spring bones |
//! | [`prepare::humanoid`] | `auto_map`, `apply`: propose the humanoid bones of a skinned skeleton, with an override table |
//! | [`fit_bones`] | `reduce_bones`: cut a skin's joints to a budget, the `fit.bones` stage |
//! | [`fit_texture`] | `resize`, `downscale_to`: the texture resampler for a platform fit, in linear light with premultiplied alpha |
//! | [`msg`], [`report_sections`] | the crate's `messages!` table (the import report's lines and fixes first) and the report's four sections by rule id |
//! | [`ops::progress::JobProgress`] | the adapter from a job's context to the io progress trait |
//!
//! Ops register themselves with `omuzaki_ops_api::register_op!` next to their definition, so nothing here lists them.
//! A binary keeps them only if it links this crate, so an app that wants the ops calls [`link`] at startup (ADR 0015).
#![deny(missing_docs)]

// How a failed import or export reaches a caller. Private: the ops use it, the public API does not show it.
pub mod convert;
mod error;
pub mod fit_bones;
pub mod fit_texture;
pub mod gather;
pub mod msg;
pub mod ops;
pub mod prepare;
pub mod report_sections;
pub mod vrchat_rank;

omuzaki_core::define_link! {
    omuzaki_char_api::link();
    omuzaki_library_api::link();
}
