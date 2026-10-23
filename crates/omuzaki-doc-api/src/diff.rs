//! The two-way tree diff (VC-03; `docs/design/branches.md` §6.2): what changed between two document states, as a
//! [`Patch`] that turns the first into the second.
//!
//! Objects are compared key by key; arrays and scalars are leaves, compared whole, as [`merge3`](crate::merge::merge3)
//! does, so a change never names a list position that neither side has. A key only one side has is one change holding
//! the whole subtree, which keeps the patch rule of [`Change`]: the parent of every change exists when it is applied,
//! so [`Patch::inverse`] is an exact way back.
//!
//! Pure and deterministic: no I/O, changes in path order (keys sorted).

use crate::{Change, DocPath, Patch, Value};

/// The changes that turn `before` into `after`, in path order. Empty when the two are equal.
///
/// When the roots themselves differ in kind (one is not an object), the patch is one change at the root.
pub fn diff(before: &Value, after: &Value) -> Patch {
    let mut changes = Vec::new();
    walk(&DocPath::root(), Some(before), Some(after), &mut changes);
    Patch { changes }
}

/// [`diff`] restricted to the paths under any of `scope` (every path when `scope` is empty).
pub fn diff_under(before: &Value, after: &Value, scope: &[DocPath]) -> Patch {
    let mut patch = diff(before, after);
    if !scope.is_empty() {
        patch
            .changes
            .retain(|change| scope.iter().any(|root| change.path.starts_with(root) || root.starts_with(&change.path)));
    }
    patch
}

fn walk(path: &DocPath, before: Option<&Value>, after: Option<&Value>, out: &mut Vec<Change>) {
    match (before, after) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                walk(&path.join(key.as_str()), a.get(key), b.get(key), out);
            }
        }
        (a, b) if a == b => {}
        (a, b) => out.push(Change { path: path.clone(), before: a.cloned(), after: b.cloned() }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::apply_to;

    fn paths(patch: &Patch) -> Vec<String> {
        patch.changes.iter().map(|c| c.path.to_string()).collect()
    }

    #[test]
    fn a_state_against_itself_is_empty() {
        let tree = json!({"characters": {"kit": {"face": {"eye": {"size": 0.6}}, "tags": [1, 2]}}});
        assert_eq!(diff(&tree, &tree), Patch::new());
    }

    #[test]
    fn one_parameter_lists_exactly_that_path() {
        let a = json!({"characters": {"kit": {"face": {"eye": {"size": 0.6, "tilt": 0.1}}}}});
        let b = json!({"characters": {"kit": {"face": {"eye": {"size": 0.7, "tilt": 0.1}}}}});
        let patch = diff(&a, &b);
        assert_eq!(paths(&patch), ["characters/kit/face/eye/size"]);
        assert_eq!(patch.changes[0].before, Some(json!(0.6)));
        assert_eq!(patch.changes[0].after, Some(json!(0.7)));
    }

    #[test]
    fn added_and_removed_keys_carry_their_whole_subtree_and_arrays_are_leaves() {
        let a = json!({"a": {"x": 1}, "list": [1, 2, 3], "gone": {"deep": {"v": 1}}});
        let b = json!({"a": {"x": 1, "new": {"deep": true}}, "list": [1, 2]});
        let patch = diff(&a, &b);
        assert_eq!(paths(&patch), ["a/new", "gone", "list"]);
        assert_eq!(patch.changes[0].before, None);
        assert_eq!(patch.changes[0].after, Some(json!({"deep": true})));
        assert_eq!(patch.changes[1].after, None);
        assert_eq!(patch.changes[2].after, Some(json!([1, 2])));
    }

    #[test]
    fn the_patch_turns_one_into_the_other_and_its_inverse_turns_it_back() {
        let a = json!({"characters": {"kit": {"name": "Kit", "parts": {"hair": {"len": 3}}}}, "scenes": {}});
        let b = json!({"characters": {"kit": {"name": "Kit 2", "parts": {"ears": {}}}}, "project": {"fps": 60}});
        let patch = diff(&a, &b);
        let mut tree = a.clone();
        apply_to(&mut tree, &patch).unwrap();
        assert_eq!(tree, b);
        apply_to(&mut tree, &patch.inverse()).unwrap();
        assert_eq!(tree, a);
    }

    #[test]
    fn a_scope_keeps_the_changes_under_it() {
        let a = json!({"characters": {"kit": {"v": 1}, "mia": {"v": 1}}});
        let b = json!({"characters": {"kit": {"v": 2}, "mia": {"v": 2}}});
        let scope = [DocPath::parse("characters/kit").unwrap()];
        assert_eq!(paths(&diff_under(&a, &b, &scope)), ["characters/kit/v"]);
        assert_eq!(diff_under(&a, &b, &[]).changes.len(), 2);
    }

    #[test]
    fn roots_of_another_kind_are_one_change_at_the_root() {
        let patch = diff(&json!(1), &json!({"a": 1}));
        assert_eq!(paths(&patch), [""]);
    }
}
