//! Cloth baked to spring-bone chains and a skinned mesh for VRM export (#386): budgets, chain structure, parameters in
//! VRM's ranges, the baked mesh at rest, colliders and warnings. The spring comparison through a hips turn is in
//! `omuzaki-test-char/tests/cloth_bake_springs.rs` (it needs `omuzaki-rig`).

#![allow(clippy::unwrap_used, clippy::cast_precision_loss)]

#[path = "cloth_runtime/skirt.rs"]
mod skirt;

use omuzaki_char::cloth::bake::{BakeTarget, ROBE_BONES, SKIRT_BONES, bake};
use omuzaki_char::cloth::collide::ColliderSet;
use omuzaki_char::cloth::runtime::{ClothRuntime, ClothSpec, SheetSize};
use omuzaki_char::template::registry;
use omuzaki_char_api::{BodyShape, HandParams, JointRef, Proportions};
use omuzaki_core::glam::Vec3;
use skirt::{leg_colliders, robe_grid, skirt_grid};

fn body() -> impl omuzaki_char_api::BodyFit {
    registry::templates()[0].layout(&Proportions::default(), &BodyShape::default(), &HandParams::default())
}

#[test]
fn a_skirt_bakes_within_80_bones_and_a_robe_within_120() {
    let fit = body();
    let skirt = bake(&skirt::skirt(), "skirt", SKIRT_BONES, &fit).unwrap();
    let robe_rt = ClothRuntime::new(&robe_grid(), &ClothSpec::default(), leg_colliders()).unwrap();
    let robe = bake(&robe_rt, "robe", ROBE_BONES, &fit).unwrap();
    println!(
        "skirt: {} chains x {} bones = {}; robe: {} x {} = {}",
        skirt.report.chains,
        skirt.report.bones_per_chain,
        skirt.rig.bones.len(),
        robe.report.chains,
        robe.report.bones_per_chain,
        robe.rig.bones.len()
    );
    assert!(skirt.rig.bones.len() <= SKIRT_BONES && skirt.rig.bones.len() >= SKIRT_BONES * 3 / 4);
    assert!(robe.rig.bones.len() <= ROBE_BONES && robe.rig.bones.len() >= ROBE_BONES * 3 / 4);
    // A tighter budget gets fewer chains.
    let small = bake(&skirt::skirt(), "skirt", 40, &fit).unwrap();
    assert!(small.rig.bones.len() <= 40);
    assert!(bake(&skirt::skirt(), "skirt", 3, &fit).is_err());
}

#[test]
fn chains_hang_from_the_waist_bone_by_bone_with_parameters_in_vrm_ranges() {
    let fit = body();
    let b = bake(&skirt::skirt(), "skirt", SKIRT_BONES, &fit).unwrap();
    let names: std::collections::BTreeSet<_> = b.rig.bones.iter().map(|x| x.name.clone()).collect();
    assert_eq!(names.len(), b.rig.bones.len(), "bone names are unique");
    for (c, chain) in b.rig.chains.iter().enumerate() {
        assert_eq!(chain.name, format!("cloth_skirt_{c}"));
        assert_eq!(chain.joints.len(), b.report.bones_per_chain);
        for (r, joint) in chain.joints.iter().enumerate() {
            let bone = &b.rig.bones[usize::from(joint.bone.0) - omuzaki_anim_api::HUMANOID_COUNT];
            // The root hangs from a humanoid joint (the hips here); every other bone from the one above it.
            match (r, &bone.parent) {
                (0, JointRef::Humanoid(_)) => {}
                (_, JointRef::Named(parent)) => assert_eq!(parent.to_string(), format!("cloth_skirt_{c}_{}", r - 1)),
                other => panic!("chain {c} joint {r}: unexpected parent {other:?}"),
            }
            let p = joint.params;
            assert!((0.0..=4.0).contains(&p.stiffness), "stiffness {}", p.stiffness);
            assert!((0.0..=1.0).contains(&p.drag_force) && (0.0..=2.0).contains(&p.gravity_power));
            assert!(p.hit_radius >= 0.0 && p.hit_radius < 0.1 && (p.gravity_dir - Vec3::NEG_Y).length() < 1e-6);
        }
        assert_eq!(chain.collider_groups, ["cloth_skirt"]);
    }
    // The root's stiffness halves to the tip.
    let chain = &b.rig.chains[0];
    let (first, last) = (chain.joints[0].params.stiffness, chain.joints.last().unwrap().params.stiffness);
    assert!((last - 0.5 * first).abs() < 1e-5);
}

#[test]
fn the_baked_mesh_at_rest_is_the_garment_at_rest() {
    let fit = body();
    let rt = skirt::skirt();
    let b = bake(&rt, "skirt", SKIRT_BONES, &fit).unwrap();
    let rest: Vec<Vec3> = rt.grid_rest().collect();
    // At rest every bone sits at its bind place, and every joint too: each target's matrix is the identity, so a
    // vertex skinned to anything stays where it was bound; the weights must still sum to 1 with at most 4.
    let mut worst = 0.0_f32;
    for (p, w) in rest.iter().zip(&b.weights) {
        assert!(!w.is_empty() && w.len() <= 4);
        let total: f32 = w.iter().map(|(_, x)| x).sum();
        assert!((total - 1.0).abs() < 1e-5, "{total}");
        let posed: Vec3 = w.iter().map(|(_, x)| *p * *x).sum();
        worst = worst.max(posed.distance(*p));
    }
    assert!(worst < 0.001 && b.report.max_rest_deviation < 0.001, "{worst}");
    // The waist row follows the body; the hem only the bones.
    let cols = rt.grid_shape().1;
    assert!(b.weights[0].iter().all(|(t, _)| matches!(t, BakeTarget::Joint(_))));
    let hem = &b.weights[rest.len() - cols / 2];
    assert!(hem.iter().all(|(t, _)| matches!(t, BakeTarget::Bone(_))));
}

#[test]
fn the_bake_reports_what_it_loses_and_exports_body_colliders() {
    let fit = body();
    let mut colliders = ColliderSet::new();
    colliders.add_body(&fit);
    colliders.add_shell(&skirt_grid(), 4, 12, 0.01, 0.03).unwrap();
    let spec = ClothSpec {
        levels: vec![SheetSize::new(24, 16)],
        lod: omuzaki_char::cloth::runtime::LodPolicy { level_px: vec![], ..Default::default() },
        ..ClothSpec::default()
    };
    let rt = ClothRuntime::new(&robe_grid(), &spec, colliders).unwrap();
    let b = bake(&rt, "robe", ROBE_BONES, &fit).unwrap();
    println!("{:#?}", b.warnings);
    assert!(b.warnings[0].contains("spring chains") && b.warnings[0].contains("lost in export"));
    assert!(b.warnings.iter().any(|w| w.contains("not exported")), "the shell's capsules are reported");
    assert_eq!(b.rig.collider_groups.len(), 1);
    assert_eq!(b.report.colliders, b.rig.collider_groups[0].colliders.len());
    assert!(b.report.colliders >= 12, "the body's shapes export: {}", b.report.colliders);
}
