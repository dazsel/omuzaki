//! A scene written as an ASCII FBX 7.4 file, the way a DCC tool exports a rigged character: Maya-style axes (Y up, front
//! +Z) in centimetres, bones as `LimbNode` models with Euler rotations, each mesh one geometry with a polygon per triangle
//! and a material slot per polygon, skin clusters whose `Transform` is the mesh-to-bone matrix at bind (as Blender and
//! ufbx read it), morph targets as blend shape channels, and textures as files beside the model. No humanoid block: FBX
//! has none, so the importer's auto-mapping has to find the bones from their names. Test-only; nothing ships it.

#![allow(dead_code, clippy::expect_used, clippy::too_many_lines, clippy::many_single_char_names)]

use std::collections::BTreeMap;
use std::fmt::Write;

use omuzaki_core::glam::{EulerRot, Mat4, Vec3};
use omuzaki_io_api::{IoError, IoScene, Resolver};

/// Centimetres per metre: the file's unit is the centimetre (`UnitScaleFactor` 1).
const CM: f32 = 100.0;

/// The files the model refers to, by name.
pub struct Folder(pub BTreeMap<String, Vec<u8>>);

impl Resolver for Folder {
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
        self.0
            .get(uri)
            .cloned()
            .ok_or_else(|| IoError::Resource { uri: uri.to_owned(), reason: "not in the folder".to_owned() })
    }
}

/// `values` as an FBX array body.
fn array<T: std::fmt::Display>(values: impl IntoIterator<Item = T>) -> String {
    let values: Vec<String> = values.into_iter().map(|v| v.to_string()).collect();
    format!("*{} {{\n\t\t\ta: {}\n\t\t}}", values.len(), values.join(","))
}

/// `m` with its translation in centimetres (the linear part has no unit).
fn in_cm(m: Mat4) -> Mat4 {
    let s = Mat4::from_scale(Vec3::splat(CM));
    s * m * s.inverse()
}

/// `scene` as an FBX file and the image files beside it.
pub fn fbx(scene: &IoScene) -> (Vec<u8>, Folder) {
    let mut objects = String::new();
    let mut links: Vec<(i64, i64, Option<&str>)> = Vec::new();
    let mut next = 1_000_i64;
    let mut id = || {
        next += 1;
        next
    };
    let joints: std::collections::BTreeSet<usize> =
        scene.skins.iter().flat_map(|s| s.joints.iter().map(|j| j.index())).collect();
    let node_ids: Vec<i64> = scene.nodes.iter().map(|_| id()).collect();
    for (n, node) in scene.nodes.iter().enumerate() {
        let kind = if joints.contains(&n) {
            "LimbNode"
        } else if node.mesh.is_some() {
            "Mesh"
        } else {
            "Null"
        };
        let t = node.transform.translation * CM;
        let (z, y, x) = node.transform.rotation.to_euler(EulerRot::ZYX);
        let r = Vec3::new(x, y, z) * (180.0 / std::f32::consts::PI);
        let s = node.transform.scale;
        write!(
            objects,
            "\tModel: {}, \"Model::{}\", \"{kind}\" {{\n\t\tVersion: 232\n\t\tProperties70:  {{\n\
             \t\t\tP: \"Lcl Translation\", \"Lcl Translation\", \"\", \"A\",{},{},{}\n\
             \t\t\tP: \"Lcl Rotation\", \"Lcl Rotation\", \"\", \"A\",{},{},{}\n\
             \t\t\tP: \"Lcl Scaling\", \"Lcl Scaling\", \"\", \"A\",{},{},{}\n\t\t}}\n\t}}\n",
            node_ids[n], node.name, t.x, t.y, t.z, r.x, r.y, r.z, s.x, s.y, s.z
        )
        .expect("writes");
        let parent = scene
            .parent_of(omuzaki_io_api::NodeIx(u32::try_from(n).expect("small")))
            .map_or(0, |p| node_ids[p.index()]);
        links.push((node_ids[n], parent, None));
    }
    // Images as files, textures by image.
    let mut folder = BTreeMap::new();
    let image_names: Vec<String> = scene
        .images
        .iter()
        .enumerate()
        .map(|(i, image)| {
            let ext = if image.mime == omuzaki_io_api::ImageMime::Png { "png" } else { "jpg" };
            let name = format!("image{i}.{ext}");
            folder.insert(name.clone(), image.bytes.as_slice().to_vec());
            name
        })
        .collect();
    let material_ids: Vec<i64> = scene.materials.iter().map(|_| id()).collect();
    for (m, material) in scene.materials.iter().enumerate() {
        let c = material.base_color;
        write!(
            objects,
            "\tMaterial: {}, \"Material::{}\", \"\" {{\n\t\tVersion: 102\n\t\tShadingModel: \"lambert\"\n\t\tProperties70:  {{\n\
             \t\t\tP: \"DiffuseColor\", \"Color\", \"\", \"A\",{},{},{}\n\t\t\tP: \"DiffuseFactor\", \"Number\", \"\", \"A\",1\n\
             \t\t\tP: \"Opacity\", \"Number\", \"\", \"A\",{}\n\t\t}}\n\t}}\n",
            material_ids[m], material.name, c.x, c.y, c.z, c.w
        )
        .expect("writes");
        if let Some(tex) = &material.base_color_texture {
            let name = &image_names[scene.textures[tex.texture.index()].image.index()];
            let texture = id();
            write!(
                objects,
                "\tTexture: {texture}, \"Texture::{name}\", \"\" {{\n\t\tType: \"TextureVideoClip\"\n\t\tVersion: 202\n\
                 \t\tTextureName: \"Texture::{name}\"\n\t\tFileName: \"{name}\"\n\t\tRelativeFilename: \"{name}\"\n\t}}\n"
            )
            .expect("writes");
            links.push((texture, material_ids[m], Some("DiffuseColor")));
        }
    }
    for (n, node) in scene.nodes.iter().enumerate() {
        let Some(mesh) = node.mesh.map(|m| &scene.meshes[m.index()]) else { continue };
        let geometry = id();
        // The mesh's materials in slot order, connected to its model in that order.
        let mut slots: Vec<usize> = Vec::new();
        let (mut points, mut normals, mut uvs, mut polygons, mut slot_of) = (vec![], vec![], vec![], vec![], vec![]);
        let mut point_joints: Vec<([u16; 4], [f32; 4])> = Vec::new();
        for primitive in &mesh.primitives {
            let base = u32::try_from(points.len() / 3).expect("small");
            let m = &primitive.mesh;
            points.extend(m.positions.iter().flat_map(|p| (*p * CM).to_array()));
            normals.extend(m.normals.iter().flat_map(Vec3::to_array));
            uvs.extend(m.uvs.iter().flat_map(|uv| [uv.x, 1.0 - uv.y]));
            point_joints.extend(m.joints.iter().copied().zip(m.weights.iter().copied()));
            let slot = primitive.material.map_or(0, |mat| {
                slots.iter().position(|&s| s == mat.index()).unwrap_or_else(|| {
                    slots.push(mat.index());
                    slots.len() - 1
                })
            });
            for t in m.indices.as_chunks::<3>().0 {
                let [a, b, c] = t.map(|i| i64::from(base + i));
                polygons.extend([a, b, -c - 1]);
                slot_of.push(slot);
            }
        }
        let has_uvs = uvs.len() / 2 == points.len() / 3;
        let mut text = format!(
            "\tGeometry: {geometry}, \"Geometry::{}\", \"Mesh\" {{\n\t\tVertices: {}\n\t\tPolygonVertexIndex: {}\n\
             \t\tLayerElementNormal: 0 {{\n\t\t\tVersion: 101\n\t\t\tName: \"\"\n\t\t\tMappingInformationType: \"ByVertice\"\n\
             \t\t\tReferenceInformationType: \"Direct\"\n\t\t\tNormals: {}\n\t\t}}\n",
            mesh.name,
            array(points.iter()),
            array(polygons.iter()),
            array(normals.iter())
        );
        let mut layer =
            "\t\t\tLayerElement:  {\n\t\t\t\tType: \"LayerElementNormal\"\n\t\t\t\tTypedIndex: 0\n\t\t\t}\n".to_owned();
        if has_uvs {
            write!(
                text,
                "\t\tLayerElementUV: 0 {{\n\t\t\tVersion: 101\n\t\t\tName: \"map1\"\n\t\t\tMappingInformationType: \"ByVertice\"\n\
                 \t\t\tReferenceInformationType: \"Direct\"\n\t\t\tUV: {}\n\t\t}}\n",
                array(uvs.iter())
            )
            .expect("writes");
            layer
                .push_str("\t\t\tLayerElement:  {\n\t\t\t\tType: \"LayerElementUV\"\n\t\t\t\tTypedIndex: 0\n\t\t\t}\n");
        }
        write!(
            text,
            "\t\tLayerElementMaterial: 0 {{\n\t\t\tVersion: 101\n\t\t\tName: \"\"\n\t\t\tMappingInformationType: \"ByPolygon\"\n\
             \t\t\tReferenceInformationType: \"IndexToDirect\"\n\t\t\tMaterials: {}\n\t\t}}\n\t\tLayer: 0 {{\n\t\t\tVersion: 100\n{layer}\
             \t\t\tLayerElement:  {{\n\t\t\t\tType: \"LayerElementMaterial\"\n\t\t\t\tTypedIndex: 0\n\t\t\t}}\n\t\t}}\n\t}}\n",
            array(slot_of.iter())
        )
        .expect("writes");
        objects.push_str(&text);
        links.push((geometry, node_ids[n], None));
        for &s in &slots {
            links.push((material_ids[s], node_ids[n], None));
        }
        // Skin clusters.
        if let Some(skin) = node.skin.map(|s| &scene.skins[s.index()]) {
            let deformer = id();
            write!(objects, "\tDeformer: {deformer}, \"Deformer::Skin\", \"Skin\" {{\n\t\tVersion: 101\n\t}}\n")
                .expect("writes");
            links.push((deformer, geometry, None));
            for (j, (joint, ibm)) in skin.joints.iter().zip(&skin.inverse_bind).enumerate() {
                let (mut indexes, mut weights) = (Vec::new(), Vec::new());
                for (p, (js, ws)) in point_joints.iter().enumerate() {
                    let w: f32 = js.iter().zip(ws).filter(|(jj, _)| usize::from(**jj) == j).map(|(_, w)| *w).sum();
                    if w > 0.0 {
                        indexes.push(p);
                        weights.push(w);
                    }
                }
                let transform = in_cm(*ibm);
                let cluster = id();
                write!(
                    objects,
                    "\tDeformer: {cluster}, \"SubDeformer::{}\", \"Cluster\" {{\n\t\tVersion: 100\n\t\tUserData: \"\", \"\"\n\
                     \t\tIndexes: {}\n\t\tWeights: {}\n\t\tTransform: {}\n\t\tTransformLink: {}\n\t}}\n",
                    scene.nodes[joint.index()].name,
                    array(indexes.iter()),
                    array(weights.iter()),
                    array(transform.to_cols_array().iter()),
                    array(transform.inverse().to_cols_array().iter())
                )
                .expect("writes");
                links.push((cluster, deformer, None));
                links.push((node_ids[joint.index()], cluster, None));
            }
        }
        // Morph targets as blend shape channels, one shape each, over every point of the geometry.
        let targets = mesh.morph_target_count();
        if targets > 0 {
            let blend = id();
            write!(objects, "\tDeformer: {blend}, \"Deformer::Blend\", \"BlendShape\" {{\n\t\tVersion: 100\n\t}}\n")
                .expect("writes");
            links.push((blend, geometry, None));
            for t in 0..targets {
                let name = mesh.morph_target_names.get(t).cloned().unwrap_or_else(|| format!("target{t}"));
                let offsets: Vec<f32> = mesh
                    .primitives
                    .iter()
                    .flat_map(|p| match p.morph_targets.get(t) {
                        Some(target) => target.positions.iter().flat_map(|d| (*d * CM).to_array()).collect::<Vec<_>>(),
                        None => vec![0.0; p.mesh.positions.len() * 3],
                    })
                    .collect();
                let (channel, shape) = (id(), id());
                write!(
                    objects,
                    "\tDeformer: {channel}, \"SubDeformer::{name}\", \"BlendShapeChannel\" {{\n\t\tVersion: 100\n\t\tDeformPercent: 0\n\
                     \t\tFullWeights: *1 {{\n\t\t\ta: 100\n\t\t}}\n\t}}\n\
                     \tGeometry: {shape}, \"Geometry::{name}\", \"Shape\" {{\n\t\tVersion: 100\n\t\tIndexes: {}\n\t\tVertices: {}\n\t}}\n",
                    array(0..offsets.len() / 3),
                    array(offsets.iter())
                )
                .expect("writes");
                links.push((channel, blend, None));
                links.push((shape, channel, None));
            }
        }
    }
    let mut connections = String::new();
    for (child, parent, property) in links {
        match property {
            Some(p) => writeln!(connections, "\tC: \"OP\",{child},{parent}, \"{p}\""),
            None => writeln!(connections, "\tC: \"OO\",{child},{parent}"),
        }
        .expect("writes");
    }
    let axis = |name: &str, index: u8| {
        format!(
            "\t\tP: \"{name}Axis\", \"int\", \"Integer\", \"\",{index}\n\t\tP: \"{name}AxisSign\", \"int\", \"Integer\", \"\",1\n"
        )
    };
    let text = format!(
        "; FBX 7.4.0 project file\nFBXHeaderExtension:  {{\n\tFBXHeaderVersion: 1003\n\tFBXVersion: 7400\n\tCreator: \"Omuzaki test writer\"\n}}\n\
         GlobalSettings:  {{\n\tVersion: 1000\n\tProperties70:  {{\n{}{}{}\t\tP: \"UnitScaleFactor\", \"double\", \"Number\", \"\",1\n\t}}\n}}\n\
         Objects:  {{\n{objects}}}\nConnections:  {{\n{connections}}}\n",
        axis("Up", 1),
        axis("Front", 2),
        axis("Coord", 0)
    );
    (text.into_bytes(), Folder(folder))
}
