//! Proves the hot-path calls allocate nothing: `MailboxWriter::publish`, `MailboxReader::poll` (empty and full) and
//! `ClockMap::map`. Only allocations on the measuring thread count (the pattern of `omuzaki-anim-api/tests/hot_loop.rs`).

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{Channel, DeviceProfile, FrameSource, HeadPose, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_track_api::{ClockMap, DeviceTime, mailbox};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
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

#[test]
fn publish_poll_and_clock_mapping_allocate_nothing() {
    let (mut tx, mut rx) = mailbox();
    let mut clock = ClockMap::new();
    let mut frame = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Webcam);
    frame.head = Some(HeadPose::default());
    let mut received = 0;

    MEASURING.with(|m| m.set(true));
    for i in 0..10_000_u64 {
        let arrived = Seconds(i as f64 / 60.0 + 0.01);
        let time = clock.map(DeviceTime(i * 16_666_667), arrived).unwrap_or(arrived);
        frame.seq = i;
        frame.time = time;
        frame.set(Channel::JAW_OPEN, (i % 10) as f32 / 10.0);
        tx.publish(&frame);
        if i % 3 == 0 {
            tx.publish(&frame);
        }
        received += usize::from(rx.poll(time).is_some());
        received += usize::from(rx.poll(time).is_some());
    }
    MEASURING.with(|m| m.set(false));

    assert_eq!(received, 10_000, "one frame per round: the second poll and the duplicate publish add none");
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
