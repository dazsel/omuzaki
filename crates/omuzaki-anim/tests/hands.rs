//! The hand-shape library (RG-09), held to its acceptance on a synthetic VRM 1.0 skeleton: the 15 required bones
//! and all 30 finger bones, built with `Skeleton::humanoid` at human proportions in the normalised T-pose. Fingertips
//! are found by forward kinematics through `Skeleton::world_pose`, so the checks measure where the fingers end up, not
//! what the code under test says it did. VRM leaves the thumb's rest to the modeller, so the thumb of the rig can be
//! set (`skeleton_with`) and the shapes are checked on thumbs that rest elsewhere than the nominal 45 degrees.

// Test helpers may panic (a panic is a clear test failure). The counting allocator is the only way to prove "no
// allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::float_cmp, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::hands::{
    Finger, FingerCurl, HandLibrary, HandRest, HandRestError, HandShape, HandShapeError, Side, apply,
};
use omuzaki_anim_api::{BoneDef, HumanoidBone, HumanoidPose, Pose, Skeleton, Transform};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, OpKind, Scope, find};
use serde_json::json;

// ---- counting allocator (see tests/retarget_alloc.rs for the pattern) ----

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

// ---- the synthetic hand ----

/// Each finger of the left hand: where its knuckle sits relative to the hand bone, then the lengths of its three
/// bones (the last is the distal bone, whose far end is the fingertip). Metres; a hand 0.19 m long.
struct Dims {
    finger: Finger,
    base: Vec3,
    lengths: [f32; 3],
}

const FORWARD_OUT: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn dims() -> [Dims; 5] {
    [
        Dims { finger: Finger::Thumb, base: Vec3::new(0.028, -0.003, 0.022), lengths: [0.046, 0.032, 0.027] },
        Dims { finger: Finger::Index, base: Vec3::new(0.090, 0.0, 0.030), lengths: [0.045, 0.026, 0.021] },
        Dims { finger: Finger::Middle, base: Vec3::new(0.092, 0.0, 0.011), lengths: [0.049, 0.030, 0.022] },
        Dims { finger: Finger::Ring, base: Vec3::new(0.087, 0.0, -0.009), lengths: [0.045, 0.028, 0.021] },
        Dims { finger: Finger::Little, base: Vec3::new(0.079, 0.0, -0.027), lengths: [0.036, 0.019, 0.018] },
    ]
}

/// Where the left thumb's bones point at rest: the bone from the metacarpal joint to the proximal joint, and the one from
/// there to the distal joint, which the tip bone continues.
type ThumbRest = [Vec3; 2];

/// One thumb rest per hand, indexed as [`Side`] is read: left, then right. Each is in the left-hand convention.
type Thumbs = [ThumbRest; 2];

/// The nominal thumb: both bones 45 degrees forward of the fingers in the palm's plane.
fn nominal_thumb() -> ThumbRest {
    [Vec3::new(FORWARD_OUT, 0.0, FORWARD_OUT); 2]
}

/// A direction `azimuth` degrees from the fingers in the palm's plane (toward the thumb side) and `elevation` degrees out
/// of it (toward the palm, which faces -Y).
fn direction(azimuth: f32, elevation: f32) -> Vec3 {
    let (az, el) = (azimuth.to_radians(), elevation.to_radians());
    Vec3::new(az.cos() * el.cos(), -el.sin(), az.sin() * el.cos())
}

/// The thumbs of the rigs a modeller might make, to hold the shapes to: the nominal one, a thumb that rests close to the
/// fingers, one raised well out of the palm's plane, and one that bends between its bones the way the template's does
/// (a metacarpal along the fingers, a free thumb 30 degrees out and 15 up).
fn thumbs() -> [(&'static str, ThumbRest); 4] {
    [
        ("nominal", nominal_thumb()),
        ("close to the fingers", [direction(22.0, 0.0), direction(22.0, 0.0)]),
        ("raised", [direction(60.0, 25.0), direction(60.0, 25.0)]),
        ("bent", [direction(3.0, -8.0), direction(30.0, 15.0)]),
    ]
}

/// The direction a finger's bones point in the T-pose, for the left hand: along +X, the thumb as the rig rests it.
fn rest_direction(finger: Finger, bone: usize, thumb: &ThumbRest) -> Vec3 {
    if finger == Finger::Thumb { thumb[bone.min(1)] } else { Vec3::X }
}

/// The index finger's length, bone to bone, the unit the heart is measured in.
fn index_length() -> f32 {
    dims()[1].lengths.iter().sum()
}

/// The reflection of a left-hand vector into the right hand's space.
fn right_of(v: Vec3) -> Vec3 {
    Vec3::new(-v.x, v.y, v.z)
}

fn side_of(bone: HumanoidBone) -> Side {
    if bone.name().starts_with("left") { Side::Left } else { Side::Right }
}

/// A bone's offset from its parent in the T-pose, in world axes.
fn rest_offset(bone: HumanoidBone, thumbs: &Thumbs) -> Vec3 {
    use HumanoidBone as B;
    let s = if side_of(bone) == Side::Left { 1.0 } else { -1.0 };
    match bone {
        B::Hips => Vec3::new(0.0, 0.95, 0.0),
        B::Spine => Vec3::new(0.0, 0.1, 0.0),
        B::Head => Vec3::new(0.0, 0.5, 0.0),
        B::LeftUpperLeg | B::RightUpperLeg => Vec3::new(s * 0.08, -0.05, 0.0),
        B::LeftLowerLeg | B::RightLowerLeg | B::LeftFoot | B::RightFoot => Vec3::new(0.0, -0.42, 0.0),
        B::LeftUpperArm | B::RightUpperArm => Vec3::new(s * 0.15, 0.4, 0.0),
        B::LeftLowerArm | B::RightLowerArm => Vec3::new(s * 0.27, 0.0, 0.0),
        B::LeftHand | B::RightHand => Vec3::new(s * 0.25, 0.0, 0.0),
        _ => finger_offset(bone, thumbs),
    }
}

fn finger_offset(bone: HumanoidBone, thumbs: &Thumbs) -> Vec3 {
    let side = side_of(bone);
    let thumb = &thumbs[usize::from(side == Side::Right)];
    for d in dims() {
        let [knuckle, middle, tip] = d.finger.bones(Side::Left);
        let [rknuckle, rmiddle, rtip] = d.finger.bones(Side::Right);
        let left = match bone {
            b if b == knuckle || b == rknuckle => d.base,
            b if b == middle || b == rmiddle => rest_direction(d.finger, 0, thumb) * d.lengths[0],
            b if b == tip || b == rtip => rest_direction(d.finger, 1, thumb) * d.lengths[1],
            _ => continue,
        };
        return if side == Side::Left { left } else { right_of(left) };
    }
    unreachable!("{bone} is not a finger bone");
}

/// The rig with the nominal thumb.
fn skeleton() -> Skeleton {
    skeleton_with(&nominal_thumb())
}

/// The rig with a thumb that rests as `thumb` on both hands.
fn skeleton_with(thumb: &ThumbRest) -> Skeleton {
    rig(&[*thumb; 2], &[], no_turn)
}

/// The rig with the thumbs given per hand, without the `omit` bones. `turn` rotates each bone's rest offset from its
/// parent (the identity for the T-pose): a turn on a hand bone tilts the forearm and palm, a turn on a finger bone
/// curls it.
fn rig(thumbs: &Thumbs, omit: &[HumanoidBone], turn: impl Fn(HumanoidBone) -> Quat) -> Skeleton {
    let mut bones: Vec<HumanoidBone> = HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect();
    bones.extend(HumanoidBone::ALL.into_iter().filter(|b| b.name().contains("Thumb") || is_finger(*b)));
    bones.retain(|b| !omit.contains(b));
    Skeleton::humanoid(&bones, |b| Transform::from_translation(turn(b) * rest_offset(b, thumbs))).unwrap()
}

/// The T-pose: no bone turned.
fn no_turn(_: HumanoidBone) -> Quat {
    Quat::IDENTITY
}

fn is_finger(bone: HumanoidBone) -> bool {
    ["Index", "Middle", "Ring", "Little"].iter().any(|f| bone.name().contains(f))
}

/// Where the fingers of one hand are in a pose, in metres from the hand bone's origin, along the world's axes.
struct Hand {
    /// The hand bone's origin: the palm.
    palm: Vec3,
    /// Every finger's three bone origins, thumb first.
    joints: [[Vec3; 3]; 5],
    /// Every fingertip.
    tips: [Vec3; 5],
}

impl Hand {
    fn tip(&self, f: Finger) -> Vec3 {
        self.tips[f as usize]
    }

    fn joint(&self, f: Finger, i: usize) -> Vec3 {
        self.joints[f as usize][i]
    }

    /// Every point of the hand that is not the palm: 15 joints and 5 tips.
    fn landmarks(&self) -> impl Iterator<Item = (Finger, Vec3)> + '_ {
        Finger::ALL
            .into_iter()
            .flat_map(|f| self.joints[f as usize].into_iter().chain([self.tips[f as usize]]).map(move |p| (f, p)))
    }
}

/// Forward kinematics of `pose` on the rig with the nominal thumb, read off for one hand.
fn hand(pose: &HumanoidPose, side: Side) -> Hand {
    hand_on(&nominal_thumb(), pose, side)
}

/// Forward kinematics: where every bone of `sk` is when posed as `pose`, in world axes.
fn world_of(sk: &Skeleton, pose: &HumanoidPose) -> Pose {
    let mut local = Pose::IDENTITY;
    for (slot, def) in local.local.iter_mut().zip(sk.bones()) {
        *slot = Transform {
            translation: def.rest.translation,
            rotation: def.humanoid.map_or(Quat::IDENTITY, |h| pose.get(h)),
        };
    }
    let mut world = Pose::IDENTITY;
    sk.world_pose(&local, &mut world);
    world
}

/// Forward kinematics of `pose` on the rig whose thumb rests as `thumb`, read off for one hand.
fn hand_on(thumb: &ThumbRest, pose: &HumanoidPose, side: Side) -> Hand {
    let sk = skeleton_with(thumb);
    let world = world_of(&sk, pose);
    let at = |b: HumanoidBone| world.local[sk.humanoid_bone(b).unwrap().index()];
    let hand_bone = if side == Side::Left { HumanoidBone::LeftHand } else { HumanoidBone::RightHand };
    let origin = at(hand_bone).translation;
    let mut out = Hand { palm: Vec3::ZERO, joints: [[Vec3::ZERO; 3]; 5], tips: [Vec3::ZERO; 5] };
    for d in dims() {
        let bones = d.finger.bones(side);
        for (i, b) in bones.into_iter().enumerate() {
            out.joints[d.finger as usize][i] = at(b).translation - origin;
        }
        let along = rest_direction(d.finger, 2, thumb) * d.lengths[2];
        let along = if side == Side::Left { along } else { right_of(along) };
        let distal = at(bones[2]);
        out.tips[d.finger as usize] = distal.translation + distal.rotation * along - origin;
    }
    out
}

fn library() -> &'static HandLibrary {
    HandLibrary::builtin().unwrap()
}

fn shape(id: &str) -> &'static HandShape {
    library().get(id).unwrap_or_else(|| panic!("no shape {id}"))
}

/// How the thumbs of the synthetic rig rest, measured the way a bind does.
fn rest() -> HandRest {
    HandRest::of(&skeleton()).unwrap()
}

fn posed(id: &str, side: Side) -> HumanoidPose {
    let mut pose = HumanoidPose::REST;
    apply(&mut pose, side, &rest(), shape(id), 1.0);
    pose
}

fn degrees(f: Finger, id: &str, pick: impl Fn(&FingerCurl) -> f32) -> f32 {
    pick(&shape(id).curl(f)).to_degrees()
}

fn angle_between(a: Vec3, b: Vec3) -> f32 {
    a.angle_between(b).to_degrees()
}

fn assert_near(a: Vec3, b: Vec3, what: &str) {
    assert!(a.abs_diff_eq(b, 1e-5), "{what}: {a} is not {b}");
}

/// The shortest distance from `p` to the segment `a..b`.
fn distance_to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

const BONES_OF_THE_LEFT_HAND: usize = 15;

/// A finger folded into the palm: its tip is back within 60% of the finger's length from its own knuckle.
fn is_curled(h: &Hand, f: Finger) -> bool {
    let length: f32 = dims()[f as usize].lengths.iter().sum();
    h.tip(f).distance(h.joint(f, 0)) < 0.6 * length
}

// ---- the library ----

#[test]
fn the_library_has_nine_shapes_and_a_rest() {
    let ids: Vec<_> = library().iter().map(|s| s.id.to_string()).collect();
    assert_eq!(ids, ["open", "fist", "point", "peace", "heart", "grip", "thumbs-up", "rest", "spread", "flat"]);
    for s in library().iter() {
        s.validate().unwrap();
        assert!(!s.summary.is_empty());
    }
    assert!(std::ptr::eq(library(), HandLibrary::builtin().unwrap()), "parsed once");
}

#[test]
fn every_shape_is_deterministic() {
    for s in library().iter() {
        let again = HandLibrary::builtin().unwrap().get(&s.id).unwrap();
        assert_eq!(s, again);
        for side in [Side::Left, Side::Right] {
            assert_eq!(posed(&s.id, side).checksum(), posed(&s.id, side).checksum(), "{} on {side:?}", s.id);
        }
    }
}

#[test]
fn applying_a_shape_touches_only_the_fifteen_finger_bones_of_that_hand() {
    let left_fingers: Vec<_> = Finger::ALL.iter().flat_map(|f| f.bones(Side::Left)).collect();
    assert_eq!(left_fingers.len(), BONES_OF_THE_LEFT_HAND);
    for s in library().iter() {
        let pose = posed(&s.id, Side::Left);
        let changed: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| pose.get(*b) != Quat::IDENTITY).collect();
        assert!(!changed.is_empty() && changed.iter().all(|b| left_fingers.contains(b)), "{}: {changed:?}", s.id);
        assert_eq!(pose.hips_offset, Vec3::ZERO);
    }
}

// ---- mirroring ----

#[test]
fn every_shape_mirrors_exactly() {
    for s in library().iter() {
        let left = posed(&s.id, Side::Left);
        let right = posed(&s.id, Side::Right);
        assert_eq!(right, left.mirrored(), "{}: the right hand is the left one reflected", s.id);
        assert_eq!(left.mirrored().mirrored(), left);
        // The same on the skeleton: every right fingertip is the left one with x negated.
        let (l, r) = (hand(&left, Side::Left), hand(&right, Side::Right));
        for f in Finger::ALL {
            assert!(r.tip(f).abs_diff_eq(right_of(l.tip(f)), 1e-5), "{} {f:?}: {} vs {}", s.id, r.tip(f), l.tip(f));
        }
    }
}

// ---- weight ----

/// The rotations the module docs promise for the left middle finger, worked out by hand: a turn about -Z per joint.
fn expected_middle(curl: [f32; 3]) -> [Quat; 3] {
    curl.map(|a| Quat::from_axis_angle(-Vec3::Z, a))
}

#[test]
fn weight_zero_is_the_identity_and_weight_one_is_the_shape() {
    let mut start = HumanoidPose::REST;
    start.set(HumanoidBone::LeftMiddleIntermediate, Quat::from_rotation_x(0.3));
    start.set(HumanoidBone::Head, Quat::from_rotation_y(0.2));
    start.hips_offset = Vec3::new(0.1, 0.2, 0.3);

    for w in [0.0, -1.0, f32::NAN, f32::NEG_INFINITY] {
        let mut pose = start;
        apply(&mut pose, Side::Left, &rest(), shape("fist"), w);
        assert_eq!(pose, start, "weight {w} must leave the pose alone");
    }

    let fist = shape("fist").curl(Finger::Middle);
    let want = expected_middle([fist.mcp, fist.pip, fist.dip]);
    for w in [1.0, 2.5, f32::INFINITY] {
        let mut pose = start;
        apply(&mut pose, Side::Left, &rest(), shape("fist"), w);
        let got = Finger::Middle.bones(Side::Left).map(|b| pose.get(b));
        // The knuckle also swings by `spread` (0 for the fist), so the whole rotation is the flexion alone.
        for (g, e) in got.iter().zip(want) {
            assert!(g.abs_diff_eq(e, 1e-6), "weight {w}: {g} is not {e}");
        }
        assert_eq!(pose.get(HumanoidBone::Head), start.get(HumanoidBone::Head));
        assert_eq!(pose.hips_offset, start.hips_offset);
    }
}

#[test]
fn a_half_weight_fist_is_half_curled() {
    let mut pose = HumanoidPose::REST;
    apply(&mut pose, Side::Left, &rest(), shape("fist"), 0.5);
    let fist = shape("fist").curl(Finger::Middle);
    for (bone, full) in Finger::Middle.bones(Side::Left).into_iter().zip([fist.mcp, fist.pip, fist.dip]) {
        let turned = pose.get(bone).angle_between(Quat::IDENTITY);
        assert!((turned - 0.5 * full).abs() < 1e-5, "{bone}: {turned} vs {}", 0.5 * full);
    }
}

#[test]
fn weight_blends_from_the_pose_that_is_already_there() {
    let mut pose = posed("fist", Side::Left);
    apply(&mut pose, Side::Left, &rest(), shape("open"), 0.5);
    let fist = shape("fist").curl(Finger::Middle);
    let turned = pose.get(HumanoidBone::LeftMiddleIntermediate).angle_between(Quat::IDENTITY);
    assert!((turned - 0.5 * fist.pip).abs() < 1e-5);
}

// ---- axes, on the skeleton ----

#[test]
fn flexion_curls_toward_the_palm_and_spread_swings_toward_the_thumb() {
    // The middle finger's knuckle turned 90 degrees: its first bone points straight down from the knuckle.
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::LeftMiddleProximal, Quat::from_axis_angle(-Vec3::Z, 90f32.to_radians()));
    let knuckle = hand(&pose, Side::Left).joint(Finger::Middle, 0);
    let bent = hand(&pose, Side::Left).joint(Finger::Middle, 1);
    assert_near(bent - knuckle, Vec3::new(0.0, -0.049, 0.0), "a quarter turn at the knuckle points the finger down");

    // The shape does the same through the library: a one-joint shape from the tracker entry point.
    let mut flexed = HumanoidPose::REST;
    let curls = HandShape::from_curls([0.0, 0.0, 1.0, 0.0, 0.0]).unwrap();
    apply(&mut flexed, Side::Left, &rest(), &curls, 1.0);
    let h = hand(&flexed, Side::Left);
    assert!(is_curled(&h, Finger::Middle), "a curled middle finger folds into the palm: {}", h.tip(Finger::Middle));
    assert!(h.tip(Finger::Index).y.abs() < 1e-6, "the index finger stays straight");

    // Spread: the index finger of `open` leans 10 degrees toward +Z (the thumb side), the little finger 20 the other way.
    let open = hand(&posed("open", Side::Left), Side::Left);
    let lean = |f: Finger| {
        let d = open.joint(f, 1) - open.joint(f, 0);
        d.z.atan2(d.x).to_degrees()
    };
    assert!((lean(Finger::Index) - 10.0).abs() < 1e-3, "{}", lean(Finger::Index));
    assert!((lean(Finger::Middle)).abs() < 1e-3);
    assert!((lean(Finger::Little) + 20.0).abs() < 1e-3, "{}", lean(Finger::Little));
}

// ---- the shapes ----

#[test]
fn the_open_hand_is_straight_with_ten_degrees_between_neighbours() {
    let curls = shape("open");
    for f in Finger::ALL {
        let c = curls.curl(f);
        assert_eq!((c.mcp, c.pip, c.dip), (0.0, 0.0, 0.0), "{f:?} is not flexed");
    }
    let open = hand(&posed("open", Side::Left), Side::Left);
    for d in dims() {
        let reach = open.tip(d.finger).distance(open.joint(d.finger, 0));
        assert!((reach - d.lengths.iter().sum::<f32>()).abs() < 1e-5, "{:?} is straight", d.finger);
    }
    let along = |f: Finger| open.tip(f) - open.joint(f, 0);
    for pair in [[Finger::Index, Finger::Middle], [Finger::Middle, Finger::Ring], [Finger::Ring, Finger::Little]] {
        let between = angle_between(along(pair[0]), along(pair[1]));
        assert!((between - 10.0).abs() < 0.5, "{pair:?}: {between} degrees");
    }
}

#[test]
fn the_fist_is_inside_the_stated_bands_with_the_thumb_across() {
    for f in [Finger::Index, Finger::Middle, Finger::Ring, Finger::Little] {
        let (mcp, pip, dip) =
            (degrees(f, "fist", |c| c.mcp), degrees(f, "fist", |c| c.pip), degrees(f, "fist", |c| c.dip));
        assert!((80.0..=90.0).contains(&mcp), "{f:?} knuckle {mcp}");
        assert!((90.0..=100.0).contains(&pip), "{f:?} middle joint {pip}");
        assert!((60.0..=70.0).contains(&dip), "{f:?} tip joint {dip}");
        assert_eq!(degrees(f, "fist", |c| c.spread), 0.0);
    }
    // The bands also hold in the pose itself: the middle joint of the middle finger turns by its `pip` about -Z.
    let pose = posed("fist", Side::Left);
    let turned = pose.get(HumanoidBone::LeftMiddleIntermediate).angle_between(Quat::IDENTITY).to_degrees();
    assert!((90.0..=100.0).contains(&turned), "{turned}");

    // Thumb across: the thumb tip lies against the curled index and middle fingers, under the knuckles.
    let h = hand(&pose, Side::Left);
    let thumb = h.tip(Finger::Thumb);
    let against = distance_to_segment(thumb, h.joint(Finger::Middle, 1), h.joint(Finger::Middle, 2));
    assert!(against < 0.02, "the thumb tip is {against} m from the middle finger");
    assert!(thumb.y < -0.04, "the thumb tip is below the palm: {thumb}");
}

#[test]
fn pointing_puts_the_index_tip_farthest_from_the_palm() {
    let h = hand(&posed("point", Side::Left), Side::Left);
    let from_palm = |p: Vec3| p.distance(h.palm);
    let index_tip = from_palm(h.tip(Finger::Index));
    let (finger, farthest_other) = h
        .landmarks()
        .filter(|(f, _)| *f != Finger::Index)
        .map(|(f, p)| (f, from_palm(p)))
        .fold((Finger::Thumb, 0.0), |a, b| if b.1 > a.1 { b } else { a });
    assert!(index_tip > farthest_other + 0.03, "index tip {index_tip}, but the {finger:?} reaches {farthest_other}");
    assert!(
        h.joints[Finger::Index as usize].iter().all(|j| from_palm(*j) < index_tip),
        "the tip is the end of the finger"
    );
    let reach = h.tip(Finger::Index).distance(h.joint(Finger::Index, 0));
    assert!((reach - index_length()).abs() < 1e-5, "the pointing finger is straight");
    // The other three fingers are the fist's.
    for f in [Finger::Middle, Finger::Ring, Finger::Little] {
        assert_eq!(shape("point").curl(f), shape("fist").curl(f));
    }
}

#[test]
fn peace_is_a_twenty_degree_v_over_curled_fingers() {
    let curls = shape("peace");
    let v = (curls.curl(Finger::Index).spread - curls.curl(Finger::Middle).spread).to_degrees();
    assert!((v - 20.0).abs() < 1.0, "{v}");
    let h = hand(&posed("peace", Side::Left), Side::Left);
    let along = |f: Finger| h.tip(f) - h.joint(f, 0);
    let measured = angle_between(along(Finger::Index), along(Finger::Middle));
    assert!((measured - 20.0).abs() < 1.0, "the fingers are {measured} degrees apart");
    for f in [Finger::Ring, Finger::Little] {
        assert!(is_curled(&h, f), "{f:?} is curled under the hand");
    }
}

#[test]
fn the_finger_heart_closes_a_loop() {
    let h = hand(&posed("heart", Side::Left), Side::Left);
    let gap = h.tip(Finger::Thumb).distance(h.tip(Finger::Index));
    assert!(gap < 0.15 * index_length(), "{gap} m between the tips, a finger is {} m", index_length());
    for f in [Finger::Middle, Finger::Ring, Finger::Little] {
        assert!(is_curled(&h, f), "{f:?} is curled out of the way");
    }
    let right = hand(&posed("heart", Side::Right), Side::Right);
    assert!(right.tip(Finger::Thumb).distance(right.tip(Finger::Index)) < 0.15 * index_length());
}

#[test]
fn the_grip_is_a_fifty_five_percent_fist_with_the_thumb_opposed() {
    let joints: [fn(&FingerCurl) -> f32; 3] = [|c| c.mcp, |c| c.pip, |c| c.dip];
    for f in [Finger::Index, Finger::Middle, Finger::Ring, Finger::Little] {
        for pick in joints {
            let (grip, fist) = (degrees(f, "grip", pick), degrees(f, "fist", pick));
            assert!((grip - 0.55 * fist).abs() <= 0.5, "{f:?}: {grip} is not 55% of {fist}");
        }
    }
    let h = hand(&posed("grip", Side::Left), Side::Left);
    let against = distance_to_segment(h.tip(Finger::Thumb), h.joint(Finger::Index, 1), h.joint(Finger::Index, 2));
    assert!(against < 0.025, "the thumb is {against} m from the index finger it opposes");
    assert!(h.tip(Finger::Thumb).distance(h.tip(Finger::Index)) > 0.02, "a grip leaves room for what is held");
}

#[test]
fn the_relaxed_rest_is_gently_curled_more_toward_the_little_finger() {
    let flex = |f: Finger| degrees(f, "rest", |c| c.mcp);
    assert!(flex(Finger::Index) > 0.0 && flex(Finger::Index) < flex(Finger::Middle));
    assert!(flex(Finger::Middle) < flex(Finger::Ring) && flex(Finger::Ring) < flex(Finger::Little));
    assert!(flex(Finger::Little) < 45.0);
}

#[test]
fn the_thumbs_up_is_a_fist_with_the_thumb_straight_out_to_the_thumb_side() {
    let h = hand(&posed("thumbs-up", Side::Left), Side::Left);
    for f in [Finger::Index, Finger::Middle, Finger::Ring, Finger::Little] {
        assert!(is_curled(&h, f), "{f:?} is in the fist");
        assert_eq!(shape("thumbs-up").curl(f), shape("fist").curl(f));
    }
    let bones = thumb_bones(&h);
    for (i, d) in bones.iter().enumerate() {
        let (azimuth, elevation) = azimuth_elevation(*d);
        assert!((azimuth - 85.0).abs() < 0.5 && elevation.abs() < 0.5, "thumb bone {i}: {azimuth} {elevation}");
    }
    let reach = h.tip(Finger::Thumb) - h.joint(Finger::Thumb, 0);
    assert!(reach.z > 0.9 * reach.length(), "the thumb sticks out toward +Z: {reach}");
    // The thumb is clear of the fist: its tip is farther from the middle finger's knuckle than a finger is long.
    assert!(h.tip(Finger::Thumb).distance(h.joint(Finger::Middle, 0)) > index_length());
}

// ---- the thumb's rest, measured from the rig ----

/// The directions of the three bones of the left thumb in `h`, joint to joint, the tip bone continuing the one before.
fn thumb_bones(h: &Hand) -> [Vec3; 3] {
    let (a, b, c) = (h.joint(Finger::Thumb, 0), h.joint(Finger::Thumb, 1), h.joint(Finger::Thumb, 2));
    let tip = (h.tip(Finger::Thumb) - c).normalize();
    [(b - a).normalize(), (c - b).normalize(), tip]
}

/// A direction's azimuth from the fingers in the palm's plane (toward the thumb side) and its elevation out of that
/// plane (toward the palm, which faces -Y), degrees, for the left hand of the T-pose.
fn azimuth_elevation(d: Vec3) -> (f32, f32) {
    (d.z.atan2(d.x).to_degrees(), (-d.y).asin().to_degrees())
}

/// `shape` on both hands of the rig whose thumb rests as `thumb`, with the rest measured from that rig.
fn posed_on(thumb: &ThumbRest, id: &str) -> HumanoidPose {
    let rest = HandRest::of(&skeleton_with(thumb)).unwrap();
    let mut pose = HumanoidPose::REST;
    apply(&mut pose, Side::Left, &rest, shape(id), 1.0);
    apply(&mut pose, Side::Right, &rest, shape(id), 1.0);
    pose
}

#[test]
fn a_rig_whose_thumb_rests_at_the_nominal_angle_gets_the_librarys_rotations_unchanged() {
    for s in library().iter() {
        for side in [Side::Left, Side::Right] {
            let (mut measured, mut nominal) = (HumanoidPose::REST, HumanoidPose::REST);
            apply(&mut measured, side, &rest(), s, 1.0);
            apply(&mut nominal, side, &HandRest::nominal(), s, 1.0);
            for bone in HumanoidBone::ALL {
                assert!(measured.get(bone).abs_diff_eq(nominal.get(bone), 1e-6), "{} {side:?} {bone}", s.id);
            }
        }
    }
}

#[test]
fn the_thumb_goes_where_the_shape_puts_it_whatever_its_own_rest() {
    // `open` has the thumb 10 degrees past its nominal 45, straight; `thumbs-up` 40. Every bone of every rig's thumb
    // lies at that angle in the palm's plane afterwards, because each is turned from where it rests to there.
    for (name, thumb) in thumbs() {
        for (id, azimuth) in [("open", 55.0), ("thumbs-up", 85.0)] {
            let pose = posed_on(&thumb, id);
            for side in [Side::Left, Side::Right] {
                let h = hand_on(&thumb, &pose, side);
                // The right hand is the left one reflected, so its bones read the same after the reflection.
                let reflect = |d: Vec3| if side == Side::Left { d } else { right_of(d) };
                for (i, d) in thumb_bones(&h).into_iter().enumerate() {
                    let (got, elevation) = azimuth_elevation(reflect(d));
                    assert!(
                        (got - azimuth).abs() < 0.5 && elevation.abs() < 0.5,
                        "{name} {id} {side:?} bone {i}: {got} {elevation}"
                    );
                }
            }
        }
    }
}

#[test]
fn every_shape_looks_the_same_on_every_thumb() {
    // The thumb bones have the same lengths on every rig, so a shape that puts every bone at the same angle puts every
    // joint and the tip at the same place, whatever the rig's thumb rested like.
    for s in library().iter() {
        let reference = hand_on(&nominal_thumb(), &posed_on(&nominal_thumb(), &s.id), Side::Left);
        for (name, thumb) in thumbs() {
            let h = hand_on(&thumb, &posed_on(&thumb, &s.id), Side::Left);
            for finger in Finger::ALL {
                assert_near(
                    h.tip(finger),
                    reference.tip(finger),
                    &format!("{} on the {name} rig, {finger:?} tip", s.id),
                );
                for i in 0..3 {
                    assert_near(
                        h.joint(finger, i),
                        reference.joint(finger, i),
                        &format!("{} on the {name} rig, {finger:?} joint {i}", s.id),
                    );
                }
            }
        }
    }
}

#[test]
fn the_t_pose_thumb_assumed_would_miss_a_thumb_that_rests_elsewhere() {
    // What the measuring is for: the same shape through the nominal rest on a rig whose thumb bends between its bones.
    let (_, bent) = thumbs()[3];
    let mut pose = HumanoidPose::REST;
    apply(&mut pose, Side::Left, &HandRest::nominal(), shape("open"), 1.0);
    let h = hand_on(&bent, &pose, Side::Left);
    let (azimuth, _) = azimuth_elevation(thumb_bones(&h)[0]);
    assert!((azimuth - 55.0).abs() > 10.0, "the nominal rest put the bent thumb at {azimuth} degrees");
}

#[test]
fn every_shape_mirrors_exactly_on_every_thumb() {
    for (name, thumb) in thumbs() {
        for s in library().iter() {
            let rest = HandRest::of(&skeleton_with(&thumb)).unwrap();
            let (mut left, mut right) = (HumanoidPose::REST, HumanoidPose::REST);
            apply(&mut left, Side::Left, &rest, s, 1.0);
            apply(&mut right, Side::Right, &rest, s, 1.0);
            for bone in HumanoidBone::ALL {
                let mirrored = left.mirrored().get(bone);
                assert!(right.get(bone).abs_diff_eq(mirrored, 1e-6), "{} on the {name} rig, {bone}", s.id);
            }
        }
    }
}

#[test]
fn a_rig_without_thumbs_has_nothing_to_measure() {
    let required: Vec<HumanoidBone> = HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect();
    let rig =
        Skeleton::humanoid(&required, |b| Transform::from_translation(rest_offset(b, &[nominal_thumb(); 2]))).unwrap();
    let rest = HandRest::of(&rig).unwrap();
    assert_eq!(rest, HandRest::nominal());
    assert_eq!(rest.warnings(), []);
}

#[test]
fn a_rig_that_is_not_a_humanoid_or_has_no_thumb_direction_is_an_error_not_a_guess() {
    let root = BoneDef { name: "root".into(), parent: None, rest: Transform::IDENTITY, humanoid: None };
    let blob = Skeleton::generic(vec![root]).unwrap();
    assert_eq!(HandRest::of(&blob).unwrap_err(), HandRestError::NotHumanoid);

    // The proximal joint sits on the metacarpal's: no direction.
    let collapsed = skeleton_with(&[Vec3::ZERO, Vec3::X]);
    assert_eq!(HandRest::of(&collapsed).unwrap_err(), HandRestError::Degenerate { side: Side::Left });
    // A thumb pointing straight out of the back of the hand has no azimuth.
    let upright = skeleton_with(&[Vec3::Y, Vec3::Y]);
    assert_eq!(HandRest::of(&upright).unwrap_err(), HandRestError::Degenerate { side: Side::Left });
}

#[test]
fn an_asymmetric_rig_gets_each_hand_its_own_thumb_and_the_shape_on_both() {
    // The left thumb rises out of the palm, the right one bends between its bones. Each hand's thumb must still read the
    // shape's angle, in the left-hand convention, from its own measured rest.
    let sk = rig(&[thumbs()[2].1, thumbs()[3].1], &[], no_turn);
    let rest = HandRest::of(&sk).unwrap();
    assert_eq!(rest.warnings(), []);
    for (id, azimuth) in [("open", 55.0), ("thumbs-up", 85.0)] {
        let mut pose = HumanoidPose::REST;
        apply(&mut pose, Side::Left, &rest, shape(id), 1.0);
        apply(&mut pose, Side::Right, &rest, shape(id), 1.0);
        let world = world_of(&sk, &pose);
        for side in [Side::Left, Side::Right] {
            let j = Finger::Thumb.bones(side).map(|b| world.local[sk.humanoid_bone(b).unwrap().index()].translation);
            // The right hand is the left one reflected, the same as `apply` reflects its rotations.
            let reflect = |d: Vec3| if side == Side::Left { d } else { right_of(d) };
            for (i, d) in [j[1] - j[0], j[2] - j[1]].into_iter().enumerate() {
                let (got, elevation) = azimuth_elevation(reflect(d.normalize()));
                assert!(
                    (got - azimuth).abs() < 0.5 && elevation.abs() < 0.5,
                    "{id} {side:?} thumb bone {i} on the asymmetric rig: {got} {elevation}"
                );
            }
        }
    }
}

#[test]
fn a_thumb_with_only_some_of_its_bones_is_measured_with_the_nominal_one_and_warned() {
    for (omit, side) in [(HumanoidBone::LeftThumbDistal, Side::Left), (HumanoidBone::RightThumbProximal, Side::Right)] {
        let partial = rig(&[nominal_thumb(); 2], &[omit], no_turn);
        let rest = HandRest::of(&partial).unwrap();
        let warnings = rest.warnings();
        assert_eq!(warnings.len(), 1, "{omit}: {warnings:?}");
        assert_eq!((warnings[0].rule, warnings[0].side), ("hands.thumb_partial", side), "{omit}");
        assert!(
            warnings[0].message.contains(&format!("{side:?}")),
            "the message names the hand: {}",
            warnings[0].message
        );
        // The partial hand gets the nominal thumb, so a shape poses it exactly as on the nominal rig.
        for s in library().iter() {
            let (mut partial_pose, mut nominal_pose) = (HumanoidPose::REST, HumanoidPose::REST);
            apply(&mut partial_pose, side, &rest, s, 1.0);
            apply(&mut nominal_pose, side, &HandRest::nominal(), s, 1.0);
            for bone in HumanoidBone::ALL {
                assert!(partial_pose.get(bone).abs_diff_eq(nominal_pose.get(bone), 1e-6), "{} {omit} {bone}", s.id);
            }
        }
    }
    assert_eq!(HandRest::of(&skeleton()).unwrap().warnings(), []);
}

#[test]
fn a_degenerate_right_thumb_is_an_error_naming_the_right_hand() {
    let collapsed = rig(&[nominal_thumb(), [Vec3::ZERO, Vec3::X]], &[], no_turn);
    assert_eq!(HandRest::of(&collapsed).unwrap_err(), HandRestError::Degenerate { side: Side::Right });
}

/// Turns `bone` about Z by `degrees` (toward -Y, so the arm hangs) when `bone` is one of `which`.
fn turn_when(which: impl Fn(HumanoidBone) -> bool, degrees: f32) -> impl Fn(HumanoidBone) -> Quat {
    let turn = Quat::from_rotation_z(degrees.to_radians());
    move |b| if which(b) { turn } else { Quat::IDENTITY }
}

/// Asserts that `rig` is refused with `NotTPose` on `bone` at `degrees`, and that the message names both.
fn assert_not_t(rig: &Skeleton, bone: HumanoidBone, degrees: f32) {
    match HandRest::of(rig) {
        Err(HandRestError::NotTPose { bone: got, degrees: measured }) => {
            assert_eq!(got, bone, "{got} measured at {measured}");
            assert!((measured - degrees).abs() < 0.5, "measured {measured}, expected {degrees}");
            let message = HandRestError::NotTPose { bone: got, degrees: measured }.to_string();
            assert!(message.contains(&bone.to_string()), "{message}");
            assert!(message.contains(&format!("{degrees:.1}")), "{message}");
        }
        other => panic!("expected NotTPose, got {other:?}"),
    }
}

#[test]
fn a_t_rig_whose_fingers_rest_curled_at_the_knuckles_passes() {
    // Every finger's first bone leans 20 degrees off the T-pose axis. The forearm and palm are straight, so this is a
    // T-pose by the check's definition, and the imported avatars that rest this way must still measure.
    let curl = turn_when(|b| is_finger(b) && (b.name().contains("Intermediate") || b.name().contains("Distal")), 20.0);
    let rig = rig(&[nominal_thumb(); 2], &[], curl);
    let rest = HandRest::of(&rig).expect("a curled T-pose is still a T-pose");
    assert_eq!(rest.warnings(), []);
}

#[test]
fn a_rig_resting_in_a_not_t_is_refused_naming_the_bone_and_the_measured_angle() {
    // Forearms and hands hang 55 degrees below the T-pose's x axis, as an A-pose's arms do. The forearm is measured
    // first, from the lower arm.
    let hang = turn_when(|b| matches!(b, HumanoidBone::LeftHand | HumanoidBone::RightHand) || is_finger(b), -55.0);
    let a_pose = rig(&[nominal_thumb(); 2], &[], hang);
    assert_not_t(&a_pose, HumanoidBone::LeftLowerArm, 55.0);
}

#[test]
fn a_t_rig_whose_lower_arm_leans_15_degrees_is_refused() {
    let lean = turn_when(|b| matches!(b, HumanoidBone::LeftHand | HumanoidBone::RightHand), -15.0);
    let leaning = rig(&[nominal_thumb(); 2], &[], lean);
    assert_not_t(&leaning, HumanoidBone::LeftLowerArm, 15.0);
}

// ---- blend and the tracking entry point ----

#[test]
fn blend_is_exact_at_the_ends_and_the_mean_between() {
    let (open, fist) = (shape("open"), shape("fist"));
    assert_eq!(open.blend(fist, 0.0).curls, open.curls);
    assert_eq!(open.blend(fist, 1.0).curls, fist.curls);
    assert_eq!(open.blend(fist, -3.0).curls, open.curls);
    assert_eq!(open.blend(fist, 7.0).curls, fist.curls);
    assert_eq!(open.blend(fist, f32::NAN).curls, open.curls);
    let half = open.blend(fist, 0.5);
    for f in Finger::ALL {
        let (a, b, m) = (open.curl(f), fist.curl(f), half.curl(f));
        assert!((m.mcp - f32::midpoint(a.mcp, b.mcp)).abs() < 1e-6);
        assert!((m.spread - f32::midpoint(a.spread, b.spread)).abs() < 1e-6);
    }
    half.validate().unwrap();
}

#[test]
fn from_curls_runs_from_open_to_fist_per_finger() {
    let (open, fist) = (shape("open"), shape("fist"));
    assert_eq!(HandShape::from_curls([0.0; 5]).unwrap().curls, open.curls);
    assert_eq!(HandShape::from_curls([1.0; 5]).unwrap().curls, fist.curls);
    assert_eq!(
        HandShape::from_curls([2.0, -1.0, f32::NAN, 1.0, 0.0]).unwrap().curls[0],
        fist.curls[0],
        "held to 0..=1"
    );
    let mixed = HandShape::from_curls([0.0, 1.0, 0.0, 0.5, 0.0]).unwrap();
    assert_eq!(mixed.curl(Finger::Thumb), open.curl(Finger::Thumb));
    assert_eq!(mixed.curl(Finger::Index), fist.curl(Finger::Index));
    assert_eq!(mixed.curl(Finger::Middle), open.curl(Finger::Middle));
    assert!(mixed.curl(Finger::Ring).pip > 0.0 && mixed.curl(Finger::Ring).pip < fist.curl(Finger::Ring).pip);
    mixed.validate().unwrap();
    // A tracked hand curls monotonically: more curl, the fingertip closer to the palm.
    let reach = |c: f32| {
        let mut pose = HumanoidPose::REST;
        apply(&mut pose, Side::Left, &rest(), &HandShape::from_curls([c; 5]).unwrap(), 1.0);
        let h = hand(&pose, Side::Left);
        h.tip(Finger::Middle).distance(h.palm)
    };
    let reaches: Vec<_> = [0.0, 0.25, 0.5, 0.75, 1.0].map(reach).to_vec();
    assert!(reaches.windows(2).all(|w| w[1] < w[0]), "{reaches:?}");
}

#[test]
fn applying_and_tracking_allocate_nothing() {
    let (fist, open) = (shape("fist").clone(), shape("open").clone());
    let mut pose = HumanoidPose::REST;
    let rig = rest();
    let warm = HandShape::from_curls([0.5; 5]).unwrap(); // the first call parses the library
    apply(&mut pose, Side::Left, &rig, &warm, 0.5);

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for i in 0..200 {
        let c = (i % 10) as f32 / 10.0;
        let tracked = HandShape::from_curls([c, 1.0 - c, c, 0.5, 0.2]).unwrap();
        apply(&mut pose, Side::Left, &rig, &tracked, 0.8);
        apply(&mut pose, Side::Right, &rig, &fist.blend(&open, c), 1.0);
    }
    MEASURING.with(|m| m.set(false));
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed) - before, 0);
}

// ---- loading ----

fn file(id: &str, middle: &str) -> String {
    let finger = "{ spread = 0.0, mcp = 0.0, pip = 0.0, dip = 0.0 }";
    format!(
        "id = \"{id}\"\nsummary = \"test\"\nthumb = {finger}\nindex = {finger}\nmiddle = {middle}\nring = {finger}\nlittle = {finger}\n"
    )
}

fn parse(files: &[String]) -> Result<HandLibrary, HandShapeError> {
    let sources: Vec<(&str, &str)> = files.iter().map(|f| ("test", f.as_str())).collect();
    HandLibrary::parse(&sources)
}

#[test]
fn the_loader_reads_degrees_and_refuses_what_cannot_be_a_hand() {
    let ok = "{ spread = 5, mcp = 90, pip = 0.0, dip = 0.0 }";
    let library = parse(&[file("a", ok), file("b", ok)]).unwrap();
    let c = library.get("a").unwrap().curl(Finger::Middle);
    assert!((c.mcp - std::f32::consts::FRAC_PI_2).abs() < 1e-6 && (c.spread - 5f32.to_radians()).abs() < 1e-6);

    let angle_error = |value: &str| {
        parse(&[file("a", &format!("{{ spread = 0.0, mcp = {value}, pip = 0.0, dip = 0.0 }}"))]).unwrap_err()
    };
    assert!(matches!(angle_error("200.0"), HandShapeError::Angle { finger: "middle", joint: "mcp", .. }));
    assert!(matches!(angle_error("-60.0"), HandShapeError::Angle { .. }));
    assert!(matches!(angle_error("nan"), HandShapeError::Angle { .. }));
    assert!(matches!(angle_error("inf"), HandShapeError::Angle { .. }));
    assert!(matches!(
        parse(&[file("a", "{ spread = 120.0, mcp = 0.0, pip = 0.0, dip = 0.0 }")]).unwrap_err(),
        HandShapeError::Angle { joint: "spread", .. }
    ));

    assert_eq!(parse(&[file("a", ok), file("a", ok)]).unwrap_err(), HandShapeError::DuplicateId("a".into()));
    assert!(matches!(parse(&[file("Not A Name", ok)]).unwrap_err(), HandShapeError::BadId { .. }));
    assert!(matches!(
        parse(&[file("a", ok).replace("summary = \"test\"", "summary = \"  \"")]).unwrap_err(),
        HandShapeError::EmptySummary { .. }
    ));
    // A misspelt key, a missing finger and broken TOML are all refused, not defaulted.
    assert!(matches!(parse(&[file("a", ok).replace("mcp", "mpc")]).unwrap_err(), HandShapeError::Parse { .. }));
    assert!(matches!(
        parse(&[file("a", ok).replace("ring = ", "ringg = ")]).unwrap_err(),
        HandShapeError::Parse { .. }
    ));
    assert!(matches!(parse(&["id = ".to_owned()]).unwrap_err(), HandShapeError::Parse { .. }));
}

// ---- the op ----

#[test]
fn hand_list_shapes_lists_the_library() {
    let reg = find("hand.list_shapes").expect("the op registers itself");
    let meta = (reg.meta)();
    assert_eq!((meta.kind, meta.scope), (OpKind::Query, Scope::Read));

    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let actor = Actor::Agent { id: "test".into(), model: None };
    let plan = (reg.decode)(json!({})).unwrap().plan(&OpCtx::new(&doc, &actor, &clock)).unwrap();
    assert!(plan.patch.is_empty(), "a query changes nothing");

    let out = plan.output.unwrap();
    let names: Vec<_> = out["shapes"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["open", "fist", "point", "peace", "heart", "grip", "thumbs-up", "rest", "spread", "flat"]);
    let fist = &out["shapes"][1];
    assert!(fist["summary"].as_str().unwrap().contains("fist"));
    assert_eq!(fist["curls_deg"]["index"]["mcp"], 80.0);
    assert_eq!(fist["curls_deg"]["thumb"]["spread"], -56.0);
    assert_eq!(out["shapes"][3]["curls_deg"]["index"]["spread"], 10.0);
}
