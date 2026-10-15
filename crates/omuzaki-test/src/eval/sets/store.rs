//! The content store of eval-set items: `<root>/objects/<blake3 hex>`, with `<root>/sha256/<sha-256 hex>` naming the
//! object that has those bytes, so a model suite can ask for an item by the SHA-256 pin its contract uses.
//!
//! The same idea as the golden store (`crate::objects`) and the asset store (`omuzaki-store-api`): an item is named by the
//! BLAKE3 hash of its bytes, identical items are stored once, and **every read checks the bytes against the name**. A
//! stored item is never rewritten while it is intact; a damaged one is replaced. Nothing here deletes an item.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_ai_api::Sha256 as PinSha256;
use omuzaki_store_api::ContentHash;
use sha2::{Digest, Sha256};

/// The variable that names the store's root (default: `evals/sets` in the assets folder).
pub const SETS_ENV: &str = "OMUZAKI_EVAL_SETS";
const OBJECTS: &str = "objects";
const INDEX: &str = "sha256";
/// The prefix of an item's hash in a manifest.
pub const HASH_PREFIX: &str = "b3:";
const CHUNK: usize = 64 * 1024;

/// Why the store could not be used.
#[derive(Debug, thiserror::Error)]
pub enum SetsError {
    /// A file or folder could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// Neither the variable nor the assets folder says where the store is.
    #[error("set {SETS_ENV} (or OMUZAKI_ASSETS, whose `evals/sets` folder is the default)")]
    NoRoot,
    /// A hash that is not `b3:` and 64 lowercase hex digits.
    #[error("`{0}` is not an item hash: `b3:` and 64 lowercase hex digits")]
    BadHash(String),
    /// A manifest or argument is not usable.
    #[error("{0}")]
    Invalid(String),
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> SetsError + '_ {
    move |source| SetsError::Io { path: path.to_owned(), source }
}

/// Parses a manifest hash: `b3:` and 64 lowercase hex digits.
///
/// # Errors
/// [`SetsError::BadHash`] for anything else.
pub fn parse_hash(text: &str) -> Result<ContentHash, SetsError> {
    text.strip_prefix(HASH_PREFIX)
        .and_then(|hex| ContentHash::from_str(hex).ok())
        .ok_or_else(|| SetsError::BadHash(text.to_owned()))
}

/// How an item is in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    /// There, and its bytes hash to its name.
    Present,
    /// Not in the store.
    Missing,
    /// There, but its bytes no longer hash to its name.
    Damaged,
}

/// What putting a file in the store gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stored {
    /// The item's name.
    pub hash: ContentHash,
    /// Its SHA-256, for the model contract's pins.
    pub sha256: PinSha256,
    /// Its length in bytes.
    pub size: u64,
}

/// The store rooted at one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetStore {
    root: PathBuf,
}

impl SetStore {
    /// The store rooted at `root` (nothing is created until the first [`SetStore::put_file`]).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store the environment names: `$OMUZAKI_EVAL_SETS`, else `$OMUZAKI_ASSETS/evals/sets`.
    ///
    /// # Errors
    /// [`SetsError::NoRoot`] when neither is set.
    pub fn locate() -> Result<Self, SetsError> {
        if let Some(root) = std::env::var_os(SETS_ENV) {
            return Ok(Self::new(root));
        }
        std::env::var_os("OMUZAKI_ASSETS")
            .map(|assets| Self::new(PathBuf::from(assets).join("evals").join("sets")))
            .ok_or(SetsError::NoRoot)
    }

    /// The store's root folder.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the item with `hash` is, or would be.
    #[must_use]
    pub fn object_path(&self, hash: &ContentHash) -> PathBuf {
        self.root.join(OBJECTS).join(hash.to_string())
    }

    fn index_path(&self, sha256: &PinSha256) -> PathBuf {
        self.root.join(INDEX).join(sha256.to_string())
    }

    /// Stores the file at `source` and returns its names. The file is copied in one pass that hashes it both ways; an item
    /// that is already there and intact is left alone, and a damaged one is replaced.
    ///
    /// # Errors
    /// [`SetsError::Io`] when the file cannot be read or the item cannot be written.
    pub fn put_file(&self, source: &Path) -> Result<Stored, SetsError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let objects = self.root.join(OBJECTS);
        fs::create_dir_all(&objects).map_err(io_err(&objects))?;
        let staging = objects.join(format!(".staging-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let copied = copy_hashing(source, &staging);
        let (hash, sha256, size) = match copied {
            Ok(done) => done,
            Err(e) => {
                // A leftover staging file is harmless; the copy's own error is the one to report.
                drop(fs::remove_file(&staging));
                return Err(e);
            }
        };
        let target = self.object_path(&hash);
        if self.state(&hash)? == ItemState::Present {
            drop(fs::remove_file(&staging));
        } else {
            fs::rename(&staging, &target).map_err(io_err(&target))?;
        }
        let index = self.index_path(&sha256);
        let index_dir = self.root.join(INDEX);
        fs::create_dir_all(&index_dir).map_err(io_err(&index_dir))?;
        fs::write(&index, hash.to_string()).map_err(io_err(&index))?;
        Ok(Stored { hash, sha256, size })
    }

    /// Stores `bytes` (the same as [`SetStore::put_file`] for a file with these contents).
    ///
    /// # Errors
    /// [`SetsError::Io`] when the item cannot be written.
    pub fn put(&self, bytes: &[u8]) -> Result<Stored, SetsError> {
        let staging = self.root.join(format!(".put-{}", std::process::id()));
        fs::create_dir_all(&self.root).map_err(io_err(&self.root))?;
        fs::write(&staging, bytes).map_err(io_err(&staging))?;
        let stored = self.put_file(&staging);
        drop(fs::remove_file(&staging));
        stored
    }

    /// How the item is in the store: present with its bytes hashing to its name, missing, or damaged.
    ///
    /// # Errors
    /// [`SetsError::Io`] for a read failure other than the file not being there.
    pub fn state(&self, hash: &ContentHash) -> Result<ItemState, SetsError> {
        let path = self.object_path(hash);
        match File::open(&path) {
            Ok(file) => {
                let (found, _, _) = hash_reader(file).map_err(io_err(&path))?;
                Ok(if found == *hash { ItemState::Present } else { ItemState::Damaged })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ItemState::Missing),
            Err(e) => Err(io_err(&path)(e)),
        }
    }

    /// The path of the item, checked first: a missing or damaged item is an error, never a path to bad bytes.
    ///
    /// # Errors
    /// [`SetsError::Invalid`] naming the item when it is missing or damaged.
    pub fn verified_path(&self, hash: &ContentHash) -> Result<PathBuf, SetsError> {
        match self.state(hash)? {
            ItemState::Present => Ok(self.object_path(hash)),
            ItemState::Missing => Err(SetsError::Invalid(format!(
                "item {HASH_PREFIX}{hash} is not in the store at {}",
                self.root.display()
            ))),
            ItemState::Damaged => Err(SetsError::Invalid(format!(
                "item {HASH_PREFIX}{hash} is damaged: its bytes no longer match its hash"
            ))),
        }
    }

    /// The bytes of the item, checked against its name.
    ///
    /// # Errors
    /// As [`SetStore::verified_path`], and [`SetsError::Io`] for a read failure.
    pub fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, SetsError> {
        let path = self.verified_path(hash)?;
        let bytes = fs::read(&path).map_err(io_err(&path))?;
        // Read again, so bytes that changed between the check and the read are not handed out.
        if ContentHash::of(&bytes) == *hash {
            Ok(bytes)
        } else {
            Err(SetsError::Invalid(format!("item {HASH_PREFIX}{hash} changed while it was being read")))
        }
    }

    /// The item whose bytes have this SHA-256, if the store holds one and it is intact. `Ok(None)` for an item that is
    /// absent, or whose bytes do not match both its name and the pin. It is never deleted: `eval sets verify` reports it.
    ///
    /// # Errors
    /// [`SetsError::Io`] for a read failure other than a file not being there.
    pub fn find_sha256(&self, sha256: &PinSha256) -> Result<Option<(ContentHash, PathBuf)>, SetsError> {
        let index = self.index_path(sha256);
        let name = match fs::read_to_string(&index) {
            Ok(name) => name,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(&index)(e)),
        };
        let Ok(hash) = ContentHash::from_str(name.trim()) else { return Ok(None) };
        let path = self.object_path(&hash);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(&path)(e)),
        };
        let (found, found_sha, _) = hash_reader(file).map_err(io_err(&path))?;
        Ok((found == hash && found_sha == *sha256).then_some((hash, path)))
    }

    /// How the item is in the store and whether the SHA-256 index names it, from one pass over its bytes.
    ///
    /// # Errors
    /// [`SetsError::Io`] for a read failure other than the file not being there.
    pub fn inspect(&self, hash: &ContentHash) -> Result<Inspection, SetsError> {
        let path = self.object_path(hash);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Inspection { state: ItemState::Missing, indexed: false });
            }
            Err(e) => return Err(io_err(&path)(e)),
        };
        let (found, sha256, _) = hash_reader(file).map_err(io_err(&path))?;
        if found != *hash {
            return Ok(Inspection { state: ItemState::Damaged, indexed: false });
        }
        let indexed = self.find_sha256(&sha256)?.is_some_and(|(named, _)| named == *hash);
        Ok(Inspection { state: ItemState::Present, indexed })
    }
}

/// What [`SetStore::inspect`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inspection {
    /// How the item is in the store.
    pub state: ItemState,
    /// Whether the SHA-256 index names it (only an intact item can be indexed).
    pub indexed: bool,
}

/// Both hashes and the length of everything `reader` yields.
fn hash_reader(mut reader: impl Read) -> io::Result<(ContentHash, PinSha256, u64)> {
    let mut blake = blake3::Hasher::new();
    let mut sha = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; CHUNK];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        blake.update(chunk);
        sha.update(chunk);
        size += u64::try_from(read).unwrap_or(u64::MAX);
    }
    Ok((ContentHash(*blake.finalize().as_bytes()), PinSha256(sha.finalize().into()), size))
}

/// Copies `source` to `target` while hashing it.
fn copy_hashing(source: &Path, target: &Path) -> Result<(ContentHash, PinSha256, u64), SetsError> {
    let mut input = File::open(source).map_err(io_err(source))?;
    let mut output = File::create(target).map_err(io_err(target))?;
    let mut blake = blake3::Hasher::new();
    let mut sha = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; CHUNK];
    loop {
        let read = input.read(&mut buffer).map_err(io_err(source))?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        blake.update(chunk);
        sha.update(chunk);
        output.write_all(chunk).map_err(io_err(target))?;
        size += u64::try_from(read).unwrap_or(u64::MAX);
    }
    // Flushed to disk before the rename, so a power cut cannot leave the name there and the contents empty.
    output.sync_all().map_err(io_err(target))?;
    Ok((ContentHash(*blake.finalize().as_bytes()), PinSha256(sha.finalize().into()), size))
}

/// The hash of the file at `path`, without storing it.
///
/// # Errors
/// [`SetsError::Io`] when it cannot be read.
pub fn hash_file(path: &Path) -> Result<ContentHash, SetsError> {
    let file = File::open(path).map_err(io_err(path))?;
    hash_reader(file).map(|(hash, _, _)| hash).map_err(io_err(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, SetStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SetStore::new(dir.path().join("sets"));
        (dir, store)
    }

    #[test]
    fn put_then_get_round_trips_and_stores_identical_items_once() {
        let (_dir, store) = store();
        let stored = store.put(b"a synthetic sheet").unwrap();
        assert_eq!(stored.hash, ContentHash::of(b"a synthetic sheet"));
        assert_eq!(stored.size, 17);
        assert_eq!(store.get(&stored.hash).unwrap(), b"a synthetic sheet");
        assert_eq!(store.put(b"a synthetic sheet").unwrap(), stored);
        let objects: Vec<_> = fs::read_dir(store.root().join("objects")).unwrap().collect();
        assert_eq!(objects.len(), 1, "same bytes, one object, no staging leftovers");
        assert_eq!(store.state(&stored.hash).unwrap(), ItemState::Present);
    }

    #[test]
    fn a_missing_item_a_damaged_one_and_a_repaired_one_are_told_apart() {
        let (_dir, store) = store();
        let stored = store.put(b"clip").unwrap();
        let ghost = ContentHash::of(b"never stored");
        assert_eq!(store.state(&ghost).unwrap(), ItemState::Missing);
        fs::write(store.object_path(&stored.hash), b"clip, but cut short by a crash").unwrap();
        assert_eq!(store.state(&stored.hash).unwrap(), ItemState::Damaged);
        let error = store.get(&stored.hash).unwrap_err().to_string();
        assert!(error.contains("damaged"), "{error}");
        assert!(store.get(&ghost).unwrap_err().to_string().contains("not in the store"));
        // Storing the right bytes again replaces the damaged object.
        store.put(b"clip").unwrap();
        assert_eq!(store.get(&stored.hash).unwrap(), b"clip");
    }

    #[test]
    fn an_item_is_found_by_its_sha256_only_while_it_is_intact() {
        let (_dir, store) = store();
        let stored = store.put(b"read speech").unwrap();
        let (hash, path) = store.find_sha256(&stored.sha256).unwrap().unwrap();
        assert_eq!((hash, path), (stored.hash, store.object_path(&stored.hash)));
        assert!(store.inspect(&stored.hash).unwrap().indexed);
        assert_eq!(store.find_sha256(&PinSha256([7; 32])).unwrap(), None);
        fs::write(store.object_path(&stored.hash), b"tampered").unwrap();
        assert_eq!(store.find_sha256(&stored.sha256).unwrap(), None);
        assert_eq!(store.inspect(&stored.hash).unwrap(), Inspection { state: ItemState::Damaged, indexed: false });
        assert!(store.object_path(&stored.hash).exists(), "a damaged item is reported, never deleted");
    }

    #[test]
    fn a_manifest_hash_is_b3_and_sixty_four_lowercase_hex_digits() {
        let hash = ContentHash::of(b"x");
        assert_eq!(parse_hash(&format!("b3:{hash}")).unwrap(), hash);
        for bad in [
            "",
            "b3:",
            "b3:aa",
            &hash.to_string(),
            &format!("B3:{hash}"),
            &format!("b3:{}", hash.to_string().to_uppercase()),
        ] {
            assert!(matches!(parse_hash(bad), Err(SetsError::BadHash(_))), "{bad}");
        }
    }

    #[test]
    fn the_hash_of_a_file_matches_the_hash_of_its_bytes() {
        let (dir, _) = store();
        let path = dir.path().join("f.bin");
        fs::write(&path, vec![9_u8; 300_000]).unwrap();
        assert_eq!(hash_file(&path).unwrap(), ContentHash::of(&vec![9_u8; 300_000]));
        assert!(matches!(hash_file(&dir.path().join("none")), Err(SetsError::Io { .. })));
    }
}
