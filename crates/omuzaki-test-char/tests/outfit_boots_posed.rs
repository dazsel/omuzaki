//! The `boots` garment posed: the real `boots` part built over the real `body` on every template, skinned by
//! `omuzaki-rig` and bent the way a stride bends the foot (ankle 25 degrees, toes 30), both ways. The upper must keep
//! its volume, and no ring of it may collapse where the foot hands over to the shaft at the ankle (a candy-wrapper
//! ankle loses area there first): at most 15 % of either may go for either joint on its own, and (the two bent together
//! in opposite senses being the harshest case) [`COMBINED_LOSS`] for both.
//!
//! The poses are the bind pose with one more rotation on the bone, in the bone's own frame, as `char_body_posed` does.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)] // test helpers: a failure is the report
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry: a, b, c; triangles as slices of 3

use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone as B, HumanoidPose, Pose};
use omuzaki_char::outfit::boots::LAYER_FOOTWEAR;
use omuzaki_char::template::registry;
use omuzaki_char_api::{
    BodyParams, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, RegistryBuilder,
};
use omuzaki_core::glam::{DVec3, Mat4, Quat, Vec3};
use omuzaki_render_api::{Camera, Scene};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;

/// Rows and columns of the upper at `Quality::Live`: 27 rows of 49.
const ROWS: usize = 27;
const COLS: usize = 49;
/// The most of the upper's volume or of any ring's area a stride may take.
const LOSS: f64 = 0.15;
/// The most both joints bent together may take.
const COMBINED_LOSS: f64 = 0.20;

fn build(template: &str) -> BuiltCharacter {
    let body = serde_json::to_value(BodyParams::default()).unwrap();
    let node = serde_json::json!({
        "name": "Posed",
        "template": template,
        "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80" },
        "parts": {
            "body": { "kind": "body", "params": body },
            "boots": { "kind": "boots", "layer": LAYER_FOOTWEAR, "params": {} }
        }
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

/// The left upper's vertices skinned by `matrices`.
fn posed_upper(built: &BuiltCharacter, matrices: &[Mat4]) -> Vec<DVec3> {
    let m = &built.meshes.iter().find(|m| m.mesh.name == "boots-leather").expect("the boots' leather").mesh;
    let g = &m.geometry;
    let bone_of: Vec<usize> = m.joint_bones.iter().map(|j| built.rig.bone(j).unwrap().index()).collect();
    (0..ROWS * COLS)
        .map(|v| {
            let p: Vec3 = (0..4)
                .map(|k| {
                    g.weights[v][k] * matrices[bone_of[usize::from(g.joints[v][k])]].transform_point3(g.positions[v])
                })
                .sum();
            assert!(p.is_finite(), "vertex {v} went NaN");
            p.as_dvec3()
        })
        .collect()
}

/// The area of the ring of row `row`: the length of its area vector, so a ring that tilts or bends keeps its area.
fn ring_area(points: &[DVec3], row: usize) -> f64 {
    let ring = &points[row * COLS..(row + 1) * COLS - 1];
    let sum: DVec3 = (0..ring.len()).map(|k| ring[k].cross(ring[(k + 1) % ring.len()])).sum();
    sum.length() / 2.0
}

/// The volume the upper encloses once its top and bottom rings are closed with fans to their centres.
fn enclosed_volume(points: &[DVec3]) -> f64 {
    let centre = |row: usize| points[row * COLS..(row + 1) * COLS - 1].iter().sum::<DVec3>() / (COLS - 1) as f64;
    let mut volume = 0.0;
    let mut tri = |a: DVec3, b: DVec3, c: DVec3| volume += a.dot(b.cross(c)) / 6.0;
    for i in 0..ROWS - 1 {
        for j in 0..COLS - 1 {
            let (a, b) = (points[i * COLS + j], points[i * COLS + j + 1]);
            let (c, d) = (points[(i + 1) * COLS + j], points[(i + 1) * COLS + j + 1]);
            tri(a, c, b);
            tri(b, c, d);
        }
    }
    for (row, sign) in [(0, 1.0), (ROWS - 1, -1.0)] {
        let o = centre(row);
        for j in 0..COLS - 1 {
            let (a, b) = (points[row * COLS + j], points[row * COLS + j + 1]);
            if sign > 0.0 { tri(o, b, a) } else { tri(o, a, b) }
        }
    }
    volume.abs()
}

/// The bind pose with each rotation applied to its bone and the mirror image to the other foot.
fn bent(built: &BuiltCharacter, deltas: &[(B, Quat)]) -> HumanoidPose {
    let mut pose = built.rig.bind;
    for &(bone, delta) in deltas {
        pose.set(bone, built.rig.bind.get(bone) * delta);
        let mirrored = Quat::from_xyzw(delta.x, -delta.y, -delta.z, delta.w);
        pose.set(bone.mirrored(), built.rig.bind.get(bone.mirrored()) * mirrored);
    }
    pose
}

#[test]
fn a_stride_does_not_collapse_the_boot_at_the_ankle() {
    let deg = |d: f32| d.to_radians();
    let strides = [
        ("ankle +25", vec![(B::LeftFoot, Quat::from_rotation_x(deg(25.0)))]),
        ("ankle -25", vec![(B::LeftFoot, Quat::from_rotation_x(-deg(25.0)))]),
        ("toes +30", vec![(B::LeftToes, Quat::from_rotation_x(deg(30.0)))]),
        ("toes -30", vec![(B::LeftToes, Quat::from_rotation_x(-deg(30.0)))]),
        (
            "ankle +25, toes -30",
            vec![(B::LeftFoot, Quat::from_rotation_x(deg(25.0))), (B::LeftToes, Quat::from_rotation_x(-deg(30.0)))],
        ),
        (
            "ankle -25, toes +30",
            vec![(B::LeftFoot, Quat::from_rotation_x(-deg(25.0))), (B::LeftToes, Quat::from_rotation_x(deg(30.0)))],
        ),
    ];
    for t in registry::templates() {
        let built = build(t.id());
        let rest = posed_upper(&built, &matrices(&built, &built.rig.bind));
        let rest_volume = enclosed_volume(&rest);
        assert!(rest_volume > 0.0);
        for (label, deltas) in &strides {
            let loss = if deltas.len() > 1 { COMBINED_LOSS } else { LOSS };
            let posed = posed_upper(&built, &matrices(&built, &bent(&built, deltas)));
            let kept = enclosed_volume(&posed) / rest_volume;
            let worst =
                (0..ROWS).map(|row| ring_area(&posed, row) / ring_area(&rest, row)).fold(f64::INFINITY, f64::min);
            eprintln!(
                "{:8} {label:22} volume kept {:.1} %, thinnest ring {:.1} %",
                t.id(),
                100.0 * kept,
                100.0 * worst
            );
            assert!(kept >= 1.0 - loss, "{} {label}: the upper keeps {:.1} % of its volume", t.id(), 100.0 * kept);
            assert!(worst >= 1.0 - loss, "{} {label}: a ring keeps {:.1} % of its area", t.id(), 100.0 * worst);
        }
    }
}

#[test]
fn the_boots_at_bind_are_the_built_mesh() {
    for t in registry::templates() {
        let built = build(t.id());
        let at_bind = matrices(&built, &built.rig.bind);
        let m = &built.meshes.iter().find(|m| m.mesh.name == "boots-leather").unwrap().mesh;
        let posed = posed_upper(&built, &at_bind);
        let worst = (0..ROWS * COLS).map(|v| posed[v].distance(m.geometry.positions[v].as_dvec3())).fold(0.0, f64::max);
        assert!(worst < 1e-5, "{}: skinning at bind moves a vertex by {worst}", t.id());
    }
}
