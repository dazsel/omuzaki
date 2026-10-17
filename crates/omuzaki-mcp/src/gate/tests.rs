//! The gate's limits, with no sockets and a paused clock: requests are handed straight to [`Gate::handle`] and time
//! moves only when a test advances it, so every timeout is checked to the millisecond and nothing sleeps.

use std::future::Future;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::task::Poll;
use std::time::Duration;

use hyper::header::{ACCEPT, AUTHORIZATION, CONNECTION, CONTENT_TYPE, HOST};
use hyper::{HeaderMap, Method, StatusCode};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request as OpRequest, Runner, TxId};
use rmcp::transport::common::http_header::HEADER_SESSION_ID;
use serde_json::json;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::access::AccessToken;
use crate::limits::test_support::fed;
use crate::server::{McpOptions, McpServer};

/// A runner with no ops: the gate's limits don't depend on what the tools do.
struct NoOps;

impl Runner for NoOps {
    fn ops(&self) -> Vec<OpInfo> {
        Vec::new()
    }
    fn run(&self, _: OpRequest) -> Result<Outcome, OpError> {
        Err(OpError::unsupported("this runner has no ops"))
    }
    fn begin(&self, _: &str, _: &Actor) -> TxId {
        TxId(1)
    }
    fn commit(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn rollback(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn undo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }
    fn redo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }
    fn subscribe(&self, _: &Grants) -> Result<Receiver<Event>, OpError> {
        Ok(channel().1)
    }
    fn get(&self, _: &DocPath, _: &Grants) -> Result<Option<Value>, OpError> {
        Ok(None)
    }
    fn log(&self, _: u64, _: usize, _: &Grants) -> Result<Vec<OpRecord>, OpError> {
        Ok(Vec::new())
    }
}

const SESSIONS: usize = 3;
const BODY_BYTES: usize = 512;
const BODY_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE: Duration = Duration::from_secs(600);

fn limits() -> Limits {
    Limits {
        idle_timeout: IDLE,
        max_sessions_per_token: SESSIONS,
        max_body_bytes: BODY_BYTES,
        body_timeout: BODY_TIMEOUT,
    }
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"gate-test","version":"1"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
const LIST_TOOLS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

struct Rig {
    gate: Gate,
    registry: Arc<Registry>,
    /// What the gate logged. The gate writes to this, not to `tracing`, so no test depends on a process-wide subscriber.
    recorder: Recorder,
}

impl Rig {
    fn new() -> Self {
        Self::with(limits())
    }

    fn with(limits: Limits) -> Self {
        let server = McpServer::new(Arc::new(NoOps), McpOptions::default()).unwrap();
        let (revocations, _) = tokio::sync::mpsc::unbounded_channel();
        let registry = Arc::new(Registry::new(server, Vec::new(), limits, CancellationToken::new(), revocations));
        let recorder = Recorder::default();
        let gate = Gate::with_sink(Arc::clone(&registry), "/mcp".to_owned(), Arc::new(recorder.clone()));
        Self { gate, registry, recorder }
    }

    fn token(&self) -> AccessToken {
        self.registry.issue("gate test".to_owned(), Grants::agent_default()).unwrap()
    }

    /// How many sessions `token` has open, as the cap sees them.
    async fn open(&self, token: &AccessToken) -> usize {
        let endpoint = self.registry.authorize(&bearer(token)).unwrap();
        endpoint.open_sessions().await
    }

    async fn send<B>(&self, request: Request<B>) -> Response<ResponseBody>
    where
        B: Body + Send + 'static,
        B::Error: Display,
    {
        self.gate.handle(request).await
    }

    /// Opens a session, returning its response (the id is in its `mcp-session-id` header).
    async fn initialize(&self, token: &AccessToken) -> Response<ResponseBody> {
        self.send(post(token, None, INITIALIZE)).await
    }

    /// Opens a session that must be admitted and completes the handshake as a client does, returning its id.
    async fn open_session(&self, token: &AccessToken) -> String {
        let response = self.initialize(token).await;
        assert_eq!(response.status(), StatusCode::OK, "{}", body_text(response).await);
        let session = response.headers()[HEADER_SESSION_ID].to_str().unwrap().to_owned();
        assert_eq!(self.status_of(post(token, Some(&session), INITIALIZED)).await, StatusCode::ACCEPTED);
        self.quiesce().await;
        session
    }

    async fn status_of(&self, request: Request<Full<Bytes>>) -> StatusCode {
        self.send(request).await.status()
    }

    async fn delete(&self, token: &AccessToken, session: &str) -> StatusCode {
        let request = Request::builder()
            .method(Method::DELETE)
            .uri("/mcp")
            .header(HOST, "127.0.0.1")
            .header(AUTHORIZATION, token.bearer_header())
            .header(HEADER_SESSION_ID, session)
            .body(Full::new(Bytes::new()))
            .unwrap();
        self.status_of(request).await
    }

    /// Lets every task of the server run until none has anything left to do. Sessions are served by tasks of their own,
    /// and a paused clock only moves once none can run, so one short virtual sleep is the point where they have all
    /// caught up (no real time passes). Without it the clock could be moved before a worker had seen its last event,
    /// and the worker would then start its idle timer from the wrong time.
    async fn quiesce(&self) {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    /// Moves the clock by `by`, with the server idle before and after.
    async fn pass(&self, by: Duration) {
        self.quiesce().await;
        tokio::time::advance(by).await;
        self.quiesce().await;
    }
}

fn bearer(token: &AccessToken) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, token.bearer_header().parse().unwrap());
    headers
}

/// A POST as an MCP client sends it, with a body of any kind.
fn post_with<B>(token: &AccessToken, session: Option<&str>, body: B) -> Request<B> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(HOST, "127.0.0.1")
        .header(AUTHORIZATION, token.bearer_header())
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json");
    if let Some(session) = session {
        request = request.header(HEADER_SESSION_ID, session);
    }
    request.body(body).unwrap()
}

fn post(token: &AccessToken, session: Option<&str>, body: &str) -> Request<Full<Bytes>> {
    post_with(token, session, Full::new(Bytes::from(body.to_owned())))
}

async fn body_text(response: Response<ResponseBody>) -> String {
    String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
}

// ---- sessions ----------------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_token_gets_its_sessions_and_the_next_is_refused_with_the_limit_named() {
    let rig = Rig::new();
    let (token, other) = (rig.token(), rig.token());
    let mut sessions = Vec::new();
    for _ in 0..SESSIONS {
        sessions.push(rig.open_session(&token).await);
    }
    assert_eq!(rig.open(&token).await, SESSIONS);

    let refused = rig.initialize(&token).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    let message = body_text(refused).await;
    assert!(message.contains("max_sessions_per_token = 3"), "{message}");
    assert!(message.contains(&format!("already has {SESSIONS} open sessions")), "{message}");
    assert!(message.contains("idle_timeout = 600s"), "{message}");

    // Nothing was evicted to make room: every session still answers.
    assert_eq!(rig.open(&token).await, SESSIONS);
    for session in &sessions {
        let alive = rig.status_of(post(&token, Some(session), LIST_TOOLS)).await;
        assert_eq!(alive, StatusCode::OK, "session {session} survived the refusal");
    }
    // The cap belongs to the token: another one is untouched.
    assert_eq!(rig.initialize(&other).await.status(), StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_second_initialize_waits_while_the_first_is_still_being_answered() {
    // The turn is what closes the window between "the count has room" and "the new session is in rmcp's table", so
    // it must be held until rmcp has answered, not released once the count has been checked. Here the first
    // initialize is stopped mid-flight (rmcp has made its session, the handshake reply has not come back) at a cap
    // of one. The second must wait for the first's turn: if it were let past the count it would be refused at once.
    let rig = Rig::with(Limits { max_sessions_per_token: 1, ..limits() });
    let token = rig.token();
    let mut first = std::pin::pin!(rig.initialize(&token));
    let mut second = std::pin::pin!(rig.initialize(&token));

    let first_poll = std::future::poll_fn(|cx| Poll::Ready(first.as_mut().poll(cx))).await;
    assert!(first_poll.is_pending(), "the first initialize is waiting for rmcp's handshake");
    assert_eq!(rig.open(&token).await, 1, "and its session is already in the table");

    let second_poll = std::future::poll_fn(|cx| Poll::Ready(second.as_mut().poll(cx))).await;
    assert!(second_poll.is_pending(), "the second must wait for the first's turn, not be judged against the count");

    // Once the first is answered the second takes its turn, and finds the token full.
    let (first, second) = tokio::join!(first, second);
    assert_eq!((first.status(), second.status()), (StatusCode::OK, StatusCode::TOO_MANY_REQUESTS));
    assert_eq!(rig.open(&token).await, 1);
}

#[tokio::test(start_paused = true)]
async fn only_sessions_are_counted_so_a_token_at_its_cap_can_still_call_statelessly() {
    let rig = Rig::new();
    let token = rig.token();
    for _ in 0..SESSIONS {
        rig.open_session(&token).await;
    }
    // A 2026-07-28 request stands alone: it opens no session, so it is not what the cap is for.
    let stateless = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {
            "name": "tx_begin", "arguments": {"label": "x"},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "s", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {},
            },
        },
    })
    .to_string();
    let mut request = post(&token, None, &stateless);
    for (name, value) in
        [("mcp-protocol-version", "2026-07-28"), ("mcp-method", "tools/call"), ("mcp-name", "tx_begin")]
    {
        request.headers_mut().insert(name, value.parse().unwrap());
    }
    assert_eq!(rig.status_of(request).await, StatusCode::OK);
    // Neither does a notification or a call with a session id, whatever else is wrong with it.
    assert_ne!(rig.status_of(post(&token, Some("no-such-session"), LIST_TOOLS)).await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(rig.open(&token).await, SESSIONS);
}

#[tokio::test(start_paused = true)]
async fn ending_a_session_frees_its_slot_and_refusals_never_leak_one() {
    let rig = Rig::new();
    let token = rig.token();
    let mut sessions = Vec::new();
    for _ in 0..SESSIONS {
        sessions.push(rig.open_session(&token).await);
    }
    // Many refusals, and many initializes that are admitted but fail in rmcp (bad JSON): none may keep a reservation.
    for _ in 0..50 {
        assert_eq!(rig.initialize(&token).await.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    assert_eq!(rig.delete(&token, &sessions[0]).await, StatusCode::ACCEPTED);
    rig.quiesce().await;
    assert_eq!(rig.open(&token).await, SESSIONS - 1);
    for _ in 0..50 {
        let malformed = rig.status_of(post(&token, None, "{\"method\":\"initialize\"")).await;
        assert_eq!(malformed, StatusCode::UNSUPPORTED_MEDIA_TYPE, "room was available, so rmcp judged it");
    }
    assert_eq!(rig.initialize(&token).await.status(), StatusCode::OK, "the freed slot is still there");
    assert_eq!(rig.initialize(&token).await.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(start_paused = true)]
async fn a_quiet_session_is_reaped_at_the_idle_timeout_and_its_slot_comes_back() {
    let rig = Rig::new();
    let token = rig.token();
    let busy = rig.open_session(&token).await;
    let quiet = [rig.open_session(&token).await, rig.open_session(&token).await];
    assert_eq!(rig.initialize(&token).await.status(), StatusCode::TOO_MANY_REQUESTS);

    // Traffic keeps a session alive: 400 s in, `busy` talks; the other two have been silent since they opened.
    rig.pass(Duration::from_secs(400)).await;
    assert_eq!(rig.status_of(post(&token, Some(&busy), LIST_TOOLS)).await, StatusCode::OK);
    // Just short of their idle timeout nothing has been closed...
    rig.pass(Duration::from_secs(198)).await; // 598 s in: two seconds short of the idle timeout for the quiet ones
    assert_eq!(rig.open(&token).await, SESSIONS, "not yet");
    // ...and just after it the quiet ones are gone, the busy one is not, and there is room again.
    rig.pass(Duration::from_secs(4)).await;
    assert_eq!(rig.open(&token).await, 1, "the two quiet sessions were reaped");
    for id in &quiet {
        assert_eq!(rig.status_of(post(&token, Some(id), LIST_TOOLS)).await, StatusCode::NOT_FOUND, "{id} was closed");
    }
    assert_eq!(rig.status_of(post(&token, Some(&busy), LIST_TOOLS)).await, StatusCode::OK);
    assert_eq!(rig.initialize(&token).await.status(), StatusCode::OK);
    assert_eq!(rig.open(&token).await, 2);

    // A client that abandons everything is back to an empty table once the timeout passes: nothing is left behind.
    rig.pass(IDLE + Duration::from_secs(2)).await;
    assert_eq!(rig.open(&token).await, 0);
}

#[tokio::test(start_paused = true)]
async fn sessions_whose_client_vanishes_before_finishing_the_handshake_are_reaped_too() {
    let rig = Rig::new();
    let token = rig.token();
    // `initialize` answered, `notifications/initialized` never sent: a client that crashed between the two.
    for _ in 0..SESSIONS {
        assert_eq!(rig.initialize(&token).await.status(), StatusCode::OK);
    }
    rig.quiesce().await;
    assert_eq!(rig.initialize(&token).await.status(), StatusCode::TOO_MANY_REQUESTS);

    rig.pass(IDLE + Duration::from_secs(2)).await;
    assert_eq!(rig.open(&token).await, 0);
    assert_eq!(rig.initialize(&token).await.status(), StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_client_that_hangs_up_halfway_through_opening_a_session_leaks_nothing() {
    let rig = Rig::new();
    let token = rig.token();
    // The connection is dropped while the request is still being served, wherever that had got to.
    for _ in 0..SESSIONS * 2 {
        let _ = tokio::time::timeout(Duration::ZERO, rig.initialize(&token)).await;
    }
    // Whatever was half-made is closed by rmcp's own handshake timeout (60 s), and no turn or slot stays taken.
    rig.pass(Duration::from_secs(120)).await;
    assert_eq!(rig.open(&token).await, 0);
    for _ in 0..SESSIONS {
        rig.open_session(&token).await;
    }
    assert_eq!(rig.initialize(&token).await.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(start_paused = true)]
async fn revoking_a_token_empties_its_sessions() {
    let rig = Rig::new();
    let token = rig.token();
    for _ in 0..SESSIONS {
        rig.open_session(&token).await;
    }
    let endpoint = rig.registry.authorize(&bearer(&token)).unwrap();
    assert!(rig.registry.revoke(token.id()).await);
    assert_eq!(endpoint.open_sessions().await, 0, "revoking released every session");
    assert_eq!(rig.initialize(&token).await.status(), StatusCode::UNAUTHORIZED);
}

// ---- request bodies ------------------------------------------------------------------------------------------------

/// A valid `initialize` padded with trailing spaces to exactly `len` bytes.
fn initialize_of_len(len: usize) -> String {
    format!("{INITIALIZE:<len$}")
}

#[tokio::test(start_paused = true)]
async fn a_body_over_the_limit_is_refused_with_413_naming_it_and_creates_nothing() {
    let rig = Rig::new();
    let token = rig.token();

    // Exactly at the limit is fine...
    let at_limit = rig.send(post(&token, None, &initialize_of_len(BODY_BYTES))).await;
    assert_eq!(at_limit.status(), StatusCode::OK);
    assert_eq!(rig.open(&token).await, 1);

    // ...one byte more is not, whether it announces its size (refused unread) or only turns out to be big.
    let announced = rig.send(post(&token, None, &initialize_of_len(BODY_BYTES + 1))).await;
    assert_eq!(announced.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(announced.headers()[CONNECTION], "close");
    assert!(body_text(announced).await.contains("max_body_bytes = 512 bytes"));

    let (tx, body) = fed();
    tx.send(Bytes::from(initialize_of_len(BODY_BYTES))).unwrap();
    tx.send(Bytes::from_static(b" ")).unwrap();
    let streamed = rig.send(post_with(&token, None, body)).await;
    assert_eq!(streamed.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(tx.is_closed(), "the connection's body was released");
    assert_eq!(rig.open(&token).await, 1, "a refused body opened no session");
}

#[tokio::test(start_paused = true)]
async fn a_stalled_body_gets_408_exactly_at_the_timeout_and_nothing_survives_it() {
    let rig = Rig::new();
    let token = rig.token();
    let (tx, body) = fed();
    tx.send(Bytes::from_static(br#"{"jsonrpc":"2.0","id":1,"method":"init"#)).unwrap();

    let started = Instant::now();
    let response = rig.send(post_with(&token, None, body)).await;
    assert_eq!(started.elapsed(), BODY_TIMEOUT);
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(response.headers()[CONNECTION], "close");
    let message = body_text(response).await;
    assert!(message.contains("body_timeout = 30s"), "{message}");
    assert!(tx.is_closed(), "the stalled body was dropped, so its connection is released");
    assert_eq!(rig.open(&token).await, 0);
}

#[tokio::test(start_paused = true)]
async fn a_slow_body_that_arrives_in_time_is_served() {
    let rig = Rig::new();
    let token = rig.token();
    let (tx, body) = fed();
    let (first, second) = INITIALIZE.split_at(40);
    let (first, second) = (Bytes::from(first.to_owned()), Bytes::from(second.to_owned()));
    let client = tokio::spawn(async move {
        tx.send(first).unwrap();
        tokio::time::sleep(Duration::from_secs(29)).await; // slow, but inside the deadline
        tx.send(second).unwrap();
    });
    let response = rig.send(post_with(&token, None, body)).await;
    client.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn nobody_without_a_token_makes_the_port_wait_for_a_body() {
    let rig = Rig::new();
    let token = rig.token();
    let (_tx, stalled) = fed();
    let mut request = post_with(&token, None, stalled);
    request.headers_mut().insert(AUTHORIZATION, "Bearer not-a-token".parse().unwrap());

    let started = Instant::now();
    assert_eq!(rig.send(request).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(started.elapsed(), Duration::ZERO, "refused on the credential, before any of the body was awaited");

    let (_tx, stalled) = fed();
    let mut elsewhere = post_with(&token, None, stalled);
    *elsewhere.uri_mut() = "/other".parse().unwrap();
    assert_eq!(rig.send(elsewhere).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(started.elapsed(), Duration::ZERO);
}

// ---- refusal logging ---------------------------------------------------------------------------------------------

/// A request with a credential the registry does not know.
fn with_unknown_token(token: &AccessToken) -> Request<Full<Bytes>> {
    let mut request = post(token, None, INITIALIZE);
    request.headers_mut().insert(AUTHORIZATION, "Bearer not-a-token".parse().unwrap());
    request
}

/// What a test subscriber saw: the level of each event and its fields as text.
#[derive(Clone, Default)]
struct Recorder(Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>);

impl Recorder {
    /// The events whose text contains `message`, as `(level, fields)` pairs.
    fn lines(&self, message: &str) -> Vec<(tracing::Level, String)> {
        let events = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        events.iter().filter(|(_, fields)| fields.contains(message)).cloned().collect()
    }
}

impl RefusalSink for Recorder {
    fn token_refused(&self, suppressed: Option<u64>, denied: Denied, method: &hyper::Method) {
        self.push(suppressed, format!("{UNAUTHORIZED_LINE}: {denied:?} {method}"));
    }

    fn body_refused(&self, suppressed: Option<u64>, status: StatusCode, message: &str) {
        self.push(suppressed, format!("{BODY_LINE}: {status} {message}"));
    }
}

impl Recorder {
    /// Records a refusal as `warn` (with its suppressed count) or `debug`, as the production sink writes it.
    fn push(&self, suppressed: Option<u64>, text: String) {
        let (level, fields) = match suppressed {
            Some(n) => (tracing::Level::WARN, format!("{text} suppressed={n}")),
            None => (tracing::Level::DEBUG, text),
        };
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((level, fields));
    }
}

const UNAUTHORIZED_LINE: &str = "refused a request without a valid access token";
const BODY_LINE: &str = "refused a request body";

#[tokio::test(start_paused = true)]
async fn a_loop_of_refused_requests_warns_once_per_interval_and_debugs_the_rest() {
    let rig = Rig::new();
    let recorder = rig.recorder.clone();
    let token = rig.token();

    for _ in 0..100 {
        assert_eq!(rig.send(with_unknown_token(&token)).await.status(), StatusCode::UNAUTHORIZED);
    }
    let lines = recorder.lines(UNAUTHORIZED_LINE);
    let warns: Vec<_> = lines.iter().filter(|(level, _)| *level == tracing::Level::WARN).collect();
    assert_eq!(warns.len(), 1, "one warning for the loop, not one per request");
    assert!(warns[0].1.contains("suppressed=0"), "the first warning has nothing suppressed yet: {}", warns[0].1);
    assert_eq!(lines.len() - warns.len(), 99, "the other 99 are debug events");
    assert!(
        lines
            .iter()
            .filter(|(level, _)| *level != tracing::Level::WARN)
            .all(|(level, _)| *level == tracing::Level::DEBUG)
    );

    rig.pass(REFUSAL_WARN_INTERVAL).await;
    assert_eq!(rig.send(with_unknown_token(&token)).await.status(), StatusCode::UNAUTHORIZED);
    let lines = recorder.lines(UNAUTHORIZED_LINE);
    let (level, fields) = lines.last().unwrap();
    assert_eq!(*level, tracing::Level::WARN, "the first refusal after the interval is written at warn");
    assert!(fields.contains("suppressed=99"), "and says how many were suppressed: {fields}");
}

#[tokio::test(start_paused = true)]
async fn a_refused_body_warns_on_its_own_limiter_and_does_not_wait_for_token_refusals() {
    let rig = Rig::new();
    let recorder = rig.recorder.clone();
    let token = rig.token();

    for _ in 0..5 {
        assert_eq!(rig.send(with_unknown_token(&token)).await.status(), StatusCode::UNAUTHORIZED);
    }
    let oversized = post(&token, None, &initialize_of_len(BODY_BYTES + 1));
    assert_eq!(rig.send(oversized).await.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let bodies = recorder.lines(BODY_LINE);
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0].0,
        tracing::Level::WARN,
        "a refused body warns even while token refusals are being suppressed"
    );
    assert!(bodies[0].1.contains("suppressed=0"), "{}", bodies[0].1);
}
