//! The body kind posed: the real `body` part, built through the reference builder on every template, skinned by
//! `omuzaki-rig` and bent at the elbow (90 degrees), the knee (100), the hip (80) and the shoulder (raised 150), the way a
//! clip authored on the T-pose would. Nothing may go NaN, and the limb that bends must keep most of its volume: a skin
//! that collapses at a joint (a candy-wrapper elbow) loses volume there. The shoulder, bent furthest, keeps the least.
//!
//! The poses are the bind pose with one more rotation on the bone, in the bone's own frame: the arm hangs at the template's
//! A-pose, so "raise 150 degrees" starts from the hang.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)] // test helpers: a failure is the report
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry: a, b, c; triangles as slices of 3

use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone as B, HumanoidPose, Pose};
use omuzaki_char::template::registry;
use omuzaki_char_api::{
    BodyParams, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, JointRef,
    RegistryBuilder,
};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_render_api::{Camera, Scene};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;

fn build(template: &str) -> BuiltCharacter {
    let body = serde_json::to_value(BodyParams::default()).unwrap();
    let node = serde_json::json!({
        "name": "Posed",
        "template": template,
        "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80" },
        "parts": { "body": { "kind": "body", "params": body } }
    });
    let def = CharacterDef::from_node(&node).unwrap();
    RegistryBuilder
        .build(&CharId::new("posed").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

/// The skinning matrices of `pose`, as a frame pushes them.
fn matrices(built: &BuiltCharacter, pose: &HumanoidPose) -> Vec<Mat4> {
    let shared = RigSkeleton::with_inverse_bind(built.rig.skeleton.clone(), built.rig.inverse_bind()).unwrap();
    let humanoid = HumanoidRig::new(Arc::clone(&shared));
    let mut skinner = Skinner::new(shared);
    let mut local = Pose::IDENTITY;
    humanoid.apply_humanoid(pose, &mut local);
    let mut scene = Scene::new(Camera::default());
    skinner.push(&local, &mut scene.joints);
    scene.joints
}

/// The signed volume of a closed mesh skinned by `matrices`, and whether every vertex stayed finite.
fn skinned_volume(built: &BuiltCharacter, mesh: &str, matrices: &[Mat4]) -> (f64, bool) {
    let m = &built.meshes.iter().find(|m| m.mesh.name == mesh).unwrap().mesh;
    let g = &m.geometry;
    let bone_of: Vec<usize> = m.joint_bones.iter().map(|j| built.rig.bone(j).unwrap().index()).collect();
    let posed: Vec<Vec3> = (0..g.positions.len())
        .map(|v| {
            (0..4)
                .map(|k| {
                    g.weights[v][k] * matrices[bone_of[usize::from(g.joints[v][k])]].transform_point3(g.positions[v])
                })
                .sum()
        })
        .collect();
    let finite = posed.iter().all(|p| p.is_finite());
    let volume = g
        .indices
        .chunks_exact(3)
        .map(|t| {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| posed[i as usize].as_dvec3());
            a.dot(b.cross(c)) / 6.0
        })
        .sum();
    (volume, finite)
}

/// The bind pose with `delta` applied to `bone` and its mirror image (reflected the way `HumanoidPose::mirrored` does).
fn bent(built: &BuiltCharacter, bone: B, delta: Quat) -> HumanoidPose {
    let mut pose = built.rig.bind;
    pose.set(bone, built.rig.bind.get(bone) * delta);
    let mirrored = Quat::from_xyzw(delta.x, -delta.y, -delta.z, delta.w);
    pose.set(bone.mirrored(), built.rig.bind.get(bone.mirrored()) * mirrored);
    pose
}

#[test]
fn bent_joints_keep_their_volume_on_every_template() {
    use std::f32::consts::PI;
    let deg = |d: f32| d * PI / 180.0;
    // (joint, bone, the rotation, the mesh whose volume is checked, the share of it that must stay)
    let cases = [
        ("elbow 90", B::LeftLowerArm, Quat::from_rotation_y(-deg(90.0)), "arm.L", 0.85),
        ("knee 100", B::LeftLowerLeg, Quat::from_rotation_x(deg(100.0)), "leg.L", 0.85),
        ("hip 80", B::LeftUpperLeg, Quat::from_rotation_x(-deg(80.0)), "leg.L", 0.95),
        // The whole arm is measured, not just the joint. The arm's hand-over from the shoulder bone to the upper arm is
        // narrow (`body/mesh.rs`), so a raised arm blends a still clavicle into only its root: soft-sd, mini, standard
        // and tall keep 59.5, 62.7, 71.7 and 75.1 % without that, and 84.1, 84.0, 85.6 and 87.7 % with it. The floor
        // sits 4 points under the worst template. The elbow, knee and hip figures did not move.
        ("shoulder 150", B::LeftUpperArm, Quat::from_rotation_z(deg(150.0)), "arm.L", 0.80),
    ];
    for t in registry::templates() {
        let built = build(t.id());
        let at_bind = matrices(&built, &built.rig.bind);
        for (label, bone, delta, mesh, floor) in cases {
            let rest = skinned_volume(&built, mesh, &at_bind);
            assert!(rest.1 && rest.0 > 0.0, "{} {label}: the bind pose is sound", t.id());
            let (volume, finite) = skinned_volume(&built, mesh, &matrices(&built, &bent(&built, bone, delta)));
            let kept = volume / rest.0;
            eprintln!("{:8} {label:13} {mesh}: {:.1} % of the volume kept", t.id(), 100.0 * kept);
            assert!(finite, "{} {label}: a vertex went NaN", t.id());
            assert!(
                kept >= floor,
                "{} {label}: {mesh} keeps {:.1} % of its volume (floor {:.0} %)",
                t.id(),
                100.0 * kept,
                100.0 * floor
            );
        }
    }
}

#[test]
fn the_body_at_bind_is_the_built_mesh() {
    // Skinning at the bind pose is the identity: the rig is bound in the pose the body was lofted in.
    for t in registry::templates() {
        let built = build(t.id());
        let at_bind = matrices(&built, &built.rig.bind);
        for m in &built.meshes {
            let g = &m.mesh.geometry;
            let bone_of: Vec<usize> =
                m.mesh.joint_bones.iter().map(|j: &JointRef| built.rig.bone(j).unwrap().index()).collect();
            let mut worst = 0.0_f32;
            for v in 0..g.positions.len() {
                let p: Vec3 = (0..4)
                    .map(|k| {
                        g.weights[v][k] * at_bind[bone_of[usize::from(g.joints[v][k])]].transform_point3(g.positions[v])
                    })
                    .sum();
                worst = worst.max(p.distance(g.positions[v]));
            }
            assert!(worst < 1e-5, "{} {}: skinning at bind moves a vertex by {worst}", t.id(), m.mesh.name);
        }
    }
}
