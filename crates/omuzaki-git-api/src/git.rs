//! The [`Git`] trait both implementations provide, and what a pull did ([`PullOutcome`]).

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cancel::Cancel;
use crate::commit::{Commit, CommitDetail, CommitId, CommitTree, LogRange};
use crate::error::GitError;
use crate::status::GitStatus;

/// What a pull did. Only fast-forwards happen; anything else is [`GitError::NotFastForward`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum PullOutcome {
    /// Already level with the remote.
    UpToDate,
    /// The branch moved forward.
    FastForwarded {
        /// Where it was.
        from: CommitId,
        /// Where it is.
        to: CommitId,
        /// How many commits it moved over.
        commits: u32,
    },
}

/// The person's own git. Calls block; the host runs them on a worker thread.
///
/// Every method takes the project folder (not a path inside it); paths in results are relative to it and use `/`.
/// Each call polls its [`Cancel`] and returns [`GitError::Cancelled`] once it is set. Every method fails with a
/// [`GitError`], which says what went wrong and what to do.
///
/// Revisions and remotes are caller text and never options. An implementation passes every caller-supplied
/// revision ([`Git::log`], through every [`LogRange`] revision, [`Git::show`], [`Git::tree_at`], [`Git::merge_base`]) and remote ([`Git::push`], [`Git::pull`]) to git
/// after `--end-of-options`, so a value such as `--upload-pack=…` or `--output=…` is read as a name, never as an
/// option. `--end-of-options` needs git 2.24 or later, which [`MIN_GIT_VERSION`] (2.30) covers. A revision that does
/// not resolve to a commit is [`GitError::BadRev`].
///
/// [`MIN_GIT_VERSION`]: crate::error::MIN_GIT_VERSION
// One shared `# Errors` note on the trait stands in for the same sentence on nine methods.
#[allow(clippy::missing_errors_doc)]
pub trait Git: Send + Sync {
    /// The folder's branch, ahead/behind, changed files and merge state.
    ///
    /// Scoped to the project folder with [`project_pathspecs`]; a tracked file under `.omuzaki/` is reported only as
    /// `private_tracked`.
    ///
    /// [`project_pathspecs`]: crate::paths::project_pathspecs
    fn status(&self, dir: &Path, cancel: &dyn Cancel) -> Result<GitStatus, GitError>;
    /// At most `limit` commits of `range` that touch the project folder, newest first.
    ///
    /// The implementation passes [`project_pathspecs`] after `--`, so commits elsewhere in the repository are not
    /// listed (design §4).
    ///
    /// [`project_pathspecs`]: crate::paths::project_pathspecs
    fn log(&self, dir: &Path, range: LogRange, limit: usize, cancel: &dyn Cancel) -> Result<Vec<Commit>, GitError>;
    /// One commit and the files it changed against its first parent, limited to the project folder
    /// ([`project_pathspecs`]).
    ///
    /// [`project_pathspecs`]: crate::paths::project_pathspecs
    ///
    /// `rev` goes to git after `--end-of-options`, never as an option; if it does not resolve, [`GitError::BadRev`].
    fn show(&self, dir: &Path, rev: &str, cancel: &dyn Cancel) -> Result<CommitDetail, GitError>;
    /// The files of a commit under the project folder, to import as a point; nothing under `.omuzaki/` is returned
    /// ([`project_pathspecs`]).
    ///
    /// [`project_pathspecs`]: crate::paths::project_pathspecs
    ///
    /// `rev` goes to git after `--end-of-options`, never as an option; if it does not resolve, [`GitError::BadRev`].
    fn tree_at(&self, dir: &Path, rev: &str, cancel: &dyn Cancel) -> Result<CommitTree, GitError>;
    /// The best common ancestor of `a` and `b`; `None` when they share none.
    ///
    /// `a` and `b` go to git after `--end-of-options`, never as options; if either does not resolve,
    /// [`GitError::BadRev`].
    fn merge_base(&self, dir: &Path, a: &str, b: &str, cancel: &dyn Cancel) -> Result<Option<CommitId>, GitError>;
    /// Commits `paths` with `message`. A person only: agents never reach it.
    ///
    /// Every path must be relative, stay inside `dir`, and never name `.omuzaki` or `.git`; otherwise the call fails
    /// with [`GitError::BadPath`] before git runs. [`validate_commit_paths`] is that check, and both the real
    /// implementation and the mock call it, so they cannot disagree. The paths reach git, for `add` and `commit` alike,
    /// as [`commit_pathspecs`] after `--`: each a literal pathspec (the `:(literal)` prefix, because `--` alone does not
    /// stop magic such as `:(top)*`), then the `.omuzaki` exclusion every time, so a selection of the whole project
    /// (`.`) cannot commit a force-added private file. The call, hooks included, is bounded by [`COMMIT_TIMEOUT`] and ends with [`GitError::Timeout`].
    ///
    /// [`validate_commit_paths`]: crate::paths::validate_commit_paths
    /// [`commit_pathspecs`]: crate::paths::commit_pathspecs
    /// [`COMMIT_TIMEOUT`]: crate::error::COMMIT_TIMEOUT
    fn commit(&self, dir: &Path, message: &str, paths: &[PathBuf], cancel: &dyn Cancel) -> Result<CommitId, GitError>;
    /// Pushes the current branch to `remote`. A person only.
    ///
    /// `remote` goes to git after `--end-of-options`, never as an option.
    fn push(&self, dir: &Path, remote: &str, cancel: &dyn Cancel) -> Result<(), GitError>;
    /// Fast-forwards the current branch from `remote`. A person only.
    ///
    /// `remote` goes to git after `--end-of-options`, never as an option.
    fn pull(&self, dir: &Path, remote: &str, cancel: &dyn Cancel) -> Result<PullOutcome, GitError>;
    /// The path of git's index lock for this folder, resolved through git (it differs for worktrees and
    /// submodules), so the keeper can tell a git operation is running (design §7, §8).
    fn index_lock_path(&self, dir: &Path, cancel: &dyn Cancel) -> Result<PathBuf, GitError>;
}
