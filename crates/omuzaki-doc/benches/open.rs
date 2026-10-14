//! The 50 ms budget for opening a project of 200 files (spec v3 §10).
//!
//! This is where the budget is measured for real, on a quiet machine. The unit test
//! `a_two_hundred_file_project_opens_quickly` keeps only a 500 ms guard against gross regressions, because CI runs
//! on a shared, loaded machine.
//!
//! Run with `cargo bench -p omuzaki-doc --bench open`. The budget harness reads the median.

// A failed setup should abort the benchmark.
#![allow(clippy::unwrap_used)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_doc::Project;
use tempfile::TempDir;

const FILES: usize = 200;

fn open(c: &mut Criterion) {
    let tmp = TempDir::new().unwrap();
    common::write_big_project(tmp.path(), FILES);
    // A distinct name from the one in `store.rs`: criterion keys its reports by name, so a clash would overwrite them.
    c.bench_function("project open, 200 files (budget 50 ms)", |b| {
        b.iter(|| black_box(Project::open(tmp.path()).unwrap()));
    });
}

criterion_group!(open_project, open);
criterion_main!(open_project);
