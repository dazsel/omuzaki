//! End to end over a real loopback port: rmcp's own Streamable HTTP client talks to `McpServer::serve_http`, and raw
//! HTTP requests probe what a client (or a web page) must not be able to do. The runner is the fake in `tests/common`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::http::{Frozen, Port, Raw};
use common::{FakeRunnerExt, fake_runner};
use common::{data, text, tool};
use omuzaki_mcp::{AccessToken, HttpOptions, McpOptions, McpServer, ServerError};
use omuzaki_ops_api::{Actor, Grants, Runner, Scope};
use reqwest::StatusCode;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{ClientHandler, ServiceExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BUILTINS: [&str; 6] = ["history_log", "history_redo", "history_undo", "tx_begin", "tx_commit", "tx_rollback"];

/// A client that introduces itself under a fixed name (the server numbers its agents `<name>#<n>`).
struct Named(&'static str);

impl ClientHandler for Named {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new(self.0, "1.0"))
    }
}

type Agent = RunningService<RoleClient, Named>;

// The rmcp client side of the shared port, kept here so `common` needs no MCP client.
impl Port {
    /// An MCP client that presents `token`, started with the `initialize` handshake.
    async fn connect(&self, token: &AccessToken, name: &'static str) -> Agent {
        let config = StreamableHttpClientTransportConfig::with_uri(self.server.url()).auth_header(token.secret());
        Named(name).serve(StreamableHttpClientTransport::from_config(config)).await.expect("client connects")
    }
}

/// Lets the server's own tasks run until `done` holds. Each step only yields to the runtime, so no real timer is
/// involved: a task that needs the clock is woken by `tokio::time::advance` first.
async fn settle(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if done() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{what} never happened");
}

async fn call(agent: &Agent, tool: &str, args: Value) -> CallToolResult {
    let args = args.as_object().expect("args are an object").clone();
    agent.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)).await.expect("call")
}

// ---- the acceptance path --------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_agent_lists_tools_previews_applies_and_undoes_over_a_real_port() {
    let port = Port::start().await;
    assert!(port.server.local_addr().ip().is_loopback());
    assert!(port.server.url().starts_with("http://127.0.0.1:") && port.server.url().ends_with("/mcp"));
    let agent = port.connect(&port.agent_token(), "http-agent").await;

    let tools = agent.list_all_tools().await.unwrap();
    // `agent.connection` is registered but not offered: this server's host wired no connection service.
    let mut expected: Vec<String> =
        port.runner.ops().iter().filter(|o| o.name != "agent.connection").map(|o| o.name.replace('.', "_")).collect();
    expected.extend(BUILTINS.map(str::to_owned));
    expected.sort();
    assert_eq!(tools.iter().map(|t| t.name.to_string()).collect::<Vec<_>>(), expected);

    let preview = call(&agent, "face_set_blend", json!({"character": "mira", "blend": 0.6, "dry_run": true})).await;
    assert_eq!((data(&preview)["mode"].clone(), data(&preview)["applied"].clone()), (json!("dry_run"), json!(false)));
    assert_eq!(port.runner.blend(), None, "a preview must not change the project");

    let applied = call(&agent, "face_set_blend", json!({"character": "mira", "blend": 0.6})).await;
    assert_eq!(data(&applied)["applied"], true, "{}", text(&applied));
    assert_eq!(port.runner.blend(), Some(json!(0.6_f32)));
    assert_eq!(port.runner.records()[0].actor, Actor::Agent { id: "http-agent#1".into(), model: None });

    let undone = call(&agent, "history_undo", json!({})).await;
    assert_eq!(undone.is_error, Some(false), "{}", text(&undone));
    assert_eq!(port.runner.blend(), None);

    agent.cancel().await.unwrap();
    port.server.shutdown().await;
}

// ---- who may connect ----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn nothing_gets_in_without_a_valid_token() {
    let port = Port::start().await;
    let token = port.agent_token();
    let raw = port.raw();
    let body = Raw::initialize_body();

    let bearer = token.bearer_header();
    let guess = format!("{bearer}0");
    for (case, headers) in [
        ("no credential", vec![]),
        ("wrong token", vec![("authorization", guess.as_str())]),
        ("wrong scheme", vec![("authorization", "Basic dXNlcjpwYXNz")]),
        ("empty bearer", vec![("authorization", "Bearer ")]),
        ("token in a query is not accepted", vec![]),
    ] {
        let url = if case.starts_with("token in a query") {
            format!("{}?token={}", raw.url, token.secret())
        } else {
            raw.url.clone()
        };
        let response = raw.post_to(&url, &headers, &body).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{case}");
        assert!(response.headers()["www-authenticate"].to_str().unwrap().starts_with("Bearer"), "{case}");
    }

    // Nor does a call get through to the runner without one; with the token the same call is applied.
    let edit = json!({"character": "mira", "blend": 0.9});
    for credential in [None, Some(guess.as_str())] {
        let response = raw.stateless_call(credential, "face_set_blend", &edit).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{credential:?}");
    }
    assert_eq!(port.runner.blend(), None);
    let applied = rpc_message(raw.stateless_call(Some(&bearer), "face_set_blend", &edit).await).await;
    assert_eq!(applied["result"]["structuredContent"]["applied"], true, "the control call works: {applied}");

    // And the handshake with the token is served, so the refusals above were about the credential.
    let ok = raw.post(&[("authorization", &bearer)], &body).await;
    assert_eq!(ok.status(), StatusCode::OK);
    port.server.shutdown().await;
}

#[tokio::test]
async fn only_the_mcp_path_is_served() {
    let port = Port::start_with(|options| options.with_path("/studio/mcp")).await;
    let token = port.agent_token();
    let raw = port.raw();
    let bearer = token.bearer_header();
    let headers = [("authorization", bearer.as_str())];

    assert_eq!(port.server.url(), format!("http://{}/studio/mcp", port.server.local_addr()));
    let body = Raw::initialize_body();
    let elsewhere = raw.post_to(&format!("http://{}/mcp", port.server.local_addr()), &headers, &body).await;
    assert_eq!(elsewhere.status(), StatusCode::NOT_FOUND);
    assert_eq!(raw.post(&headers, &body).await.status(), StatusCode::OK);
    port.server.shutdown().await;
}

#[tokio::test]
async fn a_web_page_cannot_drive_the_port_even_with_a_token() {
    let port = Port::start().await;
    let token = port.agent_token();
    let raw = port.raw();
    let bearer = token.bearer_header();
    let body = Raw::initialize_body();

    // Browsers always send `Origin` on a cross-origin request, and a page can't remove it.
    for origin in ["http://evil.example", "https://evil.example:8443", "http://localhost:3000", "null", "not an origin"]
    {
        let response = raw.post(&[("authorization", &bearer), ("origin", origin)], &body).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "Origin: {origin}");
    }
    // A DNS-rebinding page reaches the port under its own name, so the `Host` header gives it away too.
    let rebound = raw.post(&[("authorization", &bearer), ("host", "evil.example:80")], &body).await;
    assert_eq!(rebound.status(), StatusCode::FORBIDDEN, "Host: evil.example");

    assert_eq!(
        raw.post(&[("authorization", &bearer)], &body).await.status(),
        StatusCode::OK,
        "no Origin: a native client"
    );
    port.server.shutdown().await;
}

#[tokio::test]
async fn an_origin_the_host_lists_is_let_through() {
    let port = Port::start_with(|options| options.allow_origin("http://localhost:6274")).await;
    let token = port.agent_token();
    let raw = port.raw();
    let bearer = token.bearer_header();
    let body = Raw::initialize_body();

    let listed = raw.post(&[("authorization", &bearer), ("origin", "http://localhost:6274")], &body).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let other = raw.post(&[("authorization", &bearer), ("origin", "http://localhost:6275")], &body).await;
    assert_eq!(other.status(), StatusCode::FORBIDDEN, "listing one origin doesn't open the port to the rest");
    let tokenless = raw.post(&[("origin", "http://localhost:6274")], &body).await;
    assert_eq!(tokenless.status(), StatusCode::UNAUTHORIZED, "a listed origin still needs a token");
    port.server.shutdown().await;
}

#[tokio::test]
async fn the_port_only_opens_on_loopback_and_at_a_real_path() {
    let runner = Arc::new(fake_runner());
    let mcp = McpServer::new(runner, McpOptions::default()).unwrap();

    for open in ["0.0.0.0:0", "192.168.1.20:0", "[::]:0"] {
        let result = mcp.serve_http(HttpOptions::default().with_bind(open.parse().unwrap())).await;
        assert!(matches!(result, Err(ServerError::NotLoopback(addr)) if addr.to_string() == open), "{open}");
    }
    for path in ["mcp", "/mcp?x=1", "/mcp#top", ""] {
        let options = HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap()).with_path(path);
        let result = mcp.serve_http(options).await;
        assert!(matches!(result, Err(ServerError::BadPath(ref p)) if p == path), "{path:?}");
    }

    // Two ports can't share an address, and the error says which.
    let first = mcp.serve_http(HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap())).await.unwrap();
    let taken = mcp.serve_http(HttpOptions::default().with_bind(first.local_addr())).await;
    assert!(matches!(taken, Err(ServerError::Bind(addr, _)) if addr == first.local_addr()));
    first.shutdown().await;
}

#[tokio::test]
async fn the_port_also_works_on_the_ipv6_loopback() {
    let runner = Arc::new(fake_runner());
    let mcp = McpServer::new(runner, McpOptions::default()).unwrap();
    let options = HttpOptions::default().with_bind("[::1]:0".parse().unwrap());
    let Ok(server) = mcp.serve_http(options).await else {
        return; // this machine has no IPv6 loopback
    };
    let token = server.issue("v6", Grants::agent_default()).unwrap();
    assert!(server.url().starts_with("http://[::1]:"), "{}", server.url());
    let raw = Raw { http: reqwest::Client::new(), url: server.url() };
    let response = raw.post(&[("authorization", &token.bearer_header())], &Raw::initialize_body()).await;
    assert_eq!(response.status(), StatusCode::OK, "the bracketed Host header is recognised as loopback");
    server.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn connections_are_limited_and_silent_ones_are_dropped() {
    let _frozen = Frozen::new();
    let port =
        Port::start_with(|options| options.with_max_connections(1).with_header_timeout(Duration::from_millis(400)))
            .await;
    let bearer = port.agent_token().bearer_header();

    // A client that connects and says nothing holds the only slot...
    let mut silent = port.socket().await;
    // ...and a real request queues behind it. Its bytes go out now, so they are waiting the moment the server takes
    // the connection, and its own header timer cannot run out while time moves below.
    let body = Raw::initialize_body().to_string();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: {bearer}\r\nAccept: application/json, text/event-stream\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        port.server.local_addr(),
        body.len(),
    );
    let mut queued = port.socket().await;
    queued.write_all(request.as_bytes()).await.unwrap();
    let started = tokio::time::Instant::now();
    let answer = tokio::spawn(async move {
        let mut status = [0_u8; 12];
        queued.read_exact(&mut status).await.unwrap();
        (status, tokio::time::Instant::now())
    });

    // Time moves in 1 ms steps only until the silent connection is closed. The server takes no other connection
    // while it holds the slot, so it closes the silent one 400 ms after accepting it, and the queued request cannot
    // be answered before then.
    let mut byte = [0_u8; 1];
    let mut steps = 0_u32;
    loop {
        tokio::select! {
            read = silent.read(&mut byte) => {
                assert!(matches!(read, Ok(0) | Err(_)), "the silent connection was closed by the server: {read:?}");
                break;
            }
            () = tokio::time::advance(Duration::from_millis(1)) => {}
        }
        steps += 1;
        assert!(steps < 10_000, "the silent connection was never closed");
    }

    // The slot is free. Time stays put while the queued request is answered.
    let (status, answered_at) = answer.await.unwrap();
    assert_eq!(&status, b"HTTP/1.1 200", "the queued request is answered once the slot frees");
    assert!(
        answered_at.duration_since(started) >= Duration::from_millis(400),
        "the queued request got in before the silent client's header timeout: {:?}",
        answered_at.duration_since(started)
    );
    port.server.shutdown().await;
}

// ---- per-session grants -------------------------------------------------------------------------------------------------

#[tokio::test]
async fn each_token_holds_its_own_grants_and_its_tools_say_so() {
    let port = Port::start().await;
    let local = port.agent_token();
    let publisher = port.server.issue("exporter", Grants::agent_default().with(Scope::Publish)).unwrap();
    assert_eq!(local.grants(), &Grants::agent_default(), "the default is read, edit and render only");

    let plain = port.connect(&local, "plain").await;
    let trusted = port.connect(&publisher, "trusted").await;

    // What each agent is told about itself.
    let export_in = |tools: &[rmcp::model::Tool]| tool(tools, "export_vrm").description.as_deref().unwrap().to_owned();
    let plain_tools = plain.list_all_tools().await.unwrap();
    let trusted_tools = trusted.list_all_tools().await.unwrap();
    assert!(export_in(&plain_tools).contains("NOT granted in this session"), "{}", export_in(&plain_tools));
    assert!(export_in(&trusted_tools).contains("`publish`: granted in this session"), "{}", export_in(&trusted_tools));
    let guide = |agent: &Agent| agent.peer_info().unwrap().instructions.clone().unwrap();
    assert!(!guide(&plain).contains("publish"), "the default agent isn't told it may publish");
    assert!(guide(&trusted).contains("publish"));

    // And what the runner lets each do.
    let refused = call(&plain, "export_vrm", json!({"character": "mira"})).await;
    assert_eq!((refused.is_error, data(&refused)["code"].clone()), (Some(true), json!("denied")));
    let allowed = call(&trusted, "export_vrm", json!({"character": "mira"})).await;
    assert_eq!(allowed.is_error, Some(false), "{}", text(&allowed));
    // A preview needs only `read`, so the default agent can still see what the job would do.
    let preview = call(&plain, "export_vrm", json!({"character": "mira", "dry_run": true})).await;
    assert_eq!(preview.is_error, Some(false), "{}", text(&preview));

    // The server's own grants are untouched by the tokens issued from it.
    assert_eq!(port.mcp.grants(), &Grants::agent_default());
    port.server.shutdown().await;
}

#[tokio::test]
async fn a_session_belongs_to_the_token_that_opened_it() {
    let port = Port::start().await;
    let (alice, mallory) = (port.agent_token(), port.agent_token());
    let raw = port.raw();
    let session = raw.open_session(&alice).await;
    let ping = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});

    let (alice_bearer, mallory_bearer) = (alice.bearer_header(), mallory.bearer_header());
    let own = raw.post(&[("authorization", &alice_bearer), ("mcp-session-id", &session)], &ping).await;
    assert_eq!(own.status(), StatusCode::OK);
    let stolen = raw.post(&[("authorization", &mallory_bearer), ("mcp-session-id", &session)], &ping).await;
    assert_eq!(stolen.status(), StatusCode::NOT_FOUND, "another token can't ride on the session id");
    port.server.shutdown().await;
}

#[tokio::test]
async fn agents_on_one_token_are_still_separate_actors_with_their_own_undo() {
    let port = Port::start().await;
    let token = port.agent_token();
    let (first, second) = (port.connect(&token, "codex").await, port.connect(&token, "codex").await);

    call(&first, "face_set_blend", json!({"character": "mira", "blend": 0.3})).await;
    let actors: Vec<Actor> = port.runner.records().into_iter().map(|r| r.actor).collect();
    assert_eq!(actors, [Actor::Agent { id: "codex#1".into(), model: None }]);

    let nothing = call(&second, "history_undo", json!({})).await;
    // The result may also open with a heads-up about the first agent's edit (see `mcp_session.rs`); the answer is last.
    assert!(
        text(&nothing).ends_with("Nothing to undo."),
        "the second session has no steps of its own: {}",
        text(&nothing)
    );
    assert_eq!(port.runner.blend(), Some(json!(0.3_f32)));
    call(&first, "history_undo", json!({})).await;
    assert_eq!(port.runner.blend(), None);
    port.server.shutdown().await;
}

// ---- ending sessions ----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn revoking_a_token_ends_its_sessions_and_leaves_the_others() {
    let port = Port::start().await;
    let (doomed, kept) = (port.agent_token(), port.agent_token());
    let agent = port.connect(&doomed, "doomed").await;
    let bystander = port.connect(&kept, "kept").await;

    call(&agent, "tx_begin", json!({"label": "half done"})).await;
    call(&agent, "face_set_blend", json!({"character": "mira", "blend": 0.9})).await;
    assert!(port.runner.has_open_tx());

    assert!(port.server.revoke(doomed.id()).await);
    assert!(!port.server.revoke(doomed.id()).await, "revoking twice is harmless");
    settle("the revoked session's transaction to roll back", || !port.runner.has_open_tx()).await;
    assert_eq!(port.runner.blend(), None, "its pending edits were reverted");

    // The secret is dead on arrival now, and the other token's session carries on.
    let refused = port.raw().post(&[("authorization", &doomed.bearer_header())], &Raw::initialize_body()).await;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    assert!(agent.list_all_tools().await.is_err(), "the revoked client is cut off");
    assert!(bystander.list_all_tools().await.unwrap().len() > BUILTINS.len());
    port.server.shutdown().await;
}

#[tokio::test]
async fn shutting_down_closes_the_port_and_rolls_back_open_transactions() {
    let port = Port::start().await;
    let agent = port.connect(&port.agent_token(), "agent").await;
    call(&agent, "tx_begin", json!({"label": "left open"})).await;
    call(&agent, "face_set_blend", json!({"character": "mira", "blend": 0.7})).await;

    let (addr, runner) = (port.server.local_addr(), port.runner.clone());
    port.server.shutdown().await;
    settle("the open transaction to roll back", || !runner.has_open_tx()).await;
    assert_eq!(runner.blend(), None);
    assert!(tokio::net::TcpStream::connect(addr).await.is_err(), "nothing listens any more");
}

#[tokio::test(start_paused = true)]
async fn a_client_that_goes_quiet_loses_its_session_and_its_transaction() {
    let _frozen = Frozen::new();
    let port = Port::start_with(|options| options.with_idle_timeout(Duration::from_millis(300))).await;
    let agent = port.connect(&port.agent_token(), "crashy").await;
    call(&agent, "tx_begin", json!({"label": "never finished"})).await;
    call(&agent, "face_set_blend", json!({"character": "mira", "blend": 0.4})).await;

    // The client stays connected but sends nothing, as a crashed one would. Its idle limit is exactly 300 ms.
    tokio::time::advance(Duration::from_millis(299)).await;
    assert!(port.runner.has_open_tx(), "the session is still within its idle limit");
    tokio::time::advance(Duration::from_millis(1)).await;
    settle("the idle session to be closed", || !port.runner.has_open_tx()).await;
    assert_eq!(port.runner.blend(), None);
    port.server.shutdown().await;
}

// ---- protocol lifecycles ------------------------------------------------------------------------------------------------

/// The JSON-RPC message in a response that is either plain JSON or a single server-sent event.
async fn rpc_message(response: reqwest::Response) -> Value {
    let body = response.text().await.unwrap();
    let json = body.lines().find_map(|line| line.strip_prefix("data:")).map_or(body.as_str(), str::trim);
    serde_json::from_str(json).unwrap_or_else(|e| panic!("not JSON-RPC ({e}): {body}"))
}

#[tokio::test]
async fn clients_are_steered_to_sessions_because_state_cannot_live_between_stateless_requests() {
    use rmcp::model::{ErrorCode, ProtocolVersion};
    use rmcp::service::ClientInitializeError;
    use rmcp::{ClientLifecycleMode, ClientServiceExt};

    let port = Port::start().await;
    let token = port.agent_token();
    let transport = || {
        let config = StreamableHttpClientTransportConfig::with_uri(port.server.url()).auth_header(token.secret());
        StreamableHttpClientTransport::from_config(config)
    };

    // `server/discover` is answered as a server without a stateless mode would, so a client that only speaks the
    // stateless lifecycle is told so...
    let discover_only = ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] };
    let refused = Named("modern").serve_with_lifecycle(transport(), discover_only).await.err().expect("refused");
    assert!(
        matches!(&refused, ClientInitializeError::JsonRpcError(e) if e.code == ErrorCode::METHOD_NOT_FOUND),
        "{refused:?}"
    );

    // ...and one that probes first falls back to `initialize`, where transactions and undo work.
    let auto = ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        legacy_version: Some(ProtocolVersion::LATEST_WITH_INITIALIZE),
    };
    let agent = Named("probing").serve_with_lifecycle(transport(), auto).await.expect("falls back to a session");
    assert_eq!(call(&agent, "tx_begin", json!({"label": "grouped"})).await.is_error, Some(false));
    call(&agent, "face_set_blend", json!({"character": "mira", "blend": 0.2})).await;
    assert_eq!(call(&agent, "tx_commit", json!({})).await.is_error, Some(false));
    assert_eq!(call(&agent, "history_undo", json!({})).await.is_error, Some(false));
    assert_eq!(port.runner.blend(), None);
    port.server.shutdown().await;
}

#[tokio::test]
async fn a_request_that_stands_alone_can_read_the_log_with_read_and_is_refused_without_it() {
    let port = Port::start().await;
    let agent = port.agent_token();
    let blind = port.server.issue("blind", Grants::of([Scope::EditCharacter])).unwrap();
    let raw = port.raw();
    raw.stateless_call(Some(&agent.bearer_header()), "face_set_blend", &json!({"character": "mira", "blend": 0.5}))
        .await;

    // The log entry lands a moment after the edit: poll until it shows.
    let mut body = String::new();
    for _ in 0..250 {
        let read = rpc_message(raw.stateless_call(Some(&agent.bearer_header()), "history_log", &json!({})).await).await;
        body = read["result"]["content"][0]["text"].as_str().unwrap_or_default().to_owned();
        if body.contains("face.set_blend") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(body.contains("face.set_blend"), "{body}");

    let refused = rpc_message(raw.stateless_call(Some(&blind.bearer_header()), "history_log", &json!({})).await).await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(refused["result"]["structuredContent"]["code"], "denied", "{refused}");
    port.server.shutdown().await;
}

#[tokio::test]
async fn a_request_that_stands_alone_can_edit_but_is_told_why_it_cannot_use_transactions_or_undo() {
    let port = Port::start().await;
    let token = port.agent_token();
    let raw = port.raw();
    let bearer = token.bearer_header();

    let mut results = Vec::new();
    for (tool, arguments) in [
        ("face_set_blend", json!({"character": "mira", "blend": 0.5})),
        ("tx_begin", json!({"label": "x"})),
        ("history_undo", json!({})),
    ] {
        let message = rpc_message(raw.stateless_call(Some(&bearer), tool, &arguments).await).await;
        results.push(message["result"].clone());
    }
    assert_eq!(results[0]["structuredContent"]["applied"], true, "{}", results[0]);
    assert_eq!(port.runner.blend(), Some(json!(0.5_f32)));
    for refused in &results[1..] {
        assert_eq!(refused["isError"], true, "{refused}");
        assert_eq!(refused["structuredContent"]["code"], "unsupported", "{refused}");
        assert!(refused["structuredContent"]["hint"].as_str().unwrap().contains("initialize"), "{refused}");
    }
    assert!(!port.runner.has_open_tx(), "nothing was left open");
    port.server.shutdown().await;
}
