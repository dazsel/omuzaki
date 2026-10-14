//! The file operations staging and the launcher use. `omuzaki-update-fs` implements them over `std::fs`;
//! `omuzaki-update-mock` has an in-memory `FakeDisk` that fails on cue. Holding no I/O here keeps the crate pure.

use std::io;
use std::path::{Path, PathBuf};

/// One entry of [`Disk::list_dir`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// The file name, without its folder.
    pub name: String,
    /// A directory (not followed through a link).
    pub is_dir: bool,
    /// A symbolic link.
    pub is_symlink: bool,
}

/// File operations. Paths are absolute.
pub trait Disk: Send + Sync {
    /// The whole file.
    ///
    /// # Errors
    /// The file is missing or unreadable.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Creates or truncates the file. Not durable until [`Disk::fsync`].
    ///
    /// # Errors
    /// The file cannot be written.
    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// Makes a file or a directory durable.
    ///
    /// # Errors
    /// The path is missing or the sync failed.
    fn fsync(&self, path: &Path) -> io::Result<()>;
    /// Renames, replacing a file at `to`.
    ///
    /// # Errors
    /// `from` is missing or the rename failed.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Creates a directory and its parents.
    ///
    /// # Errors
    /// A part of the path is a file, or the folder cannot be made.
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    /// Removes a directory and everything in it.
    ///
    /// # Errors
    /// The directory is missing or cannot be removed.
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
    /// Whether anything is at `path`.
    fn exists(&self, path: &Path) -> bool;
    /// The entries of a directory, sorted by name.
    ///
    /// # Errors
    /// The directory is missing or unreadable.
    fn list_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;
    /// Makes a symbolic link at `link` pointing at `target`.
    ///
    /// # Errors
    /// `link` exists or cannot be made.
    fn symlink(&self, target: &str, link: &Path) -> io::Result<()>;
    /// Sets or clears the executable bit (a no-op where there is none).
    ///
    /// # Errors
    /// The file is missing.
    fn set_executable(&self, path: &Path, on: bool) -> io::Result<()>;
    /// Free bytes on the volume holding `path`.
    ///
    /// # Errors
    /// The volume cannot be queried.
    fn free_bytes(&self, path: &Path) -> io::Result<u64>;

    /// Replaces `path` whole or not at all: write `<path>.tmp`, fsync it, rename it over `path`, fsync the folder.
    ///
    /// # Errors
    /// Any step failed; `path` then holds its old content.
    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        self.write(&tmp, bytes)?;
        self.fsync(&tmp)?;
        self.rename(&tmp, path)?;
        match path.parent() {
            Some(dir) => self.fsync(dir),
            None => Ok(()),
        }
    }
}
