//! The export bake (#185) on the kit: built for export, the simulated hair hangs on spring chains that validate against
//! the built skeleton, stay within a VRM viewer's bone budget, reproduce the live hair at rest, carry normalised weights,
//! and take their springs from the hair's physics.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod common;

use omuzaki_anim_api::Pose;
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_doc_mock::MemDoc;
use serde_json::json;

fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    doc
}

fn exported(doc: &MemDoc) -> BuiltCharacter {
    let services = BuildServices::default().with_rasterizer(|| Box::new(omuzaki_face_mock::MockRasterizer));
    let options = BuildOptions { rig_export: true, ..BuildOptions::default() };
    RegistryBuilder.build(&CharId::new("kit").unwrap(), &common::definition(doc, "kit"), &options, &services).unwrap()
}

/// The hair's spring chains (named `hair_<part>_...`).
fn hair_chains(built: &BuiltCharacter) -> Vec<&omuzaki_anim_api::SpringChain> {
    built.rig.springs.chains.iter().filter(|c| c.name.starts_with("hair_hair_")).collect()
}

#[test]
fn the_kits_hair_bakes_to_valid_chains_within_the_bone_budget() {
    let built = exported(&kit());
    built.rig.springs.validate(&built.rig.skeleton).unwrap();
    let chains = hair_chains(&built);
    let bones = built.rig.skeleton.bones().iter().filter(|b| b.name.starts_with("hair_")).count();
    println!("the kit's hair exports {} spring chains on {bones} bones", chains.len());
    assert!(chains.len() >= 2, "the curtain and the framing locks swing: {}", chains.len());
    assert!(bones <= 120, "{bones} hair bones is over a VRM viewer's budget");
    assert!(built.runtimes.iter().all(|r| r.part.as_str() != "hair"), "no runtime in an export build");
}

#[test]
fn the_exported_hair_at_rest_is_the_live_hair() {
    let doc = kit();
    let live = common::build(&doc, "kit");
    let built = exported(&doc);
    let rig = &built.rig;
    let (mut world, mut skin) = (Pose::IDENTITY, vec![Mat4::ZERO; rig.skeleton.len()]);
    rig.skeleton.skin_matrices(&rig.skeleton.rest_pose(), &rig.inverse_bind(), &mut world, &mut skin);
    let mut worst = 0.0f32;
    for name in ["hair-static", "hair-dynamic"] {
        let mesh = &built.meshes.iter().find(|m| m.mesh.name == name).expect("the export has the mesh").mesh;
        let reference = &live.meshes.iter().find(|m| m.mesh.name == name).expect("the live build has it").mesh;
        let g = &mesh.geometry;
        assert_eq!(g.positions.len(), reference.geometry.positions.len(), "{name}");
        let bones: Vec<usize> =
            mesh.joint_bones.iter().map(|j| rig.bone(j).expect("a joint of the rig").index()).collect();
        for (k, p) in g.positions.iter().enumerate() {
            let (joints, weights) = (g.joints[k], g.weights[k]);
            let sum: f32 = weights.iter().sum();
            assert!((sum - 1.0).abs() < 1e-4, "{name} vertex {k}: weights sum to {sum}");
            assert!(joints.iter().all(|j| usize::from(*j) < bones.len()), "{name} vertex {k}: a joint out of the list");
            let skinned = (0..4)
                .fold(Vec3::ZERO, |at, i| at + skin[bones[usize::from(joints[i])]].transform_point3(*p) * weights[i]);
            worst = worst.max(skinned.distance(reference.geometry.positions[k]));
        }
    }
    println!("skinned at rest, the exported hair is at most {:.4} mm from the live hair", worst * 1e3);
    assert!(worst < 0.001, "{worst} m");
}

#[test]
fn a_preset_carries_into_the_exported_springs() {
    let mut doc = kit();
    let tip = |built: &BuiltCharacter| {
        let chain = hair_chains(built)[0];
        chain.joints.last().unwrap().params
    };
    let silky = tip(&exported(&doc));
    common::apply(&mut doc, "hair.physics.preset", json!({"character": "kit", "part": "hair", "preset": "heavy"}));
    let heavy = tip(&exported(&doc));
    common::apply(&mut doc, "hair.physics.preset", json!({"character": "kit", "part": "hair", "preset": "stiff"}));
    let stiff = tip(&exported(&doc));
    println!("tip springs: silky {silky:?}\n heavy {heavy:?}\n stiff {stiff:?}");
    assert!(heavy.gravity_power > silky.gravity_power && heavy.drag_force > silky.drag_force);
    assert!(stiff.stiffness > heavy.stiffness && heavy.stiffness > silky.stiffness);
}
