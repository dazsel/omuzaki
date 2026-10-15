//! `script.save`: save a script or macro in the project or in My scripts.

use omuzaki_core::id::Slug;
use omuzaki_ops_api::{Effect, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::saved::{Body, Item, MAX_NAME_CHARS, Place, call, project_patch, store};
use crate::RecordedCall;

/// Saves a script (`source`, Rhai text) or a macro (`steps`, a list of `{op, args}` calls) under `id`.
///
/// `place: project` writes the document node `scripts/<id>`, with its author and content hash: a real patch, so it is
/// journalled and branchable, and agents see it through `script.list`. `place: mine` writes My scripts through the app's
/// store and changes no document node; without a store it is `Unavailable`. Saving over an existing `id` replaces it.
///
/// It needs the `script` scope, which no default agent ceiling holds, so an agent can't plant a script for a person to
/// run. The op is `Undo::Explicit`, as its two places differ: the project save has a real patch and no effect; the My
/// scripts save has an empty patch and an effect whose undo is `script.delete` (or, over an earlier item, `script.save`
/// of it). Until the executor can reverse effects (ADR 0036 decision 9), undoing a step that holds a save is refused.
/// It can't be called from a script, which may not call `Undo::Explicit` ops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptSave {
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
    /// Where to save it.
    pub place: Place,
}

impl ScriptSave {
    fn body(&self) -> Result<Body, OpError> {
        if self.name.trim().is_empty() || self.name.chars().count() > MAX_NAME_CHARS {
            return Err(OpError::invalid("name", format!("a name is 1 to {MAX_NAME_CHARS} characters")));
        }
        Body::validated(self.source.as_deref(), self.steps.as_deref())
    }

    /// The save that puts `item` back where it was.
    pub(crate) fn restoring(item: &Item, place: Place) -> Result<Self, OpError> {
        let id = Slug::new(&item.id).map_err(|e| OpError::invalid("id", e.to_string()))?;
        let (source, steps) = match &item.body {
            Body::Source(source) => (Some(source.clone()), None),
            Body::Steps(steps) => (None, Some(steps.clone())),
        };
        Ok(Self { id, name: item.name.clone(), source, steps, place })
    }
}

impl Op for ScriptSave {
    fn meta() -> OpMeta {
        OpMeta {
            name: "script.save",
            summary: "Save a script or macro in the project or in My scripts.",
            kind: OpKind::Edit,
            scope: Scope::Script,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let body = self.body()?;
        let stored = Item::stored(self.id.as_str(), &self.name, &body);
        let output = json!({ "id": self.id, "name": self.name, "kind": body.kind(), "place": self.place, "hash": stored.hash });
        match self.place {
            Place::Project => {
                let node = Item::node(&self.name, &body, ctx.actor, &stored.hash);
                let patch = project_patch(ctx.doc, self.id.as_str(), Some(node))?;
                Ok(Plan { output: Some(output), ..Plan::edit(patch) })
            }
            Place::Mine => {
                let store = store(ctx)?;
                let previous = store.get(self.id.as_str())?;
                let undo = match &previous {
                    Some(old) => {
                        let old = Item::from_stored(old).map_err(OpError::internal)?;
                        call("script.save", json!(Self::restoring(&old, Place::Mine)?))
                    }
                    None => call("script.delete", json!({ "id": self.id, "place": Place::Mine })),
                };
                if !ctx.dry_run {
                    store.put(stored)?;
                }
                let effect = Effect {
                    summary: format!("saved `{}` in My scripts", self.id),
                    undo,
                    redo: call("script.save", json!(self)),
                };
                Ok(Plan { output: Some(output), effect: Some(effect), ..Plan::default() })
            }
        }
    }
}

register_op!(ScriptSave);
