//! One client connection: who the agent is, its open transaction, and how its calls reach the runner.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use omuzaki_history_api::JournalStatus;
use omuzaki_ops_api::{
    Actor, Budget, Cause, CauseKind, ErrorCode, ErrorDetail, Grants, Jobs, Limits, Mode, OpError, OpKind, PauseReason,
    Request, Runner, Scope, SessionSpec, SessionState, Sessions, Transport,
};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, ContentBlock, CreateTaskResult,
    DiscoverRequestMethod, DiscoverResult, GetPromptRequestParams, GetPromptResponse, GetTaskParams, GetTaskResult,
    Implementation, InitializeRequestParams, InitializeResult, JsonObject, ListPromptsResult,
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
use crate::jobs::{self, JobWatch};
use crate::names;
use crate::render::{self, Call, OpenTx};
use crate::resources::protocol_error;
use crate::schema::{DRY_RUN, check_args};
use crate::server::{Shared, View};

/// How long a reply to a change waits for the journal to save it (spec v3 §6, journal design §5.5).
const SAVE_WAIT: Duration = Duration::from_secs(5);

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
    /// The job manager, if the host wired one: job ops then answer a client that declared tasks with a task handle.
    jobs: Option<Arc<dyn Jobs>>,
    /// Fixed at the first call, so a transaction always belongs to one actor identity. If the host's registry would not
    /// open a session, the refusal is kept instead and answers every call: a connection without a session runs nothing.
    actor: OnceLock<Result<Actor, OpError>>,
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
        shared.connections.open(serial);
        let state = State { seen: shared.hub.latest(), ..State::default() };
        Self { shared, view, serial, http: false, jobs: None, actor: OnceLock::new(), state: Mutex::new(state) }
    }

    /// Gives this handler the host's job manager (see [`McpServer::with_jobs`](crate::McpServer::with_jobs)).
    #[must_use]
    pub(crate) fn with_jobs(mut self, jobs: Option<Arc<dyn Jobs>>) -> Self {
        self.jobs = jobs;
        self
    }

    /// The job manager, when this call may become a task: the host wired one and the client declared the extension.
    fn task_jobs(&self, context: &RequestContext<RoleServer>) -> Option<Arc<dyn Jobs>> {
        self.jobs.clone().filter(|_| context.client_capabilities().is_some_and(|caps| caps.supports_tasks()))
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

    /// Names this session's client from the identity a request carries in its own metadata, which is what makes it
    /// count as connected (`agent.connection`). Outside HTTP a session may start with `discover` or with any request
    /// (`subscriptions/listen` included), skipping both `initialize` and `discover`, so every request handler calls this
    /// first. Over HTTP a session starts with `initialize`, which names it.
    fn name_client(&self, context: &RequestContext<RoleServer>) {
        if self.http {
            return;
        }
        if let Some(info) = context.meta.client_info() {
            self.shared.connections.name(self.serial, &info.name);
        }
        self.open_session(context);
    }

    /// Opens this connection's session in the registry now, so a client that has only connected is already in the
    /// agent panel. A refusal stays in the cached result, and the first call that needs the actor reports it.
    fn open_session(&self, context: &RequestContext<RoleServer>) {
        let _ = self.actor(context);
    }

    fn actor(&self, context: &RequestContext<RoleServer>) -> Result<&Actor, &OpError> {
        self.actor
            .get_or_init(|| {
                let client = context.client_info().map_or_else(|| "mcp-client".to_owned(), |info| info.name);
                let Some(host) = &self.shared.sessions else {
                    return Ok(Actor::Agent { id: format!("{client}#{}", self.serial), model: None });
                };
                // The registry names the agent. No other name will do: one made here could equal a registered
                // session's actor and borrow its grants.
                let spec = SessionSpec {
                    transport: if self.http { Transport::Http { token: "mcp".to_owned() } } else { Transport::Stdio },
                    client,
                    model: None,
                    on_behalf_of: host.person.clone(),
                    grants: self.view.grants.clone(),
                    budget: Budget::default(),
                    limits: Limits::default(),
                    parent: None,
                    role: None,
                    fence: Vec::new(),
                };
                host.sessions
                    .open(spec)
                    .map(|info| info.actor)
                    .inspect_err(|e| tracing::warn!(error = %e, "couldn't open a session for a connection"))
            })
            .as_ref()
    }

    /// What this connection's session stands for right now (see [`standing`]), asked of the registry again on every
    /// request.
    fn standing(&self, context: &RequestContext<RoleServer>) -> Result<Standing, McpError> {
        let denied = |why: String| McpError::invalid_request(why, Some(json!({"code": "denied"})));
        let actor = if self.shared.sessions.is_none() {
            None
        } else {
            Some(
                self.actor(context)
                    .map_err(|e| denied(format!("this connection has no session: {}", e.message)))?
                    .clone(),
            )
        };
        standing(self.shared.sessions.as_ref().map(|h| &h.sessions), actor.as_ref(), &self.view.grants).map_err(denied)
    }

    /// What this connection may read right now: the server's grants narrowed by its session's. A paused session still
    /// reads (design §4.3: the document, the log and its own record stay readable); one that has ended, or that the
    /// registry no longer has, reads nothing.
    fn session_grants(&self, context: &RequestContext<RoleServer>) -> Result<Grants, McpError> {
        self.standing(context).map(|s| s.grants)
    }

    /// What a notification feed needs to ask the registry again later, without the request that started it.
    fn watch(&self, context: &RequestContext<RoleServer>) -> Watch {
        Watch {
            sessions: self.shared.sessions.as_ref().map(|h| Arc::clone(&h.sessions)),
            actor: self.actor(context).ok().cloned(),
            ceiling: self.view.grants.clone(),
        }
    }

    /// The op log and its notifications carry document values (patches, arguments), so reading or being notified of
    /// them needs `read`, exactly like running a query. Anything else that exposes document state to the client
    /// (`Runner::get`, resource providers) must call this first.
    fn require_read(grants: &Grants, what: &str) -> Result<(), McpError> {
        if grants.allows(Scope::Read) {
            return Ok(());
        }
        Err(McpError::invalid_request(read_needed(what), Some(json!({"code": "denied", "scope": "read"}))))
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
    fn read_log(&self, grants: &Grants) -> Result<ReadResourceResponse, McpError> {
        Self::require_read(grants, "reading the op log")?;
        let body = self
            .log_json(None)
            .map_err(|e| McpError::internal_error(format!("couldn't encode the op log: {e}"), None))?;
        let contents = ResourceContents::text(body, LOG_URI).with_mime_type("application/json");
        Ok(ReadResourceResult::new(vec![contents]).into())
    }

    /// `history_log`: the log resource's body, for clients that can't read resources. `after_n` keeps only the events
    /// numbered above it.
    fn history_log(&self, args: &JsonObject, grants: &Grants) -> Result<CallToolResult, OpError> {
        if !grants.allows(Scope::Read) {
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
    fn others_changed(&self, me: &Actor, grants: &Grants) -> Option<Report> {
        if !grants.allows(Scope::Read) {
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
        jobs::off_thread(move || work(runner.as_ref())).await
    }

    /// Every call lands here and always yields a tool result: failures become tool errors, never protocol errors.
    async fn dispatch(&self, request: CallToolRequestParams, context: &RequestContext<RoleServer>) -> Reply {
        let name = request.name.as_ref();
        let Some(entry) = self.view.catalog.get(name) else {
            return render::unknown_tool(name, &self.view.catalog.similar(name)).into();
        };
        let actor = match self.actor(context) {
            Ok(actor) => actor.clone(),
            Err(refusal) => {
                let tool = entry.tool.name.as_ref();
                return render::failure(tool, &in_tool_names(refusal.clone(), &entry.op, tool), Some(&entry.signature))
                    .into();
            }
        };
        let mut args = request.arguments.unwrap_or_default();
        // The registry's word now, not the one at connection setup: grants may have been narrowed and the session
        // paused or ended since.
        let standing = self.standing(context).map_err(|e| OpError::denied(e.message.to_string()));
        let outcome = match entry.route {
            Route::Op { kind, unit_args } => match take_dry_run(&mut args)
                .and_then(|dry_run| check_args(&args, &entry.tool.input_schema).map(|()| dry_run))
            {
                Ok(false) if kind == OpKind::Job && self.task_jobs(context).is_some() => {
                    self.start_task(entry, unit_args, args, actor, context).await
                }
                Ok(dry_run) => self.run_op(entry, kind, unit_args, args, dry_run, actor).await,
                Err(e) => Err(e),
            },
            Route::Builtin(builtin) => match check_args(&args, &entry.tool.input_schema) {
                // Reading the log keeps nothing between calls, so a request that stands alone can still do it.
                Ok(()) if builtin != Builtin::Log && self.http && stands_alone(context) => Err(needs_a_session()),
                Ok(()) => self.run_builtin(builtin, args, actor, &standing).await,
                Err(e) => Err(e),
            },
        };
        let tool = entry.tool.name.as_ref();
        match outcome {
            Ok(reply) => reply,
            Err(e) => render::failure(tool, &in_tool_names(e, &entry.op, tool), Some(&entry.signature)).into(),
        }
    }

    async fn run_op(
        &self,
        entry: &Entry,
        kind: OpKind,
        unit_args: bool,
        args: JsonObject,
        dry_run: bool,
        actor: Actor,
    ) -> Result<Reply, OpError> {
        // A query has nothing to preview. It always runs as a normal call, so a stray `dry_run` can't skip the
        // scope check the runner makes on applied calls.
        let mode = if dry_run && kind != OpKind::Query { Mode::DryRun } else { Mode::Apply };
        // Only applied edits join a transaction: a preview or a query records nothing to group, and a job has its own
        // undo in the job manager. A plain job inside a transaction is refused (ADR 0036 decision 1), so it never gets
        // the open one.
        let tx = if mode == Mode::Apply && kind == OpKind::Edit { lock(&self.state).tx.clone() } else { None };
        // Each edit that joins a transaction carries the cause its `tx_begin` gave; anything outside one has none.
        let cause = tx.as_ref().and_then(|t| t.cause.clone());
        let request = self.request(entry, unit_args, args, mode, actor, cause, tx.as_ref());
        let outcome = self.blocking(move |runner| runner.run(request)).await??;
        if let Some(record) = &outcome.applied {
            lock(&self.state).own.remember(record.tx);
        }
        let call = Call { op: &entry.tool.name, kind, mode, tx: tx.as_ref(), verify: self.shared.guidance.verify() };
        let result = render::success(&call, &outcome);
        Ok(if outcome.applied.is_some() { self.confirm_saved(result).await } else { result.into() })
    }

    /// The runner request for a call of `entry`, as `actor` with this session's grants.
    // The call's parts, which `run_op` and `start_task` each have to hand over; a struct would only rename them.
    #[allow(clippy::too_many_arguments)]
    fn request(
        &self,
        entry: &Entry,
        unit_args: bool,
        args: JsonObject,
        mode: Mode,
        actor: Actor,
        cause: Option<String>,
        tx: Option<&OpenTx>,
    ) -> Request {
        Request {
            op: entry.op.clone(),
            args: if unit_args { Value::Null } else { Value::Object(args) },
            mode,
            actor,
            grants: self.view.grants.clone(),
            cause: Some(Cause { kind: CauseKind::Prompt, text: cause }),
            tx: tx.map(|t| t.id),
            base: None,
        }
    }

    /// Starts a job op without waiting for it and answers with its task handle; a watcher then pushes the job's
    /// progress and status to the client. The job belongs to this session's actor, so only this session can cancel it.
    async fn start_task(
        &self,
        entry: &Entry,
        unit_args: bool,
        args: JsonObject,
        actor: Actor,
        context: &RequestContext<RoleServer>,
    ) -> Result<Reply, OpError> {
        let jobs = self.task_jobs(context).ok_or_else(|| OpError::unsupported("this client did not declare tasks"))?;
        // A plain job joins no transaction (ADR 0036 decision 1), so there is no cause to carry either.
        let request = self.request(entry, unit_args, args, Mode::Apply, actor, None, None);
        let hub = Arc::clone(&self.shared.hub);
        // Before the start, so the job's first events can't be missed.
        let after = hub.latest();
        let job = jobs::off_thread(move || jobs.start(request)).await??;
        let tool = entry.tool.name.to_string();
        let verify = self.shared.guidance.verify().map(str::to_owned);
        let task = jobs::task_of(&job, &tool, verify.as_deref());
        let token = context.meta.get_progress_token();
        tokio::spawn(JobWatch { hub, after, peer: context.peer.clone(), token, job, tool, verify }.run());
        Ok(Reply { task: Some(CreateTaskResult::new(task.task)), ..Reply::from(CallToolResult::success(Vec::new())) })
    }

    /// Waits until the journal has saved the change `result` reports, then returns it. A change that applied but isn't
    /// saved yet (the wait timed out, or saving fails) still succeeds: the reply carries the warning, which
    /// `call_tool` puts first.
    async fn confirm_saved(&self, result: CallToolResult) -> Reply {
        let Some(journal) = self.shared.journal.clone() else { return result.into() };
        let waited = tokio::task::spawn_blocking(move || match journal.sync(SAVE_WAIT) {
            Ok(_) => None,
            Err(_) => Some(journal.status()),
        })
        .await;
        let unsaved = match waited {
            Ok(None) => None,
            Ok(Some(status)) => Some(format!("applied, not yet saved: {}", describe(&status))),
            Err(_) => Some("applied, not yet saved: the journal's status couldn't be read".to_owned()),
        };
        Reply { result, unsaved, task: None }
    }

    async fn run_builtin(
        &self,
        builtin: Builtin,
        args: JsonObject,
        actor: Actor,
        standing: &Result<Standing, OpError>,
    ) -> Result<Reply, OpError> {
        match builtin {
            Builtin::Undo | Builtin::Redo => {
                let grants = self.view.grants.clone();
                let label = self
                    .blocking(move |runner| match builtin {
                        Builtin::Undo => runner.undo(&actor, &grants),
                        _ => runner.redo(&actor, &grants),
                    })
                    .await??;
                let changed = label.is_some();
                let result = render::history(builtin, label.as_deref());
                Ok(if changed { self.confirm_saved(result).await } else { result.into() })
            }
            Builtin::Log => self.history_log(&args, &standing.clone()?.grants).map(Reply::from),
            Builtin::TxBegin => {
                // `Executor::begin` cannot refuse, and for a paused session it hands back a transaction that was never
                // opened; so the refusal is made here, before it runs, and `begin_tx` checks again afterwards, because
                // a pause can land in between.
                if let Some((by, reason)) = standing.clone()?.paused {
                    return Err(paused(by, reason));
                }
                self.begin_tx(args, actor).await.map(Reply::from)
            }
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
        let who = actor.clone();
        let started = self.blocking(move |runner| runner.begin(&name, &actor)).await;
        let opened = started.and_then(|id| self.confirm_opened(id, &who).map(|()| id));
        let mut state = lock(&self.state);
        state.beginning = false;
        let id = opened?;
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

    /// Whether the runner really opened transaction `id` for `actor`. `Runner::begin` cannot refuse: when a pause won
    /// the race against it, it hands back an id that was never opened, and the registry (which sees the transactions
    /// it opened under the same lock) is where that shows. A pause that lands after `begin` leaves the transaction
    /// open, so it still counts as opened.
    fn confirm_opened(&self, id: omuzaki_ops_api::TxId, actor: &Actor) -> Result<(), OpError> {
        let Some(host) = &self.shared.sessions else { return Ok(()) };
        let info =
            host.sessions.session_of(actor).ok_or_else(|| OpError::denied("this connection's session is gone"))?;
        if info.open_tx.as_ref().is_some_and(|open| open.tx == id) {
            return Ok(());
        }
        Err(match info.state {
            SessionState::Paused { by, reason } => paused(by, reason),
            SessionState::Ended { .. } => OpError::denied("this connection's session has ended"),
            SessionState::Active => OpError::conflict("the transaction did not open")
                .hint("the session changed while it started: call tx_begin again"),
        })
    }

    async fn finish_tx(&self, commit: bool, actor: Actor) -> Result<Reply, OpError> {
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
        let result = render::confirmation(text, json!({"ok": true, "tx": id.0, "label": open.label}));
        Ok(self.confirm_saved(result).await)
    }
}

impl Drop for Session {
    /// A connection that ends mid-transaction rolls it back: nothing else could ever finish it.
    fn drop(&mut self) {
        self.shared.connections.close(self.serial);
        let open = lock(&self.state).tx.take();
        // A transaction can only exist once the actor is fixed, by the same call that opened it.
        if let (Some(open), Some(Ok(actor))) = (open, self.actor.get())
            && let Err(e) = self.shared.runner.rollback(open.id, actor)
        {
            tracing::warn!(tx = open.id.0, label = %open.label, error = %e, "couldn't roll back an abandoned tx");
        }
    }
}

/// A tool's reply, and the save warning that must lead it when the change applied but isn't saved yet. The warning
/// is kept apart from the result so that `call_tool` can put it in front of everything else, including the heads-up.
struct Reply {
    result: CallToolResult,
    unsaved: Option<String>,
    /// Set when the call started a job: the client gets this handle instead of the result.
    task: Option<CreateTaskResult>,
}

impl From<CallToolResult> for Reply {
    fn from(result: CallToolResult) -> Self {
        Self { result, unsaved: None, task: None }
    }
}

/// What the journal's status says about a save, for the warning.
fn describe(status: &JournalStatus) -> String {
    match status {
        JournalStatus::Healthy => "the journal is healthy and should save it shortly".to_owned(),
        JournalStatus::Behind { pending } => format!("saving is behind, with {pending} entries waiting"),
        JournalStatus::Failing { reason } => format!("saving is failing: {reason}"),
        _ => "the journal's status is unknown".to_owned(),
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

/// What a session stands for at the moment the registry is asked.
#[derive(Clone)]
struct Standing {
    /// What it may do: the server's grants narrowed by the session's effective ones.
    grants: Grants,
    /// Who paused it and why, when it is paused.
    paused: Option<(Actor, PauseReason)>,
}

/// The standing of `actor`'s session in `sessions`, or why it has none: the registry no longer has it, or it ended.
/// Without a registry the server's grants stand. A paused session stands (design §4.3: it may still read).
fn standing(sessions: Option<&Arc<dyn Sessions>>, actor: Option<&Actor>, ceiling: &Grants) -> Result<Standing, String> {
    let (Some(sessions), Some(actor)) = (sessions, actor) else {
        return Ok(Standing { grants: ceiling.clone(), paused: None });
    };
    let info = sessions.session_of(actor).ok_or_else(|| "this connection's session is gone".to_owned())?;
    let paused = match &info.state {
        SessionState::Active => None,
        SessionState::Paused { by, reason } => Some((by.clone(), *reason)),
        SessionState::Ended { .. } => return Err("this connection's session has ended".to_owned()),
    };
    Ok(Standing { grants: ceiling.intersect(&info.grants), paused })
}

/// A notification feed's way back to the registry.
struct Watch {
    sessions: Option<Arc<dyn Sessions>>,
    actor: Option<Actor>,
    ceiling: Grants,
}

impl Watch {
    /// `Some(true)` while the session may read, `Some(false)` while it may not (yet), `None` once it is over.
    fn may_read(&self) -> Option<bool> {
        standing(self.sessions.as_ref(), self.actor.as_ref(), &self.ceiling).ok().map(|s| s.grants.allows(Scope::Read))
    }
}

/// The session is paused: nothing ran, and a person must resume it.
fn paused(by: Actor, reason: PauseReason) -> OpError {
    let mut error = OpError::conflict(format!("this session is paused ({reason:?})"))
        .hint("wait for a person to resume it; do not retry")
        .with_detail(ErrorDetail::Paused { by, reason });
    error.code = ErrorCode::Waiting;
    error
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
    /// The handshake names this session's client, which is what makes it count as connected (`agent.connection`).
    /// The rest is the default: record the peer, then negotiate the protocol version.
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        self.shared.connections.name(self.serial, &request.client_info.name);
        context.peer.set_peer_info(request.clone());
        self.open_session(&context);
        self.negotiate_initialize(&request)
    }

    /// Over HTTP, answered as a server without a stateless mode would (clients then fall back to `initialize`); over
    /// other transports, the default.
    async fn discover(&self, context: RequestContext<RoleServer>) -> Result<DiscoverResult, McpError> {
        if self.http {
            return Err(McpError::method_not_found::<DiscoverRequestMethod>());
        }
        // rmcp's discovery request carries the client's identity and completes its startup, so an idle client is
        // already connected. Over HTTP it is refused above and the client falls back to `initialize`.
        self.name_client(&context);
        Ok(DiscoverResult::from_server_info(self.supported_protocol_versions().into_owned(), self.get_info()))
    }

    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_prompts()
            .enable_resources()
            .enable_resources_subscribe();
        let capabilities = if self.jobs.is_some() { capabilities.enable_tasks() } else { capabilities }.build();
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new("omuzaki", env!("CARGO_PKG_VERSION")).with_title("Omuzaki Studio"))
            .with_instructions(self.view.instructions.clone())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.name_client(&context);
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
        self.name_client(&context);
        // Matched on the resolved route, so the dotted alias (`history.log`) counts as the same tool.
        let reads_log = self
            .view
            .catalog
            .get(request.name.as_ref())
            .is_some_and(|entry| matches!(entry.route, Route::Builtin(Builtin::Log)));
        let Reply { result, unsaved, task } = self.dispatch(request, &context).await;
        if let Some(task) = task {
            return Ok(CallToolResponse::Task(task));
        }
        // A successful `history_log` is the log itself as its whole text, so nothing may be put in front of it. Its
        // snapshot already caught this session up, so later events are reported by the next call, not lost.
        let log_text = reads_log && result.is_error != Some(true);
        // After the call, so a change another actor made while it ran is included, and so this call's own events
        // (which carry this session's actor) are passed over.
        let report = if log_text {
            None
        } else {
            let grants = self.session_grants(&context).ok();
            self.actor(&context).ok().zip(grants).and_then(|(actor, grants)| self.others_changed(actor, &grants))
        };
        let result = match report {
            Some(report) => render::with_heads_up(result, &report),
            None => result,
        };
        // Last, so the save warning leads the text, ahead of the heads-up: a client that shows only the first line
        // still sees that the change isn't saved.
        let result = match unsaved {
            Some(warning) => render::with_warning(result, &warning),
            None => result,
        };
        Ok(result.into())
    }

    /// `tasks/get`: the job behind a task, as the executor reports it. Needs `read`, like `Jobs::job`.
    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        self.name_client(&context);
        let (jobs, id) = (self.jobs.clone(), jobs::job_of(&request.task_id)?);
        let jobs = jobs.ok_or_else(McpError::method_not_found::<rmcp::model::GetTaskMethod>)?;
        let info = jobs::read_job(jobs, id, self.session_grants(&context)?).await.map_err(protocol_error)?;
        let tool = names::tool_name(&info.op);
        let verify = self.shared.guidance.verify();
        Ok(GetTaskResult::new(jobs::task_of(&info, &tool, verify)))
    }

    /// `tasks/cancel`: cancels the job. The executor decides who may, so a session can cancel only its own jobs; its
    /// refusal is passed on as it is. The task ends `cancelled` once the job has stopped (see `tasks/get`).
    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.name_client(&context);
        let id = jobs::job_of(&request.task_id)?;
        let jobs = self.jobs.clone().ok_or_else(McpError::method_not_found::<rmcp::model::CancelTaskMethod>)?;
        let actor = self.actor(&context).map_err(|e| McpError::invalid_request(e.message.clone(), None))?.clone();
        jobs::off_thread(move || jobs.cancel(id, &actor)).await.and_then(|r| r).map_err(protocol_error)?;
        Ok(())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        self.name_client(&context);
        Ok(ListPromptsResult::with_all_items(self.shared.prompts.list(&self.view.catalog)))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        self.name_client(&context);
        Ok(self.shared.prompts.get(&self.view.catalog, &request.name, request.arguments)?.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        self.name_client(&context);
        let grants = self.session_grants(&context)?;
        let mut resources = vec![log_resource()];
        // Listing reads the document, so it needs `read` like reading does; without it only the log (which names
        // nothing from the document) is listed, as before.
        if grants.allows(Scope::Read) {
            let providers = self.shared.providers.clone();
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
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        self.name_client(&context);
        Ok(ListResourceTemplatesResult::with_all_items(self.shared.providers.templates()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        self.name_client(&context);
        let grants = self.session_grants(&context)?;
        if request.uri == LOG_URI {
            return self.read_log(&grants);
        }
        let Some(provider) = self.shared.providers.serving(&request.uri) else {
            return Err(unknown_resource(&request.uri, &self.shared.providers.templates()));
        };
        Self::require_read(&grants, &format!("reading {}", request.uri))?;
        let uri = request.uri;
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
        self.name_client(&context);
        let grants = self.session_grants(&context)?;
        let uri = request.uri;
        let what =
            if uri == LOG_URI { "subscribing to the op log".to_owned() } else { format!("subscribing to {uri}") };
        Self::require_read(&grants, &what)?;
        if !self.subscribable(&uri) {
            return Err(unknown_resource(&uri, &self.shared.providers.templates()));
        }
        let mut state = lock(&self.state);
        lock(&state.subscribed).insert(uri);
        if state.feed.is_none() {
            let watch = self.watch(&context);
            let (mut updates, peer) = (Updates::new(&self.shared.hub, &self.shared.providers), context.peer);
            let subscribed = Arc::clone(&state.subscribed);
            state.feed = Some(AbortOnDrop(tokio::spawn(async move {
                while updates.arrived().await {
                    let wanted = lock(&subscribed).clone();
                    // Asked again for every batch: a session that lost `read` is told nothing, one that ended no more.
                    match watch.may_read() {
                        Some(true) => {}
                        Some(false) => continue,
                        None => return,
                    }
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
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.name_client(&context);
        Self::require_read(&self.view.grants, &format!("unsubscribing from {}", request.uri))?;
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

    /// A ping may be a session's first request, and it carries the client's identity like any other.
    async fn ping(&self, context: RequestContext<RoleServer>) -> Result<(), McpError> {
        self.name_client(&context);
        Ok(())
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        self.name_client(context.request_context());
        let wanted: BTreeSet<String> = context.accepted().resource_subscriptions.iter().flatten().cloned().collect();
        // `accepted_subscription_filter` already refuses everything without `read`; checking again keeps this loop safe
        // on its own. A connection without an active session, or whose session cannot read, is told nothing either.
        let may_read = self.session_grants(context.request_context()).is_ok_and(|g| g.allows(Scope::Read));
        if !may_read || wanted.is_empty() {
            context.cancelled().await;
            return Ok(());
        }
        let mut updates = Updates::new(&self.shared.hub, &self.shared.providers);
        let watch = self.watch(context.request_context());
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                arrived = updates.arrived() => {
                    if !arrived {
                        return Ok(());
                    }
                    // As in `subscribe`: asked again for every batch.
                    match watch.may_read() {
                        Some(true) => {}
                        Some(false) => continue,
                        None => return Ok(()),
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
