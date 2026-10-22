//! [`AgentFeed`]: the studio's copy of the agent sessions and the approvals waiting for the person, kept current from
//! the runner's events.
//!
//! The executor sends a full snapshot with every session event and the whole pending record with every approval, so
//! the copy is exact without asking the registry again: the shell loads it once ([`AgentFeed::load`]) and then only
//! follows events ([`AgentFeed::follow`]). The Agent panel, the approval stack, the top-bar chip and the fence chip all
//! read this one copy, so they agree with each other in every frame.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use omuzaki_ops_api::{
    Actor, ApprovalId, Event, PauseReason, PendingApproval, SessionId, SessionInfo, SessionState, Sessions, Transport,
};

/// A session with no call for this long reads as idle (design `agent-sessions.md` section 3.1).
pub const IDLE_AFTER: Duration = Duration::from_secs(60);

/// Where a session stands, in the words the panel uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// It made a call in the last minute, or has a transaction open.
    Working,
    /// Connected, and quiet for a minute or more.
    Idle,
    /// A request of its waits for the person.
    Waiting,
    /// Paused, and why.
    Paused(PauseReason),
    /// Over.
    Ended,
}

/// The sessions and pending approvals, as the studio last heard of them.
#[derive(Clone, Debug, Default)]
pub struct AgentFeed {
    /// Every session, oldest first, ended ones included.
    sessions: Vec<SessionInfo>,
    /// The approvals waiting for a person, oldest first.
    approvals: Vec<PendingApproval>,
    /// When the studio last saw each session act.
    active: HashMap<SessionId, Instant>,
    /// Counts changes, so a view can tell its cache is stale.
    version: u64,
}

impl AgentFeed {
    /// The registry as it stands now.
    pub fn load(sessions: &dyn Sessions) -> Self {
        Self { sessions: sessions.sessions(), approvals: sessions.approvals(), version: 1, ..Self::default() }
    }

    /// Takes in this frame's events; true if anything changed.
    pub fn follow(&mut self, events: &[Event]) -> bool {
        let before = self.version;
        let now = Instant::now();
        for event in events {
            match event {
                Event::SessionOpened { session, .. } => {
                    self.active.insert(session.id, now);
                    self.upsert(session);
                }
                Event::SessionChanged { session, .. } => self.upsert(session),
                Event::SessionEnded { session, ended, .. } => {
                    self.approvals.retain(|a| !ended.withdrawn.contains(&a.id) && a.session != session.id);
                    self.upsert(session);
                }
                Event::ApprovalRequested { approval, .. } => {
                    self.active.insert(approval.session, now);
                    if !self.approvals.iter().any(|a| a.id == approval.id) {
                        self.approvals.push(approval.clone());
                        self.version += 1;
                    }
                }
                Event::ApprovalResolved { resolution, .. } => self.remove(resolution.id),
                Event::OpApplied { record, .. } => self.touch(&record.actor, now, |_| {}),
                Event::TxCommitted { actor, .. } => self.touch(actor, now, |s| {
                    s.open_tx = None;
                    s.steps = s.steps.saturating_add(1);
                }),
                Event::TxRolledBack { actor, .. } => self.touch(actor, now, |s| s.open_tx = None),
                _ => {}
            }
        }
        self.version != before
    }

    /// Every session, oldest first, ended ones included.
    pub fn sessions(&self) -> &[SessionInfo] {
        &self.sessions
    }

    /// The sessions that have not ended, oldest first.
    pub fn live(&self) -> impl Iterator<Item = &SessionInfo> {
        self.sessions.iter().filter(|s| !matches!(s.state, SessionState::Ended { .. }))
    }

    /// One session.
    pub fn session(&self, id: SessionId) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// The approvals waiting for a person, oldest first.
    pub fn approvals(&self) -> &[PendingApproval] {
        &self.approvals
    }

    /// Counts changes: equal versions mean nothing changed in between.
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Where `session` stands at `now`.
    pub fn status(&self, session: &SessionInfo, now: Instant) -> Status {
        match &session.state {
            SessionState::Ended { .. } => Status::Ended,
            SessionState::Paused { reason, .. } => Status::Paused(*reason),
            SessionState::Active if self.approvals.iter().any(|a| a.session == session.id) => Status::Waiting,
            SessionState::Active if session.open_tx.is_some() => Status::Working,
            SessionState::Active => {
                let recent = self.active.get(&session.id).is_some_and(|at| now.duration_since(*at) < IDLE_AFTER);
                if recent { Status::Working } else { Status::Idle }
            }
        }
    }

    /// When the soonest working session turns idle, so the view can repaint then and say so.
    pub fn next_change(&self, now: Instant) -> Option<Duration> {
        self.live().filter_map(|s| self.active.get(&s.id)).map(|at| IDLE_AFTER.saturating_sub(now - *at)).min()
    }

    /// The other live sessions on the same credential as `session` (an HTTP token's label), which a revoke of the
    /// credential ends too. A session with no credential has none.
    pub fn same_credential(&self, session: &SessionInfo) -> Vec<&SessionInfo> {
        let Transport::Http { token } = &session.transport else { return Vec::new() };
        self.live()
            .filter(|s| s.id != session.id && matches!(&s.transport, Transport::Http { token: t } if t == token))
            .collect()
    }

    fn upsert(&mut self, session: &SessionInfo) {
        match self.sessions.iter_mut().find(|s| s.id == session.id) {
            Some(old) => *old = session.clone(),
            None => self.sessions.push(session.clone()),
        }
        self.version += 1;
    }

    fn remove(&mut self, id: ApprovalId) {
        let before = self.approvals.len();
        self.approvals.retain(|a| a.id != id);
        if self.approvals.len() != before {
            self.version += 1;
        }
    }

    /// Marks the session whose agent is `actor` as active now, and changes it with `change`.
    fn touch(&mut self, actor: &Actor, now: Instant, change: impl FnOnce(&mut SessionInfo)) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.actor == *actor) {
            self.active.insert(session.id, now);
            change(session);
            self.version += 1;
        }
    }
}

/// Whether a session has a credential a grant can be remembered on (design `agent-sessions.md` section 3.3): an HTTP
/// token, the built-in assistant's settings. A headless stdio session has none.
pub const fn has_credential(transport: &Transport) -> bool {
    matches!(transport, Transport::Http { .. } | Transport::InApp)
}
