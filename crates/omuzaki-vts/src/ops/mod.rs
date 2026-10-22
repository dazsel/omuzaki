//! The `vts.*` ops: the `VTube Studio` bridge (CN-13) driven as ops, so a person, a hotkey, the automation graph's
//! `op.run` node and an agent steer it the same way.
//!
//! | Module | Ops |
//! |---|---|
//! | [`connection`] | `vts.connect`, `vts.disconnect`, `vts.status` |
//! | [`mapping`] | `vts.map`, `vts.suggest`, `vts.trigger`, `vts.release` |
//!
//! Everything that changes VTS is a **live action** (kind `Edit`, `Undo::None`, scope `perform`): it acts on another
//! app, never on the document. The mapping is the exception: `vts.map` edits the project's `connections/vts` (scope
//! `edit:scene`, undoable), and the other ops hand the stored mapping to the bridge before they use it.
//!
//! The authentication token never appears in an argument or an answer: the service keeps it in its token store (the
//! keychain in the app, under `vts/<instance>`). A dry run says whether the action would work now and sends nothing.

pub mod connection;
pub mod mapping;

#[cfg(test)]
mod tests;

use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use serde_json::json;

use crate::client::{BridgeStatus, Connected};
use crate::hotkeys::VtsMapping;
use crate::service::VtsService;

/// Where the project keeps its connections.
pub(crate) fn connections() -> DocPath {
    DocPath::root().join("connections")
}

/// Where the project keeps its `VTube Studio` mapping (a [`VtsMapping`]).
pub(crate) fn mapping_path() -> DocPath {
    connections().join("vts")
}

/// The project's mapping (empty when it has none).
pub(crate) fn stored_mapping(ctx: &OpCtx<'_>) -> Result<VtsMapping, OpError> {
    match ctx.doc.get(&mapping_path()) {
        Some(value) => serde_json::from_value(value.clone()).map_err(|e| {
            OpError::invalid("connections/vts", format!("the stored VTube Studio mapping doesn't read: {e}"))
                .hint("set it again with vts.map")
        }),
        None => Ok(VtsMapping::default()),
    }
}

/// The project's mapping, handed to the bridge too, so what the bridge runs is what the project says now (an undone
/// `vts.map` included).
pub(crate) fn synced_mapping(ctx: &OpCtx<'_>, service: &VtsService) -> Result<VtsMapping, OpError> {
    let mapping = stored_mapping(ctx)?;
    service.set_mapping(mapping.clone());
    Ok(mapping)
}

/// The hint for every "VTS isn't there" answer.
pub(crate) const START_HINT: &str =
    "start VTube Studio on this computer and turn on Settings > VTube Studio Plugins > Start API, then vts.connect";

pub(crate) fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a VtsService, OpError> {
    ctx.service::<VtsService>()
}

/// The session, or the error an agent can act on.
pub(crate) fn connected(service: &VtsService) -> Result<Connected, OpError> {
    match service.status() {
        BridgeStatus::Connected(c) => Ok(c),
        BridgeStatus::AwaitingApproval => Err(OpError::conflict("VTube Studio is asking the person to allow Omuzaki")
            .hint("the person clicks Allow in VTube Studio's popup")),
        BridgeStatus::Revoked { .. } | BridgeStatus::Denied => {
            Err(OpError::denied("VTube Studio's access for Omuzaki was denied or revoked")
                .hint("a person runs vts.connect and allows Omuzaki in VTube Studio's popup"))
        }
        BridgeStatus::Connecting { .. } => {
            Err(OpError::conflict("the bridge is still reaching VTube Studio").hint(START_HINT))
        }
        _ => Err(OpError::conflict("not connected to VTube Studio").hint(START_HINT)),
    }
}

/// The session; in a dry run with none, the answer saying the action would need one (a dry run never fails for want
/// of VTS, so an agent can plan before VTS is running).
pub(crate) fn session_or_offline(ctx: &OpCtx<'_>, service: &VtsService) -> Result<Result<Connected, Plan>, OpError> {
    match connected(service) {
        Ok(session) => Ok(Ok(session)),
        Err(error) if ctx.dry_run => Ok(Err(Plan::answer(json!({
            "applied": false,
            "connected": false,
            "why": error.message,
            "hint": error.hint.unwrap_or_else(|| START_HINT.to_owned()),
        })))),
        Err(error) => Err(error),
    }
}

pub(crate) fn live_action(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::Perform, undo: Undo::None }
}

pub(crate) fn query(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
}
