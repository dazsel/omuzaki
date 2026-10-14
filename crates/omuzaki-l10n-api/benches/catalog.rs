//! The catalog's budgets (design 12): building a catalog over the whole product's text takes at most 20 ms, and a
//! cached lookup allocates nothing. Run with `cargo bench -p omuzaki-l10n-api`; the allocation check also fails the run.

// The counting allocator wraps the system one; `GlobalAlloc` is an unsafe trait.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_l10n_api::{Arg, Catalog, Locale, Resource, Text, ids};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded unchanged to `System`; the counter has no effect on the memory handed out.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const GROUPS: usize = 1000;

/// 5,000 messages: per group an op summary (with a description), an argument title (with a description), a part
/// kind (with a summary) and two sentences with placeables.
fn source(language: &str) -> String {
    let mut out = String::new();
    for i in 0..GROUPS {
        let _ = write!(
            out,
            "op-mod-op{i} = {language} summary of op {i}\n    .description = {language} long description of op {i}, a sentence of some length.\n\
             op-mod-op{i}--x = {language} argument x\n    .description = {language} what x does\n\
             kind-k{i} = {language} kind {i}\n    .summary = {language} summary of kind {i}\n\
             msg-a{i} = {language} {{ $part }} is missing from {{ $character }}.\n\
             msg-b{i} = {language} removed {{ $count }} parts.\n"
        );
    }
    out
}

fn resources() -> Vec<Resource> {
    // Leaked: a benchmark process is short-lived and `Resource` holds `&'static str`.
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    vec![
        Resource { package: "bench", locale: "en", source: leak(source("English")) },
        Resource { package: "bench", locale: "ja", source: leak(source("Japanese")) },
    ]
}

/// Allocations made by `f`.
fn allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    f();
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

fn catalog(c: &mut Criterion) {
    let resources = resources();
    let build = || Catalog::from_sources(Locale::JA, &[], black_box(&resources));

    // The headline numbers, once, plainly.
    let started = Instant::now();
    let catalog = build();
    println!(
        "catalog build, 5,000 messages x 2 languages: {:.2} ms (budget 20 ms)",
        started.elapsed().as_secs_f64() * 1e3
    );

    let owner = ids::op("mod.op7");
    // Warm every cache, then count.
    let lookups = |catalog: &Catalog| {
        black_box(catalog.op_summary("mod.op7"));
        black_box(catalog.op_description("mod.op7"));
        black_box(catalog.kind_title("k7"));
        black_box(catalog.kind_summary("k7"));
        black_box(catalog.arg_title(&owner, &["x"]));
        black_box(catalog.arg_description(&owner, &["x"]));
    };
    lookups(&catalog);
    let made = allocations(|| (0..1000).for_each(|_| lookups(&catalog)));
    println!("cached lookups: {made} allocations over 6,000 lookups");
    assert_eq!(made, 0, "a cached lookup must not allocate");

    c.bench_function("catalog build: 5,000 messages, ja over en", |b| b.iter(build));
    c.bench_function("cached lookup: op_summary", |b| b.iter(|| black_box(catalog.op_summary("mod.op7"))));
    c.bench_function("cached lookup: arg_title", |b| b.iter(|| black_box(catalog.arg_title(&owner, &["x"]))));
    let text = Text::new("msg-a7").with("part", Arg::Id("hair-b".into())).with("character", Arg::Id("mira".into()));
    c.bench_function("render: sentence with two arguments", |b| b.iter(|| black_box(catalog.render(&text))));
}

criterion_group!(benches, catalog);
criterion_main!(benches);
