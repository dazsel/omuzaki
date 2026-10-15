//! `std::fs` as a [`Disk`].
//!
//! `write_atomic` is the trait's provided method (write `<path>.tmp`, fsync it, rename it over `path`, fsync the
//! folder), so the crash-safe sequence is written once, in `omuzaki-update-api`, and runs here over the real calls.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use omuzaki_update_api::disk::{DirEntry, Disk, LinkKind};

/// The install folder on the real file system.
#[derive(Copy, Clone, Debug, Default)]
pub struct StdDisk;

impl Disk for StdDisk {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        // Written through the handle rather than `fs::write` so the contract's "not durable until fsync" is plain:
        // nothing here flushes to disk.
        File::create(path)?.write_all(bytes)
    }

    fn fsync(&self, path: &Path) -> io::Result<()> {
        // Opened read-write: Windows refuses to flush a handle without write access. A folder cannot be opened there,
        // and its entries are journaled with the rename, so a folder is a no-op on Windows.
        if cfg!(windows) && fs::metadata(path)?.is_dir() {
            return Ok(());
        }
        OpenOptions::new().read(true).write(!path.is_dir()).open(path)?.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        // A link to a folder is removed as a link, never followed into its target.
        if fs::symlink_metadata(path)?.is_dir() {
            fs::remove_dir_all(path)
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidInput, "not a folder"))
        }
    }

    fn exists(&self, path: &Path) -> bool {
        // A dangling link still counts: something is at the path.
        fs::symlink_metadata(path).is_ok()
    }

    fn list_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let name = entry.file_name().into_string().map_err(|raw| {
                io::Error::new(io::ErrorKind::InvalidData, format!("file name {} is not UTF-8", raw.display()))
            })?;
            entries.push(DirEntry { name, is_dir: kind.is_dir(), is_symlink: kind.is_symlink() });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn symlink(&self, target: &str, link: &Path, kind: LinkKind) -> io::Result<()> {
        make_link(target, link, kind)
    }

    fn set_executable(&self, path: &Path, on: bool) -> io::Result<()> {
        set_exec(path, on)
    }

    fn free_bytes(&self, path: &Path) -> io::Result<u64> {
        free_space(path)
    }
}

#[cfg(unix)]
fn make_link(target: &str, link: &Path, _kind: LinkKind) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn make_link(target: &str, link: &Path, kind: LinkKind) -> io::Result<()> {
    match kind {
        LinkKind::File => std::os::windows::fs::symlink_file(target, link),
        LinkKind::Dir => std::os::windows::fs::symlink_dir(target, link),
    }
}

#[cfg(unix)]
fn set_exec(path: &Path, on: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    let mode = permissions.mode();
    // Read permission grants execute permission, as `chmod +x` does without a umask.
    let exec = (mode & 0o444) >> 2;
    permissions.set_mode(if on { mode | exec } else { mode & !0o111 });
    fs::set_permissions(path, permissions)
}

#[cfg(windows)]
fn set_exec(path: &Path, _on: bool) -> io::Result<()> {
    // No executable bit on Windows: only report a missing file, as the contract says.
    fs::metadata(path).map(drop)
}

#[cfg(unix)]
fn free_space(path: &Path) -> io::Result<u64> {
    let stat = rustix::fs::statvfs(path)?;
    // Blocks an ordinary process may still use (`f_bavail`, not `f_bfree`, which counts the superuser's reserve).
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

#[cfg(windows)]
// The one FFI call in the crate; the buffer is a local `u64` and the path a NUL-terminated wide string.
#[allow(unsafe_code)]
fn free_space(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0_u64;
    // SAFETY: `wide` ends in NUL and outlives the call; `available` is a valid out pointer; the two optional out
    // pointers are null, which the API allows.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) };
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(available) }
}
