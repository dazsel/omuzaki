//! A connection whose session can move to another branch's lane (`branch_work`, `docs/design/branches.md` 8.1).
//!
//! [`LaneSession`] serves a connection through the [`Session`] of the lane it works on, and hands every request to it.
//! `branch_work` opens a session for the same connection on the target lane's server (same grants, the budget left),
//! then ends the old one as `Closed` and serves the new one from then on. The
//! connection closes only when the session it is on ends for another reason (a person revoked it, the project closed).

use std::sync::{Arc, Mutex, RwLock};

use omuzaki_ops_api::{EndReason, OpError};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, DiscoverResult, GetPromptRequestParams,
    GetPromptResponse, GetTaskParams, GetTaskResult, InitializeRequestParams, InitializeResult, ListPromptsResult,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ServerConfig, SubscribeRequestParams, SubscriptionFilter, Tool,
    UnsubscribeRequestParams,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer, SubscriptionContext};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::events::lock;
use crate::render;
use crate::server::Served;
use crate::session::{Carry, Session};

/// A connection's handler that can move its session between lanes (see the module docs).
pub struct LaneSession {
    current: RwLock<Arc<Session>>,
    /// Set when the session the connection is on ends (not when it moves): the connection closes then.
    ended: watch::Sender<Option<EndReason>>,
    /// Forwards the current session's end to `ended`; replaced on every move.
    follow: Mutex<Option<JoinHandle<()>>>,
    /// One move at a time.
    moving: tokio::sync::Mutex<()>,
}

impl LaneSession {
    pub(crate) fn new(session: Session) -> Self {
        let this = Self {
            current: RwLock::new(Arc::new(session)),
            ended: watch::Sender::new(None),
            follow: Mutex::new(None),
            moving: tokio::sync::Mutex::new(()),
        };
        this.follow_current();
        this
    }

    fn current(&self) -> Arc<Session> {
        Arc::clone(&self.current.read().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    /// Forwards the current session's end to the connection's, stopping the forwarding of the one before.
    fn follow_current(&self) {
        let mut ended = self.current().ended();
        let to = self.ended.clone();
        let follow = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => Some(runtime.spawn(async move {
                if ended.wait_for(Option::is_some).await.is_ok() {
                    let reason = ended.borrow().clone();
                    to.send_replace(reason);
                }
            })),
            // Outside a runtime there is no connection to close yet; `serve` builds handlers inside one.
            Err(_) => None,
        };
        if let Some(old) = std::mem::replace(&mut *lock(&self.follow), follow) {
            old.abort();
        }
    }

    /// `branch_work`: moves this connection's session to the lane of the branch `args` names.
    async fn work(
        &self,
        args: &serde_json::Map<String, Value>,
        context: &RequestContext<RoleServer>,
    ) -> Result<CallToolResult, OpError> {
        let _one = self.moving.lock().await;
        let old = self.current();
        let host = old.branch_host()?.clone();
        let link = old.linked(context)?;
        let wanted = args.get("branch").and_then(Value::as_str).filter(|b| !b.is_empty());
        let wanted = wanted.ok_or_else(|| OpError::invalid("branch", "`branch` is required"))?;
        let registry = Arc::clone(old.registry()?);
        let me =
            registry.session_of(&link.actor).ok_or_else(|| OpError::not_found("this connection's session is gone"))?;
        if let Some(label) = old.open_tx_label().or_else(|| me.open_tx.as_ref().map(|tx| tx.label.clone())) {
            return Err(OpError::conflict(format!("'{label}' is still open"))
                .hint("commit it with tx_commit or abandon it with tx_rollback, then move"));
        }
        let (resolve, name) = (host.clone(), wanted.to_owned());
        let (target, server) = tokio::task::spawn_blocking(move || {
            let target = resolve.by_name(&name)?;
            let server = resolve.lanes.server(&target.id)?;
            Ok::<_, OpError>((target, server))
        })
        .await
        .map_err(|e| OpError::internal(format!("opening the branch's lane stopped: {e}")))??;
        if target.id == host.branch {
            let text = format!("This session already works on {}.", target.name);
            return Ok(render::confirmation(text, json!({ "branch": target.name, "session": me.id })));
        }

        // The new session first: if it can't open, the connection stays where it was.
        let moved = old.moved_to(&server, Carry::of(&me));
        let new_link = moved.linked(context)?;
        let new_id = new_link.id;
        *self.current.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(moved);
        self.follow_current();
        old.end_moved();
        drop(old);
        let text = format!(
            "This session now works on {name}: your edits, undo and transactions from here are {name}'s, and the person's \
             version is untouched. Session {old} ended and {new} took over with the same grants and the budget left. \
             branch_checkpoint with ready asks the person to review; branch_work moves again.",
            name = target.name,
            old = me.id.0,
            new = new_id.map_or_else(|| "a new one".to_owned(), |id| id.0.to_string()),
        );
        Ok(render::confirmation(text, json!({ "branch": target.name, "session": new_id, "moved_from": me.id })))
    }
}

impl Served for LaneSession {
    fn ended(&self) -> watch::Receiver<Option<EndReason>> {
        self.ended.subscribe()
    }
}

impl ServerHandler for LaneSession {
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        self.current().initialize(request, context).await
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        self.current().on_initialized(context).await;
    }

    async fn discover(&self, context: RequestContext<RoleServer>) -> Result<DiscoverResult, McpError> {
        self.current().discover(context).await
    }

    fn get_info(&self) -> ServerConfig {
        self.current().get_info()
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.current().list_tools(request, context).await
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.current().get_tool(name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let current = self.current();
        if !current.is_branch_work(request.name.as_ref()) {
            return current.call_tool(request, context).await;
        }
        drop(current);
        let args = request.arguments.clone().unwrap_or_default();
        Ok(match self.work(&args, &context).await {
            Ok(result) => result.into(),
            Err(error) => render::failure("branch_work", &error, None).into(),
        })
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        self.current().get_task(request, context).await
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.current().cancel_task(request, context).await
    }

    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        self.current().list_prompts(request, context).await
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        self.current().get_prompt(request, context).await
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        self.current().list_resources(request, context).await
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        self.current().list_resource_templates(request, context).await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        self.current().read_resource(request, context).await
    }

    #[allow(deprecated, reason = "the legacy `resources/subscribe` is still served, as `Session` serves it")]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.current().subscribe(request, context).await
    }

    #[allow(deprecated, reason = "the legacy `resources/unsubscribe` is still served, as `Session` serves it")]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.current().unsubscribe(request, context).await
    }

    fn accepted_subscription_filter(&self, requested: &SubscriptionFilter) -> Option<SubscriptionFilter> {
        self.current().accepted_subscription_filter(requested)
    }

    async fn ping(&self, context: RequestContext<RoleServer>) -> Result<(), McpError> {
        self.current().ping(context).await
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        self.current().listen(context).await
    }
}
