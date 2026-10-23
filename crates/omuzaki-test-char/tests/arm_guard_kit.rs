//! The arm guard (#2353) on the real kit: each template's body, built through the reference builder, is measured from
//! its own skinned meshes the way the stage measures it, and a pose that drives the forearms through the chest ends the
//! frame with them outside, while the character's own bind pose (which the body is made clear of) is left exactly as
//! it is.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)] // test helpers: a failure is the report

use std::sync::Arc;

use omuzaki_anim_api::{BoneId, HumanoidBone as B, HumanoidPose, Pose};
use omuzaki_char::template::registry;
use omuzaki_char_api::{
    BodyParams, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, RegistryBuilder,
};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_rig::ik::guard::{ArmGuard, BodyProxy, GuardConfig, SkinnedMesh};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;

fn build(template: &str) -> BuiltCharacter {
    let body = serde_json::to_value(BodyParams::default()).unwrap();
    let node = serde_json::json!({
        "name": "Guarded",
        "template": template,
        "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80" },
        "parts": { "body": { "kind": "body", "params": body } }
    });
    let def = CharacterDef::from_node(&node).unwrap();
    RegistryBuilder
        .build(&CharId::new("guarded").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

/// The guard measured from `built`'s meshes, as the stage measures it (`omuzaki-stage` `frame/rig.rs`).
fn guard(built: &BuiltCharacter) -> ArmGuard {
    let skeleton = &built.rig.skeleton;
    let rest = skeleton.rest_pose();
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&rest, &mut world);
    let inverse_bind = built.rig.inverse_bind();
    let mut vertices = Vec::new();
    for m in built.meshes.iter().map(|m| &m.mesh).filter(|m| !m.dynamic && !m.joint_bones.is_empty()) {
        let bones: Vec<Option<BoneId>> = m.joint_bones.iter().map(|j| built.rig.bone(j)).collect();
        let g = &m.geometry;
        let mesh = SkinnedMesh { positions: &g.positions, joints: &g.joints, weights: &g.weights, bones: &bones };
        BodyProxy::gather(&world, mesh, |_, bone| inverse_bind.get(bone.index()).copied(), &mut vertices);
    }
    let mut proxy = BodyProxy::fit(skeleton, &rest, &vertices).expect("the kit body is measured");
    proxy.clear_of(skeleton, &posed(built, &built.rig.bind), GuardConfig::default().margin);
    ArmGuard::new(proxy, GuardConfig::default())
}

fn posed(built: &BuiltCharacter, pose: &HumanoidPose) -> Pose {
    let rig =
        HumanoidRig::new(RigSkeleton::with_inverse_bind(built.rig.skeleton.clone(), built.rig.inverse_bind()).unwrap());
    let mut out = Pose::IDENTITY;
    rig.apply_humanoid(pose, &mut out);
    out
}

#[test]
fn each_templates_forearms_are_kept_out_of_its_chest_and_its_bind_pose_is_left_alone() {
    for template in registry::templates() {
        let built = build(template.id());
        let mut guard = guard(&built);
        let skeleton = Arc::new(built.rig.skeleton.clone());
        let parts: Vec<String> =
            guard.proxy().body.iter().map(|p| format!("{:?} {:.0} mm", p.part, p.radius * 1e3)).collect();
        println!(
            "{}: {}, forearm {:.0} mm, hand {:.0} mm",
            template.id(),
            parts.join(", "),
            guard.proxy().forearm_radius * 1e3,
            guard.proxy().hand_radius * 1e3
        );

        // The bind pose (the arms' own hang) is clear and untouched.
        let bind = posed(&built, &built.rig.bind);
        let mut pose = bind.clone();
        let report = guard.apply(&skeleton, &mut pose, 1.0 / 60.0);
        println!("  bind pose: smallest gap {:.1} mm", report.before * 1e3);
        assert!(pose == bind && report.moved == [0.0; 2], "{}: {report:?}", template.id());

        // Elbows at the sides, forearms straight across the chest.
        let mut across = HumanoidPose::REST;
        for (upper, lower, along, flip) in [
            (B::LeftUpperArm, B::LeftLowerArm, Vec3::X, 1.0_f32),
            (B::RightUpperArm, B::RightLowerArm, Vec3::NEG_X, -1.0),
        ] {
            let u = Quat::from_rotation_arc(along, Vec3::new(0.15 * flip, -0.9, 0.35).normalize());
            let f = Quat::from_rotation_arc(along, Vec3::new(-flip, 0.1, 0.05).normalize());
            across.set(upper, u);
            across.set(lower, u.inverse() * f);
        }
        guard.reset();
        let mut pose = posed(&built, &across);
        let report = guard.apply(&skeleton, &mut pose, 1.0 / 60.0);
        println!(
            "  forearms across the chest: {:.1} mm before, {:.1} mm after, wrists moved {:.0} mm",
            report.before * 1e3,
            report.after * 1e3,
            report.moved[0] * 1e3
        );
        assert!(report.before < 0.0, "{}: the pose must go into the body: {report:?}", template.id());
        assert!(report.after >= -GuardConfig::default().tolerance - 1e-4, "{}: {report:?}", template.id());
    }
}
