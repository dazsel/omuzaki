//! The session registry (design `agent-sessions.md` §3, §4.3, §5.3, §6, §8): who each agent is, whom it works for,
//! what it may do, whether it may run right now, its rate limits, its budget and its fence.
//!
//! The registry lives in [`State`](crate::state::State), under the executor's one lock, so "check the session, then
//! run" is atomic with a pause, an end or a budget pause. Nothing here sends events: callers turn the changes it
//! returns into events.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{
    Actor, Budget, EndReason, ErrorCode, ErrorDetail, Grants, OpError, PauseReason, Principals, Scope, SessionChange,
    SessionId, SessionInfo, SessionSpec, SessionState, Spend, Transport, fence_overlap, fence_within, normalize_fence,
};

use crate::budget;
use crate::fence::{FenceRefusal, check_fence};
use crate::limits::{self, Bucket, Buckets};

/// Longest client name kept in an agent id. Long enough for any real client's name, short enough that the `#<id>`
/// after it is never cut off where the id is shown.
const MAX_CLIENT_LEN: usize = 32;

/// The client name of a client that gave none (or nothing printable).
const FALLBACK_CLIENT: &str = "mcp-client";

/// The principal source until an app registers one: a person holds everything, anything else nothing.
struct LocalPerson;

impl Principals for LocalPerson {
    fn grants(&self, person: &Actor) -> Grants {
        if matches!(person, Actor::Person { .. }) { Grants::person() } else { Grants::default() }
    }
}

/// One session: the record the panel shows, plus what the executor keeps to enforce it.
struct Session {
    /// The record. `info.grants` is the session's *ceiling*; the effective grants are computed from it on every
    /// request, so a principal source that narrows takes hold at once. `info.pending` and `info.open_tx` are filled
    /// in by the snapshot, from the queue and the open transactions.
    info: SessionInfo,
    buckets: Buckets,
    /// When the current pause began, on the runner's clock.
    paused_since: Option<f64>,
    /// Seconds paused before the current pause: active time excludes them.
    paused_for: f64,
    /// A person stopped it and it has not resumed since: a joined driver job that ends commits its transaction.
    stopping: bool,
}

impl Session {
    fn is_live(&self) -> bool {
        !matches!(self.info.state, SessionState::Ended { .. })
    }

    /// Recounts active time as of `at`: the runner clock since the session opened, minus every pause.
    fn tick(&mut self, at: f64) {
        let current = self.paused_since.map_or(0.0, |since| at - since);
        let active = (at - self.info.started_at.0 - self.paused_for - current).max(0.0);
        // Whole seconds, rounded down: a cap of 60 s is passed only once 61 s have run.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let seconds = active as u64;
        self.info.spent.seconds = seconds;
    }

    /// Moves to `state` at `at`, keeping the paused-time account.
    fn transition(&mut self, state: SessionState, at: f64) {
        self.tick(at);
        let was_paused = matches!(self.info.state, SessionState::Paused { .. });
        let now_paused = matches!(state, SessionState::Paused { .. });
        match (was_paused, now_paused) {
            (false, true) => self.paused_since = Some(at),
            (true, false) => {
                if let Some(since) = self.paused_since.take() {
                    self.paused_for += (at - since).max(0.0);
                }
            }
            _ => {}
        }
        self.info.state = state;
    }
}

/// What a request from an admitted actor may use.
pub(crate) struct Admitted {
    /// Its own grants, narrowed by the session's and the principal's.
    pub(crate) grants: Grants,
    /// The acting session's index, for agents and plugins.
    pub(crate) session: Option<usize>,
}

/// How [`Registry::admit_run`] treats the session's state and limits.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// An ordinary call: the session must be active, and the call draws one token from `bucket` when there is one.
    Call(Option<Bucket>),
    /// A run inside one already admitted: a request a person approved (it goes ahead in a paused session, since the
    /// person decided it, and was counted against the limits when it was first sent), or an effect call of an undo.
    /// An ended session is still refused.
    Inner,
}

/// Every session of this executor, oldest first.
pub(crate) struct Registry {
    principals: Arc<dyn Principals>,
    sessions: Vec<Session>,
    next_id: u64,
    /// The counter shared by every lane of a project (design branches.md 5.1), when the host gave one: ids then come
    /// from it, so they stay unique across branches.
    shared_ids: Option<Arc<AtomicU64>>,
    /// How many live sessions have a fence: with none, edits skip the fence check.
    fenced: usize,
}

impl Registry {
    pub(crate) fn new() -> Self {
        Self { principals: Arc::new(LocalPerson), sessions: Vec::new(), next_id: 1, shared_ids: None, fenced: 0 }
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

    /// Takes session ids from `counter` (the next id to hand out), shared with other registries. The counter moves up
    /// to this registry's own next id first, so an id the restored history used is never handed out again.
    pub(crate) fn share_ids(&mut self, counter: Arc<AtomicU64>) {
        counter.fetch_max(self.next_id, Ordering::SeqCst);
        self.shared_ids = Some(counter);
    }

    /// What `person` may do, from the principal source.
    pub(crate) fn principal_grants(&self, person: &Actor) -> Grants {
        self.principals.grants(person)
    }

    pub(crate) fn all(&self) -> impl Iterator<Item = &SessionInfo> {
        self.sessions.iter().map(|s| &s.info)
    }

    pub(crate) fn find(&self, id: SessionId) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.info.id == id).map(|s| &s.info)
    }

    /// The index of session `id`.
    ///
    /// # Errors
    /// `NotFound`.
    pub(crate) fn index(&self, id: SessionId) -> Result<usize, OpError> {
        self.sessions
            .iter()
            .position(|s| s.info.id == id)
            .ok_or_else(|| OpError::not_found(format!("session {} is not known", id.0)))
    }

    /// The record of the session at `index`.
    pub(crate) fn info(&self, index: usize) -> &SessionInfo {
        &self.sessions[index].info
    }

    /// The session whose agent actor is exactly `actor`: a request cannot claim a session with a different model.
    pub(crate) fn of_actor(&self, actor: &Actor) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.info.actor == *actor).map(|s| &s.info)
    }

    fn index_of(&self, actor: &Actor) -> Option<usize> {
        self.sessions.iter().position(|s| s.info.actor == *actor)
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
        session.grants.intersect(&self.above(session))
    }

    /// What everything above `session` lets it hold: its principal's grants, narrowed by each ancestor's effective
    /// grants. A person can widen the session's ceiling up to this, and no further.
    fn above(&self, session: &SessionInfo) -> Grants {
        let principal = self.principals.grants(&session.on_behalf_of);
        match session.parent.and_then(|p| self.find(p)) {
            Some(parent) => principal.intersect(&self.effective(parent)),
            None => principal,
        }
    }

    /// Opens a session. The record is stored with the ceiling in `grants`.
    pub(crate) fn open(&mut self, spec: SessionSpec, at: Seconds) -> Result<SessionInfo, OpError> {
        if !matches!(spec.on_behalf_of, Actor::Person { .. }) {
            return Err(OpError::invalid("on_behalf_of", "a session works for a person").hint("name the person"));
        }
        limits::validate(&spec.limits)?;
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
        let fence = normalize_fence(spec.fence)?;
        self.check_claim(None, spec.parent, &fence)?;
        let id = match &self.shared_ids {
            Some(counter) => {
                let id = counter.fetch_add(1, Ordering::SeqCst);
                if id == u64::MAX {
                    return Err(OpError::internal("session ids ran out"));
                }
                SessionId(id)
            }
            None => {
                let id = SessionId(self.next_id);
                self.next_id = self.next_id.checked_add(1).ok_or_else(|| OpError::internal("session ids ran out"))?;
                id
            }
        };
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
            fence,
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
        self.fenced += usize::from(!info.fence.is_empty());
        let buckets = Buckets::new(&info.limits, at.0);
        self.sessions.push(Session {
            info: info.clone(),
            buckets,
            paused_since: None,
            paused_for: 0.0,
            stopping: false,
        });
        Ok(info)
    }

    /// Lets `actor`'s request through, and says what it may do: `request` narrowed by the session's grants (agents and
    /// plugins) or by the person's own (persons). Fails closed: an agent or plugin needs an active session.
    pub(crate) fn admit(&mut self, actor: &Actor, request: &Grants, at: Seconds) -> Result<Grants, OpError> {
        self.admit_run(actor, request, at, Entry::Call(None)).map(|admitted| admitted.grants)
    }

    /// [`admit`](Self::admit) for a call that counts against a rate limit (`Entry::Call` with a bucket) or that a
    /// person approved (`Entry::Inner`), returning the session it acts in.
    ///
    /// # Errors
    /// `Denied` without a session or after it ended, `Waiting` while it is paused (an approved run excepted), `Limited`
    /// when the bucket is empty.
    pub(crate) fn admit_run(
        &mut self,
        actor: &Actor,
        request: &Grants,
        at: Seconds,
        entry: Entry,
    ) -> Result<Admitted, OpError> {
        match actor {
            Actor::System => Ok(Admitted { grants: request.clone(), session: None }),
            Actor::Person { .. } => {
                Ok(Admitted { grants: request.intersect(&self.principals.grants(actor)), session: None })
            }
            Actor::Agent { .. } | Actor::Plugin { .. } => {
                let index = self.index_of(actor).ok_or_else(|| no_session(actor))?;
                match (&self.sessions[index].info.state, entry) {
                    (SessionState::Ended { reason, .. }, _) => return Err(ended(reason)),
                    (SessionState::Paused { by, reason }, Entry::Call(_)) => return Err(paused(by, *reason)),
                    _ => {}
                }
                let granted = self.effective(&self.sessions[index].info).intersect(request);
                let session = &mut self.sessions[index];
                if let Entry::Call(Some(bucket)) = entry {
                    session.buckets.take(&session.info.limits, bucket, at.0)?;
                }
                session.info.last_active_at = at;
                session.tick(at.0);
                Ok(Admitted { grants: granted, session: Some(index) })
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
        if let Some(index) = self.index_of(owner) {
            let session = &mut self.sessions[index].info;
            session.steps = session.steps.saturating_add(1);
        }
    }

    /// Checks that `by` is the person session `index` works for.
    ///
    /// # Errors
    /// `Denied` (detail `PersonOnly`) otherwise: an agent, including the session itself, never controls a session.
    pub(crate) fn check_principal(&self, index: usize, by: &Actor) -> Result<(), OpError> {
        if self.sessions[index].info.on_behalf_of == *by {
            Ok(())
        } else {
            Err(OpError::person_only("only the person a session works for can control it"))
        }
    }

    /// Finds the session `id` for a control by `by`: it must exist, `by` must be its principal and it must not have
    /// ended. Returns its index.
    fn control(&self, id: SessionId, by: &Actor) -> Result<usize, OpError> {
        let index = self.index(id)?;
        self.check_principal(index, by)?;
        if !self.sessions[index].is_live() {
            return Err(OpError::conflict(format!("session {} has ended", id.0)));
        }
        Ok(index)
    }

    /// Pauses a session and its sub-agents ([`PauseReason::Person`]). A pause already there is kept.
    pub(crate) fn pause(
        &mut self,
        id: SessionId,
        by: &Actor,
        at: Seconds,
    ) -> Result<Vec<(SessionId, SessionChange)>, OpError> {
        let root = self.control(id, by)?;
        let paused = SessionState::Paused { by: by.clone(), reason: PauseReason::Person };
        Ok(self.set_family(
            root,
            at,
            |s| (s.state == SessionState::Active).then(|| paused.clone()),
            |_| SessionChange::Paused,
        ))
    }

    /// Pauses session `index` alone for `reason`, by `by` (a person, or the agent itself for a budget pause). Returns
    /// whether it moved: a session that is already paused keeps the pause it has.
    pub(crate) fn pause_one(&mut self, index: usize, by: Actor, reason: PauseReason, at: Seconds) -> bool {
        let session = &mut self.sessions[index];
        if session.info.state != SessionState::Active {
            return false;
        }
        session.transition(SessionState::Paused { by, reason }, at.0);
        true
    }

    /// Resumes a session and the sub-agents a person paused. A pairing pause is not the person's to lift here, and a
    /// budget pause lifts only once the budget allows.
    pub(crate) fn resume(
        &mut self,
        id: SessionId,
        by: &Actor,
        at: Seconds,
    ) -> Result<Vec<(SessionId, SessionChange)>, OpError> {
        let root = self.control(id, by)?;
        let reason = match &self.sessions[root].info.state {
            SessionState::Paused { reason, .. } => Some(*reason),
            _ => None,
        };
        if reason == Some(PauseReason::Pairing) {
            return Err(OpError::denied(format!("session {} waits to be paired", id.0))
                .hint("pair or refuse it on its approval card"));
        }
        if reason == Some(PauseReason::Budget) && self.over_budget(root, 0, at).is_some() {
            return Err(OpError::denied(format!("session {} is over its budget", id.0))
                .hint("raise the session's budget first"));
        }
        if let Some(parent) = self.sessions[root].info.parent.and_then(|p| self.find(p))
            && parent.state != SessionState::Active
        {
            return Err(OpError::conflict(format!("its parent, session {}, is not active", parent.id.0))
                .hint("resume the parent first"));
        }
        let root_id = self.sessions[root].info.id;
        let resumable = |s: &SessionInfo| match s.state {
            SessionState::Paused { reason: PauseReason::Person | PauseReason::Undo | PauseReason::Step, .. } => true,
            // The root's budget was checked above; a sub-agent's own budget pause is its own.
            SessionState::Paused { reason: PauseReason::Budget, .. } => s.id == root_id,
            _ => false,
        };
        let by = by.clone();
        let changes = self.set_family(
            root,
            at,
            |s| resumable(s).then_some(SessionState::Active),
            |_| SessionChange::Resumed { by: by.clone() },
        );
        for session in self.sessions.iter_mut().filter(|s| changes.iter().any(|(id, _)| *id == s.info.id)) {
            session.stopping = false;
        }
        Ok(changes)
    }

    /// Lifts session `index`'s pause when it is paused for `reason`: a pairing a person approved, a budget a person
    /// raised. Returns whether it moved.
    pub(crate) fn lift(&mut self, index: usize, reason: PauseReason, at: Seconds) -> bool {
        let session = &mut self.sessions[index];
        if !matches!(&session.info.state, SessionState::Paused { reason: r, .. } if *r == reason) {
            return false;
        }
        session.transition(SessionState::Active, at.0);
        true
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
        if !within(&grants, &self.principals.grants(&self.sessions[index].info.on_behalf_of)) {
            return Err(OpError::denied("the grants exceed those of the person the session works for"));
        }
        if let Some(parent) = self.sessions[index].info.parent.and_then(|p| self.find(p))
            && !within(&grants, &self.effective(parent))
        {
            return Err(OpError::denied("the grants exceed those of the session's parent")
                .hint("a sub-agent holds at most what its parent holds"));
        }
        Ok(self.replace_ceiling(index, grants, by))
    }

    /// Whether session `index` could be given `scope` by a person: its principal holds it, every ancestor holds it
    /// now, and the request that asks holds it too (a transport may narrow a request, and approving a scope the
    /// caller lacks would not help the caller, `plugins.md` §4).
    pub(crate) fn may_grant(&self, index: usize, scope: Scope, request: &Grants) -> bool {
        request.allows(scope) && self.above(&self.sessions[index].info).allows(scope)
    }

    /// Adds `scope` to session `index`'s ceiling, for an approval "for this session". Returns the changes, as
    /// [`set_grants`](Self::set_grants) does.
    pub(crate) fn widen(&mut self, index: usize, scope: Scope, by: &Actor) -> Vec<(SessionId, SessionChange)> {
        let grants = self.sessions[index].info.grants.clone().with(scope);
        self.replace_ceiling(index, grants, by)
    }

    fn replace_ceiling(&mut self, index: usize, grants: Grants, by: &Actor) -> Vec<(SessionId, SessionChange)> {
        let id = self.sessions[index].info.id;
        let descendants: Vec<SessionId> = self.family(index).into_iter().skip(1).collect();
        let before: Vec<Grants> = descendants.iter().filter_map(|d| self.find(*d)).map(|s| self.effective(s)).collect();
        self.sessions[index].info.grants = grants;
        let mut changes = vec![(id, SessionChange::Grants { by: by.clone() })];
        for (descendant, was) in descendants.into_iter().zip(before) {
            if self.find(descendant).is_some_and(|s| self.effective(s) != was) {
                changes.push((descendant, SessionChange::Grants { by: by.clone() }));
            }
        }
        changes
    }

    /// Replaces the budget of session `id`. A session paused for its budget resumes when the new one allows.
    pub(crate) fn set_budget(
        &mut self,
        id: SessionId,
        budget: Budget,
        by: &Actor,
        at: Seconds,
    ) -> Result<Vec<(SessionId, SessionChange)>, OpError> {
        let index = self.control(id, by)?;
        Ok(self.replace_budget(index, budget, by, at))
    }

    /// Sets session `index`'s budget, resuming a budget pause the new budget allows. Returns the changes, the budget
    /// first.
    pub(crate) fn replace_budget(
        &mut self,
        index: usize,
        budget: Budget,
        by: &Actor,
        at: Seconds,
    ) -> Vec<(SessionId, SessionChange)> {
        let id = self.sessions[index].info.id;
        self.sessions[index].info.budget = budget;
        let mut changes = vec![(id, SessionChange::Budget { by: by.clone() })];
        if self.over_budget(index, 0, at).is_none() && self.lift(index, PauseReason::Budget, at) {
            changes.push((id, SessionChange::Resumed { by: by.clone() }));
        }
        changes
    }

    /// Whether session `index` may spend `cost` micro-dollars more now, with its active time counted to `at`. `None`
    /// when it may; otherwise what is needed and what is left.
    pub(crate) fn over_budget(&mut self, index: usize, cost: u64, at: Seconds) -> Option<(Spend, Spend)> {
        let session = &mut self.sessions[index];
        session.tick(at.0);
        budget::exceeded(&session.info.budget, &session.info.spent, cost)
    }

    /// Whether `needed` more fits session `index`'s budget now (`Sessions::request_budget`): `None` when it does;
    /// otherwise what is left.
    pub(crate) fn short_of(&mut self, index: usize, needed: Spend, at: Seconds) -> Option<Spend> {
        let session = &mut self.sessions[index];
        session.tick(at.0);
        let (budget, spent) = (&session.info.budget, &session.info.spent);
        let after = Spend {
            usd_micros: spent.usd_micros.saturating_add(needed.usd_micros),
            tokens: spent.tokens.saturating_add(needed.tokens),
            seconds: spent.seconds.saturating_add(needed.seconds),
        };
        budget::crossed(budget, &after).then(|| budget::left(budget, spent))
    }

    /// Marks session `index` stopped (or no longer, on a resume).
    pub(crate) fn set_stopping(&mut self, index: usize, stopping: bool) {
        self.sessions[index].stopping = stopping;
    }

    /// Whether session `index` was stopped and has not resumed.
    pub(crate) fn is_stopping(&self, index: usize) -> bool {
        self.sessions[index].stopping
    }

    /// Adds `usd_micros` and `tokens` to session `index`'s spend. Returns whether it is now past a cap.
    pub(crate) fn add_spend(&mut self, index: usize, usd_micros: u64, tokens: u64, at: Seconds) -> bool {
        let session = &mut self.sessions[index];
        session.tick(at.0);
        let spent = &mut session.info.spent;
        spent.usd_micros = spent.usd_micros.saturating_add(usd_micros);
        spent.tokens = spent.tokens.saturating_add(tokens);
        budget::crossed(&session.info.budget, &session.info.spent)
    }

    /// Marks session `index` ended for `reason` at `at`. A revoke also drops its ceiling to nothing. Its fence stops
    /// claiming anything at once.
    pub(crate) fn end(&mut self, index: usize, reason: EndReason, at: Seconds) {
        let session = &mut self.sessions[index];
        if matches!(reason, EndReason::Revoked { .. }) {
            session.info.grants = Grants::default();
        }
        let fenced = !session.info.fence.is_empty();
        session.transition(SessionState::Ended { reason, at }, at.0);
        self.fenced -= usize::from(fenced);
    }

    /// Whether session `index` has ended.
    pub(crate) fn has_ended(&self, index: usize) -> bool {
        !self.sessions[index].is_live()
    }

    /// Replaces the fence of session `id` (person only). The fence is normalised; an empty one releases the claim.
    ///
    /// # Errors
    /// As [`pause`](Self::pause); `Invalid` for the root path; `Conflict` (detail `Fenced`) when it overlaps another
    /// live session's; `Denied` when a sub-agent's fence would leave its parent's, or a sub-agent's would leave it.
    pub(crate) fn set_fence(
        &mut self,
        id: SessionId,
        fence: Vec<DocPath>,
        by: &Actor,
    ) -> Result<SessionChange, OpError> {
        let index = self.control(id, by)?;
        let fence = normalize_fence(fence)?;
        self.check_claim(Some(index), self.sessions[index].info.parent, &fence)?;
        let children = self.sessions.iter().filter(|s| s.is_live() && s.info.parent == Some(id));
        if children.into_iter().any(|child| !fence_within(&child.info.fence, &fence)) {
            return Err(OpError::denied("a sub-agent's fence would lie outside this one")
                .hint("narrow or release the sub-agents' fences first"));
        }
        let session = &mut self.sessions[index];
        let was = !session.info.fence.is_empty();
        session.info.fence = fence;
        let is = !session.info.fence.is_empty();
        self.fenced = self.fenced + usize::from(is) - usize::from(was);
        Ok(SessionChange::Fence { by: by.clone() })
    }

    /// Checks a claim of `fence` by session `index` (`None`: one about to open) under `parent`: inside the parent's
    /// fence, and overlapping no live session outside its own family.
    fn check_claim(&self, index: Option<usize>, parent: Option<SessionId>, fence: &[DocPath]) -> Result<(), OpError> {
        if let Some(parent) = parent.and_then(|p| self.find(p))
            && !fence_within(fence, &parent.fence)
        {
            return Err(OpError::denied("this fence is not inside the parent session's")
                .hint("a child's fence must lie inside its parent's"));
        }
        for (other, session) in self.sessions.iter().enumerate() {
            if !session.is_live() || Some(other) == index || self.related(index, parent, other) {
                continue;
            }
            if let Some((mine, _)) = fence_overlap(fence, &session.info.fence) {
                let mut refusal = OpError::fenced(mine.clone(), Some(session.info.actor.clone()));
                refusal.code = ErrorCode::Conflict;
                return Err(refusal);
            }
        }
        Ok(())
    }

    /// Whether session `other` is on the ancestor chain of the session at `index` (or of one about to open under
    /// `parent`), or below it.
    fn related(&self, index: Option<usize>, parent: Option<SessionId>, other: usize) -> bool {
        let other_id = self.sessions[other].info.id;
        let mut up = parent;
        while let Some(id) = up {
            if id == other_id {
                return true;
            }
            up = self.find(id).and_then(|s| s.parent);
        }
        index.is_some_and(|index| self.family(index).contains(&other_id))
    }

    /// Checks an agent's `patch` against the fences (design §5.3): its own, when it has one, and every other live
    /// session's outside its family. Plugin sessions are never fenced (a plugin acts for its caller, who is checked),
    /// and with no fenced session there is nothing to check.
    pub(crate) fn check_patch(&self, index: usize, patch: &Patch) -> Result<(), FenceRefusal> {
        let session = &self.sessions[index].info;
        if self.fenced == 0 || session.transport == Transport::Plugin || patch.is_empty() {
            return Ok(());
        }
        let others: Vec<(&Actor, &[DocPath])> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(other, s)| {
                s.is_live()
                    && !s.info.fence.is_empty()
                    && *other != index
                    && !self.related(Some(index), session.parent, *other)
            })
            .map(|(_, s)| (&s.info.actor, s.info.fence.as_slice()))
            .collect();
        check_fence(&session.fence, &others, patch)
    }

    /// The id of session `root` (by index) followed by the ids of its descendants.
    pub(crate) fn family(&self, root: usize) -> Vec<SessionId> {
        let mut family = vec![self.sessions[root].info.id];
        // A child is always opened after its parent, so one pass in order finds every descendant.
        for session in &self.sessions {
            if session.info.parent.is_some_and(|p| family.contains(&p)) && !family.contains(&session.info.id) {
                family.push(session.info.id);
            }
        }
        family
    }

    /// Moves session `root` and its descendants to the state `next` picks for each (`None` leaves it), returning one
    /// change per session that moved, the root first.
    fn set_family(
        &mut self,
        root: usize,
        at: Seconds,
        next: impl Fn(&SessionInfo) -> Option<SessionState>,
        change: impl Fn(&SessionInfo) -> SessionChange,
    ) -> Vec<(SessionId, SessionChange)> {
        let family = self.family(root);
        let mut changes = Vec::new();
        for session in self.sessions.iter_mut().filter(|s| family.contains(&s.info.id)) {
            if let Some(state) = next(&session.info) {
                session.transition(state, at.0);
                changes.push((session.info.id, change(&session.info)));
            }
        }
        changes
    }
}

/// Whether `grants` hold nothing that `limit` does not.
fn within(grants: &Grants, limit: &Grants) -> bool {
    grants.intersect(limit) == *grants
}

/// A client name safe to put in an actor id, an op record, a log line and a prompt: ASCII letters, digits and `-_.`,
/// at most [`MAX_CLIENT_LEN`] long. Invisible characters (controls, bidi and other format characters, tag characters)
/// are dropped; every run of anything else, whitespace included, becomes one `-`. The `#` that separates the id is
/// never kept, so no client can spell another session's actor. An empty result is [`FALLBACK_CLIENT`].
fn sanitise(client: &str) -> String {
    let mut clean = String::with_capacity(MAX_CLIENT_LEN);
    let mut gap = false;
    for c in client.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            let needed = 1 + usize::from(gap && !clean.is_empty());
            if clean.len() + needed > MAX_CLIENT_LEN {
                break;
            }
            if gap && !clean.is_empty() {
                clean.push('-');
            }
            clean.push(c);
            gap = false;
        } else if !invisible(c) {
            gap = true;
        }
    }
    if clean.is_empty() { FALLBACK_CLIENT.to_owned() } else { clean }
}

/// Characters a reader cannot see but a model reads: controls, bidi controls, zero-width and other format characters,
/// and the tag block (U+E0000 to U+E007F). Whitespace is not among them: it separates words, so it becomes a `-`.
fn invisible(c: char) -> bool {
    !c.is_whitespace()
        && (c.is_control()
            || matches!(
                c,
                '\u{00AD}'
                    | '\u{061C}'
                    | '\u{180E}'
                    | '\u{200B}'..='\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{206F}'
                    | '\u{FEFF}'
                    | '\u{FFF9}'..='\u{FFFB}'
                    | '\u{E0000}'..='\u{E007F}'
            ))
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
            budget: Budget::default(),
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
            let held = registry.effective(&registry.sessions[index].info);
            assert_eq!(held, Grants::of([Scope::Read]), "session {}", registry.sessions[index].info.id.0);
        }
        assert_eq!(grandchild.parent, Some(child.id));
        // And the child cannot be raised back above the narrowed parent, nor given what the parent lost.
        assert_eq!(registry.set_grants(child.id, spec().grants, &alice).unwrap_err().code, ErrorCode::Denied);
        assert!(!registry.may_grant(1, Scope::EditCharacter, &Grants::person()));
        // A change that leaves a descendant's effective grants alone reports the session itself only.
        let same = registry.set_grants(parent.id, Grants::of([Scope::Read]), &alice).unwrap();
        assert_eq!(same.len(), 1);
    }

    /// A pairing pause is lifted only by its approval, a budget pause only once the budget allows.
    #[test]
    fn a_person_cannot_resume_a_pairing_pause_or_an_unaffordable_budget_pause() {
        let alice = Actor::Person { name: "alice".into() };
        let mut registry = Registry::new();
        let info = registry.open(spec(), Seconds(0.0)).unwrap();
        for reason in [PauseReason::Budget, PauseReason::Pairing] {
            assert!(registry.pause_one(0, info.actor.clone(), reason, Seconds(0.0)));
            if reason == PauseReason::Budget {
                registry.add_spend(0, 1, 0, Seconds(0.0));
            }
            let e = registry.resume(info.id, &alice, Seconds(0.0)).unwrap_err();
            assert_eq!(e.code, ErrorCode::Denied, "{reason:?}");
            let refused = registry.admit(&info.actor, &Grants::person(), Seconds(1.0)).unwrap_err();
            assert_eq!(refused.code, ErrorCode::Waiting);
            assert!(registry.lift(0, reason, Seconds(1.0)));
        }
        registry.end(0, EndReason::Closed, Seconds(2.0));
        let e = registry.admit(&info.actor, &Grants::person(), Seconds(3.0)).unwrap_err();
        assert_eq!(
            (e.code, e.detail.as_deref()),
            (ErrorCode::Denied, Some(&ErrorDetail::Ended { reason: EndReason::Closed }))
        );
        assert_eq!(registry.pause(info.id, &alice, Seconds(3.0)).unwrap_err().code, ErrorCode::Conflict);
    }

    #[test]
    fn active_time_leaves_out_the_pauses() {
        let alice = Actor::Person { name: "alice".into() };
        let mut registry = Registry::new();
        let info = registry.open(spec(), Seconds(10.0)).unwrap();
        registry.pause(info.id, &alice, Seconds(15.0)).unwrap();
        registry.resume(info.id, &alice, Seconds(115.0)).unwrap();
        registry.over_budget(0, 0, Seconds(120.5));
        assert_eq!(registry.sessions[0].info.spent.seconds, 10, "5 s before the pause and 5.5 after, rounded down");
    }

    #[test]
    fn client_names_are_one_short_printable_line() {
        assert_eq!(sanitise("claude code#1"), "claude-code-1");
        assert_eq!(sanitise(""), FALLBACK_CLIENT);
        assert_eq!(sanitise("\u{202E}\u{E0041}\u{200B}"), FALLBACK_CLIENT);
        assert_eq!(sanitise("cl\u{200B}aude"), "claude", "an invisible character does not split a word");
        assert_eq!(sanitise("  evil\n\n\u{1b}[31mname  "), "evil-31mname");
        let long = sanitise(&"x".repeat(MAX_CLIENT_LEN + 5));
        assert_eq!(long.len(), MAX_CLIENT_LEN);
    }
}
