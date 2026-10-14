//! Content-addressed storage for golden images: `<store>/objects/<hash>.png`.
//!
//! The same idea as the asset store (`omuzaki-store-api`, spec v3 §7): an image is named by the BLAKE3 hash of its bytes,
//! so identical images are stored once, a name can never point at the wrong picture, and reading one back verifies it.
//! The store is a plain folder in the assets area, shared by every branch; branches differ only in which hashes their manifests
//! (in git) point at, so blessing on one branch can't change what another compares against.
//!
//! Objects are never deleted here, and an intact one is never rewritten: an old branch's manifest must keep finding its
//! images. Only a damaged object (missing, or not hashing to its name) is written again. The one thing that removes
//! objects is `omuzaki-test gc` ([`crate::gc`]), which spares anything stored or re-stored in the last week.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use omuzaki_store_api::ContentHash;

use crate::error::{GoldenError, io_err};
use crate::fsio::write_atomic;

/// Folder inside the store root that holds the images.
const OBJECTS_DIR: &str = "objects";

/// The hash an object file at `path` is named after (`<64 hex>.png`), or `None` for any other name.
pub(crate) fn object_hash(path: &Path) -> Option<ContentHash> {
    let named_png = path.extension().is_some_and(|ext| ext == "png");
    named_png.then(|| path.file_stem()?.to_str()?.parse().ok()).flatten()
}

/// The image objects under one store root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectStore {
    dir: PathBuf,
}

impl ObjectStore {
    /// The objects of the store rooted at `store_root` (nothing is created until the first [`ObjectStore::put`]).
    pub fn new(store_root: &Path) -> Self {
        Self { dir: store_root.join(OBJECTS_DIR) }
    }

    /// The folder holding the objects.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the image with `hash` is, or would be.
    pub fn path(&self, hash: &ContentHash) -> PathBuf {
        self.dir.join(format!("{hash}.png"))
    }

    /// Stores `bytes` (a PNG) and returns their hash. Storing bytes that are already there leaves them as they are and
    /// only renews the file's modification time: that time is "last chosen as a golden", which is what lets `gc` spare
    /// an old image that a bless has just picked again but whose manifest isn't written yet.
    ///
    /// A file that exists under the name but doesn't hash to it (cut short by a crash, or tampered with) is not "already
    /// there": it is replaced, atomically. That is what lets `--bless` repair a damaged object instead of leaving it
    /// permanently broken.
    ///
    /// # Errors
    /// [`GoldenError::Io`] when the object can't be written.
    pub fn put(&self, bytes: &[u8]) -> Result<ContentHash, GoldenError> {
        let hash = ContentHash::of(bytes);
        let path = self.path(&hash);
        // Any read failure counts as damaged: the write below then either repairs it or reports the real problem.
        let intact = fs::read(&path).is_ok_and(|existing| ContentHash::of(&existing) == hash);
        if intact {
            touch(&path);
        } else {
            write_atomic(&path, bytes)?;
        }
        Ok(hash)
    }

    /// The bytes stored under `hash`, checked against it.
    ///
    /// # Errors
    /// [`GoldenError::ObjectMissing`] when there is no such object, [`GoldenError::ObjectCorrupt`] when its bytes no
    /// longer hash to its name, [`GoldenError::Io`] for any other read failure.
    pub fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, GoldenError> {
        let path = self.path(hash);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(GoldenError::ObjectMissing { hash: *hash, path });
            }
            Err(e) => return Err(io_err(&path)(e)),
        };
        if ContentHash::of(&bytes) == *hash { Ok(bytes) } else { Err(GoldenError::ObjectCorrupt { hash: *hash, path }) }
    }
}

/// Sets the modification time of `path` to now, without touching its bytes. Best effort: on a read-only store the
/// object stays as old as it was, which only matters to a `gc` run racing a bless, and the bless itself still works.
fn touch(path: &Path) {
    if let Ok(file) = fs::File::options().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_round_trips_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(tmp.path());
        let hash = store.put(b"picture").unwrap();
        assert_eq!(store.path(&hash), tmp.path().join("objects").join(format!("{hash}.png")));
        assert_eq!(store.get(&hash).unwrap(), b"picture");
        assert_eq!(store.put(b"picture").unwrap(), hash);
        assert_eq!(fs::read_dir(store.dir()).unwrap().count(), 1, "same bytes, one object, no staging leftovers");
    }

    #[test]
    fn a_missing_object_and_a_damaged_one_are_told_apart() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(tmp.path());
        let hash = ContentHash::of(b"never stored");
        assert!(matches!(store.get(&hash), Err(GoldenError::ObjectMissing { .. })));

        let stored = store.put(b"original").unwrap();
        fs::write(store.path(&stored), b"tampered").unwrap();
        assert!(matches!(store.get(&stored), Err(GoldenError::ObjectCorrupt { .. })));
    }

    #[test]
    fn put_repairs_an_object_that_no_longer_hashes_to_its_name() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(tmp.path());
        let hash = store.put(b"picture").unwrap();
        for damage in [&b""[..], b"pic", b"tampered with"] {
            fs::write(store.path(&hash), damage).unwrap();
            assert!(matches!(store.get(&hash), Err(GoldenError::ObjectCorrupt { .. })));
            assert_eq!(store.put(b"picture").unwrap(), hash);
            assert_eq!(store.get(&hash).unwrap(), b"picture", "{damage:?} was replaced");
        }
        fs::remove_file(store.path(&hash)).unwrap();
        store.put(b"picture").unwrap();
        assert_eq!(store.get(&hash).unwrap(), b"picture");
        assert_eq!(fs::read_dir(store.dir()).unwrap().count(), 1, "no staging file left behind");
    }

    #[test]
    fn an_intact_object_keeps_its_bytes_and_is_marked_as_used_again() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(tmp.path());
        let hash = store.put(b"picture").unwrap();
        let month_ago = SystemTime::now() - std::time::Duration::from_hours(30 * 24);
        fs::File::options().write(true).open(store.path(&hash)).unwrap().set_modified(month_ago).unwrap();

        store.put(b"picture").unwrap();
        let modified = fs::metadata(store.path(&hash)).unwrap().modified().unwrap();
        assert!(modified > month_ago + std::time::Duration::from_hours(29 * 24), "re-storing renews the age");
        assert_eq!(store.get(&hash).unwrap(), b"picture");
        assert_eq!(fs::read_dir(store.dir()).unwrap().count(), 1, "no second copy, no staging file");
    }

    #[test]
    fn concurrent_puts_of_the_same_image_all_succeed() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(tmp.path());
        let hashes: Vec<_> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8).map(|_| scope.spawn(|| store.put(&[7u8; 4096]).unwrap())).collect();
            workers.into_iter().map(|w| w.join().unwrap()).collect()
        });
        assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(fs::read_dir(store.dir()).unwrap().count(), 1);
    }
}
