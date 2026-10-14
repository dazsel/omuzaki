//! One writer per project folder: `.omuzaki/lock` carries an OS advisory lock for as long as a [`ProjectLock`] lives.
//!
//! The lock is [`File::try_lock`]: `flock(LOCK_EX | LOCK_NB)` on Unix, and `LockFileEx` with exclusive and
//! fail-immediately flags on Windows. The OS releases it when the process exits, however it exits, so a crash can't
//! leave a stale lock.
//!
//! The holder's pid, host and start time go in a sibling `.omuzaki/lock.owner`, written after the lock is taken. The
//! lock file itself holds nothing: on Windows the lock blocks reads of the locked file, so a refused process reads the
//! owner file instead.
//!
//! # Limits
//!
//! - A lock taken inside WSL on a `/mnt` folder is invisible to a Windows process, and the other way round. Two Omuzaki
//!   instances on either side of that boundary can both open the same project.
//! - The owner file is written just after the lock is taken, so a process refused in that short window reads the
//!   previous holder's details, or none.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::StoreError;

/// The folder inside a project that holds the lock. Names starting with `.` are outside the document tree.
const LOCK_DIR: &str = ".omuzaki";
/// The lock file inside [`LOCK_DIR`]. It is only ever locked, never read or written.
const LOCK_FILE: &str = "lock";
/// The holder's details, inside [`LOCK_DIR`]. Unlocked, so a refused process can read it on every platform.
const OWNER_FILE: &str = "lock.owner";
/// What `ProjectLock::acquire` leaves in `.omuzaki/` besides `lock` and `lock.owner`: a `.gitignore` holding exactly `*\n`, so git never
/// sees the folder and nothing is added to the person's own ignore file. An existing `.gitignore` there is never rewritten.
const GITIGNORE_FILE: &str = ".gitignore";
/// Reported when the holder's host can't be named.
const UNKNOWN_HOST: &str = "unknown";

/// Exclusive use of a project folder. The project is open in this process until the value is dropped.
#[derive(Debug)]
#[must_use = "the project is unlocked as soon as this is dropped"]
pub struct ProjectLock {
    /// Held open: the OS lock lives as long as this handle does.
    _file: File,
}

impl ProjectLock {
    /// Takes the lock for the project in `dir`, creating `.omuzaki/lock` if needed, and records this process in
    /// `.omuzaki/lock.owner`.
    ///
    /// Fails with [`StoreError::Locked`] when another process (or another handle in this one) already holds it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Io`] when the lock folder or files can't be created, opened or written.
    pub fn acquire(dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let lock_dir = dir.as_ref().join(LOCK_DIR);
        fs::create_dir_all(&lock_dir).map_err(|source| StoreError::io(&lock_dir, source))?;
        let lock_path = lock_dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| StoreError::io(&lock_path, source))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(refusal(&lock_dir.join(OWNER_FILE))),
            Err(TryLockError::Error(source)) => return Err(StoreError::io(&lock_path, source)),
        }
        write_gitignore(&lock_dir)?;
        let owner_path = lock_dir.join(OWNER_FILE);
        fs::write(&owner_path, owner_text()).map_err(|source| StoreError::io(&owner_path, source))?;
        Ok(Self { _file: file })
    }
}

/// Writes `.omuzaki/.gitignore` when the folder has none. Called only by the lock holder, so nothing races it;
/// `create_new` still refuses to follow a link or to replace a file the person put there.
fn write_gitignore(lock_dir: &Path) -> Result<(), StoreError> {
    let path = lock_dir.join(GITIGNORE_FILE);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => file.write_all(b"*\n").map_err(|source| StoreError::io(&path, source)),
        Err(source) if source.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(StoreError::io(&path, source)),
    }
}

/// The `pid`, `host` and `since` lines a holder writes for a refused process to read.
fn owner_text() -> String {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("pid = {}\nhost = {}\nsince = {since}\n", std::process::id(), host_name())
}

/// The error for a lock that is already held. A missing or unreadable owner file leaves the holder unknown
/// (pid 0); the refusal itself still happens.
fn refusal(owner_path: &Path) -> StoreError {
    let text = fs::read_to_string(owner_path).unwrap_or_default();
    let owner = Owner::parse(&text);
    StoreError::Locked { pid: owner.pid, host: owner.host, since: owner.since }
}

/// The holder's details as read back from the owner file.
struct Owner {
    pid: u32,
    host: String,
    since: u64,
}

impl Owner {
    /// Reads the `key = value` lines; anything missing or unparsable stays unknown.
    fn parse(text: &str) -> Self {
        let mut owner = Self { pid: 0, host: UNKNOWN_HOST.to_owned(), since: 0 };
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                owner.set(key.trim(), value.trim());
            }
        }
        owner
    }

    /// Sets one field from a `key = value` line; unknown keys and bad numbers are ignored.
    fn set(&mut self, key: &str, value: &str) {
        match key {
            "pid" => self.pid = value.parse().unwrap_or(self.pid),
            "host" if !value.is_empty() => value.clone_into(&mut self.host),
            "since" => self.since = value.parse().unwrap_or(self.since),
            _ => {}
        }
    }
}

/// The machine's name: from the environment where the OS sets it, else the Linux kernel's hostname.
fn host_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|var| std::env::var(var).ok().filter(|name| !name.is_empty()))
        .or_else(|| fs::read_to_string("/proc/sys/kernel/hostname").ok().map(|name| name.trim().to_owned()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| UNKNOWN_HOST.to_owned())
        .replace(['\n', '\r'], " ")
}
