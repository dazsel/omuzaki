//! The stdio bridge (design `agent-sessions.md` 10.2): a local `omuzaki mcp` asks the studio's port for a bridge
//! credential with the pairing key, relays its client's stdio into the studio, and the first session with a new
//! credential waits until the person pairs it; after that the credential needs no click.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::http::install_crypto_provider;
use common::registry::Studio;
use omuzaki_mcp::{HttpOptions, HttpServer, McpOptions, McpServer, TokenRecord, bridge};
use omuzaki_ops_api::{Decision, Grants, Need, PauseReason, SessionState, Sessions, Transport};
use reqwest::StatusCode;
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use tokio::io::BufReader;

struct Rig {
    studio: Arc<Studio>,
    server: HttpServer,
    key: String,
    paired: Arc<Mutex<Vec<TokenRecord>>>,
}

async fn rig() -> Rig {
    install_crypto_provider();
    let studio = Studio::new();
    let mcp = McpServer::new(studio.clone(), McpOptions::default().with_sessions(studio.clone(), Studio::person()))
        .expect("server builds");
    let paired = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&paired);
    let options = HttpOptions::default()
        .with_bind("127.0.0.1:0".parse().unwrap())
        .on_paired(move |record| seen.lock().unwrap().push(record));
    let server = mcp.serve_http(options).await.expect("port opens");
    let key = server.enable_pairing(Grants::agent_default(), Studio::person()).unwrap();
    Rig { studio, server, key, paired }
}

struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("claude-code", "1"))
    }
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never happened: {what}");
}

/// Connects an MCP client to the studio through a relay, as `omuzaki mcp` does over stdio.
async fn connect(
    rig: &Rig,
    token: &str,
) -> (rmcp::service::RunningService<rmcp::service::RoleClient, Client>, tokio::task::JoinHandle<()>) {
    let (client_end, bridge_end) = tokio::io::duplex(1 << 16);
    let (read, write) = tokio::io::split(bridge_end);
    let (url, token) = (rig.server.url(), token.to_owned());
    let relay = tokio::spawn(async move {
        bridge::relay(&url, &token, None, BufReader::new(read), write).await.expect("the relay ends cleanly");
    });
    let client = Client.serve_with_lifecycle(client_end, ClientLifecycleMode::Initialize).await.expect("connects");
    (client, relay)
}

#[tokio::test]
async fn only_the_pairing_key_gets_a_bridge_credential_and_web_pages_never_do() {
    let rig = rig().await;
    let http = reqwest::Client::new();
    let pair = |key: &str| {
        http.post(rig.server.pair_url()).bearer_auth(key.to_owned()).body(r#"{"client":"claude-code"}"#)
    };
    assert_eq!(pair("omuzaki_mcp_wrong").send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(http.post(rig.server.pair_url()).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    let from_page = pair(&rig.key).header("Origin", "https://example.com").send().await.unwrap();
    assert_eq!(from_page.status(), StatusCode::FORBIDDEN);
    let get = http.get(rig.server.pair_url()).bearer_auth(&rig.key).send().await.unwrap();
    assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
    let nameless = http.post(rig.server.pair_url()).bearer_auth(&rig.key).body("{}").send().await.unwrap();
    assert_eq!(nameless.status(), StatusCode::BAD_REQUEST);

    let token = bridge::request_credential(&rig.server.pair_url(), &rig.key, "claude-code").await.unwrap();
    assert!(bridge::credential_accepted(&rig.server.url(), &token).await.unwrap());
    assert!(!bridge::credential_accepted(&rig.server.url(), "omuzaki_mcp_forged").await.unwrap());
    let rows = rig.server.tokens().await;
    assert!(matches!(&rows[..], [row] if row.label == "claude-code" && !row.paired && row.issuer == Studio::person()));

    // Something asking in a loop is stopped once eight wait to be paired.
    for _ in 1..8 {
        bridge::request_credential(&rig.server.pair_url(), &rig.key, "loop").await.unwrap();
    }
    let refused = bridge::request_credential(&rig.server.pair_url(), &rig.key, "loop").await.unwrap_err();
    assert!(matches!(refused, bridge::BridgeError::Refused { status: 429, .. }), "{refused}");
}

#[tokio::test]
async fn a_new_credential_waits_for_the_person_to_pair_it_once() {
    let rig = rig().await;
    let token = bridge::request_credential(&rig.server.pair_url(), &rig.key, "claude-code").await.unwrap();

    // The first session is the stdio client's, for the studio's person, and waits on a pairing card.
    let (client, relay) = connect(&rig, &token).await;
    until("the session waits to be paired", || {
        rig.studio.sessions().iter().any(|s| matches!(s.state, SessionState::Paused { reason: PauseReason::Pairing, .. }))
    })
    .await;
    let spec = rig.studio.specs().remove(0);
    assert_eq!((spec.transport, spec.client.as_str()), (Transport::Stdio, "claude-code"));
    assert_eq!(spec.on_behalf_of, Studio::person());
    let card = rig.studio.approvals().remove(0);
    assert_eq!(card.need, Need::Pairing { client: "claude-code".into() });
    assert!(rig.paired.lock().unwrap().is_empty(), "nothing is paired before the person says so");

    // The person pairs it: the session goes on, and the host is told so it can remember the credential.
    rig.studio.resolve(card.id, Decision::Approve, &Studio::person()).unwrap();
    until("the host hears of the pairing", || !rig.paired.lock().unwrap().is_empty()).await;
    let record = rig.paired.lock().unwrap()[0].clone();
    assert!(record.paired && record.bridge && record.label == "claude-code", "{record:?}");
    assert!(rig.server.tokens().await[0].paired);
    client.cancel().await.unwrap();
    relay.await.unwrap();

    // The next connection with it needs no click.
    let (client, relay) = connect(&rig, &token).await;
    until("a second session opens", || rig.studio.specs().len() == 2).await;
    let second = rig.studio.sessions().into_iter().max_by_key(|s| s.id).unwrap();
    assert_eq!(second.state, SessionState::Active);
    assert!(rig.studio.approvals().is_empty());
    client.cancel().await.unwrap();
    relay.await.unwrap();
}

#[tokio::test]
async fn a_bridge_credential_record_is_version_two_and_others_stay_version_one() {
    let rig = rig().await;
    bridge::request_credential(&rig.server.pair_url(), &rig.key, "claude-code").await.unwrap();
    let issued = rig.server.issue("Codex", Grants::agent_default()).unwrap();
    assert_eq!((issued.record().version, issued.record().bridge), (1, false));
    let rows = rig.server.tokens().await;
    let bridge_record = rig.server.record(rows[0].id).unwrap();
    assert_eq!((bridge_record.version, bridge_record.bridge), (2, true));
}
