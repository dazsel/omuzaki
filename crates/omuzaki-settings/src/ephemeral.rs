//! Session state kept in memory.
//!
//! The `Settings` trait hands out a [`SessionStore`]. Until the session file store lands (D9), the settings store
//! returns this one: it behaves as the trait says but forgets everything at exit.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::session::{RecentProject, SessionState, SessionStore, SetupProgress, WindowGeometry};

use crate::locked;

/// [`SessionStore`] over a [`SessionState`] in memory.
#[derive(Debug, Default)]
pub(crate) struct Ephemeral {
    state: Mutex<SessionState>,
}

impl SessionStore for Ephemeral {
    fn state(&self) -> SessionState {
        locked(&self.state).clone()
    }

    fn begin_open(&self, project: &Path) -> Result<(), SettingError> {
        locked(&self.state).begin_open(project);
        Ok(())
    }

    fn opened(&self, project: &Path, name: &str) -> Result<(), SettingError> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        locked(&self.state).opened(project, name, now);
        Ok(())
    }

    fn forget(&self, project: &Path) {
        locked(&self.state).forget(project);
    }

    fn restore_recent(&self, entry: RecentProject, index: usize, last: bool) {
        locked(&self.state).restore_recent(entry, index, last);
    }

    fn set_setup(&self, progress: Option<SetupProgress>) -> Result<(), SettingError> {
        locked(&self.state).set_setup(progress);
        Ok(())
    }

    fn set_window(&self, id: &str, geometry: WindowGeometry) {
        locked(&self.state).set_window(id, geometry);
    }

    fn mark_version_seen(&self, version: &str) {
        locked(&self.state).mark_version_seen(version);
    }

    fn flush(&self) -> Result<(), SettingError> {
        Ok(())
    }
}
