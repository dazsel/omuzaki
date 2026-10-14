//! Fixtures generated in code, shared by the integration tests. Nothing here is a third-party or private file
//! (`docs/licensing/policy.md`); the smoke test on private material is a separate, opt-in test.

// Each test file uses a different subset of these helpers, and the scene types are `#[non_exhaustive]`, so a fixture can
// only fill them by assigning to a constructed value. Index arithmetic on tiny fixtures and one long builder are fine.
#![allow(
    dead_code,
    clippy::field_reassign_with_default,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;

use omuzaki_core::glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    AimAxis, AlphaMode, Avatar, Blob, Collider, ColliderGroup, ColliderShape, ColorBind, ColorBindKind,
    CommercialUsage, ConstraintKind, CreditNotation, EncodedImage, ExportCtx, ExportOptions, Exporter, Expression,
    ExpressionName, Filter, FirstPerson, FirstPersonFlag, ImageMime, ImportCtx, ImportInput, ImportOptions,
    ImportOutcome, Importer, IoError, IoScene, LookAt, LookAtKind, Material, MaterialIx, Mesh, MeshAnnotation,
    MeshData, Modification, MorphBind, MorphTarget, Node, NodeConstraint, NodeIx, NormalTexture, OutlineMode, Override,
    Passthrough, Pbr, PerfectSync, Primitive, RangeMap, RollAxis, Sampler, Shading, ShiftTexture, Skin, SpringChain,
    SpringJoint, TexRef, Texture, Toon, UseTerms, UvBind, UvTransform, VRM_LICENSE_URLS, VrmMeta, Wrap, run_export,
    run_import,
};
use omuzaki_store_api::{Commercial, Provenance, Source};
use serde_json::{Value, json};

/// A real 1x1 PNG (70 bytes).
pub const PNG: [u8; 70] = [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49,
    0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64, 0x60, 0xF8, 0x5F, 0x0F, 0x00, 0x02, 0x87, 0x01, 0x80, 0xEB, 0x47, 0xBA,
    0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// JPEG start, a JFIF header and the end marker: enough for the magic-byte sniff; the crate never decodes images.
pub const JPEG: [u8; 22] = [
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00,
    0x00, 0xFF, 0xD9,
];

/// The humanoid of the fixture: VRM 1.0 name, parent bone and local position in the 1.0 orientation (front is +Z).
/// Eyes and toes have a non-zero Z so a wrong 0.x orientation conversion shows up.
const BONES: &[(&str, Option<&str>, [f32; 3])] = &[
    ("hips", None, [0.0, 0.9, 0.0]),
    ("spine", Some("hips"), [0.0, 0.1, 0.0]),
    ("chest", Some("spine"), [0.0, 0.15, 0.0]),
    ("neck", Some("chest"), [0.0, 0.25, 0.0]),
    ("head", Some("neck"), [0.0, 0.1, 0.0]),
    ("leftEye", Some("head"), [0.03, 0.08, 0.08]),
    ("rightEye", Some("head"), [-0.03, 0.08, 0.08]),
    ("jaw", Some("head"), [0.0, 0.03, 0.05]),
    ("leftShoulder", Some("chest"), [0.04, 0.2, 0.0]),
    ("leftUpperArm", Some("leftShoulder"), [0.08, 0.0, 0.0]),
    ("leftLowerArm", Some("leftUpperArm"), [0.25, 0.0, 0.0]),
    ("leftHand", Some("leftLowerArm"), [0.25, 0.0, 0.0]),
    ("leftThumbMetacarpal", Some("leftHand"), [0.02, 0.0, 0.02]),
    ("leftThumbProximal", Some("leftThumbMetacarpal"), [0.03, 0.0, 0.02]),
    ("leftThumbDistal", Some("leftThumbProximal"), [0.03, 0.0, 0.01]),
    ("leftIndexProximal", Some("leftHand"), [0.08, 0.0, 0.01]),
    ("rightShoulder", Some("chest"), [-0.04, 0.2, 0.0]),
    ("rightUpperArm", Some("rightShoulder"), [-0.08, 0.0, 0.0]),
    ("rightLowerArm", Some("rightUpperArm"), [-0.25, 0.0, 0.0]),
    ("rightHand", Some("rightLowerArm"), [-0.25, 0.0, 0.0]),
    ("leftUpperLeg", Some("hips"), [0.09, -0.05, 0.0]),
    ("leftLowerLeg", Some("leftUpperLeg"), [0.0, -0.4, 0.0]),
    ("leftFoot", Some("leftLowerLeg"), [0.0, -0.4, 0.0]),
    ("leftToes", Some("leftFoot"), [0.0, -0.05, 0.12]),
    ("rightUpperLeg", Some("hips"), [-0.09, -0.05, 0.0]),
    ("rightLowerLeg", Some("rightUpperLeg"), [0.0, -0.4, 0.0]),
    ("rightFoot", Some("rightLowerLeg"), [0.0, -0.4, 0.0]),
    ("rightToes", Some("rightFoot"), [0.0, -0.05, 0.12]),
];

/// Everything the fixture builder hands back besides the scene, so tests can name nodes and materials.
pub struct Ids {
    pub nodes: BTreeMap<&'static str, NodeIx>,
    pub body: NodeIx,
    pub hair: NodeIx,
}

impl Ids {
    pub fn node(&self, name: &str) -> NodeIx {
        self.nodes[name]
    }
}

pub fn provenance() -> Provenance {
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

fn tex(t: omuzaki_io_api::TextureIx) -> TexRef {
    TexRef::of(t)
}

/// A grid of `w` by `h` quads in the XY plane, with normals, UVs, vertex colours and skin weights over `joints`.
pub fn grid(w: u32, h: u32, joints: u16) -> MeshData {
    let (cols, rows) = (w + 1, h + 1);
    let mut m = MeshData::default();
    for y in 0..rows {
        for x in 0..cols {
            #[allow(clippy::cast_precision_loss)]
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            m.positions.push(Vec3::new(fx - 0.5, fy, 0.05 * fx));
            m.normals.push(Vec3::Z);
            m.uvs.push(Vec2::new(fx, 1.0 - fy));
            let j = u16::try_from(x + y).unwrap_or(0) % joints;
            m.joints.push([j, (j + 1) % joints, 0, 0]);
            m.weights.push([0.75, 0.25, 0.0, 0.0]);
        }
    }
    for y in 0..h {
        for x in 0..w {
            let a = y * cols + x;
            m.indices.extend([a, a + 1, a + cols, a + 1, a + cols + 1, a + cols]);
        }
    }
    m
}

/// A toon material with every `MToon` field set to a distinct non-default value.
pub fn full_toon(
    mask: omuzaki_io_api::TextureIx,
    albedo: omuzaki_io_api::TextureIx,
    matcap: omuzaki_io_api::TextureIx,
) -> Toon {
    let mut t = Toon::default();
    t.shade_color = Vec3::new(0.8, 0.5, 0.625);
    t.shade_multiply_texture = Some(TexRef {
        texture: albedo,
        uv_set: 0,
        transform: Some(UvTransform { offset: Vec2::new(0.25, 0.5), scale: Vec2::new(2.0, 0.5) }),
    });
    t.shading_shift = -0.25;
    t.shading_shift_texture = Some(ShiftTexture { texture: tex(mask), scale: 0.5 });
    t.shading_toony = 0.75;
    t.gi_equalization = 0.5;
    t.matcap_color = Vec3::new(0.5, 0.5, 1.0);
    t.matcap_texture = Some(tex(matcap));
    t.rim_color = Vec3::new(0.25, 0.5, 0.75);
    t.rim_fresnel_power = 3.0;
    t.rim_lift = 0.125;
    t.rim_multiply_texture = Some(tex(mask));
    t.rim_lighting_mix = 0.5;
    t.outline_mode = OutlineMode::WorldCoordinates;
    t.outline_width = 0.0025;
    t.outline_width_texture = Some(tex(mask));
    t.outline_color = Vec3::new(0.125, 0.0625, 0.25);
    t.outline_lighting_mix = 0.25;
    t.uv_animation_mask = Some(tex(mask));
    t.uv_scroll_x = 0.5;
    t.uv_scroll_y = -0.25;
    t.uv_rotation = 1.5;
    t
}

/// The big fixture: a 27-bone humanoid with a skinned, morphing body, four kinds of material, every expression bind
/// kind, look-at, first-person, springs with colliders, constraints, full meta, and passthrough data.
pub fn avatar() -> (IoScene, Ids) {
    let mut s = IoScene::new();
    let png = s.add_image(EncodedImage::new("albedo", ImageMime::Png, Blob::new(PNG.to_vec())));
    let jpg = s.add_image(EncodedImage::new("mask", ImageMime::Jpeg, Blob::new(JPEG.to_vec())));
    let mut thumb_bytes = PNG.to_vec();
    thumb_bytes.push(0); // a distinct image
    let thumb = s.add_image(EncodedImage::new("thumbnail", ImageMime::Png, Blob::new(thumb_bytes)));
    let t_albedo = s.add_texture(Texture { image: png, sampler: Sampler::default() });
    let t_mask = s.add_texture(Texture {
        image: jpg,
        sampler: Sampler {
            wrap_u: Wrap::ClampToEdge,
            wrap_v: Wrap::MirroredRepeat,
            mag: Filter::Nearest,
            min: Filter::Nearest,
            mipmaps: false,
        },
    });
    let t_matcap = s.add_texture(Texture { image: png, sampler: Sampler { mipmaps: false, ..Sampler::default() } });
    let t_thumb = s.add_texture(Texture { image: thumb, sampler: Sampler::default() });
    let _ = t_thumb;

    // Materials.
    let mut skin_mat = Material::new("skin", Shading::Toon(Box::new(full_toon(t_mask, t_albedo, t_matcap))));
    skin_mat.base_color = Vec4::new(0.9, 0.75, 0.625, 1.0);
    skin_mat.base_color_texture = Some(TexRef {
        texture: t_albedo,
        uv_set: 0,
        transform: Some(UvTransform { offset: Vec2::new(0.0, 0.5), scale: Vec2::ONE }),
    });
    skin_mat.normal_texture = Some(NormalTexture { texture: tex(t_mask), scale: 0.75 });
    skin_mat.emissive = Vec3::new(0.125, 0.0625, 0.0);
    skin_mat.emissive_texture = Some(tex(t_albedo));
    let skin_mat = s.add_material(skin_mat);

    let mut cloth = Material::new(
        "cloth",
        Shading::Pbr(Pbr { metallic: 0.25, roughness: 0.5, metallic_roughness_texture: Some(tex(t_mask)) }),
    );
    cloth.alpha = AlphaMode::Mask { cutoff: 0.375 };
    cloth.double_sided = true;
    cloth.emissive = Vec3::new(2.0, 1.0, 0.0); // above 1: written with KHR_materials_emissive_strength
    cloth.passthrough = Passthrough {
        extensions: [("EXT_fixture_material".to_owned(), json!({"sheen": [1, 2, 3]}))].into(),
        extras: Some(json!({"layer": "cloth"})),
    };
    let cloth = s.add_material(cloth);

    let mut hair = Toon::default();
    hair.shade_color = Vec3::new(0.25, 0.25, 0.5);
    hair.transparent_with_zwrite = true;
    hair.render_queue_offset = 3;
    let mut hair_mat = Material::new("hair", Shading::Toon(Box::new(hair)));
    hair_mat.alpha = AlphaMode::Blend;
    let hair_mat = s.add_material(hair_mat);

    let mut glass = Toon::default();
    glass.render_queue_offset = -2;
    glass.outline_mode = OutlineMode::ScreenCoordinates;
    glass.outline_width = 0.01;
    let mut glass_mat = Material::new("glass", Shading::Toon(Box::new(glass)));
    glass_mat.alpha = AlphaMode::Blend;
    let glass_mat = s.add_material(glass_mat);

    let mut unlit = Material::new("unlit", Shading::Unlit);
    unlit.base_color = Vec4::new(1.0, 0.5, 0.25, 0.5);
    unlit.alpha = AlphaMode::Blend;
    let unlit = s.add_material(unlit);

    // Nodes: an armature root, the humanoid, and the extra nodes for springs and constraints.
    let root = s.add_node(Node::new("Armature"));
    s.roots = vec![root];
    let mut nodes: BTreeMap<&'static str, NodeIx> = BTreeMap::new();
    let mut av = Avatar::new();
    for (name, parent, t) in BONES {
        let n = s.add_node(Node::new(*name).at(Vec3::from_array(*t)));
        let parent = parent.map_or(root, |p| nodes[p]);
        s.attach(parent, n);
        nodes.insert(name, n);
        av.humanoid.assign(name, n).unwrap();
    }
    let mut extra = |s: &mut IoScene, name: &'static str, parent: &str, t: [f32; 3]| {
        let n = s.add_node(Node::new(name).at(Vec3::from_array(t)));
        s.attach(nodes[parent], n);
        nodes.insert(name, n);
        n
    };
    extra(&mut s, "hairA1", "head", [0.0, 0.12, -0.05]);
    extra(&mut s, "hairA2", "hairA1", [0.0, -0.1, -0.02]);
    extra(&mut s, "hairA3", "hairA2", [0.0, -0.1, -0.02]);
    extra(&mut s, "hairB1", "head", [0.05, 0.1, -0.05]);
    extra(&mut s, "hairB2", "hairB1", [0.0, -0.12, -0.01]);
    extra(&mut s, "leftTwist", "leftLowerArm", [0.12, 0.0, 0.0]);
    extra(&mut s, "leftSleeve", "leftUpperArm", [0.1, 0.0, 0.01]);
    extra(&mut s, "rightSub", "rightLowerArm", [-0.1, 0.0, 0.0]);

    // A node with a general rotation and scale: it must come back bit for bit (importers only normalise rotations that
    // are visibly off), and survive the 0.x orientation change.
    let axis = Vec3::new(0.2, 0.5, 0.8).normalize();
    s.nodes[nodes["leftSleeve"].index()].transform.rotation = Quat::from_axis_angle(axis, 0.6);
    s.nodes[nodes["rightSub"].index()].transform.scale = Vec3::new(1.0, 2.0, 0.5);

    // Body: skinned, two primitives, four morph targets of every storage kind.
    let joint_names = ["hips", "spine", "chest", "neck", "head", "leftUpperArm"];
    let mut body_prim = Primitive::new(grid(3, 3, 6));
    body_prim.material = Some(skin_mat);
    let n = body_prim.mesh.positions.len();
    let mut dense = MorphTarget::new((0..n).map(|i| Vec3::new(0.001 * i as f32, 0.002, -0.001 * i as f32)).collect());
    dense.normals = vec![Vec3::new(0.0, 0.0, -0.5); n];
    let mut sparse_positions = vec![Vec3::ZERO; n];
    sparse_positions[3] = Vec3::new(0.0, 0.0625, 0.0);
    sparse_positions[9] = Vec3::new(-0.03125, 0.0, 0.125);
    let zero = MorphTarget::new(vec![Vec3::ZERO; n]);
    let mut sparse_normals = MorphTarget::new(sparse_positions.clone());
    sparse_normals.normals = vec![Vec3::ZERO; n];
    sparse_normals.normals[5] = Vec3::new(0.25, 0.0, 0.0);
    body_prim.morph_targets = vec![dense, MorphTarget::new(sparse_positions), zero, sparse_normals];
    let mut cloth_prim = Primitive::new(grid(2, 2, 6));
    cloth_prim.material = Some(cloth);
    cloth_prim.mesh.colours = vec![[1.0, 0.5, 0.25, 1.0]; cloth_prim.mesh.positions.len()];
    let cn = cloth_prim.mesh.positions.len();
    cloth_prim.morph_targets = (0..4).map(|_| MorphTarget::new(vec![Vec3::ZERO; cn])).collect();
    let mut body_mesh = Mesh::new("Body");
    body_mesh.primitives = vec![body_prim, cloth_prim];
    body_mesh.morph_target_names = vec!["jawOpen".into(), "blink".into(), "empty".into(), "smile".into()];
    let body_mesh = s.add_mesh(body_mesh);
    let mut hair_prim = Primitive::new(grid(1, 1, 1));
    hair_prim.mesh.joints.clear();
    hair_prim.mesh.weights.clear();
    hair_prim.material = Some(hair_mat);
    let mut glass_prim = Primitive::new(grid(1, 1, 1));
    glass_prim.mesh.joints.clear();
    glass_prim.mesh.weights.clear();
    glass_prim.material = Some(glass_mat);
    let mut unlit_prim = Primitive::new(grid(1, 1, 1));
    unlit_prim.mesh.joints.clear();
    unlit_prim.mesh.weights.clear();
    unlit_prim.material = Some(unlit);
    let mut hair_mesh = Mesh::new("Hair");
    hair_mesh.primitives = vec![hair_prim, glass_prim, unlit_prim];
    let hair_mesh = s.add_mesh(hair_mesh);

    let ibm: Vec<Mat4> = (0..6)
        .map(|i| {
            Mat4::from_rotation_translation(
                Quat::from_rotation_y(0.1 * i as f32),
                Vec3::new(0.0, -0.1 * i as f32, 0.05),
            )
        })
        .collect();
    let mut skin = Skin::new("Body skin", joint_names.iter().map(|j| nodes[j]).collect(), ibm);
    skin.skeleton = Some(nodes["hips"]);
    let skin = s.add_skin(skin);
    let mut body = Node::new("Body");
    body.mesh = Some(body_mesh);
    body.skin = Some(skin);
    body.passthrough = Passthrough {
        extensions: [("EXT_fixture_node".to_owned(), json!({"flag": true}))].into(),
        extras: Some(json!({"note": "body"})),
    };
    let body = s.add_node(body);
    s.attach(root, body);
    let mut hair_node = Node::new("Hair").at(Vec3::new(0.0, 0.02, 0.0));
    hair_node.mesh = Some(hair_mesh);
    let hair_node = s.add_node(hair_node);
    s.attach(nodes["head"], hair_node);

    // Expressions.
    let name = |n: &str| ExpressionName::parse(n).unwrap();
    let mut happy = Expression::new();
    happy.morph_binds = vec![MorphBind { node: body, index: 3, weight: 0.5 }];
    happy.color_binds = vec![ColorBind {
        material: MaterialIx(0),
        kind: ColorBindKind::Color,
        target: Vec4::new(1.0, 0.875, 0.875, 1.0),
    }];
    happy.uv_binds = vec![UvBind { material: MaterialIx(0), scale: Vec2::new(0.5, 0.5), offset: Vec2::new(0.25, 0.5) }];
    happy.override_blink = Override::Block;
    happy.override_look_at = Override::Blend;
    happy.override_mouth = Override::Block;
    av.expressions.insert(name("happy"), happy);
    let mut blink = Expression::morph(body, 1);
    blink.is_binary = true;
    av.expressions.insert(name("blink"), blink);
    av.expressions.insert(name("aa"), Expression::morph(body, 0));
    let mut angry = Expression::new();
    angry.color_binds = [
        ColorBindKind::EmissionColor,
        ColorBindKind::ShadeColor,
        ColorBindKind::MatcapColor,
        ColorBindKind::RimColor,
        ColorBindKind::OutlineColor,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, kind)| ColorBind { material: MaterialIx(0), kind, target: Vec4::splat(0.125 * (i + 1) as f32) })
    .collect();
    av.expressions.insert(name("angry"), angry);
    av.expressions.insert(name("neutral"), Expression::new());
    av.expressions.insert(name("jawOpen"), Expression::morph(body, 0));
    let mut heart = Expression::new();
    heart.uv_binds = vec![UvBind { material: MaterialIx(0), scale: Vec2::ONE, offset: Vec2::new(0.5, 0.0) }];
    av.expressions.insert(name("omuzaki_heart-eyes"), heart);

    av.look_at = Some(LookAt {
        kind: LookAtKind::Expression,
        offset_from_head_bone: Vec3::new(0.0, 0.0625, 0.0125),
        horizontal_inner: RangeMap { input_max_value: 80.0, output_scale: 0.75 },
        horizontal_outer: RangeMap { input_max_value: 70.0, output_scale: 1.0 },
        vertical_down: RangeMap { input_max_value: 45.0, output_scale: 0.5 },
        vertical_up: RangeMap { input_max_value: 30.0, output_scale: 0.25 },
    });
    av.first_person = Some(FirstPerson {
        annotations: vec![
            MeshAnnotation { node: hair_node, flag: FirstPersonFlag::ThirdPersonOnly },
            MeshAnnotation { node: body, flag: FirstPersonFlag::Both },
        ],
    });

    // Spring bones: two chains, three colliders (a sphere and two capsules) in two groups.
    let sb = &mut av.spring_bones;
    sb.colliders = vec![
        Collider {
            node: nodes["head"],
            shape: ColliderShape::Sphere { offset: Vec3::new(0.0, 0.05, 0.0), radius: 0.09 },
        },
        Collider {
            node: nodes["head"],
            shape: ColliderShape::Capsule {
                offset: Vec3::new(0.0, -0.05, 0.0),
                radius: 0.05,
                tail: Vec3::new(0.0, -0.15, 0.02),
            },
        },
        Collider {
            node: nodes["chest"],
            shape: ColliderShape::Capsule {
                offset: Vec3::new(-0.1, 0.0, 0.0),
                radius: 0.08,
                tail: Vec3::new(0.1, 0.0, 0.0),
            },
        },
    ];
    sb.collider_groups = vec![
        ColliderGroup { name: "head".into(), colliders: vec![0, 1] },
        ColliderGroup { name: "body".into(), colliders: vec![2] },
    ];
    let joint = |node: &str, i: f32| {
        let mut j = SpringJoint::new(nodes[node]);
        j.hit_radius = 0.01 * i;
        j.stiffness = 0.5 + i;
        j.gravity_power = 0.125 * i;
        j.gravity_dir = Vec3::new(0.0, -1.0, 0.0);
        j.drag_force = 0.25 * i;
        j
    };
    sb.chains = vec![
        SpringChain {
            name: "hair A".into(),
            joints: vec![joint("hairA1", 1.0), joint("hairA2", 2.0), joint("hairA3", 3.0)],
            collider_groups: vec![0, 1],
            center: Some(nodes["head"]),
        },
        SpringChain {
            name: "hair B".into(),
            joints: vec![joint("hairB1", 1.0), joint("hairB2", 1.0)],
            collider_groups: vec![1],
            center: None,
        },
    ];
    av.constraints = vec![
        NodeConstraint {
            node: nodes["leftTwist"],
            kind: ConstraintKind::Roll { source: nodes["leftLowerArm"], axis: RollAxis::X },
            weight: 0.5,
        },
        NodeConstraint {
            node: nodes["leftSleeve"],
            kind: ConstraintKind::Aim { source: nodes["leftLowerArm"], axis: AimAxis::PositiveY },
            weight: 1.0,
        },
        NodeConstraint {
            node: nodes["rightSub"],
            kind: ConstraintKind::Rotation { source: nodes["rightLowerArm"] },
            weight: 0.25,
        },
    ];
    s.avatar = Some(av);

    let mut meta = VrmMeta::new("Fixture Avatar", vec!["Author One".into(), "Author Two".into()], VRM_LICENSE_URLS[0]);
    meta.version = Some("1.2".into());
    meta.copyright_information = Some("(c) Fixture".into());
    meta.contact_information = Some("fixture@example.org".into());
    meta.references = vec!["https://example.org/a".into(), "https://example.org/b".into()];
    meta.third_party_licenses = Some("none".into());
    meta.thumbnail_image = Some(thumb);
    meta.terms = UseTerms {
        avatar_permission: omuzaki_io_api::AvatarPermission::Everyone,
        allow_excessively_violent_usage: true,
        allow_excessively_sexual_usage: false,
        commercial_usage: CommercialUsage::Corporation,
        allow_political_or_religious_usage: true,
        allow_antisocial_or_hate_usage: false,
        credit_notation: CreditNotation::Unnecessary,
        allow_redistribution: true,
        modification: Modification::AllowModificationRedistribution,
    };
    meta.other_license_url = Some("https://example.org/terms".into());
    s.meta = meta;
    s.provenance = Some(provenance());
    s.passthrough = Passthrough {
        extensions: [("EXT_fixture_root".to_owned(), json!({"a": [1, 2]}))].into(),
        extras: Some(json!({"k": "v"})),
    };
    (s, Ids { nodes, body, hair: hair_node })
}

/// Export options for fixtures that don't carry the 52 face channels.
pub fn options() -> ExportOptions {
    ExportOptions::default().with_perfect_sync(PerfectSync::Off)
}

pub fn export(exporter: &dyn Exporter, scene: &IoScene) -> Vec<u8> {
    export_with(exporter, scene, &options()).expect("export").0
}

/// Export with `options`; the file and the outcome's report.
pub fn export_with(
    exporter: &dyn Exporter,
    scene: &IoScene,
    options: &ExportOptions,
) -> Result<(Vec<u8>, omuzaki_io_api::Report), IoError> {
    let mut out = run_export(exporter, scene, &ExportCtx::new(options))?;
    assert_eq!(out.files.len(), 1);
    Ok((out.files.remove(0).bytes, out.report))
}

pub fn import(importer: &dyn Importer, bytes: &[u8]) -> ImportOutcome {
    try_import(importer, bytes, &ImportOptions::default()).expect("import")
}

pub fn try_import(importer: &dyn Importer, bytes: &[u8], options: &ImportOptions) -> Result<ImportOutcome, IoError> {
    run_import(importer, &ImportInput::new(bytes).named("fixture.vrm"), &ImportCtx::new(options))
}

/// The JSON and binary chunks of a GLB, for tests that inspect or edit what an exporter wrote.
pub fn parts(glb: &[u8]) -> (Value, Vec<u8>) {
    let word = |at: usize| u32::from_le_bytes(glb[at..at + 4].try_into().unwrap()) as usize;
    let json_len = word(12);
    let json: Value = serde_json::from_slice(&glb[20..20 + json_len]).unwrap();
    let bin_at = 20 + json_len;
    let bin = if glb.len() > bin_at + 8 { glb[bin_at + 8..bin_at + 8 + word(bin_at)].to_vec() } else { Vec::new() };
    (json, bin)
}

/// Builds a GLB from a JSON document and a binary chunk, padding both.
pub fn glb(doc: &Value, bin: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(doc).unwrap();
    json.resize(json.len().next_multiple_of(4), b' ');
    let mut bin = bin.to_vec();
    bin.resize(bin.len().next_multiple_of(4), 0);
    let total = 12 + 8 + json.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let word = |n: usize| u32::try_from(n).unwrap().to_le_bytes();
    let mut out = b"glTF".to_vec();
    out.extend(word(2));
    out.extend(word(total));
    out.extend(word(json.len()));
    out.extend(b"JSON");
    out.extend(&json);
    if !bin.is_empty() {
        out.extend(word(bin.len()));
        out.extend(b"BIN\0");
        out.extend(&bin);
    }
    out
}

/// The fixture with everything VRM 0.x cannot hold taken out or made representable, so an export to 0.x and back can
/// be compared with it: no constraints, no overrides, plain meta terms, a single reference, one texture transform per
/// toon material, sphere colliders only, uniform spring joints, group names equal to their node names.
pub fn vrm0_friendly((mut s, ids): (IoScene, Ids)) -> (IoScene, Ids) {
    let av = s.avatar.as_mut().unwrap();
    av.constraints.clear();
    for e in av.expressions.values_mut() {
        (e.override_blink, e.override_look_at, e.override_mouth) = (Override::None, Override::None, Override::None);
        e.color_binds.retain(|b| b.kind != ColorBindKind::MatcapColor);
    }
    for g in &mut av.spring_bones.collider_groups {
        g.name = String::new();
    }
    av.spring_bones.colliders[1].shape = ColliderShape::Sphere { offset: Vec3::new(0.0, -0.05, 0.0), radius: 0.05 };
    av.spring_bones.colliders[2].shape = ColliderShape::Sphere { offset: Vec3::new(-0.1, 0.0, 0.0), radius: 0.08 };
    av.spring_bones.collider_groups[0].name = "head".into();
    av.spring_bones.collider_groups[1].name = "chest".into();
    for chain in &mut av.spring_bones.chains {
        let first = chain.joints[0];
        for j in &mut chain.joints {
            let node = j.node;
            *j = first;
            j.node = node;
        }
    }
    s.meta.references = vec!["https://example.org/a".into()];
    s.meta.authors = vec!["Author One".into()];
    s.meta.third_party_licenses = None;
    s.meta.terms = UseTerms {
        avatar_permission: omuzaki_io_api::AvatarPermission::Everyone,
        allow_excessively_violent_usage: true,
        allow_excessively_sexual_usage: false,
        commercial_usage: CommercialUsage::PersonalProfit,
        allow_political_or_religious_usage: false,
        allow_antisocial_or_hate_usage: false,
        credit_notation: CreditNotation::Required,
        allow_redistribution: false,
        modification: Modification::Prohibited,
    };
    for m in &mut s.materials {
        let Shading::Toon(t) = &mut m.shading else { continue };
        let main = m.base_color_texture.as_ref().and_then(|b| b.transform);
        for r in [
            &mut m.base_color_texture,
            &mut m.emissive_texture,
            &mut t.shade_multiply_texture,
            &mut t.rim_multiply_texture,
            &mut t.outline_width_texture,
            &mut t.uv_animation_mask,
        ]
        .into_iter()
        .flatten()
        {
            r.transform = main;
        }
        if let Some(n) = &mut m.normal_texture {
            n.texture.transform = main;
        }
        t.shading_shift_texture = None;
        t.rim_lighting_mix = 1.0;
        t.render_queue_offset = 0;
        t.matcap_color = if t.matcap_texture.is_some() { Vec3::ONE } else { Vec3::ZERO };
        // The shade texture falls back to the lit one on import, so make that explicit.
        if t.shade_multiply_texture.is_none() {
            t.shade_multiply_texture = m.base_color_texture;
        }
    }
    (s, ids)
}

/// Every numeric `Toon` field in a fixed order, for comparing two toon materials within a tolerance.
pub fn toon_numbers(t: &Toon) -> Vec<f32> {
    let mut v = Vec::new();
    for c in [t.shade_color, t.matcap_color, t.rim_color, t.outline_color] {
        v.extend(c.to_array());
    }
    v.extend([
        t.shading_shift,
        t.shading_toony,
        t.gi_equalization,
        t.rim_fresnel_power,
        t.rim_lift,
        t.rim_lighting_mix,
        t.outline_width,
        t.outline_lighting_mix,
        t.uv_scroll_x,
        t.uv_scroll_y,
        t.uv_rotation,
        t.render_queue_offset as f32,
        f32::from(u8::from(t.transparent_with_zwrite)),
        f32::from(u8::from(t.outline_mode != OutlineMode::None)),
    ]);
    v
}

// ---------------------------------------------------------------------------------------------------------------
// Minimal files, assembled by hand the way another tool would write them.

/// A minimal valid VRM 1.0: the 15 required bones as plain nodes, one triangle (positions only, no normals), and a
/// `VRMC_vrm` with the meta and humanoid the spec requires. Returns the document and its binary chunk.
pub fn minimal1() -> (Value, Vec<u8>) {
    let bones = [
        ("hips", None),
        ("spine", Some("hips")),
        ("head", Some("spine")),
        ("leftUpperLeg", Some("hips")),
        ("leftLowerLeg", Some("leftUpperLeg")),
        ("leftFoot", Some("leftLowerLeg")),
        ("rightUpperLeg", Some("hips")),
        ("rightLowerLeg", Some("rightUpperLeg")),
        ("rightFoot", Some("rightLowerLeg")),
        ("leftUpperArm", Some("spine")),
        ("leftLowerArm", Some("leftUpperArm")),
        ("leftHand", Some("leftLowerArm")),
        ("rightUpperArm", Some("spine")),
        ("rightLowerArm", Some("rightUpperArm")),
        ("rightHand", Some("rightLowerArm")),
    ];
    let index = |name: &str| bones.iter().position(|(n, _)| *n == name).unwrap();
    let mut nodes: Vec<Value> =
        bones.iter().map(|(name, _)| json!({"name": name, "translation": [0.0, 0.1, 0.0]})).collect();
    for (i, (_, parent)) in bones.iter().enumerate() {
        if let Some(p) = parent {
            let slot = &mut nodes[index(p)]["children"];
            if let Some(children) = slot.as_array_mut() {
                children.push(json!(i));
            } else {
                *slot = json!([i]);
            }
        }
    }
    nodes.push(json!({"name": "tri", "mesh": 0}));
    let human_bones: serde_json::Map<String, Value> =
        bones.iter().enumerate().map(|(i, (name, _))| ((*name).to_owned(), json!({"node": i}))).collect();
    let mut bin = Vec::new();
    for c in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend(c.to_le_bytes());
    }
    let doc = json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["VRMC_vrm"],
        "scene": 0,
        "scenes": [{"nodes": [0, 15]}],
        "nodes": nodes,
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]}],
        "bufferViews": [{"buffer": 0, "byteLength": 36}],
        "buffers": [{"byteLength": 36}],
        "extensions": {"VRMC_vrm": {
            "specVersion": "1.0",
            "meta": {"name": "Minimal", "authors": ["Someone"], "licenseUrl": "https://vrm.dev/licenses/1.0/"},
            "humanoid": {"humanBones": human_bones},
        }},
    });
    (doc, bin)
}

pub const BONES0: [(&str, Option<&str>); 17] = [
    ("hips", None),
    ("spine", Some("hips")),
    ("chest", Some("spine")),
    ("neck", Some("chest")),
    ("head", Some("neck")),
    ("leftUpperLeg", Some("hips")),
    ("leftLowerLeg", Some("leftUpperLeg")),
    ("leftFoot", Some("leftLowerLeg")),
    ("rightUpperLeg", Some("hips")),
    ("rightLowerLeg", Some("rightUpperLeg")),
    ("rightFoot", Some("rightLowerLeg")),
    ("leftUpperArm", Some("chest")),
    ("leftLowerArm", Some("leftUpperArm")),
    ("leftHand", Some("leftLowerArm")),
    ("rightUpperArm", Some("chest")),
    ("rightLowerArm", Some("rightUpperArm")),
    ("rightHand", Some("rightLowerArm")),
];

/// A VRM 0.x written the way `UniVRM` does: the 17 bones as plain nodes with distinct non-zero X and Z, one triangle
/// mesh on a node of its own, and a `VRM` extension. `extra` is merged into the extension.
pub fn minimal0(extra: &Value) -> (Value, Vec<u8>) {
    let index = |name: &str| BONES0.iter().position(|(n, _)| *n == name).unwrap();
    let mut nodes: Vec<Value> = BONES0
        .iter()
        .enumerate()
        .map(|(i, (name, _))| json!({"name": name, "translation": [0.1 * (i as f32 + 1.0), 0.2, 0.3 * (i as f32 + 1.0)]}))
        .collect();
    for (i, (_, parent)) in BONES0.iter().enumerate() {
        if let Some(p) = parent {
            let slot = &mut nodes[index(p)]["children"];
            if let Some(children) = slot.as_array_mut() {
                children.push(json!(i));
            } else {
                *slot = json!([i]);
            }
        }
    }
    nodes.push(json!({"name": "tri", "mesh": 0}));
    let mut bin = Vec::new();
    for c in [0.5f32, 0.0, 2.0, 1.0, 0.0, 2.0, 0.5, 1.0, 3.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0] {
        bin.extend(c.to_le_bytes());
    }
    let human_bones: Vec<Value> = BONES0
        .iter()
        .enumerate()
        .map(|(i, (name, _))| json!({"bone": name, "node": i, "useDefaultValues": true}))
        .collect();
    let mut vrm = json!({
        "exporterVersion": "UniVRM-0.99",
        "specVersion": "0.0",
        "meta": {"title": "Hand Made", "author": "Someone", "allowedUserName": "Everyone", "commercialUssageName": "Allow",
                 "licenseName": "CC_BY", "texture": -1},
        "humanoid": {"humanBones": human_bones},
    });
    vrm.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let doc = json!({
        "asset": {"version": "2.0", "generator": "UniGLTF-1.28"},
        "extensionsUsed": ["VRM"],
        "scene": 0,
        "scenes": [{"nodes": [0, 17]}],
        "nodes": nodes,
        "meshes": [{"name": "tri", "primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1}}]}],
        "materials": [{"name": "mat", "pbrMetallicRoughness": {"baseColorFactor": [1, 1, 1, 1]}}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0, 0, 2], "max": [1, 1, 3]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
        ],
        "bufferViews": [{"buffer": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 36}],
        "buffers": [{"byteLength": 72}],
        "extensions": {"VRM": vrm},
    });
    (doc, bin)
}

/// The fixture plus a face mesh with the 52 `ARKit` channels: one morph target per channel (each moves one vertex of an
/// 8 x 8 grid), and a custom expression per channel that drives it, named exactly like Apple's key. The shape of
/// what the face baker produces.
pub fn with_arkit52((mut s, mut ids): (IoScene, Ids)) -> (IoScene, Ids) {
    let mut prim = Primitive::new(grid(8, 8, 1));
    prim.mesh.joints.clear();
    prim.mesh.weights.clear();
    let n = prim.mesh.positions.len();
    prim.morph_targets = (0..omuzaki_io_api::ARKIT_52.len())
        .map(|i| {
            let mut deltas = vec![Vec3::ZERO; n];
            deltas[i + 1] = Vec3::new(0.0, 0.01 * (i + 1) as f32, 0.0);
            MorphTarget::new(deltas)
        })
        .collect();
    let mut mesh = Mesh::new("Face");
    mesh.primitives = vec![prim];
    mesh.morph_target_names = omuzaki_io_api::ARKIT_52.iter().map(|n| (*n).to_owned()).collect();
    let mesh = s.add_mesh(mesh);
    let mut face = Node::new("Face");
    face.mesh = Some(mesh);
    let face = s.add_node(face);
    s.attach(ids.node("head"), face);
    let av = s.avatar.as_mut().unwrap();
    for (i, name) in omuzaki_io_api::ARKIT_52.iter().enumerate() {
        av.expressions.insert(ExpressionName::parse(name).unwrap(), Expression::morph(face, u32::try_from(i).unwrap()));
    }
    ids.nodes.insert("Face", face);
    (s, ids)
}

// ---------------------------------------------------------------------------------------------------------------
// Editing hand-made files.

/// Appends `bytes` to the file's binary chunk as a new buffer view and returns the view's index.
pub fn add_view(doc: &mut Value, bin: &mut Vec<u8>, bytes: &[u8]) -> usize {
    bin.resize(bin.len().next_multiple_of(4), 0);
    let offset = bin.len();
    bin.extend_from_slice(bytes);
    doc["buffers"][0]["byteLength"] = json!(bin.len());
    let views = doc["bufferViews"].as_array_mut().unwrap();
    views.push(json!({"buffer": 0, "byteOffset": offset, "byteLength": bytes.len()}));
    views.len() - 1
}

pub fn add_accessor(doc: &mut Value, accessor: &Value) -> usize {
    let list = doc["accessors"].as_array_mut().unwrap();
    list.push(accessor.clone());
    list.len() - 1
}

pub fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// A skinned triangle on a new node, with `extra` merged into the primitive's attributes. Returns the document and bin.
pub fn skinned(attributes: &Value, buffers: &[(&str, usize, Vec<u8>, &str, &str)]) -> (Value, Vec<u8>) {
    let (mut doc, mut bin) = minimal1();
    let mut attrs = json!({"POSITION": 0});
    for (name, count, bytes, component, kind) in buffers {
        let view = add_view(&mut doc, &mut bin, bytes);
        let acc = add_accessor(
            &mut doc,
            &json!({"bufferView": view, "componentType": component.parse::<u32>().unwrap(), "normalized": *component == "5121", "count": count, "type": kind}),
        );
        attrs[name] = json!(acc);
    }
    for (k, v) in attributes.as_object().unwrap() {
        attrs[k] = v.clone();
    }
    doc["meshes"][0]["primitives"][0]["attributes"] = attrs;
    doc["skins"] = json!([{"joints": [0, 1, 2, 3, 4, 5, 6, 7]}]);
    doc["nodes"][15]["skin"] = json!(0);
    (doc, bin)
}

// ---------------------------------------------------------------------------------------------------------------
// Rights fixtures: files whose meta is the subject of the test.

/// A minimal VRM 1.0 whose `meta` has the members of `patch` merged in. It has `chest` and `neck` as well, so it can
/// also be exported as 0.x, which requires them.
pub fn vrm1_with_meta(patch: &Value) -> Vec<u8> {
    let (mut doc, bin) = minimal1();
    // hips > spine > chest > (neck > head, both upper arms)
    let nodes = doc["nodes"].as_array_mut().unwrap();
    nodes.push(json!({"name": "chest", "translation": [0.0, 0.1, 0.0], "children": [17, 9, 12]}));
    nodes.push(json!({"name": "neck", "translation": [0.0, 0.1, 0.0], "children": [2]}));
    nodes[1]["children"] = json!([16]);
    let bones = doc["extensions"]["VRMC_vrm"]["humanoid"]["humanBones"].as_object_mut().unwrap();
    bones.insert("chest".to_owned(), json!({"node": 16}));
    bones.insert("neck".to_owned(), json!({"node": 17}));
    let meta = doc["extensions"]["VRMC_vrm"]["meta"].as_object_mut().unwrap();
    for (k, v) in patch.as_object().unwrap() {
        meta.insert(k.clone(), v.clone());
    }
    glb(&doc, &bin)
}

/// A minimal VRM 0.x whose `meta` is exactly `meta`.
pub fn vrm0_with_meta(meta: &Value) -> Vec<u8> {
    let (mut doc, bin) = minimal0(&json!({}));
    doc["extensions"]["VRM"]["meta"] = meta.clone();
    glb(&doc, &bin)
}

/// The `meta` object of the VRM extension `extension` (`VRM` or `VRMC_vrm`) of an exported file.
pub fn written_meta(file: &[u8], extension: &str) -> Value {
    parts(file).0["extensions"][extension]["meta"].clone()
}
