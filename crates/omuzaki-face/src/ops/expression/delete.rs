//! `face.expression.delete`: removes one named expression, except the visemes and `blink`.

use omuzaki_char_api::CharId;
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::rules::{is_reserved, refuse_reserved};
use super::store::{edited_face, entry, library, missing, patch_to};
use crate::ops_shared::edit::{changed, stored_face};

/// Removes a named expression from a character's face. The five visemes and `blink` can't be removed: lip sync and the
/// VRM export name them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteExpression {
    /// The character whose expression goes.
    pub character: CharId,
    /// The expression to remove.
    pub id: ExpressionId,
}

impl Op for DeleteExpression {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.expression.delete",
            summary: "Remove a named expression from a character's face (the visemes and blink can't be removed)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if is_reserved(&self.id) {
            return Err(refuse_reserved(&self.id, "deleted"));
        }
        let face = stored_face(ctx, &self.character)?;
        if entry(library(face.as_ref())?, &self.id).is_none() {
            return Err(missing(&self.id));
        }
        let working = edited_face(face.as_ref(), |entries| {
            entries.remove(self.id.as_str());
        })?;
        let mut plan = Plan::edit(patch_to(&self.character, face.as_ref(), &working));
        plan.output = Some(json!({ "id": self.id.as_str(), "changed": changed(&plan.patch) }));
        Ok(plan)
    }
}

register_op!(DeleteExpression);
