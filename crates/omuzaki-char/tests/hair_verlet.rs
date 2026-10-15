//! The hair simulation core (`omuzaki_char::hair::physics::verlet`) on synthetic hair: chain construction, input checks,
//! determinism, frame-rate independence, colliders, the frame accumulator, the body blend, and no allocation per frame.
//!
//! The allocation count covers only the measuring thread: the test harness's own threads allocate at unpredictable
//! times (as in `omuzaki-rig/tests/hot_loop.rs`).

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_char::hair::physics::verlet::{ChainSet, MAX_DT, MAX_STEPS, VerletError};
use omuzaki_char_api::{
    Bulge, Falloff, Follow, GroupId, HairClump, HairPhysics, JointRef, PoseSource, SphereCollider, Taper,
};
use omuzaki_core::glam::{Mat4, Quat, Vec3};

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

/// The synthetic head centre (the prototype's head height).
const HEAD: Vec3 = Vec3::new(0.0, 0.778, 0.0);

/// A pose from a head and a chest skinning matrix; every other joint is missing.
#[derive(Clone, Copy)]
struct Pose {
    head: Mat4,
    chest: Mat4,
    root: Mat4,
}

impl Pose {
    const REST: Self = Self { head: Mat4::IDENTITY, chest: Mat4::IDENTITY, root: Mat4::IDENTITY };

    /// The head turned by `yaw` radians about its own centre.
    fn yawed(yaw: f32) -> Self {
        let head =
            Mat4::from_translation(HEAD) * Mat4::from_quat(Quat::from_rotation_y(yaw)) * Mat4::from_translation(-HEAD);
        Self { head, ..Self::REST }
    }
}

impl PoseSource for Pose {
    fn skin_matrix(&self, joint: &JointRef) -> Option<Mat4> {
        if *joint == JointRef::HEAD {
            Some(self.head)
        } else if *joint == JointRef::CHEST {
            Some(self.chest)
        } else {
            None
        }
    }

    fn root(&self) -> Mat4 {
        self.root
    }
}

/// A pose with every joint: the head from `Pose`, everything else at rest.
struct Rigged(Pose);

impl PoseSource for Rigged {
    fn skin_matrix(&self, joint: &JointRef) -> Option<Mat4> {
        Some(self.0.skin_matrix(joint).unwrap_or(Mat4::IDENTITY))
    }

    fn root(&self) -> Mat4 {
        self.0.root
    }
}

fn clump(rest: Vec<Vec3>, follow: Follow) -> HairClump {
    HairClump {
        group: GroupId::new("test").unwrap(),
        rest,
        width: Taper::default(),
        thickness: Bulge::default(),
        stiffness: Falloff::default(),
        shade: 1.0,
        simulate: true,
        follow,
        tint: None,
    }
}

/// A lock from the scalp at angle `a` round the head, `points` rest points 2 cm apart hanging down the side.
fn lock(a: f32, points: usize, follow: Follow) -> HairClump {
    let out = Vec3::new(a.sin(), 0.0, a.cos());
    clump((0..points).map(|i| HEAD + out * 0.1 + Vec3::new(0.0, 0.06 - 0.02 * i as f32, 0.0)).collect(), follow)
}

/// Mira-class hair: 44 head locks and 10 long body locks down the back, with a head collider.
fn mira() -> (Vec<HairClump>, HairPhysics) {
    let mut clumps: Vec<HairClump> = (0..44).map(|n| lock(TAU * n as f32 / 44.0, 13, Follow::Head)).collect();
    clumps.extend((0..10).map(|n| lock(2.4 + 0.15 * n as f32, 25, Follow::Body)));
    let head = SphereCollider { bone: JointRef::HEAD, centre: [0.0, 0.778, 0.0], radius: 0.09 };
    (clumps, HairPhysics { colliders: vec![head], ..HairPhysics::default() })
}

fn settled(clumps: &[HairClump], physics: &HairPhysics, pose: &Pose) -> ChainSet {
    let mut set = ChainSet::from_clumps(clumps, physics, &[]).unwrap();
    set.reset(pose).unwrap();
    set.settle(480);
    set
}

#[test]
fn particles_subsample_the_rest_points_and_always_keep_the_last() {
    let clumps = [lock(0.0, 7, Follow::Head), lock(1.0, 6, Follow::Head)];
    let set = ChainSet::from_clumps(&clumps, &HairPhysics::default(), &[]).unwrap();
    assert_eq!(set.chains().collect::<Vec<_>>(), [0..4, 4..8]);
    assert_eq!(set.rest_indices(), [0, 2, 4, 6, 0, 2, 4, 5]);
    let seg = set.segment_lengths();
    assert!(seg[0] == 0.0 && seg[4] == 0.0, "roots have no segment");
    assert!((seg[1] - 0.04).abs() < 1e-6 && (seg[7] - 0.02).abs() < 1e-6, "{seg:?}");
    let mut copy = set.clone();
    assert_eq!(copy.set_stiffness(&[1.0; 7]).unwrap_err(), VerletError::BadStiffness { expected: 8 });
    assert_eq!(copy.set_stiffness(&[1.1; 8]).unwrap_err(), VerletError::BadStiffness { expected: 8 });
    copy.set_stiffness(&[0.5; 8]).unwrap();
    assert_eq!(copy.stiffness(), [0.5; 8]);
    let k = Falloff::default();
    assert_eq!(set.stiffness()[0], k.at(0.0));
    assert_eq!(set.stiffness()[6], k.at(2.0 / 3.0), "by place in the chain, not by rest point");
    assert_eq!(set.stiffness()[7], k.at(1.0));
}

#[test]
fn rigid_clumps_are_skipped_and_bad_input_is_refused() {
    let mut rigid = lock(0.0, 5, Follow::Head);
    rigid.simulate = false;
    let set = ChainSet::from_clumps(&[rigid, lock(0.0, 5, Follow::Head)], &HairPhysics::default(), &[]).unwrap();
    assert_eq!(set.positions().len(), 3);

    let physics = HairPhysics::default();
    let short = clump(vec![HEAD], Follow::Head);
    assert_eq!(ChainSet::from_clumps(&[short], &physics, &[]).unwrap_err(), VerletError::BadClump { clump: 0 });
    let mut nan = lock(0.0, 5, Follow::Head);
    nan.rest[3].y = f32::NAN;
    let err = ChainSet::from_clumps(&[lock(0.0, 3, Follow::Head), nan], &physics, &[]).unwrap_err();
    assert_eq!(err, VerletError::BadClump { clump: 1 });
    let mut loose = lock(0.0, 5, Follow::Head);
    loose.stiffness.tip = 1.5;
    assert!(matches!(ChainSet::from_clumps(&[loose], &physics, &[]), Err(VerletError::BadClump { .. })));
    let bad = HairPhysics { damping: 1.2, ..HairPhysics::default() };
    assert!(matches!(ChainSet::from_clumps(&[], &bad, &[]), Err(VerletError::Physics(_))));
    let sphere = SphereCollider { bone: JointRef::HEAD, centre: [0.0, f64::INFINITY, 0.0], radius: 0.1 };
    let err = ChainSet::from_clumps(&[], &physics, &[sphere]).unwrap_err();
    assert_eq!(err, VerletError::BadCollider { collider: 0 });
    // Finite as f64 but not as f32: refused at the door instead of turning infinite in `targets`.
    let wide = SphereCollider { bone: JointRef::HEAD, centre: [0.0, 0.7, 0.0], radius: 1e40 };
    let far = SphereCollider { bone: JointRef::HEAD, centre: [f64::MAX, 0.7, 0.0], radius: 0.1 };
    let ok = SphereCollider { bone: JointRef::HEAD, centre: [0.0, 0.7, 0.0], radius: 0.1 };
    let err = ChainSet::from_clumps(&[], &physics, &[ok.clone(), wide]).unwrap_err();
    assert_eq!(err, VerletError::BadCollider { collider: 1 });
    let err = ChainSet::from_clumps(&[], &physics, &[far, ok]).unwrap_err();
    assert_eq!(err, VerletError::BadCollider { collider: 0 });
    // Particles at one place, or too far apart for an f32 distance, make no segment.
    let doubled = clump(vec![HEAD, HEAD + Vec3::Y * 0.01, HEAD, HEAD], Follow::Head);
    assert_eq!(ChainSet::from_clumps(&[doubled], &physics, &[]).unwrap_err(), VerletError::BadClump { clump: 0 });
    let huge = clump(vec![Vec3::splat(-3e38), Vec3::splat(3e38)], Follow::Head);
    assert_eq!(ChainSet::from_clumps(&[huge], &physics, &[]).unwrap_err(), VerletError::BadClump { clump: 0 });

    let mut set = ChainSet::from_clumps(&[lock(0.0, 5, Follow::Head)], &physics, &[]).unwrap();
    assert_eq!(set.step(-0.1).unwrap_err(), VerletError::BadInput { what: "dt" });
    assert_eq!(set.step(f32::NAN).unwrap_err(), VerletError::BadInput { what: "dt" });
    assert_eq!(set.set_wind(Vec3::NAN).unwrap_err(), VerletError::BadInput { what: "wind" });
    let flat = Pose { root: Mat4::ZERO, ..Pose::REST };
    assert_eq!(set.targets(&flat).unwrap_err(), VerletError::BadInput { what: "root" });
}

#[test]
fn a_missing_joint_is_an_error_that_changes_nothing() {
    let neck = SphereCollider { bone: JointRef::NECK, centre: [0.0, 0.6, 0.0], radius: 0.05 };
    let physics = HairPhysics { colliders: vec![neck], ..HairPhysics::default() };
    let mut set = ChainSet::from_clumps(&[lock(0.0, 5, Follow::Head)], &physics, &[]).unwrap();
    let before = set.spheres().to_vec();
    assert_eq!(set.reset(&Pose::yawed(1.0)).unwrap_err(), VerletError::MissingJoint { joint: JointRef::NECK });
    assert_eq!(set.spheres(), before.as_slice());
    assert_eq!(set.positions(), set.rest(), "nothing moved");
}

#[test]
fn the_hairs_own_colliders_replace_the_rigs() {
    let rig = SphereCollider { bone: JointRef::HEAD, centre: [0.0, 0.778, 0.0], radius: 0.09 };
    let own = SphereCollider { bone: JointRef::CHEST, centre: [0.0, 0.5, 0.0], radius: 0.12 };
    let clumps = [lock(0.0, 5, Follow::Head)];
    let mut set = ChainSet::from_clumps(&clumps, &HairPhysics::default(), std::slice::from_ref(&rig)).unwrap();
    set.targets(&Pose::REST).unwrap();
    let near =
        |s: &[(Vec3, f32)], c: Vec3, r: f32| s.len() == 1 && s[0].0.distance(c) < 1e-6 && (s[0].1 - r).abs() < 1e-6;
    assert!(near(set.spheres(), HEAD, 0.098), "{:?}", set.spheres());
    let physics = HairPhysics { colliders: vec![own], ..HairPhysics::default() };
    let mut set = ChainSet::from_clumps(&clumps, &physics, &[rig]).unwrap();
    set.targets(&Pose::REST).unwrap();
    assert!(near(set.spheres(), Vec3::new(0.0, 0.5, 0.0), 0.128), "{:?}", set.spheres());
}

#[test]
fn two_runs_are_byte_identical() {
    let (clumps, physics) = mira();
    let run = || {
        let mut set = settled(&clumps, &physics, &Pose::REST);
        set.set_wind(Vec3::new(0.3, 0.0, 0.5)).unwrap();
        for i in 0..120 {
            set.targets(&Pose::yawed(0.01 * i as f32)).unwrap();
            set.step(1.0 / 60.0).unwrap();
        }
        set.positions().iter().flat_map(|p| p.to_array().map(f32::to_bits)).collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn the_settled_shape_does_not_depend_on_the_frame_rate() {
    let (clumps, physics) = mira();
    let run = |dt: f32, frames: u32| {
        let mut set = ChainSet::from_clumps(&clumps, &physics, &[]).unwrap();
        set.reset(&Pose::REST).unwrap();
        // The head turns 0.4 rad over the first second, then holds: each rate samples the turn at its own frames.
        for f in 1..=frames {
            set.targets(&Pose::yawed(0.4 * (f as f32 * dt).min(1.0))).unwrap();
            set.step(dt).unwrap();
        }
        set
    };
    let (slow, fast) = (run(1.0 / 30.0, 60), run(1.0 / 144.0, 288));
    let worst = slow.positions().iter().zip(fast.positions()).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
    eprintln!("1/30 vs 1/144 after 2 s: worst particle difference {worst:.2e} m");
    assert!(worst <= 3e-3, "{worst} m");
    let sagged = slow.positions().iter().zip(slow.rest()).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
    assert!(sagged > 1e-3, "gravity and the turn moved the hair ({sagged} m), so the comparison is not vacuous");
}

#[test]
fn colliders_hold_after_settling() {
    // Locks styled through the head collider: the spring pulls them in, the collider must keep them out.
    let clumps: Vec<_> = (0..8).map(|n| lock(TAU * n as f32 / 8.0, 13, Follow::Head)).collect();
    let ball = SphereCollider { bone: JointRef::HEAD, centre: [0.0, 0.74, 0.0], radius: 0.11 };
    let physics = HairPhysics { colliders: vec![ball], ..HairPhysics::default() };
    let set = settled(&clumps, &physics, &Pose::yawed(0.3));
    let centre = Vec3::new(0.0, 0.74, 0.0);
    let reach = 0.11 + physics.collider_margin as f32; // the margin is part of the collider the hair keeps out of
    let mut deepest = f32::NEG_INFINITY;
    for range in set.chains() {
        for p in &set.positions()[range.start + 1..range.end] {
            deepest = deepest.max(reach - p.distance(centre));
        }
    }
    eprintln!("deepest particle inside the collider: {deepest:.2e} m");
    assert!(deepest <= 1e-3, "{deepest} m inside");
    let inside_at_rest = set.rest().iter().filter(|p| p.distance(centre) < 0.11).count();
    assert!(inside_at_rest > 0, "the styled shape enters the collider, so the test is not vacuous");
}

#[test]
fn overlapping_colliders_resolve_in_order_and_stay_within_the_margin() {
    // The bench's colliders: head and neck overlap, as do the chest and shoulders.
    let (clumps, mut physics) = mira();
    physics.colliders = vec![
        SphereCollider { bone: JointRef::HEAD, centre: [0.0, 0.778, 0.0], radius: 0.09 },
        SphereCollider { bone: JointRef::NECK, centre: [0.0, 0.66, -0.01], radius: 0.045 },
        SphereCollider { bone: JointRef::CHEST, centre: [0.0, 0.52, -0.02], radius: 0.11 },
        SphereCollider { bone: JointRef::CHEST, centre: [0.12, 0.6, -0.02], radius: 0.06 },
        SphereCollider { bone: JointRef::CHEST, centre: [-0.12, 0.6, -0.02], radius: 0.06 },
    ];
    let mut set = ChainSet::from_clumps(&clumps, &physics, &[]).unwrap();
    let pose = Rigged(Pose::REST);
    set.reset(&pose).unwrap();
    set.settle(480);
    let (margin, mut deepest) = (physics.collider_margin as f32, f32::NEG_INFINITY);
    for i in 0..240 {
        set.targets(&Rigged(Pose::yawed(0.52 * (i as f32 / 90.0 * TAU).sin()))).unwrap();
        set.step(1.0 / 90.0).unwrap();
        for range in set.chains() {
            for p in &set.positions()[range.start + 1..range.end] {
                for &(centre, reach) in set.spheres() {
                    deepest = deepest.max(reach - margin - p.distance(centre));
                }
            }
        }
    }
    eprintln!(
        "overlapping colliders, head swinging: deepest particle {deepest:.2e} m inside a collider (margin excluded)"
    );
    assert!(deepest <= margin, "{deepest} m inside");
}

#[test]
fn a_particle_at_a_collider_centre_leaves_along_its_segment() {
    // The tip's styled place is the collider's centre and it is fully stiff: the spring puts it there every step.
    let rest = vec![HEAD + Vec3::new(0.0, 0.2, 0.0), HEAD + Vec3::new(0.0, 0.1, 0.0), HEAD];
    let ball = SphereCollider { bone: JointRef::HEAD, centre: [0.0, 0.778, 0.0], radius: 0.03 };
    let physics = HairPhysics { gravity_scale: 0.0, iterations: 1, colliders: vec![ball], ..HairPhysics::default() };
    let mut set = ChainSet::from_clumps(&[clump(rest, Follow::Head)], &physics, &[]).unwrap();
    set.set_stiffness(&[1.0, 1.0]).unwrap();
    set.reset(&Pose::REST).unwrap();
    set.settle(3);
    let tip = set.positions()[1];
    assert!((tip.distance(HEAD) - set.spheres()[0].1).abs() < 1e-6, "pushed out to the surface: {tip}");
    assert!(tip.y < HEAD.y && tip.x == 0.0 && tip.z == 0.0, "on along the segment from the parent above: {tip}");
}

#[test]
fn the_accumulator_carries_the_remainder_caps_the_steps_and_resets() {
    let physics = HairPhysics::default();
    let mut set = ChainSet::from_clumps(&[lock(0.0, 5, Follow::Head)], &physics, &[]).unwrap();
    set.reset(&Pose::REST).unwrap();
    assert_eq!(set.step(0.5 / 90.0).unwrap(), 0);
    assert_eq!(set.step(0.75 / 90.0).unwrap(), 1);
    assert!((set.carried() - 0.25 / 90.0).abs() < 1e-6, "{}", set.carried());
    let before = set.carried();
    assert_eq!(set.step(1.0).unwrap(), MAX_STEPS);
    let expected = before + MAX_DT - 8.0 / 90.0;
    assert!((set.carried() - expected).abs() < 1e-6, "a hitch counts as {MAX_DT} s: {}", set.carried());
    set.step(0.9 / 90.0).unwrap();
    set.reset(&Pose::REST).unwrap();
    assert_eq!(set.carried(), 0.0);
    assert_eq!(set.positions(), set.rest(), "back at the styled shape");
    assert_eq!(set.step(0.5 / 90.0).unwrap(), 0, "no remainder survived the reset");
}

#[test]
fn body_locks_follow_the_chest_below_the_blend_and_the_head_above() {
    let rest: Vec<Vec3> = (0..21).map(|i| Vec3::new(0.0, 0.8 - 0.02 * i as f32, -0.12)).collect();
    let mut set = ChainSet::from_clumps(&[clump(rest, Follow::Body)], &HairPhysics::default(), &[]).unwrap();
    let shift = Mat4::from_translation(Vec3::new(0.1, 0.0, 0.0));
    set.reset(&Pose { head: shift, ..Pose::REST }).unwrap();
    let (top, bottom) = (set.positions()[0], set.positions()[10]);
    assert!((top.x - 0.1).abs() < 1e-6, "the root (y 0.8) follows the head: {top}");
    assert!(bottom.x.abs() < 1e-6, "the tip (y 0.4) follows the chest: {bottom}");
}

#[test]
fn the_first_targets_snap_to_the_pose_and_the_distance_pass_is_one_sided() {
    let physics = HairPhysics { gravity_scale: 0.0, ..HairPhysics::default() };
    let mut set = ChainSet::from_clumps(&[lock(0.0, 7, Follow::Head)], &physics, &[]).unwrap();
    let pose = Pose::yawed(0.6);
    set.targets(&pose).unwrap();
    let mut reset = set.clone();
    reset.reset(&pose).unwrap();
    assert_eq!(set.positions(), reset.positions(), "the first targets after building snap like a reset");
    set.targets(&Pose::REST).unwrap();
    assert_eq!(set.positions(), reset.positions(), "later targets do not snap");
    // Heavy gravity stretches the root segment; the one-sided pass (child onto parent, root to tip) puts every segment
    // back at its rest length exactly, which a shared correction would not after two iterations.
    let heavy = HairPhysics { gravity_scale: 3.0, ..HairPhysics::default() };
    let mut stretched = ChainSet::from_clumps(&[lock(0.0, 7, Follow::Head)], &heavy, &[]).unwrap();
    stretched.set_stiffness(&[1.0, 0.0, 0.0, 0.0]).unwrap();
    stretched.reset(&Pose::REST).unwrap();
    stretched.settle(1);
    let p = stretched.positions();
    for (j, seg) in stretched.segment_lengths().iter().enumerate().skip(1) {
        assert!((p[j].distance(p[j - 1]) - seg).abs() < 1e-6, "segment {j} at rest length after a step");
    }
}

#[test]
fn full_stiffness_holds_the_styled_shape() {
    let clumps = [lock(0.0, 13, Follow::Head)];
    let mut set = ChainSet::from_clumps(&clumps, &HairPhysics::default(), &[]).unwrap();
    set.set_stiffness(&[1.0; 7]).unwrap();
    let pose = Pose::yawed(0.7);
    set.reset(&Pose::REST).unwrap();
    set.targets(&pose).unwrap();
    set.settle(1);
    let mut expected = set.clone();
    expected.reset(&pose).unwrap();
    let worst = set.positions().iter().zip(expected.positions()).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
    assert!(worst < 1e-5, "a fully stiff lock lands on the turned shape in one step ({worst} m off)");
}

#[test]
fn wind_and_the_root_transform_move_the_result() {
    let physics = HairPhysics { gravity_scale: 0.0, ..HairPhysics::default() };
    let mut set = settled(&[lock(0.0, 13, Follow::Head)], &physics, &Pose::REST);
    let still = set.positions().to_vec();
    set.set_wind(Vec3::new(2.0, 0.0, 0.0)).unwrap();
    set.settle(90);
    assert!(set.positions()[6].x > still[6].x + 1e-3, "the wind blew the tip along +x");
    let root = Mat4::from_translation(Vec3::new(1.0, 0.0, 0.0));
    set.targets(&Pose { head: root, chest: root, root }).unwrap();
    let p = set.positions()[0];
    assert_eq!(set.to_character(p), p - Vec3::X);
}

#[test]
fn a_frame_allocates_nothing() {
    let (clumps, physics) = mira();
    let mut set = settled(&clumps, &physics, &Pose::REST);
    let poses: Vec<Pose> = (0..16).map(|i| Pose::yawed(0.05 * i as f32)).collect();
    let mut frame = |i: usize| {
        set.targets(&poses[i % poses.len()]).unwrap();
        set.set_wind(Vec3::new(0.2, 0.0, 0.1)).unwrap();
        set.step(1.0 / 60.0).unwrap();
    };
    frame(0);

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for i in 1..=1000 {
        frame(i);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "1000 hair frames made {allocations} allocations");
}
