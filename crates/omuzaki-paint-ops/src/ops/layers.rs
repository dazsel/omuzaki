//! Layers: adding, setting, removing and moving them.

use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_paint::{BlendMode, Layer, LayerKind, Painting};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::edit;
use crate::doc::{from_paint, id, json, load, path_in};

/// Where a layer goes in the stack: the `parent`, `above` and `below` fields of an op.
struct Place<'a> {
    parent: &'a Option<String>,
    above: &'a Option<String>,
    below: &'a Option<String>,
}

/// Puts `lid` (already in `p.layers`, not in `p.order`) where `place` says, setting its parent.
fn place(p: &mut Painting, lid: &str, place: &Place<'_>) -> Result<(), OpError> {
    if place.above.is_some() && place.below.is_some() {
        return Err(OpError::invalid("above", "give above or below, not both"));
    }
    let anchor = place.above.as_ref().or(place.below.as_ref());
    let parent = match (anchor, &place.parent) {
        (Some(a), &given) => {
            let field = if place.above.is_some() { "above" } else { "below" };
            let at = p.layer(a).map_err(|_| OpError::invalid(field, format!("there is no layer {a:?}")))?;
            if given.is_some() && *given != at.parent {
                return Err(OpError::invalid("parent", format!("{a:?} is not in that group")));
            }
            at.parent.clone()
        }
        (None, &given) => given.clone(),
    };
    if let Some(g) = &parent {
        if p.layers.get(g).map(|l| l.kind) != Some(LayerKind::Group) {
            return Err(OpError::invalid("parent", format!("{g:?} is not a group")).hint("add one with kind = group"));
        }
        if g == lid || descends(p, g, lid) {
            return Err(OpError::invalid("parent", "a group can't go inside itself"));
        }
    }
    let index = match (place.above, place.below) {
        (Some(a), _) => p.order.iter().position(|x| x == a).map_or(p.order.len(), |i| i + 1),
        (_, Some(b)) => p.order.iter().position(|x| x == b).unwrap_or(0),
        _ => match &parent {
            // The top of a group: after the last layer inside it (or the group itself when empty).
            Some(g) => p.order.iter().rposition(|x| x == g || descends(p, x, g)).map_or(p.order.len(), |i| i + 1),
            None => p.order.len(),
        },
    };
    if let Some(l) = p.layers.get_mut(lid) {
        l.parent = parent;
    }
    p.order.insert(index, lid.to_owned());
    Ok(())
}

/// Whether `id` is inside group `group`, at any depth.
fn descends(p: &Painting, id: &str, group: &str) -> bool {
    let mut at = p.layers.get(id).and_then(|l| l.parent.clone());
    let mut hops = 0;
    while let Some(cur) = at {
        if cur == group {
            return true;
        }
        hops += 1;
        if hops > p.layers.len() {
            return false;
        }
        at = p.layers.get(&cur).and_then(|l| l.parent.clone());
    }
    false
}

fn layer_kind_default() -> LayerKind {
    LayerKind::Paint
}

/// Add a layer to a painting: a paint layer for marks, or a group to fold layers into. Put shading on its own layer
/// with `clip` so it stays inside the colour under it, and line art on top.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
    { "painting": "bust", "id": "highlights", "name": "Highlights", "blend": "add" },
    { "painting": "bust", "id": "rim-light", "name": "Rim light", "below": "lines", "clip": true, "blend": "screen" },
    { "painting": "bust", "id": "eyes", "name": "Eyes", "kind": "group" }
],
    "x-omuzaki-fixture" = "painting"
))]
pub struct AddLayer {
    /// The painting.
    pub painting: String,
    /// The new layer's id.
    pub id: String,
    /// What it holds. Default: its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `paint` (default) or `group`.
    #[serde(default = "layer_kind_default")]
    pub kind: LayerKind,
    /// Put it in this group (its children stack inside it). Default: where `above` or `below` is, else the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Put it just above this layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<String>,
    /// Put it just below this layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<String>,
    /// How it blends. Default normal (a group without one passes its layers through).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<BlendMode>,
    /// Opacity, 0 to 1. Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// Blur the layer by a Gaussian of this many pixels (0 to 256): soft painted edges, glows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur: Option<f64>,
    /// Clip to the layer under it. Default false.
    #[serde(default)]
    pub clip: bool,
    /// Hidden when false. Default true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// The character part it is, for layered-art export (`face`, `hair_front`, `iris`, `eye_line`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl Op for AddLayer {
    fn meta() -> OpMeta {
        edit(
            "paint.add_layer",
            "Add a paint layer or a group to a painting, with its place, blend, opacity and clipping.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut p = load(ctx.doc, &self.painting)?;
        id("id", &self.id)?;
        if p.layers.contains_key(&self.id) {
            return Err(OpError::conflict(format!("there is already a layer {:?}", self.id)));
        }
        let mut layer = Layer::paint(self.name.clone().unwrap_or_else(|| self.id.clone()));
        layer.kind = self.kind;
        layer.blend = self.blend;
        layer.opacity = self.opacity.unwrap_or(1.0);
        layer.clip = self.clip;
        layer.blur = self.blur.unwrap_or(0.0);
        layer.visible = self.visible.unwrap_or(true);
        layer.role.clone_from(&self.role);
        p.layers.insert(self.id.clone(), layer);
        let before_order = json(&load(ctx.doc, &self.painting)?.order)?;
        place(&mut p, &self.id, &Place { parent: &self.parent, above: &self.above, below: &self.below })?;
        p.check().map_err(from_paint)?;
        let layer = p.layer(&self.id).map_err(from_paint)?;
        let patch = Patch::new().with(path_in(&self.painting, &["layers", &self.id]), None, Some(json(layer)?)).with(
            path_in(&self.painting, &["order"]),
            Some(before_order),
            Some(json(&p.order)?),
        );
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "layer": self.id, "order": p.order }));
        Ok(plan)
    }
}
register_op!(AddLayer);

/// Change a layer's settings: name, visibility, opacity, blend, clipping, role. Only what you give changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
    { "painting": "bust", "layer": "skin-shadow", "opacity": 0.6, "blend": "multiply" },
    { "painting": "bust", "layer": "sketch", "visible": false }
],
    "x-omuzaki-fixture" = "painting"
))]
pub struct SetLayer {
    /// The painting.
    pub painting: String,
    /// The layer.
    pub layer: String,
    /// A new name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Shown or hidden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Opacity, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// Blend mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<BlendMode>,
    /// For a group: pass its layers through to what is under it (clears `blend`).
    #[serde(default)]
    pub pass_through: bool,
    /// Blur the layer by a Gaussian of this many pixels (0 to 256): soft painted edges, glows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur: Option<f64>,
    /// Clip to the layer under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<bool>,
    /// The character part it is; an empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl Op for SetLayer {
    fn meta() -> OpMeta {
        edit("paint.set_layer", "Change a layer's name, visibility, opacity, blend mode, blur, clipping or role.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut p = load(ctx.doc, &self.painting)?;
        let old = p.layer(&self.layer).map_err(from_paint)?.clone();
        if self.pass_through && self.blend.is_some() {
            return Err(OpError::invalid("pass_through", "give blend or pass_through, not both"));
        }
        if self.pass_through && old.kind != LayerKind::Group {
            return Err(OpError::invalid("pass_through", "only a group passes its layers through"));
        }
        let mut new = old.clone();
        if let Some(n) = &self.name {
            new.name.clone_from(n);
        }
        if let Some(v) = self.visible {
            new.visible = v;
        }
        if let Some(o) = self.opacity {
            new.opacity = o;
        }
        if self.blend.is_some() {
            new.blend = self.blend;
        }
        if self.pass_through {
            new.blend = None;
        }
        if let Some(b) = self.blur {
            new.blur = b;
        }
        if let Some(c) = self.clip {
            new.clip = c;
        }
        if let Some(r) = &self.role {
            new.role = if r.is_empty() { None } else { Some(r.clone()) };
        }
        p.layers.insert(self.layer.clone(), new.clone());
        p.check().map_err(from_paint)?;
        let (old_v, new_v) = (json(&old)?, json(&new)?);
        let mut patch = Patch::new();
        for field in ["name", "visible", "opacity", "blend", "blur", "clip", "role"] {
            let (a, b) = (old_v.get(field).cloned(), new_v.get(field).cloned());
            if a != b {
                patch = patch.with(path_in(&self.painting, &["layers", &self.layer, field]), a, b);
            }
        }
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "layer": self.layer, "settings": settings(&new) }));
        Ok(plan)
    }
}
register_op!(SetLayer);

fn settings(l: &Layer) -> Value {
    json!({ "name": l.name, "visible": l.visible, "opacity": l.opacity, "blend": l.blend, "blur": l.blur, "clip": l.clip, "role": l.role })
}

/// Remove a layer with its marks (a group goes with every layer in it). Undoable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "painting": "bust", "layer": "sketch" }],
    "x-omuzaki-fixture" = "painting"
))]
pub struct RemoveLayer {
    /// The painting.
    pub painting: String,
    /// The layer.
    pub layer: String,
}

impl Op for RemoveLayer {
    fn meta() -> OpMeta {
        edit("paint.remove_layer", "Remove a layer and its marks (a group with every layer in it).")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut p = load(ctx.doc, &self.painting)?;
        p.layer(&self.layer).map_err(from_paint)?;
        let gone: Vec<String> =
            p.order.iter().filter(|x| **x == self.layer || descends(&p, x, &self.layer)).cloned().collect();
        let before_order = json(&p.order)?;
        let mut patch = Patch::new();
        for g in &gone {
            let before = ctx.doc.get(&path_in(&self.painting, &["layers", g])).cloned();
            patch = patch.with(path_in(&self.painting, &["layers", g]), before, None);
            p.layers.remove(g);
        }
        p.order.retain(|x| !gone.contains(x));
        p.check().map_err(|e| {
            from_paint(e).hint("another layer's bucket fill samples it: remove or change that fill first")
        })?;
        patch = patch.with(path_in(&self.painting, &["order"]), Some(before_order), Some(json(&p.order)?));
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "removed": gone }));
        Ok(plan)
    }
}
register_op!(RemoveLayer);

/// Move a layer (a group moves with its layers) above or below another, into a group, or out to the top level.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
    { "painting": "bust", "layer": "lines", "above": "hair-shadow" },
    { "painting": "bust", "layer": "fringe", "parent": "hair" }
],
    "x-omuzaki-fixture" = "painting"
))]
pub struct MoveLayer {
    /// The painting.
    pub painting: String,
    /// The layer.
    pub layer: String,
    /// Put it in this group (its children stack inside it). Default: where `above` or `below` is, else the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Put it just above this layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<String>,
    /// Put it just below this layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<String>,
}

impl Op for MoveLayer {
    fn meta() -> OpMeta {
        edit("paint.move_layer", "Move a layer above or below another, into a group or out of one.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut p = load(ctx.doc, &self.painting)?;
        let old = p.layer(&self.layer).map_err(from_paint)?.clone();
        if [&self.above, &self.below].into_iter().flatten().any(|a| *a == self.layer) {
            return Err(OpError::invalid("above", "a layer can't move relative to itself"));
        }
        let before_order = json(&p.order)?;
        // The layer and everything inside it move together, keeping their order.
        let moving: Vec<String> =
            p.order.iter().filter(|x| **x == self.layer || descends(&p, x, &self.layer)).cloned().collect();
        p.order.retain(|x| !moving.contains(x));
        place(&mut p, &self.layer, &Place { parent: &self.parent, above: &self.above, below: &self.below })?;
        let at = p.order.iter().position(|x| *x == self.layer).map_or(p.order.len(), |i| i + 1);
        for (k, inner) in moving.iter().filter(|x| **x != self.layer).enumerate() {
            p.order.insert(at + k, inner.clone());
        }
        p.check().map_err(from_paint)?;
        let new_parent = p.layer(&self.layer).map_err(from_paint)?.parent.clone();
        let mut patch =
            Patch::new().with(path_in(&self.painting, &["order"]), Some(before_order), Some(json(&p.order)?));
        if new_parent != old.parent {
            patch = patch.with(
                path_in(&self.painting, &["layers", &self.layer, "parent"]),
                old.parent.map(Value::String),
                new_parent.clone().map(Value::String),
            );
        }
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "layer": self.layer, "parent": new_parent, "order": p.order }));
        Ok(plan)
    }
}
register_op!(MoveLayer);
