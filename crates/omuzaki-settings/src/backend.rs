//! Where the bytes go: the private [`Backend`] trait and the file backend (D5).
//!
//! A write takes an OS lock on a sibling `<file>.lock`, holds it across the re-read, the caller's merge and the atomic
//! replace, so two processes never merge from the same old text. The lock file is never the data file and is never
//! deleted.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use omuzaki_core::fsx::write_atomic;
use omuzaki_settings_api::error::{SettingError, SettingErrorKind};
use omuzaki_settings_api::meta::SettingScope;

use crate::paths::Paths;

/// How long a write waits for another process's lock before it reports and keeps its keys dirty.
pub(crate) const LOCK_WAIT: Duration = Duration::from_secs(2);

/// How often a waiting write tries the lock again.
const LOCK_POLL: Duration = Duration::from_millis(10);

/// A temporary file this old belongs to a process that died mid-write: a live write takes milliseconds.
const STALE_TEMP_AGE: Duration = Duration::from_secs(60);

/// A rename over a file another process holds open can fail briefly on Windows (D5): retry this often.
const RENAME_ATTEMPTS: u32 = 3;
const RENAME_PAUSE: Duration = Duration::from_millis(50);

/// What the merge closure decided about the text it was shown.
pub(crate) enum Edited {
    /// Replace the file with this text.
    Write(String),
    /// The file already says it.
    Unchanged,
    /// The file is not TOML: move it aside and show the closure an empty file.
    Unparsable,
}

/// Storage for the two settings files.
pub(crate) trait Backend: Send + Sync {
    /// The file's text, or `None` when it does not exist.
    fn read(&self, scope: SettingScope) -> Result<Option<String>, SettingError>;

    /// Renames the file to `<name>.broken-<utc>` and returns that name.
    fn quarantine(&self, scope: SettingScope) -> Result<String, SettingError>;

    /// Under the file's lock: re-reads it, shows the text to `edit` and writes what it returns. `backup` is the app
    /// version for the `.bak-<version>` copy to make first, if the file exists and has none yet.
    fn update(
        &self,
        scope: SettingScope,
        backup: Option<&str>,
        edit: &mut dyn FnMut(Option<&str>) -> Result<Edited, String>,
    ) -> Result<(), SettingError>;

    /// The file's name, for messages.
    fn file_name(&self, scope: SettingScope) -> &'static str;
}

/// The real files: `settings.toml` in the config folder, `machine.toml` in the local data folder.
pub(crate) struct FileBackend {
    paths: Paths,
    lock_wait: Duration,
    /// Runs under the lock after the re-read: the forced-interleaving test pauses a store here.
    #[cfg(test)]
    pub(crate) after_read: Option<Box<dyn Fn() + Send + Sync>>,
}

impl FileBackend {
    pub(crate) fn new(paths: Paths) -> Self {
        Self {
            paths,
            lock_wait: LOCK_WAIT,
            #[cfg(test)]
            after_read: None,
        }
    }

    /// A shorter lock wait, so a test of the timeout does not take two seconds.
    #[cfg(test)]
    pub(crate) fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    fn path(&self, scope: SettingScope) -> PathBuf {
        match scope {
            SettingScope::User => self.paths.config().join(self.file_name(scope)),
            SettingScope::Machine => self.paths.data().join(self.file_name(scope)),
        }
    }

    /// Takes the exclusive lock, trying for at most `lock_wait`.
    fn lock(&self, path: &Path) -> Result<File, SettingError> {
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        let lock_path = PathBuf::from(name);
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|e| storage(&lock_path, "create the folder", &e))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| storage(&lock_path, "open the lock file", &e))?;
        let deadline = Instant::now() + self.lock_wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => thread::sleep(LOCK_POLL),
                Err(TryLockError::WouldBlock) => {
                    let reason = format!("another process holds {} (waited {:?})", lock_path.display(), self.lock_wait);
                    return Err(SettingError::new("", SettingErrorKind::Storage { reason }));
                }
                Err(TryLockError::Error(e)) => return Err(storage(&lock_path, "lock", &e)),
            }
        }
    }
}

impl Backend for FileBackend {
    fn read(&self, scope: SettingScope) -> Result<Option<String>, SettingError> {
        let path = self.path(scope);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage(&path, "read", &e)),
        }
    }

    fn quarantine(&self, scope: SettingScope) -> Result<String, SettingError> {
        let path = self.path(scope);
        let stamp = utc_stamp(SystemTime::now());
        let mut name = format!("{}.broken-{stamp}", self.file_name(scope));
        let mut n = 1;
        while path.with_file_name(&name).exists() {
            name = format!("{}.broken-{stamp}-{n}", self.file_name(scope));
            n += 1;
        }
        fs::rename(&path, path.with_file_name(&name)).map_err(|e| storage(&path, "move aside", &e))?;
        Ok(name)
    }

    fn update(
        &self,
        scope: SettingScope,
        backup: Option<&str>,
        edit: &mut dyn FnMut(Option<&str>) -> Result<Edited, String>,
    ) -> Result<(), SettingError> {
        let path = self.path(scope);
        let _lock = self.lock(&path)?;
        remove_stale_temps(path.parent().unwrap_or(Path::new(".")));
        let mut current = self.read(scope)?;
        #[cfg(test)]
        if let Some(hook) = &self.after_read {
            hook();
        }
        let reason = |reason: String| SettingError::new("", SettingErrorKind::Storage { reason });
        let mut edited = edit(current.as_deref()).map_err(reason)?;
        if matches!(edited, Edited::Unparsable) {
            self.quarantine(scope)?;
            current = None;
            edited = edit(None).map_err(reason)?;
        }
        let Edited::Write(text) = edited else {
            return Ok(());
        };
        if let (Some(version), Some(_)) = (backup, &current) {
            let copy = path.with_file_name(format!("{}.bak-{version}", self.file_name(scope)));
            // Once per app version: an existing copy is the file as it was before the first migrated write.
            if !copy.exists() {
                fs::copy(&path, &copy).map_err(|e| storage(&copy, "back up to", &e))?;
            }
        }
        replace(&path, &text).map_err(|e| storage(&path, "write", &e))
    }

    fn file_name(&self, scope: SettingScope) -> &'static str {
        match scope {
            SettingScope::User => "settings.toml",
            SettingScope::Machine => "machine.toml",
        }
    }
}

fn storage(path: &Path, doing: &str, e: &io::Error) -> SettingError {
    let reason = format!("could not {doing} {}: {e}", path.display());
    SettingError::new("", SettingErrorKind::Storage { reason })
}

fn replace(path: &Path, text: &str) -> io::Result<()> {
    let mut attempt = 1;
    loop {
        match write_atomic(path, text) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && attempt < RENAME_ATTEMPTS => {
                attempt += 1;
                thread::sleep(RENAME_PAUSE);
            }
            done => return done,
        }
    }
}

/// Removes the temporary files of writers that died. Holding the lock means no live writer of this file exists, and a
/// writer of another file in the folder finishes in milliseconds, so only an old temp is stale. Best effort.
fn remove_stale_temps(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age >= STALE_TEMP_AGE);
        if old && entry.file_name().to_str().is_some_and(is_write_atomic_temp) {
            drop(fs::remove_file(entry.path()));
        }
    }
}

/// `omuzaki_core::fsx::write_atomic` names its temporary files `.<16 hex>-<pid>-<16 hex>-<n>.tmp`.
fn is_write_atomic_temp(name: &str) -> bool {
    let Some(core) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".tmp")) else {
        return false;
    };
    let parts: Vec<&str> = core.split('-').collect();
    let hex = |s: &str| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    matches!(parts.as_slice(), [h, pid, nonce, n] if hex(h) && number(pid) && hex(nonce) && number(n))
}

/// `YYYYMMDDTHHMMSSZ` in UTC, from the system clock: file-name safe, and no date crate for one name.
pub(crate) fn utc_stamp(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (i64::try_from(secs / 86_400).unwrap_or(0), secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z", rem / 3_600, rem % 3_600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stamp_is_utc_and_handles_leap_days() {
        let at = |secs| utc_stamp(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), "19700101T000000Z");
        assert_eq!(at(951_782_400 + 3_661), "20000229T010101Z");
        assert_eq!(at(1_776_265_200), "20260415T150000Z");
    }

    #[test]
    fn only_write_atomic_temp_names_count_as_temps() {
        assert!(is_write_atomic_temp(".0123456789abcdef-4242-fedcba9876543210-7.tmp"));
        assert!(!is_write_atomic_temp(".settings.toml.4242.tmp"));
        assert!(!is_write_atomic_temp("0123456789abcdef-4242-fedcba9876543210-7.tmp"));
        assert!(!is_write_atomic_temp(".0123456789abcdef-4242-fedcba9876543210-7.toml"));
    }

    #[test]
    fn a_stale_temp_is_removed_by_the_next_lock_holder_and_a_fresh_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(Paths::new(dir.path(), dir.path().join("data")));
        let stale = dir.path().join(".0123456789abcdef-4242-fedcba9876543210-1.tmp");
        let fresh = dir.path().join(".0123456789abcdef-4243-fedcba9876543210-2.tmp");
        for p in [&stale, &fresh] {
            fs::write(p, "half").unwrap();
        }
        let old = SystemTime::now() - STALE_TEMP_AGE - Duration::from_secs(5);
        File::options().write(true).open(&stale).unwrap().set_modified(old).unwrap();
        backend.update(SettingScope::User, None, &mut |_| Ok(Edited::Write("a = 1\n".into()))).unwrap();
        assert!(!stale.exists() && fresh.exists());
    }
}
