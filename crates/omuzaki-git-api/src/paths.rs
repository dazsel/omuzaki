//! The path rules of [`Git::commit`](crate::git::Git::commit), shared so the real git and the mock hold to one check.

use std::path::{Component, Path};

use crate::error::GitError;

/// Folder names a commit never touches: the keeper's data and git's own.
const FORBIDDEN: [&str; 2] = [".omuzaki", ".git"];

/// Checks the `paths` given to `Git::commit`.
///
/// Each must be non-empty, relative, free of `..`, and must not have a `.omuzaki` or `.git` component (any case, so a
/// case-insensitive disk cannot get around it). Anything else is [`GitError::BadPath`] naming that path. Text such as
/// `:(top)*` passes, and so does `.`, the whole project (design §6): it is a legal file name, and [`literal_pathspec`] keeps git from reading it as magic.
///
/// # Errors
/// [`GitError::BadPath`] for the first path that breaks a rule; [`GitError::NothingToCommit`] for an empty list.
pub fn validate_commit_paths<P: AsRef<Path>>(paths: &[P]) -> Result<(), GitError> {
    if paths.is_empty() {
        return Err(GitError::NothingToCommit);
    }
    for path in paths {
        let path = path.as_ref();
        let bad = || GitError::BadPath { path: path.to_string_lossy().into_owned() };
        if path.to_str().is_none_or(str::is_empty) {
            return Err(bad());
        }
        for part in path.components() {
            match part {
                Component::Normal(name) => {
                    let name = name.to_str().ok_or_else(bad)?;
                    if FORBIDDEN.iter().any(|f| name.eq_ignore_ascii_case(f)) {
                        return Err(bad());
                    }
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => return Err(bad()),
            }
        }
    }
    Ok(())
}

/// The argument that makes git read `path` as a name: `:(literal)` and the path with `/` separators. Call it on a path
/// [`validate_commit_paths`] accepted, and pass the result after `--`.
///
/// Only Windows separates with `\\`; elsewhere a backslash is a legal character in a file name, so `a\\b.txt` stays
/// that one file rather than becoming `a/b.txt` (#1622).
#[must_use]
pub fn literal_pathspec(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = if cfg!(windows) { text.replace('\\', "/") } else { text.into_owned() };
    format!(":(literal){text}")
}

/// Pathspec magic that leaves out every `.omuzaki` folder (or file) at any depth, in any case. Git applies it
/// after the positive pathspecs, so it also removes a tracked `.omuzaki/` file that was once added with `git add -f`,
/// which `.omuzaki/.gitignore` does not stop (design §6).
pub const PRIVATE_EXCLUDES: [&str; 2] = [":(exclude,glob,icase)**/.omuzaki", ":(exclude,glob,icase)**/.omuzaki/**"];

/// The pathspec for the whole project folder: `.` read literally.
pub const PROJECT_PATHSPEC: &str = ":(literal).";

/// The pathspecs that select the project's files and nothing else: the project folder, without any private `.omuzaki`
/// file. Git must be run in the project folder (`git -C <dir>`) and given these after `--`. [`Git::log`],
/// [`Git::status`] and [`Git::show`] use them, so a project inside a larger repository never reports another project's
/// commits or files, and private state never leaves the folder in a result. `git ls-tree` refuses the excludes' magic,
/// so [`Git::tree_at`] uses [`tree_pathspecs`] and [`is_private`] instead.
///
/// [`Git::log`]: crate::git::Git::log
/// [`Git::status`]: crate::git::Git::status
/// [`Git::show`]: crate::git::Git::show
/// [`Git::tree_at`]: crate::git::Git::tree_at
#[must_use]
pub fn project_pathspecs() -> Vec<String> {
    std::iter::once(PROJECT_PATHSPEC).chain(PRIVATE_EXCLUDES).map(str::to_owned).collect()
}

/// The pathspecs [`Git::tree_at`](crate::git::Git::tree_at) gives `git ls-tree` after `--`: the project folder only.
/// `ls-tree` supports no `glob`, `icase` or `exclude` magic (git 2.43 exits 128 on [`PRIVATE_EXCLUDES`]), so the
/// private entries are dropped after listing, with [`is_private`] (#1939).
#[must_use]
pub fn tree_pathspecs() -> Vec<String> {
    vec![PROJECT_PATHSPEC.to_owned()]
}

/// Whether `path` (relative, `/`-separated, as git lists it) is private state: it has a `.omuzaki` component at any
/// depth, in any case. What [`PRIVATE_EXCLUDES`] leaves out, for the commands that take no exclude magic.
#[must_use]
pub fn is_private(path: &str) -> bool {
    path.split('/').any(|part| part.eq_ignore_ascii_case(".omuzaki"))
}

/// The pathspecs [`Git::commit`](crate::git::Git::commit) gives git after `--`, for both `git add` and `git commit`:
/// each of `paths` as a literal pathspec, then [`PRIVATE_EXCLUDES`]. The excludes apply to every selection, not only
/// to `.`, because a folder such as `sub` can hold a tracked `sub/.omuzaki/` file as well.
///
/// # Errors
/// What [`validate_commit_paths`] reports, before any pathspec is built.
pub fn commit_pathspecs<P: AsRef<Path>>(paths: &[P]) -> Result<Vec<String>, GitError> {
    validate_commit_paths(paths)?;
    Ok(paths
        .iter()
        .map(|path| literal_pathspec(path.as_ref()))
        .chain(PRIVATE_EXCLUDES.iter().map(|spec| (*spec).to_owned()))
        .collect())
}
