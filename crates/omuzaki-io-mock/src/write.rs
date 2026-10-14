//! [`IoScene`] to glTF 2.0 (GLB), for the subset the mock supports. Output is deterministic: arrays are packed in a
//! fixed order, JSON keys come from structs and sorted maps, and nothing depends on time or addresses.

use omuzaki_core::glam::{Mat4, Vec2, Vec3};
use omuzaki_io_api::{
    AlphaMode, Finding, IoScene, Material, MaterialIx, Mesh, MeshIx, NodeIx, Report, Shading, SkinIx,
};

use crate::json::{
    ARRAY_BUFFER, Accessor, Asset, Buffer, BufferView, ELEMENT_ARRAY_BUFFER, GMaterial, GMesh, GNode, GPrimitive,
    GScene, GSkin, Gltf, MeshExtras, Pbr, component,
};

/// A serialised scene: the JSON model and the binary buffer it indexes.
pub(crate) struct Written {
    pub gltf: Gltf,
    pub bin: Vec<u8>,
}

/// Packs accessor data into one buffer, one view per accessor, each 4-byte aligned.
#[derive(Default)]
struct Packer {
    bin: Vec<u8>,
    views: Vec<BufferView>,
    accessors: Vec<Accessor>,
}

impl Packer {
    fn add(
        &mut self,
        bytes: &[u8],
        component_type: u32,
        kind: &str,
        count: usize,
        target: Option<u32>,
        bounds: Option<(Vec<f32>, Vec<f32>)>,
    ) -> usize {
        self.bin.resize(self.bin.len().next_multiple_of(4), 0);
        let byte_offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        self.views.push(BufferView { buffer: 0, byte_offset, byte_length: bytes.len(), byte_stride: None, target });
        let (min, max) = bounds.unzip();
        self.accessors.push(Accessor {
            buffer_view: Some(self.views.len() - 1),
            byte_offset: 0,
            component_type,
            count,
            kind: kind.to_owned(),
            min,
            max,
            sparse: None,
        });
        self.accessors.len() - 1
    }

    fn floats(
        &mut self,
        data: impl Iterator<Item = f32>,
        kind: &str,
        count: usize,
        bounds: Option<(Vec<f32>, Vec<f32>)>,
    ) -> usize {
        let bytes: Vec<u8> = data.flat_map(f32::to_le_bytes).collect();
        self.add(&bytes, component::F32, kind, count, Some(ARRAY_BUFFER), bounds)
    }

    /// A `POSITION`-style accessor: glTF requires min and max on these.
    fn positions(&mut self, data: &[Vec3]) -> usize {
        self.floats(data.iter().flat_map(Vec3::to_array), "VEC3", data.len(), Some(bounds(data)))
    }
}

/// Per-component minimum and maximum, as the spec requires for `POSITION` accessors.
fn bounds(data: &[Vec3]) -> (Vec<f32>, Vec<f32>) {
    let Some(first) = data.first() else { return (vec![0.0; 3], vec![0.0; 3]) };
    let (min, max) = data.iter().fold((*first, *first), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    (min.to_array().to_vec(), max.to_array().to_vec())
}

/// Serialises `scene`; findings say what the mock had to leave out.
pub(crate) fn write(scene: &IoScene, generator: Option<&str>, report: &mut Report) -> Written {
    let mut pack = Packer::default();
    let mut gltf = Gltf {
        asset: Asset {
            version: "2.0".into(),
            generator: generator.map(str::to_owned),
            copyright: scene.meta.copyright_information.clone(),
        },
        ..Gltf::default()
    };
    gltf.nodes = scene.nodes.iter().map(node).collect();
    gltf.meshes = scene.meshes.iter().enumerate().map(|(i, m)| mesh(&mut pack, m, i, report)).collect();
    gltf.skins = scene.skins.iter().map(|s| skin(&mut pack, s)).collect();
    gltf.materials = scene.materials.iter().enumerate().map(|(i, m)| material(m, i, report)).collect();
    if !scene.images.is_empty() {
        report.push(
            Finding::warning("gltf.images_dropped", "the mock exporter doesn't write images or textures").at("images"),
        );
    }
    gltf.extensions.clone_from(&scene.passthrough.extensions);
    gltf.extras.clone_from(&scene.passthrough.extras);
    // Every passed-through extension must be listed as used, in a stable order.
    let names: std::collections::BTreeSet<&String> = gltf
        .extensions
        .keys()
        .chain(gltf.nodes.iter().flat_map(|n| n.extensions.keys()))
        .chain(gltf.materials.iter().flat_map(|m| m.extensions.keys()))
        .collect();
    gltf.extensions_used = names.into_iter().cloned().collect();
    gltf.scenes = vec![GScene { nodes: scene.roots.iter().map(|r| r.index()).collect() }];
    gltf.scene = Some(0);
    gltf.accessors = pack.accessors;
    gltf.buffer_views = pack.views;
    if !pack.bin.is_empty() {
        gltf.buffers = vec![Buffer { byte_length: pack.bin.len(), uri: None }];
    }
    Written { gltf, bin: pack.bin }
}

fn node(n: &omuzaki_io_api::Node) -> GNode {
    let t = &n.transform;
    GNode {
        name: n.name.clone(),
        children: n.children.iter().map(|c| c.index()).collect(),
        mesh: n.mesh.map(MeshIx::index),
        skin: n.skin.map(SkinIx::index),
        translation: (t.translation != Vec3::ZERO).then(|| t.translation.to_array()),
        rotation: (t.rotation != omuzaki_core::glam::Quat::IDENTITY).then(|| t.rotation.to_array()),
        scale: (t.scale != Vec3::ONE).then(|| t.scale.to_array()),
        matrix: None,
        extensions: n.passthrough.extensions.clone(),
        extras: n.passthrough.extras.clone(),
    }
}

fn mesh(pack: &mut Packer, m: &Mesh, index: usize, report: &mut Report) -> GMesh {
    let primitives = m.primitives.iter().map(|p| primitive(pack, p, &index.to_string(), report)).collect();
    let extras = (!m.morph_target_names.is_empty()).then(|| MeshExtras { target_names: m.morph_target_names.clone() });
    GMesh { name: m.name.clone(), primitives, extras }
}

fn primitive(pack: &mut Packer, p: &omuzaki_io_api::Primitive, index: &str, report: &mut Report) -> GPrimitive {
    let m = &p.mesh;
    let n = m.positions.len();
    let mut out = GPrimitive { material: p.material.map(MaterialIx::index), ..GPrimitive::default() };
    out.attributes.insert("POSITION".into(), pack.positions(&m.positions));
    out.attributes.insert("NORMAL".into(), pack.floats(m.normals.iter().flat_map(Vec3::to_array), "VEC3", n, None));
    if !m.uvs.is_empty() {
        out.attributes.insert("TEXCOORD_0".into(), pack.floats(m.uvs.iter().flat_map(Vec2::to_array), "VEC2", n, None));
    }
    if !m.joints.is_empty() {
        let bytes: Vec<u8> = m.joints.iter().flatten().flat_map(|j| j.to_le_bytes()).collect();
        out.attributes.insert("JOINTS_0".into(), pack.add(&bytes, component::U16, "VEC4", n, Some(ARRAY_BUFFER), None));
        out.attributes.insert("WEIGHTS_0".into(), pack.floats(m.weights.iter().flatten().copied(), "VEC4", n, None));
    }
    if !m.colours.is_empty() {
        let msg = "the mock exporter doesn't write vertex colours";
        report.push(Finding::warning("gltf.vertex_colours_dropped", msg).at(format!("meshes/{index}")));
    }
    let bytes: Vec<u8> = m.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
    out.indices = Some(pack.add(&bytes, component::U32, "SCALAR", m.indices.len(), Some(ELEMENT_ARRAY_BUFFER), None));
    for t in &p.morph_targets {
        let mut target = std::collections::BTreeMap::new();
        target.insert("POSITION".to_owned(), pack.positions(&t.positions));
        if !t.normals.is_empty() {
            target.insert("NORMAL".to_owned(), pack.floats(t.normals.iter().flat_map(Vec3::to_array), "VEC3", n, None));
        }
        out.targets.push(target);
    }
    out
}

fn skin(pack: &mut Packer, s: &omuzaki_io_api::Skin) -> GSkin {
    let ibm = (!s.inverse_bind.is_empty()).then(|| {
        let n = s.inverse_bind.len();
        pack.floats(s.inverse_bind.iter().flat_map(Mat4::to_cols_array), "MAT4", n, None)
    });
    GSkin {
        name: s.name.clone(),
        joints: s.joints.iter().map(|j| j.index()).collect(),
        inverse_bind_matrices: ibm,
        skeleton: s.skeleton.map(NodeIx::index),
    }
}

fn material(m: &Material, index: usize, report: &mut Report) -> GMaterial {
    let (metallic, roughness) = match &m.shading {
        Shading::Pbr(p) => (p.metallic, p.roughness),
        Shading::Unlit | Shading::Toon(_) => {
            report.push(
                Finding::warning("gltf.toon_dropped", "unlit and toon shading are written as plain PBR by the mock")
                    .at(format!("materials/{index}")),
            );
            (0.0, 1.0)
        }
    };
    let (alpha_mode, alpha_cutoff) = match m.alpha {
        AlphaMode::Opaque => (None, None),
        AlphaMode::Mask { cutoff } => (Some("MASK".to_owned()), Some(cutoff)),
        AlphaMode::Blend => (Some("BLEND".to_owned()), None),
    };
    GMaterial {
        name: m.name.clone(),
        pbr_metallic_roughness: Pbr {
            base_color_factor: Some(m.base_color.to_array()),
            metallic_factor: Some(metallic),
            roughness_factor: Some(roughness),
        },
        emissive_factor: (m.emissive != Vec3::ZERO).then(|| m.emissive.to_array()),
        alpha_mode,
        alpha_cutoff,
        double_sided: m.double_sided,
        extensions: m.passthrough.extensions.clone(),
        extras: m.passthrough.extras.clone(),
    }
}
