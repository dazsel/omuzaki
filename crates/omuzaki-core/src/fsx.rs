//! Crash-safe file writes.
//!
//! [`write_atomic`] replaces a whole file in one step, so a reader (or a crash) sees the old text or the new text,
//! never half of one. It is the shared form of the save that project files use; the callers decide what the path
//! means and whether links in it are allowed.

use std::collections::hash_map::{DefaultHasher, RandomState};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(windows)]
use std::{thread, time::Duration};

/// Delays before each retry of a rename that Windows refuses because another process holds the target open (an
/// editor, a sync client, a virus scanner). Three retries, about 100 ms in all, then the error is returned.
#[cfg(windows)]
const RENAME_RETRY_DELAYS: [Duration; 3] =
    [Duration::from_millis(20), Duration::from_millis(30), Duration::from_millis(50)];

/// Windows' `ERROR_SHARING_VIOLATION`: the target is open in another process without sharing it.
#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;

/// How many names to try for a temporary file before giving up. A name is taken only by a file that already has it,
/// which the process id, the per-run nonce and the counter make very unlikely, so one try is almost always enough.
const TEMP_NAME_ATTEMPTS: u32 = 100;

/// Counts the temporary names handed out, so two saves in one process never ask for the same name.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A value that differs between runs of the process, so two processes saving the same file never share a name even
/// when the operating system reuses a process id. The standard library seeds its hash maps from the operating system,
/// which gives a value nobody else can predict; no extra dependency is needed.
fn run_nonce() -> u64 {
    static NONCE: OnceLock<u64> = OnceLock::new();
    *NONCE.get_or_init(|| RandomState::new().build_hasher().finish())
}

/// Replaces the file at `path` with `text`, creating any missing folders first.
///
/// The text goes to a new temporary file next to the target, named `.<hash of name>-<pid>-<nonce>-<n>.tmp`. Each call makes
/// its own, so concurrent saves of one path never write into each other's file. The temporary file is flushed to
/// disk, then renamed over the target. The rename replaces the target in one step on unix and on Windows
/// (`std::fs::rename` over an existing file, never a remove followed by a rename), so no reader ever finds the file
/// missing or half written. If the write or the rename fails, the temporary file this call created is removed and the
/// old file, if there was one, is left as it was.
///
/// On unix the parent folder is flushed after the rename, so the new name survives a crash too. Windows does not offer
/// that, so only the file's own contents are flushed there. Folders created on the way are not flushed.
///
/// A crash can leave a temporary file behind, and nothing removes it: a later save makes its own name and leaves that
/// file alone. The new file gets the default permissions of the process (the umask on unix), not those of the file it
/// replaces, and it is a new file for any other process that has the old one open.
///
/// What it does not do: take a lock. Concurrent saves each leave a complete version, and the last rename wins; nothing
/// orders them. Links are not refused here (a symlink at `path` is replaced, and a symlinked folder is followed); a
/// caller that needs that check makes it before calling. The temporary file is created with `create_new`, so an
/// existing link under its name is never written through.
///
/// # Errors
///
/// Returns an error if `path` has no file name, or if any step fails. An error from the folder flush on unix comes
/// after the rename, so the new text is already in place when it is returned.
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)?;
    let (tmp, file) = create_temp(parent, name)?;
    let staged = write_and_sync(file, text).and_then(|()| rename_over(&tmp, path));
    if let Err(e) = staged {
        // Only the file this call created, so removing it cannot touch another save's work. Best effort: the original
        // error is the one worth reporting.
        drop(fs::remove_file(&tmp));
        return Err(e);
    }
    // Windows cannot open a folder to flush it, and its rename is already journaled by the file system.
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Creates a new temporary file for a save of `name` in `parent`, and returns its path with the open file.
///
/// `create_new` never opens an existing file, so a name that is already taken is skipped and the next one is tried.
fn create_temp(parent: &Path, name: &OsStr) -> io::Result<(PathBuf, File)> {
    for _ in 0..TEMP_NAME_ATTEMPTS {
        let tmp = parent.join(temp_name(name));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free name for a temporary file"))
}

/// The file name for one save: a hidden name made of a hash of the target's name, the process id, the run's nonce and
/// a counter that never repeats in this process.
///
/// The target's own name is not copied in. A long target name plus the suffix could pass the 255-byte limit most file
/// systems put on one name; the hash keeps the name short whatever the target's length, and the suffix keeps it unique.
fn temp_name(name: &OsStr) -> OsString {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    format!(".{:016x}-{}-{:016x}-{n}.tmp", hasher.finish(), std::process::id(), run_nonce()).into()
}

/// Writes `text` into `file` and flushes it to disk. Taking the file by value closes it before the rename.
fn write_and_sync(mut file: File, text: &str) -> io::Result<()> {
    file.write_all(text.as_bytes())?;
    // Flushed before the rename, so the new name never points at contents that are not yet on disk.
    file.sync_all()
}

/// Renames `from` over `to`, replacing it. The target is never removed first: that would leave a moment with no file.
#[cfg(not(windows))]
fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

/// Renames `from` over `to`, retrying while another process holds `to` open. Only a refusal that a moment can cure is
/// retried; every other error is returned at once.
#[cfg(windows)]
fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    let mut delays = RENAME_RETRY_DELAYS.iter();
    loop {
        match fs::rename(from, to) {
            Err(e) if is_sharing_refusal(&e) => match delays.next() {
                Some(delay) => thread::sleep(*delay),
                None => return Err(e),
            },
            other => return other,
        }
    }
}

/// Whether a failed rename was refused because the target is open elsewhere, rather than being wrong.
#[cfg(windows)]
fn is_sharing_refusal(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder for one test, under the system temporary folder and named after the process and the test. It is
    /// removed when the test ends, including when an assertion fails.
    struct Scratch {
        dir: std::path::PathBuf,
    }

    impl Scratch {
        fn new(test: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("omuzaki-fsx-{}-{test}", std::process::id()));
            drop(fs::remove_dir_all(&dir));
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.dir));
        }
    }

    /// Makes a folder read-only for the test and puts it back on drop, so the scratch folder can be removed.
    #[cfg(unix)]
    struct ReadOnly<'a>(&'a Path);

    #[cfg(unix)]
    impl ReadOnly<'_> {
        fn lock(dir: &Path) -> io::Result<ReadOnly<'_>> {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o555))?;
            Ok(ReadOnly(dir))
        }
    }

    #[cfg(unix)]
    impl Drop for ReadOnly<'_> {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            drop(fs::set_permissions(self.0, fs::Permissions::from_mode(0o755)));
        }
    }

    /// The names in `dir`, sorted, so a test can see exactly what was left behind.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn replaces_an_existing_file() {
        let scratch = Scratch::new("replaces");
        let path = scratch.dir.join("doc.toml");
        write_atomic(&path, "x = 1\n").unwrap();
        write_atomic(&path, "x = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "x = 2\n");
    }

    #[test]
    fn creates_missing_folders() {
        let scratch = Scratch::new("folders");
        let path = scratch.dir.join("a").join("b").join("c.toml");
        write_atomic(&path, "ok\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "ok\n");
    }

    #[test]
    fn leaves_no_temporary_file_after_a_success() {
        let scratch = Scratch::new("no_tmp_success");
        let path = scratch.dir.join("doc.toml");
        write_atomic(&path, "x = 1\n").unwrap();
        write_atomic(&path, "x = 2\n").unwrap();
        assert_eq!(names(&scratch.dir), vec!["doc.toml"]);
    }

    #[test]
    fn leaves_a_stale_temporary_file_alone() {
        // A temporary file left by a crash belongs to that crash: the save makes its own name and does not remove it.
        let scratch = Scratch::new("stale_tmp");
        let path = scratch.dir.join("doc.toml");
        fs::write(scratch.dir.join(".doc.toml.tmp"), "left over by a crash").unwrap();
        write_atomic(&path, "fresh\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "fresh\n");
        assert_eq!(fs::read_to_string(scratch.dir.join(".doc.toml.tmp")).unwrap(), "left over by a crash");
        assert_eq!(names(&scratch.dir), vec![".doc.toml.tmp", "doc.toml"]);
    }

    #[test]
    fn failed_rename_leaves_no_temporary_file() {
        // A non-empty folder at the target makes the rename fail on every platform, after the temporary file exists.
        let scratch = Scratch::new("failed_rename");
        let target = scratch.dir.join("doc.toml");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), "").unwrap();
        assert!(write_atomic(&target, "x = 1\n").is_err());
        assert_eq!(names(&scratch.dir), vec!["doc.toml"]);
        assert_eq!(names(&target), vec!["keep"]);
    }

    #[cfg(unix)]
    #[test]
    fn failed_write_leaves_the_old_file_intact() {
        let scratch = Scratch::new("old_intact");
        let locked = scratch.dir.join("locked");
        fs::create_dir(&locked).unwrap();
        let target = locked.join("doc.toml");
        fs::write(&target, b"old\n").unwrap();
        let _restore = ReadOnly::lock(&locked).unwrap();
        // Root writes into read-only folders, so the test could not fail there; say so and skip rather than fail.
        if fs::write(locked.join("probe"), "").is_ok() {
            drop(fs::remove_file(locked.join("probe")));
            eprintln!("skipped: permissions are not enforced here (running as root?)");
            return;
        }
        assert!(write_atomic(&target, "new\n").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old\n");
        assert_eq!(names(&locked), vec!["doc.toml"]);
    }

    #[test]
    fn concurrent_saves_never_show_a_mixed_file() {
        // Writers share one target and each writes a different, repeated text many times. A reader running alongside
        // must always see one whole text, never a prefix of one save and the rest of another.
        const WRITERS: usize = 4;
        const WRITES: usize = 50;
        const REPEATS: usize = 400;
        let scratch = Scratch::new("concurrent");
        let path = scratch.dir.join("doc.toml");
        write_atomic(&path, &"writer 9 write 000\n".repeat(REPEATS)).unwrap();
        let finished = AtomicU64::new(0);
        std::thread::scope(|s| {
            for w in 0..WRITERS {
                let (path, finished) = (&path, &finished);
                s.spawn(move || {
                    for seq in 0..WRITES {
                        let text = format!("writer {w} write {seq:03}\n").repeat(REPEATS);
                        write_atomic(path, &text).unwrap();
                    }
                    finished.fetch_add(1, Ordering::SeqCst);
                });
            }
            while finished.load(Ordering::SeqCst) < WRITERS as u64 {
                let seen = fs::read_to_string(&path).unwrap();
                let first = seen.split_inclusive('\n').next().unwrap_or("");
                assert!(first.starts_with("writer "), "unexpected contents: {first:?}");
                assert_eq!(seen, first.repeat(REPEATS), "a read saw parts of two saves");
            }
        });
        assert_eq!(names(&scratch.dir), vec!["doc.toml"]);
    }

    #[test]
    fn a_long_target_name_still_saves() {
        // The temporary name must fit a file name (255 bytes on most systems) even when the target's name is close to
        // that limit, so it cannot copy the target's name in.
        let scratch = Scratch::new("long_name");
        let long = "n".repeat(250);
        let path = scratch.dir.join(&long);
        write_atomic(&path, "ok\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "ok\n");
        assert_eq!(names(&scratch.dir), vec![long]);
    }

    #[test]
    fn rejects_a_path_without_a_file_name() {
        let err = write_atomic(Path::new(".."), "x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
