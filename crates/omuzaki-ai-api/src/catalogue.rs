//! Models delivered between releases: the types of [`ModelRegistry::sync_catalogue`](crate::ModelRegistry::sync_catalogue)
//! (`docs/design/updates.md` section 8, ADR 0035 decision 6).
//!
//! The registry never trusts a manifest to say where it came from: only that entry point assigns
//! [`Origin::Catalogue`](crate::Origin::Catalogue), and its caller (`omuzaki-update`) has already verified the signed
//! catalogue.

use crate::{ManifestError, ModelId, ModelManifest, ModelRef, Sha256, Version};

/// One entry of a verified catalogue.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogueEntry {
    /// The manifest the catalogue points at.
    pub manifest: ModelManifest,
    /// SHA-256 of the manifest file's bytes, as `omuzaki-update` verified it against the signed catalogue. It is not
    /// the model's digest, which covers the model's files.
    pub manifest_sha256: Sha256,
}

/// What one sync did. Every list is empty when the catalogue changed nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Versions imported for the first time.
    pub imported: Vec<ModelRef>,
    /// Entries already imported (or already shipped) that the catalogue listed again; their provenance is current.
    pub unchanged: Vec<ModelRef>,
    /// Older-epoch imports replaced by a different manifest of the same id and version; they are now revoked.
    pub superseded: Vec<ModelRef>,
    /// Older-epoch imports the new epoch's catalogue did not list again; they are now revoked.
    pub revoked: Vec<ModelRef>,
    /// Entries for a slot or interface version this app does not register; skipped, not an error.
    pub hidden: Vec<ModelId>,
}

/// Why a catalogue was refused. Nothing changed when one is returned.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ImportError {
    /// The same id and version, a different manifest, in one epoch (or against a shipped model): a version is never
    /// replaced within an epoch.
    #[error("{model} conflicts with a manifest of the same id and version from the same epoch")]
    Conflict {
        /// The entry that conflicts.
        model: ModelRef,
    },
    /// The id starts with `user-`, which is reserved for models people add.
    #[error("the catalogue lists {0}, but ids starting with user- are reserved for models people add")]
    UserId(ModelId),
    /// A new version of an existing id for another slot or interface version.
    #[error("{model} is for a different slot than the earlier versions of its id")]
    SlotMismatch {
        /// The entry.
        model: ModelRef,
    },
    /// A new version that is not above every non-revoked version of its id.
    #[error("{model} is not newer than {have}, which is already known")]
    NotNewer {
        /// The entry.
        model: ModelRef,
        /// The newest non-revoked version known.
        have: Version,
    },
    /// A manifest that fails the shipped rules.
    #[error("a catalogue manifest is refused: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Invalid(Vec<ManifestError>),
    /// The registry's own files could not be read or written.
    #[error("the model registry's files: {0}")]
    Io(String),
}
