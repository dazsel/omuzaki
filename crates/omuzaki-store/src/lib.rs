//! The asset store on disk (spec v3 §7): a content-addressed directory of immutable objects, each with its provenance. Implements [`omuzaki_store_api::Store`]; `omuzaki-store-mock` is the in-memory twin used in tests.
//!
//! # Layout
//! ```text
//! <root>/objects/ab/cdef…(62 hex)/data             the bytes
//! <root>/objects/ab/cdef…(62 hex)/provenance.json  the provenance, as JSON
//! <root>/objects/ab/cdef…(62 hex)/committed-at     when it was committed (written after publication; see gc.rs)
//! <root>/quarantine/repair-NNNN/…                  what [`DiskStore::repair`] moved aside
//! <root>/gc.lock                                   advisory lock: puts hold it shared, a removal exclusive
//! <root>/marker.lock                               advisory lock: held while a commit marker is published
//! ```
//! The first two hex digits shard the directory so no single directory grows huge.
//!
//! # Guarantees
//! - **Crash safety.** An object and its provenance are committed together: both files are written and fsynced in a
//!   `.tmp-*` staging directory, which is then renamed into place. A reader (or a crash) sees the whole asset or
//!   nothing, so an object can never exist without its provenance, and a put that fails part-way leaves no claim
//!   behind that a retry could inherit. A crash can only leave a stray `.tmp-*` directory, which nothing reads.
//! - **Concurrency.** Any number of threads or processes may `put` and `get` at once. Renaming onto an existing asset
//!   fails, so for identical bytes the first commit wins and every caller sees its provenance. No locks are held.
//! - **Verification.** `get` re-hashes what it read and reports [`StoreError::Corrupt`] on a mismatch.
//!   [`DiskStore::verify`] does the same for the whole store, and also finds objects missing their data or provenance
//!   and stale `.tmp-*` directories; [`DiskStore::repair`] moves those into the quarantine (it never deletes).
//!
//! # Limits
//! - `put` and `get` hold a whole asset in memory. Streaming multi-GB weights is not supported yet.
//! - `put` skips existing objects without re-reading them, so it does not repair a damaged one; `get` reports it.
//!   Run [`DiskStore::repair`]: once the damaged object is quarantined, a `put` of the same bytes stores a fresh copy.
#![deny(missing_docs)]

mod atomic;
mod gc;
mod lazy;
#[cfg(test)]
mod tests;
mod verify;

pub use lazy::LazyDiskStore;
pub use verify::{DEFAULT_STALE_AFTER, Problem, Quarantined, RepairReport, VerifyReport};

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use omuzaki_store_api::{ContentHash, Provenance, Store, StoreError};

use atomic::{WriteFile, publish_dir, sync_dir, write_file};

/// File name of an asset's bytes inside its directory.
const DATA_FILE: &str = "data";
/// File name of an asset's provenance inside its directory.
const PROVENANCE_FILE: &str = "provenance.json";

/// Wraps an I/O failure as a [`StoreError::Io`] that names the action and the path, so the message is actionable.
fn io_error<'a>(action: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> StoreError + 'a {
    move |e| StoreError::Io(format!("{action} {}: {e}", path.display()))
}

/// Where one asset lives on disk.
struct Paths {
    /// The two-hex-digit directory holding this asset's directory (and any staging directories).
    shard: PathBuf,
    /// The asset's directory: it exists only once the asset is complete.
    dir: PathBuf,
    data: PathBuf,
    provenance: PathBuf,
}

/// A content-addressed asset store rooted at a directory.
#[derive(Debug, Clone)]
pub struct DiskStore {
    objects: PathBuf,
    /// Where [`DiskStore::repair`] moves damaged objects: beside `objects/`, so a move is a rename.
    quarantine: PathBuf,
    /// The advisory lock file shared by `put` and removal (see `gc.rs`).
    lock_path: PathBuf,
    /// The advisory lock file that makes publishing a commit marker monotonic (see `gc.rs`).
    marker_lock_path: PathBuf,
    /// Writes each staged file. Always [`write_file`]; a field only so tests can inject a failing write.
    write: WriteFile,
    /// The time a commit is recorded at. Always the wall clock; a field only so tests can fix it.
    clock: fn() -> SystemTime,
    /// Called at each named step of a commit; an error stops the commit there, as a crash would. Always a no-op
    /// outside tests.
    failpoint: fn(&'static str) -> io::Result<()>,
    /// How long after its stamp a commit may finish before the stamp is redone (see `gc.rs`).
    stamp_slack: Duration,
    /// When each object committed in this process finished, as read after all of its I/O (see `gc.rs`). Shared by
    /// clones, since they share the store.
    completed: gc::Completed,
}

impl DiskStore {
    /// Opens the store at `root`, creating `<root>/objects` (and any missing parents) if needed. Opening an
    /// existing store changes nothing.
    ///
    /// # Errors
    /// [`StoreError::Io`], naming the path, if the directory can't be created.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref();
        let objects = root.join("objects");
        fs::create_dir_all(&objects).map_err(io_error("create", &objects))?;
        Ok(Self {
            objects,
            quarantine: root.join("quarantine"),
            lock_path: root.join(gc::LOCK_FILE),
            marker_lock_path: root.join(gc::MARKER_LOCK_FILE),
            write: write_file,
            clock: SystemTime::now,
            failpoint: gc::no_failpoint,
            stamp_slack: gc::DEFAULT_STAMP_SLACK,
            completed: gc::Completed::default(),
        })
    }

    /// Tells the store the grace period its garbage collection will use, so that another process sweeping the same
    /// directory never collects an object sooner than `0.75 × grace` after `put` returns. The default suits any grace
    /// of a few seconds or more; pass a shorter one here. For several processes on one store, use a grace of at least
    /// one second: the record is published by a rename that the scheduler can stall, which a shorter grace can't
    /// absorb. Within the process that wrote an object, the full grace always holds.
    #[must_use]
    pub fn with_gc_grace(mut self, grace: Duration) -> Self {
        self.stamp_slack = gc::stamp_slack_for(grace);
        self
    }

    fn paths(&self, hash: &ContentHash) -> Paths {
        let hex = hash.to_string();
        let (prefix, rest) = hex.split_at(2);
        let shard = self.objects.join(prefix);
        let dir = shard.join(rest);
        Paths { data: dir.join(DATA_FILE), provenance: dir.join(PROVENANCE_FILE), dir, shard }
    }

    /// Commits the provenance and the bytes together (see [`publish_dir`]) and makes the new directory entry durable.
    /// Returns whether this call published the object: if another put committed the same bytes first, its provenance
    /// stays and ours is dropped.
    fn write_asset(&self, paths: &Paths, bytes: &[u8], provenance: &Provenance) -> Result<bool, StoreError> {
        let record = serde_json::to_vec_pretty(provenance)
            .map_err(|e| StoreError::Io(format!("serialise provenance for {}: {e}", paths.dir.display())))?;

        fs::create_dir_all(&paths.shard).map_err(io_error("create", &paths.shard))?;
        // Always sync both levels, not only when this call made the shard: an earlier put that crashed after
        // creating it may never have synced its entry in `objects/`.
        sync_dir(&self.objects).map_err(io_error("sync", &self.objects))?;
        (self.failpoint)("before_publish").map_err(io_error("commit", &paths.dir))?;
        let published = publish_dir(&paths.dir, &[(PROVENANCE_FILE, &record), (DATA_FILE, bytes)], self.write)
            .map_err(io_error("write", &paths.dir))?;
        sync_dir(&paths.shard).map_err(io_error("sync", &paths.shard))?;
        Ok(published)
    }
}

impl Store for DiskStore {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        let hash = ContentHash::of(bytes);
        let paths = self.paths(&hash);
        // Shared, so puts never wait for each other; a removal waits for this one, and this one for a removal.
        let _shared = self.lock_store(gc::LockMode::Shared)?;
        // Without writing the bytes if the object is there: an existing asset already has the first provenance beside
        // it. Either way the object gets the commit time of this put, so it gets the same grace as a first one.
        let published = if paths.data.is_file() { false } else { self.write_asset(&paths, bytes, &provenance)? };
        self.finish_put(&hash, &paths, published)?;
        Ok(hash)
    }

    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        let path = self.paths(hash).data;
        let bytes = fs::read(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => StoreError::Missing(*hash),
            _ => io_error("read", &path)(e),
        })?;
        if ContentHash::of(&bytes) == *hash { Ok(bytes) } else { Err(StoreError::Corrupt(*hash)) }
    }

    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        let paths = self.paths(hash);
        if !paths.data.is_file() {
            return Err(StoreError::Missing(*hash));
        }
        let raw = fs::read(&paths.provenance).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => StoreError::Io(format!(
                "asset {hash} has no provenance at {}; the store was modified outside Omuzaki",
                paths.provenance.display()
            )),
            _ => io_error("read", &paths.provenance)(e),
        })?;
        serde_json::from_slice(&raw)
            .map_err(|e| StoreError::Io(format!("parse provenance {}: {e}", paths.provenance.display())))
    }

    /// True if the asset's data file exists. A path that can't be inspected (permissions) counts as absent.
    fn contains(&self, hash: &ContentHash) -> bool {
        self.paths(hash).data.is_file()
    }
}
