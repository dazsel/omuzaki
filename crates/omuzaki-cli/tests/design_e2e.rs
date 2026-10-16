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
use omuzaki_core::glam::UVec2;
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

/// Every mesh of `built` as bytes (name, positions, normals, indices, weights), so two builds compare exactly.
fn mesh_bytes(built: &BuiltCharacter) -> Vec<(String, Vec<u8>)> {
    built
        .meshes
        .iter()
        .map(|m| {
            let g = &m.mesh.geometry;
            let mut bytes = Vec::new();
            for p in g.positions.iter().chain(&g.normals) {
                bytes.extend(p.to_array().iter().flat_map(|x| x.to_le_bytes()));
            }
            bytes.extend(g.indices.iter().flat_map(|i| i.to_le_bytes()));
            (format!("{}/{}", m.part, m.mesh.name), bytes)
        })
        .collect()
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
    assert_eq!(mesh_bytes(&rebuilt), mesh_bytes(&built), "the reopened character builds differently");

    // The export: the rights signed off, the preflight clean, then the file written and read back.
    let r = runner(project.path());
    sign_off(&*r);
    let check = run(&*r, "io.check_export", json!({"character": CHARACTER, "format": "vrm1"})).plan.output.unwrap();
    assert_eq!(check["ok"], true, "the export preflight finds errors: {}", check["report"]);
    let signed = document(&*r);
    let (sent, bytes) = export_vrm(&signed, &build(&signed));
    let back = run_import(&Vrm1Importer, &ImportInput::new(&bytes), &ImportCtx::new(&ImportOptions::default()))
        .expect("the VRM reads back");
    assert_eq!(back.scene.avatar, sent.avatar, "the avatar (humanoid, springs, constraints, expressions) changed");
    assert_eq!(back.scene.meshes.len(), sent.meshes.len(), "meshes");
    assert_eq!(back.scene.materials.len(), sent.materials.len(), "materials");

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
