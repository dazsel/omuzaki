//! Patch application and [`Scratch`], an owned in-memory document.
//!
//! [`apply_to`] is the one implementation of the patch rules (before-checks, the parent must exist, all or nothing),
//! shared by `omuzaki-doc-mock`, [`Scratch`] and any other [`DocWrite`](crate::DocWrite) that holds a plain tree.

use crate::{DocPath, DocRead, DocVersion, DocWrite, Patch, PatchError, Value, lookup};

/// Applies `patch` to `root` atomically: either every change lands or `root` is left exactly as it was.
///
/// Each change's `before` is checked against what the tree holds at that moment (so a later change sees the effect
/// of an earlier one). The parent of a changed path must already exist: auto-creating parents would make
/// [`Patch::inverse`] inexact, because the inverse can't know to remove them. Ops create whole objects instead.
/// Arrays change in place, append at the end (`index == len`) or pop the last item.
///
/// # Errors
/// The first [`PatchError::Conflict`] or [`PatchError::Unreachable`]; `root` is then unchanged.
pub fn apply_to(root: &mut Value, patch: &Patch) -> Result<(), PatchError> {
    // What each applied change actually replaced, for rollback. The change's own `before` is not enough: the
    // before-check uses value equality, which equates values that differ in bytes (`0.0` and `-0.0`).
    let mut replaced: Vec<Option<Value>> = Vec::with_capacity(patch.changes.len());
    for change in &patch.changes {
        match apply_change(root, change) {
            Ok(old) => replaced.push(old),
            Err(err) => {
                rollback(root, &patch.changes[..replaced.len()], replaced);
                return Err(err);
            }
        }
    }
    Ok(())
}

/// Applies one change and returns the value it replaced (`None` if the path was absent).
fn apply_change(root: &mut Value, change: &crate::Change) -> Result<Option<Value>, PatchError> {
    let found = lookup(root, &change.path);
    if found != change.before.as_ref() {
        return Err(PatchError::Conflict {
            path: change.path.clone(),
            expected: show(change.before.as_ref()),
            found: show(found),
        });
    }
    let old = found.cloned();
    set(root, &change.path, change.after.clone())?;
    Ok(old)
}

/// Undoes `applied` (the changes that succeeded), newest first, putting back exactly what each replaced. Each one just
/// succeeded, so restoring is valid by the same rules: a replaced value goes back in place, an append is popped, a
/// pop is appended again.
fn rollback(root: &mut Value, applied: &[crate::Change], replaced: Vec<Option<Value>>) {
    for (change, old) in applied.iter().zip(replaced).rev() {
        let restored = set(root, &change.path, old);
        debug_assert!(restored.is_ok(), "rolling back a change that was just applied cannot fail");
    }
}

fn show(v: Option<&Value>) -> String {
    v.map_or_else(|| "absent".to_owned(), Value::to_string)
}

/// Sets (or removes, for `None`) the value at `path`; the parent must already exist.
fn set(root: &mut Value, path: &DocPath, value: Option<Value>) -> Result<(), PatchError> {
    let unreachable = |reason: &str| PatchError::Unreachable { path: path.clone(), reason: reason.to_owned() };
    let Some((parent_path, last)) = path.split_last() else {
        *root = value.unwrap_or(Value::Null);
        return Ok(());
    };
    let mut cur = root;
    for seg in parent_path.segments() {
        cur = match cur {
            Value::Object(map) => {
                map.get_mut(seg).ok_or_else(|| unreachable("a parent doesn't exist; create it first"))?
            }
            Value::Array(arr) => {
                let i: usize = seg.parse().map_err(|_| unreachable("array index isn't a number"))?;
                arr.get_mut(i).ok_or_else(|| unreachable("array index out of range"))?
            }
            _ => return Err(unreachable("a parent isn't an object or array")),
        };
    }
    match (cur, value) {
        (Value::Object(map), Some(v)) => {
            map.insert(last.to_owned(), v);
        }
        (Value::Object(map), None) => {
            map.remove(last);
        }
        (Value::Array(arr), v) => {
            let i: usize = last.parse().map_err(|_| unreachable("array index isn't a number"))?;
            match v {
                Some(v) if i < arr.len() => arr[i] = v,
                Some(v) if i == arr.len() => arr.push(v),
                None if i.checked_add(1) == Some(arr.len()) => {
                    arr.pop();
                }
                _ => return Err(unreachable("arrays only change in place, append at the end or pop the last item")),
            }
        }
        _ => return Err(unreachable("the parent isn't an object or array")),
    }
    Ok(())
}

/// An owned, in-memory copy of a document, for planning ahead without touching the real one.
///
/// A composite op (`library.apply`, a script) plans step one, applies its patch to a `Scratch`, plans step two
/// against the result, and so on; the patches concatenated are then the combined change to the original. The copy
/// is made once, in [`Scratch::of`]; each [`apply`](DocWrite::apply) changes it in place.
#[derive(Clone, Debug, PartialEq)]
pub struct Scratch {
    root: Value,
    version: DocVersion,
}

impl Scratch {
    /// A copy of `doc` as it is now, at the same version. Clones the root once.
    pub fn of(doc: &dyn DocRead) -> Self {
        let root = doc.get(&DocPath::root()).cloned().unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        Self { root, version: doc.version() }
    }

    /// The whole tree.
    pub fn root(&self) -> &Value {
        &self.root
    }
}

impl DocRead for Scratch {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        lookup(&self.root, path)
    }

    fn version(&self) -> DocVersion {
        self.version
    }
}

impl DocWrite for Scratch {
    fn apply(&mut self, patch: &Patch) -> Result<DocVersion, PatchError> {
        apply_to(&mut self.root, patch)?;
        self.version = DocVersion(self.version.0.saturating_add(1));
        Ok(self.version)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn p(s: &str) -> DocPath {
        DocPath::parse(s).unwrap()
    }

    #[test]
    fn a_scratch_copies_the_document_and_leaves_it_alone() {
        let original = Scratch { root: json!({"a": 1}), version: DocVersion(7) };
        let mut scratch = Scratch::of(&original);
        assert_eq!(scratch.version(), DocVersion(7));
        assert_eq!(scratch.apply(&Patch::new().with(p("a"), Some(json!(1)), Some(json!(2)))).unwrap(), DocVersion(8));
        assert_eq!(scratch.get(&p("a")), Some(&json!(2)));
        assert_eq!(original.root(), &json!({"a": 1}));
    }

    #[test]
    fn a_failing_patch_leaves_the_tree_as_it_was() {
        let mut root = json!({"a": 1, "l": [1, 2]});
        let patch = Patch::new()
            .with(p("a"), Some(json!(1)), Some(json!(2)))
            .with(p("l/2"), None, Some(json!(3)))
            .with(p("l/2"), Some(json!(3)), None)
            .with(p("l/0"), Some(json!(1)), Some(json!(0)))
            .with(p("a"), Some(json!("never")), Some(json!(9)));
        assert!(matches!(apply_to(&mut root, &patch), Err(PatchError::Conflict { .. })));
        assert_eq!(root, json!({"a": 1, "l": [1, 2]}));
    }

    #[test]
    fn rollback_restores_the_bytes_not_just_an_equal_value() {
        let mut root = json!({"x": -0.0, "y": 1});
        let patch =
            Patch::new().with(p("x"), Some(json!(0.0)), Some(json!(5.0))).with(p("y"), Some(json!(99)), Some(json!(2)));
        assert!(matches!(apply_to(&mut root, &patch), Err(PatchError::Conflict { .. })));
        assert!(root["x"].as_f64().is_some_and(f64::is_sign_negative));
        assert_eq!(serde_json::to_string(&root).unwrap(), r#"{"x":-0.0,"y":1}"#);
    }

    #[test]
    fn overlapping_ancestor_and_root_changes_roll_back_in_order() {
        let mut root = json!({"a": {"n": -0.0}, "l": [1]});
        let before = serde_json::to_string(&root).unwrap();
        let patch = Patch::new()
            .with(p("a/n"), Some(json!(0.0)), Some(json!(1)))
            .with(p("a"), Some(json!({"n": 1})), Some(json!({"m": 2})))
            .with(DocPath::root(), Some(json!({"a": {"m": 2}, "l": [1]})), Some(json!({"q": [1]})))
            .with(p("q/1"), None, Some(json!(2)))
            .with(p("q/0"), Some(json!(1)), None)
            .with(p("q"), Some(json!([1, 2])), Some(json!(0)));
        assert!(matches!(
            apply_to(&mut root, &patch),
            Err(PatchError::Unreachable { .. } | PatchError::Conflict { .. })
        ));
        assert_eq!(serde_json::to_string(&root).unwrap(), before);
    }

    #[test]
    fn a_huge_array_index_is_an_error_not_an_overflow() {
        let mut scratch = Scratch { root: json!({"x": 0, "a": [0]}), version: DocVersion(3) };
        let before = serde_json::to_string(scratch.root()).unwrap();
        let huge = format!("a/{}", usize::MAX);
        let patch = Patch::new().with(p("x"), Some(json!(0)), Some(json!(1))).with(p(&huge), Some(json!(0)), None);
        // The before-check fails first (nothing is there); a `before: None` removal reaches the index arithmetic.
        let removal = Patch::new().with(p("x"), Some(json!(0)), Some(json!(1))).with(p(&huge), None, None);
        for bad in [patch, removal] {
            assert!(matches!(scratch.apply(&bad), Err(PatchError::Conflict { .. } | PatchError::Unreachable { .. })));
            assert_eq!(serde_json::to_string(scratch.root()).unwrap(), before);
            assert_eq!(scratch.version(), DocVersion(3));
        }
        let removal = Patch::new().with(p(&huge), None, None);
        assert!(matches!(scratch.apply(&removal), Err(PatchError::Unreachable { .. })));
    }

    #[test]
    fn an_unreachable_path_rolls_back_too() {
        let mut root = json!({"a": 1});
        let patch = Patch::new().with(p("b"), None, Some(json!({}))).with(p("x/y"), None, Some(json!(1)));
        assert!(matches!(apply_to(&mut root, &patch), Err(PatchError::Unreachable { .. })));
        assert_eq!(root, json!({"a": 1}));
    }

    #[test]
    fn replacing_the_root_works_and_rolls_back() {
        let mut root = json!({"a": 1});
        let patch = Patch::new().with(DocPath::root(), Some(json!({"a": 1})), Some(json!({"z": 0})));
        apply_to(&mut root, &patch).unwrap();
        assert_eq!(root, json!({"z": 0}));
        let failing = Patch::new().with(p("z"), Some(json!(0)), Some(json!(1))).with(p("z"), Some(json!(0)), None);
        assert!(apply_to(&mut root, &failing).is_err());
        assert_eq!(root, json!({"z": 0}));
    }
}
