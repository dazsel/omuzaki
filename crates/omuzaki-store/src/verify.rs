//! Whole-store inspection and repair: [`DiskStore::verify`] reports what is wrong and changes nothing,
//! [`DiskStore::repair`] moves what it can't trust into a quarantine folder and never deletes.
//!
//! Why quarantine instead of delete: a damaged object may still hold the only copy of someone's work (a flipped bit
//! in a render is better than no render), and a wrong guess about "stale" must be undoable. Moving a directory is
//! atomic and cheap, and once the damaged object is out of the way a `put` of the same bytes stores a fresh copy
//! (`put` skips an object that is there, even a damaged one, so without the move a broken directory would block its
//! hash for good).
//!
//! # What counts as a problem
//! - the bytes in `data` no longer hash to the object's name ([`Problem::Corrupt`]);
//! - an object directory without its `data` ([`Problem::MissingData`]) or without a readable
//!   `provenance.json` ([`Problem::MissingProvenance`], [`Problem::BadProvenance`]): the licence gate has nothing to
//!   read, so the asset must never be used. `DiskStore::put` can't produce this since the directory-commit layout;
//!   only a crash of an older store version or an outside edit can;
//! - a `.tmp-*` staging directory nobody has touched for longer than `stale_after` ([`Problem::StaleStaging`]):
//!   a crashed `put`. A younger one belongs to a live writer and is left alone, so `stale_after` must be longer than
//!   the slowest write you allow ([`DEFAULT_STALE_AFTER`]);
//! - things the store can't classify: files or folders that don't fit the layout ([`Problem::Stray`]) and objects
//!   the disk won't read ([`Problem::Unreadable`]). Reported, never moved: they may belong to another tool, or
//!   need a human (a failing disk, permissions).
//!
//! Verification reads every object into memory one at a time, like `get`; streaming is not supported yet.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use omuzaki_store_api::{ContentHash, Provenance, StoreError};

use crate::atomic::{STAGING_PREFIX, sync_dir};
use crate::gc;
use crate::{DiskStore, io_error};

/// A staging directory untouched for this long is a crashed `put`: live writers finish in seconds, so an hour leaves
/// room for a very large asset on a slow disk.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_hours(1);

/// One thing [`DiskStore::verify`] found wrong. An asset with several faults reports the first in the order data,
/// provenance, hash: the repair for all of them is the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The bytes no longer hash to the object's name (disk corruption or tampering).
    Corrupt(ContentHash),
    /// The object's directory has no `data` file.
    MissingData(ContentHash),
    /// The object has no `provenance.json`, so the licence gate can't clear it.
    MissingProvenance(ContentHash),
    /// `provenance.json` exists but isn't a valid provenance record.
    BadProvenance {
        /// The affected asset.
        hash: ContentHash,
        /// What the parser said.
        reason: String,
    },
    /// A file of the object couldn't be read (a failing disk, permissions). Not repaired: needs a human.
    Unreadable {
        /// The affected asset.
        hash: ContentHash,
        /// The failing read and the OS message.
        reason: String,
    },
    /// A `.tmp-*` staging directory left by a crashed `put`.
    StaleStaging {
        /// The directory.
        path: PathBuf,
        /// How long nothing in it has changed.
        idle: Duration,
    },
    /// Something under `objects/` that isn't part of the layout. Not repaired: it may belong to another tool.
    Stray(PathBuf),
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt(hash) => write!(f, "asset {hash}: its bytes no longer match the hash"),
            Self::MissingData(hash) => write!(f, "asset {hash}: the data file is missing"),
            Self::MissingProvenance(hash) => {
                write!(f, "asset {hash}: the provenance file is missing, so the licence gate can't clear it")
            }
            Self::BadProvenance { hash, reason } => {
                write!(f, "asset {hash}: the provenance file is not a valid record ({reason})")
            }
            Self::Unreadable { hash, reason } => write!(f, "asset {hash}: {reason}"),
            Self::StaleStaging { path, idle } => {
                write!(f, "{}: leftover of a crashed put, untouched for {}s", path.display(), idle.as_secs())
            }
            Self::Stray(path) => write!(f, "{}: not part of the store layout", path.display()),
        }
    }
}

/// What [`DiskStore::verify`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    /// How many object directories were examined (broken ones included).
    pub checked: usize,
    /// Every problem, in a fixed order: by shard, then by name.
    pub problems: Vec<Problem>,
}

impl VerifyReport {
    /// True if nothing is wrong.
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }
}

/// A problem [`DiskStore::repair`] fixed, and where the damaged directory went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quarantined {
    /// What was wrong with it.
    pub problem: Problem,
    /// Where it is now, inside `<root>/quarantine/`. Move it back to undo the repair.
    pub moved_to: PathBuf,
}

/// What [`DiskStore::repair`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepairReport {
    /// How many object directories were examined.
    pub checked: usize,
    /// What was moved to quarantine.
    pub quarantined: Vec<Quarantined>,
    /// Problems repair does not touch ([`Problem::Unreadable`], [`Problem::Stray`]).
    pub left_alone: Vec<Problem>,
}

/// One entry of a directory.
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

/// The entries of `dir`, sorted by name so a report comes out the same on every run and platform.
fn sorted_entries(dir: &Path) -> Result<Vec<Entry>, StoreError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(io_error("list", dir))? {
        let entry = entry.map_err(io_error("list", dir))?;
        // `file_type` doesn't follow symlinks, so a link to a directory is a stray, never something we recurse into.
        let is_dir = entry.file_type().map_err(io_error("inspect", &entry.path()))?.is_dir();
        // A name that isn't UTF-8 gets replacement characters, which match no layout name: it ends up a stray.
        entries.push(Entry { name: entry.file_name().to_string_lossy().into_owned(), path: entry.path(), is_dir });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// True for the two lowercase hex digits that name a shard directory.
fn is_shard_name(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// How long ago anything in the staging directory last changed, or `None` if the directory is gone because its writer
/// just finished. The newest of the directory's and its files' times counts: a long write to `data` updates the file,
/// not the directory.
fn idle_time(staging: &Path, now: SystemTime) -> io::Result<Option<Duration>> {
    let newest = || -> io::Result<SystemTime> {
        let mut newest = fs::metadata(staging)?.modified()?;
        for file in fs::read_dir(staging)? {
            newest = newest.max(file?.metadata()?.modified()?);
        }
        Ok(newest)
    };
    match newest() {
        // A clock set back since the write makes the file "from the future": treat it as just written.
        Ok(newest) => Ok(Some(now.duration_since(newest).unwrap_or_default())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A name for `source` that is unique within one quarantine batch: its path below `objects/`, with `/` flattened.
fn quarantine_name(objects: &Path, source: &Path) -> String {
    let relative = source.strip_prefix(objects).unwrap_or(source);
    relative.components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("-")
}

impl DiskStore {
    /// Checks the whole store and reports every [`Problem`], changing nothing. Re-hashes every object, so it takes as
    /// long as reading the store. Safe to run while other threads or processes `put`: it only reads committed
    /// objects, and leaves staging directories younger than `stale_after` alone.
    ///
    /// # Errors
    /// [`StoreError::Io`] if a directory of the store can't be listed.
    pub fn verify(&self, stale_after: Duration) -> Result<VerifyReport, StoreError> {
        let now = SystemTime::now();
        let mut report = VerifyReport::default();
        for shard in sorted_entries(&self.objects)? {
            if !(shard.is_dir && is_shard_name(&shard.name)) {
                report.problems.push(Problem::Stray(shard.path));
                continue;
            }
            for entry in sorted_entries(&shard.path)? {
                self.scan_shard_entry(&shard.name, entry, now, stale_after, &mut report)?;
            }
        }
        Ok(report)
    }

    /// Classifies one entry of a shard directory: an object to check, a staging directory to age, or a stray.
    fn scan_shard_entry(
        &self,
        shard: &str,
        entry: Entry,
        now: SystemTime,
        stale_after: Duration,
        report: &mut VerifyReport,
    ) -> Result<(), StoreError> {
        if !entry.is_dir {
            report.problems.push(Problem::Stray(entry.path));
        } else if entry.name.starts_with(STAGING_PREFIX) {
            let idle = idle_time(&entry.path, now).map_err(io_error("inspect", &entry.path))?;
            if let Some(idle) = idle.filter(|idle| *idle >= stale_after) {
                report.problems.push(Problem::StaleStaging { path: entry.path, idle });
            }
        } else if let Ok(hash) = format!("{shard}{}", entry.name).parse::<ContentHash>() {
            report.checked += 1;
            report.problems.extend(self.check_asset(hash));
        } else {
            report.problems.push(Problem::Stray(entry.path));
        }
        Ok(())
    }

    /// The first fault of one object directory, if any: cheap checks (files present, provenance parses) before the
    /// expensive one (re-hashing the data).
    fn check_asset(&self, hash: ContentHash) -> Option<Problem> {
        let paths = self.paths(&hash);
        let unreadable =
            |path: &Path, e: io::Error| Problem::Unreadable { hash, reason: format!("read {}: {e}", path.display()) };
        if !paths.data.is_file() {
            return Some(Problem::MissingData(hash));
        }
        match fs::read(&paths.provenance) {
            Ok(raw) => {
                if let Err(e) = serde_json::from_slice::<Provenance>(&raw) {
                    return Some(Problem::BadProvenance { hash, reason: e.to_string() });
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(Problem::MissingProvenance(hash)),
            Err(e) => return Some(unreadable(&paths.provenance, e)),
        }
        match fs::read(&paths.data) {
            Ok(bytes) if ContentHash::of(&bytes) == hash => None,
            Ok(_) => Some(Problem::Corrupt(hash)),
            Err(e) => Some(unreadable(&paths.data, e)),
        }
    }

    /// Verifies the store (see [`DiskStore::verify`]) and moves every repairable problem into a new folder
    /// `<root>/quarantine/repair-NNNN/`, named after where it was (`ab-cdef…` for an object, `ab-.tmp-…` for a
    /// staging directory). Nothing is deleted, and nothing is ever deleted from the quarantine either: that is for
    /// the user, once they are sure. A damaged object leaves the store entirely, provenance included, so a later
    /// `put` of the same bytes records the provenance it is given.
    ///
    /// Safe to repeat: a second run finds nothing to do, and if a failure stops a run part-way, running again
    /// finishes it. Don't run two repairs at once on one store with the same bytes being `put`: a `put` that
    /// checked an object just before its quarantine reports success without storing it (the same state `get`
    /// already reported as corrupt), and the next `verify` or `put` sorts it out.
    ///
    /// The quarantine must be on the same filesystem as `objects/`, which it is unless one was mounted separately.
    ///
    /// # Errors
    /// [`StoreError::Io`] naming the path, if a directory can't be listed or moved.
    pub fn repair(&self, stale_after: Duration) -> Result<RepairReport, StoreError> {
        let VerifyReport { checked, problems } = self.verify(stale_after)?;
        let mut movable = Vec::new();
        let mut left_alone = Vec::new();
        for problem in problems {
            match self.repair_source(&problem) {
                Some(source) => movable.push((source, problem)),
                None => left_alone.push(problem),
            }
        }

        let mut quarantined = Vec::new();
        // The batch folder is made only when there is something to move, so a clean store gains no folder.
        if !movable.is_empty() {
            let batch = self.new_quarantine_batch()?;
            for (source, problem) in movable {
                let moved_to = batch.join(quarantine_name(&self.objects, &source));
                if move_aside(&source, &moved_to)? {
                    quarantined.push(Quarantined { problem, moved_to });
                }
            }
        }
        Ok(RepairReport { checked, quarantined, left_alone })
    }

    /// The directory that holds `problem`, if repair moves it.
    fn repair_source(&self, problem: &Problem) -> Option<PathBuf> {
        match problem {
            Problem::Corrupt(hash)
            | Problem::MissingData(hash)
            | Problem::MissingProvenance(hash)
            | Problem::BadProvenance { hash, .. } => Some(self.paths(hash).dir),
            // A retired object is GC's own staging: its contents were judged garbage, so the next sweep reclaims it
            // rather than quarantining it.
            Problem::StaleStaging { path, .. } if !gc::is_retired(path) => Some(path.clone()),
            Problem::StaleStaging { .. } | Problem::Unreadable { .. } | Problem::Stray(_) => None,
        }
    }

    /// Creates the next unused `repair-NNNN` folder inside the quarantine. Exclusive creation keeps two repairs from
    /// sharing a batch.
    fn new_quarantine_batch(&self) -> Result<PathBuf, StoreError> {
        fs::create_dir_all(&self.quarantine).map_err(io_error("create", &self.quarantine))?;
        let mut n = 0_u32;
        loop {
            let batch = self.quarantine.join(format!("repair-{n:04}"));
            match fs::create_dir(&batch) {
                Ok(()) => return Ok(batch),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(io_error("create", &batch)(e)),
            }
        }
    }
}

/// Renames `source` to `target` and flushes both directories, so the move survives a power cut. Returns false if
/// `source` was already gone (another repair moved it first).
fn move_aside(source: &Path, target: &Path) -> Result<bool, StoreError> {
    match fs::rename(source, target) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(io_error("quarantine", source)(e)),
    }
    for dir in [target.parent(), source.parent()].into_iter().flatten() {
        sync_dir(dir).map_err(io_error("sync", dir))?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
