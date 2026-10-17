//! The in-process link for the built-in assistant (`McpServer::connect_in_app`): a session of its own, served over an
//! in-memory stream, with the same tools, results and rules as any session, and no port.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::registry::Studio;
use common::{FakeRunnerExt, data, text};
use omuzaki_mcp::{InAppSpec, McpOptions, McpServer, ServerError};
use omuzaki_ops_api::{Actor, EndReason, Grants, Scope, SessionId, SessionState, Sessions, Transport};
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use serde_json::json;

struct Host;

impl ClientHandler for Host {
    fn get_info(&self) -> ClientConfig {
        // The host's own name: it never decides identity.
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("pretending-to-be-someone-else", "1"))
    }
}

fn server(studio: &Arc<Studio>) -> McpServer {
    McpServer::new(studio.clone(), McpOptions::default().with_sessions(studio.clone(), Studio::person())).unwrap()
}

async fn client_of(link: omuzaki_mcp::InAppLink) -> (RunningService<RoleClient, Host>, omuzaki_mcp::InAppEnd) {
    let end = link.end();
    let client = Host.serve_with_lifecycle(link.client_io, ClientLifecycleMode::Initialize).await.expect("connects");
    (client, end)
}

async fn call(
    client: &RunningService<RoleClient, Host>,
    tool: &str,
    args: serde_json::Value,
) -> rmcp::model::CallToolResult {
    let args = args.as_object().unwrap().clone();
    client.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)).await.expect("call")
}

#[tokio::test]
async fn the_assistant_gets_its_own_session_tools_for_its_grants_and_edits_in_its_name() {
    let studio = Studio::new();
    let mcp = server(&studio);
    let link =
        mcp.connect_in_app(InAppSpec::assistant(Studio::person(), Some("claude-opus-5-5".into()))).await.unwrap();
    let id = link.session();
    let (client, _end) = client_of(link).await;

    // The registry has it as an in-app session for the person, with the assistant's model; the name in the handshake
    // changed nothing.
    let info = Sessions::session(studio.as_ref(), id).expect("registered");
    assert_eq!(info.transport, Transport::InApp);
    assert_eq!(info.on_behalf_of, Studio::person());
    assert_eq!(info.actor, Actor::Agent { id: "assistant#1".to_owned(), model: Some("claude-opus-5-5".to_owned()) });
    assert_eq!(studio.specs()[0].client, "assistant");

    // `tools/list` follows the session's grants: no publish, so the export is described as not granted.
    let tools = client.list_all_tools().await.unwrap();
    let export = tools.iter().find(|t| t.name == "export_vrm").expect("listed");
    assert!(export.description.as_deref().unwrap().contains("NOT granted"), "{:?}", export.description);

    // An applied edit is stamped with the session's actor.
    let result = call(&client, "face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    assert_eq!(result.is_error, Some(false), "{}", text(&result));
    assert_eq!(studio.runner.records()[0].actor, info.actor);
    let me = call(&client, "session_info", json!({})).await;
    assert_eq!(data(&me)["session"]["transport"], "in_app");
    assert!(data(&me)["session"]["on_behalf_of"].as_str().unwrap().contains("alice"));
}

#[tokio::test]
async fn a_spec_above_the_persons_grants_is_not_effective_and_a_child_above_its_parent_is_refused() {
    let studio = Studio::new();
    // The person holds everything but `publish`.
    studio.person_holds(Grants::person().without(Scope::Publish));
    let mcp = server(&studio);

    let mut greedy = InAppSpec::assistant(Studio::person(), None);
    greedy.grants = greedy.grants.with(Scope::Publish);
    let link = mcp.connect_in_app(greedy).await.unwrap();
    let granted = Sessions::session(studio.as_ref(), link.session()).unwrap().grants;
    assert!(!granted.allows(Scope::Publish), "the registry's effective grants cap the spec");
    let (client, _end) = client_of(link).await;
    let export = client.list_all_tools().await.unwrap().into_iter().find(|t| t.name == "export_vrm").unwrap();
    assert!(export.description.as_deref().unwrap().contains("NOT granted"));

    // A sub-agent whose ceiling exceeds its parent's is refused before any stream exists.
    let parent = mcp.connect_in_app(InAppSpec::assistant(Studio::person(), None)).await.unwrap();
    let mut child = InAppSpec::assistant(Studio::person(), None);
    child.parent = Some(parent.session());
    child.grants = child.grants.with(Scope::Render).with(Scope::Net);
    let refused = mcp.connect_in_app(child).await;
    assert!(matches!(refused, Err(ServerError::SessionRefused(_))), "{refused:?}");
    // And an unknown parent.
    let mut orphan = InAppSpec::assistant(Studio::person(), None);
    orphan.parent = Some(SessionId(404));
    assert!(matches!(mcp.connect_in_app(orphan).await, Err(ServerError::SessionRefused(_))));
}

#[tokio::test]
async fn closing_the_client_mid_transaction_ends_the_session_closed_and_abandons_the_transaction() {
    let studio = Studio::new();
    let mcp = server(&studio);
    let link = mcp.connect_in_app(InAppSpec::assistant(Studio::person(), None)).await.unwrap();
    let (mut client, end) = client_of(link).await;
    call(&client, "tx_begin", json!({"label": "reshape"})).await;
    call(&client, "face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    assert!(studio.runner.has_open_tx());

    client.close().await.unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(5), end.closed()).await.expect("closed resolves");
    assert_eq!(reason, EndReason::Closed);
    let (_, ended_with, ended) = studio.ends().remove(0);
    assert_eq!(ended_with, EndReason::Closed);
    assert_eq!(ended.abandoned.len(), 1, "the open transaction was abandoned through the registry");
    assert_eq!(studio.runner.blend(), None);
}

#[tokio::test]
async fn revoking_from_the_registry_resolves_closed_and_the_stream_reaches_eof() {
    let studio = Studio::new();
    let mcp = server(&studio);
    let link = mcp.connect_in_app(InAppSpec::assistant(Studio::person(), None)).await.unwrap();
    let id = link.session();
    let (client, end) = client_of(link).await;
    let by = Studio::person();
    studio.registry_ends(id, &EndReason::Revoked { by: by.clone(), credential: false });
    let reason = tokio::time::timeout(Duration::from_secs(5), end.closed()).await.expect("closed resolves");
    assert_eq!(reason, EndReason::Revoked { by, credential: false });
    // The stream ends: the client's connection closes.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !client.is_closed() && !client.is_transport_closed() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the stream reaches EOF");
}

#[tokio::test]
async fn an_end_before_the_client_initializes_closes_the_stream() {
    use tokio::io::AsyncReadExt;
    let studio = Studio::new();
    let mcp = server(&studio);
    let mut link = mcp.connect_in_app(InAppSpec::assistant(Studio::person(), None)).await.unwrap();
    // The client connects but never sends `initialize`; the project closes.
    studio.registry_ends(link.session(), &EndReason::Closed);
    let mut rest = Vec::new();
    let eof = tokio::time::timeout(Duration::from_secs(5), link.client_io.read_to_end(&mut rest)).await;
    assert!(eof.expect("the stream reaches EOF without an initialize").is_ok());
}

#[tokio::test]
async fn a_paused_in_app_session_is_held_then_refused_as_waiting() {
    let studio = Studio::new();
    let mcp = McpServer::new(
        studio.clone(),
        McpOptions::default()
            .with_sessions(studio.clone(), Studio::person())
            .with_pause_hold(Duration::from_millis(100)),
    )
    .unwrap();
    let link = mcp.connect_in_app(InAppSpec::assistant(Studio::person(), None)).await.unwrap();
    let id = link.session();
    let (client, _end) = client_of(link).await;
    studio.pause(id, &Studio::person()).unwrap();
    let result = call(&client, "face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    assert_eq!(data(&result)["code"], "waiting");
    assert!(text(&result).contains("nothing ran"), "{}", text(&result));
    assert!(matches!(Sessions::session(studio.as_ref(), id).unwrap().state, SessionState::Paused { .. }));
}

#[tokio::test]
async fn a_server_without_a_registry_cannot_open_one() {
    let plain = McpServer::new(Arc::new(common::fake_runner()), McpOptions::default()).unwrap();
    let refused = plain.connect_in_app(InAppSpec::assistant(Studio::person(), None)).await;
    assert!(matches!(refused, Err(ServerError::NoRegistry)));
}

/// Listening sockets this process owns, found through `/proc`: the link must not have added one.
#[cfg(target_os = "linux")]
#[allow(clippy::disallowed_methods, reason = "a test reads /proc; the fence is for ops")]
fn listening_sockets() -> usize {
    use std::collections::BTreeSet;
    let mut ours = BTreeSet::new();
    for entry in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
        if let Ok(target) = std::fs::read_link(entry.path())
            && let Some(inode) =
                target.to_string_lossy().strip_prefix("socket:[").and_then(|s| s.strip_suffix(']').map(str::to_owned))
        {
            ours.insert(inode);
        }
    }
    ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|table| {
            table
                .lines()
                .skip(1)
                .filter_map(|line| {
                    let fields: Vec<&str> = line.split_whitespace().collect();
                    // State 0A is LISTEN; the inode is the tenth field.
                    (fields.get(3) == Some(&"0A")).then(|| fields.get(9).map(|s| (*s).to_owned())).flatten()
                })
                .collect::<Vec<_>>()
        })
        .filter(|inode| ours.contains(inode))
        .count()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn no_port_is_bound() {
    let studio = Studio::new();
    let mcp = server(&studio);
    let before = listening_sockets();
    let link = mcp.connect_in_app(InAppSpec::assistant(Studio::person(), None)).await.unwrap();
    let (client, _end) = client_of(link).await;
    call(&client, "face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    assert_eq!(listening_sockets(), before, "an in-app link listens on nothing");
}
