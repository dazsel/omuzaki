//! The session store (D9): [`SessionState`] in `session.toml` in the local data folder.
//!
//! The two halves of the crash-loop guard, [`begin_open`](SessionStore::begin_open) and
//! [`opened`](SessionStore::opened), write the file **now**, under the file's lock, with `write_atomic` (which syncs),
//! and return the error. Everything else is debounced: a writer thread writes [`DEBOUNCE`] after the last change (at
//! most [`MAX_DELAY`] under a burst), a failed write is kept and retried with the next change, and
//! [`flush`](SessionStore::flush) at exit reports it. Dropping the store writes what is pending.
//!
//! A file that does not parse is moved aside to `session.toml.broken-<utc>` and the state starts empty: losing the
//! recent list is better than a start that fails.

#![allow(
    clippy::disallowed_methods,
    reason = "the app's own session file in its local data folder, never a path an op was given"
)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use omuzaki_core::fsx::write_atomic;
use omuzaki_settings_api::error::{SettingError, SettingErrorKind};
use omuzaki_settings_api::session::{RecentProject, SessionState, SessionStore, SetupProgress, WindowGeometry};
use serde::{Deserialize, Serialize};

use crate::backend::{LOCK_WAIT, lock_beside, move_aside_path, storage};
use crate::locked;
use crate::store::{DEBOUNCE, MAX_DELAY};

/// The file's name in the local data folder.
pub const SESSION_FILE: &str = "session.toml";

/// The layout this build writes.
const FORMAT: u32 = 1;

/// The whole seconds of the wall clock: when a project was opened.
pub type Now = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The file as it is written: `format` first, then the state.
#[derive(Serialize)]
struct FileOut<'a> {
    format: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_project: Option<&'a Path>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_seen_version: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    opening: Option<&'a Path>,
    recent: &'a [RecentProject],
    windows: &'a BTreeMap<String, WindowGeometry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup: Option<&'a SetupProgress>,
}

/// The file as it is read; keys this build does not know are ignored, and missing ones are empty.
#[derive(Deserialize)]
struct FileIn {
    #[serde(default)]
    format: u32,
    #[serde(default)]
    last_project: Option<PathBuf>,
    #[serde(default)]
    last_seen_version: Option<String>,
    #[serde(default)]
    opening: Option<PathBuf>,
    #[serde(default)]
    recent: Vec<RecentProject>,
    #[serde(default)]
    windows: BTreeMap<String, WindowGeometry>,
    #[serde(default)]
    setup: Option<SetupProgress>,
}

fn encode(state: &SessionState) -> Result<String, String> {
    let file = FileOut {
        format: FORMAT,
        last_project: state.last_project.as_deref(),
        last_seen_version: state.last_seen_version.as_deref(),
        opening: state.opening.as_deref(),
        recent: &state.recent,
        windows: &state.windows,
        setup: state.setup.as_ref(),
    };
    toml::to_string(&file).map_err(|e| e.to_string())
}

fn decode(text: &str) -> Result<SessionState, String> {
    let file: FileIn = toml::from_str(text).map_err(|e| e.to_string())?;
    if file.format > FORMAT {
        return Err(format!("written by a newer Omuzaki (format {})", file.format));
    }
    let mut state = SessionState::default();
    state.last_project = file.last_project;
    state.last_seen_version = file.last_seen_version;
    state.opening = file.opening;
    state.recent = file.recent;
    state.windows = file.windows;
    state.setup = file.setup;
    Ok(state)
}

/// What the store holds between writes.
struct Data {
    state: SessionState,
    /// A change not yet in the file.
    dirty: bool,
    first_dirty: Option<Instant>,
    last_change: Instant,
    /// After a failed write, not before this.
    not_before: Instant,
    error: Option<SettingError>,
    closing: bool,
}

impl Data {
    fn touch(&mut self) {
        let now = Instant::now();
        self.dirty = true;
        self.first_dirty.get_or_insert(now);
        self.last_change = now;
    }

    fn due(&self) -> Instant {
        let first = self.first_dirty.unwrap_or(self.last_change);
        (self.last_change + DEBOUNCE).min(first + MAX_DELAY).max(self.not_before)
    }
}

struct Shared {
    path: PathBuf,
    data: Mutex<Data>,
    wake: Condvar,
    now: Now,
    /// One write at a time, so the writer thread and a durable write never race on the file.
    gate: Mutex<()>,
}

impl Shared {
    /// Writes the state as it is now. Holds the gate and the file's lock; the state's lock only to copy it.
    fn write(&self) -> Result<(), SettingError> {
        let _one = locked(&self.gate);
        let state = locked(&self.data).state.clone();
        let text = encode(&state).map_err(|reason| SettingError::new("", SettingErrorKind::Storage { reason }))?;
        let written = lock_beside(&self.path, LOCK_WAIT)
            .and_then(|_lock| write_atomic(&self.path, &text).map_err(|e| storage(&self.path, "write", &e)));
        let mut data = locked(&self.data);
        match &written {
            Ok(()) if data.state == state => {
                data.dirty = false;
                data.first_dirty = None;
                data.error = None;
            }
            // Changed while it was written: the newer state is still owed.
            Ok(()) => data.error = None,
            Err(e) => {
                data.error = Some(e.clone());
                data.not_before = Instant::now() + MAX_DELAY;
            }
        }
        written
    }

    fn run_writer(&self) {
        let mut data = locked(&self.data);
        loop {
            while !data.dirty && !data.closing {
                data = self.wake.wait(data).unwrap_or_else(PoisonError::into_inner);
            }
            if data.closing {
                return;
            }
            let wait = data.due().saturating_duration_since(Instant::now());
            if wait > Duration::ZERO {
                data = self.wake.wait_timeout(data, wait).unwrap_or_else(PoisonError::into_inner).0;
                continue;
            }
            drop(data);
            // A failure is kept in `error` and retried; `flush` reports it.
            drop(self.write());
            data = locked(&self.data);
        }
    }

    /// Applies `change` and lets the writer write it later.
    fn debounced(&self, change: impl FnOnce(&mut SessionState)) {
        let mut data = locked(&self.data);
        change(&mut data.state);
        data.touch();
        self.wake.notify_all();
    }

    /// Applies `change` and writes it now.
    fn durable(&self, change: impl FnOnce(&mut SessionState)) -> Result<(), SettingError> {
        {
            let mut data = locked(&self.data);
            change(&mut data.state);
            data.touch();
        }
        self.write()
    }
}

/// The session store over `session.toml` (see the [module docs](self)).
pub struct SessionFile {
    shared: Arc<Shared>,
    writer: Option<JoinHandle<()>>,
    /// The name a damaged file was moved to when the store opened.
    recovered: Option<String>,
}

impl SessionFile {
    /// The store over `session.toml` in `data`, stamping opened projects with the system clock.
    #[must_use]
    pub fn open(data: &Path) -> Self {
        let now: Now = Arc::new(|| {
            SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        });
        Self::open_with(data, now)
    }

    /// The store over `session.toml` in `data`, stamping opened projects with `now`.
    #[must_use]
    pub fn open_with(data: &Path, now: Now) -> Self {
        let path = data.join(SESSION_FILE);
        let (state, recovered) = match fs::read_to_string(&path) {
            Ok(text) => match decode(&text) {
                Ok(state) => (state, None),
                Err(_) => (SessionState::default(), move_aside_path(&path).ok()),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => (SessionState::default(), None),
            Err(_) => (SessionState::default(), move_aside_path(&path).ok()),
        };
        let start = Instant::now();
        let shared = Arc::new(Shared {
            path,
            data: Mutex::new(Data {
                state,
                dirty: false,
                first_dirty: None,
                last_change: start,
                not_before: start,
                error: None,
                closing: false,
            }),
            wake: Condvar::new(),
            now,
            gate: Mutex::new(()),
        });
        let worker = Arc::clone(&shared);
        let writer = thread::Builder::new().name("session-writer".to_owned()).spawn(move || worker.run_writer()).ok();
        Self { shared, writer, recovered }
    }

    /// The name the damaged `session.toml` was moved to when the store opened, if it was.
    #[must_use]
    pub fn recovered(&self) -> Option<&str> {
        self.recovered.as_deref()
    }

    /// The file this store writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.shared.path
    }
}

impl Drop for SessionFile {
    fn drop(&mut self) {
        locked(&self.shared.data).closing = true;
        self.shared.wake.notify_all();
        if let Some(writer) = self.writer.take() {
            drop(writer.join());
        }
        if locked(&self.shared.data).dirty {
            drop(self.shared.write());
        }
    }
}

impl SessionStore for SessionFile {
    fn state(&self) -> SessionState {
        locked(&self.shared.data).state.clone()
    }

    fn begin_open(&self, project: &Path) -> Result<(), SettingError> {
        self.shared.durable(|state| state.begin_open(project))
    }

    fn opened(&self, project: &Path, name: &str) -> Result<(), SettingError> {
        let at = (self.shared.now)();
        self.shared.durable(|state| state.opened(project, name, at))
    }

    fn set_setup(&self, progress: Option<SetupProgress>) -> Result<(), SettingError> {
        self.shared.durable(|state| state.set_setup(progress))
    }

    fn forget(&self, project: &Path) {
        self.shared.debounced(|state| state.forget(project));
    }

    fn restore_recent(&self, entry: RecentProject, index: usize, last: bool) {
        self.shared.debounced(|state| state.restore_recent(entry, index, last));
    }

    fn set_window(&self, id: &str, geometry: WindowGeometry) {
        self.shared.debounced(|state| state.set_window(id, geometry));
    }

    fn mark_version_seen(&self, version: &str) {
        self.shared.debounced(|state| state.mark_version_seen(version));
    }

    fn flush(&self) -> Result<(), SettingError> {
        if locked(&self.shared.data).dirty { self.shared.write() } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;

    fn clock() -> (Arc<AtomicI64>, Now) {
        let at = Arc::new(AtomicI64::new(1_000));
        let read = Arc::clone(&at);
        (at, Arc::new(move || read.load(Ordering::SeqCst)))
    }

    fn read(dir: &Path) -> SessionState {
        decode(&fs::read_to_string(dir.join(SESSION_FILE)).unwrap_or_default()).unwrap_or_default()
    }

    #[test]
    fn the_opening_guard_is_on_disk_before_begin_open_returns_even_if_the_process_dies_next() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let (_, now) = clock();
        let store = SessionFile::open_with(tmp.path(), Arc::clone(&now));
        store.begin_open(Path::new("/shows/spring")).unwrap_or_else(|e| panic!("{e}"));
        // A crash: the store is never dropped, so nothing more is written.
        std::mem::forget(store);
        let again = SessionFile::open_with(tmp.path(), now);
        assert_eq!(again.state().opening.as_deref(), Some(Path::new("/shows/spring")));
    }

    #[test]
    fn opened_clears_the_guard_durably_and_orders_recent_newest_first() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let (at, now) = clock();
        let store = SessionFile::open_with(tmp.path(), now);
        for (i, project) in ["/p/a", "/p/b", "/p/a"].into_iter().enumerate() {
            at.store(1_000 + i64::try_from(i).unwrap_or(0), Ordering::SeqCst);
            store.begin_open(Path::new(project)).unwrap_or_else(|e| panic!("{e}"));
            store.opened(Path::new(project), project).unwrap_or_else(|e| panic!("{e}"));
        }
        let on_disk = read(tmp.path());
        assert_eq!(on_disk.opening, None);
        let paths: Vec<_> = on_disk.recent.iter().map(|r| r.path.display().to_string()).collect();
        assert_eq!(paths, ["/p/a", "/p/b"]);
        assert_eq!(on_disk.recent[0].opened, 1_002);
        assert_eq!(on_disk.last_project.as_deref(), Some(Path::new("/p/a")));
    }

    #[test]
    fn debounced_changes_reach_the_file_on_flush_and_on_drop() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let (_, now) = clock();
        let store = SessionFile::open_with(tmp.path(), Arc::clone(&now));
        store.opened(Path::new("/p/a"), "A").unwrap_or_else(|e| panic!("{e}"));
        store.set_window("main", WindowGeometry::new(80, 60, 1760, 960).on_monitor("Left"));
        store.flush().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read(tmp.path()).windows["main"].w, 1760);
        store.forget(Path::new("/p/a"));
        store.mark_version_seen("0.9.0");
        drop(store);
        let on_disk = read(tmp.path());
        assert!(on_disk.recent.is_empty() && on_disk.last_project.is_none());
        assert_eq!(on_disk.last_seen_version.as_deref(), Some("0.9.0"));
    }

    #[test]
    fn the_writer_thread_writes_a_debounced_change_by_itself() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let (_, now) = clock();
        let store = SessionFile::open_with(tmp.path(), now);
        store.mark_version_seen("1.2.0");
        let deadline = Instant::now() + Duration::from_secs(20);
        while read(tmp.path()).last_seen_version.is_none() {
            assert!(Instant::now() < deadline, "the writer never wrote");
            thread::sleep(Duration::from_millis(20));
        }
        drop(store);
    }

    #[test]
    fn a_damaged_file_is_moved_aside_and_the_state_starts_empty() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        fs::write(tmp.path().join(SESSION_FILE), "recent = [[[ not toml").unwrap_or_else(|e| panic!("{e}"));
        let (_, now) = clock();
        let store = SessionFile::open_with(tmp.path(), now);
        assert_eq!(store.state(), SessionState::default());
        let moved = store.recovered().unwrap_or_default().to_owned();
        assert!(moved.starts_with("session.toml.broken-"), "{moved}");
        assert!(tmp.path().join(&moved).exists() && !tmp.path().join(SESSION_FILE).exists());
    }

    #[test]
    fn a_write_that_fails_is_returned_by_begin_open_and_by_flush() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        // The data folder is a file, so nothing can be written under it.
        let blocked = tmp.path().join("data");
        fs::write(&blocked, b"a file").unwrap_or_else(|e| panic!("{e}"));
        let (_, now) = clock();
        let store = SessionFile::open_with(&blocked, now);
        assert!(store.begin_open(Path::new("/p/a")).is_err(), "the caller must not start loading");
        store.forget(Path::new("/p/a"));
        assert!(store.flush().is_err());
        assert_eq!(store.state().opening.as_deref(), Some(Path::new("/p/a")), "the state is kept in memory");
    }

    #[test]
    fn the_format_reads_back_and_keeps_its_shape() {
        let mut state = SessionState::default();
        state.opened(Path::new("/shows/spring"), "Spring show", 1_793_000_000);
        state.set_window("main", WindowGeometry::new(80, 60, 1760, 960).maximised(true));
        state.mark_version_seen("0.8.2");
        let text = encode(&state).unwrap_or_else(|e| panic!("{e}"));
        assert!(text.starts_with("format = 1\n"), "{text}");
        assert_eq!(decode(&text), Ok(state));
        assert!(decode("format = 2\n").is_err(), "a newer layout is not read as this one");
        assert_eq!(decode("format = 1\nfuture_key = 3\n"), Ok(SessionState::default()));
    }

    #[test]
    fn setup_progress_is_on_disk_before_set_setup_returns() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let (_, now) = clock();
        let store = SessionFile::open_with(tmp.path(), Arc::clone(&now));
        let progress = SetupProgress::new("input", vec!["calibrate".to_owned()], false);
        store.set_setup(Some(progress.clone())).unwrap_or_else(|e| panic!("{e}"));
        std::mem::forget(store);
        assert_eq!(read(tmp.path()).setup, Some(progress));
    }

    #[test]
    fn a_file_written_before_setup_existed_reads_unchanged_and_means_never_decided() {
        // The layout as the first build wrote it: no `[setup]` table.
        let golden = "format = 1\nlast_project = \"/shows/spring\"\n\n[[recent]]\npath = \"/shows/spring\"\n\
                      name = \"Spring show\"\nopened = 1793000000\n\n[windows]\n";
        let state = decode(golden).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(state.setup, None);
        assert_eq!(state.recent[0].name, "Spring show");
        assert!(!state.setup_needed(), "an existing user with history never sees setup");
        assert_eq!(encode(&state).unwrap_or_else(|e| panic!("{e}")), golden, "and writes back the same bytes");
    }
}
