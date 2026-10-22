//! `face.expression.set_default`: the expression a face returns to when no held expression shows (FE-03).

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::Patch;
use omuzaki_face_api::{ExpressionId, ExpressionKind};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::store::{entry, library, missing, read_entry};
use crate::ops_shared::edit::{changed, stored_face};

/// Sets the expression a character's face returns to when no held expression shows, for example a faint smile; `null`
/// returns it to the resting face. It must be one of the face's moods or accents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "character": "kit", "id": "relaxed" }, { "character": "kit", "id": null }],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetDefaultExpression {
    /// The character whose face changes.
    pub character: CharId,
    /// The default expression, or `null` for none.
    pub id: Option<ExpressionId>,
}

impl Op for SetDefaultExpression {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.expression.set_default",
            summary: "Set the expression a face returns to when no held expression shows (null for the resting face)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let face = stored_face(ctx, &self.character)?;
        if face.as_ref().is_some_and(|f| !f.is_object()) {
            return Err(OpError::conflict("the face is not an object").hint("fix the character file"));
        }
        if let Some(id) = &self.id {
            let stored = entry(library(face.as_ref())?, id).ok_or_else(|| missing(id))?;
            let kind = read_entry(stored, id)?.kind;
            if !matches!(kind, ExpressionKind::Emotion | ExpressionKind::Accent) {
                return Err(OpError::invalid("id", format!("'{id}' is a viseme or blink, not a mood or an accent"))
                    .hint("lip sync and blinking drive those; pick a mood such as relaxed or happy"));
            }
        }
        let before = face.as_ref().and_then(|f| f.get("default_expression")).cloned();
        let after = self.id.as_ref().map(|id| Value::String(id.as_str().to_owned()));
        let mut patch = Patch::new();
        if before != after {
            patch = patch.with(paths::face(&self.character).join("default_expression"), before, after);
        }
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({
            "default_expression": self.id.as_ref().map(ExpressionId::as_str),
            "changed": changed(&plan.patch),
        }));
        Ok(plan)
    }
}

register_op!(SetDefaultExpression);
