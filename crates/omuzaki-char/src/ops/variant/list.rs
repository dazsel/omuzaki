//! `variant.list`: the variants of a character, in name order, with what each covers.

use omuzaki_char_api::{CharId, ShotRange, VariantDef};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;
use crate::ops::character::access as character;

/// Lists a character's variants. Writes nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"}
    ],
    "x-omuzaki-fixture" = "character-variant"
))]
pub struct VariantList {
    /// The character whose variants to list.
    pub character: CharId,
}

/// One variant, as the list shows it.
#[derive(Serialize)]
struct Listed {
    variant: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    extends: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shot: Option<ShotRange>,
    overrides: usize,
}

impl Listed {
    fn of(name: &str, variant: VariantDef) -> Self {
        Self {
            variant: name.to_owned(),
            description: variant.description,
            extends: variant.extends.map(|parent| parent.to_string()),
            shot: variant.shot,
            overrides: variant.overrides.len(),
        }
    }
}

impl Op for VariantList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "variant.list",
            summary: "List a character's variants with what each extends, the shots it covers and how many overrides it has.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        character::load(ctx, &self.character)?;
        let mut listed = Vec::new();
        for (name, node) in access::table(ctx, &self.character).into_iter().flatten() {
            let variant = serde_json::from_value::<VariantDef>(node.clone()).map_err(|e| {
                OpError::invalid(
                    "variant",
                    format!(
                        "variant `{name}` of `{}` is stored in a shape the variant format rejects: {e}",
                        self.character
                    ),
                )
                .hint("repair the variant file (variants/<name>.toml), then retry")
            })?;
            listed.push(Listed::of(name, variant));
        }
        listed.sort_by(|a, b| a.variant.cmp(&b.variant));
        let output = json!({ "character": self.character, "count": listed.len(), "variants": listed });
        Ok(Plan::answer(output))
    }
}

register_op!(VariantList);
