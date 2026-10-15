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
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation, ProtocolVersion};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientServiceExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf};
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
        // Both lifecycles name the client at startup (the `initialize` handshake, or the identity `discover` carries),
        // so an idle client is connected before it asks anything.
        assert_eq!(
            mcp.connection(),
            json!({"connected": true, "client_name": "claude-code", "sessions": 1}),
            "{lifecycle:?} straight after connecting"
        );
        // rmcp ends a `discover` session that closes before its first request as a failed handshake, so ask once.
        connection.client.list_all_tools().await.expect("lists tools");
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

/// A client that skips both `initialize` and `discover` and writes its first request straight onto the wire, with its
/// identity in the request's own metadata (protocol 2026-07-28).
struct RawClient {
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    write: WriteHalf<DuplexStream>,
    server: JoinHandle<Result<(), ServerError>>,
}

impl RawClient {
    /// Connects to `mcp` and sends `method` with `params` as the session's first message.
    async fn start(mcp: &McpServer, method: &str, mut params: Value) -> Self {
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let serving = mcp.clone();
        let server = tokio::spawn(async move { serving.serve(server_io).await });
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": ProtocolVersion::V_2026_07_28,
            "io.modelcontextprotocol/clientInfo": Implementation::new("claude-code", "1.0"),
            "io.modelcontextprotocol/clientCapabilities": ClientCapabilities::default(),
        });
        let (read, write) = tokio::io::split(client_io);
        let mut client = Self { lines: BufReader::new(read).lines(), write, server };
        client.send(json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})).await;
        client
    }

    /// Writes one message.
    async fn send(&mut self, message: Value) {
        self.write.write_all(format!("{message}\n").as_bytes()).await.expect("sends");
    }

    /// The next message the server sends.
    async fn next(&mut self) -> Value {
        serde_json::from_str(&self.lines.next_line().await.expect("reads").expect("a message")).expect("json")
    }

    /// Hangs up and waits for the server to finish the session.
    async fn hang_up(self) {
        drop((self.lines, self.write));
        self.server.await.expect("server task").expect("server ends cleanly");
    }
}

/// A client may start with any request that carries its identity in its own metadata. That request names the client,
/// and the session stops counting once it hangs up.
#[tokio::test]
async fn a_client_that_starts_with_a_request_is_named_by_its_metadata() {
    let mcp = server();
    let mut client = RawClient::start(&mcp, "tools/list", json!({})).await;
    let reply = client.next().await;
    assert!(reply["result"]["tools"].is_array(), "the request is answered: {reply}");
    assert_eq!(mcp.connection(), json!({"connected": true, "client_name": "claude-code", "sessions": 1}));
    client.hang_up().await;
    assert_eq!(mcp.connection(), json!({"connected": false, "sessions": 0}), "after the client hung up");
}

/// `subscriptions/listen` may be a session's first request too. The subscription stays open, and its client counts as
/// connected for as long as it does.
#[tokio::test]
async fn a_client_that_starts_by_listening_is_named_while_it_listens() {
    let mcp = server();
    let params = json!({"notifications": {"resourceSubscriptions": ["omuzaki://log"]}});
    let mut client = RawClient::start(&mcp, "subscriptions/listen", params).await;
    let ack = client.next().await;
    assert_eq!(ack["method"], "notifications/subscriptions/acknowledged", "the subscription opens: {ack}");
    // rmcp sends the acknowledgment before it calls the handler, so give the handler its turn (single-threaded runtime,
    // bounded) rather than racing it.
    for _ in 0..100 {
        if mcp.connection()["connected"] == json!(true) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(mcp.connection(), json!({"connected": true, "client_name": "claude-code", "sessions": 1}));
    // Over stdio a client ends a subscription by cancelling it; rmcp would otherwise give the open handler a few
    // seconds' grace after the hang-up before it ends the session.
    client.send(json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}})).await;
    client.hang_up().await;
    assert_eq!(mcp.connection(), json!({"connected": false, "sessions": 0}), "after the client hung up");
}
