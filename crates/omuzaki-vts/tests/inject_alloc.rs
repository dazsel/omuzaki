//! `Injector::push` allocates nothing (#727): a counting allocator around a warm push of every mapped parameter.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_anim_api::channel::{CHANNELS, ChannelBuf};
use omuzaki_vts::inject::{ChannelMap, Injector, Snapshot};

/// Counts allocations on the measuring thread while it measures.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

fn measuring() -> bool {
    MEASURING.try_with(Cell::get).unwrap_or(false)
}

// SAFETY: every call forwards to the system allocator unchanged; the counter is a relaxed atomic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if measuring() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract for `alloc` is passed on as is.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if measuring() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract for `realloc` is passed on as is.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn push_and_read_allocate_nothing() {
    let mut map = ChannelMap::builtin().unwrap();
    let defaults: Vec<String> = map.defaults().map(|(name, _)| name.to_owned()).collect();
    for name in &defaults {
        map.enable_default(name, true).unwrap();
    }
    let injector = Injector::new(&map);
    assert_eq!(injector.params().len(), 99, "every custom parameter and every default");
    let mut buf = ChannelBuf::default();
    let mut snap = Snapshot::default();
    MEASURING.with(|m| m.set(true));
    for i in 0..10_000u16 {
        for (n, info) in CHANNELS.iter().enumerate().take(82) {
            buf.set(info.channel, f32::from((i + u16::try_from(n).unwrap()) % 100) / 100.0);
        }
        injector.push(&buf, i % 2 == 0);
        injector.read(&mut snap);
    }
    MEASURING.with(|m| m.set(false));
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0, "allocations in 10,000 pushes and reads");
    assert_eq!(snap.seq, 10_000);
}
