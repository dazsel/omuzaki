//! One client connection: who the agent is, its open transaction, and how its calls reach the runner.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use omuzaki_history_api::{JournalSink, JournalStatus};
use omuzaki_ops_api::{
    Actor, Budget, Cause, CauseKind, EndReason, ErrorCode, ErrorDetail, Grants, Jobs, Limits, Mode, OpError, OpKind,
    PauseReason, Request, Runner, Scope, SessionSpec, SessionState, Sessions, Transport,
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
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::agent::{self, Link};
use crate::approvals::{self, ApprovalCursor, Own, OwnApprovals};
use crate::builtin::{APPROVAL_WAIT_DEFAULT_SECONDS, APPROVAL_WAIT_MAX_SECONDS, Builtin, CAUSE_MAX_CHARS};
use crate::catalog::{Entry, Route};
use crate::changes::{OwnTxs, Report};
use crate::events::{LOG_URI, Updates, lock};
use crate::info;
use crate::jobs::{self, JobWatch};
use crate::names;
use crate::render::{self, Call, OpenTx};
use crate::resources::protocol_error;
use crate::schema::{DRY_RUN, check_args};
use crate::server::{Shared, View};

/// How long a reply to a change waits for the journal to save it (spec v3 §6, journal design §5.5).
const SAVE_WAIT: Duration = Duration::from_secs(5);

/// The longest a result waits for the event feed to deliver ops the runner's log already holds, before its heads-up is
/// built without them. The feed is a worker thread a few microseconds behind; this is for a stalled one, and the next
/// result reports what this one missed.
const FEED_LAG: Duration = Duration::from_secs(1);

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
    link: OnceLock<Result<Link, OpError>>,
    /// Set when the session ends, by the registry (a person revoked it, the project closed) or by this handler as the
    /// connection goes. The connection closes when it is set.
    ended: watch::Sender<Option<EndReason>>,
    /// Cancelled when this handler is dropped, which stops the tasks that follow the registry for it.
    alive: CancellationToken,
    /// An in-app session is admitted by its host, not by a handshake (see `in_app`).
    in_app: bool,
    /// The `Mcp-Session-Id` of this connection over HTTP, learned from its first request after `initialize`.
    http_id: Arc<OnceLock<String>>,
    /// When the client last asked for anything: what tells an idle drop from a disconnect.
    last_active: Mutex<Instant>,
    state: Arc<Mutex<State>>,
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
    /// The approvals this session asked a person for, and which decisions it has been told about.
    approvals: OwnApprovals,
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
        let latest = shared.hub.latest();
        let state = State { seen: latest, approvals: OwnApprovals::new(latest), ..State::default() };
        Self {
            shared,
            view,
            serial,
            http: false,
            jobs: None,
            link: OnceLock::new(),
            ended: watch::Sender::new(None),
            alive: CancellationToken::new(),
            in_app: false,
            http_id: Arc::new(OnceLock::new()),
            last_active: Mutex::new(Instant::now()),
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// A handler for an in-app session the host has already opened in the registry (see `in_app`): there is no handshake
    /// to wait for, and the actor is the registry's.
    pub(crate) fn in_app(shared: Arc<Shared>, view: Arc<View>, serial: u64, link: Link) -> Self {
        let mut session = Self::new(shared, view, serial);
        session.in_app = true;
        let _ = session.link.set(Ok(link));
        session
    }

    /// Follows the registry for the end of this session (an in-app host calls this once the session is open).
    pub(crate) fn follow_registry(&self, after: u64) {
        if let (Some(host), Some(Ok(link))) = (&self.shared.sessions, self.link.get()) {
            self.watch_registry(host, link, after);
        }
    }

    /// Wakes when this session ends, with the reason (see [`Session::ended`]).
    pub(crate) fn ended(&self) -> watch::Receiver<Option<EndReason>> {
        self.ended.subscribe()
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
        self.touch(context);
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
    ///
    /// Only once the client has said who it is: a `ping` may come first (the spec allows it) and would otherwise open
    /// the session as `mcp-client` and pin that name for good. A call that needs the actor before then still opens it.
    fn open_session(&self, context: &RequestContext<RoleServer>) {
        if context.client_info().is_some() {
            let _ = self.link(context);
        }
    }

    fn actor(&self, context: &RequestContext<RoleServer>) -> Result<&Actor, &OpError> {
        self.link(context).map(|link| &link.actor)
    }

    /// The registered session of this connection, opened by the first request that finds the client's name.
    fn link(&self, context: &RequestContext<RoleServer>) -> Result<&Link, &OpError> {
        self.link.get_or_init(|| self.admit(context)).as_ref()
    }

    fn admit(&self, context: &RequestContext<RoleServer>) -> Result<Link, OpError> {
        let client = context.client_info().map_or_else(|| "mcp-client".to_owned(), |info| info.name);
        let Some(host) = &self.shared.sessions else {
            return Ok(Link { id: None, actor: Actor::Agent { id: format!("{client}#{}", self.serial), model: None } });
        };
        // The registry names the agent. No other name will do: one made here could equal a registered session's actor
        // and borrow its grants.
        let (transport, person) = match &self.view.credential {
            Some(credential) => {
                // The person who issued the token, unless the host issued it as itself.
                let issuer = credential.issuer();
                let person = if matches!(issuer, Actor::Person { .. }) { issuer.clone() } else { host.person.clone() };
                (Transport::Http { token: credential.label().to_owned() }, person)
            }
            None if self.http => (Transport::Http { token: "mcp".to_owned() }, host.person.clone()),
            None => (Transport::Stdio, host.person.clone()),
        };
        let spec = SessionSpec {
            transport,
            client,
            model: None,
            on_behalf_of: person,
            grants: self.view.grants.clone(),
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        };
        // Before the open, so an end that follows at once is not missed.
        let after = self.shared.hub.latest();
        let info = host
            .sessions
            .open(spec)
            .inspect_err(|e| tracing::warn!(error = %e, "couldn't open a session for a connection"))?;
        let link = Link { id: Some(info.id), actor: info.actor };
        self.watch_registry(host, &link, after);
        Ok(link)
    }

    /// Starts following the registry for the end of this session (see [`agent::watch_end`]), and closes an HTTP
    /// connection once it has ended.
    fn watch_registry(&self, host: &crate::server::SessionHost, link: &Link, after: u64) {
        agent::watch_end(
            Arc::clone(&self.shared.hub),
            Arc::clone(&host.sessions),
            link.clone(),
            after,
            self.ended.clone(),
            self.view.credential.clone(),
            self.alive.clone(),
        );
        if let Some(credential) = self.view.credential.clone() {
            let (http_id, mut ended) = (Arc::clone(&self.http_id), self.ended.subscribe());
            tokio::spawn(async move {
                if ended.wait_for(Option::is_some).await.is_ok()
                    && let Some(id) = http_id.get()
                {
                    credential.close_session(id).await;
                }
            });
        }
    }

    /// Notes that the client is active, and remembers which HTTP session it is.
    fn touch(&self, context: &RequestContext<RoleServer>) {
        *lock(&self.last_active) = Instant::now();
        self.remember_http_session(&context.extensions);
    }

    /// Learns the `Mcp-Session-Id` of this connection from a request or notification that carries its HTTP headers. An
    /// HTTP session the registry has already ended is closed at once if this is the first time its id is known.
    fn remember_http_session(&self, extensions: &rmcp::model::Extensions) {
        if self.http_id.get().is_some() {
            return;
        }
        let Some(parts) = extensions.get::<hyper::http::request::Parts>() else { return };
        let Some(id) = parts.headers.get(rmcp::transport::common::http_header::HEADER_SESSION_ID) else { return };
        let Ok(id) = id.to_str() else { return };
        let _ = self.http_id.set(id.to_owned());
        if self.ended.borrow().is_some()
            && let Some(credential) = self.view.credential.clone()
        {
            let id = id.to_owned();
            tokio::spawn(async move { credential.close_session(&id).await });
        }
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

    /// The result processing for this session, which tasks carry to wherever their results are reported. `None` when
    /// the connection has no session (nothing to report to).
    fn reporter(&self, context: &RequestContext<RoleServer>) -> Option<Reporter> {
        let me = self.actor(context).ok()?.clone();
        Some(Reporter {
            shared: Arc::clone(&self.shared),
            state: Arc::clone(&self.state),
            me,
            watch: self.watch(context),
        })
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
            Err(e) => {
                let e = self.explain_fence(e);
                render::failure(tool, &in_tool_names(e, &entry.op, tool), Some(&entry.signature)).into()
            }
        }
    }

    /// Words a fence refusal for the agent that hit it (design §5.3): who holds the part and what they hold, or what this
    /// session's own fence is. The holder's fence comes from the registry now; names are sanitised like the heads-up.
    fn explain_fence(&self, mut error: OpError) -> OpError {
        let Some(ErrorDetail::Fenced { path, holder }) = error.detail.as_deref() else { return error };
        let paths = |fence: &[omuzaki_doc_api::DocPath]| {
            fence.iter().map(|p| crate::changes::plain(&p.to_string(), 100)).collect::<Vec<_>>().join(", ")
        };
        let registry = self.shared.sessions.as_ref().map(|host| &host.sessions);
        let path_text = crate::changes::plain(&path.to_string(), 100);
        if let Some(holder) = holder {
            let fence = registry.and_then(|r| r.session_of(holder)).map(|info| paths(&info.fence));
            let theirs = fence.filter(|f| !f.is_empty()).map(|f| format!(" (their fence: {f})")).unwrap_or_default();
            error.message = format!(
                "Refused: {path_text} is held by {}{theirs}. Nothing changed.",
                crate::changes::actor_label(holder)
            );
            error.hint = Some("Work on a part you hold, or ask the person to release it.".to_owned());
        } else {
            let mine = self
                .link
                .get()
                .and_then(|link| link.as_ref().ok())
                .zip(registry)
                .and_then(|(link, r)| r.session_of(&link.actor))
                .map(|info| paths(&info.fence))
                .unwrap_or_default();
            error.message = format!("Refused: {path_text} is outside your fence ({mine}). Nothing changed.");
            error.hint = Some("Edit only inside your fence, or ask the person to widen it.".to_owned());
        }
        error
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
        let outcome = self.run_held(request, &entry.tool.name, kind, |runner, request| runner.run(request)).await?;
        if let Some(record) = &outcome.applied {
            lock(&self.state).own.remember(record.tx);
        }
        let call = Call { op: &entry.tool.name, kind, mode, tx: tx.as_ref(), verify: self.shared.guidance.verify() };
        let result = render::success(&call, &outcome);
        Ok(if outcome.applied.is_some() { self.confirm_saved(result).await } else { result.into() })
    }

    /// Runs `request`, holding it through a pause or a rate limit instead of failing at once (design §4.3, §8.2):
    ///
    /// - refused because the session is **paused**: waits up to the host's pause hold for the person to resume it, then
    ///   runs once more. If the session is still paused, the agent is told (`waiting`, nothing ran).
    /// - refused by a **rate limit** that frees up soon: waits that long and runs again, for up to
    ///   [`agent::LIMIT_HOLD`] in all. A burst over the edit limit is slowed, not failed.
    ///
    /// `attempt` is the one try (a blocking run, or a job start), so a job started as a task gets the same holding and
    /// the same memory of a queued approval as a blocking call. A refusal that queued an approval is remembered
    /// under `tool`, so `approval_wait` knows it.
    async fn run_held<T: Send + 'static>(
        &self,
        request: Request,
        tool: &str,
        kind: OpKind,
        attempt: impl Fn(&dyn Runner, Request) -> Result<T, OpError> + Clone + Send + 'static,
    ) -> Result<T, OpError> {
        match self.hold_and_run(request, attempt).await {
            Ok(done) => Ok(done),
            Err(error) => {
                lock(&self.state).approvals.remember_queued(&error, tool, kind);
                Err(error)
            }
        }
    }

    async fn hold_and_run<T: Send + 'static>(
        &self,
        request: Request,
        attempt: impl Fn(&dyn Runner, Request) -> Result<T, OpError> + Clone + Send + 'static,
    ) -> Result<T, OpError> {
        let (Some(host), Some(Ok(link))) = (&self.shared.sessions, self.link.get()) else {
            return self.blocking(move |runner| attempt(runner, request)).await?;
        };
        let (mut held_for_pause, mut slowed_for) = (false, Duration::ZERO);
        loop {
            // Before the attempt, so a resume that lands while it runs is not missed.
            let after = self.shared.hub.latest();
            let (try_it, this) = (request.clone(), attempt.clone());
            let error = match self.blocking(move |runner| this(runner, try_it)).await? {
                Ok(done) => return Ok(done),
                Err(error) => error,
            };
            if agent::is_paused(&error) && !held_for_pause {
                held_for_pause = true;
                let resumed =
                    agent::held_until_resumed(&self.shared.hub, host.sessions.as_ref(), link, after, host.pause_hold);
                if resumed.await {
                    continue;
                }
            } else if let Some(wait) = agent::limit_wait(&error, agent::LIMIT_HOLD.saturating_sub(slowed_for)) {
                tokio::time::sleep(wait).await;
                slowed_for += wait;
                continue;
            }
            return Err(error);
        }
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
            base: self.base(),
        }
    }

    /// The newest document version this session has been told about, which its writes carry as their `base`: the
    /// executor refuses one that touches what another actor changed after it (design §5.1).
    fn base(&self) -> Option<omuzaki_doc_api::DocVersion> {
        self.shared.hub.version_at(lock(&self.state).seen)
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
        let watched = Arc::clone(&jobs);
        // A plain job joins no transaction (ADR 0036 decision 1), so there is no cause to carry either.
        let request = self.request(entry, unit_args, args, Mode::Apply, actor, None, None);
        let hub = Arc::clone(&self.shared.hub);
        // Before the start, so the job's first events can't be missed.
        let after = hub.latest();
        let job = self.run_held(request, &entry.tool.name, OpKind::Job, move |_, request| jobs.start(request)).await?;
        let tool = entry.tool.name.to_string();
        let verify = self.shared.guidance.verify().map(str::to_owned);
        let created = self.shared.task_times.of(job.id);
        let journal = self.shared.journal.clone();
        let task = jobs::task_saved(journal.clone(), None, None, &job, &tool, verify.as_deref(), &created).await;
        let token = context.meta.get_progress_token();
        tokio::spawn(
            JobWatch {
                hub,
                journal,
                jobs: watched,
                reporter: self.reporter(context),
                after,
                peer: context.peer.clone(),
                token,
                job,
                tool,
                verify,
                created,
            }
            .run(),
        );
        Ok(Reply { task: Some(CreateTaskResult::new(task.task)), ..Reply::from(CallToolResult::success(Vec::new())) })
    }

    /// Waits until the journal has saved the change `result` reports, then returns it. A change that applied but isn't
    /// saved yet (the wait timed out, or saving fails) still succeeds: the reply carries the warning, which
    /// `call_tool` puts first.
    async fn confirm_saved(&self, result: CallToolResult) -> Reply {
        let unsaved = unsaved_warning(self.shared.journal.clone()).await;
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
            Builtin::SessionInfo | Builtin::SessionList | Builtin::ApprovalWait => {
                let grants = standing.clone()?.grants;
                if !grants.allows(Scope::Read) {
                    return Err(OpError::denied(read_needed("reading session records")));
                }
                match builtin {
                    Builtin::SessionInfo => self.session_info(&actor).map(Reply::from),
                    Builtin::SessionList => self.session_list(&actor).map(Reply::from),
                    _ => self.approval_wait(&args, &actor).await,
                }
            }
        }
    }

    /// The registry, which these three tools exist only for.
    fn registry(&self) -> Result<&Arc<dyn Sessions>, OpError> {
        self.shared
            .sessions
            .as_ref()
            .map(|host| &host.sessions)
            .ok_or_else(|| OpError::unsupported("this server has no session registry"))
    }

    /// `session_info`: this session's record, from the registry now.
    fn session_info(&self, actor: &Actor) -> Result<CallToolResult, OpError> {
        let registry = self.registry()?;
        let me = registry.session_of(actor).ok_or_else(|| OpError::not_found("this connection's session is gone"))?;
        let all = registry.approvals();
        let pending = info::pending_of(&all, me.id);
        {
            // What the person is asked about is the agent's to wait on, so remember the ones it hasn't heard of.
            let mut state = lock(&self.state);
            for approval in &pending {
                state.approvals.remember(approval.id, self.own_approval(approval));
            }
        }
        Ok(info::session_info(&me, &pending, self.view.credential.as_deref()))
    }

    /// `session_list`: every live session, this one marked.
    fn session_list(&self, actor: &Actor) -> Result<CallToolResult, OpError> {
        Ok(info::session_list(&self.registry()?.sessions(), actor))
    }

    /// What is remembered about a pending approval this session did not see queued (its pairing, say).
    fn own_approval(&self, approval: &omuzaki_ops_api::PendingApproval) -> Own {
        let Some(request) = &approval.request else { return Own { tool: "pairing".to_owned(), kind: None } };
        let tool = crate::names::tool_name(&request.op);
        let kind = self.view.catalog.get(&tool).and_then(|entry| match entry.route {
            Route::Op { kind, .. } => Some(kind),
            Route::Builtin(_) => None,
        });
        Own { tool, kind }
    }

    /// `approval_wait`: waits for the person to decide one of this session's approvals and returns the outcome, once
    /// (design §7.1). Resolving is never offered here: only the person can, in Omuzaki's window.
    async fn approval_wait(&self, args: &JsonObject, actor: &Actor) -> Result<Reply, OpError> {
        let registry = Arc::clone(self.registry()?);
        let id = args
            .get("id")
            .and_then(Value::as_u64)
            .map(omuzaki_ops_api::ApprovalId)
            .ok_or_else(|| OpError::invalid("id", "`id` must be the approval's number"))?;
        let seconds = args
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(APPROVAL_WAIT_DEFAULT_SECONDS)
            .clamp(1, APPROVAL_WAIT_MAX_SECONDS);
        let me = registry.session_of(actor).ok_or_else(|| OpError::not_found("this connection's session is gone"))?;
        let pending = registry.approvals();
        let not_yours = || {
            OpError::not_found(format!("there is no approval #{} for this session", id.0)).hint(
                "approval ids come from a `waiting` result or from session_info; only this session's can be waited on",
            )
        };
        if pending.iter().any(|a| a.id == id && a.session != me.id) {
            return Err(not_yours());
        }
        {
            let mut state = lock(&self.state);
            if let Some(approval) = pending.iter().find(|a| a.id == id) {
                state.approvals.remember(id, self.own_approval(approval));
            }
            if state.approvals.get(id).is_none() {
                return Err(not_yours());
            }
        }
        let hub = &self.shared.hub;
        let mut arrivals = hub.watch();
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            if Instant::now() >= deadline {
                return Ok(Self::still_pending(id, registry.as_ref()));
            }
            if let Some(resolution) = approvals::resolution_of(hub, id) {
                let own = {
                    let mut state = lock(&self.state);
                    state.approvals.mark_reported(id);
                    let own = state.approvals.get(id).cloned();
                    // An approval this decision queued is shown with it, so it must be one the agent can wait on.
                    if let Some(own) = &own {
                        state.approvals.remember_chained(&resolution, own);
                    }
                    own
                };
                let own = own.ok_or_else(not_yours)?;
                let result = approvals::outcome(&own, &resolution, self.shared.guidance.verify());
                // Only a change that applied waits for the journal: a rejection or a withdrawal saved nothing.
                let applied = matches!(&resolution.result, omuzaki_ops_api::Resolved::Applied { outcome } if outcome.applied.is_some());
                return Ok(if applied { self.confirm_saved(result).await } else { result.into() });
            }
            if tokio::time::timeout_at(deadline, arrivals.changed()).await.is_err() {
                return Ok(Self::still_pending(id, registry.as_ref()));
            }
        }
    }

    /// The answer when `approval_wait` ran out of time: the approval is still waiting for the person (or its outcome
    /// is no longer in the feed).
    fn still_pending(id: omuzaki_ops_api::ApprovalId, registry: &dyn Sessions) -> Reply {
        let waiting = approvals::is_pending(registry, id);
        let text = if waiting {
            format!(
                "Approval #{} is still pending: the person has not decided. Call approval_wait again to keep waiting, \
                 or carry on with other work; its outcome will be in the next result's heads-up.",
                id.0
            )
        } else {
            format!(
                "Approval #{} is no longer pending, but its outcome is no longer in the event feed. Re-read the \
                 project to see whether the request ran.",
                id.0
            )
        };
        render::confirmation(text, json!({"ok": true, "approval": id.0, "pending": waiting})).into()
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

impl Session {
    /// The usage guide this client reads at the handshake: the grants' guide, and for a registered session its fence.
    fn guide(&self) -> String {
        let mut guide = self.view.instructions.clone();
        let fence = self
            .shared
            .sessions
            .as_ref()
            .zip(self.link.get().and_then(|link| link.as_ref().ok()))
            .and_then(|(host, link)| host.sessions.session_of(&link.actor))
            .map(|info| info.fence);
        if let Some(fence) = fence {
            guide.push('\n');
            guide.push_str(&crate::describe::fence_note(&fence));
        }
        guide
    }
}

impl Drop for Session {
    /// A connection that ends ends its session through the registry, which abandons the open transaction and tells
    /// everyone. If the host has no registry, or it won't end the session, the transaction is rolled back here: nothing
    /// else could ever finish it.
    fn drop(&mut self) {
        self.alive.cancel();
        self.shared.connections.close(self.serial);
        let open = lock(&self.state).tx.take();
        let Some(Ok(link)) = self.link.get() else { return };
        if self.ended.borrow().is_some() {
            // The registry ended it first and abandoned the transaction with it.
            return;
        }
        let quiet = lock(&self.last_active).elapsed();
        let reason = agent::end_reason(self.in_app, self.view.credential.as_deref(), quiet);
        self.ended.send_replace(Some(reason.clone()));
        if let (Some(host), Some(id)) = (&self.shared.sessions, link.id) {
            match host.sessions.end(id, reason) {
                Ok(_) => return,
                Err(e) => tracing::debug!(error = %e, "the registry would not end a session; rolling back instead"),
            }
        }
        if let Some(open) = open
            && let Err(e) = self.shared.runner.rollback(open.id, &link.actor)
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

/// Waits until the journal has saved what was just applied. `None` means it is saved (or there is no journal); otherwise
/// the "applied, not yet saved" warning. Shared by every reply that reports an applied change: blocking calls,
/// `approval_wait`, and tasks (polled or pushed).
pub(crate) async fn unsaved_warning(journal: Option<Arc<dyn JournalSink>>) -> Option<String> {
    let journal = journal?;
    let waited = tokio::task::spawn_blocking(move || match journal.sync(SAVE_WAIT) {
        Ok(_) => None,
        Err(_) => Some(journal.status()),
    })
    .await;
    match waited {
        Ok(None) => None,
        Ok(Some(status)) => Some(format!("applied, not yet saved: {}", describe(&status))),
        Err(_) => Some("applied, not yet saved: the journal's status couldn't be read".to_owned()),
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

/// Where a task's status pushes are up to, apart from the session's own cursors (see the module docs of `events`).
pub(crate) struct PushCursor {
    seen: u64,
    approvals: ApprovalCursor,
}

impl PushCursor {
    /// A cursor at event `at`: pushes report what happened after the task began.
    pub(crate) fn new(at: u64) -> Self {
        Self { seen: at, approvals: ApprovalCursor::new(at) }
    }
}

/// Everything a result gets after the call itself: the wait for the event feed, the heads-up about what others changed,
/// the decisions on this session's approvals, and the save warning. One place for blocking calls, `tasks/get` and the
/// task-status pushes, so a task client is told exactly what a blocking caller is.
#[derive(Clone)]
pub(crate) struct Reporter {
    shared: Arc<Shared>,
    state: Arc<Mutex<State>>,
    me: Actor,
    watch: Watch,
}

impl Reporter {
    /// What this session may do now, or `None` once it is over.
    pub(crate) fn grants(&self) -> Option<Grants> {
        standing(self.watch.sessions.as_ref(), self.watch.actor.as_ref(), &self.watch.ceiling).ok().map(|s| s.grants)
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&dyn Runner) -> T + Send + 'static,
    ) -> Result<T, OpError> {
        let runner = Arc::clone(&self.shared.runner);
        jobs::off_thread(move || work(runner.as_ref())).await
    }
    /// Waits until the event feed has delivered every op the runner's log holds, up to [`FEED_LAG`]. The feed fills from a
    /// worker thread, so without this a change that committed before a call could be missing from that call's
    /// heads-up. The log is the authority on what committed: it holds a record before any event for it exists, so a
    /// record past the feed's position means events are still on their way.
    async fn catch_up_with_log(&self, grants: &Grants) {
        if !grants.allows(Scope::Read) || !self.shared.hub.is_fed() {
            return;
        }
        let deadline = Instant::now() + FEED_LAG;
        let mut arrivals = self.shared.hub.watch();
        // Is the feed behind the log at all? Usually not, and that is the one cheap question every result asks.
        let grants_now = grants.clone();
        let delivered = self.shared.hub.log_seq();
        let behind = self.blocking(move |runner| runner.log(delivered, 1, &grants_now)).await;
        if !matches!(behind, Ok(Ok(records)) if !records.is_empty()) {
            return;
        }
        // The log's end now: the feed owes us exactly that much, however much more is committed while we wait.
        let grants = grants.clone();
        let Ok(target) = self.blocking(move |runner| crate::events::latest_log_seq(runner, &grants)).await else {
            return;
        };
        // The deadline is checked on every pass: a feed that keeps delivering must not keep the result waiting.
        while self.shared.hub.log_seq() < target && Instant::now() < deadline {
            if tokio::time::timeout_at(deadline, arrivals.changed()).await.is_err() {
                return;
            }
        }
    }

    /// What other actors changed since this session was last told, if anything, moving its position forward so each
    /// change is reported once. Needs `read` for the same reason the log does: it names paths and ops.
    ///
    /// `push` is a consumer with a cursor of its own (a task's status pushes): it is read and moved instead of the
    /// session's, which stays as it was for the results the agent calls for.
    fn others_changed(&self, grants: &Grants, push: Option<&mut PushCursor>) -> Option<Report> {
        if !grants.allows(Scope::Read) {
            return None;
        }
        let mut state = lock(&self.state);
        let seen = push.as_ref().map_or(state.seen, |p| p.seen);
        let (report, latest) = self.shared.hub.changes_since(seen, &self.me, &state.own);
        match push {
            Some(push) => push.seen = latest,
            None => state.seen = latest,
        }
        report
    }

    /// `result` with the heads-up and approval news a result carries, then the save warning in front of both.
    ///
    /// `log_text` is a result that is the log itself, which nothing may precede.
    pub(crate) async fn finish(
        &self,
        result: CallToolResult,
        unsaved: Option<String>,
        log_text: bool,
        mut push: Option<&mut PushCursor>,
    ) -> CallToolResult {
        // After the call, so a change another actor made while it ran is included, and so this call's own events
        // (which carry this session's actor) are passed over.
        let report = if log_text {
            None
        } else {
            let grants = self.grants();
            if let Some(grants) = &grants {
                self.catch_up_with_log(grants).await;
            }
            grants.and_then(|grants| self.others_changed(&grants, push.as_deref_mut()))
        };
        let result = match report {
            Some(report) => render::with_heads_up(result, &report),
            None => result,
        };
        // The agent's own approvals that were decided since it last heard, once each. `approval_wait` reports its own.
        let result = if log_text || self.shared.sessions.is_none() {
            result
        } else {
            let mut state = lock(&self.state);
            let news = match push.as_deref_mut() {
                Some(push) => state.approvals.news_for(&self.shared.hub, &mut push.approvals),
                None => state.approvals.news(&self.shared.hub),
            };
            drop(state);
            if news.is_empty() {
                result
            } else {
                let (text, data) = approvals::heads_up(&news);
                render::with_approvals(result, &text, data)
            }
        };
        // Last, so the save warning leads the text, ahead of the heads-up: a client that shows only the first line
        // still sees that the change isn't saved.
        match unsaved {
            Some(warning) => render::with_warning(result, &warning),
            None => result,
        }
    }
}

/// A notification feed's way back to the registry.
#[derive(Clone)]
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
    /// The handshake names this session's client, which is what makes it count as connected (`agent.connection`), and
    /// opens its session. Only a handshake that succeeds does either: a client whose protocol version can't be served is
    /// refused here and was never connected.
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        self.touch(&context);
        let mut result = self.negotiate_initialize(&request)?;
        self.shared.connections.name(self.serial, &request.client_info.name);
        context.peer.set_peer_info(request.clone());
        self.open_session(&context);
        // After the open: the guide carries this session's own fence.
        result.instructions = Some(self.guide());
        Ok(result)
    }

    /// The client's `notifications/initialized` is the first message after the handshake that carries the HTTP session's
    /// id, so it is where an HTTP session learns how to be closed from the inside.
    async fn on_initialized(&self, context: rmcp::service::NotificationContext<RoleServer>) {
        self.remember_http_session(&context.extensions);
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
            .with_instructions(self.guide())
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
        let result = match self.reporter(&context) {
            Some(reporter) => reporter.finish(result, unsaved, log_text, None).await,
            None => match unsaved {
                Some(warning) => render::with_warning(result, &warning),
                None => result,
            },
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
        // A task belongs to the session that started it, as `tasks/cancel` already requires: another session (another
        // token's included) is told there is no such task, not what that job is or returned.
        let mine = self.actor(&context).is_ok_and(|actor| *actor == info.actor);
        if !mine {
            return Err(protocol_error(OpError::not_found(format!("no task `{}`", jobs::task_id(id)))));
        }
        let tool = names::tool_name(&info.op);
        let verify = self.shared.guidance.verify();
        let created = self.shared.task_times.of(info.id);
        Ok(GetTaskResult::new(
            jobs::task_saved(
                self.shared.journal.clone(),
                self.reporter(&context).as_ref(),
                None,
                &info,
                &tool,
                verify,
                &created,
            )
            .await,
        ))
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
