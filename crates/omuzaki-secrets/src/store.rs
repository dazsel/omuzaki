//! [`KeychainSecrets`]: the label index over a [`Backend`], and the `keyring` backend.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use omuzaki_secrets_api::{SecretError, SecretLabel, SecretStore, SecretValue};

use crate::error::map_error;
use crate::index::{INDEX_WHAT, decode, encode, is_shard_user, shard_user};

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
    if is_shard_user(user) { INDEX_WHAT } else { user }
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

/// How many times one change to the index is tried before giving up. Each try re-reads the index, so a change another
/// program's write overwrote is made again.
const SETTLE_ATTEMPTS: usize = 4;

/// Entries keyed by label plus the label index, over any [`Backend`].
#[derive(Debug)]
pub(crate) struct Indexed<B> {
    backend: B,
    /// Serialises changes to the label index (read, change, write). Handles over the same keychain share one.
    index_lock: Arc<Mutex<()>>,
}

impl<B: Backend> Indexed<B> {
    /// A store with a lock of its own: right for a backend only this store reaches.
    #[cfg(test)]
    pub(crate) fn new(backend: B) -> Self {
        Self::sharing(backend, Arc::default())
    }

    /// A store that serialises its index changes with every other store holding `index_lock`.
    pub(crate) fn sharing(backend: B, index_lock: Arc<Mutex<()>>) -> Self {
        Self { backend, index_lock }
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        // The guard protects no data, only the order of keychain calls, so a poisoned lock is still correct.
        self.index_lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The labels and how many shards hold them.
    fn read_index(&self) -> Result<(BTreeSet<SecretLabel>, usize), SecretError> {
        let mut shards = Vec::new();
        while let Some(text) = self.backend.get(&shard_user(shards.len()))? {
            shards.push(text);
        }
        let labels = decode(shards.iter().map(SecretValue::expose))?;
        Ok((labels, shards.len()))
    }

    /// Writes `labels` as shards 0..n, then removes the `previous` shards past n, last first, so the shards stay
    /// contiguous if a call fails part-way.
    fn write_index(&self, labels: &BTreeSet<SecretLabel>, previous: usize) -> Result<(), SecretError> {
        let shards = encode(labels);
        for (n, text) in shards.iter().enumerate() {
            self.backend.set(&shard_user(n), text)?;
        }
        for n in (shards.len()..previous).rev() {
            self.backend.remove(&shard_user(n))?;
        }
        Ok(())
    }

    /// Makes the index list `label` (`listed`) or not, then reads the index again to see the change held: another
    /// program may have written the same shard between this read and write, which a lock in this process cannot
    /// stop. The change is made again, up to [`SETTLE_ATTEMPTS`] times.
    fn settle(&self, label: &SecretLabel, listed: bool) -> Result<(), SecretError> {
        let _guard = self.lock();
        for _ in 0..SETTLE_ATTEMPTS {
            let (mut labels, shards) = self.read_index()?;
            if labels.contains(label) == listed {
                return Ok(());
            }
            if listed {
                labels.insert(label.clone());
            } else {
                labels.remove(label);
            }
            self.write_index(&labels, shards)?;
        }
        Err(SecretError::backend("the label list kept changing under another program's writes"))
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
        Ok(self.read_index()?.0.into_iter().collect())
    }

    fn set(&self, label: &SecretLabel, value: SecretValue) -> Result<(), SecretError> {
        value.check_storable()?;
        // The entry first: a failure between the two steps leaves a readable secret missing from `labels`, which
        // setting it again repairs, never a listed label with nothing behind it.
        self.backend.set(label.as_str(), value.expose())?;
        self.settle(label, true)
    }

    fn delete(&self, label: &SecretLabel) -> Result<(), SecretError> {
        // The entry first: a failure between the two steps leaves a listed label with no secret, which the person
        // sees as missing and deleting again clears, never a stored secret that nothing lists.
        self.backend.remove(label.as_str())?;
        self.settle(label, false)
    }
}

/// The OS keychain as a [`SecretStore`].
///
/// Cheap to create and to share. The keychain is opened on first use, so a machine without one fails the first call
/// with [`SecretError::Unavailable`], not construction.
#[derive(Debug)]
pub struct KeychainSecrets(Indexed<KeyringBackend>);

impl KeychainSecrets {
    /// A store over the platform keychain. Every store in the process shares one index lock, so handles made
    /// separately do not overwrite each other's changes to the label list.
    #[must_use]
    pub fn new() -> Self {
        static INDEX_LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
        let lock = INDEX_LOCK.get_or_init(Arc::default);
        Self(Indexed::sharing(KeyringBackend, Arc::clone(lock)))
    }
}

impl Default for KeychainSecrets {
    fn default() -> Self {
        Self::new()
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
