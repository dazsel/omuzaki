//! The startup plan over the real folders (settings.md D9, first-run.md §6): which projects a crash left open (their
//! journals), whether the project to reopen can be opened (its `project.toml`), and `startup.reopen`, given to
//! [`omuzaki_settings::startup::startup_plan`]. The studio calls [`plan`] once at start; headless hosts never do.

#![allow(
    clippy::disallowed_methods,
    reason = "host code, not an op: looking at the folders the person's own session names"
)]

use std::path::{Path, PathBuf};

use omuzaki_doc::journal::left_open;
use omuzaki_doc::{Project, StoreError};
use omuzaki_settings::startup::{Folder, Probe, Reopen, StartupPlan, StartupReopen, startup_plan};
use omuzaki_settings_api::service::{Settings, SettingsExt};
use omuzaki_settings_api::session::SessionState;

/// Looks at project folders on disk: a folder that is not there is missing (a drive that is not connected looks the
/// same, which is why the notice asks), and one that is there is judged by its `project.toml` alone.
#[derive(Clone, Copy, Debug, Default)]
pub struct FolderProbe;

impl Probe for FolderProbe {
    fn folder(&self, project: &Path) -> Folder {
        if !project.is_dir() {
            return Folder::Missing;
        }
        match Project::check(project) {
            Ok(_) => Folder::Ready,
            Err(StoreError::SchemaTooNew { found, supported }) => Folder::Newer { found, supported },
            Err(error) => Folder::Unreadable { reason: error.to_string() },
        }
    }
}

/// The projects the session knows that Omuzaki stopped without closing: each one's live journal is read, nothing is
/// opened or written.
pub fn left_open_projects(session: &SessionState) -> Vec<PathBuf> {
    let mut known: Vec<&Path> = session.recent.iter().map(|r| r.path.as_path()).collect();
    known.extend(session.last_project.as_deref());
    known.sort_unstable();
    known.dedup();
    known.into_iter().filter(|p| left_open(p)).map(Path::to_path_buf).collect()
}

/// What the studio does at start, from the person's session and settings. A `startup.reopen` that does not read is
/// its default (`last`), as every setting is.
pub fn plan(settings: &dyn Settings) -> StartupPlan {
    let session = settings.session().state();
    let reopen = settings.get::<StartupReopen>().unwrap_or(Reopen::Last);
    startup_plan(&session, reopen, &left_open_projects(&session), &FolderProbe)
}
