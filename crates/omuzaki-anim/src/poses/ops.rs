//! The query op `pose.list`: what poses exist, so an agent or a cue editor can pick one by name.
//!
//! Playing a pose is an animator or cue concern, not a document edit, so it is not an op.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::library::PoseLibrary;

/// List the library poses (id, summary, tags).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListPoses {}

impl Op for ListPoses {
    fn meta() -> OpMeta {
        OpMeta {
            name: "pose.list",
            summary: "List the library poses (wave, ...) with their id, summary and tags.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let library = PoseLibrary::builtin().map_err(|e| OpError::internal(e.to_string()))?;
        let poses: Vec<_> = library
            .iter()
            .map(|p| json!({ "id": p.id, "summary": p.summary, "tags": p.tags, "mirrorable": p.mirrorable }))
            .collect();
        Ok(Plan::answer(json!({ "poses": poses })))
    }
}

omuzaki_ops_api::register_op!(ListPoses);
