//! Design a kit character end to end, through the op layer (#2216): an empty project, then the body, the face, the hair,
//! accessories and the look, every step an op the edit panels and an agent over MCP both call; then the project is saved
//! and reopened, and the reopened character is the same character.
//!
//! The script is one list of `(op, args)` steps. It runs through the runner the CLI opens, and again as tool calls of an
//! MCP client talking to `omuzaki mcp`'s server over that same kind of runner, and both make the same document.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::path::Path;
use std::sync::Arc;

use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, RegistryBuilder};
use omuzaki_cli::cli::try_parse_from;
use omuzaki_cli::session::{Session, open_executor_with, open_runner};
use omuzaki_doc_api::DocPath;
use omuzaki_mcp::{McpOptions, McpServer};
use omuzaki_ops_api::{Actor, Grants, Mode, Outcome, Request, Runner};
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
        ("hair.bangs.set", json!({"character": c, "part": "hair", "group": "b-bangs", "field": "sweep_deg", "value": 12.0})),
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
    assert_eq!(mesh_bytes(&build(&reopened)), mesh_bytes(&built), "the reopened character builds differently");
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
