//! `BusAnimator` against the mock oracle, the spec'd edge cases, and a zero-allocation check on the per-frame path.
//!
//! The randomised cases use the workspace's seeded [`Rng`] rather than a property-testing crate, so a run is
//! reproducible and needs no new dev-dependency.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(
    unsafe_code,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::{BusAnimator, BusAnimatorParams};
use omuzaki_anim_api::{Animator, BoneMask, Bus, Channel, HumanoidBone, HumanoidPose, Tick};
use omuzaki_anim_mock::animator::MockAnimator;
use omuzaki_core::glam::{EulerRot, Quat, Vec3};
use omuzaki_core::rng::Rng;

const DRIVEN: [HumanoidBone; 5] =
    [HumanoidBone::Neck, HumanoidBone::Head, HumanoidBone::Spine, HumanoidBone::Chest, HumanoidBone::Hips];

fn tick() -> Tick {
    Tick::at_rate(0, 30.0)
}

/// Absolute per-component difference of two quaternions after sign normalisation (`q` and `-q` are one rotation).
fn quat_gap(a: Quat, b: Quat) -> f32 {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    (a.x - b.x).abs().max((a.y - b.y).abs()).max((a.z - b.z).abs()).max((a.w - b.w).abs())
}

/// Compares every bone but Neck and Head: the mock splits the head by Euler angles, `BusAnimator` by slerp (#1875).
fn assert_poses_close(a: &HumanoidPose, b: &HumanoidPose, what: &str) {
    assert!((a.hips_offset - b.hips_offset).length() < 1e-6, "{what}: hips offset");
    for bone in HumanoidBone::ALL.into_iter().filter(|b| !matches!(b, HumanoidBone::Neck | HumanoidBone::Head)) {
        let gap = quat_gap(a.get(bone), b.get(bone));
        assert!(gap < 1e-6, "{what}: {bone} differs by {gap}");
    }
}

fn random_angle(rng: &mut Rng, bound: f32) -> f32 {
    rng.range_f32(-bound, bound)
}

/// A bus with random values in the ranges the spec gives, covering every channel `BusAnimator` reads.
fn random_bus(rng: &mut Rng) -> Bus {
    let mut bus = Bus::default();
    bus.values[Channel::HEAD_YAW] = random_angle(rng, 1.2);
    bus.values[Channel::HEAD_PITCH] = random_angle(rng, 1.2);
    bus.values[Channel::HEAD_ROLL] = random_angle(rng, 1.2);
    bus.values[Channel::LEAN] = random_angle(rng, 0.5);
    bus.values[Channel::BREATH] = rng.next_f32();
    bus.values[Channel::BODY_TURN] = random_angle(rng, 1.0);
    bus
}

/// A base pose with random rotations on the five driven bones and the left upper arm.
fn random_base(rng: &mut Rng) -> HumanoidPose {
    let mut base = HumanoidPose::REST;
    base.hips_offset = Vec3::new(random_angle(rng, 0.1), random_angle(rng, 0.1), random_angle(rng, 0.1));
    for bone in DRIVEN.iter().copied().chain([HumanoidBone::LeftUpperArm]) {
        let q = Quat::from_euler(EulerRot::XYZ, random_angle(rng, 3.0), random_angle(rng, 3.0), random_angle(rng, 3.0));
        base.set(bone, q);
    }
    base
}

#[test]
fn default_matches_the_mock_on_a_thousand_random_frames() {
    let mut rng = Rng::new(0x307);
    for case in 0..1_000 {
        let bus = random_bus(&mut rng);
        let base = random_base(&mut rng);
        let mut mock_out = base;
        let mut bus_out = base;
        MockAnimator.animate(tick(), &bus, &[], &mut mock_out);
        BusAnimator::default().animate(tick(), &bus, &[], &mut bus_out);
        assert_poses_close(&bus_out, &mock_out, &format!("case {case}"));
    }
}

#[test]
fn bus_yaw_splits_35_percent_to_the_neck_and_the_rest_to_the_head() {
    let mut bus = Bus::default();
    bus.values[Channel::HEAD_YAW] = 0.4;
    let mut pose = HumanoidPose::REST;
    BusAnimator::default().animate(tick(), &bus, &[], &mut pose);
    let (neck, head) = (pose.get(HumanoidBone::Neck), pose.get(HumanoidBone::Head));
    assert!((neck.angle_between(Quat::IDENTITY) - 0.14).abs() < 1e-5);
    assert!(((neck * head).angle_between(Quat::IDENTITY) - 0.4).abs() < 1e-5);
}

#[test]
fn neck_times_head_is_the_bus_rotation_on_a_yaw_pitch_grid_and_random_targets() {
    let mut check = |yaw: f32, pitch: f32, roll: f32, share: f32| {
        let mut bus = Bus::default();
        bus.values[Channel::HEAD_YAW] = yaw;
        bus.values[Channel::HEAD_PITCH] = pitch;
        bus.values[Channel::HEAD_ROLL] = roll;
        let mut pose = HumanoidPose::REST;
        BusAnimator::new(BusAnimatorParams { neck_share: share, ..BusAnimatorParams::default() }).animate(
            tick(),
            &bus,
            &[],
            &mut pose,
        );
        let (neck, head) = (pose.get(HumanoidBone::Neck), pose.get(HumanoidBone::Head));
        let whole = Quat::from_euler(EulerRot::YXZ, yaw, pitch, roll);
        let err = quat_gap(neck * head, whole);
        assert!(err < 1e-6, "yaw {yaw} pitch {pitch} roll {roll}: product off by {err}");
        // The neck carries `share` of the angle about the same axis.
        let want = whole.angle_between(Quat::IDENTITY) * share;
        assert!((neck.angle_between(Quat::IDENTITY) - want).abs() < 1e-5, "neck angle");
    };
    for yi in -6..=6 {
        for pi in -4..=4 {
            check((yi as f32 * 10.0).to_radians(), (pi as f32 * 10.0).to_radians(), 0.0, 0.35);
        }
    }
    let mut rng = Rng::new(0x1875);
    for _ in 0..1_000 {
        check(random_angle(&mut rng, 1.2), random_angle(&mut rng, 1.2), random_angle(&mut rng, 1.2), 0.35);
    }
}

#[test]
fn neck_share_zero_puts_all_of_the_yaw_on_the_head() {
    let mut bus = Bus::default();
    bus.values[Channel::HEAD_YAW] = 0.4;
    let mut pose = HumanoidPose::REST;
    BusAnimator::new(BusAnimatorParams { neck_share: 0.0, ..BusAnimatorParams::default() }).animate(
        tick(),
        &bus,
        &[],
        &mut pose,
    );
    assert!(pose.get(HumanoidBone::Neck).angle_between(Quat::IDENTITY) < 1e-5);
    assert!((pose.get(HumanoidBone::Head).angle_between(Quat::IDENTITY) - 0.4).abs() < 1e-5);
}

#[test]
fn neck_share_is_clamped_and_a_non_finite_breath_tip_takes_its_default() {
    let high = BusAnimator::new(BusAnimatorParams { neck_share: 3.0, ..BusAnimatorParams::default() });
    assert_eq!(high.params().neck_share, 1.0);
    let low = BusAnimator::new(BusAnimatorParams { neck_share: -1.0, ..BusAnimatorParams::default() });
    assert_eq!(low.params().neck_share, 0.0);
    let nan = BusAnimator::new(BusAnimatorParams { neck_share: f32::NAN, breath_tip_rad: f32::INFINITY });
    assert_eq!(nan.params(), BusAnimatorParams::default());
}

#[test]
fn a_non_finite_channel_reads_as_zero_and_the_pose_stays_finite() {
    let channels = [
        Channel::HEAD_YAW,
        Channel::HEAD_PITCH,
        Channel::HEAD_ROLL,
        Channel::LEAN,
        Channel::BREATH,
        Channel::BODY_TURN,
    ];
    let mut rng = Rng::new(0x5EED);
    for channel in channels {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut bus = random_bus(&mut rng);
            let mut zeroed = bus;
            bus.values[channel] = bad;
            zeroed.values[channel] = 0.0;
            let base = random_base(&mut rng);
            let mut out = base;
            let mut expected = base;
            BusAnimator::default().animate(tick(), &bus, &[], &mut out);
            BusAnimator::default().animate(tick(), &zeroed, &[], &mut expected);
            for bone in HumanoidBone::ALL {
                assert!(out.get(bone).is_finite(), "{channel:?} {bad}: {bone} is not finite");
            }
            assert_eq!(out, expected, "{channel:?} {bad} should read as 0");
        }
    }
}

#[test]
fn bones_outside_the_driven_five_are_bit_identical_to_the_base() {
    let mut rng = Rng::new(0xB0E5);
    let bus = random_bus(&mut rng);
    let base = random_base(&mut rng);
    let mut out = base;
    BusAnimator::default().animate(tick(), &bus, &[], &mut out);
    let driven = BoneMask::of(&DRIVEN);
    for bone in HumanoidBone::ALL.iter().copied().filter(|&b| !driven.contains(b)) {
        let before = base.get(bone).to_array().map(f32::to_bits);
        let after = out.get(bone).to_array().map(f32::to_bits);
        assert_eq!(before, after, "{bone} must not change");
    }
}

#[test]
fn animate_allocates_nothing_over_ten_thousand_frames() {
    let mut animator = BusAnimator::default();
    let mut rng = Rng::new(0xA11C);
    let buses: Vec<Bus> = (0..64).map(|_| random_bus(&mut rng)).collect();
    let base = random_base(&mut rng);
    let mut out = base;
    // One warm-up lap, then count the next 10,000 frames on this thread only.
    for (i, bus) in buses.iter().enumerate() {
        animator.animate(Tick::at_rate(i as u64, 30.0), bus, &[], &mut out);
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for frame in 0..10_000_u64 {
        out = base;
        animator.animate(Tick::at_rate(frame, 30.0), &buses[frame as usize % buses.len()], &[], &mut out);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "animate allocated {allocations} times");
}

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
