//! `part.set_enabled`: switch a part on or off without deleting it (hair A / B / C side by side, CB-91).

use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ops::character::access;
use crate::ops::character::tree;

/// Enables or disables a part without deleting it.
///
/// A disabled part is not built, and neither are its children, so you can keep several versions of a part (hair A, B,
/// C) and switch between them: enable one and disable the others. Refused when disabling would leave other parts
/// without a part of the kind they need (the body under a shirt), and the error names them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSetEnabled {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part to switch.
    pub part: PartId,
    /// `true` to build the part, `false` to skip it (and its children).
    pub enabled: bool,
}

impl Op for PartSetEnabled {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.set_enabled",
            summary: "Enable or disable a part without deleting it, to keep versions (hair A / B / C) side by side.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let current = access::part(&def, &self.character, &self.part, "part")?.enabled;
        let differs = current != self.enabled;

        let mut after = def.clone();
        if let Some(part) = after.parts.get_mut(&self.part) {
            part.enabled = self.enabled;
        }
        let verb = if self.enabled { "enabling" } else { "disabling" };
        let order = tree::check_edit(&def, &after, &format!("{verb} `{}`", self.part), |_| "enabled")?;

        let changes = if differs {
            let path = paths::part(&self.character, &self.part).join("enabled");
            vec![access::change(ctx, path, Some(json!(self.enabled)))]
        } else {
            Vec::new()
        };
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "enabled": self.enabled,
            "changed": differs,
            "build_order": order,
            "note": tree::idle_reason(&after, &self.part),
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PartSetEnabled);
