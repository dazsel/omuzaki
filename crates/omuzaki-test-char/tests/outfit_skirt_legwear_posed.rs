//! The skirt and the legwear posed (#192, #193): the real parts built over the body on every template, skinned by
//! `omuzaki-rig` and bent the way a clip authored on the T-pose would.
//!
//! - **Legwear, knee bent 100 degrees:** nothing goes NaN and the left leg's tights keep at least 80 % of their volume (the
//!   tube's open top is closed with a fan for the measurement, the way the body's own cap closes it). They share the leg's
//!   weights, so a collapse here would be a weights mistake.
//! - **Skirt, hips flexed 80 degrees:** no skirt vertex that is outside a thigh at rest ends up inside it. The thighs are the
//!   body's posed `leg.L` and `leg.R` meshes, and a vertex is inside one by the parity of a ray's crossings.
//!
//! The skirt is built with a plain cloth: its plaid is painted by a rasteriser this crate does not have, and the pose does
//! not depend on it. Only the skirt vertices below the hip joint are watched: above it the body's own thigh is a dome inside the torso.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)] // test helpers: a failure is the report
#![allow(clippy::cast_possible_truncation)] // positions are f32 and welded at a micrometre
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry: a, b, c; triangles as slices of 3

use std::collections::HashMap;
use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone as B, HumanoidPose, Pose};
use omuzaki_char::outfit::legwear::{LAYER_LEGWEAR, LegwearParams};
use omuzaki_char::outfit::skirt::{LAYER_SKIRT, SkirtParams};
use omuzaki_char::template::registry;
use omuzaki_char_api::{
    BodyParams, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, JointRef,
    RegistryBuilder,
};
use omuzaki_core::glam::{DVec3, Mat4, Quat, Vec3};
use omuzaki_render_api::{Camera, Scene};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;

/// How far below the hip joint a skirt vertex must be to be watched: the body's thigh ends in a dome at the hip joint, which
/// swings out of the torso when the hip is flexed, and the waist is not on the thigh.
const HIP_MARGIN: f32 = 0.01;
/// The share of its volume the tights must keep with the knee bent 100 degrees.
const KNEE_KEEPS: f64 = 0.80;

fn build(template: &str) -> BuiltCharacter {
    let body = serde_json::to_value(BodyParams::default()).unwrap();
    let skirt = SkirtParams { plaid: None, ..SkirtParams::default() };
    let node = serde_json::json!({
        "name": "Posed",
        "template": template,
        "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80" },
        "parts": {
            "body": { "kind": "body", "params": body },
            "legwear": { "kind": "legwear", "layer": LAYER_LEGWEAR, "params": serde_json::to_value(LegwearParams::default()).unwrap() },
            "skirt": { "kind": "skirt", "layer": LAYER_SKIRT, "params": serde_json::to_value(skirt).unwrap() },
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

/// The vertices of the mesh called `name`, skinned by `matrices`.
fn posed(built: &BuiltCharacter, name: &str, matrices: &[Mat4]) -> (Vec<Vec3>, Vec<u32>) {
    let m = &built.meshes.iter().find(|m| m.mesh.name == name).unwrap().mesh;
    let g = &m.geometry;
    let bone_of: Vec<usize> = m.joint_bones.iter().map(|j| built.rig.bone(j).unwrap().index()).collect();
    let verts = (0..g.positions.len())
        .map(|v| {
            (0..4)
                .map(|k| {
                    g.weights[v][k] * matrices[bone_of[usize::from(g.joints[v][k])]].transform_point3(g.positions[v])
                })
                .sum()
        })
        .collect();
    (verts, g.indices.clone())
}

/// The bind pose with `delta` on `bone` and, when `both`, on its mirror image (a rotation about the body's x axis is its
/// own mirror image).
fn bent(built: &BuiltCharacter, bone: B, delta: Quat, both: bool) -> HumanoidPose {
    let mut pose = built.rig.bind;
    pose.set(bone, built.rig.bind.get(bone) * delta);
    if both {
        pose.set(bone.mirrored(), built.rig.bind.get(bone.mirrored()) * delta);
    }
    pose
}

/// The signed volume of the left leg's tights in `verts`, the open top closed with a fan.
fn left_volume(verts: &[Vec3], indices: &[u32]) -> f64 {
    let half = verts.len() / 2;
    let tris: Vec<[u32; 3]> = indices
        .chunks_exact(3)
        .map(|t| [t[0], t[1], t[2]])
        .filter(|t| t.iter().all(|&i| (i as usize) < half))
        .collect();
    // Vertices at one place are one (the grid repeats its seam column and collapses its poles).
    let mut weld: HashMap<[i64; 3], u32> = HashMap::new();
    let key = |p: Vec3| [(p.x * 1e6).round() as i64, (p.y * 1e6).round() as i64, (p.z * 1e6).round() as i64];
    let id = |i: u32, weld: &mut HashMap<[i64; 3], u32>| *weld.entry(key(verts[i as usize])).or_insert(i);
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    let welded: Vec<[u32; 3]> = tris.iter().map(|t| t.map(|i| id(i, &mut weld))).collect();
    for t in &welded {
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            continue;
        }
        for k in 0..3 {
            *edges.entry((t[k], t[(k + 1) % 3])).or_insert(0) += 1;
        }
    }
    // A boundary edge a -> b has no b -> a; its cap triangle is (b, a, centroid of its loop).
    let open: Vec<(u32, u32)> = edges.keys().copied().filter(|&(a, b)| !edges.contains_key(&(b, a))).collect();
    let mut by_start: HashMap<u32, u32> = HashMap::new();
    for &(a, b) in &open {
        by_start.insert(a, b);
    }
    let mut seen = std::collections::HashSet::new();
    let mut volume = 0.0;
    let at = |i: u32| verts[i as usize].as_dvec3();
    let tet = |a: DVec3, b: DVec3, c: DVec3| a.dot(b.cross(c)) / 6.0;
    for t in &welded {
        volume += tet(at(t[0]), at(t[1]), at(t[2]));
    }
    for &(start, _) in &open {
        if seen.contains(&start) {
            continue;
        }
        let mut ring = vec![start];
        seen.insert(start);
        let mut next = by_start[&start];
        while next != start && seen.insert(next) {
            ring.push(next);
            next = by_start[&next];
        }
        let centre = ring.iter().map(|&i| at(i)).sum::<DVec3>() / ring.len() as f64;
        for k in 0..ring.len() {
            let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
            volume += tet(at(b), at(a), centre);
        }
    }
    volume
}

#[test]
fn the_tights_keep_their_volume_with_the_knee_bent_100_degrees() {
    let deg = |d: f32| d * std::f32::consts::PI / 180.0;
    for t in registry::templates() {
        let built = build(t.id());
        let rest = {
            let (v, i) = posed(&built, "legwear-legs", &matrices(&built, &built.rig.bind));
            left_volume(&v, &i)
        };
        assert!(rest > 0.0, "{}: the tights at rest have a positive volume ({rest})", t.id());
        for (label, bone, delta, floor) in [
            ("knee 100", B::LeftLowerLeg, Quat::from_rotation_x(deg(100.0)), KNEE_KEEPS),
            ("hip 80", B::LeftUpperLeg, Quat::from_rotation_x(-deg(80.0)), KNEE_KEEPS),
        ] {
            let (v, i) = posed(&built, "legwear-legs", &matrices(&built, &bent(&built, bone, delta, false)));
            assert!(v.iter().all(|p| p.is_finite()), "{} {label}: a vertex went NaN", t.id());
            let kept = left_volume(&v, &i) / rest;
            eprintln!("{:8} tights {label:9}: {:.1} % of the volume kept", t.id(), 100.0 * kept);
            assert!(
                kept >= floor,
                "{} {label}: the tights keep {:.1} % (floor {:.0} %)",
                t.id(),
                100.0 * kept,
                100.0 * floor
            );
        }
    }
}

/// Whether `p` is inside the closed mesh `(verts, indices)`: the parity of the crossings of a ray from it.
fn inside(p: Vec3, verts: &[Vec3], indices: &[u32]) -> bool {
    // An irrational-looking direction, so a ray never runs along an edge of a regular grid.
    let d = DVec3::new(0.577_350_3, 0.301_511_3, 0.758_94).normalize();
    let o = p.as_dvec3();
    let mut crossings = 0;
    for t in indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| verts[i as usize].as_dvec3());
        let (e1, e2) = (b - a, c - a);
        let h = d.cross(e2);
        let det = e1.dot(h);
        if det.abs() < 1e-14 {
            continue;
        }
        let f = 1.0 / det;
        let s = o - a;
        let u = f * s.dot(h);
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = s.cross(e1);
        let v = f * d.dot(q);
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        if f * e2.dot(q) > 1e-12 {
            crossings += 1;
        }
    }
    crossings % 2 == 1
}

#[test]
fn the_skirt_stays_outside_the_thighs_with_both_hips_flexed_80_degrees() {
    // Sitting down: both thighs swing forward and up. A skirt that rides on the thighs goes with them; one that stayed
    // behind would be cut through by them.
    let deg = |d: f32| d * std::f32::consts::PI / 180.0;
    let mut failures = 0;
    for t in registry::templates() {
        let built = build(t.id());
        let bind = matrices(&built, &built.rig.bind);
        let hip = built.rig.inverse_bind()[built.rig.bone(&JointRef::Humanoid(B::LeftUpperLeg)).unwrap().index()]
            .inverse()
            .transform_point3(Vec3::ZERO);
        let (rest_skirt, _) = posed(&built, "skirt-pleats", &bind);
        let pose = matrices(&built, &bent(&built, B::LeftUpperLeg, Quat::from_rotation_x(-deg(80.0)), true));
        let (skirt, _) = posed(&built, "skirt-pleats", &pose);
        assert!(skirt.iter().all(|p| p.is_finite()), "{}: a skirt vertex went NaN", t.id());
        for mesh in ["leg.L", "leg.R"] {
            let (rest_leg, rest_leg_i) = posed(&built, mesh, &bind);
            let (leg, leg_i) = posed(&built, mesh, &pose);
            let lo = leg.iter().fold(Vec3::splat(f32::INFINITY), |m, p| m.min(*p));
            let hi = leg.iter().fold(Vec3::splat(f32::NEG_INFINITY), |m, p| m.max(*p));
            let watched: Vec<usize> = (0..rest_skirt.len())
                .filter(|&v| rest_skirt[v].y < hip.y - HIP_MARGIN && !inside(rest_skirt[v], &rest_leg, &rest_leg_i))
                .collect();
            assert!(watched.len() > rest_skirt.len() / 3, "{}: most of the skirt is outside the thigh at rest", t.id());
            let through: Vec<usize> = watched
                .iter()
                .copied()
                .filter(|&v| skirt[v].cmpge(lo).all() && skirt[v].cmple(hi).all() && inside(skirt[v], &leg, &leg_i))
                .collect();
            let mut rows = std::collections::BTreeMap::new();
            for &v in &through {
                *rows.entry(v / 145).or_insert(0) += 1;
            }
            eprintln!(
                "{:8} skirt hips 80 vs {mesh}: {} of {} vertices inside, by grid row {rows:?}, first {:?}",
                t.id(),
                through.len(),
                watched.len(),
                through.first().map(|&v| (v, rest_skirt[v], skirt[v]))
            );
            failures += through.len();
        }
    }
    assert_eq!(failures, 0, "skirt vertices inside a thigh");
}
