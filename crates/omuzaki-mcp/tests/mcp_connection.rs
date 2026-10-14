//! `agent.connection` end to end: a real rmcp client connects over in-memory pipes and the probe reports it, then reports
//! the session gone once the client has disconnected. Run under both lifecycles: the `initialize` handshake, and
//! `discover` (protocol 2026-07-28), where there is no handshake and each request carries the client's identity.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::FakeRunner;
use omuzaki_mcp::{McpOptions, McpServer, ServerError};
use rmcp::ClientLifecycleMode;
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation, ProtocolVersion, SubscriptionFilter};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientServiceExt};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

/// The two ways an MCP client can start a session.
#[derive(Copy, Clone, Debug)]
enum Lifecycle {
    /// `initialize` handshake (protocol 2025-11-25 and earlier): Claude Code and Codex today.
    Initialize,
    /// `server/discover` plus per-request metadata (protocol 2026-07-28): no handshake.
    Discover,
}

impl Lifecycle {
    const ALL: [Self; 2] = [Self::Initialize, Self::Discover];

    fn protocol(self) -> ProtocolVersion {
        match self {
            Self::Initialize => ProtocolVersion::LATEST_WITH_INITIALIZE,
            Self::Discover => ProtocolVersion::V_2026_07_28,
        }
    }

    fn mode(self) -> ClientLifecycleMode {
        match self {
            Self::Initialize => ClientLifecycleMode::Initialize,
            Self::Discover => ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] },
        }
    }
}

/// A client with a fixed name, so the probe has something to report.
struct NamedClient(Lifecycle);

impl ClientHandler for NamedClient {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("claude-code", "1.0"))
            .with_protocol_version(self.0.protocol())
    }
}

/// The server side of one connection, and the client that connected to it.
struct Connection {
    client: RunningService<RoleClient, NamedClient>,
    server: JoinHandle<Result<(), ServerError>>,
}

async fn connect(mcp: &McpServer, lifecycle: Lifecycle) -> Connection {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let serving = mcp.clone();
    let server = tokio::spawn(async move { serving.serve(server_io).await });
    let client =
        NamedClient(lifecycle).serve_with_lifecycle(client_io, lifecycle.mode()).await.expect("client connects");
    Connection { client, server }
}

async fn disconnect(mut connection: Connection) {
    connection.client.close().await.expect("closes");
    connection.server.await.expect("server task").expect("server ends cleanly");
}

fn server() -> McpServer {
    McpServer::new(Arc::new(FakeRunner::new()), McpOptions::default()).expect("server builds")
}

#[tokio::test]
async fn with_no_client_nothing_is_connected() {
    let mcp = server();
    assert_eq!(mcp.connection(), json!({"connected": false, "sessions": 0}));
}

#[tokio::test]
async fn a_connected_client_is_named_and_counted() {
    for lifecycle in Lifecycle::ALL {
        let mcp = server();
        let connection = connect(&mcp, lifecycle).await;
        // Under `discover` nothing has been asked yet, so the session is counted but not named: the first request names
        // it. Under `initialize` the handshake has already named it.
        if let Lifecycle::Discover = lifecycle {
            assert_eq!(mcp.connection(), json!({"connected": false, "sessions": 1}), "{lifecycle:?} before a request");
            connection.client.list_all_tools().await.expect("lists tools");
        }
        assert_eq!(
            mcp.connection(),
            json!({"connected": true, "client_name": "claude-code", "sessions": 1}),
            "{lifecycle:?} after the first request"
        );
        disconnect(connection).await;
    }
}

#[tokio::test]
async fn a_closed_session_stops_counting() {
    for lifecycle in Lifecycle::ALL {
        let mcp = server();
        let connection = connect(&mcp, lifecycle).await;
        connection.client.list_all_tools().await.expect("lists tools");
        disconnect(connection).await;
        let answer: Value = mcp.connection();
        assert_eq!(answer, json!({"connected": false, "sessions": 0}), "{lifecycle:?} after disconnect");
    }
}

#[tokio::test]
async fn the_answer_never_carries_a_credential() {
    for lifecycle in Lifecycle::ALL {
        let mcp = server();
        let connection = connect(&mcp, lifecycle).await;
        connection.client.list_all_tools().await.expect("lists tools");
        let answer = mcp.connection();
        let keys: Vec<&str> = answer.as_object().expect("an object").keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 3, "exactly connected, client_name and sessions: {answer}");
        for key in keys {
            assert!(["connected", "client_name", "sessions"].contains(&key), "unexpected key {key}");
        }
        disconnect(connection).await;
    }
}

/// Under `discover`, the first request of any identity-bearing kind names the session: a client may start with a
/// resource or a prompt request and never list tools.
#[tokio::test]
async fn a_discover_client_is_named_by_its_first_resource_or_prompt_request() {
    for first in ["resources", "prompts"] {
        let mcp = server();
        let connection = connect(&mcp, Lifecycle::Discover).await;
        assert_eq!(mcp.connection(), json!({"connected": false, "sessions": 1}), "{first} before a request");
        match first {
            "resources" => {
                connection.client.list_all_resources().await.expect("lists resources");
            }
            _ => {
                connection.client.list_all_prompts().await.expect("lists prompts");
            }
        }
        assert_eq!(
            mcp.connection(),
            json!({"connected": true, "client_name": "claude-code", "sessions": 1}),
            "named after a {first} request"
        );
        disconnect(connection).await;
        assert_eq!(mcp.connection(), json!({"connected": false, "sessions": 0}), "{first} after disconnect");
    }
}

/// Under `discover`, a client whose first request is a subscription (`subscriptions/listen`) is named by it.
#[tokio::test]
async fn a_discover_client_is_named_by_its_first_subscription() {
    let mcp = server();
    let connection = connect(&mcp, Lifecycle::Discover).await;
    assert_eq!(mcp.connection(), json!({"connected": false, "sessions": 1}), "before a subscription");
    let filter = SubscriptionFilter::builder().resource_subscription("omuzaki://log").build();
    let mut subscription = connection.client.listen(filter).await.expect("subscribes");
    assert_eq!(
        mcp.connection(),
        json!({"connected": true, "client_name": "claude-code", "sessions": 1}),
        "named by the subscription"
    );
    subscription.cancel().await.expect("cancels");
    disconnect(connection).await;
    assert_eq!(mcp.connection(), json!({"connected": false, "sessions": 0}), "after disconnect");
}
