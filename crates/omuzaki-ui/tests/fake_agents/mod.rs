//! A session registry for the Agent panel's tests: it keeps sessions and approvals in memory, records every control
//! called on it, and sends the events the executor would, so the panel and the shell update as they do over the real
//! one. It is also a [`Runner`] with no document, whose event stream carries those events, for tests of the whole shell.
//!
//! The executor today has no `end`, `undo_session`, `approvals` or `resolve` (they arrive with app core's executor
//! work); the panel is written against the `Sessions` contract, and this fake holds it to that contract.

#![allow(dead_code, reason = "each test file uses part of it")]

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_core::time::{Clock, FakeClock};
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::confirm::ARM_DELAY;
use omuzaki_ui::ds::testing::{all_strings, harness_state};
use omuzaki_ui::panels::agent::{AgentPanel, Agents};
use omuzaki_ui_api::panel::{Panel as _, PanelCtx, ViewModels};

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_ops_api::{
    Actor, ApprovalId, ApprovalRequest, Budget, Decision, EndReason, Ended, Event, Grants, Limits, Need, OpError,
    OpInfo, OpRecord, Outcome, PendingApproval, Plan, Request, Resolution, Resolved, Runner, SessionChange, SessionId,
    SessionInfo, SessionSpec, SessionState, SessionTx, SessionUndo, Sessions, Spend, StopStep, Stopped, Transport,
    TxId,
};
use omuzaki_ui::shell::stub::NullRunner;
use serde_json::json;

/// The person every session works for.
pub fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

/// A session of `client` with id `id`, connected over `transport`, active, with the default agent grants.
pub fn session(id: u64, client: &str, transport: Transport) -> SessionInfo {
    SessionInfo {
        id: SessionId(id),
        actor: Actor::Agent { id: format!("{client}#{id}"), model: None },
        on_behalf_of: you(),
        transport,
        client: client.to_owned(),
        model: None,
        parent: None,
        role: None,
        grants: Grants::agent_default(),
        fence: Vec::new(),
        budget: Budget::default(),
        spent: Spend::default(),
        limits: Limits::default(),
        state: SessionState::Active,
        open_tx: None,
        pending: Vec::new(),
        steps: 0,
        started_at: Seconds(0.0),
        last_active_at: Seconds(0.0),
    }
}

/// `session` with a transaction open, labelled `label`.
pub fn with_tx(mut session: SessionInfo, label: &str) -> SessionInfo {
    session.open_tx = Some(SessionTx { tx: TxId(90), label: label.to_owned() });
    session
}

/// `session`, paused by the person.
pub fn paused(mut session: SessionInfo) -> SessionInfo {
    session.state = SessionState::Paused { by: you(), reason: omuzaki_ops_api::PauseReason::Person };
    session
}

/// `session`, claiming `paths`.
pub fn fenced(mut session: SessionInfo, paths: &[&str]) -> SessionInfo {
    session.fence = paths.iter().map(|p| DocPath::parse(p).unwrap_or_else(|e| panic!("{e}"))).collect();
    session
}

/// A pending approval `id` from `session` for `op`, needing `need`. Approvals with the same session, op and need
/// share a batch key, as the executor's do.
pub fn approval(id: u64, session: &SessionInfo, op: &str, need: Need) -> PendingApproval {
    let path = DocPath::parse("characters/kit/name").unwrap_or_else(|e| panic!("{e}"));
    let preview = Plan::edit(Patch::new().with(path, Some(json!("Kit")), Some(json!("Kit Two"))));
    PendingApproval {
        id: ApprovalId(id),
        session: session.id,
        actor: session.actor.clone(),
        request: Some(ApprovalRequest { op: op.to_owned(), args: json!({"character": "kit"}), cause: None, preview }),
        batch: format!("{}/{op}/{need:?}", session.id.0),
        need,
        requested_at: Seconds(0.0),
    }
}

/// What the panel called, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Pause(u64),
    Resume(u64),
    Stop(u64),
    Grants(u64, Grants),
    Budget(u64, Budget),
    End(u64, EndReason),
    UndoSession(u64),
    Resolve(u64, Decision),
}

/// The fake registry.
#[derive(Default)]
pub struct FakeAgents {
    sessions: Mutex<Vec<SessionInfo>>,
    approvals: Mutex<Vec<PendingApproval>>,
    calls: Mutex<Vec<Call>>,
    /// Events not yet taken by [`take_events`](Self::take_events).
    queued: Mutex<Vec<Event>>,
    /// Subscribers of the runner side.
    subscribers: Mutex<Vec<Sender<Event>>>,
    /// `stop` answers `Unsupported`, as the executor does today.
    pub no_stop: bool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl FakeAgents {
    /// A registry holding `sessions` and `approvals`.
    pub fn new(sessions: Vec<SessionInfo>, approvals: Vec<PendingApproval>) -> Self {
        Self { sessions: Mutex::new(sessions), approvals: Mutex::new(approvals), ..Self::default() }
    }

    /// Every control called so far.
    pub fn calls(&self) -> Vec<Call> {
        lock(&self.calls).clone()
    }

    /// This registry, with a `stop` that answers `Unsupported`, as the executor's does today.
    pub fn without_stop(mut self) -> Self {
        self.no_stop = true;
        self
    }

    /// The events sent since the last call.
    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *lock(&self.queued))
    }

    /// Sends `event` as the executor would: to the queue and to every subscriber.
    pub fn emit(&self, event: Event) {
        lock(&self.subscribers).retain(|s| s.send(event.clone()).is_ok());
        lock(&self.queued).push(event);
    }

    /// Opens `session` from outside the panel, as a transport would.
    pub fn connect(&self, session: SessionInfo) {
        lock(&self.sessions).push(session.clone());
        self.emit(Event::session_opened(session));
    }

    /// Queues `approval`, as the executor does when a request needs a person.
    pub fn ask(&self, approval: PendingApproval) {
        lock(&self.approvals).push(approval.clone());
        self.emit(Event::approval_requested(approval));
    }

    fn change(
        &self,
        id: SessionId,
        call: Call,
        edit: impl FnOnce(&mut SessionInfo),
        what: SessionChange,
    ) -> Result<SessionInfo, OpError> {
        lock(&self.calls).push(call);
        let snapshot = {
            let mut sessions = lock(&self.sessions);
            let session = sessions.iter_mut().find(|s| s.id == id).ok_or_else(|| OpError::not_found("session"))?;
            edit(session);
            session.clone()
        };
        self.emit(Event::session_changed(snapshot.clone(), what));
        Ok(snapshot)
    }
}

impl Sessions for FakeAgents {
    fn open(&self, _spec: SessionSpec) -> Result<SessionInfo, OpError> {
        Err(OpError::unsupported("the fake opens sessions with connect"))
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        lock(&self.sessions).clone()
    }

    fn session(&self, id: SessionId) -> Option<SessionInfo> {
        lock(&self.sessions).iter().find(|s| s.id == id).cloned()
    }

    fn session_of(&self, actor: &Actor) -> Option<SessionInfo> {
        lock(&self.sessions).iter().find(|s| s.actor == *actor).cloned()
    }

    fn pause(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        let by = by.clone();
        self.change(
            id,
            Call::Pause(id.0),
            |s| s.state = SessionState::Paused { by, reason: omuzaki_ops_api::PauseReason::Person },
            SessionChange::Paused,
        )
    }

    fn resume(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        let what = SessionChange::Resumed { by: by.clone() };
        self.change(id, Call::Resume(id.0), |s| s.state = SessionState::Active, what)
    }

    fn set_grants(&self, id: SessionId, grants: Grants, by: &Actor) -> Result<SessionInfo, OpError> {
        let what = SessionChange::Grants { by: by.clone() };
        let call = Call::Grants(id.0, grants.clone());
        self.change(id, call, |s| s.grants = grants, what)
    }

    fn set_budget(&self, id: SessionId, budget: Budget, by: &Actor) -> Result<SessionInfo, OpError> {
        let what = SessionChange::Budget { by: by.clone() };
        self.change(id, Call::Budget(id.0, budget), |s| s.budget = budget, what)
    }

    fn charge(&self, _id: SessionId, _spend: Spend) -> Result<SessionInfo, OpError> {
        Err(OpError::unsupported("no charges here"))
    }

    fn end(&self, id: SessionId, reason: EndReason) -> Result<Ended, OpError> {
        lock(&self.calls).push(Call::End(id.0, reason.clone()));
        let withdrawn: Vec<ApprovalId> = {
            let mut approvals = lock(&self.approvals);
            let gone = approvals.iter().filter(|a| a.session == id).map(|a| a.id).collect();
            approvals.retain(|a| a.session != id);
            gone
        };
        let snapshot = {
            let mut sessions = lock(&self.sessions);
            let session = sessions.iter_mut().find(|s| s.id == id).ok_or_else(|| OpError::not_found("session"))?;
            session.state = SessionState::Ended { reason, at: Seconds(1.0) };
            session.clone()
        };
        let ended = Ended { abandoned: Vec::new(), kept: Vec::new(), withdrawn, cancelled: Vec::new() };
        self.emit(Event::session_ended(snapshot, ended.clone()));
        Ok(ended)
    }

    fn undo_session(&self, id: SessionId, by: &Actor) -> Result<SessionUndo, OpError> {
        lock(&self.calls).push(Call::UndoSession(id.0));
        let result = SessionUndo {
            step: None,
            reverted: Vec::new(),
            skipped: Vec::new(),
            abandoned: Vec::new(),
            outside: Vec::new(),
        };
        self.emit(Event::session_undone(id, by.clone(), result.clone()));
        Ok(result)
    }

    fn approvals(&self) -> Vec<PendingApproval> {
        lock(&self.approvals).clone()
    }

    fn resolve(&self, id: ApprovalId, decision: Decision, by: &Actor) -> Result<Resolution, OpError> {
        lock(&self.calls).push(Call::Resolve(id.0, decision.clone()));
        let mut approvals = lock(&self.approvals);
        let at = approvals.iter().position(|a| a.id == id).ok_or_else(|| OpError::not_found("approval"))?;
        approvals.remove(at);
        drop(approvals);
        let result = match decision {
            Decision::Reject { comment } => Resolved::Rejected { comment },
            _ => Resolved::Applied { outcome: Outcome::new(Plan::default()) },
        };
        let resolution = Resolution { id, by: Some(by.clone()), result, at: Seconds(2.0) };
        self.emit(Event::approval_resolved(resolution.clone()));
        Ok(resolution)
    }

    fn stop(&self, id: SessionId, by: &Actor) -> Result<Stopped, OpError> {
        if self.no_stop {
            return Err(OpError::unsupported("no stop"));
        }
        let by = by.clone();
        self.change(
            id,
            Call::Stop(id.0),
            |s| s.state = SessionState::Paused { by, reason: omuzaki_ops_api::PauseReason::Person },
            SessionChange::Paused,
        )?;
        Ok(Stopped { step: StopStep::None, jobs: Vec::new() })
    }
}

impl Runner for FakeAgents {
    fn ops(&self) -> Vec<OpInfo> {
        NullRunner.ops()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        NullRunner.run(req)
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        NullRunner.begin(label, actor)
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        NullRunner.commit(tx, actor)
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        NullRunner.rollback(tx, actor)
    }

    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        NullRunner.undo(actor, grants)
    }

    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        NullRunner.redo(actor, grants)
    }

    fn subscribe(&self, _grants: &Grants) -> Result<Receiver<Event>, OpError> {
        let (send, receive) = channel();
        lock(&self.subscribers).push(send);
        Ok(receive)
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        NullRunner.get(path, grants)
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        NullRunner.log(after_seq, limit, grants)
    }
}

// ---- the Agent panel drawn alone over a fake registry ----

/// The panel, the registry and the events of the frame, as the shell hands them out.
pub struct Rig {
    pub panel: AgentPanel,
    pub fake: Arc<FakeAgents>,
    pub views: ViewModels,
    pub events: Vec<Event>,
    pub frame: Option<u64>,
}

/// The Agent panel over `fake` at `size`, with a fake clock for the confirms' dwell.
pub fn rig_with(fake: FakeAgents, size: egui::Vec2) -> (Harness<'static, Rig>, Arc<FakeClock>) {
    let fake = Arc::new(fake);
    let mut views = ViewModels::new();
    views.insert(Agents(Arc::clone(&fake) as Arc<dyn omuzaki_ops_api::Sessions>));
    let rig = Rig { panel: AgentPanel::default(), fake, views, events: Vec::new(), frame: None };
    let mut h = harness_state(Settings::default(), size, rig, |ui, r| {
        // The events of a frame, once per frame, as the shell hands them out.
        let frame = ui.ctx().cumulative_frame_nr();
        if r.frame != Some(frame) {
            r.events = r.fake.take_events();
            r.frame = Some(frame);
        }
        let actor = you();
        let ctx = PanelCtx::new(&NullRunner, &actor).with_views(&r.views).with_events(&r.events);
        r.panel.ui(ui, &ctx);
    });
    let clock = Arc::new(FakeClock::new(Seconds(10.0)));
    omuzaki_ui::ds::clock::set_clock(&h.ctx, Arc::clone(&clock) as Arc<dyn Clock>);
    h.run_steps(3);
    (h, clock)
}

/// Clicks the control labelled `label` and lets the frame settle.
pub fn click(h: &mut Harness<'_, Rig>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(3);
}

pub fn shows(h: &Harness<'_, Rig>, text: &str) -> bool {
    all_strings(h).iter().any(|s| s == text)
}

/// The two-step confirm of a dangerous dialog: the first press arms it, the second, after the dwell, confirms.
pub fn confirm(h: &mut Harness<'_, Rig>, clock: &FakeClock, first: &str, second: &str) {
    click(h, first);
    clock.advance(Seconds(ARM_DELAY + 0.1));
    h.run_steps(2);
    click(h, second);
}
