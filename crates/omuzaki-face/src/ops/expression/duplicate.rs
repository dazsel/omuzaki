//! `face.expression.duplicate`: copies a named expression under a new name, which must be free.

use omuzaki_char_api::CharId;
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::rules::{check_kind, is_reserved, refuse_reserved};
use super::store::{edited_face, entry, library, missing, patch_to, read_entry, taken};
use crate::ops_shared::edit::{changed, stored_face, to_json};

/// Copies a named expression under a new name, so a variant can be tuned without touching the original. The visemes and
/// `blink` can't be copied, and the copy's name must be free and must not be a viseme or blink name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DuplicateExpression {
    /// The character whose expression is copied.
    pub character: CharId,
    /// The expression to copy.
    pub id: ExpressionId,
    /// The copy's name. It must be free, and it can't be a viseme or blink name.
    pub to: ExpressionId,
}

impl Op for DuplicateExpression {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.expression.duplicate",
            summary: "Copy a named expression under a free name (the visemes and blink can't be copied)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if is_reserved(&self.id) {
            return Err(refuse_reserved(&self.id, "duplicated"));
        }
        let face = stored_face(ctx, &self.character)?;
        let library = library(face.as_ref())?;
        let stored = entry(library, &self.id).ok_or_else(|| missing(&self.id))?;
        let expression = read_entry(stored, &self.id)?;
        check_kind(&self.to, expression.kind, "to")?;
        if entry(library, &self.to).is_some() {
            return Err(taken(&self.to, "to"));
        }
        let (to, copy) = (self.to.as_str().to_owned(), to_json(&expression)?);
        let working = edited_face(face.as_ref(), |entries| {
            entries.insert(to, copy);
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

register_op!(DuplicateExpression);
