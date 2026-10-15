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
#[must_use]
pub fn literal_pathspec(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    format!(":(literal){text}")
}
