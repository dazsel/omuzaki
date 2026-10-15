//! Session state (D9): what was open, never what was running. It is *state*, not a setting: the app writes it as a
//! side effect, so it has no `set` op and no default, and it lives beside the machine file because paths and monitor
//! layouts are machine-bound.
//!
//! [`SessionState`] carries its own transitions so every store (the file store and the mock) applies the same rules
//! (newest first, at most [`MAX_RECENT`], one entry per path) and only decides when to write.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
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

    /// Records a window's geometry.
    pub fn set_window(&mut self, id: &str, geometry: WindowGeometry) {
        self.windows.insert(id.to_owned(), geometry);
    }

    /// Records a dismissed version, but never lowers it: after a rollback the higher value stays so the roll-forward
    /// does not show the notes twice. Compared as the leading `major.minor.patch` numbers; an equal version replaces
    /// the old text (so the stable `0.9.0` replaces a recorded `0.9.0-beta.2`), and anything unreadable replaces it.
    pub fn mark_version_seen(&mut self, version: &str) {
        let key = |v: &str| -> Option<Vec<u64>> { v.split(['.', '-']).take(3).map(|p| p.parse().ok()).collect() };
        let keep = match (self.last_seen_version.as_deref().and_then(key), key(version)) {
            (Some(old), Some(new)) => old > new,
            _ => false,
        };
        if !keep {
            self.last_seen_version = Some(version.to_owned());
        }
    }
}

/// The session store. Updates are applied by [`SessionState`]'s methods; the store decides when to write.
pub trait SessionStore: Send + Sync {
    /// A copy of the state.
    fn state(&self) -> SessionState;

    /// Marks `project` as opening and makes the marker durable **now** (written and synced, never debounced). The
    /// caller must not start loading when this fails.
    ///
    /// # Errors
    /// [`SettingErrorKind::Storage`](crate::error::SettingErrorKind::Storage).
    fn begin_open(&self, project: &Path) -> Result<(), SettingError>;

    /// The project is open and shown (see [`SessionState::opened`]).
    fn opened(&self, project: &Path, name: &str);

    /// Removes `project` from the recent list.
    fn forget(&self, project: &Path);

    /// Remembers a window's geometry.
    fn set_window(&self, id: &str, geometry: WindowGeometry);

    /// Remembers the dismissed release-notes version.
    fn mark_version_seen(&self, version: &str);
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
        s.mark_version_seen("0.11.0-beta.2");
        s.mark_version_seen("0.11.0");
        assert_eq!(s.last_seen_version.as_deref(), Some("0.11.0"), "stable replaces the beta of the same number");
    }
}
