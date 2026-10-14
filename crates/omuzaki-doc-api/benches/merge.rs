//! The 10 ms budget for a three-way merge of a 500-file project (VC-02, `docs/design/branches.md` §11).
//!
//! The tree has the shape of `write_big_project` in `crates/omuzaki-doc/tests/common/mod.rs`: characters with
//! `character`, `face`, `rig`, `look` and two variant files, each file about 70 leaves. Each side changes 50 leaves,
//! picked by a fixed stride over the sorted leaf paths, and 10 of theirs land on leaves ours also changed (to a
//! different value), so the merge reports 10 conflicts among 90 differing paths.
//!
//! Run with `cargo bench -p omuzaki-doc-api --bench merge`. The budget harness reads the median.

// A failed setup should abort the benchmark.
#![allow(clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_doc_api::merge::{Outcome, Side, merge3};
use omuzaki_doc_api::{DocPath, Value};
use serde_json::{Map, json};

const FILES: usize = 500;
const EDITS_PER_SIDE: usize = 50;
/// Every fifth of theirs edits a leaf ours also edited.
const SHARED_EVERY: usize = 5;

/// One file shaped like `realistic_file` in the library's test fixtures.
fn realistic_file(seed: usize) -> Value {
    let mut file = Map::new();
    file.insert("profile".to_owned(), json!("oval-long"));
    let blend: f64 = format!("0.{}", seed % 10).parse().unwrap();
    file.insert("blend".to_owned(), json!(blend));
    file.insert("tags".to_owned(), json!(["a", "b", "c"]));
    for section in ["eyes", "brows", "mouth", "blush"] {
        let mut body = Map::new();
        body.insert("size".to_owned(), json!(seed));
        body.insert("spacing".to_owned(), json!(0.25));
        body.insert("anchor".to_owned(), json!({ "x": 1, "y": 2 }));
        for i in 0..4 {
            body.insert(
                format!("part{i}"),
                json!({ "weight": f64::from(i) + 0.5, "name": format!("{section}-{i}"), "values": [1, 2, 3, 4, 5, 6] }),
            );
        }
        file.insert(section.to_owned(), Value::Object(body));
    }
    Value::Object(file)
}

/// A project of `files` files (the project header counts as one), laid out like `write_big_project`.
fn big_project(files: usize) -> Value {
    let mut characters = Map::new();
    let mut written = 1;
    let mut n = 0;
    while written < files {
        let mut character = Map::new();
        let mut variants = Map::new();
        for name in ["character", "face", "rig", "look", "variants/game", "variants/closeup"] {
            if written == files {
                break;
            }
            let file = realistic_file(n);
            match name.split_once('/') {
                Some((_, variant)) => variants.insert(variant.to_owned(), file),
                None => character.insert(name.to_owned(), file),
            };
            written += 1;
        }
        if !variants.is_empty() {
            character.insert("variants".to_owned(), Value::Object(variants));
        }
        characters.insert(format!("char{n:03}"), Value::Object(character));
        n += 1;
    }
    json!({ "project": { "name": "Big", "schema": 1 }, "characters": Value::Object(characters) })
}

/// Every leaf (a scalar or an array, which merge whole) as a path, in path order.
fn leaf_paths(tree: &Value) -> Vec<DocPath> {
    fn walk(value: &Value, at: &DocPath, out: &mut Vec<DocPath>) {
        match value {
            Value::Object(map) => map.iter().for_each(|(key, child)| walk(child, &at.join(key.as_str()), out)),
            _ => out.push(at.clone()),
        }
    }
    let mut out = Vec::new();
    walk(tree, &DocPath::root(), &mut out);
    out.sort();
    out
}

/// Sets the existing leaf at `path`.
fn set_leaf(tree: &mut Value, path: &DocPath, value: Value) {
    let mut at = tree;
    for segment in path.segments() {
        at = at.get_mut(segment.as_str()).unwrap();
    }
    *at = value;
}

/// Base plus two sides that each changed [`EDITS_PER_SIDE`] leaves, spread evenly over the project.
fn fixture() -> (Value, Value, Value) {
    let base = big_project(FILES);
    let leaves = leaf_paths(&base);
    let stride = leaves.len() / EDITS_PER_SIDE;
    let (mut ours, mut theirs) = (base.clone(), base.clone());
    for edit in 0..EDITS_PER_SIDE {
        let shared = edit % SHARED_EVERY == 0;
        set_leaf(&mut ours, &leaves[edit * stride], json!("ours"));
        // Half a stride away is another file's leaf, so theirs conflicts only where `shared` says.
        let theirs_at = if shared { edit * stride } else { edit * stride + stride / 2 };
        set_leaf(&mut theirs, &leaves[theirs_at], json!("theirs"));
    }
    (base, ours, theirs)
}

fn merge(c: &mut Criterion) {
    let (base, ours, theirs) = fixture();

    // The numbers mean nothing if the fixture drifts from the shape the budget was set for.
    let check = merge3(Some(&base), &ours, &theirs);
    assert_eq!(check.entries.len(), 2 * EDITS_PER_SIDE - EDITS_PER_SIDE / SHARED_EVERY);
    assert_eq!(check.conflicts().count(), EDITS_PER_SIDE / SHARED_EVERY);
    assert_eq!(
        check.entries.iter().filter(|e| e.outcome == Outcome::Theirs).count(),
        EDITS_PER_SIDE * 4 / SHARED_EVERY
    );

    c.bench_function("merge3, 500 files, 50 edits per side (budget 10 ms)", |b| {
        b.iter(|| black_box(merge3(Some(&base), &ours, &theirs)));
    });
    c.bench_function("merge3 + patch, 500 files, 50 edits per side (budget 10 ms)", |b| {
        b.iter(|| {
            let merged = merge3(Some(&base), &ours, &theirs);
            black_box(merged.patch(&ours, |_| Some(Side::Ours)).unwrap())
        });
    });
}

criterion_group!(merge_trees, merge);
criterion_main!(merge_trees);
