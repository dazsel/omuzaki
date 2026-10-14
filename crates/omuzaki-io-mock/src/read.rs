//! glTF 2.0 (GLB) to [`IoScene`], for the subset the mock supports. Everything it can't carry is either refused with
//! `Unsupported` (when the result would be wrong) or dropped with a finding (when the result is merely less).

use std::collections::BTreeMap;

use omuzaki_core::glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    AlphaMode, Finding, IoError, IoScene, Limits, Material, Mesh, MeshData, MorphTarget, Node, NodeIx, Passthrough,
    Pbr, Primitive, Report, Shading, Skin,
};
use serde_json::Value;

use crate::glb;
use crate::json::{Accessor, GMaterial, GNode, GPrimitive, GSkin, Gltf, component};

/// Extensions the mock understands; anything else in `extensionsRequired` is refused.
pub(crate) const SUPPORTED_EXTENSIONS: [&str; 2] = [crate::vrm::EXTENSION, crate::spring::EXTENSION];

/// A parsed file: the typed JSON (so the VRM layer can read its extension) and the decoded scene.
pub(crate) struct Parsed {
    pub gltf: Gltf,
    pub scene: IoScene,
}

/// Reads a GLB.
pub(crate) fn read(bytes: &[u8], limits: &Limits, report: &mut Report) -> Result<Parsed, IoError> {
    let (json, bin) = glb::split(bytes)?;
    let gltf: Gltf = serde_json::from_slice(json)
        .map_err(|e| IoError::Malformed { message: e.to_string(), at: Some("JSON chunk".into()) })?;
    if !gltf.asset.version.starts_with('2') {
        return Err(IoError::unsupported(format!("glTF version {:?}", gltf.asset.version), "only 2.x is supported"));
    }
    if let Some(ext) = gltf.extensions_required.iter().find(|e| !SUPPORTED_EXTENSIONS.contains(&e.as_str())) {
        return Err(IoError::unsupported(
            format!("required extension {ext}"),
            "remove it in the authoring tool, or add support",
        ));
    }
    if !gltf.images.is_empty() {
        report.push(
            Finding::warning("gltf.images_dropped", "the mock importer doesn't read images or textures").at("/images"),
        );
    }
    if gltf.nodes.len() > limits.max_nodes as usize {
        return Err(IoError::Limit {
            what: "node count",
            limit: u64::from(limits.max_nodes),
            got: gltf.nodes.len() as u64,
        });
    }
    let data = Data { gltf: &gltf, bin, max_items: limits.max_vertices as usize };
    let mut scene = IoScene::new();
    for (i, n) in gltf.nodes.iter().enumerate() {
        scene.add_node(node(n, i)?);
    }
    for (i, m) in gltf.meshes.iter().enumerate() {
        scene.add_mesh(mesh(&data, m, i, report)?);
    }
    for s in &gltf.skins {
        scene.add_skin(skin(&data, s)?);
    }
    for m in &gltf.materials {
        scene.add_material(material(m));
    }
    scene.roots = roots(&gltf);
    scene.source_generator.clone_from(&gltf.asset.generator);
    scene.meta.copyright_information.clone_from(&gltf.asset.copyright);
    scene.passthrough = passthrough(&gltf.extensions, gltf.extras.as_ref());
    report_dropped_extensions(&gltf, &scene, report);
    Ok(Parsed { gltf, scene })
}

/// The bag for one object: every extension the mock doesn't model, plus the object's extras, verbatim.
fn passthrough(extensions: &BTreeMap<String, Value>, extras: Option<&Value>) -> Passthrough {
    Passthrough {
        extensions: extensions
            .iter()
            .filter(|(name, _)| !SUPPORTED_EXTENSIONS.contains(&name.as_str()))
            .map(|(name, v)| (name.clone(), v.clone()))
            .collect(),
        extras: extras.cloned(),
    }
}

/// An extension the file lists as used that ended up in no bag (on a mesh, a primitive, a texture…) was not kept.
fn report_dropped_extensions(gltf: &Gltf, scene: &IoScene, report: &mut Report) {
    let kept = |name: &String| {
        scene.passthrough.extensions.contains_key(name)
            || scene.nodes.iter().any(|n| n.passthrough.extensions.contains_key(name))
            || scene.materials.iter().any(|m| m.passthrough.extensions.contains_key(name))
    };
    for ext in gltf.extensions_used.iter().filter(|e| !SUPPORTED_EXTENSIONS.contains(&e.as_str()) && !kept(e)) {
        report.push(
            Finding::info("gltf.extension_dropped", format!("extension {ext} is not supported and was dropped"))
                .at(format!("/extensionsUsed/{ext}")),
        );
    }
}

fn node(n: &GNode, i: usize) -> Result<Node, IoError> {
    if n.matrix.is_some() {
        return Err(IoError::unsupported(
            format!("matrix on node {i}"),
            "decompose it into translation, rotation and scale",
        ));
    }
    let mut out = Node::new(n.name.clone());
    out.passthrough = passthrough(&n.extensions, n.extras.as_ref());
    out.children = n.children.iter().map(|&c| ix(c)).collect();
    out.mesh = n.mesh.map(|m| omuzaki_io_api::MeshIx(u32_of(m)));
    out.skin = n.skin.map(|s| omuzaki_io_api::SkinIx(u32_of(s)));
    if let Some(t) = n.translation {
        out.transform.translation = Vec3::from_array(t);
    }
    if let Some(r) = n.rotation {
        // Importers normalise rotations: files in the wild are often a few ulps off unit length.
        let q = Quat::from_array(r);
        if !q.is_finite() || q.length_squared() == 0.0 {
            return Err(IoError::Malformed {
                message: "a rotation is zero or not finite".into(),
                at: Some(format!("/nodes/{i}/rotation")),
            });
        }
        out.transform.rotation = q.normalize();
    }
    if let Some(s) = n.scale {
        out.transform.scale = Vec3::from_array(s);
    }
    Ok(out)
}

fn ix(i: usize) -> NodeIx {
    NodeIx(u32_of(i))
}

/// Indices past `u32::MAX` can't be real; saturating makes them fail the scene's range check instead of wrapping.
fn u32_of(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn roots(gltf: &Gltf) -> Vec<NodeIx> {
    if let Some(s) = gltf.scenes.get(gltf.scene.unwrap_or(0)) {
        return s.nodes.iter().map(|&n| ix(n)).collect();
    }
    // No scene: the nodes nobody lists as a child.
    let children: std::collections::BTreeSet<usize> =
        gltf.nodes.iter().flat_map(|n| n.children.iter().copied()).collect();
    (0..gltf.nodes.len()).filter(|i| !children.contains(i)).map(ix).collect()
}

fn material(m: &GMaterial) -> Material {
    let p = &m.pbr_metallic_roughness;
    let mut out = Material::new(
        m.name.clone(),
        Shading::Pbr(Pbr {
            metallic: p.metallic_factor.unwrap_or(1.0),
            roughness: p.roughness_factor.unwrap_or(1.0),
            metallic_roughness_texture: None,
        }),
    );
    out.base_color = Vec4::from_array(p.base_color_factor.unwrap_or([1.0; 4]));
    out.emissive = Vec3::from_array(m.emissive_factor.unwrap_or([0.0; 3]));
    out.alpha = match m.alpha_mode.as_deref() {
        Some("BLEND") => AlphaMode::Blend,
        Some("MASK") => AlphaMode::Mask { cutoff: m.alpha_cutoff.unwrap_or(0.5) },
        _ => AlphaMode::Opaque,
    };
    out.double_sided = m.double_sided;
    out.passthrough = passthrough(&m.extensions, m.extras.as_ref());
    out
}

fn skin(d: &Data<'_>, s: &GSkin) -> Result<Skin, IoError> {
    let inverse_bind = match s.inverse_bind_matrices {
        Some(a) => d.floats(a, "MAT4")?.as_chunks::<16>().0.iter().map(Mat4::from_cols_array).collect(),
        None => vec![Mat4::IDENTITY; s.joints.len()],
    };
    let mut out = Skin::new(s.name.clone(), s.joints.iter().map(|&j| ix(j)).collect(), inverse_bind);
    out.skeleton = s.skeleton.map(ix);
    Ok(out)
}

fn mesh(d: &Data<'_>, m: &crate::json::GMesh, i: usize, report: &mut Report) -> Result<Mesh, IoError> {
    let mut out = Mesh::new(m.name.clone());
    for (p, prim) in m.primitives.iter().enumerate() {
        out.primitives.push(primitive(d, prim, &format!("/meshes/{i}/primitives/{p}"), report)?);
    }
    if let Some(extras) = &m.extras {
        out.morph_target_names.clone_from(&extras.target_names);
    }
    Ok(out)
}

fn primitive(d: &Data<'_>, p: &GPrimitive, at: &str, report: &mut Report) -> Result<Primitive, IoError> {
    if p.mode.is_some_and(|mode| mode != 4) {
        return Err(IoError::unsupported(
            format!("primitive mode {:?} at {at}", p.mode),
            "only triangle lists are supported",
        ));
    }
    let pos = p
        .attributes
        .get("POSITION")
        .ok_or_else(|| IoError::Malformed { message: "primitive has no POSITION".into(), at: Some(at.to_owned()) })?;
    let mut mesh = MeshData { positions: d.vec3s(*pos)?, ..MeshData::default() };
    for (name, &acc) in &p.attributes {
        match name.as_str() {
            "POSITION" => {}
            "NORMAL" => mesh.normals = d.vec3s(acc)?,
            "TEXCOORD_0" => {
                mesh.uvs = d.floats(acc, "VEC2")?.as_chunks::<2>().0.iter().map(|c| Vec2::from_array(*c)).collect();
            }
            "JOINTS_0" => mesh.joints = d.joints(acc)?,
            "WEIGHTS_0" => mesh.weights = d.floats(acc, "VEC4")?.as_chunks::<4>().0.to_vec(),
            other => report.push(
                Finding::info("gltf.attribute_dropped", format!("attribute {other} is not kept"))
                    .at(format!("{at}/attributes/{other}")),
            ),
        }
    }
    mesh.indices = match p.indices {
        Some(i) => d.indices(i)?,
        // An unindexed primitive is a plain triangle list; the shared mesh type is always indexed.
        None => (0..u32_of(mesh.positions.len())).collect(),
    };
    let mut out = Primitive::new(mesh);
    out.material = p.material.map(|m| omuzaki_io_api::MaterialIx(u32_of(m)));
    for t in &p.targets {
        let mut normals = Vec::new();
        let mut positions = Vec::new();
        for (name, &acc) in t {
            match name.as_str() {
                "POSITION" => positions = d.vec3s(acc)?,
                "NORMAL" => normals = d.vec3s(acc)?,
                other => report.push(
                    Finding::info("gltf.attribute_dropped", format!("morph attribute {other} is not kept"))
                        .at(format!("{at}/targets")),
                ),
            }
        }
        let mut target = MorphTarget::new(positions);
        target.normals = normals;
        out.morph_targets.push(target);
    }
    // The shared mesh type needs normals; glTF lets a file leave them out.
    if out.mesh.normals.is_empty() {
        if !out.generate_flat_normals() {
            return Err(IoError::Malformed {
                message: "vertex attributes or indices are inconsistent".into(),
                at: Some(at.to_owned()),
            });
        }
        let msg = "the file has no normals, so flat normals were generated (vertices are no longer shared)";
        report.push(Finding::info("gltf.normals_generated", msg).at(format!("{at}/attributes/NORMAL")));
    }
    Ok(out)
}

/// Bounds-checked access to accessor data. Every size is validated against the binary chunk and the item cap
/// *before* anything is allocated, which is the rule real importers must follow for untrusted files.
struct Data<'a> {
    gltf: &'a Gltf,
    bin: &'a [u8],
    max_items: usize,
}

fn components(kind: &str) -> Option<usize> {
    Some(match kind {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        "MAT4" => 16,
        _ => return None,
    })
}

fn size_of(component_type: u32) -> Option<usize> {
    match component_type {
        component::U8 => Some(1),
        component::U16 => Some(2),
        component::U32 | component::F32 => Some(4),
        _ => None,
    }
}

impl<'a> Data<'a> {
    /// The accessor's bytes, after checking type, count, offsets and that no interleaving or sparse data is involved.
    fn slice(&self, index: usize, kind: &str, allowed: &[u32]) -> Result<(&'a Accessor, &'a [u8]), IoError> {
        let bad = |message: String| IoError::Malformed { message, at: Some(format!("/accessors/{index}")) };
        let acc = self.gltf.accessors.get(index).ok_or_else(|| bad("no such accessor".into()))?;
        if acc.sparse.is_some() {
            return Err(IoError::unsupported(
                format!("sparse accessor {index}"),
                "the mock reads dense accessors only",
            ));
        }
        if acc.kind != kind || !allowed.contains(&acc.component_type) {
            return Err(bad(format!(
                "expected {kind} of component type {allowed:?}, found {} of {}",
                acc.kind, acc.component_type
            )));
        }
        if acc.count > self.max_items {
            return Err(IoError::Limit {
                what: "accessor element count",
                limit: self.max_items as u64,
                got: acc.count as u64,
            });
        }
        let elem = components(kind)
            .zip(size_of(acc.component_type))
            .map(|(n, s)| n * s)
            .ok_or_else(|| bad("unknown type".into()))?;
        let view_ix = acc
            .buffer_view
            .ok_or_else(|| IoError::unsupported("an accessor without a bufferView", "write real data"))?;
        let view = self.gltf.buffer_views.get(view_ix).ok_or_else(|| bad("no such bufferView".into()))?;
        if view.buffer != 0 || view.byte_stride.is_some_and(|s| s != elem && s != 0) {
            return Err(IoError::unsupported(
                "external buffers or interleaved vertex data",
                "the mock reads the GLB buffer, tightly packed",
            ));
        }
        let start = view.byte_offset.checked_add(acc.byte_offset).ok_or_else(|| bad("offset overflow".into()))?;
        let end = acc
            .count
            .checked_mul(elem)
            .and_then(|n| start.checked_add(n))
            .ok_or_else(|| bad("size overflow".into()))?;
        if end > view.byte_offset + view.byte_length || end > self.bin.len() {
            return Err(bad(format!(
                "data runs past the end of its buffer view or the binary chunk ({end} > {})",
                self.bin.len()
            )));
        }
        Ok((acc, &self.bin[start..end]))
    }

    fn floats(&self, index: usize, kind: &str) -> Result<Vec<f32>, IoError> {
        let (_, bytes) = self.slice(index, kind, &[component::F32])?;
        Ok(bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
    }

    fn vec3s(&self, index: usize) -> Result<Vec<Vec3>, IoError> {
        Ok(self.floats(index, "VEC3")?.as_chunks::<3>().0.iter().map(|c| Vec3::from_array(*c)).collect())
    }

    fn indices(&self, index: usize) -> Result<Vec<u32>, IoError> {
        let (acc, bytes) = self.slice(index, "SCALAR", &[component::U8, component::U16, component::U32])?;
        Ok(match acc.component_type {
            component::U8 => bytes.iter().map(|&b| u32::from(b)).collect(),
            component::U16 => bytes.as_chunks::<2>().0.iter().map(|c| u32::from(u16::from_le_bytes(*c))).collect(),
            _ => bytes.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect(),
        })
    }

    fn joints(&self, index: usize) -> Result<Vec<[u16; 4]>, IoError> {
        let (acc, bytes) = self.slice(index, "VEC4", &[component::U8, component::U16])?;
        Ok(if acc.component_type == component::U8 {
            bytes.as_chunks::<4>().0.iter().map(|c| c.map(u16::from)).collect()
        } else {
            bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| [0, 2, 4, 6].map(|o| u16::from_le_bytes([c[o], c[o + 1]])))
                .collect()
        })
    }
}
