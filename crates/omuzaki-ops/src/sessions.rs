//! The session registry (design `agent-sessions.md` §3, §4.3, §6): who each agent is, whom it works for, what it may
//! do, and whether it may run right now.
//!
//! The registry lives in [`State`](crate::state::State), under the executor's one lock, so "check the session, then
//! run" is atomic with a pause. Nothing here sends events: callers turn the changes it returns into events.

use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{
    Actor, EndReason, ErrorCode, ErrorDetail, Grants, OpError, PauseReason, Principals, SessionChange, SessionId,
    SessionInfo, SessionSpec, SessionState, Spend,
};

/// Longest client name kept in an agent id.
const MAX_CLIENT_LEN: usize = 64;

/// The principal source until an app registers one: a person holds everything, anything else nothing.
struct LocalPerson;

impl Principals for LocalPerson {
    fn grants(&self, person: &Actor) -> Grants {
        if matches!(person, Actor::Person { .. }) { Grants::person() } else { Grants::default() }
    }
}

/// Every session of this executor, oldest first. Each record's `grants` is the session's *ceiling*; the effective
/// grants are computed from it on every request, so a principal source that narrows takes hold at once.
pub(crate) struct Registry {
    principals: Arc<dyn Principals>,
    sessions: Vec<SessionInfo>,
    next_id: u64,
}

impl Registry {
    pub(crate) fn new() -> Self {
        Self { principals: Arc::new(LocalPerson), sessions: Vec::new(), next_id: 1 }
    }

    pub(crate) fn set_principals(&mut self, principals: Arc<dyn Principals>) {
        self.principals = principals;
    }

    /// Starts allocating ids at `first`. Ignored once a session exists: ids are unique within the history.
    pub(crate) fn set_first_id(&mut self, first: u64) {
        if self.sessions.is_empty() {
            self.next_id = first;
        }
    }

    pub(crate) fn all(&self) -> &[SessionInfo] {
        &self.sessions
    }

    pub(crate) fn find(&self, id: SessionId) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// The session whose agent actor is exactly `actor`: a request cannot claim a session with a different model.
    pub(crate) fn of_actor(&self, actor: &Actor) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.actor == *actor)
    }

    /// The person `actor`'s session works for. `None` for persons, the system and actors without a session.
    pub(crate) fn principal_of(&self, actor: &Actor) -> Option<&Actor> {
        self.of_actor(actor).map(|s| &s.on_behalf_of)
    }

    /// The id and principal of `actor`'s session, for the records of what it starts.
    pub(crate) fn session_ref(&self, actor: &Actor) -> Option<(SessionId, Actor)> {
        self.of_actor(actor).map(|s| (s.id, s.on_behalf_of.clone()))
    }

    /// What `session` may do now: its ceiling, never wider than its principal holds at this moment, nor than what each
    /// ancestor holds now (narrowing a parent narrows its sub-agents).
    pub(crate) fn effective(&self, session: &SessionInfo) -> Grants {
        let own = session.grants.intersect(&self.principals.grants(&session.on_behalf_of));
        match session.parent.and_then(|p| self.find(p)) {
            Some(parent) => own.intersect(&self.effective(parent)),
            None => own,
        }
    }

    /// Opens a session. The record is stored with the ceiling in `grants`.
    pub(crate) fn open(&mut self, spec: SessionSpec, at: Seconds) -> Result<SessionInfo, OpError> {
        if !matches!(spec.on_behalf_of, Actor::Person { .. }) {
            return Err(OpError::invalid("on_behalf_of", "a session works for a person").hint("name the person"));
        }
        if !spec.fence.is_empty() {
            return Err(OpError::unsupported("fences are not enabled by this runner"));
        }
        if !within(&spec.grants, &self.principals.grants(&spec.on_behalf_of)) {
            return Err(OpError::denied("the session's grants exceed those of the person it works for")
                .hint("ask for fewer scopes"));
        }
        if let Some(parent) = spec.parent {
            let parent = self.find(parent).ok_or_else(|| OpError::invalid("parent", "no such session"))?;
            if parent.on_behalf_of != spec.on_behalf_of {
                return Err(OpError::denied("a sub-agent works for the same person as its parent"));
            }
            if parent.state != SessionState::Active {
                return Err(OpError::conflict(format!("session {} is not active", parent.id.0)));
            }
            if !within(&spec.grants, &self.effective(parent)) {
                return Err(OpError::denied("the session's grants exceed those of its parent")
                    .hint("a sub-agent holds at most what its parent holds"));
            }
        }
        let id = SessionId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or_else(|| OpError::internal("session ids ran out"))?;
        let client = sanitise(&spec.client);
        let info = SessionInfo {
            id,
            actor: Actor::Agent { id: format!("{client}#{}", id.0), model: spec.model.clone() },
            on_behalf_of: spec.on_behalf_of,
            transport: spec.transport,
            client,
            model: spec.model,
            parent: spec.parent,
            role: spec.role,
            grants: spec.grants,
            fence: Vec::new(),
            budget: spec.budget,
            spent: Spend::default(),
            limits: spec.limits,
            state: SessionState::Active,
            open_tx: None,
            pending: Vec::new(),
            steps: 0,
            started_at: at,
            last_active_at: at,
        };
        self.sessions.push(info.clone());
        Ok(info)
    }

    /// Lets `actor`'s request through, and says what it may do: `request` narrowed by the session's grants (agents and
    /// plugins) or by the person's own (persons). Fails closed: an agent or plugin needs an active session.
    pub(crate) fn admit(&mut self, actor: &Actor, request: &Grants, at: Seconds) -> Result<Grants, OpError> {
        match actor {
            Actor::System => Ok(request.clone()),
            Actor::Person { .. } => Ok(request.intersect(&self.principals.grants(actor))),
            Actor::Agent { .. } | Actor::Plugin { .. } => {
                self.check_active(actor)?;
                let index = self.sessions.iter().position(|s| s.actor == *actor).ok_or_else(|| no_session(actor))?;
                let granted = self.effective(&self.sessions[index]).intersect(request);
                self.sessions[index].last_active_at = at;
                Ok(granted)
            }
        }
    }

    /// Whether `actor` may run right now, without touching anything: the check a pause must win against a request
    /// that was admitted a moment earlier.
    pub(crate) fn check_active(&self, actor: &Actor) -> Result<(), OpError> {
        match actor {
            Actor::System | Actor::Person { .. } => Ok(()),
            Actor::Agent { .. } | Actor::Plugin { .. } => match self.of_actor(actor).map(|s| &s.state) {
                None => Err(no_session(actor)),
                Some(SessionState::Active) => Ok(()),
                Some(SessionState::Paused { by, reason }) => Err(paused(by, *reason)),
                Some(SessionState::Ended { reason, .. }) => Err(ended(reason)),
            },
        }
    }

    /// Counts a committed step for `owner`'s session, if it has one.
    pub(crate) fn count_step(&mut self, owner: &Actor) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.actor == *owner) {
            session.steps = session.steps.saturating_add(1);
        }
    }

    /// Finds the session `id` for a control by `by`: it must exist, `by` must be its principal and it must not have
    /// ended. Returns its index.
    fn control(&self, id: SessionId, by: &Actor) -> Result<usize, OpError> {
        let index = self
            .sessions
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| OpError::not_found(format!("session {} is not known", id.0)))?;
        let session = &self.sessions[index];
        if session.on_behalf_of != *by {
            return Err(OpError::person_only("only the person a session works for can control it"));
        }
        if matches!(session.state, SessionState::Ended { .. }) {
            return Err(OpError::conflict(format!("session {} has ended", id.0)));
        }
        Ok(index)
    }

    /// Pauses a session and its sub-agents ([`PauseReason::Person`]). A pause already there is kept.
    pub(crate) fn pause(&mut self, id: SessionId, by: &Actor) -> Result<Vec<(SessionId, SessionChange)>, OpError> {
        let root = self.control(id, by)?;
        let paused = SessionState::Paused { by: by.clone(), reason: PauseReason::Person };
        Ok(self.set_family(
            root,
            |s| (s.state == SessionState::Active).then(|| paused.clone()),
            |_| SessionChange::Paused,
        ))
    }

    /// Resumes a session and the sub-agents a person paused. A budget or pairing pause is not the person's to lift
    /// here: those resume through their own flows.
    pub(crate) fn resume(&mut self, id: SessionId, by: &Actor) -> Result<Vec<(SessionId, SessionChange)>, OpError> {
        let root = self.control(id, by)?;
        if let SessionState::Paused { reason: reason @ (PauseReason::Budget | PauseReason::Pairing), .. } =
            &self.sessions[root].state
        {
            return Err(OpError::denied(format!(
                "session {} is paused for {reason:?} and cannot be resumed here",
                id.0
            ))
            .hint("resolve what it waits for first"));
        }
        if let Some(parent) = self.sessions[root].parent.and_then(|p| self.find(p))
            && parent.state != SessionState::Active
        {
            return Err(OpError::conflict(format!("its parent, session {}, is not active", parent.id.0))
                .hint("resume the parent first"));
        }
        let resumable = |s: &SessionInfo| {
            matches!(
                s.state,
                SessionState::Paused { reason: PauseReason::Person | PauseReason::Undo | PauseReason::Step, .. }
            )
        };
        let by = by.clone();
        Ok(self.set_family(
            root,
            |s| resumable(s).then_some(SessionState::Active),
            |_| SessionChange::Resumed { by: by.clone() },
        ))
    }

    /// Replaces the ceiling of session `id`, up to what its principal holds and, for a sub-agent, what its parent holds
    /// now. Returns one change for `id` and one for each descendant whose effective grants moved with it (a sub-agent
    /// holds at most what its parent holds), `id` first.
    pub(crate) fn set_grants(
        &mut self,
        id: SessionId,
        grants: Grants,
        by: &Actor,
    ) -> Result<Vec<(SessionId, SessionChange)>, OpError> {
        let index = self.control(id, by)?;
        if !within(&grants, &self.principals.grants(&self.sessions[index].on_behalf_of)) {
            return Err(OpError::denied("the grants exceed those of the person the session works for"));
        }
        if let Some(parent) = self.sessions[index].parent.and_then(|p| self.find(p))
            && !within(&grants, &self.effective(parent))
        {
            return Err(OpError::denied("the grants exceed those of the session's parent")
                .hint("a sub-agent holds at most what its parent holds"));
        }
        let descendants: Vec<SessionId> = self.family(index).into_iter().skip(1).collect();
        let before: Vec<Grants> = descendants.iter().filter_map(|d| self.find(*d)).map(|s| self.effective(s)).collect();
        self.sessions[index].grants = grants;
        let mut changes = vec![(id, SessionChange::Grants { by: by.clone() })];
        for (descendant, was) in descendants.into_iter().zip(before) {
            if self.find(descendant).is_some_and(|s| self.effective(s) != was) {
                changes.push((descendant, SessionChange::Grants { by: by.clone() }));
            }
        }
        Ok(changes)
    }

    /// Replaces the budget of session `id`.
    pub(crate) fn set_budget(
        &mut self,
        id: SessionId,
        budget: omuzaki_ops_api::Budget,
        by: &Actor,
    ) -> Result<SessionChange, OpError> {
        let index = self.control(id, by)?;
        self.sessions[index].budget = budget;
        Ok(SessionChange::Budget { by: by.clone() })
    }

    /// The id of session `root` (by index) followed by the ids of its descendants.
    fn family(&self, root: usize) -> Vec<SessionId> {
        let mut family = vec![self.sessions[root].id];
        // A child is always opened after its parent, so one pass in order finds every descendant.
        for session in &self.sessions {
            if session.parent.is_some_and(|p| family.contains(&p)) && !family.contains(&session.id) {
                family.push(session.id);
            }
        }
        family
    }

    /// Moves session `root` and its descendants to the state `next` picks for each (`None` leaves it), returning one
    /// change per session that moved, the root first.
    fn set_family(
        &mut self,
        root: usize,
        next: impl Fn(&SessionInfo) -> Option<SessionState>,
        change: impl Fn(&SessionInfo) -> SessionChange,
    ) -> Vec<(SessionId, SessionChange)> {
        let family = self.family(root);
        let mut changes = Vec::new();
        for session in self.sessions.iter_mut().filter(|s| family.contains(&s.id)) {
            if let Some(state) = next(session) {
                session.state = state;
                changes.push((session.id, change(session)));
            }
        }
        changes
    }
}

/// Whether `grants` hold nothing that `limit` does not.
fn within(grants: &Grants, limit: &Grants) -> bool {
    grants.intersect(limit) == *grants
}

/// A client name safe to put in an actor id: ASCII letters, digits and `-_.`, at most [`MAX_CLIENT_LEN`] long. The
/// `#` that separates the id is never in it, so no client can spell another session's actor.
fn sanitise(client: &str) -> String {
    let clean: String = client
        .chars()
        .take(MAX_CLIENT_LEN)
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .collect();
    if clean.is_empty() { "client".to_owned() } else { clean }
}

/// An agent or plugin made a request without an open session.
fn no_session(actor: &Actor) -> OpError {
    OpError::denied(format!("{} has no open session", crate::error::who(actor)))
        .hint("open a session through a transport first")
        .with_detail(ErrorDetail::NoSession)
}

/// The session has ended.
fn ended(reason: &EndReason) -> OpError {
    OpError::denied("this session has ended")
        .hint("open a new session")
        .with_detail(ErrorDetail::Ended { reason: reason.clone() })
}

/// The session is paused: nothing ran, and a person must resume it.
fn paused(by: &Actor, reason: PauseReason) -> OpError {
    let mut error = OpError::conflict(format!("this session is paused ({reason:?})"))
        .hint("wait for a person to resume it; do not retry")
        .with_detail(ErrorDetail::Paused { by: by.clone(), reason });
    error.code = ErrorCode::Waiting;
    error
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::Scope;

    use super::*;

    fn spec() -> SessionSpec {
        SessionSpec {
            transport: omuzaki_ops_api::Transport::Stdio,
            client: "claude".into(),
            model: None,
            on_behalf_of: Actor::Person { name: "alice".into() },
            grants: Grants::agent_default(),
            budget: omuzaki_ops_api::Budget::default(),
            limits: omuzaki_ops_api::Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        }
    }

    #[test]
    fn a_sub_agent_never_holds_more_than_its_parent_holds_now() {
        let alice = Actor::Person { name: "alice".into() };
        let mut registry = Registry::new();
        let parent = registry.open(spec(), Seconds(0.0)).unwrap();
        let child = registry.open(SessionSpec { parent: Some(parent.id), ..spec() }, Seconds(0.0)).unwrap();
        // Widening the child past its parent is refused, as at open.
        let refused = registry.set_grants(child.id, Grants::person(), &alice).unwrap_err();
        assert_eq!(refused.code, ErrorCode::Denied);
        // Narrowing the parent narrows the child, and a grandchild below it.
        let grandchild = registry.open(SessionSpec { parent: Some(child.id), ..spec() }, Seconds(0.0)).unwrap();
        let changed = registry.set_grants(parent.id, Grants::of([Scope::Read]), &alice).unwrap();
        let ids: Vec<_> = changed.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, [parent.id, child.id, grandchild.id], "every session whose effective grants moved is reported");
        for index in [1, 2] {
            let held = registry.effective(&registry.sessions[index]);
            assert_eq!(held, Grants::of([Scope::Read]), "session {}", registry.sessions[index].id.0);
        }
        assert_eq!(grandchild.parent, Some(child.id));
        // And the child cannot be raised back above the narrowed parent.
        assert_eq!(registry.set_grants(child.id, spec().grants, &alice).unwrap_err().code, ErrorCode::Denied);
        // A change that leaves a descendant's effective grants alone reports the session itself only.
        let same = registry.set_grants(parent.id, Grants::of([Scope::Read]), &alice).unwrap();
        assert_eq!(same.len(), 1);
    }

    /// Nothing sets these pauses yet (budgets and pairing are other tasks), so the state is put there by hand.
    #[test]
    fn a_person_cannot_resume_a_budget_or_pairing_pause_and_an_ended_session_is_refused() {
        let alice = Actor::Person { name: "alice".into() };
        let mut registry = Registry::new();
        let info = registry.open(spec(), Seconds(0.0)).unwrap();
        for reason in [PauseReason::Budget, PauseReason::Pairing] {
            registry.sessions[0].state = SessionState::Paused { by: info.actor.clone(), reason };
            let e = registry.resume(info.id, &alice).unwrap_err();
            assert_eq!(e.code, ErrorCode::Denied, "{reason:?}");
            let refused = registry.admit(&info.actor, &Grants::person(), Seconds(1.0)).unwrap_err();
            assert_eq!(refused.code, ErrorCode::Waiting);
        }
        let reason = EndReason::Closed;
        registry.sessions[0].state = SessionState::Ended { reason: reason.clone(), at: Seconds(2.0) };
        let e = registry.admit(&info.actor, &Grants::person(), Seconds(3.0)).unwrap_err();
        assert_eq!((e.code, e.detail.as_deref()), (ErrorCode::Denied, Some(&ErrorDetail::Ended { reason })));
        assert_eq!(registry.pause(info.id, &alice).unwrap_err().code, ErrorCode::Conflict);
    }

    #[test]
    fn client_names_cannot_spell_the_id_separator() {
        assert_eq!(sanitise("claude code#1"), "claude-code-1");
        assert_eq!(sanitise(""), "client");
        assert_eq!(sanitise(&"x".repeat(MAX_CLIENT_LEN + 5)).len(), MAX_CLIENT_LEN);
    }
}
