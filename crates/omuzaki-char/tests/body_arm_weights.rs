//! The arm's hand-over from the shoulder bone to the upper arm is narrow, and the two arms mirror.
//!
//! A wide hand-over blends a still clavicle into most of a raised arm and costs it a third of its volume (#709), so the
//! shoulder bone's weight must be zero from the arm's middle on, on every template. The volume itself is measured
//! in `crates/omuzaki-test-char/tests/char_body_posed.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures that must exist

use omuzaki_anim_api::HumanoidBone as B;
use omuzaki_char::body::mesh::{self, Detail};
use omuzaki_char::kit::skin::{SegmentSet, max_neighbour_jump};
use omuzaki_char::template::registry;
use omuzaki_char::template::skeleton::TemplateFit;
use omuzaki_char_api::{BodyRegion, BodyShape, HandParams, JointRef, MaterialName, PartMesh, Proportions};
use omuzaki_core::glam::Vec3;

/// The last ring, counted from the arm's root, the shoulder bone may still weigh on at live detail (12 rings along the
/// arm); offline detail scales it with the rings. Before the change the shoulder reached the end of the arm.
const ROOT_RINGS: usize = 7;

/// How far mirrored vertices' weights may differ: the right arm takes the left's weights, mirrored, so not at all
/// beyond rounding.
const MIRROR_MAX: f32 = 1e-6;

fn arms(template: &str, detail: Detail) -> (PartMesh, PartMesh) {
    let t = registry::find(template).unwrap();
    let layout = t.try_layout(&Proportions::default(), &BodyShape::default(), &HandParams::default()).unwrap();
    let fit = TemplateFit::new(layout);
    let segments = SegmentSet::from_fit(&fit, &mesh::all_bones()).unwrap();
    let built = mesh::build_regions(fit.layout(), &segments, detail, &MaterialName::new("body-skin").unwrap()).unwrap();
    let get = |r| built.iter().find(|(region, _)| *region == r).map(|(_, m)| m.clone()).unwrap();
    (get(BodyRegion::ArmL), get(BodyRegion::ArmR))
}

/// The weight vertex `v` of `mesh` gives `bone`.
fn weight_on(mesh: &PartMesh, v: usize, bone: B) -> f32 {
    let g = &mesh.geometry;
    let Some(slot) = mesh.joint_bones.iter().position(|j| *j == JointRef::Humanoid(bone)) else { return 0.0 };
    g.joints[v].iter().zip(&g.weights[v]).filter(|(j, _)| usize::from(**j) == slot).map(|(_, w)| *w).sum()
}

/// The ring a vertex lies on, counted from the arm's root (the start cap's rings are before it): the mesh is a pole
/// vertex, then `around + 1` vertices per row.
fn ring_of(v: usize, detail: Detail) -> usize {
    if v == 0 { 0 } else { 1 + (v - 1) / (detail.around + 1) - detail.cap_rings.min(1 + (v - 1) / (detail.around + 1)) }
}

#[test]
fn the_shoulder_weight_is_zero_past_the_arm_root_rings() {
    for detail in [Detail::LIVE, Detail::OFFLINE] {
        for t in registry::templates() {
            let (left, _) = arms(t.id(), detail);
            let mut last = 0;
            for v in 0..left.geometry.positions.len() {
                if weight_on(&left, v, B::LeftShoulder) > 0.0 {
                    last = last.max(ring_of(v, detail));
                }
            }
            let allowed = ROOT_RINGS * detail.arm_rows / Detail::LIVE.arm_rows;
            eprintln!("{} {} rows: the shoulder bone reaches ring {last}", t.id(), detail.arm_rows);
            assert!(last <= allowed, "{}: the shoulder weighs on ring {last} (limit {allowed})", t.id());
        }
    }
}

#[test]
fn the_arm_still_hands_over_smoothly_and_its_weights_sum_to_one() {
    for t in registry::templates() {
        let (left, right) = arms(t.id(), Detail::LIVE);
        for m in [&left, &right] {
            let jump = max_neighbour_jump(m);
            assert!(jump <= 0.35, "{} {}: adjacent weights jump by {jump}", t.id(), m.name);
            for w in &m.geometry.weights {
                assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-4, "{} {}: weights {w:?}", t.id(), m.name);
            }
        }
    }
}

#[test]
fn the_left_and_right_arms_mirror() {
    for t in registry::templates() {
        let (left, right) = arms(t.id(), Detail::LIVE);
        assert_eq!(left.geometry.positions.len(), right.geometry.positions.len());
        let (mut worst, mut total, mut count) = (0.0_f32, 0.0_f32, 0.0_f32);
        for (v, p) in left.geometry.positions.iter().enumerate() {
            // The right arm's vertex at the left one's reflection in the YZ plane (the rings run round the other way, so
            // the vertex numbers differ).
            let mirrored = Vec3::new(-p.x, p.y, p.z);
            let (r, gap) = right
                .geometry
                .positions
                .iter()
                .enumerate()
                .map(|(r, q)| (r, q.distance(mirrored)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            assert!(gap < 1e-4, "{} vertex {v}: no mirror image ({gap})", t.id());
            for (l, rb) in [(B::LeftShoulder, B::RightShoulder), (B::LeftUpperArm, B::RightUpperArm)] {
                let d = (weight_on(&left, v, l) - weight_on(&right, r, rb)).abs();
                worst = worst.max(d);
                total += d;
                count += 1.0;
            }
        }
        assert!(worst <= MIRROR_MAX, "{}: mirrored weights differ by {worst} (mean {})", t.id(), total / count);
    }
}
