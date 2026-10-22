//! Avatar scenes generated in code for the prepare and avatar-op tests: a VRM humanoid with eyes, fingers and toes, in
//! the T-pose or with its arms down, and skinned meshes with morph targets. Nothing third-party or private.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)] // fixture setup

pub mod head;

use std::collections::BTreeMap;

use omuzaki_anim_api::{Pose, Skeleton};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_char_api::JointRef;
use omuzaki_core::glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    Avatar, ExportCtx, ImportCtx, ImportInput, ImportOptions, IoScene, Material, Mesh, MeshData, MorphTarget, Node,
    NodeIx, Primitive, Shading, Skin, VRM_LICENSE_URLS, VrmMeta, run_export, run_import,
};
use omuzaki_store_api::ContentHash;

/// The left half and the centre line of the humanoid: each bone, its parent and its offset from the parent, in VRM
/// space (+Y up, +Z front, the character's left is +X). The right half mirrors the left across X.
pub const BONES: [(&str, Option<&str>, [f32; 3]); 31] = [
    ("hips", None, [0.0, 0.9, 0.0]),
    ("spine", Some("hips"), [0.0, 0.1, 0.0]),
    ("chest", Some("spine"), [0.0, 0.15, 0.0]),
    ("neck", Some("chest"), [0.0, 0.2, 0.0]),
    ("head", Some("neck"), [0.0, 0.1, 0.0]),
    ("leftEye", Some("head"), [0.03, 0.05, 0.06]),
    ("leftShoulder", Some("chest"), [0.03, 0.15, 0.0]),
    ("leftUpperArm", Some("leftShoulder"), [0.1, 0.0, 0.0]),
    ("leftLowerArm", Some("leftUpperArm"), [0.25, 0.0, 0.0]),
    ("leftHand", Some("leftLowerArm"), [0.25, 0.0, 0.0]),
    ("leftThumbMetacarpal", Some("leftHand"), [0.02, 0.0, 0.02]),
    ("leftThumbProximal", Some("leftThumbMetacarpal"), [0.02, 0.0, 0.02]),
    ("leftThumbDistal", Some("leftThumbProximal"), [0.02, 0.0, 0.02]),
    ("leftIndexProximal", Some("leftHand"), [0.08, 0.0, 0.02]),
    ("leftIndexIntermediate", Some("leftIndexProximal"), [0.03, 0.0, 0.0]),
    ("leftIndexDistal", Some("leftIndexIntermediate"), [0.02, 0.0, 0.0]),
    ("leftMiddleProximal", Some("leftHand"), [0.08, 0.0, 0.0]),
    ("leftMiddleIntermediate", Some("leftMiddleProximal"), [0.03, 0.0, 0.0]),
    ("leftMiddleDistal", Some("leftMiddleIntermediate"), [0.02, 0.0, 0.0]),
    ("leftRingProximal", Some("leftHand"), [0.08, 0.0, -0.02]),
    ("leftRingIntermediate", Some("leftRingProximal"), [0.03, 0.0, 0.0]),
    ("leftRingDistal", Some("leftRingIntermediate"), [0.02, 0.0, 0.0]),
    ("leftLittleProximal", Some("leftHand"), [0.07, 0.0, -0.04]),
    ("leftLittleIntermediate", Some("leftLittleProximal"), [0.02, 0.0, 0.0]),
    ("leftLittleDistal", Some("leftLittleIntermediate"), [0.02, 0.0, 0.0]),
    ("leftUpperLeg", Some("hips"), [0.1, -0.05, 0.0]),
    ("leftLowerLeg", Some("leftUpperLeg"), [0.0, -0.4, 0.0]),
    ("leftFoot", Some("leftLowerLeg"), [0.0, -0.4, 0.0]),
    ("leftToes", Some("leftFoot"), [0.0, -0.03, 0.1]),
    // A finger tip leaf below each distal, so the distal bones have a direction to measure.
    ("leftIndexTip", Some("leftIndexDistal"), [0.015, 0.0, 0.0]),
    ("leftMiddleTip", Some("leftMiddleDistal"), [0.015, 0.0, 0.0]),
];

/// A generated humanoid scene and its nodes by name.
pub struct Fixture {
    /// The scene.
    pub scene: IoScene,
    /// Every node by its name.
    pub nodes: BTreeMap<String, NodeIx>,
}

impl Fixture {
    /// The node called `name`.
    pub fn node(&self, name: &str) -> NodeIx {
        *self.nodes.get(name).unwrap_or_else(|| panic!("no node {name}"))
    }
}

/// The bones of both sides, in parent-first order, with their offsets: the left ones as listed, each right one
/// mirrored right after its left twin.
fn both_sides() -> Vec<(String, Option<String>, Vec3)> {
    let mut out = Vec::new();
    for (name, parent, offset) in BONES {
        let offset = Vec3::from_array(offset);
        out.push((name.to_owned(), parent.map(str::to_owned), offset));
        if name.starts_with("left") {
            let mirror = |s: &str| s.replacen("left", "right", 1);
            out.push((mirror(name), parent.map(mirror), offset * Vec3::new(-1.0, 1.0, 1.0)));
        }
    }
    out
}

/// A humanoid under a root node, with its upper arms turned `arms_down` degrees down from the T-pose (0 is a T-pose)
/// and the VRM humanoid map assigned. Tips (`*Tip`) are nodes, not humanoid bones. It carries a VRM meta and an
/// authored provenance record, so it exports.
pub fn humanoid(arms_down: f32) -> Fixture {
    humanoid_without(arms_down, &[])
}

/// [`humanoid`] without the bones in `skip`: each one's children hang from its parent, where they were.
pub fn humanoid_without(arms_down: f32, skip: &[&str]) -> Fixture {
    let mut scene = IoScene::new();
    let root = scene.add_node(Node::new("Root"));
    scene.roots = vec![root];
    let mut nodes = BTreeMap::from([("Root".to_owned(), root)]);
    // A skipped bone's place: its parent and its offset, which its children inherit.
    let mut skipped: BTreeMap<String, (Option<String>, Vec3)> = BTreeMap::new();
    let mut avatar = Avatar::new();
    for (name, parent, offset) in both_sides() {
        let (parent, offset) = match parent.as_ref().and_then(|p| skipped.get(p)) {
            Some((up, extra)) => (up.clone(), offset + *extra),
            None => (parent, offset),
        };
        if skip.contains(&name.as_str()) {
            skipped.insert(name, (parent, offset));
            continue;
        }
        let node = scene.add_node(Node::new(name.as_str()).at(offset));
        let up = parent.and_then(|p| nodes.get(&p).copied()).unwrap_or(root);
        scene.attach(up, node);
        if !name.ends_with("Tip") {
            avatar.humanoid.assign(&name, node).unwrap();
        }
        nodes.insert(name, node);
    }
    for (side, sign) in [("left", -1.0f32), ("right", 1.0)] {
        if let Some(arm) = nodes.get(&format!("{side}UpperArm")) {
            scene.nodes[arm.index()].transform.rotation = Quat::from_rotation_z(sign * arms_down.to_radians());
        }
    }
    scene.avatar = Some(avatar);
    scene.meta = VrmMeta::new("Generated Test Avatar", vec!["Test Author".into()], VRM_LICENSE_URLS[0]);
    scene.provenance = Some(omuzaki_io_mock::fixtures::authored_provenance());
    Fixture { scene, nodes }
}

/// `scene` written by the mock VRM 1.0 exporter and read back by its importer: what a generated `.vrm` file imports
/// as. The mock writes plain PBR materials only.
pub fn through_vrm(scene: &IoScene) -> IoScene {
    let options = omuzaki_io_mock::fixtures::options_without_face_channels();
    let mut out = run_export(&omuzaki_io_mock::MockVrm1Exporter, scene, &ExportCtx::new(&options)).expect("export");
    let bytes = out.files.remove(0).bytes;
    let input = ImportInput::new(&bytes).named("generated.vrm");
    run_import(&omuzaki_io_mock::MockVrm1Importer, &input, &ImportCtx::new(&ImportOptions::default()))
        .expect("import")
        .scene
}

/// The bytes the mock VRM 1.0 exporter writes for `scene`.
pub fn vrm_bytes(scene: &IoScene) -> Vec<u8> {
    let options = omuzaki_io_mock::fixtures::options_without_face_channels();
    let mut out = run_export(&omuzaki_io_mock::MockVrm1Exporter, scene, &ExportCtx::new(&options)).expect("export");
    out.files.remove(0).bytes
}

/// Every node's world matrix, from the scene's own transforms (scale included).
pub fn world(scene: &IoScene) -> Vec<Mat4> {
    let mut out = vec![Mat4::IDENTITY; scene.nodes.len()];
    for &r in &scene.roots {
        walk(scene, r, Mat4::IDENTITY, &mut out);
    }
    out
}

/// Places `node` under `parent` and its subtree under it, into `out`.
fn walk(scene: &IoScene, node: NodeIx, parent: Mat4, out: &mut [Mat4]) {
    let t = &scene.nodes[node.index()].transform;
    let m = parent * Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
    out[node.index()] = m;
    for &c in &scene.nodes[node.index()].children {
        walk(scene, c, m, out);
    }
}

/// A skinned mesh called `name` over the joints `joints`: a small triangle at each joint's rest position, weighted
/// fully to it, bound at the rest (each inverse bind is the joint's inverse world matrix) and drawn by a new node under
/// the root. The mesh has `targets` morph targets, target `t` moving the triangle of joint `t` up by 1 cm.
pub fn add_skinned_mesh(f: &mut Fixture, name: &str, joints: &[&str], targets: usize, material: Option<u32>) -> NodeIx {
    let world = world(&f.scene);
    let joint_nodes: Vec<NodeIx> = joints.iter().map(|j| f.node(j)).collect();
    let mut data = MeshData::default();
    for (j, node) in joint_nodes.iter().enumerate() {
        let at = world[node.index()].w_axis.truncate();
        let first = u32::try_from(data.positions.len()).unwrap();
        for corner in [Vec3::ZERO, Vec3::new(0.01, 0.0, 0.0), Vec3::new(0.0, 0.01, 0.0)] {
            data.positions.push(at + corner);
            data.normals.push(Vec3::Z);
            data.uvs.push(Vec2::ZERO);
            data.joints.push([u16::try_from(j).unwrap(), 0, 0, 0]);
            data.weights.push([1.0, 0.0, 0.0, 0.0]);
        }
        data.indices.extend([first, first + 1, first + 2]);
    }
    let count = data.positions.len();
    let mut primitive = Primitive::new(data);
    primitive.material = material.map(omuzaki_io_api::MaterialIx);
    primitive.morph_targets = (0..targets)
        .map(|t| MorphTarget::new((0..count).map(|v| if v / 3 == t { Vec3::Y * 0.01 } else { Vec3::ZERO }).collect()))
        .collect();
    let mut mesh = Mesh::new(name);
    mesh.primitives = vec![primitive];
    mesh.morph_target_names = (0..targets).map(|t| format!("Target_{t}")).collect();
    let mesh = f.scene.add_mesh(mesh);
    let inverse_bind = joint_nodes.iter().map(|n| world[n.index()].inverse()).collect();
    let skin = f.scene.add_skin(Skin::new(format!("{name}_skin"), joint_nodes, inverse_bind));
    let mut node = Node::new(name);
    node.mesh = Some(mesh);
    node.skin = Some(skin);
    let node = f.scene.add_node(node);
    f.scene.attach(f.node("Root"), node);
    f.nodes.insert(name.to_owned(), node);
    node
}

/// A toon material called `name` with base colour `base`.
pub fn toon_material(name: &str, base: Vec4) -> Material {
    let mut m = Material::new(name, Shading::Toon(Box::default()));
    m.base_color = base;
    m
}

/// The document node for an avatar imported from a file with content hash `[9; 32]`.
pub fn def() -> AvatarDef {
    AvatarDef::new(AvatarSource::new(ContentHash([9; 32]), SourceFormat::Vrm1, "test.vrm"))
}

/// The file's skinning of every drawing node's primitives, in node order then primitive order: each vertex through
/// its joints' world matrices and inverse binds (glTF 2.0 "Skins"), or the node's world matrix when it has no skin.
pub fn file_skinning(scene: &IoScene) -> Vec<Vec<Vec3>> {
    let world = world(scene);
    let mut out = Vec::new();
    for node in &scene.nodes {
        let Some(mesh) = node.mesh else { continue };
        let skin = node.skin.map(|s| &scene.skins[s.index()]);
        for p in &scene.meshes[mesh.index()].primitives {
            let positions = p.mesh.positions.iter().enumerate().map(|(v, &pos)| {
                if let Some(skin) = skin {
                    let (joints, weights) = (p.mesh.joints[v], p.mesh.weights[v]);
                    (0..4).fold(Vec3::ZERO, |acc, k| {
                        let j = usize::from(joints[k]);
                        let m = world[skin.joints[j].index()] * skin.inverse_bind[j];
                        acc + m.transform_point3(pos) * weights[k]
                    })
                } else {
                    let at = scene.nodes.iter().position(|n| std::ptr::eq(n, node)).unwrap();
                    world[at].transform_point3(pos)
                }
            });
            out.push(positions.collect());
        }
    }
    out
}

/// The runtime skinning of every avatar mesh with the skeleton at `pose` (local transforms).
pub fn runtime_skinning(avatar: &ImportedAvatar, pose: &Pose) -> Vec<Vec<Vec3>> {
    let mut world = Pose::IDENTITY;
    avatar.skeleton.world_pose(pose, &mut world);
    avatar
        .meshes
        .iter()
        .map(|m| {
            let joints: Vec<Mat4> = m
                .mesh
                .joint_bones
                .iter()
                .zip(&m.inverse_bind)
                .map(|(j, ib)| world.local[bone(&avatar.skeleton, j).index()].to_mat4() * *ib)
                .collect();
            let g = &m.mesh.geometry;
            g.positions
                .iter()
                .enumerate()
                .map(|(v, &pos)| {
                    (0..4).fold(Vec3::ZERO, |acc, k| {
                        acc + joints[usize::from(g.joints[v][k])].transform_point3(pos) * g.weights[v][k]
                    })
                })
                .collect()
        })
        .collect()
}

/// The bone a joint reference names.
pub fn bone(skeleton: &Skeleton, joint: &JointRef) -> omuzaki_anim_api::BoneId {
    match joint {
        JointRef::Humanoid(b) => skeleton.humanoid_bone(*b).expect("the humanoid bone"),
        JointRef::Named(n) => skeleton.find(n.as_str()).expect("the named bone"),
    }
}

/// The largest distance between matching points of two lists of lists.
pub fn max_error(a: &[Vec<Vec3>], b: &[Vec<Vec3>]) -> f32 {
    assert_eq!(a.len(), b.len(), "as many meshes");
    a.iter()
        .zip(b)
        .flat_map(|(x, y)| {
            assert_eq!(x.len(), y.len(), "as many vertices");
            x.iter().zip(y).map(|(p, q)| p.distance(*q))
        })
        .fold(0.0, f32::max)
}
