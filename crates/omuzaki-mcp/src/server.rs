//! The server: built once from a runner, then one [`Session`] per client connection.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_ops_api::{Grants, OpInfo, Runner};
use rmcp::ServiceExt;
use rmcp::model::Tool;
use rmcp::service::RoleServer;
use rmcp::transport::IntoTransport;
use serde_json::Value;

use crate::catalog::Catalog;
use crate::connection::Connections;
use crate::describe::{Guidance, instructions};
use crate::error::ServerError;
use crate::events::EventHub;
use crate::prompts::{PromptDef, Prompts};
use crate::resources::{Providers, ResourceProvider};
use crate::session::Session;

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
}

impl Default for McpOptions {
    fn default() -> Self {
        Self { grants: Grants::agent_default(), providers: Vec::new(), prompts: Vec::new() }
    }
}

impl McpOptions {
    /// Options with these grants.
    #[must_use]
    pub fn with_grants(grants: Grants) -> Self {
        Self { grants, providers: Vec::new(), prompts: Vec::new() }
    }

    /// Also serves the resources of `provider`: it is listed, templated and readable alongside the built-in ones.
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<dyn ResourceProvider>) -> Self {
        self.providers.push(provider);
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
        let ops = runner.ops();
        let guidance = Guidance::new(ops.iter().map(|op| op.name));
        let view = View::build(&ops, options.grants, &guidance)?;
        let hub = EventHub::start(runner.as_ref(), &view.grants)?;
        let shared = Shared {
            runner,
            guidance,
            hub,
            providers: Providers::new(&options.providers),
            prompts: Prompts::new(&options.prompts),
            connections: Connections::default(),
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
        let view = View::build(&self.shared.runner.ops(), grants, &self.shared.guidance)?;
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
