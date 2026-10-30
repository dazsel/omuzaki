//! Making, listing, reading and deleting paintings.

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_paint::{Colour, Layer, LayerKind, Painting};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{edit, query};
use crate::doc::{PAINTINGS, from_paint, id, ids, json, load, painting_path};

/// Make a new, empty painting: a canvas to draw a character, a part of one, or a texture on, mark by mark. Add layers
/// with `paint.add_layer`, then draw on them with `paint.draw`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "id": "bust", "name": "Painted bust", "width": 1080, "height": 1920, "background": "#000000" },
    { "id": "eye-l", "name": "Left eye", "width": 512, "height": 512 }
]))]
pub struct New {
    /// The painting's id: letters, digits, `-`, `_` and `.`.
    pub id: String,
    /// What it is, in a few words.
    pub name: String,
    /// Width in pixels (1 to 8192).
    pub width: u32,
    /// Height in pixels (1 to 8192).
    pub height: u32,
    /// The backdrop under every layer. Default clear.
    #[serde(default)]
    pub background: Colour,
    /// Start with one empty paint layer of this id (handy for quick sketches). Default none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_layer: Option<String>,
}

impl Op for New {
    fn meta() -> OpMeta {
        edit("paint.new", "Make a new, empty painting of a size, to draw on mark by mark.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        id("id", &self.id)?;
        if ctx.doc.get(&painting_path(&self.id)).is_some() {
            return Err(OpError::conflict(format!("there is already a painting {:?}", self.id))
                .hint("pick another id, or paint.delete the old one first"));
        }
        let mut p = Painting::new(self.name.clone(), self.width, self.height);
        p.background = self.background;
        if let Some(layer) = &self.first_layer {
            id("first_layer", layer)?;
            p.layers.insert(layer.clone(), Layer::paint(layer.clone()));
            p.order.push(layer.clone());
        }
        p.check().map_err(from_paint)?;
        let section = DocPath::root().join(PAINTINGS);
        let mut patch = Patch::new();
        if ctx.doc.get(&section).is_none() {
            // The first painting opens the section.
            patch = patch.with(section, None, Some(json!({})));
        }
        let patch = patch.with(painting_path(&self.id), None, Some(json(&p)?));
        let mut plan = Plan::edit(patch);
        plan.output =
            Some(json!({ "painting": self.id, "width": self.width, "height": self.height, "layers": p.order }));
        Ok(plan)
    }
}
register_op!(New);

/// Delete a painting and every layer and mark in it (undoable).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "painting": "bust" }]))]
pub struct Delete {
    /// The painting's id.
    pub painting: String,
}

impl Op for Delete {
    fn meta() -> OpMeta {
        edit("paint.delete", "Delete a painting with all its layers and marks.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        load(ctx.doc, &self.painting)?;
        let path = painting_path(&self.painting);
        let before = ctx.doc.get(&path).cloned();
        let mut plan = Plan::edit(Patch::new().with(path, before, None));
        plan.output = Some(json!({ "deleted": self.painting }));
        Ok(plan)
    }
}
register_op!(Delete);

/// List the paintings: each one's id, name, size, layer count and mark count.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct List {}

impl Op for List {
    fn meta() -> OpMeta {
        query("paint.list", "List the paintings with their sizes and how many layers and marks each holds.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut out = Vec::new();
        for pid in ids(ctx.doc) {
            let p = load(ctx.doc, &pid)?;
            out.push(json!({
                "id": pid, "name": p.name, "width": p.width, "height": p.height,
                "layers": p.layers.len(), "marks": p.mark_count(),
            }));
        }
        Ok(Plan::answer(json!({ "paintings": out })))
    }
}
register_op!(List);

/// Read a painting: its layers back to front with their settings and mark counts, and, for one layer, its marks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "painting": "bust" }, { "painting": "bust", "layer": "lines", "from": 0, "count": 50 }]))]
pub struct Get {
    /// The painting's id.
    pub painting: String,
    /// A layer whose marks to list. Default: no marks, only the layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// The first mark to list. Default 0.
    #[serde(default)]
    pub from: usize,
    /// How many marks to list (at most 500). Default 100.
    #[serde(default = "hundred")]
    pub count: usize,
}

fn hundred() -> usize {
    100
}

/// The most marks one `paint.get` lists.
pub const MAX_LISTED: usize = 500;

impl Op for Get {
    fn meta() -> OpMeta {
        query(
            "paint.get",
            "Read a painting's layers (back to front, with blend, opacity, clipping and mark counts) and one layer's marks.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let p = load(ctx.doc, &self.painting)?;
        if self.count > MAX_LISTED {
            return Err(OpError::invalid("count", format!("at most {MAX_LISTED} marks at a time")));
        }
        let layers: Vec<Value> = p
            .order
            .iter()
            .filter_map(|lid| p.layers.get(lid).map(|l| (lid, l)))
            .map(|(lid, l)| {
                json!({
                    "id": lid, "name": l.name,
                    "kind": if l.kind == LayerKind::Group { "group" } else { "paint" },
                    "parent": l.parent, "visible": l.visible, "opacity": l.opacity, "blend": l.blend,
                    "clip": l.clip, "role": l.role, "masked": l.mask.is_some(), "marks": l.marks.len(),
                })
            })
            .collect();
        let mut out = json!({
            "painting": self.painting, "name": p.name, "width": p.width, "height": p.height,
            "background": p.background, "layers": layers,
        });
        if let Some(lid) = &self.layer {
            let l = p.layer(lid).map_err(from_paint)?;
            let marks: Vec<Value> = l
                .marks
                .iter()
                .enumerate()
                .skip(self.from)
                .take(self.count)
                .map(|(i, m)| json(m).map(|v| json!({ "index": i, "mark": v })))
                .collect::<Result<_, _>>()?;
            out["marks"] = Value::Array(marks);
            out["mask"] = json(&l.mask)?;
        }
        Ok(Plan::answer(out))
    }
}
register_op!(Get);
