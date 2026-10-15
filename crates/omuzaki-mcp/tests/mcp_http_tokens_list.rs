//! The connected-apps listing: `HttpServer::tokens` over a real loopback port, with rmcp's own client connecting as each
//! token. Nothing here waits for a session to close; the clock is only read, never advanced.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use common::fake_runner;
use omuzaki_mcp::{AccessToken, HttpOptions, HttpServer, McpOptions, McpServer, TokenInfo};
use omuzaki_ops_api::Grants;
use reqwest::StatusCode;
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{ClientHandler, ServiceExt};
use serde_json::json;

/// A server with the fake runner on a free loopback port. Copied from the `Port` in `tests/common/http.rs`, which this
/// file leaves alone; the copies are folded together later.
struct Port {
    /// The MCP server the port serves; kept alive for as long as the port is.
    _mcp: McpServer,
    server: HttpServer,
}

impl Port {
    async fn start() -> Self {
        let mcp = McpServer::new(Arc::new(fake_runner()), McpOptions::default()).expect("server builds");
        let options = HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap());
        let server = mcp.serve_http(options).await.expect("port opens");
        Self { _mcp: mcp, server }
    }
}

/// A client that introduces itself under a fixed name.
struct Named(&'static str);

impl ClientHandler for Named {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new(self.0, "1.0"))
    }
}

type Agent = RunningService<RoleClient, Named>;

/// An rmcp client that presents `token`, started with the `initialize` handshake. Its session stays open until dropped.
async fn connect(port: &Port, token: &AccessToken) -> Agent {
    let config = StreamableHttpClientTransportConfig::with_uri(port.server.url()).auth_header(token.secret());
    Named("tokens-list").serve(StreamableHttpClientTransport::from_config(config)).await.expect("client connects")
}

/// The status of a raw `initialize` presenting `authorization` verbatim.
async fn status(port: &Port, authorization: &str) -> StatusCode {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "raw", "version": "1"}}
    });
    reqwest::Client::new()
        .post(port.server.url())
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("authorization", authorization)
        .body(body.to_string())
        .send()
        .await
        .expect("the port answers")
        .status()
}

/// Whole milliseconds since the Unix epoch, which is the precision `last_used` is kept at.
fn millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH).expect("after 1970").as_millis()
}

#[tokio::test]
async fn the_live_tokens_list_in_id_order_and_an_unused_token_shows_no_use() {
    let port = Port::start().await;
    let codex = port.server.issue("Codex", Grants::agent_default()).unwrap();
    let bot = port.server.issue("hair-bot", Grants::default()).unwrap();

    let rows = port.server.tokens().await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!((rows[0].id, rows[0].label.as_str()), (codex.id(), "Codex"));
    assert_eq!((rows[1].id, rows[1].label.as_str()), (bot.id(), "hair-bot"));
    assert_eq!(rows[0].grants, *codex.grants());
    assert_eq!(rows[1].grants, *bot.grants());
    assert_ne!(rows[0].grants, rows[1].grants, "the two tokens must differ, or the grants check proves nothing");
    for row in &rows {
        assert_eq!((row.last_used, row.open_sessions, row.paired), (None, 0, false), "{row:?}");
    }
}

#[tokio::test]
async fn a_connect_records_its_token_use_and_counts_its_session() {
    let port = Port::start().await;
    let codex = port.server.issue("Codex", Grants::agent_default()).unwrap();
    let _bot = port.server.issue("hair-bot", Grants::default()).unwrap();
    let bot_before = port.server.tokens().await[1].clone();

    let before = SystemTime::now();
    let _first = connect(&port, &codex).await;
    let after = SystemTime::now();

    let rows = port.server.tokens().await;
    let used = rows[0].last_used.expect("the connect recorded a use");
    assert!(
        millis(before) <= millis(used) && millis(used) <= millis(after),
        "{used:?} is not between {before:?} and {after:?}"
    );
    assert_eq!(rows[0].open_sessions, 1);
    assert_eq!(rows[1], bot_before, "the other token's row is unchanged");

    let _second = connect(&port, &codex).await;
    assert_eq!(port.server.tokens().await[0].open_sessions, 2);
}

#[tokio::test]
async fn a_wrong_secret_changes_no_row() {
    let port = Port::start().await;
    let codex = port.server.issue("Codex", Grants::agent_default()).unwrap();
    let _bot = port.server.issue("hair-bot", Grants::default()).unwrap();
    // A raw, awaited request rather than an rmcp client: a client sends follow-up requests in the background, which
    // would move `last_used` between the snapshot and the wrong request and make this test race.
    assert_eq!(status(&port, &codex.bearer_header()).await, StatusCode::OK);
    let before: Vec<TokenInfo> = port.server.tokens().await;
    assert!(before[0].last_used.is_some(), "the valid request must have recorded a use");

    let wrong = "Bearer omuzaki_mcp_not_a_real_secret";
    assert_eq!(status(&port, wrong).await, StatusCode::UNAUTHORIZED);
    assert_eq!(port.server.tokens().await, before);
}

#[tokio::test]
async fn revoking_removes_the_row_and_no_row_shows_a_secret() {
    let port = Port::start().await;
    let codex = port.server.issue("Codex", Grants::agent_default()).unwrap();
    let bot = port.server.issue("hair-bot", Grants::default()).unwrap();
    let _connected = connect(&port, &codex).await;

    for row in port.server.tokens().await {
        let printed = format!("{row:?}");
        assert!(printed.contains(&row.label), "{printed}");
        assert!(
            !printed.contains(codex.secret()) && !printed.contains(bot.secret()),
            "a row must never show a secret: {printed}"
        );
    }

    assert!(port.server.revoke(codex.id()).await);
    let rows = port.server.tokens().await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].id, bot.id());
    assert_eq!(status(&port, &codex.bearer_header()).await, StatusCode::UNAUTHORIZED);
}
