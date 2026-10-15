//! `StdDisk` over a temp folder. The table of cases is a function over `&dyn Disk`, so the same table runs against
//! `omuzaki-update-mock`'s `FakeDisk` once that crate is on `main`.

// Test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::io::ErrorKind;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use omuzaki_update_api::clock::WallClock;
use omuzaki_update_api::disk::{DirEntry, Disk, LinkKind};
use omuzaki_update_fs::clock::SystemWallClock;
use omuzaki_update_fs::disk::StdDisk;

fn kind<T>(result: std::io::Result<T>) -> ErrorKind {
    result.err().map(|e| e.kind()).unwrap()
}

fn entry(name: &str, is_dir: bool, is_symlink: bool) -> DirEntry {
    DirEntry { name: name.to_owned(), is_dir, is_symlink }
}

/// The behaviour every disk must share. `root` is an existing folder.
fn shared_cases(disk: &dyn Disk, root: &Path) {
    let dir = root.join("a/b");
    disk.create_dir_all(&dir).unwrap();
    disk.create_dir_all(&dir).unwrap();

    // write, read, truncate on rewrite, fsync of a file and of a folder.
    let file = dir.join("f");
    disk.write(&file, b"long text").unwrap();
    disk.write(&file, b"short").unwrap();
    disk.fsync(&file).unwrap();
    disk.fsync(&dir).unwrap();
    assert_eq!(disk.read(&file).unwrap(), b"short");
    assert_eq!(kind(disk.read(&dir.join("none"))), ErrorKind::NotFound);
    assert_eq!(kind(disk.fsync(&dir.join("none"))), ErrorKind::NotFound);
    assert_eq!(kind(disk.write(&root.join("no/such/dir/f"), b"x")), ErrorKind::NotFound);

    // A file in the way of a folder.
    assert!(disk.create_dir_all(&file.join("deeper")).is_err());

    // rename replaces a file whole and fails on a missing source.
    let other = dir.join("g");
    disk.write(&other, b"other").unwrap();
    disk.rename(&other, &file).unwrap();
    assert_eq!(disk.read(&file).unwrap(), b"other");
    assert!(!disk.exists(&other));
    assert_eq!(kind(disk.rename(&other, &file)), ErrorKind::NotFound);

    // list_dir: sorted by name, with kinds.
    disk.create_dir_all(&dir.join("sub")).unwrap();
    disk.write(&dir.join("a-file"), b"").unwrap();
    assert_eq!(
        disk.list_dir(&dir).unwrap(),
        [entry("a-file", false, false), entry("f", false, false), entry("sub", true, false)]
    );
    assert_eq!(kind(disk.list_dir(&dir.join("none"))), ErrorKind::NotFound);

    // exists
    assert!(disk.exists(&dir) && disk.exists(&file) && !disk.exists(&dir.join("none")));

    // set_executable on a missing file is an error.
    assert_eq!(kind(disk.set_executable(&dir.join("none"), true)), ErrorKind::NotFound);
    disk.set_executable(&file, true).unwrap();
    disk.set_executable(&file, false).unwrap();

    // free space can be asked of a folder, not of a missing path.
    assert!(disk.free_bytes(&dir).unwrap() > 0);
    assert_eq!(kind(disk.free_bytes(&dir.join("none"))), ErrorKind::NotFound);

    // write_atomic replaces whole and leaves no temp file.
    let state = root.join("state.toml");
    disk.write_atomic(&state, b"one").unwrap();
    disk.write_atomic(&state, b"two, longer").unwrap();
    assert_eq!(disk.read(&state).unwrap(), b"two, longer");
    assert!(!disk.exists(&root.join("state.toml.tmp")));

    // remove_dir_all removes the tree, and a missing folder is an error.
    disk.remove_dir_all(&root.join("a")).unwrap();
    assert!(!disk.exists(&file));
    assert_eq!(kind(disk.remove_dir_all(&root.join("a"))), ErrorKind::NotFound);
}

#[test]
fn std_disk_behaves_as_the_table_says() {
    let tmp = tempfile::tempdir().unwrap();
    shared_cases(&StdDisk, tmp.path());
}

/// Links, where the platform allows creating one without special rights.
fn link_cases(disk: &dyn Disk, root: &Path) {
    disk.create_dir_all(&root.join("real")).unwrap();
    disk.write(&root.join("real/f"), b"x").unwrap();
    disk.symlink("real", &root.join("dir-link"), LinkKind::Dir).unwrap();
    disk.symlink("real/f", &root.join("file-link"), LinkKind::File).unwrap();
    disk.symlink("missing", &root.join("dangling"), LinkKind::File).unwrap();
    assert_eq!(kind(disk.symlink("real", &root.join("dir-link"), LinkKind::Dir)), ErrorKind::AlreadyExists);

    // Links are listed as links, not followed, and a dangling one still exists.
    assert_eq!(
        disk.list_dir(root).unwrap(),
        [
            entry("dangling", false, true),
            entry("dir-link", false, true),
            entry("file-link", false, true),
            entry("real", true, false)
        ]
    );
    assert!(disk.exists(&root.join("dangling")));

    // Removing a folder through a link is refused and leaves the target alone.
    assert!(disk.remove_dir_all(&root.join("dir-link")).is_err());
    assert_eq!(disk.read(&root.join("real/f")).unwrap(), b"x");
}

#[cfg(unix)]
#[test]
fn std_disk_links() {
    let tmp = tempfile::tempdir().unwrap();
    link_cases(&StdDisk, tmp.path());
}

#[cfg(unix)]
#[test]
fn set_executable_sets_and_clears_the_bits_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("tool");
    StdDisk.write(&file, b"#!/bin/sh\n").unwrap();
    let mode = || std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    let before = mode();
    StdDisk.set_executable(&file, true).unwrap();
    assert_eq!(mode() & 0o111, (before & 0o444) >> 2);
    assert_ne!(mode() & 0o100, 0);
    StdDisk.set_executable(&file, false).unwrap();
    assert_eq!(mode() & 0o111, 0);
}

#[test]
fn write_atomic_leaves_the_old_file_when_the_folder_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("missing-folder/state.toml");
    assert_eq!(kind(StdDisk.write_atomic(&path, b"x")), ErrorKind::NotFound);
    assert!(!tmp.path().join("missing-folder").exists());
}

#[test]
fn write_atomic_overwrites_a_stale_temp_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.toml");
    std::fs::write(tmp.path().join("state.toml.tmp"), b"left by a crash, longer than the new text").unwrap();
    StdDisk.write_atomic(&path, b"new").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    assert!(!tmp.path().join("state.toml.tmp").exists());
}

#[test]
fn system_wall_clock_is_within_a_second_of_system_time() {
    let expected = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap();
    let got = SystemWallClock.now().0;
    assert!((got - expected).abs() <= 1, "{got} vs {expected}");
}
