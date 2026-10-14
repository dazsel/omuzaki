//! The disk half of the check: walking a place that was already allowed by name to the real place it names, one name
//! at a time, following a symbolic link only into places the request may reach.
//!
//! It asks the disk one question per name ("what is here?", without following a link at that name), and before each
//! question it checks the name against the places the request may reach. So a link that points out of them (to a
//! secret, to a network share) is refused from the link's own text: its target is never looked up, not even its
//! metadata. Tests answer through the [`Disk`] trait, which records every question, to prove it.
//!
//! The result is a real place: every name in it is a real folder (or, at the end, a file or nothing yet), none a link,
//! and none `..`. That is what [`CanonicalPath`](super::CanonicalPath) opens, and its open refuses a link at every
//! step, so a link swapped in after this walk is refused there.

use std::fs;
use std::io;
use std::path::Path;

use super::form::{Lexical, Parsed, PathKey, Step, Style};

/// The most links one walk follows, as Linux allows (`ELOOP` past 40); a loop of links ends here.
const MAX_LINKS: usize = 40;

/// The most names a walk may hold, done and to do: links cannot grow a place past what a path may hold.
const MAX_NAMES: usize = 4096;

/// What is at a path, the last name not followed if it is a link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Entry {
    /// Nothing.
    Missing,
    /// A folder.
    Folder,
    /// A file, or anything else that is not a folder or a link.
    File,
    /// A symbolic link (or, on Windows, a junction or another name-surrogate reparse point), with its target text.
    Link(String),
}

/// What the walk needs from the file system: one question, answered by the OS in production.
pub(super) trait Disk {
    /// What is at `path`. Every name but the last is a folder the walk already found real, so answering follows no
    /// link.
    fn entry(&self, path: &Path) -> io::Result<Entry>;
}

/// The real file system.
pub(super) struct OsDisk;

impl Disk for OsDisk {
    fn entry(&self, path: &Path) -> io::Result<Entry> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Entry::Missing),
            Err(error) => return Err(error),
        };
        // On Windows, `is_symlink` is true for every name surrogate (symbolic links, junctions), and `read_link`
        // reads both.
        if meta.file_type().is_symlink() {
            let target = fs::read_link(path)?.into_os_string().into_string();
            return target.map(Entry::Link).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"));
        }
        Ok(if meta.is_dir() { Entry::Folder } else { Entry::File })
    }
}

/// Where a place really is.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Resolved {
    /// The real place. If the file does not exist, its existing folder's real place plus the names below it.
    pub(super) place: Lexical,
    /// Whether the whole place exists.
    pub(super) exists: bool,
}

/// Walks `place` on `disk`. `may_visit` says whether the walk may ask the disk about a place (it is inside, or on
/// the way to, a place the request may reach); a link is followed only one name at a time, each name asked about
/// first, so the walk never asks about a place `may_visit` refuses.
///
/// # Errors
/// Any refusal by `may_visit`, any failure other than "does not exist" (permission denied, a name too long), a file
/// where a folder belongs, a link target that does not parse or climbs above its root, more than [`MAX_LINKS`]
/// links, and a link whose target is missing: creating the "missing" file would create the link's target. Callers
/// treat every error as a refusal and report all of them alike, so an error cannot be used to probe the disk.
pub(super) fn resolve(
    disk: &dyn Disk,
    may_visit: &dyn Fn(&PathKey) -> bool,
    place: &Lexical,
    style: Style,
) -> io::Result<Resolved> {
    let mut walk = Walk::new(place, style);
    let mut links = 0;
    while let Some((name, optional)) = walk.next_name()? {
        walk.enter(name);
        if !may_visit(&walk.key) {
            return Err(refused("outside every place this request may reach"));
        }
        match disk.entry(&walk.real.path(usize::MAX))? {
            Entry::Folder => {}
            Entry::File if walk.todo.is_empty() => {}
            Entry::File => return Err(refused("a file where a folder belongs")),
            // Only the request's own names may be missing (a file about to be written); everything after it is the
            // request's too, since a link's steps are always taken before the names that followed the link.
            Entry::Missing if optional => return Ok(walk.missing()),
            Entry::Missing => return Err(refused("a link to nothing")),
            Entry::Link(target) => {
                links += 1;
                if links > MAX_LINKS {
                    return Err(refused("too many links"));
                }
                walk.follow(&target)?;
            }
        }
    }
    Ok(Resolved { place: walk.real, exists: true })
}

/// The state of a walk: the real place reached so far, and the steps still to take.
struct Walk {
    style: Style,
    /// The names taken so far, each a real folder (the last one may be about to be asked about).
    real: Lexical,
    /// `real` as a key, kept in step.
    key: PathKey,
    /// Steps still to take, the next one last.
    todo: Vec<Step>,
    /// How many of the bottom entries of `todo` are the request's own names, which may be missing; a link's steps go
    /// on top of them.
    optional: usize,
}

impl Walk {
    fn new(place: &Lexical, style: Style) -> Self {
        let real = place.truncated(0);
        let todo: Vec<Step> = place.names().iter().rev().cloned().map(Step::Name).collect();
        Self { style, key: real.key(style), optional: todo.len(), real, todo }
    }

    /// The next name to take, with whether it is the request's own (and so may be missing). `..` steps from a link's
    /// target are taken on the way: the place reached so far is real, so its parent is the folder above it.
    fn next_name(&mut self) -> io::Result<Option<(String, bool)>> {
        while let Some(step) = self.todo.pop() {
            let optional = self.todo.len() < self.optional;
            self.optional = self.optional.min(self.todo.len());
            match step {
                Step::Name(name) => return Ok(Some((name, optional))),
                Step::Parent => {
                    self.real.pop().ok_or_else(|| refused("a link climbs above its root"))?;
                    self.key.pop();
                }
            }
        }
        Ok(None)
    }

    fn enter(&mut self, name: String) {
        self.key.push(&name, self.style);
        self.real.push(name);
    }

    /// Replaces the link just entered by its target, read from the folder the link is in.
    fn follow(&mut self, target: &str) -> io::Result<()> {
        self.real.pop();
        self.key.pop();
        let target =
            Parsed::parse(target, self.style).map_err(|_| refused("a link whose target is not a plain path"))?;
        self.real = target.start(&self.real);
        self.key = self.real.key(self.style);
        if self.real.depth() + self.todo.len() + target.steps().len() > MAX_NAMES {
            return Err(refused("a link that makes the path too long"));
        }
        self.todo.extend(target.steps().iter().rev().cloned());
        Ok(())
    }

    /// The walk's answer when the name just entered does not exist: the rest of the request's names, unchanged
    /// (they cannot be links, since nothing is there).
    fn missing(mut self) -> Resolved {
        while let Some(Step::Name(name)) = self.todo.pop() {
            self.real.push(name);
        }
        Resolved { place: self.real, exists: false }
    }
}

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why.to_owned())
}
