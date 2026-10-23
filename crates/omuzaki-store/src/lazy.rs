//! [`LazyDiskStore`]: a [`DiskStore`] that opens on first use, so a host can offer the asset store to every op without
//! creating its folder for a command that never stores anything.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use omuzaki_store_api::{ContentHash, Provenance, Store, StoreError};

use crate::DiskStore;

/// The [`DiskStore`] at a root, opened the first time it is used. A failed open is remembered: every call then answers
/// the same [`StoreError::Io`], naming the folder, so an op reports why it has no store instead of retrying each time.
#[derive(Debug)]
pub struct LazyDiskStore {
    root: PathBuf,
    store: OnceLock<Result<DiskStore, StoreError>>,
}

impl LazyDiskStore {
    /// The store at `root`, not opened yet.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), store: OnceLock::new() }
    }

    /// The folder the store lives in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The store, opening it on the first call.
    ///
    /// # Errors
    /// [`StoreError::Io`] when the folder can't be created.
    pub fn open(&self) -> Result<&DiskStore, StoreError> {
        self.store.get_or_init(|| DiskStore::open(&self.root)).as_ref().map_err(Clone::clone)
    }
}

impl Store for LazyDiskStore {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        self.open()?.put(bytes, provenance)
    }

    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        self.open()?.get(hash)
    }

    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.open()?.provenance(hash)
    }

    fn contains(&self, hash: &ContentHash) -> bool {
        self.open().is_ok_and(|store| store.contains(hash))
    }

    fn size(&self, hash: &ContentHash) -> Result<u64, StoreError> {
        self.open()?.size(hash)
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_store_api::Commercial;
    use omuzaki_store_mock::conformance::sample_provenance;

    use super::*;

    #[test]
    fn the_folder_is_made_on_first_use_and_the_bytes_round_trip() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let root = tmp.path().join("store");
        let store = LazyDiskStore::new(&root);
        assert!(!root.exists(), "nothing is created before the store is used");
        let hash = store.put(b"bytes", sample_provenance(Commercial::Ok)).unwrap_or_else(|e| panic!("{e}"));
        assert!(root.join("objects").is_dir());
        assert_eq!(store.get(&hash).unwrap_or_else(|e| panic!("{e}")), b"bytes");
        assert!(store.contains(&hash));
        // Another store over the folder sees the same object.
        assert!(DiskStore::open(&root).unwrap_or_else(|e| panic!("{e}")).contains(&hash));
    }

    #[test]
    fn a_folder_that_cannot_be_made_fails_every_call_with_its_name() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let file = tmp.path().join("a-file");
        std::fs::write(&file, b"not a folder").unwrap_or_else(|e| panic!("{e}"));
        let store = LazyDiskStore::new(file.join("store"));
        let error = store.put(b"x", sample_provenance(Commercial::Ok)).unwrap_err();
        assert!(matches!(&error, StoreError::Io(text) if text.contains("a-file")), "{error}");
        assert_eq!(store.get(&ContentHash::of(b"x")).unwrap_err(), error);
        assert!(!store.contains(&ContentHash::of(b"x")));
    }
}
