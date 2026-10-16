//! `std::fs` as a [`Disk`].
//!
//! Durability follows `omuzaki-core`'s `fsx::write_atomic`. On unix and macOS a file and a folder are flushed with
//! `fsync`. Windows cannot open a folder to flush it, so [`Disk::fsync`] of a folder does nothing there and a rename
//! relies on NTFS journaling its metadata; `fs::rename` replaces atomically in practice but Microsoft does not document
//! it, which `omuzaki-core` tracks as #1757. A caller that needs more on Windows must not rely on this crate.
//!
//! `write_atomic` is the trait's provided method (write `<path>.tmp`, fsync it, rename it over `path`, fsync the
//! folder), so the crash-safe sequence is written once, in `omuzaki-update-api`, and runs here over the real calls.

#[cfg(windows)]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
#[cfg(windows)]
use std::{thread, time::Duration};

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
        sync_path(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename_over(from, to)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        // A symlink is removed itself and its target is kept: `fs::remove_dir_all` does not follow it.
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() || meta.is_symlink() {
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

/// Flushes a file, or on unix a folder, to disk.
#[cfg(unix)]
fn sync_path(path: &Path) -> io::Result<()> {
    // A read-only handle can be synced, so a file the process may only read (mode 0444) still flushes.
    File::open(path)?.sync_all()
}

/// Flushes a file. A folder is a no-op: Windows cannot open one to flush it (see the module docs).
#[cfg(windows)]
fn sync_path(path: &Path) -> io::Result<()> {
    if fs::metadata(path)?.is_dir() {
        return Ok(());
    }
    // Windows refuses to flush a handle that has no write access.
    OpenOptions::new().read(true).write(true).open(path)?.sync_all()
}

#[cfg(not(windows))]
fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

/// Renames `from` over `to`, retrying while another process (an editor, a sync client, a virus scanner) holds `to`
/// open. Only a refusal that a moment can cure is retried, as in `omuzaki-core`'s `fsx`.
#[cfg(windows)]
fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    /// Waits before each retry: three retries, about 100 ms in all.
    const RETRY_DELAYS: [Duration; 3] =
        [Duration::from_millis(20), Duration::from_millis(30), Duration::from_millis(50)];
    /// `ERROR_SHARING_VIOLATION`: the target is open in another process without sharing it.
    const SHARING_VIOLATION: i32 = 32;

    let mut delays = RETRY_DELAYS.iter();
    loop {
        match fs::rename(from, to) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(SHARING_VIOLATION) => {
                match delays.next() {
                    Some(delay) => thread::sleep(*delay),
                    None => return Err(e),
                }
            }
            other => return other,
        }
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
        // `LinkKind` may grow; a kind this build does not know cannot be made into a link, so refuse rather than guess.
        _ => Err(io::Error::new(io::ErrorKind::Unsupported, "unknown link kind")),
    }
}

#[cfg(unix)]
fn set_exec(path: &Path, on: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    let mode = permissions.mode();
    // Execute goes wherever read is allowed, as `chmod +x` does, and always to the owner so a file with no read
    // bits still becomes executable.
    let exec = ((mode & 0o444) >> 2) | 0o100;
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
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            std::ptr::from_mut(&mut available),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(available) }
}
