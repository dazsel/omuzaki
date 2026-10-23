//! Helpers shared by the `accessory_*` tests: a real body and head (and optionally a real hairstyle) of each template
//! with accessories on them, closed-shell checks, and posing through the rig.
//!
//! Each test file includes this with `mod accessory_common;`, so a helper one file does not use is dead there. (The file is
//! itself a test target with no tests, which keeps it beside the `accessory_*.rs` files that share it.)

#![allow(dead_code)]
#![allow(clippy::missing_panics_doc, clippy::missing_errors_doc, clippy::must_use_candidate)] // test helpers; a panic is the failure
#![allow(clippy::many_single_char_names)] // the geometry formulas use the textbook names
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::chunks_exact_to_as_chunks)] // test arithmetic
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

use std::collections::HashMap;
use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone, Pose};
use omuzaki_char_api::{
    Assembler, Attach, AttachOffset, BodyFit, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId,
    CharacterDef, HeadFact, HeadSurface, JointRef, PartDef, PartId, PartKindId, PartMesh, PartOutput, Published,
    Recoloured, build_part, plan, recolour_part,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use serde_json::{Value, json};

/// Takes the address of `omuzaki_char::link`, so every test binary that includes this links the kit's kinds (the body,
/// head, hair and accessories it builds): their registrations are `inventory` entries, which the linker drops from a
/// binary that references nothing of their crate (ADR 0015).
#[used]
static LINKED: fn() = omuzaki_char::link;

/// The four built-in templates.
pub const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

/// The id of the accessory part in every character built here.
pub const ACC: &str = "acc";

pub fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

pub fn part(kind: &str, layer: i32, attach: Option<Attach>, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer, attach, params }
}

pub fn bone(joint: JointRef, translation: [f64; 3]) -> Attach {
    Attach::Bone { bone: joint, offset: AttachOffset { translation, ..AttachOffset::default() } }
}

/// The hairstyles the real `hair` kind makes on `main` today (the seed styles are not in yet): the default style and
/// each generator alone on the scalp shell.
pub fn hairstyles() -> Vec<(&'static str, Value)> {
    let cap = json!({ "generator": { "type": "cap" } });
    vec![
        ("default", json!({})),
        ("cap", json!({ "groups": { "a-cap": cap } })),
        ("cap-bangs", json!({ "groups": { "a-cap": cap, "b-bangs": { "generator": { "type": "bangs" } } } })),
        (
            "cap-curtain",
            json!({ "groups": { "a-cap": cap, "c-curtain": { "generator": { "type": "curtain" }, "dynamics": { "simulate": true, "follow": "body" } } } }),
        ),
    ]
}

/// A real body and head of `template`, optionally with the hairstyle `hair`, and the accessories `accessories` as
/// `(part id, kind, params, attach)`.
pub fn character_with(
    template: &str,
    hair: Option<&Value>,
    accessories: &[(&str, &str, Value, Option<Attach>)],
) -> CharacterDef {
    let mut def = mock_character().unwrap();
    def.template = template.parse().unwrap();
    def.parts.clear();
    def.parts.insert(pid("body"), part("body", 0, None, json!({})));
    def.parts.insert(pid("head"), part("head", 0, None, json!({})));
    if let Some(hair) = hair {
        def.parts.insert(pid("hair"), part("hair", 10, None, hair.clone()));
    }
    for (i, (id, kind, params, attach)) in accessories.iter().enumerate() {
        let layer = 50 + i32::try_from(i).unwrap();
        def.parts.insert(pid(id), part(kind, layer, attach.clone(), params.clone()));
    }
    def
}

/// One accessory `kind` as part `acc`.
pub fn character(template: &str, kind: &str, params: Value, attach: Option<Attach>) -> CharacterDef {
    character_with(template, None, &[(ACC, kind, params, attach)])
}

/// A built character and the head surface its head part published.
pub struct Scene {
    pub built: BuiltCharacter,
    pub head: Arc<dyn HeadSurface>,
    /// The body's outermost fit when the accessory was built.
    pub fit: Arc<dyn BodyFit>,
}

/// Builds `def` part by part, keeping the head surface the head published.
pub fn scene(def: &CharacterDef) -> Result<Scene, BuildError> {
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, def, &options);
    for p in plan(def)? {
        let output = build_part(def, &p, &options, &services, assembler.published())?;
        assembler.add(&p, output)?;
    }
    let head = assembler.published().get::<HeadFact>().clone().expect("the character has a head");
    let fit = assembler.published().outer_fit().expect("the character has a body").clone();
    Ok(Scene { built: assembler.finish()?, head, fit })
}

pub fn built_scene(template: &str, kind: &str, params: Value, attach: Option<Attach>) -> Scene {
    scene(&character(template, kind, params, attach)).unwrap_or_else(|e| panic!("{kind} on {template}: {e}"))
}

/// The meshes of part `name`.
pub fn meshes_of<'a>(b: &'a BuiltCharacter, name: &str) -> Vec<&'a PartMesh> {
    b.meshes.iter().filter(|m| m.part == pid(name)).map(|m| &m.mesh).collect()
}

/// The meshes of the accessory part.
pub fn meshes(b: &BuiltCharacter) -> Vec<&PartMesh> {
    meshes_of(b, ACC)
}

/// Every vertex of the accessory part's meshes, bind space.
pub fn vertices(b: &BuiltCharacter) -> Vec<Vec3> {
    vertices_of(b, ACC)
}

/// Every vertex of part `name`'s meshes, bind space.
pub fn vertices_of(b: &BuiltCharacter, name: &str) -> Vec<Vec3> {
    meshes_of(b, name).iter().flat_map(|m| m.geometry.positions.iter().copied()).collect()
}

/// How far `p` stands outside the head surface (negative inside), along the direction from the head centre.
pub fn above_head(head: &dyn HeadSurface, p: Vec3) -> f32 {
    let from_centre = p - head.centre();
    from_centre.length() - head.radius(from_centre.normalize_or_zero())
}

// ------------------------------------------------------------------------------------------------ closed shells

/// Welds positions that coincide (a wrapped seam repeats its vertices), drops the triangles that collapse to a line (a
/// pole's fan), and checks that every directed edge has exactly one opposite: a closed, consistently wound surface.
/// Returns the signed volume, which is positive when the normals point outward.
pub fn closed_volume(mesh: &PartMesh) -> f32 {
    let g = &mesh.geometry;
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    let weld: Vec<u32> = g
        .positions
        .iter()
        .map(|p| {
            let key = [p.x, p.y, p.z].map(|v| (f64::from(v) * 1e6).round() as i64);
            let next = u32::try_from(ids.len()).unwrap();
            *ids.entry(key).or_insert(next)
        })
        .collect();
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    let mut volume = 0.0_f32;
    for t in g.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| weld[i as usize]);
        if a == b || b == c || a == c {
            continue;
        }
        for edge in [(a, b), (b, c), (c, a)] {
            *edges.entry(edge).or_default() += 1;
        }
        let [pa, pb, pc] = [t[0], t[1], t[2]].map(|i| g.positions[i as usize]);
        volume += pa.dot(pb.cross(pc)) / 6.0;
    }
    for (&(a, b), &n) in &edges {
        assert_eq!(n, 1, "edge {a}->{b} is used {n} times in {}", mesh.name);
        assert_eq!(edges.get(&(b, a)), Some(&1), "edge {a}->{b} has no opposite in {}: the shell is open", mesh.name);
    }
    volume
}

/// Every mesh of the accessory is a closed shell with outward normals.
pub fn assert_closed(b: &BuiltCharacter, what: &str) {
    let all = meshes(b);
    assert!(!all.is_empty(), "{what}: built no mesh");
    for mesh in all {
        let v = closed_volume(mesh);
        assert!(v > 0.0, "{what}: {} has volume {v}: its normals point inward", mesh.name);
    }
}

/// Every vertex weight set sums to one and names a joint of the rig.
pub fn assert_weights_valid(b: &BuiltCharacter, what: &str) {
    for mesh in meshes(b) {
        for (v, w) in mesh.geometry.weights.iter().enumerate() {
            let sum: f32 = w.iter().sum();
            assert!((sum - 1.0).abs() < 1e-4, "{what}: {} vertex {v} weights sum to {sum}", mesh.name);
        }
        for joint in &mesh.joint_bones {
            assert!(
                b.rig.bone(joint).is_some(),
                "{what}: {} is skinned to {joint:?}, which is not in the rig",
                mesh.name
            );
        }
    }
}

// ------------------------------------------------------------------------------------------------ posing

/// The bones, joints and weights `mesh` is posed with. A simulated mesh (unskinned, its runtime moves it) is taken at
/// its rest shape carried by the head, which is where its runtime holds it when nothing swings it.
fn skin_of(mesh: &PartMesh) -> (Vec<JointRef>, Vec<[u16; 4]>, Vec<[f32; 4]>) {
    let g = &mesh.geometry;
    if mesh.dynamic && g.joints.is_empty() {
        let n = g.positions.len();
        return (vec![JointRef::HEAD], vec![[0; 4]; n], vec![[1.0, 0.0, 0.0, 0.0]; n]);
    }
    (mesh.joint_bones.clone(), g.joints.clone(), g.weights.clone())
}

/// The positions of `mesh` skinned with the local pose `local`.
pub fn skinned(b: &BuiltCharacter, mesh: &PartMesh, local: &Pose) -> Vec<Vec3> {
    let skeleton = &b.rig.skeleton;
    let inverse_bind = b.rig.inverse_bind();
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(local, &mut world);
    let (bones, joints, weights) = skin_of(mesh);
    let matrices: Vec<Mat4> = bones
        .iter()
        .map(|joint| {
            let id = b.rig.bone(joint).unwrap_or_else(|| panic!("{joint:?} is not in the rig")).index();
            world.local[id].to_mat4() * inverse_bind[id]
        })
        .collect();
    let g = &mesh.geometry;
    (0..g.positions.len())
        .map(|v| {
            (0..4).map(|k| matrices[usize::from(joints[v][k])].transform_point3(g.positions[v]) * weights[v][k]).sum()
        })
        .collect()
}

/// The geometry of `mesh` skinned with the local pose `local`: positions and normals (normals blended and renormalised).
pub fn skinned_geometry(b: &BuiltCharacter, mesh: &PartMesh, local: &Pose) -> omuzaki_core::mesh::MeshData {
    let skeleton = &b.rig.skeleton;
    let inverse_bind = b.rig.inverse_bind();
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(local, &mut world);
    let (bones, joints, weights) = skin_of(mesh);
    let matrices: Vec<Mat4> = bones
        .iter()
        .map(|joint| {
            let id = b.rig.bone(joint).unwrap_or_else(|| panic!("{joint:?} is not in the rig")).index();
            world.local[id].to_mat4() * inverse_bind[id]
        })
        .collect();
    let mut out = mesh.geometry.clone();
    for v in 0..out.positions.len() {
        let (mut p, mut n) = (Vec3::ZERO, Vec3::ZERO);
        for k in 0..4 {
            let (m, w) = (&matrices[usize::from(joints[v][k])], weights[v][k]);
            p += m.transform_point3(mesh.geometry.positions[v]) * w;
            n += m.transform_vector3(mesh.geometry.normals[v]) * w;
        }
        out.positions[v] = p;
        out.normals[v] = n.normalize_or_zero();
    }
    out
}

/// Where `bone` is in the skeleton's rest pose.
pub fn rest_joint(b: &BuiltCharacter, bone: HumanoidBone) -> Vec3 {
    let skeleton = &b.rig.skeleton;
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&skeleton.rest_pose(), &mut world);
    world.local[skeleton.humanoid_bone(bone).unwrap().index()].translation
}

/// Posing `bone` by `turn` moves every vertex of every accessory mesh as a rigid turn of the bone's joint.
pub fn assert_rides_the_bone(b: &BuiltCharacter, bone: HumanoidBone, turn: Quat, what: &str) {
    let skeleton = &b.rig.skeleton;
    let id = skeleton.humanoid_bone(bone).unwrap().index();
    let rest = skeleton.rest_pose();
    let mut posed = rest.clone();
    posed.local[id].rotation = turn * rest.local[id].rotation;
    let pivot = rest_joint(b, bone);
    for mesh in meshes(b) {
        let (before, after) = (skinned(b, mesh, &rest), skinned(b, mesh, &posed));
        let mut worst = 0.0_f32;
        for (p, q) in before.iter().zip(&after) {
            worst = worst.max((pivot + turn * (*p - pivot)).distance(*q));
        }
        assert!(worst < 1e-4, "{what}: {} is {worst} m off a rigid turn of the {bone:?}", mesh.name);
    }
}

// ------------------------------------------------------------------------------------------------ colour and speed

/// Builds every part before `ACC` and returns what they published, to build the accessory against.
pub fn published_before(def: &CharacterDef) -> Published {
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, def, &options);
    for p in plan(def).unwrap() {
        if p == pid(ACC) {
            break;
        }
        let output = build_part(def, &p, &options, &services, assembler.published()).unwrap();
        assembler.add(&p, output).unwrap();
    }
    assembler.published().clone()
}

/// The accessory alone, built against what the parts before it published.
pub fn accessory_output(def: &CharacterDef, published: &Published) -> PartOutput {
    build_part(def, &pid(ACC), &BuildOptions::default(), &BuildServices::default(), published).unwrap()
}

/// Mean milliseconds to build the accessory alone.
pub fn build_ms(def: &CharacterDef, runs: u32) -> f64 {
    let published = published_before(def);
    let start = std::time::Instant::now();
    for _ in 0..runs {
        accessory_output(def, &published);
    }
    start.elapsed().as_secs_f64() * 1e3 / f64::from(runs)
}

/// The budget for one accessory build, milliseconds: 5 ms optimised; a debug build gets a ceiling that still catches a
/// pathological slowdown.
pub fn build_budget_ms() -> f64 {
    if cfg!(debug_assertions) { 100.0 } else { 5.0 }
}

/// A palette change recolours the accessory in place: `def` colours its slots from the palette entries `names`
/// (`"@name"` in its parameters). Recolouring a built output to a new palette gives the materials a fresh build gives
/// and leaves the geometry alone.
pub fn assert_recolours_in_place(mut def: CharacterDef, names: &[&str]) {
    for (i, name) in names.iter().enumerate() {
        def.palette.0.insert(name.parse().unwrap(), format!("#3366{:02x}", 40 * i + 20).parse().unwrap());
    }
    let published = published_before(&def);
    let mut output = accessory_output(&def, &published);
    let before = output.materials.clone();
    for (i, name) in names.iter().enumerate() {
        def.palette.0.insert(name.parse().unwrap(), format!("#99{:02x}66", 40 * i + 20).parse().unwrap());
    }
    let fresh = accessory_output(&def, &published);
    let done = recolour_part(&def, &pid(ACC), &BuildServices::default(), &mut output).unwrap();
    assert_eq!(done, Recoloured::Done);
    assert_eq!(output.materials, fresh.materials, "recolouring gives the materials a rebuild gives");
    assert_ne!(output.materials, before, "the palette changed the colours");
    assert_eq!(output.meshes, fresh.meshes, "the geometry is untouched");
}

// ------------------------------------------------------------------------------------------------ the styles of each kind

/// The values of an enum parameter in its schema: `enum`, or, for a documented enum, a `oneOf` (or `anyOf`) of `const`s.
fn enum_values(property: &Value) -> Vec<String> {
    let listed = property["enum"].as_array().into_iter().flatten().filter_map(Value::as_str);
    let documented = ["oneOf", "anyOf"]
        .into_iter()
        .filter_map(|key| property[key].as_array())
        .flatten()
        .filter_map(|entry| entry["const"].as_str().or_else(|| entry["enum"][0].as_str()));
    listed.chain(documented).map(str::to_owned).collect()
}

/// Every documented preset of `kind` is among its styles: a discovery that missed some would sweep fewer cases than the
/// kind has.
pub fn assert_styles_found(kind: &str, presets: &[&str]) {
    let found: Vec<String> = styles(kind).into_iter().map(|(name, _)| name).collect();
    for preset in presets {
        assert!(
            found.iter().any(|f| f == preset),
            "{kind}: the preset '{preset}' is not among the styles found: {found:?}"
        );
    }
}

/// The styles of `kind`, found from the kind's own parameter schema, so nothing here lists the kinds: one per value of its
/// first enum parameter (`style`, `shape` or `kind`), and one per flag that is off by default turned on; `("default", {})` for
/// a kind with neither. Each is `(name, parameters)`.
pub fn styles(kind: &str) -> Vec<(String, Value)> {
    let registration = omuzaki_char_api::find_kind(kind).unwrap_or_else(|| panic!("{kind} is not registered"));
    let schema = serde_json::to_value((registration.schema)()).unwrap();
    let defaults = (registration.defaults)();
    let mut out = Vec::new();
    let properties = schema["properties"].as_object().cloned().unwrap_or_default();
    for key in ["style", "shape", "kind"] {
        if let Some(values) = properties.get(key).map(enum_values).filter(|v| !v.is_empty()) {
            out.extend(values.into_iter().map(|v| (v.clone(), json!({ key: v }))));
            break;
        }
    }
    for (key, property) in &properties {
        if property["type"] == "boolean" && defaults[key] == json!(false) {
            out.push((key.clone(), json!({ key: true })));
        }
    }
    if out.is_empty() {
        out.push(("default".to_owned(), json!({})));
    }
    out
}

// ------------------------------------------------------------------------------------------------ distances

/// The distance from `p` to the triangle `a b c` (Ericson, *Real-Time Collision Detection*, 5.1.5).
pub fn point_triangle_distance(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> f32 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return p.distance(a);
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return p.distance(b);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return p.distance(a + ab * (d1 / (d1 - d3)));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return p.distance(c);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return p.distance(a + ac * (d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return p.distance(b + (c - b) * w);
    }
    let denom = 1.0 / (va + vb + vc);
    p.distance(a + ab * (vb * denom) + ac * (vc * denom))
}

/// The distance from `p` to the nearest triangle of `mesh`.
pub fn distance_to_mesh(p: Vec3, mesh: &PartMesh) -> f32 {
    let g = &mesh.geometry;
    g.indices
        .chunks_exact(3)
        .map(|t| {
            point_triangle_distance(
                p,
                g.positions[t[0] as usize],
                g.positions[t[1] as usize],
                g.positions[t[2] as usize],
            )
        })
        .fold(f32::MAX, f32::min)
}

/// The mesh of the accessory part called `<part>-<suffix>`.
pub fn mesh_named<'a>(b: &'a BuiltCharacter, suffix: &str) -> &'a PartMesh {
    meshes(b)
        .into_iter()
        .find(|m| m.name.ends_with(suffix))
        .unwrap_or_else(|| panic!("the accessory has no mesh '{suffix}'"))
}

/// Whether `p` lies inside the closed mesh (the generalised winding number is about 1: Van Oosterom and Strackee's solid
/// angle of each triangle, summed).
pub fn inside_mesh(p: Vec3, mesh: &PartMesh) -> bool {
    let g = &mesh.geometry;
    let mut total = 0.0_f64;
    for t in g.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| (g.positions[i as usize] - p).as_dvec3());
        let (la, lb, lc) = (a.length(), b.length(), c.length());
        let numerator = a.dot(b.cross(c));
        let denominator = la * lb * lc + a.dot(b) * lc + b.dot(c) * la + c.dot(a) * lb;
        total += 2.0 * numerator.atan2(denominator);
    }
    total / (4.0 * std::f64::consts::PI) > 0.5
}

/// Every vertex has a vertex at its mirror image across the plane `x = centre_x`, within `tolerance`: the pair is one
/// shape and its reflection. (Sorting both lists and comparing in order would pair vertices wrongly wherever rounding
/// puts two of them on either side of a key boundary.)
pub fn assert_mirror_symmetric(points: &[Vec3], centre_x: f32, tolerance: f32, what: &str) {
    let worst = points
        .iter()
        .map(|p| {
            let mirrored = Vec3::new(2.0 * centre_x - p.x, p.y, p.z);
            points.iter().map(|q| q.distance(mirrored)).fold(f32::MAX, f32::min)
        })
        .fold(0.0, f32::max);
    assert!(worst < tolerance, "{what}: a vertex is {worst} m from the nearest vertex of the mirror image");
}

/// The centre of the bounding box of `points`.
pub fn box_centre(points: &[Vec3]) -> Vec3 {
    let (lo, hi) =
        points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), p| (l.min(*p), h.max(*p)));
    (lo + hi) / 2.0
}

// ------------------------------------------------------------------------------------------------ the rig

/// The pose the meshes were built in: the skeleton's rest turned by the rig's bind pose (the rest rotations are identity),
/// which is the pose in which a skinned mesh is the built mesh.
pub fn bind_pose(b: &BuiltCharacter) -> Pose {
    let skeleton = &b.rig.skeleton;
    let mut rest = skeleton.rest_pose();
    for (bone, id) in skeleton.humanoid_map().expect("a humanoid").iter() {
        rest.local[id.index()].rotation = b.rig.bind.get(bone);
    }
    let hips = skeleton.humanoid_map().unwrap().hips().index();
    rest.local[hips].translation += b.rig.bind.hips_offset;
    rest
}

/// Skinning at the bind pose gives back the built vertices: the bones and their inverse bind matrices agree with the mesh.
pub fn assert_skins_to_the_built_mesh_at_bind(b: &BuiltCharacter, what: &str) {
    let bind = bind_pose(b);
    for mesh in meshes(b) {
        let posed = skinned(b, mesh, &bind);
        let worst = posed.iter().zip(&mesh.geometry.positions).map(|(p, q)| p.distance(*q)).fold(0.0, f32::max);
        assert!(worst < 1e-4, "{what}: {} skins {worst} m away from the built mesh at the bind pose", mesh.name);
    }
}

/// The most backward point of the torso over its height: where nothing behind the back may touch.
pub fn back_of_the_torso(fit: &dyn BodyFit) -> f32 {
    (0..=20)
        .map(|k| {
            let section = fit.section(omuzaki_char_api::BodyRegion::Torso, k as f32 / 20.0).unwrap();
            section.centre.z - section.radii.y
        })
        .fold(f32::MAX, f32::min)
}
