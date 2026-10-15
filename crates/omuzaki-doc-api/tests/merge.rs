//! `merge3` (VC-02): the rows of design §7.2 one by one, the cases the table doesn't spell out (removed subtrees,
//! leaves, no base), `Merge3::patch`, and the properties of design §12.1 over random trees.

// A failed setup or a refused patch should abort the test, and an `unwrap` says so as clearly as any message.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use omuzaki_doc_api::merge::{Merge3, MergeEntry, Outcome, Side, Unresolved, merge3};
use omuzaki_doc_api::{DocPath, Patch, lookup};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use serde_json::{Map, Value, json};

fn path(s: &str) -> DocPath {
    DocPath::parse(s).unwrap()
}

/// Applies `patch` to a copy of `tree` the way a document store does: each change must find its `before` and sets
/// or removes the value under a parent that already exists (the root path replaces the whole tree). The model is
/// `set()` in `omuzaki-doc-mock`, which this crate can't depend on.
fn apply(tree: &Value, patch: &Patch) -> Value {
    let mut tree = tree.clone();
    for change in &patch.changes {
        assert_eq!(lookup(&tree, &change.path), change.before.as_ref(), "stale `before` at {}", change.path);
        set(&mut tree, &change.path, change.after.clone());
    }
    tree
}

fn set(root: &mut Value, at: &DocPath, value: Option<Value>) {
    let Some((parent, last)) = at.split_last() else {
        *root = value.unwrap();
        return;
    };
    let parent = parent.segments().iter().try_fold(root, |node, seg| node.get_mut(seg.as_str()));
    let Some(Value::Object(map)) = parent else { panic!("the parent of {at} is not an existing object") };
    match value {
        Some(v) => map.insert(last.to_owned(), v),
        None => map.remove(last),
    };
}

/// Resolves every conflict to `side` and applies the patch to `ours`.
fn merged(m: &Merge3, ours: &Value, side: Side) -> Value {
    apply(ours, &m.patch(ours, |_| Some(side)).unwrap())
}

// --- the rows of design §7.2 -------------------------------------------------------------------------------------

/// `{"keep": "same", "k": value}`, with `k` left out when absent: the key under test next to an untouched one.
fn doc(k: Option<&str>) -> Value {
    let mut map = Map::new();
    map.insert("keep".to_owned(), json!("same"));
    if let Some(k) = k {
        map.insert("k".to_owned(), json!(k));
    }
    Value::Object(map)
}

fn row_entry(base: Option<&str>, ours: Option<&str>, theirs: Option<&str>, outcome: Outcome) -> MergeEntry {
    let value = |v: Option<&str>| v.map(|s| json!(s));
    MergeEntry { path: path("k"), base: value(base), ours: value(ours), theirs: value(theirs), outcome }
}

/// A row that resolves by itself: exactly one entry at `k`, no conflict, and `merged` as the value of `k` after.
fn clean_row(base: Option<&str>, ours: Option<&str>, theirs: Option<&str>, outcome: Outcome, merged: Option<&str>) {
    let (b, o, t) = (doc(base), doc(ours), doc(theirs));
    let m = merge3(Some(&b), &o, &t);
    assert_eq!(m.entries, vec![row_entry(base, ours, theirs, outcome)]);
    assert_eq!(m.conflicts().count(), 0);
    let patch = m.patch(&o, |_| None).unwrap();
    assert_eq!(patch.changes.len(), usize::from(outcome == Outcome::Theirs), "only taking theirs changes ours");
    assert_eq!(apply(&o, &patch), doc(merged));
}

/// A row that needs a choice: one conflict at `k`, refused without a side, and each side gives that side's tree.
fn conflict_row(base: Option<&str>, ours: Option<&str>, theirs: Option<&str>) {
    let (b, o, t) = (doc(base), doc(ours), doc(theirs));
    let m = merge3(Some(&b), &o, &t);
    assert_eq!(m.entries, vec![row_entry(base, ours, theirs, Outcome::Conflict)]);
    assert_eq!(m.conflicts().count(), 1);
    assert_eq!(m.patch(&o, |_| None).unwrap_err(), Unresolved { paths: vec![path("k")] });
    assert_eq!(merged(&m, &o, Side::Ours), o);
    assert_eq!(merged(&m, &o, Side::Theirs), t);
}

/// A row where nothing differs: no entry, and the merge is ours unchanged.
fn quiet_row(value: Option<&str>) {
    let d = doc(value);
    let m = merge3(Some(&d), &d, &d);
    assert_eq!(m.entries, Vec::<MergeEntry>::new());
    assert!(m.patch(&d, |_| None).unwrap().is_empty());
}

#[test]
fn row_they_changed_it_takes_theirs() {
    clean_row(Some("x"), Some("x"), Some("y"), Outcome::Theirs, Some("y"));
}

#[test]
fn row_i_changed_it_keeps_ours() {
    clean_row(Some("x"), Some("y"), Some("x"), Outcome::Ours, Some("y"));
}

#[test]
fn row_same_change_on_both_sides_is_same() {
    clean_row(Some("x"), Some("y"), Some("y"), Outcome::Same, Some("y"));
}

#[test]
fn row_different_changes_conflict() {
    conflict_row(Some("x"), Some("y"), Some("z"));
}

#[test]
fn row_they_added_it_takes_theirs() {
    clean_row(None, None, Some("y"), Outcome::Theirs, Some("y"));
}

#[test]
fn row_both_added_the_same_is_same() {
    clean_row(None, Some("y"), Some("y"), Outcome::Same, Some("y"));
}

#[test]
fn row_both_added_different_values_conflict() {
    conflict_row(None, Some("y"), Some("z"));
}

#[test]
fn row_i_removed_it_keeps_it_removed() {
    clean_row(Some("x"), None, Some("x"), Outcome::Ours, None);
}

#[test]
fn row_i_removed_what_they_changed_conflicts() {
    conflict_row(Some("x"), None, Some("y"));
}

#[test]
fn row_they_removed_what_i_changed_conflicts() {
    conflict_row(Some("x"), Some("y"), None);
}

#[test]
fn row_they_removed_it_takes_the_removal() {
    clean_row(Some("x"), Some("x"), None, Outcome::Theirs, None);
}

#[test]
fn row_i_added_it_keeps_it() {
    clean_row(None, Some("y"), None, Outcome::Ours, Some("y"));
}

#[test]
fn row_both_removed_it_is_same() {
    clean_row(Some("x"), None, None, Outcome::Same, None);
}

#[test]
fn row_nothing_changed_has_no_entry() {
    quiet_row(Some("x"));
    quiet_row(None);
}

// --- removed subtrees -------------------------------------------------------------------------------------------

fn hat_base() -> Value {
    json!({"parts": {"hat": {"shape": "round", "trim": {"colour": "red", "width": 1}}, "scarf": {"len": 2}}})
}

fn without_hat(mut tree: Value) -> Value {
    tree["parts"].as_object_mut().unwrap().remove("hat");
    tree
}

fn with_blue_trim(mut tree: Value) -> Value {
    tree["parts"]["hat"]["trim"]["colour"] = json!("blue");
    tree
}

fn subtree_entry(base: &Value, ours: Option<Value>, theirs: Option<Value>, outcome: Outcome) -> MergeEntry {
    MergeEntry { path: path("parts/hat"), base: Some(base["parts"]["hat"].clone()), ours, theirs, outcome }
}

#[test]
fn a_subtree_removed_here_and_changed_there_is_one_conflict_at_its_root() {
    let base = hat_base();
    let (ours, theirs) = (without_hat(base.clone()), with_blue_trim(base.clone()));
    let m = merge3(Some(&base), &ours, &theirs);
    let whole = Some(theirs["parts"]["hat"].clone());
    assert_eq!(m.entries, vec![subtree_entry(&base, None, whole, Outcome::Conflict)]);
    assert_eq!(merged(&m, &ours, Side::Ours), ours);
    assert_eq!(merged(&m, &ours, Side::Theirs), theirs);
}

#[test]
fn a_subtree_changed_here_and_removed_there_is_one_conflict_at_its_root() {
    let base = hat_base();
    let (ours, theirs) = (with_blue_trim(base.clone()), without_hat(base.clone()));
    let m = merge3(Some(&base), &ours, &theirs);
    let whole = Some(ours["parts"]["hat"].clone());
    assert_eq!(m.entries, vec![subtree_entry(&base, whole, None, Outcome::Conflict)]);
    assert_eq!(merged(&m, &ours, Side::Ours), ours);
    assert_eq!(merged(&m, &ours, Side::Theirs), theirs);
}

#[test]
fn a_subtree_removed_on_one_side_and_untouched_on_the_other_is_a_clean_removal() {
    let base = hat_base();
    let removed = without_hat(base.clone());

    let m = merge3(Some(&base), &removed, &base);
    assert_eq!(m.entries, vec![subtree_entry(&base, None, Some(base["parts"]["hat"].clone()), Outcome::Ours)]);
    assert!(m.patch(&removed, |_| None).unwrap().is_empty());

    let m = merge3(Some(&base), &base, &removed);
    assert_eq!(m.entries, vec![subtree_entry(&base, Some(base["parts"]["hat"].clone()), None, Outcome::Theirs)]);
    let patch = m.patch(&base, |_| None).unwrap();
    assert_eq!(patch.changes.len(), 1, "one removal, not one per key under the subtree");
    assert_eq!(patch.changes[0].path, path("parts/hat"));
    assert_eq!(patch.changes[0].before.as_ref(), Some(&base["parts"]["hat"]));
    assert_eq!(patch.changes[0].after, None);
    assert_eq!(apply(&base, &patch), removed);
}

#[test]
fn a_subtree_removed_on_both_sides_is_same() {
    let base = hat_base();
    let removed = without_hat(base.clone());
    let m = merge3(Some(&base), &removed, &removed);
    assert_eq!(m.entries, vec![subtree_entry(&base, None, None, Outcome::Same)]);
    assert!(m.patch(&removed, |_| None).unwrap().is_empty());
}

// --- leaves -----------------------------------------------------------------------------------------------------

#[test]
fn arrays_are_leaves_so_element_edits_on_both_sides_conflict_at_the_array() {
    let base = json!({"look": {"tags": ["a", "b", "c"]}});
    let ours = json!({"look": {"tags": ["x", "b", "c"]}});
    let theirs = json!({"look": {"tags": ["a", "b", "y"]}});
    let m = merge3(Some(&base), &ours, &theirs);
    assert_eq!(m.entries.len(), 1);
    let e = &m.entries[0];
    assert_eq!(e.path, path("look/tags"));
    assert_eq!(e.outcome, Outcome::Conflict);
    assert_eq!(e.ours, Some(json!(["x", "b", "c"])));
    assert_eq!(e.theirs, Some(json!(["a", "b", "y"])));
    assert_eq!(merged(&m, &ours, Side::Theirs), theirs, "whole arrays, never an element-wise blend");
}

#[test]
fn a_scalar_base_with_different_objects_on_both_sides_is_one_conflict_there() {
    let base = json!({"k": 1, "keep": true});
    let ours = json!({"k": {"a": 1}, "keep": true});
    let theirs = json!({"k": {"a": 2}, "keep": true});
    let m = merge3(Some(&base), &ours, &theirs);
    assert_eq!(m.entries.len(), 1);
    assert_eq!((&m.entries[0].path, m.entries[0].outcome), (&path("k"), Outcome::Conflict));
    assert_eq!(merged(&m, &ours, Side::Theirs), theirs);

    // The same object on both sides is the same change, reported whole.
    let m = merge3(Some(&base), &ours, &ours);
    assert_eq!(m.entries.len(), 1);
    assert_eq!((&m.entries[0].path, m.entries[0].outcome), (&path("k"), Outcome::Same));
}

#[test]
fn a_deep_change_against_an_object_replaced_by_a_scalar_is_one_conflict_at_the_object() {
    let base = json!({"k": {"a": {"b": 1}}, "keep": true});
    let deep = json!({"k": {"a": {"b": 2}}, "keep": true});
    let scalar = json!({"k": 5, "keep": true});

    let m = merge3(Some(&base), &deep, &scalar);
    assert_eq!(m.entries.len(), 1);
    assert_eq!((&m.entries[0].path, m.entries[0].outcome), (&path("k"), Outcome::Conflict));
    assert_eq!(m.entries[0].ours, Some(json!({"a": {"b": 2}})));
    assert_eq!(merged(&m, &deep, Side::Theirs), scalar);

    let m = merge3(Some(&base), &scalar, &deep);
    assert_eq!(m.entries.len(), 1);
    assert_eq!((&m.entries[0].path, m.entries[0].outcome), (&path("k"), Outcome::Conflict));
    assert_eq!(merged(&m, &scalar, Side::Theirs), deep);
}

#[test]
fn trees_that_are_not_objects_at_the_root_compare_whole() {
    let m = merge3(Some(&json!(1)), &json!(1), &json!(2));
    assert_eq!(
        m.entries,
        vec![MergeEntry {
            path: DocPath::root(),
            base: Some(json!(1)),
            ours: Some(json!(1)),
            theirs: Some(json!(2)),
            outcome: Outcome::Theirs,
        }]
    );
    let patch = m.patch(&json!(1), |_| None).unwrap();
    assert_eq!(patch.changes.len(), 1);
    assert_eq!(patch.changes[0].path, DocPath::root());
    assert_eq!(apply(&json!(1), &patch), json!(2));

    let m = merge3(Some(&json!(1)), &json!(2), &json!(3));
    assert_eq!(m.conflicts().count(), 1);
    assert_eq!(merged(&m, &json!(2), Side::Theirs), json!(3));

    // An object at the root on one side and a scalar on the other can't be merged key by key either.
    let m = merge3(Some(&json!({"a": 1})), &json!({"a": 2}), &json!(5));
    assert_eq!(m.entries.len(), 1);
    assert_eq!((&m.entries[0].path, m.entries[0].outcome), (&DocPath::root(), Outcome::Conflict));
}

// --- key by key -------------------------------------------------------------------------------------------------

#[test]
fn edits_to_different_keys_under_one_object_combine() {
    let base = json!({"o": {"a": 1, "b": 2, "c": 3}});
    let ours = json!({"o": {"a": 10, "b": 2, "c": 3}});
    let theirs = json!({"o": {"a": 1, "b": 2, "c": 30}});
    let m = merge3(Some(&base), &ours, &theirs);
    let outcomes: Vec<_> = m.entries.iter().map(|e| (e.path.to_string(), e.outcome)).collect();
    assert_eq!(outcomes, vec![("o/a".to_owned(), Outcome::Ours), ("o/c".to_owned(), Outcome::Theirs)]);
    assert_eq!(merged(&m, &ours, Side::Ours), json!({"o": {"a": 10, "b": 2, "c": 30}}));
}

#[test]
fn objects_added_on_both_sides_merge_key_by_key() {
    let base = json!({"keep": true});
    let ours = json!({"keep": true, "n": {"a": 1, "same": 0, "both": 7}});
    let theirs = json!({"keep": true, "n": {"b": 2, "same": 0, "both": 8}});
    let m = merge3(Some(&base), &ours, &theirs);
    let outcomes: Vec<_> = m.entries.iter().map(|e| (e.path.to_string(), e.outcome)).collect();
    assert_eq!(
        outcomes,
        vec![
            ("n/a".to_owned(), Outcome::Ours),
            ("n/b".to_owned(), Outcome::Theirs),
            ("n/both".to_owned(), Outcome::Conflict),
            ("n/same".to_owned(), Outcome::Same),
        ]
    );
    assert_eq!(merged(&m, &ours, Side::Ours), json!({"keep": true, "n": {"a": 1, "b": 2, "same": 0, "both": 7}}));
    assert_eq!(merged(&m, &ours, Side::Theirs), json!({"keep": true, "n": {"a": 1, "b": 2, "same": 0, "both": 8}}));
}

#[test]
fn entries_come_out_in_path_order_whatever_order_the_maps_hold() {
    let base = json!({"z": 1, "m": {"y": 1, "b": 1}, "a": 1});
    let ours = json!({"z": 2, "m": {"y": 2, "b": 2}, "a": 2, "q": 1});
    let theirs = json!({"z": 3, "m": {"y": 3, "b": 3}, "a": 3, "c": 1});
    let m = merge3(Some(&base), &ours, &theirs);
    let paths: Vec<_> = m.entries.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(paths, ["a", "c", "m/b", "m/y", "q", "z"]);
    assert!(m.entries.windows(2).all(|w| w[0].path < w[1].path));
}

/// One `Same` entry at `at`, holding `value` on both sides over nothing in the base.
fn added_alike(at: &str, value: Value) -> MergeEntry {
    MergeEntry { path: path(at), base: None, ours: Some(value.clone()), theirs: Some(value), outcome: Outcome::Same }
}

#[test]
fn an_empty_object_added_on_both_sides_is_one_same_entry() {
    let base = json!({});
    let both = json!({"k": {}});
    let m = merge3(Some(&base), &both, &both);
    assert_eq!(m.entries, vec![added_alike("k", json!({}))], "recursing into two empty objects would report nothing");
    assert_eq!(m.conflicts().count(), 0);
    assert!(m.patch(&both, |_| None).unwrap().is_empty());
    assert_eq!(merged(&m, &both, Side::Theirs), both);
}

#[test]
fn a_non_empty_object_added_alike_on_both_sides_is_one_same_entry_at_its_root() {
    let base = json!({"keep": 1});
    let added = json!({"a": 1, "b": {"c": [2, 3], "d": {}}});
    let both = json!({"keep": 1, "n": added});
    let m = merge3(Some(&base), &both, &both);
    assert_eq!(m.entries, vec![added_alike("n", added)], "one entry for the addition, not one per leaf");
    assert!(m.patch(&both, |_| None).unwrap().is_empty());
}

#[test]
fn an_identical_object_inside_an_object_added_differently_is_still_one_entry_at_its_own_root() {
    let base = json!({});
    let ours = json!({"n": {"same": {"x": 1, "y": {}}, "k": 1}});
    let theirs = json!({"n": {"same": {"x": 1, "y": {}}, "k": 2}});
    let m = merge3(Some(&base), &ours, &theirs);
    let found: Vec<_> = m.entries.iter().map(|e| (e.path.to_string(), e.outcome)).collect();
    assert_eq!(found, vec![("n/k".to_owned(), Outcome::Conflict), ("n/same".to_owned(), Outcome::Same)]);
    assert_eq!(m.entries[1], added_alike("n/same", json!({"x": 1, "y": {}})));
}

#[test]
fn an_object_the_base_also_had_is_never_an_entry_itself() {
    // Both sides filled the same object the base had empty: the entries are its keys, and the object has none.
    let base = json!({"k": {}});
    let both = json!({"k": {"a": 1, "b": {}}});
    let m = merge3(Some(&base), &both, &both);
    assert_eq!(m.entries, vec![added_alike("k/a", json!(1)), added_alike("k/b", json!({}))]);

    // Both sides emptied it: one entry per removed key, none for the object.
    let m = merge3(Some(&both), &base, &base);
    let paths: Vec<_> = m.entries.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(paths, ["k/a", "k/b"]);
    assert!(m.entries.iter().all(|e| e.outcome == Outcome::Same));
}

#[test]
fn an_object_added_on_both_sides_with_no_base_at_all_is_not_an_entry_when_the_trees_are_equal() {
    let tree = json!({"k": {}});
    assert_eq!(merge3(None, &tree, &tree).entries, Vec::<MergeEntry>::new());
}

// --- no base ----------------------------------------------------------------------------------------------------

#[test]
fn without_a_base_every_difference_is_a_conflict() {
    let ours = json!({"a": 1, "b": {"x": 1, "y": 2}, "only_ours": true});
    let theirs = json!({"a": 1, "b": {"x": 1, "y": 3}, "only_theirs": 5});
    let m = merge3(None, &ours, &theirs);

    let found: Vec<_> = m.entries.iter().map(|e| (e.path.to_string(), e.ours.clone(), e.theirs.clone())).collect();
    assert_eq!(
        found,
        vec![
            ("b/y".to_owned(), Some(json!(2)), Some(json!(3))),
            ("only_ours".to_owned(), Some(json!(true)), None),
            ("only_theirs".to_owned(), None, Some(json!(5))),
        ],
        "equal values (a, b/x) aren't listed and nested objects still split per key"
    );
    assert!(m.entries.iter().all(|e| e.outcome == Outcome::Conflict && e.base.is_none()));
    assert_eq!(merged(&m, &ours, Side::Ours), ours);
    assert_eq!(merged(&m, &ours, Side::Theirs), theirs);
}

#[test]
fn without_a_base_equal_trees_have_no_entries() {
    let tree = json!({"a": {"b": [1, 2]}, "c": null});
    assert_eq!(merge3(None, &tree, &tree).entries, Vec::<MergeEntry>::new());
    assert_eq!(merge3(None, &json!(1), &json!(1)).entries, Vec::<MergeEntry>::new());

    let m = merge3(None, &json!(1), &json!(2));
    assert_eq!(m.entries.len(), 1);
    assert_eq!((&m.entries[0].path, m.entries[0].outcome), (&DocPath::root(), Outcome::Conflict));
}

// --- patch ------------------------------------------------------------------------------------------------------

fn four_way() -> (Value, Value, Value) {
    let base = json!({"a": 1, "b": 1, "c": 1, "d": 1, "keep": 0});
    let ours = json!({"a": 2, "b": 1, "c": 2, "d": 2, "keep": 0});
    let theirs = json!({"a": 1, "b": 2, "c": 3, "d": 2, "keep": 0});
    (base, ours, theirs)
}

#[test]
fn patch_takes_each_clean_outcome_and_the_chosen_side_for_each_conflict() {
    let (base, ours, theirs) = four_way();
    let m = merge3(Some(&base), &ours, &theirs);
    let outcomes: Vec<_> = m.entries.iter().map(|e| e.outcome).collect();
    assert_eq!(outcomes, [Outcome::Ours, Outcome::Theirs, Outcome::Conflict, Outcome::Same]);

    let choose_theirs = |p: &DocPath| (p == &path("c")).then_some(Side::Theirs);
    let patch = m.patch(&ours, choose_theirs).unwrap();
    let changed: Vec<_> = patch.changes.iter().map(|c| c.path.to_string()).collect();
    assert_eq!(changed, ["b", "c"]);
    assert_eq!(apply(&ours, &patch), json!({"a": 2, "b": 2, "c": 3, "d": 2, "keep": 0}));

    let choose_ours = |p: &DocPath| (p == &path("c")).then_some(Side::Ours);
    let patch = m.patch(&ours, choose_ours).unwrap();
    assert_eq!(apply(&ours, &patch), json!({"a": 2, "b": 2, "c": 2, "d": 2, "keep": 0}));
}

#[test]
fn patch_changes_expect_exactly_ours_so_they_apply_only_to_the_state_computed_on() {
    let (base, ours, theirs) = four_way();
    let m = merge3(Some(&base), &ours, &theirs);
    let patch = m.patch(&ours, |_| Some(Side::Theirs)).unwrap();
    assert_ne!(patch.changes.len(), 0);
    for change in &patch.changes {
        assert_eq!(change.before.as_ref(), lookup(&ours, &change.path), "before at {}", change.path);
    }
    // Applying it to a tree that has since moved on is refused by the `before` check, not applied over the edit.
    let moved_on = json!({"a": 2, "b": 99, "c": 2, "d": 2, "keep": 0});
    assert!(patch.changes.iter().any(|c| lookup(&moved_on, &c.path) != c.before.as_ref()));
}

#[test]
fn patch_skips_values_the_tree_already_holds() {
    let (base, ours, theirs) = four_way();
    let m = merge3(Some(&base), &ours, &theirs);
    assert!(m.patch(&theirs, |_| Some(Side::Theirs)).unwrap().is_empty(), "nothing to write when ours is theirs");
}

#[test]
fn patch_lists_only_the_unchosen_conflicts_in_path_order() {
    let base = json!({"a": 0, "b": 0, "c": 0, "d": 0});
    let ours = json!({"a": 1, "b": 1, "c": 1, "d": 0});
    let theirs = json!({"a": 2, "b": 2, "c": 2, "d": 2});
    let m = merge3(Some(&base), &ours, &theirs);
    assert_eq!(m.conflicts().count(), 3);

    let unresolved = m.patch(&ours, |p| (p == &path("b")).then_some(Side::Ours)).unwrap_err();
    assert_eq!(unresolved.paths, [path("a"), path("c")]);

    let all = m.patch(&ours, |_| None).unwrap_err();
    assert_eq!(all.paths, [path("a"), path("b"), path("c")]);
}

#[test]
fn unresolved_names_the_paths_in_its_message() {
    let unresolved = Unresolved { paths: vec![path("parts/hat"), path("look/tags")] };
    assert_eq!(unresolved.to_string(), "2 merge conflict(s) have no side chosen: parts/hat look/tags");
}

#[test]
fn types_round_trip_through_json_with_snake_case_names() {
    let (base, ours, theirs) = four_way();
    let m = merge3(Some(&base), &ours, &theirs);
    let text = serde_json::to_string(&m).unwrap();
    assert!(text.contains(r#""outcome":"conflict""#), "{text}");
    assert_eq!(serde_json::from_str::<Merge3>(&text).unwrap(), m);
    assert_eq!(serde_json::to_string(&Side::Theirs).unwrap(), r#""theirs""#);
}

// --- null is not absent ------------------------------------------------------------------------------------------

/// The merge after a trip through JSON text, which must be the merge it started as.
fn round_trip(m: &Merge3) -> Merge3 {
    let text = serde_json::to_string(m).unwrap();
    let back: Merge3 = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, m, "lost in transit: {text}");
    back
}

/// Absent, null, and a value: the three states each of base, ours and theirs can hold.
fn slot_states() -> [Option<Value>; 3] {
    [None, Some(Value::Null), Some(json!({"v": [1, null]}))]
}

#[test]
fn an_entry_keeps_absent_null_and_a_value_apart_through_json_in_every_slot() {
    for base in slot_states() {
        for ours in slot_states() {
            for theirs in slot_states() {
                let entry = MergeEntry {
                    path: path("k"),
                    base: base.clone(),
                    ours: ours.clone(),
                    theirs: theirs.clone(),
                    outcome: Outcome::Conflict,
                };
                let wire = serde_json::to_value(&entry).unwrap();
                for (name, slot) in [("base", &base), ("ours", &ours), ("theirs", &theirs)] {
                    assert_eq!(wire.get(name), slot.as_ref(), "{name} on the wire in {wire}");
                }
                assert_eq!(serde_json::from_value::<MergeEntry>(wire).unwrap(), entry);
                let text = serde_json::to_string(&entry).unwrap();
                assert_eq!(serde_json::from_str::<MergeEntry>(&text).unwrap(), entry, "{text}");
            }
        }
    }
}

#[test]
fn an_entry_reads_a_missing_field_as_absent_and_a_null_field_as_null() {
    let text = r#"{"path":"k","base":null,"theirs":3,"outcome":"theirs"}"#;
    let entry: MergeEntry = serde_json::from_str(text).unwrap();
    assert_eq!((entry.base, entry.ours, entry.theirs), (Some(Value::Null), None, Some(json!(3))));
}

#[test]
fn a_value_set_to_null_by_theirs_is_set_not_removed_after_a_json_round_trip() {
    let base = json!({"k": 1});
    let ours = json!({"k": 2});
    let theirs = json!({"k": null});
    let m = round_trip(&merge3(Some(&base), &ours, &theirs));
    assert_eq!(m.entries.len(), 1);
    assert_eq!((&m.entries[0].path, m.entries[0].outcome), (&path("k"), Outcome::Conflict));
    assert_eq!(m.entries[0].theirs, Some(Value::Null));

    let patch = m.patch(&ours, |_| Some(Side::Theirs)).unwrap();
    assert_eq!(patch.changes.len(), 1);
    assert_eq!(patch.changes[0].path, path("k"));
    assert_eq!(patch.changes[0].before, Some(json!(2)));
    assert_eq!(patch.changes[0].after, Some(Value::Null), "set to null, not removed");
    assert_eq!(apply(&ours, &patch), theirs);
}

#[test]
fn a_clean_take_of_null_survives_a_json_round_trip_too() {
    let base = json!({"k": 1});
    let theirs = json!({"k": null});
    let m = round_trip(&merge3(Some(&base), &base, &theirs));
    assert_eq!(m.entries[0].outcome, Outcome::Theirs);
    let patch = m.patch(&base, |_| None).unwrap();
    assert_eq!(patch.changes[0].after, Some(Value::Null));
    assert_eq!(apply(&base, &patch), theirs);
}

#[test]
fn a_null_base_is_a_base_value_not_a_missing_one_after_a_json_round_trip() {
    let base = json!({"k": null});
    let ours = json!({"k": 1});

    let theirs = json!({"k": 2});
    let m = round_trip(&merge3(Some(&base), &ours, &theirs));
    assert_eq!(m.entries[0].base, Some(Value::Null), "a null in the base, not an addition");
    let patch = m.patch(&ours, |_| Some(Side::Theirs)).unwrap();
    assert_eq!(apply(&ours, &patch), theirs);

    // Theirs removed the key the base held as null; ours changed it: a conflict whose base is still null.
    let removed = json!({});
    let m = round_trip(&merge3(Some(&base), &ours, &removed));
    assert_eq!((m.entries[0].base.clone(), m.entries[0].theirs.clone()), (Some(Value::Null), None));
    let patch = m.patch(&ours, |_| Some(Side::Theirs)).unwrap();
    assert_eq!(patch.changes[0].after, None, "an absent theirs still removes");
    assert_eq!(apply(&ours, &patch), removed);

    // Theirs changed null to a value while ours kept null.
    let m = round_trip(&merge3(Some(&base), &base, &theirs));
    assert_eq!((m.entries[0].base.clone(), m.entries[0].ours.clone()), (Some(Value::Null), Some(Value::Null)));
    assert_eq!(apply(&base, &m.patch(&base, |_| None).unwrap()), theirs);
}

#[test]
fn a_whole_document_of_null_round_trips() {
    let m = round_trip(&merge3(Some(&Value::Null), &Value::Null, &json!(5)));
    assert_eq!(m.entries.len(), 1);
    assert_eq!((&m.entries[0].path, m.entries[0].outcome), (&DocPath::root(), Outcome::Theirs));
    let patch = m.patch(&Value::Null, |_| None).unwrap();
    assert_eq!(apply(&Value::Null, &patch), json!(5));

    let m = round_trip(&merge3(Some(&json!(1)), &json!(2), &Value::Null));
    assert_eq!(m.entries[0].theirs, Some(Value::Null));
    let patch = m.patch(&json!(2), |_| Some(Side::Theirs)).unwrap();
    assert_eq!(patch.changes[0].after, Some(Value::Null));
}

#[test]
fn the_schema_leaves_base_ours_and_theirs_optional_and_lets_a_present_one_be_null() {
    let schema = serde_json::to_value(schemars::schema_for!(MergeEntry)).unwrap();
    let required: BTreeSet<_> = schema["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(required, BTreeSet::from(["path", "outcome"]), "{schema}");
    for name in ["base", "ours", "theirs"] {
        let field = &schema["properties"][name];
        // Any JSON value passes: no `type`, `anyOf`, `oneOf` or `enum` narrows it (a bare `true` or `{}` accepts all).
        let unconstrained = match field {
            Value::Bool(true) => true,
            Value::Object(keys) => keys.keys().all(|k| matches!(k.as_str(), "description" | "title")),
            _ => false,
        };
        assert!(unconstrained, "{name} must accept any JSON value, null included: {field}");
    }
}

// --- properties (design §12.1) ----------------------------------------------------------------------------------

fn config() -> Config {
    let mut config = Config { failure_persistence: None, ..Config::default() };
    if matches!(config.rng_seed, RngSeed::Random) {
        config.rng_seed = RngSeed::Fixed(0x5EED);
    }
    config
}

/// A small alphabet makes the three trees collide on the same paths, which is where merges go wrong.
fn key() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["a", "b", "c"]).prop_map(str::to_owned)
}

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        (0i64..3).prop_map(Value::from),
        Just(Value::Null),
        prop::collection::vec((0i64..3).prop_map(Value::from), 0..3).prop_map(Value::Array),
    ]
}

/// Leaves and objects nested about three deep; the root may be a leaf.
fn tree() -> impl Strategy<Value = Value> {
    leaf().prop_recursive(3, 24, 3, |inner| {
        prop::collection::btree_map(key(), inner, 0..4).prop_map(|m| Value::Object(m.into_iter().collect()))
    })
}

fn object() -> impl Strategy<Value = Value> {
    prop::collection::btree_map(key(), tree(), 0..4).prop_map(|m| Value::Object(m.into_iter().collect()))
}

/// Sets (`Some`) or removes (`None`) the value at `path`, a path of keys.
#[derive(Debug, Clone)]
struct Edit {
    path: Vec<String>,
    value: Option<Value>,
}

fn edits() -> impl Strategy<Value = Vec<Edit>> {
    let edit = (prop::collection::vec(key(), 1..4), prop::option::weighted(0.7, tree()))
        .prop_map(|(path, value)| Edit { path, value });
    prop::collection::vec(edit, 0..5)
}

/// An object, forced into being if `node` isn't one.
fn object_mut(node: &mut Value) -> &mut Map<String, Value> {
    if !node.is_object() {
        *node = json!({});
    }
    node.as_object_mut().unwrap()
}

/// A set makes the objects it needs on the way (replacing anything that isn't one); a remove never creates or
/// changes anything but the value it removes.
fn edit_at(node: &mut Value, path: &[String], value: Option<Value>) {
    let Some((key, rest)) = path.split_first() else { return };
    if rest.is_empty() {
        match value {
            Some(v) => {
                object_mut(node).insert(key.clone(), v);
            }
            None => {
                if let Value::Object(map) = node {
                    map.remove(key);
                }
            }
        }
    } else if value.is_some() || node.get(key.as_str()).is_some() {
        let child = object_mut(node).entry(key.clone()).or_insert_with(|| json!({}));
        edit_at(child, rest, value);
    }
}

fn edited(base: &Value, edits: &[Edit]) -> Value {
    let mut tree = base.clone();
    for edit in edits {
        edit_at(&mut tree, &edit.path, edit.value.clone());
    }
    tree
}

/// Mostly a base and two sets of edits to it, so the trees share structure; sometimes three unrelated trees.
fn triple() -> impl Strategy<Value = (Value, Value, Value)> {
    prop_oneof![
        4 => (object(), edits(), edits()).prop_map(|(base, e1, e2)| {
            let (ours, theirs) = (edited(&base, &e1), edited(&base, &e2));
            (base, ours, theirs)
        }),
        1 => (tree(), tree(), tree()),
    ]
}

/// Every object-key path in `tree` (the root included), arrays being leaves the walk never enters.
fn key_paths(tree: &Value, at: &DocPath, out: &mut BTreeSet<DocPath>) {
    out.insert(at.clone());
    if let Value::Object(map) = tree {
        for (key, child) in map {
            key_paths(child, &at.join(key.as_str()), out);
        }
    }
}

/// True where `merge3` (given a base) doesn't recurse: anything but two objects over an object, or over nothing when
/// they differ. Two equal objects over nothing are an addition both sides made, compared whole.
fn compared_whole(base: Option<&Value>, ours: Option<&Value>, theirs: Option<&Value>) -> bool {
    let both_objects = matches!((ours, theirs), (Some(Value::Object(_)), Some(Value::Object(_))));
    let recurses = both_objects
        && match base {
            Some(Value::Object(_)) => true,
            None => ours != theirs,
            Some(_) => false,
        };
    !recurses
}

fn covered_by_conflict(m: &Merge3, at: &DocPath) -> bool {
    m.conflicts().any(|e| at.starts_with(&e.path))
}

fn swapped(outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Ours => Outcome::Theirs,
        Outcome::Theirs => Outcome::Ours,
        other => other,
    }
}

/// Resolving every conflict either way, wherever one side changed a path nothing conflicts at, the result holds that
/// side's value there; and where all three differ, a conflict covers it.
fn check_no_silent_loss(base: &Value, ours: &Value, theirs: &Value) -> Result<(), TestCaseError> {
    let m = merge3(Some(base), ours, theirs);
    let mut paths = BTreeSet::new();
    for tree in [base, ours, theirs] {
        key_paths(tree, &DocPath::root(), &mut paths);
    }
    for at in &paths {
        let (b, o, t) = (lookup(base, at), lookup(ours, at), lookup(theirs, at));
        if !compared_whole(b, o, t) {
            continue;
        }
        let covered = covered_by_conflict(&m, at);
        if b != o && b != t && o != t {
            prop_assert!(covered, "all three differ at {at} with no conflict at it or above");
        }
        if covered {
            continue;
        }
        for side in [Side::Ours, Side::Theirs] {
            let result = merged(&m, ours, side);
            if t != b {
                prop_assert_eq!(lookup(&result, at), t, "their change at {} lost (conflicts to {:?})", at, side);
            }
            if o != b {
                prop_assert_eq!(lookup(&result, at), o, "our change at {} lost (conflicts to {:?})", at, side);
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    /// Only ours changed anything: nothing to decide, and the result is ours.
    #[test]
    fn merging_an_unchanged_theirs_keeps_ours((base, ours, _) in triple()) {
        let m = merge3(Some(&base), &ours, &base);
        prop_assert_eq!(m.conflicts().count(), 0);
        prop_assert!(m.entries.iter().all(|e| e.outcome == Outcome::Ours));
        prop_assert_eq!(merged(&m, &ours, Side::Theirs), ours);
    }

    /// Only theirs changed anything: nothing to decide, and the result is theirs.
    #[test]
    fn merging_into_an_unchanged_ours_takes_theirs((base, _, theirs) in triple()) {
        let m = merge3(Some(&base), &base, &theirs);
        prop_assert_eq!(m.conflicts().count(), 0);
        prop_assert!(m.entries.iter().all(|e| e.outcome == Outcome::Theirs));
        let patch = m.patch(&base, |_| None).unwrap();
        prop_assert_eq!(apply(&base, &patch), theirs);
    }

    /// Both made the same change: no conflict, and the result is that change.
    #[test]
    fn identical_changes_on_both_sides_are_not_a_conflict((base, same, _) in triple()) {
        let m = merge3(Some(&base), &same, &same);
        prop_assert_eq!(m.conflicts().count(), 0);
        prop_assert!(m.entries.iter().all(|e| e.outcome == Outcome::Same));
        prop_assert_eq!(merged(&m, &same, Side::Theirs), same);
    }

    /// Whatever both sides added where the base has nothing is reported, at its root or above it, empty objects
    /// included.
    #[test]
    fn an_identical_addition_is_always_reported((base, same, _) in triple()) {
        let m = merge3(Some(&base), &same, &same);
        let mut paths = BTreeSet::new();
        key_paths(&same, &DocPath::root(), &mut paths);
        for at in paths.iter().filter(|at| lookup(&base, at).is_none()) {
            prop_assert!(m.entries.iter().any(|e| at.starts_with(&e.path)), "the addition at {} is not reported", at);
        }
    }

    /// Swapping mine and theirs swaps the sides of every entry and leaves the clean results equal.
    #[test]
    fn swapping_ours_and_theirs_swaps_the_sides((base, ours, theirs) in triple()) {
        let m1 = merge3(Some(&base), &ours, &theirs);
        let m2 = merge3(Some(&base), &theirs, &ours);
        prop_assert_eq!(m1.entries.len(), m2.entries.len());
        for (a, b) in m1.entries.iter().zip(&m2.entries) {
            prop_assert_eq!(&a.path, &b.path);
            prop_assert_eq!(&a.base, &b.base);
            prop_assert_eq!(&a.ours, &b.theirs);
            prop_assert_eq!(&a.theirs, &b.ours);
            prop_assert_eq!(swapped(a.outcome), b.outcome);
        }
        // With every conflict going to the same tree, the two merges agree.
        prop_assert_eq!(merged(&m1, &ours, Side::Ours), merged(&m2, &theirs, Side::Theirs));
        prop_assert_eq!(merged(&m1, &ours, Side::Theirs), merged(&m2, &theirs, Side::Ours));
    }

    /// Where both sides changed a path differently a conflict covers it, and a change on one side is never lost
    /// outside a conflict.
    #[test]
    fn no_change_is_silently_lost((base, ours, theirs) in triple()) {
        check_no_silent_loss(&base, &ours, &theirs)?;
    }

    /// A chosen side wins exactly at the conflict paths.
    #[test]
    fn a_chosen_side_wins_at_every_conflict((base, ours, theirs) in triple()) {
        let m = merge3(Some(&base), &ours, &theirs);
        let by_ours = merged(&m, &ours, Side::Ours);
        let by_theirs = merged(&m, &ours, Side::Theirs);
        for e in m.conflicts() {
            prop_assert_eq!(lookup(&by_ours, &e.path), e.ours.as_ref());
            prop_assert_eq!(lookup(&by_theirs, &e.path), e.theirs.as_ref());
        }
    }

    /// Entries are strictly increasing by path, so one path never has two entries and none sits under another.
    #[test]
    fn entries_are_in_strict_path_order((base, ours, theirs) in triple()) {
        let m = merge3(Some(&base), &ours, &theirs);
        prop_assert!(m.entries.windows(2).all(|w| w[0].path < w[1].path));
        let unresolved = m.patch(&ours, |_| None).err().map(|u| u.paths);
        let conflicts: Vec<_> = m.conflicts().map(|e| e.path.clone()).collect();
        prop_assert_eq!(unresolved.unwrap_or_default(), conflicts);
    }

    /// With no base, every difference is a conflict, there are none iff the trees are equal, and choosing a side
    /// gives that tree.
    #[test]
    fn without_a_base_all_entries_are_conflicts((_, ours, theirs) in triple()) {
        let m = merge3(None, &ours, &theirs);
        prop_assert!(m.entries.iter().all(|e| e.outcome == Outcome::Conflict && e.base.is_none()));
        prop_assert_eq!(m.entries.is_empty(), ours == theirs);
        prop_assert_eq!(merged(&m, &ours, Side::Theirs), theirs);
        prop_assert_eq!(merged(&m, &ours, Side::Ours), ours);
    }
}
