//! The file operations the journal writer makes (journal.md 5.2 and 12): one seam, so tests can count them, fail them
//! or simulate a power cut under them, and so each platform flushes the way it should.
//!
//! [`StdFs`] is the real one. Only the writer and its preparer go through it; the reader, recovery and compaction read
//! and rename sealed segments with `std::fs`, because nothing they do is on the commit path.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// How hard a flush pushes the data down (journal.md 5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Flush {
    /// The data is durable when it returns: `fdatasync` on Linux, `FlushFileBuffers` on Windows, `F_FULLFSYNC` on
    /// Apple. Batches a `sync` caller waits on, prepared segments, and the clean close.
    Full,
    /// An ordering barrier: the drive writes this data after everything before it, but may still hold it in its cache.
    /// `F_BARRIERFSYNC` on Apple, where the OS has it; everywhere else it is the same as [`Flush::Full`]. Batches
    /// nobody external waits on (the person's own interactive edits).
    Barrier,
}

/// The file operations the writer makes. Implementations must be safe to call from the writer and the preparer
/// threads at once (each on its own files).
pub trait JournalFs: Send + Sync {
    /// Creates `dir` and its parents.
    ///
    /// # Errors
    /// As [`std::fs::create_dir_all`].
    fn create_dir_all(&self, dir: &Path) -> io::Result<()>;

    /// Creates the file `path`, which must not exist, for writing.
    ///
    /// # Errors
    /// [`io::ErrorKind::AlreadyExists`] if it exists, or any I/O error.
    fn create_new(&self, path: &Path) -> io::Result<Box<dyn JournalFile>>;

    /// Opens the existing file `path` for writing (a prepared segment a session reuses).
    ///
    /// # Errors
    /// As [`OpenOptions::open`].
    fn open_write(&self, path: &Path) -> io::Result<Box<dyn JournalFile>>;

    /// Makes the entries made in `dir` durable. Windows has no folder flush; there it does nothing.
    ///
    /// # Errors
    /// Any I/O error.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;

    /// The names of the entries in `dir`; [`io::ErrorKind::NotFound`] if it doesn't exist.
    ///
    /// # Errors
    /// Any I/O error.
    fn list(&self, dir: &Path) -> io::Result<Vec<String>>;

    /// Up to `len` bytes from the start of `path`.
    ///
    /// # Errors
    /// Any I/O error.
    fn read_start(&self, path: &Path, len: usize) -> io::Result<Vec<u8>>;

    /// Whether a [`Flush::Barrier`] makes the data durable here (everywhere but Apple with `F_BARRIERFSYNC`). When it
    /// doesn't, `sync` waits for a [`Flush::Full`].
    fn barrier_is_durable(&self) -> bool;
}

/// One segment open for writing.
pub trait JournalFile: Send {
    /// Writes all of `bytes` at `offset`.
    ///
    /// # Errors
    /// Any I/O error; part of the bytes may have been written.
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()>;

    /// Flushes what was written, as `how` says.
    ///
    /// # Errors
    /// Any I/O error: nothing written since the last good flush is known to be durable.
    fn flush(&mut self, how: Flush) -> io::Result<()>;

    /// Cuts the file to `len` bytes (a failed append is cut back, so a retry never leaves a partial frame between two
    /// good ones).
    ///
    /// # Errors
    /// Any I/O error.
    fn set_len(&mut self, len: u64) -> io::Result<()>;
}

/// The real file system.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdFs;

impl JournalFs for StdFs {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)
    }

    fn create_new(&self, path: &Path) -> io::Result<Box<dyn JournalFile>> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Box::new(StdFile(file)))
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn JournalFile>> {
        let file = OpenOptions::new().write(true).open(path)?;
        Ok(Box::new(StdFile(file)))
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        if cfg!(unix) { File::open(dir)?.sync_all() } else { Ok(()) }
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        Ok(fs::read_dir(dir)?.filter_map(Result::ok).filter_map(|e| e.file_name().into_string().ok()).collect())
    }

    fn read_start(&self, path: &Path, len: usize) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(len);
        File::open(path)?.take(len as u64).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn barrier_is_durable(&self) -> bool {
        !barrier::AVAILABLE
    }
}

struct StdFile(File);

impl JournalFile for StdFile {
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.0.seek(SeekFrom::Start(offset))?;
        self.0.write_all(bytes)
    }

    fn flush(&mut self, how: Flush) -> io::Result<()> {
        match how {
            // On Apple, std's `sync_data` is `F_FULLFSYNC`.
            Flush::Full => self.0.sync_data(),
            Flush::Barrier => barrier::flush(&self.0),
        }
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }
}

/// `F_BARRIERFSYNC` where the OS has it.
#[cfg(target_vendor = "apple")]
mod barrier {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    /// Whether a barrier is a real barrier here, not a full flush.
    pub(super) const AVAILABLE: bool = true;

    /// `fcntl(fd, F_BARRIERFSYNC)`, falling back to a full flush where the file system refuses it.
    #[allow(unsafe_code, reason = "fcntl has no safe wrapper for F_BARRIERFSYNC; the fd is borrowed from a live File")]
    pub(super) fn flush(file: &File) -> io::Result<()> {
        // SAFETY: `file` is open for the whole call, so its descriptor is valid; F_BARRIERFSYNC takes no argument.
        let done = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) };
        if done == -1 { file.sync_data() } else { Ok(()) }
    }
}

/// Everywhere else a barrier is a full flush.
#[cfg(not(target_vendor = "apple"))]
mod barrier {
    use std::fs::File;
    use std::io;

    /// Whether a barrier is a real barrier here, not a full flush.
    pub(super) const AVAILABLE: bool = false;

    pub(super) fn flush(file: &File) -> io::Result<()> {
        file.sync_data()
    }
}
