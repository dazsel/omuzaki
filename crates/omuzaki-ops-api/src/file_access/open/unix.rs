//! Opening beneath a folder handle on Linux, macOS and the other Unixes, never through a link.
//!
//! Every folder is opened relative to the one above it with `O_NOFOLLOW | O_DIRECTORY`, so a symbolic link at any
//! step fails the open (`ELOOP` or `ENOTDIR`) instead of being followed, and the file at the end is opened with
//! `O_NOFOLLOW` relative to its folder's handle. On Linux, `openat2` with `RESOLVE_NO_SYMLINKS` (and
//! `RESOLVE_BENEATH` below the allowed folder) does the whole walk in one call; where the kernel or a sandbox does not
//! offer it, the walk above is used instead. Both refuse exactly the same paths.

use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;

use rustix::fs::{self as sys, AtFlags, CWD, FileType, Mode, OFlags};
use rustix::io::Errno;

use super::{DirEntryInfo, EntryKind, Failure, How, first_free};
use crate::file_access::form::Lexical;

/// How folders are opened. Linux opens them as `O_PATH`, which needs no read permission and reads nothing; the other
/// Unixes have no `O_PATH`, so they open the folder for reading.
#[cfg(any(target_os = "linux", target_os = "android"))]
const FOLDER: OFlags = OFlags::PATH.union(OFlags::DIRECTORY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const FOLDER: OFlags = OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);

/// Flags every file open carries: never follow a link at the last name, never become the controlling terminal, and
/// do not block on a FIFO someone planted (the file type is checked right after, and only regular files pass).
const FILE: OFlags = OFlags::NOFOLLOW.union(OFlags::CLOEXEC).union(OFlags::NOCTTY).union(OFlags::NONBLOCK);

/// `rw-rw-rw-` and `rwxrwxrwx`, narrowed by the process's umask like any other new file or folder.
const NEW_FILE: Mode =
    Mode::RUSR.union(Mode::WUSR).union(Mode::RGRP).union(Mode::WGRP).union(Mode::ROTH).union(Mode::WOTH);
const NEW_FOLDER: Mode = Mode::RWXU.union(Mode::RWXG).union(Mode::RWXO);

/// The permission bits a replacement carries over from the file it replaces. Not set-user-ID, set-group-ID or sticky:
/// new content should not inherit privileges granted to the old.
const PERMISSIONS: Mode = Mode::RWXU.union(Mode::RWXG).union(Mode::RWXO);

/// A folder, reached from the top of the file system without passing a link at any step.
#[derive(Debug)]
pub(in crate::file_access) struct Dir(OwnedFd);

impl Dir {
    /// The folder `place` names (its first `depth` names), the one that allowed the path: opened from `/` with no link
    /// at any step, so it is the folder of that name and not one a link leads to.
    pub(super) fn anchor(place: &Lexical, depth: usize) -> Result<Self, Failure> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if let Some(opened) = openat2::folder(CWD, place.path(depth).as_path(), false) {
            return opened;
        }
        let root = sys::openat(CWD, "/", FOLDER, Mode::empty()).map_err(failure)?;
        Self(root).descend(&place.names()[..depth.min(place.depth())], false)
    }

    /// The folder `names` below this one, each a real folder and never a link. With `create`, missing folders are made
    /// on the way (each made relative to its parent's handle, then opened like the others).
    pub(super) fn descend(self, names: &[String], create: bool) -> Result<Self, Failure> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if !create
            && !names.is_empty()
            && let Some(opened) = openat2::folder(&self.0, names.join("/"), true)
        {
            return opened;
        }
        names.iter().try_fold(self, |dir, name| dir.child(name, create))
    }

    fn child(&self, name: &str, create: bool) -> Result<Self, Failure> {
        match sys::openat(&self.0, name, FOLDER, Mode::empty()) {
            Err(Errno::NOENT) if create => {
                match sys::mkdirat(&self.0, name, NEW_FOLDER) {
                    // Someone else made it first: fine, it is opened (and checked) like any other folder below.
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(errno) => return Err(failure(errno)),
                }
                sys::openat(&self.0, name, FOLDER, Mode::empty()).map(Self).map_err(failure)
            }
            opened => opened.map(Self).map_err(failure),
        }
    }

    /// The regular file `name` in this folder, opened as `how` says and never through a link.
    pub(super) fn file(&self, name: &str, how: How) -> Result<File, Failure> {
        self.file_with_mode(name, how, NEW_FILE)
    }

    /// [`file`](Self::file), creating a missing file with `mode` (narrowed by the umask).
    fn file_with_mode(&self, name: &str, how: How, mode: Mode) -> Result<File, Failure> {
        let flags = FILE
            | match how {
                How::Read => OFlags::RDONLY,
                How::Write => OFlags::WRONLY | OFlags::CREATE,
                How::New => OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            };
        let fd = sys::openat(&self.0, name, flags, mode).map_err(failure)?;
        let stat = sys::fstat(&fd).map_err(failure)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(Failure::NotAFile);
        }
        // Emptying a file with a second name would empty it under that name too, wherever that is (`How::Write` opens
        // without `O_TRUNC` so this can be checked first).
        if how == How::Write && stat.st_nlink > 1 {
            return Err(Failure::Linked);
        }
        let blocking = sys::fcntl_getfl(&fd).map_err(failure)?.difference(OFlags::NONBLOCK);
        sys::fcntl_setfl(&fd, blocking).map_err(failure)?;
        let file = File::from(fd);
        if how == How::Write {
            file.set_len(0).map_err(Failure::Os)?;
        }
        Ok(file)
    }

    /// The entries of this folder, listed from its own handle and typed without following links. (On Linux the handle
    /// is `O_PATH`, which cannot be read, so the folder is opened again as `.` relative to it: the same folder.)
    pub(super) fn list(&self) -> Result<Vec<DirEntryInfo>, Failure> {
        let fd = sys::openat(&self.0, ".", OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
            .map_err(failure)?;
        let mut entries = Vec::new();
        for entry in sys::Dir::new(fd).map_err(failure)? {
            let entry = entry.map_err(failure)?;
            let Ok(name) = entry.file_name().to_str() else { continue };
            if name == "." || name == ".." {
                continue;
            }
            let file_type = match entry.file_type() {
                // Not every file system fills the type in the listing: ask the folder's handle, not following a link.
                FileType::Unknown => FileType::from_raw_mode(
                    sys::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW).map_err(failure)?.st_mode,
                ),
                known => known,
            };
            let kind = match file_type {
                FileType::RegularFile => EntryKind::File,
                FileType::Directory => EntryKind::Folder,
                FileType::Symlink => EntryKind::Link,
                _ => EntryKind::Other,
            };
            entries.push(DirEntryInfo { name: name.to_owned(), kind });
        }
        Ok(entries)
    }

    /// Renames `from` to `to`, both in this folder, replacing whatever `to` is (a link there is replaced, not
    /// followed).
    pub(super) fn rename(&self, from: &str, to: &str) -> Result<(), Failure> {
        sys::renameat(&self.0, from, &self.0, to).map_err(failure)
    }

    /// Removes the file `name` from this folder (a link there is removed, not followed).
    pub(super) fn remove(&self, name: &str) -> Result<(), Failure> {
        sys::unlinkat(&self.0, name, AtFlags::empty()).map_err(failure)
    }

    /// The permission bits of the regular file `name` in this folder, asked of this folder's handle without following
    /// a link; `None` if nothing is there, or a link or anything but a regular file is.
    fn permissions_of(&self, name: &str) -> Result<Option<Mode>, Failure> {
        match sys::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {
                Ok(Some(Mode::from_raw_mode(stat.st_mode) & PERMISSIONS))
            }
            Ok(_) | Err(Errno::NOENT) => Ok(None),
            Err(errno) => Err(failure(errno)),
        }
    }

    /// Flushes this folder's entries to the disk, so a rename in it survives a crash. Best effort: some file systems
    /// (drvfs, 9p) cannot flush a folder, and the rename has happened either way.
    fn sync(&self) {
        if let Ok(fd) = sys::openat(&self.0, ".", OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()) {
            let _ = sys::fsync(&fd);
        }
    }
}

/// A replacement's new file.
#[derive(Debug)]
pub(in crate::file_access) struct Temp {
    pub(super) file: File,
    /// Its name in the folder, once it has one: on Linux it has none until the commit (`O_TMPFILE`).
    name: Option<String>,
}

impl Dir {
    /// A new, empty file in this folder to replace `replacing`. On Linux it has no name at all until the commit
    /// (`O_TMPFILE`), so there is nothing to plant or swap while it is written. Elsewhere, and on file systems without
    /// `O_TMPFILE` (drvfs and 9p under WSL), it takes the first name from `names` that nothing holds yet.
    ///
    /// It gets the permission bits of the file it replaces, read now: created with them (the umask can only narrow
    /// them), then set to exactly them, so it is never visible with wider permissions than the old file had. With no
    /// old file it is `0666` narrowed by the umask, like any new file. The owner, extended attributes and ACLs are not
    /// carried over: the new file is the process's own.
    pub(super) fn temp(
        &self,
        replacing: &str,
        names: &mut dyn Iterator<Item = Result<String, Failure>>,
    ) -> Result<Temp, Failure> {
        let like = self.permissions_of(replacing)?;
        let mode = like.unwrap_or(NEW_FILE);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let unnamed = tmpfile::create(self, mode)?;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let unnamed: Option<File> = None;
        let temp = if let Some(file) = unnamed {
            Temp { file, name: None }
        } else {
            let (file, name) = first_free(names, |name| self.file_with_mode(name, How::New, mode))?;
            Temp { file, name: Some(name) }
        };
        if let Some(like) = like {
            // On failure the temp is dropped here, and the replacement that would have removed it was never made.
            if let Err(errno) = sys::fchmod(&temp.file, like) {
                discard(self, &temp);
                return Err(failure(errno));
            }
        }
        Ok(temp)
    }
}

/// Puts a finished replacement in place: the new file gets a name in `dir` if it has none yet, then takes `name`.
pub(super) fn commit(
    dir: &Dir,
    temp: &mut Temp,
    name: &str,
    names: &mut dyn Iterator<Item = Result<String, Failure>>,
) -> Result<(), Failure> {
    let current = match temp.name.take() {
        Some(current) => current,
        None => give_name(dir, &temp.file, names)?,
    };
    let renamed = dir.rename(&current, name);
    // Kept even after a failed rename, so dropping the replacement removes it.
    temp.name = Some(current);
    renamed?;
    dir.sync();
    Ok(())
}

/// Throws an unfinished replacement away. A file that never got a name vanishes when it is closed.
pub(super) fn discard(dir: &Dir, temp: &Temp) {
    if let Some(name) = &temp.name {
        // Best effort: a temporary file that cannot be removed is only clutter, and it is inside the allowed folder.
        let _ = dir.remove(name);
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn give_name(
    dir: &Dir,
    file: &File,
    names: &mut dyn Iterator<Item = Result<String, Failure>>,
) -> Result<String, Failure> {
    first_free(names, |name| tmpfile::link(dir, file, name)).map(|((), name)| name)
}

/// Only Linux makes files without a name, so here every new file already has one.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn give_name(
    _dir: &Dir,
    _file: &File,
    _names: &mut dyn Iterator<Item = Result<String, Failure>>,
) -> Result<String, Failure> {
    Err(Failure::NotFound)
}

/// What an errno means for an open. A link where a folder or file was expected is `ELOOP` (`O_NOFOLLOW` at the last
/// name, `RESOLVE_NO_SYMLINKS` anywhere) or `ENOTDIR` (`O_DIRECTORY | O_NOFOLLOW` on a link); FreeBSD says `EMLINK`;
/// `EXDEV` is `RESOLVE_BENEATH` refusing to leave the folder.
fn failure(errno: Errno) -> Failure {
    match errno {
        Errno::LOOP | Errno::NOTDIR | Errno::MLINK | Errno::XDEV => Failure::Refused,
        Errno::NOENT => Failure::NotFound,
        Errno::EXIST => Failure::Exists,
        // A folder where a file was wanted, or a FIFO or socket nobody is at the other end of.
        Errno::ISDIR | Errno::NXIO => Failure::NotAFile,
        other => Failure::Os(io::Error::from(other)),
    }
}

/// `openat2`, the one-call walk, when this kernel offers it.
#[cfg(any(target_os = "linux", target_os = "android"))]
mod openat2 {
    use std::sync::atomic::{AtomicBool, Ordering};

    use rustix::fd::AsFd;
    use rustix::fs::{self as sys, Mode, ResolveFlags};
    use rustix::io::Errno;
    use rustix::path::Arg;

    use super::{Dir, FOLDER, Failure, failure, fallbacks_forced};

    /// Set once `openat2` turns out to be missing (a kernel before 5.6) or filtered (a seccomp sandbox), so later
    /// opens go straight to the walk.
    static UNAVAILABLE: AtomicBool = AtomicBool::new(false);

    /// Opens the folder `path` relative to `dirfd` (or the absolute `path`) with no link at any step, and with
    /// `beneath`, never above `dirfd`. `None` means "walk instead": `openat2` is not available, or it asked to be
    /// retried (`EAGAIN`, a rename raced with it), or the path is longer than one call takes.
    pub(super) fn folder(dirfd: impl AsFd, path: impl Arg, beneath: bool) -> Option<Result<Dir, Failure>> {
        if UNAVAILABLE.load(Ordering::Relaxed) || fallbacks_forced() {
            return None;
        }
        let mut resolve = ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS;
        if beneath {
            resolve |= ResolveFlags::BENEATH;
        }
        match sys::openat2(dirfd, path, FOLDER, Mode::empty(), resolve) {
            Ok(fd) => Some(Ok(Dir(fd))),
            Err(Errno::NOSYS | Errno::PERM | Errno::INVAL | Errno::TOOBIG) => {
                UNAVAILABLE.store(true, Ordering::Relaxed);
                None
            }
            Err(Errno::AGAIN | Errno::NAMETOOLONG) => None,
            Err(errno) => Some(Err(failure(errno))),
        }
    }
}

/// Files with no name (`O_TMPFILE`), named only when a replacement is committed.
#[cfg(any(target_os = "linux", target_os = "android"))]
mod tmpfile {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    use rustix::fs::{self as sys, AtFlags, CWD, Mode, OFlags};
    use rustix::io::Errno;

    use super::{Dir, Failure, failure, fallbacks_forced};

    /// An unnamed regular file in `dir`, created with `mode`, or `None` where the kernel (before 3.11) or the file
    /// system (drvfs, 9p) has no `O_TMPFILE`: the caller names one instead.
    pub(super) fn create(dir: &Dir, mode: Mode) -> Result<Option<File>, Failure> {
        if fallbacks_forced() {
            return Ok(None);
        }
        match sys::openat(&dir.0, ".", OFlags::TMPFILE | OFlags::WRONLY | OFlags::CLOEXEC, mode) {
            Ok(fd) => Ok(Some(File::from(fd))),
            Err(Errno::OPNOTSUPP | Errno::ISDIR | Errno::INVAL) => Ok(None),
            Err(errno) => Err(failure(errno)),
        }
    }

    /// Gives the unnamed `file` the name `name` in `dir` (`Exists` if something already has it). Through its
    /// `/proc/self/fd` link, which needs no privilege; `AT_EMPTY_PATH`, the fallback where `/proc` is missing, needs
    /// `CAP_DAC_READ_SEARCH` on older kernels.
    pub(super) fn link(dir: &Dir, file: &File, name: &str) -> Result<(), Failure> {
        let by_proc = format!("/proc/self/fd/{}", file.as_raw_fd());
        match sys::linkat(CWD, by_proc.as_str(), &dir.0, name, AtFlags::SYMLINK_FOLLOW) {
            Err(Errno::NOENT) => sys::linkat(file, "", &dir.0, name, AtFlags::EMPTY_PATH).map_err(failure),
            linked => linked.map_err(failure),
        }
    }
}

/// Whether this thread runs the portable fallbacks (the component walk, named temporary files) instead of `openat2`
/// and `O_TMPFILE`. Only tests turn them on, to test the fallbacks on a kernel that has both.
#[cfg(all(not(test), any(target_os = "linux", target_os = "android")))]
const fn fallbacks_forced() -> bool {
    false
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
std::thread_local! {
    static FALLBACKS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
fn fallbacks_forced() -> bool {
    FALLBACKS.with(std::cell::Cell::get)
}

/// Runs `body` with the portable fallbacks forced on this thread (see [`fallbacks_forced`]), and turns them off again
/// even if `body` panics, so a test thread that is reused does not inherit them.
#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
pub(in crate::file_access) fn with_fallbacks<T>(body: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FALLBACKS.with(|on| on.set(false));
        }
    }
    FALLBACKS.with(|on| on.set(true));
    let _reset = Reset;
    body()
}
