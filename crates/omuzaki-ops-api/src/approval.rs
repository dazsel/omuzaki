//! Approvals: the one queue for every "ask a person" (design `agent-sessions.md` §7, ADR 0020).
//!
//! A request from a session that a person could allow is queued instead of refused. Everything here is **executor
//! output**: the executor allocates ids, builds the pending record and fills in the resolution. A request, a
//! transport or an agent never constructs a [`PendingApproval`] or a [`Resolution`] and has no way to submit one.

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::Value;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::session::{Budget, EndReason, SessionId, Spend};
use crate::{Actor, Cause, OpError, Outcome, Plan, Scope};

/// Identifies one queued approval. Allocated by the executor and unique within its lifetime, so a UI can resolve
/// each card exactly once.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ApprovalId(pub u64);

/// What a person has to give before the queued request (or the waiting session) can go on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "need", rename_all = "snake_case")]
pub enum Need {
    /// The op's scope is missing from the session's grants, but its principal holds it.
    Scope {
        /// The scope to grant.
        scope: Scope,
    },
    /// The op called [`OpCtx::require_person`](crate::OpCtx::require_person) and no person is acting or confirming.
    Person {
        /// Why, shown on the card (for example the ADR 0016 disclaimer of a model switch).
        reason: String,
    },
    /// Running the request, or the session's spend or time, would pass its budget. The session is paused too.
    Budget {
        /// What the request (or the charge) needs.
        needed: Spend,
        /// What the budget still allows.
        left: Spend,
    },
    /// A bridge connection presented a credential the studio has not paired yet. There is no request.
    Pairing {
        /// The sanitised client name, as the client declared it.
        client: String,
    },
    /// A team policy wants a reviewer for this request (VC-11, phase 5). Reserved: nothing queues it yet.
    Review {
        /// The policy that asks.
        policy: String,
    },
}

/// The request a [`PendingApproval`] waits to run. Kept so the approver sees what will happen and the executor can
/// re-plan it against the current document on approval (design §4.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalRequest {
    /// The op's registered name.
    pub op: String,
    /// Its arguments, as the agent sent them.
    pub args: Value,
    /// Why the agent said it asked.
    pub cause: Option<Cause>,
    /// The dry-run's plan (patch, output, cost): the card's preview. The executor dry-runs before queueing, so a
    /// request that would fail is never queued.
    pub preview: Plan,
}

/// One approval waiting for a person. Built by the executor, never by a request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingApproval {
    /// Its id.
    pub id: ApprovalId,
    /// The session that asked.
    pub session: SessionId,
    /// That session's agent actor.
    pub actor: Actor,
    /// What was asked, or `None` for a need with no request ([`Need::Pairing`], a budget crossed by a charge).
    pub request: Option<ApprovalRequest>,
    /// What a person must give.
    pub need: Need,
    /// The batching key (session, op, need): identical keys collapse into one card in the UI.
    pub batch: String,
    /// When it was queued, on the runner's clock.
    pub requested_at: Seconds,
}

/// What a person decides. Offered subsets depend on the [`Need`] (design §7.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Allow this request (or pair, for [`Need::Pairing`]).
    Approve,
    /// Allow it and widen the session's ceiling for the rest of the session.
    ApproveForSession,
    /// Allow it and remember the grant on the session's credential. Offered only to credential sessions.
    ApproveForProject,
    /// Set a new budget and resume the budget-paused session.
    RaiseBudget {
        /// The new budget.
        budget: Budget,
    },
    /// Refuse. The agent receives the comment.
    Reject {
        /// Why, for the agent.
        comment: Option<String>,
    },
}

/// How an approval ended up.
// One per approval, built once and sent to subscribers: boxing the outcome would only change the contract's shape.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Resolved {
    /// Approved, re-planned and applied (or dry-run answered).
    Applied {
        /// What running it produced.
        outcome: Outcome,
    },
    /// Approved, but running it failed. A second need is reported here too, as a new approval.
    Failed {
        /// Why.
        error: OpError,
    },
    /// A person said no.
    Rejected {
        /// Their comment.
        comment: Option<String>,
    },
    /// The session ended first, so the request never ran.
    Withdrawn {
        /// Why the session ended.
        reason: EndReason,
    },
}

/// The final word on one approval. Exactly one per [`ApprovalId`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Resolution {
    /// Which approval.
    pub id: ApprovalId,
    /// The person who decided, or `None` when it was withdrawn.
    pub by: Option<Actor>,
    /// What happened.
    pub result: Resolved,
    /// When, on the runner's clock.
    pub at: Seconds,
}

/// Links an applied op to the approval that allowed it, and the approver (`OpRecord.approval`). Stamped by the
/// executor, so history proves who allowed what (VC-11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalRef {
    /// The approval.
    pub id: ApprovalId,
    /// The person who approved.
    pub by: Actor,
}
