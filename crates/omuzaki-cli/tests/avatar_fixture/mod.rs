//! The `avatar` fixture of the op conformance suite: a `demo` project holding one character imported from a VRM as an
//! avatar, so the ops that work on an imported character (`avatar.describe`, `avatar.map_channel`, `avatar.remap`) have
//! examples that really run.
//!
//! The VRM is generated here, written by `omuzaki-io-vrm`'s own exporter into the project folder and imported through
//! the real `io.inspect` and `io.import` ops, the way a person imports one: no model file is checked in, and nothing
//! third-party is used.
//! The store is a disk store inside the project folder, so the suite's before-and-after comparison sees it too.

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_cli::project::SavedProject;
use omuzaki_core::glam::{Mat4, Vec2, Vec3, Vec4};
use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::Value;
use omuzaki_io_api::{
    Avatar, AvatarPermission, CommercialUsage, ExportCtx, ExportOptions, Expression, ExpressionName, ImportCtx,
    ImportInput, ImportOptions, IoScene, Material, Mesh, MeshData, MorphTarget, Node, NodeIx, PerfectSync, Primitive,
    Shading, Skin, VRM_LICENSE_URLS, VrmMeta, run_export, run_import,
};
use omuzaki_io_vrm::{Vrm1Exporter, Vrm1Importer};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Actor, Grants, Mode, ProjectDir, Request, Runner, Services, ServicesBuilder, find};
use omuzaki_store::DiskStore;
use omuzaki_store_api::{Commercial, Provenance, Source, Store};

/// The name examples give in `x-omuzaki-fixture`.
pub const NAME: &str = "avatar";

/// The imported character's id, which the examples name.
pub const CHARACTER: &str = "avatar";

/// The model's file in the project folder, which the import reads.
const FILE: &str = "avatar.vrm";

/// The node that plays `leftHand`, by its index in the file: `avatar.remap`'s example names it.
pub const LEFT_HAND_NODE: usize = 8;

/// The humanoid, parents first: each bone, its parent and its offset from the parent, in VRM space (+Y up, the
/// character's left is +X), arms out in the T-pose. The root is node 0, so bone `i` here is node `i + 1`.
const BONES: [(&str, Option<&str>, [f32; 3]); 17] = [
    ("hips", None, [0.0, 0.9, 0.0]),
    ("spine", Some("hips"), [0.0, 0.1, 0.0]),
    ("chest", Some("spine"), [0.0, 0.15, 0.0]),
    ("neck", Some("chest"), [0.0, 0.2, 0.0]),
    ("head", Some("neck"), [0.0, 0.1, 0.0]),
    ("leftUpperArm", Some("chest"), [0.15, 0.15, 0.0]),
    ("leftLowerArm", Some("leftUpperArm"), [0.25, 0.0, 0.0]),
    ("leftHand", Some("leftLowerArm"), [0.25, 0.0, 0.0]),
    ("rightUpperArm", Some("chest"), [-0.15, 0.15, 0.0]),
    ("rightLowerArm", Some("rightUpperArm"), [-0.25, 0.0, 0.0]),
    ("rightHand", Some("rightLowerArm"), [-0.25, 0.0, 0.0]),
    ("leftUpperLeg", Some("hips"), [0.1, -0.05, 0.0]),
    ("leftLowerLeg", Some("leftUpperLeg"), [0.0, -0.4, 0.0]),
    ("leftFoot", Some("leftLowerLeg"), [0.0, -0.4, 0.0]),
    ("rightUpperLeg", Some("hips"), [-0.1, -0.05, 0.0]),
    ("rightLowerLeg", Some("rightUpperLeg"), [0.0, -0.4, 0.0]),
    ("rightFoot", Some("rightLowerLeg"), [0.0, -0.4, 0.0]),
];

/// A new `demo` project with the avatar imported as character [`CHARACTER`], and its store, which the avatar ops
/// read the model from.
///
/// # Errors
/// What failed, as text for the suite's report: building, writing or importing the model.
pub fn project() -> Result<(tempfile::TempDir, Arc<dyn Store>), String> {
    let dir = crate::common::new_project();
    let store: Arc<dyn Store> =
        Arc::new(DiskStore::open(dir.path().join("store")).map_err(|e| format!("opening the store failed: {e}"))?);
    std::fs::write(dir.path().join(FILE), vrm_bytes()?).map_err(|e| format!("writing the model failed: {e}"))?;
    import(dir.path(), ServicesBuilder::new().insert::<dyn Store>(store.clone()).build())?;
    Ok((dir, store))
}

/// Inspects [`FILE`] as an avatar and imports it with the token the inspection gave, as a person.
fn import(dir: &std::path::Path, services: Services) -> Result<(), String> {
    let doc = SavedProject::open(dir).map_err(|e| format!("opening the fixture failed: {e}"))?;
    let folder = ProjectDir::open(dir).map_err(|e| format!("opening the fixture folder failed: {e}"))?;
    let ops = ["io.inspect", "io.import"].map(|name| find(name).ok_or(format!("{name} is not linked")));
    let [inspect_op, import_op] = ops;
    let runner = Executor::with_registrations(Box::new(doc), Arc::new(SystemClock::new()), [inspect_op?, import_op?])
        .map_err(|e| e.to_string())?
        .with_project_dir(folder)
        .with_services(services);
    let run = |op: &str, args: Value, mode: Mode| {
        let request = Request {
            op: op.to_owned(),
            args,
            mode,
            actor: Actor::Person { name: "conformance".to_owned() },
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        runner.run(request).map_err(|e| format!("{op} failed: {}", e.message))
    };
    let file = serde_json::json!({ "path": FILE, "as": "avatar" });
    let inspected = run("io.inspect", file.clone(), Mode::DryRun)?;
    let confirm = inspected.plan.output.as_ref().map(|o| o["avatar"]["confirm"].clone()).unwrap_or_default();
    let mut args = file;
    args["id"] = CHARACTER.into();
    args["confirm"] = confirm;
    run("io.import", args, Mode::Apply).map(drop)
}

/// The model as `omuzaki-io-vrm` writes it, checked to read back with the left hand at [`LEFT_HAND_NODE`].
fn vrm_bytes() -> Result<Vec<u8>, String> {
    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
    let mut out = run_export(&Vrm1Exporter, &scene(), &ExportCtx::new(&options)).map_err(|e| e.to_string())?;
    let bytes = out.files.pop().ok_or("the exporter wrote no file")?.bytes;
    let input = ImportInput::new(&bytes).named(FILE);
    let back =
        run_import(&Vrm1Importer, &input, &ImportCtx::new(&ImportOptions::default())).map_err(|e| e.to_string())?;
    match back.scene.nodes.get(LEFT_HAND_NODE) {
        Some(node) if node.name == "leftHand" => Ok(bytes),
        other => Err(format!("node {LEFT_HAND_NODE} reads back as {:?}, not leftHand", other.map(|n| &n.name))),
    }
}

/// A T-posed humanoid with a skinned body (two morph targets, one per expression) whose terms let anyone perform as
/// it for profit, so importing it needs no acknowledgement.
fn scene() -> IoScene {
    let mut scene = IoScene::new();
    let root = scene.add_node(Node::new("Root"));
    scene.roots = vec![root];
    let mut avatar = Avatar::new();
    let mut nodes: BTreeMap<&str, NodeIx> = BTreeMap::new();
    let mut world: Vec<Mat4> = Vec::new();
    for (name, parent, offset) in BONES {
        let node = scene.add_node(Node::new(name).at(Vec3::from_array(offset)));
        let (up, up_world) =
            parent.and_then(|p| nodes.get(p).map(|&n| (n, world[n.index() - 1]))).unwrap_or((root, Mat4::IDENTITY));
        scene.attach(up, node);
        // The names are the spec's, so the map takes them.
        avatar.humanoid.assign(name, node).expect("a VRM 1.0 bone name");
        nodes.insert(name, node);
        world.push(up_world * Mat4::from_translation(Vec3::from_array(offset)));
    }
    let material = scene.add_material({
        let mut m = Material::new("Skin", Shading::Toon(Box::default()));
        m.base_color = Vec4::new(0.9, 0.75, 0.7, 1.0);
        m
    });
    let body = add_body(&mut scene, root, &world, material);
    avatar.expressions.insert(ExpressionName::parse("happy").expect("a name"), Expression::morph(body, 0));
    avatar.expressions.insert(ExpressionName::parse("aa").expect("a name"), Expression::morph(body, 1));
    scene.avatar = Some(avatar);
    scene.meta = VrmMeta::new("Conformance Avatar", vec!["Omuzaki".into()], VRM_LICENSE_URLS[0]);
    scene.meta.terms.avatar_permission = AvatarPermission::Everyone;
    scene.meta.terms.commercial_usage = CommercialUsage::PersonalProfit;
    scene.provenance = Some(Provenance {
        source: Source::Authored,
        author: Some("Omuzaki".into()),
        licence: "LicenseRef-Omuzaki-User".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    });
    scene
}

/// The body: a small triangle at each bone, weighted fully to it and bound at the rest, with morph target 0 moving the
/// head's triangle up and target 1 the hips' (one per expression). Drawn by a node under the root.
fn add_body(scene: &mut IoScene, root: NodeIx, world: &[Mat4], material: omuzaki_io_api::MaterialIx) -> NodeIx {
    let mut data = MeshData::default();
    for (j, matrix) in world.iter().enumerate() {
        let at = matrix.w_axis.truncate();
        let first = u32::try_from(data.positions.len()).unwrap_or(u32::MAX);
        for corner in [Vec3::ZERO, Vec3::new(0.02, 0.0, 0.0), Vec3::new(0.0, 0.02, 0.0)] {
            data.positions.push(at + corner);
            data.normals.push(Vec3::Z);
            data.uvs.push(Vec2::ZERO);
            data.joints.push([u16::try_from(j).unwrap_or(0), 0, 0, 0]);
            data.weights.push([1.0, 0.0, 0.0, 0.0]);
        }
        data.indices.extend([first, first + 1, first + 2]);
    }
    let count = data.positions.len();
    // Bone 4 is the head and bone 0 the hips (`BONES`); each target moves that bone's triangle by a centimetre.
    let target = |bone: usize| {
        MorphTarget::new((0..count).map(|v| if v / 3 == bone { Vec3::Y * 0.01 } else { Vec3::ZERO }).collect())
    };
    let mut primitive = Primitive::new(data);
    primitive.material = Some(material);
    primitive.morph_targets = vec![target(4), target(0)];
    let mut mesh = Mesh::new("Body");
    mesh.primitives = vec![primitive];
    mesh.morph_target_names = vec!["Smile".into(), "Open".into()];
    let mesh = scene.add_mesh(mesh);
    let joints: Vec<NodeIx> = (1..=BONES.len()).map(|i| NodeIx(u32::try_from(i).unwrap_or(0))).collect();
    let skin = scene.add_skin(Skin::new("Body_skin", joints, world.iter().map(Mat4::inverse).collect()));
    let mut node = Node::new("Body");
    node.mesh = Some(mesh);
    node.skin = Some(skin);
    let node = scene.add_node(node);
    scene.attach(root, node);
    node
}
