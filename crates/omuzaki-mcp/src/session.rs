//! One client connection: who the agent is, its open transaction, and how its calls reach the runner.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};

use omuzaki_ops_api::{Actor, Cause, CauseKind, Mode, OpError, OpKind, Request, Runner, Scope};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, DiscoverRequestMethod, DiscoverResult,
    GetPromptRequestParams, GetPromptResponse, Implementation, JsonObject, ListPromptsResult,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ResourceTemplate,
    ResourceUpdatedNotificationParam, ServerCapabilities, ServerConfig, SubscribeRequestParams, SubscriptionFilter,
    Tool, UnsubscribeRequestParams,
};
use rmcp::service::{RequestContext, RoleServer, SubscriptionContext};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::builtin::{Builtin, CAUSE_MAX_CHARS};
use crate::catalog::{Entry, Route};
use crate::changes::{OwnTxs, Report};
use crate::events::{LOG_URI, Updates, lock};
use crate::render::{self, Call, OpenTx};
use crate::resources::protocol_error;
use crate::schema::{DRY_RUN, check_args};
use crate::server::{Shared, View};

/// The handler for one client connection (an rmcp [`ServerHandler`]).
///
/// It identifies the agent from the client's name in the handshake (`claude-code#1`), keeps the session's single open
/// transaction, and rolls that back if the connection ends before `tx_commit`.
pub struct Session {
    shared: Arc<Shared>,
    /// What this connection may do and the tool list that says so. Each HTTP access token has its own, so a session
    /// can never hold more than the token it came in with.
    view: Arc<View>,
    serial: u64,
    /// Served over HTTP, where a request that stands alone gets a fresh handler (see [`Session::for_http`]).
    http: bool,
    /// Fixed at the first call, so a transaction always belongs to one actor identity.
    actor: OnceLock<Actor>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    tx: Option<OpenTx>,
    /// A `tx_begin` is waiting for the runner. Claiming the slot before the call (not after) means a second
    /// `tx_begin`, even a racing one, is refused without ever reaching the runner: no phantom transaction.
    beginning: bool,
    /// The URIs this client subscribed to with `resources/subscribe` (the log, or any provider's resource). Shared with
    /// the feed task, which reads it on every wake.
    subscribed: Arc<Mutex<BTreeSet<String>>>,
    /// The one task pushing `resources/updated` for `subscribed`; running while that set is not empty.
    feed: Option<AbortOnDrop>,
    /// The newest `omuzaki://log` event number this client has been told about, in a result's heads-up or by reading
    /// the log. Starts at the log's position when the session is created, so a new session is caught up.
    seen: u64,
    /// The transactions this session made, from `tx_begin` and from each applied edit's result. Tells its own undo of
    /// an old transaction (whose ops have left the log) apart from someone else's.
    own: OwnTxs,
}

/// Stops its task when dropped (unsubscribe, or the session ending).
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Session {
    pub(crate) fn new(shared: Arc<Shared>, view: Arc<View>, serial: u64) -> Self {
        let state = State { seen: shared.hub.latest(), ..State::default() };
        Self { shared, view, serial, http: false, actor: OnceLock::new(), state: Mutex::new(state) }
    }

    /// Marks this handler as serving HTTP.
    ///
    /// From protocol 2026-07-28 a request stands alone (no `initialize`, no session), and rmcp's HTTP service gives
    /// each one a fresh handler. State that must outlive a request (an open transaction; the agent identity that
    /// `history_undo` is keyed on) would be lost between calls, so over HTTP this handler steers clients to the
    /// `initialize` lifecycle: `server/discover` is answered as a legacy server would (clients that probe then fall
    /// back to `initialize`), and a request that stands alone anyway is told why the stateful tools can't work for it.
    /// Over stdio the connection itself is the session, so every lifecycle works there.
    #[must_use]
    pub(crate) fn for_http(mut self) -> Self {
        self.http = true;
        self
    }

    /// Whether `uri` can be subscribed to: the log, or a resource some provider serves.
    fn subscribable(&self, uri: &str) -> bool {
        uri == LOG_URI || self.shared.providers.serving(uri).is_some()
    }

    fn actor(&self, context: &RequestContext<RoleServer>) -> &Actor {
        self.actor.get_or_init(|| {
            let client = context.client_info().map_or_else(|| "mcp-client".to_owned(), |info| info.name);
            Actor::Agent { id: format!("{client}#{}", self.serial), model: None }
        })
    }

    /// The op log and its notifications carry document values (patches, arguments), so reading or being notified of
    /// them needs `read`, exactly like running a query. Anything else that exposes document state to the client
    /// (`Runner::get`, resource providers) must call this first.
    fn require_read(&self, what: &str) -> Result<(), McpError> {
        if self.has_read() {
            return Ok(());
        }
        Err(McpError::invalid_request(read_needed(what), Some(json!({"code": "denied", "scope": "read"}))))
    }

    fn has_read(&self) -> bool {
        self.view.grants.allows(Scope::Read)
    }

    /// The op log as JSON (events numbered above `after`, or all of them), catching this session up with it: having
    /// read the log, the client knows everything in it, so the next result needn't repeat that.
    fn log_json(&self, after: Option<u64>) -> Result<String, serde_json::Error> {
        let (body, latest) = self.shared.hub.read(after)?;
        let mut state = lock(&self.state);
        state.seen = state.seen.max(latest);
        Ok(body)
    }

    /// `omuzaki://log`: the newest events, catching this session up with them.
    fn read_log(&self) -> Result<ReadResourceResponse, McpError> {
        self.require_read("reading the op log")?;
        let body = self
            .log_json(None)
            .map_err(|e| McpError::internal_error(format!("couldn't encode the op log: {e}"), None))?;
        let contents = ResourceContents::text(body, LOG_URI).with_mime_type("application/json");
        Ok(ReadResourceResult::new(vec![contents]).into())
    }

    /// `history_log`: the log resource's body, for clients that can't read resources. `after_n` keeps only the events
    /// numbered above it.
    fn history_log(&self, args: &JsonObject) -> Result<CallToolResult, OpError> {
        if !self.has_read() {
            return Err(OpError::denied(read_needed("reading the op log")));
        }
        let after = args.get("after_n").map(|n| {
            n.as_u64().ok_or_else(|| OpError::invalid("after_n", "`after_n` must be a whole number, 0 or more"))
        });
        let body = self
            .log_json(after.transpose()?)
            .map_err(|e| OpError::internal(format!("couldn't encode the op log: {e}")))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(body)]))
    }

    /// What other actors changed since this session was last told, if anything, moving its position forward so each
    /// change is reported once. Needs `read` for the same reason the log does: it names paths and ops.
    fn others_changed(&self, me: &Actor) -> Option<Report> {
        if !self.view.grants.allows(Scope::Read) {
            return None;
        }
        let mut state = lock(&self.state);
        let (report, latest) = self.shared.hub.changes_since(state.seen, me, &state.own);
        state.seen = latest;
        report
    }

    /// Runs blocking runner work off the async threads, turning a panic in an op into an `internal` error instead of
    /// a dead session.
    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&dyn Runner) -> T + Send + 'static,
    ) -> Result<T, OpError> {
        let runner = Arc::clone(&self.shared.runner);
        tokio::task::spawn_blocking(move || work(runner.as_ref())).await.map_err(|e| {
            OpError::internal(format!("the operation panicked or was cancelled: {e}"))
                .hint("this is a bug in Omuzaki: tell the user, and don't retry unchanged")
        })
    }

    /// Every call lands here and always yields a tool result: failures become tool errors, never protocol errors.
    async fn dispatch(&self, request: CallToolRequestParams, context: &RequestContext<RoleServer>) -> CallToolResult {
        let name = request.name.as_ref();
        let Some(entry) = self.view.catalog.get(name) else {
            return render::unknown_tool(name, &self.view.catalog.similar(name));
        };
        let actor = self.actor(context).clone();
        let mut args = request.arguments.unwrap_or_default();
        let outcome = match entry.route {
            Route::Op { kind, unit_args } => match take_dry_run(&mut args)
                .and_then(|dry_run| check_args(&args, &entry.tool.input_schema).map(|()| dry_run))
            {
                Ok(dry_run) => self.run_op(entry, kind, unit_args, args, dry_run, actor).await,
                Err(e) => Err(e),
            },
            Route::Builtin(builtin) => match check_args(&args, &entry.tool.input_schema) {
                // Reading the log keeps nothing between calls, so a request that stands alone can still do it.
                Ok(()) if builtin != Builtin::Log && self.http && stands_alone(context) => Err(needs_a_session()),
                Ok(()) => self.run_builtin(builtin, args, actor).await,
                Err(e) => Err(e),
            },
        };
        let tool = entry.tool.name.as_ref();
        outcome.unwrap_or_else(|e| render::failure(tool, &in_tool_names(e, &entry.op, tool), Some(&entry.signature)))
    }

    async fn run_op(
        &self,
        entry: &Entry,
        kind: OpKind,
        unit_args: bool,
        args: JsonObject,
        dry_run: bool,
        actor: Actor,
    ) -> Result<CallToolResult, OpError> {
        // A query has nothing to preview. It always runs as a normal call, so a stray `dry_run` can't skip the
        // scope check the runner makes on applied calls.
        let mode = if dry_run && kind != OpKind::Query { Mode::DryRun } else { Mode::Apply };
        // Only applied edits join a transaction: a preview or a query records nothing to group, and a job has its own
        // undo in the job manager. A plain job inside a transaction is refused (ADR 0036 decision 1), so it never gets
        // the open one.
        let tx = if mode == Mode::Apply && kind == OpKind::Edit { lock(&self.state).tx.clone() } else { None };
        // Each edit that joins a transaction carries the cause its `tx_begin` gave; anything outside one has none.
        let cause = tx.as_ref().and_then(|t| t.cause.clone());
        let request = Request {
            op: entry.op.clone(),
            args: if unit_args { Value::Null } else { Value::Object(args) },
            mode,
            actor,
            grants: self.view.grants.clone(),
            cause: Some(Cause { kind: CauseKind::Prompt, text: cause }),
            tx: tx.as_ref().map(|t| t.id),
            base: None,
        };
        let outcome = self.blocking(move |runner| runner.run(request)).await??;
        if let Some(record) = &outcome.applied {
            lock(&self.state).own.remember(record.tx);
        }
        let call = Call { op: &entry.tool.name, kind, mode, tx: tx.as_ref(), verify: self.shared.guidance.verify() };
        Ok(render::success(&call, &outcome))
    }

    async fn run_builtin(&self, builtin: Builtin, args: JsonObject, actor: Actor) -> Result<CallToolResult, OpError> {
        match builtin {
            Builtin::Undo | Builtin::Redo => {
                let grants = self.view.grants.clone();
                let label = self
                    .blocking(move |runner| match builtin {
                        Builtin::Undo => runner.undo(&actor, &grants),
                        _ => runner.redo(&actor, &grants),
                    })
                    .await??;
                Ok(render::history(builtin, label.as_deref()))
            }
            Builtin::Log => self.history_log(&args),
            Builtin::TxBegin => self.begin_tx(args, actor).await,
            Builtin::TxCommit | Builtin::TxRollback => self.finish_tx(builtin == Builtin::TxCommit, actor).await,
        }
    }

    async fn begin_tx(&self, mut args: JsonObject, actor: Actor) -> Result<CallToolResult, OpError> {
        let label = match args.remove("label") {
            Some(Value::String(label)) if !label.trim().is_empty() => label,
            Some(_) => return Err(OpError::invalid("label", "`label` must be a non-empty string")),
            None => {
                return Err(OpError::invalid("label", "`label` is required")
                    .hint("name the undo step, e.g. \"make the face more oval\""));
            }
        };
        let cause = cause_arg(args.remove("cause"))?;
        {
            let mut state = lock(&self.state);
            if let Some(open) = &state.tx {
                return Err(already_open(open));
            }
            if state.beginning {
                return Err(OpError::conflict("another tx_begin in this session is still starting")
                    .hint("wait for it to finish, then use the transaction it opened"));
            }
            state.beginning = true;
        }
        let name = label.clone();
        let started = self.blocking(move |runner| runner.begin(&name, &actor)).await;
        let mut state = lock(&self.state);
        state.beginning = false;
        let id = started?;
        state.tx = Some(OpenTx { id, label: label.clone(), cause });
        state.own.remember(id);
        drop(state);
        let text = format!(
            "Opened transaction {} ({label:?}). Edits you apply now join it; call tx_commit when the group is done, or \
             tx_rollback to discard it.",
            id.0
        );
        Ok(render::confirmation(text, json!({"ok": true, "tx": id.0, "label": label})))
    }

    async fn finish_tx(&self, commit: bool, actor: Actor) -> Result<CallToolResult, OpError> {
        let open = lock(&self.state).tx.clone().ok_or_else(|| {
            OpError::not_found("no transaction is open in this session").hint("start one with tx_begin")
        })?;
        let id = open.id;
        let result = self
            .blocking(move |runner| if commit { runner.commit(id, &actor) } else { runner.rollback(id, &actor) })
            .await?;
        // `not_found` means the runner no longer has it, so forgetting it is right; a conflict leaves it open.
        if result.as_ref().is_ok() || result.as_ref().is_err_and(|e| e.code == omuzaki_ops_api::ErrorCode::NotFound) {
            lock(&self.state).tx = None;
        }
        result?;
        let (verb, tail) = if commit {
            ("Committed", "Its edits are now one undo step; history_undo reverts them together.")
        } else {
            ("Rolled back", "Every edit made since tx_begin was reverted.")
        };
        let text = format!("{verb} transaction {} ({:?}). {tail}", id.0, open.label);
        Ok(render::confirmation(text, json!({"ok": true, "tx": id.0, "label": open.label})))
    }
}

impl Drop for Session {
    /// A connection that ends mid-transaction rolls it back: nothing else could ever finish it.
    fn drop(&mut self) {
        let open = lock(&self.state).tx.take();
        // A transaction can only exist once the actor is fixed, by the same call that opened it.
        if let (Some(open), Some(actor)) = (open, self.actor.get())
            && let Err(e) = self.shared.runner.rollback(open.id, actor)
        {
            tracing::warn!(tx = open.id.0, label = %open.label, error = %e, "couldn't roll back an abandoned tx");
        }
    }
}

/// The runner names ops with dots in its messages (`arguments for face.set_profile don't match ...`); the agent knows
/// the tool as `face_set_profile`, so error text says that.
fn in_tool_names(mut error: OpError, op: &str, tool: &str) -> OpError {
    error.message = error.message.replace(op, tool);
    error.hint = error.hint.map(|hint| hint.replace(op, tool));
    error
}

fn already_open(open: &OpenTx) -> OpError {
    OpError::conflict(format!("transaction {} ({:?}) is already open in this session", open.id.0, open.label)).hint(
        "only one transaction at a time: finish it with tx_commit, or discard it with tx_rollback, then begin the next",
    )
}

/// True for a request that carries its own protocol version and capabilities (2026-07-28) instead of belonging to an
/// `initialize` session.
fn stands_alone(context: &RequestContext<RoleServer>) -> bool {
    context.meta.missing_required_keys(&ProtocolVersion::V_2026_07_28).is_empty()
}

/// Why a call needs `read`: the op log and its notifications carry document values (patches, arguments), so reading
/// them is as sensitive as running a query.
fn read_needed(what: &str) -> String {
    format!("{what} needs the `read` scope, which this session lacks. Ask the user to grant it.")
}

fn needs_a_session() -> OpError {
    OpError::unsupported(
        "this request has no session, so transactions and undo can't work: each HTTP request would be a different \
         agent with nothing to undo or commit",
    )
    .hint("connect with the `initialize` handshake (protocol 2025-11-25 or earlier); other tools work without one")
}

/// `tx_begin`'s optional `cause`. The argument check already made it a string; this enforces its length, which the
/// argument check doesn't read from the schema. A blank cause counts as absent, so it is never stamped on an edit.
fn cause_arg(value: Option<Value>) -> Result<Option<String>, OpError> {
    let cause = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(cause)) => cause,
        Some(other) => return Err(OpError::invalid("cause", format!("`cause` must be a string, got {other}"))),
    };
    if cause.trim().is_empty() {
        return Ok(None);
    }
    let length = cause.chars().count();
    if length > CAUSE_MAX_CHARS {
        return Err(OpError::invalid(
            "cause",
            format!("`cause` is {length} characters; the limit is {CAUSE_MAX_CHARS}"),
        )
        .hint("pass the first line of what the person asked for, shortened to 200 characters, or leave `cause` out"));
    }
    Ok(Some(cause))
}

/// `dry_run` must be a boolean; anything else is a mistake that could turn a preview into an apply.
fn take_dry_run(args: &mut JsonObject) -> Result<bool, OpError> {
    match args.remove(DRY_RUN) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(dry)) => Ok(dry),
        Some(other) => Err(OpError::invalid(DRY_RUN, format!("`dry_run` must be true or false, got {other}"))
            .hint("pass dry_run: true to preview, or omit it to apply")),
    }
}

fn log_resource() -> Resource {
    Resource::new(LOG_URI, "log")
        .with_title("Op log")
        .with_description(
            "The most recent events (ops applied, transactions committed, undo and redo), newest last, each with a \
             sequence number `n`. Subscribe to be notified when it changes, then read it to see what happened.",
        )
        .with_mime_type("application/json")
}

fn unknown_resource(uri: &str, templates: &[ResourceTemplate]) -> McpError {
    let mut available = vec![LOG_URI.to_owned()];
    available.extend(templates.iter().map(|t| t.uri_template.clone()));
    McpError::resource_not_found(
        format!("unknown resource {uri}; the available resources are {}", available.join(", ")),
        None,
    )
}

// Most handlers here are synchronous; rmcp's trait methods are async, so the lint's `impl Future` rewrite would only
// make them harder to read.
#[allow(clippy::unused_async_trait_impl)]
impl ServerHandler for Session {
    /// Over HTTP, answered as a server without a stateless mode would (clients then fall back to `initialize`); over
    /// other transports, the default.
    async fn discover(&self, _context: RequestContext<RoleServer>) -> Result<DiscoverResult, McpError> {
        if self.http {
            return Err(McpError::method_not_found::<DiscoverRequestMethod>());
        }
        Ok(DiscoverResult::from_server_info(self.supported_protocol_versions().into_owned(), self.get_info()))
    }

    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_prompts()
            .enable_resources()
            .enable_resources_subscribe()
            .build();
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new("omuzaki", env!("CARGO_PKG_VERSION")).with_title("Omuzaki Studio"))
            .with_instructions(self.view.instructions.clone())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.view.catalog.tools().cloned().collect()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.view.catalog.get(name).map(|entry| entry.tool.clone())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        // Matched on the resolved route, so the dotted alias (`history.log`) counts as the same tool.
        let reads_log = self
            .view
            .catalog
            .get(request.name.as_ref())
            .is_some_and(|entry| matches!(entry.route, Route::Builtin(Builtin::Log)));
        let result = self.dispatch(request, &context).await;
        // A successful `history_log` is the log itself as its whole text, so nothing may be put in front of it. Its
        // snapshot already caught this session up, so later events are reported by the next call, not lost.
        let log_text = reads_log && result.is_error != Some(true);
        // After the call, so a change another actor made while it ran is included, and so this call's own events
        // (which carry this session's actor) are passed over.
        let report = if log_text { None } else { self.others_changed(self.actor(&context)) };
        let result = match report {
            Some(report) => render::with_heads_up(result, &report),
            None => result,
        };
        Ok(result.into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(self.shared.prompts.list(&self.view.catalog)))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        Ok(self.shared.prompts.get(&self.view.catalog, &request.name, request.arguments)?.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let mut resources = vec![log_resource()];
        // Listing reads the document, so it needs `read` like reading does; without it only the log (which names
        // nothing from the document) is listed, as before.
        if self.view.grants.allows(Scope::Read) {
            let (providers, grants) = (self.shared.providers.clone(), self.view.grants.clone());
            let listed = self
                .blocking(move |runner| {
                    providers.all().iter().map(|p| p.list(runner, &grants)).collect::<Result<Vec<_>, _>>()
                })
                .await
                .and_then(|r| r)
                .map_err(protocol_error)?;
            resources.extend(listed.into_iter().flatten());
        }
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(self.shared.providers.templates()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri == LOG_URI {
            return self.read_log();
        }
        let Some(provider) = self.shared.providers.serving(&request.uri) else {
            return Err(unknown_resource(&request.uri, &self.shared.providers.templates()));
        };
        self.require_read(&format!("reading {}", request.uri))?;
        let (grants, uri) = (self.view.grants.clone(), request.uri);
        let contents = self
            .blocking(move |runner| provider.read(runner, &grants, &uri))
            .await
            .and_then(|r| r)
            .map_err(protocol_error)?;
        Ok(ReadResourceResult::new(contents).into())
    }

    /// Protocol 2025-11-25 and earlier: push `resources/updated` for the log, or for a provider's resource when an op
    /// changes it, until unsubscribed or disconnected.
    // `resources/subscribe` is legacy-only from 2026-07-28 (see `listen`), but still how today's clients subscribe.
    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let uri = request.uri;
        let what =
            if uri == LOG_URI { "subscribing to the op log".to_owned() } else { format!("subscribing to {uri}") };
        self.require_read(&what)?;
        if !self.subscribable(&uri) {
            return Err(unknown_resource(&uri, &self.shared.providers.templates()));
        }
        let mut state = lock(&self.state);
        lock(&state.subscribed).insert(uri);
        if state.feed.is_none() {
            let (mut updates, peer) = (Updates::new(&self.shared.hub, &self.shared.providers), context.peer);
            let subscribed = Arc::clone(&state.subscribed);
            state.feed = Some(AbortOnDrop(tokio::spawn(async move {
                while updates.arrived().await {
                    let wanted = lock(&subscribed).clone();
                    for uri in updates.changed(&wanted) {
                        if peer.notify_resource_updated(ResourceUpdatedNotificationParam::new(uri)).await.is_err() {
                            return;
                        }
                    }
                }
            })));
        }
        Ok(())
    }

    #[allow(deprecated)] // see `subscribe`
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.require_read(&format!("unsubscribing from {}", request.uri))?;
        if !self.subscribable(&request.uri) {
            return Err(unknown_resource(&request.uri, &self.shared.providers.templates()));
        }
        let mut state = lock(&self.state);
        let none_left = {
            let mut subscribed = lock(&state.subscribed);
            subscribed.remove(&request.uri);
            subscribed.is_empty()
        };
        if none_left {
            state.feed = None;
        }
        Ok(())
    }

    /// Protocol 2026-07-28: `subscriptions/listen` may ask for updates to the log and to provider resources.
    fn accepted_subscription_filter(&self, requested: &SubscriptionFilter) -> Option<SubscriptionFilter> {
        let wanted = requested.resource_subscriptions.iter().flatten();
        let accepted: Vec<&String> = if self.view.grants.allows(Scope::Read) {
            wanted.filter(|u| self.subscribable(u)).collect()
        } else {
            Vec::new()
        };
        Some(if accepted.is_empty() {
            SubscriptionFilter::new()
        } else {
            accepted
                .into_iter()
                .fold(SubscriptionFilter::builder(), rmcp::model::SubscriptionFilterBuilder::resource_subscription)
                .build()
        })
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        let wanted: BTreeSet<String> = context.accepted().resource_subscriptions.iter().flatten().cloned().collect();
        // `accepted_subscription_filter` already refuses everything without `read`; checking again keeps this loop safe
        // on its own.
        if !self.view.grants.allows(Scope::Read) || wanted.is_empty() {
            context.cancelled().await;
            return Ok(());
        }
        let mut updates = Updates::new(&self.shared.hub, &self.shared.providers);
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                arrived = updates.arrived() => {
                    if !arrived {
                        return Ok(());
                    }
                    for uri in updates.changed(&wanted) {
                        if context.sink().notify_resource_updated(&uri).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}
