//! What a token holder can make the HTTP port hold on to, and what stops it: session caps, request-body size and
//! time, and what is released afterwards. Real loopback sockets, the fake runner of `tests/common`.
//!
//! Tests that involve time pause tokio's clock and freeze it (see [`Frozen`]), then move it by exactly the limit under
//! test, so they never sleep and can't flake. The same limits are checked to the millisecond, without sockets, in
//! `src/gate/tests.rs` and `src/limits.rs`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::FakeRunner;
use common::http::{Frozen, Port, Raw};
use omuzaki_mcp::{AccessToken, HttpOptions, McpOptions, McpServer, ServerError};
use reqwest::StatusCode;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};

/// The headers of a POST to the port, announcing `content_length` bytes of body that the caller may never send.
fn head(port: &Port, token: Option<&AccessToken>, content_length: usize, extra: &str) -> String {
    let authorization = token.map_or_else(String::new, |t| format!("Authorization: {}\r\n", t.bearer_header()));
    format!(
        "POST /mcp HTTP/1.1\r\nHost: {}\r\n{authorization}Accept: application/json, text/event-stream\r\n\
         Content-Type: application/json\r\nContent-Length: {content_length}\r\n{extra}\r\n",
        port.server.local_addr(),
    )
}

/// Reads until the server closes the connection, returning everything it said. A server that kept the connection open
/// would hang here, so a failure shows up as the test timing out under the harness.
async fn read_to_close(socket: &mut TcpStream) -> String {
    let mut said = Vec::new();
    socket.read_to_end(&mut said).await.expect("the server closes cleanly");
    String::from_utf8_lossy(&said).into_owned()
}

/// Reads the head of one response: up to and including the blank line.
async fn read_head(socket: &mut TcpStream) -> String {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(socket.read_u8().await.expect("the server is still talking"));
    }
    String::from_utf8(head).unwrap()
}
// ---- sessions ------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_token_is_held_to_its_session_cap_and_told_which_limit_it_hit() {
    let port = Port::start_with(|options| options.with_max_sessions_per_token(4)).await;
    let (token, other) = (port.agent_token(), port.agent_token());
    let mut sessions = Vec::new();
    for _ in 0..4 {
        sessions.push(port.raw().open_session(&token).await);
    }

    let refused = port.raw().initialize(&token).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    let message = refused.text().await.unwrap();
    assert!(message.contains("max_sessions_per_token = 4"), "{message}");
    assert!(message.contains("already has 4 open sessions") && message.contains("idle_timeout = 1800s"), "{message}");

    let tools = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string();
    for session in &sessions {
        let alive = port.raw().post_as(&token, Some(session), tools.clone()).await;
        assert_eq!(alive.status(), StatusCode::OK, "the refusal closed no session");
    }
    assert_eq!(port.raw().initialize(&other).await.status(), StatusCode::OK, "another token has its own cap");
    port.server.shutdown().await;
}

#[tokio::test]
async fn ending_a_session_makes_room_for_the_next() {
    let port = Port::start_with(|options| options.with_max_sessions_per_token(2)).await;
    let token = port.agent_token();
    let first = port.raw().open_session(&token).await;
    port.raw().open_session(&token).await;
    assert_eq!(port.raw().initialize(&token).await.status(), StatusCode::TOO_MANY_REQUESTS);

    assert_eq!(port.raw().delete(&token, &first).await, StatusCode::ACCEPTED);
    assert_eq!(port.raw().initialize(&token).await.status(), StatusCode::OK);
    assert_eq!(port.raw().initialize(&token).await.status(), StatusCode::TOO_MANY_REQUESTS);
    port.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_of_concurrent_initializes_never_gets_past_the_cap() {
    const CAP: usize = 8;
    const BURST: usize = 48;
    let port = Arc::new(Port::start_with(|options| options.with_max_sessions_per_token(CAP)).await);
    let token = port.agent_token();

    let clients: Vec<_> = (0..BURST)
        .map(|_| {
            let (port, token) = (Arc::clone(&port), token.clone());
            tokio::spawn(async move { port.raw().initialize(&token).await.status() })
        })
        .collect();
    let mut statuses = Vec::new();
    for client in clients {
        statuses.push(client.await.unwrap());
    }
    let admitted = statuses.iter().filter(|s| **s == StatusCode::OK).count();
    let refused = statuses.iter().filter(|s| **s == StatusCode::TOO_MANY_REQUESTS).count();
    assert_eq!(admitted, CAP, "{statuses:?}");
    assert_eq!(admitted + refused, BURST, "every request got a clear answer: {statuses:?}");
    Arc::into_inner(port).expect("clients are done").server.shutdown().await;
}

// ---- request bodies ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_body_over_max_body_bytes_is_refused_with_413_and_the_limit_named() {
    let port = Port::start_with(|options| options.with_max_body_bytes(1024)).await;
    let token = port.agent_token();
    let padded = |len: usize| format!("{:<len$}", Raw::initialize_body().to_string());

    assert_eq!(
        port.raw().post_as(&token, None, padded(1024)).await.status(),
        StatusCode::OK,
        "exactly the limit is fine"
    );
    let refused = port.raw().post_as(&token, None, padded(1025)).await;
    assert_eq!(refused.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let message = refused.text().await.unwrap();
    assert!(message.contains("max_body_bytes = 1024 bytes"), "{message}");

    // A client that only announces a huge body is refused on the announcement, without being waited for.
    let mut socket = port.socket().await;
    socket.write_all(head(&port, Some(&token), 50_000_000, "").as_bytes()).await.unwrap();
    let said = read_to_close(&mut socket).await;
    assert!(said.starts_with("HTTP/1.1 413") && said.contains("max_body_bytes = 1024 bytes"), "{said}");
    port.server.shutdown().await;
}

#[tokio::test]
async fn a_client_without_a_valid_token_cannot_hold_a_connection_with_a_body_it_never_sends() {
    let port = Port::start_with(|options| options).await;
    for credential in ["", "Authorization: Bearer omuzaki_mcp_not_a_token\r\n"] {
        // Headers and a promise of 1000 bytes that never come. The token is checked first, so the answer is
        // immediate, and the connection is closed rather than kept waiting for the rest.
        let mut socket = port.socket().await;
        socket.write_all(head(&port, None, 1000, credential).as_bytes()).await.unwrap();
        let said = read_to_close(&mut socket).await;
        assert!(said.starts_with("HTTP/1.1 401"), "{credential:?}: {said}");
    }
    port.server.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn stalled_bodies_are_cut_off_at_the_timeout_and_the_port_serves_everyone_again() {
    let _frozen = Frozen::new();
    // Two connections in all: two clients that stall their bodies are enough to lock every other client out.
    let port =
        Port::start_with(|options| options.with_max_connections(2).with_body_timeout(Duration::from_secs(30))).await;
    let token = port.agent_token();

    let mut stalled = Vec::new();
    for _ in 0..2 {
        let mut socket = port.socket().await;
        socket.write_all(head(&port, Some(&token), 1000, "Expect: 100-continue\r\n").as_bytes()).await.unwrap();
        // The server answers `100 Continue` when it starts to read the body, so from here its timeout is running.
        let head = read_head(&mut socket).await;
        assert!(head.starts_with("HTTP/1.1 100 Continue"), "{head}");
        socket.write_all(br#"{"jsonrpc":"2.0","id":1,"met"#).await.unwrap();
        stalled.push(socket);
    }

    // The port is full, so this client waits for a slot...
    let waiting = {
        let (raw, bearer) = (port.raw(), token.bearer_header());
        tokio::spawn(async move { raw.post(&[("authorization", &bearer)], &Raw::initialize_body()).await.status() })
    };
    tokio::time::advance(Duration::from_secs(29)).await;
    assert!(!waiting.is_finished(), "the stalled clients still hold both connections after 29 s");

    // ...until the stalled ones run out of time. They are told why, and let go.
    tokio::time::advance(Duration::from_secs(1)).await;
    for socket in &mut stalled {
        let said = read_to_close(socket).await;
        assert!(said.starts_with("HTTP/1.1 408"), "{said}");
        assert!(said.contains("body_timeout = 30s"), "{said}");
    }
    assert_eq!(waiting.await.unwrap(), StatusCode::OK);
    port.server.shutdown().await;
}

// ---- responses -----------------------------------------------------------------------------------------------------

/// Reads whatever has already arrived, without waiting, and fails if the server has ended the stream. Reading all the way
/// to `WouldBlock` matters: a close can sit behind bytes that are already buffered, and only the read after them shows it.
fn drain_open(stream: &mut TcpStream) {
    let mut buf = [0; 1024];
    loop {
        match stream.try_read(&mut buf) {
            Ok(0) => panic!("the server ended the event stream"),
            Ok(_) => {}
            Err(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock, "{error}");
                return;
            }
        }
    }
}

/// Gives the server's tasks real time to run while the clock stays frozen: the server's I/O moves in real time, and
/// tokio does not auto-advance the clock while a blocking task runs.
async fn let_the_server_run() {
    tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(20))).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_client_that_never_reads_its_answer_loses_its_connection_at_the_write_timeout() {
    let _frozen = Frozen::new();
    // One connection in all: the stalled client holds the port's only slot, so the probe below waits for it.
    let port =
        Port::start_with(|options| options.with_max_connections(1).with_write_timeout(Duration::from_secs(30))).await;
    let token = port.agent_token();

    // A tiny receive buffer makes the server's writes block almost at once. The answer is about 13 MB, far more than
    // the socket buffers hold, and this client never reads a byte of it.
    let socket = TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let mut stalled = socket.connect(port.server.local_addr()).await.expect("the port accepts");
    let body = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {
            "name": "character_list_parts", "arguments": {"count": 1_000_000},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "stalled", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {},
            },
        },
    })
    .to_string();
    let extra = "mcp-protocol-version: 2026-07-28\r\nmcp-method: tools/call\r\nmcp-name: character_list_parts\r\n";
    stalled.write_all(head(&port, Some(&token), body.len(), extra).as_bytes()).await.unwrap();
    stalled.write_all(body.as_bytes()).await.unwrap();

    // A second client, which can get in only once the stalled connection's slot is given back.
    let probe = {
        let (raw, bearer) = (port.raw(), token.bearer_header());
        tokio::spawn(
            async move { raw.stateless_call(Some(bearer.as_str()), "project_ping", &json!({})).await.status() },
        )
    };
    let mut advanced = Duration::ZERO;
    while !probe.is_finished() {
        assert!(advanced < Duration::from_secs(60), "the stalled connection was never cut off");
        let_the_server_run().await;
        if probe.is_finished() {
            break;
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        advanced += Duration::from_secs(1);
    }
    // The slot is given back only once the write has been blocked for the whole timeout, never earlier.
    assert!(advanced >= Duration::from_secs(30), "the connection was cut off after only {advanced:?}");
    assert_eq!(probe.await.unwrap(), StatusCode::OK, "the freed slot serves the next client");
    drop(stalled);
    port.server.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn an_idle_event_stream_is_never_cut_off_by_the_write_timeout() {
    let _frozen = Frozen::new();
    let port = Port::start_with(|options| options.with_write_timeout(Duration::from_secs(30))).await;
    let token = port.agent_token();
    let session = port.raw().open_session(&token).await;
    let mut stream = port.socket().await;
    let request = format!(
        "GET /mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: {}\r\nMcp-Session-Id: {session}\r\n\
         Accept: text/event-stream\r\n\r\n",
        port.server.local_addr(),
        token.bearer_header(),
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let head = read_head(&mut stream).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    // Take what the stream has already sent, so the check below sees only what arrives after it.
    let_the_server_run().await;
    drain_open(&mut stream);

    // Silence for twice the write timeout, with nothing pending to write: the stream must stay open, which shows as no
    // end of stream even after everything sent in the meantime has been read.
    tokio::time::advance(Duration::from_secs(60)).await;
    let_the_server_run().await;
    drain_open(&mut stream);
    port.server.shutdown().await;
}

// ---- configuration -----------------------------------------------------------------------------------------------

#[test]
fn the_defaults_are_the_documented_safe_ones() {
    let options = HttpOptions::default();
    assert_eq!(options.max_sessions_per_token, 32);
    assert_eq!(options.max_body_bytes, 4 * 1024 * 1024);
    assert_eq!(options.body_timeout, Duration::from_secs(30));
    assert_eq!(options.write_timeout, Duration::from_secs(30));
    assert_eq!((options.max_connections, options.header_timeout), (64, Duration::from_secs(10)));
    assert_eq!(options.idle_timeout, Duration::from_mins(30));
}

#[tokio::test]
async fn a_timeout_beyond_the_documented_maximum_is_refused_naming_the_field_and_the_maximum() {
    let mcp = McpServer::new(Arc::new(FakeRunner::new()), McpOptions::default()).unwrap();
    let base = || HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap());
    let too_long = [Duration::MAX, HttpOptions::MAX_TIMEOUT + Duration::from_nanos(1)];
    for timeout in too_long {
        let cases: [(&str, HttpOptions); 4] = [
            ("idle_timeout", base().with_idle_timeout(timeout)),
            ("header_timeout", base().with_header_timeout(timeout)),
            ("body_timeout", base().with_body_timeout(timeout)),
            ("write_timeout", base().with_write_timeout(timeout)),
        ];
        for (name, options) in cases {
            let error = mcp.serve_http(options).await.err().expect("refused");
            assert!(matches!(&error, ServerError::BadLimit { name: n, .. } if *n == name), "{timeout:?}: {error}");
            let message = error.to_string();
            assert!(message.contains(name) && message.contains("must be at most 86400s"), "{message}");
        }
    }
}

#[tokio::test]
async fn timeouts_at_the_maximum_are_accepted_and_requests_are_served() {
    let max = HttpOptions::MAX_TIMEOUT;
    let port = Port::start_with(|o| o.with_idle_timeout(max).with_header_timeout(max).with_body_timeout(max)).await;
    let token = port.agent_token();
    let session = port.raw().open_session(&token).await;
    let tools = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string();
    assert_eq!(port.raw().post_as(&token, Some(&session), tools).await.status(), StatusCode::OK);
    port.server.shutdown().await;
}

#[tokio::test]
async fn a_limit_that_would_refuse_every_client_is_an_error_naming_it() {
    let mcp = McpServer::new(Arc::new(FakeRunner::new()), McpOptions::default()).unwrap();
    let base = || HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap());
    let cases: [(&str, HttpOptions); 4] = [
        ("max_sessions_per_token", base().with_max_sessions_per_token(0)),
        ("max_body_bytes", base().with_max_body_bytes(0)),
        ("body_timeout", base().with_body_timeout(Duration::ZERO)),
        ("write_timeout", base().with_write_timeout(Duration::ZERO)),
    ];
    for (name, options) in cases {
        let error = mcp.serve_http(options).await.err().expect("refused");
        assert!(matches!(&error, ServerError::BadLimit { name: n, .. } if *n == name), "{error}");
        assert!(error.to_string().contains(name), "{error}");
    }
}
