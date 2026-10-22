//! The `obs.*` ops: OBS Studio controlled as ops (spec v3 BR-19), so a person, a hotkey, the automation graph's
//! `op.run` node and the AI performer all drive OBS the same way, under the `broadcast` scope.
//!
//! | Module | Ops |
//! |---|---|
//! | [`connection`] | `obs.connect`, `obs.disconnect` |
//! | [`query`] | `obs.status`, `obs.scenes`, `obs.sources` |
//! | [`action`] | `obs.scene.set`, `obs.source.set_visible`, `obs.stream.start`/`stop`, `obs.record.start`/`stop`, `obs.replay.start`/`stop`/`save` |
//!
//! Everything that changes OBS is a **live action** (kind `Edit`, `Undo::None`): it acts on another app and on the
//! audience, never on the document. The password is never an argument: `obs.connect` names a secret label and reads the
//! value from the secret store inside its run, so no value reaches an op record, a log or MCP output. A dry run says
//! whether the action would work now and sends nothing.

pub mod action;
pub mod connection;
pub mod query;

#[cfg(test)]
mod tests;

use omuzaki_ops_api::{OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use serde_json::{Value, json};

use crate::client::ObsError;
use crate::service::ObsService;

/// Maps a connection error to the op error a person or an agent can act on.
#[allow(clippy::needless_pass_by_value)] // Used as `map_err(op_error)`.
pub(crate) fn op_error(e: ObsError) -> OpError {
    match e {
        ObsError::Unreachable { .. } => OpError::unsupported(e.to_string())
            .hint("start OBS and turn on Tools > WebSocket Server Settings > Enable WebSocket server"),
        ObsError::RemoteNotAllowed(_) => OpError::denied(e.to_string())
            .hint("a person allows it in Settings > Connectors > OBS (obs.allow_remote)"),
        ObsError::Auth(_) => OpError::denied(e.to_string()).hint(
            "the password is wrong or missing: a person stores it (OBS shows it in WebSocket Server Settings) under \
             the secret label obs/websocket, in Settings > Connectors > OBS",
        ),
        ObsError::Closed(_) => OpError::conflict(e.to_string())
            .hint("connect to OBS first (obs.connect) or check Settings > Connectors > OBS"),
        ObsError::Refused { .. } => OpError::conflict(e.to_string()),
        ObsError::Timeout(_) | ObsError::Protocol(_) => OpError::internal(e.to_string()),
    }
}

pub(crate) fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a ObsService, OpError> {
    ctx.service::<ObsService>()
}

/// The answer of a live action's dry run: whether it would reach OBS now.
pub(crate) fn would(service: &ObsService, what: &Value) -> Plan {
    let connected = service.client().is_some();
    let mut answer = json!({ "applied": false, "connected": connected });
    if let (Some(a), Some(w)) = (answer.as_object_mut(), what.as_object()) {
        a.extend(w.clone());
    }
    if !connected {
        answer["hint"] = json!("connect with obs.connect first");
    }
    Plan::answer(answer)
}

pub(crate) fn live_action(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::Broadcast, undo: Undo::None }
}

pub(crate) fn query(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
}

/// The name in `names` closest to `wanted` (case-insensitive edit distance), for "did you mean".
pub(crate) fn nearest<'a>(wanted: &str, names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let wanted = wanted.to_lowercase();
    names.into_iter().min_by_key(|name| distance(&wanted, &name.to_lowercase()))
}

/// Levenshtein distance over characters.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous + usize::from(ca != *cb);
            previous = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(previous + 1);
        }
    }
    row[b.len()]
}
