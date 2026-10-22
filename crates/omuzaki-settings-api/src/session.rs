//! Session state (D9): what was open, never what was running. It is *state*, not a setting: the app writes it as a
//! side effect, so it has no `set` op and no default, and it lives beside the machine file because paths and monitor
//! layouts are machine-bound.
//!
//! [`SessionState`] carries its own transitions so every store (the file store and the mock) applies the same rules
//! (newest first, at most [`MAX_RECENT`], one entry per path) and only decides when to write.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::SettingError;

/// How many recent projects are kept.
pub const MAX_RECENT: usize = 20;

/// A project in the Open Recent list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RecentProject {
    /// The project folder.
    pub path: PathBuf,
    /// Its display name.
    pub name: String,
    /// When it was opened, in Unix seconds (UTC).
    pub opened: i64,
}

impl RecentProject {
    /// An entry.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, name: impl Into<String>, opened: i64) -> Self {
        Self { path: path.into(), name: name.into(), opened }
    }
}

/// A window's place, used only if it still intersects a connected monitor (the shell checks).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct WindowGeometry {
    /// Left edge, in points.
    pub x: i32,
    /// Top edge, in points.
    pub y: i32,
    /// Width, in points.
    pub w: u32,
    /// Height, in points.
    pub h: u32,
    /// Whether it was maximised.
    pub maximised: bool,
    /// The monitor's name.
    pub monitor: Option<String>,
}

impl WindowGeometry {
    /// A window at `(x, y)` of size `w` by `h`, not maximised, on no named monitor.
    #[must_use]
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h, maximised: false, monitor: None }
    }

    /// Marks it maximised.
    #[must_use]
    pub fn maximised(mut self, maximised: bool) -> Self {
        self.maximised = maximised;
        self
    }

    /// Names its monitor.
    #[must_use]
    pub fn on_monitor(mut self, monitor: impl Into<String>) -> Self {
        self.monitor = Some(monitor.into());
        self
    }
}

/// Everything `session.toml` remembers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SessionState {
    /// The project open when the app last ran.
    pub last_project: Option<PathBuf>,
    /// Open Recent, newest first, at most [`MAX_RECENT`].
    pub recent: Vec<RecentProject>,
    /// Geometry per window id (`main`, `mascot`, each pop-out).
    pub windows: BTreeMap<String, WindowGeometry>,
    /// The last version whose release notes were dismissed (D10).
    pub last_seen_version: Option<String>,
    /// Set before a project opens, cleared after: if it is still set at the next start, the app died while opening it
    /// and the startup plan is Welcome, so there is no crash loop.
    pub opening: Option<PathBuf>,
}

impl SessionState {
    /// Marks `project` as being opened.
    pub fn begin_open(&mut self, project: &Path) {
        self.opening = Some(project.to_path_buf());
    }

    /// The project is open and shown: clears the guard, makes it the last project and moves it to the front of Recent.
    pub fn opened(&mut self, project: &Path, name: &str, at: i64) {
        self.opening = None;
        self.last_project = Some(project.to_path_buf());
        self.recent.retain(|r| r.path != project);
        self.recent.insert(0, RecentProject::new(project, name, at));
        self.recent.truncate(MAX_RECENT);
    }

    /// Removes `project` from Recent, and from `last_project` if it was that.
    pub fn forget(&mut self, project: &Path) {
        self.recent.retain(|r| r.path != project);
        if self.last_project.as_deref() == Some(project) {
            self.last_project = None;
        }
    }

    /// Puts a forgotten entry back at `index` of Recent (the end when the list is shorter), and makes it the last
    /// project again when `last`: the undo of [`forget`](Self::forget). An entry for the same path already there is
    /// replaced.
    pub fn restore_recent(&mut self, entry: RecentProject, index: usize, last: bool) {
        if last {
            self.last_project = Some(entry.path.clone());
        }
        self.recent.retain(|r| r.path != entry.path);
        let index = index.min(self.recent.len());
        self.recent.insert(index, entry);
        self.recent.truncate(MAX_RECENT);
    }

    /// Records a window's geometry.
    pub fn set_window(&mut self, id: &str, geometry: WindowGeometry) {
        self.windows.insert(id.to_owned(), geometry);
    }

    /// Records a dismissed version, but never lowers it: after a rollback the higher value stays so the roll-forward
    /// does not show the notes twice. Versions are compared by the full precedence rules of Semantic Versioning 2.0, so
    /// a pre-release is below its release (`0.9.0-beta.1` never replaces a recorded `0.9.0`) and build metadata is
    /// ignored. An equal version replaces the old text. A `version` that is not a semantic version is **never
    /// recorded**, whether the old record is absent, valid or damaged: it cannot be ranked, and persisting it would
    /// make the next comparison meaningless. A damaged old record is replaced by the next valid version.
    pub fn mark_version_seen(&mut self, version: &str) {
        let Ok(new) = Version::parse(version) else {
            return;
        };
        let keep = match self.last_seen_version.as_deref().map(Version::parse) {
            Some(Ok(old)) => old.cmp_precedence(&new) == Ordering::Greater,
            _ => false,
        };
        if !keep {
            self.last_seen_version = Some(version.to_owned());
        }
    }
}

/// The session store. Updates are applied by [`SessionState`]'s methods; the store decides when to write.
///
/// Its methods come in two durability classes, and the signature says which. A method that promises a durable write
/// **returns a `Result`**: [`begin_open`](Self::begin_open) and [`opened`](Self::opened), the two halves of the
/// crash-loop guard (D9). The others are debounced and cannot fail the caller: a failed flush is logged and retried,
/// and [`flush`](Self::flush) reports it. Session state is not a setting, has no [`Mode`](omuzaki_ops_api::Mode) and no
/// actor, and never enters the change log.
pub trait SessionStore: Send + Sync {
    /// A copy of the state.
    fn state(&self) -> SessionState;

    /// Marks `project` as opening and makes the marker durable **now** (written and synced, never debounced). The
    /// caller must not start loading when this fails.
    ///
    /// # Errors
    /// [`SettingErrorKind::Storage`](crate::error::SettingErrorKind::Storage).
    fn begin_open(&self, project: &Path) -> Result<(), SettingError>;

    /// The project is open and shown (see [`SessionState::opened`]): clears the `opening` guard and makes that
    /// durable **now**, like [`begin_open`](Self::begin_open), because a guard that stays set makes the next launch
    /// take a successful open for a crash. The in-memory state is updated either way. When the write fails the project
    /// is open all the same: the caller reports the error, and may call this again or [`flush`](Self::flush).
    ///
    /// # Errors
    /// [`SettingErrorKind::Storage`](crate::error::SettingErrorKind::Storage).
    fn opened(&self, project: &Path, name: &str) -> Result<(), SettingError>;

    /// Removes `project` from the recent list. **Debounced**: `state()` shows the change at once and the store writes
    /// it shortly after. A write that fails is logged as a warning and retried with the next change and by
    /// [`flush`](Self::flush); it never fails the caller.
    fn forget(&self, project: &Path);

    /// Puts a forgotten entry back at `index` (see [`SessionState::restore_recent`]): the undo of `session.forget`.
    /// Debounced, like [`forget`](Self::forget).
    fn restore_recent(&self, entry: RecentProject, index: usize, last: bool);

    /// Remembers a window's geometry. Debounced, like [`forget`](Self::forget).
    fn set_window(&self, id: &str, geometry: WindowGeometry);

    /// Remembers the dismissed release-notes version. Debounced, like [`forget`](Self::forget).
    fn mark_version_seen(&self, version: &str);

    /// Writes any debounced state now. The app calls it at exit: it is the one place a failed debounced write reaches
    /// a caller. A store with nothing pending, and a store with no files, return `Ok`.
    ///
    /// # Errors
    /// [`SettingErrorKind::Storage`](crate::error::SettingErrorKind::Storage); the state stays in memory and is
    /// retried by the next write.
    fn flush(&self) -> Result<(), SettingError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_is_newest_first_unique_and_capped() {
        let mut s = SessionState::default();
        for i in 0..25 {
            s.opened(Path::new(&format!("/p/{i}")), "n", i);
        }
        s.opened(Path::new("/p/10"), "again", 100);
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert_eq!(s.recent[0].name, "again");
        assert_eq!(s.recent.iter().filter(|r| r.path == Path::new("/p/10")).count(), 1);
        assert_eq!(s.last_project.as_deref(), Some(Path::new("/p/10")));
    }

    #[test]
    fn the_opening_guard_is_set_then_cleared() {
        let mut s = SessionState::default();
        s.begin_open(Path::new("/p/a"));
        assert_eq!(s.opening.as_deref(), Some(Path::new("/p/a")));
        s.opened(Path::new("/p/a"), "A", 1);
        assert_eq!(s.opening, None);
    }

    #[test]
    fn a_forgotten_entry_comes_back_where_it_was() {
        let mut s = SessionState::default();
        for (i, p) in ["/p/c", "/p/b", "/p/a"].iter().enumerate() {
            s.opened(Path::new(p), "n", i64::try_from(i).unwrap_or(0));
        }
        let gone = s.recent[1].clone();
        s.forget(Path::new("/p/b"));
        s.restore_recent(gone.clone(), 1, false);
        assert_eq!(s.recent[1], gone);
        assert_eq!(s.recent.len(), 3);
        s.restore_recent(gone, 99, true);
        assert_eq!(s.last_project.as_deref(), Some(Path::new("/p/b")));
        assert_eq!(s.recent.len(), 3, "one entry per path");
    }

    #[test]
    fn forgetting_the_last_project_clears_it() {
        let mut s = SessionState::default();
        s.opened(Path::new("/p/a"), "A", 1);
        s.forget(Path::new("/p/a"));
        assert!(s.recent.is_empty() && s.last_project.is_none());
    }

    #[test]
    fn a_seen_version_never_goes_down() {
        let mut s = SessionState::default();
        s.mark_version_seen("0.9.0");
        s.mark_version_seen("0.8.2");
        assert_eq!(s.last_seen_version.as_deref(), Some("0.9.0"));
        s.mark_version_seen("0.10.0");
        assert_eq!(s.last_seen_version.as_deref(), Some("0.10.0"));
    }

    #[test]
    fn a_pre_release_never_replaces_its_release_and_a_later_beta_replaces_an_earlier_one() {
        let mut s = SessionState::default();
        s.mark_version_seen("0.9.0");
        s.mark_version_seen("0.9.0-beta.1");
        assert_eq!(s.last_seen_version.as_deref(), Some("0.9.0"), "stable to beta");
        let mut s = SessionState::default();
        s.mark_version_seen("0.9.0-beta.2");
        s.mark_version_seen("0.9.0-beta.1");
        assert_eq!(s.last_seen_version.as_deref(), Some("0.9.0-beta.2"), "beta to an earlier beta");
        s.mark_version_seen("0.9.0-beta.10");
        assert_eq!(s.last_seen_version.as_deref(), Some("0.9.0-beta.10"), "numeric identifiers rank as numbers");
        s.mark_version_seen("0.9.0");
        assert_eq!(s.last_seen_version.as_deref(), Some("0.9.0"), "the release replaces its beta");
    }

    #[test]
    fn build_metadata_is_ignored_for_ranking() {
        let mut s = SessionState::default();
        s.mark_version_seen("1.2.0+b9");
        s.mark_version_seen("1.1.9+b10");
        assert_eq!(s.last_seen_version.as_deref(), Some("1.2.0+b9"), "build metadata is not a downgrade route");
    }

    #[test]
    fn a_value_that_is_not_a_semantic_version_is_never_recorded() {
        let record = |old: Option<&str>, new: &str| {
            let mut s = SessionState { last_seen_version: old.map(str::to_owned), ..SessionState::default() };
            s.mark_version_seen(new);
            s.last_seen_version
        };
        // Every (old, new) combination with an invalid new value: absent, valid and damaged records.
        assert_eq!(record(None, "nightly"), None, "no old record: still nothing recorded");
        assert_eq!(record(Some("1.2.0"), "nightly").as_deref(), Some("1.2.0"), "a valid old record is kept");
        assert_eq!(record(Some("garbled"), "nightly").as_deref(), Some("garbled"), "a damaged record is left alone");
        assert_eq!(record(Some("1.2.0"), "").as_deref(), Some("1.2.0"));
        // A valid new value still replaces a damaged or absent record.
        assert_eq!(record(Some("garbled"), "0.1.0").as_deref(), Some("0.1.0"));
        assert_eq!(record(None, "0.1.0").as_deref(), Some("0.1.0"));
    }
}
