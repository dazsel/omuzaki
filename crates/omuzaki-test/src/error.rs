//! The one error type for everything around the golden store (as opposed to an image simply not matching).

use std::io;
use std::path::{Path, PathBuf};

use image::ImageError;
use omuzaki_store_api::ContentHash;

use crate::compare::CompareError;
use crate::dirs::{GOLDENS_ENV, MANIFESTS_ENV};

/// Everything that can go wrong around the golden store.
#[derive(Debug, thiserror::Error)]
pub enum GoldenError {
    /// `OMUZAKI_GOLDENS` is unset and no directory was given.
    #[error("no golden store: set {GOLDENS_ENV} (for example $OMUZAKI_ASSETS/goldens) or pass --goldens")]
    NoGoldensDir,
    /// No manifest folder was given and none could be derived from the current directory.
    #[error(
        "no manifest folder: run inside the Omuzaki workspace (manifests are tests/goldens), \
         or set {MANIFESTS_ENV} or pass --manifests"
    )]
    NoManifestsDir,
    /// A golden id isn't `suite/name` with slug parts.
    #[error("bad golden id '{id}': {reason}")]
    BadId {
        /// The id as given.
        id: String,
        /// Which rule it broke.
        reason: String,
    },
    /// An image file could not be read, decoded or written.
    #[error("{}: {source}", path.display())]
    Image {
        /// The file involved.
        path: PathBuf,
        /// The decoder or encoder error.
        source: ImageError,
    },
    /// An image could not be encoded as PNG in memory.
    #[error("could not encode the image as PNG: {0}")]
    Encode(#[source] ImageError),
    /// A file or folder operation failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or folder involved.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// A stored `<name>.json` could not be read back.
    #[error("{}: not a valid finding file: {source}", path.display())]
    Json {
        /// The file involved.
        path: PathBuf,
        /// The parse error.
        source: serde_json::Error,
    },
    /// A manifest could not be parsed, or holds something invalid (such as a nonsense tolerance).
    #[error("{}: not a valid golden manifest: {reason}", path.display())]
    Manifest {
        /// The manifest file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// A manifest names an image the object store doesn't hold: it was blessed on another machine, or the store was
    /// cleaned.
    #[error("golden image {hash} is not in the object store ({}); bless it again or restore the store", path.display())]
    ObjectMissing {
        /// The hash the manifest asks for.
        hash: ContentHash,
        /// Where the object would be.
        path: PathBuf,
    },
    /// A stored object's bytes no longer match its name.
    #[error("golden image {hash} is corrupt ({}): its bytes no longer match the hash", path.display())]
    ObjectCorrupt {
        /// The hash the file is named after.
        hash: ContentHash,
        /// The damaged file.
        path: PathBuf,
    },
    /// A `git` command failed, or could not be started. `gc` stops at the first such failure: it deletes whatever the
    /// branches don't name, so an incomplete answer from git could delete an image a branch still needs.
    #[error("git: could not {what}: {detail}")]
    Git {
        /// What was being attempted, as a verb phrase.
        what: String,
        /// Git's own message, or the OS error when git could not be run.
        detail: String,
    },
    /// `gc` found no golden manifest on any branch or in any worktree. That means the manifest folder is wrong, not
    /// that every stored image is garbage, so nothing is listed for deletion.
    #[error(
        "no golden manifest found in '{folder}' on any branch or worktree; refusing to treat every stored image as \
         unreferenced (is --manifests right?)"
    )]
    NoManifestsAnywhere {
        /// The manifest folder, relative to the repository root.
        folder: String,
    },
    /// `verify` found no manifest in the folder, because the folder is missing or holds none. Checking nothing must
    /// not pass: a wrong `--manifests` would otherwise look like a clean store.
    #[error(
        "no golden manifest in '{}' (the folder is missing or holds none), so there is nothing to verify and that is \
         not a pass (is --manifests right?)",
        folder.display()
    )]
    NoManifestsIn {
        /// The manifest folder as given.
        folder: PathBuf,
    },
    /// `gc` could not be sure which images are wanted, so it stopped before deleting anything: the clone is shallow or
    /// fetches only some branches or last fetched too long ago, the manifest folder is spelled in two ways, a worktree on
    /// a removable drive is gone, or what the branches name changed while it ran. Every one of these is exit 2 with the
    /// images kept.
    #[error("gc stopped without deleting anything: {reason}. {fix}")]
    GcRefused {
        /// What `gc` could not be sure of.
        reason: String,
        /// What to do about it.
        fix: String,
    },
    /// The comparison itself was refused (bad tolerance).
    #[error(transparent)]
    Compare(#[from] CompareError),
}

/// Wraps an I/O error with the path it happened on: `.map_err(io_err(path))`.
pub(crate) fn io_err(path: &Path) -> impl FnOnce(io::Error) -> GoldenError + '_ {
    move |source| GoldenError::Io { path: path.to_owned(), source }
}

/// A [`GoldenError::GcRefused`]: `gc` is not sure enough to delete.
pub(crate) fn gc_refused(reason: impl Into<String>, fix: impl Into<String>) -> GoldenError {
    GoldenError::GcRefused { reason: reason.into(), fix: fix.into() }
}
