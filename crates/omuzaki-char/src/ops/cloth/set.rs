//! `cloth.set`: a garment's cloth simulation, its fabric preset and one override, in one edit.
//!
//! The edit is `part.set_param` on the garment's `common/cloth`, so the kind checks it and the patch is the smallest
//! that makes the change: the op only gathers what it is given into that object.

use omuzaki_char_api::{CharId, PartId};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};

use crate::ops::character::access;
use crate::ops::part::describe::{Params, kind_of};
use crate::ops::part::set_param::PartSetParam;
use crate::outfit::drape::ClothPreset;

/// The fabric numbers `field` may name.
pub const FIELDS: [&str; 3] = ["density", "stiffness", "damping"];

/// Turns a garment's cloth simulation on or off, picks its fabric and sets one of the fabric's numbers. Only what is
/// given changes; undo restores the garment as it was. A garment that drapes (a skirt, a dress, a robe, a cloak)
/// swings and collides with the body once cloth is on; one that cannot (a vest, boots) keeps its skin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "skirt", "enabled": true, "preset": "light"},
        {"character": "kit", "part": "skirt", "field": "stiffness", "value": 2.0}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct ClothSet {
    /// The character the garment belongs to.
    pub character: CharId,
    /// The garment.
    pub part: PartId,
    /// `true` to simulate it as cloth, `false` to skin it to the skeleton.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// The fabric: `light` (chiffon), `cotton`, `heavy` (wool) or `leather`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<ClothPreset>,
    /// A number of the fabric to change: `density` (kg/m², 0.02 to 2), `stiffness` (bending, as a factor of the
    /// preset's, 0.05 to 20) or `damping` (1/s, 0 to 20). Needs `value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The new value of `field`; `null` puts back the preset's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl Op for ClothSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "cloth.set",
            summary: "Turn a garment's cloth simulation on or off, pick its fabric (light, cotton, heavy, leather) and \
                      set one of its numbers.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let part = access::part(&def, &self.character, &self.part, "part")?;
        let kind = kind_of(&self.part, part)?;
        let params = Params::new(kind, &part.params)?;
        if params.resolve("common/cloth/enabled").is_err() {
            return Err(OpError::invalid(
                "part",
                format!("`{}` is a `{}`, which is not a garment: it has no cloth", self.part, part.kind),
            ));
        }
        let mut cloth = Map::new();
        if let Some(enabled) = self.enabled {
            cloth.insert("enabled".to_owned(), json!(enabled));
        }
        if let Some(preset) = self.preset {
            cloth.insert("preset".to_owned(), json!(preset));
        }
        match (&self.field, &self.value) {
            (Some(field), Some(value)) => {
                if !FIELDS.contains(&field.as_str()) {
                    return Err(OpError::invalid(
                        "field",
                        format!("there is no cloth number `{field}`: the numbers are {}", FIELDS.join(", ")),
                    ));
                }
                cloth.insert("overrides".to_owned(), json!({ field.as_str(): value }));
            }
            (Some(_), None) => return Err(OpError::invalid("value", "`field` needs a `value` (or null)")),
            (None, Some(_)) => return Err(OpError::invalid("field", "`value` needs the `field` it sets")),
            (None, None) => {}
        }
        if cloth.is_empty() {
            return Err(OpError::invalid("enabled", "give `enabled`, `preset` or a `field` and its `value`"));
        }
        PartSetParam {
            character: self.character.clone(),
            part: self.part.clone(),
            path: "common/cloth".to_owned(),
            value: Value::Object(cloth),
            replace: false,
        }
        .plan(ctx)
    }
}

register_op!(ClothSet);
