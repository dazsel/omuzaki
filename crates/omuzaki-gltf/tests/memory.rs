//! Memory before parsing: a JSON file must not be able to name more than the limits allow, in *any* array.
//!
//! A typed element is far larger than the `{}` that names it (a material is 888 bytes for 3 bytes of JSON), so before
//! this was checked 600 KB of `{}` in `materials` peaked at 300 MB. These tests measure the peak allocation of an
//! import with a counting allocator: a refused file must cost almost nothing, and a file at the largest size the limits
//! allow must stay within a few times what a file full of nodes costs.
//!
//! It lives in its own test file because it installs a global allocator, and the allocator counts only the measuring
//! thread: the harness's other threads allocate at unpredictable times.

// A counting allocator is the only way to measure allocation; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::mem::size_of;

use common::import_with;
use omuzaki_gltf::schema::{Accessor, Buffer, BufferView, Image, Material, Mesh, Node, Sampler, Scene, Skin, Texture};
use omuzaki_io_api::{ImportOptions, IoError, Limits, NoResolver};

struct Peak;

thread_local! {
    // `const` init with no destructor: touching them never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<i64> = const { Cell::new(0) };
    static PEAK: Cell<i64> = const { Cell::new(0) };
}

/// Adds `delta` bytes to the measuring thread's live total and keeps the high-water mark.
fn track(delta: i64) {
    if MEASURING.try_with(Cell::get).unwrap_or(false) {
        let _ = LIVE.try_with(|live| {
            live.set(live.get() + delta);
            let _ = PEAK.try_with(|peak| peak.set(peak.get().max(live.get())));
        });
    }
}

fn signed(size: usize) -> i64 {
    i64::try_from(size).unwrap_or(i64::MAX)
}

// SAFETY: every method forwards to `System` unchanged; the bookkeeping is the only addition.
unsafe impl GlobalAlloc for Peak {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            track(signed(layout.size()));
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        track(-signed(layout.size()));
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            track(signed(new_size) - signed(layout.size()));
        }
        new
    }
}

#[global_allocator]
static GLOBAL: Peak = Peak;

/// Runs `f` and returns its result with the most bytes that were live at once while it ran (on this thread).
fn peak_of<R>(f: impl FnOnce() -> R) -> (R, u64) {
    LIVE.set(0);
    PEAK.set(0);
    MEASURING.set(true);
    let result = f();
    MEASURING.set(false);
    (result, u64::try_from(PEAK.get()).unwrap_or(0))
}

/// Every top-level array of the document that holds objects, with the name the limit error gives it and the bytes of
/// one element once parsed.
const ARRAYS: [(&str, &str, usize); 13] = [
    ("nodes", "node count", size_of::<Node>()),
    ("images", "image count", size_of::<Image>()),
    ("scenes", "scene count", size_of::<Scene>()),
    ("meshes", "mesh count", size_of::<Mesh>()),
    ("skins", "skin count", size_of::<Skin>()),
    ("materials", "material count", size_of::<Material>()),
    ("textures", "texture count", size_of::<Texture>()),
    ("samplers", "sampler count", size_of::<Sampler>()),
    ("accessors", "accessor count", size_of::<Accessor>()),
    ("bufferViews", "buffer view count", size_of::<BufferView>()),
    ("buffers", "buffer count", size_of::<Buffer>()),
    ("animations", "animation count", size_of::<serde_json::Value>()),
    ("cameras", "camera count", size_of::<serde_json::Value>()),
];

/// A text glTF whose `array` holds `entries` empty objects.
fn hostile(array: &str, entries: u64) -> Vec<u8> {
    let body = vec!["{}"; usize::try_from(entries).unwrap()].join(",");
    format!(r#"{{"asset":{{"version":"2.0"}},"{array}":[{body}]}}"#).into_bytes()
}

fn import_peak(bytes: &[u8], options: &ImportOptions) -> (Result<(), IoError>, u64) {
    let (result, peak) = peak_of(|| import_with(bytes, options, &NoResolver).map(drop));
    (result, peak)
}

/// What a file refused by count may allocate: the error and nothing that grows with the file.
const REFUSED_PEAK: u64 = 64 << 10;

#[test]
fn six_hundred_kilobytes_of_empty_materials_are_refused_without_building_them() {
    let file = hostile("materials", 200_000);
    assert!((600_000..600_100).contains(&file.len()), "the reviewer's file: {} bytes", file.len());
    let (result, peak) = import_peak(&file, &ImportOptions::default());
    let Err(IoError::Limit { what: "material count", limit, got: 200_000 }) = result else {
        panic!("a material bomb is a limit error, not {result:?}");
    };
    assert!(limit < 200_000, "the default limits cap materials at {limit}");
    eprintln!("materials bomb: {} bytes refused with {peak} bytes allocated (300 MB before)", file.len());
    assert!(peak < REFUSED_PEAK, "refusing it allocated {peak} bytes (it was 300 MB before)");
}

#[test]
fn every_top_level_array_is_refused_by_count_before_anything_is_built() {
    let options = ImportOptions::default();
    let mut seen = 0;
    for (array, name, _) in ARRAYS {
        let file = hostile(array, 1_000_000);
        let (result, peak) = import_peak(&file, &options);
        let Err(IoError::Limit { what, limit, got }) = result else { panic!("{array}: {result:?}") };
        assert_eq!((what, got), (name, 1_000_000), "{array}");
        assert!(limit > 0 && limit < got, "{array}: limit {limit}");
        assert!(peak < REFUSED_PEAK, "{array}: refusing a {}-byte file allocated {peak} bytes", file.len());
        seen += 1;
    }
    assert_eq!(seen, 13, "every array of the document is covered");
}

#[test]
fn an_array_at_the_largest_size_the_limits_allow_costs_no_more_than_a_file_of_nodes() {
    // Small node limit so the test is quick; the rule is the same at any size.
    let options = ImportOptions::default().with_limits(Limits { max_nodes: 20_000, ..Limits::default() });
    let node_budget = u64::try_from(20_000 * size_of::<Node>()).unwrap();
    for (array, name, element) in ARRAYS {
        // The error of an oversized file names the cap...
        let Err(IoError::Limit { what, limit, got: _ }) = import_peak(&hostile(array, 1_000_000), &options).0 else {
            panic!("{array}")
        };
        assert_eq!(what, name);
        // ...and a file with exactly that many elements is accepted and has a bounded cost: the elements, a vector
        // that may have doubled past them, and the copy it was growing out of.
        let (result, peak) = import_peak(&hostile(array, limit), &options);
        // (Empty objects are not valid buffers or skins, so the import may fail later; it must not be the count.)
        if let Err(IoError::Limit { what, .. }) = &result {
            assert!(!what.ends_with(" count"), "{array}: {limit} elements are allowed, yet {what} was refused");
        }
        eprintln!("{array}: {limit} elements of {element} bytes peak at {peak} bytes (node budget {node_budget})");
        let bound = node_budget * 7 / 2;
        assert!(peak <= bound, "{array}: {limit} elements of {element} bytes peaked at {peak}, bound {bound}");
    }
}

#[test]
fn the_cap_follows_the_limits() {
    let cap = |max_nodes: u32| {
        let options = ImportOptions::default().with_limits(Limits { max_nodes, ..Limits::default() });
        let Err(IoError::Limit { limit, .. }) = import_peak(&hostile("materials", 1_000_000), &options).0 else {
            panic!("refused")
        };
        limit
    };
    let (small, large) = (cap(50_000), cap(400_000));
    assert!(large > small * 7, "8x the node limit gives about 8x the materials: {small} vs {large}");
    assert_eq!(cap(1), cap(2), "the floor keeps a tiny node limit usable");
    assert!(cap(1) >= 1000, "a file with a handful of nodes still has room for its materials");
}
