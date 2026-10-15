//! The project folder: reading it into a tree, and writing, replacing and deleting its files.
//!
//! # What each change needs, and what happens without it
//!
//! A change to a project file is made atomically or refused before the file is touched; there is no non-atomic
//! fallback. A refusal is [`StoreError::NoSafeChange`] naming what is missing, and the save reports it. Three
//! capabilities decide which; each depends on the system and on the file system:
//!
//! - **Exchange**: swap two names atomically (Linux `renameat2` with `RENAME_EXCHANGE`, macOS `renameatx_np` with
//!   `RENAME_SWAP`).
//! - **No-replace rename**: rename only if the new name is free, atomically (Linux `renameat2` with
//!   `RENAME_NOREPLACE`, macOS `renameatx_np` with `RENAME_EXCL`).
//! - **Hard links**: `link` fails if the new name exists, so it is also a placement that never replaces.
//!
//! | Exchange | No-replace rename | Hard links | Replace a file ([`replace_checked`]) | Write a new file | Delete ([`remove_checked`]) |
//! |---|---|---|---|---|---|
//! | yes | either | yes | linked backup, swap, check, undo on a race | no-replace link | linked backup; move aside; delete, or put back by link |
//! | yes | yes | no | copied backup, swap, check, undo on a race; the original then replaces the copy | no-replace rename | copied backup; move aside by no-replace rename; original over the copy, or put back by no-replace rename |
//! | yes | no | no | as above | refused | refused |
//! | no | either | yes | refused | no-replace link | as in the first row |
//! | no | yes | no | refused | no-replace rename | as in the second row |
//! | no | no | no | refused | refused | refused |
//!
//! Where each platform stands with what this crate's dependencies reach today:
//!
//! | Platform | Exchange | No-replace rename | Hard links |
//! |---|---|---|---|
//! | Linux | yes where the file system supports the flag (ext4, XFS, Btrfs, tmpfs); otherwise the kernel says `EINVAL` and the change is refused | the same | yes, except FAT and exFAT |
//! | macOS | yes on APFS and HFS+ (macOS 10.12 or later); a file system without it says `ENOTSUP` and the change is refused | the same | yes on APFS and HFS+ |
//! | Windows | no (Windows has no atomic exchange): replacing a file is refused | no (`MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` is not reached yet) | yes on NTFS, no on FAT and exFAT |
//!
//! Each cell of the first table is tested with the capabilities forced off through the test seam (`Hooks`).

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// The folder (inside the project) that keeps every file a save displaced; see `docs/design/journal.md` section 6.1.
///
/// - `<path>.<time>-<pid>-<n>.<hash>.kept`: a file a save replaced as expected, kept as the same file (inode), with
///   the hash its bytes had when it was replaced. An editor that still holds it open writes into this entry, not
///   into nothing. [`prune_replaced`] deletes it once it is older than [`RETAIN_UNCHANGED`] and still has that hash.
/// - `<path>.<time>-<pid>-<n>.displaced`: a file that raced a save (an editor's save or write) or that an interrupted
///   save left behind. No hash is recorded, so nothing ever deletes it automatically.
const REPLACED_DIR: &str = ".omuzaki/replaced";

/// The suffix of an entry of the replaced folder that [`prune_replaced`] may delete.
const KEPT: &str = ".kept";

/// How long a replaced file is kept even if nothing wrote to it: an editor that still holds the old file open and
/// writes within this time finds its bytes in the replaced folder.
pub(crate) const RETAIN_UNCHANGED: Duration = Duration::from_mins(10);

/// The most entries one [`prune_replaced`] call deletes, so a save never stalls on a large backlog.
pub(crate) const PRUNE_MAX: usize = 64;

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
/// 2. The target is read and hashed. If it isn't `expected` the result is [`Replace::Refused`] and nothing else is
///    done.
/// 3. The target is backed up into `.omuzaki/replaced/<path>.<time>-<pid>-<n>.<hash>.kept`: by hard link, so the
///    target never goes missing and an editor that writes in place afterwards writes into the backup too; if the file
///    system has no links (EPERM, EXDEV or EOPNOTSUPP), by a copy that is flushed and read back to verify its hash.
///    Any other failure is an error.
/// 4. The temporary file and the target are swapped atomically (see [`exchange`]). The file swapped out (now at the
///    temporary path) must be the one hashed: the backup's inode with the hashed bytes, or for a copied backup the
///    hashed bytes. If it isn't, an editor replaced or edited the target in the window: the swap is undone, the
///    editor's file is back at the target, the backup is removed and the result is [`Replace::Refused`]. Whatever is
///    found at the temporary path while undoing (the editor's file when the undo fails, or our file with edits written
///    into it between the swap and the undo) is moved into the replaced folder as
///    `<path>.<time>-<pid>-<n>.displaced`, flushed, never deleted.
/// 5. After a placement the displaced file stays in the replaced folder as the `.kept` entry: for a hard-linked backup
///    it already is that file; a copied backup is replaced by the displaced file itself (one rename over our own
///    copy). An editor that still holds the old file open and writes after the check writes into that entry, which
///    pruning then never deletes (its bytes no longer match the hash).
///
/// Where the swap isn't available (see the module's table) there is no safe way to replace a file that exists, so
/// step 4 fails with [`StoreError::NoSafeChange`] before the target is touched: the target is as it was, and the
/// backup and the temporary file (both only ours) are removed. A new file (no target) is placed with a no-replace
/// `hard_link`, or where links fail with a rename that won't replace, so a file that appeared is never replaced;
/// without either it is refused the same way.
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
    /// After the displaced file is verified, before the temporary name is cleaned up.
    verified: &'a dyn Fn(),
    /// After the swap is undone, before the file that was exposed at the target is preserved.
    undone: &'a dyn Fn(),
    /// After a deletion renamed the target aside, before the file is checked.
    quarantined: &'a dyn Fn(),
    /// Pretend the file system has no hard links (tests only).
    links_off: Cell<bool>,
    /// Pretend the system can't swap two names atomically (tests only).
    exchange_off: Cell<bool>,
    /// Pretend the system has no rename that refuses to replace (tests only).
    no_replace_off: Cell<bool>,
}

fn noop() {}

impl Default for Hooks<'_> {
    fn default() -> Self {
        Self {
            staged: &noop,
            backed_up: &noop,
            swapped: &noop,
            verified: &noop,
            undone: &noop,
            quarantined: &noop,
            links_off: Cell::new(false),
            exchange_off: Cell::new(false),
            no_replace_off: Cell::new(false),
        }
    }
}

impl Hooks<'_> {
    /// A hard link, unless a test turned links off.
    fn link(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.links_off.get() { Err(io::ErrorKind::Unsupported.into()) } else { fs::hard_link(from, to) }
    }

    /// [`exchange`], unless a test turned it off.
    fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
        if self.exchange_off.get() { Err(io::ErrorKind::Unsupported.into()) } else { exchange(a, b) }
    }

    /// [`rename_no_replace`], unless a test turned it off.
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<bool> {
        if self.no_replace_off.get() { Err(io::ErrorKind::Unsupported.into()) } else { rename_no_replace(from, to) }
    }
}

/// What a refusal names as missing when there is no atomic swap.
const NO_EXCHANGE: &str = "atomic exchange of two file names (RENAME_EXCHANGE, RENAME_SWAP)";

/// What a refusal names as missing when a file can't be placed without risking replacing another.
const NO_NO_REPLACE: &str = "hard links and no rename that refuses to replace a file (RENAME_NOREPLACE, RENAME_EXCL)";

/// Maps an error of a capability-dependent call: [`io::ErrorKind::Unsupported`] (the capability is missing) becomes
/// [`StoreError::NoSafeChange`] naming `missing`, anything else an I/O error for `path`.
fn capability_error(path: &Path, missing: &'static str, e: io::Error) -> StoreError {
    if e.kind() == io::ErrorKind::Unsupported {
        StoreError::NoSafeChange { path: path.to_path_buf(), missing }
    } else {
        StoreError::io(path, e)
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
    let outcome = match back_up(dir, path, expected, hooks)? {
        Backup::Absent => {
            (hooks.backed_up)();
            place_no_replace(&tmp, path, hooks).map(|placed| if placed { Replace::Placed } else { Replace::Refused })
        }
        Backup::Mismatch => Ok(Replace::Refused),
        Backup::Held(held) => {
            let outcome = replace_held(dir, path, &tmp, &held, hooks);
            // After a placement the backup is the kept entry (step 5). Otherwise the target is as it was (or the
            // editor's) and any displaced file is kept separately by `preserve`, so the backup can go.
            if !matches!(outcome, Ok(Replace::Placed)) {
                drop(fs::remove_file(&held.path));
            }
            outcome
        }
    };
    // A refusal, or a missing capability (refused before anything moved), leaves our own, unexposed temporary file
    // behind. After any other error it may hold a displaced file, so it stays until the next save moves it into the
    // replaced folder.
    if matches!(outcome, Ok(Replace::Refused) | Err(StoreError::NoSafeChange { .. })) {
        drop(fs::remove_file(&tmp));
    }
    outcome
}

/// Steps 4 and 5 for a target that has a backup.
fn replace_held(dir: &Path, path: &Path, tmp: &Path, held: &Held, hooks: &Hooks<'_>) -> Result<Replace, StoreError> {
    (hooks.backed_up)();
    // Without a swap nothing has moved yet, and nothing will: see the module's table.
    hooks.exchange(tmp, path).map_err(|e| capability_error(path, NO_EXCHANGE, e))?;
    (hooks.swapped)();
    // The file we displaced is at `tmp`, and it must still hold the bytes that were hashed. An in-place edit after
    // they were hashed fails the check (even for a link, whose inode is unchanged) and falls through to the undo below.
    if held.is_file_at(tmp) {
        (hooks.verified)();
        if held.linked.is_some() {
            // The backup is the same file, so it is the kept entry and this second name can go.
            drop(fs::remove_file(tmp));
        } else {
            // A copy doesn't follow an editor that still holds the original open, so the original takes the copy's
            // place (a rename over our own copy). Best effort: a failure leaves it at `tmp`, where the next save moves
            // it, and the copy stays.
            if fs::rename(tmp, &held.path).is_ok() {
                drop(sync_folder(held.path.parent()));
            }
        }
        return Ok(Replace::Placed);
    }
    // Someone replaced or edited the target: put their file back.
    if let Err(e) = hooks.exchange(tmp, path) {
        preserve(dir, tmp)?;
        return Err(capability_error(path, NO_EXCHANGE, e));
    }
    (hooks.undone)();
    // Our file was visible at the target between the swap and the undo, so an editor may have it open and write into
    // it later, through a handle no hash check can see. It is therefore always kept, even if it still reads as ours.
    preserve(dir, tmp)?;
    Ok(Replace::Refused)
}

/// A backup of the file at the target.
struct Held {
    /// Where the backup is: the `.kept` entry if the save places its file.
    path: PathBuf,
    /// The hash of the target's bytes when it was backed up (the expected one); the displaced file must still hash to
    /// this.
    found: ContentHash,
    /// For a hard link, the (device, inode) of the backup, which is the target's file; `None` for a verified copy.
    linked: Option<FileId>,
    /// Whether the backup is a hard link (even where [`file_id`] can't confirm it is the same file), which shows that
    /// this file system makes hard links.
    by_link: bool,
}

/// A file's identity on its device.
type FileId = (u64, u64);

impl Held {
    /// True if the file at `other` is the backed-up one with the bytes it had when backed up: for a link the same
    /// inode (an in-place edit keeps the inode, so the bytes are checked too), for a copy the hash alone. The file is
    /// opened once, so the identity and the bytes are those of one file.
    fn is_file_at(&self, other: &Path) -> bool {
        let Ok(mut file) = File::open(other) else { return false };
        if self.linked.is_some() && file_id(&file.metadata()) != self.linked {
            return false;
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).is_ok() && ContentHash::of(&bytes) == self.found
    }
}

/// The identity of a file from its metadata; `None` where this system has no cheap one (then a link is never trusted
/// by identity and counts as a copy).
#[cfg(unix)]
fn file_id(meta: &io::Result<fs::Metadata>) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    meta.as_ref().ok().map(|m| (m.dev(), m.ino()))
}

/// Windows has hard links too, but no swap, so there this check is never needed.
#[cfg(not(unix))]
fn file_id(_: &io::Result<fs::Metadata>) -> Option<FileId> {
    None
}

/// Swaps the names `a` and `b` atomically: `renameat2` with `RENAME_EXCHANGE` on Linux, `renameatx_np` with
/// `RENAME_SWAP` on macOS. [`io::ErrorKind::Unsupported`] if this system or file system can't (then nothing was
/// changed).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    renameat_with(CWD, a, CWD, b, RenameFlags::EXCHANGE).map_err(rename_error)
}

/// The error of a flagged rename: the kernel or file system not knowing the flag (`EINVAL`, `ENOSYS`, `ENOTSUP`,
/// `EOPNOTSUPP`) is [`io::ErrorKind::Unsupported`]; anything else is itself.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_error(e: rustix::io::Errno) -> io::Error {
    use rustix::io::Errno;

    if [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP, Errno::OPNOTSUPP].contains(&e) {
        io::ErrorKind::Unsupported.into()
    } else {
        e.into()
    }
}

/// Windows has no atomic swap of two names, so replacing an existing file is refused there; see the module's table.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exchange(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
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

/// Deletes up to [`PRUNE_MAX`] `.kept` entries of the replaced folder of the project `dir` that are older than
/// [`RETAIN_UNCHANGED`] at `now` and whose bytes still have the hash in their name; returns how many it deleted.
///
/// An entry that was written to after its save (an editor that held the old file open) no longer matches its hash
/// and is never deleted here, nor is a `.displaced` entry or anything else in the folder. Best effort: what can't be
/// read or deleted is left alone, and so is everything if the folder is reached through a symbolic link.
pub(crate) fn prune_replaced(dir: &Path, now: SystemTime) -> usize {
    let root = dir.join(REPLACED_DIR);
    if refuse_links(dir, &root).is_err() {
        return 0;
    }
    let Some(cutoff) = now.checked_sub(RETAIN_UNCHANGED).and_then(|t| t.duration_since(UNIX_EPOCH).ok()) else {
        return 0;
    };
    let mut pruned = 0;
    let mut folders = vec![root];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            if pruned == PRUNE_MAX {
                return pruned;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                folders.push(entry.path());
                continue;
            }
            let name = entry.file_name();
            let Some((saved, hash)) = name.to_str().and_then(kept_stamp) else { continue };
            if kind.is_file()
                && saved < cutoff
                && fs::read(entry.path()).is_ok_and(|bytes| ContentHash::of(&bytes) == hash)
                && fs::remove_file(entry.path()).is_ok()
            {
                pruned += 1;
            }
        }
    }
    pruned
}

/// The time and hash in the name of a `.kept` entry (`<name>.<nanos>-<pid>-<n>.<hash>.kept`), or `None` for any
/// other name.
fn kept_stamp(name: &str) -> Option<(Duration, ContentHash)> {
    let (rest, hash) = name.strip_suffix(KEPT)?.rsplit_once('.')?;
    let (_, stamp) = rest.rsplit_once('.')?;
    let nanos: u64 = stamp.split('-').next()?.parse().ok()?;
    Some((Duration::from_nanos(nanos), hash.parse().ok()?))
}

/// Flushes a folder so a rename into it survives a crash (Unix; other systems have no way to open one).
#[cfg_attr(not(unix), expect(clippy::unnecessary_wraps, reason = "the Unix version can fail"))]
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
    /// The file there isn't the expected one; nothing was backed up.
    Mismatch,
    /// The file was backed up.
    Held(Held),
}

/// Hashes the file at `path` and, if it is `expected`, backs it up into the replaced folder under that hash: a hard
/// link, or a verified copy where links aren't allowed.
///
/// The bytes are read before the link is made, so the backup itself is never read back. An editor's save in between
/// links its file instead; the check after the swap (same inode and the hashed bytes) refuses that, and with no swap
/// the backup keeps the editor's bytes under a hash they don't match, so pruning never deletes them.
fn back_up(dir: &Path, path: &Path, expected: Option<ContentHash>, hooks: &Hooks<'_>) -> Result<Backup, StoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Backup::Absent),
        Err(e) => return Err(StoreError::io(path, e)),
    };
    let found = ContentHash::of(&bytes);
    if Some(found) != expected {
        return Ok(Backup::Mismatch);
    }
    let backup = replaced_name(dir, path, &format!(".{found}{KEPT}"))?;
    // The folder usually exists already, so link first and make folders only when that says they are missing.
    let link = |from: &Path, to: &Path| hooks.link(from, to);
    let mut linked = link(path, &backup);
    if linked.as_ref().is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
        && let Some(parent) = backup.parent()
        && path.exists()
    {
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
        linked = link(path, &backup);
    }
    match linked {
        // Without an identity the link can't be trusted as the same file, so it is checked like a copy.
        Ok(()) => {
            let linked = file_id(&fs::symlink_metadata(&backup));
            Ok(Backup::Held(Held { path: backup, found, linked, by_link: true }))
        }
        // Deleted between the read and the link: nothing to protect.
        Err(e) if e.kind() == io::ErrorKind::NotFound && !path.exists() => Ok(Backup::Absent),
        // The file system has no links (or won't make this one): copy instead.
        Err(e) if links_unsupported(&e) => copy_backup(path, &bytes, backup),
        Err(e) => Err(StoreError::io(path, e)),
    }
}

/// True for the errors file systems without hard links give (EPERM, EXDEV, EOPNOTSUPP).
fn links_unsupported(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::CrossesDevices | io::ErrorKind::Unsupported)
}

/// Writes `bytes` (read from `path`) to `backup`, flushes it and reads it back to check the hash.
///
/// The folder is not synced here: until the swap the target still holds these bytes, and after a placement the
/// displaced file takes this name and the folder is synced once then.
fn copy_backup(path: &Path, bytes: &[u8], backup: PathBuf) -> Result<Backup, StoreError> {
    let found = ContentHash::of(bytes);
    if let Some(parent) = backup.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .map_err(|e| StoreError::io(&backup, e))?;
    let copied = fs::read(&backup).map_err(|e| StoreError::io(&backup, e))?;
    if ContentHash::of(&copied) != found {
        // Our own bad copy: nothing of the user's is in it.
        drop(fs::remove_file(&backup));
        return Err(StoreError::Internal(format!("the backup of {} didn't read back identical", path.display())));
    }
    Ok(Backup::Held(Held { path: backup, found, linked: None, by_link: false }))
}

/// Gives the file `from` the name `to` unless `to` exists; true if it was placed. `from`'s old name is removed.
///
/// A no-replace hard link, or where the file system has no links an atomic rename that won't replace. Without either
/// it is [`StoreError::NoSafeChange`] and nothing was moved: checking for `to` and then renaming would replace a file
/// created in between.
fn place_no_replace(from: &Path, to: &Path, hooks: &Hooks<'_>) -> Result<bool, StoreError> {
    let placed = match hooks.link(from, to) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        // No hard links here (FAT, exFAT) or not allowed: an atomic rename that won't replace.
        Err(e) if links_unsupported(&e) => {
            hooks.rename_no_replace(from, to).map_err(|e| capability_error(to, NO_NO_REPLACE, e))?
        }
        Err(e) => return Err(StoreError::io(to, e)),
    };
    if placed {
        // After a hard link this drops the second name of the same file; after the rename it is already gone.
        drop(fs::remove_file(from));
    }
    Ok(placed)
}

/// Renames `from` to `to` only if `to` doesn't exist, atomically (`renameat2` with `RENAME_NOREPLACE` on Linux,
/// `renameatx_np` with `RENAME_EXCL` on macOS); false if it does. [`io::ErrorKind::Unsupported`] if the system or
/// file system can't (then nothing was changed).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<bool> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;

    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(true),
        Err(Errno::EXIST) => Ok(false),
        Err(e) => Err(rename_error(e)),
    }
}

/// No rename that refuses to replace is reachable on Windows yet (`std::fs::rename` overwrites, and `MoveFileExW` is
/// not in this crate's dependencies), so this is always unsupported; it is reached only on a file system without hard
/// links (FAT, exFAT).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_no_replace(_: &Path, _: &Path) -> io::Result<bool> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Deletes the file for the document path `segs` if its bytes are `expected`, then any folders it leaves empty (never
/// `dir` itself); false if it holds anything else, in which case nothing is lost. A file that is already gone is fine.
///
/// Like a replacement, a deletion keeps what it removes: the file is backed up first into the replaced folder (see
/// [`replace_checked`], step 3), so an editor that still holds it open can't lose what it writes there, and pruning
/// deletes the backup later only while it is unchanged.
///
/// The name is never unlinked directly, because an editor's atomic save may land between the backup and the unlink
/// and the unlink would delete its new file. Instead the target is renamed to a unique hidden name beside it, which
/// is atomic and takes whatever file is there at that instant, and that file is checked against the backup (same
/// inode and the hashed bytes, see [`Held::is_file_at`]). If it is the backed-up file it is deleted (or, for a copied
/// backup, renamed over the copy). If not, it is the editor's: it is put back with a placement that won't replace, or,
/// if the name has been taken again, moved into the replaced folder as `.displaced`, and the result is false (a
/// conflict) either way. If putting it back fails, the error is returned and the file stays beside the target under
/// the hidden name the error gives.
///
/// Putting a file back needs a hard link or a rename that won't replace. A hard-linked backup shows the file system
/// has links; with only a copy, the move aside is itself made with the rename that won't replace, so a system without
/// either refuses with [`StoreError::NoSafeChange`] before the target is touched.
pub(crate) fn remove_checked(dir: &Path, segs: &[String], expected: Option<ContentHash>) -> Result<bool, StoreError> {
    remove_checked_with(dir, segs, expected, &Hooks::default())
}

fn remove_checked_with(
    dir: &Path,
    segs: &[String],
    expected: Option<ContentHash>,
    hooks: &Hooks<'_>,
) -> Result<bool, StoreError> {
    let path = file_path(dir, segs);
    refuse_links(dir, &path)?;
    match back_up(dir, &path, expected, hooks)? {
        Backup::Mismatch => return Ok(false),
        Backup::Absent => {}
        Backup::Held(held) => {
            (hooks.backed_up)();
            if !unlink_held(dir, &path, &held, hooks)? {
                return Ok(false);
            }
            if held.linked.is_none() {
                sync_folder(held.path.parent())?;
            }
        }
    }
    let mut folder = path.parent();
    while let Some(f) = folder.filter(|f| *f != dir) {
        // `remove_dir` refuses a folder that still holds anything, which is exactly when to stop.
        if fs::remove_dir(f).is_err() {
            break;
        }
        folder = f.parent();
    }
    Ok(true)
}

/// Removes the file at `path` if it is the backed-up one; false if another file was there, which is kept.
fn unlink_held(dir: &Path, path: &Path, held: &Held, hooks: &Hooks<'_>) -> Result<bool, StoreError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".{}-{}.del", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let quarantine = path.with_file_name(name);
    // With links an editor's file can be put back by a no-replace link. Without them it can only be put back by a
    // rename that won't replace, so the move aside uses that rename too: where it is missing, nothing moves.
    let moved = if held.by_link {
        fs::rename(path, &quarantine).map(|()| true)
    } else {
        hooks.rename_no_replace(path, &quarantine)
    };
    let moved = moved.and_then(|placed| if placed { Ok(()) } else { Err(io::ErrorKind::AlreadyExists.into()) });
    match moved {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => {
            drop(fs::remove_file(&held.path));
            return Err(capability_error(path, NO_NO_REPLACE, e));
        }
    }
    (hooks.quarantined)();
    if held.is_file_at(&quarantine) {
        if held.linked.is_some() {
            return fs::remove_file(&quarantine).map(|()| true).map_err(|e| StoreError::io(&quarantine, e));
        }
        // The backup is only a copy, so the original inode (which an editor may still hold open) is renamed over it
        // and kept, not unlinked.
        return fs::rename(&quarantine, &held.path).map(|()| true).map_err(|e| StoreError::io(&quarantine, e));
    }
    // An editor's file: put it back unless something took the name again, and never delete it.
    match place_no_replace(&quarantine, path, hooks) {
        Ok(true) => Ok(false),
        // The name was taken again: that file stays at the target and this one is kept in the replaced folder.
        Ok(false) => {
            preserve(dir, &quarantine)?;
            Ok(false)
        }
        // It stays beside the target, where the error says, not out of sight in the replaced folder.
        Err(e) => Err(StoreError::io(
            &quarantine,
            io::Error::other(format!(
                "the file that took the place of {} was kept here because it couldn't be put back: {e}",
                path.display()
            )),
        )),
    }
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
        let err = remove_checked(dir, &["linked".into(), "kept".into()], None).unwrap_err();
        assert!(matches!(err, StoreError::Symlink { .. }), "{err}");
        assert_eq!(fs::read_to_string(outside.path().join("kept.toml")).unwrap(), "a = 1\n");
        assert_eq!(all_names(outside.path()), ["kept.toml"]);

        // A link sitting where the temporary file goes is replaced, not written through.
        replace_checked(dir, &dir.join("real/x.toml"), "c = 3\n", Some(ContentHash::of(b""))).unwrap();
        assert_eq!(fs::read_to_string(dir.join("real/x.toml")).unwrap(), "c = 3\n");
        assert_eq!(fs::read_to_string(outside.path().join("kept.toml")).unwrap(), "a = 1\n");
    }

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
    fn replace_checked_replaces_keeps_the_old_file_under_its_hash_and_leaves_no_temporary_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a/b.toml");
        assert_eq!(replace_checked(tmp.path(), &path, "x = 1\n", None).unwrap(), Replace::Placed);
        assert_eq!(kept(tmp.path()), [], "a new file displaces nothing");
        let first = ContentHash::of(b"x = 1\n");
        assert_eq!(replace_checked(tmp.path(), &path, "x = 2\n", Some(first)).unwrap(), Replace::Placed);
        assert_eq!(fs::read_to_string(&path).unwrap(), "x = 2\n");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["b.toml"]);
        let items = kept(tmp.path());
        assert_eq!(texts(&items), ["x = 1\n"]);
        let name = items[0].0.rsplit('/').next().unwrap();
        assert!(name.starts_with("b.toml.") && name.ends_with(&format!(".{first}.kept")), "{name}");
        assert!(kept_stamp(name).is_some_and(|(_, hash)| hash == first));
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn an_in_place_edit_after_the_backup_is_refused_and_both_versions_survive() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let edit = || fs::write(dir.join("f.toml"), "theirs = 2\n").unwrap();
        let out = replace_with(dir, expected, &Hooks { backed_up: &edit, ..Hooks::default() }).unwrap();
        // The inode is unchanged but the displaced bytes are not the hashed ones: the swap is undone and refused.
        assert_eq!(out, Replace::Refused);
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 2\n");
        // Our own file was exposed at the target for an instant, so it is kept.
        let items = kept(dir);
        assert_eq!(texts(&items), ["ours = 3\n"]);
        assert!(items[0].0.ends_with(".displaced"), "{items:?}");
        assert!(no_temporary_file(dir));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        let displaced: Vec<_> = items.iter().filter(|(n, _)| n.ends_with(".displaced")).map(|(_, t)| t).collect();
        assert_eq!(displaced, ["displaced by a crash = 5\n"]);
        let replaced: Vec<_> = items.iter().filter(|(n, _)| n.ends_with(KEPT)).map(|(_, t)| t).collect();
        assert_eq!(replaced, [MINE]);
        assert!(no_temporary_file(dir));
    }

    #[test]
    fn a_copied_backup_is_flushed_and_verified() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, _)) = (tmp.path(), with_file(tmp.path()));
        let backup = dir.join(REPLACED_DIR).join("f.toml.copy");
        let bytes = fs::read(&path).unwrap();
        let Backup::Held(held) = copy_backup(&path, &bytes, backup.clone()).unwrap() else { panic!("no backup") };
        assert!(held.linked.is_none() && held.found == ContentHash::of(MINE.as_bytes()));
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    /// An editor that opened the file before the save and writes through that handle after the save checked the
    /// displaced bytes writes into the kept entry, so nothing is lost, and pruning never deletes that entry.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_write_through_a_handle_held_across_the_save_survives_in_the_replaced_folder() {
        use std::io::Write as _;

        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let handle = OpenOptions::new().append(true).open(&path).unwrap();
        let late_write = || handle.try_clone().unwrap().write_all(b"late = 9\n").unwrap();
        let out = replace_with(dir, expected, &Hooks { verified: &late_write, ..Hooks::default() }).unwrap();
        handle.sync_all().unwrap();
        assert_eq!(out, Replace::Placed);
        assert_eq!(fs::read_to_string(&path).unwrap(), "ours = 3\n");
        let items = kept(dir);
        assert_eq!(texts(&items), ["mine = 1\nlate = 9\n"]);
        assert!(items[0].0.ends_with(KEPT), "{items:?}");
        assert!(no_temporary_file(dir));
        // Long after, the entry still isn't pruned: its bytes are not the ones it was kept with.
        assert_eq!(prune_replaced(dir, later()), 0);
        assert_eq!(texts(&kept(dir)), ["mine = 1\nlate = 9\n"]);
    }

    /// An editor that holds the original open and writes through it after the swap but before the check is caught by
    /// the check: the swap is undone, the editor's bytes are at the target and our file is kept.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_write_through_a_held_handle_before_the_check_is_never_lost() {
        use std::os::unix::fs::FileExt;

        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let handle = OpenOptions::new().write(true).open(&path).unwrap();
        let early_write = || handle.write_all_at(b"theirs = 2\n", 0).unwrap();
        let out = replace_with(dir, expected, &Hooks { swapped: &early_write, ..Hooks::default() }).unwrap();
        assert_eq!(out, Replace::Refused);
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 2\n");
        let items = kept(dir);
        assert_eq!(texts(&items), ["ours = 3\n"]);
        assert!(items[0].0.ends_with(".displaced"), "{items:?}");
        assert!(no_temporary_file(dir));
    }

    /// Matrix, exchange without links (with or without a no-replace rename): a write through a handle opened before
    /// the save lands in the kept original, which replaced the copied backup.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn with_exchange_but_no_links_the_original_inode_is_kept_for_a_held_handle() {
        use std::io::Write as _;

        for no_replace_off in [false, true] {
            let tmp = TempDir::new().unwrap();
            let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
            let mut handle = OpenOptions::new().append(true).open(&path).unwrap();
            let hooks = Hooks { links_off: true.into(), no_replace_off: no_replace_off.into(), ..Hooks::default() };
            assert_eq!(replace_with(dir, expected, &hooks).unwrap(), Replace::Placed);
            handle.write_all(b"late = 9\n").unwrap();
            handle.sync_all().unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "ours = 3\n");
            assert_eq!(texts(&kept(dir)), ["mine = 1\nlate = 9\n"]);
            assert!(no_temporary_file(dir));
            assert_eq!(prune_replaced(dir, later()), 0);
        }
    }

    /// Matrix, no exchange (every combination of the others): replacing is refused before anything moves.
    #[test]
    fn without_exchange_a_replacement_is_refused_and_the_file_is_untouched() {
        for (links_off, no_replace_off) in [(false, false), (false, true), (true, false), (true, true)] {
            let tmp = TempDir::new().unwrap();
            let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
            let inode_before = file_id(&fs::metadata(&path));
            let hooks = Hooks {
                links_off: links_off.into(),
                exchange_off: true.into(),
                no_replace_off: no_replace_off.into(),
                ..Hooks::default()
            };
            let err = replace_with(dir, expected, &hooks).unwrap_err();
            assert!(matches!(err, StoreError::NoSafeChange { missing: NO_EXCHANGE, .. }), "{err}");
            assert!(err.to_string().contains("exchange"), "{err}");
            assert_eq!(fs::read_to_string(&path).unwrap(), MINE);
            assert_eq!(file_id(&fs::metadata(&path)), inode_before);
            assert_eq!(kept(dir), [], "links off: {links_off}, no-replace off: {no_replace_off}");
            assert!(no_temporary_file(dir));
        }
    }

    /// Matrix, a new file: a hard link alone or a no-replace rename alone places it; neither refuses.
    #[test]
    fn a_new_file_needs_links_or_a_no_replace_rename() {
        let new_file = |links_off: bool, no_replace_off: bool| {
            let tmp = TempDir::new().unwrap();
            let hooks = Hooks {
                links_off: links_off.into(),
                exchange_off: true.into(),
                no_replace_off: no_replace_off.into(),
                ..Hooks::default()
            };
            let out = replace_with(tmp.path(), None, &hooks);
            let text = fs::read_to_string(tmp.path().join("f.toml")).ok();
            assert!(no_temporary_file(tmp.path()));
            (out, text)
        };
        let (out, text) = new_file(false, true);
        assert_eq!((out.unwrap(), text.as_deref()), (Replace::Placed, Some("ours = 3\n")));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let (out, text) = new_file(true, false);
            assert_eq!((out.unwrap(), text.as_deref()), (Replace::Placed, Some("ours = 3\n")));
        }
        let (out, text) = new_file(true, true);
        let err = out.unwrap_err();
        assert!(matches!(err, StoreError::NoSafeChange { missing: NO_NO_REPLACE, .. }), "{err}");
        assert_eq!(text, None);
    }

    /// Matrix, a deletion with links (no exchange, no no-replace rename needed).
    #[test]
    fn with_links_a_deletion_needs_nothing_else() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let hooks = Hooks { exchange_off: true.into(), no_replace_off: true.into(), ..Hooks::default() };
        assert!(remove_checked_with(d, &["a".to_owned()], Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap());
        assert!(!d.join("a.toml").exists());
        assert_eq!(texts(&kept(d)), ["x = 1\n"]);
    }

    /// Matrix, a deletion with neither links nor a no-replace rename: refused before the file moves.
    #[test]
    fn without_links_or_a_no_replace_rename_a_deletion_is_refused_and_the_file_is_untouched() {
        for exchange_off in [false, true] {
            let tmp = TempDir::new().unwrap();
            let d = tmp.path();
            write(d, "a.toml", "x = 1\n");
            let inode_before = file_id(&fs::metadata(d.join("a.toml")));
            let hooks = Hooks {
                links_off: true.into(),
                exchange_off: exchange_off.into(),
                no_replace_off: true.into(),
                ..Hooks::default()
            };
            let err = remove_checked_with(d, &["a".to_owned()], Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap_err();
            assert!(matches!(err, StoreError::NoSafeChange { missing: NO_NO_REPLACE, .. }), "{err}");
            assert_eq!(fs::read_to_string(d.join("a.toml")).unwrap(), "x = 1\n");
            assert_eq!(file_id(&fs::metadata(d.join("a.toml"))), inode_before);
            assert_eq!(kept(d), []);
            assert_eq!(all_names(d), [".omuzaki", "a.toml"]);
        }
    }

    /// An editor's file found by a deletion that can't be put back is never hidden: the error is returned and the
    /// file stays beside the target, where the error says. With links (the restore link fails) and without (the
    /// no-replace rename fails).
    #[test]
    fn a_restore_that_fails_after_a_raced_deletion_is_an_error_and_the_file_stays_beside_the_target() {
        let cases: &[bool] =
            if cfg!(any(target_os = "linux", target_os = "macos")) { &[false, true] } else { &[false] };
        for &links_off in cases {
            let tmp = TempDir::new().unwrap();
            let d = tmp.path();
            write(d, "a.toml", "x = 1\n");
            let path = d.join("a.toml");
            let save = || {
                fs::write(d.join("editor.tmp"), "theirs = 2\n").unwrap();
                fs::rename(d.join("editor.tmp"), &path).unwrap();
            };
            let slot: std::cell::OnceCell<&Hooks<'_>> = std::cell::OnceCell::new();
            // From here on the file can't be put back.
            let break_restore = || {
                let hooks = slot.get().unwrap();
                hooks.links_off.set(true);
                hooks.no_replace_off.set(true);
            };
            let hooks = Hooks {
                links_off: links_off.into(),
                backed_up: &save,
                quarantined: &break_restore,
                ..Hooks::default()
            };
            assert!(slot.set(&hooks).is_ok());
            let err = remove_checked_with(d, &["a".to_owned()], Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap_err();
            let StoreError::Io { path: beside, .. } = &err else { panic!("{err}") };
            assert_eq!(beside.parent(), Some(d), "{err}");
            assert_eq!(fs::read_to_string(beside).unwrap(), "theirs = 2\n");
            assert!(err.to_string().contains("couldn't be put back"), "{err}");
            assert_eq!(texts(&kept(d)), ["x = 1\n"], "links off: {links_off}");
        }
    }

    /// The same for a deletion: the quarantined original is renamed over the copy, not unlinked.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn without_links_a_deleted_file_keeps_its_inode_for_a_held_handle() {
        use std::io::Write as _;

        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let mut handle = OpenOptions::new().append(true).open(d.join("a.toml")).unwrap();
        let hooks = Hooks { links_off: true.into(), ..Hooks::default() };
        let segs = ["a".to_owned()];
        assert!(remove_checked_with(d, &segs, Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap());
        handle.write_all(b"late = 9\n").unwrap();
        handle.sync_all().unwrap();
        assert!(!d.join("a.toml").exists());
        assert_eq!(texts(&kept(d)), ["x = 1\nlate = 9\n"]);
    }

    /// Just past the retention time from now.
    fn later() -> SystemTime {
        SystemTime::now() + RETAIN_UNCHANGED + Duration::from_secs(1)
    }

    /// Replaces `rel` in `dir` (writing it first if needed), so its old text is kept in the replaced folder.
    fn replace_once(dir: &Path, rel: &str) {
        let path = dir.join(rel);
        if !path.exists() {
            write(dir, rel, MINE);
        }
        let text = fs::read(&path).unwrap();
        let out = replace_checked(dir, &path, "ours = 3\n", Some(ContentHash::of(&text))).unwrap();
        assert_eq!(out, Replace::Placed);
    }

    #[test]
    fn pruning_deletes_only_old_kept_entries_that_nothing_wrote_to() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        replace_once(dir, "a.toml");
        replace_once(dir, "deep/b.toml");
        replace_once(dir, "c.toml");
        let items = kept(dir);
        assert_eq!(items.len(), 3, "{items:?}");
        // One entry was written to after its save; a raced file and a stray file sit beside them.
        let changed = dir.join(REPLACED_DIR).join(&items.iter().find(|(n, _)| n.starts_with("c.toml")).unwrap().0);
        fs::write(&changed, "edited later = 1\n").unwrap();
        write(dir, ".omuzaki/replaced/d.toml.1-2-3.displaced", "raced = 1\n");
        write(dir, ".omuzaki/replaced/notes.txt", "mine\n");

        // Young entries are kept, however unchanged.
        assert_eq!(prune_replaced(dir, SystemTime::now()), 0);
        assert_eq!(kept(dir).len(), 5);
        // Old and unchanged go; changed, raced and anything else stay.
        assert_eq!(prune_replaced(dir, later()), 2);
        let left: Vec<_> = kept(dir).into_iter().map(|(n, t)| (n.split('.').next().unwrap().to_owned(), t)).collect();
        assert_eq!(
            left,
            [
                ("c".to_owned(), "edited later = 1\n".to_owned()),
                ("d".to_owned(), "raced = 1\n".to_owned()),
                ("notes".to_owned(), "mine\n".to_owned()),
            ]
        );
        // Nothing to prune is fine, and so is a project with no replaced folder.
        assert_eq!(prune_replaced(dir, later()), 0);
        assert_eq!(prune_replaced(&dir.join("deep"), later()), 0);
    }

    #[test]
    fn pruning_deletes_at_most_its_limit_per_call() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        for _ in 0..PRUNE_MAX + 6 {
            replace_once(dir, "a.toml");
            fs::write(dir.join("a.toml"), MINE).unwrap();
        }
        assert_eq!(kept(dir).len(), PRUNE_MAX + 6);
        assert_eq!(prune_replaced(dir, later()), PRUNE_MAX);
        assert_eq!(kept(dir).len(), 6);
        assert_eq!(prune_replaced(dir, later()), 6);
        assert_eq!(kept(dir), []);
    }

    #[test]
    fn only_kept_names_carry_a_time_and_a_hash() {
        let hash = ContentHash::of(b"x");
        assert_eq!(kept_stamp(&format!("f.toml.1500-7-0.{hash}.kept")), Some((Duration::from_nanos(1500), hash)));
        for name in ["f.toml.1500-7-0.displaced", "f.toml.1500-7-0.nothex.kept", &format!("f.toml.x-7-0.{hash}.kept")] {
            assert_eq!(kept_stamp(name), None, "{name}");
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_no_replace_rename_refuses_an_existing_target_and_leaves_both_files() {
        let tmp = TempDir::new().unwrap();
        let (dir, path) = (tmp.path(), tmp.path().join("f.toml"));
        let from = dir.join(".f.toml.tmp");
        fs::write(&from, "ours = 3\n").unwrap();
        fs::write(&path, "theirs = 1\n").unwrap();
        // The kernel makes the check and the rename one step, so there is no window for a file to be lost in.
        assert!(!rename_no_replace(&from, &path).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 1\n");
        assert_eq!(fs::read_to_string(&from).unwrap(), "ours = 3\n");
        fs::remove_file(&path).unwrap();
        assert!(rename_no_replace(&from, &path).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "ours = 3\n");
        assert!(!from.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_file_created_while_links_fail_is_never_replaced() {
        let tmp = TempDir::new().unwrap();
        let (dir, path) = (tmp.path(), tmp.path().join("f.toml"));
        // Links fail, and an editor creates the target in the instant before the no-replace rename runs.
        let create = || fs::write(dir.join("f.toml"), "theirs = 1\n").unwrap();
        let hooks = Hooks { links_off: true.into(), backed_up: &create, ..Hooks::default() };
        assert_eq!(replace_with(dir, None, &hooks).unwrap(), Replace::Refused);
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 1\n");
        assert!(no_temporary_file(dir));
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
        let empty = Some(ContentHash::of(b""));
        assert!(remove_checked(d, &["a".into(), "b".into(), "c".into()], empty).unwrap());
        assert!(!d.join("a/b").exists() && d.join("a").exists());
        assert!(remove_checked(d, &["a".into(), "keep".into(), "y".into()], empty).unwrap());
        assert!(d.join("a/keep/x.track").exists());
        // already gone: fine
        assert!(remove_checked(d, &["a".into(), "keep".into(), "y".into()], empty).unwrap());
    }

    #[test]
    fn remove_checked_refuses_other_bytes_and_keeps_what_it_deletes() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let segs = ["a".to_owned()];
        assert!(!remove_checked(d, &segs, Some(ContentHash::of(b"x = 2\n"))).unwrap());
        assert!(!remove_checked(d, &segs, None).unwrap());
        assert_eq!(fs::read_to_string(d.join("a.toml")).unwrap(), "x = 1\n");
        assert!(remove_checked(d, &segs, Some(ContentHash::of(b"x = 1\n"))).unwrap());
        assert!(!d.join("a.toml").exists());
        assert_eq!(texts(&kept(d)), ["x = 1\n"]);
    }

    #[test]
    fn a_save_between_the_backup_and_the_deletion_survives_and_is_a_conflict() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let path = d.join("a.toml");
        let save = || {
            fs::write(d.join(".a.toml.editor"), "theirs = 2\n").unwrap();
            fs::rename(d.join(".a.toml.editor"), &path).unwrap();
        };
        let hooks = Hooks { backed_up: &save, ..Hooks::default() };
        let segs = ["a".to_owned()];
        assert!(!remove_checked_with(d, &segs, Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs = 2\n");
        assert_eq!(texts(&kept(d)), ["x = 1\n"]);
        let names: Vec<_> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(names.iter().all(|n| !n.to_string_lossy().ends_with(".del")), "{names:?}");
    }

    #[test]
    fn a_name_taken_again_during_a_conflicting_deletion_keeps_both_files() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let path = d.join("a.toml");
        let swap = || {
            fs::remove_file(&path).unwrap();
            fs::write(&path, "theirs = 2\n").unwrap();
        };
        let retake = || fs::write(&path, "newer = 3\n").unwrap();
        let hooks = Hooks { backed_up: &swap, quarantined: &retake, ..Hooks::default() };
        let segs = ["a".to_owned()];
        assert!(!remove_checked_with(d, &segs, Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "newer = 3\n");
        let items = kept(d);
        assert!(items.iter().any(|(n, t)| n.ends_with(".displaced") && t == "theirs = 2\n"), "{items:?}");
    }
}
