//! The hair's shared volume and flow (#2478): one parting, one crown whorl and one fullness that every generator reads.
//!
//! | Op | Does |
//! |---|---|
//! | `hair.flow.set` | the parting (or none) and the crown whorl the scalp shell, the part sweeps and the fringe follow |
//! | `hair.volume.set` | how far the hair stands off the head, overall, at the crown and at the back; or no shared volume |
//!
//! Both write the part's `volume` back as one change (`part::changes`), so undo restores it exactly. A hair part
//! without one gets the default volume (no parting, the whorl a little behind the top, fullness 1) on its first edit.

use omuzaki_char_api::{CharId, HairVolume, PartId};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::part;
use crate::ops::character::access as character;

/// Sets the hair's flow: `parting`, an object of `side_deg` (`-60` to `60`, how far the parting sits off the middle,
/// positive toward the character's left; `0` a centre part), `front_deg` (`0` to `90`, where it starts forward of the
/// top of the head) and `back_deg` (`0` to `90`, where it ends behind it), or `"none"` for no parting; and `whorl`, an
/// object of `polar_deg` (`0` to `70`, how far from the top) and `azimuth_deg` (`-180` to `180`, which way: `180`
/// straight back). Give either; a parting keeps the fields it isn't given. The scalp shell combs its strands away from
/// the parting and the whorl, part sweeps start on the parting, and the fringe sweeps away from it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "parting": {"side_deg": 20}},
        {"character": "kit", "part": "hair", "parting": "none", "whorl": {"polar_deg": 35, "azimuth_deg": 160}}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairFlowSet {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The parting's fields to change, or `"none"`.
    #[serde(default)]
    pub parting: Option<Value>,
    /// The whorl's fields to change.
    #[serde(default)]
    pub whorl: Option<Value>,
}

impl Op for HairFlowSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.flow.set",
            summary: "Set the hair's parting (side, front and back, or none) and crown whorl, which the scalp shell, \
                      the part sweeps and the fringe all follow",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.parting.is_none() && self.whorl.is_none() {
            return Err(OpError::invalid("parting", "give a parting, a whorl or both")
                .hint("for example {\"parting\": {\"side_deg\": 20}}"));
        }
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let before = after.volume.clone();
        let volume = after.volume.get_or_insert_with(HairVolume::default);
        match &self.parting {
            Some(Value::String(none)) if none == "none" => volume.part = None,
            Some(given) => volume.part = Some(merged(&volume.part.unwrap_or_default(), given, "parting")?),
            None => {}
        }
        if let Some(given) = &self.whorl {
            volume.whorl = merged(&volume.whorl, given, "whorl")?;
        }
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "before": before,
            "after": after.volume,
        });
        Ok(character::edit_plan(part::changes(ctx, &found, &after, &["volume"], "parting")?, summary))
    }
}

register_op!(HairFlowSet);

/// Sets how far the hair stands off the head: `fullness` (`0.5` to `2.5` times what each generator makes), `crown` (the
/// fullness at the top against the sides) and `back` (at the back), all `0.5` to `2.5`; or `clear` to drop the shared
/// volume and flow, leaving each generator to its own. Give any of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "fullness": 1.3, "crown": 1.4},
        {"character": "kit", "part": "hair", "back": 1.2}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairVolumeSet {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// How far the hair stands off the head.
    #[serde(default)]
    pub fullness: Option<f64>,
    /// The fullness at the crown against the sides.
    #[serde(default)]
    pub crown: Option<f64>,
    /// The fullness at the back against the sides.
    #[serde(default)]
    pub back: Option<f64>,
    /// Drop the shared volume and flow.
    #[serde(default)]
    pub clear: bool,
}

impl Op for HairVolumeSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.volume.set",
            summary: "Set how far the hair stands off the head (fullness, at the crown, at the back), or clear the \
                      shared volume and flow",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let given = [self.fullness, self.crown, self.back].iter().any(Option::is_some);
        if given == self.clear {
            return Err(OpError::invalid(
                "fullness",
                if self.clear { "clear takes nothing else" } else { "give fullness, crown, back or clear" },
            )
            .hint("for example {\"fullness\": 1.3}"));
        }
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let before = after.volume.clone();
        if self.clear {
            after.volume = None;
        } else {
            let volume = after.volume.get_or_insert_with(HairVolume::default);
            volume.fullness = self.fullness.unwrap_or(volume.fullness);
            volume.crown = self.crown.unwrap_or(volume.crown);
            volume.back = self.back.unwrap_or(volume.back);
        }
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "before": before,
            "after": after.volume,
        });
        Ok(character::edit_plan(part::changes(ctx, &found, &after, &["volume"], "fullness")?, summary))
    }
}

register_op!(HairVolumeSet);

/// `current` with the fields of `given` (an object of some of its fields) set; refused by name under `field`.
fn merged<T: Clone + Serialize + serde::de::DeserializeOwned>(
    current: &T,
    given: &Value,
    field: &str,
) -> Result<T, OpError> {
    let Some(fields) = given.as_object() else {
        return Err(OpError::invalid(field, format!("{field} is an object of its fields")));
    };
    fields.iter().try_fold(current.clone(), |value, (name, to)| {
        part::with_field(&value, name, to, &[]).map_err(|mut e| {
            e.field = Some(format!("{field}/{name}"));
            e
        })
    })
}
