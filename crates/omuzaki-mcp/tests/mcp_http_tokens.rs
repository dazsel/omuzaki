//! Access tokens across a restart: what the host stores, and that a revoked token stays revoked. Each test starts a
//! server, persists the token records as JSON (the host's job), drops the server, and starts a new one.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::fake_runner;
use omuzaki_mcp::{HttpOptions, HttpServer, McpOptions, McpServer, ServerError, TokenRecord};
use omuzaki_ops_api::{Actor, Grants};
use reqwest::StatusCode;
use serde_json::json;

async fn start() -> HttpServer {
    let mcp = McpServer::new(Arc::new(fake_runner()), McpOptions::default()).expect("server builds");
    let options = HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap());
    mcp.serve_http(options).await.expect("port opens")
}

/// The status of an `initialize` presenting `authorization` verbatim.
async fn status(server: &HttpServer, authorization: &str) -> StatusCode {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "raw", "version": "1"}}
    });
    common::http::install_crypto_provider();
    reqwest::Client::new()
        .post(server.url())
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("authorization", authorization)
        .body(body.to_string())
        .send()
        .await
        .expect("the port answers")
        .status()
}

/// What the host would write to its store, and read back.
fn stored(record: &TokenRecord) -> TokenRecord {
    serde_json::from_str(&serde_json::to_string(record).unwrap()).unwrap()
}

fn person() -> Actor {
    Actor::Person { name: "Sam".to_owned() }
}

#[tokio::test]
async fn the_same_secret_works_after_a_restart_and_a_changed_byte_does_not() {
    let before = start().await;
    let token = before.issue_for("Codex", Grants::agent_default(), person()).unwrap();
    assert_eq!(status(&before, &token.bearer_header()).await, StatusCode::OK);
    let json = serde_json::to_string(&token.record()).unwrap();
    assert!(!json.contains(token.secret()), "the record must not hold the secret: {json}");
    before.shutdown().await;

    let after = start().await;
    assert_eq!(status(&after, &token.bearer_header()).await, StatusCode::UNAUTHORIZED, "nothing is admitted yet");
    let id = after.admit(serde_json::from_str(&json).unwrap()).expect("admits");
    assert_eq!(status(&after, &token.bearer_header()).await, StatusCode::OK);
    let mut changed = token.secret().to_owned();
    changed.pop();
    changed.push(if token.secret().ends_with('0') { '1' } else { '0' });
    assert_eq!(status(&after, &format!("Bearer {changed}")).await, StatusCode::UNAUTHORIZED);

    let record = after.record(id).expect("live");
    assert_eq!((record.label.as_str(), &record.issuer, record.paired), ("Codex", &person(), false));
    assert_eq!(&record.grants, token.grants());
    after.shutdown().await;
}

#[tokio::test]
async fn a_deleted_record_stays_revoked_after_a_restart() {
    let before = start().await;
    let token = before.issue("Codex", Grants::agent_default()).unwrap();
    assert!(before.revoke(token.id()).await);
    assert!(!before.revoke(token.id()).await, "already revoked");
    assert_eq!(status(&before, &token.bearer_header()).await, StatusCode::UNAUTHORIZED);
    before.shutdown().await;

    // The host deleted the record, so there is nothing to admit: the secret stays unknown.
    let after = start().await;
    assert_eq!(status(&after, &token.bearer_header()).await, StatusCode::UNAUTHORIZED);
    after.shutdown().await;
}

#[tokio::test]
async fn a_tombstone_is_refused_by_admit() {
    let before = start().await;
    let token = before.issue("Codex", Grants::agent_default()).unwrap();
    let tombstone = before.revoke_record(token.id()).await.expect("was live");
    assert!(tombstone.revoked);
    assert!(before.record(token.id()).is_none());
    before.shutdown().await;

    let after = start().await;
    let refused = after.admit(stored(&tombstone));
    assert!(matches!(refused, Err(ServerError::TokenRevoked(ref label)) if label == "Codex"), "{refused:?}");
    assert_eq!(status(&after, &token.bearer_header()).await, StatusCode::UNAUTHORIZED);
    after.shutdown().await;
}

#[tokio::test]
async fn paired_and_grant_changes_are_in_the_record_and_survive_a_restart() {
    let before = start().await;
    let token = before.issue_for("Bridge", Grants::agent_default(), person()).unwrap();
    assert!(before.set_paired(token.id(), true));
    let wider = Grants::agent_default().with_fs_root("/projects/show");
    assert!(before.set_grants(token.id(), wider.clone()).await.unwrap());
    let record = before.record(token.id()).unwrap();
    assert!(record.paired && record.grants == wider);
    assert_ne!(token.record().grants, wider, "an issued token is a snapshot");
    before.revoke(token.id()).await;
    assert!(!before.set_paired(token.id(), true) && !before.set_grants(token.id(), wider.clone()).await.unwrap());
    before.shutdown().await;

    let after = start().await;
    let id = after.admit(stored(&record)).unwrap();
    let back = after.record(id).unwrap();
    assert!(back.paired && back.grants == wider && back.issuer == person());
    assert_eq!(status(&after, &token.bearer_header()).await, StatusCode::OK);
    after.shutdown().await;
}

#[tokio::test]
async fn admit_refuses_bad_future_and_duplicate_records() {
    let server = start().await;
    let token = server.issue("Codex", Grants::agent_default()).unwrap();
    let record = token.record();

    let duplicate = server.admit(record.clone());
    assert!(matches!(duplicate, Err(ServerError::BadTokenRecord { .. })), "{duplicate:?}");
    let future = server.admit(TokenRecord { version: omuzaki_mcp::RECORD_VERSION + 1, ..record.clone() });
    assert!(matches!(future, Err(ServerError::BadTokenRecord { .. })), "{future:?}");
    let malformed = server.admit(TokenRecord { hash: "not a hash".to_owned(), ..record });
    assert!(matches!(malformed, Err(ServerError::BadTokenRecord { .. })), "{malformed:?}");
    assert_eq!(status(&server, &token.bearer_header()).await, StatusCode::OK, "the live token is untouched");
    server.shutdown().await;
}

#[tokio::test]
async fn a_record_written_before_pairing_existed_still_admits() {
    let before = start().await;
    let token = before.issue("Old", Grants::agent_default()).unwrap();
    let mut value = serde_json::to_value(token.record()).unwrap();
    for field in ["version", "paired", "revoked"] {
        value.as_object_mut().unwrap().remove(field);
    }
    before.shutdown().await;

    let after = start().await;
    let id = after.admit(serde_json::from_value(value).unwrap()).unwrap();
    assert!(!after.record(id).unwrap().paired);
    after.shutdown().await;
}
