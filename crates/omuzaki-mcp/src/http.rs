//! The Streamable HTTP transport: a loopback port other apps connect to, with a separate access token (and so separate
//! grants) for each client. See the crate docs for the security model.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use omuzaki_ops_api::{Actor, Grants};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::access::{AccessToken, Registry, TokenId, TokenRecord};
use crate::error::ServerError;
use crate::gate::Gate;
use crate::limits::Limits;
use crate::server::McpServer;

/// How long open connections get to finish when the server shuts down.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// Pause after a failed `accept` (out of file descriptors, say) so a persistent error can't spin the loop.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// How the HTTP port is set up.
///
/// The defaults are the safe ones: loopback only, every request needs a token, browsers are refused, and a client
/// can't hold more than a bounded amount of the port's memory, sockets or time. Every limit below is a field with a
/// `with_*` setter; a request that hits one is refused with a message naming it and its value. A request body is
/// held here and again while rmcp parses it, and a buffer grows by doubling, so allow about `4 x max_body_bytes` for
/// each request being read, and at most `max_connections` of those at once.
///
/// Every timeout may be at most [`HttpOptions::MAX_TIMEOUT`]; [`McpServer::serve_http`] refuses longer ones.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HttpOptions {
    /// Where to listen. Must be a loopback address: remote access needs OAuth, which isn't built yet. Defaults to
    /// `127.0.0.1:7437` (a fixed port so a client's configuration survives restarts); use port 0 for any free port.
    pub bind: SocketAddr,
    /// The URL path the MCP endpoint lives at. Defaults to `/mcp`.
    pub path: String,
    /// Browser origins (`scheme://host:port`, RFC 6454) allowed to call the port. Empty by default: any request that
    /// carries an `Origin` header is refused, which is what stops a web page from driving the studio. Native clients
    /// send no `Origin`, so they are unaffected. Listing an origin lets requests from it past this check only; they
    /// still need a token, and the port sends no CORS headers, so a page can't read the answers anyway.
    pub allowed_origins: Vec<String>,
    /// A session with no traffic for this long is closed, its open transaction rolled back, and its place under
    /// `max_sessions_per_token` freed. Defaults to 30 minutes: a crashed client never says goodbye, and its
    /// half-finished edits must not stay pending forever. At most [`HttpOptions::MAX_TIMEOUT`].
    pub idle_timeout: Duration,
    /// How many connections may be open at once. Defaults to 64.
    pub max_connections: usize,
    /// How long a connection may take to send its request headers before it is dropped, so sockets that never say
    /// anything can't use up `max_connections`. Defaults to 10 seconds. At most [`HttpOptions::MAX_TIMEOUT`].
    pub header_timeout: Duration,
    /// How many sessions one token may have open at once. Defaults to 32: far more than the agents of one client
    /// need, and few enough that a client that opens a session per call (or loops on reconnect) is stopped at about
    /// 1.6 MB (a session costs roughly 50 KB) instead of leaking until `idle_timeout`. At the limit a new
    /// `initialize` is refused with `429 Too Many Requests`; sessions already open are never closed to make room,
    /// and a slot frees when a session is ended (`DELETE`), goes idle, or its token is revoked. Must not be zero.
    pub max_sessions_per_token: usize,
    /// The largest request body accepted, in bytes; a bigger one gets `413 Payload Too Large` (without being read
    /// if it declares its size). Defaults to 4 MiB, which is also rmcp's own limit. Must not be zero.
    pub max_body_bytes: usize,
    /// How long a request body may take to arrive in full, counted from when the port starts reading it (after the
    /// headers and the token check). A body still incomplete then gets `408 Request Timeout` and the connection is
    /// closed. It is a deadline for the whole body, not a gap between chunks, so a client can't hold a connection
    /// by dripping bytes. Defaults to 30 seconds. Must not be zero, and at most [`HttpOptions::MAX_TIMEOUT`].
    pub body_timeout: Duration,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 7437)),
            path: "/mcp".to_owned(),
            allowed_origins: Vec::new(),
            idle_timeout: Duration::from_mins(30),
            max_connections: 64,
            header_timeout: Duration::from_secs(10),
            max_sessions_per_token: 32,
            max_body_bytes: 4 * 1024 * 1024,
            body_timeout: Duration::from_secs(30),
        }
    }
}

impl HttpOptions {
    /// The longest any timeout (`idle_timeout`, `header_timeout`, `body_timeout`) may be: 24 hours. Deadlines are
    /// built by adding a timeout to the current time, which overflows (and panics) for a huge one such as
    /// `Duration::MAX`, the obvious spelling of "no limit". A day is longer than any client should hold a
    /// connection or a session idle, and a port that really should not time sessions out can still use it.
    pub const MAX_TIMEOUT: Duration = Duration::from_hours(24);

    /// Listens on `bind` (a loopback address).
    #[must_use]
    pub fn with_bind(mut self, bind: SocketAddr) -> Self {
        self.bind = bind;
        self
    }

    /// Serves MCP at `path` (it must start with `/`).
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// Lets requests carrying this `Origin` past the browser check (see [`HttpOptions::allowed_origins`]).
    #[must_use]
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.allowed_origins.push(origin.into());
        self
    }

    /// Closes sessions that have been quiet for `idle_timeout`.
    #[must_use]
    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }

    /// Allows `max_connections` open connections at once.
    #[must_use]
    pub fn with_max_connections(mut self, max_connections: usize) -> Self {
        self.max_connections = max_connections;
        self
    }

    /// Drops connections that take longer than `header_timeout` to send their request headers.
    #[must_use]
    pub fn with_header_timeout(mut self, header_timeout: Duration) -> Self {
        self.header_timeout = header_timeout;
        self
    }

    /// Lets one token have `max_sessions_per_token` sessions open at once.
    #[must_use]
    pub fn with_max_sessions_per_token(mut self, max_sessions_per_token: usize) -> Self {
        self.max_sessions_per_token = max_sessions_per_token;
        self
    }

    /// Accepts request bodies of up to `max_body_bytes`.
    #[must_use]
    pub fn with_max_body_bytes(mut self, max_body_bytes: usize) -> Self {
        self.max_body_bytes = max_body_bytes;
        self
    }

    /// Gives a request body `body_timeout` to arrive in full.
    #[must_use]
    pub fn with_body_timeout(mut self, body_timeout: Duration) -> Self {
        self.body_timeout = body_timeout;
        self
    }

    fn check(&self) -> Result<(), ServerError> {
        // A zero here would not be a strict limit but a port that refuses every client, so it is an error to say so.
        let zero = |name: &'static str, value: String| {
            Err(ServerError::BadLimit { name, value, rule: "must not be zero".to_owned() })
        };
        if self.max_sessions_per_token == 0 {
            return zero("max_sessions_per_token", "0".to_owned());
        }
        if self.max_body_bytes == 0 {
            return zero("max_body_bytes", "0".to_owned());
        }
        if self.body_timeout.is_zero() {
            return zero("body_timeout", format!("{:?}", self.body_timeout));
        }
        let timeouts = [
            ("idle_timeout", self.idle_timeout),
            ("header_timeout", self.header_timeout),
            ("body_timeout", self.body_timeout),
        ];
        for (name, timeout) in timeouts {
            if timeout > Self::MAX_TIMEOUT {
                let rule = format!("must be at most {:?}", Self::MAX_TIMEOUT);
                return Err(ServerError::BadLimit { name, value: format!("{timeout:?}"), rule });
            }
        }
        if !self.bind.ip().is_loopback() {
            return Err(ServerError::NotLoopback(self.bind));
        }
        if !self.path.starts_with('/') || self.path.contains(['?', '#']) {
            return Err(ServerError::BadPath(self.path.clone()));
        }
        Ok(())
    }
}

/// A running HTTP port. Nobody can use it until the host issues a token with [`HttpServer::issue`].
///
/// Dropping it stops listening; [`HttpServer::shutdown`] also ends the sessions and waits for the port to close.
pub struct HttpServer {
    addr: SocketAddr,
    path: String,
    registry: Arc<Registry>,
    accept: JoinHandle<()>,
    /// Stops the listener and the open connections when dropped, so a server that is dropped without `shutdown` still
    /// closes. Its sessions then end as the connections and the registry are dropped.
    stop: DropGuard,
}

impl McpServer {
    /// Serves MCP over Streamable HTTP on a local port, until the returned [`HttpServer`] is shut down or dropped.
    ///
    /// The port starts with no valid credentials, so every request is refused until the host (the studio, after the
    /// user approves a client) calls [`HttpServer::issue`] and gives the client its token. Each token carries its own
    /// grants, which can differ from this server's own.
    ///
    /// # Errors
    /// [`ServerError::NotLoopback`], [`ServerError::BadPath`] or [`ServerError::BadLimit`] (a zero limit, or a timeout
    /// over [`HttpOptions::MAX_TIMEOUT`]) for options that aren't allowed, and [`ServerError::Bind`] if the port can't
    /// be opened.
    pub async fn serve_http(&self, options: HttpOptions) -> Result<HttpServer, ServerError> {
        options.check()?;
        let listener =
            TcpListener::bind(options.bind).await.map_err(|source| ServerError::Bind(options.bind, source))?;
        let addr = listener.local_addr().map_err(|source| ServerError::Bind(options.bind, source))?;
        let cancel = CancellationToken::new();
        let limits = Limits {
            idle_timeout: options.idle_timeout,
            max_sessions_per_token: options.max_sessions_per_token,
            max_body_bytes: options.max_body_bytes,
            body_timeout: options.body_timeout,
        };
        let registry = Arc::new(Registry::new(self.clone(), options.allowed_origins, limits, cancel.clone()));
        let gate = Arc::new(Gate::new(Arc::clone(&registry), options.path.clone()));
        let connections = Arc::new(Semaphore::new(options.max_connections.clamp(1, Semaphore::MAX_PERMITS)));
        let acceptor =
            Acceptor { listener, gate, connections, header_timeout: options.header_timeout, cancel: cancel.clone() };
        let accept = tokio::spawn(acceptor.run());
        tracing::info!(
            %addr, path = %options.path, ?limits, max_connections = options.max_connections,
            header_timeout = ?options.header_timeout, "MCP HTTP port open (loopback only; tokens required)",
        );
        Ok(HttpServer { addr, path: options.path, registry, accept, stop: cancel.drop_guard() })
    }
}

impl HttpServer {
    /// The address the port is listening on (useful after binding port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL clients connect to, e.g. `http://127.0.0.1:7437/mcp`.
    pub fn url(&self) -> String {
        format!("http://{}{}", self.addr, self.path)
    }

    /// Creates a token that lets one client in with exactly `grants`. `label` says whose it is (it appears in logs).
    ///
    /// Pass the grants the user approved for this client, e.g. [`Grants::agent_default`] for a coding agent. A client
    /// never gets more than its token's grants, and `broadcast`, `publish` and `spend` appear only if listed here.
    ///
    /// # Errors
    /// [`ServerError::Entropy`] if the OS can't provide random bytes, and [`ServerError::UnusableOp`] or
    /// [`ServerError::DuplicateTool`] if the runner's ops changed into something that can't be tools.
    pub fn issue(&self, label: impl Into<String>, grants: Grants) -> Result<AccessToken, ServerError> {
        let token = self.registry.issue(label.into(), grants)?;
        tracing::info!(token = %token.id(), label = token.label(), "access token issued");
        Ok(token)
    }

    /// Like [`issue`](Self::issue), recording who issued the token: the person (or agent) whose principal the sessions
    /// opened with it act under. [`issue`](Self::issue) records [`Actor::System`].
    ///
    /// # Errors
    /// As [`issue`](Self::issue).
    pub fn issue_for(
        &self,
        label: impl Into<String>,
        grants: Grants,
        issuer: Actor,
    ) -> Result<AccessToken, ServerError> {
        let token = self.registry.issue_for(label.into(), grants, issuer)?;
        tracing::info!(token = %token.id(), label = token.label(), "access token issued");
        Ok(token)
    }

    /// Registers a stored token (from [`AccessToken::record`]) so the client's existing secret works again, without
    /// the secret ever passing through here. The token gets a new [`TokenId`]: ids last for one run of the server, so
    /// the host keeps its own key for the stored record. Open sessions don't carry over a restart; clients reconnect.
    ///
    /// # Errors
    /// [`ServerError::TokenRevoked`] for a tombstone, and [`ServerError::BadTokenRecord`] for a record in a newer
    /// format than this build reads, a malformed hash, or a secret that is already admitted.
    pub fn admit(&self, record: TokenRecord) -> Result<TokenId, ServerError> {
        let label = record.label.clone();
        let id = self.registry.admit(record)?;
        tracing::info!(token = %id, %label, "access token re-admitted");
        Ok(id)
    }

    /// The record of a live token as it is now (grants and pairing included), for the host to store. `None` if the
    /// token is unknown or revoked.
    pub fn record(&self, id: TokenId) -> Option<TokenRecord> {
        self.registry.record(id)
    }

    /// Marks the token as paired in the studio window (or not). False if the token is unknown or revoked. Store the
    /// new [`record`](Self::record) to keep it across restarts.
    pub fn set_paired(&self, id: TokenId, paired: bool) -> bool {
        self.registry.set_paired(id, paired)
    }

    /// Replaces the token's grants, e.g. after the user chooses "grant for this project". The token's open sessions
    /// end (they were opened under the old grants); sessions opened from now on hold the new ones. False if the token
    /// is unknown or revoked. Store the new [`record`](Self::record) to keep the change across restarts.
    ///
    /// # Errors
    /// As [`issue`](Self::issue), for the unusable-ops cases.
    pub async fn set_grants(&self, id: TokenId, grants: Grants) -> Result<bool, ServerError> {
        self.registry.set_grants(id, grants).await
    }

    /// Like [`revoke`](Self::revoke), and returns the tombstone to store if the host keeps revoked records (see
    /// [`TokenRecord`]); a host that deletes the token's record on revoke can ignore it.
    pub async fn revoke_record(&self, id: TokenId) -> Option<TokenRecord> {
        self.registry.revoke_record(id).await
    }

    /// Stops accepting the token and ends its sessions (their open transactions are rolled back). Returns false if the
    /// token was already revoked.
    pub async fn revoke(&self, id: TokenId) -> bool {
        self.registry.revoke(id).await
    }

    /// Closes the port, ends every session and waits for the listener to stop.
    pub async fn shutdown(self) {
        let Self { registry, accept, stop, .. } = self;
        drop(stop); // cancels the listener and the open connections
        registry.close_all().await;
        if let Err(error) = accept.await {
            tracing::warn!(%error, "the MCP HTTP accept loop ended abnormally");
        }
    }
}

/// Accepts connections until cancelled, serving each on its own task.
struct Acceptor {
    listener: TcpListener,
    gate: Arc<Gate>,
    connections: Arc<Semaphore>,
    header_timeout: Duration,
    cancel: CancellationToken,
}

impl Acceptor {
    async fn run(self) {
        loop {
            // The permit is taken before accepting, so at the limit new connections wait in the kernel's backlog
            // instead of becoming tasks.
            let permit = tokio::select! {
                () = self.cancel.cancelled() => return,
                permit = Arc::clone(&self.connections).acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(_) => return,
                },
            };
            let stream = tokio::select! {
                () = self.cancel.cancelled() => return,
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        tracing::warn!(%error, "couldn't accept an MCP HTTP connection");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                        continue;
                    }
                },
            };
            let (gate, cancel, header_timeout) =
                (Arc::clone(&self.gate), self.cancel.child_token(), self.header_timeout);
            tokio::spawn(async move {
                serve_connection(stream, gate, header_timeout, cancel).await;
                drop(permit);
            });
        }
    }
}

async fn serve_connection(stream: TcpStream, gate: Arc<Gate>, header_timeout: Duration, cancel: CancellationToken) {
    let service = service_fn(move |request| {
        let gate = Arc::clone(&gate);
        async move { Ok::<_, Infallible>(gate.handle(request).await) }
    });
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        // hyper adds this to `std::time::Instant::now()`, which panics on overflow, so a huge value never reaches it
        // (`HttpOptions::check` refuses one; this keeps the guarantee local).
        .header_read_timeout(header_timeout.min(HttpOptions::MAX_TIMEOUT))
        .serve_connection(TokioIo::new(stream), service);
    tokio::pin!(connection);
    tokio::select! {
        outcome = connection.as_mut() => {
            if let Err(error) = outcome {
                tracing::debug!(%error, "MCP HTTP connection ended with an error");
            }
        }
        () = cancel.cancelled() => {
            connection.as_mut().graceful_shutdown();
            // Open event streams end when the server shuts down; a client that still holds the socket open is cut off.
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, connection).await;
        }
    }
}
