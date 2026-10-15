//! The working tree: [`GitStatus`] and each [`ChangedFile`] with its [`ChangeKind`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commit::CommitId;

/// What git says about the folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitStatus {
    /// The current branch; `None` when `HEAD` is detached.
    pub branch: Option<String>,
    /// The commit `HEAD` points at; `None` in a repository with no commits.
    pub head: Option<CommitId>,
    /// Commits the branch has that its upstream lacks.
    pub ahead: u32,
    /// Commits the upstream has that the branch lacks.
    pub behind: u32,
    /// Changed files, relative to the folder.
    pub changed: Vec<ChangedFile>,
    /// Tracked paths under `.omuzaki/`, which should stay private.
    pub private_tracked: Vec<String>,
    /// Whether a merge is half done.
    pub merge_in_progress: bool,
}

/// A changed file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangedFile {
    /// Path relative to the folder, `/`-separated.
    pub path: String,
    /// What happened to it.
    pub kind: ChangeKind,
}

/// How a file changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum ChangeKind {
    /// New and staged or committed.
    Added,
    /// Content or mode changed.
    Modified,
    /// Removed.
    Deleted,
    /// Moved from another path.
    Renamed {
        /// The old path, relative to the folder, `/`-separated.
        from: String,
    },
    /// Not tracked by git.
    Untracked,
    /// Both sides of a merge changed it.
    Conflicted,
}
