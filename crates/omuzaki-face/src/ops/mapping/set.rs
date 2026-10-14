//! `face.set_mapping`: writes one exaggeration coefficient (`face/mapping/eyes/wide` and the like).

use omuzaki_char_api::CharId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::edit::{mapping_keys, plan_edit};

/// Sets one coefficient of a character's face mapping: how a channel moves a parameter (`face/mapping/mouth/smile`). An
/// object value sets the coefficients it names. The resting face (`face/mapping/rest/...`) is set with `face.set_rest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetMapping {
    /// The character whose mapping changes.
    pub character: CharId,
    /// Where in the mapping, for example `face/mapping/eyes/wide`. The leading `face/mapping/` may be left out. See
    /// `face.describe_mapping` for every path and its formula.
    pub path: String,
    /// The new coefficient, a number inside the range the refusal names.
    pub value: Value,
}

impl Op for SetMapping {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.set_mapping",
            summary: "Set one exaggeration coefficient of a face's mapping (how a channel moves a face part), validate \
                      the whole face, and return the changed paths and the resting paint before and after",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let keys = mapping_keys(&self.path)?;
        if keys.get(1).map(String::as_str) == Some("rest") {
            return Err(OpError::invalid(
                format!("face/{}", keys.join("/")),
                "the resting face is set with `face.set_rest`, not `face.set_mapping`",
            )
            .hint("use `face.set_rest` with the field name, for example { \"field\": \"eye_open\" }"));
        }
        plan_edit(ctx, &self.character, &keys, self.value.clone())
    }
}

register_op!(SetMapping);
