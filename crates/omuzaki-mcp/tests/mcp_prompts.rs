//! Prompts end to end: a recipe is listed only when the session has every op it needs, and `get` fills it in.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::fake_runner;
use omuzaki_mcp::{McpOptions, McpServer, PromptArg, PromptDef};
use rmcp::model::{
    ClientCapabilities, ClientConfig, ContentBlock, ErrorCode, GetPromptRequestParams, Implementation, ProtocolVersion,
};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use serde_json::{Value, json};

struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("test-client", "1.0"))
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
    }
}

const ROUND_FACE: PromptDef = PromptDef {
    name: "round_face",
    title: "Round the face",
    description: "Make a character's face rounder.",
    arguments: &[
        PromptArg { name: "character", description: "The character's id.", required: true },
        PromptArg { name: "note", description: "Anything else to keep in mind.", required: false },
    ],
    needs: &["face.set_blend", "character.describe_face"],
    template: "Round {character}'s face with {tool:face.set_blend}, checking with {tool:character.describe_face}. {note}",
};

/// Needs an op the fake runner doesn't have.
const IMPOSSIBLE: PromptDef = PromptDef {
    name: "impossible",
    title: "Impossible",
    description: "Needs an op that doesn't exist.",
    arguments: &[],
    needs: &["face.set_blend", "face.no_such_op"],
    template: "never shown",
};

async fn connect(options: McpOptions) -> RunningService<RoleClient, Client> {
    let server = McpServer::new(Arc::new(fake_runner()), options).expect("server builds");
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move { server.serve(server_io).await });
    Client.serve_with_lifecycle(client_io, ClientLifecycleMode::Initialize).await.expect("client connects")
}

fn with_recipes() -> McpOptions {
    McpOptions::default().with_prompt(ROUND_FACE).with_prompt(IMPOSSIBLE)
}

async fn names(client: &RunningService<RoleClient, Client>) -> Vec<String> {
    client.list_all_prompts().await.unwrap().into_iter().map(|p| p.name).collect()
}

fn get_args(name: &str, args: &Value) -> GetPromptRequestParams {
    let mut request = GetPromptRequestParams::new(name);
    request.arguments = args.as_object().cloned();
    request
}

async fn get_error(client: &RunningService<RoleClient, Client>, request: GetPromptRequestParams) -> rmcp::ErrorData {
    match client.get_prompt(request).await.unwrap_err() {
        ServiceError::McpError(error) => error,
        other => panic!("expected a protocol error, got {other:?}"),
    }
}

#[tokio::test]
async fn the_server_advertises_prompts() {
    let client = connect(McpOptions::default()).await;
    assert!(client.peer_info().expect("handshake done").capabilities.prompts.is_some());
}

#[tokio::test]
async fn no_recipe_ships_yet() {
    let client = connect(McpOptions::default()).await;
    assert_eq!(names(&client).await, Vec::<String>::new());
}

#[tokio::test]
async fn only_a_recipe_whose_ops_exist_is_listed() {
    let client = connect(with_recipes()).await;
    let prompts = client.list_all_prompts().await.unwrap();
    assert_eq!(prompts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["round_face"]);
    let listed = &prompts[0];
    assert_eq!(listed.title.as_deref(), Some("Round the face"));
    let args = listed.arguments.as_ref().unwrap();
    assert_eq!(
        args.iter().map(|a| (a.name.as_str(), a.required)).collect::<Vec<_>>(),
        [("character", Some(true)), ("note", Some(false))]
    );
}

#[tokio::test]
async fn get_fills_in_the_arguments_and_uses_tool_names() {
    let client = connect(with_recipes()).await;
    let result = client
        .get_prompt(get_args("round_face", &json!({"character": "mira", "note": "Keep the jaw."})))
        .await
        .unwrap();
    assert_eq!(result.description.as_deref(), Some("Make a character's face rounder."));
    let [message] = result.messages.as_slice() else { panic!("one message") };
    let ContentBlock::Text(text) = &message.content else { panic!("text, got {:?}", message.content) };
    assert_eq!(
        text.text,
        "Round mira's face with face_set_blend, checking with character_describe_face. Keep the jaw."
    );
}

#[tokio::test]
async fn a_missing_required_argument_is_invalid_params_naming_it() {
    let client = connect(with_recipes()).await;
    for args in [json!({}), json!({"note": "x"})] {
        let error = get_error(&client, get_args("round_face", &args)).await;
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        assert!(error.message.contains("`character`"), "{}", error.message);
    }
}

#[tokio::test]
async fn an_unknown_argument_is_refused() {
    let client = connect(with_recipes()).await;
    let error = get_error(&client, get_args("round_face", &json!({"character": "a", "chracter": "b"}))).await;
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert!(error.message.contains("chracter"), "{}", error.message);
}

#[tokio::test]
async fn an_unknown_or_unlisted_prompt_is_invalid_params_listing_the_available() {
    let client = connect(with_recipes()).await;
    for name in ["no_such_prompt", "impossible"] {
        let error = get_error(&client, get_args(name, &json!({}))).await;
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(error.data, Some(json!({"available_prompts": ["round_face"]})));
    }
}
