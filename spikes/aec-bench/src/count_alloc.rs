//! Allocation counters: how many heap allocations a candidate makes per 10 ms block in steady state. AGENTS.md bans
//! allocation on the real-time path, so this decides whether a candidate can run in the audio callback as is.
//!
//! Two counts, kept apart. The Rust count is a counting `GlobalAlloc`, which only sees Rust allocations. The C and C++
//! candidates (Speex, WebRTC) call `malloc` and `new` directly, so the process also interposes the glibc allocation
//! entry points (`malloc`, `calloc`, `realloc`, `memalign`, `posix_memalign`, `aligned_alloc`) and counts every call.
//! Rust's `System` allocator goes through `malloc` too, so the native count is the `malloc` total minus the Rust count.
//! This needs Linux with glibc; elsewhere the native count reads as not measured.

// A `GlobalAlloc` impl and an interposed `malloc` are `unsafe` by definition; both only forward to the real allocator.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static RUST_ALLOCS: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(target_os = "linux", target_env = "gnu"))]
static MALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);

pub struct Counting;

// SAFETY: every method forwards unchanged to `System`, which upholds the `GlobalAlloc` contract.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        RUST_ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        RUST_ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        RUST_ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// The glibc entry points that allocate, replaced by counting wrappers around glibc's own `__libc_*` implementations.
/// The executable's symbols win over libc's, so C and C++ code linked into the process (statically or not) lands here.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod interpose {
    use super::{MALLOC_CALLS, Ordering};
    use std::ffi::{c_int, c_void};

    unsafe extern "C" {
        fn __libc_malloc(size: usize) -> *mut c_void;
        fn __libc_calloc(n: usize, size: usize) -> *mut c_void;
        fn __libc_realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
        fn __libc_memalign(align: usize, size: usize) -> *mut c_void;
    }

    fn bump() {
        MALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    /// # Safety
    /// As `malloc(3)`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
        bump();
        unsafe { __libc_malloc(size) }
    }

    /// # Safety
    /// As `calloc(3)`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn calloc(n: usize, size: usize) -> *mut c_void {
        bump();
        unsafe { __libc_calloc(n, size) }
    }

    /// # Safety
    /// As `realloc(3)`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
        bump();
        unsafe { __libc_realloc(ptr, size) }
    }

    /// # Safety
    /// As `memalign(3)`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn memalign(align: usize, size: usize) -> *mut c_void {
        bump();
        unsafe { __libc_memalign(align, size) }
    }

    /// # Safety
    /// As `aligned_alloc(3)`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aligned_alloc(align: usize, size: usize) -> *mut c_void {
        bump();
        unsafe { __libc_memalign(align, size) }
    }

    /// # Safety
    /// As `posix_memalign(3)`: `out` must be valid for a pointer write.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int {
        const EINVAL: c_int = 22;
        const ENOMEM: c_int = 12;
        if !align.is_power_of_two() || !align.is_multiple_of(size_of::<*mut c_void>()) {
            return EINVAL;
        }
        bump();
        let ptr = unsafe { __libc_memalign(align, size) };
        if ptr.is_null() {
            return ENOMEM;
        }
        unsafe { *out = ptr };
        0
    }
}

/// A reading of both counters, taken before and after a stretch of work.
#[derive(Clone, Copy)]
pub struct Snapshot {
    rust: usize,
    malloc: usize,
}

pub fn snapshot() -> Snapshot {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    let malloc = MALLOC_CALLS.load(Ordering::Relaxed);
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    let malloc = 0;
    Snapshot { rust: RUST_ALLOCS.load(Ordering::Relaxed), malloc }
}

impl Snapshot {
    /// Rust allocations since `earlier`, by every thread.
    pub fn rust_since(self, earlier: Snapshot) -> usize {
        self.rust - earlier.rust
    }

    /// Allocations made outside Rust's allocator (C and C++ code) since `earlier`: `malloc`-family calls minus the
    /// Rust allocations that went through them. `None` where the interposer is not built.
    pub fn native_since(self, earlier: Snapshot) -> Option<usize> {
        cfg!(all(target_os = "linux", target_env = "gnu"))
            .then(|| (self.malloc - earlier.malloc).saturating_sub(self.rust - earlier.rust))
    }
}
