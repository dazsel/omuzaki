//! Crash-safe publishing of a whole directory, the building block `DiskStore::put` uses to commit an object and its
//! provenance in one step.

#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// What every staging directory's name starts with; [`crate::DiskStore::verify`] finds leftovers by it.
pub(crate) const STAGING_PREFIX: &str = ".tmp-";

/// Makes temp names unique within this process; the process id keeps them unique across processes.
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// How a file is written into the staging directory. `DiskStore` holds one so tests can make a write fail at the
/// moment a full disk or a file-size limit would; production always uses [`write_file`].
pub(crate) type WriteFile = fn(&Path, &[u8]) -> io::Result<()>;

/// A staging directory beside its destination, deleted on drop unless it was published. A failed delete is ignored:
/// a stray `.tmp-*` directory is harmless (nothing reads them) and must not mask the error that got us here.
struct StagingDir {
    path: PathBuf,
    published: bool,
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Creates an empty, exclusively owned staging directory in the same directory as `dst` (same directory means same
/// filesystem, which a rename needs).
fn create_staging_beside(dst: &Path) -> io::Result<StagingDir> {
    loop {
        let seq = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = dst.with_file_name(format!("{STAGING_PREFIX}{}-{seq}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(StagingDir { path, published: false }),
            // A crashed process with the same id left this name behind: take the next one.
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
}

/// Creates the new file `path` holding `bytes` and fsyncs it.
///
/// # Errors
/// Any I/O failure, including a file that already exists.
pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Atomically creates the directory `dst` holding `files` (name and contents), durably, unless `dst` already exists.
/// Returns whether this call created it.
///
/// The files are written and fsynced inside a staging directory next to `dst`, which is then renamed into place.
/// The rename is the commit: a reader or a crash sees the whole directory or none of it. `dst` is never empty once
/// it exists, so renaming onto it fails instead of replacing it: of two concurrent writers exactly one wins, and the
/// loser (told by `Ok(false)`) leaves the winner's content untouched. Any failure before the rename removes the
/// staging directory and leaves no trace at `dst`, so a failed write can never leave half of an asset behind.
///
/// Fsyncing the parent of `dst` afterwards (so the new entry survives a power cut) is the caller's job.
///
/// # Errors
/// Any I/O failure while staging, or a rename failure that isn't "`dst` already exists".
pub(crate) fn publish_dir(dst: &Path, files: &[(&str, &[u8])], write: WriteFile) -> io::Result<bool> {
    let mut staging = create_staging_beside(dst)?;
    for (name, bytes) in files {
        write(&staging.path.join(name), bytes)?;
    }
    sync_dir(&staging.path)?;
    match fs::rename(&staging.path, dst) {
        Ok(()) => {
            staging.published = true;
            Ok(true)
        }
        // The error kind for "target is a non-empty directory" differs between Unix and Windows, so ask the
        // filesystem instead: if `dst` is there, someone else committed first.
        Err(_) if dst.is_dir() => Ok(false),
        Err(e) => Err(e),
    }
}

/// Flushes the directory entries of `dir` to disk, so files linked into it survive a power cut.
///
/// # Errors
/// Any I/O failure.
#[cfg(unix)]
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Windows has no directory fsync: NTFS journals metadata changes itself, so there is nothing to flush.
#[cfg(not(unix))]
pub(crate) fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}
