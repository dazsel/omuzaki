//! Random editing sessions: the store must behave like the mock, and what it saves must come back as it was.

mod common;

use omuzaki_doc::Project;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch, PatchError, lookup};
use omuzaki_doc_mock::MemDoc;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

/// A small alphabet makes patches hit existing paths often, which is where the bugs are. A few keys can't be file
/// names, so the refusal paths run too.
fn key() -> impl Strategy<Value = String> {
    prop_oneof![
        8 => prop::sample::select(vec!["a", "b", "c", "d"]).prop_map(str::to_owned),
        1 => Just("Bad Key".to_owned()),
        1 => Just(String::new()),
    ]
}

fn value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        any::<bool>().prop_map(Value::Bool),
        (-5i64..5).prop_map(Value::from),
        "[a-z]{0,3}".prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 16, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..3).prop_map(Value::Array),
            prop::collection::btree_map(key(), inner, 0..3).prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// Nested objects ending in empty ones (sometimes a plain value): the shapes that become folders of files.
fn tree() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![4 => Just(Value::Object(Map::new())), 1 => any::<bool>().prop_map(Value::Bool)];
    leaf.prop_recursive(4, 16, 3, |inner| {
        prop::collection::btree_map(key(), inner, 0..4).prop_map(|m| Value::Object(m.into_iter().collect()))
    })
}

fn any_value() -> impl Strategy<Value = Value> {
    prop_oneof![value(), tree()]
}

#[derive(Debug, Clone)]
enum Kind {
    /// Set the node picked to this value.
    Replace(Value),
    /// Remove the node picked.
    Remove,
    /// Add `key` (or append, to a list) under the container picked.
    Add(String, Value),
}

#[derive(Debug, Clone)]
struct Edit {
    pick: usize,
    kind: Kind,
    /// Claim the wrong current value, to provoke a conflict.
    stale: bool,
}

#[derive(Debug, Clone)]
enum Step {
    Patch(Vec<Edit>),
    Save,
    /// Save, then continue the session on a freshly opened project.
    Reopen,
}

fn edit() -> impl Strategy<Value = Edit> {
    let kind = prop_oneof![
        3 => any_value().prop_map(Kind::Replace),
        2 => Just(Kind::Remove),
        4 => (key(), any_value()).prop_map(|(k, v)| Kind::Add(k, v)),
    ];
    (any::<usize>(), kind, prop::bool::weighted(0.08)).prop_map(|(pick, kind, stale)| Edit { pick, kind, stale })
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        8 => prop::collection::vec(edit(), 1..4).prop_map(Step::Patch),
        2 => Just(Step::Save),
        1 => Just(Step::Reopen),
    ]
}

/// Every node below the root, outside `project`, and whether it is a container (object or list).
fn nodes(value: &Value, at: &DocPath, out: &mut Vec<(DocPath, bool)>) {
    let children: Vec<(String, &Value)> = match value {
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Value::Array(a) => a.iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
        _ => return,
    };
    for (key, child) in children {
        let path = at.join(key);
        out.push((path.clone(), child.is_object() || child.is_array()));
        nodes(child, &path, out);
    }
}

/// Turns the abstract edits into a patch that is valid for `doc` (apart from the deliberate mistakes), tracking the
/// effect of each change so the next one sees it.
fn build_patch(doc: &MemDoc, edits: &[Edit]) -> Patch {
    let mut scratch = doc.clone();
    let mut patch = Patch::new();
    for e in edits {
        let mut all = Vec::new();
        nodes(scratch.root(), &DocPath::root(), &mut all);
        all.retain(|(p, _)| p.segments().first().is_none_or(|s| s != "project"));
        let containers: Vec<DocPath> =
            std::iter::once(DocPath::root()).chain(all.iter().filter(|(_, c)| *c).map(|(p, _)| p.clone())).collect();
        let (target, after) = match &e.kind {
            Kind::Replace(v) if !all.is_empty() => (all[e.pick % all.len()].0.clone(), Some(v.clone())),
            Kind::Remove if !all.is_empty() => (all[e.pick % all.len()].0.clone(), None),
            Kind::Add(k, v) => {
                let parent = &containers[e.pick % containers.len()];
                let key = match lookup(scratch.root(), parent) {
                    Some(Value::Array(a)) => a.len().to_string(),
                    _ => k.clone(),
                };
                (parent.join(key), Some(v.clone()))
            }
            Kind::Replace(_) | Kind::Remove => continue,
        };
        // A top-level entry is a file or folder, so it has to be an object; the refusal of anything else is tested
        // directly, because the mock would accept it and the two would disagree on which change fails first.
        let after = if target.segments().len() == 1 {
            after.map(|v| if v.is_object() { v } else { json!({"v": v}) })
        } else {
            after
        };
        let before = if e.stale { Some(Value::from("stale")) } else { lookup(scratch.root(), &target).cloned() };
        let change = Patch::new().with(target, before, after);
        // Keep the scratch copy in step; a change the mock refuses just doesn't move it, like in the real session.
        drop(scratch.apply(&change));
        patch.extend(change);
    }
    patch
}

fn same_kind(a: &PatchError, b: &PatchError) -> bool {
    match (a, b) {
        (PatchError::Conflict { path: p, .. }, PatchError::Conflict { path: q, .. }) => p == q,
        (PatchError::Unreachable { .. }, PatchError::Unreachable { .. }) => true,
        _ => false,
    }
}

/// Deterministic unless `PROPTEST_RNG_SEED` is set (to explore further: `PROPTEST_RNG_SEED=7 PROPTEST_CASES=5000`),
/// and without regression files: a failure found that way becomes an ordinary test.
fn config() -> Config {
    let mut config = Config { failure_persistence: None, ..Config::default() };
    if matches!(config.rng_seed, RngSeed::Random) {
        config.rng_seed = RngSeed::Fixed(0x5EED);
    }
    config
}

proptest! {
    #![proptest_config(config())]

    /// The store answers every patch as the mock does (refusing, with `Unreachable`, only what files can't hold),
    /// and whatever it saves reopens as the same tree.
    #[test]
    fn random_sessions_agree_with_the_mock_and_survive_reopening(steps in prop::collection::vec(step(), 1..14)) {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("p");
        let mut project = Project::create(&dir, "P").unwrap();
        let mut mock = MemDoc::from_value(project.root().clone());

        for step in steps {
            match step {
                Step::Patch(edits) => {
                    let patch = build_patch(&mock, &edits);
                    let before = project.root().clone();
                    let version = project.version();
                    let ours = project.apply(&patch);
                    let theirs = mock.apply(&patch);
                    match (ours, theirs) {
                        (Ok(v), Ok(_)) => {
                            prop_assert_eq!(v.0, version.0 + 1);
                            prop_assert_eq!(project.root(), mock.root());
                        }
                        (Err(a), Err(b)) => {
                            prop_assert!(same_kind(&a, &b), "{a} vs {b}");
                            prop_assert_eq!(project.root(), &before);
                            prop_assert_eq!(project.version(), version);
                        }
                        (Err(a), Ok(_)) => {
                            // Only what files can't hold may be refused when the mock accepts.
                            prop_assert!(matches!(a, PatchError::Unreachable { .. }), "{a}");
                            prop_assert_eq!(project.root(), &before);
                            prop_assert_eq!(project.version(), version);
                            mock = MemDoc::from_value(before);
                        }
                        (Ok(_), Err(b)) => prop_assert!(false, "the store accepted a patch the mock refused: {b}"),
                    }
                }
                Step::Save => {
                    project.save().unwrap();
                    prop_assert!(!project.is_dirty());
                    prop_assert_eq!(project.save().unwrap().written, 0, "a second save has nothing left to write");
                }
                Step::Reopen => {
                    project.save().unwrap();
                    let reopened = Project::open(&dir).unwrap();
                    prop_assert_eq!(reopened.root(), project.root());
                    prop_assert!(!reopened.is_dirty());
                    project = reopened;
                }
            }
        }

        project.save().unwrap();
        let reopened = Project::open(&dir).unwrap();
        prop_assert_eq!(reopened.root(), mock.root());
        prop_assert_eq!(reopened.get(&DocPath::root()), Some(mock.root()));
    }
}
