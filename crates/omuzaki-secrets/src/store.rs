//! [`KeychainSecrets`]: the label index over a [`Backend`], and the `keyring` backend.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use omuzaki_secrets_api::error::SecretError;
use omuzaki_secrets_api::label::SecretLabel;
use omuzaki_secrets_api::store::SecretStore;
use omuzaki_secrets_api::value::SecretValue;

use crate::error::map_error;
use crate::index::{HEAD_USER, INDEX_WHAT, decode, decode_head, encode, encode_head, is_index_user, shard_user};

/// The keychain service name every Omuzaki entry is filed under.
pub const SERVICE: &str = "omuzaki";

/// The name of the lock file in the data directory.
pub const LOCK_FILE: &str = "secrets.lock";

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
    if is_index_user(user) { INDEX_WHAT } else { user }
}

/// The platform keychain through `keyring`. Holds no connection: the keychain is opened on first use.
#[derive(Debug)]
pub(crate) struct KeyringBackend {
    /// The keychain service every entry of this store is filed under.
    service: String,
}

impl KeyringBackend {
    fn entry(&self, user: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, user).map_err(|e| map_error(e, describe(user)))
    }
}

impl Backend for KeyringBackend {
    fn get(&self, user: &str) -> Result<Option<SecretValue>, SecretError> {
        match self.entry(user)?.get_password() {
            Ok(text) => Ok(Some(SecretValue::new(text))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(e, describe(user))),
        }
    }

    fn set(&self, user: &str, text: &str) -> Result<(), SecretError> {
        self.entry(user)?.set_password(text).map_err(|e| map_error(e, describe(user)))
    }

    fn remove(&self, user: &str) -> Result<(), SecretError> {
        match self.entry(user)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_error(e, describe(user))),
        }
    }
}

/// An exclusive lock on a file, held across the programs on this machine that share the data directory. It is what
/// gives the label index one writer at a time: `std::fs::File::lock` is `flock` on Unix and `LockFileEx` on Windows,
/// and it also excludes a second handle in the same process.
#[derive(Debug)]
pub(crate) struct DirLock {
    path: PathBuf,
}

/// The lock while held; dropping it releases it.
#[derive(Debug)]
pub(crate) struct Held(File);

impl DirLock {
    /// A lock on the file `name` in `data_dir`. Nothing is created until the first [`acquire`](Self::acquire).
    pub(crate) fn new(data_dir: &Path, name: &str) -> Self {
        Self { path: data_dir.join(name) }
    }

    /// Waits for the lock.
    fn acquire(&self) -> Result<Held, SecretError> {
        let io = |what: &str, e: std::io::Error| SecretError::backend(format!("{what}: {}", e.kind()));
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| io("cannot create the data directory for the store's lock", e))?;
        }
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.path)
            .map_err(|e| io("cannot open the store's lock file", e))?;
        file.lock().map_err(|e| io("cannot lock the store", e))?;
        Ok(Held(file))
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // Closing the file releases the lock too; unlocking first just does it at once. An error here has nothing
        // to say to anyone: the lock is released either way.
        let _ = self.0.unlock();
    }
}

/// The index as read: its labels and where they are kept.
#[derive(Debug, Default)]
struct IndexState {
    labels: BTreeSet<SecretLabel>,
    /// The current generation and how many shards it has; `None` when there is no head.
    current: Option<(u64, usize)>,
}

/// Entries keyed by label plus the label index, over any [`Backend`].
#[derive(Debug)]
pub(crate) struct Indexed<B> {
    backend: B,
    /// Held around every call that reads or changes the index, and around the secret entry a change belongs to.
    lock: DirLock,
}

impl<B: Backend> Indexed<B> {
    pub(crate) fn new(backend: B, lock: DirLock) -> Self {
        Self { backend, lock }
    }

    fn read_index(&self) -> Result<IndexState, SecretError> {
        let Some(head) = self.backend.get(HEAD_USER)? else {
            return Ok(IndexState::default());
        };
        let (generation, count) = decode_head(head.expose())?;
        // Every shard the head names has to be there: a missing one is lost labels, not a shorter list.
        let mut shards = Vec::with_capacity(count);
        for n in 0..count {
            match self.backend.get(&shard_user(generation, n))? {
                Some(text) => shards.push(text),
                None => return Err(SecretError::corrupt(INDEX_WHAT)),
            }
        }
        let labels = decode(shards.iter().map(SecretValue::expose))?;
        Ok(IndexState { labels, current: Some((generation, count)) })
    }

    /// Makes `labels` the index: the next generation's shards in full, then the head, then the old shards go. A
    /// failure before the head leaves the old index as it was; one after it leaves unreferenced clutter.
    fn publish(&self, labels: &BTreeSet<SecretLabel>, from: Option<(u64, usize)>) -> Result<(), SecretError> {
        // A head already at the last generation cannot be stepped past: say so, never wrap to one that may have
        // stale shards behind it.
        let next = match from {
            None => 0,
            Some((generation, _)) => generation.checked_add(1).ok_or_else(|| SecretError::corrupt(INDEX_WHAT))?,
        };
        let shards = encode(labels);
        if shards.is_empty() {
            self.backend.remove(HEAD_USER)?;
        } else {
            // Shards an earlier, failed attempt at this generation left behind are replaced or cleared first, so
            // none is left beyond the end once this generation is published.
            let mut n = shards.len();
            while self.backend.get(&shard_user(next, n))?.is_some() {
                self.backend.remove(&shard_user(next, n))?;
                n += 1;
            }
            for (n, text) in shards.iter().enumerate() {
                self.backend.set(&shard_user(next, n), text)?;
            }
            self.backend.set(HEAD_USER, &encode_head(next, shards.len()))?;
        }
        if let Some((old, count)) = from {
            for n in 0..count {
                // Unreferenced now, so a failure only leaves clutter that nothing reads.
                let _ = self.backend.remove(&shard_user(old, n));
            }
        }
        Ok(())
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
        let _held = self.lock.acquire()?;
        Ok(self.read_index()?.labels.into_iter().collect())
    }

    fn set(&self, label: &SecretLabel, value: SecretValue) -> Result<(), SecretError> {
        value.check_storable()?;
        let _held = self.lock.acquire()?;
        // The entry first: a failure between the two steps leaves a readable secret missing from `labels`, which
        // setting it again repairs, never a listed label with nothing behind it.
        self.backend.set(label.as_str(), value.expose())?;
        let mut index = self.read_index()?;
        if index.labels.insert(label.clone()) {
            self.publish(&index.labels, index.current)?;
        }
        Ok(())
    }

    fn delete(&self, label: &SecretLabel) -> Result<(), SecretError> {
        let _held = self.lock.acquire()?;
        // The entry first: a failure between the two steps leaves a listed label with no secret, which the person
        // sees as missing and deleting again clears, never a stored secret that nothing lists.
        self.backend.remove(label.as_str())?;
        let mut index = self.read_index()?;
        if index.labels.remove(label) {
            self.publish(&index.labels, index.current)?;
        }
        Ok(())
    }
}

/// The OS keychain as a [`SecretStore`].
///
/// Cheap to create and to share. The keychain is opened on first use, so a machine without one fails the first call
/// with [`SecretError::Unavailable`], not construction.
///
/// Every `set`, `delete` and `labels` holds a lock on a file in the data directory, so the label list has one writer
/// at a time across every handle and every Omuzaki process that shares the keychain service **and** the data
/// directory. The file holds nothing: no secret and no label ever touches the disk.
///
/// The keychain service and the lock file go together: the label list belongs to the service, and its lock to the
/// directory. [`new`](Self::new) is the service `omuzaki` and `secrets.lock`, so every Omuzaki process of one OS
/// user has to pass the app's one local data directory. Anything that must not share that list, such as a test,
/// uses [`in_namespace`](Self::in_namespace), which has a service and a lock file of its own.
#[derive(Debug)]
pub struct KeychainSecrets(Indexed<KeyringBackend>);

impl KeychainSecrets {
    /// The store the app uses: the keychain service `omuzaki`, with its lock file in `data_dir` (the app's local data
    /// directory). The directory is created on first use.
    #[must_use]
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        let backend = KeyringBackend { service: SERVICE.to_owned() };
        Self(Indexed::new(backend, DirLock::new(data_dir.as_ref(), LOCK_FILE)))
    }

    /// A store with a namespace of its own: the keychain service `omuzaki.<namespace>` and the lock file
    /// `secrets-<namespace>.lock` in `data_dir`. It shares no label list and no lock with [`new`](Self::new) or with
    /// another namespace, wherever their data directories are. For tests and tools that must not touch the app's
    /// secrets.
    ///
    /// # Errors
    /// [`SecretError::Rejected`] when `namespace` is not 1 to 40 characters of `a-z`, `0-9` and `-`.
    pub fn in_namespace(namespace: &str, data_dir: impl AsRef<Path>) -> Result<Self, SecretError> {
        let valid = (1..=40).contains(&namespace.len())
            && namespace.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !valid {
            return Err(SecretError::rejected("a namespace is 1 to 40 characters of a-z, 0-9 and -"));
        }
        let backend = KeyringBackend { service: format!("{SERVICE}.{namespace}") };
        Ok(Self(Indexed::new(backend, DirLock::new(data_dir.as_ref(), &format!("secrets-{namespace}.lock")))))
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
