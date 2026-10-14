//! The project folder: reading it into a tree, and writing, replacing and deleting its files.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use omuzaki_store_api::ContentHash;
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
    /// Every file read.
    pub files: Vec<LoadedFile>,
}

/// One file read from the folder.
#[derive(Debug)]
pub(crate) struct LoadedFile {
    /// Its document path.
    pub segs: Segs,
    /// Whether it has content.
    pub sticky: bool,
    /// Its exact text, kept so a save can edit it and notice if it changes on disk.
    pub text: String,
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
    accept(&parse_file(&project_file)?.0)?;
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

fn load_folder(dir: &Path, at: &mut Segs, files: &mut Vec<LoadedFile>) -> Result<Map<String, Value>, StoreError> {
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
                let (table, text) = parse_file(&dir.join(format!("{name}{EXT}")))?;
                at.push(name.clone());
                files.push(LoadedFile { segs: at.clone(), sticky: !table.is_empty(), text });
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

/// The table in a file, and the file's text.
fn parse_file(path: &Path) -> Result<(Map<String, Value>, String), StoreError> {
    let text = read_text(path)?;
    parse_table(&text).map(|table| (table, text)).map_err(|e| match e {
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

/// The error for a file that isn't what this process last saw: [`StoreError::Syntax`] (or `Unsupported`) if it no
/// longer parses, which says more than that it changed, and otherwise [`StoreError::ChangedOnDisk`].
pub(crate) fn why_changed(path: PathBuf) -> StoreError {
    match parse_file(&path) {
        Err(e @ (StoreError::Syntax { .. } | StoreError::Unsupported { .. })) => e,
        _ => StoreError::ChangedOnDisk { path },
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

/// The folder (inside the project) that holds a backup of every file a save replaced, and every file a save displaced.
/// Only an explicit cleanup policy ever deletes from it; a save never does.
const REPLACED_DIR: &str = ".omuzaki/replaced";

/// What [`replace_checked`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Replace {
    /// The new text is in place.
    Placed,
    /// The file at the path was not the one expected (or was replaced mid-save); our text was not placed over it.
    Refused,
}

/// Replaces the file at `path` (inside the project `dir`) with `text`, but only if the file there is `expected`
/// (`None`: there must be no file). A file that is absent is simply written, creating folders as needed.
///
/// Checking a hash and then renaming leaves a gap in which an editor's save would be overwritten, and moving the file
/// away first leaves a gap in which the path is missing. This does neither, and never deletes a byte it did not write:
/// 1. `text` is written to a hidden temporary file next to the target and flushed. A temporary file left by an
///    earlier, interrupted save is not deleted: it is moved into the replaced folder (it may hold a displaced file).
/// 2. The target is backed up into `.omuzaki/replaced/<path>.<time>-<pid>-<n>`: by hard link, so the target never goes
///    missing and an editor that writes in place afterwards writes into the backup too; if the file system has no
///    links (EPERM, EXDEV or EOPNOTSUPP), by a copy that is flushed and read back to verify its hash. Any other
///    failure is an error.
/// 3. The backup is hashed. If it isn't `expected` the backup is removed and the result is [`Replace::Refused`].
/// 4. On Linux the temporary file and the target are swapped atomically (`renameat2` with `RENAME_EXCHANGE`). The file
///    swapped out (now at the temporary path) must be the backup's: the same inode, or for a copied backup the same
///    hash. If it isn't, an editor replaced or edited the target in the window: the swap is undone, the editor's file
///    is back at the target and the result is [`Replace::Refused`]. Whatever is found at the temporary path while
///    undoing (the editor's file when the undo fails, or our file with edits written into it between the swap and the
///    undo) is moved into the replaced folder as `<path>.<time>-<pid>-<n>.displaced`, flushed, never deleted.
///
/// If the kernel or file system can't exchange names (EINVAL, ENOSYS), and on macOS and Windows, step 4 is a
/// plain `rename` over the target, with a one-time warning on Linux. That leaves a race of microseconds: an editor's
/// atomic save between steps 3 and 4 is overwritten, though the backup still holds what was validated. A new file (no
/// target) is placed with a no-replace `hard_link`, or where links fail a no-replace rename (`RENAME_NOREPLACE` on
/// Linux; an error if that is unsupported), so a file that appeared is never replaced.
///
/// The target exists at every instant of a replacement, so a crash leaves the old file or the new one, plus at worst
/// a backup, the hidden temporary file, or both.
pub(crate) fn replace_checked(
    dir: &Path,
    path: &Path,
    text: &str,
    expected: Option<ContentHash>,
) -> Result<Replace, StoreError> {
    replace_checked_with(dir, path, text, expected, &Hooks::default())
}

/// Points at which tests make an outside change; production code passes the no-op default.
struct Hooks<'a> {
    /// After the temporary file is staged.
    staged: &'a dyn Fn(),
    /// After the backup is verified, before the swap.
    backed_up: &'a dyn Fn(),
    /// After the swap, before it is checked or undone.
    swapped: &'a dyn Fn(),
    /// After the swap is undone, before the file that was exposed at the target is preserved.
    undone: &'a dyn Fn(),
}

fn noop() {}

impl Default for Hooks<'_> {
    fn default() -> Self {
        Self { staged: &noop, backed_up: &noop, swapped: &noop, undone: &noop }
    }
}

fn replace_checked_with(
    dir: &Path,
    path: &Path,
    text: &str,
    expected: Option<ContentHash>,
    hooks: &Hooks<'_>,
) -> Result<Replace, StoreError> {
    refuse_links(dir, path)?;
    let tmp = stage(dir, path, text)?;
    (hooks.staged)();
    let outcome = match back_up(dir, path)? {
        Backup::Absent => {
            (hooks.backed_up)();
            place_no_replace(&tmp, path).map(|placed| if placed { Replace::Placed } else { Replace::Refused })
        }
        Backup::Held(held) => {
            let outcome = replace_held(dir, path, &tmp, &held, expected, hooks);
            if !matches!(outcome, Ok(Replace::Placed)) {
                // A refusal or an error: the target is as it was (or the editor's), so the backup adds nothing.
                // Deleting it is safe because it is a copy of, or a second name for, a file still at the target.
                drop(fs::remove_file(&held.path));
            }
            outcome
        }
    };
    // Only a refusal leaves our own, unexposed temporary file behind. After an error it may hold a displaced file, so
    // it stays until the next save moves it into the replaced folder.
    if matches!(outcome, Ok(Replace::Refused)) {
        drop(fs::remove_file(&tmp));
    }
    outcome
}

/// Steps 3 and 4 for a target that has a backup.
fn replace_held(
    dir: &Path,
    path: &Path,
    tmp: &Path,
    held: &Held,
    expected: Option<ContentHash>,
    hooks: &Hooks<'_>,
) -> Result<Replace, StoreError> {
    if Some(held.found) != expected {
        return Ok(Replace::Refused);
    }
    (hooks.backed_up)();
    if !exchange(tmp, path).map_err(|e| StoreError::io(path, e))? {
        fs::rename(tmp, path).map_err(|e| StoreError::io(path, e))?;
        return Ok(Replace::Placed);
    }
    (hooks.swapped)();
    // The file we displaced is at `tmp`. A hard-linked backup is the same file, so it keeps every byte and the name
    // can go. A copy does not follow an editor that still holds the original open, so that file is kept as well.
    if held.is_file_at(tmp) {
        if held.linked {
            drop(fs::remove_file(tmp));
        } else {
            // Best effort: a failure leaves it at `tmp`, where the next save moves it, and the new text is placed.
            drop(preserve(dir, tmp));
        }
        return Ok(Replace::Placed);
    }
    // Someone replaced or edited the target: put their file back.
    if let Err(e) = exchange(tmp, path) {
        preserve(dir, tmp)?;
        return Err(StoreError::io(path, e));
    }
    (hooks.undone)();
    // Our file was visible at the target between the swap and the undo, so an editor may have it open and write into
    // it later, through a handle no hash check can see. It is therefore always kept, even if it still reads as ours.
    preserve(dir, tmp)?;
    Ok(Replace::Refused)
}

/// A backup of the file at the target.
struct Held {
    /// Where the backup is.
    path: PathBuf,
    /// The hash of its bytes when it was made.
    found: ContentHash,
    /// True if it is a hard link (the same inode as the target was); false for a verified copy.
    linked: bool,
}

impl Held {
    /// True if the file at `other` is the backed-up one: its inode for a link, its hash for a copy.
    fn is_file_at(&self, other: &Path) -> bool {
        if self.linked {
            same_inode(&self.path, other)
        } else {
            fs::read(other).is_ok_and(|bytes| ContentHash::of(&bytes) == self.found)
        }
    }
}

#[cfg(unix)]
fn same_inode(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let id = |p: &Path| fs::symlink_metadata(p).map(|m| (m.dev(), m.ino()));
    matches!((id(a), id(b)), (Ok(x), Ok(y)) if x == y)
}

/// Hard links exist off Unix too, but the swap that needs this check is Linux-only.
#[cfg(not(unix))]
fn same_inode(_: &Path, _: &Path) -> bool {
    false
}

/// Swaps the names `a` and `b` atomically. False if this system can't (nothing was changed).
#[cfg(target_os = "linux")]
fn exchange(a: &Path, b: &Path) -> io::Result<bool> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;

    match renameat_with(CWD, a, CWD, b, RenameFlags::EXCHANGE) {
        Ok(()) => Ok(true),
        Err(Errno::INVAL | Errno::NOSYS) => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                eprintln!(
                    "omuzaki-doc: this file system can't swap files atomically; saves fall back to a plain rename, \
                     which can overwrite an editor's save made in the same instant (#789)"
                );
            });
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

/// No atomic swap here yet.
#[cfg(not(target_os = "linux"))]
fn exchange(_: &Path, _: &Path) -> io::Result<bool> {
    Ok(false)
}

/// Writes `text` to the hidden temporary file beside `path` and flushes it; returns the temporary file's path.
fn stage(dir: &Path, path: &Path, text: &str) -> Result<PathBuf, StoreError> {
    let io_err = |e| StoreError::io(path, e);
    let Some(parent) = path.parent() else { return Err(io_err(io::ErrorKind::InvalidInput.into())) };
    fs::create_dir_all(parent).map_err(io_err)?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(path.file_name().unwrap_or_default());
    tmp_name.push(".tmp");
    let tmp = parent.join(tmp_name);
    // A leftover may be the file an interrupted save displaced, so it is kept, not deleted. `create_new` never
    // follows a link, so what is there (or a link put there) must be out of the way first.
    if fs::symlink_metadata(&tmp).is_ok() {
        preserve(dir, &tmp)?;
    }
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()));
    written.map_err(|e| {
        // Best effort: the original error is the one worth reporting. The file is ours and incomplete.
        drop(fs::remove_file(&tmp));
        io_err(e)
    })?;
    Ok(tmp)
}

/// A fresh name for `path` in the replaced folder: `<path relative to dir>.<time>-<pid>-<n><suffix>`.
fn replaced_name(dir: &Path, path: &Path, suffix: &str) -> Result<PathBuf, StoreError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let below = path
        .strip_prefix(dir)
        .map_err(|_| StoreError::Internal(format!("{} isn't inside {}", path.display(), dir.display())))?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut name = dir.join(REPLACED_DIR).join(below).into_os_string();
    name.push(format!(".{nanos}-{}-{}{suffix}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let name = PathBuf::from(name);
    refuse_links(dir, &name)?;
    Ok(name)
}

/// Moves `from` (a displaced or leftover file) into the replaced folder as `<name>.displaced` and flushes it and the
/// folder. Never deletes.
fn preserve(dir: &Path, from: &Path) -> Result<PathBuf, StoreError> {
    let to = replaced_name(dir, from, ".displaced")?;
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
    }
    if fs::symlink_metadata(from).is_ok_and(|m| m.is_file()) {
        File::open(from).and_then(|f| f.sync_all()).map_err(|e| StoreError::io(from, e))?;
    }
    fs::rename(from, &to).map_err(|e| StoreError::io(from, e))?;
    sync_folder(to.parent())?;
    Ok(to)
}

/// Flushes a folder so a rename into it survives a crash (Unix; other systems have no way to open one).
fn sync_folder(folder: Option<&Path>) -> Result<(), StoreError> {
    #[cfg(unix)]
    if let Some(folder) = folder {
        File::open(folder).and_then(|f| f.sync_all()).map_err(|e| StoreError::io(folder, e))?;
    }
    #[cfg(not(unix))]
    let _ = folder;
    Ok(())
}

/// What [`back_up`] found.
enum Backup {
    /// There is no file at the path.
    Absent,
    /// The file was backed up.
    Held(Held),
}

/// Backs up the file at `path` into the replaced folder: a hard link, or a verified copy where links aren't allowed.
fn back_up(dir: &Path, path: &Path) -> Result<Backup, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Backup::Absent),
        Err(e) => return Err(StoreError::io(path, e)),
    }
    let backup = replaced_name(dir, path, "")?;
    // The folder usually exists already, so link first and make folders only when that says they are missing.
    let mut linked = fs::hard_link(path, &backup);
    if linked.as_ref().is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
        && let Some(parent) = backup.parent()
        && path.exists()
    {
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
        linked = fs::hard_link(path, &backup);
    }
    match linked {
        Ok(()) => {
            let found =
                fs::read(&backup).map(|bytes| ContentHash::of(&bytes)).map_err(|e| StoreError::io(&backup, e))?;
            Ok(Backup::Held(Held { path: backup, found, linked: true }))
        }
        // Deleted between the check and the link: nothing to protect.
        Err(e) if e.kind() == io::ErrorKind::NotFound && !path.exists() => Ok(Backup::Absent),
        // The file system has no links (or won't make this one): copy instead.
        Err(e) if links_unsupported(&e) => copy_backup(path, backup),
        Err(e) => Err(StoreError::io(path, e)),
    }
}

/// True for the errors file systems without hard links give (EPERM, EXDEV, EOPNOTSUPP).
fn links_unsupported(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::CrossesDevices | io::ErrorKind::Unsupported)
}

/// Copies the file at `path` to `backup`, flushes it and reads it back to check the hash.
fn copy_backup(path: &Path, backup: PathBuf) -> Result<Backup, StoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Backup::Absent),
        Err(e) => return Err(StoreError::io(path, e)),
    };
    let found = ContentHash::of(&bytes);
    if let Some(parent) = backup.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
        .and_then(|mut f| f.write_all(&bytes).and_then(|()| f.sync_all()))
        .map_err(|e| StoreError::io(&backup, e))?;
    let copied = fs::read(&backup).map_err(|e| StoreError::io(&backup, e))?;
    if ContentHash::of(&copied) != found {
        // Our own bad copy: nothing of the user's is in it.
        drop(fs::remove_file(&backup));
        return Err(StoreError::Internal(format!("the backup of {} didn't read back identical", path.display())));
    }
    sync_folder(backup.parent())?;
    Ok(Backup::Held(Held { path: backup, found, linked: false }))
}

/// Gives the file `from` the name `to` unless `to` exists; true if it was placed. `from`'s old name is removed.
fn place_no_replace(from: &Path, to: &Path) -> Result<bool, StoreError> {
    place_no_replace_with(from, to, &|a, b| fs::hard_link(a, b))
}

/// [`place_no_replace`] with the hard link injectable, so a test can race the fallback.
fn place_no_replace_with(
    from: &Path,
    to: &Path,
    link: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Result<bool, StoreError> {
    let placed = match link(from, to) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        // No hard links here (FAT, exFAT) or not allowed: an atomic rename that won't replace.
        Err(e) if links_unsupported(&e) => rename_no_replace(from, to)?,
        Err(e) => return Err(StoreError::io(to, e)),
    };
    if placed {
        // After a hard link this drops the second name of the same file; after the rename fallback it is already gone.
        drop(fs::remove_file(from));
    }
    Ok(placed)
}

/// Renames `from` to `to` only if `to` doesn't exist, atomically (`renameat2` with `RENAME_NOREPLACE`); false if it
/// does. A Linux file system that can't do this is an error: a file that appeared must never be replaced unseen.
#[cfg(target_os = "linux")]
fn rename_no_replace(from: &Path, to: &Path) -> Result<bool, StoreError> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;

    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(true),
        Err(Errno::EXIST) => Ok(false),
        Err(e @ (Errno::INVAL | Errno::NOSYS)) => Err(StoreError::io(
            to,
            io::Error::new(io::ErrorKind::Unsupported, format!("this file system can't place a new file safely: {e}")),
        )),
        Err(e) => Err(StoreError::io(to, e.into())),
    }
}

/// Check, then rename; the race of microseconds this leaves is.
#[cfg(not(target_os = "linux"))]
fn rename_no_replace(from: &Path, to: &Path) -> Result<bool, StoreError> {
    if fs::symlink_metadata(to).is_ok() {
        return Ok(false);
    }
    fs::rename(from, to).map(|()| true).map_err(|e| StoreError::io(to, e))
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
        let mut files: Vec<String> = loaded.files.iter().map(|f| f.segs.join("/")).collect();
        files.sort();
        assert_eq!(files, ["characters/mia/face", "characters/mia/variants/game", "project", "scenes/empty"]);
        assert!(loaded.files.iter().any(|f| f.segs == ["scenes", "empty"] && !f.sticky));
        let face = loaded.files.iter().find(|f| f.segs == ["characters", "mia", "face"]).unwrap();
        assert_eq!(face.text, "profile = \"round\"\n");
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
            let err = replace_checked(dir, &dir.join(target), "b = 2\n", None).unwrap_err();
            assert!(matches!(err, StoreError::Symlink { .. }), "{target}: {err}");
            assert!(err.to_string().contains("symbolic link"), "{err}");
        }
        let err = remove_file(dir, &["linked".into(), "kept".into()]).unwrap_err();
        assert!(matches!(err, StoreError::Symlink { .. }), "{err}");
        assert_eq!(fs::read_to_string(outside.path().join("kept.toml")).unwrap(), "a = 1\n");
        assert_eq!(all_names(outside.path()), ["kept.toml"]);

        // A link sitting where the temporary file goes is replaced, not written through.
        replace_checked(dir, &dir.join("real/x.toml"), "c = 3\n", Some(ContentHash::of(b""))).unwrap();
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

    fn kept(dir: &Path) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let root = dir.join(REPLACED_DIR);
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).into_iter().flatten().map(Result::unwrap) {
                if e.path().is_dir() {
                    stack.push(e.path());
                } else {
                    let rel = e.path().strip_prefix(&root).unwrap().to_string_lossy().into_owned();
                    out.push((rel, fs::read_to_string(e.path()).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    fn texts(items: &[(String, String)]) -> Vec<&str> {
        items.iter().map(|(_, t)| t.as_str()).collect()
    }

    #[test]
    fn replace_checked_replaces_keeps_the_old_file_and_leaves_no_temporary_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a/b.toml");
        assert_eq!(replace_checked(tmp.path(), &path, "x = 1\n", None).unwrap(), Replace::Placed);
        let first = Some(ContentHash::of(b"x = 1\n"));
        assert_eq!(replace_checked(tmp.path(), &path, "x = 2\n", first).unwrap(), Replace::Placed);
        assert_eq!(fs::read_to_string(&path).unwrap(), "x = 2\n");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["b.toml"]);
        let old = kept(tmp.path());
        assert_eq!(texts(&old), ["x = 1\n"]);
        assert!(old[0].0.starts_with("a/b.toml."), "{old:?}");
    }

    const MINE: &str = "mine = 1\n";

    fn with_file(dir: &Path) -> (PathBuf, Option<ContentHash>) {
        let path = dir.join("f.toml");
        fs::write(&path, MINE).unwrap();
        (path, Some(ContentHash::of(MINE.as_bytes())))
    }

    fn no_temporary_file(dir: &Path) -> bool {
        fs::read_dir(dir).unwrap().all(|e| e.unwrap().file_name() != ".f.toml.tmp")
    }

    fn replace_with(dir: &Path, expected: Option<ContentHash>, hooks: &Hooks<'_>) -> Result<Replace, StoreError> {
        replace_checked_with(dir, &dir.join("f.toml"), "ours = 3\n", expected, hooks)
    }

    /// What an editor's atomic save does: a new file renamed over the target.
    fn atomic_save(dir: &Path, text: &str) {
        fs::write(dir.join("editor.tmp"), text).unwrap();
        fs::rename(dir.join("editor.tmp"), dir.join("f.toml")).unwrap();
    }

    #[test]
    fn an_in_place_edit_after_staging_is_refused_and_untouched() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let edit = || fs::write(dir.join("f.toml"), "theirs = 2\n").unwrap();
        let out = replace_with(dir, expected, &Hooks { staged: &edit, ..Hooks::default() }).unwrap();
        assert_eq!(out, Replace::Refused);
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 2\n");
        assert_eq!(kept(dir), []);
        assert!(no_temporary_file(dir));
    }

    #[test]
    fn an_in_place_edit_after_the_backup_lands_in_the_backup_too() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let edit = || fs::write(dir.join("f.toml"), "theirs = 2\n").unwrap();
        let out = replace_with(dir, expected, &Hooks { backed_up: &edit, ..Hooks::default() }).unwrap();
        // The swap still succeeds (the inode is unchanged) and the edit is not lost: the backup holds it.
        assert_eq!(out, Replace::Placed);
        assert_eq!(fs::read_to_string(&path).unwrap(), "ours = 3\n");
        assert_eq!(texts(&kept(dir)), ["theirs = 2\n"]);
        assert!(no_temporary_file(dir));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_atomic_save_before_the_swap_is_swapped_back_and_both_files_survive() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let save = || atomic_save(dir, "theirs = 2\n");
        let out = replace_with(dir, expected, &Hooks { backed_up: &save, ..Hooks::default() }).unwrap();
        assert_eq!(out, Replace::Refused);
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 2\n");
        // Our own file was exposed at the target for an instant, so it is kept rather than deleted.
        let items = kept(dir);
        assert_eq!(texts(&items), ["ours = 3\n"]);
        assert!(items[0].0.ends_with(".displaced"), "{items:?}");
        assert!(no_temporary_file(dir));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_edit_between_the_swap_and_the_undo_is_kept() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let save = || atomic_save(dir, "theirs = 2\n");
        // After the swap the target is our file; an editor with it open writes into it before we undo.
        let late = || fs::write(dir.join("f.toml"), "late edit = 4\n").unwrap();
        let hooks = Hooks { backed_up: &save, swapped: &late, ..Hooks::default() };
        assert_eq!(replace_with(dir, expected, &hooks).unwrap(), Replace::Refused);
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 2\n");
        let items = kept(dir);
        assert_eq!(texts(&items), ["late edit = 4\n"]);
        assert!(items[0].0.ends_with(".displaced"), "{items:?}");
        assert!(no_temporary_file(dir));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_error_while_undoing_the_swap_keeps_the_displaced_file() {
        let tmp = TempDir::new().unwrap();
        let (dir, expected) = (tmp.path(), with_file(tmp.path()).1);
        let save = || atomic_save(dir, "theirs = 2\n");
        // The undo is an exchange, which fails if the target has vanished.
        let vanish = || fs::remove_file(dir.join("f.toml")).unwrap();
        let hooks = Hooks { backed_up: &save, swapped: &vanish, ..Hooks::default() };
        let err = replace_with(dir, expected, &hooks).unwrap_err();
        assert!(matches!(err, StoreError::Io { .. }), "{err}");
        let items = kept(dir);
        assert_eq!(texts(&items), ["theirs = 2\n"]);
        assert!(items[0].0.ends_with(".displaced"), "{items:?}");
    }

    #[test]
    fn a_temporary_file_left_by_a_crash_is_moved_not_deleted_by_the_next_save() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        fs::write(dir.join(".f.toml.tmp"), "displaced by a crash = 5\n").unwrap();
        assert_eq!(replace_checked(dir, &path, "ours = 3\n", expected).unwrap(), Replace::Placed);
        assert_eq!(fs::read_to_string(&path).unwrap(), "ours = 3\n");
        let items = kept(dir);
        let mut all = texts(&items);
        all.sort_unstable();
        assert_eq!(all, ["displaced by a crash = 5\n", MINE]);
        assert!(no_temporary_file(dir));
    }

    #[test]
    fn a_copied_backup_is_flushed_and_verified() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, _)) = (tmp.path(), with_file(tmp.path()));
        let backup = dir.join(REPLACED_DIR).join("f.toml.copy");
        let Backup::Held(held) = copy_backup(&path, backup.clone()).unwrap() else { panic!("no backup") };
        assert!(!held.linked && held.found == ContentHash::of(MINE.as_bytes()));
        assert_eq!(fs::read_to_string(&backup).unwrap(), MINE);
        assert!(held.is_file_at(&path));
        fs::write(&path, "changed = 1\n").unwrap();
        assert!(!held.is_file_at(&path));
    }

    #[test]
    fn only_link_failures_are_treated_as_no_links() {
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::CrossesDevices, io::ErrorKind::Unsupported] {
            assert!(links_unsupported(&kind.into()), "{kind:?}");
        }
        for kind in [io::ErrorKind::OutOfMemory, io::ErrorKind::StorageFull, io::ErrorKind::Other] {
            assert!(!links_unsupported(&kind.into()), "{kind:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn our_file_exposed_by_a_lost_race_is_kept_even_if_an_editor_writes_into_it_after_the_undo() {
        use std::cell::RefCell;
        use std::io::Write as _;

        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let save = || atomic_save(dir, "theirs = 2\n");
        // After the swap the target is our file; an editor opens it and keeps the handle.
        let handle = RefCell::new(None);
        let open = || *handle.borrow_mut() = Some(OpenOptions::new().write(true).open(dir.join("f.toml")).unwrap());
        // After the undo it writes through that handle, past any hash check.
        let write = || {
            let mut file = handle.borrow_mut().take().unwrap();
            file.write_all(b"late = 9\n").unwrap();
            file.sync_all().unwrap();
        };
        let hooks = Hooks { backed_up: &save, swapped: &open, undone: &write, ..Hooks::default() };
        assert_eq!(replace_with(dir, expected, &hooks).unwrap(), Replace::Refused);
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 2\n");
        let items = kept(dir);
        assert_eq!(items.len(), 1, "{items:?}");
        assert!(items[0].0.ends_with(".displaced"), "{items:?}");
        assert!(texts(&items)[0].ends_with("late = 9\n"), "{items:?}");
        assert!(no_temporary_file(dir));
    }

    #[test]
    fn a_file_created_while_links_fail_is_never_replaced() {
        let tmp = TempDir::new().unwrap();
        let (dir, path) = (tmp.path(), tmp.path().join("f.toml"));
        let from = dir.join(".f.toml.tmp");
        fs::write(&from, "ours = 3\n").unwrap();
        // The link fails, and an editor creates the target in the instant before the fallback runs.
        let racing_link = |_: &Path, to: &Path| {
            fs::write(to, "theirs = 1\n").unwrap();
            Err(io::ErrorKind::PermissionDenied.into())
        };
        assert!(!place_no_replace_with(&from, &path, &racing_link).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 1\n");
    }

    #[test]
    fn a_new_file_is_placed_by_rename_where_links_fail_and_other_link_errors_propagate() {
        let tmp = TempDir::new().unwrap();
        let (dir, path) = (tmp.path(), tmp.path().join("f.toml"));
        let from = dir.join(".f.toml.tmp");
        fs::write(&from, "ours = 3\n").unwrap();
        let no_links = |_: &Path, _: &Path| Err(io::ErrorKind::PermissionDenied.into());
        assert!(place_no_replace_with(&from, &path, &no_links).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "ours = 3\n");
        assert!(!from.exists());

        let other = dir.join("g.toml");
        fs::write(dir.join(".g.toml.tmp"), "x = 1\n").unwrap();
        let broken = |_: &Path, _: &Path| Err(io::ErrorKind::StorageFull.into());
        let err = place_no_replace_with(&dir.join(".g.toml.tmp"), &other, &broken).unwrap_err();
        assert!(matches!(err, StoreError::Io { .. }), "{err}");
        assert!(!other.exists());
    }

    #[test]
    fn a_new_file_is_refused_if_one_appeared() {
        let tmp = TempDir::new().unwrap();
        let (dir, path) = (tmp.path(), tmp.path().join("f.toml"));
        let create = || fs::write(dir.join("f.toml"), "theirs = 1\n").unwrap();
        assert_eq!(replace_with(dir, None, &Hooks { staged: &create, ..Hooks::default() }).unwrap(), Replace::Refused);
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 1\n");
        assert!(no_temporary_file(dir));
    }

    #[test]
    fn an_unexpected_file_where_none_is_expected_is_refused() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, _)) = (tmp.path(), with_file(tmp.path()));
        assert_eq!(replace_checked(dir, &path, "ours = 3\n", None).unwrap(), Replace::Refused);
        assert_eq!(fs::read_to_string(&path).unwrap(), MINE);
        assert_eq!(kept(dir), []);
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
