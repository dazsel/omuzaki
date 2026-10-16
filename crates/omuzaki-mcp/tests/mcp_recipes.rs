//! The built-in recipes (spec v3 §11): each is offered only to a session whose tools can do what it says, and reads with
//! the tool names the agent calls. The ops are stand-ins registered here under the real ops' names, because this crate's
//! tests may not link the crates that define them.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use common::{FakeRunner, fake_runner};
use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_mcp::{McpOptions, McpServer};
use omuzaki_ops_api::{
    Actor, Event, Grants, Op, OpCtx, OpError, OpInfo, OpKind, OpMeta, OpRecord, Outcome, Plan, Request, Runner, Scope,
    TxId, Undo,
};
use rmcp::model::{ClientCapabilities, ClientConfig, GetPromptRequestParams, Implementation};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

macro_rules! stand_ins {
    ($($ty:ident => $name:literal),* $(,)?) => {$(
        #[derive(Serialize, Deserialize, JsonSchema)]
        struct $ty;

        impl Op for $ty {
            fn meta() -> OpMeta {
                OpMeta {
                    name: $name,
                    summary: "A stand-in for the real op of this name.",
                    kind: OpKind::Edit,
                    scope: Scope::EditCharacter,
                    undo: Undo::Auto,
                }
            }

            fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
                Ok(Plan::edit(Patch::new()))
            }
        }

        omuzaki_ops_api::register_op!($ty);
    )*};
}

stand_ins! {
    CharacterCreate => "character.create",
    BodySetTemplate => "body.set_template",
    PaletteSet => "palette.set",
    PartAdd => "part.add",
    FaceSetProfile => "face.set_profile",
    LookListPresets => "look.list_presets",
    LookSetPreset => "look.set_preset",
    LookSetParam => "look.set_param",
    ExpressionSet => "face.expression.set",
    ExpressionDuplicate => "face.expression.duplicate",
    ExpressionRename => "face.expression.rename",
    ExpressionValues => "face.expression.values",
    VariantCreate => "variant.create",
    VariantSetOverride => "variant.set_override",
    CheckExport => "io.check_export",
}

/// The fake runner with some ops hidden, as a build without those features would be.
struct Without {
    inner: FakeRunner,
    hidden: &'static [&'static str],
}

impl Runner for Without {
    fn ops(&self) -> Vec<OpInfo> {
        self.inner.ops().into_iter().filter(|op| !self.hidden.contains(&op.name)).collect()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        self.inner.run(req)
    }
    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.inner.begin(label, actor)
    }
    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.commit(tx, actor)
    }
    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.rollback(tx, actor)
    }
    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.undo(actor, grants)
    }
    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.redo(actor, grants)
    }
    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        self.inner.subscribe(grants)
    }
    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.inner.get(path, grants)
    }
    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.inner.log(after_seq, limit, grants)
    }
}

struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("test-client", "1.0"))
    }
}

async fn connect(hidden: &'static [&'static str]) -> RunningService<RoleClient, Client> {
    let runner = Arc::new(Without { inner: fake_runner(), hidden });
    let server = McpServer::new(runner, McpOptions::default()).expect("server builds");
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move { server.serve(server_io).await });
    Client.serve_with_lifecycle(client_io, ClientLifecycleMode::Initialize).await.expect("client connects")
}

async fn names(client: &RunningService<RoleClient, Client>) -> Vec<String> {
    client.list_all_prompts().await.unwrap().into_iter().map(|p| p.name).collect()
}

#[tokio::test]
async fn every_recipe_is_offered_when_its_ops_exist() {
    let client = connect(&[]).await;
    assert_eq!(
        names(&client).await,
        ["create_character_from_sheet", "make_emote_pack", "make_vrchat_quest_variant", "match_reference_look"]
    );
}

#[tokio::test]
async fn a_recipe_whose_op_is_missing_is_not_offered_and_not_gettable() {
    let client = connect(&["io.check_export"]).await;
    let offered = names(&client).await;
    assert!(!offered.iter().any(|n| n == "make_vrchat_quest_variant"), "{offered:?}");
    let error = client
        .get_prompt(
            GetPromptRequestParams::new("make_vrchat_quest_variant")
                .with_arguments(json!({"character": "mira"}).as_object().unwrap().clone()),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
}

#[tokio::test]
async fn a_recipe_reads_with_the_tool_names_and_its_arguments() {
    let client = connect(&[]).await;
    let args = json!({"sheet": "sheets/mira.png", "name": "mira"}).as_object().unwrap().clone();
    let prompt = client
        .get_prompt(GetPromptRequestParams::new("create_character_from_sheet").with_arguments(args))
        .await
        .unwrap();
    let text = prompt.messages[0].content.as_text().expect("text").text.clone();
    for expected in ["sheets/mira.png", "character_create", "body_set_template", "palette_set", "part_add", "tx_begin"]
    {
        assert!(text.contains(expected), "{expected} in {text}");
    }
    assert!(!text.contains('{'), "every placeholder was filled: {text}");
}

#[tokio::test]
async fn a_recipe_refuses_a_missing_required_argument() {
    let client = connect(&[]).await;
    let error = client.get_prompt(GetPromptRequestParams::new("match_reference_look")).await.unwrap_err();
    assert!(error.to_string().contains("needs the argument"), "{error}");
}
