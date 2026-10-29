//! `face.expression.set_playback`: how a named expression plays when a trigger shows it (FE-03): its fades, auto-stop,
//! exclusive group, layer, dwell and cut-in.

use omuzaki_char_api::CharId;
use omuzaki_face_api::{ExpressionId, PlaybackLayer, Validate};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::store::{entry, library, missing, read_entry, replace_entry};
use crate::ops_shared::edit::{changed, stored_face, to_json};

/// Sets how a named expression plays when a hotkey or another trigger shows it: how fast it fades in and out, whether it
/// stops on its own, which exclusive group it is in (showing one hides the others in the group, one mood at a time), and
/// whether it is held or momentary (a momentary one plays over the held ones and gives the face back when it ends), how
/// long it holds at least before another in its group replaces it, and whether it cuts in through another's hold. The
/// fields left out keep their values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "character": "kit", "id": "happy", "fade_in_s": 0.3, "group": "mood", "dwell_s": 1.1 }, { "character": "kit", "id": "surprised", "cuts_in": true }, { "character": "kit", "id": "wink", "layer": "momentary", "auto_stop_s": 0.6 }],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetExpressionPlayback {
    /// The character whose expression changes.
    pub character: CharId,
    /// The expression.
    pub id: ExpressionId,
    /// Seconds to fade in, `0..=10`; 0 shows it at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in_s: Option<f64>,
    /// Seconds to fade out, `0..=10`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out_s: Option<f64>,
    /// Stop on its own this many seconds after it is shown, `0.05..=600`; 0 turns auto-stop off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_stop_s: Option<f64>,
    /// Its exclusive group (lowercase letters, digits, `_`, `-`), for example `mood`; an empty string takes it out of
    /// its group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// `held` (stays until hidden or replaced) or `momentary` (plays over the held ones).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<PlaybackLayer>,
    /// The least seconds it holds before another in its group may replace it, `0..=10`; a face asked for sooner waits.
    /// 0 turns it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dwell_s: Option<f64>,
    /// Replace another in its group at once, whatever that one's dwell (a startle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuts_in: Option<bool>,
}

impl Op for SetExpressionPlayback {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.expression.set_playback",
            summary: "Set how a named expression plays when a trigger shows it: fade in and out, auto-stop, exclusive \
                      group, held or momentary, dwell and cut-in",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let face = stored_face(ctx, &self.character)?;
        let stored = entry(library(face.as_ref())?, &self.id).ok_or_else(|| missing(&self.id))?.clone();
        let mut expression = read_entry(&stored, &self.id)?;
        let p = &mut expression.playback;
        if let Some(v) = self.fade_in_s {
            p.fade_in_s = v;
        }
        if let Some(v) = self.fade_out_s {
            p.fade_out_s = v;
        }
        if let Some(v) = self.auto_stop_s {
            p.auto_stop_s = (v != 0.0).then_some(v);
        }
        if let Some(group) = &self.group {
            p.group = (!group.is_empty()).then(|| group.clone());
        }
        if let Some(layer) = self.layer {
            p.layer = layer;
        }
        if let Some(v) = self.dwell_s {
            p.dwell_s = (v != 0.0).then_some(v);
        }
        if let Some(v) = self.cuts_in {
            p.cuts_in = v;
        }
        if let Err(errors) = p.validate() {
            let first = errors.0.first().map(|e| (e.path.clone(), e.message.clone())).unwrap_or_default();
            return Err(OpError::invalid(first.0, first.1)
                .hint("fades and dwell are 0 to 10 s, auto-stop 0.05 to 600 s (0 turns it off), a group is a short lowercase name"));
        }
        let playback = to_json(&expression.playback)?;
        let patch = replace_entry(&self.character, &self.id, stored, to_json(&expression)?);
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "id": self.id.as_str(), "playback": playback, "changed": changed(&plan.patch) }));
        Ok(plan)
    }
}

register_op!(SetExpressionPlayback);
