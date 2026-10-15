//! An in-memory [`Disk`] that fails on cue and forgets what was never synced, as a killed process would.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
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
    },
}

#[derive(Clone, Debug)]
enum Node {
    Dir,
    File { bytes: Vec<u8>, executable: bool, synced: bool },
    Link { target: String },
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

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why.to_owned())
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

    fn parent_dir(&self, path: &Path) -> io::Result<()> {
        if !path.is_absolute() {
            return Err(invalid("path is not absolute"));
        }
        match path.parent() {
            Some(parent) if !self.is_dir(parent) => Err(not_found(parent)),
            _ => Ok(()),
        }
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
                    Node::Link { target } => Entry::Link { target: target.clone() },
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
        match inner.nodes.get(path) {
            Some(Node::File { bytes, .. }) => Ok(bytes.clone()),
            Some(Node::Dir) => Err(is_a_folder(path)),
            Some(Node::Link { .. }) => Err(invalid("not a file")),
            None if Inner::is_root(path) => Err(is_a_folder(path)),
            None => Err(not_found(path)),
        }
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.tick(Op::Write)?;
        inner.parent_dir(path)?;
        if Inner::is_root(path) {
            return Err(is_a_folder(path));
        }
        match inner.nodes.get_mut(path) {
            Some(Node::File { bytes: old, synced, .. }) => {
                bytes.clone_into(old);
                *synced = false;
            }
            Some(Node::Dir) => return Err(is_a_folder(path)),
            Some(Node::Link { .. }) => return Err(invalid("not a file")),
            None => {
                inner
                    .nodes
                    .insert(path.to_path_buf(), Node::File { bytes: bytes.to_vec(), executable: false, synced: false });
            }
        }
        Ok(())
    }

    fn fsync(&self, path: &Path) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.tick(Op::Fsync)?;
        if let Some(Node::File { synced, .. }) = inner.nodes.get_mut(path) {
            *synced = true;
            return Ok(());
        }
        if inner.is_dir(path) || inner.nodes.contains_key(path) { Ok(()) } else { Err(not_found(path)) }
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.tick(Op::Rename)?;
        inner.parent_dir(to)?;
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
        match inner.nodes.get(to) {
            Some(Node::Dir) if !from_is_dir => return Err(invalid("target is a folder")),
            Some(Node::Dir) if !inner.descendants(to).is_empty() => return Err(invalid("target folder is not empty")),
            Some(Node::File { .. } | Node::Link { .. }) if from_is_dir => {
                return Err(invalid("target is not a folder"));
            }
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
        if !path.is_absolute() {
            return Err(invalid("path is not absolute"));
        }
        let mut chain: Vec<&Path> = path.ancestors().filter(|p| p.parent().is_some()).collect();
        chain.reverse();
        for dir in chain {
            match inner.nodes.get(dir) {
                Some(Node::Dir) => {}
                Some(_) => return Err(invalid("a part of the path is not a folder")),
                None => {
                    inner.nodes.insert(dir.to_path_buf(), Node::Dir);
                }
            }
        }
        Ok(())
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.alive()?;
        if Inner::is_root(path) {
            return Err(root_busy(path));
        }
        match inner.nodes.get(path) {
            Some(Node::Dir) => {}
            Some(_) => return Err(invalid("not a folder")),
            None => return Err(not_found(path)),
        }
        for gone in inner.descendants(path) {
            inner.nodes.remove(&gone);
        }
        inner.nodes.remove(path);
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        let inner = locked(&self.inner);
        inner.alive().is_ok() && (inner.is_dir(path) || inner.nodes.contains_key(path))
    }

    fn list_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let inner = locked(&self.inner);
        inner.alive()?;
        if !inner.is_dir(path) {
            return Err(if inner.nodes.contains_key(path) { invalid("not a folder") } else { not_found(path) });
        }
        // `nodes` is ordered by path, so the children come out sorted by name.
        Ok(inner
            .nodes
            .iter()
            .filter(|(k, _)| k.parent() == Some(path))
            .filter_map(|(k, node)| {
                Some(DirEntry {
                    name: k.file_name()?.to_string_lossy().into_owned(),
                    is_dir: matches!(node, Node::Dir),
                    is_symlink: matches!(node, Node::Link { .. }),
                })
            })
            .collect())
    }

    fn symlink(&self, target: &str, link: &Path, _kind: LinkKind) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.alive()?;
        inner.parent_dir(link)?;
        if Inner::is_root(link) || inner.nodes.contains_key(link) {
            return Err(exists_already(link));
        }
        inner.nodes.insert(link.to_path_buf(), Node::Link { target: target.to_owned() });
        Ok(())
    }

    fn set_executable(&self, path: &Path, on: bool) -> io::Result<()> {
        let mut inner = locked(&self.inner);
        inner.alive()?;
        match inner.nodes.get_mut(path) {
            Some(Node::File { executable, .. }) => {
                *executable = on;
                Ok(())
            }
            Some(Node::Dir) => Ok(()),
            Some(Node::Link { .. }) => Err(invalid("not a file")),
            None if Inner::is_root(path) => Ok(()),
            None => Err(not_found(path)),
        }
    }

    fn free_bytes(&self, path: &Path) -> io::Result<u64> {
        let inner = locked(&self.inner);
        inner.alive()?;
        if inner.is_dir(path) || inner.nodes.contains_key(path) { Ok(inner.free) } else { Err(not_found(path)) }
    }
}
