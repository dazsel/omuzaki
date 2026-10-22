//! Counts the calls the calling thread makes to the C allocator, which is where ONNX Runtime's own (C++) allocations
//! go: they bypass Rust's `#[global_allocator]`, so a counter there never sees them.
//!
//! A test binary that wants the count includes this file (`#[path = ...] mod native_alloc;`) and adds a build script that
//! passes `-Wl,--export-dynamic` to its tests. This file defines `malloc` and its siblings in the test executable; with
//! the symbols exported, the `dlopen`ed runtime and `libstdc++` bind to them first (the executable leads the global
//! lookup scope) and every call lands here, then goes on to glibc's own allocator unchanged. [`interposed`] says whether
//! that took hold; without it the count would silently be zero.
//!
//! What is counted is every call to `malloc`, `calloc`, `realloc`, `posix_memalign`, `aligned_alloc` and `memalign` on
//! the calling thread between [`start`] and [`stop`]: Rust's allocations included, because `std`'s `System` allocator
//! calls the same functions. A caller that also counts Rust's allocations (one per `alloc` or `realloc`) subtracts them
//! to get the native ones. The count is per thread, so it is exact when the measured work runs on the calling thread:
//! ONNX Runtime with one intra-op thread, the default of `SessionOptions`, does.
//!
//! Linux with glibc only (`__libc_malloc` and its siblings are glibc's); elsewhere [`SUPPORTED`] is false.
#![allow(dead_code, unsafe_code, clippy::missing_safety_doc)] // the interposed functions are the C allocator's own ABI

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod imp {
    use std::cell::Cell;
    use std::ffi::c_void;

    thread_local! {
        // `const` init and no destructor: reading or writing them never allocates, so they are safe inside `malloc`.
        static WATCHING: Cell<bool> = const { Cell::new(false) };
        static CALLS: Cell<usize> = const { Cell::new(0) };
    }

    unsafe extern "C" {
        fn __libc_malloc(size: usize) -> *mut c_void;
        fn __libc_calloc(count: usize, size: usize) -> *mut c_void;
        fn __libc_realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
        fn __libc_memalign(align: usize, size: usize) -> *mut c_void;
    }

    /// One call on a watched thread. `try_with`: a call during thread teardown, after the thread-locals are gone, is not
    /// counted instead of panicking inside the allocator.
    fn note() {
        if WATCHING.try_with(Cell::get).unwrap_or(false) {
            let _ = CALLS.try_with(|n| n.set(n.get() + 1));
        }
    }

    // SAFETY (all six): each forwards its arguments unchanged to glibc's implementation of the same function, so it keeps
    // that function's contract; the only addition is the thread-local count.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
        note();
        unsafe { __libc_malloc(size) }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
        note();
        unsafe { __libc_calloc(count, size) }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
        note();
        unsafe { __libc_realloc(ptr, size) }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn memalign(align: usize, size: usize) -> *mut c_void {
        note();
        unsafe { __libc_memalign(align, size) }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn aligned_alloc(align: usize, size: usize) -> *mut c_void {
        note();
        unsafe { __libc_memalign(align, size) }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> i32 {
        // POSIX: the alignment is a power of two and a multiple of the pointer size; anything else is EINVAL (22).
        if !align.is_power_of_two() || !align.is_multiple_of(size_of::<*mut c_void>()) {
            return 22;
        }
        note();
        let ptr = unsafe { __libc_memalign(align, size) };
        if ptr.is_null() {
            return 12; // ENOMEM
        }
        unsafe { *out = ptr };
        0
    }

    pub const SUPPORTED: bool = true;

    unsafe extern "C" {
        fn strdup(s: *const std::ffi::c_char) -> *mut std::ffi::c_char;
        fn free(ptr: *mut c_void);
    }

    /// Whether a C library function that allocates inside the C library (`strdup`, not a call from Rust) is counted.
    pub fn interposed() -> bool {
        start();
        // SAFETY: a NUL-terminated literal; the copy is freed by the `free` that pairs with `strdup`.
        unsafe { free(strdup(c"interposition probe".as_ptr()).cast()) };
        stop() > 0
    }

    pub fn start() {
        CALLS.with(|n| n.set(0));
        WATCHING.with(|w| w.set(true));
    }

    pub fn stop() -> usize {
        WATCHING.with(|w| w.set(false));
        CALLS.with(Cell::get)
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
mod imp {
    pub const SUPPORTED: bool = false;
    pub fn interposed() -> bool {
        false
    }
    pub fn start() {}
    pub fn stop() -> usize {
        0
    }
}

/// Whether this platform can count C allocator calls at all.
pub const SUPPORTED: bool = imp::SUPPORTED;

/// Whether the interposition took hold: a C library allocation is counted. False means a count of zero proves nothing.
pub fn interposed() -> bool {
    imp::interposed()
}

/// Starts counting this thread's C allocator calls from zero.
pub fn start() {
    imp::start();
}

/// Stops counting and returns the calls made on this thread since [`start`].
pub fn stop() -> usize {
    imp::stop()
}
