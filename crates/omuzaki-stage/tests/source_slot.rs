//! The hand-off of frame sources from the ops side to the perform thread: newest wins, the perform thread takes no
//! lock and never allocates or frees, and every source is dropped by the ops side.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (the other hot-loop tests do the same).
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

#[path = "live/support.rs"]
mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{FrameSource, HumanoidPose, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_stage::perform::input::SourceSlot;
use support::performer;

struct Counting;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    let _ = ACTIVE.try_with(|active| {
        if active.get() {
            let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        }
    });
}

// SAFETY: every call forwards unchanged to the system allocator; counting touches only const-initialised cells.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Allocations and frees made by `f` on this thread.
fn touches_in<R>(f: impl FnOnce() -> R) -> (R, u64) {
    COUNT.with(|count| count.set(0));
    ACTIVE.with(|active| active.set(true));
    let result = f();
    ACTIVE.with(|active| active.set(false));
    (result, COUNT.with(Cell::get))
}

/// A source that says when it is dropped, and what it is.
struct Tagged {
    id: usize,
    dropped: Arc<AtomicUsize>,
}

impl FrameSource for Tagged {
    fn poll(&mut self, _now: Seconds) -> Option<TrackFrame> {
        None
    }
}

impl Drop for Tagged {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn tagged(id: usize, dropped: &Arc<AtomicUsize>) -> Box<dyn FrameSource> {
    Box::new(Tagged { id, dropped: Arc::clone(dropped) })
}

#[test]
fn an_empty_slot_swaps_nothing_and_touches_no_memory() {
    let slot = SourceSlot::new();
    let mut performer = performer(&HumanoidPose::REST);
    let (swapped, touched) = touches_in(|| (0..1000).any(|_| slot.swap_into(&mut performer)));
    assert!(!swapped);
    assert_eq!(touched, 0);
}

#[test]
fn the_newest_posted_source_wins_and_every_source_is_dropped_by_the_ops_side() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let slot = SourceSlot::new();
    let mut performer = performer(&HumanoidPose::REST);
    slot.post(tagged(1, &dropped));
    slot.post(tagged(2, &dropped)); // replaces 1 before the performer took it
    assert_eq!(dropped.load(Ordering::SeqCst), 1, "the superseded source is dropped by the poster");
    let (swapped, touched) = touches_in(|| slot.swap_into(&mut performer));
    assert!(swapped);
    assert_eq!(touched, 0, "the swap allocated or freed on the perform thread");
    assert_eq!(dropped.load(Ordering::SeqCst), 1, "nothing was dropped by the swap");
    // The performer now holds source 2; the one it had (silent) is waiting to be collected, which a post does first.
    slot.post(tagged(3, &dropped));
    let (swapped, touched) = touches_in(|| slot.swap_into(&mut performer));
    assert!(swapped && touched == 0);
    slot.collect();
    assert_eq!(dropped.load(Ordering::SeqCst), 2, "source 2 came back and was dropped by the ops side");
    drop(slot);
    drop(performer);
    assert_eq!(dropped.load(Ordering::SeqCst), 3, "everything is freed in the end");
    let _ = Tagged { id: 0, dropped }.id;
}

#[test]
fn a_poster_and_the_perform_thread_hand_over_without_losing_a_source() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let slot = Arc::new(SourceSlot::new());
    let posts = 2000;
    let poster = {
        let (slot, dropped) = (Arc::clone(&slot), Arc::clone(&dropped));
        std::thread::spawn(move || {
            for id in 0..posts {
                slot.post(tagged(id, &dropped));
            }
        })
    };
    let mut performer = performer(&HumanoidPose::REST);
    while !poster.is_finished() {
        slot.swap_into(&mut performer);
        std::thread::yield_now();
    }
    poster.join().unwrap();
    slot.swap_into(&mut performer);
    slot.collect();
    drop(slot);
    drop(performer);
    assert_eq!(dropped.load(Ordering::SeqCst), posts, "every posted source was dropped exactly once");
}
