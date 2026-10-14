//! `variant.delete`: removes a variant, refused while another variant extends it.

use omuzaki_char_api::{CharId, VariantId, paths};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;
use crate::ops::character::access as character;
use crate::ops::character::access::list;

/// Deletes a variant and its overrides. Undo restores the whole variant as it was.
///
/// Refused while another variant extends it, and the error names those variants: they would lose the overrides they
/// start from. Nothing else refers to a variant by name (colourways hold palettes, and the stage picks a variant from
/// the head's size on screen), so there is nothing else to check.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VariantDelete {
    /// The character the variant belongs to.
    pub character: CharId,
    /// The variant to delete.
    pub variant: VariantId,
}

impl Op for VariantDelete {
    fn meta() -> OpMeta {
        OpMeta {
            name: "variant.delete",
            summary: "Delete a variant and its overrides; refused, naming them, while another variant extends it.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        character::load(ctx, &self.character)?;
        let removed = access::load(ctx, &self.character, &self.variant)?;
        let dependants = access::extending(ctx, &self.character, &self.variant);
        if !dependants.is_empty() {
            return Err(OpError::invalid(
                "variant",
                format!("can't delete `{}`: {} extend it", self.variant, list(&dependants)),
            )
            .hint(format!(
                "delete {} first with variant.delete, or keep `{}`",
                list(&dependants),
                self.variant
            )));
        }

        let change = character::change(ctx, paths::variant(&self.character, &self.variant), None);
        let summary = json!({
            "character": self.character,
            "removed": self.variant,
            "overrides": removed.overrides.len(),
        });
        Ok(character::edit_plan(vec![change], summary))
    }
}

register_op!(VariantDelete);
