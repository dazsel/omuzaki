//! Tray records (D7): one [`Candidate`] per generated asset, waiting for a person's (or the asking agent's) decision.

use omuzaki_ai_api::Version;
use omuzaki_core::time::Seconds;
use omuzaki_ops_api::Actor;
use omuzaki_store_api::{ContentHash, Provenance};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::{CandidateId, ConnectorId, MediaKind, RunId};
use crate::request::Request;

/// Where a candidate is. Only a pending one moves, once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CandidateState {
    /// Waiting for a decision; consumers refuse it.
    Pending,
    /// Usable.
    #[non_exhaustive]
    Accepted {
        /// Who accepted it.
        by: Actor,
        /// When.
        at: Seconds,
    },
    /// Not usable; the record stays for the audit log.
    #[non_exhaustive]
    Rejected {
        /// Who rejected it.
        by: Actor,
        /// When.
        at: Seconds,
        /// Why, if they said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl CandidateState {
    /// [`CandidateState::Accepted`].
    pub fn accepted(by: Actor, at: Seconds) -> Self {
        Self::Accepted { by, at }
    }

    /// [`CandidateState::Rejected`].
    pub fn rejected(by: Actor, at: Seconds, reason: Option<String>) -> Self {
        Self::Rejected { by, at, reason }
    }

    /// True for [`CandidateState::Pending`].
    pub fn is_pending(&self) -> bool {
        matches!(self, CandidateState::Pending)
    }
}

/// A generated asset in the tray, at `tray/<id>` in the project document. Rejected ones keep their record (request
/// and provenance) for the audit log; the bytes are in the store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Candidate {
    /// Its id, `r<run>-<index>`.
    pub id: CandidateId,
    /// The run that made it.
    pub run: RunId,
    /// The connector that made it.
    pub connector: ConnectorId,
    /// The connector manifest's version: what replay compares (D10).
    pub connector_version: Version,
    /// The effective request: the seed filled in by the host.
    pub request: Request,
    /// The bytes, by hash.
    pub asset: ContentHash,
    /// Their media type.
    pub media_type: String,
    /// Their kind.
    pub kind: MediaKind,
    /// The candidate's own provenance, a copy, so the audit survives garbage collection. Every reader of a stored
    /// hash joins it in ([`tray::effective_provenance`](crate::tray::effective_provenance)).
    pub provenance: Provenance,
    /// What it cost, micro-dollars.
    pub usd_micros: u64,
    /// Who asked for it.
    pub requested_by: Actor,
    /// When it landed.
    pub created_at: Seconds,
    /// Where it is.
    pub state: CandidateState,
    /// The candidate it was regenerated from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regenerated_from: Option<CandidateId>,
}

impl Candidate {
    /// Whether `actor` may accept or reject it (D7): a person may decide anything; an agent only what it asked for
    /// itself (the same agent id); nobody else.
    pub fn may_decide(&self, actor: &Actor) -> bool {
        match (actor, &self.requested_by) {
            (Actor::Person { .. }, _) => true,
            (Actor::Agent { id, .. }, Actor::Agent { id: asked, .. }) => id == asked,
            _ => false,
        }
    }
}

/// Whether an op may use a stored asset (D7, [`tray::usable`](crate::tray::usable)).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "usable", content = "candidate", rename_all = "snake_case")]
pub enum Usable {
    /// Yes: no record names it, or one accepted it.
    Yes,
    /// Not yet: accept it in the tray first.
    Pending(CandidateId),
    /// No: it was rejected.
    Rejected(CandidateId),
}
