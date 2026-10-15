//! Contract for the library (spec v3 §28, ADR 0019, design `docs/design/library.md`).
//!
//! An item is one TOML manifest: metadata, rights and a recipe of op requests with whole-value placeholders. It has
//! no code, no expression language and no mesh. Applying it plans the recipe's ops against the document and
//! materialises the values (the real apply is in `omuzaki-library`); this crate holds what everyone must agree on:
//!
//! - [`ItemId`], [`Version`], [`VersionReq`]: `publisher/name`, plain semver and its ranges
//! - [`ItemManifest`]: parse, [`canonical_bytes`](ItemManifest::canonical_bytes) and [`hash`](ItemManifest::hash)
//!   (BLAKE3, keys sorted, ADR 0009 floats), [`check`](ItemManifest::check), [`check_ops`]
//! - [`Section`], [`ItemKind`], [`Target`]: the 25 sections, what applying does, what it applies to (only the
//!   character target is open until the scene contract)
//! - [`ItemRights`]: the provenance fields, and the one constant every first-party item carries
//! - [`Publisher`] and [`ItemManifest::verify`]: ed25519 verification; a name is never trusted without its key
//! - [`expand`]: bindings and parameters to concrete [`StepRequest`]s, pure
//! - [`ApplicationRecord`]: the pin and what an application wrote, stored in the document (the records are the lock)
//! - [`ItemSource`] and [`register_library_source!`]: first-party items register per crate, from data
//! - [`Library`]: the service that lists, searches, describes and resolves items, with [`Query`], [`SearchPage`],
//!   [`ItemInfo`] and [`Facets`]
//! - [`migrations`]: append-only migrations of the manifest format
#![deny(missing_docs)]

mod check;
mod id;
mod manifest;
omuzaki_core::define_link! {
    omuzaki_char_api::link();
}

pub mod migrations;
mod placeholder;
mod publisher;
mod recipe;
mod record;
mod rights;
mod section;
mod service;
mod source;

pub use check::{Problem, check_ops};
pub use id::{FIRST_PARTY_PUBLISHER, FIXTURE_PUBLISHER, IdError, ItemId, Version, VersionError, VersionReq};
pub use manifest::{
    BodyKind, Compat, Creates, Framing, ItemManifest, ItemStep, Migrate, OpStep, ParamSpec, ParamType, ParseError,
    PreviewSpec, Step, Turn, When,
};
pub use publisher::{OMUZAKI_PUBLISHER_KEY, Origin, Publisher, PublisherCheck, PublisherKey};
pub use recipe::{Bindings, Params, RecipeError, StepRequest, expand};
pub use record::{
    APPLIED_KEY, ApplicationRecord, Dep, RECORD_SCHEMA, RecordBindings, RecordError, Resolved, record_path,
};
pub use rights::{FIRST_PARTY_LICENCE, ItemRights};
pub use section::{ItemKind, Section, Target, UnknownSection};
pub use service::{
    CompatStatus, CompatTarget, Facets, ItemDetail, ItemInfo, ItemScope, Library, LibraryError, Query, ResolvedItem,
    RightsBadge, SearchPage, Usage,
};
pub use source::{ItemSource, SourceRegistration, sources};

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
