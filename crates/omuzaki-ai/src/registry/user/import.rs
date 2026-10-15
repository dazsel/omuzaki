//! Copying a person's file into the cache: hashed while it is copied, stored under its hash by the cache's own code
//! (`crate::fetch::import`), so the `sha256/<hex>` layout, its records and its locks have one owner.

use std::path::{Path, PathBuf};

use omuzaki_ai_api::{FetchError, Progress, RegistryError, Sha256};

/// One file now in the cache.
#[derive(Clone, Debug)]
pub(super) struct Imported {
    /// The original's file name, which the manifest calls it by.
    pub name: String,
    pub sha256: Sha256,
    pub size: u64,
}

/// Where the cache keeps the blob with hash `sha256` (the layout `VerifiedCache` reads and `FileSource::lookup`
/// finds again).
pub(super) fn blob_path(cache: &Path, sha256: Sha256) -> PathBuf {
    cache.join("sha256").join(sha256.to_string())
}

/// Copies `source` into `cache` under its SHA-256 and returns what it found. Errors name the file, never the folder
/// it came from (where a person keeps their files is theirs, ADR 0013).
pub(super) fn import(cache: &Path, source: &Path, progress: &dyn Progress) -> Result<Imported, RegistryError> {
    let name = source.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    match crate::fetch::import(cache, source, progress) {
        Ok((sha256, size)) => Ok(Imported { name, sha256, size }),
        Err(FetchError::Cancelled) => Err(RegistryError::Cancelled),
        Err(FetchError::Io { path, reason }) if Path::new(&path) == source => {
            Err(RegistryError::Io(format!("{name}: {reason}")))
        }
        Err(FetchError::Io { reason, .. }) => Err(RegistryError::Io(format!("the model cache: {reason}"))),
        Err(other) => Err(RegistryError::Fetch(other)),
    }
}
