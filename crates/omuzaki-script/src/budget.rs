//! The memory budget of a run: bytes the run's thread holds on the heap, counted by a global allocator.
//!
//! Rhai's size limits count the values held in arrays and maps (and the strings in them), not the keys of a map, so
//! the data limits alone don't bound a run's memory: a script could insert a thousand long keys and use gigabytes
//! without reaching one. [`MemoryBudget`] closes the gap by counting what the run's own thread allocates, and the run
//! stops with [`LimitKind::Memory`](crate::LimitKind::Memory) when that passes [`Limits::memory_bytes`](crate::Limits).
//!
//! A binary that runs scripts installs it as its global allocator, and a run refuses to start in a binary that
//! didn't, so the limit can't be lost by leaving the line out:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOCATOR: omuzaki_script::MemoryBudget = omuzaki_script::MemoryBudget::system();
//! ```
//!
//! # Cost
//!
//! Every allocation, on every thread, pays one relaxed load of a flag (to tell that the budget is installed) and one
//! read of a thread-local flag; a thread without an active budget stops there. On the run's thread it also adds the
//! size to a thread-local counter, which is a read and a write of memory the thread owns: no atomic, no lock, no
//! allocation. The counter is checked once per Rhai operation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

thread_local! {
    /// Whether the budget counts on this thread. `const` initialisation and no destructor: reading it never allocates.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    /// Bytes allocated minus bytes freed on this thread since counting started. Signed: a block freed here that was
    /// allocated before counting started would otherwise wrap.
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

/// Set by the first allocation that goes through a [`MemoryBudget`].
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// A global allocator that wraps `A` and counts the bytes held by a thread that is running a script.
///
/// Install it with `#[global_allocator]` in every binary that runs scripts (see the module docs).
pub struct MemoryBudget<A = System> {
    inner: A,
}

impl MemoryBudget<System> {
    /// The budget over the system allocator.
    pub const fn system() -> Self {
        Self { inner: System }
    }
}

impl<A> MemoryBudget<A> {
    /// The budget over `inner`.
    pub const fn new(inner: A) -> Self {
        Self { inner }
    }
}

/// Adds `delta` bytes to the counter of this thread if it is counting.
#[inline]
fn note(delta: isize) {
    if !INSTALLED.load(Ordering::Relaxed) {
        INSTALLED.store(true, Ordering::Relaxed);
    }
    // `try_with`: the slots are gone while a thread is being torn down, and an allocation may happen then.
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            let _ = LIVE.try_with(|live| live.set(live.get().saturating_add(delta)));
        }
    });
}

/// Converts a size to the counter's type; no real allocation is over `isize::MAX`.
#[inline]
fn signed(size: usize) -> isize {
    isize::try_from(size).unwrap_or(isize::MAX)
}

#[allow(unsafe_code)] // implementing `GlobalAlloc` is unsafe; every method forwards to `inner` unchanged
// SAFETY: each method calls the same method of `inner` with the same arguments and returns its answer, so the
// allocator contract is `inner`'s. The bookkeeping only touches thread-local `Cell`s of plain integers and an atomic
// flag, and allocates nothing.
unsafe impl<A: GlobalAlloc> GlobalAlloc for MemoryBudget<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc`, passed on.
        let ptr = unsafe { self.inner.alloc(layout) };
        if !ptr.is_null() {
            note(signed(layout.size()));
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc_zeroed`, passed on.
        let ptr = unsafe { self.inner.alloc_zeroed(layout) };
        if !ptr.is_null() {
            note(signed(layout.size()));
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc`, passed on.
        unsafe { self.inner.dealloc(ptr, layout) };
        note(-signed(layout.size()));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller's contract for `realloc`, passed on.
        let moved = unsafe { self.inner.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            note(signed(new_size) - signed(layout.size()));
        }
        moved
    }
}

/// Whether a [`MemoryBudget`] is this binary's global allocator. True once anything has allocated through it, which a
/// running program has long before it runs a script.
pub(crate) fn installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// Counts the allocations of the current thread until dropped.
pub(crate) struct Counting {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Counting {
    /// Starts counting on this thread from zero.
    pub(crate) fn start() -> Self {
        LIVE.with(|live| live.set(0));
        COUNTING.with(|counting| counting.set(true));
        Self { _not_send: std::marker::PhantomData }
    }
}

impl Drop for Counting {
    fn drop(&mut self) {
        let _ = COUNTING.try_with(|counting| counting.set(false));
    }
}

/// Bytes the current thread holds beyond what it held when its [`Counting`] started; 0 when it isn't counting.
pub(crate) fn used_here() -> usize {
    LIVE.with(|live| usize::try_from(live.get()).unwrap_or(0))
}
