//! `script.save_mine`: save a script or macro in My scripts.

use omuzaki_core::id::Slug;
use omuzaki_ops_api::{Effect, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::RecordedCall;
use crate::ops::save::validated_body;
use crate::saved::{Item, call, store};

/// Saves a script (`source`) or a macro (`steps`) in My scripts, the person's own folder across projects, through the
/// app's [`ScriptStore`](crate::ScriptStore); without a store it is `Unavailable`. It changes no document node.
/// Saving over an existing `id` replaces it.
///
/// It needs the `script` scope, like [`script.save`](crate::ops::save::ScriptSave). The op is `Undo::Explicit`: the patch
/// is empty and the effect, whose undo is `script.delete_mine` (or, over an earlier item, `script.save_mine` of it),
/// lives outside the document. Until the executor can reverse effects (ADR 0036 decision 9), undoing a step that holds
/// one is refused. A script can't call it, since a rollback could not undo the write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptSaveMine {
    /// The id to save under: a slug (lowercase letters, digits, `-` and `_`).
    pub id: Slug,
    /// The name shown in lists (at most 100 characters).
    pub name: String,
    /// The Rhai source of a script (at most 64 KiB). Give this or `steps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The calls of a macro. Give this or `source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<RecordedCall>>,
}

impl ScriptSaveMine {
    /// The save that puts `item` back where it was.
    pub(crate) fn restoring(item: &Item) -> Result<Self, OpError> {
        let id = Slug::new(&item.id).map_err(|e| OpError::invalid("id", e.to_string()))?;
        let (source, steps) = match &item.body {
            crate::saved::Body::Source(source) => (Some(source.clone()), None),
            crate::saved::Body::Steps(steps) => (None, Some(steps.clone())),
        };
        Ok(Self { id, name: item.name.clone(), source, steps })
    }
}

impl Op for ScriptSaveMine {
    fn meta() -> OpMeta {
        OpMeta {
            name: "script.save_mine",
            summary: "Save a script or macro in My scripts.",
            kind: OpKind::Edit,
            scope: Scope::Script,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let body = validated_body(&self.name, self.source.as_deref(), self.steps.as_deref())?;
        let stored = Item::stored(self.id.as_str(), &self.name, &body);
        let output = json!({ "id": self.id, "name": self.name, "kind": body.kind(), "place": "mine", "hash": stored.hash });
        let store = store(ctx)?;
        let previous = store.get(self.id.as_str())?;
        let undo = match &previous {
            Some(old) => {
                let old = Item::from_stored(old).map_err(OpError::internal)?;
                call("script.save_mine", json!(Self::restoring(&old)?))
            }
            None => call("script.delete_mine", json!({ "id": self.id })),
        };
        if !ctx.dry_run {
            store.put(stored)?;
        }
        let effect = Effect {
            summary: format!("saved `{}` in My scripts", self.id),
            undo,
            redo: call("script.save_mine", json!(self)),
        };
        Ok(Plan { output: Some(output), effect: Some(effect), ..Plan::default() })
    }
}

register_op!(ScriptSaveMine);
