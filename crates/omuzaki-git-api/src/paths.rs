//! The path rules of [`Git::commit`](crate::git::Git::commit), shared so the real git and the mock hold to one check.

use std::path::{Component, Path};

use crate::error::GitError;

/// Folder names a commit never touches: the keeper's data and git's own.
const FORBIDDEN: [&str; 2] = [".omuzaki", ".git"];

/// The pathspec that removes the keeper's private folder from any selection (design §6): `.omuzaki/` is never staged,
/// committed, logged or listed, even if someone force-added a file there.
pub const PRIVATE_EXCLUDE: &str = ":(exclude,literal).omuzaki";

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
#[must_use]
pub fn literal_pathspec(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    format!(":(literal){text}")
}

/// The pathspecs for a [`Git::commit`](crate::git::Git::commit) selection: each of `paths` through [`literal_pathspec`],
/// then [`PRIVATE_EXCLUDE`] always, so a selection of the whole project (`.`) can never stage or commit a force-added
/// file under `.omuzaki/`. Call it on paths [`validate_commit_paths`] accepted; both implementations use it for the
/// `add` and the `commit` command alike.
#[must_use]
pub fn commit_pathspecs<P: AsRef<Path>>(paths: &[P]) -> Vec<String> {
    let mut specs: Vec<String> = paths.iter().map(|p| literal_pathspec(p.as_ref())).collect();
    specs.push(PRIVATE_EXCLUDE.to_owned());
    specs
}

/// The pathspecs that scope a read ([`Git::status`](crate::git::Git::status), [`Git::log`](crate::git::Git::log),
/// [`Git::show`](crate::git::Git::show)) to the project folder: the folder itself, without `.omuzaki/`. Run git with
/// `dir` as its working directory and pass these after `--`.
#[must_use]
pub fn project_pathspecs() -> [&'static str; 2] {
    [":(literal).", PRIVATE_EXCLUDE]
}
