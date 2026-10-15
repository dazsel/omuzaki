//! The HTTP side of the omuzaki-mcp tests, shared by every binary that talks to a real loopback port: a server with the fake
//! runner, plain requests a well-behaved MCP client never sends, and [`Frozen`], which keeps tokio's clock still so a test
//! moves time only by the amount it names.

use std::sync::Arc;
use std::sync::mpsc;

use omuzaki_mcp::{AccessToken, HttpOptions, HttpServer, McpOptions, McpServer};
use omuzaki_ops_api::Grants;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpStream;

use super::{FakeRunner, fake_runner};

/// reqwest is built without a crypto provider, so every client needs `ring` installed first. Once per process; an
/// already installed provider is fine.
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A server with the fake runner, listening on a free loopback port.
pub struct Port {
    /// The fake runner behind the port, for checking what the server did to it.
    pub runner: Arc<FakeRunner>,
    /// The MCP server the port serves; kept alive for as long as the port is.
    pub mcp: McpServer,
    /// The running HTTP port.
    pub server: HttpServer,
    http: reqwest::Client,
}

impl Port {
    /// A port with default limits on a free loopback address.
    pub async fn start() -> Self {
        Self::start_with(|options| options).await
    }

    /// A port whose options `configure` adjusts from the defaults, still on a free loopback address.
    pub async fn start_with(configure: impl FnOnce(HttpOptions) -> HttpOptions) -> Self {
        install_crypto_provider();
        let runner = Arc::new(fake_runner());
        let mcp = McpServer::new(runner.clone(), McpOptions::default()).expect("server builds");
        let options = configure(HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap()));
        let server = mcp.serve_http(options).await.expect("port opens");
        Self { runner, mcp, server, http: reqwest::Client::new() }
    }

    /// A token with the grants an agent gets by default.
    pub fn agent_token(&self) -> AccessToken {
        self.server.issue("test agent", Grants::agent_default()).expect("token")
    }

    /// Plain HTTP to the port, for what a well-behaved MCP client would never send.
    pub fn raw(&self) -> Raw {
        Raw { http: self.http.clone(), url: self.server.url() }
    }

    /// A raw connection to the port, for requests that are announced and then left unfinished.
    pub async fn socket(&self) -> TcpStream {
        TcpStream::connect(self.server.local_addr()).await.expect("the port accepts")
    }
}

/// Plain HTTP, for what a well-behaved MCP client would never send.
#[derive(Clone)]
pub struct Raw {
    /// The client the requests go out on.
    pub http: reqwest::Client,
    /// The port's MCP endpoint, `http://127.0.0.1:<port>/mcp`.
    pub url: String,
}

impl Raw {
    /// The body of an `initialize` request.
    pub fn initialize_body() -> Value {
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "raw", "version": "1"}}
        })
    }

    /// POSTs an MCP message with the headers the transport requires plus `headers`.
    pub async fn post(&self, headers: &[(&str, &str)], body: &Value) -> reqwest::Response {
        self.post_to(&self.url, headers, body).await
    }

    /// Like [`Raw::post`], but to `url` rather than the port's own endpoint, for requests that name another host.
    pub async fn post_to(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> reqwest::Response {
        self.send_to(url, headers, body.to_string()).await
    }

    /// POSTs `body` exactly as given, for bodies that are not valid JSON (or are padded to a size).
    pub async fn send(&self, headers: &[(&str, &str)], body: String) -> reqwest::Response {
        self.send_to(&self.url, headers, body).await
    }

    async fn send_to(&self, url: &str, headers: &[(&str, &str)], body: String) -> reqwest::Response {
        let mut request = self
            .http
            .post(url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .body(body);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.send().await.expect("the port answers")
    }

    /// POSTs `body` with `token`'s bearer header and, when given, the session id.
    pub async fn post_as(&self, token: &AccessToken, session: Option<&str>, body: String) -> reqwest::Response {
        let bearer = token.bearer_header();
        let mut headers = vec![("authorization", bearer.as_str())];
        if let Some(session) = session {
            headers.push(("mcp-session-id", session));
        }
        self.send(&headers, body).await
    }

    /// `initialize` with `token`, answered with the whole response for the caller to inspect.
    pub async fn initialize(&self, token: &AccessToken) -> reqwest::Response {
        self.post_as(token, None, Self::initialize_body().to_string()).await
    }

    /// `initialize` with `token`, returning the session id the server assigned.
    pub async fn open_session(&self, token: &AccessToken) -> String {
        let response = self.initialize(token).await;
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()["mcp-session-id"].to_str().unwrap().to_owned()
    }

    /// Ends `session` on behalf of `token`, returning the status the server answered with.
    pub async fn delete(&self, token: &AccessToken, session: &str) -> StatusCode {
        let bearer = token.bearer_header();
        let request = self.http.delete(&self.url).header("authorization", bearer).header("mcp-session-id", session);
        request.send().await.expect("the port answers").status()
    }

    /// A `tools/call` as a 2026-07-28 client sends it: no handshake and no session, everything in the one request.
    pub async fn stateless_call(&self, bearer: Option<&str>, tool: &str, arguments: &Value) -> reqwest::Response {
        let body = json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {
                "name": tool, "arguments": arguments,
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": {"name": "stateless", "version": "1"},
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        });
        let mut headers =
            vec![("mcp-protocol-version", "2026-07-28"), ("mcp-method", "tools/call"), ("mcp-name", tool)];
        if let Some(bearer) = bearer {
            headers.push(("authorization", bearer));
        }
        self.post(&headers, &body).await
    }
}

/// Keeps tokio's paused clock from jumping ahead while a test waits for real socket I/O.
///
/// With a paused clock tokio moves time forward by itself whenever the runtime is idle, and a runtime waiting for a
/// byte from the kernel looks idle, so a timeout could "fire" before the server had even read the request. While any
/// blocking task runs, tokio does not auto-advance (see `tokio::time::pause`), so this parks one for the test's
/// duration; time then moves only when the test calls `tokio::time::advance`.
pub struct Frozen {
    _release: mpsc::Sender<()>,
    _held: tokio::task::JoinHandle<()>,
}

impl Frozen {
    /// Freezes the clock until the returned guard is dropped at the end of the test.
    pub fn new() -> Self {
        let (release, held) = mpsc::channel::<()>();
        // Ends when `release` is dropped with the test.
        let held = tokio::task::spawn_blocking(move || {
            let _ = held.recv();
        });
        Self { _release: release, _held: held }
    }
}
