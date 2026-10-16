//! A loopback `hyper` fake server for the provider tests (llm-api D10): it plays scripted replies, records every
//! request (headers included, for the secret checks) and notes when each connection closed, so a test can measure
//! how fast a cancelled turn hangs up. No network leaves the machine.

// Shared by several test crates, each of which uses a part.
#![allow(dead_code, clippy::unwrap_used)]

use std::collections::VecDeque;
use std::convert::Infallible;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::{Body, Frame};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use omuzaki_llm::runtime;
use omuzaki_llm_api::provider::{ProviderConfig, ProviderEnv, RuntimeHandle, SecretLabel, SecretStore, SecretValue};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// What the server saw of one request.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

/// How an event stream ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// A clean end of the body.
    Close,
    /// The connection breaks mid-body (no chunk terminator).
    Abort,
    /// The body stays open until the client hangs up.
    Hold,
}

/// One scripted reply.
#[derive(Clone, Debug)]
pub enum Reply {
    /// A complete non-streaming answer, such as an error.
    Json { status: u16, headers: Vec<(&'static str, String)>, body: String },
    /// A `200` event stream.
    Sse { chunks: Vec<Vec<u8>>, end: End, pause: Duration, split: Option<usize>, first_byte_delay: Duration },
}

impl Reply {
    pub fn status(status: u16, body: &str) -> Self {
        Self::Json { status, headers: Vec::new(), body: body.to_owned() }
    }

    pub fn status_with(status: u16, headers: &[(&'static str, &str)], body: &str) -> Self {
        let headers = headers.iter().map(|(n, v)| (*n, (*v).to_owned())).collect();
        Self::Json { status, headers, body: body.to_owned() }
    }

    /// An event stream sent as one chunk per element of `chunks`, then closed.
    pub fn sse(chunks: Vec<Vec<u8>>) -> Self {
        Self::Sse { chunks, end: End::Close, pause: Duration::ZERO, split: None, first_byte_delay: Duration::ZERO }
    }

    /// An event stream of the text `wire`, closed at the end.
    pub fn sse_text(wire: &str) -> Self {
        Self::sse(vec![wire.as_bytes().to_vec()])
    }

    #[must_use]
    pub fn ending(mut self, how: End) -> Self {
        if let Self::Sse { end, .. } = &mut self {
            *end = how;
        }
        self
    }

    /// Sends the body in pieces of at most `size` bytes.
    #[must_use]
    pub fn split_every(mut self, size: usize) -> Self {
        if let Self::Sse { split, .. } = &mut self {
            *split = Some(size);
        }
        self
    }

    /// Waits `delay` between pieces.
    #[must_use]
    pub fn pausing(mut self, delay: Duration) -> Self {
        if let Self::Sse { pause, .. } = &mut self {
            *pause = delay;
        }
        self
    }

    /// Waits `delay` before the response head.
    #[must_use]
    pub fn after(mut self, delay: Duration) -> Self {
        if let Self::Sse { first_byte_delay, .. } = &mut self {
            *first_byte_delay = delay;
        }
        self
    }
}

#[derive(Default)]
struct State {
    replies: VecDeque<Reply>,
    /// Replies for one `"METHOD /path"` each; a request with no route and no general reply left gets a 404.
    routes: Vec<(String, VecDeque<Reply>)>,
    requests: Vec<Recorded>,
    /// When the body of request `i` was dropped, which is when the client hung up or the body finished.
    closed: Vec<Option<Instant>>,
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

struct ChanBody {
    rx: mpsc::UnboundedReceiver<Result<Bytes, io::Error>>,
    state: Shared,
    index: usize,
}

impl Body for ChanBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        self.rx.poll_recv(cx).map(|item| item.map(|r| r.map(Frame::data)))
    }
}

impl Drop for ChanBody {
    fn drop(&mut self) {
        if let Some(slot) = lock(&self.state).closed.get_mut(self.index) {
            slot.get_or_insert_with(Instant::now);
        }
    }
}

/// A running fake server on `127.0.0.1`.
pub struct Server {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    state: Shared,
    // Keeps the server's threads alive for the test.
    _runtime: tokio::runtime::Runtime,
}

impl Server {
    /// Starts a server that answers requests with `replies`, in order.
    pub fn start(replies: Vec<Reply>) -> Self {
        Self::with(replies, Vec::new())
    }

    /// Starts a server that answers each `"METHOD /path"` from its own list of replies, and any other request with a 404.
    pub fn routed(routes: Vec<(&str, Vec<Reply>)>) -> Self {
        Self::with(Vec::new(), routes.into_iter().map(|(k, r)| (k.to_owned(), r.into())).collect())
    }

    /// As [`routed`](Self::routed), on `127.0.0.1:port` (a port a dropped server has just left, for the tests that
    /// need two different servers at one address).
    pub fn routed_on(port: u16, routes: Vec<(&str, Vec<Reply>)>) -> Self {
        Self::bound(
            &format!("127.0.0.1:{port}"),
            Vec::new(),
            routes.into_iter().map(|(k, r)| (k.to_owned(), r.into())).collect(),
        )
    }

    fn with(replies: Vec<Reply>, routes: Vec<(String, VecDeque<Reply>)>) -> Self {
        Self::bound("127.0.0.1:0", replies, routes)
    }

    fn bound(addr: &str, replies: Vec<Reply>, routes: Vec<(String, VecDeque<Reply>)>) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let state: Shared = Arc::new(Mutex::new(State { replies: replies.into(), routes, ..State::default() }));
        let listener = runtime.block_on(TcpListener::bind(addr)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let shared = Arc::clone(&state);
        runtime.spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { return };
                let state = Arc::clone(&shared);
                tokio::spawn(async move {
                    let service = service_fn(move |request| handle(request, Arc::clone(&state)));
                    // A client that hangs up mid-answer is the point of some tests; the error is not news.
                    let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
                });
            }
        });
        Self { url, state, _runtime: runtime }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        lock(&self.state).requests.clone()
    }

    /// When the connection of request `index` closed, if it has.
    pub fn closed_at(&self, index: usize) -> Option<Instant> {
        lock(&self.state).closed.get(index).copied().flatten()
    }

    /// Replies not yet used.
    pub fn unused(&self) -> usize {
        let state = lock(&self.state);
        state.replies.len() + state.routes.iter().map(|(_, q)| q.len()).sum::<usize>()
    }

    /// How many requests `"METHOD /path"` has had.
    pub fn count(&self, route: &str) -> usize {
        lock(&self.state)
            .requests
            .iter()
            .filter(|r| format!("{} {}", r.method, r.path.split('?').next().unwrap_or_default()) == route)
            .count()
    }
}

async fn handle(request: Request<hyper::body::Incoming>, state: Shared) -> Result<Response<ChanBody>, Infallible> {
    let (parts, body) = request.into_parts();
    let body = body.collect().await.map(|b| b.to_bytes().to_vec()).unwrap_or_default();
    let headers = parts
        .headers
        .iter()
        .map(|(n, v)| (n.as_str().to_owned(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    let (reply, index) = {
        let mut guard = lock(&state);
        guard.requests.push(Recorded {
            method: parts.method.to_string(),
            path: parts.uri.path_and_query().map(ToString::to_string).unwrap_or_default(),
            headers,
            body,
        });
        guard.closed.push(None);
        let key = format!("{} {}", parts.method, parts.uri.path());
        let routed = guard.routes.iter_mut().find(|(k, _)| *k == key).map(|(_, queue)| queue.pop_front());
        let reply = match routed {
            Some(reply) => reply,
            None if guard.routes.is_empty() => guard.replies.pop_front(),
            None => Some(Reply::status(404, r#"{"error":{"message":"no such route"}}"#)),
        };
        (reply, guard.requests.len() - 1)
    };
    let reply = reply.unwrap_or_else(|| Reply::status(500, r#"{"error":{"message":"no reply scripted"}}"#));
    let (tx, rx) = mpsc::unbounded_channel();
    let (status, content_type, delay) = match &reply {
        Reply::Json { status, .. } => (*status, "application/json", Duration::ZERO),
        Reply::Sse { first_byte_delay, .. } => (200, "text/event-stream", *first_byte_delay),
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let mut response = Response::builder().status(status).header("content-type", content_type);
    if let Reply::Json { headers, .. } = &reply {
        for (name, value) in headers {
            response = response.header(*name, value.as_str());
        }
    }
    tokio::spawn(feed(reply, tx));
    Ok(response.body(ChanBody { rx, state, index }).unwrap())
}

async fn feed(reply: Reply, tx: mpsc::UnboundedSender<Result<Bytes, io::Error>>) {
    match reply {
        Reply::Json { body, .. } => {
            let _ = tx.send(Ok(Bytes::from(body)));
        }
        Reply::Sse { chunks, end, pause, split, .. } => {
            for chunk in chunks {
                let pieces: Vec<Vec<u8>> = match split {
                    Some(size) => chunk.chunks(size.max(1)).map(<[u8]>::to_vec).collect(),
                    None => vec![chunk],
                };
                for piece in pieces {
                    if tx.send(Ok(Bytes::from(piece))).is_err() {
                        return;
                    }
                    if !pause.is_zero() {
                        tokio::time::sleep(pause).await;
                    } else if split.is_some() {
                        tokio::task::yield_now().await;
                    }
                }
            }
            match end {
                End::Close => {}
                End::Abort => {
                    // Let the chunks written so far reach the client before the connection breaks.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let _ = tx.send(Err(io::Error::new(io::ErrorKind::ConnectionReset, "scripted drop")));
                    // Hold the sender until the error has been read, or hyper may see a clean end instead.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                End::Hold => tx.closed().await,
            }
        }
    }
}

/// A secret store with one fixed key.
pub struct OneKey(pub SecretValue);

impl SecretStore for OneKey {
    fn get(&self, _label: &SecretLabel) -> Option<SecretValue> {
        Some(self.0.clone())
    }
}

/// An empty secret store.
pub struct NoKeys;

impl SecretStore for NoKeys {
    fn get(&self, _label: &SecretLabel) -> Option<SecretValue> {
        None
    }
}

/// The key every test uses; it must appear in no log, error, `Debug` output or golden file.
pub const KEY: &str = "sk-test-0123456789-secret";

pub fn runtime_handle() -> RuntimeHandle {
    runtime::dedicated().unwrap()
}

pub fn env(runtime: &RuntimeHandle) -> ProviderEnv {
    ProviderEnv::new(Arc::new(OneKey(SecretValue::new(KEY))), runtime.clone())
}

pub fn env_without_keys(runtime: &RuntimeHandle) -> ProviderEnv {
    ProviderEnv::new(Arc::new(NoKeys), runtime.clone())
}

pub fn key_ref() -> omuzaki_llm_api::provider::SecretRef {
    omuzaki_llm_api::provider::SecretRef(SecretLabel("connector/llm/test".into()))
}

/// A configuration for `kind` at `server`, with the test key and `model`.
pub fn config(kind: &str, server: &Server, model: &str) -> ProviderConfig {
    ProviderConfig::new("test", kind, server.url.clone()).with_key(key_ref()).with_model(model)
}

/// Compares `text` with the golden file at `tests/goldens/<name>`; blesses it when `UPDATE_GOLDENS` is set.
pub fn golden(name: &str, text: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens").join(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(want == text, "{name} differs from its golden; bless with UPDATE_GOLDENS=1\n{text}");
}

/// A fixture file's bytes, `tests/fixtures/<name>`.
pub fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// How long a test waits for the next event before it calls the stream stuck.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// Pulls a stream to its end: every event, and the error that ended it, if any.
pub fn drain(
    stream: &mut omuzaki_llm_api::stream::TurnStream,
) -> (Vec<omuzaki_llm_api::stream::LlmEvent>, Option<omuzaki_llm_api::error::LlmError>) {
    use omuzaki_llm_api::stream::Next;
    let mut events = Vec::new();
    loop {
        match stream.next_timeout(PATIENCE) {
            Ok(Next::Event(e)) => events.push(e),
            Ok(Next::Wait) => panic!("the stream went quiet after {events:?}"),
            Ok(Next::End) => return (events, None),
            Ok(other) => panic!("unexpected {other:?}"),
            Err(e) => return (events, Some(e)),
        }
    }
}

/// One user message of text.
pub fn user(text: &str) -> Vec<omuzaki_llm_api::message::Message> {
    use omuzaki_llm_api::message::{ContentBlock, Message, Role};
    vec![Message::new(Role::User, vec![ContentBlock::text(text)])]
}

/// How many `Retrying` events `events` holds.
pub fn retries(events: &[omuzaki_llm_api::stream::LlmEvent]) -> usize {
    events.iter().filter(|e| matches!(e, omuzaki_llm_api::stream::LlmEvent::Retrying { .. })).count()
}

/// The golden form of a turn: the request as the server saw it (no headers), then each event, one JSON per line.
pub fn transcript(request: &Recorded, events: &[omuzaki_llm_api::stream::LlmEvent]) -> String {
    let mut out =
        serde_json::json!({"request": {"method": request.method, "path": request.path, "body": request.json()}})
            .to_string();
    out.push('\n');
    for event in events {
        out.push_str(&serde_json::to_string(event).unwrap());
        out.push('\n');
    }
    out
}
