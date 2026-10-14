//! Proves the rig's per-frame stage allocates nothing (spec §8 "no allocation in the hot loop", §39.3 standard 2): the
//! humanoid solve through the [`Rig`] trait, the read-back, and skinning into the scene's joint palette, on the
//! 128-joint skeleton.
//!
//! It lives in its own test file, and the allocator counts only allocations made on the measuring thread: the test
//! harness's own threads allocate at unpredictable times, which makes a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{full_character, random_pose};
use omuzaki_anim_api::{HumanoidPose, Pose, Rig, Tick};
use omuzaki_core::rng::Rng;
use omuzaki_render_api::{Camera, Scene};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;

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
fn a_rig_frame_allocates_nothing() {
    // Everything that allocates is built before the counter starts.
    let ch = full_character(true);
    assert_eq!(ch.skeleton.len(), 128);
    let mut rig = HumanoidRig::new(RigSkeleton::new(ch.skeleton.clone()));
    let mut skinner = Skinner::new(rig.rig_skeleton().clone());
    let mut rng = Rng::new(3);
    let poses: Vec<HumanoidPose> = (0..16).map(|_| random_pose(&mut rng, 1.0)).collect();
    let (mut local, mut back) = (Pose::IDENTITY, HumanoidPose::REST);
    let mut scene = Scene::new(Camera::default());

    let mut frame = |i: usize| {
        scene.joints.clear(); // what `Scene::clear` does each frame; the capacity stays
        rig.solve(Tick::at_rate(i as u64, 30.0), &poses[i % poses.len()], &mut local);
        rig.to_humanoid(&local, &mut back);
        let range = skinner.push(&local, &mut scene.joints);
        assert_eq!((range.start, range.len), (0, 128));
    };
    frame(0); // warm-up: the palette grows to its working size

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for i in 1..=1000 {
        frame(i);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "1000 rig frames made {allocations} allocations");
}
