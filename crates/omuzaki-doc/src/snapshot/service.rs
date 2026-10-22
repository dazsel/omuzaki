//! The [`Snapshots`] service of an open session (journal.md 10): what the `backup.*`, `project.save` and
//! `project.reload` ops reach snapshots and files through. The app registers it in the executor's `Services`.

use std::fs;
use std::io;
use std::path::{Component, Path};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use omuzaki_core::fsx::write_atomic_bytes;
use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_history_api::{Footprint, SnapshotError, SnapshotId, SnapshotInfo, Snapshots, SyncError, Trigger, Usage};

use super::{Snapshotter, snapshot_dir};
use crate::journal::{ProjectJournal, archive_dir, journal_dir};
use crate::toml_read::parse_table;

/// The extension of the tree's files.
const TOML: &str = ".toml";

/// The snapshots of one open project. Listing and loading work from the start; taking and saving go through the
/// session's keeper, so they need [`bind`](Self::bind) first (the executor is built, and its services fixed, before
/// the journal starts).
pub struct SessionSnapshots {
    snapshots: Arc<Snapshotter>,
    journal: OnceLock<Arc<ProjectJournal>>,
}

impl SessionSnapshots {
    /// The service over `snapshots`, before the session's journal exists.
    pub fn new(snapshots: Arc<Snapshotter>) -> Self {
        Self { snapshots, journal: OnceLock::new() }
    }

    /// Binds the session's journal. A second call is ignored: a session has one journal.
    pub fn bind(&self, journal: Arc<ProjectJournal>) {
        drop(self.journal.set(journal));
    }

    fn journal(&self) -> Result<&Arc<ProjectJournal>, SnapshotError> {
        self.journal.get().ok_or_else(|| SnapshotError::Io("the project's session hasn't started".to_owned()))
    }

    /// The ref of `id`, if one names it.
    fn info(&self, id: &SnapshotId) -> Result<SnapshotInfo, SnapshotError> {
        self.snapshots.refs()?.into_iter().find(|info| info.id == *id).ok_or(SnapshotError::NotFound(*id))
    }
}

fn from_sync(error: SyncError) -> SnapshotError {
    match error {
        SyncError::Timeout { .. } => SnapshotError::Timeout,
        other => SnapshotError::Io(other.to_string()),
    }
}

/// Bytes and files under `dir`, recursively; a missing folder is empty.
fn footprint(dir: &Path) -> Result<Footprint, SnapshotError> {
    fn walk(dir: &Path, total: &mut Footprint) -> io::Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(&entry.path(), total)?;
            } else if kind.is_file() {
                total.bytes += entry.metadata()?.len();
                total.count += 1;
            }
        }
        Ok(())
    }
    let mut total = Footprint::default();
    walk(dir, &mut total).map_err(|e| SnapshotError::Io(format!("{}: {e}", dir.display())))?;
    Ok(total)
}

/// The document path of the tree file `rel` (`characters/mia/face.toml` is `characters/mia/face`), refusing anything
/// that isn't a relative path to a tree file inside the project.
fn tree_path(rel: &str) -> Result<DocPath, SnapshotError> {
    let invalid = || SnapshotError::Invalid(format!("{rel} isn't a project file (a relative path to a .toml file)"));
    let stem = rel.strip_suffix(TOML).filter(|s| !s.is_empty()).ok_or_else(invalid)?;
    let path = Path::new(stem);
    let mut doc = DocPath::root();
    for component in path.components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_str().filter(|n| !n.starts_with('.')).ok_or_else(invalid)?;
                doc = doc.join(name);
            }
            _ => return Err(invalid()),
        }
    }
    Ok(doc)
}

impl Snapshots for SessionSnapshots {
    fn list(&self) -> Result<Vec<SnapshotInfo>, SnapshotError> {
        self.snapshots.refs()
    }

    fn take(&self, trigger: Trigger, name: Option<&str>) -> Result<SnapshotId, SnapshotError> {
        let allowed = match trigger {
            Trigger::Named => name.is_some_and(|n| !n.trim().is_empty()),
            Trigger::Take => true,
            _ => name.is_none(),
        };
        if !allowed {
            return Err(SnapshotError::Invalid(
                "a name goes with a named version, a stem with a take, and no other snapshot takes one".to_owned(),
            ));
        }
        let wait = Duration::from_secs(30);
        let info = self.journal()?.take_snapshot(trigger, name.map(str::to_owned), wait).map_err(from_sync)?;
        Ok(info.id)
    }

    fn tree(&self, id: &SnapshotId) -> Result<Value, SnapshotError> {
        self.info(id)?;
        Ok(self.snapshots.load(id)?.root().clone())
    }

    fn restore_outside_tree(&self, id: &SnapshotId) -> Result<(), SnapshotError> {
        self.info(id)?;
        let dir = self.snapshots.dir();
        for (rel, bytes) in self.snapshots.files(id)? {
            if rel.ends_with(TOML) {
                continue;
            }
            let path = dir.join(&rel);
            let io = |e: io::Error| SnapshotError::Io(format!("{}: {e}", path.display()));
            if fs::read(&path).is_ok_and(|now| now == bytes) {
                continue;
            }
            write_atomic_bytes(&path, &bytes).map_err(io)?;
        }
        Ok(())
    }

    fn save_files(&self, timeout: Duration) -> Result<(), SnapshotError> {
        self.journal()?.save_now(timeout).map(drop).map_err(from_sync)
    }

    fn reload_patch(&self, paths: &[String]) -> Result<Patch, SnapshotError> {
        let dir = self.snapshots.dir();
        let mut every = Vec::new();
        if paths.is_empty() {
            crate::exact::tree_files(dir, &mut Vec::new(), &mut every).map_err(|e| SnapshotError::Io(e.to_string()))?;
        }
        let paths = if paths.is_empty() { every.as_slice() } else { paths };
        let mut patch = Patch::new();
        for rel in paths {
            let at = tree_path(rel)?;
            let file = dir.join(rel);
            let after = match fs::read_to_string(&file) {
                Ok(text) => Some(Value::Object(
                    parse_table(&text).map_err(|e| SnapshotError::Invalid(format!("{rel} doesn't read: {e:?}")))?,
                )),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(SnapshotError::Io(format!("{}: {e}", file.display()))),
            };
            // The op that applies this fills in each `before` from the document it plans against.
            patch = patch.with(at, None, after);
        }
        Ok(patch)
    }

    fn usage(&self) -> Result<Usage, SnapshotError> {
        let dir = self.snapshots.dir();
        let mut usage = Usage::default();
        usage.journal = footprint(&journal_dir(dir))?;
        usage.archive = footprint(&archive_dir(dir))?;
        usage.snapshots = footprint(&snapshot_dir(dir))?;
        Ok(usage)
    }
}
