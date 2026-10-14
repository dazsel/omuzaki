//! The pose library (RG-06): format, blending, mirroring and layering, held to the acceptance.

// The counting allocator is the only way to prove "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::hands::{Finger, Side};
use omuzaki_anim::hands::{HandLibrary, HandRest};
use omuzaki_anim::poses::{Hand, PoseDef, PoseError, PoseLibrary, PoseMask, Posture};
use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, OpKind, Scope, find};
use serde_json::json;

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

fn pose(id: &str) -> PoseDef {
    PoseLibrary::builtin().unwrap().get(id).unwrap().clone()
}

fn shape(id: &str) -> &'static omuzaki_anim::hands::HandShape {
    HandLibrary::builtin().unwrap().get(id).unwrap()
}

fn posture(id: &str) -> Posture {
    pose(id).posture(HandLibrary::builtin().unwrap()).unwrap()
}

/// The largest difference between any two rotation components or hip coordinates.
fn distance(a: &HumanoidPose, b: &HumanoidPose) -> f32 {
    let rotations = a.rotations.iter().zip(&b.rotations);
    let q = rotations.flat_map(|(x, y)| x.to_array().into_iter().zip(y.to_array()));
    let h = a.hips_offset.to_array().into_iter().zip(b.hips_offset.to_array());
    q.chain(h).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// A posture that turns many bones, the hips and both hands, so the identities have something to bite on.
fn busy() -> Posture {
    let mut p = posture("wave-right");
    p.body.hips_offset = Vec3::new(0.02, -0.05, 0.01);
    p.body.set(HumanoidBone::Spine, Quat::from_euler(omuzaki_core::glam::EulerRot::XYZ, 0.1, 0.2, -0.3));
    p.left = Hand::of(shape("fist"), 0.5);
    p
}

#[test]
fn the_built_in_poses_load_and_are_valid() {
    let lib = PoseLibrary::builtin().unwrap();
    let ids: Vec<_> = lib.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["wave-right", "wave-left"]);
    for p in lib.iter() {
        p.validate().unwrap();
        p.posture(HandLibrary::builtin().unwrap()).unwrap();
    }
}

#[test]
fn to_humanoid_poses_the_bones_and_the_hand() {
    let h = pose("wave-right").to_humanoid().unwrap();
    assert!(h.get(HumanoidBone::RightLowerArm).angle_between(Quat::IDENTITY) > 1.0);
    assert_eq!(h.get(HumanoidBone::LeftShoulder), Quat::IDENTITY, "unlisted bones stay at the T-pose");
    // The open hand is applied to the right hand and not to the left.
    assert_ne!(h.get(HumanoidBone::RightIndexProximal), Quat::IDENTITY);
    assert_eq!(h.get(HumanoidBone::LeftIndexProximal), Quat::IDENTITY);
}

#[test]
fn mirroring_twice_gives_back_the_same_pose() {
    let def = pose("wave-right");
    assert_eq!(def.mirrored().unwrap().mirrored().unwrap(), def);
    let p = busy();
    assert_eq!(p.mirrored().mirrored(), p);
}

#[test]
fn the_mirror_of_a_right_hand_pose_is_its_authored_left_version() {
    let mirrored = pose("wave-right").mirrored().unwrap().to_humanoid().unwrap();
    let authored = pose("wave-left").to_humanoid().unwrap();
    assert!(distance(&mirrored, &authored) <= 1e-6, "{}", distance(&mirrored, &authored));
    // The same through the posture, which is what the animator mirrors.
    let through = posture("wave-right").mirrored().to_humanoid(&HandRest::nominal());
    assert!(distance(&through, &authored) <= 1e-6);
}

#[test]
fn a_pose_that_is_not_mirrorable_refuses() {
    let mut def = pose("wave-right");
    def.mirrorable = false;
    assert_eq!(def.mirrored().unwrap_err(), PoseError::NotMirrorable("wave-right".into()));
}

#[test]
fn blend_endpoints_are_exact() {
    let (a, b) = (busy(), posture("wave-left"));
    assert_eq!(Posture::blend(&a, &b, 0.0), a);
    assert_eq!(Posture::blend(&a, &b, 1.0), b);
    assert_eq!(Posture::blend(&a, &b, -3.0), a, "weights are held to 0..=1");
    assert_eq!(Posture::blend(&a, &b, f32::NAN), a);
    assert_eq!(Posture::blend(&a, &b, 7.0), b);
}

#[test]
fn blend_halfway_is_the_midpoint_of_bones_hips_and_hands() {
    let (a, b) = (busy(), Posture { body: HumanoidPose::REST, left: Hand::NONE, right: Hand::NONE });
    let mid = Posture::blend(&a, &b, 0.5);
    assert_eq!(mid.body.hips_offset, a.body.hips_offset * 0.5);
    let want = Quat::IDENTITY.slerp(a.body.get(HumanoidBone::RightLowerArm), 0.5);
    assert!(mid.body.get(HumanoidBone::RightLowerArm).angle_between(want) < 1e-6);
    // A hand one pose sets and the other does not fades.
    assert_eq!(mid.left.bones[1][0].weight, 0.25);
    assert_eq!(mid.left.bones[1][0].curl, a.left.bones[1][0].curl);
}

fn only(left: &Hand) -> Posture {
    Posture { body: HumanoidPose::REST, left: *left, right: Hand::NONE }
}

#[test]
fn blending_two_hand_shapes_mixes_their_angles() {
    let (a, b) = (only(&Hand::of(shape("open"), 1.0)), only(&Hand::of(shape("fist"), 1.0)));
    let mid = Posture::blend(&a, &b, 0.5).left;
    let want = shape("open").blend(shape("fist"), 0.5);
    for f in Finger::ALL {
        for bone in mid.bones[f as usize] {
            assert_eq!(bone.curl, want.curl(f));
            assert_eq!(bone.weight, 1.0);
        }
    }
}

#[test]
fn a_zero_weight_shape_has_no_say_and_equals_an_absent_one() {
    let open = only(&Hand::of(shape("open"), 1.0));
    let ghost = only(&Hand::of(shape("fist"), 0.0));
    let none = only(&Hand::NONE);
    let rest = HandRest::nominal();
    for w in [0.25, 0.5, 0.9] {
        let (a, b) = (Posture::blend(&ghost, &open, w), Posture::blend(&none, &open, w));
        let key = |hand: &Hand| hand.bones.map(|finger| finger.map(|bone| (bone.curl, bone.weight)));
        assert_eq!(key(&a.left), key(&b.left));
        assert_eq!(a.to_humanoid(&rest), b.to_humanoid(&rest));
        // And from the other side.
        let (c, d) = (Posture::blend(&open, &ghost, w), Posture::blend(&open, &none, w));
        assert_eq!(c.to_humanoid(&rest), d.to_humanoid(&rest));
    }
    // The fist at weight 0 must not bend the index at the halfway point: only the open hand's half weight applies.
    let half = Posture::blend(&ghost, &open, 0.5).to_humanoid(&rest);
    let straight = Posture::blend(&none, &open, 0.5).to_humanoid(&rest);
    assert_eq!(half.get(HumanoidBone::LeftIndexIntermediate), straight.get(HumanoidBone::LeftIndexIntermediate));
}

#[test]
fn unequal_weights_blend_by_effective_influence() {
    let (a, b) = (only(&Hand::of(shape("fist"), 0.25)), only(&Hand::of(shape("open"), 0.75)));
    let mid = Posture::blend(&a, &b, 0.5).left.bones[1][0];
    // Influences 0.125 and 0.375: the mean sits three quarters of the way to open, at weight 0.5.
    assert!((mid.weight - 0.5).abs() < 1e-7);
    let (fist, open) = (shape("fist").curl(Finger::Index), shape("open").curl(Finger::Index));
    let want = fist.mcp + (open.mcp - fist.mcp) * 0.75;
    assert!((mid.curl.mcp - want).abs() < 1e-6);
}

#[test]
fn a_mask_touches_only_its_bones() {
    let (base, overlay) = (posture("wave-left"), busy());
    let mask = PoseMask::subtree(HumanoidBone::RightShoulder);
    let out = Posture::layer(&base, &overlay, mask, 1.0);
    for bone in HumanoidBone::ALL {
        let from = if mask.contains(bone) { &overlay } else { &base };
        assert_eq!(out.body.get(bone), from.body.get(bone), "{bone}");
    }
    assert_eq!(out.body.hips_offset, base.body.hips_offset, "hips are outside the mask");
    assert_eq!(out.left, base.left, "the left hand is outside the mask");
    assert_eq!(out.right, overlay.right, "the right hand is inside it");
}

/// Base and overlay with a different hand on each, so a leak shows in the resolved bones.
fn hands_pair() -> (Posture, Posture) {
    let base =
        Posture { body: HumanoidPose::REST, left: Hand::of(shape("fist"), 1.0), right: Hand::of(shape("fist"), 1.0) };
    let overlay =
        Posture { body: HumanoidPose::REST, left: Hand::of(shape("open"), 1.0), right: Hand::of(shape("peace"), 1.0) };
    (base, overlay)
}

#[test]
fn a_wrist_only_mask_keeps_the_base_fingers_after_resolution() {
    let (base, overlay) = hands_pair();
    let rest = HandRest::nominal();
    let mask = PoseMask::of([HumanoidBone::RightHand]);
    let out = Posture::layer(&base, &overlay, mask, 1.0).to_humanoid(&rest);
    let (b, o) = (base.to_humanoid(&rest), overlay.to_humanoid(&rest));
    for bone in HumanoidBone::ALL {
        let from = if bone == HumanoidBone::RightHand { &o } else { &b };
        assert_eq!(out.get(bone), from.get(bone), "{bone}");
    }
}

#[test]
fn a_finger_only_mask_takes_that_finger_and_nothing_else_after_resolution() {
    let (base, overlay) = hands_pair();
    let rest = HandRest::nominal();
    let mask = PoseMask::of(Finger::Index.bones(Side::Right));
    let out = Posture::layer(&base, &overlay, mask, 1.0).to_humanoid(&rest);
    let (b, o) = (base.to_humanoid(&rest), overlay.to_humanoid(&rest));
    let index = Finger::Index.bones(Side::Right);
    for bone in HumanoidBone::ALL {
        let from = if index.contains(&bone) { &o } else { &b };
        assert_eq!(out.get(bone), from.get(bone), "{bone}");
    }
    // A mask of the other fingers' bones, not the index, leaves the index to the base.
    let others =
        PoseMask::of(Finger::ALL.into_iter().filter(|f| *f != Finger::Index).flat_map(|f| f.bones(Side::Right)));
    let out = Posture::layer(&base, &overlay, others, 1.0).to_humanoid(&rest);
    for bone in index {
        assert_eq!(out.get(bone), b.get(bone), "{bone}");
    }
}

#[test]
fn a_single_joint_mask_takes_that_joint_after_resolution() {
    // Each hand bone on its own: base fist, overlay open, on both hands. The masked joint must read as the overlay, and
    // every other hand bone must still read as the base, once the hand shapes are applied.
    let base =
        Posture { body: HumanoidPose::REST, left: Hand::of(shape("fist"), 1.0), right: Hand::of(shape("fist"), 1.0) };
    let overlay =
        Posture { body: HumanoidPose::REST, left: Hand::of(shape("open"), 1.0), right: Hand::of(shape("open"), 1.0) };
    let rest = HandRest::nominal();
    let (b, o) = (base.to_humanoid(&rest), overlay.to_humanoid(&rest));
    for finger in Finger::ALL {
        for joint in finger.bones(Side::Right) {
            let out = Posture::layer(&base, &overlay, PoseMask::of([joint]), 1.0).to_humanoid(&rest);
            assert_ne!(b.get(joint), o.get(joint), "{joint}: the test needs base and overlay to differ");
            assert_eq!(out.get(joint), o.get(joint), "{joint} takes the overlay");
            for bone in HumanoidBone::ALL.into_iter().filter(|bone| *bone != joint) {
                assert_eq!(out.get(bone), b.get(bone), "{bone} is outside the mask when {joint} is masked");
            }
        }
    }
}

#[test]
fn layer_weight_zero_is_the_base_and_half_is_between() {
    let (base, overlay) = (posture("wave-left"), busy());
    let mask = PoseMask::ALL;
    assert_eq!(Posture::layer(&base, &overlay, mask, 0.0), base);
    assert_eq!(Posture::layer(&base, &overlay, mask, 1.0), overlay);
    assert_eq!(Posture::layer(&base, &overlay, mask, 0.5), Posture::blend(&base, &overlay, 0.5));
    let hips = PoseMask::of([HumanoidBone::Hips]);
    let out = Posture::layer(&base, &overlay, hips, 0.5);
    assert_eq!(out.body.hips_offset, overlay.body.hips_offset * 0.5);
    assert_eq!(out.body.rotations, base.body.rotations);
}

#[test]
fn masks_know_their_subtrees_and_their_mirror() {
    let right_arm = PoseMask::subtree(HumanoidBone::RightShoulder);
    assert!(right_arm.contains(HumanoidBone::RightHand) && right_arm.contains(HumanoidBone::RightLittleDistal));
    assert!(!right_arm.contains(HumanoidBone::LeftHand) && !right_arm.contains(HumanoidBone::Chest));
    assert_eq!(right_arm.mirrored(), PoseMask::subtree(HumanoidBone::LeftShoulder));
    assert_eq!(PoseMask::subtree(HumanoidBone::Hips), PoseMask::ALL);
    assert_eq!(PoseMask::NONE.union(PoseMask::NONE), PoseMask::NONE);
}

#[test]
fn everything_is_deterministic() {
    let run = || {
        let m = Posture::layer(&busy(), &posture("wave-left"), PoseMask::subtree(HumanoidBone::Spine), 0.37);
        Posture::blend(&m, &m.mirrored(), 0.61).to_humanoid(&HandRest::nominal()).checksum()
    };
    assert_eq!(run(), run());
}

fn parse(text: &str) -> Result<PoseLibrary, PoseError> {
    PoseLibrary::parse(&[("test", text)])
}

#[test]
fn bad_files_are_rejected_with_a_reason() {
    let ok = "id = \"p\"\nsummary = \"s\"\n";
    assert_eq!(parse(ok).unwrap().iter().len(), 1);
    assert!(matches!(parse("id = ").unwrap_err(), PoseError::Parse { .. }));
    assert!(matches!(parse(&format!("{ok}[bones]\nnoSuchBone = [0,0,0]")).unwrap_err(), PoseError::Parse { .. }));
    assert!(matches!(parse(&format!("{ok}extra = 1")).unwrap_err(), PoseError::Parse { .. }));
    assert!(matches!(parse("id = \"Bad Id\"\nsummary = \"s\"").unwrap_err(), PoseError::BadName { what: "id", .. }));
    let tagged = format!("{ok}tags = [\"No Good\"]");
    assert!(matches!(parse(&tagged).unwrap_err(), PoseError::BadName { what: "tag", .. }));
    assert_eq!(parse("id = \"p\"\nsummary = \" \"").unwrap_err(), PoseError::EmptySummary("p".into()));
    let nan = format!("{ok}hips_offset = [nan, 0, 0]");
    assert!(matches!(parse(&nan).unwrap_err(), PoseError::BadNumber { .. }));
    let heavy = format!("{ok}[hands.left]\nshape = \"fist\"\nweight = 1.5");
    assert!(matches!(parse(&heavy).unwrap_err(), PoseError::BadNumber { .. }));
    let twice = PoseLibrary::parse(&[("a", ok), ("b", ok)]).unwrap_err();
    assert_eq!(twice, PoseError::DuplicateId("p".into()));
}

#[test]
fn an_unknown_hand_shape_is_reported_when_resolving() {
    let lib = parse("id = \"p\"\nsummary = \"s\"\n[hands.left]\nshape = \"jazz\"").unwrap();
    let err = lib.get("p").unwrap().to_humanoid().unwrap_err();
    assert_eq!(err, PoseError::UnknownHandShape { id: "p".into(), shape: "jazz".into() });
}

#[test]
fn pose_list_lists_the_library() {
    let reg = find("pose.list").expect("the op registers itself");
    let meta = (reg.meta)();
    assert_eq!((meta.kind, meta.scope), (OpKind::Query, Scope::Read));
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let actor = Actor::Agent { id: "test".into(), model: None };
    let plan = (reg.decode)(json!({})).unwrap().plan(&OpCtx::new(&doc, &actor, &clock)).unwrap();
    assert!(plan.patch.is_empty(), "a query changes nothing");
    let out = plan.output.unwrap();
    let ids: Vec<_> = out["poses"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["wave-right", "wave-left"]);
    assert_eq!(out["poses"][0]["tags"], json!(["greeting", "arm"]));
    assert!(out["poses"][0]["summary"].as_str().unwrap().contains("Right arm"));
}

#[test]
fn blend_mirror_layer_and_resolution_never_allocate() {
    let (a, b) = (busy(), posture("wave-left"));
    let rest = HandRest::nominal();
    let mask = PoseMask::subtree(HumanoidBone::RightShoulder);
    MEASURING.with(|m| m.set(true));
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let mut sum = 0_u64;
    for i in 0..100_u8 {
        let w = f32::from(i) / 100.0;
        let blended = Posture::blend(&a, &b, w);
        let layered = Posture::layer(&blended, &blended.mirrored(), mask, w);
        sum = sum.wrapping_add(layered.to_humanoid(&rest).checksum());
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(false));
    assert_eq!(after - before, 0, "the pose path allocated (checksum {sum})");
}
