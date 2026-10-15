//! Proves `VrmConstraintSolver::apply` allocates nothing: 32 mixed constraints (roll, aim, rotation, some chained) on
//! the 128-bone character, over many frames of changing poses.
//!
//! Its own test binary, and the allocator counts only allocations made on the measuring thread, as in
//! `spring_alloc.rs`: the harness's other threads would make a process-wide count flaky.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{Axis, BoneConstraint, BoneId, ConstraintKind, ConstraintSet, SignedAxis};
use omuzaki_core::glam::{EulerRot, Quat};
use omuzaki_rig::constraint::VrmConstraintSolver;

#[path = "common/mod.rs"]
mod common;

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
fn applying_32_constraints_allocates_nothing() {
    // Everything that allocates is built before the counter starts.
    let ch = common::full_character(true);
    let constraints = (1_u16..=32)
        .map(|i| BoneConstraint {
            bone: BoneId(i),
            source: BoneId(if i % 4 == 0 { i - 1 } else { 64 + i }),
            kind: match i % 3 {
                0 => ConstraintKind::Roll { axis: Axis::X },
                1 => ConstraintKind::Aim { axis: SignedAxis::PositiveY },
                _ => ConstraintKind::Rotation,
            },
            weight: 0.5,
        })
        .collect();
    let solver = VrmConstraintSolver::new(&ConstraintSet { constraints }, &ch.skeleton).expect("the set is valid");
    let mut pose = ch.skeleton.rest_pose();

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for frame in 0..2000_u16 {
        let a = f32::from(frame) * 0.01;
        for (i, t) in pose.local.iter_mut().take(ch.skeleton.len()).enumerate() {
            t.rotation = Quat::from_euler(EulerRot::YXZ, a + i as f32, a * 0.5, -a);
        }
        solver.apply(&ch.skeleton, &mut pose);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "2000 frames of 32 constraints made {allocations} allocations");
}
