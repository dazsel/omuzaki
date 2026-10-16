//! `script.save`: save a script or macro in the project.

use omuzaki_core::id::Slug;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::RecordedCall;
use crate::saved::{Body, Item, MAX_NAME_CHARS, project_patch};

/// Saves a script (`source`, Rhai text) or a macro (`steps`, a list of `{op, args}` calls) in the project under `id`.
///
/// It writes the document node `scripts/<id>`, with its author and content hash: a real patch, so it is journalled,
/// branchable and undone with Ctrl+Z (`Undo::Auto`), and agents see it through `script.list`. Saving over an existing
/// `id` replaces it. The copy in My scripts is [`script.save_mine`](crate::ops::save_mine::ScriptSaveMine).
///
/// It needs the `script` scope, which no default agent ceiling holds, so an agent can't plant a script for a person to
/// run. A script may call it: it is a project edit like any other and the run's rollback undoes it.
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
}

/// Checks a save's name and body; shared by the project and My scripts ops.
///
/// # Errors
/// `Invalid`, naming `name`, `source` or `steps`.
pub(crate) fn validated_body(
    name: &str,
    source: Option<&str>,
    steps: Option<&[RecordedCall]>,
) -> Result<Body, OpError> {
    if name.trim().is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(OpError::invalid("name", format!("a name is 1 to {MAX_NAME_CHARS} characters")));
    }
    Body::validated(source, steps)
}

impl Op for ScriptSave {
    fn meta() -> OpMeta {
        OpMeta {
            name: "script.save",
            summary: "Save a script or macro in the project.",
            kind: OpKind::Edit,
            scope: Scope::Script,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let body = validated_body(&self.name, self.source.as_deref(), self.steps.as_deref())?;
        let stored = Item::stored(self.id.as_str(), &self.name, &body);
        let output = json!({ "id": self.id, "name": self.name, "kind": body.kind(), "place": "project", "hash": stored.hash });
        let node = Item::node(&self.name, &body, ctx.actor, &stored.hash);
        let patch = project_patch(ctx.doc, self.id.as_str(), Some(node))?;
        Ok(Plan { output: Some(output), ..Plan::edit(patch) })
    }
}

register_op!(ScriptSave);
