//! Proves the hot path allocates nothing once output buffers have grown: a session run (with its signature checks)
//! and a stream's push and take, 1000 times each. The fakes follow the same rules the real implementations must, so
//! `omuzaki-ai`'s stream host and slot sessions can reuse this test.
//!
//! The allocator counts only allocations on the measuring thread (the test harness allocates on its own threads).

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_ai_api::{Session, StreamSpec, TensorBuf, TensorRef};
use omuzaki_ai_mock::{FakeSession, InlineStream, echo_signature};

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
fn runs_pushes_and_takes_allocate_nothing_after_warm_up() {
    let mut session = FakeSession::affine(echo_signature(), 0.5, 0.25);
    let mut out = TensorBuf::for_outputs(session.signature());
    let frame = [0.5f32; 480];
    session.run(&[TensorRef::f32(&frame, &[480])], &mut out).unwrap(); // warm-up: the output grows once
    let runs = measure(|| {
        for _ in 0..1000 {
            session.run(&[TensorRef::f32(&frame, &[480])], &mut out).unwrap();
        }
    });
    assert_eq!(runs, 0, "Session::run allocated");

    let spec = StreamSpec { sample_rate: 48_000, block: 480, latency: 0 };
    let (mut sink, mut source) =
        InlineStream::open(Box::new(FakeSession::affine(echo_signature(), 1.0, 0.0)), spec, 2, 4);
    let mut taken = TensorBuf::for_outputs(&echo_signature());
    // Warm-up in the same pattern: every ring slot and the output buffer grow once.
    for seq in 0..3_u64 {
        sink.push(&[TensorRef::f32(&frame, &[480])]).unwrap();
        if let Some(due) = seq.checked_sub(2) {
            let _ = source.take(due, &mut taken);
        }
    }
    let streamed = measure(|| {
        for seq in 3..1003 {
            sink.push(&[TensorRef::f32(&frame, &[480])]).unwrap();
            let _ = source.take(seq - 2, &mut taken);
        }
    });
    assert_eq!(streamed, 0, "push or take allocated");
    assert_eq!(source.stats().missed, 0);
}
