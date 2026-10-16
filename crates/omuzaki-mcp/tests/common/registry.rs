//! A small stand-in for the executor's session machinery (`omuzaki-ops`), so the MCP tests can drive sessions, pause,
//! approvals, fences, rate limits and the stale-write guard through a real rmcp client. It wraps the shared in-memory
//! runner: the runner does the editing, this adds what the executor adds around it.
//!
//! It is a test oracle for the MCP crate's behaviour, not a second executor: it implements only the rules the MCP tests
//! lean on (design `agent-sessions.md` §4 to §8), in the simplest form that shows them.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{
    Actor, ApprovalId, ApprovalRequest, Budget, Decision, EndReason, Ended, ErrorCode, ErrorDetail, Event, Grants,
    Mode, Need, OpError, OpInfo, OpKind, OpRecord, Outcome, PauseReason, PendingApproval, Plan, Request, Resolution,
    Resolved, Runner, SessionChange, SessionId, SessionInfo, SessionSpec, SessionState, SessionTx, SessionUndo,
    Sessions, Spend, Transport, TxId,
};

use super::{FakeRunner, fake_runner, fake_runner_with};

/// What the registry does with a new HTTP session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pairing {
    /// Admit it active.
    #[default]
    Off,
    /// Admit it paused until a person pairs it (design §10.2).
    Required,
}

#[derive(Default)]
struct State {
    next_session: u64,
    next_approval: u64,
    sessions: Vec<SessionInfo>,
    opened: Vec<SessionSpec>,
    ended: Vec<(SessionId, EndReason, Ended)>,
    approvals: Vec<PendingApproval>,
    /// The request each approval would run if approved.
    queued: BTreeMap<ApprovalId, Request>,
    sinks: Vec<Sender<Event>>,
    /// The scopes the person holds: a session never gets more (design §3.3).
    principal: Option<Grants>,
    pairing: Pairing,
    /// Rate-limit refusals still to hand out, each telling the caller to retry after this many seconds.
    limited: Vec<f32>,
    /// How many questions about sessions were asked (a test can see that a session was looked up).
    runs: usize,
}

/// The runner and the registry in one object, like the executor.
pub struct Studio {
    pub runner: Arc<FakeRunner>,
    state: Mutex<State>,
    /// How long the runner's own events are held back before they reach a subscriber, to model a slow event feed.
    feed_delay: Arc<Mutex<std::time::Duration>>,
}

impl Studio {
    pub fn new() -> Arc<Self> {
        Self::around(fake_runner())
    }

    /// A studio over a document of its own.
    pub fn with_document(doc: Value) -> Arc<Self> {
        Self::around(fake_runner_with(doc))
    }

    fn around(runner: FakeRunner) -> Arc<Self> {
        let studio = Arc::new(Self {
            runner: Arc::new(runner),
            state: Mutex::new(State::default()),
            feed_delay: Arc::default(),
        });
        studio.lock().next_session = 1;
        studio.lock().next_approval = 1;
        studio
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // ---- what a test can do -------------------------------------------------------------------------------

    /// The person's own scopes; sessions are capped by them.
    pub fn person_holds(&self, grants: Grants) {
        self.lock().principal = Some(grants);
    }

    /// Holds the runner's events back by `delay` before they reach a subscriber, as a slow feed would.
    pub fn delay_events(&self, delay: std::time::Duration) {
        *self.feed_delay.lock().unwrap_or_else(PoisonError::into_inner) = delay;
    }

    pub fn require_pairing(&self) {
        self.lock().pairing = Pairing::Required;
    }

    /// The next `n` runs are refused with a rate limit that frees up after `seconds`.
    pub fn limit_next(&self, n: usize, seconds: f32) {
        self.lock().limited = vec![seconds; n];
    }

    /// Sets the fence of the session of `actor`, as the person would.
    pub fn fence(&self, actor: &Actor, paths: &[&str]) {
        let fence = paths.iter().map(|p| DocPath::parse(p).expect("valid")).collect();
        let mut state = self.lock();
        let session = state.sessions.iter_mut().find(|s| &s.actor == actor).expect("a session");
        session.fence = fence;
    }

    pub fn specs(&self) -> Vec<SessionSpec> {
        self.lock().opened.clone()
    }

    pub fn ends(&self) -> Vec<(SessionId, EndReason, Ended)> {
        self.lock().ended.clone()
    }

    pub fn session_count(&self) -> usize {
        self.lock().sessions.iter().filter(|s| !matches!(s.state, SessionState::Ended { .. })).count()
    }

    /// The agent actor of the only live session.
    pub fn only_actor(&self) -> Actor {
        let state = self.lock();
        let live: Vec<_> = state.sessions.iter().filter(|s| !matches!(s.state, SessionState::Ended { .. })).collect();
        assert_eq!(live.len(), 1, "one live session");
        live[0].actor.clone()
    }

    /// The registry ends a session on its own (a person revoked it, the project closed).
    pub fn registry_ends(&self, id: SessionId, reason: EndReason) {
        self.end_session(id, reason).expect("ends");
    }

    /// A person edits `mira`'s blend, as the UI would.
    pub fn person_sets_blend(&self, blend: f32) {
        use super::FakeRunnerExt;
        self.runner.person_sets_blend(blend);
    }

    pub fn emit(&self, event: &Event) {
        self.lock().sinks.retain(|sink| sink.send(event.clone()).is_ok());
    }

    pub fn person() -> Actor {
        Actor::Person { name: "alice".to_owned() }
    }

    // ---- the registry's own rules -------------------------------------------------------------------------

    fn snapshot(info: &SessionInfo) -> SessionInfo {
        info.clone()
    }

    fn end_session(&self, id: SessionId, reason: EndReason) -> Result<Ended, OpError> {
        let (info, tx) = {
            let mut state = self.lock();
            let session = state
                .sessions
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| OpError::not_found(format!("no session {}", id.0)))?;
            if matches!(session.state, SessionState::Ended { .. }) {
                return Err(OpError::conflict("the session already ended"));
            }
            let tx = session.open_tx.take();
            session.state = SessionState::Ended { reason: reason.clone(), at: Seconds(1.0) };
            (Self::snapshot(session), tx)
        };
        let mut ended = Ended { abandoned: Vec::new(), kept: Vec::new(), withdrawn: Vec::new(), cancelled: Vec::new() };
        if let Some(tx) = tx {
            // The transaction is abandoned through the runner, as the executor does.
            if self.runner.rollback(tx.tx, &info.actor).is_ok() {
                ended.abandoned.push(tx.tx);
            }
        }
        {
            let mut state = self.lock();
            let withdrawn: Vec<ApprovalId> = state.approvals.iter().filter(|a| a.session == id).map(|a| a.id).collect();
            state.approvals.retain(|a| a.session != id);
            ended.withdrawn = withdrawn;
            state.ended.push((id, reason.clone(), ended.clone()));
        }
        for approval in ended.withdrawn.clone() {
            self.emit(&Event::approval_resolved(Resolution {
                id: approval,
                by: None,
                result: Resolved::Withdrawn { reason: reason.clone() },
                at: Seconds(1.0),
            }));
        }
        self.emit(&Event::session_ended(info, ended.clone()));
        Ok(ended)
    }

    fn fence_check(&self, req: &Request, me: &SessionInfo) -> Result<(), OpError> {
        let Some(character) = req.args.get("character").and_then(Value::as_str) else { return Ok(()) };
        let path = DocPath::parse(&format!("characters/{character}")).expect("valid");
        let under = |fence: &DocPath| {
            path.to_string() == fence.to_string() || path.to_string().starts_with(&format!("{fence}/"))
        };
        let others: Vec<SessionInfo> = self
            .lock()
            .sessions
            .iter()
            .filter(|s| s.id != me.id && !matches!(s.state, SessionState::Ended { .. }))
            .cloned()
            .collect();
        if let Some(holder) =
            others.iter().find(|s| s.fence.iter().any(|f| under(f) || f.to_string().starts_with(&path.to_string())))
        {
            return Err(OpError::fenced(path, Some(holder.actor.clone())));
        }
        if !me.fence.is_empty() && !me.fence.iter().any(under) {
            return Err(OpError::fenced(path, None));
        }
        Ok(())
    }

    fn stale_check(&self, req: &Request) -> Result<(), OpError> {
        let Some(base) = req.base else { return Ok(()) };
        let Some(character) = req.args.get("character").and_then(Value::as_str) else { return Ok(()) };
        let path = DocPath::parse(&format!("characters/{character}/face/blend")).expect("valid");
        let records: Vec<OpRecord> = self.runner.log(0, 10_000, &Grants::person()).expect("log");
        let changed = records
            .iter()
            .rev()
            .find(|r| r.actor != req.actor && r.version > base && r.patch.changes.iter().any(|c| c.path == path));
        match changed {
            Some(record) => Err(OpError::conflict(format!("`{path}` changed after you last looked"))
                .hint("re-read it; your next call carries the newer base")
                .with_detail(ErrorDetail::Changed { path, by: record.actor.clone(), version: record.version })),
            None => Ok(()),
        }
    }

    fn waiting(by: Actor, reason: PauseReason) -> OpError {
        let mut error = OpError::conflict("this session is paused").with_detail(ErrorDetail::Paused { by, reason });
        error.code = ErrorCode::Waiting;
        error
    }
}

impl Runner for Studio {
    fn ops(&self) -> Vec<OpInfo> {
        self.runner.ops()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        let me = self.session_of(&req.actor);
        if let Some(me) = &me {
            self.lock().runs += 1;
            match &me.state {
                SessionState::Paused { by, reason } => return Err(Self::waiting(by.clone(), *reason)),
                SessionState::Ended { reason, .. } => {
                    return Err(OpError::denied("this session has ended")
                        .with_detail(ErrorDetail::Ended { reason: reason.clone() }));
                }
                SessionState::Active => {}
            }
            {
                let mut state = self.lock();
                if !state.limited.is_empty() {
                    let seconds = state.limited.remove(0);
                    let mut error = OpError::conflict("rate limited")
                        .with_detail(ErrorDetail::RetryAfter { seconds, limit: "edits per minute".to_owned() });
                    error.code = ErrorCode::Limited;
                    return Err(error);
                }
            }
            if req.mode == Mode::Apply {
                self.fence_check(&req, me)?;
                self.stale_check(&req)?;
            }
            // A scope the session lacks but its person holds is queued, not refused (design §7.1).
            let info = self.runner.ops().into_iter().find(|o| o.name == req.op);
            if let Some(info) = info
                && req.mode == Mode::Apply
                && !req.grants.allows(info.scope)
                && self.lock().principal.as_ref().is_none_or(|p| p.allows(info.scope))
            {
                return Err(self.queue(me, req, Need::Scope { scope: info.scope }, info.kind));
            }
        }
        self.runner.run(req)
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        let tx = self.runner.begin(label, actor);
        let mut state = self.lock();
        if let Some(session) = state.sessions.iter_mut().find(|s| &s.actor == actor) {
            session.open_tx = Some(SessionTx { tx, label: label.to_owned() });
        }
        tx
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        let result = self.runner.commit(tx, actor);
        self.forget_tx(actor, tx);
        result
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        let result = self.runner.rollback(tx, actor);
        self.forget_tx(actor, tx);
        result
    }

    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.runner.undo(actor, grants)
    }

    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.runner.redo(actor, grants)
    }

    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        let inner = self.runner.subscribe(grants)?;
        let (tx, rx) = channel();
        self.lock().sinks.push(tx.clone());
        // The runner's own events (ops applied, undo) join the ones this registry emits.
        let delay = Arc::clone(&self.feed_delay);
        std::thread::spawn(move || {
            for event in inner {
                let wait = *delay.lock().unwrap_or_else(PoisonError::into_inner);
                std::thread::sleep(wait);
                if tx.send(event).is_err() {
                    return;
                }
            }
        });
        Ok(rx)
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.runner.get(path, grants)
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.runner.log(after_seq, limit, grants)
    }
}

impl Studio {
    fn forget_tx(&self, actor: &Actor, tx: TxId) {
        let mut state = self.lock();
        if let Some(session) = state.sessions.iter_mut().find(|s| &s.actor == actor)
            && session.open_tx.as_ref().is_some_and(|open| open.tx == tx)
        {
            session.open_tx = None;
        }
    }

    /// Queues `req` for a person and returns the refusal that tells the agent so.
    fn queue(&self, me: &SessionInfo, req: Request, need: Need, kind: OpKind) -> OpError {
        let _ = kind;
        let approval = {
            let mut state = self.lock();
            let id = ApprovalId(state.next_approval);
            state.next_approval += 1;
            let pending = PendingApproval {
                id,
                session: me.id,
                actor: me.actor.clone(),
                request: Some(ApprovalRequest {
                    op: req.op.clone(),
                    args: req.args.clone(),
                    cause: req.cause.clone(),
                    preview: Plan::default(),
                }),
                need: need.clone(),
                batch: String::new(),
                requested_at: Seconds(1.0),
            };
            state.approvals.push(pending.clone());
            state.queued.insert(id, req);
            if let Some(session) = state.sessions.iter_mut().find(|s| s.id == me.id) {
                session.pending.push(id);
            }
            pending
        };
        self.emit(&Event::approval_requested(approval.clone()));
        let mut error =
            OpError::conflict("queued for a person").with_detail(ErrorDetail::Queued { approval: approval.id, need });
        error.code = ErrorCode::Waiting;
        error
    }
}

impl Sessions for Studio {
    fn open(&self, spec: SessionSpec) -> Result<SessionInfo, OpError> {
        let mut state = self.lock();
        let principal = state.principal.clone().unwrap_or_else(Grants::person);
        let parent = match spec.parent {
            Some(parent) => Some(
                state
                    .sessions
                    .iter()
                    .find(|s| s.id == parent)
                    .cloned()
                    .ok_or_else(|| OpError::not_found(format!("no parent session {}", parent.0)))?,
            ),
            None => None,
        };
        if let Some(parent) = &parent
            && spec.grants.scopes().any(|s| !parent.grants.allows(s))
        {
            return Err(OpError::denied("the session asks for more than its parent holds"));
        }
        let effective = spec.grants.intersect(&principal);
        let id = SessionId(state.next_session);
        state.next_session += 1;
        let pairing = state.pairing == Pairing::Required && matches!(spec.transport, Transport::Http { .. });
        let info = SessionInfo {
            id,
            actor: Actor::Agent { id: format!("{}#{}", spec.client, id.0), model: spec.model.clone() },
            on_behalf_of: spec.on_behalf_of.clone(),
            transport: spec.transport.clone(),
            client: spec.client.clone(),
            model: spec.model.clone(),
            parent: spec.parent,
            role: spec.role.clone(),
            grants: effective,
            fence: spec.fence.clone(),
            budget: spec.budget,
            spent: Spend::default(),
            limits: spec.limits,
            state: if pairing {
                SessionState::Paused { by: Actor::System, reason: PauseReason::Pairing }
            } else {
                SessionState::Active
            },
            open_tx: None,
            pending: Vec::new(),
            steps: 0,
            started_at: Seconds(0.0),
            last_active_at: Seconds(0.0),
        };
        state.opened.push(spec);
        state.sessions.push(info.clone());
        drop(state);
        self.emit(&Event::session_opened(info.clone()));
        if pairing {
            let id = {
                let mut state = self.lock();
                let id = ApprovalId(state.next_approval);
                state.next_approval += 1;
                let pending = PendingApproval {
                    id,
                    session: info.id,
                    actor: info.actor.clone(),
                    request: None,
                    need: Need::Pairing { client: info.client.clone() },
                    batch: String::new(),
                    requested_at: Seconds(1.0),
                };
                state.approvals.push(pending);
                if let Some(session) = state.sessions.iter_mut().find(|s| s.id == info.id) {
                    session.pending.push(id);
                }
                id
            };
            let _ = id;
        }
        Ok(self.session(info.id).expect("just opened"))
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        self.lock().sessions.clone()
    }

    fn session(&self, id: SessionId) -> Option<SessionInfo> {
        self.lock().sessions.iter().find(|s| s.id == id).cloned()
    }

    fn session_of(&self, actor: &Actor) -> Option<SessionInfo> {
        self.lock().sessions.iter().find(|s| &s.actor == actor).cloned()
    }

    fn pause(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        let info = {
            let mut state = self.lock();
            let session =
                state.sessions.iter_mut().find(|s| s.id == id).ok_or_else(|| OpError::not_found("no such session"))?;
            session.state = SessionState::Paused { by: by.clone(), reason: PauseReason::Person };
            session.clone()
        };
        self.emit(&Event::session_changed(info.clone(), SessionChange::Paused));
        Ok(info)
    }

    fn resume(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        let info = {
            let mut state = self.lock();
            let session =
                state.sessions.iter_mut().find(|s| s.id == id).ok_or_else(|| OpError::not_found("no such session"))?;
            session.state = SessionState::Active;
            session.clone()
        };
        self.emit(&Event::session_changed(info.clone(), SessionChange::Resumed { by: by.clone() }));
        Ok(info)
    }

    fn set_grants(&self, _: SessionId, _: Grants, _: &Actor) -> Result<SessionInfo, OpError> {
        Err(OpError::unsupported("not needed by these tests"))
    }

    fn set_budget(&self, _: SessionId, _: Budget, _: &Actor) -> Result<SessionInfo, OpError> {
        Err(OpError::unsupported("not needed by these tests"))
    }

    fn charge(&self, _: SessionId, _: Spend) -> Result<SessionInfo, OpError> {
        Err(OpError::unsupported("not needed by these tests"))
    }

    fn end(&self, id: SessionId, reason: EndReason) -> Result<Ended, OpError> {
        self.end_session(id, reason)
    }

    fn undo_session(&self, _: SessionId, _: &Actor) -> Result<SessionUndo, OpError> {
        Err(OpError::unsupported("not needed by these tests"))
    }

    fn approvals(&self) -> Vec<PendingApproval> {
        self.lock().approvals.clone()
    }

    fn resolve(&self, id: ApprovalId, decision: Decision, by: &Actor) -> Result<Resolution, OpError> {
        let (pending, request) = {
            let mut state = self.lock();
            let at = state.approvals.iter().position(|a| a.id == id).ok_or_else(|| OpError::not_found("gone"))?;
            let pending = state.approvals.remove(at);
            if let Some(session) = state.sessions.iter_mut().find(|s| s.id == pending.session) {
                session.pending.retain(|p| *p != id);
            }
            (pending, state.queued.remove(&id))
        };
        let result = match decision {
            Decision::Reject { comment } => Resolved::Rejected { comment },
            _ => match (&pending.need, request) {
                (Need::Pairing { .. }, _) => {
                    self.resume(pending.session, by)?;
                    Resolved::Applied { outcome: Outcome::new(Plan::default()) }
                }
                (Need::Scope { scope }, Some(mut request)) => {
                    request.grants = request.grants.with(*scope);
                    match self.runner.run(request) {
                        Ok(outcome) => Resolved::Applied { outcome },
                        Err(error) => Resolved::Failed { error },
                    }
                }
                _ => Resolved::Failed { error: OpError::internal("nothing to run") },
            },
        };
        let resolution = Resolution { id, by: Some(by.clone()), result, at: Seconds(2.0) };
        self.emit(&Event::approval_resolved(resolution.clone()));
        Ok(resolution)
    }
}
