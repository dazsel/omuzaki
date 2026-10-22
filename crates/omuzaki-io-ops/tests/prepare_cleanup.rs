//! `prepare::cleanup`: every scene is built here from generated geometry.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// Counters and coordinates here are tiny, so the numeric casts are exact.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use omuzaki_core::glam::{Mat4, Vec2, Vec3, Vec4};
use omuzaki_core::id::Slug;
use omuzaki_io_api::{
    Avatar, Blob, ColorBind, ColorBindKind, EncodedImage, Expression, ExpressionName, ExtraAttribute, ImageIx,
    ImageMime, IoScene, Material, MaterialIx, Mesh, MeshData, MorphBind, MorphTarget, Node, NodeIx, NormalTexture, Pbr,
    Primitive, Sampler, Shading, ShiftTexture, Skin, TexRef, Texture, TextureIx, Toon, UvBind,
};
use omuzaki_io_ops::prepare::cleanup::{CleanupOptions, CleanupReport, cleanup};

const WELD: f32 = 1e-6;
const ALL: CleanupOptions = CleanupOptions { weld: Some(WELD), merge_by_material: true, drop_unused: true };
const WELD_ONLY: CleanupOptions = CleanupOptions { weld: Some(WELD), merge_by_material: false, drop_unused: false };
const MERGE_ONLY: CleanupOptions = CleanupOptions { weld: None, merge_by_material: true, drop_unused: false };
const DROP_ONLY: CleanupOptions = CleanupOptions { weld: None, merge_by_material: false, drop_unused: true };

/// The six faces of a unit cube as 24 split vertices; `shared_normal` gives every vertex the same normal.
fn cube(shared_normal: bool) -> Primitive {
    let faces = [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::Y, Vec3::X),
    ];
    let mut mesh = MeshData::default();
    for (n, u, v) in faces {
        let base = u32::try_from(mesh.positions.len()).unwrap();
        for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            mesh.positions.push((n + u * a + v * b) * 0.5);
            mesh.normals.push(if shared_normal { Vec3::Z } else { n });
        }
        mesh.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Primitive::new(mesh)
}

/// A unit quad in the XY plane at depth `z`.
fn quad(z: f32) -> Primitive {
    let mesh = MeshData {
        positions: vec![Vec3::new(0.0, 0.0, z), Vec3::new(1.0, 0.0, z), Vec3::new(1.0, 1.0, z), Vec3::new(0.0, 1.0, z)],
        normals: vec![Vec3::Z; 4],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..MeshData::default()
    };
    Primitive::new(mesh)
}

fn on_material(mut p: Primitive, material: usize) -> Primitive {
    p.material = Some(MaterialIx(u32::try_from(material).unwrap()));
    p
}

fn scene_with(prims: Vec<Primitive>) -> IoScene {
    let mut scene = IoScene::new();
    let mut mesh = Mesh::new("m");
    mesh.primitives = prims;
    let ix = scene.add_mesh(mesh);
    let mut node = Node::new("n");
    node.mesh = Some(ix);
    let root = scene.add_node(node);
    scene.roots.push(root);
    scene
}

fn prims(scene: &IoScene) -> Vec<&Primitive> {
    scene.meshes.iter().flat_map(|m| m.primitives.iter()).collect()
}

fn vertex_count(scene: &IoScene) -> usize {
    prims(scene).iter().map(|p| p.mesh.positions.len()).sum()
}

fn run(scene: &mut IoScene, options: &CleanupOptions) -> CleanupReport {
    assert!(!scene.check().has_errors(), "the input is valid: {:?}", scene.check());
    let report = cleanup(scene, options);
    assert!(!scene.check().has_errors(), "the output is valid: {:?}", scene.check());
    let again = scene.clone();
    let second = cleanup(scene, options);
    assert_eq!(*scene, again, "a second run changes nothing");
    assert_eq!(second.vertices.0, second.vertices.1);
    assert_eq!(second.primitives.0, second.primitives.1);
    report
}

/// A triangle is the ordered triple of its corners, rounded so the comparison does not depend on rounding noise.
type Triangle = [[i64; 3]; 3];

fn round(v: Vec3) -> [i64; 3] {
    [v.x, v.y, v.z].map(|c| (f64::from(c) * 1e5).round() as i64)
}

/// Every triangle of every primitive with its corners passed through `position`, sorted.
fn triangles(scene: &IoScene, position: impl Fn(&Primitive, usize) -> Vec3) -> Vec<Triangle> {
    let mut all: Vec<Triangle> = prims(scene)
        .iter()
        .flat_map(|p| {
            p.mesh.indices.chunks(3).map(|t| [0, 1, 2].map(|k| round(position(p, t[k] as usize)))).collect::<Vec<_>>()
        })
        .collect();
    all.sort_unstable();
    all
}

fn plain_triangles(scene: &IoScene) -> Vec<Triangle> {
    triangles(scene, |p, v| p.mesh.positions[v])
}

#[test]
fn weld_keeps_split_normals_and_joins_shared_ones() {
    let mut split = scene_with(vec![cube(false)]);
    let report = run(&mut split, &WELD_ONLY);
    assert_eq!(report.vertices, (24, 24), "per-face normals differ");

    let mut smooth = scene_with(vec![cube(true)]);
    let before = plain_triangles(&smooth);
    let report = run(&mut smooth, &WELD_ONLY);
    assert_eq!((report.vertices, vertex_count(&smooth)), ((24, 8), 8));
    assert_eq!(plain_triangles(&smooth), before, "the same triangles");
}

#[test]
fn weld_joins_positions_within_the_tolerance_and_not_beyond() {
    let mut p = cube(true);
    p.mesh.positions[0] += Vec3::splat(2e-7);
    let mut near = scene_with(vec![p.clone()]);
    assert_eq!(run(&mut near, &WELD_ONLY).vertices.1, 8);

    p.mesh.positions[0] += Vec3::splat(1e-4);
    let mut far = scene_with(vec![p]);
    assert_eq!(run(&mut far, &WELD_ONLY).vertices.1, 9, "the displaced corner stays apart");
}

/// Wraps `p` in a scene with a two-joint skin whose bind pose differs from the current pose, so the skin matrices
/// are not the identity.
fn skinned(mut p: Primitive, joints: Vec<[u16; 4]>, weights: Vec<[f32; 4]>) -> IoScene {
    p.mesh.joints = joints;
    p.mesh.weights = weights;
    let mut scene = scene_with(vec![p]);
    add_skin(&mut scene);
    scene
}

fn add_skin(scene: &mut IoScene) {
    let a = scene.add_node(Node::new("a").at(Vec3::new(0.0, 0.5, 0.0)));
    let b = scene.add_node(Node::new("b").at(Vec3::new(0.3, 1.0, 0.1)));
    let bind = [Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0)), Mat4::from_translation(Vec3::new(0.0, 2.0, 0.5))];
    let skin = scene.add_skin(Skin::new("s", vec![a, b], bind.map(|m| m.inverse()).to_vec()));
    scene.nodes[0].skin = Some(skin);
    scene.nodes[0].children = vec![a, b];
}

/// The skinned world position of a vertex (nodes here have no parents beyond a translation-free root).
fn skinned_position(scene: &IoScene, p: &Primitive, v: usize) -> Vec3 {
    let skin = &scene.skins[0];
    let matrices: Vec<Mat4> = skin
        .joints
        .iter()
        .zip(&skin.inverse_bind)
        .map(|(j, ibm)| Mat4::from_translation(scene.nodes[j.index()].transform.translation) * *ibm)
        .collect();
    let (joints, weights) = (p.mesh.joints[v], p.mesh.weights[v]);
    let blended: Mat4 = (0..4).map(|k| matrices[joints[k] as usize] * weights[k]).fold(Mat4::ZERO, |a, m| a + m);
    blended.transform_point3(p.mesh.positions[v])
}

/// A quad plus a fifth vertex that copies vertex 0's position and normal and forms a triangle with vertices 1 and 2.
fn quad_with_twin() -> Primitive {
    let mut p = quad(0.0);
    p.mesh.positions.push(p.mesh.positions[0]);
    p.mesh.normals.push(Vec3::Z);
    p.mesh.indices.extend([4, 1, 2]);
    p
}

#[test]
fn weld_keeps_vertices_that_differ_in_joints_and_skinning_survives() {
    let mut joints = vec![[0, 1, 0, 0]; 4];
    joints.push([1, 0, 0, 0]);
    let mut weights = vec![[0.75, 0.25, 0.0, 0.0]; 4];
    weights.push([1.0, 0.0, 0.0, 0.0]);
    let mut scene = skinned(quad_with_twin(), joints, weights);
    let before = triangles(&scene, |p, v| skinned_position(&scene, p, v));
    assert_eq!(run(&mut scene, &WELD_ONLY).vertices, (5, 5), "the twin binds to other joints");
    assert_eq!(triangles(&scene, |p, v| skinned_position(&scene, p, v)), before);

    // Control: the same twin with vertex 0's joints and weights does weld, and the skinned result is unchanged.
    let mut scene = skinned(quad_with_twin(), vec![[0, 1, 0, 0]; 5], vec![[0.75, 0.25, 0.0, 0.0]; 5]);
    let before = triangles(&scene, |p, v| skinned_position(&scene, p, v));
    assert_eq!(run(&mut scene, &WELD_ONLY).vertices, (5, 4));
    assert_eq!(triangles(&scene, |p, v| skinned_position(&scene, p, v)), before, "skinned positions are unchanged");
}

#[test]
fn weld_respects_morph_targets() {
    let build = |second: Vec3, second_normal: Vec3| {
        let mut p = cube(true);
        let mut target = MorphTarget::new(vec![Vec3::ZERO; 24]);
        target.normals = vec![Vec3::ZERO; 24];
        target.positions[1] = second;
        target.normals[1] = second_normal;
        p.morph_targets = vec![target];
        scene_with(vec![p])
    };
    // Vertices 0 and 1 of the first face are distinct corners, so duplicate corner 0 into a fresh coincident vertex.
    let with_twin = |mut scene: IoScene, delta: Vec3, normal_delta: Vec3| {
        let p = &mut scene.meshes[0].primitives[0];
        p.mesh.positions.push(p.mesh.positions[0]);
        p.mesh.normals.push(p.mesh.normals[0]);
        p.mesh.indices.extend([24, 1, 2]);
        p.morph_targets[0].positions.push(delta);
        p.morph_targets[0].normals.push(normal_delta);
        scene
    };
    let mut apart = with_twin(build(Vec3::ZERO, Vec3::ZERO), Vec3::new(1e-3, 0.0, 0.0), Vec3::ZERO);
    assert_eq!(run(&mut apart, &WELD_ONLY).vertices, (25, 9), "only the position deltas differ: 8 corners + the twin");
    let mut apart_by_normal = with_twin(build(Vec3::ZERO, Vec3::ZERO), Vec3::ZERO, Vec3::new(0.0, 1e-3, 0.0));
    assert_eq!(run(&mut apart_by_normal, &WELD_ONLY).vertices.1, 9, "the normal deltas differ");
    let mut together = with_twin(build(Vec3::ZERO, Vec3::ZERO), Vec3::new(1e-7, 0.0, 0.0), Vec3::ZERO);
    assert_eq!(run(&mut together, &WELD_ONLY).vertices.1, 8, "deltas within the tolerance weld");
}

#[test]
fn weld_keeps_morph_deltas_aligned_with_their_vertices() {
    let mut p = cube(true);
    let mut target = MorphTarget::new((0..24).map(|i| Vec3::new(0.0, 0.0, i as f32 * 1e-3)).collect());
    target.normals = (0..24).map(|i| Vec3::new(i as f32, 0.0, 0.0)).collect();
    // The delta depends on the vertex's position (its corner), so coincident vertices agree and welding is possible.
    for i in 0..24 {
        let corner = p.mesh.positions[i];
        target.positions[i] = corner * 0.1;
        target.normals[i] = corner * 0.2;
    }
    p.morph_targets = vec![target];
    let mut scene = scene_with(vec![p]);
    run(&mut scene, &WELD_ONLY);
    let welded = &scene.meshes[0].primitives[0];
    assert_eq!(welded.mesh.positions.len(), 8);
    for (v, pos) in welded.mesh.positions.iter().enumerate() {
        assert!((welded.morph_targets[0].positions[v] - *pos * 0.1).length() < 1e-7, "position delta of {v}");
        assert!((welded.morph_targets[0].normals[v] - *pos * 0.2).length() < 1e-7, "normal delta of {v}");
    }
}

#[test]
fn weld_compares_uvs_colours_and_extra_attributes() {
    let extra = |values: Vec<f32>| ExtraAttribute { name: Slug::new("strand").unwrap(), components: 1, data: values };
    for kind in 0..3 {
        let p = quad_with_twin();
        let apart = |differs: bool| {
            let mut q = p.clone();
            let twin = f32::from(differs);
            match kind {
                0 => q.mesh.uvs = vec![Vec2::ZERO, Vec2::X, Vec2::ONE, Vec2::Y, Vec2::new(twin, 0.0)],
                1 => q.mesh.colours = [[1.0; 4], [1.0; 4], [1.0; 4], [1.0; 4], [1.0, 1.0, 1.0, 1.0 - twin]].to_vec(),
                _ => q.mesh.extra = vec![extra(vec![0.0, 1.0, 2.0, 3.0, twin * 5.0])],
            }
            let mut scene = scene_with(vec![q]);
            run(&mut scene, &WELD_ONLY).vertices.1
        };
        assert_eq!(apart(true), 5, "attribute kind {kind} differs, so the twin stays");
        assert_eq!(apart(false), 4, "attribute kind {kind} is equal, so the twin welds");
    }
}

#[test]
fn merge_by_material_groups_in_first_occurrence_order() {
    let (a, b) = (MaterialIx(0), MaterialIx(1));
    let mut scene = scene_with((0..5).map(|i| quad(i as f32)).collect());
    scene.add_material(Material::new("a", Shading::Unlit));
    scene.add_material(Material::new("b", Shading::Unlit));
    let before = plain_triangles(&scene);
    for (p, m) in scene.meshes[0].primitives.iter_mut().zip([Some(a), Some(b), Some(a), None, None]) {
        p.material = m;
    }
    let report = run(&mut scene, &MERGE_ONLY);
    assert_eq!((report.primitives, report.vertices), ((5, 3), (20, 20)));
    let merged = prims(&scene);
    assert_eq!(merged.iter().map(|p| p.material).collect::<Vec<_>>(), [Some(a), Some(b), None]);
    assert_eq!(merged.iter().map(|p| p.mesh.positions.len()).collect::<Vec<_>>(), [8, 4, 8]);
    assert_eq!(merged.iter().map(|p| p.mesh.indices.len()).collect::<Vec<_>>(), [12, 6, 12], "indices add up");
    assert_eq!(plain_triangles(&scene), before, "the triangle set is identical");
}

#[test]
fn merge_keeps_different_attribute_sets_apart() {
    let mut with_colours = quad(1.0);
    with_colours.mesh.colours = vec![[1.0; 4]; 4];
    let mut with_morph = quad(2.0);
    with_morph.morph_targets = vec![MorphTarget::new(vec![Vec3::Y; 4])];
    let mut with_extra = quad(3.0);
    with_extra.mesh.extra = vec![ExtraAttribute { name: Slug::new("w").unwrap(), components: 1, data: vec![0.0; 4] }];
    let mut scene = scene_with(vec![quad(0.0), with_colours, with_morph, with_extra, quad(4.0)]);
    scene.meshes[0].morph_target_names = vec!["t".to_owned()];
    let report = run(&mut scene, &MERGE_ONLY);
    assert_eq!(report.primitives, (5, 4), "only the two plain quads merge");
    assert_eq!(prims(&scene)[0].mesh.positions.len(), 8);
}

#[test]
fn merge_concatenates_morph_targets_and_extras_and_keeps_indices() {
    let build = |z: f32, delta: Vec3, tag: f32| {
        let mut p = quad(z);
        let mut t = MorphTarget::new(vec![delta; 4]);
        t.normals = vec![delta; 4];
        p.morph_targets = vec![t, MorphTarget::new(vec![-delta; 4])];
        p.mesh.extra = vec![ExtraAttribute { name: Slug::new("tag").unwrap(), components: 1, data: vec![tag; 4] }];
        p
    };
    let mut scene = scene_with(vec![build(0.0, Vec3::X, 1.0), build(1.0, Vec3::Y, 2.0)]);
    scene.meshes[0].morph_target_names = vec!["a".to_owned(), "b".to_owned()];
    let mut avatar = Avatar::new();
    avatar.expressions.insert(ExpressionName::parse("happy").unwrap(), Expression::morph(NodeIx(0), 1));
    scene.avatar = Some(avatar);
    run(&mut scene, &ALL);
    let p = prims(&scene)[0];
    assert_eq!(p.morph_targets.len(), 2, "the targets keep their indices");
    assert_eq!(p.morph_targets[0].positions, [vec![Vec3::X; 4], vec![Vec3::Y; 4]].concat());
    assert_eq!(p.morph_targets[0].normals.len(), 8);
    assert_eq!(p.morph_targets[1].positions, [vec![Vec3::NEG_X; 4], vec![Vec3::NEG_Y; 4]].concat());
    assert_eq!(p.mesh.extra[0].data, [1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0]);
    let expressions = &scene.avatar.as_ref().unwrap().expressions;
    let bind: &MorphBind = &expressions.values().next().unwrap().morph_binds[0];
    assert_eq!((bind.node, bind.index), (NodeIx(0), 1));
}

#[test]
fn merge_then_weld_keep_skinned_positions() {
    // Quads a and c are the same skinned quad; b sits beside them on other joints.
    let skin_a = |mut q: Primitive| {
        q.mesh.joints = vec![[0, 1, 0, 0]; 4];
        q.mesh.weights = vec![[0.75, 0.25, 0.0, 0.0]; 4];
        q
    };
    let mut b = quad(0.0);
    b.mesh.positions.iter_mut().for_each(|p| p.x += 1.0);
    b.mesh.joints = vec![[1, 0, 0, 0]; 4];
    b.mesh.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
    let mut scene = scene_with(vec![skin_a(quad(0.0)), b, skin_a(quad(0.0))]);
    add_skin(&mut scene);
    let before = triangles(&scene, |p, v| skinned_position(&scene, p, v));
    let report = run(&mut scene, &ALL);
    assert_eq!(report.primitives, (3, 1));
    assert_eq!(report.vertices, (12, 8), "the third quad is the first again and welds into it");
    let mut after = triangles(&scene, |p, v| skinned_position(&scene, p, v));
    let mut expected = before;
    expected.sort_unstable();
    after.sort_unstable();
    assert_eq!(after, expected, "all six triangles keep their skinned corners");
}

/// A scene with an unused image first, then `n` chains (material i uses texture i uses image i), one primitive per
/// entry of `used` and a thumbnail on `thumbnail`'s image.
fn arena_scene(n: usize, used: &[usize], thumbnail: usize) -> IoScene {
    let prims = used.iter().map(|&m| on_material(quad(0.0), m)).collect();
    let mut scene = scene_with(prims);
    scene.add_image(EncodedImage::new("orphan", ImageMime::Png, Blob::new(vec![9u8])));
    for i in 0..n {
        let image = scene.add_image(EncodedImage::new(format!("img{i}"), ImageMime::Png, Blob::new(vec![i as u8])));
        let texture = scene.add_texture(Texture { image, sampler: Sampler::default() });
        let mut material = Material::new(format!("mat{i}"), Shading::Unlit);
        material.base_color_texture = Some(TexRef::of(texture));
        scene.add_material(material);
    }
    scene.meta.thumbnail_image = Some(ImageIx(thumbnail as u32 + 1));
    scene
}

/// The name of the image behind each of a material's textures.
fn image_names<'a>(scene: &'a IoScene, material: &Material) -> Vec<&'a str> {
    let image_of = |t: &TexRef| scene.images[scene.textures[t.texture.index()].image.index()].name.as_str();
    material.texture_refs().iter().map(|(_, t)| image_of(t)).collect()
}

#[test]
fn drop_unused_removes_the_chain_and_remaps_everything() {
    let mut scene = arena_scene(4, &[3, 1, 3], 3);
    let mut expression = Expression::new();
    expression.color_binds.push(ColorBind { material: MaterialIx(2), kind: ColorBindKind::Color, target: Vec4::ONE });
    expression.uv_binds.push(UvBind { material: MaterialIx(2), scale: Vec2::ONE, offset: Vec2::ZERO });
    let mut avatar = Avatar::new();
    avatar.expressions.insert(ExpressionName::parse("happy").unwrap(), expression);
    scene.avatar = Some(avatar);

    let report = run(&mut scene, &DROP_ONLY);
    assert_eq!((report.dropped_materials, report.dropped_textures, report.dropped_images), (1, 1, 2));
    let names = |scene: &IoScene| scene.materials.iter().map(|m| m.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&scene), ["mat1", "mat2", "mat3"], "the bind-only material stays, the unused one goes");
    assert_eq!(scene.textures.len(), 3);
    assert_eq!(scene.images.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["img1", "img2", "img3"]);

    // Rebuild every reference by name and compare with what the scene should say.
    let material_of = |scene: &IoScene, ix: MaterialIx| scene.materials[ix.index()].name.clone();
    let prim_materials: Vec<_> = prims(&scene).iter().map(|p| material_of(&scene, p.material.unwrap())).collect();
    assert_eq!(prim_materials, ["mat3", "mat1", "mat3"]);
    let happy = &scene.avatar.as_ref().unwrap().expressions.values().next().unwrap();
    assert_eq!(material_of(&scene, happy.color_binds[0].material), "mat2");
    assert_eq!(material_of(&scene, happy.uv_binds[0].material), "mat2");
    for (i, material) in scene.materials.iter().enumerate() {
        assert_eq!(image_names(&scene, material), [format!("img{}", i + 1)], "{}", material.name);
    }
    assert_eq!(scene.images[scene.meta.thumbnail_image.unwrap().index()].name, "img3");
}

#[test]
fn drop_unused_keeps_the_thumbnail_image_of_a_dropped_material() {
    let mut scene = arena_scene(2, &[1], 0);
    let report = run(&mut scene, &DROP_ONLY);
    assert_eq!((report.dropped_materials, report.dropped_textures, report.dropped_images), (1, 1, 1));
    assert_eq!(scene.images.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["img0", "img1"]);
    assert_eq!(scene.meta.thumbnail_image, Some(ImageIx(0)));
}

#[test]
fn drop_unused_remaps_every_material_texture_slot() {
    // Texture 0 is unused, so all eleven slot textures shift down by one.
    let mut scene = scene_with(vec![on_material(quad(0.0), 0)]);
    scene.meshes[0].primitives.push(on_material(quad(1.0), 1));
    let image = scene.add_image(EncodedImage::new("i", ImageMime::Png, Blob::new(vec![0u8])));
    for _ in 0..11 {
        scene.add_texture(Texture { image, sampler: Sampler::default() });
    }
    let slot = |n: u32| TexRef::of(TextureIx(n));
    let mut toon = Toon::default();
    toon.shade_multiply_texture = Some(slot(4));
    toon.shading_shift_texture = Some(ShiftTexture { texture: slot(5), scale: 1.0 });
    toon.matcap_texture = Some(slot(6));
    toon.rim_multiply_texture = Some(slot(7));
    toon.outline_width_texture = Some(slot(8));
    toon.uv_animation_mask = Some(slot(9));
    let mut mtoon = Material::new("toon", Shading::Toon(Box::new(toon)));
    mtoon.base_color_texture = Some(slot(1));
    mtoon.emissive_texture = Some(slot(2));
    mtoon.normal_texture = Some(NormalTexture { texture: slot(3), scale: 1.0 });
    let mut pbr =
        Material::new("pbr", Shading::Pbr(Pbr { metallic_roughness_texture: Some(slot(10)), ..Pbr::default() }));
    pbr.base_color_texture = Some(slot(1));
    scene.add_material(mtoon);
    scene.add_material(pbr);

    let before: Vec<Vec<u32>> =
        scene.materials.iter().map(|m| m.texture_refs().iter().map(|(_, t)| t.texture.0).collect()).collect();
    assert_eq!(before[0].len(), 9, "all nine toon-material slots are in use");
    let report = run(&mut scene, &DROP_ONLY);
    assert_eq!((report.dropped_textures, report.dropped_materials), (1, 0));
    let after: Vec<Vec<u32>> =
        scene.materials.iter().map(|m| m.texture_refs().iter().map(|(_, t)| t.texture.0).collect()).collect();
    let shifted: Vec<Vec<u32>> = before.iter().map(|m| m.iter().map(|t| t - 1).collect()).collect();
    assert_eq!(after, shifted);
}

#[test]
fn nothing_to_do_changes_nothing_and_reports_nothing() {
    let mut scene = scene_with(vec![quad(0.0)]);
    let before = scene.clone();
    let report = run(&mut scene, &ALL);
    assert_eq!(scene, before);
    assert_eq!((report.vertices, report.primitives), ((4, 4), (1, 1)));
    assert_eq!(report.report().findings.len(), 0);
}

#[test]
fn the_report_names_each_kind_of_change() {
    let mut scene = arena_scene(2, &[0, 0], 0);
    scene.meshes[0].primitives.push(cube(true));
    let report = run(&mut scene, &ALL);
    let findings = report.report().findings;
    let rules: Vec<_> = findings.iter().map(|f| f.rule.as_str()).collect();
    assert_eq!(rules, ["cleanup.welded", "cleanup.merged", "cleanup.dropped_unused"]);
    assert!(findings.iter().all(|f| f.severity == omuzaki_io_api::Severity::Info));
    assert_eq!(report.primitives, (3, 2), "the two quads merge and the cube has no material");
    assert_eq!(report.vertices, (8 + 24, 4 + 8));
    assert!(findings[0].message.contains("32") && findings[0].message.contains("12"), "{}", findings[0].message);
    assert!(findings[1].message.contains('3') && findings[1].message.contains('2'), "{}", findings[1].message);
    assert!(findings[2].message.contains("1 materials, 1 textures, 2 images"), "{}", findings[2].message);

    let only_drop = CleanupReport { dropped_images: 2, ..CleanupReport::default() };
    assert_eq!(only_drop.report().findings.len(), 1);
}
