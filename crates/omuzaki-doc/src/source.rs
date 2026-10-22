//! Where a project's files are read from: its folder, or a set of file bytes (a snapshot). One loader reads both
//! (`disk::load_from`), so a snapshot opens by exactly the rules a folder does.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::StoreError;

/// What an entry of a folder is, as far as the tree cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    /// A folder.
    Folder,
    /// A regular file.
    File,
    /// Anything else (a link, a device): never part of the tree.
    Other,
}

/// The files of a project, read either from its folder or from memory. Paths are relative to the project, as
/// segments; `dir` is where the project lives (or would save), and names files in errors.
pub(crate) enum FileSource<'a> {
    /// The project folder on disk.
    Folder(&'a Path),
    /// File bytes by `/`-separated relative path, as a snapshot holds them, for a project that saves into `dir`.
    Files { dir: &'a Path, files: &'a BTreeMap<String, Vec<u8>> },
}

impl FileSource<'_> {
    /// The project's folder: where it saves, and what errors name.
    pub(crate) fn dir(&self) -> &Path {
        match self {
            Self::Folder(dir) | Self::Files { dir, .. } => dir,
        }
    }

    /// The path `rel` names, for errors.
    pub(crate) fn path(&self, rel: &[String]) -> PathBuf {
        let mut path = self.dir().to_path_buf();
        path.extend(rel);
        path
    }

    /// Whether `rel` is a regular file.
    pub(crate) fn is_file(&self, rel: &[String]) -> bool {
        match self {
            Self::Folder(_) => fs::symlink_metadata(self.path(rel)).is_ok_and(|m| m.is_file()),
            Self::Files { files, .. } => files.contains_key(&rel.join("/")),
        }
    }

    /// The entries of the folder `rel` that aren't hidden (names starting with `.`) and whose names are UTF-8, sorted
    /// by name so which error is reported first is repeatable.
    pub(crate) fn list(&self, rel: &[String]) -> Result<Vec<(String, EntryKind)>, StoreError> {
        let mut found = match self {
            Self::Folder(_) => {
                let dir = self.path(rel);
                let mut found = Vec::new();
                for entry in fs::read_dir(&dir).map_err(|e| StoreError::io(&dir, e))? {
                    let entry = entry.map_err(|e| StoreError::io(&dir, e))?;
                    let Ok(name) = entry.file_name().into_string() else { continue };
                    let kind = entry.file_type().map_err(|e| StoreError::io(entry.path(), e))?;
                    let kind = if kind.is_dir() {
                        EntryKind::Folder
                    } else if kind.is_file() {
                        EntryKind::File
                    } else {
                        EntryKind::Other
                    };
                    found.push((name, kind));
                }
                found
            }
            Self::Files { files, .. } => {
                let prefix = if rel.is_empty() { String::new() } else { format!("{}/", rel.join("/")) };
                let mut found: BTreeMap<String, EntryKind> = BTreeMap::new();
                for path in files.keys().filter_map(|p| p.strip_prefix(&prefix)) {
                    match path.split_once('/') {
                        Some((folder, _)) => {
                            found.insert(folder.to_owned(), EntryKind::Folder);
                        }
                        None => {
                            found.entry(path.to_owned()).or_insert(EntryKind::File);
                        }
                    }
                }
                found.into_iter().collect()
            }
        };
        found.retain(|(name, _)| !name.is_empty() && !name.starts_with('.'));
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    /// The text of the file `rel`.
    pub(crate) fn read_text(&self, rel: &[String]) -> Result<String, StoreError> {
        let path = self.path(rel);
        match self {
            Self::Folder(_) => fs::read_to_string(&path).map_err(|e| StoreError::io(&path, e)),
            Self::Files { files, .. } => {
                let bytes = files
                    .get(&rel.join("/"))
                    .ok_or_else(|| StoreError::io(&path, io::Error::from(io::ErrorKind::NotFound)))?;
                String::from_utf8(bytes.clone()).map_err(|_| {
                    StoreError::io(&path, io::Error::new(io::ErrorKind::InvalidData, "stream did not contain valid UTF-8"))
                })
            }
        }
    }
}
