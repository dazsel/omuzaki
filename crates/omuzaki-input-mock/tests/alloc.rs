//! `drain` allocates nothing: counted by a global allocator, over the mock's feeds and the replay.
#![allow(clippy::unwrap_used, unsafe_code)] // a counting allocator wraps the system one; the test unwraps

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_input_api::binding::{BindingAction, Edge, Intent};
use omuzaki_input_api::event::{DeviceClass, MotionEvent, PadId, PadSet, PadState, SourceTag};
use omuzaki_input_api::feed::{InputControl, IntentFeed, IntentFrame, MotionFeed, MotionFrame};
use omuzaki_input_api::zones::KeyZone;
use omuzaki_input_mock::fake::FakeInput;
use omuzaki_input_mock::replay::{Entry, ReplaySource};

struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards to the system allocator unchanged; the wrapper only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract is passed through.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract is passed through.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract is passed through.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The allocations `f` makes. One test only, so no other thread allocates while counting.
fn allocations(f: impl FnOnce()) -> usize {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    f();
    COUNTING.store(false, Ordering::Relaxed);
    ALLOCATIONS.load(Ordering::Relaxed)
}

#[test]
fn draining_allocates_nothing() {
    let src = SourceTag::new(1).unwrap();
    let pad = PadId::new(0).unwrap();

    let fake = FakeInput::new(Arc::new(FakeClock::new(Seconds(0.0))));
    fake.connect("gamepad.gilrs", SourceTag::new(2).unwrap(), DeviceClass::Gamepad, Some(pad));
    for i in 0..300u32 {
        let at = Seconds(f64::from(i) / 120.0);
        fake.push("windows.raw_input", MotionEvent::key(src, KeyZone::SPACE, i % 2 == 0, at));
        fake.push("windows.raw_input", MotionEvent::pointer(src, 0.001, 0.0, 0.0, at));
    }
    let walk = fake.add_binding("walk", DeviceClass::Keyboard, BindingAction::Intent(Intent::moving(0, 127))).unwrap();
    fake.hit(walk, Edge::Press, Seconds(0.1));
    fake.hit(walk, Edge::Release, Seconds(0.2));
    let mut replay = ReplaySource::new();
    for i in 0..300u32 {
        replay
            .push("b", Entry::Event(MotionEvent::key(src, KeyZone::NUMPAD, i % 2 == 0, Seconds(f64::from(i) / 120.0))));
        replay.push("g", Entry::Pad(PadState::neutral(pad, Seconds(f64::from(i) / 120.0))));
    }

    let (mut frame, mut intents, mut pads) = (MotionFrame::new(), IntentFrame::new(), PadSet::new());
    let mut motion = fake.clone();
    let mut intent_feed = fake.clone();
    let count = allocations(|| {
        for f in 1..=150u32 {
            let up_to = Seconds(f64::from(f) / 60.0);
            MotionFeed::drain(&mut motion, up_to, &mut frame);
            std::hint::black_box(motion.pad(pad));
            motion.pads(&mut pads);
            IntentFeed::drain(&mut intent_feed, up_to, &mut intents);
            replay.drain(up_to, &mut frame);
            std::hint::black_box(replay.pad(pad));
            replay.pads(&mut pads);
        }
    });
    assert_eq!(count, 0, "draining allocated");
    assert!(replay.is_done());
}
