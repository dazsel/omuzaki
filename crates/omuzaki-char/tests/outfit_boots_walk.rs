//! Boots in a walking pose (#326): the ankle turned 25 degrees either way and the toes bent 30 degrees, the boot's
//! leather keeps at least 85% of its volume at rest (no collapse at the ankle), on every template.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers: a panic is a clear failure

mod common;

use std::collections::HashMap;

use omuzaki_anim_api::{HumanoidBone, Pose};
use omuzaki_char_api::BuiltCharacter;
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_doc_mock::MemDoc;
use serde_json::json;

/// The starter character, which wears boots, on `template`.
fn built(template: &str) -> BuiltCharacter {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": template}));
    common::build(&doc, "kit")
}

/// The skin matrices of the rest pose with each bone of `turns` turned about the body's side-to-side axis.
fn posed(built: &BuiltCharacter, turns: &[(HumanoidBone, f32)]) -> Vec<Mat4> {
    let skeleton = &built.rig.skeleton;
    let mut local = skeleton.rest_pose();
    for &(bone, degrees) in turns {
        let id = skeleton.humanoid_bone(bone).expect("the template has the bone");
        let slot = &mut local.local[id.index()];
        slot.rotation = Quat::from_rotation_x(degrees.to_radians()) * slot.rotation;
    }
    let (mut world, mut out) = (Pose::IDENTITY, vec![Mat4::ZERO; skeleton.len()]);
    skeleton.skin_matrices(&local, &built.rig.inverse_bind(), &mut world, &mut out);
    out
}

/// The root of `x` in the union-find `parent`.
fn root(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

/// The volume the boots' leather encloses under `matrices`: its triangles, with each open border closed by a fan to its
/// centre (the shaft's top and bottom, the pull tab's ends), welded by position first so seams don't count as borders.
fn leather_volume(built: &BuiltCharacter, matrices: &[Mat4]) -> f32 {
    let mesh = &built.meshes.iter().find(|m| m.mesh.name.ends_with("-leather")).expect("the boots' leather").mesh;
    let g = &mesh.geometry;
    let bones: Vec<usize> =
        mesh.joint_bones.iter().map(|j| built.rig.bone(j).expect("a joint of the rig").index()).collect();
    let skinned: Vec<Vec3> = g
        .positions
        .iter()
        .enumerate()
        .map(|(k, p)| {
            (0..4).fold(Vec3::ZERO, |sum, i| {
                sum + matrices[bones[usize::from(g.joints[k][i])]].transform_point3(*p) * g.weights[k][i]
            })
        })
        .collect();
    let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
    let weld: Vec<usize> = g
        .positions
        .iter()
        .map(|p| {
            #[allow(clippy::cast_possible_truncation)] // a micrometre grid
            let key = [p.x, p.y, p.z].map(|c| (f64::from(c) * 1e6).round() as i64);
            let next = ids.len();
            *ids.entry(key).or_insert(next)
        })
        .collect();
    let mut at = vec![Vec3::ZERO; ids.len()];
    for (k, &w) in weld.iter().enumerate() {
        at[w] = skinned[k];
    }
    let mut volume = 0.0;
    let mut edges: HashMap<(usize, usize), i32> = HashMap::new();
    for tri in g.indices.as_chunks::<3>().0 {
        let [a, b, c] = tri.map(|i| weld[i as usize]);
        volume += at[a].dot(at[b].cross(at[c])) / 6.0;
        for (u, v) in [(a, b), (b, c), (c, a)] {
            *edges.entry((u, v)).or_default() += 1;
        }
    }
    // The open borders: edges with no twin running the other way. Group them into loops by shared vertices.
    let border: Vec<(usize, usize)> = edges.keys().filter(|(u, v)| !edges.contains_key(&(*v, *u))).copied().collect();
    let mut parent: Vec<usize> = (0..at.len()).collect();
    for &(u, v) in &border {
        let (ru, rv) = (root(&mut parent, u), root(&mut parent, v));
        parent[ru] = rv;
    }
    let mut centres: HashMap<usize, (Vec3, f32)> = HashMap::new();
    for &(u, _) in &border {
        let r = root(&mut parent, u);
        let entry = centres.entry(r).or_insert((Vec3::ZERO, 0.0));
        entry.0 += at[u];
        entry.1 += 1.0;
    }
    for &(u, v) in &border {
        let (sum, n) = centres[&root(&mut parent, u)];
        let c = sum / n;
        volume += at[v].dot(at[u].cross(c)) / 6.0;
    }
    volume
}

#[test]
fn a_walking_ankle_and_bent_toes_keep_the_boot_from_collapsing() {
    use HumanoidBone::{LeftFoot, LeftToes, RightFoot, RightToes};
    for template in ["soft-sd", "mini", "standard", "tall"] {
        let built = built(template);
        let rest = leather_volume(&built, &posed(&built, &[]));
        assert!(rest > 0.0, "{template}: the leather encloses {rest} m3 at rest");
        for (ankle, toes) in [(25.0, 30.0), (-25.0, -30.0), (25.0, -30.0), (-25.0, 30.0)] {
            let pose = [(LeftFoot, ankle), (RightFoot, ankle), (LeftToes, toes), (RightToes, toes)];
            let walked = leather_volume(&built, &posed(&built, &pose));
            let kept = walked / rest;
            println!(
                "{template}: ankle {ankle:+} deg, toes {toes:+} deg: the boots keep {:.1}% of their volume",
                kept * 100.0
            );
            assert!(kept >= 0.85, "{template}: ankle {ankle}, toes {toes}: the boots keep only {kept} of their volume");
        }
    }
}
