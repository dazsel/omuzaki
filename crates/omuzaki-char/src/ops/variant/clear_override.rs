//! `variant.clear_override`: removes one override from a variant, so the base value shows through again.

use omuzaki_char_api::{CharId, VariantId, resolve_variant};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;
use crate::ops::character::access as character;
use crate::ops::character::access::{list, not_found};

/// Removes one override from a variant: the base character's value at that path shows through again. The variant must
/// still resolve without it, so an override another override depends on is refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "variant": "close-up", "path": "face/layout/eye_spacing"}
    ],
    "x-omuzaki-fixture" = "character-variant"
))]
pub struct VariantClearOverride {
    /// The character the variant belongs to.
    pub character: CharId,
    /// The variant to change.
    pub variant: VariantId,
    /// The override to remove, written exactly as `variant.set_override` stored it (for example `face/layout/mouth_y`).
    pub path: String,
}

impl Op for VariantClearOverride {
    fn meta() -> OpMeta {
        OpMeta {
            name: "variant.clear_override",
            summary: "Remove one override from a variant, so the base value shows through again.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = character::load(ctx, &self.character)?;
        let mut variant = access::load(ctx, &self.character, &self.variant)?;
        let path = access::parse_path(&self.path)?;
        if variant.overrides.remove(&path).is_none() {
            let overrides = variant.overrides.keys().cloned().collect::<Vec<_>>();
            return Err(not_found(
                "path",
                format!("variant `{}` of `{}` has no override `{path}`", self.variant, self.character),
            )
            .hint(if overrides.is_empty() {
                format!("`{}` has no overrides; variant.list shows each variant's count", self.variant)
            } else {
                format!("its overrides are: {}", list(overrides))
            }));
        }

        let set = access::with_ancestors(ctx, &self.character, &self.variant, variant.clone())?;
        resolve_variant(&def, &set, &self.variant).map_err(|e| access::refusal(&e, "path"))?;

        // An empty table is removed rather than stored as `overrides = {}`, so the file reads as it did before.
        let after = if variant.overrides.is_empty() {
            None
        } else {
            Some(
                serde_json::to_value(&variant.overrides)
                    .map_err(|e| OpError::internal(format!("serialising the overrides failed: {e}")))?,
            )
        };
        let change = character::change(ctx, access::overrides_path(&self.character, &self.variant), after);
        let summary = json!({
            "character": self.character,
            "variant": self.variant,
            "path": path.to_string(),
            "remaining": variant.overrides.len(),
        });
        Ok(character::edit_plan(vec![change], summary))
    }
}

register_op!(VariantClearOverride);
