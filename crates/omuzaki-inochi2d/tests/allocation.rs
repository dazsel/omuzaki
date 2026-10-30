//! A declared length of `u32::MAX` costs no big allocation: the largest single allocation made while refusing it is
//! measured with a counting allocator, which is why this test has a binary of its own.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
// The counting allocator forwards to the system allocator; `GlobalAlloc` is an unsafe trait.
#![allow(unsafe_code)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use omuzaki_inochi2d::{ImportError, Limits, import, read_container};

struct Counting;

thread_local! {
    /// The largest allocation this thread has asked for since the last reset.
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn note(size: usize) {
    let _ = LARGEST.try_with(|l| l.set(l.get().max(size)));
}

// SAFETY: every call forwards to `System` with the same arguments; the bookkeeping touches only a thread-local `Cell`.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        // SAFETY: the caller's contract is `System`'s.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract is `System`'s.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        // SAFETY: the caller's contract is `System`'s.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The largest single allocation `f` makes on this thread.
fn largest_allocation<T>(f: impl FnOnce() -> T) -> (T, usize) {
    LARGEST.with(|l| l.set(0));
    let out = f();
    (out, LARGEST.with(Cell::get))
}

const SMALL: usize = 1 << 20;

#[test]
fn a_max_length_allocates_nothing_for_itself() {
    let bytes = common::minimal_file();
    let payload_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let tex_count_at = 12 + payload_len + 8;
    let open = Limits {
        max_payload_bytes: u64::MAX,
        max_textures: usize::MAX,
        max_texture_bytes: u64::MAX,
        ..Limits::default()
    };
    for limits in [Limits::default(), open] {
        for at in [8, tex_count_at, tex_count_at + 4] {
            let mut evil = bytes.clone();
            evil[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
            let (result, largest) = largest_allocation(|| import(&evil, &limits));
            assert!(matches!(result, Err(ImportError::Limit { .. } | ImportError::Malformed { .. })), "{result:?}");
            assert!(largest < SMALL, "length at {at}: a {largest} byte allocation");
        }
    }
}

#[test]
fn a_max_inp2_length_allocates_nothing_for_itself() {
    // A blob and a string whose long-form length is u32::MAX, and an array whose count is the largest there is.
    for ty in [0x05u32, 0x06] {
        let mut w = common::Inp2::new();
        w.object(1).key("INP_SECT");
        w.buf.extend_from_slice(&(ty | (0x00FF_FFFF << 8)).to_le_bytes());
        w.buf.extend_from_slice(&u32::MAX.to_le_bytes());
        let (result, largest) = largest_allocation(|| read_container(&w.buf, &Limits::default()));
        assert!(result.is_err());
        assert!(largest < SMALL, "type {ty:#x}: a {largest} byte allocation");
    }
    let mut w = common::Inp2::new();
    w.object(1).key("INP_SECT").array(0x00FF_FFFF);
    let (result, largest) = largest_allocation(|| read_container(&w.buf, &Limits::default()));
    assert!(result.is_err());
    assert!(largest < SMALL, "a {largest} byte allocation");
}
