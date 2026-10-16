//! Test rig for the host: a scripted rmcp server over `tokio::io::duplex`, a recording fake `Sessions`, a recording
//! `EntrySink` and `FakeLlm`. Nothing here touches the network or the real `omuzaki-mcp`.

// Each test file uses some of the rig; the rest is dead code to that file.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use omuzaki_agent::sink::EntrySink;
use omuzaki_agent::{AssistantHost, Caps, HostConfig, McpClientLink};
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::entry::{Entry, StoredBlock};
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::hub::EventReceiver;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{ImageMime, TurnRequest};
use omuzaki_llm_api::provider::RuntimeHandle;
use omuzaki_llm_api::stream::{Cancel, EventSource, LlmEvent, Next, StopReason, TurnStream};
use omuzaki_llm_api::usage::ModelInfo;
use omuzaki_llm_api::usage::{PriceTable, PriceTier, Rates, Usage};
use omuzaki_llm_mock::{FakeLlm, Script, ScriptTurn};
use omuzaki_ops_api::{
    Actor, ApprovalId, Budget, EndReason, ErrorCode, ErrorDetail, Grants, Limits, Need, OpError, PauseReason,
    PendingApproval, SessionId, SessionInfo, SessionSpec, SessionState, SessionTx, Sessions, Spend, StopStep, Stopped,
    TxId,
};
use rmcp::ErrorData as McpError;
use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResponse,
    GetPromptResult, JsonObject, ListPromptsResult, ListToolsResult, MetaObject, PaginatedRequestParams, Prompt,
    PromptMessage, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ResourceContents, Role,
    ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use serde_json::{Value, json};

// ----- the scripted server -------------------------------------------------------------------------------------

/// What a scripted tool does when called: its result, and optionally a gate it waits on first.
#[derive(Clone)]
pub struct Outcome {
    pub result: CallToolResult,
    pub gate: Option<Arc<tokio::sync::Notify>>,
}

impl Outcome {
    pub fn text(text: &str) -> Self {
        Self { result: CallToolResult::success(vec![ContentBlock::text(text)]), gate: None }
    }

    pub fn failure(text: &str) -> Self {
        Self { result: CallToolResult::error(vec![ContentBlock::text(text)]), gate: None }
    }

    /// An error as `omuzaki-mcp` renders it: `structuredContent { ok: false, code, message, hint, detail }`.
    pub fn refusal(code: &str, detail: &ErrorDetail) -> Self {
        let data =
            json!({"ok": false, "op": "x", "code": code, "message": format!("{code}!"), "hint": "h", "detail": detail});
        let mut result = CallToolResult::error(vec![ContentBlock::text(format!("x failed [{code}]"))]);
        result.structured_content = Some(data);
        Self { result, gate: None }
    }

    pub fn image(png: &[u8]) -> Self {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(png);
        Self { result: CallToolResult::success(vec![ContentBlock::image(data, "image/png")]), gate: None }
    }

    pub fn held(mut self, gate: &Arc<tokio::sync::Notify>) -> Self {
        self.gate = Some(Arc::clone(gate));
        self
    }
}

/// Called with the name and arguments of every tool call, before it is answered.
pub type CallHook = Arc<dyn Fn(&str, &JsonObject) + Send + Sync>;

type Behaviour = Arc<dyn Fn(&JsonObject, usize) -> Outcome + Send + Sync>;

/// One tool of the scripted server.
pub struct ToolSpec {
    pub tool: Tool,
    pub behaviour: Behaviour,
}

pub fn tool(name: &str, title: &str, kind: &str, read_only: bool) -> Tool {
    let schema = json!({"type": "object", "additionalProperties": true});
    let meta = MetaObject::from(JsonObject::from_iter([("omuzaki/kind".to_owned(), json!(kind))]));
    let mut tool =
        Tool::new(name.to_owned(), format!("{name} does its work."), schema.as_object().cloned().unwrap_or_default())
            .with_title(title)
            .with_meta(meta);
    if read_only {
        tool = tool.with_annotations(ToolAnnotations::new().read_only(true));
    }
    tool
}

/// A tool with no `_meta` kind, as an older server lists it.
pub fn kindless(name: &str, read_only: bool) -> Tool {
    let schema = json!({"type": "object", "additionalProperties": true});
    let mut tool = Tool::new(name.to_owned(), "an old tool", schema.as_object().cloned().unwrap_or_default());
    if read_only {
        tool = tool.with_annotations(ToolAnnotations::new().read_only(true));
    }
    tool
}

#[derive(Default)]
pub struct ServerLog {
    /// Every call, in arrival order: the tool and its arguments.
    pub calls: Vec<(String, Value)>,
    pub in_flight: usize,
    pub max_in_flight: usize,
    pub cancelled: usize,
    pub prompt_lists: usize,
    pub prompt_gets: Vec<(String, Value)>,
}

#[derive(Clone)]
pub struct ScriptedServer {
    pub tools: Arc<Mutex<Vec<ToolSpec>>>,
    pub log: Arc<Mutex<ServerLog>>,
    pub prompts: Arc<Mutex<Vec<(Prompt, String)>>>,
    pub prompt_failure: Arc<Mutex<Option<String>>>,
    pub prompt_gate: Arc<Mutex<Option<Arc<tokio::sync::Notify>>>>,
    pub resources: Arc<Mutex<BTreeMap<String, ResourceContents>>>,
    pub instructions: String,
    pub on_call: Arc<Mutex<Option<CallHook>>>,
    counts: Arc<Mutex<BTreeMap<String, usize>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ScriptedServer {
    pub fn new() -> Self {
        Self {
            tools: Arc::default(),
            log: Arc::default(),
            prompts: Arc::default(),
            prompt_failure: Arc::default(),
            prompt_gate: Arc::default(),
            resources: Arc::default(),
            instructions: "Server instructions.".to_owned(),
            on_call: Arc::default(),
            counts: Arc::default(),
        }
    }

    pub fn add(&self, tool: Tool, behaviour: impl Fn(&JsonObject, usize) -> Outcome + Send + Sync + 'static) {
        let mut tools = lock(&self.tools);
        // A tool added again replaces the earlier one of its name.
        tools.retain(|t| t.tool.name != tool.name);
        tools.push(ToolSpec { tool, behaviour: Arc::new(behaviour) });
    }

    pub fn add_fixed(&self, tool: Tool, outcome: Outcome) {
        self.add(tool, move |_, _| outcome.clone());
    }

    pub fn calls(&self) -> Vec<(String, Value)> {
        lock(&self.log).calls.clone()
    }

    pub fn called(&self, name: &str) -> usize {
        lock(&self.log).calls.iter().filter(|(n, _)| n == name).count()
    }
}

// rmcp's trait methods are async; this scripted server has nothing to await in most of them.
#[allow(clippy::unused_async_trait_impl)]
impl ServerHandler for ScriptedServer {
    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder().enable_tools().enable_prompts().enable_resources().build();
        ServerConfig::new(capabilities).with_instructions(self.instructions.clone())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(lock(&self.tools).iter().map(|t| t.tool.clone()).collect()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        let args = request.arguments.unwrap_or_default();
        let (behaviour, index) = {
            let tools = lock(&self.tools);
            let spec = tools
                .iter()
                .find(|t| t.tool.name == name.as_str())
                .ok_or_else(|| McpError::invalid_params(format!("unknown tool {name}"), None))?;
            let mut counts = lock(&self.counts);
            let index = *counts.entry(name.clone()).and_modify(|n| *n += 1).or_insert(0);
            (Arc::clone(&spec.behaviour), index)
        };
        {
            let mut log = lock(&self.log);
            log.calls.push((name.clone(), Value::Object(args.clone())));
            log.in_flight += 1;
            log.max_in_flight = log.max_in_flight.max(log.in_flight);
        }
        let hook = lock(&self.on_call).clone();
        if let Some(hook) = hook {
            hook(&name, &args);
        }
        let outcome = behaviour(&args, index);
        if let Some(gate) = &outcome.gate {
            gate.notified().await;
        }
        lock(&self.log).in_flight -= 1;
        Ok(outcome.result.into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        lock(&self.log).prompt_lists += 1;
        if let Some(why) = lock(&self.prompt_failure).clone() {
            return Err(McpError::internal_error(why, None));
        }
        Ok(ListPromptsResult::with_all_items(lock(&self.prompts).iter().map(|(p, _)| p.clone()).collect()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        lock(&self.log)
            .prompt_gets
            .push((request.name.clone(), Value::Object(request.arguments.clone().unwrap_or_default())));
        let gate = lock(&self.prompt_gate).clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        let text = lock(&self.prompts)
            .iter()
            .find(|(p, _)| p.name == request.name)
            .map(|(_, text)| text.clone())
            .ok_or_else(|| McpError::invalid_params(format!("unknown prompt {}", request.name), None))?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]).into())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let contents = lock(&self.resources)
            .get(&request.uri)
            .cloned()
            .ok_or_else(|| McpError::invalid_params(format!("unknown resource {}", request.uri), None))?;
        Ok(ReadResourceResult::new(vec![contents]).into())
    }
}

/// A minimal JSON-RPC MCP server: it holds `render_preview` until the client cancels it, then answers anyway.
async fn late_server(io: tokio::io::DuplexStream) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (read, mut write) = tokio::io::split(io);
    let mut lines = BufReader::new(read).lines();
    let tools = json!([{
        "name": "render_preview",
        "description": "renders",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": true},
        "_meta": {"omuzaki/kind": "query"}
    }]);
    let mut held: Option<Value> = None;
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let id = msg.get("id").cloned();
        let reply = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
        let answer = match msg["method"].as_str() {
            Some("initialize") => Some(reply(json!({
                "protocolVersion": msg["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "late", "version": "1"}
            }))),
            Some("tools/list") => Some(reply(json!({"tools": tools}))),
            Some("tools/call") => {
                held = id.clone();
                None
            }
            Some("notifications/cancelled") => held.take().map(|held_id| {
                json!({"jsonrpc": "2.0", "id": held_id, "result": {"content": [{"type": "text", "text": "rendered"}]}})
            }),
            Some(_) if id.is_some() => Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no such method"}})),
            _ => None,
        };
        if let Some(answer) = answer
            && write.write_all(format!("{answer}\n").as_bytes()).await.is_err()
        {
            return;
        }
    }
}

// ----- the fake sessions ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum StopBehaviour {
    /// Returns `Committed(tx)` after the hold.
    Committed,
    /// Returns `Pending`; the open transaction closes `close_after` seconds of clock time later.
    Pending { close_after: f64 },
}

struct SessionsState {
    info: SessionInfo,
    /// Micro-dollars the budget still allows; `None` is unlimited.
    left: Option<u64>,
    next_approval: u64,
    approvals: Vec<PendingApproval>,
    charges: Vec<Spend>,
    budget_requests: Vec<Spend>,
    stop_calls: Vec<Actor>,
    resume_calls: Vec<Actor>,
    pause_calls: usize,
    end_calls: Vec<EndReason>,
    stop_behaviour: StopBehaviour,
    stop_hold: f64,
    close_open_tx_at: Option<f64>,
    /// A pending stop reverts every edit, so closing it makes no step.
    pending_reverts: bool,
    session_polls: usize,
}

/// A recording `Sessions`: one session, a settable budget, a settable state.
pub struct FakeSessions {
    state: Mutex<SessionsState>,
    clock: Arc<dyn Clock>,
}

pub fn person() -> Actor {
    Actor::Person { name: "sam".into() }
}

impl FakeSessions {
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        let info = SessionInfo {
            id: SessionId(1),
            actor: Actor::Agent { id: "assistant-1".into(), model: None },
            on_behalf_of: person(),
            transport: omuzaki_ops_api::Transport::InApp,
            client: "assistant".into(),
            model: None,
            parent: None,
            role: Some("assistant".into()),
            grants: Grants::agent_default(),
            fence: Vec::new(),
            budget: Budget { usd_micros: 1_000_000, tokens: None, seconds: None },
            spent: Spend::default(),
            limits: Limits::default(),
            state: SessionState::Active,
            open_tx: None,
            pending: Vec::new(),
            steps: 0,
            started_at: Seconds(0.0),
            last_active_at: Seconds(0.0),
        };
        let state = SessionsState {
            info,
            left: None,
            next_approval: 7,
            approvals: Vec::new(),
            charges: Vec::new(),
            budget_requests: Vec::new(),
            stop_calls: Vec::new(),
            resume_calls: Vec::new(),
            pause_calls: 0,
            end_calls: Vec::new(),
            stop_behaviour: StopBehaviour::Committed,
            stop_hold: 0.0,
            close_open_tx_at: None,
            pending_reverts: false,
            session_polls: 0,
        };
        Arc::new(Self { state: Mutex::new(state), clock })
    }

    fn st(&self) -> std::sync::MutexGuard<'_, SessionsState> {
        lock(&self.state)
    }

    // ----- what tests set
    pub fn set_left(&self, left: Option<u64>) {
        self.st().left = left;
    }

    pub fn set_state(&self, state: SessionState) {
        self.st().info.state = state;
    }

    pub fn set_stop(&self, behaviour: StopBehaviour, hold: f64) {
        let mut st = self.st();
        st.stop_behaviour = behaviour;
        st.stop_hold = hold;
    }

    /// A pending stop will revert all of the transaction's edits, so no undo step results.
    pub fn set_pending_stop_reverts(&self) {
        self.st().pending_reverts = true;
    }

    /// The open transaction, if any.
    pub fn open_tx(&self) -> Option<SessionTx> {
        self.st().info.open_tx.clone()
    }

    /// An open transaction closed by a commit: one more step.
    pub fn commit_open_tx(&self) {
        let mut st = self.st();
        if st.info.open_tx.take().is_some() {
            st.info.steps += 1;
        }
    }

    pub fn set_open_tx(&self, tx: Option<SessionTx>) {
        self.st().info.open_tx = tx;
    }

    pub fn revoke(&self) {
        let reason = EndReason::Revoked { by: person(), credential: true };
        self.st().info.state = SessionState::Ended { reason, at: Seconds(0.0) };
    }

    /// The person raises the budget on the pending approval: it resolves and the session runs again.
    pub fn raise_budget(&self) {
        let mut st = self.st();
        st.approvals.clear();
        st.left = None;
        st.info.state = SessionState::Active;
    }

    pub fn reject_budget(&self) {
        self.st().approvals.clear();
    }

    /// An approval the person has not decided yet, for `approval_wait` to wait on.
    pub fn queue_approval(&self, id: u64) {
        let mut st = self.st();
        let actor = st.info.actor.clone();
        st.approvals.push(PendingApproval {
            id: ApprovalId(id),
            session: SessionId(1),
            actor,
            request: None,
            need: Need::Person { reason: "r".into() },
            batch: "b".into(),
            requested_at: Seconds(0.0),
        });
    }

    /// The person decides every pending approval.
    pub fn resolve_approvals(&self) {
        self.st().approvals.clear();
    }

    // ----- what tests read
    pub fn charges(&self) -> Vec<Spend> {
        self.st().charges.clone()
    }

    pub fn budget_requests(&self) -> Vec<Spend> {
        self.st().budget_requests.clone()
    }

    pub fn session_polls(&self) -> usize {
        self.st().session_polls
    }

    pub fn stop_calls(&self) -> usize {
        self.st().stop_calls.len()
    }

    pub fn stop_principals(&self) -> Vec<Actor> {
        self.st().stop_calls.clone()
    }

    pub fn resume_calls(&self) -> Vec<Actor> {
        self.st().resume_calls.clone()
    }

    pub fn pause_calls(&self) -> usize {
        self.st().pause_calls
    }

    pub fn end_calls(&self) -> Vec<EndReason> {
        self.st().end_calls.clone()
    }

    fn unsupported<T>() -> Result<T, OpError> {
        Err(OpError::unsupported("not in the fake"))
    }
}

impl Sessions for FakeSessions {
    fn open(&self, _spec: SessionSpec) -> Result<SessionInfo, OpError> {
        Self::unsupported()
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        vec![self.st().info.clone()]
    }

    fn session(&self, id: SessionId) -> Option<SessionInfo> {
        let mut st = self.st();
        st.session_polls += 1;
        if id != st.info.id {
            return None;
        }
        // A transaction a stopping executor is still closing.
        if let Some(at) = st.close_open_tx_at
            && self.clock.now().0 >= at
        {
            st.info.open_tx = None;
            st.close_open_tx_at = None;
            if !st.pending_reverts {
                st.info.steps += 1;
            }
        }
        Some(st.info.clone())
    }

    fn session_of(&self, _actor: &Actor) -> Option<SessionInfo> {
        Some(self.st().info.clone())
    }

    fn pause(&self, _id: SessionId, _by: &Actor) -> Result<SessionInfo, OpError> {
        let mut st = self.st();
        st.pause_calls += 1;
        Ok(st.info.clone())
    }

    fn resume(&self, _id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        let mut st = self.st();
        st.resume_calls.push(by.clone());
        st.info.state = SessionState::Active;
        Ok(st.info.clone())
    }

    fn set_grants(&self, _id: SessionId, _grants: Grants, _by: &Actor) -> Result<SessionInfo, OpError> {
        Self::unsupported()
    }

    fn set_budget(&self, _id: SessionId, _budget: Budget, _by: &Actor) -> Result<SessionInfo, OpError> {
        Self::unsupported()
    }

    fn charge(&self, _id: SessionId, spend: Spend) -> Result<SessionInfo, OpError> {
        let mut st = self.st();
        if matches!(st.info.state, SessionState::Ended { .. }) {
            return Err(OpError {
                code: ErrorCode::Conflict,
                message: "the session has ended".into(),
                field: None,
                hint: None,
                detail: None,
            });
        }
        st.charges.push(spend);
        st.info.spent.usd_micros += spend.usd_micros;
        st.info.spent.tokens += spend.tokens;
        if let Some(left) = st.left.as_mut() {
            *left = left.saturating_sub(spend.usd_micros);
        }
        Ok(st.info.clone())
    }

    fn end(&self, _id: SessionId, reason: EndReason) -> Result<omuzaki_ops_api::Ended, OpError> {
        let mut st = self.st();
        st.end_calls.push(reason.clone());
        st.info.state = SessionState::Ended { reason, at: Seconds(0.0) };
        Ok(omuzaki_ops_api::Ended { abandoned: vec![], kept: vec![], withdrawn: vec![], cancelled: vec![] })
    }

    fn undo_session(&self, _id: SessionId, _by: &Actor) -> Result<omuzaki_ops_api::SessionUndo, OpError> {
        Self::unsupported()
    }

    fn approvals(&self) -> Vec<PendingApproval> {
        self.st().approvals.clone()
    }

    fn resolve(
        &self,
        _id: ApprovalId,
        _decision: omuzaki_ops_api::Decision,
        _by: &Actor,
    ) -> Result<omuzaki_ops_api::Resolution, OpError> {
        Self::unsupported()
    }

    fn request_budget(&self, id: SessionId, needed: Spend) -> Result<(), OpError> {
        let mut st = self.st();
        st.budget_requests.push(needed);
        let fits = st.left.is_none_or(|left| needed.usd_micros <= left);
        if fits {
            return Ok(());
        }
        let approval = ApprovalId(st.next_approval);
        st.next_approval += 1;
        let need = Need::Budget { needed, left: Spend { usd_micros: st.left.unwrap_or(0), tokens: 0, seconds: 0 } };
        let actor = st.info.actor.clone();
        st.approvals.push(PendingApproval {
            id: approval,
            session: id,
            actor,
            request: None,
            need: need.clone(),
            batch: "budget".into(),
            requested_at: Seconds(0.0),
        });
        let by = st.info.actor.clone();
        st.info.state = SessionState::Paused { by, reason: PauseReason::Budget };
        Err(OpError {
            code: ErrorCode::Waiting,
            message: "waiting".into(),
            field: None,
            hint: None,
            detail: Some(Box::new(ErrorDetail::Queued { approval, need })),
        })
    }

    fn stop(&self, _id: SessionId, by: &Actor) -> Result<Stopped, OpError> {
        let (hold, behaviour) = {
            let mut st = self.st();
            st.stop_calls.push(by.clone());
            if !matches!(st.info.state, SessionState::Paused { .. }) {
                st.info.state = SessionState::Paused { by: by.clone(), reason: PauseReason::Person };
            }
            (st.stop_hold, st.stop_behaviour)
        };
        // An op holding the executor's lock delays the control; the test moves the clock.
        let until = self.clock.now().0 + hold;
        while self.clock.now().0 < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut st = self.st();
        let had_tx = st.info.open_tx.take();
        match behaviour {
            StopBehaviour::Committed => {
                if had_tx.is_some() {
                    st.info.steps += 1;
                }
                let step = had_tx.map_or(StopStep::None, |t| StopStep::Committed(t.tx));
                Ok(Stopped { step, jobs: vec![] })
            }
            StopBehaviour::Pending { close_after } => {
                st.info.open_tx = had_tx;
                st.close_open_tx_at = Some(self.clock.now().0 + close_after);
                Ok(Stopped { step: StopStep::Pending, jobs: vec![] })
            }
        }
    }
}

// ----- the recording sink --------------------------------------------------------------------------------------

#[derive(Default)]
pub struct RecordingSink {
    entries: Mutex<Vec<Entry>>,
    /// Makes `put_blob` fail, as a full disk would.
    pub fail_blobs: std::sync::atomic::AtomicBool,
    /// Makes `put_blob` wait (and say so in `blob_waiting`) until this is cleared, so a test can act mid-store.
    pub hold_blobs: std::sync::atomic::AtomicBool,
    pub blob_waiting: std::sync::atomic::AtomicBool,
    /// Makes the append of a `Step` wait (and say so in `step_waiting`) until this is cleared.
    pub hold_steps: std::sync::atomic::AtomicBool,
    pub step_waiting: std::sync::atomic::AtomicBool,
}

impl RecordingSink {
    pub fn entries(&self) -> Vec<Entry> {
        lock(&self.entries).clone()
    }
}

impl EntrySink for RecordingSink {
    fn append(&self, entry: &Entry) -> Result<(), HostError> {
        if matches!(entry, Entry::Step { .. }) {
            while self.hold_steps.load(std::sync::atomic::Ordering::SeqCst) {
                self.step_waiting.store(true, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        lock(&self.entries).push(entry.clone());
        Ok(())
    }

    fn put_blob(&self, bytes: &[u8], mime: ImageMime) -> Result<StoredBlock, HostError> {
        while self.hold_blobs.load(std::sync::atomic::Ordering::SeqCst) {
            self.blob_waiting.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if self.fail_blobs.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(HostError::Store("the disk is full".into()));
        }
        Ok(StoredBlock::image(mime, format!("{:x}", bytes.len())))
    }
}

// ----- the rig -------------------------------------------------------------------------------------------------

/// The design's section 4 prices for `claude-haiku-5-5`, µ$ per million tokens.
pub fn haiku_prices() -> PriceTable {
    let rates = Rates::new(100_000, 500_000).with_cache_read(10_000).with_cache_write(125_000);
    PriceTable::new("2026-10-01", "design section 4", vec![PriceTier::new(None, rates)])
}

pub fn free_prices() -> PriceTable {
    PriceTable::new("2026-10-01", "local", vec![PriceTier::new(None, Rates::new(0, 0))])
}

// ----- a stream that can be held mid-turn --------------------------------------------------------------------------

/// Holds every turn of the wrapped client after `after` events until [`Gate::open`] is called, so a test can act while
/// a stream is in flight.
pub struct Gate {
    open: std::sync::atomic::AtomicBool,
    pub delivered: AtomicUsize,
    after: usize,
}

impl Gate {
    pub fn open(&self) {
        self.open.store(true, Ordering::Release);
    }
}

pub struct GatedLlm {
    inner: Arc<FakeLlm>,
    gate: Arc<Gate>,
    info: ModelInfo,
}

struct Held {
    stream: TurnStream,
    gate: Arc<Gate>,
}

impl EventSource for Held {
    fn poll(&mut self, wait: Duration) -> Result<Next, LlmError> {
        let held =
            !self.gate.open.load(Ordering::Acquire) && self.gate.delivered.load(Ordering::Acquire) >= self.gate.after;
        if held {
            std::thread::sleep(Duration::from_millis(1));
            return Ok(Next::Wait);
        }
        let next = self.stream.next_timeout(wait)?;
        if matches!(next, Next::Event(_)) {
            self.gate.delivered.fetch_add(1, Ordering::AcqRel);
        }
        Ok(next)
    }
}

impl LlmClient for GatedLlm {
    fn info(&self) -> &ModelInfo {
        &self.info
    }

    fn stream(&self, req: TurnRequest, cancel: &Cancel) -> Result<TurnStream, LlmError> {
        let stream = self.inner.stream(req, cancel)?;
        Ok(TurnStream::new(Box::new(Held { stream, gate: Arc::clone(&self.gate) }), cancel))
    }
}

pub struct Rig {
    pub rt: tokio::runtime::Runtime,
    pub server: ScriptedServer,
    pub sessions: Arc<FakeSessions>,
    pub sink: Arc<RecordingSink>,
    pub llm: Arc<FakeLlm>,
    pub clock: Arc<FakeClock>,
    pub host: AssistantHost,
    pub rx: EventReceiver,
    pub gate: Arc<Gate>,
    pub server_peer: Arc<Mutex<Option<rmcp::service::Peer<RoleServer>>>>,
}

pub struct RigOptions {
    pub caps: Caps,
    pub prices: PriceTable,
    pub history: Vec<Entry>,
    pub person_reply: Option<String>,
    pub max_output: u32,
    /// Hold each stream after this many events until `rig.gate.open()`.
    pub hold_after: Option<usize>,
    /// The model's context window, when a test needs a small one.
    pub window: Option<u64>,
    /// The reply language.
    pub language: Option<omuzaki_agent::Language>,
    /// Serve a hand-written JSON-RPC server that answers `render_preview` even after the call was cancelled, as a
    /// server that finished the work first does. rmcp's own server never does.
    pub late_server: bool,
    /// The sink refuses to keep images.
    pub fail_blobs: bool,
    /// The sink holds `put_blob` until `rig.sink.hold_blobs` is cleared.
    pub hold_blobs: bool,
}

impl Default for RigOptions {
    fn default() -> Self {
        Self {
            caps: Caps::default(),
            prices: haiku_prices(),
            history: Vec::new(),
            person_reply: None,
            max_output: 2_048,
            hold_after: None,
            window: None,
            language: None,
            late_server: false,
            fail_blobs: false,
            hold_blobs: false,
        }
    }
}

/// The usual tools: a query, an edit, a job and the transaction built-ins, with `tx_begin` and `tx_commit` keeping the
/// fake session's open transaction in step like the executor does.
pub fn standard_server(sessions: &Arc<FakeSessions>) -> ScriptedServer {
    let server = ScriptedServer::new();
    server.add_fixed(tool("render_preview", "Render preview", "query", true), Outcome::text("rendered"));
    server.add_fixed(tool("face_set_profile", "Face profile", "edit", false), Outcome::text("applied"));
    server.add_fixed(tool("export_video", "Export video", "job", false), Outcome::text("job done"));
    server.add_fixed(tool("script_run", "Run script", "driver_job", false), Outcome::text("script done"));
    server.add_fixed(tool("history_undo", "history.undo", "builtin", false), Outcome::text("Undid it."));
    server.add_fixed(tool("history_redo", "history.redo", "builtin", false), Outcome::text("Redid it."));
    server.add(tool("approval_wait", "approval.wait", "builtin", false), |_, _| {
        // The real tool holds the call for up to 20 s; a short real pause keeps the host's loop from spinning.
        std::thread::sleep(Duration::from_millis(5));
        Outcome::text("approval applied")
    });
    // The transaction tools change the fake session only when they succeed, so a test that replaces one with a refusal
    // leaves the transaction open, like the executor does.
    let begin = Arc::clone(sessions);
    server.add(tool("tx_begin", "tx.begin", "builtin", false), move |args, _| {
        let label = args.get("label").and_then(Value::as_str).unwrap_or("").to_owned();
        begin.set_open_tx(Some(SessionTx { tx: TxId(31), label }));
        Outcome::text("tx started")
    });
    let commit = Arc::clone(sessions);
    server.add(tool("tx_commit", "tx.commit", "builtin", false), move |_, _| {
        commit.commit_open_tx();
        Outcome::text("tx committed")
    });
    let rollback = Arc::clone(sessions);
    server.add(tool("tx_rollback", "tx.rollback", "builtin", false), move |_, _| {
        rollback.set_open_tx(None);
        Outcome::text("tx rolled back")
    });
    server
}

impl Rig {
    pub fn new(script: Script) -> Self {
        Self::with(script, RigOptions::default(), standard_server)
    }

    pub fn with(
        script: Script,
        options: RigOptions,
        make_server: impl FnOnce(&Arc<FakeSessions>) -> ScriptedServer,
    ) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("runtime");
        let llm = Arc::new(FakeLlm::new(script));
        let clock = llm.clock();
        let sessions = FakeSessions::new(clock.clone());
        let server = make_server(&sessions);
        let sink = Arc::new(RecordingSink::default());
        sink.fail_blobs.store(options.fail_blobs, std::sync::atomic::Ordering::SeqCst);
        sink.hold_blobs.store(options.hold_blobs, std::sync::atomic::Ordering::SeqCst);
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let serving = server.clone();
        // `serve` returns only once the client has shaken hands, so it runs on the runtime while the client connects.
        let server_peer: Arc<Mutex<Option<rmcp::service::Peer<RoleServer>>>> = Arc::default();
        let peer_slot = Arc::clone(&server_peer);
        if options.late_server {
            rt.spawn(late_server(server_io));
        } else {
            rt.spawn(async move {
                if let Ok(service) = serving.serve(server_io).await {
                    *lock(&peer_slot) = Some(service.peer().clone());
                    let _ = service.waiting().await;
                }
            });
        }
        let link =
            McpClientLink::connect(client_io, &RuntimeHandle::new(rt.handle().clone())).expect("client connects");
        let mut cfg = HostConfig::new(person(), sink.clone(), clock.clone(), options.prices)
            .with_caps(options.caps)
            .with_history(options.history)
            .with_max_output(options.max_output);
        if let Some(language) = options.language {
            cfg = cfg.with_language(language);
        }
        if let Some(reply) = options.person_reply {
            cfg = cfg.with_person_reply(reply);
        }
        let gate = Arc::new(Gate {
            open: std::sync::atomic::AtomicBool::new(options.hold_after.is_none()),
            delivered: AtomicUsize::new(0),
            after: options.hold_after.unwrap_or(0),
        });
        let mut info = llm.info().clone();
        if let Some(window) = options.window {
            info.context_window = window;
        }
        let client: Arc<dyn LlmClient> = Arc::new(GatedLlm { inner: llm.clone(), gate: Arc::clone(&gate), info });
        let host = AssistantHost::start(link, client, sessions.clone(), SessionId(1), cfg);
        let rx = host.events();
        Self { rt, server, sessions, sink, llm, clock, host, rx, gate, server_peer }
    }

    /// Sends `text` and returns every event up to `Done`, advancing the fake clock by `tick` seconds per millisecond of
    /// real time while it waits, so waits that run on the clock finish.
    pub fn say(&self, text: &str, tick: f64) -> Vec<AssistantEvent> {
        self.host.send(UserMessage::new(text)).expect("accepted");
        self.until_done(tick)
    }

    pub fn until_done(&self, tick: f64) -> Vec<AssistantEvent> {
        let mut events = Vec::new();
        let started = Instant::now();
        loop {
            while let Some(event) = self.rx.try_recv() {
                let done = matches!(event, AssistantEvent::Done);
                events.push(event);
                if done {
                    return events;
                }
            }
            assert!(started.elapsed() < Duration::from_secs(20), "no Done within 20 s; events so far: {events:?}");
            if tick > 0.0 {
                self.clock.advance(Seconds(tick));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Waits (real time) until `check` is true.
    pub fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
        let started = Instant::now();
        while !check() {
            assert!(started.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

// ----- script helpers ------------------------------------------------------------------------------------------

pub static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub fn usage(input: u64, output: u64) -> LlmEvent {
    LlmEvent::Usage(Usage::new(input, output))
}

/// A turn that says `text` and ends.
pub fn say_turn(text: &str) -> ScriptTurn {
    ScriptTurn::new(vec![
        LlmEvent::start("m"),
        LlmEvent::TextDelta(text.into()),
        usage(10, 5),
        LlmEvent::Stop(StopReason::EndTurn),
    ])
}

/// A turn that calls tools: `(id, name, json arguments)`.
pub fn call_turn(text: &str, calls: &[(&str, &str, &str)]) -> ScriptTurn {
    let mut events = vec![LlmEvent::start("m")];
    if !text.is_empty() {
        events.push(LlmEvent::TextDelta(text.into()));
    }
    for (id, name, args) in calls {
        events.push(LlmEvent::tool_use_start(*id, *name));
        events.push(LlmEvent::ToolUseInput((*args).into()));
        events.push(LlmEvent::tool_use_end(*id));
    }
    events.push(usage(10, 5));
    events.push(LlmEvent::Stop(StopReason::ToolUse));
    ScriptTurn::new(events)
}

pub fn script(turns: Vec<ScriptTurn>) -> Script {
    turns.into_iter().fold(Script::new("m"), Script::with_turn)
}

/// The texts of every `TextDelta` in `events`, joined.
pub fn text_of(events: &[AssistantEvent]) -> String {
    events.iter().filter_map(|e| if let AssistantEvent::TextDelta(t) = e { Some(t.as_str()) } else { None }).collect()
}

pub fn count(events: &[AssistantEvent], pick: impl Fn(&AssistantEvent) -> bool) -> usize {
    events.iter().filter(|e| pick(e)).count()
}
