//! HTTP sessions through the registry: the transport and person a token's sessions are opened with, how they end
//! (the client leaves, the token is revoked, they go idle), and what happens to a token when the registry revokes the
//! credential of one of its sessions (design `agent-sessions.md` §3, §6, ADR 0020).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::http::{Raw, install_crypto_provider};
use common::registry::Studio;
use omuzaki_mcp::{AccessToken, HttpOptions, HttpServer, McpOptions, McpServer};
use omuzaki_ops_api::{Actor, EndReason, Grants, Sessions, Transport};
use reqwest::StatusCode;

fn sam() -> Actor {
    Actor::Person { name: "sam".to_owned() }
}

struct Rig {
    studio: Arc<Studio>,
    server: HttpServer,
    raw: Raw,
}

async fn rig(configure: impl FnOnce(HttpOptions) -> HttpOptions) -> Rig {
    install_crypto_provider();
    let studio = Studio::new();
    let mcp = McpServer::new(studio.clone(), McpOptions::default().with_sessions(studio.clone(), Studio::person()))
        .expect("server builds");
    let options = configure(HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap()));
    let server = mcp.serve_http(options).await.expect("port opens");
    let raw = Raw { http: reqwest::Client::new(), url: server.url() };
    Rig { studio, server, raw }
}

impl Rig {
    /// Opens an HTTP session with `token`, completing the handshake, and returns its id.
    async fn open(&self, token: &AccessToken) -> String {
        let id = self.raw.open_session(token).await;
        let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_owned();
        let response = self.raw.post_as(token, Some(&id), initialized).await;
        assert!(response.status().is_success(), "{}", response.status());
        id
    }

    async fn until_ended(&self, n: usize) {
        for _ in 0..300 {
            if self.studio.ends().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the registry never saw {n} sessions end: {:?}", self.studio.ends());
    }
}

#[tokio::test]
async fn a_token_session_opens_over_http_for_the_person_who_issued_the_token() {
    let rig = rig(|o| o).await;
    let token = rig.server.issue_for("Codex", Grants::agent_default(), sam()).unwrap();
    rig.open(&token).await;
    let specs = rig.studio.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].transport, Transport::Http { token: "Codex".to_owned() });
    assert_eq!(specs[0].on_behalf_of, sam(), "the person who issued the token, not the studio's");
    assert_eq!(specs[0].client, "raw");
    assert_eq!(specs[0].grants, Grants::agent_default());

    // A token the studio issued as itself works for the studio's person.
    let system = rig.server.issue("Other", Grants::agent_default()).unwrap();
    rig.open(&system).await;
    assert_eq!(rig.studio.specs()[1].on_behalf_of, Studio::person());
}

#[tokio::test]
async fn the_client_ending_its_session_ends_it_in_the_registry_as_disconnected() {
    let rig = rig(|o| o).await;
    let token = rig.server.issue("Codex", Grants::agent_default()).unwrap();
    let id = rig.open(&token).await;
    assert_eq!(rig.raw.delete(&token, &id).await, StatusCode::ACCEPTED);
    rig.until_ended(1).await;
    assert_eq!(rig.studio.ends()[0].1, EndReason::Disconnected);
}

#[tokio::test]
async fn revoking_a_token_ends_its_sessions_as_revoked_with_the_credential_and_the_old_secret_is_refused() {
    let rig = rig(|o| o).await;
    let token = rig.server.issue("Codex", Grants::agent_default()).unwrap();
    rig.open(&token).await;
    rig.open(&token).await;
    assert!(rig.server.revoke(token.id()).await);
    rig.until_ended(2).await;
    for (_, reason, _) in rig.studio.ends() {
        assert_eq!(reason, EndReason::Revoked { by: Studio::person(), credential: true });
    }
    assert_eq!(rig.raw.initialize(&token).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_session_the_registry_revokes_with_its_credential_takes_the_token_and_its_other_sessions_with_it() {
    let rig = rig(|o| o).await;
    let token = rig.server.issue("Codex", Grants::agent_default()).unwrap();
    rig.open(&token).await;
    rig.open(&token).await;
    let first = Sessions::sessions(rig.studio.as_ref())[0].id;
    rig.studio.registry_ends(first, EndReason::Revoked { by: Studio::person(), credential: true });

    // The token stops working, so a reconnect with the old secret gets 401, and the other session ends too.
    for _ in 0..300 {
        if rig.raw.initialize(&token).await.status() == StatusCode::UNAUTHORIZED {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(rig.raw.initialize(&token).await.status(), StatusCode::UNAUTHORIZED);
    rig.until_ended(2).await;
}

#[tokio::test]
async fn a_session_the_registry_revokes_without_its_credential_leaves_the_token_working() {
    let rig = rig(|o| o).await;
    let token = rig.server.issue("Codex", Grants::agent_default()).unwrap();
    let id = rig.open(&token).await;
    let session = Sessions::sessions(rig.studio.as_ref())[0].id;
    rig.studio.registry_ends(session, EndReason::Revoked { by: Studio::person(), credential: false });

    // The ended session's connection is closed, so its id no longer answers; the token itself still works.
    for _ in 0..300 {
        let response =
            rig.raw.post_as(&token, Some(&id), r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#.to_owned()).await;
        if response.status() == StatusCode::NOT_FOUND {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let again = rig.raw.initialize(&token).await;
    assert_eq!(again.status(), StatusCode::OK, "the token is still good");
    let gone = rig.raw.post_as(&token, Some(&id), r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#.to_owned()).await;
    assert_eq!(gone.status(), StatusCode::NOT_FOUND, "the ended session is closed");
}

#[tokio::test]
async fn a_session_that_goes_quiet_ends_as_idle() {
    let rig = rig(|o| o.with_idle_timeout(Duration::from_millis(300))).await;
    let token = rig.server.issue("Codex", Grants::agent_default()).unwrap();
    rig.open(&token).await;
    rig.until_ended(1).await;
    assert_eq!(rig.studio.ends()[0].1, EndReason::IdleTimeout);
}

#[tokio::test]
async fn changing_a_tokens_grants_ends_its_sessions_without_revoking_the_credential() {
    let rig = rig(|o| o).await;
    let token = rig.server.issue("Codex", Grants::agent_default()).unwrap();
    rig.open(&token).await;
    assert!(
        rig.server
            .set_grants(token.id(), Grants::agent_default().without(omuzaki_ops_api::Scope::Render))
            .await
            .unwrap()
    );
    rig.until_ended(1).await;
    assert_eq!(rig.studio.ends()[0].1, EndReason::Revoked { by: Studio::person(), credential: false });
    assert_eq!(rig.raw.initialize(&token).await.status(), StatusCode::OK);
}
