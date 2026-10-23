//! The audit facts the journal keeps in order with the ops (#1058, ADR 0020 §9, `journal.md` §4.4 kind 11): who opened
//! a session for whom and with what grants, every person's change to it, and every approval asked and answered.
//!
//! Per-op facts (`on_behalf_of`, the approval an op ran under) are on the [`OpRecord`](omuzaki_ops_api::OpRecord)
//! already. A fact is built from the runner's [`Event`] in the same lock hold, so journal order is the order of
//! effect. Facts hold labels and ids, never a secret: a session's credential is not on its [`SessionInfo`], and an
//! approval's request is recorded by its op's name, not its arguments.

use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{
    Actor, ApprovalId, Budget, EndReason, Event, Grants, Need, PauseReason, Resolved, SessionChange, SessionId,
    SessionInfo, SessionState, Transport,
};
use serde::{Deserialize, Serialize};

/// The most characters of a person's free text (a rejection's comment, a reason) an audit fact keeps; longer text is
/// cut and ends with [`CUT_MARKER`].
pub const AUDIT_TEXT_MAX: usize = 1000;

/// What ends text cut to [`AUDIT_TEXT_MAX`].
pub const CUT_MARKER: &str = " [cut]";

/// One audit fact. Built by [`AuditFact::from_event`]; read by the history and review surfaces.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum AuditFact {
    /// A session opened.
    #[non_exhaustive]
    SessionOpened {
        /// Which.
        session: SessionId,
        /// Its agent actor.
        actor: Actor,
        /// The person it works for.
        on_behalf_of: Actor,
        /// How the client connected.
        transport: Transport,
        /// The client's sanitised name.
        client: String,
        /// The session that spawned it, for a sub-agent.
        parent: Option<SessionId>,
        /// Its role, if it was given one.
        role: Option<String>,
        /// Its effective grants at the open.
        grants: Grants,
    },
    /// A person changed a session's ceiling, or an ancestor's change moved its effective grants.
    #[non_exhaustive]
    GrantsChanged {
        /// Which.
        session: SessionId,
        /// Who.
        by: Actor,
        /// Its effective grants after the change.
        grants: Grants,
    },
    /// A person changed a session's budget.
    #[non_exhaustive]
    BudgetChanged {
        /// Which.
        session: SessionId,
        /// Who.
        by: Actor,
        /// The new budget.
        budget: Budget,
    },
    /// A person changed a session's fence.
    #[non_exhaustive]
    FenceChanged {
        /// Which.
        session: SessionId,
        /// Who.
        by: Actor,
        /// The new fence; empty releases the claim.
        fence: Vec<DocPath>,
    },
    /// A session was paused, by a person or by its budget.
    #[non_exhaustive]
    Paused {
        /// Which.
        session: SessionId,
        /// Who (the agent actor itself for a budget pause).
        by: Actor,
        /// Why.
        reason: PauseReason,
    },
    /// A session was resumed.
    #[non_exhaustive]
    Resumed {
        /// Which.
        session: SessionId,
        /// Who.
        by: Actor,
    },
    /// A session ended. Its withdrawn approvals come before it, each as an [`ApprovalResolved`](Self::ApprovalResolved).
    #[non_exhaustive]
    SessionEnded {
        /// Which.
        session: SessionId,
        /// Why.
        reason: EndReason,
    },
    /// A request or a session waits for a person.
    #[non_exhaustive]
    ApprovalRequested {
        /// The approval.
        approval: ApprovalId,
        /// The session it belongs to.
        session: SessionId,
        /// The agent actor that asked.
        actor: Actor,
        /// The op it would run, if it is a request (a budget or pairing approval has none).
        op: Option<String>,
        /// What the person has to give.
        need: Need,
    },
    /// An approval was answered, or withdrawn.
    #[non_exhaustive]
    ApprovalResolved {
        /// The approval.
        approval: ApprovalId,
        /// The person who answered; `None` for a withdrawal.
        by: Option<Actor>,
        /// The answer.
        decision: AuditDecision,
        /// A rejection's comment, cut to [`AUDIT_TEXT_MAX`] characters.
        comment: Option<String>,
    },
}

/// How an approval ended, as the audit trail tells it.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDecision {
    /// Approved, and the request ran.
    Approved,
    /// Approved, and running the request failed.
    Failed,
    /// A person said no.
    Rejected,
    /// The session ended first.
    Withdrawn,
}

/// `text` cut to [`AUDIT_TEXT_MAX`] characters, with the marker when it was cut.
pub fn cut_text(text: &str) -> String {
    match text.char_indices().nth(AUDIT_TEXT_MAX) {
        Some((at, _)) => format!("{}{CUT_MARKER}", &text[..at]),
        None => text.to_owned(),
    }
}

impl AuditFact {
    /// The fact `event` records, if it records one: session opens, ends and a person's changes, and approvals.
    /// Spending, undo and job events are not audit facts (an undo is in the journal as itself).
    pub fn from_event(event: &Event) -> Option<Self> {
        match event {
            Event::SessionOpened { session, .. } => Some(Self::opened(session)),
            Event::SessionChanged { session, change, .. } => Self::changed(session, change),
            Event::SessionEnded { session, .. } => match &session.state {
                SessionState::Ended { reason, .. } => {
                    Some(Self::SessionEnded { session: session.id, reason: reason.clone() })
                }
                _ => None,
            },
            Event::ApprovalRequested { approval, .. } => Some(Self::ApprovalRequested {
                approval: approval.id,
                session: approval.session,
                actor: approval.actor.clone(),
                op: approval.request.as_ref().map(|request| request.op.clone()),
                need: cut_need(&approval.need),
            }),
            Event::ApprovalResolved { resolution, .. } => {
                let (decision, comment) = match &resolution.result {
                    Resolved::Applied { .. } => (AuditDecision::Approved, None),
                    Resolved::Failed { .. } => (AuditDecision::Failed, None),
                    Resolved::Rejected { comment } => (AuditDecision::Rejected, comment.as_deref().map(cut_text)),
                    Resolved::Withdrawn { .. } => (AuditDecision::Withdrawn, None),
                };
                Some(Self::ApprovalResolved { approval: resolution.id, by: resolution.by.clone(), decision, comment })
            }
            _ => None,
        }
    }

    fn opened(session: &SessionInfo) -> Self {
        Self::SessionOpened {
            session: session.id,
            actor: session.actor.clone(),
            on_behalf_of: session.on_behalf_of.clone(),
            transport: session.transport.clone(),
            client: cut_text(&session.client),
            parent: session.parent,
            role: session.role.as_deref().map(cut_text),
            grants: session.grants.clone(),
        }
    }

    fn changed(session: &SessionInfo, change: &SessionChange) -> Option<Self> {
        let id = session.id;
        Some(match change {
            SessionChange::Grants { by } => {
                Self::GrantsChanged { session: id, by: by.clone(), grants: session.grants.clone() }
            }
            SessionChange::Budget { by } => Self::BudgetChanged { session: id, by: by.clone(), budget: session.budget },
            SessionChange::Fence { by } => {
                Self::FenceChanged { session: id, by: by.clone(), fence: session.fence.clone() }
            }
            SessionChange::Paused => match &session.state {
                SessionState::Paused { by, reason } => Self::Paused { session: id, by: by.clone(), reason: *reason },
                _ => return None,
            },
            SessionChange::Resumed { by } => Self::Resumed { session: id, by: by.clone() },
            SessionChange::Spent => return None,
        })
    }
}

/// A need with a person's free text cut.
fn cut_need(need: &Need) -> Need {
    match need {
        Need::Person { reason } => Need::Person { reason: cut_text(reason) },
        Need::Pairing { client } => Need::Pairing { client: cut_text(client) },
        other => other.clone(),
    }
}
