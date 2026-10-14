//! `face.expression.rename`: gives a named expression a new name, which must be free.

use omuzaki_char_api::CharId;
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::rules::{check_kind, is_reserved, refuse_reserved};
use super::store::{edited_face, entry, library, missing, patch_to, read_entry, taken};
use crate::ops::shared::edit::{changed, stored_face, to_json};

/// Gives a named expression a new name. The visemes and `blink` can't be renamed, and the new name must be free and must
/// not be a viseme or blink name, since only the expression of that kind may hold one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameExpression {
    /// The character whose expression is renamed.
    pub character: CharId,
    /// The expression's current name.
    pub id: ExpressionId,
    /// The new name. It must be free, and it can't be a viseme or blink name.
    pub to: ExpressionId,
}

impl Op for RenameExpression {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.expression.rename",
            summary: "Rename a named expression to a free name (the visemes and blink can't be renamed)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if is_reserved(&self.id) {
            return Err(refuse_reserved(&self.id, "renamed"));
        }
        let face = stored_face(ctx, &self.character)?;
        let library = library(face.as_ref())?;
        let stored = entry(library, &self.id).ok_or_else(|| missing(&self.id))?;
        let expression = read_entry(stored, &self.id)?;
        check_kind(&self.to, expression.kind, "to")?;
        if entry(library, &self.to).is_some() {
            return Err(taken(&self.to, "to"));
        }
        let (from, to) = (self.id.as_str().to_owned(), self.to.as_str().to_owned());
        let moved = to_json(&expression)?;
        let working = edited_face(face.as_ref(), |entries| {
            entries.remove(&from);
            entries.insert(to, moved);
        })?;
        let mut plan = Plan::edit(patch_to(&self.character, face.as_ref(), &working));
        plan.output = Some(json!({
            "from": self.id.as_str(),
            "to": self.to.as_str(),
            "changed": changed(&plan.patch),
        }));
        Ok(plan)
    }
}

register_op!(RenameExpression);
