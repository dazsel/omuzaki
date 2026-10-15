//! `Disk::write_atomic` with each step failing in turn: before the rename the old content stays; a failed folder fsync
//! after the rename leaves the new content in place and still reports the error.

// Test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use omuzaki_update_api::disk::{DirEntry, Disk, LinkKind};

/// The step that fails, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fail {
    Nothing,
    Write,
    FsyncFile,
    Rename,
    FsyncDir,
}

/// Files in memory; folders always exist.
struct MemDisk {
    files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
    fail: Fail,
    links: Mutex<Vec<(String, PathBuf, LinkKind)>>,
}

impl MemDisk {
    fn new(fail: Fail) -> Self {
        let files = BTreeMap::from([(PathBuf::from("/install/state.toml"), b"old".to_vec())]);
        Self { files: Mutex::new(files), fail, links: Mutex::new(Vec::new()) }
    }

    fn injected(&self, step: Fail) -> io::Result<()> {
        if self.fail == step { Err(io::Error::other("injected")) } else { Ok(()) }
    }
}

impl Disk for MemDisk {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files.lock().unwrap().get(path).cloned().ok_or_else(|| io::ErrorKind::NotFound.into())
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.injected(Fail::Write)?;
        self.files.lock().unwrap().insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }
    fn fsync(&self, path: &Path) -> io::Result<()> {
        let is_file = self.files.lock().unwrap().contains_key(path);
        self.injected(if is_file { Fail::FsyncFile } else { Fail::FsyncDir })
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.injected(Fail::Rename)?;
        let mut files = self.files.lock().unwrap();
        let bytes = files.remove(from).ok_or(io::ErrorKind::NotFound)?;
        files.insert(to.to_path_buf(), bytes);
        Ok(())
    }
    fn create_dir_all(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn remove_dir_all(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn exists(&self, path: &Path) -> bool {
        self.files.lock().unwrap().contains_key(path)
    }
    fn list_dir(&self, _: &Path) -> io::Result<Vec<DirEntry>> {
        Ok(Vec::new())
    }
    fn symlink(&self, target: &str, link: &Path, kind: LinkKind) -> io::Result<()> {
        self.links.lock().unwrap().push((target.to_owned(), link.to_path_buf(), kind));
        Ok(())
    }
    fn set_executable(&self, _: &Path, _: bool) -> io::Result<()> {
        Ok(())
    }
    fn free_bytes(&self, _: &Path) -> io::Result<u64> {
        Ok(u64::MAX)
    }
}

fn attempt(fail: Fail) -> (io::Result<()>, Vec<u8>) {
    let disk = MemDisk::new(fail);
    let path = Path::new("/install/state.toml");
    let result = disk.write_atomic(path, b"new");
    (result, disk.read(path).unwrap())
}

#[test]
fn a_clean_write_replaces_the_file() {
    let (result, bytes) = attempt(Fail::Nothing);
    assert!(result.is_ok());
    assert_eq!(bytes, b"new");
}

#[test]
fn a_failure_up_to_the_rename_keeps_the_old_content() {
    for fail in [Fail::Write, Fail::FsyncFile, Fail::Rename] {
        let (result, bytes) = attempt(fail);
        assert!(result.is_err());
        assert_eq!(bytes, b"old");
    }
}

#[test]
fn a_failed_folder_fsync_reports_an_error_after_the_replacement() {
    let (result, bytes) = attempt(Fail::FsyncDir);
    assert!(result.is_err());
    assert_eq!(bytes, b"new");
}

#[test]
fn symlink_records_the_kind_it_was_given() {
    let disk = MemDisk::new(Fail::Nothing);
    disk.symlink("lib/real", Path::new("/install/dir-link"), LinkKind::Dir).unwrap();
    disk.symlink("bin/tool", Path::new("/install/file-link"), LinkKind::File).unwrap();
    assert_eq!(
        *disk.links.lock().unwrap(),
        [
            ("lib/real".to_owned(), PathBuf::from("/install/dir-link"), LinkKind::Dir),
            ("bin/tool".to_owned(), PathBuf::from("/install/file-link"), LinkKind::File),
        ]
    );
}
