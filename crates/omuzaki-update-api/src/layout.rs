//! Where things are under the install root (design section 5).

use std::path::{Path, PathBuf};

use semver::Version;

/// The install folder's paths. Pure: it computes names and touches nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// The layout under `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The install root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `state.toml`.
    #[must_use]
    pub fn state(&self) -> PathBuf {
        self.root.join("state.toml")
    }

    /// `state.toml.lock`, held by a running launcher.
    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.root.join("state.toml.lock")
    }

    /// The marker `Updater::healthy` writes.
    #[must_use]
    pub fn healthy_marker(&self) -> PathBuf {
        self.root.join("healthy")
    }

    /// `versions/<v>/`, one complete app tree.
    #[must_use]
    pub fn version_dir(&self, version: &Version) -> PathBuf {
        self.root.join("versions").join(version.to_string())
    }

    /// `staging/<v>.partial/`, where a package unpacks. Deleted on start if found.
    #[must_use]
    pub fn staging_dir(&self, version: &Version) -> PathBuf {
        self.root.join("staging").join(format!("{version}.partial"))
    }

    /// `downloads/`, verified packages by hash.
    #[must_use]
    pub fn downloads(&self) -> PathBuf {
        self.root.join("downloads")
    }

    /// `backup/<v>/`, the settings files copied at the first start of the next version.
    #[must_use]
    pub fn backup_dir(&self, version: &Version) -> PathBuf {
        self.root.join("backup").join(version.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_the_design() {
        let layout = Layout::new("/r");
        let v = Version::new(1, 4, 0);
        assert_eq!(layout.state(), Path::new("/r/state.toml"));
        assert_eq!(layout.lock(), Path::new("/r/state.toml.lock"));
        assert_eq!(layout.healthy_marker(), Path::new("/r/healthy"));
        assert_eq!(layout.version_dir(&v), Path::new("/r/versions/1.4.0"));
        assert_eq!(layout.staging_dir(&v), Path::new("/r/staging/1.4.0.partial"));
        assert_eq!(layout.downloads(), Path::new("/r/downloads"));
        assert_eq!(layout.backup_dir(&v), Path::new("/r/backup/1.4.0"));
    }
}
