//! The approval queue (design `agent-sessions.md` §7, ADR 0020): every "ask a person" waits here, and each entry is
//! taken out exactly once.
//!
//! The queue only stores and checks. Deciding whether a request queues, running an approved one and emitting the
//! events are the executor's (`run.rs`, `controls.rs`), under the same lock, so an approval is resolved or withdrawn
//! exactly once even when a person and a session end race.

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{
    Actor, ApprovalId, ApprovalRequest, Decision, Need, OpError, PendingApproval, Plan, Request, Scope, SessionId,
    Transport,
};

/// One queued approval and what the executor needs to act on it.
pub(crate) struct Entry {
    /// What the person sees.
    pub(crate) pending: PendingApproval,
    /// The request to run on approval, with the grants it was sent with (they only narrow). `None` for a need with no
    /// request.
    pub(crate) request: Option<Request>,
    /// For a review of a scope grant: the scope the approved run is given.
    pub(crate) grant: Option<Scope>,
    /// For a review: how many distinct reviewers must approve, and who has so far.
    pub(crate) required: u8,
    pub(crate) reviewers: Vec<Actor>,
}

/// What [`Queue::push`] stores for a request.
pub(crate) struct Ask {
    pub(crate) session: SessionId,
    pub(crate) actor: Actor,
    pub(crate) need: Need,
    pub(crate) request: Option<(Request, Plan)>,
    pub(crate) grant: Option<Scope>,
    pub(crate) required: u8,
    pub(crate) at: Seconds,
}

/// Every approval waiting for a person, oldest first.
pub(crate) struct Queue {
    entries: Vec<Entry>,
    last_id: u64,
}

impl Queue {
    pub(crate) fn new() -> Self {
        Self { entries: Vec::new(), last_id: 0 }
    }

    /// Queues `ask` and returns its snapshot. Ids are never reused.
    pub(crate) fn push(&mut self, ask: Ask) -> PendingApproval {
        self.last_id += 1;
        let id = ApprovalId(self.last_id);
        let op = ask.request.as_ref().map(|(request, _)| request.op.as_str());
        let pending = PendingApproval {
            id,
            session: ask.session,
            actor: ask.actor,
            batch: batch_key(ask.session, op, &ask.need),
            need: ask.need,
            request: ask.request.as_ref().map(|(request, preview)| ApprovalRequest {
                op: request.op.clone(),
                args: request.args.clone(),
                cause: request.cause.clone(),
                preview: preview.clone(),
            }),
            requested_at: ask.at,
        };
        self.entries.push(Entry {
            pending: pending.clone(),
            request: ask.request.map(|(request, _)| request),
            grant: ask.grant,
            required: ask.required,
            reviewers: Vec::new(),
        });
        pending
    }

    /// Every pending approval, oldest first.
    pub(crate) fn all(&self) -> Vec<PendingApproval> {
        self.entries.iter().map(|entry| entry.pending.clone()).collect()
    }

    /// The ids of `session`'s pending approvals, oldest first.
    pub(crate) fn of_session(&self, session: SessionId) -> Vec<ApprovalId> {
        self.entries.iter().filter(|entry| entry.pending.session == session).map(|entry| entry.pending.id).collect()
    }

    /// How many approvals `session` has waiting.
    pub(crate) fn count(&self, session: SessionId) -> usize {
        self.entries.iter().filter(|entry| entry.pending.session == session).count()
    }

    pub(crate) fn get(&self, id: ApprovalId) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.pending.id == id)
    }

    pub(crate) fn get_mut(&mut self, id: ApprovalId) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|entry| entry.pending.id == id)
    }

    /// Takes approval `id` out: the one step that makes a resolution happen exactly once.
    pub(crate) fn take(&mut self, id: ApprovalId) -> Option<Entry> {
        let index = self.entries.iter().position(|entry| entry.pending.id == id)?;
        Some(self.entries.remove(index))
    }

    /// Takes out every approval of `session`, oldest first: a session that ends withdraws them.
    pub(crate) fn withdraw(&mut self, session: SessionId) -> Vec<Entry> {
        let (gone, kept) = std::mem::take(&mut self.entries).into_iter().partition(|e| e.pending.session == session);
        self.entries = kept;
        gone
    }
}

/// The batching key (design §7.1, item 6): session, op and need kind, plus the scope or policy that is asked for.
/// Identical keys collapse into one card in the UI; each id is still resolved on its own.
fn batch_key(session: SessionId, op: Option<&str>, need: &Need) -> String {
    let need = match need {
        Need::Scope { scope } => format!("scope:{scope}"),
        Need::Person { .. } => "person".to_owned(),
        Need::Budget { .. } => "budget".to_owned(),
        Need::Pairing { .. } => "pairing".to_owned(),
        Need::Review { policy } => format!("review:{policy}"),
    };
    format!("{}/{}/{need}", session.0, op.unwrap_or("-"))
}

/// Checks that `decision` is one the need offers (design §7.1 table), before anything is taken out of the queue.
/// A plugin session's scope needs offer only `Approve` and `Reject`, because nothing widens a plugin's ceiling
/// (`plugins.md` §4); "for this project" needs a credential, which a headless stdio session does not have.
///
/// # Errors
/// `Invalid` (field `decision`), naming what the need offers.
pub(crate) fn check_decision(
    need: &Need,
    decision: &Decision,
    transport: &Transport,
    has_request: bool,
) -> Result<(), OpError> {
    let offered: &[&str] = match need {
        Need::Scope { .. } if *transport == Transport::Plugin => &["approve", "reject"],
        Need::Scope { .. } if *transport == Transport::Stdio => &["approve", "approve_for_session", "reject"],
        Need::Scope { .. } => &["approve", "approve_for_session", "approve_for_project", "reject"],
        // Approving "once" needs a request to run once; a charge or an announced need has none.
        Need::Budget { .. } if has_request => &["approve", "raise_budget", "reject"],
        Need::Budget { .. } => &["raise_budget", "reject"],
        Need::Person { .. } | Need::Pairing { .. } | Need::Review { .. } => &["approve", "reject"],
    };
    let name = decision_name(decision);
    if offered.contains(&name) {
        Ok(())
    } else {
        Err(OpError::invalid("decision", format!("`{name}` is not a decision this approval offers"))
            .hint(format!("decide with one of: {}", offered.join(", "))))
    }
}

/// The decision's name as its JSON spells it.
fn decision_name(decision: &Decision) -> &'static str {
    match decision {
        Decision::Approve => "approve",
        Decision::ApproveForSession => "approve_for_session",
        Decision::ApproveForProject => "approve_for_project",
        Decision::RaiseBudget { .. } => "raise_budget",
        Decision::Reject { .. } => "reject",
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::{Budget, Spend};

    use super::*;

    #[test]
    fn decisions_follow_the_need_and_the_transport() {
        let scope = Need::Scope { scope: Scope::Publish };
        let http = Transport::Http { token: "ci".into() };
        check_decision(&scope, &Decision::ApproveForProject, &http, true).unwrap();
        assert!(check_decision(&scope, &Decision::ApproveForProject, &Transport::Stdio, true).is_err());
        for widening in [Decision::ApproveForSession, Decision::ApproveForProject] {
            assert!(check_decision(&scope, &widening, &Transport::Plugin, true).is_err());
        }
        let budget = Need::Budget { needed: Spend::default(), left: Spend::default() };
        let raise = Decision::RaiseBudget { budget: Budget::default() };
        check_decision(&budget, &raise, &http, false).unwrap();
        assert!(check_decision(&budget, &Decision::Approve, &http, false).is_err(), "nothing to run once");
        check_decision(&budget, &Decision::Approve, &http, true).unwrap();
        let person = Need::Person { reason: "r".into() };
        assert!(check_decision(&person, &raise, &http, true).is_err());
        let e = check_decision(&person, &Decision::ApproveForSession, &http, true).unwrap_err();
        assert_eq!(e.field.as_deref(), Some("decision"));
    }

    #[test]
    fn batch_keys_name_the_session_op_and_need() {
        assert_eq!(batch_key(SessionId(3), Some("vrm.export"), &Need::Scope { scope: Scope::Publish }), "3/vrm.export/scope:publish");
        assert_eq!(batch_key(SessionId(3), None, &Need::Pairing { client: "c".into() }), "3/-/pairing");
    }
}
