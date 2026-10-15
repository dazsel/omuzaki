//! The per-mesh joint runs of [`Skinner::push_run_from_world`] (spec §8, `docs/design/byom.md` D4): one mesh's inverse bind
//! matrices are its own, so two meshes that name the same bone get different matrices for it, and a run is the bone's
//! world transform times that mesh's inverse bind. The palette grows once and then allocates nothing.
//!
//! The counting allocator lives in this file because the workspace's other allocation test (`hot_loop.rs`) has its own
//! binary. As there, it counts only the measuring thread, so the harness's threads cannot make the count flaky.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
// Test helpers may panic (a panic is a clear test failure).
#![allow(unsafe_code, clippy::unwrap_used, clippy::cast_possible_truncation)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::full_character;
use omuzaki_anim_api::{BoneId, Pose, Skeleton};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_render_api::JointRange;
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

/// Every bone of the 128-joint skeleton, in order: the bones of a mesh bound to all of them.
fn every_bone(skeleton: &Skeleton) -> Vec<BoneId> {
    (0..skeleton.len()).map(|i| BoneId(u16::try_from(i).unwrap())).collect()
}

/// A skeleton pose with every bone turned by a random angle about each axis, from a seed.
fn seeded_pose(skeleton: &Skeleton, seed: u64) -> Pose {
    let mut rng = Rng::new(seed);
    let mut pose = skeleton.rest_pose();
    for bone in &mut pose.local[..skeleton.len()] {
        bone.rotation = Quat::from_rotation_x(rng.range_f32(-1.0, 1.0))
            * Quat::from_rotation_y(rng.range_f32(-1.0, 1.0))
            * Quat::from_rotation_z(rng.range_f32(-1.0, 1.0));
    }
    pose
}

/// The world pose of `local`, computed by the contract's forward-kinematics pass.
fn world_of(skeleton: &Skeleton, local: &Pose) -> Pose {
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(local, &mut world);
    world
}

/// The CPU reference of a run, written out here rather than shared with the code under test: `world(bone) × inverse_bind`.
fn reference(world: &Pose, bones: &[BoneId], inverse_bind: &[Mat4]) -> Vec<Mat4> {
    bones.iter().zip(inverse_bind).map(|(bone, ib)| world.local[bone.index()].to_mat4() * *ib).collect()
}

#[test]
fn two_meshes_naming_one_bone_get_their_own_inverse_bind_matrices() {
    let ch = full_character(true);
    let rig = RigSkeleton::new(ch.skeleton.clone());
    let skinner = Skinner::new(rig.clone());
    let local = seeded_pose(&ch.skeleton, 1);
    let world = world_of(&ch.skeleton, &local);

    let hips = ch.skeleton.humanoid_bone(omuzaki_anim_api::HumanoidBone::Hips).unwrap();
    let bones = [hips];
    let own = vec![rig.inverse_bind()[hips.index()]];
    // A second mesh bound to the same bone, in a different place: its inverse bind is shifted by a metre.
    let other = vec![Mat4::from_translation(Vec3::X) * own[0]];

    let mut joints = Vec::new();
    let first = skinner.push_run_from_world(&world, &bones, &own, &mut joints);
    let second = skinner.push_run_from_world(&world, &bones, &other, &mut joints);

    let (a, b) = (joints[first.start as usize], joints[second.start as usize]);
    assert_ne!(a, b, "one bone, two inverse bind matrices, two different joints");
    assert!(a.abs_diff_eq(reference(&world, &bones, &own)[0], 1e-5));
    assert!(b.abs_diff_eq(reference(&world, &bones, &other)[0], 1e-5));
}

#[test]
fn a_run_equals_the_cpu_reference_on_a_128_bone_skeleton_at_eight_seeded_poses() {
    let ch = full_character(true);
    assert_eq!(ch.skeleton.len(), 128);
    let rig = RigSkeleton::new(ch.skeleton.clone());
    let skinner = Skinner::new(rig.clone());
    let bones = every_bone(&ch.skeleton);

    for seed in 0..8 {
        let world = world_of(&ch.skeleton, &seeded_pose(&ch.skeleton, seed));
        let mut joints = Vec::new();
        let run = skinner.push_run_from_world(&world, &bones, rig.inverse_bind(), &mut joints);
        assert_eq!((run.start, run.len), (0, 128), "seed {seed}");
        let want = reference(&world, &bones, rig.inverse_bind());
        for (i, (got, want)) in joints.iter().zip(&want).enumerate() {
            assert!(got.abs_diff_eq(*want, 1e-5), "seed {seed}, bone {i}: {got:?} != {want:?}");
        }
    }
}

#[test]
fn two_runs_on_one_palette_are_laid_end_to_end_and_a_cleared_palette_keeps_its_capacity() {
    let ch = full_character(true);
    let rig = RigSkeleton::new(ch.skeleton.clone());
    let skinner = Skinner::new(rig.clone());
    let world = world_of(&ch.skeleton, &seeded_pose(&ch.skeleton, 2));
    let all = every_bone(&ch.skeleton);
    let few = &all[..3];

    let mut joints = Vec::new();
    let first = skinner.push_run_from_world(&world, few, &rig.inverse_bind()[..3], &mut joints);
    let second = skinner.push_run_from_world(&world, &all, rig.inverse_bind(), &mut joints);
    assert_eq!(first, JointRange { start: 0, len: 3 });
    assert_eq!(second, JointRange { start: 3, len: 128 });
    assert_eq!(joints.len(), 131);

    joints.clear();
    let capacity = joints.capacity();
    let again = skinner.push_run_from_world(&world, few, &rig.inverse_bind()[..3], &mut joints);
    assert_eq!(again, JointRange { start: 0, len: 3 });
    assert_eq!(joints.capacity(), capacity, "a cleared palette keeps its storage");
}

#[test]
fn a_run_allocates_nothing_once_the_palette_has_grown() {
    // Everything that allocates is built before the counter starts.
    let ch = full_character(true);
    let rig = RigSkeleton::new(ch.skeleton.clone());
    let skinner = Skinner::new(rig.clone());
    let bones = every_bone(&ch.skeleton);
    let worlds: Vec<Pose> = (0..8).map(|seed| world_of(&ch.skeleton, &seeded_pose(&ch.skeleton, seed))).collect();
    let mut joints: Vec<Mat4> = Vec::new();

    let mut frame = |i: usize| {
        joints.clear(); // what `Scene::clear` does each frame; the capacity stays
        let world = &worlds[i % worlds.len()];
        let a = skinner.push_run_from_world(world, &bones, rig.inverse_bind(), &mut joints);
        let b = skinner.push_run_from_world(world, &bones, rig.inverse_bind(), &mut joints);
        assert_eq!((a.start, b.start), (0, 128));
    };
    frame(0); // warm-up: the palette grows to its working size

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for i in 1..=200 {
        frame(i);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "200 frames of two runs made {allocations} allocations");
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "one inverse bind matrix per bone")]
fn mismatched_bones_and_inverse_binds_are_named_in_a_debug_build() {
    let ch = full_character(true);
    let rig = RigSkeleton::new(ch.skeleton.clone());
    let skinner = Skinner::new(rig.clone());
    let world = world_of(&ch.skeleton, &seeded_pose(&ch.skeleton, 0));
    let mut joints = Vec::new();
    skinner.push_run_from_world(&world, &every_bone(&ch.skeleton)[..2], &rig.inverse_bind()[..3], &mut joints);
}
