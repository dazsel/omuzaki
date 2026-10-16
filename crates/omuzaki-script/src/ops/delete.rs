//! `script.delete`: remove a saved script or macro from the project.

use omuzaki_core::id::Slug;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::saved::project_patch;

/// Removes the script or macro saved as `id` in the project (the document node `scripts/<id>`). `NotFound` when nothing
/// is saved there. The one in My scripts is [`script.delete_mine`](crate::ops::delete_mine::ScriptDeleteMine).
///
/// It needs the `script` scope, like `script.save`, and undoes with its patch (`Undo::Auto`), so Ctrl+Z puts the item
/// back. A script may call it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptDelete {
    /// The id it was saved under.
    pub id: Slug,
}

impl Op for ScriptDelete {
    fn meta() -> OpMeta {
        OpMeta {
            name: "script.delete",
            summary: "Delete a saved script or macro from the project.",
            kind: OpKind::Edit,
            scope: Scope::Script,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let patch = project_patch(ctx.doc, self.id.as_str(), None)?;
        Ok(Plan { output: Some(json!({ "id": self.id, "place": "project" })), ..Plan::edit(patch) })
    }
}

register_op!(ScriptDelete);
