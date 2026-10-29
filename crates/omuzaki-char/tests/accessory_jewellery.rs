//! Fine jewellery (#2413): `ring`, `earrings`, `bracelet` and the `filigree-collar` on the kit character of the standard
//! and tall templates. Each builds in the `metal` finish with its stones in `gem`, sits where it belongs (the ring on its
//! finger's first segment, an earring under each side ear, the bracelet above the wrist, the collar's motifs across the
//! front of the neck), and follows the joint it sits on.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

mod common;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::accessory::attach::{AttachTarget, attach_points};
use omuzaki_char_api::{BuiltCharacter, JointRef, MaterialClass, MaterialName};
use omuzaki_core::glam::Vec3;
use omuzaki_doc_mock::MemDoc;
use omuzaki_render_api::Shading;
use serde_json::{Value, json};

fn dressed(template: &str, parts: &[(&str, &str, Value)]) -> BuiltCharacter {
    let mut doc = MemDoc::default();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": template}));
    for (kind, id, params) in parts {
        common::round_trip(&mut doc, "part.add", json!({"character": "kit", "kind": kind, "id": id, "params": params}));
    }
    common::build(&doc, "kit")
}

/// The middle of every vertex of the meshes whose name starts with `prefix`.
fn centre(built: &BuiltCharacter, prefix: &str) -> Vec3 {
    let points: Vec<Vec3> = built
        .meshes
        .iter()
        .filter(|m| m.mesh.name.starts_with(prefix))
        .flat_map(|m| m.mesh.geometry.positions.iter().copied())
        .collect();
    assert!(!points.is_empty(), "no mesh starts with {prefix}");
    points.iter().sum::<Vec3>() / points.len() as f32
}

fn joint(built: &BuiltCharacter, bone: HumanoidBone) -> Vec3 {
    attach_points(built)
        .into_iter()
        .find_map(|p| match p.target {
            AttachTarget::Bone { joint: JointRef::Humanoid(b) } if b == bone => Some(Vec3::from(p.position)),
            _ => None,
        })
        .unwrap()
}

fn program(built: &BuiltCharacter, material: &str) -> String {
    let name = MaterialName::new(material).unwrap();
    match &built.materials[&name].shading {
        Shading::Program { name, .. } => name.to_string(),
        other => panic!("{material} is {other:?}"),
    }
}

#[test]
fn every_piece_builds_in_metal_with_gem_stones_on_both_adult_templates() {
    for template in ["standard", "tall"] {
        let built = dressed(
            template,
            &[
                ("ring", "ring", json!({})),
                ("earrings", "earrings", json!({})),
                ("bracelet", "bracelet", json!({"style": "cuff", "stone": {}})),
                ("filigree-collar", "collar", json!({})),
            ],
        );
        for part in ["ring", "earrings", "bracelet", "collar"] {
            assert_eq!(program(&built, &format!("{part}-primary")), "metal", "{template}: {part}");
            assert_eq!(program(&built, &format!("{part}-accent")), "gem", "{template}: {part}");
            let class = built.material_classes[&MaterialName::new(format!("{part}-primary")).unwrap()];
            assert_eq!(class, MaterialClass::Metal, "{template}: {part}");
        }
    }
}

#[test]
fn a_ring_sits_on_its_fingers_first_segment_on_either_hand() {
    let built =
        dressed("standard", &[("ring", "l", json!({})), ("ring", "r", json!({"hand": "right", "finger": "index"}))]);
    let (root, next) =
        (joint(&built, HumanoidBone::LeftRingProximal), joint(&built, HumanoidBone::LeftRingIntermediate));
    let on = centre(&built, "l-");
    let along = (on - root).dot((next - root).normalize());
    assert!((0.0..=root.distance(next)).contains(&along), "between the knuckle and the next joint: {along}");
    assert!(on.distance(root.lerp(next, 0.45)) < 0.012, "{on} against {root} to {next}");
    let right = centre(&built, "r-");
    let index = joint(&built, HumanoidBone::RightIndexProximal);
    assert!(right.x < 0.0 && right.distance(index) < 0.03, "the right index finger: {right} near {index}");
}

#[test]
fn earrings_hang_under_each_side_ear_and_one_side_makes_one() {
    let built = dressed("standard", &[("earrings", "earrings", json!({}))]);
    let head = joint(&built, HumanoidBone::Head);
    let (left, right) = (centre(&built, "earrings-primary-0"), centre(&built, "earrings-primary-1"));
    assert!(left.x > 0.03 && right.x < -0.03, "one each side: {left} {right}");
    assert!((left.x + right.x).abs() < 1e-3 && (left.y - right.y).abs() < 1e-3, "mirrored: {left} {right}");
    assert!(left.y < head.y + 0.1, "low on the head: {left} against the head joint {head}");
    let one = dressed("standard", &[("earrings", "earrings", json!({"ears": "left", "style": "hoop"}))]);
    let names: Vec<_> =
        one.meshes.iter().filter(|m| m.part.as_str() == "earrings").map(|m| m.mesh.name.clone()).collect();
    assert_eq!(names, ["earrings-primary"], "a hoop on one ear is one metal mesh");
}

#[test]
fn a_bracelet_wraps_the_forearm_above_the_wrist() {
    let built = dressed("standard", &[("bracelet", "bracelet", json!({"along": 0.015}))]);
    let (wrist, elbow) = (joint(&built, HumanoidBone::LeftHand), joint(&built, HumanoidBone::LeftLowerArm));
    let on = centre(&built, "bracelet-");
    let toward_elbow = (on - wrist).dot((elbow - wrist).normalize());
    assert!((0.005..0.03).contains(&toward_elbow), "a little above the wrist: {toward_elbow}");
}

#[test]
fn the_collar_hangs_its_motifs_across_the_front() {
    let count = |motifs: u8| {
        let built = dressed("standard", &[("filigree-collar", "collar", json!({"motifs": motifs}))]);
        let accent: usize = built
            .meshes
            .iter()
            .filter(|m| m.mesh.name == "collar-accent")
            .map(|m| m.mesh.geometry.positions.len())
            .sum();
        (built, accent)
    };
    let (one, stones_1) = count(1);
    let (seven, stones_7) = count(7);
    assert_eq!(stones_7, 7 * stones_1, "a stone at each motif's foot");
    let (neck, front) = (joint(&seven, HumanoidBone::Neck), centre(&seven, "collar-accent"));
    assert!(front.z > neck.z, "the motifs hang in front of the neck: {front} against {neck}");
    let _ = one;
}
