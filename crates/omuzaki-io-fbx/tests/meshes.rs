//! Meshes, skins, blend shapes and materials from FBX (EX-09): what a character file holds besides its nodes, in canonical
//! space. Every file is written by the test (`common::FbxBuilder` plus the objects below); no third-party model.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fmt::Write;

use common::{Axis, FbxBuilder, SCENE_ROOT};
use omuzaki_core::glam::{Mat4, Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    ImportCtx, ImportInput, ImportOptions, ImportOutcome, IoError, IoScene, MeshIx, NodeIx, Resolver, Wrap, run_import,
};
use omuzaki_io_fbx::FbxImporter;

fn import(bytes: &[u8]) -> ImportOutcome {
    run_import(&FbxImporter, &ImportInput::new(bytes), &ImportCtx::new(&ImportOptions::default())).expect("imports")
}

fn rules(outcome: &ImportOutcome) -> Vec<&str> {
    outcome.report.findings.iter().map(|f| f.rule.as_str()).collect()
}

/// A Maya-style file: Y up, front +Z, one unit is one centimetre.
fn maya() -> FbxBuilder {
    FbxBuilder::new(Axis::Y, Axis::Z, Axis::X, 1.0)
}

/// `values` as an FBX array: `*n { a: v,v,v }`.
fn array<T: std::fmt::Display>(values: &[T]) -> String {
    let joined = values.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
    format!("*{} {{\n\t\t\ta: {joined}\n\t\t}}", values.len())
}

/// A polygon mesh to write: control points, polygons (indices into the points), and optional per-corner normals and UVs
/// and per-polygon material slots.
#[derive(Default)]
struct Geometry {
    points: Vec<[f64; 3]>,
    polygons: Vec<Vec<i32>>,
    normals: Option<Vec<[f64; 3]>>,
    uvs: Option<Vec<[f64; 2]>>,
    materials: Option<Vec<i32>>,
}

impl Geometry {
    /// Writes the geometry, drawn by `model`, and returns its id.
    fn write(&self, file: &mut FbxBuilder, model: i64) -> i64 {
        let id = file.id();
        let points: Vec<f64> = self.points.iter().flatten().copied().collect();
        let indices: Vec<i32> = self
            .polygons
            .iter()
            .flat_map(|p| p.iter().enumerate().map(move |(k, &i)| if k + 1 == p.len() { -i - 1 } else { i }))
            .collect();
        let mut text = format!(
            "\tGeometry: {id}, \"Geometry::\", \"Mesh\" {{\n\t\tVertices: {}\n\t\tPolygonVertexIndex: {}\n",
            array(&points),
            array(&indices)
        );
        let mut layers = String::new();
        if let Some(normals) = &self.normals {
            let flat: Vec<f64> = normals.iter().flatten().copied().collect();
            write!(
                text,
                "\t\tLayerElementNormal: 0 {{\n\t\t\tVersion: 101\n\t\t\tName: \"\"\n\t\t\tMappingInformationType: \"ByPolygonVertex\"\n\
                 \t\t\tReferenceInformationType: \"Direct\"\n\t\t\tNormals: {}\n\t\t}}\n",
                array(&flat)
            )
            .unwrap();
            layers.push_str(
                "\t\t\tLayerElement:  {\n\t\t\t\tType: \"LayerElementNormal\"\n\t\t\t\tTypedIndex: 0\n\t\t\t}\n",
            );
        }
        if let Some(uvs) = &self.uvs {
            let flat: Vec<f64> = uvs.iter().flatten().copied().collect();
            let index: Vec<i32> = (0..i32::try_from(uvs.len()).unwrap()).collect();
            write!(
                text,
                "\t\tLayerElementUV: 0 {{\n\t\t\tVersion: 101\n\t\t\tName: \"map1\"\n\t\t\tMappingInformationType: \"ByPolygonVertex\"\n\
                 \t\t\tReferenceInformationType: \"IndexToDirect\"\n\t\t\tUV: {}\n\t\t\tUVIndex: {}\n\t\t}}\n",
                array(&flat),
                array(&index)
            )
            .unwrap();
            layers
                .push_str("\t\t\tLayerElement:  {\n\t\t\t\tType: \"LayerElementUV\"\n\t\t\t\tTypedIndex: 0\n\t\t\t}\n");
        }
        if let Some(materials) = &self.materials {
            write!(
                text,
                "\t\tLayerElementMaterial: 0 {{\n\t\t\tVersion: 101\n\t\t\tName: \"\"\n\t\t\tMappingInformationType: \"ByPolygon\"\n\
                 \t\t\tReferenceInformationType: \"IndexToDirect\"\n\t\t\tMaterials: {}\n\t\t}}\n",
                array(materials)
            )
            .unwrap();
            layers.push_str(
                "\t\t\tLayerElement:  {\n\t\t\t\tType: \"LayerElementMaterial\"\n\t\t\t\tTypedIndex: 0\n\t\t\t}\n",
            );
        }
        write!(text, "\t\tLayer: 0 {{\n\t\t\tVersion: 100\n{layers}\t\t}}\n\t}}\n").unwrap();
        file.raw(text, vec![(id, model, None)]);
        id
    }
}

/// A Lambert material with a diffuse colour, used by `model`; returns its id.
fn material(file: &mut FbxBuilder, model: i64, name: &str, colour: [f64; 3]) -> i64 {
    let id = file.id();
    let [r, g, b] = colour;
    let text = format!(
        "\tMaterial: {id}, \"Material::{name}\", \"\" {{\n\t\tVersion: 102\n\t\tShadingModel: \"lambert\"\n\t\tProperties70:  {{\n\
         \t\t\tP: \"DiffuseColor\", \"Color\", \"\", \"A\",{r},{g},{b}\n\t\t\tP: \"DiffuseFactor\", \"Number\", \"\", \"A\",1\n\t\t}}\n\t}}\n"
    );
    file.raw(text, vec![(id, model, None)]);
    id
}

/// A texture file named `file_name`, as the diffuse colour of `material` (not embedded: read through the resolver).
fn texture(file: &mut FbxBuilder, material: i64, file_name: &str) {
    let id = file.id();
    let text = format!(
        "\tTexture: {id}, \"Texture::{file_name}\", \"\" {{\n\t\tType: \"TextureVideoClip\"\n\t\tVersion: 202\n\
         \t\tTextureName: \"Texture::{file_name}\"\n\t\tFileName: \"{file_name}\"\n\t\tRelativeFilename: \"{file_name}\"\n\
         \t\tProperties70:  {{\n\t\t\tP: \"WrapModeU\", \"enum\", \"\", \"\",1\n\t\t}}\n\t}}\n"
    );
    file.raw(text, vec![(id, material, Some("DiffuseColor".to_owned()))]);
}

/// A column-major matrix as FBX writes it (16 numbers).
fn matrix(m: Mat4) -> String {
    let values: Vec<f64> = m.to_cols_array().iter().map(|&v| f64::from(v)).collect();
    array(&values)
}

/// One skin cluster: the bone model, the mesh-to-bone matrix at bind, the bone's bind matrix, and its points and weights.
type Cluster = (i64, Mat4, Mat4, Vec<i32>, Vec<f64>);

/// A skin on `geometry` with these clusters.
fn skin(file: &mut FbxBuilder, geometry: i64, clusters: &[Cluster]) {
    let skin = file.id();
    file.raw(
        format!(
            "\tDeformer: {skin}, \"Deformer::Skin\", \"Skin\" {{\n\t\tVersion: 101\n\t\tLink_DeformAcuracy: 50\n\t}}\n"
        ),
        vec![(skin, geometry, None)],
    );
    for (bone, transform, link, points, weights) in clusters {
        let id = file.id();
        let text = format!(
            "\tDeformer: {id}, \"SubDeformer::Cluster\", \"Cluster\" {{\n\t\tVersion: 100\n\t\tUserData: \"\", \"\"\n\
             \t\tIndexes: {}\n\t\tWeights: {}\n\t\tTransform: {}\n\t\tTransformLink: {}\n\t}}\n",
            array(points),
            array(weights),
            matrix(*transform),
            matrix(*link)
        );
        file.raw(text, vec![(id, skin, None), (*bone, id, None)]);
    }
}

/// A blend shape channel `name` on `geometry` that moves `points` by `offsets`.
fn blend_shape(file: &mut FbxBuilder, geometry: i64, name: &str, points: &[i32], offsets: &[[f64; 3]]) {
    let (blend, channel, shape) = (file.id(), file.id(), file.id());
    let flat: Vec<f64> = offsets.iter().flatten().copied().collect();
    file.raw(
        format!("\tDeformer: {blend}, \"Deformer::Blend\", \"BlendShape\" {{\n\t\tVersion: 100\n\t}}\n"),
        vec![(blend, geometry, None)],
    );
    file.raw(
        format!(
            "\tDeformer: {channel}, \"SubDeformer::{name}\", \"BlendShapeChannel\" {{\n\t\tVersion: 100\n\t\tDeformPercent: 0\n\
             \t\tFullWeights: *1 {{\n\t\t\ta: 100\n\t\t}}\n\t}}\n"
        ),
        vec![(channel, blend, None)],
    );
    file.raw(
        format!(
            "\tGeometry: {shape}, \"Geometry::{name}\", \"Shape\" {{\n\t\tVersion: 100\n\t\tIndexes: {}\n\t\tVertices: {}\n\t}}\n",
            array(points),
            array(&flat)
        ),
        vec![(shape, channel, None)],
    );
}

/// A unit quad in the XY plane facing +Z, in centimetres: four points, one polygon, a normal and a UV per corner.
fn quad() -> Geometry {
    Geometry {
        points: vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [100.0, 100.0, 0.0], [0.0, 100.0, 0.0]],
        polygons: vec![vec![0, 1, 2, 3]],
        normals: Some(vec![[0.0, 0.0, 1.0]; 4]),
        uvs: Some(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
        materials: None,
    }
}

/// The mesh the node named `name` draws.
fn mesh_of(scene: &IoScene, name: &str) -> MeshIx {
    scene.nodes.iter().find(|n| n.name == name).and_then(|n| n.mesh).expect("the node draws a mesh")
}

/// Every triangle's face normal agrees with its corners' normals: the winding faces the way the normals say.
fn faces_agree(scene: &IoScene, mesh: MeshIx) -> bool {
    scene.meshes[mesh.index()].primitives.iter().all(|p| {
        let m = &p.mesh;
        m.indices.as_chunks::<3>().0.iter().all(|t| {
            let [a, b, c] = t.map(|i| m.positions[i as usize]);
            let face = (b - a).cross(c - a);
            face.dot(m.normals[t[0] as usize]) > 0.0
        })
    })
}

#[test]
fn a_quad_is_triangulated_in_metres_with_gltf_uvs_and_its_front_kept() {
    let mut file = maya();
    let model = file.model("Card", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    file.kind(model, "Mesh");
    quad().write(&mut file, model);
    let out = import(&file.bytes());
    let mesh = mesh_of(&out.scene, "Card");
    let p = &out.scene.meshes[mesh.index()].primitives;
    assert_eq!(p.len(), 1, "no material: one primitive");
    let m = &p[0].mesh;
    assert_eq!((m.positions.len(), m.indices.len()), (4, 6), "four corners, two triangles");
    assert!(m.positions.contains(&Vec3::new(1.0, 1.0, 0.0)), "centimetres to metres: {:?}", m.positions);
    let corner = m.positions.iter().position(|&p| p == Vec3::new(1.0, 1.0, 0.0)).unwrap();
    assert_eq!(m.uvs[corner], Vec2::new(1.0, 0.0), "FBX's v up becomes glTF's v down");
    assert!(m.normals.iter().all(|n| n.abs_diff_eq(Vec3::Z, 1e-6)));
    assert!(faces_agree(&out.scene, mesh));
    assert!(p[0].material.is_none());
}

#[test]
fn a_z_up_file_and_a_left_handed_file_keep_every_triangle_facing_its_normals() {
    // 3ds Max style: Z up, front -Y. The quad is turned to face the file's front, so it faces +Z after the import.
    let mut max = FbxBuilder::new(Axis::Z, Axis::NEG_Y, Axis::X, 1.0);
    let model = max.model("Card", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    let mut g = quad();
    g.points = vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [100.0, 0.0, 100.0], [0.0, 0.0, 100.0]];
    g.normals = Some(vec![[0.0, -1.0, 0.0]; 4]);
    g.write(&mut max, model);
    let out = import(&max.bytes());
    let mesh = mesh_of(&out.scene, "Card");
    let m = &out.scene.meshes[mesh.index()].primitives[0].mesh;
    assert!(m.normals.iter().all(|n| n.abs_diff_eq(Vec3::Z, 1e-6)), "the file's front is +Z: {:?}", m.normals);
    assert!(faces_agree(&out.scene, mesh));
    // A left-handed file (right is -X): the conversion mirrors, and the corners are reversed to keep the front.
    let mut left = FbxBuilder::new(Axis::Y, Axis::Z, Axis::NEG_X, 1.0);
    let model = left.model("Card", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    quad().write(&mut left, model);
    let out = import(&left.bytes());
    assert!(faces_agree(&out.scene, mesh_of(&out.scene, "Card")));
}

#[test]
fn polygons_split_by_material_into_primitives_with_their_colours() {
    let mut file = maya();
    let model = file.model("Two", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    let mut g = quad();
    g.points.extend([[200.0, 0.0, 0.0], [200.0, 100.0, 0.0]]);
    g.polygons.push(vec![1, 4, 5, 2]);
    g.normals = Some(vec![[0.0, 0.0, 1.0]; 8]);
    g.uvs = None;
    g.materials = Some(vec![0, 1]);
    g.write(&mut file, model);
    material(&mut file, model, "skin", [1.0, 0.8, 0.7]);
    material(&mut file, model, "cloth", [0.1, 0.2, 0.9]);
    let out = import(&file.bytes());
    let scene = &out.scene;
    let mesh = &scene.meshes[mesh_of(scene, "Two").index()];
    assert_eq!(mesh.primitives.len(), 2);
    let colours: Vec<Vec4> =
        mesh.primitives.iter().map(|p| scene.materials[p.material.expect("a material").index()].base_color).collect();
    assert!(
        colours.contains(&Vec4::new(1.0, 0.8, 0.7, 1.0)) && colours.contains(&Vec4::new(0.1, 0.2, 0.9, 1.0)),
        "{colours:?}"
    );
    let names: Vec<&str> = scene.materials.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["skin", "cloth"]);
    assert!(!rules(&out).contains(&"fbx.content_dropped"), "{:?}", rules(&out));
}

/// The world matrix of node `n` in `scene`.
fn world(scene: &IoScene, n: NodeIx) -> Mat4 {
    let t = &scene.nodes[n.index()].transform;
    let local = Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
    scene.parent_of(n).map_or(local, |p| world(scene, p) * local)
}

#[test]
fn a_skin_binds_its_clusters_as_joints_with_gltf_inverse_bind_matrices_and_four_weights() {
    // Two bones up the Y axis in a centimetre file; a strip of three point pairs, the middle pair shared.
    let mut file = maya();
    let root = file.model("Hips", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    let upper = file.model("Spine", root, [0.0, 100.0, 0.0], [0.0; 3]);
    file.kind(root, "LimbNode");
    file.kind(upper, "LimbNode");
    let body = file.model("Body", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    file.kind(body, "Mesh");
    let geometry = Geometry {
        points: vec![
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [0.0, 100.0, 0.0],
            [10.0, 100.0, 0.0],
            [0.0, 200.0, 0.0],
            [10.0, 200.0, 0.0],
        ],
        polygons: vec![vec![0, 1, 3, 2], vec![2, 3, 5, 4]],
        normals: Some(vec![[0.0, 0.0, 1.0]; 8]),
        ..Geometry::default()
    }
    .write(&mut file, body);
    // Each cluster's Transform is the mesh-to-bone matrix at bind (the inverse of the bone's world), TransformLink the
    // bone's world. Point 4 has no weight at all.
    let up = |cm: f32| Mat4::from_translation(Vec3::new(0.0, cm, 0.0));
    skin(
        &mut file,
        geometry,
        &[
            (root, up(0.0), up(0.0), vec![0, 1, 2, 3], vec![1.0, 1.0, 0.25, 0.25]),
            (upper, up(-100.0), up(100.0), vec![2, 3, 5], vec![0.75, 0.75, 1.0]),
        ],
    );
    let out = import(&file.bytes());
    let scene = &out.scene;
    let node = scene.nodes.iter().find(|n| n.name == "Body").unwrap();
    let skin = &scene.skins[node.skin.expect("the mesh is skinned").index()];
    let names: Vec<&str> = skin.joints.iter().map(|j| scene.nodes[j.index()].name.as_str()).collect();
    assert_eq!(names, ["Hips", "Spine"]);
    // At bind, a joint's world times its inverse bind matrix is the mesh's own space: the identity here.
    for (j, ibm) in skin.joints.iter().zip(&skin.inverse_bind) {
        let product = world(scene, *j) * *ibm;
        assert!(product.abs_diff_eq(Mat4::IDENTITY, 1e-5), "{product}");
    }
    let m = &scene.meshes[node.mesh.unwrap().index()].primitives[0].mesh;
    let mut middle = 0;
    for (k, p) in m.positions.iter().enumerate() {
        let (joints, weights) = (m.joints[k], m.weights[k]);
        assert!((weights.iter().sum::<f32>() - 1.0).abs() < 1e-6, "renormalised: {weights:?}");
        if (p.y - 1.0).abs() < 1e-6 {
            middle += 1;
            assert_eq!(joints[..2], [1, 0], "the heavier joint first");
            assert!((weights[0] - 0.75).abs() < 1e-6 && (weights[1] - 0.25).abs() < 1e-6, "{weights:?}");
        }
    }
    assert_eq!(middle, 2, "both middle points are shared by the two quads");
    let unweighted =
        out.report.findings.iter().find(|f| f.rule == "fbx.unweighted_vertices").expect("one point unweighted");
    assert!(unweighted.message.starts_with("1 vertices"), "{}", unweighted.message);
}

#[test]
fn a_blend_shape_is_a_morph_target_whose_offsets_follow_their_points_across_a_seam() {
    let mut file = maya();
    let model = file.model("Face", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    let mut g = quad();
    // A second quad on the same points with other normals: points 1 and 2 split into two vertices each.
    g.points.extend([[200.0, 0.0, 0.0], [200.0, 100.0, 0.0]]);
    g.polygons.push(vec![1, 4, 5, 2]);
    let mut normals = vec![[0.0, 0.0, 1.0]; 4];
    normals.extend([[0.0, 0.6, 0.8]; 4]);
    g.normals = Some(normals);
    g.uvs = None;
    let geometry = g.write(&mut file, model);
    blend_shape(&mut file, geometry, "Smile", &[2], &[[0.0, 0.0, 50.0]]);
    let out = import(&file.bytes());
    let mesh = &out.scene.meshes[mesh_of(&out.scene, "Face").index()];
    assert_eq!(mesh.morph_target_names, ["Smile"]);
    let p = &mesh.primitives[0];
    assert_eq!(p.mesh.positions.len(), 8, "the seam splits two points");
    let moved: Vec<Vec3> = p
        .mesh
        .positions
        .iter()
        .zip(&p.morph_targets[0].positions)
        .filter(|(_, d)| **d != Vec3::ZERO)
        .map(|(p, _)| *p)
        .collect();
    assert_eq!(moved, [Vec3::new(1.0, 1.0, 0.0); 2], "both copies of point 2 move, half a metre forward");
    assert!(
        p.morph_targets[0].positions.iter().all(|d| *d == Vec3::ZERO || d.abs_diff_eq(Vec3::new(0.0, 0.0, 0.5), 1e-6))
    );
}

/// Serves `skin.png` (a PNG signature and nothing else; the contract stores encoded images unread) and `skin.tga`.
struct Files;

impl Resolver for Files {
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
        match uri {
            "skin.png" => Ok(b"\x89PNG\r\n\x1a\n-test-".to_vec()),
            "skin.tga" => Ok(vec![0, 0, 2, 0]),
            _ => Err(IoError::Resource { uri: uri.to_owned(), reason: "no such file".to_owned() }),
        }
    }
}

#[test]
fn a_texture_file_is_kept_as_its_png_and_another_format_is_left_out_with_a_warning() {
    let mut file = maya();
    let model = file.model("Body", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    let mut g = quad();
    g.points.extend([[200.0, 0.0, 0.0], [200.0, 100.0, 0.0]]);
    g.polygons.push(vec![1, 4, 5, 2]);
    g.normals = Some(vec![[0.0, 0.0, 1.0]; 8]);
    g.uvs = Some(vec![[0.0, 0.0]; 8]);
    g.materials = Some(vec![0, 1]);
    g.write(&mut file, model);
    let png = material(&mut file, model, "skin", [1.0; 3]);
    texture(&mut file, png, "skin.png");
    let tga = material(&mut file, model, "hair", [1.0; 3]);
    texture(&mut file, tga, "skin.tga");
    let options = ImportOptions::default();
    let out =
        run_import(&FbxImporter, &ImportInput::new(&file.bytes()), &ImportCtx::new(&options).with_resolver(&Files))
            .expect("imports");
    let scene = &out.scene;
    assert_eq!(scene.images.len(), 1);
    assert_eq!(scene.images[0].bytes.as_slice(), b"\x89PNG\r\n\x1a\n-test-");
    let skin = scene.materials.iter().find(|m| m.name == "skin").unwrap();
    let texture = &scene.textures[skin.base_color_texture.expect("the PNG is the base colour").texture.index()];
    assert_eq!(texture.sampler.wrap_u, Wrap::ClampToEdge, "WrapModeU 1 is clamp");
    let hair = scene.materials.iter().find(|m| m.name == "hair").unwrap();
    assert!(hair.base_color_texture.is_none());
    let dropped = out.report.findings.iter().find(|f| f.rule == "fbx.texture_dropped").expect("the TGA is reported");
    assert!(dropped.message.contains("neither PNG nor JPEG"), "{}", dropped.message);
}

#[test]
fn an_open_surface_is_double_sided_and_a_closed_one_is_not() {
    let mut file = maya();
    let card = file.model("Card", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    quad().write(&mut file, card);
    material(&mut file, card, "hair", [1.0; 3]);
    // A tetrahedron: every edge has two triangles, and its UV seams don't open it.
    let solid = file.model("Solid", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    Geometry {
        points: vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 100.0, 0.0], [0.0, 0.0, 100.0]],
        polygons: vec![vec![0, 2, 1], vec![0, 1, 3], vec![0, 3, 2], vec![1, 2, 3]],
        uvs: Some((0..12).map(|k| [f64::from(k) / 12.0, 0.0]).collect()),
        ..Geometry::default()
    }
    .write(&mut file, solid);
    material(&mut file, solid, "skin", [1.0; 3]);
    let out = import(&file.bytes());
    let sided: Vec<(&str, bool)> = out.scene.materials.iter().map(|m| (m.name.as_str(), m.double_sided)).collect();
    assert_eq!(sided, [("hair", true), ("skin", false)]);
}
