//! The verified file cache (ADR 0017): [`VerifiedCache`] implements [`FileSource`] over a [`Fetcher`] and a folder.
//!
//! Layout under the cache root: `sha256/<hex>` is a verified blob, `sha256/<hex>.rec` its `(size, mtime)` record,
//! `sha256/<hex>.partial` the kept part of an interrupted resumable download (one per pin, whichever URL it came
//! from) and `sha256/*.tmp` files being written. A finished `get` leaves only the blob and its record. Per-pin lock
//! files live apart, in `locks/`, so that one pin is downloaded by one thread or process at a time.

mod digest;
mod download;
mod record;
#[cfg(test)]
mod tests;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use omuzaki_ai_api::{FetchError, Fetcher, FileSource, PinnedFile, Progress, RESUME_MIN_SIZE, Sha256};
use sha2::{Digest, Sha256 as Hasher};

pub use digest::{model_ref, sha256_file};
use download::{Ended, Stop, Target, copy};
use record::Record;

/// The progress stage name downloads report under.
pub(crate) const STAGE: &str = "download";

/// A [`FileSource`] that downloads pinned files through a [`Fetcher`], verifies them and keeps them under a root
/// folder. Safe to share across threads and processes: files appear by atomic rename and are checked before that.
pub struct VerifiedCache<F> {
    root: PathBuf,
    fetcher: F,
    resume_min_size: u64,
}

impl<F: Fetcher> VerifiedCache<F> {
    /// A cache in `root` (created on first use) that downloads through `fetcher`.
    pub fn new(root: impl Into<PathBuf>, fetcher: F) -> Self {
        Self { root: root.into(), fetcher, resume_min_size: RESUME_MIN_SIZE }
    }

    /// Resumes downloads of pins of at least `bytes` instead of [`RESUME_MIN_SIZE`]. Tests use it to exercise
    /// resuming with small bodies.
    #[must_use]
    pub fn with_resume_min_size(mut self, bytes: u64) -> Self {
        self.resume_min_size = bytes;
        self
    }

    /// The fetcher this cache downloads through.
    pub fn fetcher(&self) -> &F {
        &self.fetcher
    }

    /// Downloads the pin from one URL into place.
    fn fetch_from(
        &self,
        url: &str,
        pin: &PinnedFile,
        paths: &Paths,
        progress: &dyn Progress,
    ) -> Result<(), FetchError> {
        if pin.size >= self.resume_min_size {
            self.fetch_resumable(url, pin, paths, progress)
        } else {
            self.fetch_whole(url, pin, paths, progress)
        }
    }

    /// A small file: streamed into a temporary file while hashing; nothing survives a failure.
    fn fetch_whole(
        &self,
        url: &str,
        pin: &PinnedFile,
        paths: &Paths,
        progress: &dyn Progress,
    ) -> Result<(), FetchError> {
        let mut tmp = tempfile::Builder::new()
            .prefix(&paths.hex)
            .suffix(".tmp")
            .tempfile_in(&paths.dir)
            .map_err(|e| io_err(&paths.dir, &e))?;
        let body = self.open(url, 0)?;
        let mut hasher = Hasher::new();
        let tmp_path = tmp.path().to_owned();
        let target =
            Target { file: tmp.as_file_mut(), path: &tmp_path, already: 0, size: pin.size, hasher: Some(&mut hasher) };
        let ended = copy(body.reader, target, progress, STAGE).map_err(|stop| stop_error(url, stop))?;
        check_length(url, pin, &ended)?;
        let actual = digest::finish(hasher);
        if actual != pin.sha256 {
            return Err(FetchError::Hash { url: url.to_owned(), expected: pin.sha256, actual });
        }
        tmp.as_file().sync_all().map_err(|e| io_err(&tmp_path, &e))?;
        if let Err(e) = tmp.persist(&paths.blob) {
            accept_existing(pin, paths, &e.error)?;
        }
        write_record(paths)
    }

    /// A large file: the partial file is kept across failures and restarts, then the whole file is hashed. A
    /// resumed file that fails the hash is downloaded once more from 0; a second mismatch is an error.
    fn fetch_resumable(
        &self,
        url: &str,
        pin: &PinnedFile,
        paths: &Paths,
        progress: &dyn Progress,
    ) -> Result<(), FetchError> {
        // A mismatch deletes the file and downloads once more from 0; a second mismatch is the answer.
        let mut retried = false;
        loop {
            let kept = fs::metadata(&paths.partial).map_or(0, |m| m.len());
            // More than the pin allows can't be a prefix of the file.
            let kept = if kept > pin.size {
                remove(&paths.partial);
                0
            } else {
                kept
            };
            if kept < pin.size {
                self.append_partial(url, pin, paths, kept, progress)?;
            }
            let actual = sha256_file(&paths.partial)?;
            if actual == pin.sha256 {
                return store_partial(pin, paths);
            }
            remove(&paths.partial);
            if retried {
                tracing::error!(%url, "two complete downloads do not match their pin");
                return Err(FetchError::Hash { url: url.to_owned(), expected: pin.sha256, actual });
            }
            tracing::warn!(%url, "a download does not match its pin; downloading it again from the start");
            retried = true;
        }
    }

    /// Fetches the rest of the partial file (`kept` bytes are there), or all of it if the server ignores ranges.
    /// On a read error what arrived stays in the partial file for the next attempt.
    fn append_partial(
        &self,
        url: &str,
        pin: &PinnedFile,
        paths: &Paths,
        kept: u64,
        progress: &dyn Progress,
    ) -> Result<(), FetchError> {
        let body = self.open(url, kept)?;
        let (file, already) = if body.start == kept {
            (OpenOptions::new().create(true).append(true).open(&paths.partial), kept)
        } else if body.start == 0 {
            (File::create(&paths.partial), 0)
        } else {
            return Err(net(url, &format!("sent bytes from {} when {kept} were asked for", body.start)));
        };
        let mut file = file.map_err(|e| io_err(&paths.partial, &e))?;
        let target = Target { file: &mut file, path: &paths.partial, already, size: pin.size, hasher: None };
        let ended = copy(body.reader, target, progress, STAGE).map_err(|stop| stop_error(url, stop))?;
        let result = check_length(url, pin, &ended);
        if matches!(ended, Ended::Overrun) {
            remove(&paths.partial);
        }
        result
    }

    fn open(&self, url: &str, from: u64) -> Result<omuzaki_ai_api::Body, FetchError> {
        self.fetcher.open(url, from)
    }
}

impl<F: Fetcher> FileSource for VerifiedCache<F> {
    fn get(&self, pin: &PinnedFile, progress: &dyn Progress) -> Result<PathBuf, FetchError> {
        let paths = Paths::new(&self.root, pin.sha256);
        // One thread or process per pin, from the first look at the cache: checking it can delete a bad blob, and an
        // unlocked check could delete the good one another caller has just published. The others wait here and then
        // find the blob; without the lock, resumable downloads would also append to the same partial file.
        let _lock = PinLock::acquire(&paths)?;
        if cached(pin.sha256, Some(pin.size), &paths)? {
            return Ok(paths.blob);
        }
        fs::create_dir_all(&paths.dir).map_err(|e| io_err(&paths.dir, &e))?;
        let mut errors = Vec::with_capacity(pin.urls.len());
        for url in &pin.urls {
            match self.fetch_from(url, pin, &paths, progress) {
                Ok(()) => return Ok(paths.blob),
                // Saying "stop" or a broken cache folder is the same at every URL: don't try the next.
                Err(e @ (FetchError::Cancelled | FetchError::Io { .. })) => return Err(e),
                Err(e) => {
                    tracing::warn!(%url, error = %e, "this source failed; trying the next");
                    errors.push(e);
                }
            }
        }
        Err(FetchError::AllFailed(errors))
    }

    fn lookup(&self, sha256: Sha256) -> Result<Option<PathBuf>, FetchError> {
        let paths = Paths::new(&self.root, sha256);
        // Nothing to check (and nothing to lock) when there is no blob: a lookup must not create cache folders.
        match fs::metadata(&paths.blob) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(&paths.blob, &e)),
        }
        // Under the pin's lock, as `get` is: a bad blob is deleted here, and that must not hit a good one `get` has
        // just published.
        let _lock = PinLock::acquire(&paths)?;
        Ok(cached(sha256, None, &paths)?.then_some(paths.blob))
    }

    fn forget(&self, sha256: Sha256) -> Result<(), FetchError> {
        let paths = Paths::new(&self.root, sha256);
        // Under the pin's lock, as `get` is: a download verifying this blob must not see it vanish part-way.
        let _lock = PinLock::acquire(&paths)?;
        delete(&paths.rec)?;
        delete(&paths.blob)
    }
}

/// The cached blob if it is there and valid; invalid blobs are deleted. A blob whose size and mtime match its
/// record is trusted; any other is hashed again and kept only if it still matches. `size` is the pin's length, or
/// `None` when only the hash is known: the hash check then stands alone, and it is the stronger test anyway.
/// An error other than "missing" is returned, never read as missing: the caller must not download over a blob it
/// could not inspect, nor report a blob as absent because its deletion failed.
fn cached(sha256: Sha256, size: Option<u64>, paths: &Paths) -> Result<bool, FetchError> {
    let meta = match fs::metadata(&paths.blob) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(io_err(&paths.blob, &e)),
    };
    if size.is_none_or(|size| meta.len() == size) {
        if Record::read(&paths.rec) == Some(Record::of(&meta)) {
            return Ok(true);
        }
        if sha256_file(&paths.blob)? == sha256 {
            write_record(paths)?;
            return Ok(true);
        }
    }
    tracing::warn!(path = %paths.blob.display(), "cached file does not match its pin; fetching it again");
    delete(&paths.blob)?;
    delete(&paths.rec)?;
    Ok(false)
}

/// Copies a local file into the cache under `root` (the folder a [`VerifiedCache`] over the same root reads), hashing
/// it while it copies, and stores it as a verified blob with its record, as a download would be: under the hash's
/// lock, by atomic rename, accepting the same verified blob another writer stored first. Returns its SHA-256 and size.
/// This is how a model the person adds enters the cache: its bytes are only known once they are read, so there is no
/// pin to [`get`](FileSource::get).
///
/// # Errors
/// [`FetchError::Io`] naming the cache file for a cache that can't be written; `source` errors are `Io` too, with the
/// path the caller gave (the caller words them); [`FetchError::Cancelled`] when `progress` says stop.
pub(crate) fn import(root: &Path, source: &Path, progress: &dyn Progress) -> Result<(Sha256, u64), FetchError> {
    let mut from = File::open(source).map_err(|e| io_err(source, &e))?;
    let dir = root.join("sha256");
    fs::create_dir_all(&dir).map_err(|e| io_err(&dir, &e))?;
    let mut tmp = tempfile::Builder::new().suffix(".tmp").tempfile_in(&dir).map_err(|e| io_err(&dir, &e))?;
    let tmp_path = tmp.path().to_owned();
    let (sha256, size) = digest::copy_hashing((&mut from, source), (tmp.as_file_mut(), &tmp_path), progress)?;
    let paths = Paths::new(root, sha256);
    tmp.as_file().sync_all().map_err(|e| io_err(tmp.path(), &e))?;
    let _lock = PinLock::acquire(&paths)?;
    // The same bytes may be there already (added before, or shipped): keep them. A blob there that fails its check is
    // deleted first, so a corrupted cache can't block an import where a rename doesn't overwrite (Windows).
    if !cached(sha256, Some(size), &paths)? {
        if let Err(e) = tmp.persist(&paths.blob) {
            accept_existing(&PinnedFile::new(std::iter::empty::<String>(), sha256, size), &paths, &e.error)?;
        }
        write_record(&paths)?;
    }
    Ok((sha256, size))
}

/// Where one pin's files live.
struct Paths {
    hex: String,
    dir: PathBuf,
    blob: PathBuf,
    rec: PathBuf,
    partial: PathBuf,
    lock: PathBuf,
}

impl Paths {
    fn new(root: &Path, sha256: Sha256) -> Self {
        let hex = sha256.to_string();
        let dir = root.join("sha256");
        Self {
            blob: dir.join(&hex),
            rec: dir.join(format!("{hex}.rec")),
            partial: dir.join(format!("{hex}.partial")),
            // Outside `sha256/`, which holds only blobs, records and partials. Never deleted, so two waiters can't
            // end up locking different files.
            lock: root.join("locks").join(&hex),
            dir,
            hex,
        }
    }
}

/// An exclusive lock on one pin's lock file, released on drop (or when the process dies).
struct PinLock(File);

impl PinLock {
    fn acquire(paths: &Paths) -> Result<Self, FetchError> {
        if let Some(parent) = paths.lock.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err(parent, &e))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&paths.lock)
            .map_err(|e| io_err(&paths.lock, &e))?;
        file.lock().map_err(|e| io_err(&paths.lock, &e))?;
        Ok(Self(file))
    }
}

impl Drop for PinLock {
    fn drop(&mut self) {
        // Closing the handle releases the lock anyway; unlocking now just does it sooner.
        let _ = self.0.unlock();
    }
}

/// Moves the verified partial file into place.
fn store_partial(pin: &PinnedFile, paths: &Paths) -> Result<(), FetchError> {
    // Opened for writing: Windows refuses to flush a read-only handle.
    let synced = OpenOptions::new().write(true).open(&paths.partial).and_then(|f| f.sync_all());
    synced.map_err(|e| io_err(&paths.partial, &e))?;
    if let Err(e) = fs::rename(&paths.partial, &paths.blob) {
        // Another writer stored the same verified blob first (Windows refuses to overwrite).
        accept_existing(pin, paths, &e)?;
        remove(&paths.partial);
    }
    write_record(paths)
}

/// After a failed rename: another writer may have stored the same blob first (Windows refuses to overwrite). That is
/// fine only if what is there is a regular file that really is the pin; anything else is the original `Io` error.
fn accept_existing(pin: &PinnedFile, paths: &Paths, rename_error: &io::Error) -> Result<(), FetchError> {
    let verified = fs::symlink_metadata(&paths.blob).is_ok_and(|m| m.is_file() && m.len() == pin.size)
        && sha256_file(&paths.blob).is_ok_and(|actual| actual == pin.sha256);
    if verified { Ok(()) } else { Err(io_err(&paths.blob, rename_error)) }
}

/// Records the blob's current size and mtime, atomically.
fn write_record(paths: &Paths) -> Result<(), FetchError> {
    let meta = fs::metadata(&paths.blob).map_err(|e| io_err(&paths.blob, &e))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(&paths.hex)
        .suffix(".tmp")
        .tempfile_in(&paths.dir)
        .map_err(|e| io_err(&paths.dir, &e))?;
    tmp.write_all(Record::of(&meta).to_text().as_bytes()).map_err(|e| io_err(&paths.rec, &e))?;
    tmp.persist(&paths.rec).map_err(|e| io_err(&paths.rec, &e.error))?;
    Ok(())
}

/// Refuses a body of the wrong length: longer is `TooLarge`, shorter is `Size`.
fn check_length(url: &str, pin: &PinnedFile, ended: &Ended) -> Result<(), FetchError> {
    match *ended {
        Ended::Overrun => Err(FetchError::TooLarge { url: url.to_owned(), limit: pin.size }),
        Ended::Eof { total } if total != pin.size => {
            Err(FetchError::Size { url: url.to_owned(), expected: pin.size, actual: total })
        }
        Ended::Eof { .. } => Ok(()),
    }
}

/// The error a stopped copy stands for.
fn stop_error(url: &str, stop: Stop) -> FetchError {
    match stop {
        Stop::Read(reason) => net(url, &reason),
        Stop::Cancelled => FetchError::Cancelled,
        Stop::Write(e) => e,
    }
}

fn net(url: &str, reason: &str) -> FetchError {
    FetchError::Network { url: url.to_owned(), reason: reason.to_owned() }
}

fn io_err(path: &Path, e: &io::Error) -> FetchError {
    FetchError::Io { path: path.display().to_string(), reason: e.to_string() }
}

/// Deletes a file we own; a missing file is the goal already. A failure is an error, for the callers that must know.
fn delete(path: &Path) -> Result<(), FetchError> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_err(path, &e)),
        _ => Ok(()),
    }
}

/// Deletes a file we own, logging a failure and leaving it for the next run.
fn remove(path: &Path) {
    if let Err(e) = delete(path) {
        tracing::warn!(error = %e, "could not delete a cache file");
    }
}
