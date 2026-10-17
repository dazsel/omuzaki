//! `hair.bangs.set`: sets one field of a bangs group.

use omuzaki_char_api::{BangLayer, BangsGen, CharId, GroupId, PartId, Wisp};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access::{self, MAX_CLUMPS, MAX_LAYERS};
use crate::ops::character::access as character;

/// Sets one field of a bangs group of a hair part: how many layers it has, each layer's clumps, tip heights and hook,
/// the sweep, or the loose wisps. The whole fringe is checked before it is written, so a value out of range is
/// refused naming the field and its range, and changes nothing.
///
/// `field` is one of `layers` (the number of layers; a new layer copies the top one, and fewer layers drop the top
/// ones), `sweep_deg` (degrees toward the character's right; negative sweeps left), `wisps` (a list of
/// `{phi_deg, tip_height, width}`), or a layer field: `clumps` (clumps in the layer), `tip_heights` (a list of
/// head-local heights, left to right, relative to the head's centre; a single number is one height) and `hook_deg`
/// (positive hooks the tips inward, negative outward). A layer field alone applies to every layer; `layers/<i>/<field>` (`layers/1/hook_deg`) applies to layer `i`, counting from 0, the bottom layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "b-bangs", "field": "sweep_deg", "value": 12.0}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairBangsSet {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The bangs group of the part.
    pub group: GroupId,
    /// The field to set.
    pub field: String,
    /// The new value, of the type the field takes.
    pub value: Value,
}

impl Op for HairBangsSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.bangs.set",
            summary: "Set one field of a hair part's bangs group: layers, clumps, tip_heights, sweep_deg, hook_deg or \
                      wisps. A refusal names the field and its range.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = access::load(ctx, &self.character, &self.part, &self.group)?;
        let target = Target::parse(&self.field, found.bangs.layers.len())?;
        let mut after = found.bangs.clone();
        target.write(&mut after, &self.value, &self.field)?;
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "group": self.group,
            "field": self.field,
            "before": target.read(&found.bangs),
            "after": target.read(&after),
        });
        Ok(character::edit_plan(access::changes(ctx, &found, &after)?, summary))
    }
}

register_op!(HairBangsSet);

/// A field of a layer.
#[derive(Clone, Copy, Debug, PartialEq)]
enum LayerField {
    Clumps,
    TipHeights,
    Hook,
}

const LAYER_FIELDS: [(&str, LayerField); 3] =
    [("clumps", LayerField::Clumps), ("tip_heights", LayerField::TipHeights), ("hook_deg", LayerField::Hook)];

/// What a `field` names.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Layers,
    Sweep,
    Wisps,
    /// A layer field of one layer, or of every layer.
    Layer(Option<usize>, LayerField),
}

/// The fields that are not a layer's.
const TOP_FIELDS: [(&str, Target); 3] = [("layers", Target::Layers), ("sweep_deg", Target::Sweep), ("wisps", Target::Wisps)];

impl Target {
    /// Reads `field`, for a fringe of `layers` layers.
    fn parse(field: &str, layers: usize) -> Result<Self, OpError> {
        let unknown = || {
            let names = TOP_FIELDS.iter().map(|f| f.0).chain(LAYER_FIELDS.iter().map(|f| f.0));
            let hint = format!(
                "{}the bangs fields are layers, sweep_deg, wisps, and per layer clumps, tip_heights and hook_deg \
                 (layers/<i>/<field> for one layer)",
                access::did_you_mean(field.rsplit('/').next().unwrap_or(field), names)
            );
            OpError::invalid("field", format!("`{field}` is not a bangs field")).hint(hint)
        };
        if let Some((_, target)) = TOP_FIELDS.iter().find(|(name, _)| *name == field) {
            return Ok(*target);
        }
        if let Some((_, kind)) = LAYER_FIELDS.iter().find(|(name, _)| *name == field) {
            return Ok(Self::Layer(None, *kind));
        }
        let mut steps = field.split('/');
        let (Some("layers"), Some(index), Some(name), None) = (steps.next(), steps.next(), steps.next(), steps.next())
        else {
            return Err(unknown());
        };
        let Some((_, kind)) = LAYER_FIELDS.iter().find(|(n, _)| *n == name) else { return Err(unknown()) };
        let Ok(index) = index.parse::<usize>() else { return Err(unknown()) };
        if index >= layers {
            return Err(OpError::invalid("field", format!("`{field}`: the group has {layers} layers, numbered from 0"))
                .hint("set `layers` to add one first"));
        }
        Ok(Self::Layer(Some(index), *kind))
    }

    /// Writes `value` into `bangs`. Range checks are the fringe's, made by [`access::changes`].
    fn write(self, bangs: &mut BangsGen, value: &Value, field: &str) -> Result<(), OpError> {
        match self {
            Self::Layers => resize(bangs, whole(value, field, 1, MAX_LAYERS)?),
            Self::Sweep => bangs.sweep_deg = number(value, field)?,
            Self::Wisps => {
                bangs.wisps = serde_json::from_value::<Vec<Wisp>>(value.clone()).map_err(|e| {
                    OpError::invalid("value", format!("`{field}` takes a list of {{phi_deg, tip_height, width}}: {e}"))
                })?;
            }
            Self::Layer(index, kind) => {
                let layers = bangs.layers.iter_mut().enumerate().filter(|(i, _)| index.is_none_or(|only| only == *i));
                for (_, layer) in layers {
                    kind.write(layer, value, field)?;
                }
            }
        }
        Ok(())
    }

    /// The current value of the field: of the bottom layer named, or a list of the layers' for every layer.
    fn read(self, bangs: &BangsGen) -> Value {
        match self {
            Self::Layers => json!(bangs.layers.len()),
            Self::Sweep => json!(bangs.sweep_deg),
            Self::Wisps => json!(bangs.wisps),
            Self::Layer(Some(i), kind) => bangs.layers.get(i).map_or(Value::Null, |l| kind.read(l)),
            Self::Layer(None, kind) => Value::Array(bangs.layers.iter().map(|l| kind.read(l)).collect()),
        }
    }
}

impl LayerField {
    fn write(self, layer: &mut BangLayer, value: &Value, field: &str) -> Result<(), OpError> {
        match self {
            Self::Clumps => {
                let count = whole(value, field, 1, usize::from(MAX_CLUMPS))?;
                layer.count = u8::try_from(count).map_err(|_| OpError::internal("a checked count left u8"))?;
            }
            Self::Hook => layer.hook_deg = number(value, field)?,
            Self::TipHeights => {
                layer.tip_heights = match value {
                    Value::Array(items) => items.iter().map(|v| number(v, field)).collect::<Result<_, _>>()?,
                    single => vec![number(single, field)?],
                };
            }
        }
        Ok(())
    }

    fn read(self, layer: &BangLayer) -> Value {
        match self {
            Self::Clumps => json!(layer.count),
            Self::TipHeights => json!(layer.tip_heights),
            Self::Hook => json!(layer.hook_deg),
        }
    }
}

/// Gives the fringe `count` layers: a new one copies the top layer, and fewer drop the top ones.
fn resize(bangs: &mut BangsGen, count: usize) {
    let top = bangs.layers.last().cloned().unwrap_or_default();
    bangs.layers.resize(count, top);
}

/// `value` as a finite number.
fn number(value: &Value, field: &str) -> Result<f64, OpError> {
    value.as_f64().filter(|n| n.is_finite()).ok_or_else(|| {
        OpError::invalid("value", format!("`{field}` takes a number, but {value} was given"))
    })
}

/// `value` as a whole number within `lo..=hi`, the range `access::check` enforces (a number outside it is refused
/// naming the range).
fn whole(value: &Value, field: &str, lo: usize, hi: usize) -> Result<usize, OpError> {
    let n = value.as_u64().ok_or_else(|| {
        OpError::invalid("value", format!("`{field}` takes a whole number, but {value} was given"))
    })?;
    match usize::try_from(n) {
        Ok(n) if (lo..=hi).contains(&n) => Ok(n),
        _ => Err(OpError::invalid("value", format!("`{field}`: {n} is out of range: use {lo} to {hi}"))),
    }
}
