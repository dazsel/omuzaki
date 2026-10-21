//! The shirt posed (#190): the real `body` and `shirt` kinds through the reference builder on every template, skinned by
//! `omuzaki-rig`, with the left arm raised 150 degrees and the elbow bent 100. The sleeve must stay clear of the arm
//! inside it (no sleeve vertex inside the arm) and close to it (none further than three eases from the arm's surface),
//! and nothing may go NaN.
//!
//! The poses are the bind pose with one more rotation on the bone, in the bone's own frame, as `char_body_posed.rs` does.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)] // test helpers: a failure is the report
#![allow(clippy::many_single_char_names, clippy::similar_names, clippy::chunks_exact_to_as_chunks)] // geometry: a, b, c; rest and now

use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone as B, HumanoidPose, Pose};
use omuzaki_char::template::registry;
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, RegistryBuilder,
};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_render_api::{Camera, Scene};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;
use serde_json::json;

/// The shirt's ease, metres (the default).
const EASE: f64 = 0.004;

fn build(template: &str) -> BuiltCharacter {
    let node = json!({
        "name": "Posed",
        "template": template,
        "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80" },
        "parts": {
            "body": { "kind": "body", "params": {} },
            "shirt": { "kind": "shirt", "layer": 20, "params": {} }
        }
    });
    let def = CharacterDef::from_node(&node).unwrap();
    RegistryBuilder
        .build(&CharId::new("posed").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

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

/// The skinned positions of mesh `name`, and the weight each vertex gives the left arm's bones.
fn posed(built: &BuiltCharacter, name: &str, matrices: &[Mat4]) -> Vec<(Vec3, f32)> {
    let m = &built.meshes.iter().find(|m| m.mesh.name == name).unwrap_or_else(|| panic!("no mesh `{name}`")).mesh;
    let g = &m.geometry;
    let bone_of: Vec<usize> = m.joint_bones.iter().map(|j| built.rig.bone(j).unwrap().index()).collect();
    let arm: Vec<bool> = m
        .joint_bones
        .iter()
        .map(|j| matches!(j, omuzaki_char_api::JointRef::Humanoid(B::LeftUpperArm | B::LeftLowerArm)))
        .collect();
    (0..g.positions.len())
        .map(|v| {
            let mut p = Vec3::ZERO;
            let mut on_arm = 0.0;
            for k in 0..4 {
                let (j, w) = (usize::from(g.joints[v][k]), g.weights[v][k]);
                p += w * matrices[bone_of[j]].transform_point3(g.positions[v]);
                if arm[j] {
                    on_arm += w;
                }
            }
            (p, on_arm)
        })
        .collect()
}

#[test]
fn the_sleeve_keeps_its_clearance_to_the_raised_bent_arm_on_every_template() {
    use std::f32::consts::PI;
    let deg = |d: f32| d * PI / 180.0;
    for t in registry::templates() {
        let built = build(t.id());
        let mut pose = built.rig.bind;
        pose.set(B::LeftUpperArm, built.rig.bind.get(B::LeftUpperArm) * Quat::from_rotation_z(deg(150.0)));
        pose.set(B::LeftLowerArm, built.rig.bind.get(B::LeftLowerArm) * Quat::from_rotation_y(-deg(100.0)));
        let (at_bind, bent) = (matrices(&built, &built.rig.bind), matrices(&built, &pose));
        let sleeve_bind = posed(&built, "shirt-shirt", &at_bind);
        let sleeve = posed(&built, "shirt-shirt", &bent);
        assert!(sleeve.iter().all(|(p, _)| p.is_finite()), "{}: a vertex went NaN", t.id());
        // The arm's own triangles, at rest and posed: a sleeve vertex's signed distance to the nearest one (negative inside).
        let arm_mesh = &built.meshes.iter().find(|m| m.mesh.name == "arm.L").unwrap().mesh.geometry;
        let triangles = |positions: &[Vec3]| -> Vec<[Vec3; 3]> {
            arm_mesh.indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| positions[t[k] as usize])).collect()
        };
        let arm_posed = |matrices: &[Mat4]| {
            triangles(&posed(&built, "arm.L", matrices).iter().map(|(p, _)| *p).collect::<Vec<_>>())
        };
        let (arm_at_bind, arm_bent) = (arm_posed(&at_bind), arm_posed(&bent));
        let signed = |p: Vec3, arm: &[[Vec3; 3]]| -> f32 {
            let mut best = (f32::INFINITY, 1.0_f32);
            for [a, b, c] in arm {
                let q = closest_on_triangle(p, *a, *b, *c);
                let d = p.distance(q);
                if d < best.0 {
                    let n = (*b - *a).cross(*c - *a);
                    best = (d, if (p - q).dot(n) < 0.0 { -1.0 } else { 1.0 });
                }
            }
            best.0 * best.1
        };
        // The vertices that follow the arm alone (the shoulder is shared with the chest, which stays).
        let (mut closest, mut stand_off) = (f32::INFINITY, f32::NEG_INFINITY);
        let mut worst = Vec3::ZERO;
        for (k, (p, on_arm)) in sleeve.iter().enumerate() {
            if *on_arm < 0.999 || sleeve_bind[k].0.x < 0.0 {
                continue;
            }
            let rest = signed(sleeve_bind[k].0, &arm_at_bind).abs();
            let now = signed(*p, &arm_bent);
            if now < closest {
                worst = sleeve_bind[k].0;
            }
            closest = closest.min(now);
            stand_off = stand_off.max(now - rest);
        }
        println!(
            "{:8} posed: sleeve {:.1} mm outside the arm at the least (at rest {:?}), and at most {:.1} mm further out than at rest",
            t.id(),
            closest * 1e3,
            worst,
            stand_off * 1e3
        );
        // Clear of the arm inside it.
        assert!(closest >= 0.0, "{}: the sleeve is inside the arm by {}", t.id(), -closest);
        // Close to it: no vertex is further from the arm's surface than three eases more than it is at rest.
        assert!(
            f64::from(stand_off) <= 3.0 * EASE,
            "{}: the sleeve stands off the arm by {stand_off} more than at rest",
            t.id()
        );
    }
}

/// The point of triangle `abc` nearest `p` (Ericson, Real-Time Collision Detection 5.1.5).
fn closest_on_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}
