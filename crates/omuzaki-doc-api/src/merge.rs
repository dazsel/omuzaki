//! Three-way merge of document trees (VC-02; `docs/design/branches.md` §7.2, ADR 0025).
//!
//! [`merge3`] compares a common base with two descendants of it, "ours" (the branch being merged into) and "theirs",
//! and says what happened at every path where the three don't agree. Objects merge key by key; arrays and scalars are
//! leaves, compared whole (as the library's update does, ADR 0019 D10), because merging list items by position would
//! invent states neither side ever had.
//!
//! **Conflicts are never resolved silently.** A path both sides changed differently, or one side removed while the
//! other changed something under it, is a [`Outcome::Conflict`] that [`Merge3::patch`] refuses to settle without a
//! [`Side`] chosen for it. Without a base nothing is known about who changed what, so every difference is a conflict.
//!
//! Pure and deterministic: no I/O, entries in path order. The op (`branch.merge`) groups entries into units and
//! validates the result; this module only knows trees.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{Change, DocPath, Patch, Value, lookup};

/// A JSON object, as the tree holds it.
type Map = serde_json::Map<String, Value>;

/// One of the two branches being merged.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The branch being merged into.
    Ours,
    /// The branch being merged in.
    Theirs,
}

/// What the merge does at one path (the rows of design §7.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Only ours changed it: the result keeps ours.
    Ours,
    /// Only theirs changed it: the result takes theirs.
    Theirs,
    /// Both changed it to the same value (both removed it, or both added the same thing).
    Same,
    /// Both changed it differently, one side removed what the other changed, or there is no base: someone chooses.
    Conflict,
}

/// One path where base, ours and theirs don't all agree.
///
/// **Encoding.** `None` means absent there (and, for `base`, also that the merge had no base). On the wire absence is
/// an omitted field and presence is the field's value, so a present `null` (`Some(Value::Null)`) stays distinct from
/// absent through JSON: `{"base": null}` reads back as `Some(Null)`, not `None`. (A plain `Option<Value>` would
/// serialise `Some(Null)` as `null` and read it back as `None`, turning "set this key to null" into "remove it".)
/// The JSON schema matches: the three fields are not required, and a present one may be any JSON value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MergeEntry {
    /// Where. An object present on both sides is never an entry itself, only its keys are, with one exception: an
    /// object that ours and theirs both added, identically, where the base has nothing, is one [`Outcome::Same`] entry
    /// at its root, not one per key (an empty one would otherwise go unreported). A conflict on a removed subtree sits
    /// at the subtree's root.
    pub path: DocPath,
    /// The common base's value.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub base: Option<Value>,
    /// Our value.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub ours: Option<Value>,
    /// Their value.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub theirs: Option<Value>,
    /// What the merge does here.
    pub outcome: Outcome,
}

/// Reads a field that is present as `Some`, whatever it holds, `null` included. A missing field never reaches this
/// (`#[serde(default)]` gives `None`), which is what keeps absent and null apart.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// The result of [`merge3`]: one entry per path that differs anywhere, in path order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Merge3 {
    /// The differing paths, sorted.
    pub entries: Vec<MergeEntry>,
}

/// Conflicts that [`Merge3::patch`] was given no side for, in path order. Nothing was decided for them.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct Unresolved {
    /// The conflicts' paths.
    pub paths: Vec<DocPath>,
}

impl fmt::Display for Unresolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} merge conflict(s) have no side chosen:", self.paths.len())?;
        for path in &self.paths {
            write!(f, " {path}")?;
        }
        Ok(())
    }
}

/// Merges `ours` and `theirs` against their common `base`, reporting every path where the three differ.
///
/// With `base: None` (no common version is known, design §7.3) every difference between ours and theirs is a
/// conflict, including keys only one side has.
pub fn merge3(base: Option<&Value>, ours: &Value, theirs: &Value) -> Merge3 {
    let mut walk = Walk { has_base: base.is_some(), segments: Vec::new(), entries: Vec::new() };
    walk.node(base, Some(ours), Some(theirs));
    Merge3 { entries: walk.entries }
}

impl MergeEntry {
    /// The side the merged result takes at this path, or `None` for a conflict `choose` has no side for.
    fn side(&self, choose: impl Fn(&DocPath) -> Option<Side>) -> Option<Side> {
        match self.outcome {
            Outcome::Ours | Outcome::Same => Some(Side::Ours),
            Outcome::Theirs => Some(Side::Theirs),
            Outcome::Conflict => choose(&self.path),
        }
    }
}

impl Merge3 {
    /// The entries that need a side chosen.
    pub fn conflicts(&self) -> impl Iterator<Item = &MergeEntry> {
        self.entries.iter().filter(|e| e.outcome == Outcome::Conflict)
    }

    /// The patch that turns `ours` into the merged result: every clean outcome, plus the side `choose` picks for each
    /// conflict (keyed by the conflict's path).
    ///
    /// `ours` must be the tree [`merge3`] was given: every change's `before` is read from it, so the patch applies
    /// only to that state, and a document that moved on since refuses it instead of losing the newer edit. A change
    /// that would write the value `ours` already holds is left out, so a tree that already has theirs gets no patch.
    ///
    /// # Errors
    /// [`Unresolved`] lists every conflict `choose` returned `None` for.
    pub fn patch(&self, ours: &Value, choose: impl Fn(&DocPath) -> Option<Side>) -> Result<Patch, Unresolved> {
        let mut patch = Patch::new();
        let mut unresolved = Vec::new();
        for entry in &self.entries {
            match entry.side(&choose) {
                Some(Side::Ours) => {} // ours already holds the result
                Some(Side::Theirs) => {
                    let before = lookup(ours, &entry.path).cloned();
                    if before != entry.theirs {
                        patch.changes.push(Change { path: entry.path.clone(), before, after: entry.theirs.clone() });
                    }
                }
                None => unresolved.push(entry.path.clone()),
            }
        }
        if unresolved.is_empty() { Ok(patch) } else { Err(Unresolved { paths: unresolved }) }
    }
}

/// The depth-first walk behind [`merge3`]. Path segments borrow the trees' keys and become a [`DocPath`] only for an
/// entry, so the walk allocates for what differs and for the key lists of the objects it recurses into.
struct Walk<'a> {
    has_base: bool,
    segments: Vec<&'a str>,
    entries: Vec<MergeEntry>,
}

impl<'a> Walk<'a> {
    /// Merges one path. It recurses where ours and theirs are both objects and the base is one too (or absent there):
    /// the key-by-key case. Anywhere else the three values are compared whole, which is what makes a subtree removed
    /// on one side and changed on the other a single conflict at its root, and an object both sides added identically
    /// a single [`Outcome::Same`] at its root.
    fn node(&mut self, base: Option<&'a Value>, ours: Option<&'a Value>, theirs: Option<&'a Value>) {
        if self.unchanged(base, ours, theirs) {
            return;
        }
        if let (Some(Value::Object(o)), Some(Value::Object(t))) = (ours, theirs) {
            match base {
                Some(Value::Object(b)) => return self.children(Some(b), o, t),
                // Both sides added the same object where the base has nothing: one entry for the addition. Recursing
                // would report a leaf per key, and nothing at all for an empty object.
                None if self.has_base && o == t => {}
                None => return self.children(None, o, t),
                Some(_) => {} // the base is a leaf here: both sides replaced it, compare whole
            }
        }
        if let Some(outcome) = self.outcome(base, ours, theirs) {
            self.entries.push(MergeEntry {
                path: DocPath(self.segments.iter().map(|s| (*s).to_owned()).collect()),
                base: base.cloned(),
                ours: ours.cloned(),
                theirs: theirs.cloned(),
                outcome,
            });
        }
    }

    /// True when nothing under this path differs anywhere. One comparison settles a whole untouched subtree, which
    /// is nearly all of a project: a merge changes a few files out of hundreds, and a visit per key would cost far
    /// more than comparing them.
    fn unchanged(&self, base: Option<&Value>, ours: Option<&Value>, theirs: Option<&Value>) -> bool {
        ours == theirs && (!self.has_base || base == ours)
    }

    /// Recurses into the union of the three objects' keys in sorted order, so entries come out in path order whatever
    /// order the maps keep their keys in.
    fn children(&mut self, base: Option<&'a Map>, ours: &'a Map, theirs: &'a Map) {
        let mut keys: Vec<&'a str> =
            ours.keys().chain(theirs.keys()).chain(base.into_iter().flat_map(Map::keys)).map(String::as_str).collect();
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            self.segments.push(key);
            self.node(base.and_then(|b| b.get(key)), ours.get(key), theirs.get(key));
            self.segments.pop();
        }
    }

    /// The table of design §7.2 for one leaf (absent counts as a value), or `None` when nothing differs.
    fn outcome(&self, base: Option<&Value>, ours: Option<&Value>, theirs: Option<&Value>) -> Option<Outcome> {
        if ours == theirs {
            return (self.has_base && base != ours).then_some(Outcome::Same);
        }
        Some(if !self.has_base {
            Outcome::Conflict
        } else if base == ours {
            Outcome::Theirs
        } else if base == theirs {
            Outcome::Ours
        } else {
            Outcome::Conflict
        })
    }
}
