//! The eval-set store as a [`FileSource`] (ADR 0017): what `EvalEnv::files()` hands a model suite.
//!
//! A suite asks for a fixture by the same pin its model files use, and gets a local path holding exactly those bytes.
//! This source never downloads: the store is filled by `eval sets add`, and an item the store lacks is an error that says
//! so. Nothing unverified is ever returned: the item is hashed (BLAKE3 for its name, SHA-256 for the pin) as the path is
//! handed out, and its length is the pin's.

use std::path::PathBuf;

use omuzaki_ai_api::{FetchError, FileSource, PinnedFile, Progress, Sha256};

use super::store::{SetStore, SetsError};

/// A [`FileSource`] over a [`SetStore`].
#[derive(Debug, Clone)]
pub struct StoreFiles {
    store: SetStore,
}

impl StoreFiles {
    /// A source over `store`.
    #[must_use]
    pub fn new(store: SetStore) -> Self {
        Self { store }
    }

    fn io(&self, reason: String) -> FetchError {
        FetchError::Io { path: self.store.root().display().to_string(), reason }
    }
}

fn fetch_error(store: &SetStore, error: &SetsError) -> FetchError {
    FetchError::Io { path: store.root().display().to_string(), reason: error.to_string() }
}

impl FileSource for StoreFiles {
    fn get(&self, pin: &PinnedFile, _progress: &dyn Progress) -> Result<PathBuf, FetchError> {
        let Some(path) = self.lookup(pin.sha256)? else {
            return Err(self.io(format!(
                "no intact item with SHA-256 {} in the eval-set store; add it with `omuzaki-test eval sets add`",
                pin.sha256
            )));
        };
        let actual = std::fs::metadata(&path).map_err(|e| self.io(format!("{}: {e}", path.display())))?.len();
        if actual == pin.size {
            Ok(path)
        } else {
            Err(FetchError::Size { url: path.display().to_string(), expected: pin.size, actual })
        }
    }

    fn lookup(&self, sha256: Sha256) -> Result<Option<PathBuf>, FetchError> {
        self.store
            .find_sha256(&sha256)
            .map(|found| found.map(|(_, path)| path))
            .map_err(|e| fetch_error(&self.store, &e))
    }
}
