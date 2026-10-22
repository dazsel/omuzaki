//! Proves `observe` allocates nothing: it runs on every frame (spec §8, "no allocation in the hot loop"). The
//! allocator counts only allocations made on the measuring thread, as in `omuzaki-render-api`'s hot-loop test.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_budget::Scheduler;
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{FrameReport, GpuTimings, Pass, PassTimes, Quality, budget};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it's safe inside the allocator.
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

fn report(frame: u64, ms: f64) -> FrameReport {
    let mut passes = PassTimes::default();
    passes.set(Pass::Opaque, Seconds(ms / 1000.0));
    FrameReport {
        frame,
        render_size: UVec2::new(1920, 1080),
        cpu: PassTimes::default(),
        gpu: Some(GpuTimings { frame, passes, painters: None }),
        draw_calls: 0,
        triangles: 0,
    }
}

#[test]
fn observe_never_allocates_including_when_it_changes_quality() {
    let mut scheduler = Scheduler::new(Quality::high(), budget::HIGH_GPU);
    MEASURING.with(|m| m.set(true));
    let mut changes = 0;
    // Overloaded then idle: steps down all the way, then back up.
    for frame in 1..=4000u64 {
        let ms = if frame <= 1000 { 50.0 } else { 0.5 };
        changes += usize::from(scheduler.observe(&report(frame, ms)).is_some());
    }
    MEASURING.with(|m| m.set(false));
    assert!(changes >= 8, "the run exercised both directions ({changes} changes)");
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0, "observe allocated");
}
