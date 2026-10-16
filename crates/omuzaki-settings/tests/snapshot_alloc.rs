//! Reading the snapshot allocates nothing (D6): it is an `Arc` clone and a map lookup.
//!
//! The counting allocator forwards every call to the system allocator and only counts, so it changes no behaviour.
//! The count is per thread, so the store's writer thread cannot disturb it.
#![allow(unsafe_code)] // implementing `GlobalAlloc` is unsafe by definition; each method forwards unchanged

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use omuzaki_settings::{Paths, Store};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::registry::SettingRegistration;
use omuzaki_settings_api::service::Settings;
use omuzaki_settings_api::setting::Setting;

thread_local! {
    /// Allocations made by this thread: the store's writer thread must not count against the reader.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn count() {
    // `try_with` because the allocator also runs while a thread is being torn down.
    let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
}

struct Counting;

// SAFETY: every method forwards its arguments to `System`, which upholds the `GlobalAlloc` contract.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

struct Scale;

impl Setting for Scale {
    type Value = u16;
    const META: SettingMeta = SettingMeta::new(
        "bench.scale",
        "scale",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Excluded,
        Apply::Live,
        1,
    );
    fn default() -> u16 {
        100
    }
}

#[test]
fn a_snapshot_read_allocates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let registration: &'static SettingRegistration = Box::leak(Box::new(SettingRegistration::of::<Scale>("test")));
    let store = Store::open_with(&Paths::new(dir.path().join("c"), dir.path().join("d")), [registration]);

    let before = ALLOCATIONS.with(Cell::get);
    for _ in 0..10_000 {
        let snapshot = store.snapshot();
        black_box(snapshot.value("bench.scale"));
    }
    let after = ALLOCATIONS.with(Cell::get);
    assert_eq!(after - before, 0, "snapshot() and value() allocated");
}
