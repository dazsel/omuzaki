//! [`SavedProject`]: a [`Project`] that saves after every patch it applies, for an app that keeps no unsaved state.

use omuzaki_doc_api::{DocPath, DocRead, DocVersion, DocWrite, Patch, PatchError, Value};

use crate::StoreError;
use crate::project::Project;

/// A [`Project`] saved to its folder after every patch it applies, so the folder always holds what the app shows. A save
/// rewrites only the files whose part of the document changed. If a save fails the patch is taken back out and the
/// apply fails with [`PatchError::Unreachable`] naming why, so a refused change leaves the document and the files
/// agreeing.
#[derive(Debug)]
pub struct SavedProject {
    project: Project,
}

impl SavedProject {
    /// `project`, saved after every patch from now on. What it held unsaved before is saved with the first patch.
    pub const fn new(project: Project) -> Self {
        Self { project }
    }

    /// Opens the project in `dir`, saved after every patch from now on.
    ///
    /// # Errors
    /// As [`Project::open`].
    pub fn open(dir: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        Project::open(dir).map(Self::new)
    }

    /// The project.
    pub const fn project(&self) -> &Project {
        &self.project
    }

    /// The project, no longer saved after each patch.
    pub fn into_inner(self) -> Project {
        self.project
    }
}

impl DocRead for SavedProject {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        self.project.get(path)
    }

    fn version(&self) -> DocVersion {
        self.project.version()
    }
}

impl DocWrite for SavedProject {
    fn apply(&mut self, patch: &Patch) -> Result<DocVersion, PatchError> {
        let version = self.project.apply(patch)?;
        let Err(save_error) = self.project.save() else { return Ok(version) };
        let at = patch.changes.first().map_or_else(DocPath::root, |change| change.path.clone());
        let why = match self.project.apply(&patch.inverse()) {
            Err(e) => format!("couldn't save the project ({save_error}) or take the change back out ({e})"),
            Ok(_) => match self.project.save() {
                Ok(_) => format!("couldn't save the project ({save_error}); the change was not made"),
                Err(again) => format!("couldn't save the project ({save_error}), and the files may not match: {again}"),
            },
        };
        Err(PatchError::Unreachable { path: at, reason: why })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn path(text: &str) -> DocPath {
        DocPath::parse(text).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn every_applied_patch_is_on_disk_when_apply_returns() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let project = Project::create(dir.path(), "Show").unwrap_or_else(|e| panic!("{e}"));
        let mut saved = SavedProject::new(project);
        let patch = Patch::new().with(path("characters"), None, Some(json!({"kit": {"name": "Kit"}})));
        saved.apply(&patch).unwrap_or_else(|e| panic!("{e}"));
        assert!(!saved.project().is_dirty());
        drop(saved);
        let reopened = Project::open(dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(reopened.get(&path("characters/kit/name")), Some(&json!("Kit")));
    }
}
