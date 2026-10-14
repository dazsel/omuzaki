//! Contract for the project document (spec v3 §7): the tree that project files load into, and the only way to
//! change it, a [`Patch`].
//!
//! Why a generic tree plus patches, instead of typed structs mutated in place:
//! - **Dry-run is free.** An op computes a patch without touching anything; showing it *is* the dry-run (§6).
//! - **Undo is automatic.** A patch records `before` and `after`, so [`Patch::inverse`] undoes it exactly.
//! - **Conflicts are detected, not overwritten.** Applying checks every `before` against the current value, so two
//!   agents editing the same field can't silently clobber each other (§12 fences).
//! - **Reviewable.** A patch is a readable list of path changes: what the Build Theatre and PR reviews show.
//!
//! Subsystems keep typed views (serde structs) over their part of the tree; see [`DocReadExt::get_as`].
#![deny(missing_docs)]

pub mod merge;
mod scratch;

use std::fmt;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use scratch::{Scratch, apply_to};

/// A value in the document tree. At rest it's TOML (§7); in memory and over MCP it's JSON-shaped.
pub type Value = serde_json::Value;

/// A path into the document, e.g. `characters/mira/face/profile`.
///
/// Segments are object keys; a segment of digits indexes into an array. Paths are written with `/` and never start
/// with one.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct DocPath(Vec<String>);

/// Why a string isn't a valid [`DocPath`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid document path {path:?}: {reason}")]
pub struct DocPathError {
    /// The rejected path.
    pub path: String,
    /// What's wrong with it.
    pub reason: &'static str,
}

impl DocPath {
    /// The root of the document.
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// Parses `a/b/c`.
    ///
    /// # Errors
    /// Fails on a leading or trailing `/` or an empty segment.
    pub fn parse(s: &str) -> Result<Self, DocPathError> {
        if s.is_empty() {
            return Ok(Self::root());
        }
        let segs: Vec<String> = s.split('/').map(str::to_owned).collect();
        if segs.iter().any(String::is_empty) {
            return Err(DocPathError { path: s.to_owned(), reason: "empty segment (leading, trailing or double '/')" });
        }
        Ok(Self(segs))
    }

    /// This path with one more segment.
    #[must_use]
    pub fn join(&self, segment: impl Into<String>) -> Self {
        let mut v = self.0.clone();
        v.push(segment.into());
        Self(v)
    }

    /// The segments, root first.
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// The parent path and last segment, or `None` for the root.
    pub fn split_last(&self) -> Option<(DocPath, &str)> {
        let (last, rest) = self.0.split_last()?;
        Some((DocPath(rest.to_vec()), last.as_str()))
    }

    /// True if `self` is `other` or lies under it.
    pub fn starts_with(&self, other: &DocPath) -> bool {
        self.0.starts_with(&other.0)
    }
}

impl TryFrom<String> for DocPath {
    type Error = DocPathError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<DocPath> for String {
    fn from(p: DocPath) -> Self {
        p.0.join("/")
    }
}

impl fmt::Display for DocPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join("/"))
    }
}

impl fmt::Debug for DocPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DocPath({self})")
    }
}

/// One change at one path. `None` means "absent": `before: None` creates, `after: None` removes.
///
/// The parent of `path` must exist when the change is applied; create a new object with one change holding the
/// whole object. That rule is what makes [`Patch::inverse`] an exact undo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Change {
    /// Where the change happens.
    pub path: DocPath,
    /// The value the change expects to find (checked when applying).
    pub before: Option<Value>,
    /// The value after the change.
    pub after: Option<Value>,
}

/// An ordered list of changes, applied atomically: all of them or none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Patch {
    /// The changes, in application order.
    pub changes: Vec<Change>,
}

impl Patch {
    /// A patch with no changes.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a change (builder style).
    #[must_use]
    pub fn with(mut self, path: DocPath, before: Option<Value>, after: Option<Value>) -> Self {
        self.changes.push(Change { path, before, after });
        self
    }

    /// True if applying it would change nothing.
    pub fn is_empty(&self) -> bool {
        self.changes.iter().all(|c| c.before == c.after)
    }

    /// The patch that undoes this one: changes reversed, `before` and `after` swapped.
    #[must_use]
    pub fn inverse(&self) -> Self {
        Self {
            changes: self
                .changes
                .iter()
                .rev()
                .map(|c| Change { path: c.path.clone(), before: c.after.clone(), after: c.before.clone() })
                .collect(),
        }
    }

    /// Appends another patch's changes (for grouping ops into one transaction).
    pub fn extend(&mut self, other: Patch) {
        self.changes.extend(other.changes);
    }
}

/// A monotonically increasing document version; bumps on every applied patch.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct DocVersion(pub u64);

/// Why a patch couldn't be applied. Nothing was changed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PatchError {
    /// The document doesn't hold what the change expected: someone else changed it first.
    #[error("conflict at {path}: expected {expected}, found {found}")]
    Conflict {
        /// Where.
        path: DocPath,
        /// What the change expected (JSON, or `absent`).
        expected: String,
        /// What the document holds (JSON, or `absent`).
        found: String,
    },
    /// The path goes through something that isn't an object or array, or past the end of an array.
    #[error("can't reach {path}: {reason}")]
    Unreachable {
        /// Where.
        path: DocPath,
        /// Why.
        reason: String,
    },
}

/// Why a typed read failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DocError {
    /// Nothing at that path.
    #[error("nothing at {0}")]
    Missing(DocPath),
    /// The value doesn't have the expected shape.
    #[error("{path} has the wrong shape: {reason}")]
    Shape {
        /// Where.
        path: DocPath,
        /// The deserialisation error.
        reason: String,
    },
}

/// Read access to a document.
pub trait DocRead: Send + Sync {
    /// The value at `path`, if any.
    fn get(&self, path: &DocPath) -> Option<&Value>;
    /// The current version.
    fn version(&self) -> DocVersion;
}

/// Write access: the only mutation is applying a whole patch.
pub trait DocWrite: DocRead {
    /// Applies `patch` atomically, checking each change's `before`, and bumps the version.
    ///
    /// # Errors
    /// Returns the first conflict or unreachable path; the document is then unchanged.
    fn apply(&mut self, patch: &Patch) -> Result<DocVersion, PatchError>;
}

/// Typed reads on top of [`DocRead`] (kept separate so `DocRead` stays object-safe).
pub trait DocReadExt: DocRead {
    /// Deserialises the value at `path` into `T`.
    ///
    /// # Errors
    /// [`DocError::Missing`] if absent, [`DocError::Shape`] if it doesn't fit `T`.
    fn get_as<T: DeserializeOwned>(&self, path: &DocPath) -> Result<T, DocError> {
        let v = self.get(path).ok_or_else(|| DocError::Missing(path.clone()))?;
        T::deserialize(v).map_err(|e| DocError::Shape { path: path.clone(), reason: e.to_string() })
    }
}

impl<D: DocRead + ?Sized> DocReadExt for D {}

/// Follows `path` from `root`; shared by every [`DocRead`] implementation so path semantics live in one place.
pub fn lookup<'a>(root: &'a Value, path: &DocPath) -> Option<&'a Value> {
    path.segments().iter().try_fold(root, |v, seg| match v {
        Value::Object(map) => map.get(seg),
        Value::Array(arr) => seg.parse::<usize>().ok().and_then(|i| arr.get(i)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths_parse_display_and_reject_empty_segments() {
        let p = DocPath::parse("characters/mira/face").unwrap();
        assert_eq!(p.to_string(), "characters/mira/face");
        assert_eq!(p.segments().len(), 3);
        assert!(DocPath::parse("/a").is_err());
        assert!(DocPath::parse("a//b").is_err());
        assert_eq!(DocPath::parse("").unwrap(), DocPath::root());
        let (parent, last) = p.split_last().unwrap();
        assert_eq!((parent.to_string().as_str(), last), ("characters/mira", "face"));
        assert!(p.starts_with(&parent));
    }

    #[test]
    fn inverse_undoes_in_reverse_order() {
        let a = DocPath::parse("a").unwrap();
        let p = Patch::new().with(a.clone(), None, Some(json!(1))).with(a.clone(), Some(json!(1)), Some(json!(2)));
        let inv = p.inverse();
        assert_eq!(inv.changes[0], Change { path: a.clone(), before: Some(json!(2)), after: Some(json!(1)) });
        assert_eq!(inv.changes[1], Change { path: a, before: Some(json!(1)), after: None });
        assert_eq!(inv.inverse(), p);
    }

    #[test]
    fn lookup_walks_objects_and_arrays() {
        let doc = json!({"chars": {"d": {"tags": ["x", "y"]}}});
        assert_eq!(lookup(&doc, &DocPath::parse("chars/d/tags/1").unwrap()), Some(&json!("y")));
        assert_eq!(lookup(&doc, &DocPath::parse("chars/d/tags/9").unwrap()), None);
        assert_eq!(lookup(&doc, &DocPath::parse("chars/d/tags/x").unwrap()), None);
    }
}
