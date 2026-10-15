//! Proves `evaluate` allocates nothing: 1 000 calls over every path (drivers, the presets gaze route, binary steps,
//! overrides, morph, colour and UV binds) with allocation counting on (#865, spec §8).
//!
//! It lives in its own test file, and the allocator counts only allocations made on the measuring thread: the test
//! harness's own threads allocate at unpredictable times.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::cast_precision_loss)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{avatar, ix};
use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_avatar::evaluator::ExpressionEvaluator;
use omuzaki_avatar_api::expression::{ColourBind, Overrides, UvBind};
use omuzaki_avatar_api::gaze::AvatarGaze;
use omuzaki_core::glam::{Vec2, Vec4};
use omuzaki_io_api::{ColorBindKind, Override};

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

#[test]
fn a_thousand_evaluations_allocate_nothing() {
    let mut a = avatar(false, &["happy", "blink", "jawOpen", "lookUp", "lookDown", "lookLeft", "lookRight"]);
    a.gaze = AvatarGaze::Presets;
    let (happy, blink) = (ix(&a, "happy"), ix(&a, "blink"));
    a.drivers.set(Channel::CUSTOM_00, Some(happy));
    a.drivers.set(Channel::EYE_BLINK_LEFT, Some(blink));
    a.expressions[blink.index()].binary = true;
    let e = &mut a.expressions[happy.index()];
    e.overrides = Overrides::new(Override::Blend, Override::Block, Override::Blend);
    e.colour.push(ColourBind::new(common::material(), ColorBindKind::Color, Vec4::ONE, Vec4::ZERO));
    e.uv.push(UvBind::new(common::material(), Vec2::splat(0.5), Vec2::splat(0.5)));
    assert_eq!(a.validate(), Ok(()));

    let evaluator = ExpressionEvaluator::new(&a);
    let (mut weights, mut binds) = (evaluator.morph_weights(), evaluator.bind_deltas());
    let frames: Vec<ChannelBuf> = (0..1000)
        .map(|i| {
            let t = i as f32 / 1000.0;
            let mut c = ChannelBuf::NEUTRAL;
            c.set(Channel::CUSTOM_00, t);
            c.set(Channel::EYE_BLINK_LEFT, (t * 7.0).fract());
            c.set(Channel::JAW_OPEN, (t * 3.0).fract());
            c.set_gaze((t * 5.0).sin(), (t * 4.0).cos());
            c
        })
        .collect();
    evaluator.evaluate(&frames[0], &mut weights, &mut binds);

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for frame in &frames {
        evaluator.evaluate(frame, &mut weights, &mut binds);
    }
    MEASURING.with(|m| m.set(false));
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed) - before, 0, "evaluate allocated");
    assert!(weights.as_slice().iter().any(|w| *w > 0.0), "the frames exercised the binds");
}
