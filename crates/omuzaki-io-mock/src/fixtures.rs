//! Test fixtures generated in code: nothing here is a third-party or private file (`docs/licensing/policy.md`).
//!
//! - [`tiny_glb`] builds a one-triangle GLB **by hand** (JSON plus binary buffer), independently of the mock
//!   exporter, so import is tested without depending on export.
//! - [`tiny_avatar`] builds a minimal valid VRM scene: the 15 required humanoid bones in a skeleton, one skinned
//!   triangle with a morph target, two expressions, meta and a provenance record.

use std::collections::BTreeMap;

use omuzaki_core::glam::{Mat4, Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    Avatar, ExpressionName, IoScene, Material, Mesh, MeshData, MorphTarget, Node, NodeIx, Override, Pbr, Primitive,
    Shading, Skin, VRM_LICENSE_URLS, VrmMeta,
};
use omuzaki_store_api::{Commercial, Provenance, Source};
use serde_json::json;

use crate::glb;

/// The 15 bones VRM 1.0 requires, each with its parent and its local position: a small T-shaped skeleton.
pub const SKELETON: [(&str, Option<&str>, [f32; 3]); 15] = [
    ("hips", None, [0.0, 0.9, 0.0]),
    ("spine", Some("hips"), [0.0, 0.1, 0.0]),
    ("head", Some("spine"), [0.0, 0.5, 0.0]),
    ("leftUpperLeg", Some("hips"), [0.1, -0.05, 0.0]),
    ("leftLowerLeg", Some("leftUpperLeg"), [0.0, -0.4, 0.0]),
    ("leftFoot", Some("leftLowerLeg"), [0.0, -0.4, 0.0]),
    ("rightUpperLeg", Some("hips"), [-0.1, -0.05, 0.0]),
    ("rightLowerLeg", Some("rightUpperLeg"), [0.0, -0.4, 0.0]),
    ("rightFoot", Some("rightLowerLeg"), [0.0, -0.4, 0.0]),
    ("leftUpperArm", Some("spine"), [0.15, 0.4, 0.0]),
    ("leftLowerArm", Some("leftUpperArm"), [0.25, 0.0, 0.0]),
    ("leftHand", Some("leftLowerArm"), [0.25, 0.0, 0.0]),
    ("rightUpperArm", Some("spine"), [-0.15, 0.4, 0.0]),
    ("rightLowerArm", Some("rightUpperArm"), [-0.25, 0.0, 0.0]),
    ("rightHand", Some("rightLowerArm"), [-0.25, 0.0, 0.0]),
];

/// Export options for scenes that have no baked face channels: `ExportOptions::default()` asks for the 52 `ARKit`
/// channels (perfect sync is on by default), which a hand-built scene doesn't have.
pub fn options_without_face_channels() -> omuzaki_io_api::ExportOptions {
    omuzaki_io_api::ExportOptions::default().with_perfect_sync(omuzaki_io_api::PerfectSync::Off)
}

/// The §7 record for material this project made itself: cleared for commercial use, not private.
pub fn authored_provenance() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: Some("Test Author".into()),
        licence: "LicenseRef-Omuzaki-User".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

/// The scene built by [`tiny_avatar`] has these node indices: 0 is the root, 1..=15 the bones in [`SKELETON`] order,
/// 16 the body mesh.
pub const BODY_NODE: NodeIx = NodeIx(16);

/// A minimal valid VRM 1.0 scene. It passes `IoScene::check`, `vrm_profile` and every `preflight` rule when
/// exported privately, so tests mutate it to break one rule at a time.
pub fn tiny_avatar() -> IoScene {
    let mut s = IoScene::new();
    let root = s.add_node(Node::new("root"));
    s.roots = vec![root];
    let mut bones: BTreeMap<&str, NodeIx> = BTreeMap::new();
    let mut av = Avatar::new();
    for (name, parent, t) in SKELETON {
        let node = s.add_node(Node::new(name).at(Vec3::from_array(t)));
        let parent = parent.and_then(|p| bones.get(p).copied()).unwrap_or(root);
        s.attach(parent, node);
        bones.insert(name, node);
        // The names above are the spec's, so assigning cannot fail; a typo would fail the fixture's own tests.
        let _ = av.humanoid.assign(name, node);
    }
    let material = s.add_material({
        let mut m = Material::new(
            "skin",
            Shading::Pbr(Pbr { metallic: 0.0, roughness: 1.0, metallic_roughness_texture: None }),
        );
        m.base_color = Vec4::new(0.9, 0.7, 0.6, 1.0);
        m
    });
    let mut tri = Primitive::new(MeshData {
        positions: vec![Vec3::ZERO, Vec3::new(0.1, 0.0, 0.0), Vec3::new(0.0, 0.1, 0.0)],
        normals: vec![Vec3::Z; 3],
        uvs: vec![Vec2::ZERO, Vec2::X, Vec2::Y],
        joints: vec![[0, 0, 0, 0]; 3],
        weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
        indices: vec![0, 1, 2],
        ..MeshData::default()
    });
    tri.material = Some(material);
    tri.morph_targets = vec![MorphTarget::new(vec![Vec3::new(0.0, 0.01, 0.0), Vec3::ZERO, Vec3::ZERO])];
    let mut mesh = Mesh::new("Body");
    mesh.primitives = vec![tri];
    mesh.morph_target_names = vec!["jawOpen".into()];
    let mesh = s.add_mesh(mesh);
    let joints: Vec<NodeIx> = SKELETON.iter().filter_map(|(n, _, _)| bones.get(n).copied()).collect();
    let skin = s.add_skin(Skin::new("skeleton", joints, vec![Mat4::IDENTITY; SKELETON.len()]));
    let mut body = Node::new("Body");
    body.mesh = Some(mesh);
    body.skin = Some(skin);
    let body = s.add_node(body);
    s.attach(root, body);

    let mut happy = omuzaki_io_api::Expression::morph(body, 0);
    happy.morph_binds[0].weight = 0.5;
    happy.override_blink = Override::Block;
    av.expressions.insert(ExpressionName::parse("happy").unwrap_or_else(|_| unreachable_name()), happy);
    av.expressions.insert(
        ExpressionName::parse("jawOpen").unwrap_or_else(|_| unreachable_name()),
        omuzaki_io_api::Expression::morph(body, 0),
    );
    s.avatar = Some(av);

    s.meta = VrmMeta::new("Tiny Test Avatar", vec!["Test Author".into()], VRM_LICENSE_URLS[0]);
    s.meta.terms.commercial_usage = omuzaki_io_api::CommercialUsage::PersonalProfit;
    s.provenance = Some(authored_provenance());
    s
}

/// `ExpressionName::parse` only fails on an empty string, and the fixture passes literals.
fn unreachable_name() -> ExpressionName {
    ExpressionName::Preset(omuzaki_io_api::PresetExpression::Neutral)
}

/// A one-triangle GLB assembled byte by byte: a node with a mesh (positions and `u16` indices) and a red material.
pub fn tiny_glb() -> Vec<u8> {
    let mut bin = Vec::new();
    for corner in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
        for c in corner {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    for i in [0u16, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    let doc = json!({
        "asset": { "version": "2.0", "generator": "omuzaki-io-mock fixture", "copyright": "Test Author" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "name": "tri", "mesh": 0, "translation": [0.0, 1.0, 0.0] }],
        "meshes": [{ "name": "tri", "primitives": [{ "attributes": { "POSITION": 0 }, "indices": 1, "material": 0 }] }],
        "materials": [{ "name": "red", "pbrMetallicRoughness": { "baseColorFactor": [1.0, 0.0, 0.0, 1.0], "metallicFactor": 0.0 } }],
        "accessors": [
            { "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] },
            { "bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR" }
        ],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": 36, "target": 34962 },
            { "buffer": 0, "byteOffset": 36, "byteLength": 6, "target": 34963 }
        ],
        "buffers": [{ "byteLength": 42 }]
    });
    glb::join(doc.to_string().as_bytes(), &bin)
}

/// Wraps a glTF JSON document and a binary buffer in a GLB container, for tests that hand-edit a file.
pub fn glb_from_json(doc: &serde_json::Value, bin: &[u8]) -> Vec<u8> {
    glb::join(doc.to_string().as_bytes(), bin)
}

/// The JSON chunk of a GLB and its binary chunk, for tests that inspect or edit what an exporter wrote.
pub fn glb_parts(bytes: &[u8]) -> Option<(serde_json::Value, Vec<u8>)> {
    let (json, bin) = glb::split(bytes).ok()?;
    Some((serde_json::from_slice(json).ok()?, bin.to_vec()))
}
