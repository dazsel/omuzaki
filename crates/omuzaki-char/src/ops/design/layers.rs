//! The material layer stack (#2609, #2610): `part.layer.add`, `.remove`, `.move`, `.set`, `.mask`, `.list` and
//! `part.decal.add`.
//!
//! Every part has a stack of layers over its surface colour (a tint, mottled detail, wear on the edges, wetness, makeup,
//! paint dabs, nail polish, a decal, greying, the folds' cavity, spots), each with an opacity and masks (a body region,
//! a landmark of the face, a height band, the way the surface faces, noise, convex or concave curvature). The stack
//! is evaluated when the character builds, so a recolour, a new colourway, a template change, a dial or a sculpt
//! stroke re-derives it; nothing is repainted by hand.

use omuzaki_char_api::design::{DecalShape, LayerKind, Mask, MaterialLayer};
use omuzaki_char_api::{CharId, LayerId, PartId};
use omuzaki_face_api::ColourRef;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::sculpt::{StrokeAt, part_point};
use super::{load, store};
use crate::ops::character::access;

/// The `invalid` error for a layer that breaks its rules, naming the field.
fn bad_layer(e: &str) -> OpError {
    let field = e.split(':').next().unwrap_or("layer");
    OpError::invalid(format!("layer/{field}"), e.to_owned())
        .hint("opacity, amounts and mask bounds are 0 to 1; sizes and scales are above 0")
}

/// The index of `layer` in `part`'s stack, or `not_found` (field `layer`) naming the layers it has.
fn find(stack: &[MaterialLayer], part: &PartId, layer: &LayerId) -> Result<usize, OpError> {
    stack.iter().position(|l| &l.id == layer).ok_or_else(|| {
        access::not_found("layer", format!("`{part}` has no layer `{layer}`"))
            .hint(format!("the layers of `{part}` are: {}", access::list(stack.iter().map(|l| &l.id))))
    })
}

/// Adds a layer to a part's material stack, on top (or at `index`, counted from the bottom). Its `kind` is one of
/// `tint`, `detail`, `wear`, `wet`, `makeup`, `paint`, `polish`, `decal`, `grey`, `cavity`, `spots`; `masks` say where
/// it shows. Colours are `#rrggbb` or a palette slot (`@skin`), so a colourway change recolours the layer too.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "head", "layer": {"id": "blush", "kind": {"type": "makeup", "zone": "blush", "colour": "#e0607a", "amount": 0.6}}},
        {"character": "kit", "part": "boots", "layer": {"id": "scuffs", "kind": {"type": "wear", "colour": "#d8d0c0", "amount": 0.7}, "masks": [{"mask": {"type": "height", "from": 0.0, "to": 0.4}}]}},
        {"character": "kit", "part": "body", "layer": {"id": "freckles", "kind": {"type": "spots", "colour": "#a06040", "density": 0.5, "amount": 0.5, "seed": 2}, "masks": [{"mask": {"type": "region", "group": "arm_left"}}]}, "index": 0}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PartLayerAdd {
    /// The character.
    pub character: CharId,
    /// The part (any part).
    pub part: PartId,
    /// The layer: its `id` (unique within the part), `kind`, and optionally `opacity`, `masks` and `enabled`.
    pub layer: MaterialLayer,
    /// Where in the stack, counted from the bottom. Left out: on top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

impl Op for PartLayerAdd {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.layer.add",
            summary: "Add a material layer (tint, detail, wear, wet, makeup, paint, polish, decal, grey, cavity, spots) to a part, with masks.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (def, mut design) = load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        self.layer.check().map_err(|e| bad_layer(&e))?;
        let stack = design.layers.entry(self.part.clone()).or_default();
        if stack.iter().any(|l| l.id == self.layer.id) {
            return Err(OpError::invalid("layer/id", format!("`{}` already has a layer `{}`", self.part, self.layer.id))
                .hint("give the layer another id, or change that one with part.layer.set"));
        }
        let index = self.index.unwrap_or(stack.len());
        if index > stack.len() {
            return Err(OpError::invalid("index", format!("the stack of `{}` has {} layer(s), so {index} is past its top", self.part, stack.len()))
                .hint("leave index out to add the layer on top"));
        }
        stack.insert(index, self.layer.clone());
        let count = stack.len();
        let changes = store(ctx, &self.character, &design)?;
        let summary = json!({ "character": self.character, "part": self.part, "layer": self.layer.id, "index": index, "layers": count });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PartLayerAdd);

/// Removes a layer from a part's stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "head", "layer": "blush"}
    ],
    "x-omuzaki-fixture" = "character-design"
))]
pub struct PartLayerRemove {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The layer's id.
    pub layer: LayerId,
}

impl Op for PartLayerRemove {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.layer.remove",
            summary: "Remove a material layer from a part's stack.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (def, mut design) = load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        let stack = design.layers.entry(self.part.clone()).or_default();
        let index = find(stack, &self.part, &self.layer)?;
        stack.remove(index);
        if stack.is_empty() {
            design.layers.remove(&self.part);
        }
        let changes = store(ctx, &self.character, &design)?;
        Ok(access::edit_plan(changes, json!({ "character": self.character, "part": self.part, "removed": self.layer })))
    }
}

register_op!(PartLayerRemove);

/// Moves a layer to `index` in its part's stack (counted from the bottom: `0` is under every other layer).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "head", "layer": "blush", "index": 1}
    ],
    "x-omuzaki-fixture" = "character-design"
))]
pub struct PartLayerMove {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The layer's id.
    pub layer: LayerId,
    /// Its new place, counted from the bottom.
    pub index: usize,
}

impl Op for PartLayerMove {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.layer.move",
            summary: "Reorder a part's material stack: move a layer to a new place (0 is the bottom).",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (def, mut design) = load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        let stack = design.layers.entry(self.part.clone()).or_default();
        let from = find(stack, &self.part, &self.layer)?;
        if self.index >= stack.len() {
            return Err(OpError::invalid("index", format!("the stack of `{}` has {} layer(s), numbered from 0", self.part, stack.len()))
                .hint(format!("give an index from 0 to {}", stack.len() - 1)));
        }
        let layer = stack.remove(from);
        stack.insert(self.index, layer);
        let order: Vec<String> = stack.iter().map(|l| l.id.to_string()).collect();
        let changes = store(ctx, &self.character, &design)?;
        Ok(access::edit_plan(changes, json!({ "character": self.character, "part": self.part, "order": order })))
    }
}

register_op!(PartLayerMove);

/// Changes a layer: its opacity, whether it shows, or its whole `kind` (same id, same place, same masks).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "head", "layer": "blush", "opacity": 0.5},
        {"character": "kit", "part": "head", "layer": "blush", "enabled": false},
        {"character": "kit", "part": "head", "layer": "blush", "kind": {"type": "makeup", "zone": "blush", "colour": "#f08070", "amount": 0.8}}
    ],
    "x-omuzaki-fixture" = "character-design"
))]
pub struct PartLayerSet {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The layer's id.
    pub layer: LayerId,
    /// How much of it shows, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    /// Whether it shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// A new kind and its parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<LayerKind>,
}

impl Op for PartLayerSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.layer.set",
            summary: "Change a material layer's opacity, visibility or kind and parameters.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (def, mut design) = load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        if self.opacity.is_none() && self.enabled.is_none() && self.kind.is_none() {
            return Err(OpError::invalid("opacity", "nothing to change").hint("give opacity, enabled or kind"));
        }
        let stack = design.layers.entry(self.part.clone()).or_default();
        let index = find(stack, &self.part, &self.layer)?;
        let layer = &mut stack[index];
        if let Some(o) = self.opacity {
            layer.opacity = o;
        }
        if let Some(e) = self.enabled {
            layer.enabled = e;
        }
        if let Some(k) = &self.kind {
            layer.kind = k.clone();
        }
        layer.check().map_err(|e| bad_layer(&e))?;
        let layer = layer.clone();
        let changes = store(ctx, &self.character, &design)?;
        Ok(access::edit_plan(changes, json!({ "character": self.character, "part": self.part, "layer": layer })))
    }
}

register_op!(PartLayerSet);

/// Sets where a layer shows: its masks, multiplied together (none: everywhere). A mask is a body `region` (with a
/// `from`/`to` band along it), a face `landmark` with a `reach`, a `height` band, a `facing` direction, `noise`, or
/// `convex`/`concave` curvature; `invert` flips it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "head", "layer": "blush", "masks": [{"mask": {"type": "noise", "scale": 40.0, "threshold": 0.4, "softness": 0.3, "seed": 1}}]},
        {"character": "kit", "part": "head", "layer": "blush", "masks": []}
    ],
    "x-omuzaki-fixture" = "character-design"
))]
pub struct PartLayerMask {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The layer's id.
    pub layer: LayerId,
    /// The masks, replacing the layer's.
    pub masks: Vec<Mask>,
}

impl Op for PartLayerMask {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.layer.mask",
            summary: "Set where a material layer shows: body regions, face landmarks, height, facing, noise or curvature masks.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (def, mut design) = load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        let stack = design.layers.entry(self.part.clone()).or_default();
        let index = find(stack, &self.part, &self.layer)?;
        stack[index].masks.clone_from(&self.masks);
        stack[index].check().map_err(|e| bad_layer(&e))?;
        let changes = store(ctx, &self.character, &design)?;
        Ok(access::edit_plan(changes, json!({ "character": self.character, "part": self.part, "layer": self.layer, "masks": self.masks.len() })))
    }
}

register_op!(PartLayerMask);

/// Lists the material stacks of a character's parts, bottom first. Writes nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "part": "head"}
    ],
    "x-omuzaki-fixture" = "character-design"
))]
pub struct PartLayerList {
    /// The character.
    pub character: CharId,
    /// Only this part's stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<PartId>,
}

impl Op for PartLayerList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.layer.list",
            summary: "List the material layer stacks of a character's parts, bottom first.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (def, design) = load(ctx, &self.character)?;
        if let Some(part) = &self.part {
            access::part(&def, &self.character, part, "part")?;
        }
        let parts: serde_json::Map<String, serde_json::Value> = design
            .layers
            .iter()
            .filter(|(p, _)| self.part.as_ref().is_none_or(|q| q == *p))
            .map(|(p, s)| (p.to_string(), json!(s)))
            .collect();
        Ok(Plan::answer(json!({ "character": self.character, "age": design.age, "parts": parts })))
    }
}

register_op!(PartLayerList);

/// Projects a decal onto a part (a tattoo, a scar, a birthmark, a beauty mark, a star under the eye): a shape at a
/// point, `size` metres across, rotated about the surface's normal. The point is stored in the part's coordinates,
/// so the decal stays put through every shape change; `across_parts` lets it carry on over the character's other skin
/// parts (from the neck onto the head). It is a layer of the part's stack (`part.layer.*` changes it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "head", "id": "mark", "shape": "star", "at": {"on": "head", "dir": [-0.45, -0.1, 0.9]}, "size": 0.012, "colour": "#4060c0"},
        {"character": "kit", "part": "body", "id": "scar", "shape": "scar", "at": [0.2, 1.0, 0.03], "size": 0.06, "rotation": 30.0, "colour": "#c08a80"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PartDecalAdd {
    /// The character.
    pub character: CharId,
    /// The part it lands on.
    pub part: PartId,
    /// The layer's id.
    pub id: LayerId,
    /// The shape: `disc`, `ring`, `heart`, `star`, `diamond`, `cross`, `bar` or `scar`.
    pub shape: DecalShape,
    /// Where its centre is: a bind-space point in metres or a part point.
    pub at: StrokeAt,
    /// Metres across.
    pub size: f32,
    /// Degrees about the surface's normal.
    #[serde(default)]
    pub rotation: f32,
    /// Its colour (`#rrggbb` or a palette slot).
    pub colour: ColourRef,
    /// How much shows, 0 to 1.
    #[serde(default = "one")]
    pub opacity: f32,
    /// Carry on over the character's other skin parts.
    #[serde(default)]
    pub across_parts: bool,
}

fn one() -> f32 {
    1.0
}

impl Op for PartDecalAdd {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.decal.add",
            summary: "Project a decal (tattoo, scar, birthmark, star...) onto a part at a point; it follows every shape change.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (def, mut design) = load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        let at = part_point(ctx, &self.character, &self.part, &self.at)?;
        let kind = LayerKind::Decal {
            shape: self.shape,
            at,
            size: self.size,
            rotation: self.rotation,
            colour: self.colour.clone(),
            across_parts: self.across_parts,
        };
        let layer = MaterialLayer { opacity: self.opacity, ..MaterialLayer::new(self.id.clone(), kind) };
        layer.check().map_err(|e| bad_layer(&e))?;
        let stack = design.layers.entry(self.part.clone()).or_default();
        if stack.iter().any(|l| l.id == self.id) {
            return Err(OpError::invalid("id", format!("`{}` already has a layer `{}`", self.part, self.id))
                .hint("give the decal another id"));
        }
        stack.push(layer);
        let changes = store(ctx, &self.character, &design)?;
        Ok(access::edit_plan(changes, json!({ "character": self.character, "part": self.part, "decal": self.id, "at": at })))
    }
}

register_op!(PartDecalAdd);
