//! Costs that matter: opening a project, applying a patch (every UI edit does), and saving one changed file.
//!
//! Run with `cargo bench -p omuzaki-doc`. The project is 200 files of about 1.5 KB each.

// A failed setup should abort the benchmark.
#![allow(clippy::unwrap_used)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_doc::Project;
use omuzaki_doc_api::{DocWrite, Patch};
use serde_json::json;
use tempfile::TempDir;

const FILES: usize = 200;

fn benches(c: &mut Criterion) {
    let tmp = TempDir::new().unwrap();
    common::write_big_project(tmp.path(), FILES);

    c.bench_function("open 200 files", |b| b.iter(|| black_box(Project::open(tmp.path()).unwrap())));

    let mut project = Project::open(tmp.path()).unwrap();
    let up = Patch::new().with(common::p("characters/char007/face/blend"), Some(json!(0.7)), Some(json!(0.9)));
    let down = up.inverse();
    // One iteration is two applies (the edit and its undo), so the cost of one is half of what is reported.
    c.bench_function("apply a field edit and undo it (2 applies)", |b| {
        b.iter(|| {
            black_box(project.apply(&up).unwrap());
            black_box(project.apply(&down).unwrap());
        });
    });

    let entity = json!({"face": {"profile": "round", "eyes": {"size": 4}}, "rig": {"template": "humanoid"}, "look": {"outline": 1}});
    let create = Patch::new().with(common::p("characters/new-one"), None, Some(entity));
    let remove = create.inverse();
    c.bench_function("apply a new three-file character and remove it (2 applies)", |b| {
        b.iter(|| {
            black_box(project.apply(&create).unwrap());
            black_box(project.apply(&remove).unwrap());
        });
    });

    let mut saving = c.benchmark_group("save");
    saving.sample_size(20);
    saving.bench_function("one edited file of 200 (edit in place, flush, rename)", |b| {
        b.iter(|| {
            project.apply(&up).unwrap();
            black_box(project.save().unwrap());
            project.apply(&down).unwrap();
            black_box(project.save().unwrap());
        });
    });
    saving.finish();
}

criterion_group!(store, benches);
criterion_main!(store);
