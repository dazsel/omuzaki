//! The project folder as ops see it: validated once, when the app opens it, so a bad folder is an error there and
//! never a wide grant later.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::form::{Lexical, PathKey, Style};

/// Why a folder cannot be a [`ProjectDir`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectDirError {
    /// Nothing is there.
    #[error("the project folder `{}` does not exist", .0.display())]
    NotFound(PathBuf),
    /// A file, not a folder.
    #[error("`{}` is not a folder", .0.display())]
    NotAFolder(PathBuf),
    /// A filesystem root (`/`, `C:\`), a drive or a network share's top. Reading "inside the project" would mean
    /// reading everything on it, so the folder must be one the project actually lives in.
    #[error("`{}` is a filesystem root, not a project folder", .0.display())]
    FilesystemRoot(PathBuf),
    /// A path Omuzaki cannot compare exactly (not valid UTF-8, or a form this machine does not read).
    #[error("`{}` is not a path Omuzaki can compare exactly", .0.display())]
    Unsupported(PathBuf),
    /// The folder could not be read.
    #[error("the project folder `{}` cannot be read: {kind}", path.display())]
    Unreadable {
        /// The folder.
        path: PathBuf,
        /// What the OS said.
        kind: io::ErrorKind,
    },
}

/// The folder the open project lives in: where [`OpCtx::fs_access`](crate::OpCtx::fs_access) lets `read` reach, and
/// what relative paths are relative to. Build one with [`ProjectDir::open`] and give it to the executor.
///
/// It holds the folder's real path (symlinks resolved) and also the path it was opened by, so a request may name the
/// folder either way. A request by the opened-by name is moved onto the real folder before anything is looked up
/// ([`rebase`](Self::rebase)): the folder is the one that was opened, wherever that name points later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectDir {
    real: PathBuf,
    /// The real path as a place, the base of relative paths.
    base: Lexical,
    /// The real path as a key. Resolved paths are compared with this one only: a resolved path has no link left in
    /// it, so one under the opened-by name would mean the folder was swapped.
    resolved: PathKey,
    /// The path the folder was opened by, when it differs from the real one. A request is gated by name against
    /// both.
    aliases: Vec<PathKey>,
}

impl ProjectDir {
    /// Opens `dir`: made absolute against the current directory, resolved, and checked to be a folder that is not
    /// a filesystem root. A relative `dir` is pinned to where it points now, so a later `chdir` cannot move it.
    ///
    /// # Errors
    /// [`ProjectDirError`], one per way the folder can be unfit.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, ProjectDirError> {
        let dir = dir.as_ref();
        let given = std::path::absolute(dir).map_err(|e| unreadable(dir, &e))?;
        let real = fs::canonicalize(&given).map_err(|e| unreadable(&given, &e))?;
        let meta = fs::metadata(&real).map_err(|e| unreadable(&real, &e))?;
        if !meta.is_dir() {
            return Err(ProjectDirError::NotAFolder(real));
        }
        Self::from_paths(real, given.to_str(), Style::HOST)
    }

    /// The pure part of [`open`](Self::open), so the Windows rules can be tested on any host.
    pub(crate) fn from_paths(real: PathBuf, opened_by: Option<&str>, style: Style) -> Result<Self, ProjectDirError> {
        let base = real
            .to_str()
            .and_then(|text| Lexical::absolute(text, style))
            .ok_or_else(|| ProjectDirError::Unsupported(real.clone()))?;
        if base.depth() == 0 {
            return Err(ProjectDirError::FilesystemRoot(real));
        }
        let resolved = base.key(style);
        // The name the folder was opened by, when it is a plain place of its own (no `..`) and not itself a root.
        let opened_by = opened_by.and_then(|text| Lexical::absolute(text, style)).filter(|place| place.depth() > 0);
        let aliases = opened_by.map(|place| place.key(style)).filter(|key| *key != resolved).into_iter().collect();
        Ok(Self { real, base, resolved, aliases })
    }

    /// The folder's real path.
    pub fn as_path(&self) -> &Path {
        &self.real
    }

    /// The base of relative paths.
    pub(crate) fn base(&self) -> &Lexical {
        &self.base
    }

    /// Whether a requested place lies in the folder, by name (before the disk is asked anything).
    pub(crate) fn covers_request(&self, place: &PathKey) -> bool {
        self.resolved.covers(place) || self.aliases.iter().any(|alias| alias.covers(place))
    }

    /// Whether the folder lies in `place`, by either name: `place` is the folder or one above it, so a walk to the
    /// folder passes through it.
    pub(crate) fn lies_in(&self, place: &PathKey) -> bool {
        place.covers(&self.resolved) || self.aliases.iter().any(|alias| place.covers(alias))
    }

    /// A requested place in the folder, moved from the name the folder was opened by onto its real path (unchanged if
    /// it already names the real one, or is not in the folder).
    pub(crate) fn rebase(&self, place: &Lexical, style: Style) -> Lexical {
        let key = place.key(style);
        if self.resolved.covers(&key) {
            return place.clone();
        }
        match self.aliases.iter().find(|alias| alias.covers(&key)) {
            Some(alias) => place.rebased(alias.depth(), &self.base),
            None => place.clone(),
        }
    }

    /// Whether a resolved path lies in the folder.
    pub(crate) fn covers_resolved(&self, path: &PathKey) -> bool {
        self.resolved.covers(path)
    }
}

fn unreadable(path: &Path, error: &io::Error) -> ProjectDirError {
    match error.kind() {
        io::ErrorKind::NotFound => ProjectDirError::NotFound(path.to_path_buf()),
        kind => ProjectDirError::Unreadable { path: path.to_path_buf(), kind },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows(real: &str, opened_by: Option<&str>) -> Result<ProjectDir, ProjectDirError> {
        ProjectDir::from_paths(PathBuf::from(real), opened_by, Style::Windows)
    }

    fn posix(real: &str, opened_by: Option<&str>) -> Result<ProjectDir, ProjectDirError> {
        ProjectDir::from_paths(PathBuf::from(real), opened_by, Style::Posix)
    }

    #[test]
    fn filesystem_roots_are_not_project_folders() {
        for root in ["/", "//", "/./"] {
            assert!(matches!(posix(root, None), Err(ProjectDirError::FilesystemRoot(_))), "{root}");
        }
        for root in [r"C:\", "c:/", r"\\?\C:\", r"\\server\share", r"\\server\share\", r"\\?\UNC\server\share"] {
            assert!(matches!(windows(root, None), Err(ProjectDirError::FilesystemRoot(_))), "{root}");
        }
    }

    #[test]
    fn a_folder_below_a_root_is_fine_and_keeps_both_names() {
        let dir = windows(r"\\?\C:\Data\Proj", Some(r"c:\data\link")).unwrap();
        let inside = |text: &str| PathKey::absolute(text, Style::Windows).unwrap();
        assert!(dir.covers_request(&inside(r"C:\Data\Proj\a.vrm")));
        assert!(dir.covers_request(&inside(r"C:\DATA\LINK\a.vrm")), "the name it was opened by");
        assert!(!dir.covers_request(&inside(r"C:\Data\Other")));
        assert!(dir.covers_resolved(&inside(r"C:\Data\Proj\a.vrm")));
        assert!(!dir.covers_resolved(&inside(r"C:\Data\Link\a.vrm")), "a resolved path never goes by the opened name");
        assert_eq!(dir.base().depth(), 2);
        // Both names lie in `C:\Data`, and only there.
        assert!(dir.lies_in(&inside(r"C:\data")) && dir.lies_in(&inside(r"C:\Data\Link")));
        assert!(!dir.lies_in(&inside(r"C:\Data\Proj\a.vrm")) && !dir.lies_in(&inside(r"C:\Other")));
    }

    #[test]
    fn a_request_by_the_opened_by_name_is_moved_onto_the_real_folder() {
        let dir = windows(r"\\?\C:\Data\Proj", Some(r"c:\data\link")).unwrap();
        let place = |text: &str| Lexical::absolute(text, Style::Windows).unwrap();
        let spelt = |text: &str| dir.rebase(&place(text), Style::Windows).path(usize::MAX);
        assert_eq!(spelt(r"C:\DATA\LINK\sub\a.vrm"), PathBuf::from(r"C:\Data\Proj\sub\a.vrm"));
        assert_eq!(spelt(r"c:\data\proj\b.vrm"), PathBuf::from(r"C:\data\proj\b.vrm"), "the real name stays");
        assert_eq!(spelt(r"C:\Other\c.vrm"), PathBuf::from(r"C:\Other\c.vrm"), "not in the folder");
    }

    #[test]
    fn a_name_the_folder_was_opened_by_that_is_a_root_is_ignored() {
        let dir = posix("/data/proj", Some("/")).unwrap();
        assert!(!dir.covers_request(&PathKey::absolute("/etc/passwd", Style::Posix).unwrap()));
    }

    #[test]
    fn text_that_cannot_be_compared_exactly_is_unsupported() {
        assert!(matches!(posix("relative/dir", None), Err(ProjectDirError::Unsupported(_))));
        assert!(matches!(posix("/a/../b", None), Err(ProjectDirError::Unsupported(_))));
        assert!(matches!(windows(r"C:\proj\CON", None), Err(ProjectDirError::Unsupported(_))));
        assert!(matches!(windows(r"\\.\C:\proj", None), Err(ProjectDirError::Unsupported(_))));
    }
}
