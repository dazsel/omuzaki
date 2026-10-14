//! [`Project`]: a project folder held in memory as a document tree.

use std::fs;
use std::path::{Path, PathBuf};

use omuzaki_doc_api::{Change, DocPath, DocRead, DocVersion, DocWrite, Patch, PatchError, lookup};
use serde_json::{Map, Value, json};

use crate::disk::{self, Loaded, PROJECT_KEY};
use crate::error::StoreError;
use crate::layout::{Entry, Layout, Pending, Segs, doc_path};
use crate::migrate;
use crate::toml_write::{RenderError, render};
use crate::tree::{changed_paths, check_change, set, show};

/// What [`Project::save`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveReport {
    /// Files written (created, or changed).
    pub written: usize,
    /// Files deleted because their part of the document is gone.
    pub removed: usize,
}

/// A project folder, open for reading and editing.
///
/// The document is a tree held in memory. [`DocWrite::apply`] changes it (all of a patch or none of it), and
/// [`save`](Self::save) writes the files whose part of the tree changed. Nothing touches the disk in between, so a
/// project that is opened, edited and dropped without saving leaves its folder exactly as it was.
///
/// One `Project` should be the only thing writing a folder at a time: files changed behind its back are not
/// noticed, and a save overwrites them.
///
/// See the [crate docs](crate) for how folders and files map onto the tree.
#[derive(Debug)]
pub struct Project {
    dir: PathBuf,
    root: Value,
    version: DocVersion,
    layout: Layout,
    upgraded_from: Option<u32>,
}

impl Project {
    /// Opens the project in `dir`, upgrading it in memory if it was written by an older version.
    ///
    /// An upgraded project is not rewritten until [`save`](Self::save), so opening never changes files.
    ///
    /// # Errors
    /// The folder isn't a project, a file can't be read or parsed, two entries claim one name, the schema is newer
    /// than this build understands, or a migration fails.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let dir = dir.as_ref();
        let migrations = migrate::registered();
        let supported = migrate::latest(&migrations);
        let too_new = |project: &Map<String, Value>| match migrate::schema_of_project(project)? {
            found if found > supported => Err(StoreError::SchemaTooNew { found, supported }),
            _ => Ok(()),
        };
        let Loaded { mut root, files } = disk::load(dir, too_new)?;
        let mut layout = Layout::from_disk(files);
        let found = migrate::schema_of(&root)?;
        let upgraded_from = (found < supported).then_some(found);
        if upgraded_from.is_some() {
            let before = root.clone();
            migrate::upgrade(&mut root, found, &migrations)?;
            let edit = layout
                .plan(&root, &changed_paths(&before, &root))
                .map_err(|e| StoreError::MigratedTree(e.to_string()))?;
            layout.commit(edit);
        }
        Ok(Self { dir: dir.to_path_buf(), root, version: DocVersion(0), layout, upgraded_from })
    }

    /// Creates a project called `name` in `dir` (making the folder if needed) and writes its `project.toml`.
    ///
    /// # Errors
    /// The name is blank, `dir` already holds a project, or the file can't be written.
    pub fn create(dir: impl AsRef<Path>, name: &str) -> Result<Self, StoreError> {
        let dir = dir.as_ref();
        if name.trim().is_empty() {
            return Err(StoreError::EmptyName);
        }
        if dir.join(disk::PROJECT_FILE).exists() {
            return Err(StoreError::AlreadyExists { dir: dir.to_path_buf() });
        }
        fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
        let root = json!({"project": {"name": name, "schema": migrate::supported_schema()}});
        let mut layout = Layout::default();
        let edit =
            layout.plan(&root, &[DocPath::root().join("project")]).map_err(|e| StoreError::Internal(e.to_string()))?;
        layout.commit(edit);
        let mut project = Self { dir: dir.to_path_buf(), root, version: DocVersion(0), layout, upgraded_from: None };
        project.save()?;
        Ok(project)
    }

    /// The project folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The whole document tree.
    pub fn root(&self) -> &Value {
        &self.root
    }

    /// The schema in `project.toml` right now, if it is a valid one. After [`open`](Self::open) it is the newest
    /// this build supports.
    pub fn schema_version(&self) -> Option<u32> {
        migrate::schema_of(&self.root).ok()
    }

    /// The schema the project was saved with, if opening had to upgrade it. A prompt to save (and so keep the
    /// upgrade) belongs on this.
    pub fn upgraded_from(&self) -> Option<u32> {
        self.upgraded_from
    }

    /// True if the document differs from the files, so [`save`](Self::save) would write or delete something.
    pub fn is_dirty(&self) -> bool {
        self.layout.is_dirty()
    }

    /// Writes what changed since the last save (or since opening), then deletes files whose part of the document is
    /// gone, then writes `project.toml` (if it changed).
    ///
    /// Each file is replaced atomically (written beside the target, flushed, renamed over it). Several files are
    /// not one transaction: if the machine dies half way, some files are new and some old, which is what version
    /// control is for (one case also stops the project opening). `project.toml` goes
    /// last because it holds the schema: a save of a migrated project that stops early still says the old schema, so
    /// the migrations run again on the next open instead of the new schema being recorded over data that was never
    /// migrated. A file that was edited by hand keeps its comments, order and layout; only the values that changed
    /// are touched. If a save fails, calling it again continues where it stopped.
    ///
    /// # Errors
    /// A file can't be read, written or deleted; a file about to be edited was broken by hand and is no longer valid
    /// TOML (nothing is overwritten in that case); or a file or folder to write into is a symbolic link
    /// ([`StoreError::Symlink`]: the store never writes through or over a link).
    pub fn save(&mut self) -> Result<SaveReport, StoreError> {
        let (project, others): (Vec<_>, Vec<_>) =
            self.layout.pending().into_iter().partition(|(segs, _)| segs.as_slice() == [PROJECT_KEY]);
        let mut report = SaveReport::default();
        self.write_files(others, &mut report)?;
        // Deleting before the last write means an interrupted save never leaves a hole where a file's replacement
        // should be, and never leaves files the new schema has no place for.
        for segs in self.layout.deleted() {
            disk::remove_file(&self.dir, &segs)?;
            self.layout.mark_deleted(&segs);
            report.removed += 1;
        }
        // `project.toml` holds the schema, so it goes last: if the save stops anywhere before this, the project still
        // says it is the old schema and the migrations run again on the next open. The other order could record the
        // new schema over data that was never migrated.
        self.write_files(project, &mut report)?;
        Ok(report)
    }

    /// Writes `files` (document path and what is known about each), skipping any whose text would not change.
    fn write_files(&mut self, files: Vec<(Segs, Entry)>, report: &mut SaveReport) -> Result<(), StoreError> {
        for (segs, entry) in files {
            let path = disk::file_path(&self.dir, &segs);
            let Some(Value::Object(table)) = lookup(&self.root, &doc_path(&segs)) else {
                return Err(StoreError::Internal(format!("{} is a file but isn't an object", path.display())));
            };
            // A file edited in place is read fresh; one that vanished since it was read is simply written again.
            let current =
                if entry.pending == Pending::Edit && entry.on_disk { disk::read_text_if_exists(&path)? } else { None };
            let text = render(current.as_deref(), table).map_err(|e| match e {
                RenderError::Syntax(source) => StoreError::Syntax { path: path.clone(), source },
                RenderError::Unstorable(u) => {
                    StoreError::Internal(format!("{}: {} (at {})", path.display(), u.reason, u.at))
                }
            })?;
            if current.as_deref() != Some(text.as_str()) {
                disk::write_atomic(&self.dir, &path, &text)?;
                report.written += 1;
            }
            self.layout.mark_written(&segs);
        }
        Ok(())
    }

    /// Applies one change to the tree if it matches and can be stored.
    fn apply_change(&mut self, change: &Change) -> Result<(), PatchError> {
        let found = lookup(&self.root, &change.path);
        if found != change.before.as_ref() {
            return Err(PatchError::Conflict {
                path: change.path.clone(),
                expected: show(change.before.as_ref()),
                found: show(found),
            });
        }
        check_change(change)?;
        set(&mut self.root, &change.path, change.after.clone())
    }

    /// Reverts `applied`, the changes that succeeded before a later one failed. Each reverse step undoes a step that
    /// worked, so it can't fail on a sound tree; if it does anyway the error says the document is damaged.
    fn revert(&mut self, applied: &[Change], cause: PatchError) -> PatchError {
        for change in applied.iter().rev() {
            if let Err(e) = set(&mut self.root, &change.path, change.before.clone()) {
                return PatchError::Unreachable {
                    path: change.path.clone(),
                    reason: format!("internal error: couldn't undo a half-applied patch ({e}); reopen the project"),
                };
            }
        }
        cause
    }
}

impl DocRead for Project {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        lookup(&self.root, path)
    }

    fn version(&self) -> DocVersion {
        self.version
    }
}

impl DocWrite for Project {
    /// All of the patch or none of it, with the same conflict and unreachable-parent rules as `omuzaki-doc-mock`.
    ///
    /// A patch is also refused, with [`PatchError::Unreachable`], if it asks for something TOML files can't hold:
    /// `null`, integers above `i64::MAX`, a non-object at the top level, a replaced root, or a new name that can't
    /// be a file or folder name. (`PatchError` has no variant for "valid but not storable".)
    fn apply(&mut self, patch: &Patch) -> Result<DocVersion, PatchError> {
        for (i, change) in patch.changes.iter().enumerate() {
            if let Err(e) = self.apply_change(change) {
                return Err(self.revert(&patch.changes[..i], e));
            }
        }
        let touched: Vec<DocPath> =
            patch.changes.iter().filter(|c| c.before != c.after).map(|c| c.path.clone()).collect();
        match self.layout.plan(&self.root, &touched) {
            Ok(edit) => self.layout.commit(edit),
            Err(e) => return Err(self.revert(&patch.changes, e.into_patch_error())),
        }
        self.version = DocVersion(self.version.0 + 1);
        Ok(self.version)
    }
}
