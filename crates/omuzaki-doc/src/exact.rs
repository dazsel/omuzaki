//! Writing the folder to match a project exactly (journal.md 8.5): what recovery and a backup restore do, where an
//! ordinary save would refuse files that aren't what it last saw.

use std::fs;
use std::path::{Path, PathBuf};

use omuzaki_core::fsx::write_atomic;

use crate::disk::PROJECT_FILE;
use crate::error::StoreError;
use crate::{Project, SaveReport};

/// The extension of the tree's files.
const TOML: &str = ".toml";

impl Project {
    /// Writes the folder to match this project exactly: every tree file of the project (each atomically), then every
    /// tree file under the folder that the project doesn't have is removed, with the folders that held only those,
    /// `project.toml` last. Other files, names starting with `.` and links are never touched. Afterwards the project is
    /// the folder as reopened, so its last-known texts are the files' and saves work from there.
    ///
    /// Unlike [`save`](Self::save), this replaces files whatever they hold: it is for recovery and restore, after a raw
    /// snapshot has kept what was there. It is idempotent: an interrupted run completes when run again.
    ///
    /// # Errors
    /// A file can't be rendered, written or removed, or the folder doesn't reopen to the same tree (a bug).
    pub fn write_exact(&mut self) -> Result<SaveReport, StoreError> {
        let dir = self.dir().to_path_buf();
        let files = self.rendered_files()?;
        let mut report = SaveReport::default();
        let keep: Vec<String> = files.iter().map(|f| slash(&f.path)).collect();
        let (last, first): (Vec<_>, Vec<_>) = files.into_iter().partition(|f| f.path == Path::new(PROJECT_FILE));
        for file in &first {
            report.written += usize::from(write_if_different(&dir.join(&file.path), &file.text)?);
        }
        let mut stray = Vec::new();
        tree_files(&dir, &mut Vec::new(), &mut stray)?;
        for rel in stray.iter().filter(|rel| !keep.contains(rel)) {
            let path = dir.join(rel);
            fs::remove_file(&path).map_err(|e| StoreError::io(&path, e))?;
            report.removed += 1;
            // Folders emptied by the removal go too, up to the project folder.
            let mut folder = path.parent().map(Path::to_path_buf);
            while let Some(f) = folder.filter(|f| *f != dir) {
                if fs::remove_dir(&f).is_err() {
                    break;
                }
                folder = f.parent().map(Path::to_path_buf);
            }
        }
        for file in &last {
            report.written += usize::from(write_if_different(&dir.join(&file.path), &file.text)?);
        }
        let reopened = Project::open(&dir)?;
        if reopened.root() != self.root() {
            return Err(StoreError::Internal(format!(
                "{} was written to match the project, but reads back differently",
                dir.display()
            )));
        }
        *self = reopened;
        Ok(report)
    }
}

/// Writes `text` to `path` atomically unless it already holds exactly that. Returns whether it wrote.
fn write_if_different(path: &Path, text: &str) -> Result<bool, StoreError> {
    if fs::read(path).is_ok_and(|bytes| bytes == text.as_bytes()) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
    }
    write_atomic(path, text).map_err(|e| StoreError::io(path, e))?;
    Ok(true)
}

/// Every tree file under `dir/rel`, `/`-separated: regular `.toml` files with a name, in folders that aren't hidden.
pub(crate) fn tree_files(dir: &Path, rel: &mut Vec<String>, out: &mut Vec<String>) -> Result<(), StoreError> {
    let mut path: PathBuf = dir.to_path_buf();
    path.extend(rel.iter());
    for entry in fs::read_dir(&path).map_err(|e| StoreError::io(&path, e))? {
        let entry = entry.map_err(|e| StoreError::io(&path, e))?;
        let Ok(name) = entry.file_name().into_string() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let kind = entry.file_type().map_err(|e| StoreError::io(entry.path(), e))?;
        rel.push(name.clone());
        if kind.is_dir() {
            tree_files(dir, rel, out)?;
        } else if kind.is_file() && name.len() > TOML.len() && name.ends_with(TOML) {
            out.push(rel.join("/"));
        }
        rel.pop();
    }
    Ok(())
}

fn slash(path: &Path) -> String {
    path.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}
