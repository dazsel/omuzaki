//! `MockSessions`: an in-memory session registry, so plugin hosts open and end real [`SessionInfo`]s in tests without
//! the executor.

use std::sync::Mutex;

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_ops_api::{
    Actor, ApprovalId, Budget, Decision, EndReason, Ended, Grants, OpError, PendingApproval, Resolution, SessionId,
    SessionInfo, SessionSpec, SessionState, SessionUndo, Sessions, Spend, Transport,
};

/// Opens, lists and ends sessions as the executor would for plugins: the actor is `plugin <id>#<session>` built from
/// the client name `plugin:<id>@<version>`, the effective grants are the ceiling the spec carries, and every end
/// reason is kept. Controls the plugin host never uses (pause, budgets, undo, approvals) answer `Unsupported`.
pub struct MockSessions {
    clock: std::sync::Arc<dyn Clock>,
    sessions: Mutex<Vec<SessionInfo>>,
}

impl MockSessions {
    /// An empty registry on `clock`.
    pub fn new(clock: std::sync::Arc<dyn Clock>) -> Self {
        Self { clock, sessions: Mutex::new(Vec::new()) }
    }

    /// Why each ended session ended, in the order they opened.
    pub fn end_reasons(&self) -> Vec<(SessionId, EndReason)> {
        self.sessions()
            .into_iter()
            .filter_map(|s| match s.state {
                SessionState::Ended { reason, .. } => Some((s.id, reason)),
                _ => None,
            })
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<SessionInfo>> {
        // A panic while holding the lock (a failing test thread) leaves plain data behind; keep reading it.
        self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn with_open<T>(&self, id: SessionId, f: impl FnOnce(&mut SessionInfo) -> T) -> Result<T, OpError> {
        let mut sessions = self.lock();
        let session = sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| OpError::not_found(format!("no session {}", id.0)))?;
        if matches!(session.state, SessionState::Ended { .. }) {
            return Err(OpError::conflict(format!("session {} has ended", id.0)));
        }
        Ok(f(session))
    }
}

/// The plugin id inside a client name `plugin:<id>@<version>`, or the whole name.
fn client_id(client: &str) -> &str {
    let rest = client.strip_prefix("plugin:").unwrap_or(client);
    rest.split_once('@').map_or(rest, |(id, _)| id)
}

fn unsupported<T>(what: &str) -> Result<T, OpError> {
    Err(OpError::unsupported(format!("MockSessions does not support {what}")))
}

impl Sessions for MockSessions {
    fn open(&self, spec: SessionSpec) -> Result<SessionInfo, OpError> {
        if !matches!(spec.on_behalf_of, Actor::Person { .. }) {
            return Err(OpError::invalid("on_behalf_of", "a session works for a person"));
        }
        let mut sessions = self.lock();
        let id = SessionId(sessions.len() as u64 + 1);
        let actor = match spec.transport {
            Transport::Plugin => Actor::Plugin { id: format!("{}#{}", client_id(&spec.client), id.0) },
            _ => Actor::Agent { id: format!("{}#{}", spec.client, id.0), model: spec.model.clone() },
        };
        let now = self.clock.now();
        let info = SessionInfo {
            id,
            actor,
            on_behalf_of: spec.on_behalf_of,
            transport: spec.transport,
            client: spec.client,
            model: spec.model,
            parent: spec.parent,
            role: spec.role,
            grants: spec.grants,
            fence: spec.fence,
            budget: spec.budget,
            spent: Spend::default(),
            limits: spec.limits,
            state: SessionState::Active,
            open_tx: None,
            pending: Vec::new(),
            steps: 0,
            started_at: now,
            last_active_at: now,
        };
        sessions.push(info.clone());
        Ok(info)
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        self.lock().clone()
    }

    fn session(&self, id: SessionId) -> Option<SessionInfo> {
        self.lock().iter().find(|s| s.id == id).cloned()
    }

    fn session_of(&self, actor: &Actor) -> Option<SessionInfo> {
        self.lock().iter().find(|s| &s.actor == actor).cloned()
    }

    fn pause(&self, _: SessionId, _: &Actor) -> Result<SessionInfo, OpError> {
        unsupported("pause")
    }

    fn resume(&self, _: SessionId, _: &Actor) -> Result<SessionInfo, OpError> {
        unsupported("resume")
    }

    fn set_grants(&self, id: SessionId, grants: Grants, by: &Actor) -> Result<SessionInfo, OpError> {
        if !matches!(by, Actor::Person { .. }) {
            return Err(OpError::denied("only a person changes a session's grants"));
        }
        self.with_open(id, |session| {
            session.grants = grants;
            session.clone()
        })
    }

    fn set_budget(&self, _: SessionId, _: Budget, _: &Actor) -> Result<SessionInfo, OpError> {
        unsupported("budgets")
    }

    fn charge(&self, _: SessionId, _: Spend) -> Result<SessionInfo, OpError> {
        unsupported("charges")
    }

    fn end(&self, id: SessionId, reason: EndReason) -> Result<Ended, OpError> {
        let at: Seconds = self.clock.now();
        self.with_open(id, |session| {
            session.state = SessionState::Ended { reason, at };
            Ended { abandoned: Vec::new(), kept: Vec::new(), withdrawn: Vec::new(), cancelled: Vec::new() }
        })
    }

    fn undo_session(&self, _: SessionId, _: &Actor) -> Result<SessionUndo, OpError> {
        unsupported("session undo")
    }

    fn approvals(&self) -> Vec<PendingApproval> {
        Vec::new()
    }

    fn resolve(&self, _: ApprovalId, _: Decision, _: &Actor) -> Result<Resolution, OpError> {
        unsupported("approvals")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use omuzaki_core::time::FakeClock;
    use omuzaki_ops_api::{Limits, Scope};

    use super::*;

    fn spec(client: &str) -> SessionSpec {
        SessionSpec {
            transport: Transport::Plugin,
            client: client.into(),
            model: None,
            on_behalf_of: Actor::Person { name: "p".into() },
            grants: Grants::of([Scope::Read]),
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        }
    }

    #[test]
    fn opens_plugin_actors_and_keeps_end_reasons() {
        let clock = Arc::new(FakeClock::new(Seconds(1.0)));
        let sessions = MockSessions::new(clock.clone());
        let a = sessions.open(spec("plugin:example@0.1.0")).unwrap();
        assert_eq!(a.actor, Actor::Plugin { id: "example#1".into() });
        clock.advance(Seconds(2.0));
        sessions.end(a.id, EndReason::faulted("trap in init")).unwrap();
        assert_eq!(sessions.end_reasons(), [(a.id, EndReason::faulted("trap in init"))]);
        assert_eq!(sessions.end(a.id, EndReason::Closed).unwrap_err().code, omuzaki_ops_api::ErrorCode::Conflict);
        let agent = Actor::Agent { id: "x".into(), model: None };
        let mut bad = spec("plugin:b@1.0.0");
        bad.on_behalf_of = agent.clone();
        assert!(sessions.open(bad).is_err());
        let b = sessions.open(spec("plugin:b@1.0.0")).unwrap();
        assert!(sessions.set_grants(b.id, Grants::default(), &agent).is_err());
        assert_eq!(sessions.set_grants(b.id, Grants::default(), &a.on_behalf_of).unwrap().grants, Grants::default());
    }
}
