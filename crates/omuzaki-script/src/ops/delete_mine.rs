//! `script.delete_mine`: remove a saved script or macro from My scripts.

use omuzaki_core::id::Slug;
use omuzaki_ops_api::{Effect, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ops::save_mine::ScriptSaveMine;
use crate::saved::{Item, Place, call, not_saved, store};

/// Removes the script or macro saved as `id` in My scripts, through the app's [`ScriptStore`](crate::ScriptStore)
/// (`Unavailable` without one). `NotFound` when nothing is saved there.
///
/// It needs the `script` scope, like `script.save_mine`, and is `Undo::Explicit` for the same reason: the removal is an
/// effect whose undo is a `script.save_mine` of what was removed. Until the executor can reverse effects (ADR 0036
/// decision 9), undoing a step that holds one is refused. A script can't call it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptDeleteMine {
    /// The id it was saved under.
    pub id: Slug,
}

impl Op for ScriptDeleteMine {
    fn meta() -> OpMeta {
        OpMeta {
            name: "script.delete_mine",
            summary: "Delete a saved script or macro from My scripts.",
            kind: OpKind::Edit,
            scope: Scope::Script,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let output = json!({ "id": self.id, "place": "mine" });
        let store = store(ctx)?;
        let old = store.get(self.id.as_str())?.ok_or_else(|| not_saved(self.id.as_str(), Place::Mine))?;
        let old = Item::from_stored(&old).map_err(OpError::internal)?;
        let restore = ScriptSaveMine::restoring(&old)?;
        if !ctx.dry_run {
            store.remove(self.id.as_str())?;
        }
        let effect = Effect {
            summary: format!("deleted `{}` from My scripts", self.id),
            undo: call("script.save_mine", json!(restore)),
            redo: call("script.delete_mine", json!(self)),
        };
        Ok(Plan { output: Some(output), effect: Some(effect), ..Plan::default() })
    }
}

register_op!(ScriptDeleteMine);
