//! Helpers shared by the integration tests: entry points, a hand-built glTF writer, a seeded scene generator and a
//! scene comparison that knows which fields a glTF file can't carry.

// Each test binary uses a different subset of these helpers.
// Generated test data is tiny, so the index casts below cannot truncate.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

use std::collections::HashMap;

use omuzaki_core::glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use omuzaki_core::id::Slug;
use omuzaki_core::rng::Rng;
use omuzaki_gltf::NoHooks;
use omuzaki_io_api::{
    AlphaMode, Blob, Confidence, EncodedImage, ExportCtx, ExportOptions, ExportOutcome, Exporter, ExtraAttribute,
    Filter, FormatInfo, ImageIx, ImageMime, ImportCtx, ImportInput, ImportOptions, ImportOutcome, Importer, IoError,
    IoScene, LicenceEvidence, Material, MaterialIx, Mesh, MeshData, MeshIx, MorphTarget, Node, NormalTexture,
    OutputFile, Passthrough, Pbr, Primitive, Report, Resolver, Sampler, Shading, Skin, TexRef, Texture, TextureIx,
    UvTransform, Wrap, run_export, run_import,
};
use omuzaki_io_mock::fixtures::{authored_provenance, options_without_face_channels};
use serde_json::{Value, json};

fn info() -> FormatInfo {
    FormatInfo { id: "glb", name: "glTF 2.0", extensions: &["glb", "gltf"], media_type: "model/gltf-binary" }
}

/// The library as an importer with no hooks: plain glTF, every extension carried, nothing refused beyond the
/// library's own rules. (`omuzaki-io-gltf` adds the policy of the registered format and tests that.)
pub struct PlainImporter;

impl Importer for PlainImporter {
    fn info(&self) -> FormatInfo {
        info()
    }

    fn sniff(&self, input: &ImportInput<'_>) -> Confidence {
        if omuzaki_gltf::probe(input.bytes).is_some() { Confidence::Possible } else { Confidence::No }
    }

    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        omuzaki_gltf::import(input, ctx, &mut NoHooks, LicenceEvidence::Absent)
    }
}

/// The library as an exporter with no hooks.
pub struct PlainExporter;

impl Exporter for PlainExporter {
    fn info(&self) -> FormatInfo {
        info()
    }

    fn check(&self, scene: &IoScene, _: &ExportOptions) -> Report {
        omuzaki_gltf::check(scene, &mut NoHooks)
    }

    fn export(&self, scene: &IoScene, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError> {
        omuzaki_gltf::export(scene, ctx, &mut NoHooks)
    }
}

/// A valid 1x1 RGBA PNG (70 bytes).
pub const PNG: [u8; 70] = [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49,
    0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64, 0x60, 0xF8, 0x5F, 0x0F, 0x00, 0x02, 0x87, 0x01, 0x80, 0xEB, 0x47, 0xBA,
    0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// Bytes with a JPEG signature: the importer identifies images by signature and never decodes them.
pub fn fake_jpeg(tag: u8) -> Vec<u8> {
    vec![0xFF, 0xD8, 0xFF, 0xE0, tag, 0xFF, 0xD9]
}

// ---------------------------------------------------------------- entry points

pub fn import_with(bytes: &[u8], options: &ImportOptions, resolver: &dyn Resolver) -> Result<ImportOutcome, IoError> {
    let ctx = ImportCtx::new(options).with_resolver(resolver);
    run_import(&PlainImporter, &ImportInput::new(bytes).named("test.glb"), &ctx)
}

pub fn try_import(bytes: &[u8]) -> Result<ImportOutcome, IoError> {
    import_with(bytes, &ImportOptions::default(), &omuzaki_io_api::NoResolver)
}

pub fn import(bytes: &[u8]) -> ImportOutcome {
    try_import(bytes).expect("import")
}

pub fn export_files(scene: &IoScene, options: &ExportOptions) -> Result<Vec<OutputFile>, IoError> {
    run_export(&PlainExporter, scene, &ExportCtx::new(options)).map(|o| o.files)
}

/// The scene as one GLB, with the options the fixtures use (no face channels expected).
pub fn export_glb(scene: &IoScene) -> Vec<u8> {
    let mut files = export_files(scene, &options_without_face_channels()).expect("export");
    assert_eq!(files.len(), 1);
    files.remove(0).bytes
}

/// A resolver backed by a map of file name to bytes, the way a host would serve a `.gltf`'s sidecars.
pub struct MapResolver(pub HashMap<String, Vec<u8>>);

impl Resolver for MapResolver {
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
        self.0
            .get(uri)
            .cloned()
            .ok_or_else(|| IoError::Resource { uri: uri.to_owned(), reason: "not in the test's file map".into() })
    }
}

// ---------------------------------------------------------------- hand-built glTF

/// A glTF document under construction, with its binary buffer. Edit `doc` directly for anything the helpers don't cover.
pub struct Builder {
    pub doc: Value,
    pub bin: Vec<u8>,
}

impl Builder {
    pub fn new() -> Self {
        Self { doc: json!({ "asset": { "version": "2.0" } }), bin: Vec::new() }
    }

    /// Appends `value` to the top-level array `name` and returns its index.
    pub fn push(&mut self, name: &str, value: Value) -> usize {
        let list = self.doc[name].take();
        let mut list = if list.is_null() { json!([]) } else { list };
        let array = list.as_array_mut().unwrap();
        array.push(value);
        let ix = array.len() - 1;
        self.doc[name] = list;
        ix
    }

    /// A buffer view over `bytes` (4-byte aligned in the buffer), optionally strided.
    pub fn view(&mut self, bytes: &[u8], stride: Option<usize>) -> usize {
        self.bin.resize(self.bin.len().next_multiple_of(4), 0);
        let mut view = json!({ "buffer": 0, "byteOffset": self.bin.len(), "byteLength": bytes.len() });
        if let Some(s) = stride {
            view["byteStride"] = json!(s);
        }
        self.bin.extend_from_slice(bytes);
        self.push("bufferViews", view)
    }

    /// An accessor over a view.
    pub fn accessor(&mut self, view: usize, component_type: u32, count: usize, kind: &str) -> usize {
        self.push(
            "accessors",
            json!({ "bufferView": view, "componentType": component_type, "count": count, "type": kind }),
        )
    }

    pub fn f32s(&mut self, data: &[f32], kind: &str) -> usize {
        let comps = match kind {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => 16,
        };
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&bytes, None);
        self.accessor(view, 5126, data.len() / comps, kind)
    }

    pub fn u16s(&mut self, data: &[u16], kind: &str) -> usize {
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&bytes, None);
        let comps = if kind == "SCALAR" { 1 } else { 4 };
        self.accessor(view, 5123, data.len() / comps, kind)
    }

    /// A one-triangle mesh in a node in the default scene; returns the primitive's JSON path for further edits.
    pub fn triangle(&mut self) -> usize {
        let pos = self.f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0], "VEC3");
        let idx = self.u16s(&[0, 1, 2], "SCALAR");
        let mesh =
            self.push("meshes", json!({ "primitives": [{ "attributes": { "POSITION": pos }, "indices": idx }] }));
        let node = self.push("nodes", json!({ "mesh": mesh }));
        self.doc["scene"] = json!(0);
        self.doc["scenes"] = json!([{ "nodes": [node] }]);
        mesh
    }

    /// Finishes the buffer description and returns the document.
    pub fn finish(&mut self) -> &mut Value {
        if !self.bin.is_empty() {
            self.doc["buffers"] = json!([{ "byteLength": self.bin.len() }]);
        }
        &mut self.doc
    }

    pub fn glb(&mut self) -> Vec<u8> {
        let bin = self.bin.clone();
        omuzaki_io_mock::fixtures::glb_from_json(self.finish(), &bin)
    }

    /// The document as text glTF whose buffer is the file `scene.bin`; returns the text and the sidecar map.
    pub fn gltf_with_sidecar(&mut self) -> (Vec<u8>, MapResolver) {
        let len = self.bin.len();
        self.doc["buffers"] = json!([{ "uri": "scene.bin", "byteLength": len }]);
        let files = HashMap::from([("scene.bin".to_owned(), self.bin.clone())]);
        (self.doc.to_string().into_bytes(), MapResolver(files))
    }
}

// ---------------------------------------------------------------- seeded scenes

const SAMPLERS: [Sampler; 4] = [
    Sampler { wrap_u: Wrap::Repeat, wrap_v: Wrap::Repeat, mag: Filter::Linear, min: Filter::Linear, mipmaps: true },
    Sampler {
        wrap_u: Wrap::ClampToEdge,
        wrap_v: Wrap::ClampToEdge,
        mag: Filter::Nearest,
        min: Filter::Nearest,
        mipmaps: false,
    },
    Sampler {
        wrap_u: Wrap::MirroredRepeat,
        wrap_v: Wrap::Repeat,
        mag: Filter::Linear,
        min: Filter::Nearest,
        mipmaps: true,
    },
    Sampler {
        wrap_u: Wrap::Repeat,
        wrap_v: Wrap::ClampToEdge,
        mag: Filter::Nearest,
        min: Filter::Linear,
        mipmaps: false,
    },
];

struct Gen(Rng);

impl Gen {
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.0.next_u64() % n.max(1) as u64).unwrap()
    }
    fn coin(&mut self) -> bool {
        self.0.next_u64() & 1 == 1
    }
    fn unit(&mut self) -> f32 {
        self.0.next_f32()
    }
    fn vec3(&mut self, lo: f32, hi: f32) -> Vec3 {
        Vec3::new(self.0.range_f32(lo, hi), self.0.range_f32(lo, hi), self.0.range_f32(lo, hi))
    }
    fn direction(&mut self) -> Vec3 {
        loop {
            let v = self.vec3(-1.0, 1.0);
            if v.length() > 0.1 {
                return v.normalize();
            }
        }
    }
    fn rotation(&mut self) -> Quat {
        let axis = self.direction();
        Quat::from_axis_angle(axis, self.0.range_f32(0.0, 6.0))
    }
}

/// A scene that exercises every part of the glTF mapping, from a seed: a node tree, meshes with several primitives,
/// UVs, vertex colours, skinning, morph targets with names, custom attributes, materials of both shadings with every
/// texture slot and alpha mode, textures with each sampler, PNG and JPEG images, bright emissive colours and pass-through
/// JSON. It passes `IoScene::check` and survives a glTF round trip exactly.
pub fn random_scene(seed: u64) -> IoScene {
    let mut g = Gen(Rng::new(seed));
    let mut scene = IoScene::new();
    scene.provenance = Some(authored_provenance());
    if g.coin() {
        scene.meta.copyright_information = Some(format!("(c) test {seed}"));
    }
    for i in 0..g.below(3) {
        let (mime, bytes) =
            if g.coin() { (ImageMime::Png, PNG.to_vec()) } else { (ImageMime::Jpeg, fake_jpeg(i as u8)) };
        scene.add_image(EncodedImage::new(format!("image{i}"), mime, Blob::new(bytes)));
    }
    if !scene.images.is_empty() {
        for _ in 0..g.below(5) {
            let image = ImageIx(g.below(scene.images.len()) as u32);
            scene.add_texture(Texture { image, sampler: SAMPLERS[g.below(SAMPLERS.len())] });
        }
    }
    for i in 0..=g.below(4) {
        let material = random_material(&mut g, &scene, i);
        scene.add_material(material);
    }
    let skin_joints = 1 + g.below(4);
    for i in 0..=g.below(3) {
        let mesh = random_mesh(&mut g, i, &scene, skin_joints);
        scene.add_mesh(mesh);
    }
    build_nodes(&mut g, &mut scene, skin_joints);
    if g.coin() {
        scene.passthrough.extras = Some(json!({ "pipeline": format!("v{seed}"), "tags": ["a", "b"] }));
        scene.passthrough.extensions.insert("EXT_studio_tag".into(), json!({ "shot": seed % 100, "refs": [0, 1] }));
    }
    scene
}

/// A 300-vertex mesh whose morph targets move a handful of vertices (so the writer stores them sparsely), plus one that
/// moves none (an accessor without data). Passes `IoScene::check` and survives a round trip exactly.
pub fn sparse_morph_scene() -> IoScene {
    let n = 300;
    let mut scene = IoScene::new();
    scene.provenance = Some(authored_provenance());
    let mut data = MeshData {
        positions: (0..n).map(|i| Vec3::new(i as f32 * 0.01, (i % 7) as f32, 0.0)).collect(),
        normals: vec![Vec3::Z; n],
        ..MeshData::default()
    };
    data.indices = (0..n as u32 - 2).flat_map(|i| [i, i + 1, i + 2]).collect();
    let mut prim = Primitive::new(data);
    for moved in [&[2usize, 150, 299][..], &[0], &[]] {
        let mut deltas = vec![Vec3::ZERO; n];
        for &v in moved {
            deltas[v] = Vec3::new(0.01 * (v as f32 + 1.0), -0.5, 0.25);
        }
        prim.morph_targets.push(MorphTarget::new(deltas));
    }
    let mut mesh = Mesh::new("sparse");
    mesh.primitives.push(prim);
    mesh.morph_target_names = vec!["a".into(), "b".into(), "none".into()];
    let mesh = scene.add_mesh(mesh);
    let mut node = Node::new("holder");
    node.mesh = Some(mesh);
    let node = scene.add_node(node);
    scene.roots = vec![node];
    scene
}

fn random_material(g: &mut Gen, scene: &IoScene, index: usize) -> Material {
    let shading = if g.below(5) == 0 {
        Shading::Unlit
    } else {
        let mut pbr = Pbr { metallic: g.unit(), roughness: g.unit(), metallic_roughness_texture: None };
        pbr.metallic_roughness_texture = random_texref(g, scene);
        Shading::Pbr(pbr)
    };
    let unlit = shading == Shading::Unlit;
    let mut m = Material::new(format!("material{index}"), shading);
    m.base_color = Vec4::new(g.unit(), g.unit(), g.unit(), g.unit());
    m.base_color_texture = random_texref(g, scene);
    let brightness = if g.coin() { 1.0 } else { 7.5 };
    // Unlit ignores emissive, and glTF validators flag the pair, so generated unlit materials carry none.
    m.emissive = if unlit || g.coin() { Vec3::ZERO } else { g.vec3(0.0, brightness) };
    m.emissive_texture = random_texref(g, scene);
    m.normal_texture = random_texref(g, scene).map(|texture| NormalTexture { texture, scale: 0.25 + g.unit() });
    m.alpha = match g.below(3) {
        0 => AlphaMode::Opaque,
        1 => AlphaMode::Mask { cutoff: g.unit() },
        _ => AlphaMode::Blend,
    };
    m.double_sided = g.coin();
    if g.below(3) == 0 {
        m.passthrough = Passthrough {
            extensions: [("KHR_materials_sheen".to_owned(), json!({ "sheenRoughnessFactor": 0.75 }))].into(),
            extras: Some(json!({ "slot": index })),
        };
    }
    m
}

fn random_texref(g: &mut Gen, scene: &IoScene) -> Option<TexRef> {
    if scene.textures.is_empty() || g.coin() {
        return None;
    }
    let mut t = TexRef::of(TextureIx(g.below(scene.textures.len()) as u32));
    if g.coin() {
        t.transform = Some(UvTransform {
            offset: Vec2::new(g.unit(), g.unit()),
            scale: Vec2::new(0.25 + g.unit(), 0.25 + g.unit()),
        });
    }
    Some(t)
}

fn random_mesh(g: &mut Gen, index: usize, scene: &IoScene, skin_joints: usize) -> Mesh {
    let mut mesh = Mesh::new(format!("mesh{index}"));
    let targets = g.below(4);
    let skinned = g.coin();
    for _ in 0..=g.below(2) {
        let mut p = random_primitive(g, targets, skinned.then_some(skin_joints));
        p.material =
            (!scene.materials.is_empty() && g.coin()).then(|| MaterialIx(g.below(scene.materials.len()) as u32));
        // A textured material needs UVs to sample with.
        let textured = p.material.is_some_and(|m| !scene.materials[m.index()].texture_refs().is_empty());
        if textured && p.mesh.uvs.is_empty() {
            p.mesh.uvs = (0..p.mesh.positions.len()).map(|_| Vec2::new(g.unit(), g.unit())).collect();
        }
        mesh.primitives.push(p);
    }
    if targets > 0 && g.coin() {
        mesh.morph_target_names = (0..targets).map(|t| format!("shape{t}")).collect();
    }
    mesh
}

fn random_primitive(g: &mut Gen, targets: usize, joints: Option<usize>) -> Primitive {
    let n = 3 + g.below(38);
    let normals = (0..n).map(|_| g.direction()).collect();
    let mut data = MeshData { positions: (0..n).map(|_| g.vec3(-2.0, 2.0)).collect(), normals, ..MeshData::default() };
    data.indices = (0..3 * (1 + g.below(12))).map(|_| g.below(n) as u32).collect();
    if g.coin() {
        data.uvs = (0..n).map(|_| Vec2::new(g.unit(), g.unit())).collect();
    }
    if g.coin() {
        data.colours = (0..n).map(|_| [g.unit(), g.unit(), g.unit(), g.unit()]).collect();
    }
    if let Some(count) = joints {
        (data.joints, data.weights) = (0..n).map(|_| skin_influences(g, count)).unzip();
    }
    if g.coin() {
        // One attribute name always has the same width, as in real files (Blender merges primitives by name).
        let width = 1 + g.below(4);
        let name = Slug::new(format!("strand_{width}")).unwrap();
        let values = (0..n * width).map(|_| g.unit()).collect();
        data.extra.push(ExtraAttribute { name, components: width as u8, data: values });
    }
    let mut p = Primitive::new(data);
    for _ in 0..targets {
        let mut t = MorphTarget::new((0..n).map(|_| g.vec3(-0.1, 0.1)).collect());
        if g.coin() {
            t.normals = (0..n).map(|_| g.vec3(-0.1, 0.1)).collect();
        }
        p.morph_targets.push(t);
    }
    p
}

/// Up to four distinct joints with weights summing to 1; unused slots are joint 0 with weight 0, as the spec says.
fn skin_influences(g: &mut Gen, joint_count: usize) -> ([u16; 4], [f32; 4]) {
    let mut pool: Vec<u16> = (0..joint_count as u16).collect();
    let used = pool.len().min(4);
    let (mut joints, mut weights) = ([0u16; 4], [0f32; 4]);
    for slot in 0..used {
        joints[slot] = pool.swap_remove(g.below(pool.len()));
        weights[slot] = g.unit() + 0.1;
    }
    let sum: f32 = weights.iter().sum();
    (joints, weights.map(|w| w / sum))
}

/// A forest: a chain of `skin_joints` joint nodes, a few ordinary nodes, and a root-level node (identity transform, as
/// glTF wants for skinned meshes) for every skinned mesh.
fn build_nodes(g: &mut Gen, scene: &mut IoScene, skin_joints: usize) {
    let transform = |g: &mut Gen, node: &mut Node| {
        node.transform.translation = if g.coin() { Vec3::ZERO } else { g.vec3(-3.0, 3.0) };
        node.transform.rotation = if g.coin() { Quat::IDENTITY } else { g.rotation() };
        node.transform.scale = if g.coin() { Vec3::ONE } else { g.vec3(0.5, 2.0) };
    };
    let mut roots = Vec::new();
    let mut joints = Vec::new();
    for i in 0..skin_joints {
        let mut node = Node::new(format!("joint{i}"));
        transform(g, &mut node);
        let ix = scene.add_node(node);
        match joints.last() {
            Some(&parent) => scene.attach(parent, ix),
            None => roots.push(ix),
        }
        joints.push(ix);
    }
    let mut ordinary = Vec::new();
    for i in 0..=g.below(5) {
        let mut node = Node::new(if g.below(4) == 0 { String::new() } else { format!("node{i}") });
        transform(g, &mut node);
        if g.below(4) == 0 {
            node.passthrough.extras = Some(json!({ "rig": i }));
        }
        let ix = scene.add_node(node);
        match ordinary.last().filter(|_| g.below(3) != 0) {
            Some(&parent) => scene.attach(parent, ix),
            None => roots.push(ix),
        }
        ordinary.push(ix);
    }
    let ibm = joints
        .iter()
        .map(|_| Mat4::from_scale_rotation_translation(g.vec3(0.5, 2.0), g.rotation(), g.vec3(-1.0, 1.0)))
        .collect();
    let mut skin = Skin::new("skin", joints.clone(), ibm);
    skin.skeleton = g.coin().then_some(joints[0]);
    let skin = scene.add_skin(skin);
    for m in 0..scene.meshes.len() {
        let skinned = scene.meshes[m].primitives.iter().any(|p| p.mesh.is_skinned());
        let mut holder = Node::new(format!("mesh node {m}"));
        holder.mesh = Some(MeshIx(m as u32));
        if skinned {
            holder.skin = Some(skin);
            roots.push(scene.add_node(holder));
        } else {
            transform(g, &mut holder);
            let ix = scene.add_node(holder);
            let parent = ordinary[g.below(ordinary.len())];
            scene.attach(parent, ix);
        }
    }
    scene.roots = roots;
}

/// Everything a glTF round trip must preserve, compared field by field so a failure names the field.
pub fn assert_same_scene(got: &IoScene, want: &IoScene) {
    assert_eq!(got.nodes, want.nodes, "nodes");
    assert_eq!(got.roots, want.roots, "roots");
    assert_eq!(got.meshes, want.meshes, "meshes");
    assert_eq!(got.skins, want.skins, "skins");
    assert_same_materials(&got.materials, &want.materials);
    assert_eq!(got.textures, want.textures, "textures");
    assert_eq!(got.images, want.images, "images");
    assert_eq!(got.passthrough, want.passthrough, "root pass-through");
    assert_eq!(got.meta.copyright_information, want.meta.copyright_information, "copyright");
}

/// Materials are compared exactly except for a bright emissive colour: glTF stores it as a factor times a strength (its
/// peak), the product rounds, and a colour that no factor reaches reads back a few ulps higher (see
/// `omuzaki_gltf::split_emissive`). Four ulps per channel is the allowance; the rest of the material must be identical.
fn assert_same_materials(got: &[Material], want: &[Material]) {
    assert_eq!(got.len(), want.len(), "material count");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        let near = g
            .emissive
            .to_array()
            .into_iter()
            .zip(w.emissive.to_array())
            .all(|(a, b)| (a - b).abs() <= 4.0 * f32::EPSILON * b.abs());
        let mut g = g.clone();
        if near {
            g.emissive = w.emissive;
        }
        assert_eq!(&g, w, "material {i}");
    }
}

/// Standard base64 with padding, for building `data:` URIs.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, &b)| acc | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The first finding with this rule, for assertions on severity and wording.
pub fn finding<'a>(report: &'a omuzaki_io_api::Report, rule: &str) -> &'a omuzaki_io_api::Finding {
    report.findings.iter().find(|f| f.rule == rule).unwrap_or_else(|| panic!("no {rule} in {report:?}"))
}
