//! [`Project`]: a project folder held in memory as a document tree.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use omuzaki_doc_api::{Change, DocPath, DocRead, DocVersion, DocWrite, Patch, PatchError, lookup};
use omuzaki_store_api::ContentHash;
use serde_json::{Map, Value, json};

use crate::disk::{self, Loaded, PROJECT_KEY, Replace};
use crate::error::StoreError;
use crate::known::Known;
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

/// One project file as a save would write it, rendered from memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedFile {
    /// The file's path relative to the project folder, for example `characters/mia/face.toml`.
    pub path: PathBuf,
    /// The text the file has, or would have after a save.
    pub text: String,
    /// The hash of `text`'s bytes.
    pub hash: ContentHash,
    /// Whether a save would write it: the text differs from the last-known text, or the file is new.
    pub changed: bool,
}

/// A project folder, open for reading and editing.
///
/// The document is a tree held in memory. [`DocWrite::apply`] changes it (all of a patch or none of it), and
/// [`save`](Self::save) writes the files whose part of the tree changed. Nothing touches the disk in between, so a
/// project that is opened, edited and dropped without saving leaves its folder exactly as it was.
///
/// The project keeps the exact text and hash of every file as it last read or wrote it. Saves are rendered against
/// that text, never a fresh read, and before a file is replaced its hash on disk must still be the last-known one:
/// a file edited outside Omuzaki is refused with [`StoreError::ChangedOnDisk`], never overwritten.
///
/// Cloning a project copies the tree, the layout and the known texts, so the clone is independent.
///
/// See the [crate docs](crate) for how folders and files map onto the tree.
#[derive(Clone, Debug)]
pub struct Project {
    dir: PathBuf,
    root: Value,
    version: DocVersion,
    layout: Layout,
    known: Known,
    upgraded_from: Option<u32>,
    /// When this project last pruned the replaced folder; `None` until its first save.
    pruned_at: Option<Instant>,
}

/// How often saves prune the replaced folder: often enough to keep it small, rarely enough that walking it doesn't
/// slow saves that come every few seconds.
const PRUNE_EVERY: Duration = Duration::from_mins(1);

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
        let mut known = Known::default();
        let mut layout_files = Vec::with_capacity(files.len());
        for file in files {
            layout_files.push((file.segs.clone(), file.sticky));
            let hash = ContentHash::of(file.text.as_bytes());
            known.record(file.segs, file.text, hash);
        }
        let mut layout = Layout::from_disk(layout_files);
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
        Ok(Self { dir: dir.to_path_buf(), root, version: DocVersion(0), layout, known, upgraded_from, pruned_at: None })
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
        let mut project = Self {
            dir: dir.to_path_buf(),
            root,
            version: DocVersion(0),
            layout,
            known: Known::default(),
            upgraded_from: None,
            pruned_at: None,
        };
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

    /// The hash of every file as this project last read or wrote it, by path relative to the project folder.
    pub fn file_hashes(&self) -> BTreeMap<PathBuf, ContentHash> {
        self.known.hashes().map(|(segs, hash)| (disk::file_path(Path::new(""), segs), hash)).collect()
    }

    /// Every file of the project as a save would leave it, in path order, rendered from memory: no disk reads.
    ///
    /// Files with pending edits are rendered against their last-known text, exactly as [`save`](Self::save) does,
    /// and are marked `changed` when that differs from it; the rest are returned as last known.
    ///
    /// # Errors
    /// A file's last-known text is no longer valid TOML, or the tree holds something TOML can't store (both are
    /// bugs in the store, not in the caller).
    pub fn rendered_files(&self) -> Result<Vec<RenderedFile>, StoreError> {
        self.layout
            .entries()
            .map(|(segs, entry)| {
                let Rendered { text, hash, changed } = self.render_file(segs, *entry)?;
                Ok(RenderedFile { path: disk::file_path(Path::new(""), segs), text, hash, changed })
            })
            .collect()
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
    /// A file is replaced only if its bytes on disk are the ones this project last read or wrote. That is checked on
    /// the file itself after it has been moved aside (so an editor's save can't slip in between the check and the
    /// replacement); see `disk::replace_checked`. A file that
    /// isn't what was expected is put back and left alone, the save carries on with the rest, and the first such
    /// file is reported as [`StoreError::ChangedOnDisk`] (or [`StoreError::Syntax`] if it no longer parses).
    /// `project.toml` is not written after a refusal, for the reason above: the files it describes aren't all up to
    /// date.
    ///
    /// Each file a save replaces is kept for a while under `.omuzaki/replaced/`, so an editor that still holds the
    /// old file open can't lose what it writes there. The first save of a `Project`, and then at most one save a
    /// minute, deletes up to 64 of those that are over 10 minutes old and that nothing wrote to since (see
    /// `docs/design/journal.md` section 6.1).
    ///
    /// # Errors
    /// A file can't be read, written or deleted; a file about to be edited was broken by hand and is no longer valid
    /// TOML (nothing is overwritten in that case); a file or folder to write into is a symbolic link
    /// ([`StoreError::Symlink`]: the store never writes through or over a link); or a file was changed on disk
    /// ([`StoreError::ChangedOnDisk`]).
    pub fn save(&mut self) -> Result<SaveReport, StoreError> {
        let now = Instant::now();
        if self.pruned_at.is_none_or(|at| now.duration_since(at) >= PRUNE_EVERY) {
            disk::prune_replaced(&self.dir, SystemTime::now());
            self.pruned_at = Some(now);
        }
        let (project, others): (Vec<_>, Vec<_>) =
            self.layout.pending().into_iter().partition(|(segs, _)| segs.as_slice() == [PROJECT_KEY]);
        let mut report = SaveReport::default();
        let mut refused = None;
        self.write_files(others, &mut report, &mut refused)?;
        // Deleting before the last write means an interrupted save never leaves a hole where a file's replacement
        // should be, and never leaves files the new schema has no place for.
        for segs in self.layout.deleted() {
            // Like a replacement, a deletion needs the file to hold the bytes this project last saw.
            if !disk::remove_checked(&self.dir, &segs, self.known.hash(&segs))? {
                refused.get_or_insert(disk::why_changed(disk::file_path(&self.dir, &segs)));
                continue;
            }
            self.layout.mark_deleted(&segs);
            self.known.forget(&segs);
            report.removed += 1;
        }
        // `project.toml` holds the schema, so it goes last: if the save stops anywhere before this, the project still
        // says it is the old schema and the migrations run again on the next open. The other order could record the
        // new schema over data that was never migrated. A refused file counts as stopping early.
        if refused.is_none() {
            self.write_files(project, &mut report, &mut refused)?;
        }
        refused.map_or(Ok(report), Err)
    }

    /// Renders the file `segs` from memory: pending edits against its last-known text, a fresh file from scratch.
    /// `changed` says whether it is new or differs from the last-known text.
    fn render_file(&self, segs: &[String], entry: Entry) -> Result<Rendered, StoreError> {
        let known = self.known.text(segs);
        if entry.pending == Pending::Clean {
            let (text, hash) = known.zip(self.known.hash(segs)).ok_or_else(|| missing_text(segs))?;
            return Ok(Rendered { text: text.to_owned(), hash, changed: false });
        }
        let path = disk::file_path(&self.dir, segs);
        let Some(Value::Object(table)) = lookup(&self.root, &doc_path(segs)) else {
            return Err(StoreError::Internal(format!("{} is a file but isn't an object", path.display())));
        };
        // A file edited in place starts from its last-known text; a new one, or one deleted and made again, from nothing.
        let base = if entry.pending == Pending::Edit && entry.on_disk {
            Some(known.ok_or_else(|| missing_text(segs))?)
        } else {
            None
        };
        let text = render(base, table).map_err(|e| match e {
            RenderError::Syntax(source) => StoreError::Syntax { path: path.clone(), source },
            RenderError::Unstorable(u) => {
                StoreError::Internal(format!("{}: {} (at {})", path.display(), u.reason, u.at))
            }
        })?;
        let changed = base != Some(text.as_str());
        let hash = ContentHash::of(text.as_bytes());
        Ok(Rendered { text, hash, changed })
    }

    /// Writes `files` (document path and what is known about each), skipping any whose text would not change and
    /// any that changed on disk. The first of those is left in `refused`.
    fn write_files(
        &mut self,
        files: Vec<(Segs, Entry)>,
        report: &mut SaveReport,
        refused: &mut Option<StoreError>,
    ) -> Result<(), StoreError> {
        for (segs, entry) in files {
            let rendered = self.render_file(&segs, entry)?;
            if rendered.changed {
                let path = disk::file_path(&self.dir, &segs);
                let expected = self.known.hash(&segs);
                if disk::replace_checked(&self.dir, &path, &rendered.text, expected)? == Replace::Refused {
                    refused.get_or_insert(disk::why_changed(path));
                    continue;
                }
                self.known.record(segs.clone(), rendered.text, rendered.hash);
                report.written += 1;
            } else if entry.pending == Pending::Edit {
                // An edit that was undone renders the text the file had. It is clean only if the regular file that was
                // loaded is still there with those bytes. A symbolic link at the file or at a folder above it is never
                // followed (the same check `replace_checked` makes): it is an external change and the edit stays owed.
                let path = disk::file_path(&self.dir, &segs);
                if let Err(e) = disk::refuse_links(&self.dir, &path) {
                    refused.get_or_insert(e);
                    continue;
                }
                if self.vanished(&segs) {
                    // The file itself is gone: it is written again, from the last-known text, like any other file
                    // deleted behind our back.
                    if disk::replace_checked(&self.dir, &path, &rendered.text, None)? == Replace::Refused {
                        refused.get_or_insert(disk::why_changed(path));
                        continue;
                    }
                    report.written += 1;
                } else if self.differs_on_disk(&segs)? {
                    // The file was changed behind our back since: it is not ours to mark as written.
                    refused.get_or_insert(disk::why_changed(path));
                    continue;
                }
            }
            self.layout.mark_written(&segs);
        }
        Ok(())
    }

    /// True if something exists at the path of `segs` that isn't a regular file with the last-known bytes. The caller
    /// has already refused links on the path, so this never follows one.
    fn differs_on_disk(&self, segs: &[String]) -> Result<bool, StoreError> {
        let path = disk::file_path(&self.dir, segs);
        match fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_file() => return Ok(true),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(StoreError::io(&path, e)),
        }
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(ContentHash::of(&bytes)) != self.known.hash(segs)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(StoreError::io(&path, e)),
        }
    }

    /// True if nothing exists at the path of the file `segs` (not even a broken link).
    fn vanished(&self, segs: &[String]) -> bool {
        fs::symlink_metadata(disk::file_path(&self.dir, segs)).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
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

/// The outcome of rendering one file.
struct Rendered {
    text: String,
    hash: ContentHash,
    changed: bool,
}

fn missing_text(segs: &[String]) -> StoreError {
    StoreError::Internal(format!("no last-known text for {}", segs.join("/")))
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
