//! Capsule-segment auto-weights and the weight utilities, on synthetic data with answers worked out by hand or stated as
//! properties. Parity with the prototype is in `crates/omuzaki-test/tests/char_auto_weights_parity.rs`.
//!
//! The test body is a T-pose with all 55 humanoid joints (30 of them finger bones), so the five-finger hand is exercised
//! like any other limb. Two deformation cases pose real skinned meshes: a bent elbow and a fist. With
//! `OMUZAKI_EVIDENCE_DIR` set, `deformation_evidence` also writes their rest and posed vertices for the renders in the PR.

#![allow(clippy::float_cmp)] // exact values that are exact by construction (0, 1, a copied weight)
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // small loop counters and f32 inputs
#![allow(clippy::many_single_char_names)] // x, y, z, p, q
#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a failure should stop the test with its reason

use std::collections::BTreeMap;
use std::f32::consts::FRAC_PI_2;

use omuzaki_anim_api::HumanoidBone as H;
use omuzaki_char::kit::KitError;
use omuzaki_char::kit::skin::{
    CapsuleParams, FOOT_BALL_RATIO, HAND_REACH, HEAD_REACH, MAX_INFLUENCES, MirrorReport, Segment, SegmentSet, Side,
    Symmetry, TIP_RATIO, TOES_REACH, blend_between, brush_from_stroke, brush_mirrored, finger_chains, limit_influences,
    max_neighbour_jump, mirror, normalise, smooth,
};
use omuzaki_char_api::apply_brushes;
use omuzaki_char_api::{BoneName, BrushFalloff, JointFit, JointRef, MaterialName, MeshData, PartMesh, WeightBrush};
use omuzaki_core::glam::{DVec3, Mat4, Quat, Vec3};
use proptest::prelude::*;

fn v(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

fn role(bone: H) -> JointRef {
    JointRef::Humanoid(bone)
}

fn named(name: &str) -> JointRef {
    JointRef::Named(BoneName::new(name).unwrap())
}

// ------------------------------------------------------------------------------------------------------ the test body

/// The finger layout of one hand: z offset of each chain's knuckle (thumb, index, middle, ring, little) and a length scale.
const FINGERS: [(f32, f32); 5] = [(0.04, 1.0), (0.027, 0.95), (0.009, 1.0), (-0.009, 0.95), (-0.027, 0.8)];

/// A T-pose body, metres, +Y up, facing +Z, the character's left on +X. Fingers lie along the arm with 18 mm between
/// them. `fingers` false leaves the 30 finger bones out, for a mitten-style template.
fn body(fingers: bool) -> BTreeMap<H, Vec3> {
    let mut m = BTreeMap::new();
    m.extend([
        (H::Hips, v(0.0, 0.90, 0.0)),
        (H::Spine, v(0.0, 0.98, 0.0)),
        (H::Chest, v(0.0, 1.10, 0.0)),
        (H::UpperChest, v(0.0, 1.25, 0.0)),
        (H::Neck, v(0.0, 1.45, 0.0)),
        (H::Head, v(0.0, 1.50, 0.0)),
        (H::Jaw, v(0.0, 1.55, 0.05)),
        (H::LeftEye, v(0.03, 1.62, 0.08)),
        (H::RightEye, v(-0.03, 1.62, 0.08)),
    ]);
    for left in [true, false] {
        let s = if left { 1.0 } else { -1.0 };
        let (shoulder, upper, lower, hand) = if left {
            (H::LeftShoulder, H::LeftUpperArm, H::LeftLowerArm, H::LeftHand)
        } else {
            (H::RightShoulder, H::RightUpperArm, H::RightLowerArm, H::RightHand)
        };
        let (up_leg, low_leg, foot, toes) = if left {
            (H::LeftUpperLeg, H::LeftLowerLeg, H::LeftFoot, H::LeftToes)
        } else {
            (H::RightUpperLeg, H::RightLowerLeg, H::RightFoot, H::RightToes)
        };
        m.extend([
            (shoulder, v(0.05 * s, 1.40, 0.0)),
            (upper, v(0.18 * s, 1.40, 0.0)),
            (lower, v(0.45 * s, 1.40, 0.0)),
            (hand, v(0.70 * s, 1.40, 0.0)),
            (up_leg, v(0.09 * s, 0.85, 0.0)),
            (low_leg, v(0.09 * s, 0.45, 0.01)),
            (foot, v(0.09 * s, 0.08, 0.0)),
            (toes, v(0.09 * s, 0.03, 0.12)),
        ]);
        if fingers {
            for (chain, (z, scale)) in finger_chains(hand).iter().zip(FINGERS) {
                // Fingers run along the arm; the thumb starts nearer the wrist and angles forward.
                let (root, lengths) = if chain[0] == H::LeftThumbMetacarpal || chain[0] == H::RightThumbMetacarpal {
                    (0.02, [0.03, 0.025, 0.02])
                } else {
                    (0.085, [0.04, 0.025, 0.02])
                };
                let mut at = v(0.70 * s + root * s, 1.40, z);
                for (bone, length) in chain.iter().zip(lengths) {
                    m.insert(*bone, at);
                    at += v(length * scale * s, 0.0, 0.0);
                }
            }
        }
    }
    m
}

fn fit(fingers: bool) -> JointFit {
    JointFit::new(body(fingers).into_iter().map(|(bone, at)| (role(bone), at)))
}

fn all_roles() -> Vec<JointRef> {
    H::ALL.into_iter().map(role).collect()
}

fn segments(fingers: bool) -> SegmentSet {
    SegmentSet::from_fit(&fit(fingers), &all_roles()).unwrap()
}

fn d(p: Vec3) -> DVec3 {
    p.as_dvec3()
}

// ------------------------------------------------------------------------------------------------------------ segments

#[test]
fn a_segments_distance_and_parameter_are_worked_out_by_hand() {
    let s = Segment { joint: JointRef::HIPS, head: DVec3::ZERO, tail: DVec3::Y };
    assert_eq!(s.distance(DVec3::new(1.0, 0.5, 0.0)), (1.0, 0.5), "beside the middle");
    assert_eq!(s.distance(DVec3::new(0.0, 2.0, 0.0)), (1.0, 1.0), "past the tail: the end cap");
    assert_eq!(s.distance(DVec3::new(0.0, -1.0, 0.0)), (1.0, 0.0), "before the head");
    let (dist, t) = s.distance(DVec3::new(3.0, 5.0, 0.0));
    assert!((dist - 5.0).abs() < 1e-12 && t == 1.0, "{dist} {t}");
    let point = Segment { joint: JointRef::HIPS, head: DVec3::X, tail: DVec3::X };
    assert_eq!(point.distance(DVec3::new(1.0, 2.0, 0.0)), (2.0, 0.0), "a zero-length axis is a point, not NaN");
}

#[test]
fn capsule_weights_are_the_prototypes_inverse_power() {
    let set = SegmentSet::default()
        .with_segment(Segment { joint: JointRef::HIPS, head: DVec3::ZERO, tail: DVec3::Y })
        .with_segment(Segment { joint: JointRef::CHEST, head: DVec3::Y, tail: DVec3::new(0.0, 2.0, 0.0) });
    let p = DVec3::new(0.1, 1.2, 0.0);
    let raw = set.capsule_weights(p, &[JointRef::HIPS, JointRef::CHEST, JointRef::HEAD], CapsuleParams::default());
    assert_eq!(raw.len(), 2, "a bone without an axis is skipped");
    let (to_hips, to_chest): (f64, f64) = ((0.1_f64.powi(2) + 0.2_f64.powi(2)).sqrt(), 0.1);
    assert!((raw[0].1 - 1.0 / (to_hips + 0.012).powi(4)).abs() < 1e-6 * raw[0].1);
    assert!((raw[1].1 - 1.0 / (to_chest + 0.012).powi(4)).abs() < 1e-6 * raw[1].1);
    let soft = CapsuleParams::new(2.5, 0.05).unwrap();
    let raw = set.capsule_weights(p, &[JointRef::CHEST], soft);
    assert!((raw[0].1 - 1.0 / (to_chest + 0.05_f64).powf(2.5)).abs() < 1e-9 * raw[0].1, "a fractional exponent");
    assert_eq!(CapsuleParams::default(), CapsuleParams::PROTOTYPE);
    assert!(matches!(CapsuleParams::new(f64::NAN, 0.01), Err(KitError::NonFinite { .. })));
    assert!(matches!(CapsuleParams::new(4.0, 0.0), Err(KitError::Degenerate { .. })));
    assert!(matches!(CapsuleParams::new(-1.0, 0.01), Err(KitError::Degenerate { .. })));
}

#[test]
fn bones_hand_over_at_the_joint_and_the_nearest_wins() {
    let set = segments(true);
    let bones = [role(H::LeftUpperArm), role(H::LeftLowerArm)];
    let blend = |x: f32| set.weights_at(d(v(x, 1.43, 0.0)), &bones, CapsuleParams::default());
    let upper = |w: &[(JointRef, f32)]| w.iter().find(|(j, _)| *j == bones[0]).map_or(0.0, |(_, w)| *w);
    assert!(upper(&blend(0.25)) > 0.99, "a hand's width from the elbow it is all upper arm");
    assert!(upper(&blend(0.65)) < 0.01, "and all forearm at the other end");
    assert!((upper(&blend(0.45)) - 0.5).abs() < 1e-3, "at the elbow it is half and half");
    assert!(upper(&blend(0.40)) > upper(&blend(0.45)) && upper(&blend(0.45)) > upper(&blend(0.50)), "monotone across");
}

// ----------------------------------------------------------------------------------------------------------- from_fit

#[test]
fn all_55_humanoid_bones_get_an_axis_and_the_chains_end_in_each_other() {
    let set = segments(true);
    assert_eq!(set.segments().len(), 55, "every VRM humanoid bone, 30 of them finger bones");
    for hand in [H::LeftHand, H::RightHand] {
        let chains = finger_chains(hand);
        assert_eq!(chains.len(), 5);
        assert_eq!(
            chains.iter().map(Vec::len).collect::<Vec<_>>(),
            [3, 3, 3, 3, 3],
            "thumb and four fingers of three bones"
        );
        for chain in &chains {
            for pair in chain.windows(2) {
                let (a, b) = (set.get(&role(pair[0])).unwrap(), set.get(&role(pair[1])).unwrap());
                assert_eq!(a.tail, b.head, "{:?} ends where {:?} starts", pair[0], pair[1]);
            }
            let (previous, last) = (set.get(&role(chain[1])).unwrap(), set.get(&role(chain[2])).unwrap());
            let along = (previous.tail - previous.head) * TIP_RATIO;
            assert!(
                (last.tail - last.head - along).length() < 1e-6,
                "{:?}: a fingertip continues the finger",
                chain[2]
            );
        }
    }
    assert_eq!(finger_chains(H::Head), Vec::<Vec<H>>::new(), "only hands have fingers");
    let mut bones: Vec<H> = [H::LeftHand, H::RightHand].into_iter().flat_map(finger_chains).flatten().collect();
    bones.sort();
    bones.dedup();
    assert_eq!(bones.len(), 30);
}

#[test]
fn the_hand_ends_at_the_knuckle_or_without_fingers_reaches_along_the_forearm() {
    let with = segments(true);
    let hand = with.get(&role(H::LeftHand)).unwrap();
    assert_eq!(hand.tail, d(body(true)[&H::LeftMiddleProximal]), "the palm ends at the middle finger's knuckle");
    // Without a middle finger the palm ends at the index finger's knuckle; without any, at the prototype's 7 cm.
    let mut joints = body(true);
    joints.remove(&H::LeftMiddleProximal);
    let set = SegmentSet::from_fit(&JointFit::new(joints.into_iter().map(|(b, p)| (role(b), p))), &[role(H::LeftHand)])
        .unwrap();
    assert_eq!(set.get(&role(H::LeftHand)).unwrap().tail, d(body(true)[&H::LeftIndexProximal]));
    let bare = segments(false);
    let hand = bare.get(&role(H::LeftHand)).unwrap();
    assert!((hand.tail - hand.head - DVec3::X * HAND_REACH).length() < 1e-6, "{hand:?}: 7 cm along the (+X) forearm");
    let right = bare.get(&role(H::RightHand)).unwrap();
    assert!((right.tail - right.head + DVec3::X * HAND_REACH).length() < 1e-6, "and mirrored on the right");
    assert_eq!(bare.segments().len(), 25, "the fingerless body has 55 - 30 bones");
}

#[test]
fn toes_head_and_a_missing_chest_follow_the_prototypes_terminal_rules() {
    let set = segments(false).with_crown(DVec3::new(0.0, 1.9, -0.02));
    let toes = set.get(&role(H::LeftToes)).unwrap();
    assert!((toes.tail - toes.head - DVec3::Z * TOES_REACH).length() < 1e-9, "4 cm forward");
    assert_eq!(set.get(&JointRef::HEAD).unwrap().tail, DVec3::new(0.0, 1.9, -0.02), "the crown the head part knows");
    let uncrowned = segments(false);
    let head = uncrowned.get(&JointRef::HEAD).unwrap();
    assert!((head.tail - head.head - DVec3::Y * HEAD_REACH).length() < 1e-6, "else a head's height straight up");
    // A template without upperChest: chest runs to the neck. Without chest too: spine runs to the neck.
    let mut joints = body(false);
    joints.remove(&H::UpperChest);
    let narrow =
        SegmentSet::from_fit(&JointFit::new(joints.clone().into_iter().map(|(b, p)| (role(b), p))), &all_roles())
            .unwrap();
    assert_eq!(narrow.get(&JointRef::CHEST).unwrap().tail, d(joints[&H::Neck]));
    joints.remove(&H::Chest);
    let narrower =
        SegmentSet::from_fit(&JointFit::new(joints.clone().into_iter().map(|(b, p)| (role(b), p))), &all_roles())
            .unwrap();
    assert_eq!(narrower.get(&role(H::Spine)).unwrap().tail, d(joints[&H::Neck]));
    assert!(narrower.get(&JointRef::CHEST).is_none(), "and a bone the fit lacks has no axis");
    // The hips run to the spine, the upper arm to the elbow.
    assert_eq!(set.get(&JointRef::HIPS).unwrap().tail, d(body(false)[&H::Spine]));
    assert_eq!(set.get(&role(H::LeftUpperArm)).unwrap().tail, d(body(false)[&H::LeftLowerArm]));
}

#[test]
fn a_foot_without_toes_runs_forward_and_no_axis_goes_under_the_floor() {
    // The bug: with no toe joint the foot followed its shin down, 0.8 shin length below the sole.
    let mut joints = body(false);
    joints.remove(&H::LeftToes);
    joints.remove(&H::RightToes);
    let set = SegmentSet::from_fit(&JointFit::new(joints.clone().into_iter().map(|(b, p)| (role(b), p))), &all_roles())
        .unwrap();
    for (foot, shin) in [(H::LeftFoot, H::LeftLowerLeg), (H::RightFoot, H::RightLowerLeg)] {
        let axis = set.get(&role(foot)).unwrap();
        let shin_length = (d(joints[&shin]) - d(joints[&foot])).length();
        assert!(((axis.tail - axis.head).length() - FOOT_BALL_RATIO * shin_length).abs() < 1e-9, "the tip length");
        assert!(
            (axis.tail - axis.head - DVec3::Z * FOOT_BALL_RATIO * shin_length).length() < 1e-9,
            "{foot:?}: forward, {FOOT_BALL_RATIO} of the shin: {axis:?}"
        );
    }
    for segment in set.segments() {
        assert!(
            segment.head.y >= 0.0 && segment.tail.y >= 0.0,
            "{:?} runs under the floor: {:?}",
            segment.joint,
            segment
        );
    }
}

#[test]
fn from_fit_skips_extra_bones_and_refuses_a_joint_that_is_not_finite() {
    let set = SegmentSet::from_fit(&fit(false), &[named("hair_front_0"), JointRef::HIPS]).unwrap();
    assert_eq!(set.segments().len(), 1, "an extra bone has no place in the humanoid tree");
    let bad = JointFit::new([(JointRef::HIPS, v(f32::NAN, 0.0, 0.0))]);
    assert!(matches!(SegmentSet::from_fit(&bad, &[JointRef::HIPS]), Err(KitError::NonFinite { .. })));
    let added = set.with_segment(Segment { joint: named("hair_front_0"), head: DVec3::ZERO, tail: DVec3::Y });
    assert_eq!(added.segments().len(), 2);
    let replaced = added.with_segment(Segment { joint: named("hair_front_0"), head: DVec3::X, tail: DVec3::Y });
    assert_eq!(replaced.segments().len(), 2, "one axis per bone");
    assert_eq!(replaced.get(&named("hair_front_0")).unwrap().head, DVec3::X);
    assert_eq!(
        SegmentSet::default().with_crown(DVec3::Y),
        SegmentSet::default(),
        "no head, nothing to end at the crown"
    );
}

// ------------------------------------------------------------------------------------------------------ top four, blends

fn weights_of(blend: &[(JointRef, f32)]) -> Vec<f32> {
    blend.iter().map(|(_, w)| *w).collect()
}

#[test]
fn the_top_four_keeps_the_heaviest_in_order_merges_repeats_and_drops_the_unusable() {
    let (a, b, c, e, f) = (JointRef::HIPS, JointRef::CHEST, JointRef::NECK, JointRef::HEAD, role(H::Jaw));
    let mut blend = vec![
        (a.clone(), 0.2),
        (b.clone(), 0.5),
        (c.clone(), 0.0),
        (e.clone(), f32::NAN),
        (f.clone(), 0.1),
        (a.clone(), 0.2),
        (b.clone(), -1.0),
    ];
    limit_influences(&mut blend, 4);
    assert_eq!(blend, [(b.clone(), 0.5), (a.clone(), 0.4), (f.clone(), 0.1)], "heaviest first; hips twice added up");
    normalise(&mut blend);
    assert!((weights_of(&blend).iter().sum::<f32>() - 1.0).abs() < 1e-6);

    let mut many: Vec<_> = (0..6).map(|i| (named(&format!("b{i}")), 1.0)).collect();
    limit_influences(&mut many, MAX_INFLUENCES);
    assert_eq!(
        many.iter().map(|(j, _)| j.clone()).collect::<Vec<_>>(),
        (0..4).map(|i| named(&format!("b{i}"))).collect::<Vec<_>>(),
        "ties keep the given order"
    );

    let mut none = vec![(b, 0.0), (c, f32::INFINITY)];
    limit_influences(&mut none, 4);
    normalise(&mut none);
    assert_eq!(none, [(JointRef::HIPS, 1.0)], "nothing usable falls back to the hips, as the prototype does");
    let mut empty = Vec::new();
    normalise(&mut empty);
    assert_eq!(empty, [(JointRef::HIPS, 1.0)]);
}

#[test]
fn a_joint_blend_is_a_smoothstep_between_two_joints_either_way_round() {
    let down = blend_between(JointRef::HIPS, role(H::LeftUpperLeg), 0.425, 0.335);
    assert_eq!(down.amount(0.5), 0.0);
    assert_eq!(down.amount(0.3), 1.0);
    assert!((down.amount(0.38) - 0.5).abs() < 1e-12);
    let [(hips, w_hips), (leg, w_leg)] = down.at(0.38);
    assert_eq!((hips, leg), (JointRef::HIPS, role(H::LeftUpperLeg)));
    assert!((w_hips + w_leg - 1.0).abs() < 1e-6 && (w_hips - 0.5).abs() < 1e-6);
    let up = blend_between(JointRef::CHEST, JointRef::NECK, 0.53, 0.568);
    assert!(up.amount(0.5) == 0.0 && up.amount(0.6) == 1.0);
    let mut at_end = up.at(0.5).to_vec();
    limit_influences(&mut at_end, 4);
    assert_eq!(at_end, [(JointRef::CHEST, 1.0)], "a zero share drops out");
}

// ------------------------------------------------------------------------------------------------------------- meshes

/// A skinned mesh of loose points (one triangle, which only `validate` needs) with the given weights.
fn cloud(positions: Vec<Vec3>, joint_bones: Vec<JointRef>, skin: Vec<([u16; 4], [f32; 4])>) -> PartMesh {
    let n = positions.len();
    let (joints, weights): (Vec<_>, Vec<_>) = skin.into_iter().unzip();
    PartMesh {
        name: "m".to_owned(),
        geometry: MeshData {
            positions,
            normals: vec![Vec3::Y; n],
            joints,
            weights,
            indices: vec![0, 1.min(n as u32 - 1), 2.min(n as u32 - 1)],
            ..MeshData::default()
        },
        joint_bones,
        material: MaterialName::new("m").unwrap(),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    }
}

/// A vertex's skin as sorted `(joint, weight)` pairs, zero weights left out.
fn skin_of(mesh: &PartMesh, vertex: usize) -> Vec<(JointRef, f32)> {
    let mut out: Vec<(JointRef, f32)> = mesh.geometry.joints[vertex]
        .iter()
        .zip(mesh.geometry.weights[vertex])
        .filter(|(_, w)| *w > 0.0)
        .map(|(j, w)| (mesh.joint_bones[usize::from(*j)].clone(), w))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn assert_valid_skin(mesh: &PartMesh) {
    mesh.validate().unwrap();
    for (vertex, (joints, weights)) in mesh.geometry.joints.iter().zip(&mesh.geometry.weights).enumerate() {
        let sum: f32 = weights.iter().sum();
        assert!((sum - 1.0).abs() <= 1e-6, "vertex {vertex}: weights {weights:?} sum to {sum}");
        assert!(weights.iter().filter(|w| **w > 0.0).count() <= MAX_INFLUENCES);
        assert!(joints.iter().zip(weights).all(|(j, w)| *w == 0.0 || usize::from(*j) < mesh.joint_bones.len()));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Whatever the points and candidate bones: weights sum to 1 within 1e-6 over at most four influences, and skinning
    /// a whole mesh gives each vertex what `weights_at` gives that point.
    #[test]
    fn auto_weights_are_normalised_with_at_most_four_influences(
        points in proptest::collection::vec((-0.6_f32..0.6, -0.1_f32..1.9, -0.3_f32..0.3), 1..40),
        picked in proptest::collection::vec(0_usize..55, 0..30),
        power in 1.0_f64..8.0,
        softening in 0.002_f64..0.05,
    ) {
        let set = segments(true);
        let bones: Vec<JointRef> = picked.iter().map(|i| role(H::ALL[*i])).collect();
        let params = CapsuleParams::new(power, softening).unwrap();
        let positions: Vec<Vec3> = points.iter().map(|(x, y, z)| v(*x, *y, *z)).collect();
        let n = positions.len();
        let mut mesh = cloud(positions.clone(), Vec::new(), vec![([0; 4], [1.0, 0.0, 0.0, 0.0]); n]);
        set.skin_mesh(&mut mesh, &bones, params).unwrap();
        assert_valid_skin(&mesh);
        for (vertex, p) in positions.iter().enumerate() {
            let single = set.weights_at(d(*p), &bones, params);
            prop_assert!(single.len() <= MAX_INFLUENCES);
            prop_assert!((weights_of(&single).iter().sum::<f32>() - 1.0).abs() <= 1e-6);
            let mut single_sorted = single;
            single_sorted.sort_by(|a, b| a.0.cmp(&b.0));
            let bulk = skin_of(&mesh, vertex);
            prop_assert_eq!(bulk.len(), single_sorted.len(), "vertex {}", vertex);
            for ((bj, bw), (sj, sw)) in bulk.iter().zip(&single_sorted) {
                prop_assert_eq!(bj, sj);
                prop_assert!((bw - sw).abs() <= 1e-6, "vertex {}: {} vs {}", vertex, bw, sw);
            }
        }
    }
}

/// Regression for the flaky proptest above: five candidates for four slots whose pulls differ in f64 but tie in f32.
/// `weights_at` and `skin_mesh` must keep the same four bones and weights.
#[test]
fn the_single_and_bulk_paths_keep_the_same_bones_when_pulls_tie_in_f32() {
    let set = segments(true);
    let picked = [21, 22, 9, 10, 11];
    let bones: Vec<JointRef> = picked.iter().map(|i| role(H::ALL[*i])).collect();
    let params = CapsuleParams::new(1.0, 0.002).unwrap();
    let p = v(-0.180_067, 0.0, 0.0);
    let mut mesh = cloud(vec![p], Vec::new(), vec![([0; 4], [1.0, 0.0, 0.0, 0.0])]);
    set.skin_mesh(&mut mesh, &bones, params).unwrap();
    assert_valid_skin(&mesh);
    let mut single = set.weights_at(d(p), &bones, params);
    single.sort_by(|a, b| a.0.cmp(&b.0));
    let bulk = skin_of(&mesh, 0);
    assert_eq!(bulk.len(), single.len(), "{bulk:?} vs {single:?}");
    for ((bj, bw), (sj, sw)) in bulk.iter().zip(&single) {
        assert_eq!(bj, sj);
        assert!((bw - sw).abs() <= 1e-6, "{bw} vs {sw}");
    }
}

#[test]
fn a_mesh_with_nothing_to_pull_it_goes_on_the_hips_and_skin_mesh_lists_only_the_joints_it_uses() {
    let set = segments(false);
    let mut mesh =
        cloud(vec![Vec3::ZERO, Vec3::X, Vec3::Y], vec![JointRef::CHEST], vec![([0; 4], [1.0, 0.0, 0.0, 0.0]); 3]);
    set.skin_mesh(&mut mesh, &[named("not_in_the_set")], CapsuleParams::default()).unwrap();
    assert_eq!(mesh.joint_bones, [JointRef::HIPS]);
    assert!(mesh.geometry.weights.iter().all(|w| *w == [1.0, 0.0, 0.0, 0.0]));
    // Six candidates, four slots: the right arm's bones are too far to make the top four of either point, so the mesh's
    // joint list (the joints used, in first-use order) leaves them out.
    let mut mesh = cloud(vec![v(0.7, 1.4, 0.0), v(0.2, 1.4, 0.0)], Vec::new(), vec![([0; 4], [1.0, 0.0, 0.0, 0.0]); 2]);
    let candidates = [
        role(H::LeftHand),
        role(H::LeftLowerArm),
        role(H::LeftUpperArm),
        role(H::LeftShoulder),
        role(H::RightHand),
        role(H::RightLowerArm),
    ];
    set.skin_mesh(&mut mesh, &candidates, CapsuleParams::default()).unwrap();
    let heaviest = skin_of(&mesh, 0).into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap().0;
    assert_eq!(heaviest, role(H::LeftHand));
    assert!(!mesh.joint_bones.contains(&role(H::RightHand)), "{:?}", mesh.joint_bones);
    assert!(!mesh.joint_bones.contains(&role(H::RightLowerArm)), "{:?}", mesh.joint_bones);
    assert_valid_skin(&mesh);
}

// ----------------------------------------------------------------------------------------------------------- mirroring

/// A mesh symmetric in x: `half` points on +X, their mirror images, and `centre` points on the plane. Left vertices get
/// `skins[i]`; the rest start on the hips so a mirror has something to overwrite.
fn symmetric(half: &[Vec3], centre: &[Vec3], skins: &[([u16; 4], [f32; 4])], joints: &[JointRef]) -> PartMesh {
    let n = half.len();
    let mut positions: Vec<Vec3> = half.to_vec();
    positions.extend(half.iter().map(|p| v(-p.x, p.y, p.z)));
    positions.extend(centre);
    let mut skin: Vec<_> = skins[..n].to_vec();
    skin.extend(std::iter::repeat_n(([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]), n + centre.len()));
    cloud(positions, joints.to_vec(), skin)
}

fn swapped(skin: &[(JointRef, f32)]) -> Vec<(JointRef, f32)> {
    let mut out: Vec<_> = skin.iter().map(|(j, w)| (omuzaki_char::kit::grid::mirror_joint(j), *w)).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn same_skin(a: &[(JointRef, f32)], b: &[(JointRef, f32)]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|((ja, wa), (jb, wb))| ja == jb && (wa - wb).abs() <= 1e-6)
}

const ARM_JOINTS: [H; 4] = [H::LeftUpperArm, H::LeftLowerArm, H::RightUpperArm, H::RightLowerArm];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Mirrored weights of an x-symmetric mesh equal the source's, joints swapped, within 1e-6; on-plane vertices come out
    /// symmetric; the source half is untouched.
    #[test]
    fn mirroring_an_x_symmetric_mesh_gives_equal_weights_with_sides_swapped(
        half in proptest::collection::vec((0.01_f32..0.5, -0.5_f32..1.5, -0.3_f32..0.3), 1..25),
        centre in proptest::collection::vec((-0.5_f32..1.5, -0.3_f32..0.3), 0..5),
        raw in proptest::collection::vec(proptest::collection::vec(0.01_f32..1.0, 4), 25),
        from_right in any::<bool>(),
    ) {
        let joints: Vec<JointRef> = ARM_JOINTS.into_iter().map(role).collect();
        let half: Vec<Vec3> = half.iter().map(|(x, y, z)| v(*x, *y, *z)).collect();
        let centre: Vec<Vec3> = centre.iter().map(|(y, z)| v(0.0, *y, *z)).collect();
        // The source half's skins: weights over the first two (left) or last two (right) joints.
        let (own, other) = if from_right { ([2_u16, 3, 0, 1], Side::Right) } else { ([0_u16, 1, 2, 3], Side::Left) };
        let skins: Vec<_> = raw.iter().map(|w| {
            let total: f32 = w.iter().sum();
            (own, [w[0] / total, w[1] / total, w[2] / total, w[3] / total])
        }).collect();
        // Put the source on +X for Left, on -X for Right.
        let sign = if from_right { -1.0 } else { 1.0 };
        let half: Vec<Vec3> = half.iter().map(|p| v(p.x * sign, p.y, p.z)).collect();
        let mut mesh = symmetric(&half, &centre, &skins, &joints);
        let before = mesh.clone();
        let report = mirror(&mut mesh, Symmetry::from_side(other)).unwrap();
        prop_assert_eq!(report.unmatched, 0);
        prop_assert_eq!(report.mirrored, half.len());
        prop_assert_eq!(report.centred, centre.len());
        assert_valid_skin(&mesh);
        for i in 0..half.len() {
            prop_assert_eq!(skin_of(&mesh, i), skin_of(&before, i), "the source is untouched");
            let (mine, theirs) = (skin_of(&mesh, half.len() + i), swapped(&skin_of(&mesh, i)));
            prop_assert!(same_skin(&mine, &theirs), "vertex {}: {:?} vs {:?}", i, mine, theirs);
        }
        for c in 0..centre.len() {
            let s = skin_of(&mesh, 2 * half.len() + c);
            prop_assert!(same_skin(&s, &swapped(&s)), "on-plane vertex {}: {:?}", c, s);
        }
    }
}

#[test]
fn mirroring_adds_the_opposite_joints_and_reports_vertices_without_a_partner() {
    // Left vertices skinned to the left arm only; the mesh does not list the right arm yet.
    let half = [v(0.2, 1.4, 0.0), v(0.3, 1.4, 0.0)];
    let skins = [([0, 1, 0, 0], [0.75, 0.25, 0.0, 0.0]); 2];
    let mut mesh = symmetric(&half, &[], &skins, &[role(H::LeftUpperArm), role(H::LeftLowerArm)]);
    mesh.geometry.positions[3].y += 0.01; // one right vertex is off its partner's place
    let report = mirror(&mut mesh, Symmetry::from_side(Side::Left)).unwrap();
    assert_eq!((report.mirrored, report.unmatched), (1, 1));
    assert_eq!(mesh.joint_bones.len(), 4, "the right arm's joints were added: {:?}", mesh.joint_bones);
    assert_eq!(skin_of(&mesh, 2), vec![(role(H::RightUpperArm), 0.75), (role(H::RightLowerArm), 0.25)]);
    assert_eq!(skin_of(&mesh, 3), vec![(role(H::LeftUpperArm), 1.0)], "no partner: left as it was");
    assert!(matches!(
        mirror(&mut mesh, Symmetry { source: Side::Left, tolerance: 0.0 }),
        Err(KitError::Degenerate { .. })
    ));
    assert!(matches!(
        mirror(&mut mesh, Symmetry { source: Side::Left, tolerance: f32::NAN }),
        Err(KitError::NonFinite { .. })
    ));
    let mut rigid = cloud(vec![Vec3::ZERO; 3], Vec::new(), Vec::new());
    assert_eq!(mirror(&mut rigid, Symmetry::from_side(Side::Left)).unwrap(), MirrorReport::default());
}

#[test]
fn extra_bones_mirror_by_their_l_and_r_name_segments() {
    let half = [v(0.1, 1.6, 0.0)];
    let mut mesh = symmetric(&half, &[], &[([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0])], &[named("hair_l_2")]);
    mirror(&mut mesh, Symmetry::from_side(Side::Left)).unwrap();
    assert_eq!(skin_of(&mesh, 1), vec![(named("hair_r_2"), 1.0)]);
}

#[test]
fn all_fifteen_finger_bones_of_a_hand_mirror_to_the_same_finger_on_the_other_hand() {
    // One vertex per left finger bone, fully skinned to that bone: its mirror must be fully skinned to the matching right
    // finger bone. The chains pair up in order, so chain k of the left hand mirrors chain k of the right.
    let left: Vec<H> = finger_chains(H::LeftHand).into_iter().flatten().collect();
    let right: Vec<H> = finger_chains(H::RightHand).into_iter().flatten().collect();
    assert_eq!((left.len(), right.len()), (15, 15));
    let half: Vec<Vec3> = (0..15).map(|i| v(0.7 + 0.01 * i as f32, 1.4, 0.001 * i as f32)).collect();
    let joints: Vec<JointRef> = left.iter().copied().map(role).collect();
    let skins: Vec<_> = (0..15_u16).map(|i| ([i, 0, 0, 0], [1.0, 0.0, 0.0, 0.0])).collect();
    let mut mesh = symmetric(&half, &[], &skins, &joints);
    let report = mirror(&mut mesh, Symmetry::from_side(Side::Left)).unwrap();
    assert_eq!((report.mirrored, report.unmatched), (15, 0));
    for (i, bone) in right.iter().enumerate() {
        assert_eq!(
            skin_of(&mesh, 15 + i),
            vec![(role(*bone), 1.0)],
            "finger bone {i}: {:?} mirrors to {bone:?}",
            left[i]
        );
    }
}

// ----------------------------------------------------------------------------------------------------------- smoothing

/// A `w` x `h` grid in the XY plane (vertex `y * w + x`), two triangles a quad, skinned by `skin(x, y)`.
fn grid_mesh(w: usize, h: usize, joints: &[JointRef], skin: impl Fn(usize, usize) -> ([u16; 4], [f32; 4])) -> PartMesh {
    let mut positions = Vec::new();
    let mut skins = Vec::new();
    for y in 0..h {
        for x in 0..w {
            positions.push(v(x as f32 * 0.01, y as f32 * 0.01, 0.0));
            skins.push(skin(x, y));
        }
    }
    let mut mesh = cloud(positions, joints.to_vec(), skins);
    mesh.geometry.indices.clear();
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            let i = (y * w + x) as u32;
            let (right, up) = (i + 1, i + w as u32);
            mesh.geometry.indices.extend([i, right, up, right, up + 1, up]);
        }
    }
    mesh
}

#[test]
fn smoothing_softens_a_hard_step_and_keeps_the_skin_valid() {
    let joints = [JointRef::CHEST, JointRef::NECK];
    let mut mesh = grid_mesh(12, 4, &[joints[0].clone(), joints[1].clone()], |x, _| {
        if x < 6 { ([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]) } else { ([1, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]) }
    });
    assert_eq!(max_neighbour_jump(&mesh), 1.0);
    smooth(&mut mesh, 0);
    assert_eq!(max_neighbour_jump(&mesh), 1.0, "no passes, no change");
    let mut previous = 1.0;
    for _ in 0..4 {
        smooth(&mut mesh, 1);
        let now = max_neighbour_jump(&mesh);
        assert!(now < previous, "each pass over a hard step softens it: {previous} then {now}");
        previous = now;
        assert_valid_skin(&mesh);
    }
    assert!(previous < 0.5, "after four passes the step is spread over several vertices: {previous}");
    let mid = skin_of(&mesh, 5);
    assert!(mid.iter().any(|(j, w)| *j == JointRef::NECK && *w > 0.0), "weight crossed the step: {mid:?}");
}

#[test]
fn smoothing_treats_a_seam_as_one_vertex_and_leaves_rigid_meshes_alone() {
    // A ring of four columns whose last column repeats the first (a lathe's seam): the duplicates are 0 and 4.
    let joints = [JointRef::CHEST, JointRef::NECK];
    let mut mesh = grid_mesh(5, 3, &joints, |x, y| {
        let t = if (x % 4 == 0) ^ (y == 1) { 1.0 } else { 0.0 };
        ([0, 1, 0, 0], [1.0 - t, t, 0.0, 0.0])
    });
    for y in 0..3 {
        mesh.geometry.positions[y * 5 + 4] = mesh.geometry.positions[y * 5]; // the seam: same place, own index
    }
    smooth(&mut mesh, 3);
    for y in 0..3 {
        assert_eq!(skin_of(&mesh, y * 5), skin_of(&mesh, y * 5 + 4), "row {y}: the seam stays closed");
    }
    let mut rigid = cloud(vec![Vec3::ZERO; 3], Vec::new(), Vec::new());
    let before = rigid.clone();
    smooth(&mut rigid, 5);
    assert_eq!(rigid, before);
    assert_eq!(max_neighbour_jump(&rigid), 0.0);
}

fn irregular(
    points: &[(f32, f32, f32)],
    triangles: &[(usize, usize, usize)],
    weights: &[Vec<f32>],
    order: &[Vec<u16>],
) -> PartMesh {
    let joints: Vec<JointRef> =
        [H::Hips, H::Chest, H::Neck, H::Head, H::LeftUpperArm, H::RightUpperArm].into_iter().map(role).collect();
    let n = points.len();
    let skins: Vec<_> = (0..n)
        .map(|i| {
            let total: f32 = weights[i].iter().sum::<f32>() + 0.01;
            let mut w = [0.0; 4];
            for k in 0..4 {
                w[k] = weights[i][k] / total;
            }
            w[0] += 0.01 / total; // never all zero
            ([order[i][0], order[i][1], order[i][2], order[i][3]], w)
        })
        .collect();
    let mut mesh = cloud(points.iter().map(|(x, y, z)| v(*x, *y, *z)).collect(), joints, skins);
    mesh.geometry.indices = triangles
        .iter()
        .filter(|(a, b, c)| a % n != b % n && b % n != c % n && a % n != c % n)
        .flat_map(|(a, b, c)| [(a % n) as u32, (b % n) as u32, (c % n) as u32])
        .collect();
    mesh
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// On any mesh, however irregular its triangles, smoothing never raises the biggest jump across an edge, and the skin
    /// stays normalised over at most four influences.
    #[test]
    fn smoothing_never_increases_the_maximum_neighbour_jump(
        points in proptest::collection::vec((0.0_f32..1.0, 0.0_f32..1.0, 0.0_f32..1.0), 6..40),
        triangles in proptest::collection::vec((0_usize..400, 0_usize..400, 0_usize..400), 4..60),
        weights in proptest::collection::vec(proptest::collection::vec(0.0_f32..1.0, 4), 40),
        order in proptest::collection::vec(Just((0_u16..6).collect::<Vec<_>>()).prop_shuffle(), 40),
        iterations in 1_u32..6,
    ) {
        let mut mesh = irregular(&points, &triangles, &weights, &order);
        prop_assume!(!mesh.geometry.indices.is_empty());
        let before = max_neighbour_jump(&mesh);
        smooth(&mut mesh, iterations);
        let after = max_neighbour_jump(&mesh);
        prop_assert!(after <= before, "{} passes: {} became {}", iterations, before, after);
        for (joints, weights) in mesh.geometry.joints.iter().zip(&mesh.geometry.weights) {
            let sum: f32 = weights.iter().sum();
            prop_assert!((sum - 1.0).abs() <= 1e-6, "{:?}", weights);
            prop_assert!(weights.iter().filter(|w| **w > 0.0).count() <= MAX_INFLUENCES);
            prop_assert!(joints.iter().zip(weights).all(|(j, w)| *w == 0.0 || usize::from(*j) < 6));
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Hub-and-spoke meshes, the shape of the two-hub test below: two joined hubs, each held by its own leaves. The shares
    /// are close together, as there, so the mesh's biggest jump is small and an averaging step that overshoots it is
    /// possible. Random shares far apart would leave the guard nothing to catch. The guard must hold for every such shape.
    #[test]
    fn smoothing_never_steepens_hubs_held_by_their_leaves(
        hub in 0.2_f32..0.8,
        hub_gap in -0.1_f32..0.1,
        offsets_a in proptest::collection::vec(-0.1_f32..0.1, 1..12),
        offsets_b in proptest::collection::vec(-0.1_f32..0.1, 1..12),
        iterations in 1_u32..6,
    ) {
        let (share_a, share_b) = (hub, hub + hub_gap);
        let leaves_a = offsets_a.iter().map(|o| (share_a + o).clamp(0.0, 1.0));
        let leaves_b = offsets_b.iter().map(|o| (share_b + o).clamp(0.0, 1.0));
        let shares: Vec<f32> = [share_a, share_b].into_iter().chain(leaves_a).chain(leaves_b).collect();
        let n = shares.len();
        let mut mesh = grid_mesh(n, 1, &[JointRef::CHEST, JointRef::NECK], |x, _| {
            ([0, 1, 0, 0], [1.0 - shares[x], shares[x], 0.0, 0.0])
        });
        // Vertex 0 and 1 are the hubs, joined; then hub 0's leaves, then hub 1's. Edges only, as in the two-hub test.
        let first_b = 2 + offsets_a.len();
        let edges = (2..first_b).map(|k| (0, k)).chain((first_b..n).map(|k| (1, k))).chain([(0, 1)]);
        mesh.geometry.indices = edges.flat_map(|(hub, other)| [hub, other, other]).map(|k| k as u32).collect();
        let before = max_neighbour_jump(&mesh);
        for pass in 1..=iterations {
            smooth(&mut mesh, 1);
            prop_assert!(max_neighbour_jump(&mesh) <= before, "pass {}: {} became {}", pass, before, max_neighbour_jump(&mesh));
        }
        assert_valid_skin(&mesh);
    }
}

#[test]
fn smoothing_does_not_steepen_two_hubs_each_held_by_its_leaves() {
    // Hub 0 (share 0.5 on the neck) has nine leaves at 0.6; hub 1 (0.4) has nine at 0.3; the hubs are joined. Every edge
    // jumps 0.1. Plain averaging moves hub 0 to 0.54 and hub 1 to 0.36, so the hubs' edge jumps 0.18, steeper than
    // anything before: each hub's leaves hold it back from following the other.
    let share = |x: usize| match x {
        0 => 0.5,
        1 => 0.4,
        2..=10 => 0.6,
        _ => 0.3,
    };
    let mut mesh = grid_mesh(20, 1, &[JointRef::CHEST, JointRef::NECK], |x, _| {
        ([0, 1, 0, 0], [1.0 - share(x), share(x), 0.0, 0.0])
    });
    let leaves = (2..11).map(|k| (0, k)).chain((11..20).map(|k| (1, k)));
    mesh.geometry.indices = leaves.chain([(0, 1)]).flat_map(|(hub, other)| [hub, other, other]).collect(); // edges only
    let before = max_neighbour_jump(&mesh);
    assert!((before - 0.1).abs() < 1e-6, "{before}");
    for pass in 1..=6 {
        smooth(&mut mesh, 1);
        assert!(max_neighbour_jump(&mesh) <= before, "pass {pass}: {} became {}", before, max_neighbour_jump(&mesh));
    }
    assert_valid_skin(&mesh);
    let hub_edge = (skin_of(&mesh, 0)[1].1 - skin_of(&mesh, 1)[1].1).abs();
    assert!(hub_edge < before, "the hubs still moved toward each other: {hub_edge}");
}

// ------------------------------------------------------------------------------------------------------------ brushes

fn brush(joint: JointRef, centre: [f64; 3]) -> WeightBrush {
    WeightBrush { centre, radius: 0.1, joint, strength: 0.7, falloff: BrushFalloff::Linear }
}

#[test]
fn a_mirrored_brush_flips_x_and_the_side_and_nothing_else() {
    let b = brush(role(H::LeftUpperArm), [0.25, 1.4, 0.02]);
    let m = brush_mirrored(&b);
    assert_eq!(m.centre, [-0.25, 1.4, 0.02]);
    assert_eq!(m.joint, role(H::RightUpperArm));
    assert_eq!((m.radius, m.strength, m.falloff), (b.radius, b.strength, b.falloff));
    assert_eq!(brush_mirrored(&m), b, "mirroring twice is the original");
    let own = brush(JointRef::CHEST, [0.0, 1.2, 0.0]);
    assert_eq!(brush_mirrored(&own), own, "a centre brush on a centre bone is its own mirror, and x stays +0");
    assert!(brush_mirrored(&own).centre[0].is_sign_positive());
    assert_eq!(brush_mirrored(&brush(named("ear_l_1"), [0.1, 1.6, 0.0])).joint, named("ear_r_1"));
}

#[test]
fn a_stroke_becomes_a_brush_covering_it_and_full_strength_at_the_middle_takes_the_joint() {
    let stroke = [[0.20, 1.40, 0.0], [0.30, 1.40, 0.0], [0.25, 1.46, 0.0], [0.25, 1.34, 0.0]];
    let b = brush_from_stroke(&stroke, role(H::LeftLowerArm), 1.0).unwrap();
    assert!(
        b.centre.iter().zip([0.25, 1.40, 0.0]).all(|(c, e)| (c - e).abs() < 1e-12),
        "the middle of the stroke's box: {:?}",
        b.centre
    );
    assert!((b.radius - 0.06).abs() < 1e-12, "reaching its farthest point: {}", b.radius);
    assert_eq!((b.joint.clone(), b.strength, b.falloff), (role(H::LeftLowerArm), 1.0, BrushFalloff::Smooth));

    // A vertex at the middle gives the joint at least 99% of its weight; the stroke's far end gives it nothing.
    let mut mesh = cloud(
        vec![v(0.25, 1.4, 0.0), v(0.31, 1.4, 0.0), v(0.9, 1.4, 0.0)],
        vec![role(H::LeftUpperArm)],
        vec![([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]); 3],
    );
    apply_brushes(&mut mesh, &[b]);
    let on_forearm = |vertex: usize| {
        skin_of(&mesh, vertex).into_iter().find(|(j, _)| *j == role(H::LeftLowerArm)).map_or(0.0, |(_, w)| w)
    };
    assert!(on_forearm(0) >= 0.99, "{}", on_forearm(0));
    assert_eq!(on_forearm(1), 0.0, "at the radius the falloff is out");
    assert_eq!(on_forearm(2), 0.0, "far away");
    assert_valid_skin(&mesh);

    assert_eq!(brush_from_stroke(&stroke, JointRef::HEAD, 7.0).unwrap().strength, 1.0, "strength is limited to 0..=1");
    assert_eq!(brush_from_stroke(&stroke, JointRef::HEAD, -1.0).unwrap().strength, 0.0);
    let mirrored = brush_mirrored(&brush_from_stroke(&stroke, role(H::LeftHand), 0.5).unwrap());
    assert_eq!(mirrored.centre[0], -0.25);
}

#[test]
fn bad_strokes_are_refused_with_the_reason() {
    assert!(matches!(brush_from_stroke(&[], JointRef::HEAD, 1.0), Err(KitError::TooFew { .. })));
    assert!(matches!(brush_from_stroke(&[[0.0, f64::NAN, 0.0]], JointRef::HEAD, 1.0), Err(KitError::NonFinite { .. })));
    assert!(matches!(
        brush_from_stroke(&[[0.0; 3], [1.0; 3]], JointRef::HEAD, f64::INFINITY),
        Err(KitError::NonFinite { .. })
    ));
    assert!(matches!(brush_from_stroke(&[[1.0; 3], [1.0; 3]], JointRef::HEAD, 1.0), Err(KitError::Degenerate { .. })));
}

// --------------------------------------------------------------------------------------------------------- deformation

/// Rotation about a joint, parent first: the skin matrix of every bone for a pose given as local rotations.
struct Pose {
    rest: BTreeMap<H, Vec3>,
    local: BTreeMap<H, Quat>,
}

impl Pose {
    /// `skin matrix = pose world * inverse(rest world)` of `bone`.
    fn skin_matrix(&self, bone: H) -> Mat4 {
        let rest_world = |b: H| Mat4::from_translation(self.rest[&b]);
        let chain: Vec<H> =
            std::iter::successors(Some(bone), |b| b.parent()).filter(|b| self.rest.contains_key(b)).collect();
        let mut world = Mat4::IDENTITY;
        let mut parent_rest = Vec3::ZERO;
        for b in chain.into_iter().rev() {
            let q = self.local.get(&b).copied().unwrap_or(Quat::IDENTITY);
            world *= Mat4::from_translation(self.rest[&b] - parent_rest) * Mat4::from_quat(q);
            parent_rest = self.rest[&b];
        }
        world * rest_world(bone).inverse()
    }

    /// Linear blend skinning of `mesh`.
    fn skin(&self, mesh: &PartMesh) -> Vec<Vec3> {
        let matrices: Vec<Mat4> = mesh
            .joint_bones
            .iter()
            .map(|j| match j {
                JointRef::Humanoid(h) => self.skin_matrix(*h),
                JointRef::Named(_) => Mat4::IDENTITY,
            })
            .collect();
        (0..mesh.geometry.positions.len())
            .map(|i| {
                let p = mesh.geometry.positions[i];
                let blended: Vec3 = mesh.geometry.joints[i]
                    .iter()
                    .zip(mesh.geometry.weights[i])
                    .map(|(j, w)| w * matrices[usize::from(*j)].transform_point3(p))
                    .sum();
                blended
            })
            .collect()
    }
}

/// An open tube of `sides` round `centres` (radius `radius`), with caps' ends left open, wound outward.
fn tube(centres: &[Vec3], radius: f32, sides: usize) -> (Vec<Vec3>, Vec<u32>) {
    let mut positions = Vec::new();
    for (k, c) in centres.iter().enumerate() {
        let axis = (centres[(k + 1).min(centres.len() - 1)] - centres[k.saturating_sub(1)]).normalize();
        let side = axis.cross(Vec3::Y).try_normalize().unwrap_or(Vec3::Z);
        let up = side.cross(axis);
        for s in 0..sides {
            let a = std::f32::consts::TAU * s as f32 / sides as f32;
            positions.push(*c + radius * (side * a.cos() + up * a.sin()));
        }
    }
    let mut indices = Vec::new();
    for k in 0..centres.len() - 1 {
        for s in 0..sides {
            let (a, b) = ((k * sides + s) as u32, (k * sides + (s + 1) % sides) as u32);
            let (c, e) = (a + sides as u32, b + sides as u32);
            indices.extend([a, b, c, b, e, c]);
        }
    }
    (positions, indices)
}

fn mesh_of((positions, indices): (Vec<Vec3>, Vec<u32>)) -> PartMesh {
    let n = positions.len();
    let mut mesh = cloud(positions, Vec::new(), vec![([0; 4], [1.0, 0.0, 0.0, 0.0]); n]);
    mesh.geometry.indices = indices;
    mesh
}

fn line(from: Vec3, to: Vec3, steps: usize) -> Vec<Vec3> {
    (0..=steps).map(|i| from.lerp(to, i as f32 / steps as f32)).collect()
}

/// The shortest and longest edge after posing, as a share of the edge at rest.
fn edge_stretch(mesh: &PartMesh, posed: &[Vec3]) -> (f32, f32) {
    let mut range = (f32::MAX, 0.0_f32);
    for &[a, b, c] in mesh.geometry.indices.as_chunks::<3>().0 {
        for (a, b) in [(a, b), (b, c), (c, a)].map(|(a, b)| (a as usize, b as usize)) {
            let rest = mesh.geometry.positions[a].distance(mesh.geometry.positions[b]);
            let ratio = posed[a].distance(posed[b]) / rest;
            range = (range.0.min(ratio), range.1.max(ratio));
        }
    }
    range
}

/// The left arm from shoulder to wrist as a 40 mm tube, skinned to the arm bones.
fn elbow_case() -> (PartMesh, Pose) {
    let joints = body(true);
    let mut mesh = mesh_of(tube(&line(joints[&H::LeftUpperArm], joints[&H::LeftHand], 56), 0.04, 14));
    let bones = [role(H::LeftUpperArm), role(H::LeftLowerArm)];
    segments(true).skin_mesh(&mut mesh, &bones, CapsuleParams::default()).unwrap();
    // Bend the elbow a right angle, the forearm swinging forward (+Z).
    let local = BTreeMap::from([(H::LeftLowerArm, Quat::from_rotation_y(-FRAC_PI_2))]);
    (mesh, Pose { rest: joints, local })
}

#[test]
fn a_bent_elbow_keeps_the_arm_whole_and_the_ends_rigid() {
    let (mesh, pose) = elbow_case();
    assert_valid_skin(&mesh);
    let posed = pose.skin(&mesh);
    let elbow = body(true)[&H::LeftLowerArm];
    let (shortest, longest) = edge_stretch(&mesh, &posed);
    println!("bent elbow: {} vertices, edges {shortest:.2} to {longest:.2} of their rest length", posed.len());
    assert!(shortest > 0.35 && longest < 1.5, "no edge collapses or tears: {shortest} to {longest}");
    let forearm = pose.skin_matrix(H::LeftLowerArm);
    let mut blended = 0;
    for (i, p) in mesh.geometry.positions.iter().enumerate() {
        let near = (p.x - elbow.x).abs();
        let on = |h: H| skin_of(&mesh, i).into_iter().find(|(j, _)| *j == role(h)).map_or(0.0, |(_, w)| w);
        if near > 0.2 {
            let (bone, expected) =
                if p.x < elbow.x { (H::LeftUpperArm, *p) } else { (H::LeftLowerArm, forearm.transform_point3(*p)) };
            assert!(on(bone) > 0.99, "vertex {i} {:.2} m from the elbow is rigid on its bone: {}", near, on(bone));
            assert!((posed[i] - expected).length() < 0.01 * posed[i].length().max(1.0), "vertex {i} follows its bone");
        } else if on(H::LeftUpperArm) > 0.01 && on(H::LeftLowerArm) > 0.01 {
            blended += 1;
        }
    }
    assert!(blended > 0, "the elbow itself is a blend of the two bones");
}

/// A finger (radius 8 mm) along `chain`, from its first joint to the continued tip, with rows along each phalanx so the
/// blend over a knuckle has vertices to act on. Not yet skinned.
fn finger_tube(chain: &[H]) -> PartMesh {
    let (joints, set) = (body(true), segments(true));
    let tip = set.get(&role(chain[2])).unwrap().tail.as_vec3();
    let mut centres: Vec<Vec3> = chain.iter().map(|b| joints[b]).collect();
    centres.push(tip);
    let rows: Vec<Vec3> = centres.windows(2).flat_map(|w| line(w[0], w[1], 6)[..6].to_vec()).chain([tip]).collect();
    mesh_of(tube(&rows, 0.008, 10))
}

/// The left hand with a palm block and the five finger tubes (radius 8 mm), each finger skinned to its own chain, the
/// hand and the forearm; the thumb and the palm to the hand and the knuckle bones.
fn fist_case() -> (PartMesh, Pose, Vec<usize>) {
    let joints = body(true);
    let set = segments(true);
    let hand = joints[&H::LeftHand];
    let mut parts: Vec<PartMesh> = Vec::new();
    let mut owner: Vec<usize> = Vec::new(); // which finger (0 thumb .. 4 little) each vertex belongs to; 5 is the palm
    for (finger, chain) in finger_chains(H::LeftHand).iter().enumerate() {
        let mut finger_mesh = finger_tube(chain);
        let mut candidates: Vec<JointRef> = vec![role(H::LeftLowerArm), role(H::LeftHand)];
        candidates.extend(chain.iter().map(|b| role(*b)));
        set.skin_mesh(&mut finger_mesh, &candidates, CapsuleParams::new(4.0, 0.004).unwrap()).unwrap();
        owner.extend(std::iter::repeat_n(finger, finger_mesh.geometry.positions.len()));
        parts.push(finger_mesh);
    }
    let mut palm = mesh_of(tube(&line(hand - Vec3::X * 0.02, hand + Vec3::X * 0.09, 6), 0.03, 12));
    set.skin_mesh(&mut palm, &[role(H::LeftLowerArm), role(H::LeftHand)], CapsuleParams::default()).unwrap();
    owner.extend(std::iter::repeat_n(5, palm.geometry.positions.len()));
    parts.push(palm);

    // Merge the parts into one mesh, joint lists unified.
    let mut merged = cloud(vec![Vec3::ZERO; 3], Vec::new(), vec![([0; 4], [1.0, 0.0, 0.0, 0.0]); 3]);
    merged.geometry = MeshData::default();
    for part in parts {
        let base = merged.geometry.positions.len() as u32;
        let remap: Vec<u16> = part
            .joint_bones
            .iter()
            .map(|j| {
                let at = merged.joint_bones.iter().position(|k| k == j).unwrap_or_else(|| {
                    merged.joint_bones.push(j.clone());
                    merged.joint_bones.len() - 1
                });
                at as u16
            })
            .collect();
        merged.geometry.positions.extend(&part.geometry.positions);
        merged.geometry.normals.extend(&part.geometry.normals);
        merged.geometry.joints.extend(part.geometry.joints.iter().map(|j| j.map(|k| remap[usize::from(k)])));
        merged.geometry.weights.extend(&part.geometry.weights);
        merged.geometry.indices.extend(part.geometry.indices.iter().map(|i| i + base));
    }
    // Curl: every finger closes toward the palm (-Y) by 80, 90 and 60 degrees at its three joints; the thumb folds less.
    let mut local = BTreeMap::new();
    for (finger, chain) in finger_chains(H::LeftHand).iter().enumerate() {
        let angles = if finger == 0 { [-0.5, -0.7, -0.6] } else { [-1.4, -1.6, -1.0] };
        for (bone, angle) in chain.iter().zip(angles) {
            local.insert(*bone, Quat::from_rotation_z(angle));
        }
    }
    (merged, Pose { rest: joints, local }, owner)
}

#[test]
fn each_finger_is_weighted_to_its_own_bones_and_a_fist_keeps_the_hand_whole() {
    let (mesh, pose, owner) = fist_case();
    assert_valid_skin(&mesh);
    let chains = finger_chains(H::LeftHand);
    let mut worst_leak = 0.0_f32;
    for (vertex, finger) in owner.iter().enumerate() {
        for (j, w) in skin_of(&mesh, vertex) {
            let JointRef::Humanoid(bone) = j else { panic!("only humanoid joints here") };
            let other_finger = chains.iter().enumerate().any(|(f, c)| f != *finger && c.contains(&bone));
            if other_finger {
                worst_leak = worst_leak.max(w);
            }
        }
    }
    println!("fist: weight on another finger's bones, worst vertex {worst_leak}");
    assert_eq!(worst_leak, 0.0, "a finger vertex pulls only on its own chain, the hand and the forearm");

    // Each of the hand's 15 finger bones owns vertices.
    for chain in &chains {
        for bone in chain {
            let used = (0..mesh.geometry.positions.len())
                .any(|i| skin_of(&mesh, i).iter().any(|(j, w)| *j == role(*bone) && *w > 0.5));
            assert!(used, "{bone:?} owns some vertices");
        }
    }
    let posed = pose.skin(&mesh);
    let (shortest, longest) = edge_stretch(&mesh, &posed);
    println!("fist: {} vertices, edges {shortest:.2} to {longest:.2} of their rest length", posed.len());
    assert!(shortest > 0.3 && longest < 1.7, "no finger collapses or tears: {shortest} to {longest}");
    // The tips end up under the knuckles: each fingertip is closer to the palm than at rest.
    let palm = body(true)[&H::LeftHand];
    let tip_of = |finger: usize| -> usize { owner.iter().rposition(|f| *f == finger).unwrap() };
    for finger in 1..5 {
        let tip = tip_of(finger);
        assert!(
            posed[tip].distance(palm) < 0.6 * mesh.geometry.positions[tip].distance(palm),
            "finger {finger} curls in"
        );
    }
}

#[test]
fn restricting_candidates_to_the_fingers_chain_is_what_keeps_neighbouring_fingers_apart() {
    // The index finger against all 15 finger bones: the vertices on the side facing the middle finger are about as close
    // to its axis as to their own (the fingers' skins are 2 mm apart), so no softening makes the weights clean. The
    // restricted weights of the fist test are clean by construction.
    let set = segments(true);
    let chains = finger_chains(H::LeftHand);
    let own = &chains[1];
    let mut everything: Vec<JointRef> = vec![role(H::LeftLowerArm), role(H::LeftHand)];
    everything.extend(chains.iter().flatten().map(|b| role(*b)));
    let leak = |params: CapsuleParams| {
        let mut mesh = finger_tube(own);
        set.skin_mesh(&mut mesh, &everything, params).unwrap();
        (0..mesh.geometry.positions.len())
            .map(|i| {
                skin_of(&mesh, i)
                    .into_iter()
                    .filter(|(j, _)| matches!(j, JointRef::Humanoid(b) if chains.iter().flatten().any(|c| c == b) && !own.contains(b)))
                    .map(|(_, w)| w)
                    .sum::<f32>()
            })
            .fold(0.0, f32::max)
    };
    let prototype = leak(CapsuleParams::default());
    let tight = leak(CapsuleParams::new(4.0, 0.004).unwrap());
    println!(
        "index finger against all 15 finger bones: worst weight on other fingers {prototype:.3} (e = 0.012), {tight:.3} (e = 0.004)"
    );
    assert!(prototype > 0.3 && tight > 0.3, "fingers 18 mm apart are one radius from each other's skin");
}

/// Writes the rest and posed vertices of the two deformation cases for the renders in the PR. Does nothing unless
/// `OMUZAKI_EVIDENCE_DIR` names a directory.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn deformation_evidence() {
    let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") else { return };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (elbow, elbow_pose) = elbow_case();
    let (fist, fist_pose, owner) = fist_case();
    for (name, mesh, pose) in [("elbow", &elbow, &elbow_pose), ("fist", &fist, &fist_pose)] {
        let names: Vec<String> = mesh.joint_bones.iter().map(|j| format!("{j:?}")).collect();
        let json = serde_json::json!({
            "joints": names,
            "rest": mesh.geometry.positions.iter().map(Vec3::to_array).collect::<Vec<_>>(),
            "posed": pose.skin(mesh).iter().map(Vec3::to_array).collect::<Vec<_>>(),
            "indices": mesh.geometry.indices,
            "skin_joints": mesh.geometry.joints,
            "skin_weights": mesh.geometry.weights,
            "owner": if name == "fist" { serde_json::json!(owner) } else { serde_json::Value::Null },
        });
        std::fs::write(dir.join(format!("{name}.json")), json.to_string()).unwrap();
    }
}

// ----------------------------------------------------------------------------------------------------- mirrored brushes

const SIDE_JOINTS: [H; 3] = [H::LeftUpperArm, H::LeftLowerArm, H::LeftHand];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Up to three brushes and their mirror images, however they overlap and straddle the centre plane, and wherever they
    /// sit in the list among centre-joint brushes, give a skin equal to its own mirror image within 1e-6. Plane
    /// vertices see several equal left/right pairs, so this covers the tie at the four-influence cut too.
    #[test]
    fn brushes_and_their_mirrors_give_a_skin_equal_to_its_mirror(
        half in proptest::collection::vec((0.0_f32..0.4, -0.3_f32..0.3, -0.3_f32..0.3), 1..20),
        pairs in proptest::collection::vec(
            ((-0.3_f64..0.3, -0.3_f64..0.3, -0.3_f64..0.3), 0.05_f64..0.5, 0.0_f64..1.0, 0_usize..3), 1..4),
        own in proptest::collection::vec((0.05_f64..0.4, 0.0_f64..1.0, any::<bool>()), 0..3),
        order in Just((0_usize..10).collect::<Vec<_>>()).prop_shuffle(),
    ) {
        let mut brushes = Vec::new();
        for &(centre, radius, strength, joint) in &pairs {
            let left = WeightBrush {
                centre: centre.into(),
                radius,
                joint: role(SIDE_JOINTS[joint]),
                strength,
                falloff: BrushFalloff::Smooth,
            };
            brushes.push(left.mirrored());
            brushes.push(left);
        }
        brushes.extend(own.iter().map(|&(radius, strength, chest)| WeightBrush {
            centre: [0.0, 0.0, 0.0], // on the plane with a centre joint: its own mirror
            radius,
            joint: if chest { JointRef::CHEST } else { JointRef::HIPS },
            strength,
            falloff: BrushFalloff::Linear,
        }));
        let brushes: Vec<_> = order.iter().filter_map(|&i| brushes.get(i).cloned()).collect();

        let n = half.len();
        let mut positions: Vec<Vec3> = half.iter().map(|&(x, y, z)| v(x, y, z)).collect();
        positions.extend(half.iter().map(|&(x, y, z)| v(-x, y, z)));
        // Vertices exactly on the plane, where the two sides of each pair meet.
        let plane = [v(0.0, 0.0, 0.0), v(0.0, 0.1, 0.05), v(0.0, -0.2, -0.1)];
        positions.extend(plane);
        let skin = ([0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0]);
        let mut mesh = cloud(positions, vec![JointRef::CHEST, JointRef::HIPS], vec![skin; 2 * n + plane.len()]);
        apply_brushes(&mut mesh, &brushes);
        assert_valid_skin(&mesh);
        for i in 0..n {
            let (here, there) = (skin_of(&mesh, i), swapped(&skin_of(&mesh, n + i)));
            prop_assert!(same_skin(&here, &there), "vertex {i}: {here:?} vs {there:?}");
        }
        for i in 2 * n..2 * n + plane.len() {
            let (here, there) = (skin_of(&mesh, i), swapped(&skin_of(&mesh, i)));
            prop_assert!(same_skin(&here, &there), "plane vertex {i}: {here:?} vs {there:?}");
        }
    }
}

#[test]
fn a_tie_at_the_four_influence_cut_drops_the_whole_group() {
    // A plane vertex with three strong influences (chest 0.4, hips 0.3, neck 0.3) and a brush pair that gives the left and
    // right upper arm 0.1 each, bit-equal by the pair's symmetry. Five weights compete for four slots and the arms tie at
    // the cut: keeping one would favour a side, so both go and the vertex keeps its three strong ones.
    let arm = WeightBrush {
        centre: [0.02, 0.0, 0.0],
        radius: 0.2,
        joint: role(H::LeftUpperArm),
        strength: 0.1,
        falloff: BrushFalloff::Constant,
    };
    let joints = vec![JointRef::CHEST, JointRef::HIPS, role(H::Neck)];
    let mut mesh = cloud(vec![v(0.0, 0.0, 0.0); 3], joints, vec![([0, 1, 2, 0], [0.4, 0.3, 0.3, 0.0]); 3]);
    apply_brushes(&mut mesh, &[arm.clone(), arm.mirrored()]);
    assert_valid_skin(&mesh);
    let skin = skin_of(&mesh, 0);
    let arms = [role(H::LeftUpperArm), role(H::RightUpperArm)];
    assert_eq!(skin.len(), 3, "{skin:?}");
    assert!(skin.iter().all(|(j, _)| !arms.contains(j)), "neither arm is kept: {skin:?}");
    assert!(same_skin(&skin, &swapped(&skin)), "{skin:?}");
}
