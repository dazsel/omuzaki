//! The publish path allocates nothing, and a read allocates only the `budget` vector.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_core::time::Seconds;
use omuzaki_metrics_api::{AudioMetrics, FrameMetrics, PerformMetrics, TrackerMetrics, metrics};

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

/// Allocations `f` makes on this thread.
fn count(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

#[test]
fn publishing_allocates_nothing_over_an_hour_of_frames() {
    let (mut w, _reader) = metrics();
    let mut frame = FrameMetrics::new(Seconds(0.0));
    let mut perform = PerformMetrics::new(Seconds(0.0));
    let mut tracker = TrackerMetrics::new(Seconds(0.0));
    let mut audio = AudioMetrics::new(Seconds(0.0));
    let allocations = count(|| {
        for i in 0..3_600u32 {
            let at = Seconds(f64::from(i) / 60.0);
            (frame.at, perform.at, tracker.at, audio.at) = (at, at, at, at);
            frame.fps = 60.0;
            frame.gpu_ms = Some(1.0);
            w.frame.publish(&frame);
            w.perform.publish(&perform);
            w.tracker.publish(&tracker);
            w.audio.publish(&audio);
        }
    });
    assert_eq!(allocations, 0, "3600 publishes made {allocations} allocations");
}

#[test]
fn a_read_allocates_only_the_budget_vector() {
    let (mut w, reader) = metrics();
    assert_eq!(count(|| drop(reader.read())), 0, "nothing published: nothing to allocate");
    let mut frame = FrameMetrics::new(Seconds(1.0));
    frame.gpu_ms = Some(1.0);
    frame.track_to_submit_ms = Some(30.0);
    w.frame.publish(&frame);
    w.tracker.publish(&TrackerMetrics::new(Seconds(1.0)));
    w.audio.publish(&AudioMetrics::new(Seconds(1.0)));
    let allocations = count(|| drop(reader.read()));
    assert_eq!(allocations, 1, "one read made {allocations} allocations");
}
