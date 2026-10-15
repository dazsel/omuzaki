//! A slot session's steady state allocates nothing: 1000 runs, once the output buffers have grown. The counting
//! allocator is the one from `omuzaki-ai-mock`'s `hot_loop` test; it counts only on the measuring thread.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};
use omuzaki_ai_api::{ModelRegistry, NoProgress, TensorBuf, TensorRef};
use omuzaki_ai_mock::fixtures_fetcher;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn measure(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

#[test]
fn a_thousand_steady_state_runs_allocate_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let files = std::sync::Arc::new(VerifiedCache::new(dir.path().join(CACHE_DIR), fixtures_fetcher()));
    let registry = Registry::new(dir.path(), files, "1.0.0".parse().unwrap(), RegistrySettings::default()).unwrap();
    let slot = "test_echo@1".parse().unwrap();
    let echo = registry.models(Some(&slot)).remove(0).model;
    registry.install(&echo, &NoProgress).unwrap();
    let mut session = registry.open(&slot, None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    let frame = [0.5f32; 480];
    session.run(&[TensorRef::f32(&frame, &[480])], &mut out).unwrap(); // warm-up: the output grows once

    let allocated = measure(|| {
        for _ in 0..1000 {
            session.run(&[TensorRef::f32(&frame, &[480])], &mut out).unwrap();
        }
    });

    assert_eq!(allocated, 0, "SlotSession::run allocated");
}
