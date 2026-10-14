//! The project folder: reading it into a tree, and writing, replacing and deleting its files.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::StoreError;
use crate::layout::Segs;
use crate::toml_read::{ReadError, parse_table};

/// Extension of the files that make up the tree.
const EXT: &str = ".toml";

/// The file that holds the project's own settings; a folder without it isn't a project.
pub(crate) const PROJECT_FILE: &str = "project.toml";

/// The document key of [`PROJECT_FILE`]. `save` writes it last (see `Project::save`).
pub(crate) const PROJECT_KEY: &str = "project";

/// A project folder read into memory.
#[derive(Debug)]
pub(crate) struct Loaded {
    /// The whole tree.
    pub root: Value,
    /// The document path of every file read, each with whether it has content.
    pub files: Vec<(Segs, bool)>,
}

/// The path of the file that holds the document path `segs`: `a/b` is `<dir>/a/b.toml`.
pub(crate) fn file_path(dir: &Path, segs: &[String]) -> PathBuf {
    let mut path = dir.to_path_buf();
    path.extend(segs);
    path.as_mut_os_string().push(EXT);
    path
}

/// Reads the folder `dir` into a tree.
///
/// `project.toml` is read first and on its own, and `accept` looks at it before anything else is read. That is how
/// a project from a newer Omuzaki is reported as that, and not as a parse error in some other file that only the newer
/// version understands.
///
/// Folders are objects and `<name>.toml` files are objects under `<name>`. Everything else is not part of the tree
/// and is left alone: other file types, anything whose name starts with `.` (like `.omuzaki/` and the temporary files
/// `save` leaves behind if it is interrupted), symbolic links (so a link can never lead the store out of the
/// project), names that aren't valid UTF-8, and folders holding no TOML.
pub(crate) fn load(
    dir: &Path,
    accept: impl FnOnce(&Map<String, Value>) -> Result<(), StoreError>,
) -> Result<Loaded, StoreError> {
    let project_file = dir.join(PROJECT_FILE);
    if !fs::symlink_metadata(&project_file).is_ok_and(|m| m.is_file()) {
        return Err(StoreError::NotAProject { dir: dir.to_path_buf() });
    }
    accept(&parse_file(&project_file)?)?;
    // The walk below reads `project.toml` again; one small file is cheaper than special-casing it.
    let mut files = Vec::new();
    let root = Value::Object(load_folder(dir, &mut Vec::new(), &mut files)?);
    Ok(Loaded { root, files })
}

/// What an entry of the tree is on disk. Its name is the folder's name, or the file's without `.toml`.
enum Kind {
    Folder,
    Toml,
}

fn load_folder(dir: &Path, at: &mut Segs, files: &mut Vec<(Segs, bool)>) -> Result<Map<String, Value>, StoreError> {
    let mut map = Map::new();
    for (name, kind) in list(dir)? {
        match kind {
            Kind::Folder => {
                at.push(name.clone());
                let child = load_folder(&dir.join(&name), at, files)?;
                at.pop();
                if !child.is_empty() {
                    insert(&mut map, dir, &name, Value::Object(child))?;
                }
            }
            Kind::Toml => {
                let table = parse_file(&dir.join(format!("{name}{EXT}")))?;
                at.push(name.clone());
                files.push((at.clone(), !table.is_empty()));
                at.pop();
                insert(&mut map, dir, &name, Value::Object(table))?;
            }
        }
    }
    Ok(map)
}

/// The entries of `dir` that belong to the tree, by name.
fn list(dir: &Path) -> Result<Vec<(String, Kind)>, StoreError> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| StoreError::io(dir, e))? {
        let entry = entry.map_err(|e| StoreError::io(dir, e))?;
        let Ok(name) = entry.file_name().into_string() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let kind = entry.file_type().map_err(|e| StoreError::io(entry.path(), e))?;
        if kind.is_dir() {
            found.push((name, Kind::Folder));
        } else if kind.is_file()
            && let Some(stem) = name.strip_suffix(EXT).filter(|s| !s.is_empty())
        {
            found.push((stem.to_owned(), Kind::Toml));
        }
    }
    // Read order is the operating system's; sorting makes which error is reported first repeatable.
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

fn insert(map: &mut Map<String, Value>, dir: &Path, key: &str, value: Value) -> Result<(), StoreError> {
    if map.insert(key.to_owned(), value).is_some() {
        return Err(StoreError::Ambiguous {
            key: key.to_owned(),
            file: dir.join(format!("{key}{EXT}")),
            folder: dir.join(key),
        });
    }
    Ok(())
}

fn parse_file(path: &Path) -> Result<Map<String, Value>, StoreError> {
    parse_table(&read_text(path)?).map_err(|e| match e {
        ReadError::Syntax(source) => StoreError::Syntax { path: path.to_path_buf(), source },
        ReadError::Unsupported { at, reason } => {
            StoreError::Unsupported { path: path.to_path_buf(), reason: format!("{reason} (at {at})") }
        }
    })
}

/// The text of a file.
pub(crate) fn read_text(path: &Path) -> Result<String, StoreError> {
    fs::read_to_string(path).map_err(|e| StoreError::io(path, e))
}

/// The text of a file, or `None` if it isn't there (someone deleted it since it was read).
pub(crate) fn read_text_if_exists(path: &Path) -> Result<Option<String>, StoreError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::io(path, e)),
    }
}

/// Fails if `path` (a file in the project `dir`) is a symbolic link, or lies inside a folder that is one.
///
/// The loader skips links, so the tree never contains what is behind them; writing there anyway would put files
/// outside the project (through a linked folder) or silently replace the user's link (a linked file). Components
/// that don't exist yet are fine: the store creates those as real folders. `dir` itself may be reached through a
/// link, as a project opened by a shortcut path is.
fn refuse_links(dir: &Path, path: &Path) -> Result<(), StoreError> {
    let below = path
        .strip_prefix(dir)
        .map_err(|_| StoreError::Internal(format!("{} isn't inside {}", path.display(), dir.display())))?;
    let mut current = dir.to_path_buf();
    for part in below.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(StoreError::Symlink { path: current }),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(StoreError::io(current, e)),
        }
    }
    Ok(())
}

/// Replaces the file at `path` (inside the project `dir`) with `text`, creating folders as needed.
///
/// The text goes to a hidden temporary file next to the target, is flushed to disk, then renamed over the target,
/// so a crash leaves either the old file or the new one, never half of one. (The loader skips the leftover
/// temporary file of an interrupted save: it starts with `.`.)
pub(crate) fn write_atomic(dir: &Path, path: &Path, text: &str) -> Result<(), StoreError> {
    refuse_links(dir, path)?;
    let io_err = |e| StoreError::io(path, e);
    let Some(parent) = path.parent() else { return Err(io_err(io::ErrorKind::InvalidInput.into())) };
    fs::create_dir_all(parent).map_err(io_err)?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(path.file_name().unwrap_or_default());
    tmp_name.push(".tmp");
    let tmp = parent.join(tmp_name);
    // `create_new` never follows a link, so what an interrupted save left under this name (or a link put there) is
    // removed first.
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(StoreError::io(&tmp, e)),
    }
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()));
    written.and_then(|()| fs::rename(&tmp, path)).map_err(|e| {
        // Best effort: the original error is the one worth reporting.
        drop(fs::remove_file(&tmp));
        io_err(e)
    })
}

/// Deletes the file for the document path `segs`, then any folders it leaves empty (never `dir` itself).
/// A file that is already gone is fine.
pub(crate) fn remove_file(dir: &Path, segs: &[String]) -> Result<(), StoreError> {
    let path = file_path(dir, segs);
    refuse_links(dir, &path)?;
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(StoreError::io(&path, e)),
    }
    let mut folder = path.parent();
    while let Some(f) = folder.filter(|f| *f != dir) {
        // `remove_dir` refuses a folder that still holds anything, which is exactly when to stop.
        if fs::remove_dir(f).is_err() {
            break;
        }
        folder = f.parent();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn folders_and_toml_files_become_the_tree_and_the_rest_is_ignored() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "project.toml", "name = \"x\"\n");
        write(d, "characters/mia/face.toml", "profile = \"round\"\n");
        write(d, "characters/mia/variants/game.toml", "scale = 2\n");
        write(d, "characters/mia/notes.txt", "not toml");
        write(d, "performances/p1/takes/1.track", "binary");
        write(d, ".omuzaki/cache.toml", "a = 1\n");
        write(d, "library.lock", "x");
        write(d, "scenes/.hidden.toml", "a = 1\n");
        write(d, "scenes/empty.toml", "");
        let loaded = load(d, |_| Ok(())).unwrap();
        assert_eq!(
            loaded.root,
            json!({
                "project": {"name": "x"},
                "characters": {"mia": {"face": {"profile": "round"}, "variants": {"game": {"scale": 2}}}},
                "scenes": {"empty": {}},
            })
        );
        let mut files: Vec<String> = loaded.files.iter().map(|(s, _)| s.join("/")).collect();
        files.sort();
        assert_eq!(files, ["characters/mia/face", "characters/mia/variants/game", "project", "scenes/empty"]);
        assert!(loaded.files.iter().any(|(s, content)| s == &["scenes", "empty"] && !content));
    }

    #[test]
    fn a_folder_without_project_toml_is_not_a_project() {
        let tmp = TempDir::new().unwrap();
        assert!(matches!(load(tmp.path(), |_| Ok(())), Err(StoreError::NotAProject { .. })));
    }

    #[test]
    fn a_file_and_a_folder_with_one_name_are_reported() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "project.toml", "");
        write(d, "scenes.toml", "a = 1\n");
        write(d, "scenes/s1.toml", "b = 2\n");
        let err = load(d, |_| Ok(())).unwrap_err();
        assert!(matches!(&err, StoreError::Ambiguous { key, .. } if key == "scenes"), "{err}");
        assert!(err.to_string().contains("scenes.toml") && err.to_string().contains("scenes"));
    }

    #[test]
    fn a_syntax_error_names_the_file() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "project.toml", "name = \n");
        let err = load(tmp.path(), |_| Ok(())).unwrap_err();
        assert!(matches!(err, StoreError::Syntax { .. }));
        assert!(err.to_string().contains("project.toml"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_not_followed() {
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        write(outside.path(), "secret.toml", "a = 1\n");
        write(tmp.path(), "project.toml", "");
        std::os::unix::fs::symlink(outside.path(), tmp.path().join("linked")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.toml"), tmp.path().join("secret.toml")).unwrap();
        assert_eq!(load(tmp.path(), |_| Ok(())).unwrap().root, json!({"project": {}}));
    }

    #[test]
    fn the_project_file_is_judged_before_anything_else_is_read() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "project.toml", "schema = 99\n");
        write(tmp.path(), "future.toml", "v = inf\n");
        let refuse = |_: &Map<String, Value>| Err(StoreError::BadSchema("too new".into()));
        assert!(matches!(load(tmp.path(), refuse), Err(StoreError::BadSchema(_))));
        assert!(matches!(load(tmp.path(), |_| Ok(())), Err(StoreError::Unsupported { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn writing_through_or_over_a_link_is_refused() {
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let dir = tmp.path();
        write(outside.path(), "kept.toml", "a = 1\n");
        std::os::unix::fs::symlink(outside.path(), dir.join("linked")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("kept.toml"), dir.join("file.toml")).unwrap();
        write(dir, "real/x.toml", "");
        std::os::unix::fs::symlink(outside.path().join("kept.toml"), dir.join("real/.x.toml.tmp")).unwrap();

        for target in ["linked/new.toml", "linked/deep/new.toml", "file.toml"] {
            let err = write_atomic(dir, &dir.join(target), "b = 2\n").unwrap_err();
            assert!(matches!(err, StoreError::Symlink { .. }), "{target}: {err}");
            assert!(err.to_string().contains("symbolic link"), "{err}");
        }
        let err = remove_file(dir, &["linked".into(), "kept".into()]).unwrap_err();
        assert!(matches!(err, StoreError::Symlink { .. }), "{err}");
        assert_eq!(fs::read_to_string(outside.path().join("kept.toml")).unwrap(), "a = 1\n");
        assert_eq!(all_names(outside.path()), ["kept.toml"]);

        // A link sitting where the temporary file goes is replaced, not written through.
        write_atomic(dir, &dir.join("real/x.toml"), "c = 3\n").unwrap();
        assert_eq!(fs::read_to_string(dir.join("real/x.toml")).unwrap(), "c = 3\n");
        assert_eq!(fs::read_to_string(outside.path().join("kept.toml")).unwrap(), "a = 1\n");
    }

    #[cfg(unix)]
    fn all_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_temporary_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a/b.toml");
        write_atomic(tmp.path(), &path, "x = 1\n").unwrap();
        write_atomic(tmp.path(), &path, "x = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "x = 2\n");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["b.toml"]);
    }

    #[test]
    fn remove_file_prunes_empty_folders_but_not_ones_with_other_files() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a/b/c.toml", "");
        write(d, "a/keep/x.track", "");
        write(d, "a/keep/y.toml", "");
        remove_file(d, &["a".into(), "b".into(), "c".into()]).unwrap();
        assert!(!d.join("a/b").exists() && d.join("a").exists());
        remove_file(d, &["a".into(), "keep".into(), "y".into()]).unwrap();
        assert!(d.join("a/keep/x.track").exists());
        remove_file(d, &["a".into(), "keep".into(), "y".into()]).unwrap(); // already gone: fine
    }
}
