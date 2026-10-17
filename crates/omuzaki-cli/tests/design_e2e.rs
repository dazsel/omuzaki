//! Design a kit character end to end, through the op layer (#2216): an empty project, then the body, the face, the hair,
//! accessories and the look, every step an op the edit panels and an agent over MCP both call; then the project is saved
//! and reopened, and the reopened character is the same character.
//!
//! The rights are signed off through the licence ops and the export preflight passes (`io.check_export`); the character
//! is written as a VRM 1.0 file and read back, and the avatar that comes back is the one that went out.
//!
//! The finished character is rendered as a turnaround (front, three-quarter, side and back, the face painted) before and
//! after the reopen, pixel for pixel the same; `OMUZAKI_DESIGN_E2E_DIR` writes the views as PNG for the evidence sheet.
//!
//! The script is one list of `(op, args)` steps. It runs through the runner the CLI opens, and again as tool calls of an
//! MCP client talking to `omuzaki mcp`'s server over that same kind of runner, and both make the same document.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::path::Path;
use std::sync::Arc;

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, RegistryBuilder,
};
use omuzaki_cli::cli::try_parse_from;
use omuzaki_cli::session::{Session, open_executor_with, open_runner};
use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_doc_api::DocPath;
use omuzaki_io_api::{
    ExportCtx, ExportOptions, ImportCtx, ImportInput, ImportOptions, IoScene, PerfectSync, run_export, run_import,
};
use omuzaki_io_ops::gather::avatar::{LicenceRecord, add_avatar, add_rights, record_path};
use omuzaki_io_ops::gather::{GatherOptions, GatherPurpose, gather};
use omuzaki_io_vrm::{Vrm1Exporter, Vrm1Importer};
use omuzaki_mcp::{McpOptions, McpServer};
use omuzaki_ops_api::{Actor, Grants, Mode, Outcome, Request, Runner};
use omuzaki_render_api::{AlphaEncoding, Camera, Image, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::face::{FaceRig, InlineExecutor};
use omuzaki_stage::frame::{Stage, StageClock};
use omuzaki_stage::ops::render::still::encode_png;
use omuzaki_stage::sheet::turnaround::{Lens, TurnaroundSpec, render_turnaround};
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use serde_json::{Value, json};

/// The character the script designs.
const CHARACTER: &str = "kit";

/// The design, step by step: what a person does in the edit panels, or an agent over MCP, to make the character.
fn script() -> Vec<(&'static str, Value)> {
    let c = CHARACTER;
    vec![
        // An empty project gets a character on the standard template: a body, a head and hair to start from.
        ("character.create", json!({"id": c, "name": "Kit", "template": "standard", "seed": 7})),
        // The body: a slightly fuller build, longer legs, mitten hands.
        ("body.set_shape", json!({"character": c, "axis": "build", "value": 0.2})),
        ("body.set_proportion", json!({"character": c, "name": "leg_length", "value": 1.08})),
        ("body.set_hand_style", json!({"character": c, "style": "mitten"})),
        // The face: the layout, the eyes and brows, a beauty mark.
        ("face.set_layout", json!({"character": c, "field": "eye_spacing", "value": 0.09})),
        ("face.set_style", json!({"character": c, "path": "eyes/iris/mid", "value": "#4a7fb0"})),
        ("face.set_style", json!({"character": c, "path": "brows/arch", "value": 0.006})),
        (
            "face.add_mark",
            json!({"character": c, "mark": {
                "kind": "beauty_mark", "at": [0.03, -0.125], "size": 0.002, "colour": "#5a3a30", "opacity": 0.9,
                "count": 1, "seed": 1,
            }}),
        ),
        // The hair: a swept fringe and a darker colour.
        (
            "hair.bangs.set",
            json!({"character": c, "part": "hair", "group": "b-bangs", "field": "sweep_deg", "value": 12.0}),
        ),
        ("part.set_param", json!({"character": c, "part": "hair", "path": "shading/mid", "value": "#6b4a3a"})),
        // Accessories: a hairpin and a choker.
        ("part.add", json!({"character": c, "kind": "hairpin", "id": "pin"})),
        ("part.add", json!({"character": c, "kind": "choker", "id": "choker"})),
        // The look: the skin tone and the shading preset.
        ("palette.set", json!({"character": c, "slot": "skin", "colour": "#f6d2c4"})),
        ("look.set_preset", json!({"character": c, "preset": "soft-sd"})),
    ]
}

/// A runner over the project folder `dir`, as `omuzaki --project <dir> mcp` and the CLI's own commands open it.
fn runner(dir: &Path) -> Arc<dyn Runner> {
    let cli = try_parse_from(["omuzaki", "--project", dir.to_str().unwrap(), "ops"]).expect("a valid command line");
    open_runner(&cli).expect("the project opens")
}

/// Runs `op` with `args` as a person, applied.
fn run(runner: &dyn Runner, op: &str, args: Value) -> Outcome {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: Actor::Person { name: "designer".to_owned() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).unwrap_or_else(|e| panic!("{op} failed: {} ({:?})", e.message, e.hint))
}

/// The whole document, as the runner reads it.
fn document(runner: &dyn Runner) -> Value {
    runner.get(&DocPath::root(), &Grants::person()).expect("the person may read").expect("the project has a document")
}

/// The character built from `doc` at the export quality, as the stage and the exporter build it.
fn build(doc: &Value) -> BuiltCharacter {
    let node = &doc["characters"][CHARACTER];
    let def = CharacterDef::from_node(node).expect("the character's node reads back");
    let options = BuildOptions { variant: None, quality: omuzaki_char_api::Quality::Offline, rig_export: true };
    RegistryBuilder
        .build(&CharId::new(CHARACTER).unwrap(), &def, &options, &BuildServices::default())
        .unwrap_or_else(|e| panic!("the designed character doesn't build: {e}"))
}

/// Every float and index of `geometry` as raw bits, attribute after attribute: positions, normals, UVs, colours, joint
/// indices, weights, the extra attributes (name, width and data) and the triangle indices. Two meshes give the same
/// bits only when they are bit for bit the same (`==` on floats would call `0.0` and `-0.0` equal). The pattern names
/// every field, so a field added to `MeshData` stops this compiling until it is compared too.
fn geometry_bits(geometry: &MeshData) -> Vec<u32> {
    let MeshData { positions, normals, uvs, colours, joints, weights, extra, indices } = geometry;
    let mut bits = Vec::new();
    for v in positions.iter().chain(normals) {
        bits.extend(v.to_array().map(f32::to_bits));
    }
    bits.extend(uvs.iter().flat_map(|uv| uv.to_array().map(f32::to_bits)));
    bits.extend(colours.iter().chain(weights).flat_map(|c| c.map(f32::to_bits)));
    bits.extend(joints.iter().flat_map(|j| j.map(u32::from)));
    for attribute in extra {
        bits.extend(attribute.name.as_str().bytes().map(u32::from));
        bits.push(u32::from(attribute.components));
        bits.extend(attribute.data.iter().map(|x| x.to_bits()));
    }
    bits.extend(indices);
    bits
}

/// Asserts that `rebuilt` is the character `built` was: every mesh bit for bit (all of [`geometry_bits`]), with the
/// same joint bindings, material and draw settings; the same materials, textures, skeleton and springs, and face
/// binding.
fn assert_same_build(built: &BuiltCharacter, rebuilt: &BuiltCharacter) {
    assert_eq!(rebuilt.meshes.len(), built.meshes.len(), "the number of meshes");
    for (a, b) in built.meshes.iter().zip(&rebuilt.meshes) {
        let name = format!("{}/{}", a.part, a.mesh.name);
        assert_eq!((&b.part, b.local, &b.mesh.name), (&a.part, a.local, &a.mesh.name), "mesh order");
        assert!(geometry_bits(&b.mesh.geometry) == geometry_bits(&a.mesh.geometry), "{name}: the geometry differs");
        assert_eq!(b.mesh.joint_bones, a.mesh.joint_bones, "{name}: the joint bindings");
        assert_eq!(
            (&b.mesh.material, b.mesh.casts_shadow, b.mesh.render_order, b.mesh.dynamic),
            (&a.mesh.material, a.mesh.casts_shadow, a.mesh.render_order, a.mesh.dynamic),
            "{name}: material and draw settings",
        );
    }
    assert!(rebuilt.materials == built.materials, "the materials differ");
    assert!(rebuilt.textures == built.textures, "the textures differ");
    assert!(rebuilt.rig == built.rig, "the skeleton or the springs differ");
    assert!(rebuilt.face == built.face, "the face binding differs");
}

/// One view of the turnaround: its size, and its pixels.
const CELL: UVec2 = UVec2::new(256, 512);

/// The turnaround of `built` through the deterministic mock renderer, with the production face rig painting the face
/// (inline, so every view has its face before it is drawn): `(view, image)` in front, three-quarter, side, back order.
fn turnaround(built: BuiltCharacter) -> Vec<(String, Image)> {
    let mut renderer = MockRenderer::new();
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::splat(32), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let options = UploadOptions { face_px_per_m: 2000.0 };
    let mut stage = Stage::new(
        &mut renderer,
        built,
        options,
        Scene::new(Camera::default()),
        target,
        Quality::live(),
        StageClock::fixed(60.0),
    )
    .expect("the designed character uploads");
    stage.set_face(&mut renderer, FaceRig::standard().with_executor(Box::new(InlineExecutor))).expect("the face binds");
    let spec = TurnaroundSpec::new(CELL).with_lens(Lens::Orthographic);
    let sheet = render_turnaround(&mut stage, &mut renderer, &spec).expect("the turnaround renders");
    stage.release(&mut renderer).expect("the stage gives its resources back");
    sheet.views.into_iter().map(|v| (format!("{:?}", v.view).to_lowercase(), v.image)).collect()
}

/// Writes `views` as `<name>-<view>.png` into `OMUZAKI_DESIGN_E2E_DIR` when it is set (the evidence sheet).
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn keep(name: &str, views: &[(String, Image)]) {
    let Some(dir) = std::env::var_os("OMUZAKI_DESIGN_E2E_DIR") else { return };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (view, image) in views {
        std::fs::write(dir.join(format!("{name}-{view}.png")), encode_png(image).unwrap()).unwrap();
    }
}

/// The rights sign-off a person makes before an export: the record the parts allow (`licence.derive`), with the
/// designer named as the author, stored with `licence.set`.
fn sign_off(runner: &dyn Runner) {
    let derived = run(runner, "licence.derive", json!({"character": CHARACTER})).plan.output.expect("an answer");
    let mut record = derived["record"].clone();
    record["provenance"]["author"] = json!("Kit's designer");
    record["meta"]["authors"] = json!(["Kit's designer"]);
    run(runner, "licence.set", json!({"character": CHARACTER, "record": record}));
}

/// The designed character as a VRM 1.0 file, the way `io.export` is to write it (gather, rights, avatar, the exporter),
/// with the face channels left to #2057's baker: perfect sync off, the VRM presets only.
fn export_vrm(doc: &Value, built: &BuiltCharacter) -> (IoScene, Vec<u8>) {
    let path = record_path(&CharId::new(CHARACTER).unwrap());
    let node = path.segments().iter().fold(doc, |node, key| &node[key.as_str()]);
    let record = LicenceRecord::from_node(node).expect("the signed-off record reads back");
    let mut gathered =
        gather(built, &GatherOptions { target_height: None, purpose: GatherPurpose::Vrm }).expect("gathers");
    add_rights(&mut gathered.scene, built, Some(&record));
    add_avatar(&mut gathered, built).expect("the avatar block");
    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
    let outcome = run_export(&Vrm1Exporter, &gathered.scene, &ExportCtx::new(&options)).expect("the VRM exports");
    let bytes = outcome.files.into_iter().next().expect("the .vrm file").bytes;
    (gathered.scene, bytes)
}

/// How far a value read back from the VRM file may be from the value written, by kind. The file stores 32-bit floats,
/// and neither the exporter nor the importer converts units or axes for VRM 1.0, so these allow float rounding only:
/// a micrometre for lengths, a millionth for unit quantities and quaternion components.
mod tolerance {
    /// Positions, morph offsets and translations, metres.
    pub const LENGTH_M: f32 = 1e-6;
    /// Normals, UVs, vertex colours, skin weights, scales, rotation quaternion components, material factors.
    pub const UNIT: f32 = 1e-6;
    /// Inverse bind matrix entries (rotation, scale and metres mixed).
    pub const MATRIX: f32 = 1e-5;
}

/// How far `y` is from `x`. A NaN on either side is infinite drift: a VRM never holds one, and `f32::max` would
/// silently drop it.
fn drift(x: f32, y: f32) -> f32 {
    if x.is_nan() || y.is_nan() { f32::INFINITY } else { (x - y).abs() }
}

#[test]
fn a_nan_on_either_side_is_infinite_drift() {
    assert!(drift(1.0, f32::NAN).is_infinite());
    assert!(drift(f32::NAN, 1.0).is_infinite());
    assert!(drift(f32::NAN, f32::NAN).is_infinite());
    let mut d = Drift::default();
    d.floats("unit", tolerance::UNIT, "nan check", &[0.0, 1.0], &[0.0, f32::NAN]);
    assert!(d.0["unit"].0.is_infinite());
}

/// The largest difference seen per kind of value in a VRM round trip, against the [`tolerance`] for that kind.
#[derive(Default)]
struct Drift(std::collections::BTreeMap<&'static str, (f32, f32)>);

impl Drift {
    /// Records `a` against `b` (same length, or the comparison fails) under `kind` with `limit`.
    fn floats(&mut self, kind: &'static str, limit: f32, what: &str, a: &[f32], b: &[f32]) {
        assert_eq!(a.len(), b.len(), "{what}: {kind} count");
        let worst = a.iter().zip(b).map(|(&x, &y)| drift(x, y)).fold(0.0, |m: f32, d| if d > m { d } else { m });
        let entry = self.0.entry(kind).or_insert((0.0, limit));
        entry.0 = entry.0.max(worst);
    }

    /// Records every number in two JSON values of the same shape under `kind`; anything else in them must be equal.
    fn json(&mut self, kind: &'static str, limit: f32, what: &str, a: &Value, b: &Value) {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => {
                #[allow(clippy::cast_possible_truncation, reason = "the file's values are f32")]
                let (x, y) = (x.as_f64().unwrap() as f32, y.as_f64().unwrap() as f32);
                self.floats(kind, limit, what, &[x], &[y]);
            }
            (Value::Array(x), Value::Array(y)) => {
                assert_eq!(x.len(), y.len(), "{what}: list length");
                x.iter().zip(y).for_each(|(x, y)| self.json(kind, limit, what, x, y));
            }
            (Value::Object(x), Value::Object(y)) => {
                assert_eq!(x.keys().collect::<Vec<_>>(), y.keys().collect::<Vec<_>>(), "{what}: fields");
                x.iter().for_each(|(key, x)| self.json(kind, limit, &format!("{what}.{key}"), x, &y[key]));
            }
            _ => assert_eq!(a, b, "{what}"),
        }
    }

    /// Fails naming every kind whose drift went past its tolerance; prints the table either way.
    fn check(&self) {
        for (kind, (worst, limit)) in &self.0 {
            println!("VRM round trip: {kind:<22} worst {worst:.3e} (tolerance {limit:.0e})");
        }
        let over: Vec<_> = self.0.iter().filter(|(_, (worst, limit))| worst > limit).map(|(kind, _)| *kind).collect();
        assert!(over.is_empty(), "the VRM round trip moved {over:?} past their tolerances");
    }
}

/// Asserts that `back`, read from the VRM file, is the scene `sent` was written from: the node tree with every bone's
/// transform, the skins and their inverse bind matrices, every primitive's geometry (positions, normals, UVs, colours,
/// joints, weights, indices) and morph targets, the materials' values, the textures and images, the metadata and the
/// avatar block, within [`tolerance`].
fn assert_round_trip(sent: &IoScene, back: &IoScene) {
    use tolerance::{LENGTH_M, MATRIX, UNIT};
    let mut drift = Drift::default();
    let flat3 = |v: &[Vec3]| v.iter().flat_map(Vec3::to_array).collect::<Vec<f32>>();

    assert_eq!(back.nodes.len(), sent.nodes.len(), "the number of nodes");
    assert_eq!(back.roots, sent.roots, "the root nodes");
    for (a, b) in sent.nodes.iter().zip(&back.nodes) {
        let what = format!("node {}", a.name);
        assert_eq!((&b.name, &b.children, b.mesh, b.skin), (&a.name, &a.children, a.mesh, a.skin), "{what}");
        let (ta, tb) = (&a.transform, &b.transform);
        drift.floats("bone translation", LENGTH_M, &what, &ta.translation.to_array(), &tb.translation.to_array());
        drift.floats("bone scale", UNIT, &what, &ta.scale.to_array(), &tb.scale.to_array());
        // q and -q are the same rotation: compare against whichever sign of the read one is nearer.
        let rb = if ta.rotation.dot(tb.rotation) < 0.0 { -tb.rotation } else { tb.rotation };
        drift.floats("bone rotation", UNIT, &what, &ta.rotation.to_array(), &rb.to_array());
    }

    assert_eq!(back.skins.len(), sent.skins.len(), "the number of skins");
    for (a, b) in sent.skins.iter().zip(&back.skins) {
        let what = format!("skin {}", a.name);
        assert_eq!((&b.name, &b.joints, b.skeleton), (&a.name, &a.joints, a.skeleton), "{what}");
        let matrices =
            |s: &omuzaki_io_api::Skin| s.inverse_bind.iter().flat_map(Mat4::to_cols_array).collect::<Vec<_>>();
        drift.floats("inverse bind", MATRIX, &what, &matrices(a), &matrices(b));
    }

    assert_eq!(back.meshes.len(), sent.meshes.len(), "the number of meshes");
    for (a, b) in sent.meshes.iter().zip(&back.meshes) {
        assert_eq!((&b.name, &b.morph_target_names), (&a.name, &a.morph_target_names), "mesh {}", a.name);
        assert_eq!(b.primitives.len(), a.primitives.len(), "mesh {}: primitives", a.name);
        for (n, (pa, pb)) in a.primitives.iter().zip(&b.primitives).enumerate() {
            let what = format!("mesh {} primitive {n}", a.name);
            let (ga, gb) = (&pa.mesh, &pb.mesh);
            assert_eq!(pb.material, pa.material, "{what}: material");
            assert_eq!(gb.indices, ga.indices, "{what}: triangle indices");
            assert_eq!(gb.joints, ga.joints, "{what}: joint indices");
            drift.floats("position (m)", LENGTH_M, &what, &flat3(&ga.positions), &flat3(&gb.positions));
            drift.floats("normal", UNIT, &what, &flat3(&ga.normals), &flat3(&gb.normals));
            let uvs = |g: &MeshData| g.uvs.iter().flat_map(Vec2::to_array).collect::<Vec<_>>();
            drift.floats("uv", UNIT, &what, &uvs(ga), &uvs(gb));
            drift.floats("vertex colour", UNIT, &what, ga.colours.as_flattened(), gb.colours.as_flattened());
            drift.floats("skin weight", UNIT, &what, ga.weights.as_flattened(), gb.weights.as_flattened());
            assert_eq!(pb.morph_targets.len(), pa.morph_targets.len(), "{what}: morph targets");
            for (ma, mb) in pa.morph_targets.iter().zip(&pb.morph_targets) {
                drift.floats("morph offset (m)", LENGTH_M, &what, &flat3(&ma.positions), &flat3(&mb.positions));
                drift.floats("morph normal", UNIT, &what, &flat3(&ma.normals), &flat3(&mb.normals));
            }
        }
    }

    assert_eq!(back.materials.len(), sent.materials.len(), "the number of materials");
    for (a, b) in sent.materials.iter().zip(&back.materials) {
        let json = |m: &omuzaki_io_api::Material| serde_json::to_value(m).expect("a material serialises");
        drift.json("material value", UNIT, &format!("material {}", a.name), &json(a), &json(b));
    }
    assert!(back.textures == sent.textures, "the textures differ");
    assert!(back.images == sent.images, "the images differ");
    assert_eq!(back.meta, sent.meta, "the metadata (rights, authors, licence)");
    assert_eq!(back.avatar, sent.avatar, "the avatar (humanoid, springs, constraints, expressions)");
    drift.check();
}

#[test]
fn a_character_designed_through_ops_saves_reopens_and_builds_the_same() {
    let project = common::new_project();
    let designed = {
        let r = runner(project.path());
        for (op, args) in script() {
            let outcome = run(&*r, op, args);
            assert!(outcome.applied.is_some(), "{op} was not applied");
        }
        document(&*r)
    };
    let built = build(&designed);
    let parts: Vec<&str> = built.meshes.iter().map(|m| m.part.as_str()).collect();
    for part in ["body", "head", "hair", "pin", "choker"] {
        assert!(parts.contains(&part), "the {part} part built no mesh: {parts:?}");
    }

    // A second process opens the saved folder: the document and the character it builds are the same.
    let reopened = document(&*runner(project.path()));
    assert_eq!(reopened, designed, "the reopened document differs from the one designed");
    let rebuilt = build(&reopened);
    assert_same_build(&built, &rebuilt);

    // The export: the rights signed off, the preflight clean, then the file written and read back.
    let r = runner(project.path());
    sign_off(&*r);
    let check = run(&*r, "io.check_export", json!({"character": CHARACTER, "format": "vrm1"})).plan.output.unwrap();
    assert_eq!(check["ok"], true, "the export preflight finds errors: {}", check["report"]);
    let signed = document(&*r);
    let (sent, bytes) = export_vrm(&signed, &build(&signed));
    let back = run_import(&Vrm1Importer, &ImportInput::new(&bytes), &ImportCtx::new(&ImportOptions::default()))
        .expect("the VRM reads back");
    assert_round_trip(&sent, &back.scene);

    // The turnaround of the finished character, and of the reopened one: the same pixels.
    let views = turnaround(built);
    assert_eq!(views.len(), 4, "front, three-quarter, side and back");
    keep("designed", &views);
    let again = turnaround(rebuilt);
    for ((view, image), (_, after)) in views.iter().zip(&again) {
        assert!(image.rgba8 == after.rgba8, "the {view} view of the reopened character differs");
    }
}

/// An MCP client that only calls tools, as an agent designing a character does.
struct Agent;

impl ClientHandler for Agent {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("designer-agent", "1.0"))
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
    }
}

/// The document `script` makes when an agent runs it as MCP tool calls against the server `omuzaki mcp` builds over the
/// project in `dir`: the same executor and sessions, wired as the CLI wires them, with the agent working for the person
/// who started the server.
async fn designed_over_mcp(dir: &Path) -> Value {
    let cli = try_parse_from(["omuzaki", "--project", dir.to_str().unwrap(), "mcp"]).expect("a valid command line");
    let session = Session::person("designer");
    let executor = open_executor_with(&cli, &session.connections).expect("the project opens");
    let options = McpOptions::default()
        .with_connections(session.connections.clone())
        .with_sessions(executor.clone(), session.actor.clone());
    let mcp = McpServer::new(executor.clone(), options).expect("the server builds");
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let serving = mcp.clone();
    let server = tokio::spawn(async move { serving.serve(server_io).await });
    let mut client =
        Agent.serve_with_lifecycle(client_io, ClientLifecycleMode::Initialize).await.expect("the agent connects");
    for (op, args) in script() {
        // A tool is its op with dots as underscores (the MCP name rule).
        let call = CallToolRequestParams::new(op.replace('.', "_")).with_arguments(args.as_object().unwrap().clone());
        let result = client.call_tool(call).await.unwrap_or_else(|e| panic!("{op} over MCP failed: {e}"));
        assert_ne!(result.is_error, Some(true), "{op} over MCP was refused: {:?}", result.content);
    }
    client.close().await.expect("the agent disconnects");
    server.await.expect("the server task").expect("the server ends cleanly");
    document(&*executor)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_design_over_mcp_makes_the_same_document() {
    let (by_runner, by_agent) = (common::new_project(), common::new_project());
    let through_runner = {
        let r = runner(by_runner.path());
        for (op, args) in script() {
            run(&*r, op, args);
        }
        document(&*r)
    };
    let through_mcp = designed_over_mcp(by_agent.path()).await;
    assert_eq!(through_mcp, through_runner, "the agent's character differs from the person's");
}
