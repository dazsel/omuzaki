//! `rig.weights.clear`: remove a part's weight brushes.

use omuzaki_char_api::{CharId, PartId};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{brushes_of, store};
use crate::ops::character::access;

/// Removes weight brushes from a part: the brush at `index`, or every brush the part has when `index` is left out.
///
/// Indices count from 0 in the order the brushes were added, as `rig.weights.report` lists them. Removing the last brush
/// of a part removes its list. Undo puts the brushes back in their order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeightsClear {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part whose brushes are removed. Must exist.
    pub part: PartId,
    /// The brush to remove, counted from 0. Left out: every brush of the part.
    #[serde(default)]
    pub index: Option<usize>,
}

impl Op for WeightsClear {
    fn meta() -> OpMeta {
        OpMeta {
            name: "rig.weights.clear",
            summary: "Remove one weight brush from a part, or all of its brushes.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        let mut brushes = brushes_of(&def, &self.part);
        let held = brushes.len();
        match self.index {
            Some(index) if index < held => {
                brushes.remove(index);
            }
            Some(index) => {
                let hint = if held == 0 {
                    format!("`{}` has no weight brushes, so there is nothing to clear", self.part)
                } else {
                    format!(
                        "the brushes of `{}` are numbered 0 to {}; rig.weights.report lists them",
                        self.part,
                        held - 1
                    )
                };
                return Err(OpError::invalid(
                    "index",
                    format!("`{}` has {held} weight brush(es), so there is no brush {index}", self.part),
                )
                .hint(hint));
            }
            None => brushes.clear(),
        }
        let changes = store(ctx, &self.character, &self.part, &brushes)?;
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "removed": held - brushes.len(),
            "brushes": brushes.len(),
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(WeightsClear);
