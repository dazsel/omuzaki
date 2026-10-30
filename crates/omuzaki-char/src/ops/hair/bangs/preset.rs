//! `hair.bangs.preset`: restyles a bangs group with one of the presets, replacing its whole fringe.

use omuzaki_char_api::{CharId, GroupId, PartId};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;
use super::list_presets::NAME as LIST_PRESETS;
use super::presets::{PRESETS, find};
use crate::ops::character::access as character;
use crate::ops::character::access::not_found;

/// Restyles a bangs group of a hair part with a preset, replacing every layer, wisp and the sweep. The group stays
/// enabled, mirrored and dynamic as it was; undo restores the fringe it had.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "b-bangs", "preset": "side-swept"},
        {"character": "kit", "part": "hair", "group": "b-bangs", "preset": "blunt"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairBangsPreset {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The bangs group of the part.
    pub group: GroupId,
    /// The preset: `blunt`, `side-swept`, `curtain`, `wispy`, `anime` or `none`. `hair.bangs.list_presets` shows what each
    /// looks like.
    pub preset: String,
}

impl Op for HairBangsPreset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.bangs.preset",
            summary: "Restyle a bangs group with a preset (blunt, side-swept, curtain, wispy, anime, none), replacing its \
                      whole fringe.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = access::load(ctx, &self.character, &self.part, &self.group)?;
        let Some(preset) = find(&self.preset) else {
            let ids = PRESETS.iter().map(|p| p.id);
            return Err(not_found(
                "preset",
                format!("there is no bangs preset `{}`: {}the presets are {}", self.preset, did_you_mean(&self.preset), ids.clone().collect::<Vec<_>>().join(", ")),
            )
            .hint(format!("{LIST_PRESETS} shows each preset")));
        };
        let after = (preset.make)();
        let changes = access::changes(ctx, &found, &after)?;
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "group": self.group,
            "preset": preset.id,
            "changed": !changes.is_empty(),
        });
        Ok(character::edit_plan(changes, summary))
    }
}

/// The "did you mean" clause for an unknown preset id.
fn did_you_mean(wanted: &str) -> String {
    access::did_you_mean(wanted, PRESETS.iter().map(|p| p.id))
}

register_op!(HairBangsPreset);
