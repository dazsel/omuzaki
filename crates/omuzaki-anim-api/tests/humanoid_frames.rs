//! `HumanoidFrames`: the round trip on four kinds of skeleton, the garbage-input rule, and that the hot methods allocate
//! nothing (counted per measuring thread, as in `hot_loop.rs`).

#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::humanoid_frames::RestLengthMismatch;
use omuzaki_anim_api::{BoneDef, BoneId, HumanoidBone, HumanoidFrames, HumanoidPose, Pose, Skeleton, Transform};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::rng::Rng;

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

fn vec(rng: &mut Rng, spread: f32) -> Vec3 {
    Vec3::new(rng.range_f32(-spread, spread), rng.range_f32(-spread, spread), rng.range_f32(-spread, spread))
}

fn rotation(rng: &mut Rng) -> Quat {
    Quat::from_axis_angle(vec(rng, 1.0).try_normalize().unwrap_or(Vec3::Y), rng.range_f32(-1.5, 1.5))
}

fn random_pose(rng: &mut Rng) -> HumanoidPose {
    let mut pose = HumanoidPose::REST;
    for bone in HumanoidBone::ALL {
        pose.set(bone, rotation(rng));
    }
    pose.hips_offset = vec(rng, 0.3);
    pose
}

/// The four skeletons the acceptance names.
fn skeletons() -> Vec<(&'static str, Skeleton)> {
    let mut rng = Rng::new(5);
    let at_rest = |_| Transform::from_translation(Vec3::Y * 0.1);
    let identity = Skeleton::humanoid(&HumanoidBone::ALL, at_rest).unwrap();
    let cell = std::cell::RefCell::new(&mut rng);
    let arbitrary = Skeleton::humanoid(&HumanoidBone::ALL, |_| {
        let mut rng = cell.borrow_mut();
        Transform { translation: vec(&mut rng, 0.2), rotation: rotation(&mut rng) }
    })
    .unwrap();
    let required: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect();
    let missing = Skeleton::humanoid(&required, |_| Transform {
        translation: Vec3::Y * 0.1,
        rotation: Quat::from_rotation_z(0.4),
    })
    .unwrap();
    let generic = Skeleton::generic(vec![
        BoneDef { name: "root".into(), parent: None, rest: Transform::IDENTITY, humanoid: None },
        BoneDef {
            name: "tail".into(),
            parent: Some(BoneId(0)),
            rest: Transform::from_translation(Vec3::Z),
            humanoid: None,
        },
    ])
    .unwrap();
    vec![("identity", identity), ("arbitrary", arbitrary), ("missing optional bones", missing), ("generic", generic)]
}

fn rest_local(skeleton: &Skeleton) -> Vec<Transform> {
    skeleton.bones().iter().map(|b| b.rest).collect()
}

#[test]
fn to_humanoid_inverts_apply_humanoid() {
    let mut rng = Rng::new(11);
    for (name, skeleton) in skeletons() {
        let (frames, rest) = (HumanoidFrames::new(&skeleton), rest_local(&skeleton));
        for _ in 0..64 {
            let pose = random_pose(&mut rng);
            let (mut local, mut back) = (Pose::IDENTITY, HumanoidPose::REST);
            frames.apply_humanoid(&rest, &pose, &mut local).expect("rest_local has one entry per joint");
            frames.to_humanoid(&rest, &local, &mut back).expect("rest_local has one entry per joint");
            for bone in HumanoidBone::ALL {
                let present = skeleton.humanoid_bone(bone).is_some();
                let want = if present { pose.get(bone) } else { Quat::IDENTITY };
                // q and -q are the same rotation: compare up to sign.
                let (got, want) = (back.get(bone), want);
                let sign = if got.dot(want) < 0.0 { -1.0 } else { 1.0 };
                assert!((got * sign).abs_diff_eq(want, 1e-6), "{name}: {bone}: {got:?} vs {want:?}");
            }
            let want_offset = if skeleton.is_humanoid() { pose.hips_offset } else { Vec3::ZERO };
            assert!((back.hips_offset - want_offset).abs().max_element() < 1e-6, "{name}: hips offset");
        }
    }
}

#[test]
fn a_generic_skeleton_has_no_frames_and_stays_at_rest() {
    let (_, generic) = skeletons().pop().unwrap();
    let frames = HumanoidFrames::new(&generic);
    assert!(frames.is_empty());
    let rest = rest_local(&generic);
    let mut local = Pose::IDENTITY;
    frames
        .apply_humanoid(&rest, &random_pose(&mut Rng::new(3)), &mut local)
        .expect("rest_local has one entry per joint");
    assert_eq!(&local.local[..rest.len()], &rest[..]);
}

#[test]
fn garbage_input_leaves_a_bone_at_rest() {
    let (_, skeleton) = skeletons().remove(1);
    let (frames, rest) = (HumanoidFrames::new(&skeleton), rest_local(&skeleton));
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::Head, Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0));
    pose.set(HumanoidBone::Neck, Quat::from_xyzw(0.0, 0.0, 0.0, 0.0));
    pose.set(HumanoidBone::Chest, Quat::from_xyzw(f32::INFINITY, 0.0, 0.0, 1.0));
    pose.hips_offset = Vec3::new(f32::INFINITY, 0.0, 0.0);
    let mut local = Pose::IDENTITY;
    frames.apply_humanoid(&rest, &pose, &mut local).expect("rest_local has one entry per joint");
    for bone in [HumanoidBone::Head, HumanoidBone::Neck, HumanoidBone::Chest, HumanoidBone::Hips] {
        let id = skeleton.humanoid_bone(bone).unwrap().index();
        assert_eq!(local.local[id], rest[id], "{bone}");
    }
    // A non-finite solved pose reads back as the identity, not as NaN.
    let hips = skeleton.humanoid_bone(HumanoidBone::Head).unwrap().index();
    local.local[hips].rotation = Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0);
    let mut back = HumanoidPose::REST;
    frames.to_humanoid(&rest, &local, &mut back).expect("rest_local has one entry per joint");
    assert_eq!(back.get(HumanoidBone::Head), Quat::IDENTITY);
}

#[test]
fn the_conversions_allocate_nothing() {
    let (_, skeleton) = skeletons().remove(1);
    let (frames, rest) = (HumanoidFrames::new(&skeleton), rest_local(&skeleton));
    let pose = random_pose(&mut Rng::new(2));
    let (mut local, mut back) = (Pose::IDENTITY, HumanoidPose::REST);
    MEASURING.with(|m| m.set(true));
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..100 {
        frames.apply_humanoid(&rest, &pose, &mut local).expect("rest_local has one entry per joint");
        frames.to_humanoid(&rest, &local, &mut back).expect("rest_local has one entry per joint");
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(false));
    assert_eq!(after - before, 0, "both conversions allocate nothing");
}

#[test]
fn a_short_or_long_rest_local_is_an_error_and_writes_nothing() {
    let (_, skeleton) = skeletons().remove(1);
    let (frames, rest) = (HumanoidFrames::new(&skeleton), rest_local(&skeleton));
    let joints = skeleton.len();
    let pose = random_pose(&mut Rng::new(4));
    let sentinel = Transform::from_translation(Vec3::splat(9.0));
    let mut local = Pose { local: [sentinel; omuzaki_anim_api::MAX_BONES] };
    let mut back = HumanoidPose::REST;
    back.hips_offset = Vec3::splat(7.0);

    let short = &rest[..joints - 1];
    let long: Vec<Transform> = rest.iter().copied().chain([Transform::IDENTITY]).collect();
    for given in [short, &long[..]] {
        let want = RestLengthMismatch { expected: joints, actual: given.len() };
        assert_eq!(frames.apply_humanoid(given, &pose, &mut local), Err(want));
        assert_eq!(frames.to_humanoid(given, &local, &mut back), Err(want));
    }
    assert!(local.local.iter().all(|t| *t == sentinel), "apply_humanoid wrote into out on a mismatch");
    assert_eq!(back.hips_offset, Vec3::splat(7.0), "to_humanoid wrote into out on a mismatch");
}

#[test]
fn the_error_names_both_lengths() {
    let err = RestLengthMismatch { expected: 55, actual: 54 };
    assert_eq!(err.to_string(), "rest_local has 54 transforms, but the skeleton has 55 joints");
}
