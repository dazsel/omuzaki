//! The server: built once from a runner, then one [`Session`] per client connection.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_history_api::JournalSink;
use omuzaki_ops_api::{Grants, OpInfo, Runner};
use rmcp::ServiceExt;
use rmcp::model::Tool;
use rmcp::service::RoleServer;
use rmcp::transport::IntoTransport;
use serde_json::Value;

use crate::catalog::Catalog;
use crate::connection::{self, Connections};
use crate::describe::{Guidance, instructions};
use crate::error::ServerError;
use crate::events::EventHub;
use crate::prompts::{PromptDef, Prompts};
use crate::resources::{Providers, ResourceProvider};
use crate::session::Session;

/// The host's journal, wrapped so that [`McpOptions`] can stay `Debug`.
#[derive(Clone)]
struct Journal(Arc<dyn JournalSink>);

impl fmt::Debug for Journal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JournalSink")
    }
}

/// How a server is set up.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct McpOptions {
    /// What agents connected to this server may do (spec §6). Defaults to [`Grants::agent_default`]: read, edit and
    /// render. `broadcast`, `publish` and `spend` need an explicit per-session grant.
    pub grants: Grants,
    /// Resource families the host adds to the built-in ones (see [`ResourceProvider`]).
    providers: Vec<Arc<dyn ResourceProvider>>,
    /// Prompts the host adds to the built-in ones (see [`PromptDef`]).
    prompts: Vec<PromptDef>,
    /// The connection list the host wired into its runner's services, if it did.
    connections: Option<Connections>,
    /// The journal the host runs, if it has one: a reply to a change waits for it (see [`McpOptions::with_journal`]).
    journal: Option<Journal>,
}

impl Default for McpOptions {
    fn default() -> Self {
        Self::with_grants(Grants::agent_default())
    }
}

impl McpOptions {
    /// Options with these grants.
    #[must_use]
    pub fn with_grants(grants: Grants) -> Self {
        Self { grants, providers: Vec::new(), prompts: Vec::new(), connections: None, journal: None }
    }

    /// Also serves the resources of `provider`: it is listed, templated and readable alongside the built-in ones.
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<dyn ResourceProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    /// Uses `journal` for the durability of changes: after a call applies an edit, commits or rolls back a transaction,
    /// or undoes or redoes, the reply waits until the journal has saved it (up to five seconds). A save that times out
    /// or fails still succeeds, since the change did apply: the reply's text opens with a `Warning` line saying so, with
    /// the journal's status, and `structuredContent.warning` carries the same text. Queries and previews never wait.
    /// Without a journal, replies don't wait.
    #[must_use]
    pub fn with_journal(mut self, journal: Arc<dyn JournalSink>) -> Self {
        self.journal = Some(Journal(journal));
        self
    }

    /// Serves `connections` as the sessions `agent.connection` reports, and lists that op. The host must have inserted
    /// the same handle into its runner's services as a [`ConnectionService`](omuzaki_ops_api::ConnectionService):
    /// the op reads it from there. Without this the server neither lists nor routes `agent.connection`, since a call
    /// could only fail as unavailable.
    #[must_use]
    pub fn with_connections(mut self, connections: Connections) -> Self {
        self.connections = Some(connections);
        self
    }

    /// Also offers `prompt` (a recipe) to agents whose session has every op in its [`needs`](PromptDef::needs). A
    /// prompt with the name of one already added, built-in or the host's, replaces it.
    #[must_use]
    pub fn with_prompt(mut self, prompt: PromptDef) -> Self {
        self.prompts.push(prompt);
        self
    }
}

/// State every session shares, whatever its grants.
pub(crate) struct Shared {
    pub runner: Arc<dyn Runner>,
    pub guidance: Guidance,
    pub hub: Arc<EventHub>,
    pub providers: Providers,
    pub prompts: Prompts,
    pub connections: Connections,
    /// The host wired the connection service, so `agent.connection` is offered.
    pub connection_wired: bool,
    /// The host's journal, which replies to changes wait for. `None` means replies don't wait.
    pub journal: Option<Arc<dyn JournalSink>>,
    next_session: AtomicU64,
}

/// What one set of grants sees: the grants, the tool list (whose descriptions say which scopes are held) and the guide.
///
/// Built once per grant set, not per session: the stdio server has one, and each HTTP access token has its own.
pub(crate) struct View {
    pub grants: Grants,
    pub catalog: Catalog,
    pub instructions: String,
}

impl View {
    fn build(ops: &[OpInfo], grants: Grants, guidance: &Guidance) -> Result<Self, ServerError> {
        let catalog = Catalog::build(ops, &grants, guidance)?;
        let instructions = instructions(&grants, guidance);
        Ok(Self { grants, catalog, instructions })
    }
}

/// The runner's ops that this server can serve: all of them, except `agent.connection` unless its service is wired.
fn offered(runner: &dyn Runner, connection_wired: bool) -> Vec<OpInfo> {
    let mut ops = runner.ops();
    if !connection_wired {
        ops.retain(|op| op.name != connection::OP);
    }
    ops
}

/// An MCP server exposing every op of a [`Runner`] as a tool.
///
/// Cheap to clone: clones share the runner and event feed. Each connection gets its own [`Session`] (its own agent
/// identity and open transaction), holding the grants of the server it came from; [`McpServer::with_grants`] makes a
/// server for other grants.
#[derive(Clone)]
pub struct McpServer {
    shared: Arc<Shared>,
    view: Arc<View>,
}

impl McpServer {
    /// Builds the server: reads `runner.ops()` once and generates the tool list from it.
    ///
    /// # Errors
    /// [`ServerError::UnusableOp`] or [`ServerError::DuplicateTool`] if an op can't be a tool, and
    /// [`ServerError::EventFeed`] if the event worker can't start.
    pub fn new(runner: Arc<dyn Runner>, options: McpOptions) -> Result<Self, ServerError> {
        let connection_wired = options.connections.is_some();
        let ops = offered(runner.as_ref(), connection_wired);
        let guidance = Guidance::new(ops.iter().map(|op| op.name));
        let view = View::build(&ops, options.grants, &guidance)?;
        let hub = EventHub::start(runner.as_ref(), &view.grants)?;
        let shared = Shared {
            runner,
            guidance,
            hub,
            providers: Providers::new(&options.providers),
            prompts: Prompts::new(&options.prompts),
            connections: options.connections.unwrap_or_default(),
            connection_wired,
            journal: options.journal.map(|journal| journal.0),
            next_session: AtomicU64::new(1),
        };
        Ok(Self { shared: Arc::new(shared), view: Arc::new(view) })
    }

    /// Resolves once the event log holds event `n` (or a later one), at once if it already does. Await it before
    /// reading `omuzaki://log` for an op's event: the feed fills the log from a worker thread, so the log can lag the op
    /// that caused the event, and polling the log on a timer would race that thread.
    pub async fn wait_for_log(&self, n: u64) {
        self.shared.hub.wait_for(n).await;
    }

    /// A server over the same runner and event feed whose sessions hold `grants` instead.
    ///
    /// Its tool descriptions and instructions are rebuilt for those grants, so an agent is told what *it* may do. This
    /// is how the HTTP transport gives every access token its own scopes.
    ///
    /// # Errors
    /// As [`McpServer::new`]; an op list that built once builds again, so this only fails if the runner's ops changed.
    pub fn with_grants(&self, grants: Grants) -> Result<Self, ServerError> {
        let view = View::build(
            &offered(self.shared.runner.as_ref(), self.shared.connection_wired),
            grants,
            &self.shared.guidance,
        )?;
        Ok(Self { shared: Arc::clone(&self.shared), view: Arc::new(view) })
    }

    /// The tools this server lists, sorted by name: one per op, plus the built-ins.
    pub fn tools(&self) -> Vec<Tool> {
        self.view.catalog.tools().cloned().collect()
    }

    /// The grants this server's sessions hold.
    pub fn grants(&self) -> &Grants {
        &self.view.grants
    }

    /// The `agent.connection` probe: `{connected, client_name?, sessions}` for the sessions this server holds now.
    /// `client_name` is the newest client that has completed its handshake; `sessions` counts every live session,
    /// named or not. Holds no credential, so it is safe to show the person at setup.
    pub fn connection(&self) -> Value {
        self.shared.connections.answer()
    }

    /// A handler for one client connection. Pass it to rmcp's `serve` with any transport.
    pub fn session(&self) -> Session {
        let serial = self.shared.next_session.fetch_add(1, Ordering::Relaxed);
        Session::new(Arc::clone(&self.shared), Arc::clone(&self.view), serial)
    }

    /// A handler for one HTTP session: as [`McpServer::session`], steering clients to the `initialize` lifecycle (see
    /// [`Session::for_http`]).
    pub(crate) fn http_session(&self) -> Session {
        self.session().for_http()
    }

    /// Serves one client over `transport` until it disconnects.
    ///
    /// # Errors
    /// [`ServerError::Handshake`] if the transport fails before the MCP handshake, [`ServerError::Session`] if the
    /// connection then ends abnormally.
    pub async fn serve<T, E, A>(&self, transport: T) -> Result<(), ServerError>
    where
        T: IntoTransport<RoleServer, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let running = self.session().serve(transport).await.map_err(|e| ServerError::Handshake(e.to_string()))?;
        running.waiting().await.map(drop).map_err(|e| ServerError::Session(e.to_string()))
    }

    /// Serves one client over this process's stdin and stdout until it disconnects: what `omuzaki mcp` runs.
    ///
    /// Stdout carries the protocol, so nothing else may write to it; send logs to stderr.
    ///
    /// # Errors
    /// As [`McpServer::serve`].
    pub async fn serve_stdio(&self) -> Result<(), ServerError> {
        self.serve(rmcp::transport::stdio()).await
    }
}
