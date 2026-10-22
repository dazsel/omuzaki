//! `face.expression.set`: creates a named expression, or replaces it, and returns the patch that writes it.

use omuzaki_char_api::CharId;
use omuzaki_face_api::{ChannelValues, Expression, ExpressionId, ExpressionKind, IntensityCurve, Playback, Validate};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::rules::{check_curve, check_deltas, check_kind};
use super::store::{edited_face, entry, library, patch_to, read_entry, replace_entry};
use crate::ops_shared::edit::{changed, stored_face, to_json};

/// Creates a named expression in a character's face, or replaces it when the name is taken. An expression is a set of
/// channel deltas (how far each channel moves at full weight), a kind and an intensity curve that shapes how the weight
/// applies. Expressions add when a driver blends them, so none has priority over another.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetExpression {
    /// The character whose expression changes.
    pub character: CharId,
    /// The expression's name, for example `happy` or `aa`. The visemes and `blink` keep their kinds.
    pub id: ExpressionId,
    /// What the expression is for: `emotion`, `viseme`, `blink` or `accent`. Only the five visemes are visemes, and only
    /// `blink` is blink.
    pub kind: ExpressionKind,
    /// How far each channel moves at full weight, by channel name. Only the channels listed move; a delta can be negative.
    pub deltas: ChannelValues,
    /// How the weight maps to how much of the deltas applies. Linear when omitted.
    #[serde(default = "linear")]
    pub curve: IntensityCurve,
    /// How it plays when a trigger shows it (fades, auto-stop, group, layer). Left out, a replaced expression keeps its
    /// playback and a new one gets the default; `face.expression.set_playback` changes single fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback: Option<Playback>,
}

/// The default curve: the weight applies in proportion.
fn linear() -> IntensityCurve {
    IntensityCurve::Linear
}

impl Op for SetExpression {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.expression.set",
            summary: "Create or replace a named expression (channel deltas, kind and intensity curve) in a character's face",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_kind(&self.id, self.kind, "kind")?;
        check_deltas(&self.deltas)?;
        check_curve(self.curve)?;
        let face = stored_face(ctx, &self.character)?;
        let before = entry(library(face.as_ref())?, &self.id).cloned();
        let playback = match (&self.playback, &before) {
            (Some(given), _) => given.clone(),
            (None, Some(stored)) => read_entry(stored, &self.id).map(|e| e.playback).unwrap_or_default(),
            (None, None) => Playback::default(),
        };
        if let Err(errors) = playback.validate() {
            let first = errors.0.first().map(|e| (format!("playback/{}", e.path), e.message.clone())).unwrap_or_default();
            return Err(OpError::invalid(first.0, first.1).hint("see face.expression.set_playback for each field's range"));
        }
        let expression = Expression { kind: self.kind, deltas: self.deltas.clone(), curve: self.curve, playback };
        let after = to_json(&expression)?;
        let patch = if let Some(stored) = &before {
            replace_entry(&self.character, &self.id, stored.clone(), after)
        } else {
            let id = self.id.as_str().to_owned();
            let working = edited_face(face.as_ref(), |entries| {
                entries.insert(id, after);
            })?;
            patch_to(&self.character, face.as_ref(), &working)
        };
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({
            "id": self.id.as_str(),
            "created": before.is_none(),
            "changed": changed(&plan.patch),
        }));
        Ok(plan)
    }
}

register_op!(SetExpression);
