//! [`KeychainSecrets`]: the label index over a [`Backend`], and the `keyring` backend.

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_secrets_api::{SecretError, SecretLabel, SecretStore, SecretValue};

use crate::error::map_error;
use crate::index::{INDEX_USER, INDEX_WHAT, decode, encode};

/// The keychain service name every Omuzaki entry is filed under.
pub const SERVICE: &str = "omuzaki";

/// One keychain entry per user name, under [`SERVICE`]. The seam the tests replace with a map; `keyring` is the
/// only real implementation.
pub(crate) trait Backend: Send + Sync {
    /// The text of the entry `user`, or `None` when it does not exist.
    fn get(&self, user: &str) -> Result<Option<SecretValue>, SecretError>;
    /// Creates or replaces the entry `user`.
    fn set(&self, user: &str, text: &str) -> Result<(), SecretError>;
    /// Removes the entry `user`; one that does not exist is not an error.
    fn remove(&self, user: &str) -> Result<(), SecretError>;
}

/// What an entry is called in an error.
fn describe(user: &str) -> &str {
    if user == INDEX_USER { INDEX_WHAT } else { user }
}

/// The platform keychain through `keyring`. Holds no connection: the keychain is opened on first use.
#[derive(Debug, Default)]
pub(crate) struct KeyringBackend;

impl KeyringBackend {
    fn entry(user: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(SERVICE, user).map_err(|e| map_error(e, describe(user)))
    }
}

impl Backend for KeyringBackend {
    fn get(&self, user: &str) -> Result<Option<SecretValue>, SecretError> {
        match Self::entry(user)?.get_password() {
            Ok(text) => Ok(Some(SecretValue::new(text))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(e, describe(user))),
        }
    }

    fn set(&self, user: &str, text: &str) -> Result<(), SecretError> {
        Self::entry(user)?.set_password(text).map_err(|e| map_error(e, describe(user)))
    }

    fn remove(&self, user: &str) -> Result<(), SecretError> {
        match Self::entry(user)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_error(e, describe(user))),
        }
    }
}

/// Entries keyed by label plus the label index, over any [`Backend`].
#[derive(Debug, Default)]
pub(crate) struct Indexed<B> {
    backend: B,
    /// Serialises changes to the label index within this process (read, change, write).
    index_lock: Mutex<()>,
}

impl<B: Backend> Indexed<B> {
    pub(crate) fn new(backend: B) -> Self {
        Self { backend, index_lock: Mutex::new(()) }
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        // The guard protects no data, only the order of keychain calls, so a poisoned lock is still correct.
        self.index_lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read_index(&self) -> Result<BTreeSet<SecretLabel>, SecretError> {
        match self.backend.get(INDEX_USER)? {
            Some(text) => decode(text.expose()),
            None => Ok(BTreeSet::new()),
        }
    }

    fn write_index(&self, labels: &BTreeSet<SecretLabel>) -> Result<(), SecretError> {
        if labels.is_empty() {
            return self.backend.remove(INDEX_USER);
        }
        self.backend.set(INDEX_USER, &encode(labels))
    }
}

impl<B: Backend> SecretStore for Indexed<B> {
    fn get(&self, label: &SecretLabel) -> Result<Option<SecretValue>, SecretError> {
        self.backend.get(label.as_str())
    }

    fn contains(&self, label: &SecretLabel) -> Result<bool, SecretError> {
        Ok(self.get(label)?.is_some())
    }

    fn labels(&self) -> Result<Vec<SecretLabel>, SecretError> {
        Ok(self.read_index()?.into_iter().collect())
    }

    fn set(&self, label: &SecretLabel, value: SecretValue) -> Result<(), SecretError> {
        value.check_storable()?;
        let _guard = self.lock();
        // The entry first: a failure between the two steps leaves a readable secret missing from `labels`, which
        // setting it again repairs, never a listed label with nothing behind it.
        self.backend.set(label.as_str(), value.expose())?;
        let mut labels = self.read_index()?;
        if labels.insert(label.clone()) {
            self.write_index(&labels)?;
        }
        Ok(())
    }

    fn delete(&self, label: &SecretLabel) -> Result<(), SecretError> {
        let _guard = self.lock();
        // The entry first: a failure between the two steps leaves a listed label with no secret, which the person
        // sees as missing and deleting again clears, never a stored secret that nothing lists.
        self.backend.remove(label.as_str())?;
        let mut labels = self.read_index()?;
        if labels.remove(label) {
            self.write_index(&labels)?;
        }
        Ok(())
    }
}

/// The OS keychain as a [`SecretStore`].
///
/// Cheap to create and to share. The keychain is opened on first use, so a machine without one fails the first call
/// with [`SecretError::Unavailable`], not construction.
#[derive(Debug, Default)]
pub struct KeychainSecrets(Indexed<KeyringBackend>);

impl KeychainSecrets {
    /// A store over the platform keychain.
    #[must_use]
    pub fn new() -> Self {
        Self(Indexed::new(KeyringBackend))
    }
}

impl SecretStore for KeychainSecrets {
    fn get(&self, label: &SecretLabel) -> Result<Option<SecretValue>, SecretError> {
        self.0.get(label)
    }

    fn contains(&self, label: &SecretLabel) -> Result<bool, SecretError> {
        self.0.contains(label)
    }

    fn labels(&self) -> Result<Vec<SecretLabel>, SecretError> {
        self.0.labels()
    }

    fn set(&self, label: &SecretLabel, value: SecretValue) -> Result<(), SecretError> {
        self.0.set(label, value)
    }

    fn delete(&self, label: &SecretLabel) -> Result<(), SecretError> {
        self.0.delete(label)
    }
}

#[cfg(test)]
mod tests;
