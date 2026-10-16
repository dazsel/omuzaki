//! The project folder: reading it into a tree, and writing, replacing and deleting its files.
//!
//! # What each change needs, and what happens without it
//!
//! A change to a project file is made atomically or refused before the file is touched; there is no non-atomic
//! fallback. A refusal is [`StoreError::NoSafeChange`] naming what is missing, and the save reports it. Four
//! capabilities decide which; each depends on the system and on the file system:
//!
//! - **Exchange**: swap two names atomically (Linux `renameat2` with `RENAME_EXCHANGE`, macOS `renameatx_np` with
//!   `RENAME_SWAP`).
//! - **No-replace rename**: rename only if the new name is free, atomically (Linux `renameat2` with
//!   `RENAME_NOREPLACE`, macOS `renameatx_np` with `RENAME_EXCL`).
//! - **Hard links**: `link` fails if the new name exists, so it is also a placement that never replaces.
//! - **POSIX-semantics rename** (Windows only): rename over an existing file atomically, so the path names the old file
//!   or the new one at every instant (`FileRenameInfoEx`, below). It stands in for the exchange, which Windows lacks;
//!   the table's first three columns don't include it, and its row says where it applies.
//!
//! | Exchange | No-replace rename | Hard links | Replace a file ([`replace_checked`]) | Write a new file | Delete ([`remove_checked`]) |
//! |---|---|---|---|---|---|
//! | yes | yes | yes | linked backup, swap, check, undo on a race | no-replace link | linked backup; move aside by no-replace rename; delete, or put back by link |
//! | yes | no | yes | as above | no-replace link | refused |
//! | yes | yes | no | copied backup, swap, check, undo on a race; the original then replaces the copy | no-replace rename | copied backup; move aside by no-replace rename; original over the copy, or put back by no-replace rename |
//! | yes | no | no | as above | refused | refused |
//! | no | yes | yes | refused, except Windows: linked backup, then a POSIX-semantics rename over the target (below) | no-replace link | as in the first row |
//! | no | no | yes | refused | no-replace link | refused |
//! | no | yes | no | refused | no-replace rename | as in the third row |
//! | no | no | no | refused | refused | refused |
//!
//! Every move into a slot of our own (a deletion's hidden quarantine name beside the file, a `.displaced` entry of the
//! replaced folder) is a no-replace rename too, and an occupied slot (a crashed process's leftover whose pid was
//! reused) is skipped for the next name, never replaced.
//!
//! Where each platform stands with what this crate's dependencies reach today:
//!
//! | Platform | Exchange | No-replace rename | Hard links |
//! |---|---|---|---|
//! | Linux | yes where the file system supports the flag (ext4, XFS, Btrfs, tmpfs); otherwise the kernel says `EINVAL` and the change is refused | the same | yes, except FAT and exFAT |
//! | macOS | yes on APFS and HFS+ (macOS 10.12 or later); a file system without it says `ENOTSUP` and the change is refused | the same | yes on APFS and HFS+ |
//! | Windows | no (Windows has no atomic exchange); see below | yes on every volume (`SetFileInformationByHandle` with `FileRenameInfo`, `ReplaceIfExists` false) | yes on NTFS, no on FAT and exFAT |
//!
//! **Replacing a file on Windows.** With no exchange, Windows replaces a file that has a hard-linked backup with one
//! atomic rename over it: `SetFileInformationByHandle` with `FileRenameInfoEx` and `FILE_RENAME_FLAG_REPLACE_IF_EXISTS
//! | FILE_RENAME_FLAG_POSIX_SEMANTICS` (Windows 10 1607 or later, NTFS), so the path names the old file or the new one
//! at every instant, and a reader that holds the old one open keeps it. `std::fs::rename` is not used: in Rust 1.99
//! (`library/std/src/sys/fs/windows.rs`, `rename`) it calls `MoveFileExW` and reaches POSIX semantics only after access
//! is denied. A volume or system without the POSIX-semantics rename (FAT, exFAT, older Windows) is refused with
//! [`StoreError::NoSafeChange`] before anything moves; FAT and exFAT have no hard links either, so they are refused
//! before that. Unlike the swap, a rename over the target can't hand back the file it displaced, so there is no check
//! after it: an editor's save that lands between the hash check and the rename is replaced, and the checked file
//! survives in the kept entry. That window is the hash check's own, not a missing capability.
//!
//! Each cell of the first table is tested with the capabilities forced off through the test seam (`Hooks`).

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
pub(crate) fn refuse_links(dir: &Path, path: &Path) -> Result<(), StoreError> {
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
///   into nothing. [`Retention::prune`] deletes it once it is older than [`RETAIN_UNCHANGED`] and still has that hash.
/// - `<path>.<time>-<pid>-<n>.displaced`: a file that raced a save (an editor's save or write) or that an interrupted
///   save left behind. No hash is recorded, so nothing ever deletes it automatically.
const REPLACED_DIR: &str = ".omuzaki/replaced";

/// The suffix of an entry of the replaced folder that [`Retention::prune`] may delete.
const KEPT: &str = ".kept";

/// How long a replaced file is kept even if nothing wrote to it: an editor that still holds the old file open and
/// writes within this time finds its bytes in the replaced folder.
pub(crate) const RETAIN_UNCHANGED: Duration = Duration::from_mins(10);

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
/// Where the swap isn't available (see the module's table) a file that exists is replaced only on Windows, and only
/// with a hard-linked backup: step 4 is then a POSIX-semantics rename of the temporary file over the target (see the
/// module's note on Windows). It can't hand the displaced file back, so there is no swap to undo and no check after it;
/// the file hashed in step 2 stays in the kept entry (the backup link), and an editor's save that lands between that
/// hash check and the rename is replaced. Everywhere else, and on Windows without links or without that rename (FAT,
/// exFAT, older Windows), there is no safe way to replace it, so step 4 fails with [`StoreError::NoSafeChange`] before
/// the target is touched: the target is as it was, and the backup and the temporary file (both only ours) are removed.
///
/// A new file (no target) is placed with a no-replace `hard_link`, or where links fail with a rename that won't replace, so a file that appeared is never replaced;
/// without either it is refused the same way.
///
/// The target exists at every instant of a replacement, so a crash leaves the old file or the new one, plus at worst
/// a backup, the hidden temporary file, or both.
pub(crate) fn replace_checked(
    dir: &Path,
    path: &Path,
    text: &str,
    expected: Option<ContentHash>,
    retention: &mut Retention,
) -> Result<Replace, StoreError> {
    let hooks = Hooks::default();
    let outcome = replace_checked_with(dir, path, text, expected, &hooks);
    register_backup(&hooks, retention);
    outcome
}

/// Registers the backup a call made with `retention`, if it is still there: the kept entry of a replacement or a
/// deletion. One that was tidied away (nothing placed) is not.
fn register_backup(hooks: &Hooks<'_>, retention: &mut Retention) {
    if let Some(backup) = hooks.made.borrow_mut().take()
        && fs::symlink_metadata(&backup).is_ok()
    {
        retention.created(backup, clock().1);
    }
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
    /// After [`file_state`] looked at the path, before it reads the file.
    stat_done: &'a dyn Fn(),
    /// Pretend the file system has no hard links (tests only).
    links_off: Cell<bool>,
    /// Pretend the system can't swap two names atomically (tests only).
    exchange_off: Cell<bool>,
    /// Pretend the system has no rename that refuses to replace (tests only).
    no_replace_off: Cell<bool>,
    /// Pretend the volume has no POSIX-semantics rename over a file (Windows; tests only).
    posix_rename_off: Cell<bool>,
    /// Make flushing a folder fail, where a step goes through [`Hooks::sync_folder`] (tests only).
    sync_off: Cell<bool>,
    /// Make flushing a displaced file fail after it was opened (tests only).
    flush_file_off: Cell<bool>,
    /// Make looking at the target fail with an error other than "not found" after a deletion moved it aside (tests only).
    look_off: Cell<bool>,
    /// Make every [`rename_no_replace`] fail after this many have succeeded (tests only).
    rename_fails_after: Cell<Option<u32>>,
    /// Take Windows's replacement without a swap on any system (tests only).
    windows_shaped: Cell<bool>,
    /// Pretend the system has no file identities, as on Windows: a linked backup records none (tests only).
    ids_off: Cell<bool>,
    /// Make writing a copied backup fail after the file was created (tests only).
    copy_fails: Cell<bool>,
    /// The backup this call made, if any: the public functions register it for pruning if it is still there.
    made: RefCell<Option<PathBuf>>,
}

fn noop() {}

/// Faults a test elsewhere in the crate (which can't pass hooks through `Project`) sets for the default hooks of the
/// calls made on its own thread. Compiled only into tests; elsewhere they are always off.
pub(crate) mod faults {
    #[cfg(test)]
    thread_local! {
        /// Pretend the file system has no hard links.
        pub(crate) static LINKS_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        /// Make flushing a folder fail.
        pub(crate) static SYNC_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        /// How far ahead of the real clocks the wall clock and the monotonic clock are.
        pub(crate) static CLOCK_SKEW: std::cell::Cell<(std::time::Duration, std::time::Duration)> =
            const { std::cell::Cell::new((std::time::Duration::ZERO, std::time::Duration::ZERO)) };
    }

    /// Whether this thread's test turned hard links off.
    pub(super) fn links_off() -> bool {
        #[cfg(test)]
        return LINKS_OFF.with(std::cell::Cell::get);
        #[cfg(not(test))]
        false
    }

    /// How far this thread's test moved the wall clock and the monotonic clock ahead.
    pub(super) fn clock_skew() -> (std::time::Duration, std::time::Duration) {
        #[cfg(test)]
        return CLOCK_SKEW.with(std::cell::Cell::get);
        #[cfg(not(test))]
        (std::time::Duration::ZERO, std::time::Duration::ZERO)
    }

    /// Whether this thread's test made flushing a folder fail.
    pub(super) fn sync_off() -> bool {
        #[cfg(test)]
        return SYNC_OFF.with(std::cell::Cell::get);
        #[cfg(not(test))]
        false
    }
}

/// Marks an I/O error of a step after a change was committed as [`StoreError::NotDurable`].
fn not_durable(e: StoreError) -> StoreError {
    match e {
        StoreError::Io { path, source } => StoreError::NotDurable { path, source },
        other => other,
    }
}

/// What is at a file's path, compared with the bytes last seen there.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FileState {
    /// Nothing is there.
    Vanished,
    /// A regular file with the expected bytes.
    Same,
    /// Anything else: other bytes, or something that isn't a regular file.
    Different,
}

/// Looks at the file at `path` (whose path the caller has already checked for links) against the hash `expected`.
/// Every absence it sees, at any step, is [`FileState::Vanished`]: a file deleted between looking at the path and
/// reading it is gone, never "unchanged".
pub(crate) fn file_state(path: &Path, expected: Option<ContentHash>) -> Result<FileState, StoreError> {
    file_state_with(path, expected, &Hooks::default())
}

fn file_state_with(path: &Path, expected: Option<ContentHash>, hooks: &Hooks<'_>) -> Result<FileState, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => return Ok(FileState::Different),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(FileState::Vanished),
        Err(e) => return Err(StoreError::io(path, e)),
    }
    (hooks.stat_done)();
    // The type and the bytes come from one handle opened without following a link, so a link swapped in after the
    // look above is seen as what it is, never read through.
    let mut file = match open_no_follow(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(FileState::Vanished),
        Err(e) if is_link_error(&e) => return Ok(FileState::Different),
        Err(e) => return Err(StoreError::io(path, e)),
    };
    if !file.metadata().map_err(|e| StoreError::io(path, e))?.is_file() {
        return Ok(FileState::Different);
    }
    let mut bytes = Vec::new();
    match file.read_to_end(&mut bytes) {
        Ok(_) if Some(ContentHash::of(&bytes)) == expected => Ok(FileState::Same),
        Ok(_) => Ok(FileState::Different),
        Err(e) => Err(StoreError::io(path, e)),
    }
}

/// Opens `path` for reading without following a link at its last component: `O_NOFOLLOW` (Linux, macOS), where a
/// link fails to open ([`is_link_error`]); `FILE_FLAG_OPEN_REPARSE_POINT` (Windows), where the link itself opens and
/// its metadata says it isn't a file.
fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open a link itself, never what it points to.
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

/// True for the error opening a link without following it gives (`ELOOP`).
fn is_link_error(e: &io::Error) -> bool {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    return e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error());
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = e;
        false
    }
}

impl Default for Hooks<'_> {
    fn default() -> Self {
        Self {
            staged: &noop,
            backed_up: &noop,
            swapped: &noop,
            verified: &noop,
            undone: &noop,
            quarantined: &noop,
            stat_done: &noop,
            links_off: Cell::new(faults::links_off()),
            exchange_off: Cell::new(false),
            no_replace_off: Cell::new(false),
            posix_rename_off: Cell::new(false),
            sync_off: Cell::new(faults::sync_off()),
            flush_file_off: Cell::new(false),
            look_off: Cell::new(false),
            rename_fails_after: Cell::new(None),
            windows_shaped: Cell::new(false),
            ids_off: Cell::new(false),
            copy_fails: Cell::new(false),
            made: RefCell::new(None),
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

    /// A POSIX-semantics rename of `from` over `to` (`rename_by_handle`), unless a test turned it off. Off Windows it
    /// is reached only through [`Hooks::windows_shaped`], and a plain rename stands in for it.
    fn rename_over_posix(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.posix_rename_off.get() {
            return Err(io::ErrorKind::Unsupported.into());
        }
        #[cfg(windows)]
        return rename_by_handle(from, to, OnExisting::ReplacePosix).map(|_| ());
        #[cfg(not(windows))]
        fs::rename(from, to)
    }

    /// [`sync_folder`], unless a test made it fail.
    fn sync_folder(&self, folder: Option<&Path>) -> Result<(), StoreError> {
        match folder {
            Some(folder) if self.sync_off.get() => {
                Err(StoreError::io(folder, io::Error::other("flushing the folder failed (forced by a test)")))
            }
            _ => sync_folder(folder),
        }
    }

    /// `fs::symlink_metadata`, unless a test made it fail.
    fn look(&self, path: &Path) -> io::Result<fs::Metadata> {
        if self.look_off.get() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "looking at the file failed (forced by a test)",
            ));
        }
        fs::symlink_metadata(path)
    }

    /// [`flush_file`], unless a test made it fail.
    fn flush_file(&self, path: &Path) -> Option<io::Error> {
        if self.flush_file_off.get() {
            return Some(io::Error::other("flushing the file failed (forced by a test)"));
        }
        flush_file(path)
    }

    /// [`rename_no_replace`], unless a test turned it off.
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<bool> {
        if let Some(left) = self.rename_fails_after.get() {
            if left == 0 {
                return Err(io::Error::other("moving the file failed (forced by a test)"));
            }
            self.rename_fails_after.set(Some(left - 1));
        }
        if self.no_replace_off.get() { Err(io::ErrorKind::Unsupported.into()) } else { rename_no_replace(from, to) }
    }
}

/// What a refusal names as missing when there is no atomic swap.
const NO_EXCHANGE: &str = "atomic exchange of two file names (RENAME_EXCHANGE, RENAME_SWAP)";

/// What a refusal names as missing on a Windows volume that can't rename over a file atomically.
const NO_POSIX_RENAME: &str =
    "rename over a file with POSIX semantics (FileRenameInfoEx: Windows 10 1607 or later, NTFS)";

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
    let staged = stage(dir, path, text, hooks)?;
    let tmp = staged.path().to_path_buf();
    (hooks.staged)();
    let mut outcome = match back_up(dir, path, expected, hooks)? {
        Backup::Absent => {
            (hooks.backed_up)();
            place_no_replace(&tmp, path, hooks).map(|placed| if placed { Replace::Placed } else { Replace::Refused })
        }
        Backup::Mismatch => Ok(Replace::Refused),
        Backup::Held(held) => {
            let mut committed = false;
            let mut outcome = replace_held(dir, path, &tmp, &held, &mut committed, hooks);
            // Once our file is committed at the target the backup is the kept entry (step 5), even if a later step
            // failed: it may already be the original itself. Otherwise the target is as it was (or the editor's) and
            // any displaced file is kept separately by `preserve`, so the backup can go.
            if !committed {
                tidy(&mut outcome, &held.path);
            }
            outcome
        }
    };
    // A refusal, or a missing capability (refused before anything moved), leaves our own, unexposed temporary file
    // behind. After any other error it may hold a displaced file, so it stays until the next save moves it into the
    // replaced folder.
    if matches!(outcome, Ok(Replace::Refused) | Err(StoreError::NoSafeChange { .. })) {
        tidy(&mut outcome, &tmp);
    }
    // Every outcome has now dealt with the staged file (placed it, tidied it, or left what it holds for the next save
    // to move aside); only an early return above leaves it to the guard.
    staged.keep();
    outcome
}

/// Removes `file`, one of our own that the outcome no longer needs, before anything of ours was committed. A failure
/// doesn't pass silently: it becomes the error, unless the outcome already is one (that error says more, and only one
/// can be returned; the leftover is ours and the next save or pruning deals with it).
fn tidy(outcome: &mut Result<Replace, StoreError>, file: &Path) {
    match fs::remove_file(file) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            if outcome.is_ok() {
                *outcome = Err(StoreError::io(file, e));
            }
        }
    }
}

/// Steps 4 and 5 for a target that has a backup.
/// Sets `committed` once our file is at the target for good, after which the backup must be kept whatever happens.
fn replace_held(
    dir: &Path,
    path: &Path,
    tmp: &Path,
    held: &Held,
    committed: &mut bool,
    hooks: &Hooks<'_>,
) -> Result<Replace, StoreError> {
    (hooks.backed_up)();
    match hooks.exchange(tmp, path) {
        Ok(()) => {}
        // Windows has no swap: with a linked backup, one atomic rename over the target (see the module's table).
        // Nothing has moved if it fails. Tests can take this path on any system.
        Err(e)
            if e.kind() == io::ErrorKind::Unsupported
                && held.by_link
                && (cfg!(windows) || hooks.windows_shaped.get()) =>
        {
            hooks.rename_over_posix(tmp, path).map_err(|e| capability_error(path, NO_POSIX_RENAME, e))?;
            // Our file is at the target: the linked backup is the kept entry.
            *committed = true;
            return Ok(Replace::Placed);
        }
        // Without a swap nothing has moved yet, and nothing will: see the module's table.
        Err(e) => return Err(capability_error(path, NO_EXCHANGE, e)),
    }
    (hooks.swapped)();
    // The file we displaced is at `tmp`, and it must still hold the bytes that were hashed. An in-place edit after
    // they were hashed fails the check (even for a link, whose inode is unchanged) and falls through to the undo below.
    if held.is_file_at(tmp) {
        *committed = true;
        (hooks.verified)();
        if held.by_link {
            // The backup is a hard link to the file we displaced (on a system without file identities, Windows, the
            // check above was by bytes alone), so it is the kept entry and this second name can go. Renaming one name
            // of a file over another of the same file changes nothing, so the copy's branch would leave it behind.
            fs::remove_file(tmp).map_err(|e| StoreError::NotDurable { path: tmp.to_path_buf(), source: e })?;
        } else {
            // A copy doesn't follow an editor that still holds the original open, so the original takes the copy's
            // place (a rename over our own copy), and the folder is flushed so that survives a crash. Our file is
            // already at the target, but if either step fails the save reports it: what it promised is not durable.
            // A failed rename leaves the original at `tmp`, where the next save moves it into the replaced folder.
            fs::rename(tmp, &held.path).map_err(|e| StoreError::NotDurable { path: tmp.to_path_buf(), source: e })?;
            hooks.sync_folder(held.path.parent()).map_err(not_durable)?;
        }
        return Ok(Replace::Placed);
    }
    // Someone replaced or edited the target: put their file back.
    if let Err(e) = hooks.exchange(tmp, path) {
        preserve(dir, tmp, hooks)?;
        return Err(capability_error(path, NO_EXCHANGE, e));
    }
    (hooks.undone)();
    // Our file was visible at the target between the swap and the undo, so an editor may have it open and write into
    // it later, through a handle no hash check can see. It is therefore always kept, even if it still reads as ours.
    preserve(dir, tmp, hooks)?;
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

/// No cheap file identity here (Windows). A linked backup is then checked by its bytes alone, and the code that tidies
/// up after a check branches on [`Held::by_link`], never on an identity.
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

/// Windows has no atomic swap of two names; with a hard-linked backup it replaces a file by a POSIX-semantics rename
/// instead (see the module's table).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exchange(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Writes `text` to the hidden temporary file beside `path` and flushes it; returns the temporary file's path.
fn stage(dir: &Path, path: &Path, text: &str, hooks: &Hooks<'_>) -> Result<Artifact, StoreError> {
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
        preserve(dir, &tmp, hooks)?;
    }
    let file = OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(io_err)?;
    // From here the file is ours; the guard removes it if the save doesn't get as far as keeping it.
    let mut staged = Artifact::new(tmp);
    (&file).write_all(text.as_bytes()).and_then(|()| file.sync_all()).map_err(io_err)?;
    // Complete and flushed: if it ever can't be removed, it can go where pruning deletes it once it is old.
    staged.prunable = replaced_name(dir, path, &format!(".{}{KEPT}", ContentHash::of(text.as_bytes()))).ok();
    Ok(staged)
}

/// A file a save created that must not outlive the save unless the save keeps it. Dropping the guard without
/// [`Artifact::keep`] removes the file, so every early return (`?`) cleans up. If the removal fails and `prunable`
/// names a `.kept` slot (the file is complete and has that hash), the file is moved there, where pruning deletes it
/// once it is old; otherwise it stays where it is, and each use says what then happens to it.
struct Artifact {
    /// The file.
    path: PathBuf,
    /// Where it may go if it can't be removed.
    prunable: Option<PathBuf>,
    /// Whether dropping the guard still removes the file.
    armed: bool,
}

impl Artifact {
    fn new(path: PathBuf) -> Self {
        Self { path, prunable: None, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// The save keeps the file, or has dealt with it itself: the guard lets it be.
    fn keep(mut self) -> PathBuf {
        self.armed = false;
        std::mem::take(&mut self.path)
    }
}

impl Drop for Artifact {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            // A destructor can't report; this is the most it can do, and what stays is only ours.
            Err(_) => {
                if let Some(slot) = &self.prunable {
                    if let Some(parent) = slot.parent() {
                        drop(fs::create_dir_all(parent));
                    }
                    drop(fs::rename(&self.path, slot));
                }
            }
        }
    }
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
///
/// The move never depends on the flush. Where the file can't be flushed (on Windows another handle may deny write
/// sharing so it can't be opened for it, or the file may be read-only; or the flush itself fails), it is still moved, and the error then says that it is kept but not
/// flushed ([`flush_file`]), so the limitation is reported and the file is not left under its hidden name.
fn preserve(dir: &Path, from: &Path, hooks: &Hooks<'_>) -> Result<PathBuf, StoreError> {
    if let Some(parent) = replaced_name(dir, from, "")?.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
    }
    // A flush that fails, however it fails, is kept and reported after the move: nothing between displacing a file and
    // moving it may skip the move.
    let unflushed = if fs::symlink_metadata(from).is_ok_and(|m| m.is_file()) { hooks.flush_file(from) } else { None };
    let to = move_to_free_slot(from, &|| replaced_name(dir, from, ".displaced"), hooks)?
        .ok_or_else(|| StoreError::io(from, io::ErrorKind::NotFound.into()))?;
    sync_folder(to.parent())?;
    match unflushed {
        None => Ok(to),
        Some(e) => Err(StoreError::io(
            &to,
            io::Error::new(e.kind(), format!("it is kept here, but its contents could not be flushed: {e}")),
        )),
    }
}

/// Flushes the file at `path` to disk; the error if it couldn't be. Windows flushes only through a handle with write
/// access (`FlushFileBuffers` fails with "Access is denied" on a read-only one), so there the file is opened for
/// writing, without truncating or writing anything; elsewhere a read-only handle can be flushed. Only the new handle's
/// share modes are ours (std opens with all three): a handle that is already open can deny write sharing, or the file
/// can be read-only, and then the open fails. Either way, and when the flush fails after the open, the caller learns
/// that the file couldn't be flushed and goes on with what it must do.
fn flush_file(path: &Path) -> Option<io::Error> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    options.write(true);
    options.open(path).and_then(|file| file.sync_all()).err()
}

/// How many occupied names [`move_to_free_slot`] skips before it gives up.
const SLOT_TRIES: usize = 1024;

/// Moves `from` to the first name `slot` gives that is free, never replacing one that is occupied (a crashed
/// process's leftover can hold the same name once its pid is reused): each name is tried with [`rename_no_replace`],
/// and an occupied one is skipped for the next. `None` if `from` is gone.
fn move_to_free_slot(
    from: &Path,
    slot: &dyn Fn() -> Result<PathBuf, StoreError>,
    hooks: &Hooks<'_>,
) -> Result<Option<PathBuf>, StoreError> {
    for _ in 0..SLOT_TRIES {
        let to = slot()?;
        match hooks.rename_no_replace(from, &to) {
            Ok(true) => return Ok(Some(to)),
            Ok(false) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(capability_error(from, NO_NO_REPLACE, e)),
        }
    }
    Err(StoreError::io(from, io::Error::new(io::ErrorKind::AlreadyExists, "found no free name to move it to")))
}

/// How many entries a pruning pass deletes beyond those created since the last pass, to work off a backlog.
pub(crate) const PRUNE_CATCH_UP: usize = 16;

/// The most queued entries a pass looks at beyond its budget (ones not yet old by the wall clock, or written to), so
/// a pass costs a small fixed amount on top of what it deletes.
const PRUNE_EXAMINE_SLACK: usize = 16;

/// Retention of a project's replaced folder: which `.kept` entries exist and how old each is. A `Project` keeps one
/// and runs [`Retention::prune`] on every save.
///
/// An entry may be deleted only once it is at least [`RETAIN_UNCHANGED`] old and its bytes still have the hash in its
/// name. Its age is its wall-clock age, from the time in its name; a time in the future counts as age 0 until the
/// clock passes it. An entry this process made also has a monotonic age, from when it was made, and both must pass,
/// so a wall clock that jumps forward can't make it old early. An entry an earlier process left keeps its real age:
/// it is aged by its name alone and never reset to the moment it was found.
///
/// Not `Clone`: a copy would stop seeing what the other registers. `Project` clones share one through an `Arc`.
#[derive(Debug, Default)]
pub(crate) struct Retention {
    /// Entries this process made, with the monotonic time each was made, oldest first (one not yet old on the wall
    /// clock goes to the back again).
    made: VecDeque<(PathBuf, Instant)>,
    /// Entries an earlier process left, aged by their names alone, in the order they were found.
    found: VecDeque<PathBuf>,
    /// Every entry in either queue, and those found written to (never deleted), so a walk doesn't add one twice.
    known: HashSet<PathBuf>,
    /// Entries made since the last pass: the least the next pass may delete.
    owed: usize,
    /// Whether the folder was walked for the entries earlier processes left. Entries this process makes are
    /// registered as they are made, and the project lock keeps other Omuzaki processes out, so one walk is enough.
    scanned: bool,
}

/// What a pass decided about one entry.
enum Verdict {
    /// Deleted.
    Pruned,
    /// Not old enough on the wall clock yet, or couldn't be read or deleted: try again later.
    Later,
    /// Gone, or never to be deleted (written to, not a plain file, not a `.kept` name): stop tracking it.
    Done,
}

impl Retention {
    /// Registers `entry`, a `.kept` entry this process just made at `now` (monotonic).
    pub(crate) fn created(&mut self, entry: PathBuf, now: Instant) {
        if self.known.insert(entry.clone()) {
            self.made.push_back((entry, now));
            self.owed += 1;
        }
    }

    /// One pruning pass over the replaced folder of the project `dir` at the wall time `wall` and monotonic time
    /// `mono`; returns how many entries it deleted.
    ///
    /// It deletes at most the entries made since the last pass plus [`PRUNE_CATCH_UP`], so pruning keeps pace with
    /// saves and works off a backlog, and looks at no more than [`PRUNE_EXAMINE_SLACK`] entries beyond that. The first
    /// pass also walks the folder once for entries earlier processes left. An entry that was written to after its save
    /// no longer matches its hash and is never deleted, nor is a `.displaced` entry or anything else in the folder.
    /// Best effort: what can't be read or deleted is tried again later, and nothing is touched if the folder is
    /// reached through a symbolic link.
    pub(crate) fn prune(&mut self, dir: &Path, wall: SystemTime, mono: Instant) -> usize {
        let root = dir.join(REPLACED_DIR);
        if refuse_links(dir, &root).is_err() {
            return 0;
        }
        if !self.scanned {
            self.scan(&root);
        }
        let budget = self.owed + PRUNE_CATCH_UP;
        self.owed = 0;
        let wall_now = wall.duration_since(UNIX_EPOCH).unwrap_or_default();
        let limit = budget + PRUNE_EXAMINE_SLACK;
        let (mut pruned, mut examined) = (0, 0);
        // Entries this process made: in monotonic order, so the first one not old enough ends this part.
        while pruned < budget && examined < limit {
            let Some((entry, since)) = self.made.pop_front() else { break };
            if mono.saturating_duration_since(since) < RETAIN_UNCHANGED {
                self.made.push_front((entry, since));
                break;
            }
            examined += 1;
            match self.judge(&entry, wall_now) {
                Verdict::Pruned => pruned += 1,
                Verdict::Later => self.made.push_back((entry, since)),
                Verdict::Done => {}
            }
        }
        // Entries earlier processes left: each looked at once per pass at most.
        let mut left = self.found.len();
        while pruned < budget && examined < limit && left > 0 {
            let Some(entry) = self.found.pop_front() else { break };
            left -= 1;
            examined += 1;
            match self.judge(&entry, wall_now) {
                Verdict::Pruned => pruned += 1,
                Verdict::Later => self.found.push_back(entry),
                Verdict::Done => {}
            }
        }
        pruned
    }

    /// Deletes `entry` if it is old on the wall clock (`wall_now` since the epoch) and unchanged.
    fn judge(&mut self, entry: &Path, wall_now: Duration) -> Verdict {
        let Some((saved, hash)) = entry.file_name().and_then(|n| n.to_str()).and_then(kept_stamp) else {
            self.known.remove(entry);
            return Verdict::Done;
        };
        // A time in the future is age 0.
        if wall_now.saturating_sub(saved) < RETAIN_UNCHANGED {
            return Verdict::Later;
        }
        match fs::symlink_metadata(entry) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.known.remove(entry);
                return Verdict::Done;
            }
            Ok(meta) if meta.is_file() => {}
            // Not a plain file any more: never ours to delete. It stays known so a walk doesn't add it again.
            Ok(_) => return Verdict::Done,
            Err(_) => return Verdict::Later,
        }
        match fs::read(entry) {
            Ok(bytes) if ContentHash::of(&bytes) == hash => {
                if fs::remove_file(entry).is_ok() {
                    self.known.remove(entry);
                    Verdict::Pruned
                } else {
                    Verdict::Later
                }
            }
            // Written to after its save: someone's edits, which only a person deletes. It stays known.
            Ok(_) => Verdict::Done,
            Err(_) => Verdict::Later,
        }
    }

    /// Walks the replaced folder at `root` and queues every `.kept` entry not known yet, aged by its name.
    fn scan(&mut self, root: &Path) {
        self.scanned = true;
        let mut found = Vec::new();
        let mut folders = vec![root.to_path_buf()];
        while let Some(folder) = folders.pop() {
            let Ok(entries) = fs::read_dir(&folder) else { continue };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else { continue };
                if kind.is_dir() {
                    folders.push(entry.path());
                } else if kind.is_file() && entry.file_name().to_str().and_then(kept_stamp).is_some() {
                    found.push(entry.path());
                }
            }
        }
        // Read order is the operating system's; sorting keeps passes repeatable.
        found.sort();
        for entry in found {
            if self.known.insert(entry.clone()) {
                self.found.push_back(entry);
            }
        }
    }
}

/// The wall and monotonic time now. Tests can move both forward, or the wall clock alone (see [`faults`]).
pub(crate) fn clock() -> (SystemTime, Instant) {
    let (wall_skew, mono_skew) = faults::clock_skew();
    (SystemTime::now() + wall_skew, Instant::now() + mono_skew)
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
            let linked = if hooks.ids_off.get() { None } else { file_id(&fs::symlink_metadata(&backup)) };
            *hooks.made.borrow_mut() = Some(backup.clone());
            Ok(Backup::Held(Held { path: backup, found, linked, by_link: true }))
        }
        // Deleted between the read and the link: nothing to protect.
        Err(e) if e.kind() == io::ErrorKind::NotFound && !path.exists() => Ok(Backup::Absent),
        // The file system has no links (or won't make this one): copy instead.
        Err(e) if links_unsupported(&e) => {
            *hooks.made.borrow_mut() = Some(backup.clone());
            copy_backup(path, &bytes, backup, hooks)
        }
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
fn copy_backup(path: &Path, bytes: &[u8], backup: PathBuf, hooks: &Hooks<'_>) -> Result<Backup, StoreError> {
    let found = ContentHash::of(bytes);
    if let Some(parent) = backup.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
    }
    let file = OpenOptions::new().write(true).create_new(true).open(&backup).map_err(|e| StoreError::io(&backup, e))?;
    // Ours from here, and incomplete until checked: any failure below drops the guard, which removes it. (A copy that
    // can't be removed stays; its bytes don't match the hash in its name, so pruning never takes it for a backup.)
    let copy = Artifact::new(backup);
    let written = if hooks.copy_fails.get() {
        Err(io::Error::other("writing the copy failed (forced by a test)"))
    } else {
        (&file).write_all(bytes).and_then(|()| file.sync_all())
    };
    written.map_err(|e| StoreError::io(copy.path(), e))?;
    let copied = fs::read(copy.path()).map_err(|e| StoreError::io(copy.path(), e))?;
    if ContentHash::of(&copied) != found {
        return Err(StoreError::Internal(format!("the backup of {} didn't read back identical", path.display())));
    }
    Ok(Backup::Held(Held { path: copy.keep(), found, linked: None, by_link: false }))
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
        // After a hard link this drops the second name of the same file; after the rename it is already gone. The
        // file is placed either way, so a failure here is reported as one after the commit.
        match fs::remove_file(from) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(StoreError::NotDurable { path: from.to_path_buf(), source: e }),
        }
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

/// On Windows: `SetFileInformationByHandle` with `FileRenameInfo` and `ReplaceIfExists` false, atomic on every volume.
#[cfg(windows)]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<bool> {
    rename_by_handle(from, to, OnExisting::Keep)
}

/// `FILE_RENAME_FLAG_REPLACE_IF_EXISTS` (`WinBase.h`). `windows-sys` has it only behind a feature the workspace doesn't
/// enable (`Win32_System_WindowsProgramming`), so the value is written here.
#[cfg(windows)]
const FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 0x1;

/// `FILE_RENAME_FLAG_POSIX_SEMANTICS` (`WinBase.h`), likewise.
#[cfg(windows)]
const FILE_RENAME_FLAG_POSIX_SEMANTICS: u32 = 0x2;

/// What [`rename_by_handle`] does with a file already at the new name.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnExisting {
    /// Fail: a no-replace rename (`FileRenameInfo` with `ReplaceIfExists` false), atomic on every volume.
    Keep,
    /// Replace it atomically with POSIX semantics (`FileRenameInfoEx` with `FILE_RENAME_FLAG_REPLACE_IF_EXISTS |
    /// FILE_RENAME_FLAG_POSIX_SEMANTICS`): Windows 10 1607 or later, on NTFS.
    ReplacePosix,
}

/// Renames `from` to `to` with `SetFileInformationByHandle`, the one call in this crate that needs `unsafe` (the
/// Director's exception on #1757). True if renamed; false if `to` exists and `on_existing` is [`OnExisting::Keep`]. A
/// volume or system that doesn't support the requested rename (`ERROR_INVALID_FUNCTION`, `ERROR_NOT_SUPPORTED`,
/// `ERROR_INVALID_PARAMETER`) is [`io::ErrorKind::Unsupported`], and then nothing was moved.
#[cfg(windows)]
#[allow(unsafe_code)]
fn rename_by_handle(from: &Path, to: &Path, on_existing: OnExisting) -> io::Result<bool> {
    use std::mem::offset_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_RENAME_INFO, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FileRenameInfo, FileRenameInfoEx, SetFileInformationByHandle,
    };

    // The buffer below is made of `u64`s, so its alignment must cover the struct's (8, for the handle field).
    const _: () = assert!(align_of::<FILE_RENAME_INFO>() <= align_of::<u64>());

    // A full path, so `RootDirectory` can be null. Kept without a NUL: `FileNameLength` counts bytes and excludes it,
    // and an embedded NUL would cut the name short, so it is refused.
    let name: Vec<u16> = std::path::absolute(to)?.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let name_bytes = name.len().checked_mul(2).ok_or(io::ErrorKind::InvalidInput)?;
    // The header up to `FileName`, the name, and a trailing NUL; never less than the struct itself, whose `FileName`
    // field is declared as one element.
    let size = (offset_of!(FILE_RENAME_INFO, FileName) + name_bytes + 2).max(size_of::<FILE_RENAME_INFO>());
    let size_u32 = u32::try_from(size).map_err(|_| io::ErrorKind::InvalidInput)?;
    let name_bytes = u32::try_from(name_bytes).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut buffer = vec![0u64; size.div_ceil(size_of::<u64>())];

    // The handle needs only DELETE access to rename. The share modes leave other readers and writers alone; a reparse
    // point is renamed itself, never followed; backup semantics lets a folder open too.
    let file = OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(from)?;

    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: buffer size and alignment. `info` points at the start of `buffer`, which is zeroed, aligned for
    // `FILE_RENAME_INFO` (the assertion above) and at least `size` bytes long, which covers the whole struct, so every
    // field written here lies inside it. Fields are written through raw pointers (`&raw mut`), never references, so no
    // reference to a partly written struct exists.
    // SAFETY: the trailing UTF-16 name. It is copied as `name.len()` units starting at `FileName`, so it ends at byte
    // `offset_of!(FileName) + name_bytes`, inside `size`. `name` and `buffer` are separate allocations, so they don't
    // overlap.
    // SAFETY: the NUL. `size` reserves two bytes past the name that nothing writes; they stay zero (the buffer was
    // zeroed) and are the NUL the name ends with, which `FileNameLength` excludes as the API requires. `name` has no
    // NUL of its own (checked above), so the system reads exactly this name.
    unsafe {
        if on_existing == OnExisting::ReplacePosix {
            (&raw mut (*info).Anonymous.Flags)
                .write(FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS);
        }
        // `OnExisting::Keep` leaves `ReplaceIfExists` false and `RootDirectory` null (both zeroed).
        (&raw mut (*info).FileNameLength).write(name_bytes);
        name.as_ptr().copy_to_nonoverlapping((&raw mut (*info).FileName).cast::<u16>(), name.len());
    }
    let class = match on_existing {
        OnExisting::Keep => FileRenameInfo,
        OnExisting::ReplacePosix => FileRenameInfoEx,
    };
    // SAFETY: handle validity and lifetime. `file` was opened above with DELETE access and is dropped only at the end
    // of this function, after the call returns, so the handle is valid for the whole call.
    // SAFETY: the buffer. Its pointer is valid for reads of `size_u32` bytes (`buffer` holds at least `size`, and
    // `size_u32 == size`), aligned for `FILE_RENAME_INFO`, fully initialised, and outlives the call; the system only
    // reads it.
    let renamed = unsafe { SetFileInformationByHandle(file.as_raw_handle(), class, buffer.as_ptr().cast(), size_u32) };
    if renamed != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error().and_then(|code| u32::try_from(code).ok()) {
        Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_INVALID_PARAMETER) => {
            Err(io::ErrorKind::Unsupported.into())
        }
        _ if error.kind() == io::ErrorKind::AlreadyExists && on_existing == OnExisting::Keep => Ok(false),
        _ => Err(error),
    }
}

/// No rename that refuses to replace is reachable on other systems.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
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
/// The move aside is a rename that won't replace (an occupied quarantine name is skipped for the next), so a system
/// without one refuses with [`StoreError::NoSafeChange`] before the target is touched. Putting a file back is a no-replace hard link, or that rename where links fail.
pub(crate) fn remove_checked(
    dir: &Path,
    segs: &[String],
    expected: Option<ContentHash>,
    retention: &mut Retention,
) -> Result<bool, StoreError> {
    let hooks = Hooks::default();
    let outcome = remove_checked_with(dir, segs, expected, &hooks);
    register_backup(&hooks, retention);
    outcome
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
            if !held.by_link {
                hooks.sync_folder(held.path.parent()).map_err(not_durable)?;
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

/// Counts the quarantine names this process has handed out.
static QUARANTINE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh hidden name beside `path` to move it aside to: `.<name>.<pid>-<n>.del`.
fn quarantine_name(path: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".{}-{}.del", std::process::id(), QUARANTINE_COUNTER.fetch_add(1, Ordering::Relaxed)));
    path.with_file_name(name)
}

/// The error for a target that couldn't be looked at when the rescue of the file moved aside from it failed as well:
/// both failures, and where the file is. A rescue that failed before the move leaves it at `quarantine`; one that
/// failed after the move (flushing) names the file's new place.
fn stranded(path: &Path, source: io::Error, quarantine: &Path, rescue: StoreError) -> StoreError {
    let moved_to = match &rescue {
        StoreError::Io { path: to, .. } if fs::symlink_metadata(quarantine).is_err() => Some(to.clone()),
        _ => None,
    };
    StoreError::Stranded {
        path: path.to_path_buf(),
        source,
        kept_at: moved_to.unwrap_or_else(|| quarantine.to_path_buf()),
        rescue: Box::new(rescue),
    }
}

/// Removes the file at `path` if it is the backed-up one; false if another file was there, which is kept.
fn unlink_held(dir: &Path, path: &Path, held: &Held, hooks: &Hooks<'_>) -> Result<bool, StoreError> {
    // The move aside never replaces an occupied quarantine name, and where there is no rename that won't replace,
    // nothing moves: see `move_to_free_slot`.
    let quarantine = match move_to_free_slot(path, &|| Ok(quarantine_name(path)), hooks) {
        Ok(Some(quarantine)) => quarantine,
        Ok(None) => return Ok(true),
        Err(e) => {
            // The move's error is the one returned; a backup left behind still has its hash, so pruning removes it.
            drop(fs::remove_file(&held.path));
            return Err(e);
        }
    };
    (hooks.quarantined)();
    if held.is_file_at(&quarantine) {
        // An editor may have created the file again since it was moved aside. Then that file is theirs and the
        // deletion is a conflict: the file moved aside is kept in the replaced folder, never deleted.
        match hooks.look(path) {
            Ok(_) => {
                preserve(dir, &quarantine, hooks)?;
                return Ok(false);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                // Whether the name is taken is unknown, but the file moved aside must not stay under its hidden name:
                // it is kept. If keeping it fails too, the error also says where the file is.
                return Err(match preserve(dir, &quarantine, hooks) {
                    Ok(_) => StoreError::io(path, e),
                    Err(rescue) => stranded(path, e, &quarantine, rescue),
                });
            }
        }
        // The target is gone from its name: the deletion has happened, and what follows only tidies up.
        // A linked backup is the kept entry (keyed on the link, not on a file identity, which Windows doesn't give):
        // the name moved aside goes.
        let tidied = if held.by_link {
            fs::remove_file(&quarantine)
        } else {
            // The backup is only a copy, so the original inode (which an editor may still hold open) is renamed over
            // it and kept, not unlinked.
            fs::rename(&quarantine, &held.path)
        };
        return tidied.map(|()| true).map_err(|e| StoreError::NotDurable { path: quarantine, source: e });
    }
    // An editor's file: put it back unless something took the name again, and never delete it.
    match place_no_replace(&quarantine, path, hooks) {
        Ok(true) => Ok(false),
        // The name was taken again: that file stays at the target and this one is kept in the replaced folder.
        Ok(false) => {
            preserve(dir, &quarantine, hooks)?;
            Ok(false)
        }
        // The editor's file is back at the target, but its hidden second name beside it couldn't be removed. The
        // deletion did not happen, so this is a plain error, not one after a commit.
        Err(StoreError::NotDurable { path: second, source }) => Err(StoreError::io(
            &second,
            io::Error::new(
                source.kind(),
                format!(
                    "the file that took the place of {} was put back, but this second name of it stayed: {source}",
                    path.display()
                ),
            ),
        )),
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
        // The path below the replaced folder uses the platform's separator.
        let name = items[0].0.rsplit(['/', '\\']).next().unwrap();
        assert!(name.starts_with("b.toml.") && name.ends_with(&format!(".{first}.kept")), "{name}");
        assert!(kept_stamp(name).is_some_and(|(_, hash)| hash == first));
    }

    const MINE: &str = "mine = 1\n";

    fn with_file(dir: &Path) -> (PathBuf, Option<ContentHash>) {
        let path = dir.join("f.toml");
        fs::write(&path, MINE).unwrap();
        (path, Some(ContentHash::of(MINE.as_bytes())))
    }

    /// A handle opened on a file before a call that must leave it alone, for telling afterwards that the path still
    /// names that very file (an equal-content copy would not): a write through the handle must show at the path. Works
    /// where [`file_id`] has no identity (Windows); std opens with all share modes, so the call can still rename.
    struct SameFile(File);

    impl SameFile {
        fn open(path: &Path) -> Self {
            Self(OpenOptions::new().append(true).open(path).unwrap())
        }

        fn assert_still_the_file_at(mut self, path: &Path) {
            use std::io::Write as _;

            let before = fs::read_to_string(path).unwrap();
            self.0.write_all(b"# probe\n").and_then(|()| self.0.sync_all()).unwrap();
            assert_eq!(fs::read_to_string(path).unwrap(), format!("{before}# probe\n"), "not the same file");
        }
    }

    fn no_temporary_file(dir: &Path) -> bool {
        fs::read_dir(dir).unwrap().all(|e| e.unwrap().file_name() != ".f.toml.tmp")
    }

    fn replace_with(dir: &Path, expected: Option<ContentHash>, hooks: &Hooks<'_>) -> Result<Replace, StoreError> {
        replace_checked_with(dir, &dir.join("f.toml"), "ours = 3\n", expected, hooks)
    }

    /// What an editor's atomic save does: a new file renamed over the target.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    /// A displaced file whose flush fails after it was opened is still moved into the replaced folder; the error says
    /// it was kept unflushed.
    #[test]
    fn a_failed_flush_of_a_displaced_file_still_moves_it() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let leftover = dir.join(".f.toml.tmp");
        fs::write(&leftover, "left by a crash = 5\n").unwrap();
        let hooks = Hooks { flush_file_off: true.into(), ..Hooks::default() };
        let err = preserve(dir, &leftover, &hooks).unwrap_err();
        assert!(err.to_string().contains("could not be flushed"), "{err}");
        assert!(!leftover.exists(), "the leftover stayed under its hidden name");
        let items = kept(dir);
        assert_eq!(texts(&items), ["left by a crash = 5\n"]);
        assert!(items[0].0.ends_with(".displaced"), "{items:?}");
    }

    /// A leftover that can't be opened for flushing is still moved into the replaced folder, and the error says it
    /// could not be flushed, never that nothing moved. On Windows a handle that denies write sharing makes the open
    /// fail; elsewhere the flush works and the move is plain.
    #[test]
    fn a_leftover_that_cannot_be_flushed_is_still_moved() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let leftover = dir.join(".f.toml.tmp");
        fs::write(&leftover, "left by a crash = 5\n").unwrap();
        #[cfg(windows)]
        let _editor = {
            use std::os::windows::fs::OpenOptionsExt;
            // Holds it open for writing and lets others read and delete it, but not write: a flush handle can't open.
            OpenOptions::new().write(true).share_mode(1 | 4).open(&leftover).unwrap()
        };
        let out = preserve(dir, &leftover, &Hooks::default());
        if cfg!(windows) {
            let err = out.unwrap_err();
            assert!(err.to_string().contains("could not be flushed"), "{err}");
        } else {
            out.unwrap();
        }
        assert!(!leftover.exists(), "the leftover stayed under its hidden name");
        let items = kept(dir);
        assert_eq!(texts(&items), ["left by a crash = 5\n"]);
        assert!(items[0].0.ends_with(".displaced"), "{items:?}");
    }

    #[test]
    fn a_copied_backup_is_flushed_and_verified() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, _)) = (tmp.path(), with_file(tmp.path()));
        let backup = dir.join(REPLACED_DIR).join("f.toml.copy");
        let bytes = fs::read(&path).unwrap();
        let Backup::Held(held) = copy_backup(&path, &bytes, backup.clone(), &Hooks::default()).unwrap() else {
            panic!("no backup")
        };
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
        assert_eq!(prune_later(dir), 0);
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
            assert_eq!(prune_later(dir), 0);
        }
    }

    /// A file deleted between looking at its path and reading it is vanished, never unchanged; a changed one and a
    /// folder in its place are different.
    #[test]
    fn a_file_deleted_before_it_is_read_has_vanished() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        assert_eq!(file_state_with(&path, expected, &Hooks::default()).unwrap(), FileState::Same);
        let delete = || fs::remove_file(dir.join("f.toml")).unwrap();
        let hooks = Hooks { stat_done: &delete, ..Hooks::default() };
        assert_eq!(file_state_with(&path, expected, &hooks).unwrap(), FileState::Vanished);
        assert_eq!(file_state_with(&path, expected, &Hooks::default()).unwrap(), FileState::Vanished);
        fs::write(&path, "other = 1\n").unwrap();
        assert_eq!(file_state_with(&path, expected, &Hooks::default()).unwrap(), FileState::Different);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(file_state_with(&path, expected, &Hooks::default()).unwrap(), FileState::Different);
    }

    /// A link swapped in after the path was looked at is never read through: the file opened without following links
    /// is the link, so it is different, even though what it points to has the expected bytes.
    #[cfg(unix)]
    #[test]
    fn a_link_swapped_in_before_the_read_is_different() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        write(dir, "elsewhere.toml", MINE);
        let swap = || {
            fs::remove_file(dir.join("f.toml")).unwrap();
            std::os::unix::fs::symlink(dir.join("elsewhere.toml"), dir.join("f.toml")).unwrap();
        };
        let hooks = Hooks { stat_done: &swap, ..Hooks::default() };
        assert_eq!(file_state_with(&path, expected, &hooks).unwrap(), FileState::Different);
    }

    /// Every path that places our file over an existing one keeps the original as the `.kept` entry: the swap with a
    /// linked backup, the swap with a copied backup, and Windows's rename over the target (driven here on any system).
    #[test]
    fn every_successful_replacement_keeps_the_backup() {
        let exchange = cfg!(any(target_os = "linux", target_os = "macos"));
        let cases = [
            ("swap, linked backup", exchange, Hooks::default()),
            ("swap, linked backup, no file identities", exchange, Hooks { ids_off: true.into(), ..Hooks::default() }),
            ("swap, copied backup", exchange, Hooks { links_off: true.into(), ..Hooks::default() }),
            (
                "Windows-shaped",
                true,
                Hooks { exchange_off: true.into(), windows_shaped: true.into(), ..Hooks::default() },
            ),
        ];
        for (name, reachable, hooks) in cases {
            if !reachable {
                continue;
            }
            let tmp = TempDir::new().unwrap();
            let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
            assert_eq!(replace_with(dir, expected, &hooks).unwrap(), Replace::Placed, "{name}");
            assert_eq!(fs::read_to_string(&path).unwrap(), "ours = 3\n", "{name}");
            let items = kept(dir);
            assert_eq!(texts(&items), [MINE], "{name}");
            assert!(items[0].0.ends_with(KEPT), "{name}: {items:?}");
            assert!(no_temporary_file(dir), "{name}");
        }
    }

    /// With a copied backup, a failure to flush the folder after the original takes the copy's place is reported, not
    /// dropped: the new text is at the target, but the save no longer promises it is durable. The original is kept.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_failed_flush_after_a_copied_backup_is_reported() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let hooks = Hooks { links_off: true.into(), sync_off: true.into(), ..Hooks::default() };
        let err = replace_with(dir, expected, &hooks).unwrap_err();
        assert!(matches!(&err, StoreError::NotDurable { .. }) && err.to_string().contains("flushing"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "ours = 3\n");
        assert_eq!(texts(&kept(dir)), [MINE]);
        assert!(no_temporary_file(dir));
    }

    /// Matrix, no exchange (every combination of the others): replacing is refused before anything moves.
    #[test]
    fn without_exchange_a_replacement_is_refused_and_the_file_is_untouched() {
        // Windows's rename over the target is turned off too (its own tests are below).
        for (links_off, no_replace_off) in [(false, false), (false, true), (true, false), (true, true)] {
            let tmp = TempDir::new().unwrap();
            let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
            let probe = SameFile::open(&path);
            let hooks = Hooks {
                links_off: links_off.into(),
                exchange_off: true.into(),
                no_replace_off: no_replace_off.into(),
                posix_rename_off: true.into(),
                ..Hooks::default()
            };
            let err = replace_with(dir, expected, &hooks).unwrap_err();
            // On Windows with links the refusal names the missing POSIX-semantics rename; elsewhere the exchange.
            let missing = if cfg!(windows) && !links_off { NO_POSIX_RENAME } else { NO_EXCHANGE };
            assert!(matches!(err, StoreError::NoSafeChange { missing: m, .. } if m == missing), "{err}");
            assert_eq!(fs::read_to_string(&path).unwrap(), MINE);
            probe.assert_still_the_file_at(&path);
            assert_eq!(kept(dir), [], "links off: {links_off}, no-replace off: {no_replace_off}");
            assert!(no_temporary_file(dir));
        }
    }

    /// Matrix, Windows with links and no exchange: the original is kept by hard link and stays reachable through it up
    /// to the commit (checked just before the rename, with our file still staged); the POSIX-semantics rename then puts
    /// our file at the target, and the backup is the kept entry.
    #[cfg(windows)]
    #[test]
    fn on_windows_a_replacement_is_a_posix_rename_over_the_target_after_a_linked_backup() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let checked = Cell::new(false);
        let before_commit = || {
            assert_eq!(fs::read_to_string(dir.join("f.toml")).unwrap(), MINE);
            assert_eq!(texts(&kept(dir)), [MINE]);
            assert!(!no_temporary_file(dir));
            checked.set(true);
        };
        let hooks = Hooks { backed_up: &before_commit, ..Hooks::default() };
        assert_eq!(replace_with(dir, expected, &hooks).unwrap(), Replace::Placed);
        assert!(checked.get());
        assert_eq!(fs::read_to_string(&path).unwrap(), "ours = 3\n");
        let items = kept(dir);
        assert_eq!(texts(&items), [MINE]);
        assert!(items[0].0.ends_with(KEPT), "{items:?}");
        assert!(no_temporary_file(dir));
    }

    /// Matrix, a volume without a POSIX-semantics rename (forced through `Hooks`, driven Windows-shaped on any system):
    /// refused before anything moves, naming the missing capability.
    #[test]
    fn without_a_posix_rename_a_windows_replacement_is_refused_before_anything_moves() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let probe = SameFile::open(&path);
        let hooks = Hooks {
            exchange_off: true.into(),
            windows_shaped: true.into(),
            posix_rename_off: true.into(),
            ..Hooks::default()
        };
        let err = replace_with(dir, expected, &hooks).unwrap_err();
        assert!(matches!(err, StoreError::NoSafeChange { missing: NO_POSIX_RENAME, .. }), "{err}");
        assert!(err.to_string().contains("POSIX"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), MINE);
        probe.assert_still_the_file_at(&path);
        assert_eq!(kept(dir), []);
        assert!(no_temporary_file(dir));
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
        let (out, text) = new_file(true, false);
        assert_eq!((out.unwrap(), text.as_deref()), (Replace::Placed, Some("ours = 3\n")));
        let (out, text) = new_file(true, true);
        let err = out.unwrap_err();
        assert!(matches!(err, StoreError::NoSafeChange { missing: NO_NO_REPLACE, .. }), "{err}");
        assert_eq!(text, None);
    }

    /// Matrix, a deletion with links but no no-replace rename: refused before the file moves.
    #[test]
    fn with_links_but_no_no_replace_rename_a_deletion_is_refused() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let hooks = Hooks { exchange_off: true.into(), no_replace_off: true.into(), ..Hooks::default() };
        let err = remove_checked_with(d, &["a".to_owned()], Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap_err();
        assert!(matches!(err, StoreError::NoSafeChange { missing: NO_NO_REPLACE, .. }), "{err}");
        assert_eq!(fs::read_to_string(d.join("a.toml")).unwrap(), "x = 1\n");
        assert_eq!(kept(d), []);
        assert_eq!(all_names(d), [".omuzaki", "a.toml"]);
    }

    /// An occupied slot (here a leftover with the same pid and counter) is never replaced: the move takes the next
    /// name.
    #[test]
    fn a_move_into_a_slot_skips_an_occupied_name_and_keeps_it_intact() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        write(d, ".a.toml.7-0.del", "leftover = 1\n");
        let n = Cell::new(0);
        let slot = || {
            let name = d.join(format!(".a.toml.7-{}.del", n.get()));
            n.set(n.get() + 1);
            Ok(name)
        };
        let moved = move_to_free_slot(&d.join("a.toml"), &slot, &Hooks::default()).unwrap();
        assert_eq!(moved, Some(d.join(".a.toml.7-1.del")));
        assert_eq!(fs::read_to_string(d.join(".a.toml.7-0.del")).unwrap(), "leftover = 1\n");
        assert_eq!(fs::read_to_string(d.join(".a.toml.7-1.del")).unwrap(), "x = 1\n");
        assert!(!d.join("a.toml").exists());
        // A source that is gone is reported as gone, not as an error.
        assert_eq!(move_to_free_slot(&d.join("a.toml"), &slot, &Hooks::default()).unwrap(), None);
    }

    /// The same through a deletion: leftovers holding this process's next quarantine names stay intact.
    #[test]
    fn a_deletion_never_replaces_a_leftover_quarantine_file() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let start = QUARANTINE_COUNTER.load(Ordering::Relaxed);
        let leftovers: Vec<String> =
            (start..start + 64).map(|n| format!(".a.toml.{}-{n}.del", std::process::id())).collect();
        for name in &leftovers {
            write(d, name, "leftover = 1\n");
        }
        assert!(remove_checked(d, &["a".to_owned()], Some(ContentHash::of(b"x = 1\n"))).unwrap());
        assert!(!d.join("a.toml").exists());
        for name in &leftovers {
            assert_eq!(fs::read_to_string(d.join(name)).unwrap(), "leftover = 1\n", "{name}");
        }
        assert_eq!(texts(&kept(d)), ["x = 1\n"]);
    }

    /// Matrix, a deletion with neither links nor a no-replace rename: refused before the file moves.
    #[test]
    fn without_links_or_a_no_replace_rename_a_deletion_is_refused_and_the_file_is_untouched() {
        for exchange_off in [false, true] {
            let tmp = TempDir::new().unwrap();
            let d = tmp.path();
            write(d, "a.toml", "x = 1\n");
            let probe = SameFile::open(&d.join("a.toml"));
            let hooks = Hooks {
                links_off: true.into(),
                exchange_off: exchange_off.into(),
                no_replace_off: true.into(),
                ..Hooks::default()
            };
            let err = remove_checked_with(d, &["a".to_owned()], Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap_err();
            assert!(matches!(err, StoreError::NoSafeChange { missing: NO_NO_REPLACE, .. }), "{err}");
            assert_eq!(fs::read_to_string(d.join("a.toml")).unwrap(), "x = 1\n");
            probe.assert_still_the_file_at(&d.join("a.toml"));
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

    /// Just past the retention time.
    fn past() -> Duration {
        RETAIN_UNCHANGED + Duration::from_secs(1)
    }

    /// Prunes `dir` with a fresh retention that finds its entries now, then once more with both clocks just past the
    /// retention time; returns how many the second pass deleted.
    fn prune_later(dir: &Path) -> usize {
        let (wall, mono) = (SystemTime::now(), Instant::now());
        let mut retention = Retention::default();
        assert_eq!(retention.prune(dir, wall, mono), 0, "nothing is old yet");
        retention.prune(dir, wall + past(), mono + past())
    }

    /// The public functions, with a retention nothing reads.
    fn replace_checked(
        dir: &Path,
        path: &Path,
        text: &str,
        expected: Option<ContentHash>,
    ) -> Result<Replace, StoreError> {
        super::replace_checked(dir, path, text, expected, &mut Retention::default())
    }

    fn remove_checked(dir: &Path, segs: &[String], expected: Option<ContentHash>) -> Result<bool, StoreError> {
        super::remove_checked(dir, segs, expected, &mut Retention::default())
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

        let (wall, mono) = (SystemTime::now(), Instant::now());
        let mut retention = Retention::default();
        // Young entries are kept, however unchanged.
        assert_eq!(retention.prune(dir, wall, mono), 0);
        assert_eq!(kept(dir).len(), 5);
        // Old and unchanged go; changed, raced and anything else stay.
        assert_eq!(retention.prune(dir, wall + past(), mono + past()), 2);
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
        assert_eq!(retention.prune(dir, wall + past(), mono + past()), 0);
        assert_eq!(prune_later(&dir.join("deep")), 0);
    }

    /// Without entries created since the last pass, one pass deletes its catch-up share and the next the rest.
    #[test]
    fn a_pass_without_new_entries_deletes_its_catch_up_share() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        for _ in 0..PRUNE_CATCH_UP + 6 {
            replace_once(dir, "a.toml");
            fs::write(dir.join("a.toml"), MINE).unwrap();
        }
        let (wall, mono) = (SystemTime::now(), Instant::now());
        let mut retention = Retention::default();
        assert_eq!(retention.prune(dir, wall, mono), 0);
        assert_eq!(retention.prune(dir, wall + past(), mono + past()), PRUNE_CATCH_UP);
        assert_eq!(kept(dir).len(), 6);
        assert_eq!(retention.prune(dir, wall + past(), mono + past()), 6);
        assert_eq!(kept(dir), []);
    }

    /// Entries created since the last pass raise its budget, so pruning keeps pace however many a burst made.
    #[test]
    fn a_pass_deletes_at_least_what_was_created_since_the_last() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut retention = Retention::default();
        let created = 3 * PRUNE_CATCH_UP;
        for _ in 0..created {
            let path = dir.join("a.toml");
            if !path.exists() {
                write(dir, "a.toml", MINE);
            }
            let expected = Some(ContentHash::of(&fs::read(&path).unwrap()));
            super::replace_checked(dir, &path, "ours = 3\n", expected, &mut retention).unwrap();
            fs::write(&path, MINE).unwrap();
        }
        assert_eq!(kept(dir).len(), created);
        let (wall, mono) = (SystemTime::now(), Instant::now());
        assert_eq!(retention.prune(dir, wall + past(), mono + past()), created);
        assert_eq!(kept(dir), []);
    }

    /// For an entry this process made, the wall clock alone never makes it old: a forward jump doesn't, and neither
    /// does an old wall time while the monotonic age is short.
    #[test]
    fn a_wall_clock_jump_never_prunes_an_entry_this_process_made_early() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write(dir, "a.toml", MINE);
        let mut retention = Retention::default();
        let path = dir.join("a.toml");
        super::replace_checked(dir, &path, "ours = 3\n", Some(ContentHash::of(MINE.as_bytes())), &mut retention)
            .unwrap();
        let (wall, mono) = (SystemTime::now(), Instant::now());
        let jumped = wall + Duration::from_hours(24);
        // The clock jumped a day forward, but no time has passed.
        assert_eq!(retention.prune(dir, jumped, mono), 0);
        assert_eq!(retention.prune(dir, jumped, mono + RETAIN_UNCHANGED / 2), 0);
        // Enough time has passed on the monotonic clock, but the wall clock went back.
        assert_eq!(retention.prune(dir, wall - Duration::from_mins(1), mono + past()), 0);
        assert_eq!(kept(dir).len(), 1);
        // Old on both.
        assert_eq!(retention.prune(dir, wall + past(), mono + past()), 1);
        assert_eq!(kept(dir), []);
    }

    /// An entry an earlier process left keeps its real age: a short-lived process that kept it 11 minutes ago has its
    /// entry pruned by the next process's first pass. One stamped in the future is age 0 until the clock passes it.
    #[test]
    fn an_entry_an_earlier_process_left_is_aged_by_its_name() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let wall = SystemTime::now();
        let stamp = |at: SystemTime| at.duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let hash = ContentHash::of(b"old = 1\n");
        let eleven_ago = wall - Duration::from_mins(11);
        write(dir, &format!(".omuzaki/replaced/a.toml.{}-7-0.{hash}.kept", stamp(eleven_ago)), "old = 1\n");
        let ahead = wall + Duration::from_hours(1);
        write(dir, &format!(".omuzaki/replaced/b.toml.{}-7-1.{hash}.kept", stamp(ahead)), "old = 1\n");
        let mut retention = Retention::default();
        assert_eq!(retention.prune(dir, wall, Instant::now()), 1);
        let left = kept(dir);
        assert_eq!(left.len(), 1);
        assert!(left[0].0.starts_with("b.toml."), "{left:?}");
        // Once the clock is past its time by the retention, the one from the future goes too.
        assert_eq!(retention.prune(dir, ahead + past(), Instant::now()), 1);
        assert_eq!(kept(dir), []);
    }

    /// A save that fails after staging (here the target is a folder, which can't be read as a file) leaves no
    /// temporary file: the guard removes it.
    #[test]
    fn a_failure_after_staging_leaves_no_temporary_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        fs::create_dir(dir.join("f.toml")).unwrap();
        let err = replace_with(dir, Some(ContentHash::of(MINE.as_bytes())), &Hooks::default()).unwrap_err();
        assert!(matches!(err, StoreError::Io { .. }), "{err}");
        assert!(no_temporary_file(dir));
        assert!(dir.join("f.toml").is_dir());
    }

    /// A copied backup whose write fails after the file was created leaves no `.kept` entry: the guard removes it.
    #[test]
    fn a_copy_that_fails_after_it_was_created_leaves_no_kept_entry() {
        let tmp = TempDir::new().unwrap();
        let (dir, (path, expected)) = (tmp.path(), with_file(tmp.path()));
        let hooks = Hooks { links_off: true.into(), copy_fails: true.into(), ..Hooks::default() };
        let err = replace_with(dir, expected, &hooks).unwrap_err();
        assert!(err.to_string().contains("forced by a test"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), MINE);
        assert_eq!(kept(dir), []);
        assert!(no_temporary_file(dir));
    }

    #[test]
    fn only_kept_names_carry_a_time_and_a_hash() {
        let hash = ContentHash::of(b"x");
        assert_eq!(kept_stamp(&format!("f.toml.1500-7-0.{hash}.kept")), Some((Duration::from_nanos(1500), hash)));
        for name in ["f.toml.1500-7-0.displaced", "f.toml.1500-7-0.nothex.kept", &format!("f.toml.x-7-0.{hash}.kept")] {
            assert_eq!(kept_stamp(name), None, "{name}");
        }
    }

    #[test]
    fn the_no_replace_rename_refuses_an_existing_target_and_leaves_both_files() {
        let tmp = TempDir::new().unwrap();
        let (dir, path) = (tmp.path(), tmp.path().join("f.toml"));
        let from = dir.join(".f.toml.tmp");
        fs::write(&from, "ours = 3\n").unwrap();
        fs::write(&path, "theirs = 1\n").unwrap();
        // The system makes the check and the rename one step, so there is no window for a file to be lost in.
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

    /// A deletion with a hard-linked backup on a system without file identities (Windows, forced here on any system)
    /// keeps the backup as the kept entry and leaves no hidden name behind.
    #[test]
    fn a_linked_deletion_without_file_identities_leaves_no_hidden_name() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let hooks = Hooks { ids_off: true.into(), ..Hooks::default() };
        assert!(remove_checked_with(d, &["a".to_owned()], Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap());
        assert!(!d.join("a.toml").exists());
        let items = kept(d);
        assert_eq!(texts(&items), ["x = 1\n"]);
        assert!(items[0].0.ends_with(KEPT), "{items:?}");
        assert_eq!(all_names(d), [".omuzaki"]);
    }

    /// A file created again at the target after our file was moved aside makes the deletion a conflict: the new file
    /// stays, the file moved aside is kept (not deleted), and nothing is left under a hidden name.
    #[test]
    fn a_target_recreated_after_the_move_aside_makes_the_deletion_a_conflict() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let recreate = || fs::write(d.join("a.toml"), "theirs = 2\n").unwrap();
        let hooks = Hooks { quarantined: &recreate, ..Hooks::default() };
        assert!(!remove_checked_with(d, &["a".to_owned()], Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap());
        assert_eq!(fs::read_to_string(d.join("a.toml")).unwrap(), "theirs = 2\n");
        let items = kept(d);
        assert!(items.iter().any(|(n, t)| n.ends_with(".displaced") && t == "x = 1\n"), "{items:?}");
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

    /// A target that cannot be looked at after the move aside (an error other than "not found") returns that error, but
    /// the file moved aside is kept in the replaced folder with its bytes, never left under its hidden `.del` name.
    #[test]
    fn a_failed_look_at_the_target_after_the_move_aside_keeps_the_original_and_returns_the_error() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let hooks = Hooks::default();
        hooks.look_off.set(true); // the one place that looks at the target is right after the move aside
        let err = remove_checked_with(d, &["a".to_owned()], Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap_err();
        assert!(err.to_string().contains("forced by a test"), "the target's error is the one returned: {err}");
        let items = kept(d);
        assert!(items.iter().any(|(n, t)| n.ends_with(".displaced") && t == "x = 1\n"), "{items:?}");
        let names: Vec<_> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(names.iter().all(|n| !n.to_string_lossy().ends_with(".del")), "{names:?}");
    }

    /// When the target can't be looked at after the move aside and the file can't be moved into the replaced folder
    /// either, the error carries both failures and names the hidden file, which is still whole there.
    #[test]
    fn a_failed_look_and_a_failed_rescue_report_where_the_file_is() {
        let tmp = TempDir::new().unwrap();
        let d = tmp.path();
        write(d, "a.toml", "x = 1\n");
        let hooks = Hooks::default();
        hooks.look_off.set(true);
        hooks.rename_fails_after.set(Some(1)); // the move aside succeeds; the move into the replaced folder fails
        let err = remove_checked_with(d, &["a".to_owned()], Some(ContentHash::of(b"x = 1\n")), &hooks).unwrap_err();
        let StoreError::Stranded { kept_at, rescue, .. } = &err else { panic!("{err}") };
        assert!(kept_at.to_string_lossy().ends_with(".del"), "{kept_at:?}");
        assert_eq!(fs::read_to_string(kept_at).unwrap(), "x = 1\n", "the original is whole where the error says");
        assert!(rescue.to_string().contains("forced by a test"), "{rescue}");
        let text = err.to_string();
        assert!(text.contains(&kept_at.display().to_string()) && text.contains("looking at the file failed"), "{text}");
    }
}
