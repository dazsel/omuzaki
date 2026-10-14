use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How long a local read (status, log, show, tree, merge-base) may run before [`GitError::Timeout`].
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a push or pull may run before [`GitError::Timeout`].
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(600);
/// The oldest git (major, minor) the contract supports.
pub const MIN_GIT_VERSION: (u32, u32) = (2, 30);

/// Why a git call failed. The text says what to do.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum GitError {
    /// No usable git was found.
    #[error("Install git 2.30 or later to use this{}", found_note(.found.as_deref()))]
    Missing {
        /// The version text of an older git that was found, if any.
        found: Option<String>,
    },
    /// Git cannot be used here: this build has no git (a web build), or the installed git lacks what the call needs.
    #[error("Git isn't available for this here. Use the desktop app with git 2.30 or later")]
    Unsupported,
    /// The folder is not inside a git repository.
    #[error("This folder is not a git repository. Run `git init` in it first")]
    NotARepo,
    /// A revision does not exist or is not a valid id.
    #[error("That revision does not exist. Pick a commit from the log")]
    BadRev,
    /// Nothing is staged or changed for the paths given.
    #[error("Nothing to commit. Change a file first")]
    NothingToCommit,
    /// A path is outside the folder or otherwise unusable.
    #[error("The path `{path}` is not inside this project folder. Pick a file in the project")]
    BadPath {
        /// The offending path.
        path: String,
    },
    /// Pull cannot fast-forward.
    #[error(
        "The branch has diverged ({ahead} ahead, {behind} behind). Merge or rebase in your own git, then try again"
    )]
    NotFastForward {
        /// Commits only here.
        ahead: u32,
        /// Commits only on the remote.
        behind: u32,
    },
    /// The remote wants credentials git cannot supply without a prompt.
    #[error("The remote needs sign-in. Sign in with your git credential helper or ssh agent, then try again")]
    AuthRequired,
    /// A commit hook refused the commit.
    #[error("A git hook rejected the commit: {output}. Fix what it reports, then try again")]
    HookRejected {
        /// What the hook printed.
        output: String,
    },
    /// The call ran past its timeout.
    #[error("Git took too long and was stopped. Check the connection or repository size, then try again")]
    Timeout,
    /// The caller's [`Cancel`](crate::Cancel) was set.
    #[error("The git call was cancelled")]
    Cancelled,
    /// Git exited with an error not covered above.
    #[error("Git failed (exit code {}): {stderr}. Fix what it reports in your own git, then try again", code_text(*.code))]
    Failed {
        /// The exit code; `None` when git was ended by a signal.
        code: Option<i32>,
        /// What git printed to stderr.
        stderr: String,
    },
}

fn found_note(found: Option<&str>) -> String {
    found.map(|v| format!(" (found: {v})")).unwrap_or_default()
}

fn code_text(code: Option<i32>) -> String {
    code.map_or_else(|| "none".to_owned(), |c| c.to_string())
}
