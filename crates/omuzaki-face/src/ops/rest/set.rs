//! `face.set_rest`: writes one field of the resting face (`face/mapping/rest/eye_open` and the like).

use omuzaki_char_api::CharId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ops::mapping::plan_edit;

/// Sets one field of a character's resting face, in paint terms: a sleepy half-lid, a slightly downturned mouth, a usual
/// blush. The fields are `eye_open`, `lower_lid`, `eye_tilt`, `pupil`, `brow_height`, `brow_tilt`, `brow_arch`,
/// `mouth_smile`, `mouth_wide` and `blush`, and each is a base that the channels move (see `face.describe_mapping`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetRest {
    /// The character whose resting face changes.
    pub character: CharId,
    /// The field of the resting face, for example `eye_open`.
    pub field: String,
    /// The new value, a number inside the range the refusal names.
    pub value: Value,
}

impl Op for SetRest {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.set_rest",
            summary: "Set one field of a character's resting face (eye_open, mouth_smile, blush and the like), validate \
                      the whole face, and return the changed paths and the resting paint before and after",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let keys = ["mapping", "rest", self.field.as_str()].map(str::to_owned);
        plan_edit(ctx, &self.character, &keys, self.value.clone())
    }
}

register_op!(SetRest);
