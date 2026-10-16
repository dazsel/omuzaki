//! `avatar.map_channel`: sets or clears the person's choice of expression for one channel of an imported character
//! (design D3, D6).
//!
//! The choice is stored in `characters/<id>/avatar/channels` and wins over the driver table. The channel and the
//! expression are checked against the rebuilt avatar before anything is written: an unknown channel, an expression the
//! model does not have (the error names the nearest), or a gaze channel on a model whose gaze runs through its look-at
//! is refused and changes nothing. One undo reverts it.

use omuzaki_anim_api::Channel;
use omuzaki_avatar_api::def::DriverChoice;
use omuzaki_char_api::CharId;
use omuzaki_core::text::nearest;
use omuzaki_io_api::ExpressionName;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{def_patch, read_def, rebuild, require_clean};

/// Sets or clears which expression one channel drives on an imported character.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "character": "avatar", "channel": "custom03", "expression": "happy" }],
    "x-omuzaki-fixture" = "avatar"
))]
pub struct MapChannel {
    /// The imported character.
    pub character: CharId,
    /// The channel, by name (`cheekPuff`, `custom03`).
    pub channel: String,
    /// The expression it drives, by its name in the model (`Puff`, `happy`); `null` clears the choice, so the driver
    /// table decides again.
    #[serde(default)]
    pub expression: Option<String>,
}

impl Op for MapChannel {
    fn meta() -> OpMeta {
        OpMeta {
            name: "avatar.map_channel",
            summary: "Set or clear which expression of an imported character's model a channel drives.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut def = read_def(ctx.doc, &self.character)?;
        let channel = Channel::from_name(&self.channel).ok_or_else(|| {
            let names = (0..omuzaki_anim_api::CHANNEL_CAPACITY).filter_map(Channel::from_index).map(Channel::name);
            let error = OpError::invalid("channel", format!("{:?} is not a channel", self.channel));
            match nearest(&self.channel, names) {
                Some(near) => error.hint(format!("did you mean {near}?")),
                None => error.hint("use a channel name such as cheekPuff or custom03"),
            }
        })?;
        match &self.expression {
            Some(expression) => def.channels.insert(channel, DriverChoice::Expression(expression.clone())),
            None => def.channels.remove(&channel),
        };
        let prepared = rebuild(ctx, def.clone())?;
        require_clean(&prepared.report, "expression")?;
        let location = format!("avatar/channels/{channel}");
        let refused = prepared
            .report
            .findings
            .iter()
            .find(|f| f.rule == "avatar.channel_override_invalid" && f.location.as_deref() == Some(location.as_str()));
        if let Some(refused) = refused {
            // Either the model has no such expression (name the nearest one it has), or it has it but gaze runs
            // through its look-at (the finding's own hint says so).
            let wanted = self.expression.as_deref().unwrap_or_default();
            let exists = ExpressionName::parse(wanted).ok().and_then(|n| prepared.avatar.expression(&n)).is_some();
            let names: Vec<String> = prepared.avatar.expressions.iter().map(|e| e.name.to_string()).collect();
            let hint = if exists {
                refused.hint.clone()
            } else {
                nearest(wanted, names.iter().map(String::as_str)).map(|near| format!("did you mean {near}?"))
            };
            let error = OpError::invalid("expression", refused.message.clone());
            return Err(match hint {
                Some(hint) => error.hint(hint),
                None => error,
            });
        }
        let output = json!({ "character": self.character, "channel": channel, "driven": prepared.avatar.driven() });
        Ok(Plan { output: Some(output), ..Plan::edit(def_patch(ctx.doc, &self.character, &def)?) })
    }
}

register_op!(MapChannel);
