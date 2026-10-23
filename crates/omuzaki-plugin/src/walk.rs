//! Reads a package folder without ever following a link (design §7, "hostile packages"): nothing outside the folder
//! is hashed or staged, however its entries are swapped while it is read.
//!
//! - **Unix:** each folder is opened relative to the handle of the one above it, with `O_NOFOLLOW | O_DIRECTORY`, and
//!   listed from its own handle, so a folder swapped for a link (and back) between two looks can't redirect the walk.
//!   Each file is opened the same way with `O_NOFOLLOW | O_NONBLOCK`, and kept only if the open handle is a regular
//!   file with one name and the device and inode the listing saw: a link swapped in fails the open, and a FIFO
//!   swapped in opens without waiting and is refused.
//! - **Windows:** Win32 has no handle-relative open, so every folder on the way down is held open without
//!   `FILE_SHARE_DELETE` while the walk is inside it: nothing can rename it, delete it or put a junction in its place
//!   until the walk leaves. Each folder and file is opened with `FILE_FLAG_OPEN_REPARSE_POINT` and refused if it is a
//!   reparse point (a symbolic link or a junction).
//! - Elsewhere no folder package can be read safely, and reading one is refused.
//!
//! The folder the person names may itself be reached through a link; nothing below it is.

use std::path::Path;

use crate::package::{Collector, PackageError, canonical, io};

/// Reads every entry under `root` into `collector`, folders before what they hold, names sorted by their bytes.
///
/// # Errors
/// [`PackageError::NotRegular`] for a link, a device, a FIFO, a socket, a file with more than one name, or an entry
/// that changed between two looks; [`PackageError::UnsafePath`] for a name that is not UTF-8; the collector's limits;
/// [`PackageError::Io`] for what the OS refused.
pub(crate) fn walk(root: &Path, collector: &mut Collector) -> Result<(), PackageError> {
    sys::walk(root, collector)
}

/// The path of `names`, for an error about a name that is not UTF-8.
fn lossy(names: &[String], last: &str) -> PackageError {
    let mut path = names.join("/");
    if !path.is_empty() {
        path.push('/');
    }
    path.push_str(last);
    PackageError::UnsafePath { path, why: "is not UTF-8" }
}

#[cfg(unix)]
mod sys {
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::os::fd::OwnedFd;
    use std::path::Path;

    use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, Stat, fstat, openat, statat};
    use rustix::io::Errno;

    use super::{Collector, PackageError, canonical, io, lossy};

    fn os(errno: Errno) -> PackageError {
        io(&std::io::Error::from(errno))
    }

    pub(super) fn walk(root: &Path, collector: &mut Collector) -> Result<(), PackageError> {
        let fd = openat(CWD, root, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).map_err(os)?;
        folder(&fd, &mut Vec::new(), collector)
    }

    /// The names in the folder `fd`, sorted by their bytes, without `.` and `..`.
    pub(super) fn names(fd: &OwnedFd) -> Result<Vec<CString>, Errno> {
        let mut names = Vec::new();
        for entry in Dir::read_from(fd)? {
            let name = entry?.file_name().to_owned();
            if name.as_bytes() != b"." && name.as_bytes() != b".." {
                names.push(name);
            }
        }
        names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        Ok(names)
    }

    fn folder(fd: &OwnedFd, prefix: &mut Vec<String>, collector: &mut Collector) -> Result<(), PackageError> {
        for name in names(fd).map_err(os)? {
            let Ok(text) = name.to_str() else { return Err(lossy(prefix, &name.to_string_lossy())) };
            prefix.push(text.to_owned());
            let path = canonical(prefix.join("/").as_bytes())?;
            let seen = statat(fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW).map_err(os)?;
            match FileType::from_raw_mode(seen.st_mode) {
                FileType::Directory => {
                    collector.entry(&path)?;
                    let Some(below) = open_folder(fd, &name, &seen).map_err(os)? else {
                        return Err(PackageError::NotRegular(path));
                    };
                    folder(&below, prefix, collector)?;
                }
                FileType::RegularFile => {
                    let Some((mut file, size)) = open_file(fd, &name, &seen).map_err(os)? else {
                        return Err(PackageError::NotRegular(path));
                    };
                    collector.file(path, size, &mut file)?;
                }
                _ => return Err(PackageError::NotRegular(path)),
            }
            prefix.pop();
        }
        Ok(())
    }

    /// The folder `name` under `dir`, if it is still the folder `seen` described; `None` for a link or anything else
    /// that took its place.
    pub(super) fn open_folder(dir: &OwnedFd, name: &CStr, seen: &Stat) -> Result<Option<OwnedFd>, Errno> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        match openat(dir, name, flags, Mode::empty()) {
            Ok(fd) => {
                let opened = fstat(&fd)?;
                Ok(same(seen, &opened).then_some(fd))
            }
            Err(Errno::LOOP | Errno::NOTDIR) => Ok(None),
            Err(errno) => Err(errno),
        }
    }

    /// The file `name` under `dir` and its size, if it is still the regular file `seen` described, with one name;
    /// `None` for a link, a FIFO or anything else that took its place. The open never waits.
    pub(super) fn open_file(dir: &OwnedFd, name: &CStr, seen: &Stat) -> Result<Option<(File, u64)>, Errno> {
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        match openat(dir, name, flags, Mode::empty()) {
            Ok(fd) => {
                let opened = fstat(&fd)?;
                let regular = FileType::from_raw_mode(opened.st_mode) == FileType::RegularFile;
                // A hard link could make one staged file another file's bytes.
                let ok = regular && same(seen, &opened) && opened.st_nlink == 1;
                let size = u64::try_from(opened.st_size).unwrap_or(u64::MAX);
                Ok(ok.then(|| (File::from(fd), size)))
            }
            Err(Errno::LOOP) => Ok(None),
            Err(errno) => Err(errno),
        }
    }

    fn same(a: &Stat, b: &Stat) -> bool {
        a.st_dev == b.st_dev && a.st_ino == b.st_ino
    }
}

#[cfg(windows)]
mod sys {
    use std::fs::{self, File, OpenOptions};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };

    use super::{Collector, PackageError, canonical, io, lossy};
    use crate::package::hard_linked;

    pub(super) fn walk(root: &Path, collector: &mut Collector) -> Result<(), PackageError> {
        let Some(held) = open_folder(root, true).map_err(|e| io(&e))? else {
            return Err(PackageError::Io("the package folder is not a folder".into()));
        };
        folder(root, &held, &mut Vec::new(), collector)
    }

    /// `_held` is `dir`'s handle, kept open while the walk is inside it.
    fn folder(
        dir: &Path,
        _held: &File,
        prefix: &mut Vec<String>,
        collector: &mut Collector,
    ) -> Result<(), PackageError> {
        let mut entries: Vec<_> =
            fs::read_dir(dir).map_err(|e| io(&e))?.collect::<Result<_, _>>().map_err(|e| io(&e))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            let Some(text) = name.to_str() else { return Err(lossy(prefix, &name.to_string_lossy())) };
            prefix.push(text.to_owned());
            let path = canonical(prefix.join("/").as_bytes())?;
            let full = entry.path();
            let seen = fs::symlink_metadata(&full).map_err(|e| io(&e))?;
            if reparse(&seen) {
                return Err(PackageError::NotRegular(path));
            }
            if seen.is_dir() {
                collector.entry(&path)?;
                let Some(held) = open_folder(&full, false).map_err(|e| io(&e))? else {
                    return Err(PackageError::NotRegular(path));
                };
                folder(&full, &held, prefix, collector)?;
            } else if seen.is_file() {
                let mut file = OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ)
                    .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(&full)
                    .map_err(|e| io(&e))?;
                let opened = file.metadata().map_err(|e| io(&e))?;
                if !opened.is_file() || reparse(&opened) || hard_linked(&file).map_err(|e| io(&e))? {
                    return Err(PackageError::NotRegular(path));
                }
                collector.file(path, opened.len(), &mut file)?;
            } else {
                return Err(PackageError::NotRegular(path));
            }
            prefix.pop();
        }
        Ok(())
    }

    /// Opens and holds the folder at `path`: without `FILE_SHARE_DELETE`, nothing renames or deletes it while the
    /// handle is open. `None` when it is a reparse point, unless `follow` (the folder the person named).
    fn open_folder(path: &Path, follow: bool) -> std::io::Result<Option<File>> {
        let flags =
            if follow { FILE_FLAG_BACKUP_SEMANTICS } else { FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT };
        let held = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(flags)
            .open(path)?;
        let opened = held.metadata()?;
        Ok((opened.is_dir() && (follow || !reparse(&opened))).then_some(held))
    }

    fn reparse(meta: &fs::Metadata) -> bool {
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
}

#[cfg(not(any(unix, windows)))]
mod sys {
    use std::path::Path;

    use super::{Collector, PackageError};

    pub(super) fn walk(_: &Path, _: &mut Collector) -> Result<(), PackageError> {
        Err(PackageError::Io("package folders can't be read safely on this platform".into()))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::ffi::CString;
    use std::fs;
    use std::time::{Duration, Instant};

    use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, mknodat, openat, statat};

    use super::sys::{names, open_file, open_folder};

    fn c(name: &str) -> CString {
        CString::new(name).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_fifo_swapped_in_for_a_file_is_refused_without_waiting() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        fs::write(dir.path().join("plugin.wasm"), b"package").unwrap_or_else(|e| panic!("{e}"));
        let fd = openat(CWD, dir.path(), OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
            .unwrap_or_else(|e| panic!("{e}"));
        let seen =
            statat(&fd, c("plugin.wasm").as_c_str(), AtFlags::SYMLINK_NOFOLLOW).unwrap_or_else(|e| panic!("{e}"));
        fs::remove_file(dir.path().join("plugin.wasm")).unwrap_or_else(|e| panic!("{e}"));
        mknodat(&fd, c("plugin.wasm").as_c_str(), FileType::Fifo, Mode::RUSR | Mode::WUSR, 0)
            .unwrap_or_else(|e| panic!("{e}"));
        let started = Instant::now();
        let opened = open_file(&fd, c("plugin.wasm").as_c_str(), &seen).unwrap_or_else(|e| panic!("{e}"));
        assert!(opened.is_none(), "a FIFO is not the file the listing saw");
        assert!(started.elapsed() < Duration::from_secs(1), "the open did not wait for a writer");
    }

    #[test]
    fn a_folder_swapped_for_a_link_and_back_never_lists_the_outside() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let (sub, outside, moved) = (dir.path().join("sub"), dir.path().join("outside"), dir.path().join("moved"));
        fs::create_dir(&sub).unwrap_or_else(|e| panic!("{e}"));
        fs::write(sub.join("inside.txt"), b"package").unwrap_or_else(|e| panic!("{e}"));
        fs::create_dir(&outside).unwrap_or_else(|e| panic!("{e}"));
        fs::write(outside.join("secret.txt"), b"not the package's").unwrap_or_else(|e| panic!("{e}"));
        let root = openat(CWD, dir.path(), OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
            .unwrap_or_else(|e| panic!("{e}"));
        let seen = statat(&root, c("sub").as_c_str(), AtFlags::SYMLINK_NOFOLLOW).unwrap_or_else(|e| panic!("{e}"));

        // Swapped for a link before the open: the open refuses it.
        fs::rename(&sub, &moved).unwrap_or_else(|e| panic!("{e}"));
        std::os::unix::fs::symlink(&outside, &sub).unwrap_or_else(|e| panic!("{e}"));
        assert!(open_folder(&root, c("sub").as_c_str(), &seen).unwrap_or_else(|e| panic!("{e}")).is_none());

        // Swapped back, opened, then swapped for the link again while it is read: the listing is the handle's.
        fs::remove_file(&sub).unwrap_or_else(|e| panic!("{e}"));
        fs::rename(&moved, &sub).unwrap_or_else(|e| panic!("{e}"));
        let held = open_folder(&root, c("sub").as_c_str(), &seen)
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|| panic!("the folder it saw"));
        fs::rename(&sub, &moved).unwrap_or_else(|e| panic!("{e}"));
        std::os::unix::fs::symlink(&outside, &sub).unwrap_or_else(|e| panic!("{e}"));
        let listed = names(&held).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(listed, [c("inside.txt")], "never the outside folder's entries");
    }
}
