//! Proves a living-body frame allocates nothing (spec §8 "no allocation in the hot loop"): talking, gesture phrases
//! with predicted and reactive strokes, fidgets, weight shifts, a laugh, a hand-off to a tracked arm, a bonk and a
//! fall all run in the measured frames. Its own test file, counting only the measuring thread (see `life_hot_loop.rs`).

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::body::LivingBody;
use omuzaki_anim::body::measure::{TAKE_FPS, Take};
use omuzaki_anim::body::ops::standing_base;
use omuzaki_anim_api::{Animator, BodyPart, BodyStyle, BoneMask, HumanoidBone, Impact, Personality, Tick};

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
fn a_living_body_frame_allocates_nothing() {
    let take = Take::synthesize(90.0, 0.7, 3);
    let base = standing_base();
    let mut body = LivingBody::new(BodyStyle::preset(Personality::Energetic), 3);
    // Warm up once outside the count: the built-in libraries are parsed on first use.
    body.animate(Tick::at_rate(0, TAKE_FPS), &take.frames[0].bus, &[], &mut base.clone());
    let mut phrases = 0;
    MEASURING.with(|m| m.set(true));
    for (i, frame) in take.frames.iter().enumerate() {
        if i == 3000 {
            body.set_tracked(BoneMask::of(&[HumanoidBone::LeftUpperArm]));
        }
        if i == 3600 {
            body.set_style(&BodyStyle::preset(Personality::Calm));
        }
        if i == 1200 {
            body.react(&Impact::hit(BodyPart::Face, [0.0, 0.0, -1.0], 1.0));
        }
        if i == 2000 {
            body.react(&Impact::fall(BodyPart::LeftShoulder, [-1.0, 0.0, -0.5], 2.0));
        }
        body.hear(&frame.cues);
        let mut pose = base;
        body.animate(Tick::at_rate(i as u64 + 1, TAKE_FPS), &frame.bus, &[], &mut pose);
        phrases += usize::from(body.stroke_log().phrase);
    }
    MEASURING.with(|m| m.set(false));
    assert!(phrases > 10, "the measured frames gestured: {phrases}");
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
