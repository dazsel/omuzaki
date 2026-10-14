//! The verified open: how an op reads or writes the file [`OpCtx::fs_access`](crate::OpCtx::fs_access) approved.
//!
//! A [`CanonicalPath`] remembers the allowed folder it lies in (the project folder or a granted root) and the names
//! below it. Opening it opens that folder from the top of the file system, then each folder below it and the file,
//! refusing a symbolic link or a junction at every step: on Unix each one relative to the handle of the one above; on
//! Windows each by its full path, with every handle that is used checked to lie inside the allowed folder. So a file or
//! folder swapped for a link after the check makes the open fail (`Denied`); it cannot redirect it anywhere, inside
//! the folder or out. The platform halves are in `unix.rs` and `windows.rs`.
//!
//! What it does not change, by design (the threat model is in ADR 0013: file grants confine ops, they are not a
//! sandbox against processes that can already reach a file):
//! - **Hard links.** A hard link is not a link to a file but a second name of it, the same file under both names, and
//!   nothing on the disk says which name came first. Reading one reads the file. Writing in place
//!   ([`WriteMode::CreateOrTruncate`]) is refused for a file with more than one name, since it would change the file
//!   under its other names too; [`CanonicalPath::replace`] writes a new file under this name and leaves the others
//!   alone. That guard reads the file system's link count, which drvfs and 9p (`/mnt/*` under WSL) always report as
//!   1, so there it cannot see a hard link.
//! - **Mount points differ by platform.** On Linux and macOS a mount point (a bind mount, say) is a folder like any
//!   other, so the walk goes into it; on Windows a volume mount point is a junction, and is refused.
//! - **A folder moved while it is being opened.** The open holds each folder by its handle, so if one is renamed to
//!   somewhere else midway, the file is opened in that folder where it now is. Moving a folder out needs write access
//!   to both places, so this gives nobody a file they could not reach already.
//! - **Windows, during a race.** Win32 has no handle-relative open, so a link swapped in while the walk is under way
//!   can be followed by the OS before the handle check refuses the result: its target is touched (and, for a write,
//!   an empty file or folder can be created there and is deleted again), but never read from or written to.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;
#[cfg(not(any(unix, windows)))]
mod unsupported;
#[cfg(not(any(unix, windows)))]
use unsupported as sys;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::{self, Write};

use super::{CanonicalPath, FsMode};
use crate::OpError;

/// How [`CanonicalPath::open_write`] treats a file that is already there.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WriteMode {
    /// Create the file, or empty it if it exists (like `File::create`).
    CreateOrTruncate,
    /// Create the file; fail with `Conflict` if anything is already there.
    CreateNew,
}

/// What a folder entry is, as the folder itself reports it: a link is a link and is never looked through.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A folder.
    Folder,
    /// A symbolic link or a junction (on Windows, any name-surrogate reparse point). Its target is never looked up.
    Link,
    /// Anything else: a device, a FIFO, a socket.
    Other,
}

/// One entry of a folder listing ([`CanonicalPath::read_dir`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DirEntryInfo {
    /// The entry's own name (no path).
    pub name: String,
    /// What the entry is, without following a link.
    pub kind: EntryKind,
}

/// How the platform layer opens a file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum How {
    /// An existing regular file, for reading.
    Read,
    /// For writing: created if missing, emptied (after the checks) if not.
    Write,
    /// For writing, and only if nothing is there yet.
    New,
}

/// Why the platform layer did not open something.
// Where nothing can be opened (`unsupported.rs`), only `Unsupported` is ever made.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
#[derive(Debug)]
enum Failure {
    /// A link (symbolic link, junction) or a file where a folder belongs, on the way or at the end; or, on Windows, a
    /// handle that turned out not to be inside the allowed folder.
    Refused,
    NotFound,
    Exists,
    /// A folder, a device, a FIFO or a socket where a regular file was wanted.
    NotAFile,
    /// An in-place write to a file with more than one name (a hard link).
    Linked,
    /// Anything else the OS said (permissions, a full disk, ...).
    Os(io::Error),
    /// This platform has no file system ops can use.
    #[cfg(not(any(unix, windows)))]
    Unsupported,
}

/// What an open was doing, for its error.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Doing {
    Reading,
    Writing,
    MakingFolders,
    Replacing,
    Listing,
}

impl Doing {
    const fn verb(self) -> &'static str {
        match self {
            Self::Reading => "reading",
            Self::Writing => "writing",
            Self::MakingFolders => "making the folders of",
            Self::Replacing => "replacing",
            Self::Listing => "listing",
        }
    }
}

impl CanonicalPath {
    /// Opens the file for reading. The path must have been checked for [`FsMode::Read`].
    ///
    /// # Errors
    /// - `Denied` when a symbolic link or a junction is now on the path or at its end (it was swapped in after the
    ///   check), or a folder on it became a file, or the OS refuses access. Nothing outside the allowed folder is
    ///   read, and the error says nothing about what is there.
    /// - `NotFound` when the file is gone; `Invalid` (field `path`) when it is not a regular file.
    /// - `Internal` when the path was checked for writing only (a bug in the op).
    pub fn open_read(&self) -> Result<File, OpError> {
        self.require(FsMode::Read)?;
        self.open_file(How::Read)
    }

    /// Lists the folder, sorted by name. The folder is opened like a file is (from the allowed folder, refusing a link
    /// at every step) and listed from that handle, so a folder swapped for a link after the check cannot redirect the
    /// listing. Each entry says what it is without following links: a link is listed as a [`EntryKind::Link`] and its
    /// target is never looked up. Names that are not valid UTF-8 are left out, as no path can name them. The path must
    /// have been checked for [`FsMode::Read`].
    ///
    /// # Errors
    /// - `Denied` as for [`open_read`](Self::open_read): a link is now on the path or at its end, or a folder on it
    ///   became a file (or this path is a file, not a folder).
    /// - `NotFound` when the folder is gone.
    /// - `Internal` when the path was checked for writing only (a bug in the op).
    pub fn read_dir(&self) -> Result<Vec<DirEntryInfo>, OpError> {
        self.require(FsMode::Read)?;
        let dir = self.folder(self.below_anchor(), false, Doing::Listing)?;
        let mut entries = dir.list().map_err(|failure| error(self, failure, Doing::Listing))?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// Opens the file for writing, creating it if needed. Its folder must exist ([`parent`](Self::parent) and
    /// [`create_dir_all`](Self::create_dir_all) make it). The path must have been checked for [`FsMode::Write`].
    ///
    /// For a file other programs may be reading, prefer [`replace`](Self::replace), which never shows them a
    /// half-written file.
    ///
    /// # Errors
    /// - `Denied` as for [`open_read`](Self::open_read), and for [`WriteMode::CreateOrTruncate`] on a file with
    ///   more than one name (a hard link: emptying it would empty it under the other names too).
    /// - `Conflict` for [`WriteMode::CreateNew`] when something is already there.
    /// - `NotFound` when its folder does not exist; `Invalid` (field `path`) when it is a folder or a device.
    /// - `Internal` when the path was checked for reading only (a bug in the op).
    pub fn open_write(&self, mode: WriteMode) -> Result<File, OpError> {
        self.require(FsMode::Write)?;
        self.open_file(match mode {
            WriteMode::CreateOrTruncate => How::Write,
            WriteMode::CreateNew => How::New,
        })
    }

    /// Starts replacing the file: the returned [`Replacement`] is a new, empty file in the same folder, which
    /// [`Replacement::commit`] renames over it in one step. Readers see the old file or the new one, never a mix, and
    /// a replacement dropped without a commit is deleted. The new file has no name at all until the commit on Linux
    /// (`O_TMPFILE`); elsewhere it has a name of 128 random bits, so nobody can plant a file or a link under it in
    /// advance. The path must have been checked for [`FsMode::Write`].
    ///
    /// The new file keeps the old one's permissions where it can. On Unix it gets the old file's permission bits
    /// (read when the replacement starts; not set-user-ID, set-group-ID or sticky), set before it is ever visible,
    /// so a private file stays private; with no old file it is `0666` narrowed by the umask. Its owner, extended
    /// attributes and ACLs are not carried over: it belongs to the process. On Windows it gets the permissions its
    /// folder passes on to any new file, so an explicit ACL on the old file, and its attributes, are lost.
    ///
    /// # Errors
    /// As for [`open_write`](Self::open_write), but a hard link is fine: the new file takes this name only.
    /// `Conflict` also when every temporary name tried was taken, which only someone planting files can cause.
    pub fn replace(&self) -> Result<Replacement, OpError> {
        self.require(FsMode::Write)?;
        self.replace_naming(TempNames::random())
    }

    /// [`replace`](Self::replace), with the temporary names taken from `names` (tests plant some of them first).
    fn replace_naming(&self, mut names: TempNames) -> Result<Replacement, OpError> {
        let (folder, name) = self.split().ok_or_else(|| error(self, Failure::NotAFile, Doing::Replacing))?;
        let dir = self.folder(folder, false, Doing::Replacing)?;
        let temp = dir.temp(name, &mut names).map_err(|failure| error(self, failure, Doing::Replacing))?;
        let (name, path) = (name.to_owned(), self.clone());
        Ok(Replacement { temp, dir, names, name, path, committed: false })
    }

    /// Makes this folder, and any missing folder between it and the allowed folder it lies in, never through a link.
    /// Succeeds if the folder is already there. The path must have been checked for [`FsMode::Write`].
    ///
    /// # Errors
    /// `Denied` as for [`open_read`](Self::open_read) (a link, or a file where a folder belongs);
    /// `Internal` when the path was checked for reading only.
    pub fn create_dir_all(&self) -> Result<(), OpError> {
        self.require(FsMode::Write)?;
        self.folder(self.below_anchor(), true, Doing::MakingFolders).map(drop)
    }

    /// The folder this path is in, checked the same way, while that folder is still inside the allowed folder;
    /// `None` for the allowed folder itself.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        (self.place.depth() > self.anchor).then(|| {
            let mut parent = self.clone();
            parent.place.pop();
            parent
        })
    }

    /// The last name of the path (`model.vrm`), for messages and for picking a format by extension.
    pub fn file_name(&self) -> Option<&str> {
        self.place.names().last().map(String::as_str)
    }

    /// The path's names below the allowed folder.
    fn below_anchor(&self) -> &[String] {
        &self.place.names()[self.anchor..]
    }

    /// The folder names below the allowed folder and the file's own name, or `None` for the allowed folder itself.
    fn split(&self) -> Option<(&[String], &str)> {
        let (name, folder) = self.below_anchor().split_last()?;
        Some((folder, name))
    }

    /// Opens the allowed folder, then the folders `names` below it (made on the way with `create`).
    fn folder(&self, names: &[String], create: bool, doing: Doing) -> Result<sys::Dir, OpError> {
        sys::Dir::anchor(&self.place, self.anchor)
            .and_then(|dir| dir.descend(names, create))
            .map_err(|failure| error(self, failure, doing))
    }

    fn open_file(&self, how: How) -> Result<File, OpError> {
        let doing = if how == How::Read { Doing::Reading } else { Doing::Writing };
        let (folder, name) = self.split().ok_or_else(|| error(self, Failure::NotAFile, doing))?;
        self.folder(folder, false, doing)?.file(name, how).map_err(|failure| error(self, failure, doing))
    }

    /// Refuses an open for a mode the path was not checked for: `fs_access` approved one mode, and writing a path
    /// checked for reading would skip the `fs` scope.
    fn require(&self, mode: FsMode) -> Result<(), OpError> {
        if self.mode == mode {
            return Ok(());
        }
        Err(OpError::internal(format!("`{self}` was checked for {}, not for {}", self.mode.verb(), mode.verb()))
            .hint("the op must ask `fs_access` again with the mode it opens the file in"))
    }
}

/// How many temporary names one replacement may try, at its start and at its commit together. Each is 128 random
/// bits, so a name that is taken was planted (or is a 2^-128 accident): a few tries are plenty, and the bound keeps a
/// folder full of plants from costing more than that.
const TEMP_ATTEMPTS: u32 = 16;

/// Names for a replacement's temporary file: `.omuzaki-<128 random bits>.tmp`, from the OS's random source, so nobody
/// can know one in advance to plant a file or a link under it.
#[derive(Debug)]
struct TempNames {
    left: u32,
    /// Tests hand out these names first, to plant some of them.
    #[cfg(test)]
    fixed: std::collections::VecDeque<String>,
}

impl TempNames {
    fn random() -> Self {
        Self {
            left: TEMP_ATTEMPTS,
            #[cfg(test)]
            fixed: std::collections::VecDeque::new(),
        }
    }
}

impl Iterator for TempNames {
    type Item = Result<String, Failure>;

    fn next(&mut self) -> Option<Self::Item> {
        self.left = self.left.checked_sub(1)?;
        #[cfg(test)]
        if let Some(name) = self.fixed.pop_front() {
            return Some(Ok(name));
        }
        Some(random_name())
    }
}

#[cfg(any(unix, windows))]
fn random_name() -> Result<String, Failure> {
    let mut bits = [0_u8; 16];
    getrandom::fill(&mut bits).map_err(|error| Failure::Os(io::Error::other(error.to_string())))?;
    Ok(format!(".omuzaki-{:032x}.tmp", u128::from_le_bytes(bits)))
}

#[cfg(not(any(unix, windows)))]
fn random_name() -> Result<String, Failure> {
    Err(Failure::Unsupported)
}

/// Tries `take` with each name from `names` until one is free: a taken name (`Exists`) moves on to the next, any other
/// failure stops. `Exists` when every name was taken.
#[cfg(any(unix, windows))]
fn first_free<T>(
    names: &mut dyn Iterator<Item = Result<String, Failure>>,
    mut take: impl FnMut(&str) -> Result<T, Failure>,
) -> Result<(T, String), Failure> {
    for name in names {
        let name = name?;
        match take(&name) {
            Ok(taken) => return Ok((taken, name)),
            Err(Failure::Exists) => {}
            Err(failure) => return Err(failure),
        }
    }
    Err(Failure::Exists)
}

/// A new file being written beside the one it will replace ([`CanonicalPath::replace`]). Write to it like a
/// [`File`], then [`commit`](Self::commit). Dropped without a commit, it is deleted.
#[derive(Debug)]
pub struct Replacement {
    temp: sys::Temp,
    dir: sys::Dir,
    /// Where a name comes from at the commit, if the new file has none yet (Linux `O_TMPFILE`).
    names: TempNames,
    name: String,
    path: CanonicalPath,
    /// Set once the new file has the name, so dropping it deletes nothing.
    committed: bool,
}

impl Replacement {
    /// The new file, for writing (and seeking, and reading back what was written).
    pub fn file(&mut self) -> &mut File {
        &mut self.temp.file
    }

    /// Flushes the new file to the disk and renames it over the old one, in its folder. Whatever had the name before
    /// (a file, a hard link, a symbolic link) is replaced, not written through. On Unix the folder is flushed too,
    /// so the rename survives a crash, where the file system can (drvfs and 9p cannot); NTFS journals the rename.
    ///
    /// # Errors
    /// `Denied` or `Invalid` (field `path`) when the disk refuses the flush or the rename (a folder has the name, the
    /// OS refuses access); the new file is deleted and the old one is left as it was.
    pub fn commit(mut self) -> Result<(), OpError> {
        let synced = self.temp.file.sync_all().map_err(Failure::Os);
        // On failure `self` drops on return, which deletes the new file.
        synced
            .and_then(|()| sys::commit(&self.dir, &mut self.temp, &self.name, &mut self.names))
            .map_err(|failure| error(&self.path, failure, Doing::Replacing))?;
        self.committed = true;
        Ok(())
    }
}

impl Write for Replacement {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.temp.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.temp.file.flush()
    }
}

impl Drop for Replacement {
    fn drop(&mut self) {
        if !self.committed {
            sys::discard(&self.dir, &self.temp);
        }
    }
}

/// The op error for a platform failure. A refusal never says which step failed or why: all that is known is that the
/// path no longer is what was checked, and what lies beyond a link was never looked at.
fn error(path: &CanonicalPath, failure: Failure, doing: Doing) -> OpError {
    let verb = doing.verb();
    match failure {
        Failure::Refused => OpError::denied(format!(
            "{verb} `{path}` is refused: since it was checked, a symbolic link or a junction appeared on its way, or \
             something on its way stopped being a folder"
        ))
        .hint("Omuzaki opens a file only along the path it checked, never through a link: ask for it by its real path"),
        Failure::NotFound if matches!(doing, Doing::Reading | Doing::Listing) => {
            OpError::not_found(format!("`{path}` does not exist"))
        }
        Failure::NotFound => {
            OpError::not_found(format!("a folder on the way to `{path}` does not exist")).hint("make the folder first")
        }
        Failure::Exists if doing == Doing::Replacing => OpError::conflict(format!(
            "no temporary file could be made beside `{path}`: every random name tried was taken"
        ))
        .hint("something is planting files in that folder; look at what else writes there"),
        Failure::Exists => {
            OpError::conflict(format!("`{path}` already exists")).hint("pick another name, or replace the file")
        }
        Failure::NotAFile => OpError::invalid("path", format!("`{path}` is not a regular file"))
            .hint("pass the path of a file, not of a folder or a device"),
        Failure::Linked => OpError::denied(format!(
            "writing `{path}` in place is refused: it is a hard link, one file with more than one name, and writing \
             it would change it under its other names too"
        ))
        .hint("replace the file instead: a new file under this name leaves the other names alone"),
        Failure::Os(os) if os.kind() == io::ErrorKind::PermissionDenied => {
            OpError::denied(format!("the operating system refused {verb} `{path}`"))
                .hint("check the permissions of the file and its folders")
        }
        Failure::Os(os) => OpError::invalid("path", format!("{verb} `{path}` failed: {os}")),
        #[cfg(not(any(unix, windows)))]
        Failure::Unsupported => OpError::unsupported("this platform has no file system ops can open files on"),
    }
}
