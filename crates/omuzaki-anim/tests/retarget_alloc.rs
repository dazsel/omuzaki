//! Proves the retarget allocates nothing per frame: tracking (all 52 channels, head, gaze angles), a lost-face span and
//! its fade, with and without mirroring and exaggeration (spec §8 "no allocation in the hot loop").
//!
//! It lives in its own test file, and the allocator counts only allocations made on the measuring thread: the test
//! harness's own threads allocate at unpredictable times, which makes a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim_api::{BlendMode, Calibration, DeviceProfile, Layer, Retarget, RetargetConfig};

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

/// Frames of `retarget` (after a warm-up lap) made with allocation counting on; returns the allocations seen.
fn allocations_over_a_lap(mut retarget: ArkitRetarget) -> usize {
    let frames = common::frames(240);
    let mut out = Layer::absent(BlendMode::Override);
    let mut lap = |retarget: &mut ArkitRetarget| {
        for frame in &frames {
            retarget.apply(frame, 1.0 / 30.0, &mut out);
        }
    };
    lap(&mut retarget);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    lap(&mut retarget);
    lap(&mut retarget);
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

#[test]
fn a_frame_of_the_retarget_allocates_nothing() {
    let configs = [
        RetargetConfig::default(),
        RetargetConfig { mirror: true, exaggeration: 1.5, ..RetargetConfig::default() },
        RetargetConfig { smoothing: 0.0, ..RetargetConfig::default() },
    ];
    for config in configs {
        let retarget =
            ArkitRetarget::with(omuzaki_anim::retarget::default_calibration(DeviceProfile::Webcam), config.clone());
        let allocations = allocations_over_a_lap(retarget);
        assert_eq!(allocations, 0, "{allocations} allocations over 480 frames with {config:?}");
    }
    let identity = ArkitRetarget::with(Calibration::identity(DeviceProfile::Arkit), RetargetConfig::default());
    assert_eq!(allocations_over_a_lap(identity), 0);
}
