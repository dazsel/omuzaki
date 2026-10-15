//! `script.delete`: remove a saved script or macro.

use omuzaki_core::id::Slug;
use omuzaki_ops_api::{Effect, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ops::save::ScriptSave;
use crate::saved::{Item, Place, call, not_saved, project_patch, store};

/// Removes the script or macro saved as `id` from the project (`place: project`, the document node `scripts/<id>`) or
/// from My scripts (`place: mine`, through the app's store; `Unavailable` without one). `NotFound` when nothing is saved
/// there.
///
/// It needs the `script` scope, like `script.save`, and is `Undo::Explicit` for the same reason: the project removal is a
/// patch, the My scripts removal an effect whose undo is a `script.save` of what was removed. Until the executor can
/// reverse effects (ADR 0036 decision 9), undoing a step that holds a delete is refused. A script can't call it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptDelete {
    /// The id it was saved under.
    pub id: Slug,
    /// Where it is saved.
    pub place: Place,
}

impl Op for ScriptDelete {
    fn meta() -> OpMeta {
        OpMeta {
            name: "script.delete",
            summary: "Delete a saved script or macro from the project or from My scripts.",
            kind: OpKind::Edit,
            scope: Scope::Script,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let output = json!({ "id": self.id, "place": self.place });
        match self.place {
            Place::Project => {
                let patch = project_patch(ctx.doc, self.id.as_str(), None)?;
                Ok(Plan { output: Some(output), ..Plan::edit(patch) })
            }
            Place::Mine => {
                let store = store(ctx)?;
                let old = store.get(self.id.as_str())?.ok_or_else(|| not_saved(self.id.as_str(), Place::Mine))?;
                let old = Item::from_stored(&old).map_err(OpError::internal)?;
                let restore = ScriptSave::restoring(&old, Place::Mine)?;
                if !ctx.dry_run {
                    store.remove(self.id.as_str())?;
                }
                let effect = Effect {
                    summary: format!("deleted `{}` from My scripts", self.id),
                    undo: call("script.save", json!(restore)),
                    redo: call("script.delete", json!(self)),
                };
                Ok(Plan { output: Some(output), effect: Some(effect), ..Plan::default() })
            }
        }
    }
}

register_op!(ScriptDelete);
