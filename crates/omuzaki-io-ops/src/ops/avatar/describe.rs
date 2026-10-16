//! `avatar.describe`: the current report of an imported character, against its stored original and its avatar node,
//! with the channels the model shows (design D6). The same preparation the import ran, so an unedited avatar describes
//! exactly as it imported.

use omuzaki_char_api::CharId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{read_def, rebuild};

/// Reports on an imported character as it is now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "character": "avatar" }], "x-omuzaki-fixture" = "avatar"))]
pub struct Describe {
    /// The imported character.
    pub character: CharId,
}

impl Op for Describe {
    fn meta() -> OpMeta {
        OpMeta {
            name: "avatar.describe",
            summary: "Report on an imported character as it is now: every finding, and the channels its model shows.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = read_def(ctx.doc, &self.character)?;
        let prepared = rebuild(ctx, def)?;
        let output = json!({
            "character": self.character,
            "report": prepared.report,
            "driven": prepared.avatar.driven(),
            "summary": prepared.summary,
        });
        Ok(Plan::answer(output))
    }
}

register_op!(Describe);
