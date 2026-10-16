//! Eval sets (design 4, 10.1, 11): the non-code inputs of evals, as manifests in git and items in a content store.
//!
//! - [`store`]: [`SetStore`], the content-addressed objects at `<root>/objects/<hash>`, verified on every read
//! - [`files`]: [`StoreFiles`], an [`omuzaki_ai_api::FileSource`] over the store, for model suites
//! - [`verify`]: which items of the manifests are missing or damaged
//! - [`add`]: `eval sets add`, which stores an item and writes its manifest entry, refusing what the rules refuse
//! - [`holdout`]: `eval sets holdout-check`, which finds real and held-out items in a training manifest
//!
//! The manifests (`crates/<crate>/evalsets/<id>.toml`) are found by [`discover`](super::discover); the items are not in
//! git (`docs/decisions/0003-assets-outside-git.md`).

pub mod add;
pub mod files;
pub mod holdout;
pub mod store;
pub mod verify;

pub use self::files::StoreFiles;
pub use self::store::{ItemState, SETS_ENV, SetStore, SetsError};

/// The folder of a set's manifests inside a crate.
pub const MANIFEST_DIR: &str = "evalsets";
