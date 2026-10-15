//! A counting global allocator for the no-allocation check.
//!
//! A library cannot install a global allocator for the programs that link it, so the test binary does:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: omuzaki_audio_mock::conformance::CountingAlloc = omuzaki_audio_mock::conformance::CountingAlloc;
//! ```
//!
//! The count is per thread, so other test threads allocating at the same time do not disturb a measurement.

// A global allocator is `unsafe` by definition; this one only counts and forwards to the system allocator.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    // Const-initialised and without a destructor, so touching it never allocates.
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

/// A [`GlobalAlloc`] that counts the allocations (and reallocations) of each thread and forwards to [`System`].
#[derive(Debug, Default)]
pub struct CountingAlloc;

// SAFETY: every call is forwarded unchanged to `System`, which upholds the `GlobalAlloc` contract; the counter is a
// const-initialised thread-local `Cell`, which neither allocates nor panics (`try_with` ignores a thread being torn down).
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        // SAFETY: the caller upholds `alloc`'s contract, which is `System`'s.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` come from the matching `alloc` of `System`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        // SAFETY: `ptr` and `layout` come from the matching `alloc` of `System`, and the caller checked `new_size`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// How many allocations the calling thread makes while `f` runs. Zero for everything if [`CountingAlloc`] is not the
/// global allocator, which the suite detects before it trusts a zero.
pub fn allocations_during(f: &mut dyn FnMut()) -> u64 {
    let before = COUNT.with(Cell::get);
    f();
    COUNT.with(Cell::get) - before
}
