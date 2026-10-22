//! A run allocates nothing once the session has seen its shapes (#1750, `Session::run`'s contract). Counts the calling
//! thread's allocations over 1000 runs after a warm-up. LiteRT's own C++ allocations don't go through Rust's
//! allocator, so this is the session's code, which is what this crate controls; the unit tests pin the same on the
//! fake (`src/fake.rs`).
//!
//! This file holds the only `GlobalAlloc` in its binary. Skips with a message without `OMUZAKI_LITERT_CACHE`.
#![allow(clippy::doc_markdown, clippy::unwrap_used)] // product names are prose; panics are test failures

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use common::model::add_graph;
use common::{add_slot, library, load_synthetic};
use omuzaki_ai_api::{Dim, Session as _, TensorBuf, TensorRef};

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// The allocator only counts and forwards to `System`, so it adds no obligations beyond `GlobalAlloc`'s own contract;
// implementing that unsafe trait is the one reason this test file allows `unsafe_code`.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn a_run_allocates_nothing_after_warm_up_at_one_and_four_threads() {
    if !library("a_run_allocates_nothing_after_warm_up_at_one_and_four_threads") {
        return;
    }
    let (a, b) = ([1.0f32, 2.0, 3.0, 4.0], [4.0f32; 4]);
    for threads in [1, 4] {
        let mut session = load_synthetic(&add_graph(), &add_slot(&[Dim::Fixed(1), Dim::Fixed(4)]), threads);
        let mut out = TensorBuf::for_outputs(session.signature());
        let inputs = [TensorRef::f32(&a, &[1, 4]), TensorRef::f32(&b, &[1, 4])];
        for _ in 0..10 {
            session.run(&inputs, &mut out).unwrap();
        }
        ALLOCATIONS.with(|n| n.set(0));
        WATCHING.with(|w| w.set(true));
        for _ in 0..1000 {
            session.run(&inputs, &mut out).unwrap();
        }
        WATCHING.with(|w| w.set(false));
        let total = ALLOCATIONS.with(Cell::get);
        eprintln!("{threads} thread(s): {total} allocations in 1000 runs after warm-up");
        assert_eq!(total, 0, "{threads} thread(s)");
    }
}
