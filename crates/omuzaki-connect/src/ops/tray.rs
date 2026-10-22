//! The tray's ops (D7, CN-03): list the candidates, and accept or reject one. Agents propose, people decide: an agent
//! may decide only what its own session asked for.

use omuzaki_connect_api::{Candidate, CandidateId, CandidateState, Capability, ConnectorId, MediaKind, tray};
use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Actor, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// A candidate's state, as a filter.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrayState {
    /// Waiting for a decision.
    Pending,
    /// Accepted: usable.
    Accepted,
    /// Rejected: kept for the audit log.
    Rejected,
}

impl TrayState {
    fn of(state: &CandidateState) -> Self {
        match state {
            CandidateState::Pending => Self::Pending,
            CandidateState::Accepted { .. } => Self::Accepted,
            CandidateState::Rejected { .. } => Self::Rejected,
        }
    }
}

/// Lists the tray's candidates, sorted by id: generated files waiting for a decision, and those accepted or rejected.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectTrayList {
    /// Only candidates in this state.
    #[serde(default)]
    pub state: Option<TrayState>,
    /// Only candidates made for this capability.
    #[serde(default)]
    pub capability: Option<Capability>,
}

/// One row of `connect.tray_list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrayRow {
    /// The candidate.
    pub id: CandidateId,
    /// The connector that made it.
    pub connector: ConnectorId,
    /// What kind of file it is.
    pub kind: MediaKind,
    /// Where it is.
    pub state: TrayState,
    /// What it cost, micro-dollars.
    pub usd_micros: u64,
    /// When it landed.
    pub created_at: Seconds,
    /// Who asked for it.
    pub requested_by: Actor,
    /// The candidate it was regenerated from.
    pub regenerated_from: Option<CandidateId>,
    /// Its prompt.
    pub prompt: Option<String>,
}

impl TrayRow {
    fn of(c: Candidate) -> Self {
        Self {
            state: TrayState::of(&c.state),
            prompt: c.request.prompt_text().map(str::to_owned),
            id: c.id,
            connector: c.connector,
            kind: c.kind,
            usd_micros: c.usd_micros,
            created_at: c.created_at,
            requested_by: c.requested_by,
            regenerated_from: c.regenerated_from,
        }
    }
}

impl Op for ConnectTrayList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "connect.tray_list",
            summary: "List the tray's generated files: pending ones wait for a decision, accepted ones are usable.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let rows: Vec<TrayRow> = tray::read_all(ctx.doc)
            .map_err(super::tray_error)?
            .into_iter()
            .filter(|c| self.state.is_none_or(|s| TrayState::of(&c.state) == s))
            .filter(|c| self.capability.is_none_or(|cap| c.request.capability() == cap))
            .map(TrayRow::of)
            .collect();
        let rows = serde_json::to_value(rows).map_err(|e| OpError::internal(format!("writing the tray: {e}")))?;
        Ok(Plan::answer(rows))
    }
}

/// The record `id`, if `actor` may decide it.
fn decidable(ctx: &OpCtx<'_>, id: &CandidateId) -> Result<Candidate, OpError> {
    let c = tray::get(ctx.doc, id)
        .map_err(super::tray_error)?
        .ok_or_else(|| OpError::not_found(format!("no tray candidate {id}")).hint("connect.tray_list shows them"))?;
    if !c.may_decide(ctx.actor) {
        return Err(OpError::denied(format!("candidate {id} was asked for by someone else"))
            .hint("an agent decides only what it asked for; ask the person to decide this one"));
    }
    Ok(c)
}

/// Accepts a pending tray candidate, so other ops may use its file. A person may accept any; an agent only what it
/// asked for. Accepting asserts no rights: the file keeps the licence on record, and publishing still checks it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectAccept {
    /// The candidate, as `connect.tray_list` names it.
    pub candidate: CandidateId,
}

impl Op for ConnectAccept {
    fn meta() -> OpMeta {
        OpMeta {
            name: "connect.accept",
            summary: "Accept a pending tray candidate so its file can be used (an agent may accept only what it asked for).",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        decidable(ctx, &self.candidate)?;
        let state = CandidateState::accepted(ctx.actor.clone(), ctx.clock.now());
        let patch = tray::decide(ctx.doc, &self.candidate, &state).map_err(super::tray_error)?;
        Ok(Plan { patch, output: Some(json!({ "candidate": self.candidate, "state": "accepted" })), ..Plan::default() })
    }
}

/// Rejects a pending tray candidate. Its record stays, with its request and provenance, for the audit log; its
/// file is not deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectReject {
    /// The candidate, as `connect.tray_list` names it.
    pub candidate: CandidateId,
    /// Why, for the record.
    #[serde(default)]
    pub reason: Option<String>,
}

impl Op for ConnectReject {
    fn meta() -> OpMeta {
        OpMeta {
            name: "connect.reject",
            summary: "Reject a pending tray candidate; its record stays for the audit log (an agent may reject only what it asked for).",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        decidable(ctx, &self.candidate)?;
        let state = CandidateState::rejected(ctx.actor.clone(), ctx.clock.now(), self.reason.clone());
        let patch = tray::decide(ctx.doc, &self.candidate, &state).map_err(super::tray_error)?;
        Ok(Plan { patch, output: Some(json!({ "candidate": self.candidate, "state": "rejected" })), ..Plan::default() })
    }
}

register_op!(ConnectTrayList);
register_op!(ConnectAccept);
register_op!(ConnectReject);
