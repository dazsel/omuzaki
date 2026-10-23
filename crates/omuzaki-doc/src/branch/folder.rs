//! `folder.toml` (design 4.3 and 5.5): which branch the project folder shows, and the marker a switch leaves while it
//! rewrites the folder, so a crash halfway is finished at the next open instead of adopting the old branch's files.

use std::path::{Path, PathBuf};

use omuzaki_core::fsx::write_atomic;
use omuzaki_history_api::branch::{BranchError, BranchId};
use serde::{Deserialize, Serialize};

use super::branches_dir;

/// The marker's file name inside `.omuzaki/branches/`.
const FOLDER_FILE: &str = "folder.toml";

/// Whether a switch is under way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FolderState {
    /// The folder holds `branch`'s state.
    #[default]
    Ok,
    /// The folder is being rewritten from `from` to `to`; opening finishes it.
    Switching,
}

/// The contents of `folder.toml`. Absent means `main`, `ok`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FolderMarker {
    /// The branch the folder shows (during a switch: the one it showed before).
    pub branch: BranchId,
    /// Whether a switch is under way.
    #[serde(default)]
    pub state: FolderState,
    /// The branch a switch leaves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<BranchId>,
    /// The branch a switch goes to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<BranchId>,
}

impl Default for FolderMarker {
    fn default() -> Self {
        Self { branch: BranchId::main(), state: FolderState::Ok, from: None, to: None }
    }
}

fn path(project_dir: &Path) -> PathBuf {
    branches_dir(project_dir).join(FOLDER_FILE)
}

impl FolderMarker {
    /// The marker of the project in `project_dir`; `main`, `ok` when there is none.
    ///
    /// # Errors
    /// [`BranchError::Io`] when it exists and can't be read or parsed.
    pub fn read(project_dir: &Path) -> Result<Self, BranchError> {
        let path = path(project_dir);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| BranchError::Io(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(BranchError::Io(format!("{}: {e}", path.display()))),
        }
    }

    /// The branch whose state the folder must hold once any switch under way is finished: `to` while switching.
    pub fn target(&self) -> &BranchId {
        match (&self.state, &self.to) {
            (FolderState::Switching, Some(to)) => to,
            _ => &self.branch,
        }
    }

    /// Writes the marker for a switch from `from` to `to` (design 5.5 step 3), flushed before it returns.
    ///
    /// # Errors
    /// [`BranchError::Io`].
    pub fn begin_switch(project_dir: &Path, from: &BranchId, to: &BranchId) -> Result<Self, BranchError> {
        let marker = Self {
            branch: from.clone(),
            state: FolderState::Switching,
            from: Some(from.clone()),
            to: Some(to.clone()),
        };
        marker.write(project_dir)?;
        Ok(marker)
    }

    /// Writes the marker for a folder that holds `to` (design 5.5 step 6), flushed before it returns.
    ///
    /// # Errors
    /// [`BranchError::Io`].
    pub fn finish_switch(project_dir: &Path, to: &BranchId) -> Result<Self, BranchError> {
        let marker = Self { branch: to.clone(), ..Self::default() };
        marker.write(project_dir)?;
        Ok(marker)
    }

    fn write(&self, project_dir: &Path) -> Result<(), BranchError> {
        let path = path(project_dir);
        let text = toml::to_string(self).map_err(|e| BranchError::Io(e.to_string()))?;
        write_atomic(&path, &text).map_err(|e| BranchError::Io(format!("{}: {e}", path.display())))
    }
}
