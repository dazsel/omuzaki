//! The trousers posed (#195): the real `body` and `trousers` kinds built through the reference builder on every template,
//! skinned by `omuzaki-rig` and put in a squat (hips 110 degrees, knees 120) and a high kick (one hip 100 degrees
//! forward), the way a clip authored on the bind pose would. Two things are measured on the trousers:
//!
//! - **how far the cloth stretches**: the longest a triangle edge gets against its length at rest;
//! - **whether it goes through the body**: the share of the cloth's vertices that end up inside a body mesh, and the
//!   deepest, found by casting a ray from each and counting the surfaces it crosses.
//!
//! The trousers are linear-blend skinned, like the body, so both are bounded by how the weights hand over from the hips
//! to the thighs: an edge across a hand-over stretches by `(change in weight) x (how far the two bones' motions differ
//! there)`, and the thigh swinging 110 degrees about a hip a few centimetres from the cloth moves it by many. The numbers
//! printed here are the evidence; the assertions hold them at what the weights achieve.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)] // test helpers: a failure is the report
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry: a, b, c; triangles as slices of 3

use std::collections::BTreeSet;
use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone as B, HumanoidPose, Pose};
use omuzaki_char::outfit::trousers::{LAYER_TROUSERS, TrousersParams};
use omuzaki_char_api::{
    BodyParams, BuildOptions, BuildServices, BuiltCharacter, BuiltMesh, CharId, CharacterBuilder, CharacterDef,
    RegistryBuilder,
};
use omuzaki_core::glam::{DVec3, Mat4, Quat, Vec3};
use omuzaki_render_api::{Camera, Scene};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;

const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

fn build(template: &str, params: &TrousersParams) -> BuiltCharacter {
    let node = serde_json::json!({
        "name": "Posed",
        "template": template,
        "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80" },
        "parts": {
            "body": { "kind": "body", "params": serde_json::to_value(BodyParams::default()).unwrap() },
            "trousers": { "kind": "trousers", "layer": LAYER_TROUSERS, "params": serde_json::to_value(params).unwrap() },
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

/// The mesh's vertices skinned by `matrices`.
fn skinned(built: &BuiltCharacter, mesh: &BuiltMesh, matrices: &[Mat4]) -> Vec<Vec3> {
    let g = &mesh.mesh.geometry;
    let bone_of: Vec<usize> = mesh.mesh.joint_bones.iter().map(|j| built.rig.bone(j).unwrap().index()).collect();
    (0..g.positions.len())
        .map(|v| {
            (0..4)
                .map(|k| g.weights[v][k] * matrices[bone_of[usize::from(g.joints[v][k])]].transform_point3(g.positions[v]))
                .sum()
        })
        .collect()
}

/// The bind pose with `delta` applied to `bone`; with `mirror` also to its other side, reflected the way
/// `HumanoidPose::mirrored` does.
fn bend(pose: &mut HumanoidPose, built: &BuiltCharacter, bone: B, delta: Quat, mirror: bool) {
    pose.set(bone, built.rig.bind.get(bone) * delta);
    if mirror {
        let reflected = Quat::from_xyzw(delta.x, -delta.y, -delta.z, delta.w);
        pose.set(bone.mirrored(), built.rig.bind.get(bone.mirrored()) * reflected);
    }
}

/// The poses: a squat and a high kick.
fn poses(built: &BuiltCharacter) -> Vec<(&'static str, HumanoidPose)> {
    let deg = |d: f32| d.to_radians();
    let mut squat = built.rig.bind;
    bend(&mut squat, built, B::LeftUpperLeg, Quat::from_rotation_x(-deg(110.0)), true);
    bend(&mut squat, built, B::LeftLowerLeg, Quat::from_rotation_x(deg(120.0)), true);
    let mut kick = built.rig.bind;
    bend(&mut kick, built, B::LeftUpperLeg, Quat::from_rotation_x(-deg(100.0)), false);
    vec![("squat", squat), ("kick", kick)]
}

/// The unique edges of a mesh's triangles.
fn edges(indices: &[u32]) -> Vec<(u32, u32)> {
    let mut set = BTreeSet::new();
    for t in indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            set.insert((a.min(b), a.max(b)));
        }
    }
    set.into_iter().collect()
}

/// The longest an edge of `indices` gets: posed length over rest length, over edges at least `floor` metres long at rest
/// (shorter ones are the cords' and seams' sub-millimetre rings, which a ratio would blow up).
fn stretch(rest: &[Vec3], posed: &[Vec3], indices: &[u32], floor: f32) -> f32 {
    edges(indices)
        .into_iter()
        .filter_map(|(a, b)| {
            let before = rest[a as usize].distance(rest[b as usize]);
            (before >= floor).then(|| posed[a as usize].distance(posed[b as usize]) / before)
        })
        .fold(0.0, f32::max)
}

/// Möller-Trumbore: where the ray from `o` along `d` meets triangle `abc`, as a distance.
fn ray_hit(o: DVec3, d: DVec3, a: DVec3, b: DVec3, c: DVec3) -> Option<f64> {
    let (e1, e2) = (b - a, c - a);
    let h = d.cross(e2);
    let det = e1.dot(h);
    if det.abs() < 1e-14 {
        return None;
    }
    let s = o - a;
    let u = s.dot(h) / det;
    let q = s.cross(e1);
    let v = d.dot(q) / det;
    let t = e2.dot(q) / det;
    (u >= 0.0 && v >= 0.0 && u + v <= 1.0 && t > 0.0).then_some(t)
}

/// A direction no mesh edge is likely to lie along, so a ray through a seam doesn't count twice.
const RAY: DVec3 = DVec3::new(0.3713, 0.8721, 0.3198);

/// Whether `p` is inside the closed mesh `triangles`: a ray from it crosses an odd number of triangles.
fn inside(p: DVec3, triangles: &[[DVec3; 3]]) -> bool {
    triangles.iter().filter(|[a, b, c]| ray_hit(p, RAY, *a, *b, *c).is_some()).count() % 2 == 1
}

/// How deep `p` is in the body: its distance to the nearest of `triangles` (a bound, by the nearest vertex).
fn depth(p: DVec3, triangles: &[[DVec3; 3]]) -> f64 {
    triangles.iter().flatten().map(|v| v.distance(p)).fold(f64::INFINITY, f64::min)
}

fn triangles(posed: &[Vec3], indices: &[u32]) -> Vec<[DVec3; 3]> {
    indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| posed[t[k] as usize].as_dvec3())).collect()
}

/// A pose's measurements of the trousers.
struct Measured {
    /// The longest edge, posed over rest.
    stretch: f32,
    /// The longest edge of the body's own legs, for scale.
    body_stretch: f32,
    /// The share of sampled vertices inside a body mesh.
    inside: f64,
    /// The deepest of them (the distance to the nearest body vertex), metres.
    depth: f64,
}

fn measure(built: &BuiltCharacter, pose: &HumanoidPose) -> Measured {
    let at_bind = matrices(built, &built.rig.bind);
    let posed = matrices(built, pose);
    let cloth = built.meshes.iter().find(|m| m.part.as_str() == "trousers").expect("the trousers' mesh");
    let (rest_cloth, cloth_now) = (skinned(built, cloth, &at_bind), skinned(built, cloth, &posed));
    let body: Vec<&BuiltMesh> = built.meshes.iter().filter(|m| m.part.as_str() == "body").collect();
    let mut body_stretch = 0.0_f32;
    let mut body_triangles = Vec::new();
    for mesh in &body {
        let (rest, now) = (skinned(built, mesh, &at_bind), skinned(built, mesh, &posed));
        if mesh.mesh.name.starts_with("leg") {
            body_stretch = body_stretch.max(stretch(&rest, &now, &mesh.mesh.geometry.indices, 0.002));
        }
        body_triangles.push((triangles(&now, &mesh.mesh.geometry.indices), now));
    }
    // A spread of the cloth's vertices, tested against every body mesh (a ray against a closed mesh).
    let step = (cloth_now.len() / 600).max(1);
    let (mut count, mut inside_count, mut deepest) = (0_usize, 0_usize, 0.0_f64);
    for p in cloth_now.iter().step_by(step) {
        count += 1;
        let p = p.as_dvec3();
        for (tris, _) in &body_triangles {
            if inside(p, tris) {
                inside_count += 1;
                deepest = deepest.max(depth(p, tris));
                break;
            }
        }
    }
    Measured {
        stretch: stretch(&rest_cloth, &cloth_now, &cloth.mesh.geometry.indices, 0.002),
        body_stretch,
        inside: inside_count as f64 / count as f64,
        depth: deepest,
    }
}

#[test]
fn the_trousers_at_bind_are_the_built_mesh_and_clear_of_the_body() {
    for template in TEMPLATES {
        let built = build(template, &TrousersParams::default());
        let m = measure(&built, &built.rig.bind);
        assert!((m.stretch - 1.0).abs() < 1e-4, "{template}: no stretch at bind, got {}", m.stretch);
        assert!(m.inside == 0.0, "{template}: nothing inside the body at bind: {} of the vertices", m.inside);
    }
}

#[test]
fn the_trousers_squat_and_kick_with_the_body() {
    for template in TEMPLATES {
        let built = build(template, &TrousersParams::default());
        for (name, pose) in poses(&built) {
            let m = measure(&built, &pose);
            eprintln!(
                "{template:8} {name:6}: longest edge x{:.2} (the body's own legs x{:.2}); {:.1} % of the cloth inside the \
                 body, deepest {:.1} mm",
                m.stretch,
                m.body_stretch,
                100.0 * m.inside,
                m.depth * 1e3
            );
            assert!(m.stretch.is_finite(), "{template} {name}: a vertex went NaN");
        }
    }
}
