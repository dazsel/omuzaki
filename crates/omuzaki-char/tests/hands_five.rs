//! Five-finger hands (RG-09): both hands on every template, within their triangle budget, finite, skinned to valid
//! normalised weights that reach every finger bone, each finger ending near where its own joints say its tip is, and
//! mirrored exactly.

#![allow(clippy::unwrap_used, clippy::expect_used)] // fixtures that must exist

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::body::hands;
use omuzaki_char::template::registry;
use omuzaki_char_api::{BodyParams, HandParams, HandStyle, JointRef, MaterialName, PartMesh};
use omuzaki_core::glam::Vec3;

/// Every body template.
const TEMPLATES: [&str; 4] = ["soft-sd", "standard", "mini", "tall"];

/// A five-finger hand's triangle budget at Live quality: the mitten's palm and thumb, and four fingers of 15 rings.
const BUDGET: usize = 3200;

fn params(hand: HandParams) -> BodyParams {
    BodyParams { hands: HandStyle::FiveFinger, hand, ..BodyParams::default() }
}

fn build(template: &str, p: &BodyParams) -> (omuzaki_char::template::layout::BodyLayout, Vec<PartMesh>) {
    let layout = registry::find(template).unwrap().try_layout(&p.proportions, &p.shape, &p.hand).unwrap();
    let parts = hands::build(p, &layout, &MaterialName::new("body-skin").unwrap()).unwrap();
    (layout, parts)
}

fn mesh<'a>(parts: &'a [PartMesh], name: &str) -> &'a PartMesh {
    parts.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("no mesh `{name}`"))
}

/// The bones a mesh's vertices actually lean on, with more than a trace of weight.
fn bones_used(m: &PartMesh) -> Vec<JointRef> {
    let mut used: Vec<JointRef> = Vec::new();
    for (joints, weights) in m.geometry.joints.iter().zip(&m.geometry.weights) {
        for (j, w) in joints.iter().zip(weights) {
            let bone = m.joint_bones[usize::from(*j)].clone();
            if *w > 0.05 && !used.contains(&bone) {
                used.push(bone);
            }
        }
    }
    used
}

#[test]
fn both_hands_build_on_every_template_within_budget_and_finite() {
    for template in TEMPLATES {
        let (_, parts) = build(template, &params(HandParams::default()));
        let names: Vec<&str> = parts.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["hand-l", "hand-r"], "{template}");
        for m in &parts {
            let triangles = m.geometry.triangle_count();
            assert!(triangles <= BUDGET, "{template} {}: {triangles} triangles over {BUDGET}", m.name);
            assert!(m.geometry.positions.iter().all(|p| p.is_finite()), "{template} {}", m.name);
            assert!(m.geometry.normals.iter().all(|n| (n.length() - 1.0).abs() < 1e-3), "{template} {}", m.name);
            for w in &m.geometry.weights {
                let sum: f32 = w.iter().sum();
                assert!((sum - 1.0).abs() < 1e-4, "{template} {}: weights sum to {sum}", m.name);
            }
        }
    }
}

#[test]
fn every_finger_bone_of_both_hands_moves_part_of_the_mesh() {
    let (_, parts) = build("soft-sd", &params(HandParams::default()));
    for (name, side) in [("hand-l", true), ("hand-r", false)] {
        let used = bones_used(mesh(&parts, name));
        let fingers = HumanoidBone::ALL.into_iter().filter(|b| {
            let n = b.name();
            n.starts_with(if side { "left" } else { "right" })
                && ["Thumb", "Index", "Middle", "Ring", "Little"].iter().any(|f| n.contains(f))
        });
        for bone in fingers {
            assert!(used.contains(&JointRef::Humanoid(bone)), "{name}: nothing is skinned to {bone:?}");
        }
    }
}

#[test]
fn each_finger_ends_where_its_joints_say_and_follows_the_finger_length() {
    for template in TEMPLATES {
        for finger_length in [0.7, 1.0, 1.4] {
            let (layout, parts) = build(template, &params(HandParams { finger_length, ..HandParams::default() }));
            let hand = mesh(&parts, "hand-l");
            for [proximal, intermediate, distal] in [
                [HumanoidBone::LeftIndexProximal, HumanoidBone::LeftIndexIntermediate, HumanoidBone::LeftIndexDistal],
                [
                    HumanoidBone::LeftLittleProximal,
                    HumanoidBone::LeftLittleIntermediate,
                    HumanoidBone::LeftLittleDistal,
                ],
            ] {
                let joint = |b| layout.joint64(b).unwrap().as_vec3();
                let (p0, p1, p2) = (joint(proximal), joint(intermediate), joint(distal));
                let direction = (p2 - p1).normalize();
                // The finger's furthest reach along its distal segment, from the knuckle, over the vertices its distal
                // bone carries most of.
                let g = &hand.geometry;
                let reach = (0..g.positions.len())
                    .filter(|&v| {
                        g.joints[v]
                            .iter()
                            .zip(&g.weights[v])
                            .any(|(j, w)| hand.joint_bones[usize::from(*j)] == JointRef::Humanoid(distal) && *w > 0.5)
                    })
                    .map(|v| (g.positions[v] - p0).dot(direction))
                    .fold(f32::MIN, f32::max);
                let length = p0.distance(p1) + p1.distance(p2) + p2.distance(p1) * (0.25 / 0.30);
                let error = (reach - length).abs() / length;
                assert!(error < 0.2, "{template} x{finger_length} {proximal:?}: reaches {reach} for a {length} finger");
            }
        }
    }
}

#[test]
fn the_right_hand_is_the_left_mirrored() {
    let (_, parts) = build("standard", &params(HandParams::default()));
    let (left, right) = (mesh(&parts, "hand-l"), mesh(&parts, "hand-r"));
    assert_eq!(left.geometry.positions.len(), right.geometry.positions.len());
    for (l, r) in left.geometry.positions.iter().zip(&right.geometry.positions) {
        assert!((*l * Vec3::new(-1.0, 1.0, 1.0) - *r).length() < 1e-6);
    }
}

#[test]
fn thickness_and_taper_shape_the_fingers() {
    let width = |hand: HandParams| {
        let (_, parts) = build("soft-sd", &params(hand));
        let m = mesh(&parts, "hand-l");
        let (lo, hi) = m.geometry.positions.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        (hi - lo).length()
    };
    let thin = width(HandParams { finger_thickness: 0.6, ..HandParams::default() });
    let thick = width(HandParams { finger_thickness: 1.6, ..HandParams::default() });
    assert!(thick > thin, "thicker fingers make a bigger hand ({thick} vs {thin})");
    let (_, blunt) = build("soft-sd", &params(HandParams { taper: 0.5, ..HandParams::default() }));
    let (_, sharp) = build("soft-sd", &params(HandParams { taper: 2.0, ..HandParams::default() }));
    assert_ne!(mesh(&blunt, "hand-l").geometry.positions, mesh(&sharp, "hand-l").geometry.positions);
}
