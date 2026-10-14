//! The `Library` service (design D11, D13): list, search, describe and resolve items from every scope.
//!
//! Defined here, implemented by `omuzaki-library` (and by the mock), and injected by the app like the store, so
//! ops reach it through `OpCtx::service::<dyn Library>()`.

use std::collections::BTreeMap;

use omuzaki_char_api::CharId;
use omuzaki_store_api::{Commercial, ContentHash};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ItemId, ItemKind, ItemManifest, PublisherCheck, Section, Version, VersionReq};

/// Where an item lives (design D11).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemScope {
    /// Compiled into this build, or a signed first-party pack.
    FirstParty,
    /// The person's own library.
    Mine,
    /// A team library (LB-07), by team id. In the contract now so search and records have a place for it.
    Team(String),
}

/// What an item is wanted for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Usage {
    /// Anything that may be published: hides items whose rights would block `io.check_export`.
    Publish,
}

/// What an item must be compatible with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompatTarget {
    /// A character in the project.
    Character(CharId),
}

/// A search or list request (design D13). Every filter is optional and they combine with AND.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Query {
    /// Free text over title, localised titles, tags, summary and id.
    pub text: Option<String>,
    /// Every tag listed must be present.
    pub tags: Vec<String>,
    /// One section.
    pub section: Option<Section>,
    /// One kind.
    pub kind: Option<ItemKind>,
    /// One scope.
    pub scope: Option<ItemScope>,
    /// Filters out incompatible items; style mismatches stay but rank lower and carry the warning.
    pub compatible_with: Option<CompatTarget>,
    /// Filters out items that would not be fit for the use.
    pub usable_for: Option<Usage>,
    /// Image search (LB-05): a store hash of the picture. Answers [`LibraryError::Unsupported`] until then.
    pub image: Option<ContentHash>,
    /// "Items like this" (LB-05): an item id. Answers [`LibraryError::Unsupported`] until then.
    pub like: Option<ItemId>,
    /// How many results to skip.
    pub offset: usize,
    /// The most results to return; implementations cap it.
    pub limit: usize,
}

impl Query {
    /// The refusal for the parts of the query that arrive later. Every implementation checks it first, so they
    /// all answer the same way until LB-05.
    ///
    /// # Errors
    /// [`LibraryError::Unsupported`] when `image` or `like` is set.
    pub fn check_supported(&self) -> Result<(), LibraryError> {
        if self.image.is_some() {
            return Err(LibraryError::Unsupported { feature: "image search (LB-05)" });
        }
        if self.like.is_some() {
            return Err(LibraryError::Unsupported { feature: "search by similar item (LB-05)" });
        }
        Ok(())
    }
}

/// The rights badge on a result row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RightsBadge {
    /// The SPDX expression or `LicenseRef`.
    pub licence: String,
    /// Commercial status.
    pub commercial: Commercial,
    /// The result belongs to whoever applies it.
    pub owned: bool,
    /// Private test material.
    pub private_test: bool,
    /// Whether what it makes may be published (`io.check_export` would pass).
    pub publish_ok: bool,
}

/// Whether an item fits a target (`compatible_with`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompatStatus {
    /// No target was named, or it fits.
    Compatible,
    /// Fits, with a style warning.
    StyleMismatch {
        /// The warning.
        reason: String,
    },
    /// Refused for this target.
    Incompatible {
        /// Why.
        reason: String,
    },
}

/// One result row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ItemInfo {
    /// The item.
    pub id: ItemId,
    /// The version shown (the newest compatible one).
    pub version: Version,
    /// Its manifest hash.
    pub hash: ContentHash,
    /// Title, in the requested locale when there is one.
    pub title: String,
    /// One sentence.
    pub summary: String,
    /// Browsing group.
    pub section: Section,
    /// Kind.
    pub kind: ItemKind,
    /// Tags.
    pub tags: Vec<String>,
    /// Where it lives.
    pub scope: ItemScope,
    /// Rights at a glance.
    pub rights: RightsBadge,
    /// Fit with the requested target.
    pub compat: CompatStatus,
    /// The cached preview for the requested template, else the default template's.
    pub preview: Option<ContentHash>,
    /// Why it can't be applied here (an op it needs is missing, a newer schema), if so.
    pub unavailable: Option<String>,
}

/// Counts per value over the whole result set (not just the page), for the panel's chips.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Facets {
    /// Per section, in section order.
    pub sections: BTreeMap<Section, usize>,
    /// Per kind.
    pub kinds: BTreeMap<ItemKind, usize>,
    /// Per tag.
    pub tags: BTreeMap<String, usize>,
    /// Per scope.
    pub scopes: BTreeMap<ItemScope, usize>,
}

/// One page of results.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchPage {
    /// How many items matched in all.
    pub total: usize,
    /// The rows for `offset` and `limit`, ranked.
    pub items: Vec<ItemInfo>,
    /// Counts over all matches.
    pub facets: Facets,
}

/// An item as the library holds it: the manifest and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedItem {
    /// The manifest.
    pub manifest: ItemManifest,
    /// Its hash.
    pub hash: ContentHash,
    /// Which scope supplied it.
    pub scope: ItemScope,
    /// How far its publisher can be trusted.
    pub publisher: PublisherCheck,
}

/// Everything `library.describe` shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemDetail {
    /// The row.
    pub info: ItemInfo,
    /// The item as held.
    pub item: ResolvedItem,
    /// Every installed version, oldest first.
    pub versions: Vec<Version>,
}

/// Why a library call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LibraryError {
    /// No item with that id (or hash).
    #[error("no item {0:?} in the library")]
    NotFound(String),
    /// The item exists but no installed version is in the range.
    #[error("no version of {item} matches {req}; installed: {installed:?}")]
    NoMatchingVersion {
        /// The item.
        item: ItemId,
        /// The range asked for.
        req: VersionReq,
        /// What is installed.
        installed: Vec<Version>,
    },
    /// Two publishers hold the same id, so the call can't say which is meant.
    #[error("{item} is published by more than one publisher ({}); name one", .candidates.join(", "))]
    Ambiguous {
        /// The item.
        item: ItemId,
        /// `name (fingerprint)` for each.
        candidates: Vec<String>,
    },
    /// The query uses something that arrives later.
    #[error("{feature} is not available yet")]
    Unsupported {
        /// What was asked for.
        feature: &'static str,
    },
    /// The library couldn't be read.
    #[error("library storage failed: {0}")]
    Storage(String),
}

/// The library: first-party items, the person's own and, later, team libraries, behind one lookup (design D11).
///
/// A project never needs it to build: applying materialises values into the document (design D5). It is for
/// finding, describing and applying items.
pub trait Library: Send + Sync {
    /// Items in section order, then id, optionally narrowed to a section and scope. With no section the facets
    /// carry the section counts.
    ///
    /// # Errors
    /// [`LibraryError::Storage`].
    fn list(
        &self,
        section: Option<Section>,
        scope: Option<&ItemScope>,
        offset: usize,
        limit: usize,
    ) -> Result<SearchPage, LibraryError>;

    /// Ranked search with filters and facets. Ties go by section order, then id, so results are deterministic.
    ///
    /// # Errors
    /// [`LibraryError::Unsupported`] for `image` and `like` until LB-05; [`LibraryError::Storage`].
    fn search(&self, query: &Query) -> Result<SearchPage, LibraryError>;

    /// One item in full. Without a version, the newest.
    ///
    /// # Errors
    /// [`LibraryError::NotFound`], [`LibraryError::NoMatchingVersion`], [`LibraryError::Ambiguous`].
    fn describe(&self, item: &ItemId, version: Option<&Version>) -> Result<ItemDetail, LibraryError>;

    /// The newest installed version of `item` in `req`, by resolution order: first-party, then the person's
    /// library, then team scopes.
    ///
    /// # Errors
    /// [`LibraryError::NotFound`], [`LibraryError::NoMatchingVersion`], [`LibraryError::Ambiguous`].
    fn resolve(&self, item: &ItemId, req: &VersionReq) -> Result<ResolvedItem, LibraryError>;

    /// The item whose canonical manifest has this hash, wherever it is held.
    ///
    /// # Errors
    /// [`LibraryError::NotFound`].
    fn by_hash(&self, hash: &ContentHash) -> Result<ResolvedItem, LibraryError>;

    /// Every installed version of `item`, oldest first.
    ///
    /// # Errors
    /// [`LibraryError::NotFound`], [`LibraryError::Ambiguous`].
    fn versions(&self, item: &ItemId) -> Result<Vec<Version>, LibraryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_and_like_answer_unsupported_until_lb05() {
        assert_eq!(Query::default().check_supported(), Ok(()));
        let image = Query { image: Some(ContentHash::of(b"x")), ..Query::default() };
        assert!(
            matches!(image.check_supported(), Err(LibraryError::Unsupported { feature }) if feature.contains("image"))
        );
        let like = Query { like: Some(ItemId::new("omuzaki/hair/twintails").unwrap()), ..Query::default() };
        assert!(matches!(like.check_supported(), Err(LibraryError::Unsupported { .. })));
    }

    #[test]
    fn a_query_reads_from_a_sparse_json_object() {
        let q: Query = serde_json::from_str(r#"{"text":"twin","section":"hair","limit":10}"#).unwrap();
        assert_eq!((q.text.as_deref(), q.section, q.limit, q.offset), (Some("twin"), Some(Section::Hair), 10, 0));
        assert!(serde_json::from_str::<Query>(r#"{"txet":"x"}"#).is_err());
    }

    #[test]
    fn the_library_is_usable_as_an_injected_service() {
        fn is_service<S: ?Sized + omuzaki_ops_api::Service>() {}
        is_service::<dyn Library>();
    }

    #[test]
    fn errors_say_what_to_do() {
        let e = LibraryError::Ambiguous {
            item: ItemId::new("mira/hair").unwrap(),
            candidates: vec!["Mira (ab12)".into(), "Mira (cd34)".into()],
        };
        assert!(e.to_string().contains("Mira (ab12), Mira (cd34)"), "{e}");
    }
}
