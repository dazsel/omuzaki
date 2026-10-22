//! Proves a spring step allocates nothing (spec §8 "no allocation in the hot loop", §39.3 standard 2), through every
//! path the real-time loop can take: steps at several frame rates, a frame too short for a sub-step, the scheduler
//! lowering the cap, a reset and a warm-up.
//!
//! It is its own test binary, and the allocator counts only allocations made on the measuring thread: the test
//! harness's own threads allocate at unpredictable times, which makes a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::spring::JointLimit;
use omuzaki_anim_api::{SpringSolver, Tick};
use omuzaki_rig::spring::VrmSpringSolver;

#[path = "support/spring_rig.rs"]
mod spring_rig;

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
fn a_spring_step_allocates_nothing() {
    allocates_nothing(&spring_rig::hair_rig(30, 11));
}

#[test]
fn a_spring_step_with_inside_and_plane_colliders_allocates_nothing() {
    let rig = spring_rig::extended_hair_rig(30, 11);
    assert_eq!(rig.set.groups[1].colliders.len(), 3);
    allocates_nothing(&rig);
}

#[test]
fn a_spring_step_with_every_kind_of_limit_allocates_nothing() {
    let mut rig = spring_rig::extended_hair_rig(30, 11);
    for (i, chain) in rig.set.chains.iter_mut().enumerate() {
        for joint in &mut chain.joints {
            joint.limit = Some(match i % 3 {
                0 => JointLimit::cone(0.3),
                1 => JointLimit::hinge(0.4),
                _ => JointLimit::spherical(0.3, 0.2),
            });
        }
    }
    allocates_nothing(&rig);
}

#[test]
fn a_spring_step_with_the_pose_interpolated_allocates_nothing() {
    allocates_nothing_with(&spring_rig::extended_hair_rig(30, 11), true);
}

fn allocates_nothing(rig: &spring_rig::HairRig) {
    allocates_nothing_with(rig, false);
}

fn allocates_nothing_with(rig: &spring_rig::HairRig, interpolate: bool) {
    // Everything that allocates is built before the counter starts.
    let mut solver = VrmSpringSolver::new(&rig.set, &rig.skeleton).expect("the rig's springs are valid");
    solver.set_pose_interpolation(interpolate);
    let mut pose = rig.skeleton.rest_pose();
    rig.animate(&mut pose, 0);
    solver.step(Tick::at_rate(0, 60.0), &rig.skeleton, &mut pose); // warm-up, so lazily sized state exists

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for frame in 1..=1200_u64 {
        rig.animate(&mut pose, frame);
        let fps = [30.0, 60.0, 144.0, 1000.0][(frame % 4) as usize];
        solver.step(Tick::at_rate(frame, fps), &rig.skeleton, &mut pose);
        let _ = solver.stats();
        match frame {
            300 => solver.set_max_substeps(2),
            600 => solver.reset(),
            900 => solver.warm_up(&rig.skeleton, &mut pose, 0.75),
            1000 => solver.set_pose_interpolation(!interpolate),
            1100 => solver.set_pose_interpolation(interpolate),
            _ => {}
        }
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "1200 frames of 300 swinging joints made {allocations} allocations");
}
