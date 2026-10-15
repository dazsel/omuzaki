//! A counting global allocator: how many heap allocations a candidate makes per 10 ms block in steady state. AGENTS.md
//! bans allocation on the real-time path, so this decides whether a candidate can run in the audio callback as is.

// A `GlobalAlloc` impl is `unsafe` by definition; it only forwards to the system allocator.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

pub struct Counting;

// SAFETY: every method forwards unchanged to `System`, which upholds the `GlobalAlloc` contract.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Allocations made so far, by every thread.
pub fn count() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}
