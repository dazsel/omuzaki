//! An in-memory [`Disk`] that fails on cue and forgets what was never synced, as a killed process would.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use omuzaki_update_api::disk::{DirEntry, Disk, LinkKind};

use crate::locked;

/// The operations [`FakeDisk::fail_after`] counts.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// [`Disk::write`].
    Write,
    /// [`Disk::rename`].
    Rename,
    /// [`Disk::fsync`].
    Fsync,
    /// A write, a rename or an fsync, counted together.
    Any,
}

/// One node of the tree, as [`FakeDisk::snapshot`] shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A folder.
    Dir,
    /// A file.
    File {
        /// Its bytes.
        bytes: Vec<u8>,
        /// Whether the executable bit is set.
        executable: bool,
    },
    /// A symbolic link to `target`.
    Link {
        /// What it points at, as given.
        target: String,
        /// What the caller said the target is.
        kind: LinkKind,
    },
}

#[derive(Clone, Debug)]
enum Node {
    Dir,
    File { bytes: Vec<u8>, executable: bool, synced: bool },
    Link { target: String, kind: LinkKind },
}

struct Failure {
    op: Op,
    /// Operations of `op` still allowed before the failure; 0 means it has started.
    remaining: u64,
}

struct Inner {
    nodes: BTreeMap<PathBuf, Node>,
    writes: u64,
    renames: u64,
    fsyncs: u64,
    failure: Option<Failure>,
    free: u64,
}

/// A tree of folders, files and links in memory. Paths must be absolute; the root always exists.
///
/// Errors carry the kinds a unix disk gives (`IsADirectory`, `NotADirectory`, `DirectoryNotEmpty`, ...). One rule is
/// stricter on purpose: [`Disk::rename`] never replaces a folder, empty or not, because Windows cannot.
///
/// Links are followed as on a real disk: a path through a [`LinkKind::Dir`] link reaches the target's contents, a link
/// that is relative starts from its own folder, and a chain of more than 40 links is a loop and fails. A
/// [`LinkKind::File`] link cannot be walked through (`NotADirectory`, as on Windows, where the kind is fixed when the
/// link is made), and a link to nothing is `NotFound`. Reads, writes, `fsync` and `list_dir` follow a link at the end
/// of the path; `rename`, `remove_dir_all`, `exists` and a new `symlink` act on the link itself.
///
/// A file is durable only after [`Disk::fsync`]: [`FakeDisk::revive`], the next boot after a kill, empties every file
/// that was written and never synced, which is how a missing `fsync` shows up in a test.
pub struct FakeDisk {
    inner: Mutex<Inner>,
}

impl Default for FakeDisk {
    fn default() -> Self {
        Self::new()
    }
}

fn killed() -> io::Error {
    io::Error::other("injected failure: the process was killed")
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{} not found", path.display()))
}

fn exists_already(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists", path.display()))
}

fn is_a_folder(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::IsADirectory, format!("{} is a folder", path.display()))
}

/// What the real disk answers when the root folder is renamed or removed (`EBUSY`).
fn root_busy(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::ResourceBusy, format!("{} is the root folder", path.display()))
}

fn not_a_folder(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotADirectory, format!("{} is not a folder", path.display()))
}

/// A link that `resolve` should have followed. Reported, not assumed away, in case a walk ever misses one.
fn unresolved(path: &Path) -> io::Error {
    io::Error::other(format!("{} is a link the fake disk failed to follow", path.display()))
}

/// `ELOOP`; `ErrorKind::FilesystemLoop` is not stable, so the kind is `Other` and the text says what happened.
fn loop_error(path: &Path) -> io::Error {
    io::Error::other(format!("too many levels of symbolic links at {}", path.display()))
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why.to_owned())
}

/// How many links one path may pass through before it counts as a loop (the limit Linux uses).
const MAX_LINK_HOPS: u32 = 40;

/// One piece of a path being resolved.
enum Step {
    /// Start again from this root: an absolute path, or an absolute link target.
    Reset(PathBuf),
    /// `..`
    Parent,
    /// A name.
    Name(OsString),
}

fn steps(path: &Path) -> VecDeque<Step> {
    let mut root = PathBuf::new();
    let mut out = VecDeque::new();
    for part in path.components() {
        match part {
            Component::Prefix(_) => root.push(part.as_os_str()),
            Component::RootDir => {
                root.push(part.as_os_str());
                out.push_back(Step::Reset(std::mem::take(&mut root)));
            }
            Component::CurDir => {}
            Component::ParentDir => out.push_back(Step::Parent),
            Component::Normal(name) => out.push_back(Step::Name(name.to_os_string())),
        }
    }
    out
}

impl Inner {
    /// Refuses once a failure has started, for every operation, reads included.
    fn alive(&self) -> io::Result<()> {
        match &self.failure {
            Some(f) if f.remaining == 0 => Err(killed()),
            _ => Ok(()),
        }
    }

    /// Counts one mutating `op` and fails it when the armed failure says so.
    fn tick(&mut self, op: Op) -> io::Result<()> {
        self.alive()?;
        match op {
            Op::Write => self.writes += 1,
            Op::Rename => self.renames += 1,
            Op::Fsync => self.fsyncs += 1,
            Op::Any => {}
        }
        if let Some(f) = &mut self.failure
            && (f.op == Op::Any || f.op == op)
        {
            f.remaining -= 1;
            if f.remaining == 0 {
                return Err(killed());
            }
        }
        Ok(())
    }

    /// The root folder is implicit: it is not in `nodes`, and nothing may replace, move or remove it.
    fn is_root(path: &Path) -> bool {
        path.has_root() && path.parent().is_none()
    }

    fn is_dir(&self, path: &Path) -> bool {
        Self::is_root(path) || matches!(self.nodes.get(path), Some(Node::Dir))
    }

    /// Resolves `path` to the name it has in `nodes`: every link on the way is followed, as a real disk does, so
    /// `link/child` is `real/child` when `link` points at `real`. The last component is followed only with
    /// `follow_last` (a read, a write) and kept as the link itself otherwise (a rename, a new link). With `create`,
    /// folders that are missing on the way are returned too, in order, for `create_dir_all` to make.
    ///
    /// A file, or a link marked [`LinkKind::File`], above the last component is `NotADirectory`; a missing one, or a
    /// link to nothing, is `NotFound`; more than [`MAX_LINK_HOPS`] links in one path is a loop.
    fn resolve(&self, path: &Path, follow_last: bool, create: bool) -> io::Result<(PathBuf, Vec<PathBuf>)> {
        if !path.is_absolute() {
            return Err(invalid("path is not absolute"));
        }
        let mut queue = steps(path);
        let (mut found, mut missing, mut hops) = (PathBuf::new(), Vec::new(), 0_u32);
        while let Some(step) = queue.pop_front() {
            let name = match step {
                Step::Reset(root) => {
                    found = root;
                    continue;
                }
                Step::Parent => {
                    if !Self::is_root(&found) {
                        found.pop();
                    }
                    continue;
                }
                Step::Name(name) => name,
            };
            let here = found.join(name);
            let last = queue.is_empty();
            match self.nodes.get(&here) {
                Some(Node::Link { .. }) if last && !follow_last => found = here,
                Some(Node::Link { target, kind }) => {
                    hops += 1;
                    if hops > MAX_LINK_HOPS {
                        return Err(loop_error(path));
                    }
                    if !last && !matches!(kind, LinkKind::Dir) {
                        // A file link cannot be walked through; a dangling one is still "not found", as on a real disk.
                        let (goes_to, _) = self.resolve(&found.join(target), true, false)?;
                        return Err(if self.is_dir(&goes_to) || self.nodes.contains_key(&goes_to) {
                            not_a_folder(&here)
                        } else {
                            not_found(&goes_to)
                        });
                    }
                    // The target is relative to the link's own folder (`found`) unless it starts at the root.
                    for step in steps(Path::new(target)).into_iter().rev() {
                        queue.push_front(step);
                    }
                }
                Some(Node::File { .. }) if !last => return Err(not_a_folder(&here)),
                Some(_) => found = here,
                None if last || create => {
                    if create && !Self::is_root(&here) {
                        missing.push(here.clone());
                    }
                    found = here;
                }
                None => return Err(not_found(&here)),
            }
        }
        Ok((found, missing))
    }

    fn descendants(&self, path: &Path) -> Vec<PathBuf> {
        self.nodes.keys().filter(|k| k.starts_with(path) && *k != path).cloned().collect()
    }
}

impl FakeDisk {
    /// An empty disk with unlimited free space.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                nodes: BTreeMap::new(),
                writes: 0,
                renames: 0,
                fsyncs: 0,
                failure: None,
                free: u64::MAX,
            }),
        }
    }

    /// Arms a failure: the `n`th operation of kind `op` (counting from now) fails, and so does every call after it,
    /// reads too, as when the process was killed there. `n == 0` fails the next call of any kind at once.
    pub fn fail_after(&self, n: u64, op: Op) {
        locked(&self.inner).failure = Some(Failure { op, remaining: n });
    }

    /// The next boot: lifts the failure and empties every file that was written but never synced.
    pub fn revive(&self) {
        let mut inner = locked(&self.inner);
        inner.failure = None;
        for node in inner.nodes.values_mut() {
            if let Node::File { bytes, synced: false, .. } = node {
                bytes.clear();
            }
        }
    }

    /// How many calls of kind `op` were made. `Op::Any` is the sum.
    #[must_use]
    pub fn count(&self, op: Op) -> u64 {
        let inner = locked(&self.inner);
        match op {
            Op::Write => inner.writes,
            Op::Rename => inner.renames,
            Op::Fsync => inner.fsyncs,
            Op::Any => inner.writes + inner.renames + inner.fsyncs,
        }
    }

    /// What [`Disk::free_bytes`] reports from now on.
    pub fn set_free_bytes(&self, bytes: u64) {
        locked(&self.inner).free = bytes;
    }

    /// Every node, sorted by path.
    #[must_use]
    pub fn snapshot(&self) -> BTreeMap<PathBuf, Entry> {
        let inner = locked(&self.inner);
        inner
            .nodes
            .iter()
            .map(|(path, node)| {
                let entry = match node {
                    Node::Dir => Entry::Dir,
                    Node::File { bytes, executable, .. } => {
                        Entry::File { bytes: bytes.clone(), executable: *executable }
                    }
                    Node::Link { target, kind } => Entry::Link { target: target.clone(), kind: *kind },
                };
                (path.clone(), entry)
            })
            .collect()
    }
}

impl Disk for FakeDisk {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let inner = locked(&self.inner);
        inner.alive()?;
        let (path, _) = inner.resolve(path, true, false)?;
        match inner.nodes.get(&path) {
            Some(Node::File { bytes, .. }) => Ok(bytes.clone()),
            Some(Node::Dir) => Err(is_a_folder(&path)),
            None if Inner::is_root(&path) => Err(is_a_folder(&path)),
            Some(Node::Link { .. }) => Err(unresolved(&path)),
            None => Err(not_found(&path)),
        }
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.tick(Op::Write)?;
        let (path, _) = inner.resolve(path, true, false)?;
        if Inner::is_root(&path) {
            return Err(is_a_folder(&path));
        }
        match inner.nodes.get_mut(&path) {
            Some(Node::File { bytes: old, synced, .. }) => {
                bytes.clone_into(old);
                *synced = false;
            }
            Some(Node::Dir) => return Err(is_a_folder(&path)),
            Some(Node::Link { .. }) => return Err(unresolved(&path)),
            None => {
                inner.nodes.insert(path, Node::File { bytes: bytes.to_vec(), executable: false, synced: false });
            }
        }
        Ok(())
    }

    fn fsync(&self, path: &Path) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.tick(Op::Fsync)?;
        let (path, _) = inner.resolve(path, true, false)?;
        if let Some(Node::File { synced, .. }) = inner.nodes.get_mut(&path) {
            *synced = true;
            return Ok(());
        }
        if inner.is_dir(&path) { Ok(()) } else { Err(not_found(&path)) }
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.tick(Op::Rename)?;
        // A rename moves the link itself, never what it points at.
        let (from, _) = inner.resolve(from, false, false)?;
        let (to, _) = inner.resolve(to, false, false)?;
        let (from, to) = (from.as_path(), to.as_path());
        if Inner::is_root(from) {
            return Err(root_busy(from));
        }
        if Inner::is_root(to) {
            return Err(root_busy(to));
        }
        if !inner.nodes.contains_key(from) {
            return Err(not_found(from));
        }
        // Renaming a path onto itself changes nothing, for a file and for a folder with contents alike.
        if from == to {
            return Ok(());
        }
        if to.starts_with(from) {
            return Err(invalid("cannot move a folder into itself"));
        }
        let from_is_dir = matches!(inner.nodes.get(from), Some(Node::Dir));
        // Only a file or a link is replaced. A folder target is always refused, even an empty one that a unix `rename`
        // would replace, because Windows cannot; code that passes here must work on both.
        match inner.nodes.get(to) {
            Some(Node::Dir) if !from_is_dir => return Err(is_a_folder(to)),
            Some(Node::Dir) if !inner.descendants(to).is_empty() => {
                return Err(io::Error::new(io::ErrorKind::DirectoryNotEmpty, format!("{} is not empty", to.display())));
            }
            Some(Node::Dir) => return Err(exists_already(to)),
            Some(Node::File { .. } | Node::Link { .. }) if from_is_dir => return Err(not_a_folder(to)),
            _ => {}
        }
        // Every new name is worked out before anything moves, so a failure cannot leave half a folder behind.
        let moves = inner
            .nodes
            .iter()
            .filter(|(old, _)| old.starts_with(from))
            .map(|(old, node)| {
                let rest = old.strip_prefix(from).map_err(|e| invalid(&e.to_string()))?;
                Ok((old.clone(), to.join(rest), node.clone()))
            })
            .collect::<io::Result<Vec<_>>>()?;
        for (old, new, node) in moves {
            inner.nodes.remove(&old);
            inner.nodes.insert(new, node);
        }
        Ok(())
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.alive()?;
        let (found, missing) = inner.resolve(path, false, true)?;
        if matches!(inner.nodes.get(&found), Some(Node::Link { .. })) {
            // Through a link to a folder it already exists; a link to a file, or to nothing, is in the way.
            let (target, _) = inner.resolve(&found, true, false).map_err(|_| exists_already(&found))?;
            return if inner.is_dir(&target) { Ok(()) } else { Err(exists_already(&found)) };
        }
        if matches!(inner.nodes.get(&found), Some(Node::File { .. })) {
            return Err(exists_already(&found));
        }
        for dir in missing {
            inner.nodes.insert(dir, Node::Dir);
        }
        Ok(())
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.alive()?;
        let (path, _) = inner.resolve(path, false, false)?;
        if Inner::is_root(&path) {
            return Err(root_busy(&path));
        }
        match inner.nodes.get(&path) {
            Some(Node::Dir) => {}
            Some(_) => return Err(not_a_folder(&path)),
            None => return Err(not_found(&path)),
        }
        for gone in inner.descendants(&path) {
            inner.nodes.remove(&gone);
        }
        inner.nodes.remove(&path);
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        let inner = locked(&self.inner);
        // Something at the path counts, a dangling link too, as `symlink_metadata` does.
        inner.alive().is_ok()
            && inner
                .resolve(path, false, false)
                .is_ok_and(|(found, _)| inner.is_dir(&found) || inner.nodes.contains_key(&found))
    }

    fn list_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let inner = locked(&self.inner);
        inner.alive()?;
        let (path, _) = inner.resolve(path, true, false)?;
        if !inner.is_dir(&path) {
            return Err(if inner.nodes.contains_key(&path) { not_a_folder(&path) } else { not_found(&path) });
        }
        // `nodes` is ordered by path, so the children come out sorted by name.
        Ok(inner
            .nodes
            .iter()
            .filter(|(k, _)| k.parent() == Some(path.as_path()))
            .filter_map(|(k, node)| {
                Some(DirEntry {
                    name: k.file_name()?.to_string_lossy().into_owned(),
                    is_dir: matches!(node, Node::Dir),
                    is_symlink: matches!(node, Node::Link { .. }),
                })
            })
            .collect())
    }

    fn symlink(&self, target: &str, link: &Path, kind: LinkKind) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.alive()?;
        let (link, _) = inner.resolve(link, false, false)?;
        if Inner::is_root(&link) || inner.nodes.contains_key(&link) {
            return Err(exists_already(&link));
        }
        inner.nodes.insert(link, Node::Link { target: target.to_owned(), kind });
        Ok(())
    }

    fn set_executable(&self, path: &Path, on: bool) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.alive()?;
        let (path, _) = inner.resolve(path, true, false)?;
        match inner.nodes.get_mut(&path) {
            Some(Node::File { executable, .. }) => {
                *executable = on;
                Ok(())
            }
            Some(Node::Dir) => Ok(()),
            Some(Node::Link { .. }) => Err(unresolved(&path)),
            None if Inner::is_root(&path) => Ok(()),
            None => Err(not_found(&path)),
        }
    }

    fn free_bytes(&self, path: &Path) -> io::Result<u64> {
        let inner = locked(&self.inner);
        inner.alive()?;
        let (path, _) = inner.resolve(path, true, false)?;
        if inner.is_dir(&path) || inner.nodes.contains_key(&path) { Ok(inner.free) } else { Err(not_found(&path)) }
    }
}
